# hash98-miner

> **A multi-GPU miner for the [HASH98](https://www.h98hash.xyz/) free PoW-gated mint on Ethereum mainnet.**
>
> [简体中文版 →](README.md)　·　[Original Python version →](README.python.md)

---

## What is it?

HASH98 is a **free-to-mint** Ethereum inscription contract at [`0x1E5a…9e6f`](https://etherscan.io/address/0x1E5adF70321CA28b3Ead70Eac545E6055E969e6f).
But to mint, you first have to solve a SHA-256 puzzle (proof-of-work) — only then will the contract accept your transaction.

This tool will:

1. Brute-force a valid nonce on your GPU(s);
2. Sign and broadcast a `mint()` transaction once one is found;
3. Manage a pool of wallets and rotate through them (each capped at 5 mints).

> **Minting itself is free**, but every successful `mint()` transaction still costs Ethereum gas. Use throwaway wallets — **never your main wallet**.

---

## Who is this for?

- Anyone with a discrete NVIDIA / AMD / Intel **GPU** (integrated graphics work too, but slowly).
- People who want to mint HASH98 without solving the PoW manually.
- People who don't mind paying Ethereum mainnet gas fees.

---

## One-click start (easiest)

**Windows users**: double-click `start.bat`.

On first launch it will:

1. Walk you through entering your private key, RPC URL, and other basics, then write `.env`;
2. Automatically build the program (`cargo build --release`);
3. Show a menu:

   | # | Action |
   |---|--------|
   | 1 | Run miner (real transactions) |
   | 2 | Dry-run (find nonces but don't broadcast) |
   | 3 | Benchmark (measure GPU hashrate) |
   | 4 | List all GPU devices |
   | 5 | Show wallet status |
   | 6 | Self-test (verify kernel correctness) |

Subsequent launches skip setup & build and go straight to the menu.

---

## Installation

### 1. Install Rust

Get the installer at <https://rustup.rs/> and accept the defaults.
After it finishes, close and reopen your terminal, then run `cargo --version` to confirm.

### 2. Install the OpenCL SDK (**required on Windows**)

`opencl3` needs to link against `OpenCL.lib` at build time. **Install any one** of:

- **NVIDIA GPU** → [CUDA Toolkit](https://developer.nvidia.com/cuda-downloads)
- **Intel GPU / iGPU** → [Intel oneAPI Base Toolkit](https://www.intel.com/content/www/us/en/developer/tools/oneapi/base-toolkit-download.html)
- **AMD GPU** → [AMD ROCm](https://rocm.docs.amd.com/) or AMD APP SDK

Then make sure the directory containing `OpenCL.lib` is on `LIB`, e.g.:

```bat
set LIB=%LIB%;C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.5\lib\x64
```

### 3. Install GPU drivers

Make sure you have the latest vendor GPU drivers (NVIDIA / AMD / Intel official drivers all ship an OpenCL runtime, which is what actually runs the kernel).

### 4. Launch

```bat
start.bat
```

Or manually:

```bash
cargo build --release
target\release\hashminer.exe devices    # confirm your GPU is detected
target\release\hashminer.exe selftest   # verify kernel correctness
target\release\hashminer.exe run        # start mining
```

---

## Configuration (`.env`)

`start.bat` generates `.env` for you on first launch. You can also edit it manually:

| Variable | Meaning | Example / Default |
|----------|---------|-------------------|
| `HASH98_PRIVATE_KEY` | Single wallet private key (0x-prefixed) | `0xabc123…` |
| `HASH98_KEYS_FILE` | Multi-wallet: path to a text file, **one private key per line** | `keys.txt` |
| `HASH98_RPC_URL` | Ethereum RPC endpoint | `https://ethereum-rpc.publicnode.com` |
| `HASH98_RPC_FALLBACKS` | Comma-separated fallback RPCs, used when the primary fails | `https://eth.llamarpc.com,…` |
| `HASH98_WS_URL` | Optional WebSocket for live new-block notifications | (empty = HTTP polling) |
| `HASH98_GPU_DEVICES` | Which GPUs to use. `all` or comma-separated indices like `0,1` | `all` |
| `HASH98_UNROLL` | Kernel unroll mode (affects speed, see below) | `compact` |
| `HASH98_LOCAL_SIZE` | OpenCL work-group size | `64` (try `128` / `256`) |
| `HASH98_DRY_RUN` | Set `true` to find nonces without broadcasting | `false` |
| `HASH98_LOG_LEVEL` | Logging verbosity | `INFO` |

### Private key safety

- Use **either** `HASH98_PRIVATE_KEY` **or** `HASH98_KEYS_FILE`, not both.
- Keys belong **only** in `.env`, environment variables, or `keys.txt`. **Do not** put them in `miner.toml` — the program will refuse to load.
- `.env`, `keys.txt`, `miner.toml`, and `hash98-state.json` are all in `.gitignore`. **Never commit them.**
- Strongly recommended: use throwaway wallets, each funded with just enough ETH for 5 mints.

### How to choose `--unroll`

| Value | Best for |
|-------|----------|
| `compact` | NVIDIA Ampere (RTX 30 series) — usually fastest |
| `full` | NVIDIA Ada / Blackwell (RTX 40/50 series) — usually fastest |
| `auto` | Let the OpenCL compiler decide |
| Integer (e.g. `8`) | Custom unroll factor |

On a new GPU, run `hashminer bench --unroll compact` and `--unroll full` once each and compare GH/s.

---

## Command reference

```bash
hashminer devices               # List all GPU devices and their indices
hashminer selftest              # Sanity check: kernel vs CPU + a real on-chain digest
hashminer bench                 # Benchmark each GPU and print GH/s
hashminer accounts              # Per-wallet status: mints used / ETH balance / eligibility
hashminer run                   # Mine for real (broadcasts transactions)
hashminer run --dry-run         # Full pipeline but never broadcasts
```

Every command supports `--help`.

---

## How it works (brief)

```
┌──────────┐   read difficulty / challenge       ┌──────────┐
│ Ethereum │ ─────────────────────────────────▶  │ chain.rs │
│ mainnet  │                                     └─────┬────┘
└──────────┘                                           │ set mining job
                                                       ▼
                                              ┌──────────────┐
                                              │   miner.rs   │ orchestrator
                                              └─────┬────────┘
                                                    │ broadcast job to all GPUs
                                                    ▼
                                              ┌──────────────┐
                                              │   gpu.rs     │ one thread per GPU
                                              │ (OpenCL)     │ brute-force nonce
                                              └─────┬────────┘
                                                    │ report candidates
                                                    ▼
                                              ┌──────────────┐
                                              │  verify.rs   │ CPU re-hash SHA-256
                                              └─────┬────────┘
                                                    │ verified
                                                    ▼
                                              ┌──────────────┐
   sign & broadcast mint() tx  ◀──────────────│  submit.rs   │
                                              └──────────────┘
```

For full details see [reference/SPEC.md](reference/SPEC.md).

---

## FAQ

**Q: How long until I get a mint?**
About `2^difficulty / total_hashrate` seconds. Current difficulty is around 40, so a modern GPU averages a few to ~15 minutes per mint.

**Q: Does adding more wallets speed up a single solution?**
No. Multi-wallet lets you mint **more total** (5 per wallet) and keeps the GPU busy while a `mint()` tx confirms.

**Q: Could I waste gas on invalid transactions?**
No. Every candidate goes through ① CPU re-hash and ② on-chain `verifyProof()` pre-flight before broadcast.

**Q: What if the contract difficulty goes up?**
The miner re-reads difficulty every block. If it rises between "found" and "sent", the stale answer is discarded.

**Q: Linux / macOS support?**
The code is cross-platform. `start.bat` is Windows-only, but `cargo build --release && ./target/release/hashminer run` works fine on Linux. (Apple deprecated OpenCL on macOS, so it's not recommended there.)

---

## Project layout

```
hash98-miner-cli/
├── src/                    Rust source
│   ├── pow.rs              PoW algorithm (ground truth)
│   ├── gpu.rs              OpenCL multi-device farm
│   ├── chain.rs            Ethereum RPC wrapper
│   ├── accounts.rs         Multi-wallet pool
│   ├── submit.rs           Tx build & broadcast
│   ├── miner.rs            Main orchestrator
│   ├── verify.rs           CPU re-check
│   ├── abi.rs              HASH98 contract ABI
│   ├── config.rs           Config loading
│   └── cli.rs              CLI entry
├── kernels/sha256_pow.cl   OpenCL SHA-256 search kernel
├── reference/              Contract spec & reverse-engineering notes
├── python-legacy/          Original Python implementation (kept for reference)
├── start.bat               Windows one-click launcher
├── .env.example            Config template
└── miner.example.toml      Optional TOML config template
```

---

## License

MIT
