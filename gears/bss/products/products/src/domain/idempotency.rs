//! The payload digest a replay key is claimed against (P-D-201).
//!
//! # Parsed, not the received bytes
//!
//! A client that re-serialises its `JSON` on retry — a different key order, a
//! reflowed body, `1` where it first sent `1.0` — is making **the same
//! request**. A digest over the received bytes would answer that retry
//! `IDEMPOTENCY_CONFLICT` instead of replaying its outcome, breaking
//! idempotency exactly where a client needs it. [`payload_digest`] therefore
//! takes a [`JsonValue`] the door has already parsed, never a body slice.
//!
//! # The precondition is not part of what the request *is*
//!
//! `If-Match` is a header, and a client refused `STALE_REVISION` that re-read
//! the head and retried is making the same request with a fresher tag.
//! Nothing in this module can see a header: its whole operand is the value
//! its caller builds out of the parsed body's own fields, which is what makes
//! the exclusion structural rather than a rule a later edit could forget.
//!
//! # One rendering
//!
//! The rendering lives in [`crate::domain::canonical`], not here, so a later
//! door cannot quietly grow a second answer. A member the request omits is
//! omitted from the rendering and an explicit `null` is rendered `null`, so a
//! `PATCH` that omits a field and one that clears it hash **differently**.
//!
//! # The digest primitive
//!
//! [`payload_digest`] is `SHA-256` through `aws-lc-rs`, stored as its 32
//! bytes in `payload_hash` (`bytea` on Postgres, `blob` on `SQLite`).
//! **A later reader can reproduce a digest** without this crate, the digest
//! being a plain `SHA-256` over the rendering with no namespace, no salt and
//! no length prefix: `printf '%s' '<the canonical rendering>' | sha256sum`.
//! `idempotency_tests
//! ::the_digest_is_stable_across_runs_and_reproducible_outside_this_crate`
//! pins one such vector byte for byte.
//!
//! **Changing the primitive or the rendering against live data** strands every
//! key claimed before the change: an in-window retry meets a digest it cannot
//! match and is refused `IDEMPOTENCY_CONFLICT`. Such a change waits out the
//! retention window before the new rule answers a claim.

use serde_json::Value as JsonValue;

use crate::domain::canonical::content_digest;

/// The digest of one parsed request, as `products_idempotency.payload_hash`
/// stores it.
///
/// `payload` is the **fields the request carries**, already parsed: the
/// caller builds a [`JsonValue::Object`] out of its own DTO and omits the
/// fields the request did not send (P-D-201). Two consequences the caller
/// owns rather than this function:
///
/// - Nothing about the transport may enter `payload` — not the precondition,
///   not a correlation id, not a retry counter. Anything that varies between
///   two attempts at the same act turns every honest retry into
///   `IDEMPOTENCY_CONFLICT`.
/// - A DTO whose `Option` field cannot tell an absent key from an explicit
///   `null` renders the two identically. Each door states which of its own
///   fields that applies to.
#[must_use]
pub fn payload_digest(payload: &JsonValue) -> [u8; 32] {
    content_digest(&canonical_rendering(payload))
}

/// The canonical rendering [`payload_digest`] hashes (P-D-201).
///
/// Public because the rendering, not the digest, is what a test, and a later
/// reader reproducing a stored digest by hand, both need to see.
#[must_use]
pub fn canonical_rendering(payload: &JsonValue) -> String {
    crate::domain::canonical::canonical_rendering(payload)
}

#[cfg(test)]
#[path = "idempotency_tests.rs"]
mod idempotency_tests;
