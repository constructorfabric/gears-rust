//! Canonical, byte-reproducible framing primitives shared by the gear's
//! tamper-evidence hash encoders (the per-tenant posting chain in
//! [`super::chain`] and the secured-audit chain in [`super::audit_chain`]).
//!
//! Every field is length-prefixed and NULL-safe so two different field
//! boundaries can never collide: a NULL field is a bare [`ABSENT`] byte; a
//! present field is [`PRESENT`] ‖ u32-BE length ‖ bytes. A `None` and an empty
//! string therefore hash differently. SHA-256 routes through the toolkit-security
//! facade and its selected platform/FIPS backend.

use toolkit_security::crypto::hash::Sha256;
use uuid::Uuid;

/// NULL-safe presence marker for an absent (NULL) field: a bare byte.
pub(crate) const ABSENT: u8 = 0x00;
/// NULL-safe presence marker for a present field: `PRESENT` ‖ u32-BE len ‖ bytes.
pub(crate) const PRESENT: u8 = 0x01;

/// Appends a present field: [`PRESENT`] ‖ u32-BE length (saturating at `u32::MAX`) ‖ bytes.
pub(crate) fn put(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.push(PRESENT);
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}
/// Appends a bare [`ABSENT`] byte marking a NULL field.
pub(crate) fn put_none(buf: &mut Vec<u8>) {
    buf.push(ABSENT);
}
/// Appends an optional field: framed bytes when `Some`, a bare [`ABSENT`] byte when `None`.
pub(crate) fn put_opt(buf: &mut Vec<u8>, bytes: Option<&[u8]>) {
    match bytes {
        Some(b) => put(buf, b),
        None => put_none(buf),
    }
}
/// Appends a present string field as its UTF-8 bytes.
pub(crate) fn put_str(buf: &mut Vec<u8>, s: &str) {
    put(buf, s.as_bytes());
}
/// Appends an optional string field: UTF-8 bytes when `Some`, a bare [`ABSENT`] byte when `None`.
pub(crate) fn put_opt_str(buf: &mut Vec<u8>, s: Option<&str>) {
    put_opt(buf, s.map(str::as_bytes));
}
/// Appends a present UUID field as its 16 raw bytes.
pub(crate) fn put_uuid(buf: &mut Vec<u8>, u: Uuid) {
    put(buf, u.as_bytes());
}
/// Appends an optional UUID field: encoded when `Some`, a bare [`ABSENT`] byte when `None`.
pub(crate) fn put_opt_uuid(buf: &mut Vec<u8>, u: Option<Uuid>) {
    match u {
        Some(u) => put_uuid(buf, u),
        None => put_none(buf),
    }
}
/// Appends a present `i64` field as its 8-byte big-endian encoding.
pub(crate) fn put_i64(buf: &mut Vec<u8>, n: i64) {
    put(buf, &n.to_be_bytes());
}
/// Appends a present `i32` field as its 4-byte big-endian encoding.
pub(crate) fn put_i32(buf: &mut Vec<u8>, n: i32) {
    put(buf, &n.to_be_bytes());
}

/// SHA-256 digest of the framed buffer, as a fixed 32-byte array.
pub(crate) fn digest32(buf: &[u8]) -> [u8; 32] {
    let d = Sha256::digest(buf);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

/// [`digest32`] rendered as lowercase hexadecimal.
pub(crate) fn digest32_hex(buf: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for b in digest32(buf) {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Frame canonical major-unit text with both stored currency dimensions.
pub(crate) fn put_money(buf: &mut Vec<u8>, money: &bss_ledger_sdk::PostedMoney) {
    put_str(buf, &bss_ledger_sdk::canonical_decimal(money.amount()));
    put_str(buf, money.currency().code());
    put(buf, &[money.currency().scale()]);
}
/// Optional money keeps an absent triple distinct from every present value.
pub(crate) fn put_opt_money(buf: &mut Vec<u8>, money: Option<&bss_ledger_sdk::PostedMoney>) {
    if let Some(money) = money {
        put_money(buf, money);
    } else {
        put_none(buf);
        put_none(buf);
        put_none(buf);
    }
}
