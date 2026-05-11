//! Pure-Rust HASH98 proof-of-work primitives — the single source of truth for both the OpenCL
//! kernel-argument encoding (used by `gpu.rs`) and CPU re-verification (used by `verify.rs`).
//!
//!     challenge16 = bytes16( keccak256( abi.encodePacked(
//!                       address account, uint256 mintNonce, uint256 chainId,
//!                       address contract, "HASH98" ) ) )
//!     digest32    = SHA-256( challenge16 ‖ nonce16 )
//!     valid       <=>  uint256(digest, big-endian) < 2**(256 - difficulty)
//!
//! Nonce layout (the 16 bytes the contract sees in `mint(bytes16)`): `salt64.be ‖ counter64.be`.

use sha2::{Digest, Sha256};
use tiny_keccak::{Hasher, Keccak};

pub const CHALLENGE_LEN: usize = 16;
pub const NONCE_LEN: usize = 16;

/// The 16-byte challenge as 4 big-endian 32-bit message words W0..W3 (the SHA-256 byte→word map).
pub fn challenge_words(challenge: &[u8; CHALLENGE_LEN]) -> [u32; 4] {
    let mut w = [0u32; 4];
    for i in 0..4 {
        w[i] = u32::from_be_bytes(challenge[4 * i..4 * i + 4].try_into().unwrap());
    }
    w
}

/// `bytes16(keccak256(abi.encodePacked(account, mintNonce, chainId, contract, "HASH98")))`.
/// Cross-checked against a real on-chain `Minted` event.
pub fn compute_challenge(
    account: [u8; 20],
    mint_nonce: u64,
    chain_id: u64,
    contract: [u8; 20],
) -> [u8; CHALLENGE_LEN] {
    let mut k = Keccak::v256();
    k.update(&account);
    k.update(&u256_be_from_u64(mint_nonce));
    k.update(&u256_be_from_u64(chain_id));
    k.update(&contract);
    k.update(b"HASH98");
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    let mut ch = [0u8; CHALLENGE_LEN];
    ch.copy_from_slice(&out[..CHALLENGE_LEN]);
    ch
}

fn u256_be_from_u64(v: u64) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[24..].copy_from_slice(&v.to_be_bytes());
    buf
}

/// `D = required leading-zero bits` → target ceiling `T = 2^(256-D)`. Valid iff `digest < T`.
/// Returned as a 256-bit value packed into 4 big-endian u64 limbs (most-significant first).
pub fn target_from_difficulty(difficulty: u32) -> [u64; 4] {
    assert!(difficulty <= 256, "difficulty out of range");
    if difficulty == 0 {
        // Saturate to 2^256 - 1 (the kernel only carries 256 bits, so all-ones == "always valid").
        return [u64::MAX; 4];
    }
    if difficulty == 256 {
        return [0, 0, 0, 1];
    }
    // T = 1 << (256 - D)
    let shift = 256 - difficulty as usize;
    let mut limbs = [0u64; 4];
    let limb_idx = 3 - (shift / 64); // limbs[3] = LS, limbs[0] = MS
    let bit_in_limb = shift % 64;
    limbs[limb_idx] = 1u64 << bit_in_limb;
    limbs
}

/// `target` as 8 big-endian u32 limbs (most-significant first) — the layout the kernel expects.
pub fn target_limbs_be32(difficulty: u32) -> [u32; 8] {
    let limbs64 = target_from_difficulty(difficulty);
    let mut out = [0u32; 8];
    for i in 0..4 {
        out[2 * i] = (limbs64[i] >> 32) as u32;
        out[2 * i + 1] = (limbs64[i] & 0xffff_ffff) as u32;
    }
    out
}

/// Big-endian `bytes16` representation of a 128-bit nonce.
pub fn nonce_to_bytes16(nonce: u128) -> [u8; NONCE_LEN] {
    nonce.to_be_bytes()
}

/// Split a 128-bit nonce base into `(nb_lo, nb_hi)` little-endian u64 limbs.
pub fn nonce_base_limbs_le(nonce_base: u128) -> (u64, u64) {
    (nonce_base as u64, (nonce_base >> 64) as u64)
}

/// Reassemble a 128-bit nonce from the kernel's 2 LE u64 limbs: `nonce128 = hi64 << 64 | lo64`.
pub fn nonce_from_limbs_le(lo64: u64, hi64: u64) -> u128 {
    ((hi64 as u128) << 64) | (lo64 as u128)
}

/// `sha256(challenge[16] ‖ nonce_be16)`.
pub fn pow_digest(challenge: &[u8; CHALLENGE_LEN], nonce: u128) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(challenge);
    h.update(nonce_to_bytes16(nonce));
    h.finalize().into()
}

/// Number of leading zero bits of a 32-byte digest read big-endian (256 if all-zero).
pub fn leading_zero_bits(digest: &[u8; 32]) -> u32 {
    let mut count = 0u32;
    for &b in digest.iter() {
        if b == 0 {
            count += 8;
        } else {
            count += b.leading_zeros();
            return count;
        }
    }
    count
}

/// True iff `sha256(challenge ‖ nonce)` has `>= difficulty` leading zero bits.
pub fn is_valid_nonce(challenge: &[u8; CHALLENGE_LEN], nonce: u128, difficulty: u32) -> bool {
    let digest = pow_digest(challenge, nonce);
    digest_lt_target(&digest, &target_from_difficulty(difficulty))
}

/// big-endian `uint256(digest) < target` where target is 4 BE u64 limbs (MS first).
pub fn digest_lt_target(digest: &[u8; 32], target: &[u64; 4]) -> bool {
    for i in 0..4 {
        let d = u64::from_be_bytes(digest[8 * i..8 * i + 8].try_into().unwrap());
        if d < target[i] {
            return true;
        }
        if d > target[i] {
            return false;
        }
    }
    false // equal -> not strictly less
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_onchain_digest() {
        // From reference/HASH98_ABI.md (real Minted event).
        let mut acct = [0u8; 20];
        hex::decode_to_slice("1547aa95FAE1E9bE0447b6a5C55B6665E01a7866", &mut acct).unwrap();
        let mut contract = [0u8; 20];
        hex::decode_to_slice("1E5adF70321CA28b3Ead70Eac545E6055E969e6f", &mut contract).unwrap();
        let ch = compute_challenge(acct, 0, 1, contract);
        let mut nz = [0u8; 16];
        hex::decode_to_slice("d8c3ba740000000f000044a20000415d", &mut nz).unwrap();
        let nonce = u128::from_be_bytes(nz);
        let digest = pow_digest(&ch, nonce);
        assert_eq!(
            hex::encode(digest),
            "00000000003c398fa44e2b50c49d0519969c343b40a5da9974fb7ed6dc7e8e56"
        );
    }

    #[test]
    fn target_layout() {
        // D=0 -> all ones
        assert_eq!(target_from_difficulty(0), [u64::MAX; 4]);
        // D=256 -> 1
        assert_eq!(target_from_difficulty(256), [0, 0, 0, 1]);
        // D=8 -> top byte is zero
        let t = target_from_difficulty(8);
        assert_eq!(t[0] >> 56, 0);
        assert_eq!(t[0] >> 48, 1);
    }
}
