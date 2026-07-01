//! Integration-test harness for chat-engine wired against the same
//! `DBProvider` the production module uses.
//!
//! The harness opens a single in-memory `SQLite` database through
//! `toolkit_db::connect_db`, applies the production migration set via
//! `run_migrations_for_testing`, and wraps the resulting `Db` in a
//! `DBProvider<ChatEngineError>` keyed on `ChatEngineError`. Every repo
//! and every test helper receives that same `Arc<ChatEngineDb>`, so
//! migrations and queries always land on the same connection — no
//! sibling `SQLite` pool, no silent fall-back to a private memory DB.
//!
//! `SQLite`'s `:memory:` mode keeps each pooled connection isolated, so
//! the harness caps the pool at a single connection — without that the
//! migration runner could land on one connection and the repos query
//! another.
//
// @cpt-cf-chat-engine-e2e-harness:p16

#![allow(dead_code)]

use std::sync::Arc;

use chat_engine::infra::db::Migrator;
use chat_engine::infra::db::entity::{file_citation, message, message_part, session, session_type};
use chat_engine::infra::db::repo::ChatEngineDb;
use chat_engine::infra::db::repo::message_repo::{MessageRepo, SeaMessageRepo};
use chat_engine::infra::db::repo::plugin_config_repo::{PluginConfigRepo, SeaPluginConfigRepo};
use chat_engine::infra::db::repo::session_repo::{SeaSessionRepo, SessionRepo};
use chat_engine::infra::db::repo::session_type_repo::{SeaSessionTypeRepo, SessionTypeRepo};
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryOrder};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, SecureEntityExt, SecureInsertExt};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

/// Production-shaped DB harness wrapping the live repos. Tests drive the
/// public repo trait surface; raw row lookups go through the harness's
/// helpers below which themselves run against the same `DBProvider`.
pub struct DbHarness {
    pub db: Arc<ChatEngineDb>,
    pub sessions: Arc<dyn SessionRepo>,
    pub session_types: Arc<dyn SessionTypeRepo>,
    pub messages: Arc<dyn MessageRepo>,
    pub plugin_configs: Arc<dyn PluginConfigRepo>,
}

/// Open a fresh in-memory `SQLite` database, apply every chat-engine
/// migration through toolkit-db, and wire the production repo impls on
/// top of the resulting `DBProvider`.
///
/// `max_conns: Some(1)` is load-bearing: `sqlite::memory:` gives each
/// connection in the pool its own private database, so without the cap
/// migrations would land on one connection and the repos would query an
/// empty one. Mirrors the account-management harness pattern.
pub async fn setup_sqlite() -> DbHarness {
    use sea_orm_migration::MigratorTrait;

    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect sqlite::memory:");

    // Apply the production migration set against the toolkit-db handle so
    // every repo built below queries the same connection state.
    toolkit_db::migration_runner::run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("apply chat-engine migrations");

    let provider: Arc<ChatEngineDb> = Arc::new(DBProvider::new(db));

    let sessions: Arc<dyn SessionRepo> = Arc::new(SeaSessionRepo::new(Arc::clone(&provider)));
    let session_types: Arc<dyn SessionTypeRepo> =
        Arc::new(SeaSessionTypeRepo::new(Arc::clone(&provider)));
    let messages: Arc<dyn MessageRepo> = Arc::new(SeaMessageRepo::new(Arc::clone(&provider)));
    let plugin_configs: Arc<dyn PluginConfigRepo> =
        Arc::new(SeaPluginConfigRepo::new(Arc::clone(&provider)));

    DbHarness {
        db: provider,
        sessions,
        session_types,
        messages,
        plugin_configs,
    }
}

/// Insert a `session_types` row bound to `plugin_instance_id`. Returns the
/// generated session-type id.
pub async fn seed_session_type(h: &DbHarness, plugin_instance_id: &str) -> Uuid {
    let id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    let am = session_type::ActiveModel {
        session_type_id: Set(id),
        name: Set("integration-test".to_owned()),
        plugin_instance_id: Set(Some(plugin_instance_id.to_owned())),
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
        tenant_id: Set(tenant_id.to_owned()),
        user_id: Set(user_id.to_owned()),
        client_id: Set(None),
        session_type_id: Set(Some(session_type_id)),
        enabled_capabilities: Set(None),
        metadata: Set(None),
        lifecycle_state: Set("active".to_owned()),
        share_token: Set(None),
        deleted_at: Set(None),
        scheduled_hard_delete_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    h.sessions.insert(am).await.expect("insert session row");
    id
}

/// Insert a `messages` row under `parent_message_id` (NULL for a root)
/// with the given `variant_index`. Siblings under the same parent must use
/// distinct indices (the `uq_messages_session_parent_variant` constraint).
/// Returns the generated message id. Used to build subtrees for the
/// cascade-delete tests.
pub async fn seed_message(
    h: &DbHarness,
    session_id: Uuid,
    parent_message_id: Option<Uuid>,
    variant_index: i32,
) -> Uuid {
    let conn = h.db.conn().expect("conn for seed_message");
    let scope = AccessScope::allow_all();
    let id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    let am = message::ActiveModel {
        message_id: Set(id),
        session_id: Set(session_id),
        tenant_id: Set(None),
        user_id: Set(None),
        parent_message_id: Set(parent_message_id),
        role: Set(message::MessageRole::User),
        file_ids: Set(None),
        variant_index: Set(variant_index),
        is_active: Set(true),
        is_complete: Set(true),
        is_hidden_from_user: Set(false),
        is_hidden_from_backend: Set(false),
        metadata: Set(None),
        created_at: Set(now),
    };
    message::Entity::insert(am)
        .secure()
        .scope_unchecked(&scope)
        .expect("scope message insert")
        .exec(&conn)
        .await
        .expect("insert message row");
    id
}

/// Direct `SeaORM` lookup of a `messages` row by primary key — used by
/// persistence assertions that cannot rely on the repo's filtered reads
/// (the assistant stub is `is_complete=false` so `fetch_active_history`
/// hides it).
pub async fn find_message(db: &Arc<ChatEngineDb>, message_id: Uuid) -> Option<message::Model> {
    let conn = db.conn().expect("conn for find_message");
    let scope = AccessScope::allow_all();
    message::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(message::Column::MessageId.eq(message_id)))
        .one(&conn)
        .await
        .expect("read messages row")
}

/// Return every `messages` row for `session_id` in `created_at ASC` order.
/// Useful for asserting both the user row and the assistant stub landed.
pub async fn list_messages(db: &Arc<ChatEngineDb>, session_id: Uuid) -> Vec<message::Model> {
    let conn = db.conn().expect("conn for list_messages");
    let scope = AccessScope::allow_all();
    message::Entity::find()
        .order_by_asc(message::Column::CreatedAt)
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(message::Column::SessionId.eq(session_id)))
        .all(&conn)
        .await
        .expect("list messages")
}

/// Locate the assistant message inserted by `send_message`. There is
/// exactly one per call.
pub async fn find_assistant_message(
    db: &Arc<ChatEngineDb>,
    session_id: Uuid,
) -> Option<message::Model> {
    let conn = db.conn().expect("conn for find_assistant_message");
    let scope = AccessScope::allow_all();
    message::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(
            Condition::all()
                .add(message::Column::SessionId.eq(session_id))
                .add(message::Column::Role.eq("assistant")),
        )
        .one(&conn)
        .await
        .expect("find assistant row")
}

/// Return the parts of every message with `role` in `session_id`, ordered by
/// `number`, as `(type_string, number, content)` tuples. Used by part-aware
/// persistence assertions (FR-022).
pub async fn message_parts_ordered(
    db: &Arc<ChatEngineDb>,
    session_id: Uuid,
    role: &str,
) -> Vec<(String, i32, serde_json::Value)> {
    let conn = db.conn().expect("conn for message_parts_ordered");
    let scope = AccessScope::allow_all();
    let msgs = message::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(
            Condition::all()
                .add(message::Column::SessionId.eq(session_id))
                .add(message::Column::Role.eq(role)),
        )
        .all(&conn)
        .await
        .expect("load messages by role");
    let ids: Vec<Uuid> = msgs.iter().map(|m| m.message_id).collect();
    let rows = message_part::Entity::find()
        .order_by_asc(message_part::Column::Number)
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(message_part::Column::MessageId.is_in(ids)))
        .all(&conn)
        .await
        .expect("load message parts");
    rows.into_iter()
        .map(|p| (part_type_str(&p.r#type).to_owned(), p.number, p.content))
        .collect()
}

/// Return the stored `file_citations.content` payloads for every part of
/// `message_id`, ordered by part then citation `number` (FR-023).
pub async fn file_citations_for_message(
    db: &Arc<ChatEngineDb>,
    message_id: Uuid,
) -> Vec<serde_json::Value> {
    let conn = db.conn().expect("conn for file_citations_for_message");
    let scope = AccessScope::allow_all();
    let parts = message_part::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(message_part::Column::MessageId.eq(message_id)))
        .all(&conn)
        .await
        .expect("load parts");
    let pids: Vec<Uuid> = parts.iter().map(|p| p.id).collect();
    let rows = file_citation::Entity::find()
        .order_by_asc(file_citation::Column::Number)
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(file_citation::Column::MessagePartId.is_in(pids)))
        .all(&conn)
        .await
        .expect("load file citations");
    rows.into_iter().map(|r| r.content).collect()
}

/// Map a persisted part-type enum to its wire string (test-side mirror of the
/// domain mapping).
fn part_type_str(t: &message_part::MessagePartType) -> &'static str {
    match t {
        message_part::MessagePartType::Text => "text",
        message_part::MessagePartType::Code => "code",
        message_part::MessagePartType::Images => "images",
        message_part::MessagePartType::Videos => "videos",
        message_part::MessagePartType::Links => "links",
        message_part::MessagePartType::Statuses => "statuses",
    }
}

/// Concatenate the `text` part bodies of a persisted message (ordered by
/// `number`). Returns the empty string when the message has no text parts.
/// Reads `message_parts` since the body no longer lives on `messages`.
pub async fn message_text(db: &Arc<ChatEngineDb>, message_id: Uuid) -> String {
    let conn = db.conn().expect("conn for message_text");
    let scope = AccessScope::allow_all();
    let rows = message_part::Entity::find()
        .order_by_asc(message_part::Column::Number)
        .secure()
        .scope_with(&scope)
        .filter(
            Condition::all()
                .add(message_part::Column::MessageId.eq(message_id))
                .add(message_part::Column::Type.eq("text")),
        )
        .all(&conn)
        .await
        .expect("load message parts");
    rows.iter()
        .filter_map(|p| p.content.get("text").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Poll until the assistant row for `session_id` reaches `is_complete =
/// expected_complete`, or `deadline` elapses. Returns the latest snapshot
/// either way. The driver task finalises the row in a detached
/// `tokio::spawn`, so tests can't synchronise on a `JoinHandle`; this is
/// the deterministic equivalent of awaiting one.
pub async fn wait_for_finalize(
    db: &Arc<ChatEngineDb>,
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
            assert!(
                started.elapsed() < deadline,
                "assistant row for session {session_id} not finalised within \
                 {deadline:?}; last row = {m:?}",
            );
        } else if started.elapsed() >= deadline {
            panic!("no assistant row appeared for session {session_id} within {deadline:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// Pretty alias used by some assertions — exposes the harness's
/// `Arc<ChatEngineDb>` under the `db_provider` field name the tests had
/// historically expected.
pub fn db_provider(h: &DbHarness) -> &Arc<ChatEngineDb> {
    &h.db
}

/// Test-only raw column flip on `sessions.lifecycle_state`. Production
/// code MUST go through
/// [`chat_engine::infra::db::repo::session_repo::SessionRepo::update_lifecycle_state`]
/// so the service-layer transition checks fire; this bypass exists only
/// to put a fixture row into any state the surrounding test wants
/// to assert against (e.g. seeding a `soft_deleted` session and then
/// verifying that `send_message` refuses to write).
pub async fn force_lifecycle_state(db: &Arc<ChatEngineDb>, session_id: Uuid, new_state: &str) {
    use sea_orm::sea_query::Expr;
    use toolkit_db::secure::SecureUpdateExt;

    let conn = db.conn().expect("conn for force_lifecycle_state");
    let scope = AccessScope::allow_all();
    session::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(session::Column::SessionId.eq(session_id)))
        .col_expr(
            session::Column::LifecycleState,
            Expr::value(new_state.to_owned()),
        )
        .col_expr(
            session::Column::UpdatedAt,
            Expr::value(OffsetDateTime::now_utc()),
        )
        .exec(&conn)
        .await
        .expect("flip lifecycle state");
}
