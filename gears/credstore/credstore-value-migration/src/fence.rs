//! The shipped value-fingerprint fence, reduced to what the migration needs.
//!
//! Mirrors `domain/secret/fence.rs` of the shipped gear: the fence key is
//! stored raw (32 bytes) in the legacy store under [`FENCE_KEY_REF`] (nil
//! tenant, tenant key class), and the fingerprint is
//! `HMAC-SHA256(fence_key, value)` compared in constant time.

use aws_lc_rs::hmac;

/// Reserved legacy reference of the fence key (nil tenant, owner `None`).
pub const FENCE_KEY_REF: &str = "cfs-internal-fence-key";

/// The only `fp_key_id` the shipped gear ever wrote.
pub const CURRENT_FENCE_KEY_ID: i16 = 1;

/// Constant-time check of `fp == HMAC-SHA256(key, value)`.
#[must_use]
pub fn verify_fp(key: &[u8], value: &[u8], fp: &[u8]) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::verify(&key, value, fp).is_ok()
}

/// `HMAC-SHA256(key, value)`; exposed for tests and fixtures.
#[must_use]
pub fn compute_fp(key: &[u8], value: &[u8]) -> Vec<u8> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&key, value).as_ref().to_vec()
}
