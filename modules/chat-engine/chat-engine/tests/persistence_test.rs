//! Persistence-side integration tests for `MessageService`.
//!
//! These tests construct a real `MessageService` over a SeaORM
//! SQLite-in-memory stack (every production repo impl, the real
//! `assign_variant_index` helper, the real `finalize_assistant` UPDATE) and
//! drive it with a scripted plugin so the finalize-on-cancel and
//! finalize-on-pre-stream-timeout paths are exercised end-to-end.
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
use chat_engine_sdk::{
    ChatEngineBackendPlugin, PluginError, StreamingChunkEvent, StreamingEvent,
};
use futures::StreamExt;
use modkit::ClientHub;
use modkit::client_hub::ClientScope;
use tokio_util::sync::CancellationToken;
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
        content: serde_json::json!({"text": "hello"}),
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
    let session_id =
        db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

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
        db::message_text(&row),
        "alpha-beta-",
        "persisted partial content must equal the chunks emitted before cancel",
    );
    let metadata = row
        .metadata
        .as_ref()
        .expect("finalize_assistant must write metadata on cancel");
    assert_eq!(
        metadata.get("cancelled").and_then(|v| v.as_bool()),
        Some(true),
        "cancel finalize must stamp metadata.cancelled=true; got {metadata}",
    );
    assert_eq!(
        metadata.get("partial").and_then(|v| v.as_bool()),
        Some(true),
        "cancel finalize must stamp metadata.partial=true; got {metadata}",
    );
    // Plugin was invoked exactly once per request.
    assert_eq!(plugin.call_count(), 1);
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
    let session_id =
        db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    // Plugin rejects the request before the stream starts — the canonical
    // upstream-timeout shape from the SDK error taxonomy.
    let plugin = FakePlugin::new(
        plugin_id,
        FakePluginScript::PreError(PluginError::timeout()),
    );
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let err = match svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
    {
        Ok(_) => panic!("pre-stream timeout must surface as Err"),
        Err(e) => e,
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
        db::message_text(&row),
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
        metadata.get("partial").and_then(|v| v.as_bool()),
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
    let session_id =
        db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    // Flip the seeded session into the soft_deleted lifecycle state.
    use sea_orm::ActiveModelTrait;
    use sea_orm::ActiveValue::Set;
    use sea_orm::EntityTrait;
    let existing = chat_engine::infra::db::entity::session::Entity::find_by_id(session_id)
        .one(&harness.db)
        .await
        .expect("read seeded session")
        .expect("seeded session must exist");
    let mut am: chat_engine::infra::db::entity::session::ActiveModel = existing.into();
    am.lifecycle_state = Set("soft_deleted".to_string());
    am.update(&harness.db).await.expect("flip lifecycle state");

    let plugin = FakePlugin::new(plugin_id, FakePluginScript::Events(vec![]));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let cancel = CancellationToken::new();
    let err = match svc
        .send_message(make_request(session_id), make_identity(), cancel)
        .await
    {
        Ok(_) => panic!("soft_deleted session must reject send_message"),
        Err(e) => e,
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
// 4. Auth scoping — a different tenant calling send_message on the same
//    session id must 404 (anti-enumeration). No DB mutation.
// ===========================================================================

#[tokio::test]
async fn cross_tenant_send_returns_not_found_against_sqlite() {
    let harness = db::setup_sqlite().await;
    let plugin_id = "cross-tenant-plugin";
    let session_type_id = db::seed_session_type(&harness, plugin_id).await;
    let session_id =
        db::seed_active_session(&harness, TENANT_ID, USER_ID, session_type_id).await;

    let plugin = FakePlugin::new(plugin_id, FakePluginScript::Events(vec![]));
    let plugin_dyn: Arc<dyn ChatEngineBackendPlugin> = plugin;
    let svc = build_service(&harness, plugin_id, plugin_dyn);

    let intruder = Identity::new("tenant-other", USER_ID, None).unwrap();
    let cancel = CancellationToken::new();
    let err = match svc
        .send_message(make_request(session_id), intruder, cancel)
        .await
    {
        Ok(_) => panic!("cross-tenant send must reject"),
        Err(e) => e,
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
