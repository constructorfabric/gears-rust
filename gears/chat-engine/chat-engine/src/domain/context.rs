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
    let obj = meta
        .as_object_mut()
        .expect("metadata coerced to object above");
    let encoded = serde_json::to_value(strategy)
        .expect("MemoryStrategy is always serializable to JSON");
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
mod tests {
    use super::*;
    use serde_json::json;

    // ---- Flat tagged-enum serde round-trip ----------------------------

    #[test]
    fn memory_strategy_serializes_full_as_flat_type() {
        let s = MemoryStrategy::Full;
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v, json!({"type": "full"}));
    }

    #[test]
    fn memory_strategy_serializes_sliding_window_with_flat_field() {
        let s = MemoryStrategy::SlidingWindow { window_size: 10 };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v, json!({"type": "sliding_window", "window_size": 10}));
    }

    #[test]
    fn memory_strategy_serializes_summarized_with_flat_field() {
        let s = MemoryStrategy::Summarized {
            recent_messages_to_keep: 5,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v,
            json!({"type": "summarized", "recent_messages_to_keep": 5})
        );
    }

    #[test]
    fn memory_strategy_deserializes_three_flat_shapes() {
        let full: MemoryStrategy = serde_json::from_value(json!({"type": "full"})).unwrap();
        assert!(matches!(full, MemoryStrategy::Full));

        let sliding: MemoryStrategy =
            serde_json::from_value(json!({"type": "sliding_window", "window_size": 3}))
                .unwrap();
        assert!(matches!(
            sliding,
            MemoryStrategy::SlidingWindow { window_size: 3 }
        ));

        let summarized: MemoryStrategy = serde_json::from_value(
            json!({"type": "summarized", "recent_messages_to_keep": 4}),
        )
        .unwrap();
        assert!(matches!(
            summarized,
            MemoryStrategy::Summarized {
                recent_messages_to_keep: 4
            }
        ));
    }

    #[test]
    fn memory_strategy_rejects_nested_config_wrapper() {
        // The PRD forbids `{ "type": "sliding_window", "config": { ... } }`.
        // serde with `#[serde(tag = "type")]` requires the field flattened.
        let bad = json!({"type": "sliding_window", "config": {"window_size": 10}});
        let res: std::result::Result<MemoryStrategy, _> = serde_json::from_value(bad);
        assert!(
            res.is_err(),
            "nested-config shape must be rejected by serde"
        );
    }

    #[test]
    fn memory_strategy_rejects_unknown_type() {
        let bad = json!({"type": "no_such_strategy"});
        let res: std::result::Result<MemoryStrategy, _> = serde_json::from_value(bad);
        assert!(res.is_err(), "unknown type must surface as serde error");
    }

    // ---- Read / write helpers ----------------------------------------

    #[test]
    fn read_returns_full_when_metadata_is_null() {
        let meta = Value::Null;
        assert!(matches!(read_memory_strategy(&meta), MemoryStrategy::Full));
    }

    #[test]
    fn read_returns_full_when_metadata_is_empty_object() {
        let meta = json!({});
        assert!(matches!(read_memory_strategy(&meta), MemoryStrategy::Full));
    }

    #[test]
    fn read_returns_full_when_key_absent() {
        let meta = json!({"other": "value"});
        assert!(matches!(read_memory_strategy(&meta), MemoryStrategy::Full));
    }

    #[test]
    fn read_returns_full_when_stored_value_is_garbage() {
        // Defensive fall-through: a corrupted persisted value MUST NOT block
        // message dispatch. We silently downgrade to Full.
        let meta = json!({"memory_strategy": {"type": "no_such"}});
        assert!(matches!(read_memory_strategy(&meta), MemoryStrategy::Full));
    }

    #[test]
    fn read_round_trip_three_shapes() {
        let cases = [
            MemoryStrategy::Full,
            MemoryStrategy::SlidingWindow { window_size: 7 },
            MemoryStrategy::Summarized {
                recent_messages_to_keep: 3,
            },
        ];
        for s in cases {
            let mut meta = Value::Null;
            write_memory_strategy(&mut meta, &s);
            match (&s, read_memory_strategy(&meta)) {
                (MemoryStrategy::Full, MemoryStrategy::Full) => {}
                (
                    MemoryStrategy::SlidingWindow { window_size: a },
                    MemoryStrategy::SlidingWindow { window_size: b },
                ) if a == &b => {}
                (
                    MemoryStrategy::Summarized {
                        recent_messages_to_keep: a,
                    },
                    MemoryStrategy::Summarized {
                        recent_messages_to_keep: b,
                    },
                ) if a == &b => {}
                (lhs, rhs) => panic!("round-trip mismatch: {lhs:?} vs {rhs:?}"),
            }
        }
    }

    #[test]
    fn write_preserves_sibling_keys() {
        let mut meta = json!({
            "title": "session-1",
            "tags": ["alpha", "beta"],
            "retention_policy": {"type": "none"},
        });
        write_memory_strategy(
            &mut meta,
            &MemoryStrategy::SlidingWindow { window_size: 2 },
        );
        let obj = meta.as_object().unwrap();
        assert_eq!(obj.get("title"), Some(&Value::String("session-1".into())));
        assert!(obj.contains_key("tags"));
        assert!(obj.contains_key("retention_policy"));
        assert_eq!(
            obj.get(MEMORY_STRATEGY_KEY),
            Some(&json!({"type": "sliding_window", "window_size": 2}))
        );
    }

    #[test]
    fn write_replaces_existing_strategy_atomically() {
        let mut meta = json!({"memory_strategy": {"type": "full"}});
        write_memory_strategy(
            &mut meta,
            &MemoryStrategy::Summarized {
                recent_messages_to_keep: 4,
            },
        );
        assert_eq!(
            meta.get(MEMORY_STRATEGY_KEY).unwrap(),
            &json!({"type": "summarized", "recent_messages_to_keep": 4})
        );
        // No leftover partial state from the previous shape.
        let obj = meta.as_object().unwrap();
        assert_eq!(obj.len(), 1);
    }

    #[test]
    fn write_promotes_null_metadata_to_object() {
        let mut meta = Value::Null;
        write_memory_strategy(&mut meta, &MemoryStrategy::Full);
        assert!(meta.is_object());
    }

    // ---- Validation --------------------------------------------------

    #[test]
    fn validate_full_passes() {
        validate_memory_strategy(&MemoryStrategy::Full).expect("Full is always valid");
    }

    #[test]
    fn validate_sliding_window_accepts_positive() {
        validate_memory_strategy(&MemoryStrategy::SlidingWindow { window_size: 1 })
            .expect("window_size=1 must be accepted");
        validate_memory_strategy(&MemoryStrategy::SlidingWindow { window_size: 50 })
            .expect("window_size=50 must be accepted");
    }

    #[test]
    fn validate_sliding_window_rejects_zero() {
        let err = validate_memory_strategy(&MemoryStrategy::SlidingWindow { window_size: 0 })
            .unwrap_err();
        match err {
            ChatEngineError::BadRequest { reason } => {
                assert!(reason.contains("window_size"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn validate_summarized_accepts_two_or_more() {
        validate_memory_strategy(&MemoryStrategy::Summarized {
            recent_messages_to_keep: 2,
        })
        .expect("recent_messages_to_keep=2 must be accepted");
        validate_memory_strategy(&MemoryStrategy::Summarized {
            recent_messages_to_keep: 100,
        })
        .expect("recent_messages_to_keep=100 must be accepted");
    }

    #[test]
    fn validate_summarized_rejects_below_two() {
        for k in [0u32, 1u32] {
            let err = validate_memory_strategy(&MemoryStrategy::Summarized {
                recent_messages_to_keep: k,
            })
            .unwrap_err();
            match err {
                ChatEngineError::BadRequest { reason } => {
                    assert!(reason.contains("recent_messages_to_keep"));
                }
                other => panic!("expected BadRequest, got {other:?}"),
            }
        }
    }

    // ---- Overflow-error matcher --------------------------------------

    #[test]
    fn is_context_overflow_recognises_prefix() {
        assert!(is_context_overflow_error(
            "context_overflow: prompt exceeds 8k tokens"
        ));
        assert!(is_context_overflow_error("context_overflow:"));
    }

    #[test]
    fn is_context_overflow_ignores_non_overflow_errors() {
        assert!(!is_context_overflow_error("timeout: 30s elapsed"));
        assert!(!is_context_overflow_error(""));
        assert!(!is_context_overflow_error("Context_overflow: case-sensitive"));
    }

    // ---- Reserved-key constant consistency ---------------------------

    #[test]
    fn reserved_key_matches_session_module() {
        // Cross-check the duplicated constant agrees with the session
        // module so the two never drift.
        assert_eq!(
            MEMORY_STRATEGY_KEY,
            crate::domain::session::METADATA_KEY_MEMORY_STRATEGY
        );
    }
}
