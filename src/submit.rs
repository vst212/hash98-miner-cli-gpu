//! Build / sign / send `mint(bytes16)` EIP-1559 transactions, with optional `verifyProof`
//! pre-flight, per-address tx-nonce counter, and receipt polling.

use anyhow::{anyhow, bail, Context, Result};
use alloy::network::{Ethereum, EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

use crate::abi::{decode_revert, Bytes16, Hash98};
use crate::accounts::{Wallet, WalletPool};
use crate::chain::ChainClient;
use crate::config::Config;
use crate::verify::VerifiedSolution;

#[derive(Debug, Clone, Copy, Default)]
pub struct SubmitStats {
    pub sent: u64,
    pub confirmed: u64,
    pub failed: u64,
    pub preflight_rejected: u64,
}

#[derive(Debug, Clone)]
pub struct PendingTx {
    pub tx_hash: B256,
    pub from: Address,
    pub nonce: u64,
    pub sent_at: SystemTime,
    pub solution: VerifiedSolution,
}

pub struct Submitter {
    pub chain: Arc<ChainClient>,
    pub pool: Arc<WalletPool>,
    pub gas: GasParams,
    pub verify_proof_onchain: bool,
    pub dry_run: bool,
    pub confirmations: u64,
    tx_nonces: Mutex<HashMap<Address, u64>>,
    pub pending: Mutex<Vec<PendingTx>>,
    pub stats: Mutex<SubmitStats>,
}

#[derive(Debug, Clone)]
pub struct GasParams {
    pub priority_wei: u128,
    pub max_fee_wei_cap: Option<u128>,
    pub base_fee_multiplier: f64,
    pub gas_limit: Option<u64>,
    pub gas_limit_multiplier: f64,
}

impl GasParams {
    pub fn from_cfg(cfg: &Config) -> Self {
        let to_wei = |gwei: f64| (gwei * 1e9) as u128;
        Self {
            priority_wei: to_wei(cfg.gas.priority_gwei),
            max_fee_wei_cap: cfg.gas.max_fee_gwei.map(to_wei),
            base_fee_multiplier: cfg.gas.base_fee_multiplier,
            gas_limit: cfg.gas.gas_limit,
            gas_limit_multiplier: cfg.gas.gas_limit_multiplier,
        }
    }
}

impl Submitter {
    pub fn new(chain: Arc<ChainClient>, pool: Arc<WalletPool>, cfg: &Config) -> Self {
        Self {
            chain,
            pool,
            gas: GasParams::from_cfg(cfg),
            verify_proof_onchain: cfg.verify_proof_onchain,
            dry_run: cfg.dry_run,
            confirmations: cfg.confirmations,
            tx_nonces: Mutex::new(HashMap::new()),
            pending: Mutex::new(Vec::new()),
            stats: Mutex::new(SubmitStats::default()),
        }
    }

    async fn next_tx_nonce(&self, addr: Address) -> Result<u64> {
        // Hold the lock briefly; fetch from RPC outside it.
        {
            let g = self.tx_nonces.lock();
            if let Some(n) = g.get(&addr).copied() {
                drop(g);
                let mut g = self.tx_nonces.lock();
                let cur = *g.get(&addr).unwrap_or(&n);
                let next = cur;
                g.insert(addr, cur + 1);
                return Ok(next);
            }
        }
        let pending_count = self.chain.try_each(|p| async move {
            Ok(p.get_transaction_count(addr).pending().await?)
        }).await?;
        let mut g = self.tx_nonces.lock();
        let cur = *g.entry(addr).or_insert(pending_count);
        g.insert(addr, cur + 1);
        Ok(cur)
    }

    pub async fn submit(&self, w: Arc<Wallet>, sol: VerifiedSolution) -> Result<Option<PendingTx>> {
        let addr = w.address;
        let nonce_b16 = sol.nonce_bytes16();

        // Optional verifyProof pre-flight.
        if self.verify_proof_onchain {
            match self.chain.verify_proof_onchain(addr, nonce_b16).await {
                Ok(true) => debug!("verifyProof OK for {addr:#x}"),
                Ok(false) => {
                    warn!("verifyProof returned false — skipping submit");
                    let mut s = self.stats.lock();
                    s.preflight_rejected += 1;
                    return Ok(None);
                }
                Err(e) => {
                    let msg = format!("{e:?}");
                    warn!("verifyProof preflight error: {} ({})", decode_revert(&msg), msg);
                    let mut s = self.stats.lock();
                    s.preflight_rejected += 1;
                    return Ok(None);
                }
            }
        }

        if self.dry_run {
            info!(
                "[dry-run] would mint  from={addr:#x}  nonce={}  digest={}",
                hex::encode(nonce_b16),
                hex::encode(sol.digest)
            );
            return Ok(None);
        }

        // Build, sign, send.
        let tx_nonce = self.next_tx_nonce(addr).await?;
        let chain_id = self.chain.chain_id;
        let contract_addr = self.chain.contract_addr;
        let gas = self.gas.clone();
        let signer = w.signer.clone();
        let nonce_typed = Bytes16::from(nonce_b16);

        let solution_for_pending = sol.clone();
        let tx_hash = self.chain.try_each(move |p| {
            let signer = signer.clone();
            let nonce_typed = nonce_typed.clone();
            let gas = gas.clone();
            async move {
                // Build call data via sol! contract typing.
                let contract = Hash98::new(contract_addr, &p);
                let call = contract.mint(nonce_typed);
                let data = call.calldata().clone();

                // Fee data.
                let latest = p
                    .get_block(alloy::eips::BlockId::latest(), alloy::rpc::types::BlockTransactionsKind::Hashes)
                    .await?;
                let base_fee = latest
                    .as_ref()
                    .and_then(|b| b.header.inner.base_fee_per_gas)
                    .unwrap_or(0u64) as u128;
                let max_priority = gas.priority_wei;
                let mut max_fee = ((base_fee as f64) * gas.base_fee_multiplier) as u128 + max_priority;
                if let Some(cap) = gas.max_fee_wei_cap {
                    if max_fee > cap { max_fee = cap.max(max_priority + 1); }
                }

                let mut req = TransactionRequest::default()
                    .with_from(addr)
                    .with_to(contract_addr)
                    .with_chain_id(chain_id)
                    .with_nonce(tx_nonce)
                    .with_input(data)
                    .with_value(U256::ZERO)
                    .with_max_priority_fee_per_gas(max_priority)
                    .with_max_fee_per_gas(max_fee);

                let gas_limit = if let Some(g) = gas.gas_limit {
                    g
                } else {
                    let est = p.estimate_gas(&req).await? as u64;
                    ((est as f64) * gas.gas_limit_multiplier) as u64
                };
                req = req.with_gas_limit(gas_limit);

                use alloy::eips::eip2718::Encodable2718;
                let wallet: EthereumWallet = signer.into();
                let envelope = req.build(&wallet).await?;
                let raw: alloy::primitives::Bytes = envelope.encoded_2718().into();
                let pending = p.send_raw_transaction(&raw).await?;
                Ok::<B256, anyhow::Error>(*pending.tx_hash())
            }
        }).await?;

        info!(
            "submitted mint tx {}  from={addr:#x}  tx_nonce={tx_nonce}  pow_nonce={}",
            tx_hash, hex::encode(nonce_b16)
        );

        {
            let mut s = self.stats.lock();
            s.sent += 1;
        }
        let pending = PendingTx {
            tx_hash,
            from: addr,
            nonce: tx_nonce,
            sent_at: SystemTime::now(),
            solution: solution_for_pending,
        };
        self.pending.lock().push(pending.clone());
        self.pool.record_mint_sent(&w);
        Ok(Some(pending))
    }

    pub async fn poll_receipts(&self) -> Result<()> {
        let snapshot: Vec<PendingTx> = self.pending.lock().clone();
        if snapshot.is_empty() { return Ok(()); }
        let mut still_pending = Vec::new();
        for p in snapshot {
            let tx_hash = p.tx_hash;
            let receipt = self.chain.try_each(|prov| async move {
                Ok(prov.get_transaction_receipt(tx_hash).await?)
            }).await.ok().flatten();
            match receipt {
                Some(r) => {
                    let success = r.status();
                    let from = p.from;
                    if success {
                        info!("confirmed  tx {}  from={from:#x}  block={:?}", tx_hash, r.block_number);
                        let mut s = self.stats.lock();
                        s.confirmed += 1;
                        if let Some(w) = self.pool.get(&from) {
                            self.pool.record_mint_confirmed(&w);
                        }
                    } else {
                        warn!("FAILED tx {}  from={from:#x}  block={:?}", tx_hash, r.block_number);
                        let mut s = self.stats.lock();
                        s.failed += 1;
                        if let Some(w) = self.pool.get(&from) {
                            self.pool.record_mint_failed(&w);
                        }
                    }
                }
                None => still_pending.push(p),
            }
        }
        *self.pending.lock() = still_pending;
        Ok(())
    }

    pub fn stats_snapshot(&self) -> SubmitStats {
        *self.stats.lock()
    }
}

// silence the unused import in some build modes
#[allow(unused_imports)]
use alloy::eips;
