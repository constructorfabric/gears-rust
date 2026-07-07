//! Canonical, byte-reproducible framing primitives shared by the gear's
//! tamper-evidence hash encoders (the per-tenant posting chain in
//! [`super::chain`] and the secured-audit chain in [`super::audit_chain`]).
//!
//! Every field is length-prefixed and NULL-safe so two different field
//! boundaries can never collide: a NULL field is a bare [`ABSENT`] byte; a
//! present field is [`PRESENT`] ‖ u32-BE length ‖ bytes. A `None` and an empty
//! string therefore hash differently. SHA-256 via the FIPS-validated
//! `aws-lc-rs` provider — `sha2` is blocked by dylint DE0708.

use aws_lc_rs::digest::{SHA256, digest as sha256};
use uuid::Uuid;

/// NULL-safe presence marker for an absent (NULL) field: a bare byte.
pub(crate) const ABSENT: u8 = 0x00;
/// NULL-safe presence marker for a present field: `PRESENT` ‖ u32-BE len ‖ bytes.
pub(crate) const PRESENT: u8 = 0x01;

pub(crate) fn put(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.push(PRESENT);
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}
pub(crate) fn put_none(buf: &mut Vec<u8>) {
    buf.push(ABSENT);
}
pub(crate) fn put_opt(buf: &mut Vec<u8>, bytes: Option<&[u8]>) {
    match bytes {
        Some(b) => put(buf, b),
        None => put_none(buf),
    }
}
pub(crate) fn put_str(buf: &mut Vec<u8>, s: &str) {
    put(buf, s.as_bytes());
}
pub(crate) fn put_opt_str(buf: &mut Vec<u8>, s: Option<&str>) {
    put_opt(buf, s.map(str::as_bytes));
}
pub(crate) fn put_uuid(buf: &mut Vec<u8>, u: Uuid) {
    put(buf, u.as_bytes());
}
pub(crate) fn put_opt_uuid(buf: &mut Vec<u8>, u: Option<Uuid>) {
    match u {
        Some(u) => put_uuid(buf, u),
        None => put_none(buf),
    }
}
pub(crate) fn put_i64(buf: &mut Vec<u8>, n: i64) {
    put(buf, &n.to_be_bytes());
}
pub(crate) fn put_opt_i64(buf: &mut Vec<u8>, n: Option<i64>) {
    match n {
        Some(n) => put_i64(buf, n),
        None => put_none(buf),
    }
}
pub(crate) fn put_i32(buf: &mut Vec<u8>, n: i32) {
    put(buf, &n.to_be_bytes());
}

pub(crate) fn digest32(buf: &[u8]) -> [u8; 32] {
    let d = sha256(&SHA256, buf);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}
