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
        serde_json::from_value(json!({"type": "sliding_window", "window_size": 3})).unwrap();
    assert!(matches!(
        sliding,
        MemoryStrategy::SlidingWindow { window_size: 3 }
    ));

    let summarized: MemoryStrategy =
        serde_json::from_value(json!({"type": "summarized", "recent_messages_to_keep": 4}))
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
    write_memory_strategy(&mut meta, &MemoryStrategy::SlidingWindow { window_size: 2 });
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
    let err =
        validate_memory_strategy(&MemoryStrategy::SlidingWindow { window_size: 0 }).unwrap_err();
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
    assert!(!is_context_overflow_error(
        "Context_overflow: case-sensitive"
    ));
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
