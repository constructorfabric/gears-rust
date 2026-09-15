// Created: 2026-08-12 by Constructor Tech
//! Precondition-violation types this gear emits.
//!
//! Kept here rather than in the SDK on purpose. ADR 0005 drives the SDK's
//! projected vocabulary by **consumer dispatch**, and a lost `If-Match` on an
//! administrative write is not something a settings *consumer* ever branches on
//! — it belongs to the admin request path. The SDK's
//! `precondition::SETTING_RETIRED` is projected because a reader must stop
//! retrying; this one has no such consumer.

/// A conditional write whose `If-Match` no longer matches current state.
pub const ETAG_MISMATCH: &str = "ETAG_MISMATCH";
