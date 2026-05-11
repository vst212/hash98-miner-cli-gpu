//! ABI for the HASH98 contract at `0x1E5adF70321CA28b3Ead70Eac545E6055E969e6f` (Ethereum mainnet).
//!
//! The contract source is **not** verified on Etherscan/Sourcify. This ABI subset was extracted
//! from the official frontend's JS bundle and cross-checked against the live contract — see
//! `reference/HASH98_ABI.md` and `python-legacy/hashminer/abi.py`.

use alloy::primitives::{address, Address, FixedBytes};
use alloy::sol;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use tiny_keccak::{Hasher, Keccak};

pub const HASH98_CONTRACT_ADDRESS: Address = address!("1E5adF70321CA28b3Ead70Eac545E6055E969e6f");
pub const MAINNET_CHAIN_ID: u64 = 1;

sol! {
    #[sol(rpc)]
    contract Hash98 {
        // mutating
        function mint(bytes16 nonce) external payable returns (uint256 mintIndex);

        // views
        function challengeFor(address account) external view returns (bytes16);
        function verifyProof(address account, bytes16 nonce) external view returns (bool);
        function mintNonce(address account) external view returns (uint256);
        function difficulty() external view returns (uint256);
        function mintOpen() external view returns (bool);
        function publicMinted() external view returns (uint256);
        function reserveMinted() external view returns (uint256);
        function MAX_MINTS_PER_WALLET() external view returns (uint256);
        function MAX_PUBLIC_MINTS() external view returns (uint256);
        function MINT_AMOUNT() external view returns (uint256);
        function MINT_PRICE() external view returns (uint256);
        function balanceOf(address account) external view returns (uint256);
        function totalSupply() external view returns (uint256);
        function name() external view returns (string);
        function symbol() external view returns (string);

        function getStats() external view returns (
            uint256 publicMinted_, uint256 treasuryReserved_, uint256 totalSupply_,
            uint256 activeListings_, uint256 difficulty_, bool mintOpen_,
            bool marketOpen_, bool listingOpen_, bool buyingOpen_,
            bool batchOpen_, uint8 marketMode_
        );
    }
}

/// Convenience alias — `bytes16` (the on-chain `mint(bytes16)` argument shape).
pub type Bytes16 = FixedBytes<16>;

const _ERROR_SIGNATURES: &[&str] = &[
    "Closed()", "InvalidProof()", "WalletMintLimitReached()", "SoldOut()", "IncorrectPayment()",
    "InvalidAmount()", "InvalidConfig()", "InvalidPrice()", "ReentrancyGuardReentrantCall()",
    "TransferFailed()", "InsufficientBalance()", "NotListed()", "NotListingSeller()",
    "CannotBuyOwnListing()", "BatchDisabled()", "BatchTooLarge()",
    "OwnableInvalidOwner(address)", "OwnableUnauthorizedAccount(address)",
    "ERC20InsufficientBalance(address,uint256,uint256)",
    "ERC20InsufficientAllowance(address,uint256,uint256)",
    "ERC20InvalidApprover(address)", "ERC20InvalidReceiver(address)",
    "ERC20InvalidSender(address)", "ERC20InvalidSpender(address)",
];

/// `0x<4-byte selector>` → human error name (e.g. `"WalletMintLimitReached"`).
pub static ERROR_SELECTORS: Lazy<HashMap<String, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    for sig in _ERROR_SIGNATURES {
        let mut k = Keccak::v256();
        k.update(sig.as_bytes());
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        let selector = format!("0x{}", hex::encode(&out[..4]));
        let name: &'static str = sig.split('(').next().unwrap_or(*sig);
        m.insert(selector, name);
    }
    m
});

/// Best-effort: scan a revert / estimate_gas error string for a 4-byte selector and map it to a name.
pub fn decode_revert(s: &str) -> String {
    // Find any 0x[0-9a-fA-F]{8} substring — naive scan.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 10 <= bytes.len() {
        if bytes[i] == b'0' && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
            let candidate = &s[i..i + 10];
            if candidate[2..].chars().all(|c| c.is_ascii_hexdigit()) {
                let lower = candidate.to_ascii_lowercase();
                if let Some(name) = ERROR_SELECTORS.get(&lower) {
                    return name.to_string();
                }
            }
        }
        i += 1;
    }
    if s.len() < 140 {
        s.to_string()
    } else {
        format!("{}...", &s[..137])
    }
}
