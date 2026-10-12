//! The gear's **one** canonical rendering rule, and the `SHA-256` digest
//! taken over it — the request digest a replay key is claimed against
//! (P-D-201).
//!
//! # The rule
//!
//! - `JSON`, object keys **sorted lexicographically by field name** at every
//!   depth, `UTF-8` without `BOM`, and no insignificant whitespace at all.
//! - Integers and decimals as bare decimal strings, no locale and **no
//!   trailing zeroes** — so `1` and `1.0` render identically and hash equal.
//! - A string, a timestamp included, is carried verbatim: nothing here
//!   converts an instant.
//! - A member the request omits is omitted from the rendering, and an
//!   explicit `null` is rendered `null`, so a `PATCH` that omits a field and
//!   one that sends it `null` hash **differently**.
//! - Computed **application-side**, so both engines store identical bytes.
//!
//! **An array is rendered in the order received.** No request on this surface
//! carries a collection whose order is not its meaning; the first one that
//! does owes its sort **here**, rather than at its own call site.
//!
//! # The digest
//!
//! [`content_digest`] is `SHA-256` through `aws-lc-rs`, the platform's
//! `FIPS`-validated provider, reached with the same call the pricing gear
//! uses. `sha2`, `sha1` and `md5` are refused outright by architecture lint
//! `DE0708` (`docs/security/SECURITY.md`), which allow-lists direct imports
//! of those crates and does not reach `aws-lc-rs`.

use aws_lc_rs::digest::{SHA256, digest as sha256};
use serde_json::{Number, Value as JsonValue};

/// The `SHA-256` digest of a canonical rendering, as the 32 raw bytes a
/// `bytea`/`blob` column stores.
///
/// Takes the rendering rather than the value, so that a caller — and a test,
/// and a later reader reproducing a stored digest by hand — can see and pin
/// the exact string that went in. The rendering is the part the design set
/// pins; the digest is a function of it and of nothing else.
///
/// **Only the digest is stored, never the payload**: keeping request bodies
/// beside their digests would put a second, unmanaged copy of what callers
/// sent next to the audit trail that is supposed to be the one place it
/// lives.
///
/// A SHA-256 digest is always 32 bytes, and the type says so (RS-52): a reader of the tag needs no
/// "short digest" branch that cannot happen.
#[must_use]
pub fn content_digest(canonical: &str) -> [u8; 32] {
    let digest = sha256(&SHA256, canonical.as_bytes());
    let mut out = [0_u8; 32];
    for (slot, byte) in out.iter_mut().zip(digest.as_ref()) {
        *slot = *byte;
    }
    out
}

/// Render `value` canonically.
///
/// Public because the rendering, not the digest, is what a reader
/// reproducing a stored digest by hand needs to see.
#[must_use]
pub fn canonical_rendering(value: &JsonValue) -> String {
    let mut rendered = String::new();
    render_into(value, &mut rendered);
    rendered
}

/// Append `value`'s canonical rendering to `out`.
///
/// Recursive rather than iterative for the reason the shape is recursive:
/// a value nests, and the sort applies at every object level, not only the
/// outermost one.
fn render_into(value: &JsonValue, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        JsonValue::Number(number) => out.push_str(&render_number(number)),
        JsonValue::String(text) => out.push_str(&render_string(text)),
        JsonValue::Array(items) => {
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                render_into(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(map) => {
            // Sorted here rather than trusted from the map: `serde_json`'s
            // own ordering depends on whether its `preserve_order` feature
            // is on anywhere in the graph, and a digest that changed with a
            // feature unification elsewhere in the workspace would be the
            // opposite of canonical.
            let mut entries: Vec<(&String, &JsonValue)> = map.iter().collect();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            out.push('{');
            for (position, (key, entry)) in entries.into_iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                out.push_str(&render_string(key));
                out.push(':');
                render_into(entry, out);
            }
            out.push('}');
        }
    }
}

/// One string, escaped the way `JSON` escapes strings.
///
/// Reached through [`JsonValue`]'s own infallible `Display` rather than
/// `serde_json::to_string`'s `Result`: escaping a string cannot fail, and a
/// fallible call here would need a fallback branch that could only ever
/// render something *other* than the canonical form.
fn render_string(text: &str) -> String {
    JsonValue::String(text.to_owned()).to_string()
}

/// One number, as a bare decimal string with no trailing zeroes.
///
/// Integers render exactly. A fractional value renders through `f64`'s own
/// shortest-round-trip `Display`, which prints `1.0` as `1` — so a client
/// that sent `1` on its first attempt and `1.0` on its retry hashes the same,
/// which is the whole point of hashing a *parsed* request.
///
/// # The two costs, measured
///
/// An earlier revision of this doc named a third that does not exist —
/// *"a magnitude large or small enough to make `f64`'s `Display` reach
/// exponent form renders in that form"*. Rust's `Display` for floats **never**
/// emits exponent notation; only `{:e}` does. `format!("{}", 1e300f64)` prints
/// a 301-digit integer string, and `format!("{}", 1e-9f64)` prints
/// `0.000000001`. The claim was withdrawn rather than repaired, because the
/// two real costs are both stronger than it was:
///
/// - **A large magnitude renders as a several-hundred-digit integer string.**
///   `1e300` is 301 characters of `JSON` inside the rendering this module
///   hashes, and inside anything a reader reproduces the digest from by hand.
///   Nothing here bounds that length.
/// - **Two distinct `JSON` literals differing below `f64` precision render
///   identically, and therefore hash equal.** `0.1 + 0.2` renders
///   `0.30000000000000004`, and every literal that parses to the same `f64`
///   renders to that same string. At the idempotency door this means two
///   requests that a client considers different are one request, and the
///   second is answered by replaying the first's outcome.
///
/// The second is the one that matters: the first is ugly, the second is a
/// wrong answer. No request field on this surface is numeric today; the first
/// door that adds one owes a decimal-string operand rather than a float, which
/// is this module's own rule ("integers and decimals as bare decimal strings") read
/// strictly, and which neither cost can reach.
fn render_number(number: &Number) -> String {
    if let Some(value) = number.as_i64() {
        return value.to_string();
    }
    if let Some(value) = number.as_u64() {
        return value.to_string();
    }
    match number.as_f64() {
        Some(value) => format!("{value}"),
        None => number.to_string(),
    }
}

#[cfg(test)]
#[path = "canonical_tests.rs"]
mod canonical_tests;
