//! Wallet pool — owns the burner keys, tracks per-wallet `mintNonce` (== mint count, capped at 5),
//! ETH balance, and persists the bookkeeping to a JSON state file so restarts resume cleanly.

use anyhow::{bail, Context, Result};
use alloy::primitives::{Address, U256};
use alloy::signers::local::PrivateKeySigner;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::chain::WalletState;
use crate::config::Config;

pub const MAX_MINTS_PER_WALLET: u64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedWallet {
    pub address: String,
    pub mints_confirmed: u64,
    pub last_seen_mint_nonce: u64,
    pub eth_wei_str: String,           // String so we don't lose precision
    pub balance_token_wei_str: String,
    pub last_refreshed_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedState {
    pub wallets: Vec<PersistedWallet>,
}

#[derive(Debug)]
pub struct Wallet {
    pub address: Address,
    pub signer: PrivateKeySigner,
    pub mint_nonce: Mutex<u64>,
    pub eth_wei: Mutex<U256>,
    pub balance_token_wei: Mutex<U256>,
    pub challenge16: Mutex<Option<[u8; 16]>>,
    pub last_refreshed: Mutex<SystemTime>,
    pub sent_unconfirmed: Mutex<u64>,
}

impl Wallet {
    pub fn from_signer(signer: PrivateKeySigner) -> Self {
        let addr = signer.address();
        Self {
            address: addr,
            signer,
            mint_nonce: Mutex::new(0),
            eth_wei: Mutex::new(U256::ZERO),
            balance_token_wei: Mutex::new(U256::ZERO),
            challenge16: Mutex::new(None),
            last_refreshed: Mutex::new(SystemTime::UNIX_EPOCH),
            sent_unconfirmed: Mutex::new(0),
        }
    }

    pub fn mints_used(&self) -> u64 {
        *self.mint_nonce.lock()
    }
    pub fn mints_remaining(&self) -> u64 {
        MAX_MINTS_PER_WALLET.saturating_sub(self.mints_used())
    }
    pub fn parked(&self) -> bool {
        *self.sent_unconfirmed.lock() > 0
    }
}

pub struct WalletPool {
    pub wallets: Vec<Arc<Wallet>>,
    pub by_address: HashMap<Address, Arc<Wallet>>,
    pub min_gas_balance_wei: U256,
    pub skip_unfunded: bool,
    pub state_path: Option<PathBuf>,
    cursor: Mutex<usize>,
}

impl WalletPool {
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let mut wallets = Vec::new();
        let mut by_address = HashMap::new();
        let keys = cfg.all_private_keys();
        if keys.is_empty() {
            bail!(
                "no private keys configured — set HASH98_PRIVATE_KEY or [accounts].keys_file"
            );
        }
        for raw in keys {
            let signer: PrivateKeySigner = raw
                .strip_prefix("0x").unwrap_or(&raw)
                .parse()
                .context("invalid private key")?;
            let w = Arc::new(Wallet::from_signer(signer));
            by_address.insert(w.address, w.clone());
            wallets.push(w);
        }
        let min_eth = cfg.accounts.min_gas_balance_eth.max(0.0);
        let min_wei = U256::from((min_eth * 1e18) as u128);
        Ok(Self {
            wallets,
            by_address,
            min_gas_balance_wei: min_wei,
            skip_unfunded: cfg.accounts.skip_unfunded,
            state_path: cfg.accounts.state_file.clone(),
            cursor: Mutex::new(0),
        })
    }

    pub fn get(&self, addr: &Address) -> Option<Arc<Wallet>> {
        self.by_address.get(addr).cloned()
    }

    pub fn apply_state(&self, w: &Wallet, st: &WalletState) {
        *w.mint_nonce.lock() = st.mint_nonce;
        *w.eth_wei.lock() = st.eth_wei;
        *w.balance_token_wei.lock() = st.token_wei;
        *w.challenge16.lock() = Some(st.challenge16);
        *w.last_refreshed.lock() = SystemTime::now();
    }

    pub fn next_eligible(&self, dry_run: bool) -> Option<Arc<Wallet>> {
        let n = self.wallets.len();
        if n == 0 {
            return None;
        }
        let mut cursor = self.cursor.lock();
        for _ in 0..n {
            let idx = *cursor % n;
            *cursor = (*cursor + 1) % n;
            let w = &self.wallets[idx];
            if w.mints_remaining() == 0 {
                continue;
            }
            if w.parked() {
                continue;
            }
            if !dry_run && self.skip_unfunded {
                let bal = *w.eth_wei.lock();
                if bal < self.min_gas_balance_wei {
                    continue;
                }
            }
            return Some(w.clone());
        }
        None
    }

    pub fn record_mint_sent(&self, w: &Wallet) {
        *w.sent_unconfirmed.lock() += 1;
    }

    pub fn record_mint_confirmed(&self, w: &Wallet) {
        let mut s = w.sent_unconfirmed.lock();
        if *s > 0 {
            *s -= 1;
        }
        *w.mint_nonce.lock() += 1;
        // Challenge becomes stale — force refresh on next read.
        *w.challenge16.lock() = None;
        let _ = self.persist();
    }

    pub fn record_mint_failed(&self, w: &Wallet) {
        let mut s = w.sent_unconfirmed.lock();
        if *s > 0 {
            *s -= 1;
        }
        let _ = self.persist();
    }

    pub fn persist(&self) -> Result<()> {
        let Some(path) = self.state_path.as_ref() else { return Ok(()) };
        let mut state = PersistedState::default();
        for w in &self.wallets {
            state.wallets.push(PersistedWallet {
                address: format!("{:#x}", w.address),
                mints_confirmed: *w.mint_nonce.lock(),
                last_seen_mint_nonce: *w.mint_nonce.lock(),
                eth_wei_str: w.eth_wei.lock().to_string(),
                balance_token_wei_str: w.balance_token_wei.lock().to_string(),
                last_refreshed_unix: SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
        }
        let json = serde_json::to_string_pretty(&state)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load_persisted(&self) {
        let Some(path) = self.state_path.as_ref() else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Ok(state): Result<PersistedState, _> = serde_json::from_str(&text) else { return };
        for pw in state.wallets {
            if let Ok(addr) = pw.address.parse::<Address>() {
                if let Some(w) = self.by_address.get(&addr) {
                    *w.mint_nonce.lock() = pw.last_seen_mint_nonce;
                }
            }
        }
    }

    pub fn snapshot(&self) -> Vec<(Address, u64, U256)> {
        self.wallets.iter().map(|w| (w.address, *w.mint_nonce.lock(), *w.eth_wei.lock())).collect()
    }
}
