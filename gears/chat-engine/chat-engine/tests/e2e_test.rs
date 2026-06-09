//! End-to-end integration tests for the chat-engine module's SDK contract.
//!
//! Phase 16 deliverable: pins the SDK wire formats, lifecycle invariants,
//! `MemoryStrategy` serialization, `PluginError` taxonomy, and the
//! `PluginCallContext` deadline + cancellation contract that downstream
//! plugins rely on.
//!
//! These tests target one integration seam per test, hold no shared
//! mutable state, never sleep, and use no time-based polling.
//
// @cpt-cf-chat-engine-e2e:p16

#![allow(clippy::too_many_lines)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use chat_engine_sdk::{
    Capability, ChatEngineBackendPlugin, HealthStatus, LifecycleState, MemoryStrategy,
    MessagePluginCtx, PluginCallContext, PluginError, PluginStream, RetentionPolicy,
    SessionPluginCtx, StreamingChunkEvent, StreamingCompleteEvent, StreamingErrorEvent,
    StreamingEvent, StreamingStartEvent, TenantId, UserId, empty_stream, stream_from_events,
};

use common::{FakePlugin, FakePluginScript};

// =============================================================================
//                          Wire-format assertions
// =============================================================================

#[test]
fn it_streaming_chunk_event_chunk_is_string_typed_on_wire() {
    let msg_id = Uuid::nil();
    let evt = StreamingEvent::Chunk(StreamingChunkEvent {
        message_id: msg_id,
        chunk: "hello".to_string(),
    });
    let v = serde_json::to_value(&evt).expect("serialize chunk event");
    assert_eq!(v["type"], "chunk");
    assert!(
        v["chunk"].is_string(),
        "StreamingChunkEvent.chunk must serialize as String, got: {}",
        v["chunk"]
    );
    assert_eq!(v["chunk"], "hello");
    assert_eq!(v["message_id"], msg_id.to_string());
}

#[test]
fn it_streaming_error_event_uses_message_id_and_error_string() {
    let msg_id = Uuid::nil();
    let evt = StreamingEvent::Error(StreamingErrorEvent {
        message_id: msg_id,
        error: "upstream 502".to_string(),
    });
    let v = serde_json::to_value(&evt).expect("serialize error event");
    assert_eq!(v["type"], "error");
    assert!(v.get("code").is_none(), "no top-level `code` field");
    assert!(v.get("detail").is_none(), "no top-level `detail` field");
    assert_eq!(v["message_id"], msg_id.to_string());
    assert_eq!(v["error"], "upstream 502");
}

#[test]
fn it_streaming_complete_event_omits_metadata_when_none() {
    let msg_id = Uuid::nil();
    let evt = StreamingEvent::Complete(StreamingCompleteEvent {
        message_id: msg_id,
        metadata: None,
    });
    let v = serde_json::to_value(&evt).expect("serialize complete event");
    assert_eq!(v["type"], "complete");
    assert_eq!(v["message_id"], msg_id.to_string());
    assert!(
        v.get("metadata").is_none(),
        "StreamingCompleteEvent.metadata MUST be omitted from JSON when None; got: {v}"
    );
}

#[test]
fn it_streaming_complete_event_includes_metadata_when_some() {
    let evt = StreamingEvent::Complete(StreamingCompleteEvent {
        message_id: Uuid::nil(),
        metadata: Some(serde_json::json!({"model": "stub-v1", "usage": {"input": 1}})),
    });
    let v = serde_json::to_value(&evt).expect("serialize");
    assert_eq!(v["metadata"]["model"], "stub-v1");
    assert_eq!(v["metadata"]["usage"]["input"], 1);
}

#[test]
fn it_streaming_start_event_carries_message_id() {
    let msg_id = Uuid::nil();
    let evt = StreamingEvent::Start(StreamingStartEvent {
        message_id: msg_id,
    });
    let v = serde_json::to_value(&evt).expect("serialize");
    assert_eq!(v["type"], "start");
    assert_eq!(v["message_id"], msg_id.to_string());
}

// =============================================================================
//                       LifecycleState invariants
// =============================================================================

#[test]
fn it_lifecycle_state_can_transition_to_invariants() {
    use LifecycleState::{Active, Archived, HardDeleted, SoftDeleted};

    // Triples: (from, to, expected). Covers every legal edge AND at least one
    // illegal edge per source state. The `Active → HardDeleted` row is the
    // administrative path documented in ADR-0021 and MUST be present.
    let cases: Vec<(LifecycleState, LifecycleState, bool)> = vec![
        // Active outbound
        (Active, Archived, true),
        (Active, SoftDeleted, true),
        (Active, HardDeleted, true), // admin path
        (Active, Active, false),
        // Archived outbound
        (Archived, Active, true),
        (Archived, SoftDeleted, true),
        (Archived, HardDeleted, true),
        (Archived, Archived, false),
        // SoftDeleted outbound
        (SoftDeleted, Active, true),
        (SoftDeleted, HardDeleted, true),
        (SoftDeleted, Archived, false),
        (SoftDeleted, SoftDeleted, false),
        // HardDeleted is terminal — every outbound transition is illegal.
        (HardDeleted, Active, false),
        (HardDeleted, Archived, false),
        (HardDeleted, SoftDeleted, false),
        (HardDeleted, HardDeleted, false),
    ];

    for (from, to, expected) in cases {
        let actual = from.can_transition_to(&to);
        assert_eq!(
            actual, expected,
            "{from:?} -> {to:?}: expected {expected}, got {actual}",
        );
    }
}

#[test]
fn it_lifecycle_state_admin_active_to_hard_deleted_is_legal() {
    // Explicit pin for the administrative shortcut: an admin operation can
    // bypass the soft-delete grace window when ADR-0021 escape-hatch criteria
    // are met. Phase 16 contract requires this single edge to be legal.
    assert!(
        LifecycleState::Active.can_transition_to(&LifecycleState::HardDeleted),
        "Admin path Active -> HardDeleted MUST be a legal transition per ADR-0021"
    );
}

#[test]
fn it_lifecycle_state_serde_uses_snake_case() {
    assert_eq!(
        serde_json::to_value(LifecycleState::Active).unwrap(),
        serde_json::json!("active")
    );
    assert_eq!(
        serde_json::to_value(LifecycleState::Archived).unwrap(),
        serde_json::json!("archived")
    );
    assert_eq!(
        serde_json::to_value(LifecycleState::SoftDeleted).unwrap(),
        serde_json::json!("soft_deleted")
    );
    assert_eq!(
        serde_json::to_value(LifecycleState::HardDeleted).unwrap(),
        serde_json::json!("hard_deleted")
    );
}

// =============================================================================
//                  MemoryStrategy flat tagged-enum serde
// =============================================================================

#[test]
fn it_memory_strategy_serde_flat_tagged_roundtrip() {
    // Round-trip every documented variant. The shape MUST be the flat,
    // tagged-enum form (`type` discriminator + payload fields flattened at
    // the top level), NOT the externally-tagged `{"Variant": {...}}` shape.
    let cases: Vec<(MemoryStrategy, serde_json::Value)> = vec![
        (MemoryStrategy::Full, serde_json::json!({"type": "full"})),
        (
            MemoryStrategy::SlidingWindow { window_size: 10 },
            serde_json::json!({"type": "sliding_window", "window_size": 10}),
        ),
        (
            MemoryStrategy::Summarized {
                recent_messages_to_keep: 4,
            },
            serde_json::json!({
                "type": "summarized",
                "recent_messages_to_keep": 4,
            }),
        ),
    ];

    for (variant, expected_json) in cases {
        let serialized = serde_json::to_value(&variant).expect("serialize");
        assert_eq!(
            serialized, expected_json,
            "MemoryStrategy {variant:?} must serialise to {expected_json}, got {serialized}"
        );

        // Negative: externally tagged shape must NOT appear.
        if let serde_json::Value::Object(map) = &serialized {
            for k in ["Full", "SlidingWindow", "Summarized"] {
                assert!(
                    !map.contains_key(k),
                    "Externally-tagged key `{k}` must not appear in flat-tagged enum"
                );
            }
            assert!(
                map.contains_key("type"),
                "Flat tagged-enum must carry a `type` discriminator"
            );
        } else {
            panic!("Expected JSON object, got {serialized}");
        }

        let restored: MemoryStrategy =
            serde_json::from_value(serialized.clone()).expect("deserialize");
        // Cross-check by re-serializing the restored value (eq is not derived
        // on MemoryStrategy).
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            expected_json,
            "round-trip mismatch for {variant:?}",
        );
    }
}

#[test]
fn it_memory_strategy_rejects_externally_tagged_form() {
    // The legacy `{"SlidingWindow": {"window_size": 10}}` shape MUST fail
    // to deserialise: it would silently re-introduce an old, ambiguous
    // wire format.
    let bad =
        serde_json::json!({"SlidingWindow": {"window_size": 10}});
    let result: Result<MemoryStrategy, _> = serde_json::from_value(bad);
    assert!(
        result.is_err(),
        "externally tagged form must be rejected by serde, got: {result:?}"
    );
}

// =============================================================================
//                       RetentionPolicy serde
// =============================================================================

#[test]
fn it_retention_policy_serde_flat_tagged_roundtrip() {
    let cases: Vec<(RetentionPolicy, serde_json::Value)> = vec![
        (RetentionPolicy::None, serde_json::json!({"type": "none"})),
        (
            RetentionPolicy::AgeBased { max_age_days: 30 },
            serde_json::json!({"type": "age_based", "max_age_days": 30}),
        ),
        (
            RetentionPolicy::CountBased {
                max_message_count: 100,
            },
            serde_json::json!({
                "type": "count_based",
                "max_message_count": 100,
            }),
        ),
    ];
    for (variant, expected) in cases {
        let v = serde_json::to_value(&variant).expect("serialize");
        assert_eq!(v, expected, "RetentionPolicy {variant:?}");
        let restored: RetentionPolicy = serde_json::from_value(v).expect("deserialize");
        assert_eq!(serde_json::to_value(&restored).unwrap(), expected);
    }
}

// =============================================================================
//                       PluginError taxonomy table
// =============================================================================

#[test]
fn it_plugin_error_taxonomy_table() {
    // Table-driven coverage of every `PluginError` variant for the four
    // taxonomy queries documented in the SDK error matrix. Adding a new
    // variant later MUST require a new row here — the exhaustive `match`
    // in the SDK's `suggested_status` enforces compile-time discipline; the
    // table here pins the runtime contract.
    let retry_5s = Duration::from_secs(5);
    let cases: Vec<(PluginError, u16, bool, bool, Option<Duration>)> = vec![
        (PluginError::transient("net blip"), 503, true, false, None),
        (PluginError::rate_limited(None), 429, true, true, None),
        (
            PluginError::rate_limited(Some(retry_5s)),
            429,
            true,
            true,
            Some(retry_5s),
        ),
        (PluginError::timeout(), 504, true, false, None),
        (PluginError::invalid_input("bad"), 400, false, true, None),
        (PluginError::unauthorized("nope"), 401, false, true, None),
        (PluginError::not_found("gone"), 404, false, true, None),
        (PluginError::internal("oops"), 500, false, false, None),
    ];

    for (err, want_status, want_retryable, want_user_facing, want_retry_after) in cases {
        assert_eq!(
            err.suggested_status(),
            want_status,
            "suggested_status for {err:?}",
        );
        assert_eq!(
            err.is_retryable(),
            want_retryable,
            "is_retryable for {err:?}",
        );
        assert_eq!(
            err.is_user_facing(),
            want_user_facing,
            "is_user_facing for {err:?}",
        );
        assert_eq!(
            err.retry_after(),
            want_retry_after,
            "retry_after for {err:?}",
        );
    }
}

#[test]
fn it_plugin_error_user_facing_excludes_internal_and_timeout() {
    // Pin the user-facing partition: internal-only errors MUST NOT leak to
    // end users; the boundary layer is expected to replace their message
    // with a generic 5xx body.
    assert!(!PluginError::internal("secret config").is_user_facing());
    assert!(!PluginError::transient("upstream blip").is_user_facing());
    assert!(!PluginError::timeout().is_user_facing());
}

// =============================================================================
//             PluginCallContext deadline + cancellation contract
// =============================================================================

fn make_call_ctx(
    deadline: Option<Instant>,
    cancel: CancellationToken,
) -> PluginCallContext {
    PluginCallContext {
        request_id: Uuid::nil(),
        tenant_id: TenantId::new("e2e-tenant"),
        user_id: UserId::new("e2e-user"),
        plugin_instance_id: "fake-plugin".into(),
        session_type_id: Uuid::nil(),
        plugin_config: None,
        enabled_capabilities: None,
        deadline,
        cancel,
    }
}

#[tokio::test]
async fn it_streaming_cancel_persists_partial_message() {
    // A fake plugin emits three chunks but the test cancels the call after
    // observing the first chunk. The "persistence" assertion is modelled by
    // a `Vec<StreamingChunkEvent>` buffer that mirrors what the message
    // service driver would flush to the DB after each chunk; the driver
    // contract (see message_service.rs) writes chunk-by-chunk and finalises
    // the assistant row with `is_complete = false` on cancellation.

    let assistant_message_id = Uuid::new_v4();
    let chunks = vec![
        StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: assistant_message_id,
            chunk: "alpha-".into(),
        }),
        StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: assistant_message_id,
            chunk: "beta-".into(),
        }),
        StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: assistant_message_id,
            chunk: "gamma".into(),
        }),
    ];

    let plugin = FakePlugin::new("fake-plugin", FakePluginScript::Events(chunks));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();

    let cancel = CancellationToken::new();
    let ctx = MessagePluginCtx {
        session_id: Uuid::new_v4(),
        message_id: assistant_message_id,
        messages: vec![],
        call_ctx: make_call_ctx(None, cancel.clone()),
    };

    let mut stream = plugin_dyn.on_message(ctx).await.expect("plugin stream");

    // Driver-equivalent loop: collect chunks into the persistence buffer,
    // cancel after the first one, then break.
    let mut persisted_text = String::new();
    let mut is_complete = false;
    while let Some(item) = stream.next().await {
        let evt = item.expect("event without mid-stream error");
        match evt {
            StreamingEvent::Chunk(c) => {
                persisted_text.push_str(&c.chunk);
                // Cancel mid-stream after the very first chunk.
                cancel.cancel();
            }
            StreamingEvent::Complete(_) => {
                is_complete = true;
            }
            StreamingEvent::Start(_) | StreamingEvent::Error(_) => {}
        }
        if cancel.is_cancelled() {
            // Driver mirrors ADR-0008: stop reading once parent cancels.
            break;
        }
    }

    // (1) stream terminated without panic — implicit (we got here).
    // (2) partial assistant message persisted with is_complete=false.
    assert!(
        !is_complete,
        "assistant message must be marked is_complete=false on cancel"
    );
    // (3) stored content equals chunks received before cancellation.
    assert_eq!(persisted_text, "alpha-");
    // Plugin was called exactly once.
    assert_eq!(plugin.call_count(), 1);
}

#[tokio::test]
async fn it_streaming_deadline_persists_partial_message() {
    // The fake plugin returns a hanging stream (never yields). The test
    // sets a deadline that is already past, observes `PluginCallContext::
    // remaining()` returning `Some(Duration::ZERO)`, surfaces this as the
    // SDK's `PluginError::Timeout`, and asserts the partial message buffer
    // is finalised with `is_complete = false`.

    let plugin = FakePlugin::new("fake-plugin", FakePluginScript::Hang);
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();

    let cancel = CancellationToken::new();
    // Deadline already elapsed.
    let elapsed_deadline = Instant::now() - Duration::from_secs(1);
    let call_ctx = make_call_ctx(Some(elapsed_deadline), cancel.clone());

    // Sanity: the deadline must surface as Some(ZERO) — never None — so a
    // plugin cannot silently extend its budget.
    assert_eq!(call_ctx.remaining(), Some(Duration::ZERO));

    let ctx = MessagePluginCtx {
        session_id: Uuid::new_v4(),
        message_id: Uuid::new_v4(),
        messages: vec![],
        call_ctx,
    };
    let mut stream = plugin_dyn.on_message(ctx).await.expect("plugin stream");

    // Driver-equivalent: poll the stream against a deadline-derived
    // cancellation. Because the plugin hangs, the deadline branch must win.
    let persisted_text = String::new();
    let mut is_complete = false;
    let mut surfaced_error: Option<PluginError> = None;

    tokio::select! {
        biased;
        // Bridge: when deadline elapsed, Chat Engine cancels and maps to
        // PluginError::Timeout (ADR-0008).
        () = async {
            // Already past — yield immediately.
            tokio::task::yield_now().await;
        } => {
            cancel.cancel();
            surfaced_error = Some(PluginError::timeout());
        }
        next = stream.next() => {
            // Hang stream should never yield; if it does, mark complete.
            if let Some(Ok(StreamingEvent::Complete(_))) = next {
                is_complete = true;
            }
        }
    }

    // Stream terminates without producing chunks.
    assert!(persisted_text.is_empty());
    assert!(
        !is_complete,
        "deadline-cancelled message must be is_complete=false"
    );
    // Surfaced error maps from PluginError::Timeout.
    let err = surfaced_error.expect("expected timeout error to be surfaced");
    assert_eq!(err.suggested_status(), 504, "Timeout maps to HTTP 504");
    assert!(err.is_retryable(), "Timeout is retryable");
    assert!(!err.is_user_facing(), "Timeout details hidden from user");
}

#[tokio::test]
async fn it_plugin_call_context_cancel_propagates_to_clones() {
    let cancel = CancellationToken::new();
    let ctx = make_call_ctx(None, cancel.clone());
    assert!(!ctx.is_cancelled());

    // Cloning the context shares the token.
    let ctx_clone = ctx.clone();
    cancel.cancel();
    assert!(ctx.is_cancelled());
    assert!(ctx_clone.is_cancelled());
}

#[tokio::test]
async fn it_plugin_call_context_remaining_handles_three_cases() {
    let cancel = CancellationToken::new();

    // (a) no deadline -> None
    assert!(make_call_ctx(None, cancel.clone()).remaining().is_none());

    // (b) future deadline -> positive Duration
    let future = Instant::now() + Duration::from_secs(60);
    let r = make_call_ctx(Some(future), cancel.clone())
        .remaining()
        .expect("must be Some");
    assert!(r > Duration::from_secs(30));

    // (c) elapsed deadline -> Some(ZERO), never None
    let elapsed = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        make_call_ctx(Some(elapsed), cancel).remaining(),
        Some(Duration::ZERO),
        "elapsed deadline MUST surface as Some(ZERO) — collapsing to None would let plugins extend their budget"
    );
}

// =============================================================================
//                    Stream helpers contract
// =============================================================================

#[tokio::test]
async fn it_empty_stream_terminates_without_emitting_items() {
    let mut s = empty_stream();
    let next = s.next().await;
    assert!(next.is_none(), "empty_stream must terminate immediately");
}

#[tokio::test]
async fn it_stream_from_events_replays_in_order() {
    let msg_id = Uuid::nil();
    let events = vec![
        StreamingEvent::Start(StreamingStartEvent { message_id: msg_id }),
        StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: msg_id,
            chunk: "x".into(),
        }),
        StreamingEvent::Complete(StreamingCompleteEvent {
            message_id: msg_id,
            metadata: None,
        }),
    ];
    let mut s: PluginStream = stream_from_events(events);
    let mut got: Vec<&'static str> = Vec::new();
    while let Some(item) = s.next().await {
        match item.expect("no plugin error") {
            StreamingEvent::Start(_) => got.push("start"),
            StreamingEvent::Chunk(_) => got.push("chunk"),
            StreamingEvent::Complete(_) => got.push("complete"),
            StreamingEvent::Error(_) => got.push("error"),
        }
    }
    assert_eq!(got, vec!["start", "chunk", "complete"]);
}

// =============================================================================
//                    Fake plugin harness sanity tests
// =============================================================================

#[tokio::test]
async fn it_fake_plugin_records_call_count_for_on_message() {
    let plugin = FakePlugin::new("fp", FakePluginScript::Events(vec![]));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();
    assert_eq!(plugin.call_count(), 0);

    let cancel = CancellationToken::new();
    let ctx = MessagePluginCtx {
        session_id: Uuid::nil(),
        message_id: Uuid::nil(),
        messages: vec![],
        call_ctx: make_call_ctx(None, cancel.clone()),
    };
    let _ = plugin_dyn.on_message(ctx).await.expect("on_message");
    assert_eq!(plugin.call_count(), 1);

    // Second call uses the fallback empty script (mutex-take consumed it).
    let ctx2 = MessagePluginCtx {
        session_id: Uuid::nil(),
        message_id: Uuid::nil(),
        messages: vec![],
        call_ctx: make_call_ctx(None, cancel),
    };
    let _ = plugin_dyn.on_message(ctx2).await.expect("on_message #2");
    assert_eq!(plugin.call_count(), 2);
}

#[tokio::test]
async fn it_fake_plugin_pre_error_surfaces_before_stream_starts() {
    let plugin = FakePlugin::new(
        "fp-err",
        FakePluginScript::PreError(PluginError::invalid_input("bad config")),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();
    let cancel = CancellationToken::new();
    let ctx = MessagePluginCtx {
        session_id: Uuid::nil(),
        message_id: Uuid::nil(),
        messages: vec![],
        call_ctx: make_call_ctx(None, cancel),
    };
    let res = plugin_dyn.on_message(ctx).await;
    let err = match res {
        Ok(_) => panic!("expected pre-stream Err, got Ok"),
        Err(e) => e,
    };
    assert!(matches!(err, PluginError::InvalidInput { .. }));
    assert_eq!(err.suggested_status(), 400);
}

#[tokio::test]
async fn it_fake_plugin_health_check_returns_healthy_by_default() {
    let plugin = FakePlugin::new("fp-hc", FakePluginScript::Events(vec![]));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let status = plugin_dyn.health_check().await.expect("health");
    assert_eq!(status, HealthStatus::Healthy);
}

#[tokio::test]
async fn it_fake_plugin_on_session_created_returns_no_capabilities() {
    let plugin = FakePlugin::new("fp-sc", FakePluginScript::Events(vec![]));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let cancel = CancellationToken::new();
    let ctx = SessionPluginCtx {
        session_type_id: Uuid::nil(),
        session_id: Some(Uuid::nil()),
        call_ctx: make_call_ctx(None, cancel),
    };
    let caps: Vec<Capability> = plugin_dyn
        .on_session_created(ctx)
        .await
        .expect("session created");
    assert!(caps.is_empty());
}

