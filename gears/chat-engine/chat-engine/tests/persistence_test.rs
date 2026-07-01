//! Persistence-side integration tests for `MessageService`.
//!
//! These tests construct a real `MessageService` over a `SeaORM`
//! SQLite-in-memory stack (every production repo impl, the real
//! variant-index allocation transaction, the real `finalize_assistant`
//! UPDATE) and drive it with a scripted plugin so the finalize-on-cancel
//! and finalize-on-pre-stream-timeout paths are exercised end-to-end.
//!
//! The earlier `e2e_test.rs::it_streaming_cancel_persists_partial_message`
//! test mirrored the driver loop in the test body — it never touched the
//! DB, so a regression in `MessageService` / `MessageRepo` could not fail
//! it. The two tests below close that gap: they construct a session row,
//! call `send_message`, cancel mid-stream, and assert on the persisted
//! `messages` row's `is_complete`, `content`, and `metadata` columns.
//
// @cpt-cf-chat-engine-message-service:p5
// @cpt-cf-chat-engine-message-repo:p5

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use chat_engine::domain::service::message_service::{MessageService, SendMessageRequest};
use chat_engine::domain::service::plugin_service::PluginService;
use chat_engine::domain::service::session_service::Identity;
use chat_engine::infra::db::repo::stream_event_repo::{SeaStreamEventBuffer, StreamEventBuffer};
use chat_engine_sdk::models::{FileCitation, MessagePartInput, MessagePartType};
use chat_engine_sdk::{
    ChatEngineBackendPlugin, PluginError, StreamingChunkEvent, StreamingCompleteEvent,
    StreamingEvent, StreamingPartEvent, StreamingStateEvent, StreamingToolEvent,
};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use uuid::Uuid;

use common::db::{self, DbHarness};
use common::{FakePlugin, FakePluginScript};

const TENANT_ID: &str = "tenant-it";
const USER_ID: &str = "user-it";

/// Build a `MessageService` bound to `harness` with `plugin` registered
/// against `plugin_instance_id`. Mirrors the production wiring in
/// `chat_engine::module::ChatEngineModule::init` minus the optional
/// collaborators (`with_webhook_emitter`, `with_plugin_deadline`, …).
fn build_service(
    harness: &DbHarness,
    plugin_instance_id: &str,
    plugin: Arc<dyn ChatEngineBackendPlugin>,
) -> MessageService {
    let hub = Arc::new(ClientHub::new());
    hub.register_scoped::<dyn ChatEngineBackendPlugin>(
        ClientScope::gts_id(plugin_instance_id),
        plugin,
    );
    let plugins = PluginService::new(hub, Arc::clone(&harness.plugin_configs));

    MessageService::new(
        Arc::clone(&harness.sessions),
        Arc::clone(&harness.session_types),
        Arc::clone(&harness.messages),
        plugins,
    )
}

fn make_request(session_id: Uuid) -> SendMessageRequest {
    SendMessageRequest {
        session_id,
        parts: vec![MessagePartInput {
            part_type: MessagePartType::Text,
            content: serde_json::json!({"text": "hello"}),
            file_citations: vec![],
            link_citations: vec![],
            references: vec![],
        }],
        file_ids: vec![],
        parent_message_id: None,
        capabilities: None,
    }
}

fn make_identity() -> Identity {
    Identity::new(TENANT_ID, USER_ID, None).unwrap()
}

// ===========================================================================
// 1. Cancel mid-stream — partial chunks must persist with is_complete=false
// ===========================================================================

#[tokio::test]
async fn cancel_after_partial_chunks_persists_is_complete_false_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "cancel-persists-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    // Plugin emits two chunks under the SDK's placeholder message_id (the
    // driver re-stamps each chunk with the assistant id before forwarding)
    // then stalls on a never-ready future. The driver pumps both chunks
    // into the mpsc sink and then waits for either the plugin to advance
    // or the parent cancel token to fire.
    let placeholder = Uuid::nil();
    let plugin = FakePlugin::new(
        plugin_id,
        FakePluginScript::EventsThenHang(vec![
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: placeholder,
                chunk: "alpha-".into(),
            }),
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: placeholder,
                chunk: "beta-".into(),
            }),
        ]),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin.clone();
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let mut stream = svc
        .send_message(make_request(session_id), make_identity(), cancel.clone())
        .await
        .expect("send_message dispatch");

    // Drive the stream: read Start + both chunks, accumulate the wire text
    // so we can cross-check it against the persisted column later.
    let mut wire_text = String::new();
    let mut chunks_seen = 0;
    let mut start_seen = false;
    while let Some(evt) = stream.next().await {
        match evt {
            StreamingEvent::Start(_) => start_seen = true,
            StreamingEvent::Chunk(c) => {
                wire_text.push_str(&c.chunk);
                chunks_seen += 1;
                if chunks_seen == 2 {
                    // Both chunks observed — issue the cancel and exit the
                    // read loop. The driver's biased select! will pick up
                    // `cancel.cancelled()` on the next iteration and
                    // finalize the row.
                    cancel.cancel();
                    break;
                }
            }
            StreamingEvent::Complete(_) | StreamingEvent::Error(_) => {
                panic!("driver should not have produced Complete/Error before cancel")
            }
            _ => {}
        }
    }
    assert!(start_seen, "driver must emit Start before any chunk");
    assert_eq!(chunks_seen, 2, "wire stream lost a chunk");
    assert_eq!(wire_text, "alpha-beta-", "wire payload was tampered with");

    // Assistant row finalised by the detached driver task. Poll until the
    // row has a non-NULL metadata column (the finalize UPDATE writes
    // metadata atomically with content + is_complete).
    let row = db::wait_for_finalize(&harness.db, session_id, Duration::from_secs(2)).await;
    assert!(
        !row.is_complete,
        "cancelled assistant row MUST be is_complete=false; persisted row = {row:?}"
    );
    assert_eq!(
        db::message_text(&harness.db, row.message_id).await,
        "alpha-beta-",
        "persisted partial content must equal the chunks emitted before cancel",
    );
    let metadata = row
        .metadata
        .as_ref()
        .expect("finalize_assistant must write metadata on cancel");
    assert_eq!(
        metadata
            .get("cancelled")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "cancel finalize must stamp metadata.cancelled=true; got {metadata}",
    );
    assert_eq!(
        metadata.get("partial").and_then(serde_json::Value::as_bool),
        Some(true),
        "cancel finalize must stamp metadata.partial=true; got {metadata}",
    );
    // Plugin was invoked exactly once per request.
    assert_eq!(plugin.call_count(), 1);
}

// ===========================================================================
// 1a. Multi-part body — a message sent with several ordered typed parts
//     persists them into message_parts and reads them back in order (FR-022).
// ===========================================================================

#[tokio::test]
async fn multi_part_user_message_round_trips_in_order_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "multi-part-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    let plugin = FakePlugin::new(plugin_id, FakePluginScript::Events(vec![]));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    // text → code → links, in that order.
    let req = SendMessageRequest {
        session_id,
        parts: vec![
            MessagePartInput {
                part_type: MessagePartType::Text,
                content: serde_json::json!({"text": "look at this"}),
                file_citations: vec![],
                link_citations: vec![],
                references: vec![],
            },
            MessagePartInput {
                part_type: MessagePartType::Code,
                content: serde_json::json!({"language": "rust", "code": "fn main() {}"}),
                file_citations: vec![],
                link_citations: vec![],
                references: vec![],
            },
            MessagePartInput {
                part_type: MessagePartType::Links,
                content: serde_json::json!({"links": [{"url": "https://example.com"}]}),
                file_citations: vec![],
                link_citations: vec![],
                references: vec![],
            },
        ],
        file_ids: vec![],
        parent_message_id: None,
        capabilities: None,
    };

    let cancel = CancellationToken::new();
    let mut stream = svc
        .send_message(req, make_identity(), cancel)
        .await
        .expect("send_message dispatch");
    while stream.next().await.is_some() {}

    // Read the user message's parts directly, ordered by `number`.
    let parts = db::message_parts_ordered(&harness.db, session_id, "user").await;
    let types: Vec<&str> = parts.iter().map(|(t, _, _)| t.as_str()).collect();
    assert_eq!(
        types,
        vec!["text", "code", "links"],
        "parts must persist in submitted order",
    );
    let numbers: Vec<i32> = parts.iter().map(|(_, n, _)| *n).collect();
    assert_eq!(
        numbers,
        vec![0, 1, 2],
        "part numbers must be 0-based and contiguous"
    );
    assert_eq!(
        parts[1].2.get("language").and_then(|v| v.as_str()),
        Some("rust"),
        "code part content must round-trip verbatim",
    );
}

// ===========================================================================
// 1c. Citations — a plugin's terminal Complete event carries a file citation;
//     it is persisted against the assistant's text part and read back (FR-023).
// ===========================================================================

#[tokio::test]
async fn assistant_file_citation_persists_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "citation-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    let cite: FileCitation = serde_json::from_value(serde_json::json!({
        "document_id": "doc-1",
        "document_name": "Doc One",
        "index": 1,
        "quote": "the answer is 42",
        "text_positions": [7],
    }))
    .expect("build file citation");

    let plugin = FakePlugin::new(
        plugin_id,
        FakePluginScript::Events(vec![
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: Uuid::nil(),
                chunk: "answer".into(),
            }),
            StreamingEvent::Complete(StreamingCompleteEvent {
                message_id: Uuid::nil(),
                metadata: None,
                file_citations: vec![cite],
                link_citations: vec![],
                references: vec![],
            }),
        ]),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let mut stream = svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
        .expect("send_message dispatch");
    while stream.next().await.is_some() {}

    let assistant = db::find_assistant_message(&harness.db, session_id)
        .await
        .expect("assistant row persisted");
    let cites = db::file_citations_for_message(&harness.db, assistant.message_id).await;
    assert_eq!(cites.len(), 1, "one file citation must be persisted");
    assert_eq!(cites[0]["document_id"], "doc-1");
    assert_eq!(cites[0]["index"], 1);
    assert_eq!(
        cites[0]["text_positions"],
        serde_json::json!([7]),
        "text_positions must round-trip verbatim",
    );
}

// ===========================================================================
// 1b. Author/tenant stamping — the persisted user message carries the JWT
//     tenant + author; the assistant stub inherits the tenant but has no
//     human author. Exercises the write-path threading end-to-end.
// ===========================================================================

#[tokio::test]
async fn send_message_stamps_tenant_and_author_against_sqlite() {
    use chat_engine::infra::db::entity::message;

    let harness = db::setup_sqlite().await;
    let plugin_id = "tenant-stamp-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    // A script that closes cleanly so both the user message and the
    // finalized assistant row land on disk.
    let plugin = FakePlugin::new(
        plugin_id,
        FakePluginScript::Events(vec![StreamingEvent::Chunk(StreamingChunkEvent {
            message_id: Uuid::nil(),
            chunk: "ok".into(),
        })]),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let mut stream = svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
        .expect("send_message dispatch");
    while stream.next().await.is_some() {}

    let rows = db::list_messages(&harness.db, session_id).await;
    let user = rows
        .iter()
        .find(|m| matches!(m.role, message::MessageRole::User))
        .expect("user message persisted");
    assert_eq!(
        user.tenant_id.as_deref(),
        Some(TENANT_ID),
        "user message must inherit the JWT tenant",
    );
    assert_eq!(
        user.user_id.as_deref(),
        Some(USER_ID),
        "user message must record its JWT author",
    );

    let assistant = rows
        .iter()
        .find(|m| matches!(m.role, message::MessageRole::Assistant))
        .expect("assistant stub persisted");
    assert_eq!(
        assistant.tenant_id.as_deref(),
        Some(TENANT_ID),
        "assistant message must inherit the owning tenant",
    );
    assert_eq!(
        assistant.user_id, None,
        "assistant message has no human author",
    );
}

// ===========================================================================
// 2. Pre-stream timeout — the assistant stub must be finalised with
//    finish_reason=timeout, is_complete=false, against the real DB
// ===========================================================================

#[tokio::test]
async fn pre_stream_timeout_persists_finish_reason_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "pre-stream-timeout-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    // Plugin rejects the request before the stream starts — the canonical
    // upstream-timeout shape from the SDK error taxonomy.
    let plugin = FakePlugin::new(
        plugin_id,
        FakePluginScript::PreError(PluginError::timeout()),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let Err(err) = svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
    else {
        panic!("pre-stream timeout must surface as Err");
    };
    // ChatEngineError::BackendUnavailable carries the HTTP-504 mapping per
    // the production error matrix; we only need the discriminant here.
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("BackendUnavailable"),
        "expected BackendUnavailable, got {dbg}",
    );

    // The pre-stream failure path is awaited (no driver task), but
    // finalize_assistant is called inline before send_message returns —
    // by the time we get here the row is on disk.
    let row = db::find_assistant_message(&harness.db, session_id)
        .await
        .expect("pre-stream timeout MUST still have inserted the assistant stub");
    assert!(
        !row.is_complete,
        "pre-stream timeout row MUST be is_complete=false; row = {row:?}",
    );
    assert_eq!(
        db::message_text(&harness.db, row.message_id).await,
        "",
        "pre-stream timeout must persist empty content (no chunks observed)",
    );
    let metadata = row
        .metadata
        .as_ref()
        .expect("pre-stream timeout must write metadata");
    assert_eq!(
        metadata.get("finish_reason").and_then(|v| v.as_str()),
        Some("timeout"),
        "pre-stream timeout must stamp metadata.finish_reason=timeout; got {metadata}",
    );
    assert_eq!(
        metadata.get("partial").and_then(serde_json::Value::as_bool),
        Some(true),
        "errored finalize must stamp metadata.partial=true; got {metadata}",
    );
}

// ===========================================================================
// 3. Lifecycle guard — sending into a soft-deleted session must 409 and
//    MUST NOT mutate the messages table.
// ===========================================================================

#[tokio::test]
async fn soft_deleted_session_rejects_send_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "lifecycle-guard-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    // Flip the seeded session into the soft_deleted lifecycle state. The
    // production repo's `update_lifecycle_state` is the right tool, but
    // tests reach for the raw test-only helper here so the assertion
    // doesn't depend on the service-layer transition rules.
    db::force_lifecycle_state(&harness.db, session_id, "soft_deleted").await;

    let plugin = FakePlugin::new(plugin_id, FakePluginScript::Events(vec![]));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let Err(err) = svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
    else {
        panic!("soft_deleted session must reject send_message");
    };
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("Conflict"),
        "soft_deleted session must surface as Conflict, got {dbg}",
    );

    // Critical persistence invariant: a rejected request leaves the DB
    // untouched. No user message, no assistant stub.
    let rows = db::list_messages(&harness.db, session_id).await;
    assert!(
        rows.is_empty(),
        "rejected send must not insert any messages; got {rows:?}",
    );
}

// ===========================================================================
// 4. Cascade delete — delete_message_subtree removes the whole subtree
//    (layered collect + per-level leaf-to-root delete) and leaves unrelated
//    subtrees intact, against the real DB.
// ===========================================================================

#[tokio::test]
async fn delete_message_subtree_removes_whole_tree_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "subtree-delete-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    // Deep + wide tree under `root`:  root → {a, b};  a → gc.
    // Plus an unrelated root (`other`) that must survive the delete.
    // Siblings under a shared parent need distinct variant_index — including
    // the two NULL-parent roots, now that the root partial UNIQUE index
    // enforces uniqueness for `parent_message_id IS NULL`.
    let root = db::seed_message(&harness, session_id, None, 0).await;
    let a = db::seed_message(&harness, session_id, Some(root), 0).await;
    let b = db::seed_message(&harness, session_id, Some(root), 1).await;
    let gc = db::seed_message(&harness, session_id, Some(a), 0).await;
    let other = db::seed_message(&harness, session_id, None, 1).await;

    let removed = harness
        .messages
        .delete_message_subtree(session_id, root)
        .await
        .expect("delete subtree");
    assert_eq!(
        removed, 4,
        "root + a + b + gc must all be deleted; got {removed}"
    );

    for (label, id) in [("root", root), ("a", a), ("b", b), ("gc", gc)] {
        assert!(
            db::find_message(&harness.db, id).await.is_none(),
            "{label} ({id}) should be gone after subtree delete",
        );
    }
    assert!(
        db::find_message(&harness.db, other).await.is_some(),
        "unrelated subtree must survive the delete",
    );

    // Idempotency: deleting an already-removed root is a no-op.
    let again = harness
        .messages
        .delete_message_subtree(session_id, root)
        .await
        .expect("re-delete missing root");
    assert_eq!(
        again, 0,
        "second delete of the same root must remove nothing"
    );
}

// ===========================================================================
// 5. Auth scoping — a different tenant calling send_message on the same
//    session id must 404 (anti-enumeration). No DB mutation.
// ===========================================================================

#[tokio::test]
async fn cross_tenant_send_returns_not_found_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "cross-tenant-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    let plugin = FakePlugin::new(plugin_id, FakePluginScript::Events(vec![]));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let intruder = Identity::new("tenant-other", USER_ID, None).unwrap();
    let cancel = CancellationToken::new();
    let Err(err) = svc
        .send_message(make_request(session_id), intruder, cancel)
        .await
    else {
        panic!("cross-tenant send must reject");
    };
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("NotFound"),
        "cross-tenant send must surface as NotFound (anti-enumeration), got {dbg}",
    );

    let rows = db::list_messages(&harness.db, session_id).await;
    assert!(
        rows.is_empty(),
        "cross-tenant rejected send must not insert any messages; got {rows:?}",
    );
}

// ===========================================================================
// 3b. True live-tail (FR-024, 3b-3): dropping the client stream mid-flight
// must NOT abort generation. The detached driver runs to completion so a
// reconnect can resume — the persisted row ends up COMPLETE, not cancelled.
// ===========================================================================

#[tokio::test]
async fn dropped_client_stream_still_completes_generation_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "live-tail-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    let placeholder = Uuid::nil();
    let plugin = FakePlugin::new(
        plugin_id,
        FakePluginScript::Events(vec![
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: placeholder,
                chunk: "Hel".into(),
            }),
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: placeholder,
                chunk: "lo".into(),
            }),
            StreamingEvent::Complete(StreamingCompleteEvent {
                message_id: placeholder,
                metadata: None,
                file_citations: vec![],
                link_citations: vec![],
                references: vec![],
            }),
        ]),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let stream = svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
        .expect("send_message dispatch");
    // Simulate a client that disconnects immediately: drop the response stream
    // without consuming a single event. The driver must press on regardless.
    drop(stream);

    let row = db::wait_for_finalize(&harness.db, session_id, Duration::from_secs(2)).await;
    assert!(
        row.is_complete,
        "client disconnect must NOT cancel generation under live-tail; row = {row:?}",
    );
    assert_eq!(
        db::message_text(&harness.db, row.message_id).await,
        "Hello",
        "full plugin output must persist even though the client left",
    );
}

// ===========================================================================
// 3c. Vocabulary persistence (FR-024 Phase B): a streamed Part persists as an
// extra message part; State/Tool events fold into the message metadata.
// ===========================================================================

#[tokio::test]
async fn streamed_parts_and_metadata_persist_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "vocab-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    let placeholder = Uuid::nil();
    let plugin = FakePlugin::new(
        plugin_id,
        FakePluginScript::Events(vec![
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: placeholder,
                chunk: "Answer".into(),
            }),
            StreamingEvent::Part(StreamingPartEvent {
                message_id: placeholder,
                part: MessagePartInput {
                    part_type: MessagePartType::Links,
                    content: serde_json::json!({ "links": [{ "url": "https://example.com" }] }),
                    file_citations: vec![],
                    link_citations: vec![],
                    references: vec![],
                },
            }),
            StreamingEvent::State(StreamingStateEvent {
                message_id: placeholder,
                state: serde_json::json!({ "phase": "final" }),
            }),
            StreamingEvent::Tool(StreamingToolEvent {
                message_id: placeholder,
                tool: "file_search".into(),
                payload: serde_json::json!({ "q": "x" }),
            }),
            StreamingEvent::Complete(StreamingCompleteEvent {
                message_id: placeholder,
                metadata: Some(serde_json::json!({ "finish_reason": "stop" })),
                file_citations: vec![],
                link_citations: vec![],
                references: vec![],
            }),
        ]),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let mut stream = svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
        .expect("send_message dispatch");
    while stream.next().await.is_some() {}

    let row = db::wait_for_finalize(&harness.db, session_id, Duration::from_secs(2)).await;
    assert!(
        row.is_complete,
        "completed send must finalize is_complete=true"
    );

    // Parts: primary text (number 0) + the streamed links part (number 1).
    let parts = db::message_parts_ordered(&harness.db, session_id, "assistant").await;
    assert_eq!(parts.len(), 2, "expected text + links parts; got {parts:?}");
    assert_eq!((parts[0].0.as_str(), parts[0].1), ("text", 0));
    assert_eq!((parts[1].0.as_str(), parts[1].1), ("links", 1));

    // State + Tool fold into the persisted message metadata.
    let meta = row.metadata.expect("metadata present");
    assert_eq!(
        meta["state"]["phase"], "final",
        "State event must persist under metadata.state"
    );
    assert_eq!(
        meta["tools"][0]["tool"], "file_search",
        "Tool event must persist under metadata.tools",
    );
    assert_eq!(
        meta["finish_reason"], "stop",
        "plugin metadata must be preserved"
    );
}

// ===========================================================================
// 4. Resume buffer is populated while the stream runs (FR-024, 3b-2)
//
// The detached driver tees every projected wire event into the
// `StreamEventBuffer` as it pumps the live channel. Drive a full
// Chunk+Chunk+Complete send and assert the buffer holds the projected
// start / delta(s) / complete frames with a contiguous per-message seq —
// the substrate a `Last-Event-ID` reconnect will later replay.
// ===========================================================================

#[tokio::test]
async fn streamed_events_are_buffered_for_resume_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "resume-buffer-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id = db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    let buffer: Arc<dyn StreamEventBuffer> =
        Arc::new(SeaStreamEventBuffer::new(Arc::clone(&harness.db)));

    // The SDK placeholder id; the driver re-stamps each event with the real
    // assistant id before projecting, so the buffered events key off that.
    let placeholder = Uuid::nil();
    let plugin = FakePlugin::new(
        plugin_id,
        FakePluginScript::Events(vec![
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: placeholder,
                chunk: "Hel".into(),
            }),
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: placeholder,
                chunk: "lo".into(),
            }),
            StreamingEvent::Complete(StreamingCompleteEvent {
                message_id: placeholder,
                metadata: None,
                file_citations: vec![],
                link_citations: vec![],
                references: vec![],
            }),
        ]),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;

    // Mirror `build_service` plus the resume-buffer collaborator.
    let hub = Arc::new(ClientHub::new());
    hub.register_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(plugin_id), plugin_dyn);
    let plugins = PluginService::new(hub, Arc::clone(&harness.plugin_configs));
    let svc = MessageService::new(
        Arc::clone(&harness.sessions),
        Arc::clone(&harness.session_types),
        Arc::clone(&harness.messages),
        plugins,
    )
    .with_stream_buffer(Arc::clone(&buffer));

    let cancel = CancellationToken::new();
    let mut stream = svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
        .expect("send_message dispatch");
    while stream.next().await.is_some() {}

    // The finalize UPDATE runs in the detached driver after the channel
    // closes; wait for it so the assistant row (and its buffered tail) is
    // settled before we read the buffer.
    let row = db::wait_for_finalize(&harness.db, session_id, Duration::from_secs(2)).await;
    assert!(
        row.is_complete,
        "completed send must finalize is_complete=true"
    );

    let events = buffer
        .read_since(row.message_id, None)
        .await
        .expect("read resume buffer");
    assert!(
        events.len() >= 3,
        "expected start + delta(s) + complete; got {events:?}",
    );

    let types: Vec<&str> = events
        .iter()
        .map(|e| e.event["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        types.first(),
        Some(&"message.start"),
        "first buffered event is message.start",
    );
    assert_eq!(
        types.last(),
        Some(&"message.complete"),
        "last buffered event is message.complete",
    );
    assert!(
        types.contains(&"message.text.delta"),
        "text chunks must project to message.text.delta events; got {types:?}",
    );

    // seq is a contiguous per-message counter starting at 0 — the SSE `id:`.
    for (i, e) in events.iter().enumerate() {
        assert_eq!(
            e.seq, i as u64,
            "buffered seq must be contiguous; got {events:?}"
        );
    }
}
