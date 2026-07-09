//! Context & memory-management primitives.
//!
//! Phase 7 owns this module. It hosts the reserved-metadata helpers for the
//! per-session [`MemoryStrategy`] (read/write/validate) plus the SDK-aligned
//! flat-tagged-enum wire shape used by `PATCH /sessions/{id}.memory_strategy`.
//!
//! The actual context-construction algorithm (`apply_memory_strategy`) and
//! the overflow-recovery dispatch (`handle_context_overflow`) live in
//! [`crate::domain::service::message_service::MessageService`]. The helpers
//! here are deliberately pure JSON / enum manipulation so they can be reused
//! from both the message-service and any future surface (e.g., Phase 8's
//! intelligence service that re-reads the current strategy after persisting
//! a summary).
//!
//! ## Reserved metadata key
//!
//! The [`MEMORY_STRATEGY_KEY`] constant is the *only* sanctioned writer of
//! `session.metadata["memory_strategy"]` (cross-checked against
//! [`crate::domain::session::METADATA_KEY_MEMORY_STRATEGY`]). Both names
//! collide on purpose — duplicating the string here keeps the context module
//! self-contained while [`debug_assert!`]-equal at runtime guarantees the two
//! constants never drift.
//!
//! ## Wire contract
//!
//! [`MemoryStrategy`] (re-exported from `chat_engine_sdk::models`) serializes
//! as a flat internally-tagged enum:
//!
//! ```json
//! {"type": "full"}
//! {"type": "sliding_window", "window_size": 10}
//! {"type": "summarized", "recent_messages_to_keep": 5}
//! ```
//!
//! The type-specific fields are flattened at the top level next to `"type"`;
//! there is no nested `config` wrapper. See ADR-0017 for the JSONB metadata
//! contract this module piggy-backs on.
//
// @cpt-cf-chat-engine-domain-context:p7
// @cpt-cf-chat-engine-adr-session-metadata:p7
// @cpt-cf-chat-engine-algo-context-management-validate-strategy:p7

use serde_json::{Map, Value};

use crate::domain::error::{ChatEngineError, Result};
use crate::domain::memory_strategy::MemoryStrategy;

pub use chat_engine_sdk::models::Message;

/// Reserved `Session.metadata` key holding the per-session memory strategy.
///
/// This MUST stay in sync with
/// [`crate::domain::session::METADATA_KEY_MEMORY_STRATEGY`]. The duplicate
/// declaration is intentional: it keeps the context module self-contained
/// for callers that only need the strategy helpers without pulling the rest
/// of the session module.
pub const MEMORY_STRATEGY_KEY: &str = "memory_strategy";

/// Read [`MemoryStrategy`] from a `session.metadata` JSON value.
///
/// Returns [`MemoryStrategy::Full`] when:
/// - the metadata is absent / `null`,
/// - the metadata is not a JSON object,
/// - the reserved key is absent,
/// - the stored value fails to decode as a [`MemoryStrategy`] (defensive
///   round-trip — invalid persisted data MUST never block message dispatch;
///   it silently downgrades to the safe default).
///
/// Sibling keys in `metadata` are NOT touched.
#[must_use]
pub fn read_memory_strategy(meta: &Value) -> MemoryStrategy {
    meta.as_object()
        .and_then(|obj| obj.get(MEMORY_STRATEGY_KEY))
        .and_then(|v| serde_json::from_value::<MemoryStrategy>(v.clone()).ok())
        .unwrap_or(MemoryStrategy::Full)
}

/// Write [`MemoryStrategy`] into a `session.metadata` JSON value under the
/// reserved key.
///
/// - Promotes a non-object `meta` (including `Value::Null`) to an empty
///   object before insertion so callers can start from "no metadata" without
///   manual initialisation.
/// - Sibling keys in the existing metadata object are preserved verbatim —
///   only the reserved key is mutated.
/// - The write is atomic at the JSON-value level (single `Map::insert`); no
///   intermediate partial state can be observed by readers of `meta`.
pub fn write_memory_strategy(meta: &mut Value, strategy: &MemoryStrategy) {
    if !meta.is_object() {
        *meta = Value::Object(Map::new());
    }
    // `meta` is an object by the branch above; `MemoryStrategy` is always
    // JSON-serializable. Both fall-throughs are unreachable in practice, so a
    // future invariant break degrades to a no-op write rather than a panic.
    let (Some(obj), Ok(encoded)) = (meta.as_object_mut(), serde_json::to_value(strategy)) else {
        return;
    };
    obj.insert(MEMORY_STRATEGY_KEY.to_string(), encoded);
}

/// Validate a [`MemoryStrategy`] payload per the PRD bounds.
///
/// Returns:
/// - `Ok(())` for [`MemoryStrategy::Full`].
/// - `Err(BadRequest)` for [`MemoryStrategy::SlidingWindow`] when
///   `window_size < 1`.
/// - `Err(BadRequest)` for [`MemoryStrategy::Summarized`] when
///   `recent_messages_to_keep < 2`.
///
/// The "unknown type" branch is enforced at the serde-deserialization layer:
/// the SDK enum's `#[serde(tag = "type", rename_all = "snake_case")]`
/// attribute rejects unknown discriminators with a parse error which the API
/// layer maps to `400 Bad Request` automatically.
pub fn validate_memory_strategy(strategy: &MemoryStrategy) -> Result<()> {
    match strategy {
        MemoryStrategy::Full => Ok(()),
        MemoryStrategy::SlidingWindow { window_size } => {
            if *window_size < 1 {
                Err(ChatEngineError::bad_request(
                    "window_size required and must be >= 1",
                ))
            } else {
                Ok(())
            }
        }
        MemoryStrategy::Summarized {
            recent_messages_to_keep,
        } => {
            if *recent_messages_to_keep < 2 {
                Err(ChatEngineError::bad_request(
                    "recent_messages_to_keep required and must be >= 2",
                ))
            } else {
                Ok(())
            }
        }
    }
}

/// Sentinel substring prefixing the `StreamingErrorEvent.error` payload a
/// plugin emits when its backend rejects the request for context-window
/// reasons. The message-service dispatch loop matches this prefix to route
/// the failure into [`handle_context_overflow`] (Phase 7 + Phase 8).
///
/// Per ADR-0023 the canonical wire shape is:
///
/// ```json
/// {"type": "error", "error": "context_overflow: <plugin detail>"}
/// ```
pub const CONTEXT_OVERFLOW_ERROR_PREFIX: &str = "context_overflow:";

/// Return `true` if a streaming `error` string indicates the plugin's
/// backend rejected the request because the context window was exceeded.
///
/// The matcher is intentionally permissive about the suffix — different
/// plugins phrase the detail differently. Case is preserved (per ADR-0023
/// the prefix is lowercase).
#[must_use]
pub fn is_context_overflow_error(error: &str) -> bool {
    error.starts_with(CONTEXT_OVERFLOW_ERROR_PREFIX)
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod context_tests;
