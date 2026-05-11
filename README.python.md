# hash98-miner

Multi-GPU (OpenCL) proof-of-work miner for the **HASH98** inscription mint at
[`h98hash.xyz`](https://www.h98hash.xyz/) — ERC-20 `0x1E5adF70321CA28b3Ead70Eac545E6055E969e6f`
on Ethereum mainnet. Brute-forces `sha256(challengeFor(wallet) ‖ nonce16)` on every OpenCL device
(NVIDIA RTX 4090 / 5090 etc.), rotates through a pool of burner wallets (each capped at **5 mints**),
and submits `mint(bytes16 nonce)` transactions. The on-chain PoW is pinned in
[`reference/SPEC.md`](reference/SPEC.md); the contract interface (the source is unverified, so this
is reverse-engineered) is in [`reference/HASH98_ABI.md`](reference/HASH98_ABI.md).

> Mint is **free** (you pay only gas). The protocol caps public mints at 20,000 total — it sells out;
> mine sooner rather than later. Difficulty is on-chain and rises as hashpower joins (40+ as of writing).

## Install

```bash
cd hash98-miner
pip install -e .                 # add ".[dev]" for the test deps
hashminer devices                # list OpenCL devices (flat indices used elsewhere)
hashminer selftest               # SHA-256 kernel vs hashlib + a real on-chain digest; brute-force roundtrip
hashminer bench --device all     # measure GH/s; sweep --unroll {compact|full|auto|<int>} and --local-size to tune
```

Needs an NVIDIA driver + OpenCL ICD (the NVIDIA driver ships one). `pyopencl` is installed by the
above. `python>=3.11`.

## Run it

1. Make some **burner wallets** and put their private keys (one `0x…` hex key per line) in a file —
   e.g. `keys.txt` (it's gitignored; never commit keys). Fund each with a little ETH for gas (a
   `mint()` tx is ~55–60k gas — `~0.001 ETH` covers several at low gas prices). 5 mints per wallet,
   so N wallets ⇒ up to 5N mints.
2. `cp miner.example.toml miner.toml` and edit: set `[accounts].keys_file = "keys.txt"`, point
   `[network].rpc_url` at a good RPC (the public defaults are rate-limited — use your own node or a
   paid provider for serious mining), set `[gpu].unroll` to whatever `hashminer bench` says is
   fastest on your cards.
3. Mine:

```bash
hashminer run --dry-run          # full pipeline against live mainnet — finds nonces, would-send txs, never broadcasts
hashminer run                    # mine for real (sends mint() txs from your keys; spends gas)
hashminer accounts               # show each wallet's mints-used / ETH balance / eligibility
```

Keys can also come from the `HASH98_PRIVATE_KEY` env var (single wallet). Per-wallet progress is
persisted to `hash98-state.json` so a restart resumes. The miner mines for one wallet at a time (all
GPUs focused), submits its `mint()`, then moves to the next eligible wallet while that tx confirms.

### Common knobs

| | |
|---|---|
| `--unroll {compact\|full\|auto\|<int>}` / `[gpu].unroll` | round-loop unroll. `compact` (no unroll) is often best on Ampere; `full` (unroll 64) on Ada/Blackwell. **Bench it.** |
| `--local-size N` / `[gpu].local_size` | OpenCL work-group size (64 default; try 128/256 on Blackwell). |
| `--devices "all"\|"0,2"` / `[gpu].devices` | which GPUs. |
| `[gas].gas_limit` (default 90000) | fixed gas limit (skips an `estimate_gas` RPC per tx). |
| `[gas].max_fee_gwei` (default 30) | skip a submission if the needed maxFee exceeds this. |
| `[behaviour].verify_proof_onchain` (default true) | pre-flight a found nonce with the on-chain `verifyProof` view before sending. |
| `HASH98_*` env vars | override anything in `miner.toml` (env wins). |

## How it works

```
chain.py ──block poll/WS, difficulty(), getStats()──▶ miner.py ──set_job(challengeFor(wallet), difficulty, tag)──▶ gpu.py (one OpenCL worker thread per device)
   ▲                                                     │ drain thread pulls Found(nonce, tag)            │
accounts.py (wallet pool: mintNonce 0..5, ETH balance)   ▼                                                 ▼
   ▲ refresh on confirm                          verify.py (CPU re-hash with hashlib.sha256) ◀── farm.results queue
submit.py ── verifyProof() pre-flight ── build/sign/send mint(bytes16) (EIP-1559) ─▶ public mempool
```

- `challenge16 = bytes16(keccak256(abi.encodePacked(account, mintNonce, chainId, contract, "HASH98")))` —
  read live via `challengeFor(account)` (depends on the wallet's current `mintNonce`, which ++ after each mint).
- The kernel hashes the fixed 32-byte preimage `challenge16 ‖ nonce16` (one SHA-256 compression) and
  reports any nonce whose digest, big-endian, has ≥ `difficulty()` leading zero bits — exactly what
  the contract re-checks. The nonce's high 64 bits are a per-device random salt; the low 64 bits are
  the search counter.
- Every GPU hit is re-hashed on the CPU (`hashlib.sha256`) and re-checked against the *current*
  difficulty before it can become a tx (and optionally pre-flighted with the on-chain `verifyProof`),
  so a difficulty bump between "found it" and "sent it" can't push a bad tx.

## Tests

```bash
pip install -e ".[dev]"
pytest                                  # GPU tests skip cleanly if no OpenCL device
pytest tests/test_sha256_vectors.py
hashminer selftest                      # on-demand kernel-vs-reference check
```

## Notes

- The HASH98 contract source is **not verified** on Etherscan; the ABI / PoW were reverse-engineered
  from the frontend JS and the live chain (and cross-checked against a real `Minted` event — the
  challenge derivation + SHA-256 reproduce the on-chain `digest`). See `reference/HASH98_ABI.md`.
- This is a permissionless, free, PoW-gated public mint whose own docs invite external miners and
  concurrent multi-wallet mining. More hashpower just mints faster (and nudges the on-chain
  difficulty up). With a funded key it sends real mainnet transactions and spends real ETH on gas —
  use dedicated burner keys.
- **Secrets**: keys live in `[accounts].keys_file` / `HASH98_PRIVATE_KEY` / `.env` — never in
  `miner.toml`, never in a committed file, never in a shell command that lands in history. `.gitignore`
  covers `miner.toml`, `.env`, `keys*.txt`, `*.key`, `hash98-state.json`.
