//! Thin wrapper around alloy `Provider` with HTTP RPC failover, plus block-following (HTTP poll
//! fallback or WebSocket `newHeads` if `cfg.ws_url` is set).

use anyhow::{anyhow, bail, Context, Result};
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::transports::BoxTransport;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

use crate::abi::{Bytes16, Hash98};
use crate::config::Config;

pub type DynProvider = RootProvider<BoxTransport>;

#[derive(Debug, Clone)]
pub struct ProtocolState {
    pub difficulty: u32,
    pub mint_open: bool,
    pub public_minted: u64,
    pub max_public: u64,
    pub reserve_minted: u64,
    pub block_number: u64,
}

#[derive(Debug, Clone)]
pub struct WalletState {
    pub address: Address,
    pub challenge16: [u8; 16],
    pub mint_nonce: u64,
    pub eth_wei: U256,
    pub token_wei: U256,
}

pub struct ChainClient {
    pub contract_addr: Address,
    pub chain_id: u64,
    pub urls: Mutex<Vec<String>>,
    pub ws_url: Option<String>,
    pub timeout: Duration,
    pub challenge_check_done: Mutex<bool>,
}

impl ChainClient {
    pub fn from_config(cfg: &Config) -> Result<Arc<Self>> {
        let contract_addr: Address = cfg.contract.parse().context("contract address")?;
        let urls = cfg.rpc_chain();
        if urls.is_empty() {
            bail!("no RPC URL configured");
        }
        Ok(Arc::new(Self {
            contract_addr,
            chain_id: cfg.chain_id,
            urls: Mutex::new(urls),
            ws_url: cfg.ws_url.clone(),
            timeout: Duration::from_secs_f64(cfg.request_timeout_s.max(1.0)),
            challenge_check_done: Mutex::new(false),
        }))
    }

    fn current_url(&self) -> Option<String> {
        self.urls.lock().first().cloned()
    }

    fn rotate(&self) {
        let mut g = self.urls.lock();
        if g.len() > 1 {
            let head = g.remove(0);
            g.push(head);
        }
    }

    /// Build a provider from the *current* head URL.
    pub async fn provider(&self) -> Result<DynProvider> {
        let url = self.current_url().ok_or_else(|| anyhow!("no RPC urls"))?;
        Ok(ProviderBuilder::new().on_builtin(&url).await.with_context(|| format!("build provider for {url}"))?)
    }

    /// Run an async op, rotating the URL ring on failure.
    pub async fn try_each<F, Fut, T>(&self, mut op: F) -> Result<T>
    where
        F: FnMut(DynProvider) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let attempts = self.urls.lock().len();
        let mut last: Option<anyhow::Error> = None;
        for _ in 0..attempts {
            let p = match self.provider().await {
                Ok(p) => p,
                Err(e) => {
                    warn!("provider build failed: {e:?}");
                    self.rotate();
                    last = Some(e);
                    continue;
                }
            };
            match op(p).await {
                Ok(r) => return Ok(r),
                Err(e) => {
                    warn!("RPC op failed on {}: {e:?}", self.current_url().unwrap_or_default());
                    self.rotate();
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("all RPCs failed")))
    }

    pub async fn block_number(&self) -> Result<u64> {
        let addr = self.contract_addr;
        let _ = addr; // silence
        self.try_each(|p| async move { Ok(p.get_block_number().await?) }).await
    }

    pub async fn protocol_state(&self) -> Result<ProtocolState> {
        let contract_addr = self.contract_addr;
        self.try_each(|p| async move {
            let c = Hash98::new(contract_addr, &p);
            let stats = c.getStats().call().await?;
            let max_public = c.MAX_PUBLIC_MINTS().call().await?._0;
            let bn = p.get_block_number().await?;
            Ok(ProtocolState {
                difficulty: u256_to_u32(stats.difficulty_),
                mint_open: stats.mintOpen_,
                public_minted: u256_to_u64(stats.publicMinted_),
                max_public: u256_to_u64(max_public),
                reserve_minted: u256_to_u64(stats.treasuryReserved_),
                block_number: bn,
            })
        })
        .await
    }

    pub async fn wallet_state(&self, addr: Address) -> Result<WalletState> {
        let contract_addr = self.contract_addr;
        self.try_each(|p| async move {
            let c = Hash98::new(contract_addr, &p);
            let challenge: Bytes16 = c.challengeFor(addr).call().await?._0;
            let mint_nonce_u = c.mintNonce(addr).call().await?._0;
            let token_bal = c.balanceOf(addr).call().await?._0;
            let eth = p.get_balance(addr).await?;
            let mut ch = [0u8; 16];
            ch.copy_from_slice(&challenge.0);
            Ok(WalletState {
                address: addr,
                challenge16: ch,
                mint_nonce: u256_to_u64(mint_nonce_u),
                eth_wei: eth,
                token_wei: token_bal,
            })
        })
        .await
    }

    pub async fn challenge_for(&self, addr: Address) -> Result<[u8; 16]> {
        let contract_addr = self.contract_addr;
        let chain_id = self.chain_id;
        let challenge: Bytes16 = self
            .try_each(|p| async move {
                let c = Hash98::new(contract_addr, &p);
                Ok(c.challengeFor(addr).call().await?._0)
            })
            .await?;
        let mut ch = [0u8; 16];
        ch.copy_from_slice(&challenge.0);

        // First-call cross-check against compute_challenge (mintNonce reads as well).
        let mut done = self.challenge_check_done.lock();
        if !*done {
            let nonce_u = self
                .try_each(|p| async move {
                    let c = Hash98::new(contract_addr, &p);
                    Ok(c.mintNonce(addr).call().await?._0)
                })
                .await?;
            let mn = u256_to_u64(nonce_u);
            let computed = crate::pow::compute_challenge(addr.0.into(), mn, chain_id, contract_addr.0.into());
            if computed != ch {
                bail!(
                    "challenge cross-check failed: on-chain={} computed={} (mintNonce={mn})",
                    hex::encode(ch),
                    hex::encode(computed)
                );
            }
            *done = true;
            debug!("challenge cross-check OK for {addr:#x}");
        }
        Ok(ch)
    }

    pub async fn verify_proof_onchain(&self, addr: Address, nonce_b16: [u8; 16]) -> Result<bool> {
        let contract_addr = self.contract_addr;
        let nonce = Bytes16::from(nonce_b16);
        self.try_each(|p| async move {
            let c = Hash98::new(contract_addr, &p);
            Ok(c.verifyProof(addr, nonce).call().await?._0)
        })
        .await
    }
}

fn u256_to_u64(v: U256) -> u64 {
    let limbs = v.into_limbs();
    limbs[0]
}
fn u256_to_u32(v: U256) -> u32 {
    let limbs = v.into_limbs();
    limbs[0] as u32
}

/// Block-following: yields whenever a new head is observed (number, hash optional).
pub async fn follow_blocks(
    chain: Arc<ChainClient>,
    poll_interval: Duration,
) -> tokio::sync::mpsc::Receiver<u64> {
    let (tx, rx) = tokio::sync::mpsc::channel::<u64>(8);
    let ws_url = chain.ws_url.clone();
    tokio::spawn(async move {
        if let Some(ws) = ws_url {
            if try_ws_loop(&ws, &tx).await.is_ok() {
                return;
            }
            warn!("WS subscribe failed for {ws} — falling back to HTTP polling");
        }
        // HTTP polling fallback.
        let mut last = 0u64;
        loop {
            match chain.block_number().await {
                Ok(n) if n != last => {
                    last = n;
                    if tx.send(n).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) => warn!("get_block_number: {e:?}"),
            }
            tokio::time::sleep(poll_interval).await;
        }
    });
    rx
}

async fn try_ws_loop(ws_url: &str, tx: &tokio::sync::mpsc::Sender<u64>) -> Result<()> {
    use alloy::providers::WsConnect;
    use futures_util::StreamExt;
    let provider = ProviderBuilder::new().on_ws(WsConnect::new(ws_url)).await?;
    let sub = provider.subscribe_blocks().await?;
    let mut stream = sub.into_stream();
    while let Some(blk) = stream.next().await {
        let n = blk.inner.number;
        if tx.send(n).await.is_err() {
            break;
        }
    }
    Ok(())
}
