//! Orchestrator: drives the GpuFarm by pointing all GPUs at one wallet's challenge at a time,
//! drains verified hits, submits the first one per `(wallet, mintNonce)`, parks that wallet until
//! its tx confirms, and re-points the farm at the next eligible wallet. On every block: re-read
//! `getStats()`, refresh wallets, and poll receipts.

use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::accounts::{Wallet, WalletPool};
use crate::chain::{follow_blocks, ChainClient, ProtocolState};
use crate::config::Config;
use crate::gpu::{select_devices, Found, GpuFarm};
use crate::submit::Submitter;
use crate::verify::verify;

pub struct Miner {
    pub cfg: Config,
    pub chain: Arc<ChainClient>,
    pub pool: Arc<WalletPool>,
    pub submitter: Arc<Submitter>,
    pub farm: Arc<Mutex<GpuFarm>>,
    pub epoch: Arc<AtomicU64>,
    pub current_wallet: Arc<Mutex<Option<Arc<Wallet>>>>,
    pub stop: Arc<AtomicBool>,
    pub state: Arc<Mutex<ProtocolState>>,
}

impl Miner {
    pub async fn build(cfg: Config) -> Result<Arc<Self>> {
        let chain = ChainClient::from_config(&cfg)?;
        let pool = Arc::new(WalletPool::from_config(&cfg)?);
        pool.load_persisted();
        let submitter = Arc::new(Submitter::new(chain.clone(), pool.clone(), &cfg));

        let devices = select_devices(&cfg.gpu_devices)?;
        info!("selected {} OpenCL device(s):", devices.len());
        for d in &devices {
            info!(
                "  #{} {} ({}, CU={}, max_wg={})",
                d.flat_index, d.device_name, d.platform_name, d.compute_units, d.max_work_group_size
            );
        }
        let farm = GpuFarm::start(devices, cfg.sha256_unroll.clone(), cfg.local_size)
            .context("start GPU farm")?;

        let initial_state = chain.protocol_state().await?;
        info!(
            "protocol  difficulty={}  mint_open={}  publicMinted={}/{}",
            initial_state.difficulty, initial_state.mint_open, initial_state.public_minted, initial_state.max_public
        );

        Ok(Arc::new(Self {
            cfg,
            chain,
            pool,
            submitter,
            farm: Arc::new(Mutex::new(farm)),
            epoch: Arc::new(AtomicU64::new(0)),
            current_wallet: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(initial_state)),
        }))
    }

    pub async fn refresh_all_wallets(&self) -> Result<()> {
        for w in &self.pool.wallets {
            if w.parked() {
                continue;
            }
            match self.chain.wallet_state(w.address).await {
                Ok(st) => self.pool.apply_state(w, &st),
                Err(e) => warn!("refresh {}: {e:?}", w.address),
            }
        }
        let _ = self.pool.persist();
        Ok(())
    }

    pub async fn point_at_next_wallet(&self) -> Result<bool> {
        let next = match self.pool.next_eligible(self.cfg.dry_run) {
            Some(w) => w,
            None => return Ok(false),
        };
        // Make sure we have a fresh challenge.
        let challenge = match *next.challenge16.lock() {
            Some(c) => c,
            None => self.chain.challenge_for(next.address).await?,
        };
        *next.challenge16.lock() = Some(challenge);

        let difficulty = self.state.lock().difficulty;
        let new_epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        self.farm.lock().set_job(challenge, difficulty, new_epoch);
        *self.current_wallet.lock() = Some(next.clone());
        info!(
            "→ now mining for {:#x}  challenge={}  difficulty={}",
            next.address, hex::encode(challenge), difficulty
        );
        Ok(true)
    }

    pub async fn run(self: &Arc<Self>) -> Result<()> {
        // Initial wallet refresh.
        self.refresh_all_wallets().await?;

        if !self.point_at_next_wallet().await? {
            bail!("no eligible wallets to mine — all hit cap or unfunded");
        }

        // Spawn block-following loop.
        let me_blocks = self.clone();
        tokio::spawn(async move {
            me_blocks.block_loop().await;
        });

        // Drain GPU results in this task.
        self.drain_loop().await;
        Ok(())
    }

    async fn drain_loop(self: &Arc<Self>) {
        let results_rx = {
            // Take ownership of receiver from farm — but farm holds it. We'll clone via the channel
            // by sharing through farm's existing receiver (it's bounded and Receiver: Clone? No.)
            // Workaround: peek without moving — use try_recv via the farm directly.
            self.farm.lock().results.clone()
        };
        loop {
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            match results_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(found) => self.handle_found(found).await,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    async fn handle_found(self: &Arc<Self>, found: Found) {
        let cur_epoch = self.epoch.load(Ordering::SeqCst);
        if found.epoch != cur_epoch {
            return; // stale
        }
        let cur_wallet = match self.current_wallet.lock().clone() {
            Some(w) => w,
            None => return,
        };
        let challenge = match *cur_wallet.challenge16.lock() {
            Some(c) => c,
            None => return,
        };
        let difficulty = self.state.lock().difficulty;
        let Some(sol) = verify(&challenge, found.nonce, difficulty, found.epoch) else {
            warn!(
                "GPU #{} reported invalid nonce {} (CPU re-check failed)",
                found.device_index, hex::encode(found.nonce.to_be_bytes())
            );
            return;
        };
        info!(
            "✔ verified nonce  device=#{}  lz_bits={}  digest={}",
            found.device_index, sol.leading_zero_bits(), hex::encode(sol.digest)
        );
        // Submit inline (BoxTransport futures aren't Send; submit is short anyway).
        match self.submitter.submit(cur_wallet.clone(), sol).await {
            Ok(_) => {}
            Err(e) => warn!("submit error: {e:?}"),
        }
        if let Err(e) = self.point_at_next_wallet().await {
            warn!("advance: {e:?}");
        }
    }

    async fn block_loop(self: Arc<Self>) {
        let mut rx = follow_blocks(
            self.chain.clone(),
            Duration::from_secs_f64(self.cfg.poll_interval_s.max(0.5)),
        )
        .await;
        let mut last_refresh = Instant::now();
        while let Some(bn) = rx.recv().await {
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            // Re-read protocol state.
            match self.chain.protocol_state().await {
                Ok(ps) => {
                    let prev_d = self.state.lock().difficulty;
                    if ps.difficulty != prev_d {
                        info!("difficulty changed {} → {} (block {})", prev_d, ps.difficulty, bn);
                        // Re-point farm with new difficulty.
                        if let Some(w) = self.current_wallet.lock().clone() {
                            if let Some(ch) = *w.challenge16.lock() {
                                let new_epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
                                self.farm.lock().set_job(ch, ps.difficulty, new_epoch);
                            }
                        }
                    }
                    if !ps.mint_open || ps.public_minted >= ps.max_public {
                        if self.cfg.dry_run {
                            info!("mint sold out / closed (dry_run: continuing anyway for logic test)");
                        } else {
                            info!("mint sold out / closed — stopping");
                            self.stop.store(true, Ordering::Release);
                            break;
                        }
                    }
                    *self.state.lock() = ps;
                }
                Err(e) => warn!("getStats: {e:?}"),
            }
            // Poll receipts.
            if let Err(e) = self.submitter.poll_receipts().await {
                warn!("poll_receipts: {e:?}");
            }
            // Refresh wallets occasionally.
            if last_refresh.elapsed().as_secs_f64() > self.cfg.poll_idle_seconds {
                let _ = self.refresh_all_wallets().await;
                last_refresh = Instant::now();
            }
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        let mut farm = self.farm.lock();
        farm.shutdown();
    }
}
