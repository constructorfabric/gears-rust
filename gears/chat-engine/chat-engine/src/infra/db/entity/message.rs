// @cpt-cf-chat-engine-dbtable-messages:p1
// @cpt-cf-chat-engine-adr-message-tree-structure:p1

use modkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use sea_orm::{Condition, QueryOrder, QuerySelect, SqlErr};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::infra::db::migrations::UQ_VARIANT_INDEX;

/// Maximum retries when racing the `uq_messages_session_parent_variant`
/// constraint. After exhaustion callers MUST map the returned error to
/// HTTP `409 Conflict` (see DESIGN §3.7 "Variant Index Concurrency").
pub const VARIANT_INDEX_MAX_RETRIES: u32 = 3;

// Tenant / user scoping for messages is enforced indirectly via the
// owning `sessions` row, which the repo joins on per request. There is
// no per-message `tenant_id` column to wire `Scopable` against, so the
// entity is marked unrestricted and scoping stays in the explicit
// session join below.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "messages")]
#[secure(unrestricted)]
#[allow(clippy::struct_excessive_bools)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub message_id: Uuid,
    pub session_id: Uuid,
    pub parent_message_id: Option<Uuid>,
    pub role: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub content: serde_json::Value,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub file_ids: Option<serde_json::Value>,
    pub variant_index: i32,
    pub is_active: bool,
    pub is_complete: bool,
    pub is_hidden_from_user: bool,
    pub is_hidden_from_backend: bool,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::session::Entity",
        from = "Column::SessionId",
        to = "super::session::Column::SessionId",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Session,
    #[sea_orm(
        belongs_to = "Entity",
        from = "Column::ParentMessageId",
        to = "Column::MessageId",
        on_update = "NoAction",
        on_delete = "Restrict"
    )]
    Parent,
    #[sea_orm(has_many = "super::message_reaction::Entity")]
    Reaction,
}

impl Related<super::session::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Session.def()
    }
}

impl Related<super::message_reaction::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Reaction.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Compute the next `variant_index` for the sibling group identified by
/// `(session_id, parent_message_id)` **inside the caller's transaction**.
///
/// This is the SELECT half of the variant-index allocation; the matching
/// INSERT MUST be issued against the same transaction handle so a
/// concurrent caller cannot observe-then-claim the same index between the
/// read and the write. Callers wrap both operations in a single
/// SERIALIZABLE transaction plus a retry loop bounded by
/// [`VARIANT_INDEX_MAX_RETRIES`] — see the call sites in
/// `infra::db::repo::message_repo` and `infra::db::repo::variant_repo`.
///
/// The earlier `assign_variant_index` helper opened its own transaction
/// for the SELECT only and returned `i32` to the caller, who then issued
/// the INSERT in a *separate* transaction. That created a race window
/// where a concurrent caller could claim the same index between the two
/// transactions, surfacing the unique violation as a raw 500 instead of
/// retrying. The helper was removed in favour of this primitive plus
/// inline retry at every call site.
pub async fn compute_next_variant_index<R>(
    runner: &R,
    session_id: Uuid,
    parent: Option<Uuid>,
) -> Result<i32, ChatEngineError>
where
    R: DBRunner,
{
    let scope = AccessScope::allow_all();

    let parent_filter = match parent {
        Some(p) => Condition::all().add(Column::ParentMessageId.eq(p)),
        None => Condition::all().add(Column::ParentMessageId.is_null()),
    };

    let row = Entity::find()
        .order_by_desc(Column::VariantIndex)
        .limit(1)
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(Column::SessionId.eq(session_id)))
        .filter(parent_filter)
        .one(runner)
        .await?;

    Ok(match row {
        Some(row) => row.variant_index + 1,
        None => 0,
    })
}

/// Detect whether `err` is a UNIQUE-constraint violation on
/// [`UQ_VARIANT_INDEX`] (`uq_messages_session_parent_variant`) — i.e.,
/// a retryable variant-index collision.
///
/// Classification anchors on SeaORM's structured
/// [`SqlErr::UniqueConstraintViolation`], which is in turn derived from
/// the driver's SQLSTATE / extended result code (Postgres `23505`,
/// SQLite `2067`/`1555`, MySQL `1062`/etc.). The substring narrowing
/// inside the violation's message text only filters out unrelated
/// UNIQUE violations elsewhere in the schema; it is no longer matching
/// on the unstructured `Display` of the whole `DbErr`. A driver locale
/// change or SeaORM `Display` tweak therefore cannot silently turn a
/// retryable conflict into a hard failure: the structured discriminant
/// stays put and only the narrowing message-text fallback would skew.
///
/// Postgres includes the index name (`uq_messages_session_parent_variant`)
/// in the violation message; SQLite emits the offending column list
/// (`messages.session_id, messages.parent_message_id,
/// messages.variant_index`). Either pattern unambiguously identifies
/// this crate's only UNIQUE index. If neither matches, we return
/// `false` and the caller surfaces the error without retrying — the
/// safe default given an unrecognised UNIQUE violation might belong to
/// a different schema constraint added later.
pub fn is_variant_unique_violation(err: &DbErr) -> bool {
    let Some(SqlErr::UniqueConstraintViolation(message)) = err.sql_err() else {
        return false;
    };
    // Postgres path — the message embeds the index name verbatim.
    if message.contains(UQ_VARIANT_INDEX) {
        return true;
    }
    // SQLite path — no constraint name in the message; identify the
    // UQ by the column triple it covers. Two of the three columns
    // would already be ambiguous (the `idx_messages_session_parent`
    // INDEX is on the first two columns but is not UNIQUE, so it does
    // not raise this error), but matching on all three makes the
    // classification air-tight.
    message.contains("messages.session_id")
        && message.contains("messages.parent_message_id")
        && message.contains("messages.variant_index")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;

    /// End-to-end: drive a real `uq_messages_session_parent_variant`
    /// violation against an in-memory SQLite and assert
    /// `is_variant_unique_violation` returns `true`. Anchors the test
    /// on the *structured* `SqlErr::UniqueConstraintViolation`
    /// discriminant instead of the unstructured `Display` form, so a
    /// future SeaORM / sqlx text reshuffle that breaks substring
    /// matching would also fail this test (the failure mode is
    /// visible, not a silent 500 in production).
    #[tokio::test]
    async fn detects_real_sqlite_uq_violation() {
        use sea_orm::ConnectOptions;

        let mut opts = ConnectOptions::new("sqlite::memory:".to_string());
        opts.max_connections(1);
        let db = Database::connect(opts).await.expect("connect");
        crate::infra::db::Migrator::up(&db, None)
            .await
            .expect("migrations");

        // Seed a session + session_type so the FK on messages is
        // satisfied. The migration set installs both tables.
        let session_id = uuid::Uuid::new_v4();
        let session_type_id = uuid::Uuid::new_v4();
        let now = OffsetDateTime::now_utc().to_string();
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_types (session_type_id, name, plugin_instance_id, \
                 created_at, updated_at) VALUES ('{session_type_id}', 'test', NULL, \
                 '{now}', '{now}')",
            ),
        ))
        .await
        .expect("seed session_type");
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO sessions (session_id, tenant_id, user_id, client_id, \
                 session_type_id, enabled_capabilities, metadata, lifecycle_state, \
                 share_token, deleted_at, scheduled_hard_delete_at, created_at, \
                 updated_at) VALUES ('{session_id}', 't', 'u', NULL, \
                 '{session_type_id}', NULL, NULL, 'active', NULL, NULL, NULL, '{now}', \
                 '{now}')",
            ),
        ))
        .await
        .expect("seed session");

        // Seed a root + one child message under that root with
        // variant_index=0. The UNIQUE INDEX treats NULL columns as
        // distinct (standard SQL behaviour), so we use a non-NULL
        // parent to ensure the constraint actually fires on the
        // duplicate INSERT below.
        let root_id = uuid::Uuid::new_v4();
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO messages (message_id, session_id, parent_message_id, role, \
                 content, file_ids, variant_index, is_active, is_complete, \
                 is_hidden_from_user, is_hidden_from_backend, metadata, created_at) \
                 VALUES ('{root_id}', '{session_id}', NULL, 'user', '{{\"text\":\"a\"}}', \
                 NULL, 0, 1, 1, 0, 0, NULL, '{now}')",
            ),
        ))
        .await
        .expect("seed root message");
        let child_id = uuid::Uuid::new_v4();
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO messages (message_id, session_id, parent_message_id, role, \
                 content, file_ids, variant_index, is_active, is_complete, \
                 is_hidden_from_user, is_hidden_from_backend, metadata, created_at) \
                 VALUES ('{child_id}', '{session_id}', '{root_id}', 'assistant', \
                 '{{\"text\":\"b\"}}', NULL, 0, 1, 1, 0, 0, NULL, '{now}')",
            ),
        ))
        .await
        .expect("seed first child message");

        // Duplicate (session_id, parent=root_id, variant_index=0) →
        // must violate `uq_messages_session_parent_variant`.
        let dup_id = uuid::Uuid::new_v4();
        let err = db
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!(
                    "INSERT INTO messages (message_id, session_id, parent_message_id, \
                     role, content, file_ids, variant_index, is_active, is_complete, \
                     is_hidden_from_user, is_hidden_from_backend, metadata, created_at) \
                     VALUES ('{dup_id}', '{session_id}', '{root_id}', 'assistant', \
                     '{{\"text\":\"c\"}}', NULL, 0, 1, 1, 0, 0, NULL, '{now}')",
                ),
            ))
            .await
            .expect_err("duplicate variant_index must violate uq_messages_session_parent_variant");

        assert!(
            is_variant_unique_violation(&err),
            "real SQLite UQ violation must classify as retryable; got: {err:?}",
        );
        // Defense-in-depth: confirm the structured discriminant fires
        // — not just the legacy substring path.
        assert!(
            matches!(err.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))),
            "DbErr::sql_err() must surface UniqueConstraintViolation; got: {err:?}",
        );
    }

    #[test]
    fn rejects_non_unique_dberr() {
        // A connection-time error is not a unique violation. We use
        // DbErr::Custom because it bypasses sqlx::Error::Database and
        // therefore `sql_err()` correctly returns None — exactly the
        // path a future locale / driver wording change would land on
        // for unrecognised errors. The classifier MUST NOT retry it.
        let err = DbErr::Custom("some unrelated error".to_string());
        assert!(
            !is_variant_unique_violation(&err),
            "non-unique DbErr must not classify as retryable",
        );
    }
}
