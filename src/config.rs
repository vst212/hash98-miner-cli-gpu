//! Configuration: `miner.toml` (TOML) + `HASH98_*` environment overrides + `.env`.
//!
//! Private keys are *never* read from `miner.toml` — only from `[accounts].keys_file` (one hex key
//! per line) or the `HASH98_PRIVATE_KEY` env var.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::abi::{HASH98_CONTRACT_ADDRESS, MAINNET_CHAIN_ID};

pub const GWEI: u128 = 1_000_000_000;
pub const ENV_KEY_VAR: &str = "HASH98_PRIVATE_KEY";

pub const DEFAULT_PUBLIC_RPCS: &[&str] = &[
    "https://ethereum-rpc.publicnode.com",
    "https://eth.llamarpc.com",
    "https://rpc.ankr.com/eth",
    "https://eth.drpc.org",
    "https://cloudflare-eth.com",
];

#[derive(Debug, Clone)]
pub enum DeviceSpec {
    All,
    Indices(Vec<usize>),
}

impl Default for DeviceSpec {
    fn default() -> Self {
        DeviceSpec::All
    }
}

#[derive(Debug, Clone)]
pub enum UnrollSpec {
    Compact,
    Full,
    Auto,
    N(u32),
}

impl Default for UnrollSpec {
    fn default() -> Self {
        UnrollSpec::Compact
    }
}

impl UnrollSpec {
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim().to_ascii_lowercase();
        Ok(match s.as_str() {
            "compact" => UnrollSpec::Compact,
            "full" => UnrollSpec::Full,
            "auto" => UnrollSpec::Auto,
            other => UnrollSpec::N(other.parse().context("unroll must be compact|full|auto|<int>")?),
        })
    }

    pub fn build_flag(&self) -> String {
        match self {
            UnrollSpec::Compact => "-D SHA256_UNROLL=1".into(),
            UnrollSpec::Full => "-D SHA256_UNROLL=64".into(),
            UnrollSpec::Auto => "-D SHA256_UNROLL=0".into(),
            UnrollSpec::N(n) => format!("-D SHA256_UNROLL={}", n),
        }
    }

    pub fn label(&self) -> String {
        match self {
            UnrollSpec::Compact => "compact".into(),
            UnrollSpec::Full => "full".into(),
            UnrollSpec::Auto => "auto".into(),
            UnrollSpec::N(n) => n.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GasConfig {
    pub priority_gwei: f64,
    pub max_fee_gwei: Option<f64>,
    pub base_fee_multiplier: f64,
    pub gas_limit: Option<u64>,
    pub gas_limit_multiplier: f64,
}

impl Default for GasConfig {
    fn default() -> Self {
        Self {
            priority_gwei: 1.0,
            max_fee_gwei: Some(30.0),
            base_fee_multiplier: 2.0,
            gas_limit: Some(90_000),
            gas_limit_multiplier: 1.30,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccountsConfig {
    pub keys_file: Option<PathBuf>,
    pub min_gas_balance_eth: f64,
    pub skip_unfunded: bool,
    pub state_file: Option<PathBuf>,
    pub miner_address: Option<String>,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            keys_file: None,
            min_gas_balance_eth: 0.0008,
            skip_unfunded: true,
            state_file: Some(PathBuf::from("hash98-state.json")),
            miner_address: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    // network
    pub rpc_url: String,
    pub rpc_fallbacks: Vec<String>,
    pub ws_url: Option<String>,
    pub chain_id: u64,
    pub contract: String,
    pub poll_interval_s: f64,
    pub request_timeout_s: f64,

    // gpu
    pub gpu_devices: DeviceSpec,
    pub local_size: Option<usize>,
    pub sha256_unroll: UnrollSpec,

    // behaviour
    pub dry_run: bool,
    pub confirmations: u64,
    pub log_level: String,
    pub submit_concurrency: usize,
    pub verify_proof_onchain: bool,
    pub poll_idle_seconds: f64,

    pub gas: GasConfig,
    pub accounts: AccountsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rpc_url: DEFAULT_PUBLIC_RPCS[0].to_string(),
            rpc_fallbacks: DEFAULT_PUBLIC_RPCS[1..].iter().map(|s| s.to_string()).collect(),
            ws_url: None,
            chain_id: MAINNET_CHAIN_ID,
            contract: format!("{:#x}", HASH98_CONTRACT_ADDRESS),
            poll_interval_s: 4.0,
            request_timeout_s: 6.0,
            gpu_devices: DeviceSpec::All,
            local_size: None,
            sha256_unroll: UnrollSpec::Compact,
            dry_run: false,
            confirmations: 1,
            log_level: "INFO".into(),
            submit_concurrency: 4,
            verify_proof_onchain: true,
            poll_idle_seconds: 20.0,
            gas: GasConfig::default(),
            accounts: AccountsConfig::default(),
        }
    }
}

// ----- TOML deserialization (mirror of the Python schema) -------------------------------

#[derive(Debug, Deserialize, Default)]
struct RawToml {
    #[serde(default)]
    network: NetworkRaw,
    #[serde(default)]
    gpu: GpuRaw,
    #[serde(default, alias = "behavior")]
    behaviour: BehaviourRaw,
    #[serde(default)]
    gas: GasRaw,
    #[serde(default, alias = "wallet")]
    accounts: AccountsRaw,
}

#[derive(Debug, Deserialize, Default)]
struct NetworkRaw {
    rpc_url: Option<String>,
    rpc_fallbacks: Option<Vec<String>>,
    ws_url: Option<String>,
    chain_id: Option<u64>,
    contract: Option<String>,
    poll_interval_s: Option<f64>,
    request_timeout_s: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
struct GpuRaw {
    devices: Option<toml::Value>,
    local_size: Option<usize>,
    #[serde(alias = "sha256_unroll")]
    unroll: Option<toml::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct BehaviourRaw {
    dry_run: Option<bool>,
    confirmations: Option<u64>,
    log_level: Option<String>,
    submit_concurrency: Option<usize>,
    verify_proof_onchain: Option<bool>,
    poll_idle_seconds: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
struct GasRaw {
    priority_gwei: Option<f64>,
    max_fee_gwei: Option<f64>,
    base_fee_multiplier: Option<f64>,
    gas_limit: Option<u64>,
    gas_limit_multiplier: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
struct AccountsRaw {
    keys_file: Option<String>,
    min_gas_balance_eth: Option<f64>,
    skip_unfunded: Option<bool>,
    state_file: Option<String>,
    miner_address: Option<String>,
    // Forbidden — refuse to read keys from miner.toml.
    private_key: Option<toml::Value>,
    private_keys: Option<toml::Value>,
    key_private: Option<toml::Value>,
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let _ = dotenvy::dotenv(); // optional .env
        let mut cfg = Config::default();
        let toml_path = path.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("miner.toml"));
        if path.is_some() || toml_path.exists() {
            let raw_text = std::fs::read_to_string(&toml_path)
                .with_context(|| format!("read {}", toml_path.display()))?;
            let raw: RawToml = toml::from_str(&raw_text).context("parse miner.toml")?;
            apply_toml(&mut cfg, raw)?;
        }
        apply_env(&mut cfg)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn rpc_chain(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for u in std::iter::once(&self.rpc_url).chain(self.rpc_fallbacks.iter()) {
            if !u.is_empty() && seen.insert(u.clone()) {
                out.push(u.clone());
            }
        }
        out
    }

    pub fn env_private_key(&self) -> Option<String> {
        std::env::var(ENV_KEY_VAR).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    pub fn keys_file_keys(&self) -> Vec<String> {
        let Some(p) = self.accounts.keys_file.as_ref() else { return Vec::new() };
        let p = expand_user(p);
        let Ok(text) = std::fs::read_to_string(&p) else { return Vec::new() };
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if !line.is_empty() {
                out.push(line.to_string());
            }
        }
        out
    }

    pub fn all_private_keys(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut push = |k: String| {
            let l = k.to_lowercase();
            if seen.insert(l) {
                out.push(k);
            }
        };
        if let Some(k) = self.env_private_key() {
            push(k);
        }
        for k in self.keys_file_keys() {
            push(k);
        }
        out
    }

    pub fn validate(&self) -> Result<()> {
        if self.rpc_chain().is_empty() {
            bail!("no RPC URL configured");
        }
        if self.chain_id == 0 {
            bail!("chain_id must be positive");
        }
        Ok(())
    }
}

fn expand_user(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~") {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']));
        }
    }
    p.to_path_buf()
}

fn apply_toml(cfg: &mut Config, raw: RawToml) -> Result<()> {
    let n = raw.network;
    if let Some(v) = n.rpc_url { cfg.rpc_url = v; }
    if let Some(v) = n.rpc_fallbacks { cfg.rpc_fallbacks = v; }
    cfg.ws_url = n.ws_url.or(cfg.ws_url.take());
    if let Some(v) = n.chain_id { cfg.chain_id = v; }
    if let Some(v) = n.contract { cfg.contract = v; }
    if let Some(v) = n.poll_interval_s { cfg.poll_interval_s = v; }
    if let Some(v) = n.request_timeout_s { cfg.request_timeout_s = v; }

    let g = raw.gpu;
    if let Some(v) = g.devices { cfg.gpu_devices = parse_device_spec_value(&v)?; }
    if let Some(v) = g.local_size { cfg.local_size = Some(v); }
    if let Some(v) = g.unroll { cfg.sha256_unroll = parse_unroll_value(&v)?; }

    let b = raw.behaviour;
    if let Some(v) = b.dry_run { cfg.dry_run = v; }
    if let Some(v) = b.confirmations { cfg.confirmations = v; }
    if let Some(v) = b.log_level { cfg.log_level = v; }
    if let Some(v) = b.submit_concurrency { cfg.submit_concurrency = v; }
    if let Some(v) = b.verify_proof_onchain { cfg.verify_proof_onchain = v; }
    if let Some(v) = b.poll_idle_seconds { cfg.poll_idle_seconds = v; }

    let gs = raw.gas;
    if let Some(v) = gs.priority_gwei { cfg.gas.priority_gwei = v; }
    cfg.gas.max_fee_gwei = gs.max_fee_gwei.or(cfg.gas.max_fee_gwei);
    if let Some(v) = gs.base_fee_multiplier { cfg.gas.base_fee_multiplier = v; }
    if gs.gas_limit.is_some() { cfg.gas.gas_limit = gs.gas_limit; }
    if let Some(v) = gs.gas_limit_multiplier { cfg.gas.gas_limit_multiplier = v; }

    let a = raw.accounts;
    if a.private_key.is_some() || a.private_keys.is_some() || a.key_private.is_some() {
        bail!("refusing to read private keys from miner.toml — use the HASH98_PRIVATE_KEY env var or [accounts].keys_file");
    }
    if let Some(v) = a.keys_file { cfg.accounts.keys_file = Some(PathBuf::from(v)); }
    if let Some(v) = a.min_gas_balance_eth { cfg.accounts.min_gas_balance_eth = v; }
    if let Some(v) = a.skip_unfunded { cfg.accounts.skip_unfunded = v; }
    if let Some(v) = a.state_file { cfg.accounts.state_file = Some(PathBuf::from(v)); }
    if let Some(v) = a.miner_address { cfg.accounts.miner_address = Some(v); }
    Ok(())
}

fn parse_device_spec_value(v: &toml::Value) -> Result<DeviceSpec> {
    Ok(match v {
        toml::Value::String(s) => {
            let s = s.trim().to_ascii_lowercase();
            if s == "all" || s == "*" || s.is_empty() { DeviceSpec::All }
            else { DeviceSpec::Indices(s.split(',').filter_map(|x| x.trim().parse().ok()).collect()) }
        }
        toml::Value::Array(arr) => {
            let mut idx = Vec::new();
            for v in arr {
                idx.push(v.as_integer().ok_or_else(|| anyhow!("device index must be int"))? as usize);
            }
            DeviceSpec::Indices(idx)
        }
        _ => return Err(anyhow!(r#"gpu.devices must be "all" or a list of ints"#)),
    })
}

fn parse_unroll_value(v: &toml::Value) -> Result<UnrollSpec> {
    Ok(match v {
        toml::Value::String(s) => UnrollSpec::parse(s)?,
        toml::Value::Integer(n) => UnrollSpec::N(*n as u32),
        _ => return Err(anyhow!(r#"gpu.unroll must be a string or int"#)),
    })
}

fn apply_env(cfg: &mut Config) -> Result<()> {
    let e = |k: &str| std::env::var(k).ok();
    if let Some(v) = e("HASH98_RPC_URL") { cfg.rpc_url = v; }
    if let Some(v) = e("HASH98_RPC_FALLBACKS") {
        cfg.rpc_fallbacks = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    if let Some(v) = e("HASH98_WS_URL") { cfg.ws_url = Some(v); }
    if let Some(v) = e("HASH98_CONTRACT") { cfg.contract = v; }
    if let Some(v) = e("HASH98_CHAIN_ID") { cfg.chain_id = v.parse().context("HASH98_CHAIN_ID")?; }
    if let Some(v) = e("HASH98_KEYS_FILE") { cfg.accounts.keys_file = Some(PathBuf::from(v)); }
    if let Some(v) = e("HASH98_MINER_ADDRESS") { cfg.accounts.miner_address = Some(v); }
    if let Some(v) = e("HASH98_GPU_DEVICES") {
        cfg.gpu_devices = if v.trim().eq_ignore_ascii_case("all") {
            DeviceSpec::All
        } else {
            DeviceSpec::Indices(v.replace(' ', "").split(',').filter_map(|x| x.parse().ok()).collect())
        };
    }
    if let Some(v) = e("HASH98_UNROLL") { cfg.sha256_unroll = UnrollSpec::parse(&v)?; }
    if let Some(v) = e("HASH98_LOCAL_SIZE") { cfg.local_size = Some(v.parse().context("HASH98_LOCAL_SIZE")?); }
    if let Some(v) = e("HASH98_DRY_RUN") {
        cfg.dry_run = matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
    }
    if let Some(v) = e("HASH98_LOG_LEVEL") { cfg.log_level = v; }
    Ok(())
}
