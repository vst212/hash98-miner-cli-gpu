//! OpenCL device enumeration + per-device PoW search worker + multi-GPU farm.
//!
//! One `GpuWorker` per OpenCL device, each running its own `std::thread`. Workers share a single
//! `Job { challenge, target, epoch }` under a `Mutex` and emit `Found { nonce, epoch }` on a
//! crossbeam channel. The orchestrator (`miner::Miner`) updates the job atomically when it wants
//! all GPUs to switch to a new wallet's challenge.

use anyhow::{anyhow, bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use opencl3::command_queue::{CommandQueue, CL_QUEUE_PROFILING_ENABLE};
use opencl3::context::Context as ClContext;
use opencl3::device::{get_all_devices, Device, CL_DEVICE_TYPE_GPU};
use opencl3::kernel::{ExecuteKernel, Kernel};
use opencl3::memory::{Buffer, CL_MEM_READ_WRITE};
use opencl3::platform::get_platforms;
use opencl3::program::Program;
use opencl3::types::{cl_uint, cl_ulong, CL_NON_BLOCKING};
use parking_lot::Mutex;
use rand::RngCore;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::config::{DeviceSpec, UnrollSpec};
use crate::pow::{
    challenge_words, nonce_base_limbs_le, nonce_from_limbs_le, target_limbs_be32, CHALLENGE_LEN,
};
use crate::KERNEL_SRC;

const MAX_RESULTS_PER_LAUNCH: usize = 16;
const TARGET_LAUNCH_MS: f64 = 80.0;
const INITIAL_BATCH: usize = 1 << 20; // 1,048,576 work items

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub flat_index: usize,
    pub platform_name: String,
    pub device_name: String,
    pub device_type: String,
    pub compute_units: u32,
    pub max_work_group_size: usize,
    pub global_mem_mb: u64,
}

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mut out = Vec::new();
    let platforms = get_platforms().map_err(|e| anyhow!("get_platforms: {e:?}"))?;
    let mut flat = 0usize;
    for p in platforms {
        let pname = p.name().unwrap_or_default();
        let dev_ids = match p.get_devices(CL_DEVICE_TYPE_GPU) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for did in dev_ids {
            let dev = Device::new(did);
            let info = DeviceInfo {
                flat_index: flat,
                platform_name: pname.clone(),
                device_name: dev.name().unwrap_or_default(),
                device_type: "GPU".into(),
                compute_units: dev.max_compute_units().unwrap_or(0),
                max_work_group_size: dev.max_work_group_size().unwrap_or(0),
                global_mem_mb: dev.global_mem_size().unwrap_or(0) / (1024 * 1024),
            };
            out.push(info);
            flat += 1;
        }
    }
    if out.is_empty() {
        // Some implementations only expose CPU under CL_DEVICE_TYPE_ALL; fall back via the helper.
        if let Ok(all_dev_ids) = get_all_devices(CL_DEVICE_TYPE_GPU) {
            for did in all_dev_ids {
                let dev = Device::new(did);
                out.push(DeviceInfo {
                    flat_index: out.len(),
                    platform_name: "(default)".into(),
                    device_name: dev.name().unwrap_or_default(),
                    device_type: "GPU".into(),
                    compute_units: dev.max_compute_units().unwrap_or(0),
                    max_work_group_size: dev.max_work_group_size().unwrap_or(0),
                    global_mem_mb: dev.global_mem_size().unwrap_or(0) / (1024 * 1024),
                });
            }
        }
    }
    Ok(out)
}

pub fn select_devices(spec: &DeviceSpec) -> Result<Vec<DeviceInfo>> {
    let all = list_devices()?;
    if all.is_empty() {
        bail!("no OpenCL GPU devices detected");
    }
    Ok(match spec {
        DeviceSpec::All => all,
        DeviceSpec::Indices(idx) => {
            let mut out = Vec::new();
            for &i in idx {
                let d = all.iter().find(|d| d.flat_index == i)
                    .ok_or_else(|| anyhow!("device index {i} out of range (have {})", all.len()))?;
                out.push(d.clone());
            }
            out
        }
    })
}

fn raw_device_id_for_flat_index(flat: usize) -> Result<*mut std::ffi::c_void> {
    let platforms = get_platforms().map_err(|e| anyhow!("get_platforms: {e:?}"))?;
    let mut cur = 0usize;
    for p in platforms {
        let dev_ids = p.get_devices(CL_DEVICE_TYPE_GPU).unwrap_or_default();
        for did in dev_ids {
            if cur == flat {
                return Ok(did);
            }
            cur += 1;
        }
    }
    Err(anyhow!("flat device index {flat} not found"))
}

#[derive(Debug, Clone)]
pub struct Job {
    pub challenge: [u8; CHALLENGE_LEN],
    pub difficulty: u32,
    pub epoch: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Found {
    pub nonce: u128,
    pub epoch: u64,
    pub device_index: usize,
}

#[derive(Debug, Default)]
pub struct DeviceStats {
    pub hashes: AtomicU64,
    pub launches: AtomicU64,
}

pub struct GpuWorker {
    info: DeviceInfo,
    context: ClContext,
    queue: CommandQueue,
    kernel: Kernel,
    out_count_buf: Buffer<cl_uint>,
    out_nonces_buf: Buffer<cl_ulong>,
    salt_hi64: u64,
    nonce_counter: u64,
    batch_size: usize,
    local_size: Option<usize>,
}

impl GpuWorker {
    pub fn new(info: DeviceInfo, unroll: &UnrollSpec, local_size: Option<usize>) -> Result<Self> {
        let did = raw_device_id_for_flat_index(info.flat_index)?;
        let device = Device::new(did);
        let context = ClContext::from_device(&device)
            .map_err(|e| anyhow!("create context for {}: {e:?}", info.device_name))?;
        let queue = CommandQueue::create_default(&context, CL_QUEUE_PROFILING_ENABLE)
            .map_err(|e| anyhow!("create command queue: {e:?}"))?;

        let opts = unroll.build_flag();
        let program = Program::create_and_build_from_source(&context, KERNEL_SRC, &opts)
            .map_err(|e| anyhow!("build kernel for {}: {e}", info.device_name))?;
        let kernel = Kernel::create(&program, "sha256_pow_search")
            .map_err(|e| anyhow!("create kernel: {e:?}"))?;

        let out_count_buf = unsafe {
            Buffer::<cl_uint>::create(&context, CL_MEM_READ_WRITE, 1, std::ptr::null_mut())
                .map_err(|e| anyhow!("alloc out_count: {e:?}"))?
        };
        let out_nonces_buf = unsafe {
            Buffer::<cl_ulong>::create(
                &context,
                CL_MEM_READ_WRITE,
                MAX_RESULTS_PER_LAUNCH * 2,
                std::ptr::null_mut(),
            )
            .map_err(|e| anyhow!("alloc out_nonces: {e:?}"))?
        };

        let salt_hi64 = rand::thread_rng().next_u64();

        Ok(Self {
            info,
            context,
            queue,
            kernel,
            out_count_buf,
            out_nonces_buf,
            salt_hi64,
            nonce_counter: 0,
            batch_size: INITIAL_BATCH,
            local_size,
        })
    }

    pub fn device(&self) -> &DeviceInfo {
        &self.info
    }

    /// Run a single batch. Returns (hashes_done, hits, elapsed_ms).
    pub fn search_batch(
        &mut self,
        challenge: &[u8; CHALLENGE_LEN],
        difficulty: u32,
    ) -> Result<(u64, Vec<u128>, f64)> {
        let cw = challenge_words(challenge);
        let tw = target_limbs_be32(difficulty);

        let nb_lo = self.nonce_counter;
        let nb_hi = self.salt_hi64;
        let n_total = self.batch_size as cl_uint;

        // Zero out_count.
        let zero = [0u32; 1];
        unsafe {
            self.queue
                .enqueue_write_buffer(&mut self.out_count_buf, CL_NON_BLOCKING as u32, 0, &zero, &[])
                .map_err(|e| anyhow!("write out_count: {e:?}"))?;
        }

        let started = Instant::now();
        let mut exec = ExecuteKernel::new(&self.kernel);
        unsafe {
            exec.set_arg(&cw[0]).set_arg(&cw[1]).set_arg(&cw[2]).set_arg(&cw[3]);
            exec.set_arg(&tw[0]).set_arg(&tw[1]).set_arg(&tw[2]).set_arg(&tw[3])
                .set_arg(&tw[4]).set_arg(&tw[5]).set_arg(&tw[6]).set_arg(&tw[7]);
            exec.set_arg(&(nb_lo as cl_ulong));
            exec.set_arg(&(nb_hi as cl_ulong));
            exec.set_arg(&n_total);
            exec.set_arg(&(MAX_RESULTS_PER_LAUNCH as cl_uint));
            exec.set_arg(&self.out_count_buf);
            exec.set_arg(&self.out_nonces_buf);
            exec.set_global_work_size(self.batch_size);
            if let Some(ls) = self.local_size {
                exec.set_local_work_size(ls);
            }
            exec.enqueue_nd_range(&self.queue)
                .map_err(|e| anyhow!("enqueue kernel: {e:?}"))?;
        }
        self.queue.finish().map_err(|e| anyhow!("finish: {e:?}"))?;
        let elapsed = started.elapsed();
        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;

        // Read back count.
        let mut count_host = [0u32; 1];
        unsafe {
            self.queue
                .enqueue_read_buffer(&self.out_count_buf, true as u32, 0, &mut count_host, &[])
                .map_err(|e| anyhow!("read out_count: {e:?}"))?;
        }
        let n_hits = (count_host[0] as usize).min(MAX_RESULTS_PER_LAUNCH);

        let mut hits = Vec::new();
        if n_hits > 0 {
            let mut nonces_host = vec![0u64; n_hits * 2];
            unsafe {
                self.queue
                    .enqueue_read_buffer(&self.out_nonces_buf, true as u32, 0, &mut nonces_host, &[])
                    .map_err(|e| anyhow!("read out_nonces: {e:?}"))?;
            }
            for i in 0..n_hits {
                let lo = nonces_host[i * 2];
                let hi = nonces_host[i * 2 + 1];
                hits.push(nonce_from_limbs_le(lo, hi));
            }
        }

        // Advance counter (with carry into salt_hi64 if it overflows — vanishingly unlikely).
        let (new_lo, carry) = self.nonce_counter.overflowing_add(self.batch_size as u64);
        self.nonce_counter = new_lo;
        if carry {
            self.salt_hi64 = self.salt_hi64.wrapping_add(1);
        }

        // Adaptive batch sizing — aim at ~80 ms/launch.
        if elapsed_ms > 0.5 {
            let ratio = TARGET_LAUNCH_MS / elapsed_ms;
            let new_batch = ((self.batch_size as f64) * ratio.clamp(0.5, 2.0)) as usize;
            let new_batch = new_batch.max(1 << 16).min(1 << 28);
            // Round to multiple of local size.
            let ls = self.local_size.unwrap_or(64).max(1);
            let new_batch = (new_batch / ls).max(1) * ls;
            self.batch_size = new_batch;
        }

        Ok((n_total as u64, hits, elapsed_ms))
    }
}

pub struct GpuFarm {
    pub job: Arc<Mutex<Option<Job>>>,
    pub stop: Arc<AtomicBool>,
    pub results: Receiver<Found>,
    pub stats: Vec<Arc<DeviceStats>>,
    pub devices: Vec<DeviceInfo>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl GpuFarm {
    pub fn start(
        devices: Vec<DeviceInfo>,
        unroll: UnrollSpec,
        local_size: Option<usize>,
    ) -> Result<Self> {
        if devices.is_empty() {
            bail!("no devices selected");
        }
        let job: Arc<Mutex<Option<Job>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = bounded::<Found>(256);
        let mut stats = Vec::new();
        let mut threads = Vec::new();
        for d in &devices {
            let st = Arc::new(DeviceStats::default());
            stats.push(st.clone());
            let job_c = job.clone();
            let stop_c = stop.clone();
            let tx_c = tx.clone();
            let info = d.clone();
            let unroll_c = unroll.clone();
            let device_index = d.flat_index;
            let handle = std::thread::Builder::new()
                .name(format!("gpu-{}", d.flat_index))
                .spawn(move || {
                    if let Err(e) = run_worker(info, unroll_c, local_size, job_c, stop_c, tx_c, st, device_index) {
                        warn!("gpu worker {} crashed: {e:?}", device_index);
                    }
                })
                .context("spawn gpu thread")?;
            threads.push(handle);
        }
        // Drop original tx so when all workers exit, rx closes.
        drop(tx);
        Ok(Self { job, stop, results: rx, stats, devices, threads })
    }

    pub fn set_job(&self, challenge: [u8; CHALLENGE_LEN], difficulty: u32, epoch: u64) {
        let mut g = self.job.lock();
        *g = Some(Job { challenge, difficulty, epoch });
    }

    pub fn clear_job(&self) {
        *self.job.lock() = None;
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }

    pub fn total_hashrate(&self) -> f64 {
        // Caller should sample stats.hashes / elapsed externally; this is a convenience accumulator.
        self.stats.iter().map(|s| s.hashes.load(Ordering::Relaxed) as f64).sum()
    }
}

impl Drop for GpuFarm {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker(
    info: DeviceInfo,
    unroll: UnrollSpec,
    local_size: Option<usize>,
    job: Arc<Mutex<Option<Job>>>,
    stop: Arc<AtomicBool>,
    tx: Sender<Found>,
    stats: Arc<DeviceStats>,
    device_index: usize,
) -> Result<()> {
    info!(
        "init GPU #{}: {} ({}, CU={})",
        info.flat_index, info.device_name, info.platform_name, info.compute_units
    );
    let mut worker = GpuWorker::new(info.clone(), &unroll, local_size)?;
    let mut last_epoch = u64::MAX;

    while !stop.load(Ordering::Acquire) {
        let cur = { job.lock().clone() };
        let Some(j) = cur else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        if j.epoch != last_epoch {
            last_epoch = j.epoch;
            // Re-randomise the salt high 64 on every epoch so two restarts don't collide.
            worker.salt_hi64 = rand::thread_rng().next_u64();
            worker.nonce_counter = 0;
        }
        match worker.search_batch(&j.challenge, j.difficulty) {
            Ok((n, hits, _ms)) => {
                stats.hashes.fetch_add(n, Ordering::Relaxed);
                stats.launches.fetch_add(1, Ordering::Relaxed);
                for nonce in hits {
                    let _ = tx.send(Found { nonce, epoch: j.epoch, device_index });
                }
            }
            Err(e) => {
                warn!("GPU #{} batch error: {e:?}", info.flat_index);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    debug!("GPU #{} stopped", info.flat_index);
    Ok(())
}
