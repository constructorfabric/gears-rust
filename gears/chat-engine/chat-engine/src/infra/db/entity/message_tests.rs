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
             file_ids, variant_index, is_active, is_complete, \
             is_hidden_from_user, is_hidden_from_backend, metadata, created_at) \
             VALUES ('{root_id}', '{session_id}', NULL, 'user', \
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
             file_ids, variant_index, is_active, is_complete, \
             is_hidden_from_user, is_hidden_from_backend, metadata, created_at) \
             VALUES ('{child_id}', '{session_id}', '{root_id}', 'assistant', \
             NULL, 0, 1, 1, 0, 0, NULL, '{now}')",
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
                 role, file_ids, variant_index, is_active, is_complete, \
                 is_hidden_from_user, is_hidden_from_backend, metadata, created_at) \
                 VALUES ('{dup_id}', '{session_id}', '{root_id}', 'assistant', \
                 NULL, 0, 1, 1, 0, 0, NULL, '{now}')",
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

/// Two ROOT messages (`parent_message_id` NULL) with the same
/// `(session_id, variant_index)` must collide on the root partial UNIQUE
/// index. The composite `uq_messages_session_parent_variant` alone treats
/// the NULL parents as distinct and would let both through — this asserts
/// the partial index closes that gap and the collision is retryable.
#[tokio::test]
async fn detects_real_sqlite_root_uq_violation() {
    use sea_orm::ConnectOptions;

    let mut opts = ConnectOptions::new("sqlite::memory:".to_string());
    opts.max_connections(1);
    let db = Database::connect(opts).await.expect("connect");
    crate::infra::db::Migrator::up(&db, None)
        .await
        .expect("migrations");

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

    let insert_root = |id: uuid::Uuid| {
        Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO messages (message_id, session_id, parent_message_id, \
                 role, file_ids, variant_index, is_active, is_complete, \
                 is_hidden_from_user, is_hidden_from_backend, metadata, created_at) \
                 VALUES ('{id}', '{session_id}', NULL, 'user', \
                 NULL, 0, 1, 1, 0, 0, NULL, '{now}')",
            ),
        )
    };
    db.execute(insert_root(uuid::Uuid::new_v4()))
        .await
        .expect("seed first root");
    let err = db
        .execute(insert_root(uuid::Uuid::new_v4()))
        .await
        .expect_err("second root with same variant_index must violate the root UNIQUE index");

    assert!(
        is_variant_unique_violation(&err),
        "root variant collision must classify as retryable; got: {err:?}",
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
