//! Click-style CLI: `devices`, `selftest`, `bench`, `accounts`, `run`.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

use crate::accounts::WalletPool;
use crate::chain::ChainClient;
use crate::config::{Config, DeviceSpec, UnrollSpec};
use crate::gpu::{list_devices, select_devices, GpuFarm, GpuWorker};
use crate::miner::Miner;
use crate::pow::{compute_challenge, is_valid_nonce, pow_digest, CHALLENGE_LEN};

#[derive(Parser, Debug)]
#[command(
    name = "hashminer",
    version = crate::VERSION,
    about = "Multi-GPU OpenCL miner for HASH98 (h98hash.xyz) — Rust port"
)]
struct Cli {
    /// Path to miner.toml (default: ./miner.toml if present).
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// RPC URL override (also: HASH98_RPC_URL env var).
    #[arg(long, global = true)]
    rpc: Option<String>,

    /// Override --devices "all" or "0,2".
    #[arg(long, global = true)]
    devices: Option<String>,

    /// Override --unroll compact|full|auto|<int>.
    #[arg(long, global = true)]
    unroll: Option<String>,

    /// Override --local-size N.
    #[arg(long, global = true)]
    local_size: Option<usize>,

    /// Log level: trace|debug|info|warn|error.
    #[arg(long, global = true)]
    log_level: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// List OpenCL devices with their flat indices.
    Devices,
    /// SHA-256 kernel sanity tests + on-chain digest reproduction.
    Selftest {
        /// Difficulty for the brute-force roundtrip test (default 18).
        #[arg(long, default_value_t = 18u32)]
        difficulty: u32,
    },
    /// Benchmark — measure GH/s on the selected device(s).
    Bench {
        /// Seconds per device.
        #[arg(long, default_value_t = 10u64)]
        seconds: u64,
        /// Difficulty (sets target — too low and the kernel reports too many hits).
        #[arg(long, default_value_t = 40u32)]
        difficulty: u32,
    },
    /// List configured wallets, mint progress and ETH balances.
    Accounts,
    /// Run the miner.
    Run {
        /// Don't broadcast transactions; show what would be sent.
        #[arg(long)]
        dry_run: bool,
        /// Disable on-chain `verifyProof` pre-flight.
        #[arg(long)]
        no_verify_proof: bool,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.log_level.as_deref())?;

    let mut cfg = Config::load(cli.config.as_deref())?;
    if let Some(rpc) = cli.rpc.clone() {
        cfg.rpc_url = rpc;
    }
    if let Some(d) = &cli.devices {
        cfg.gpu_devices = if d.eq_ignore_ascii_case("all") {
            DeviceSpec::All
        } else {
            DeviceSpec::Indices(d.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        };
    }
    if let Some(u) = &cli.unroll {
        cfg.sha256_unroll = UnrollSpec::parse(u)?;
    }
    if let Some(ls) = cli.local_size {
        cfg.local_size = Some(ls);
    }

    match cli.cmd {
        Cmd::Devices => cmd_devices(),
        Cmd::Selftest { difficulty } => cmd_selftest(&cfg, difficulty),
        Cmd::Bench { seconds, difficulty } => cmd_bench(&cfg, seconds, difficulty),
        Cmd::Accounts => async_runtime()?.block_on(cmd_accounts(&cfg)),
        Cmd::Run { dry_run, no_verify_proof } => {
            if dry_run { cfg.dry_run = true; }
            if no_verify_proof { cfg.verify_proof_onchain = false; }
            async_runtime()?.block_on(cmd_run(cfg))
        }
    }
}

fn async_runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

fn init_tracing(level_override: Option<&str>) -> Result<()> {
    let lvl = level_override.map(|s| s.to_string()).unwrap_or_else(|| "info".to_string());
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&lvl));
    fmt().with_env_filter(filter).with_target(false).try_init().ok();
    Ok(())
}

// ----- subcommands -----------------------------------------------------------------------

fn cmd_devices() -> Result<()> {
    let devices = list_devices()?;
    if devices.is_empty() {
        println!("(no OpenCL GPU devices detected)");
        return Ok(());
    }
    println!("idx  platform                                  device");
    for d in devices {
        println!(
            "{:>3}  {:<40}  {}  [CU={}, max_wg={}, mem={} MB]",
            d.flat_index, d.platform_name, d.device_name, d.compute_units, d.max_work_group_size, d.global_mem_mb
        );
    }
    Ok(())
}

fn cmd_selftest(cfg: &Config, difficulty: u32) -> Result<()> {
    println!("== known on-chain digest reproduction ==");
    let mut acct = [0u8; 20];
    hex::decode_to_slice("1547aa95FAE1E9bE0447b6a5C55B6665E01a7866", &mut acct)?;
    let mut contract = [0u8; 20];
    hex::decode_to_slice("1E5adF70321CA28b3Ead70Eac545E6055E969e6f", &mut contract)?;
    let ch = compute_challenge(acct, 0, 1, contract);
    let mut nz = [0u8; 16];
    hex::decode_to_slice("d8c3ba740000000f000044a20000415d", &mut nz)?;
    let nonce = u128::from_be_bytes(nz);
    let digest = pow_digest(&ch, nonce);
    let want = "00000000003c398fa44e2b50c49d0519969c343b40a5da9974fb7ed6dc7e8e56";
    println!("got : {}", hex::encode(digest));
    println!("want: {}", want);
    if hex::encode(digest) != want {
        bail!("on-chain digest mismatch");
    }
    println!("✔ matches");
    println!();

    println!("== GPU kernel SHA-256 round-trip @ difficulty={difficulty} ==");
    let devices = select_devices(&cfg.gpu_devices)?;
    let info = devices[0].clone();
    println!("using device #{}: {}", info.flat_index, info.device_name);
    let mut worker = GpuWorker::new(info, &cfg.sha256_unroll, cfg.local_size)?;
    let mut total_h: u64 = 0;
    let mut found = Vec::new();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) && found.is_empty() {
        let (n, hits, _ms) = worker.search_batch(&ch, difficulty)?;
        total_h += n;
        found.extend(hits);
    }
    println!("hashed {} attempts in {:.2}s — {} hit(s) found",
             total_h, started.elapsed().as_secs_f64(), found.len());
    for n in &found {
        let ok = is_valid_nonce(&ch, *n, difficulty);
        println!("  nonce={} valid={} digest={}",
                 hex::encode(n.to_be_bytes()), ok, hex::encode(pow_digest(&ch, *n)));
        if !ok {
            bail!("kernel reported invalid nonce");
        }
    }
    println!("✔ all hits CPU-verified");
    Ok(())
}

fn cmd_bench(cfg: &Config, seconds: u64, difficulty: u32) -> Result<()> {
    let devices = select_devices(&cfg.gpu_devices)?;
    println!("benching {} device(s) × {} s @ difficulty={}, unroll={}",
             devices.len(), seconds, difficulty, cfg.sha256_unroll.label());
    let challenge: [u8; CHALLENGE_LEN] = rand::random();
    for d in devices {
        println!("\n--- #{} {} ({}) ---", d.flat_index, d.device_name, d.platform_name);
        let mut w = GpuWorker::new(d, &cfg.sha256_unroll, cfg.local_size)?;
        let started = Instant::now();
        let mut total: u64 = 0;
        while started.elapsed() < Duration::from_secs(seconds) {
            let (n, _hits, _ms) = w.search_batch(&challenge, difficulty)?;
            total += n;
        }
        let elapsed = started.elapsed().as_secs_f64();
        let ghs = (total as f64) / elapsed / 1e9;
        println!("  {:.2} GH/s  ({} hashes / {:.1} s)", ghs, total, elapsed);
    }
    Ok(())
}

async fn cmd_accounts(cfg: &Config) -> Result<()> {
    let chain = ChainClient::from_config(cfg)?;
    let pool = WalletPool::from_config(cfg)?;
    pool.load_persisted();
    println!("{} wallet(s) configured  (cap = 5 mints/wallet)", pool.wallets.len());
    println!("idx  address                                       used  remaining  ETH         eligible?");
    for (i, w) in pool.wallets.iter().enumerate() {
        let st = chain.wallet_state(w.address).await
            .with_context(|| format!("wallet {:#x}", w.address))?;
        pool.apply_state(w, &st);
        let eth = wei_to_eth_str(st.eth_wei);
        let used = st.mint_nonce;
        let remain = 5u64.saturating_sub(used);
        let eligible = remain > 0
            && (cfg.dry_run
                || !cfg.accounts.skip_unfunded
                || st.eth_wei >= alloy::primitives::U256::from((cfg.accounts.min_gas_balance_eth * 1e18) as u128));
        println!("{:>3}  {:#x}  {:>4}  {:>9}  {:>10}  {}",
                 i, w.address, used, remain, eth, if eligible { "yes" } else { "no" });
    }
    let _ = pool.persist();
    Ok(())
}

fn wei_to_eth_str(wei: alloy::primitives::U256) -> String {
    // Approximate via f64 for display only.
    let f: f64 = wei.to_string().parse().unwrap_or(0.0);
    format!("{:.6}", f / 1e18)
}

async fn cmd_run(cfg: Config) -> Result<()> {
    info!("starting hashminer (Rust port)");
    if cfg.dry_run {
        info!("DRY-RUN MODE: no transactions will be broadcast");
    }
    let miner = Miner::build(cfg).await?;

    // Ctrl+C handler.
    let m_for_signal = miner.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("Ctrl+C received — shutting down");
        m_for_signal.shutdown();
    });

    miner.run().await?;
    miner.shutdown();
    Ok(())
}
