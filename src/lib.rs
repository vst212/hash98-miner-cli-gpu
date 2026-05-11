//! `hash98-miner` — multi-GPU OpenCL proof-of-work miner for the HASH98 inscription mint.
//!
//! Rust port of the original Python project; see `README.md` and `reference/SPEC.md`.

pub mod abi;
pub mod accounts;
pub mod chain;
pub mod cli;
pub mod config;
pub mod gpu;
pub mod miner;
pub mod pow;
pub mod submit;
pub mod verify;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const KERNEL_SRC: &str = include_str!("../kernels/sha256_pow.cl");
