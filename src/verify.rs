//! CPU re-verification of GPU-reported nonces.
//!
//! A GPU hit is *never* trusted blindly — every nonce is re-hashed here with `sha2::Sha256` (the
//! same hash the contract uses) and re-checked against the current difficulty before it can become
//! a transaction.

use crate::pow::{digest_lt_target, leading_zero_bits, pow_digest, target_from_difficulty, CHALLENGE_LEN};

#[derive(Debug, Clone)]
pub struct VerifiedSolution {
    pub challenge: [u8; CHALLENGE_LEN],
    pub nonce: u128,
    pub epoch: u64,
    pub digest: [u8; 32],
    pub difficulty: u32,
}

impl VerifiedSolution {
    pub fn leading_zero_bits(&self) -> u32 {
        leading_zero_bits(&self.digest)
    }

    pub fn nonce_bytes16(&self) -> [u8; 16] {
        self.nonce.to_be_bytes()
    }
}

pub fn verify(
    challenge: &[u8; CHALLENGE_LEN],
    nonce: u128,
    difficulty: u32,
    epoch: u64,
) -> Option<VerifiedSolution> {
    if difficulty > 256 {
        return None;
    }
    let digest = pow_digest(challenge, nonce);
    if digest_lt_target(&digest, &target_from_difficulty(difficulty)) {
        Some(VerifiedSolution {
            challenge: *challenge,
            nonce,
            epoch,
            digest,
            difficulty,
        })
    } else {
        None
    }
}
