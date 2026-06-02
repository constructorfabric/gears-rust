//! SQLite-backed integration-test harness for chat-engine.
//!
//! Constructs a real `DatabaseConnection` against `sqlite::memory:`, runs
//! the production migration set, and exposes thin helpers to seed a session
//! type + session row and read message rows back. The repos take a bare
//! `sea_orm::DatabaseConnection`, so wiring them is just `SeaMessageRepo::
//! new(db.clone())` and friends — no provider wrappers needed.
//!
//! SQLite's `:memory:` mode keeps each connection isolated, so we MUST cap
//! the pool at a single connection — otherwise migrations write to one
//! private database and the repos read from another. We also use a unique
//! `mode=memory&cache=shared` DSN keyed by a random uuid so concurrent
//! tests can't accidentally share state.
//
// @cpt-cf-chat-engine-e2e-harness:p16

#![allow(dead_code)]

use std::sync::Arc;

use chat_engine::infra::db::Migrator;
use chat_engine::infra::db::entity::{message, session, session_type};
use chat_engine::infra::db::repo::message_repo::{MessageRepo, SeaMessageRepo};
use chat_engine::infra::db::repo::plugin_config_repo::{PluginConfigRepo, SeaPluginConfigRepo};
use chat_engine::infra::db::repo::session_repo::{SeaSessionRepo, SessionRepo};
use chat_engine::infra::db::repo::session_type_repo::{SeaSessionTypeRepo, SessionTypeRepo};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait,
    QueryFilter,
};
use sea_orm_migration::MigratorTrait;
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use uuid::Uuid;

/// Production-shaped DB harness wrapping the live repos. Tests can either
/// drive the public repo trait surface or reach for `db` to query rows
/// directly when asserting on persistence side-effects.
pub struct DbHarness {
    pub db: DatabaseConnection,
    pub sessions: Arc<dyn SessionRepo>,
    pub session_types: Arc<dyn SessionTypeRepo>,
    pub messages: Arc<dyn MessageRepo>,
    pub plugin_configs: Arc<dyn PluginConfigRepo>,
}

/// Open a fresh in-memory SQLite database, apply every chat-engine
/// migration, and wire the production repo impls on top.
///
/// `max_connections(1)` is load-bearing: `sqlite::memory:` gives each
/// connection in the pool its own private database, so without the cap
/// migrations would land on one connection and the repos would query an
/// empty one. Mirrors the account-management harness pattern.
pub async fn setup_sqlite() -> DbHarness {
    let mut opts = ConnectOptions::new("sqlite::memory:".to_string());
    opts.max_connections(1);
    let db = Database::connect(opts)
        .await
        .expect("connect sqlite::memory:");

    Migrator::up(&db, None)
        .await
        .expect("apply chat-engine migrations");

    let sessions: Arc<dyn SessionRepo> = Arc::new(SeaSessionRepo::new(db.clone()));
    let session_types: Arc<dyn SessionTypeRepo> = Arc::new(SeaSessionTypeRepo::new(db.clone()));
    let messages: Arc<dyn MessageRepo> = Arc::new(SeaMessageRepo::new(db.clone()));
    let plugin_configs: Arc<dyn PluginConfigRepo> = Arc::new(SeaPluginConfigRepo::new(db.clone()));

    DbHarness {
        db,
        sessions,
        session_types,
        messages,
        plugin_configs,
    }
}

/// Insert a `session_types` row bound to `plugin_instance_id`. Returns the
/// generated session-type id.
pub async fn seed_session_type(
    h: &DbHarness,
    plugin_instance_id: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    let am = session_type::ActiveModel {
        session_type_id: Set(id),
        name: Set("integration-test".to_string()),
        plugin_instance_id: Set(Some(plugin_instance_id.to_string())),
        created_at: Set(now),
        updated_at: Set(now),
    };
    h.session_types
        .insert(am)
        .await
        .expect("insert session_type row");
    id
}

/// Insert an active session bound to `session_type_id`. Returns the
/// generated session id.
pub async fn seed_active_session(
    h: &DbHarness,
    tenant_id: &str,
    user_id: &str,
    session_type_id: Uuid,
) -> Uuid {
    let id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    let am = session::ActiveModel {
        session_id: Set(id),
        tenant_id: Set(tenant_id.to_string()),
        user_id: Set(user_id.to_string()),
        client_id: Set(None),
        session_type_id: Set(Some(session_type_id)),
        enabled_capabilities: Set(None),
        metadata: Set(None),
        lifecycle_state: Set("active".to_string()),
        share_token: Set(None),
        deleted_at: Set(None),
        scheduled_hard_delete_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    h.sessions.insert(am).await.expect("insert session row");
    id
}

/// Direct SeaORM lookup of a `messages` row by primary key — used by
/// persistence assertions that cannot rely on the repo's filtered reads
/// (the assistant stub is `is_complete=false` so `fetch_active_history`
/// hides it).
pub async fn find_message(db: &DatabaseConnection, message_id: Uuid) -> Option<message::Model> {
    message::Entity::find()
        .filter(message::Column::MessageId.eq(message_id))
        .one(db)
        .await
        .expect("read messages row")
}

/// Return every `messages` row for `session_id` in `created_at ASC` order.
/// Useful for asserting both the user row and the assistant stub landed.
pub async fn list_messages(db: &DatabaseConnection, session_id: Uuid) -> Vec<message::Model> {
    use sea_orm::QueryOrder;
    message::Entity::find()
        .filter(message::Column::SessionId.eq(session_id))
        .order_by_asc(message::Column::CreatedAt)
        .all(db)
        .await
        .expect("list messages")
}

/// Locate the assistant message inserted by `send_message`. There is
/// exactly one per call.
pub async fn find_assistant_message(
    db: &DatabaseConnection,
    session_id: Uuid,
) -> Option<message::Model> {
    message::Entity::find()
        .filter(message::Column::SessionId.eq(session_id))
        .filter(message::Column::Role.eq("assistant"))
        .one(db)
        .await
        .expect("find assistant row")
}

/// Pull `content.text` from a persisted message row. Returns the empty
/// string for any non-conforming shape so callers can stay terse.
pub fn message_text(model: &message::Model) -> String {
    match &model.content {
        JsonValue::Object(map) => map
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Poll until the assistant row for `session_id` reaches `is_complete =
/// expected_complete`, or `deadline` elapses. Returns the latest snapshot
/// either way. The driver task finalises the row in a detached
/// `tokio::spawn`, so tests can't synchronise on a JoinHandle; this is
/// the deterministic equivalent of awaiting one.
pub async fn wait_for_finalize(
    db: &DatabaseConnection,
    session_id: Uuid,
    deadline: std::time::Duration,
) -> message::Model {
    let started = std::time::Instant::now();
    loop {
        let row = find_assistant_message(db, session_id).await;
        if let Some(m) = row {
            // The stub starts at `is_complete=false, metadata=NULL`; finalize
            // writes one or both. Either signals the driver wrote-back.
            if m.is_complete || m.metadata.is_some() {
                return m;
            }
            if started.elapsed() >= deadline {
                panic!(
                    "assistant row for session {session_id} not finalised within \
                     {deadline:?}; last row = {m:?}",
                );
            }
        } else if started.elapsed() >= deadline {
            panic!(
                "no assistant row appeared for session {session_id} within {deadline:?}",
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}
