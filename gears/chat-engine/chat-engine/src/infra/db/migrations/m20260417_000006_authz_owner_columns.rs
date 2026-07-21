// @cpt-cf-chat-engine-dbtable-authz-owner-columns
// @cpt-cf-chat-engine-principle-owner-denorm-invariant
//
// Adds owner (tenant_id / user_id) denormalization to all child tables and
// casts sessions.tenant_id / sessions.user_id from TEXT to UUID on Postgres.
// This migration is engine-aware: Postgres branches use ALTER COLUMN TYPE and
// correlated UPDATE ... FROM; SQLite branches use typeless TEXT columns and
// UPDATE ... WHERE EXISTS (SQLite does not support FROM in UPDATE).
//
// SEAM CAVEAT (Phase 1/8): after this migration the Postgres schema declares
// sessions.tenant_id as UUID while session.rs still maps it as String. Entity
// reads on Postgres will produce a decode error until Phase 2 (Scopable entity
// updates) lands. SQLite is unaffected (typeless storage).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Child tables that receive owner_tenant_id / owner_id columns.
///
/// `message_reactions` is intentionally excluded: it is an unrestricted,
/// trust-parent table (row access governed by parent-message authorization),
/// so it carries no owner columns. Its `user_id` cast to UUID below is a
/// separate concern (reactor identity / storage consistency), not scoping.
const OWNER_CHILD_TABLES: [&str; 5] = [
    "messages",
    "message_parts",
    "file_citations",
    "link_citations",
    "link_references",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = manager.get_database_backend();

        // ------------------------------------------------------------------ //
        // Step A — Pre-check and cast sessions.tenant_id / sessions.user_id  //
        // ------------------------------------------------------------------ //

        // Guard 1: abort on any NULL values (NULLs cast to NULL and would
        // violate the NOT NULL invariant; they are not caught by the cast guard).
        let null_count: i64 = {
            let row = db
                .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                    backend,
                    "SELECT COUNT(*) AS cnt FROM sessions \
                     WHERE tenant_id IS NULL OR user_id IS NULL",
                ))
                .await?;
            row.map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                .unwrap_or(0)
        };
        if null_count > 0 {
            return Err(DbErr::Custom(format!(
                "Migration aborted: {null_count} row(s) in `sessions` have NULL \
                 tenant_id or user_id. All rows must carry non-NULL UUID-formatted \
                 values before this migration can proceed."
            )));
        }

        // Guard 2: abort on non-UUID-castable text values.
        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                let bad_count: i64 = {
                    let row = db
                        .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                            backend,
                            "SELECT COUNT(*) AS cnt FROM sessions \
                             WHERE tenant_id !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' \
                             OR user_id   !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'",
                        ))
                        .await?;
                    row.map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                        .unwrap_or(0)
                };
                if bad_count > 0 {
                    return Err(DbErr::Custom(format!(
                        "Migration aborted: {bad_count} row(s) in `sessions` have \
                         tenant_id or user_id values that cannot be cast to UUID."
                    )));
                }

                db.execute_unprepared(
                    "ALTER TABLE sessions \
                     ALTER COLUMN tenant_id TYPE UUID USING tenant_id::UUID",
                )
                .await?;
                db.execute_unprepared(
                    "ALTER TABLE sessions \
                     ALTER COLUMN user_id TYPE UUID USING user_id::UUID",
                )
                .await?;

                // message_reactions.user_id (reactor identity, a composite-PK
                // member) is also stored as UUID-formatted text sourced from
                // SecurityContext. The Scopable entity declares it as `Uuid`, so
                // the physical column must be cast to match, or Postgres reads
                // decode-fail. (Reactor identity is attribution, not a scoping
                // key — its type is Uuid purely for storage consistency.)
                let bad_reaction: i64 = {
                    let row = db
                        .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                            backend,
                            "SELECT COUNT(*) AS cnt FROM message_reactions \
                             WHERE user_id !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'",
                        ))
                        .await?;
                    row.map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                        .unwrap_or(0)
                };
                if bad_reaction > 0 {
                    return Err(DbErr::Custom(format!(
                        "Migration aborted: {bad_reaction} row(s) in `message_reactions` \
                         have user_id values that cannot be cast to UUID."
                    )));
                }
                db.execute_unprepared(
                    "ALTER TABLE message_reactions \
                     ALTER COLUMN user_id TYPE UUID USING user_id::UUID",
                )
                .await?;
            }
            sea_orm::DatabaseBackend::Sqlite => {
                // SQLite is typeless; the format guard (regex check) is the
                // enforcement mechanism. No DDL change is needed or possible.
                let bad_count: i64 = {
                    let row = db
                        .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                            backend,
                            "SELECT COUNT(*) AS cnt FROM sessions \
                             WHERE length(tenant_id) != 36 OR length(user_id) != 36 \
                             OR substr(tenant_id,9,1) != '-' OR substr(user_id,9,1) != '-'",
                        ))
                        .await?;
                    row.map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                        .unwrap_or(0)
                };
                if bad_count > 0 {
                    return Err(DbErr::Custom(format!(
                        "Migration aborted: {bad_count} row(s) in `sessions` have \
                         tenant_id or user_id values that do not match UUID format."
                    )));
                }
                // No DDL change for SQLite — typeless storage.
            }
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(
                    "Migration m20260417_000006 does not support MySQL (ADR-0019).".into(),
                ));
            }
        }

        // Composite index on sessions owner pair.
        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_owner")
                    .table(Alias::new("sessions"))
                    .col(Alias::new("tenant_id"))
                    .col(Alias::new("user_id"))
                    .to_owned(),
            )
            .await?;

        // ------------------------------------------------------------------ //
        // Step B — Add nullable owner columns to all child tables             //
        // ------------------------------------------------------------------ //

        let col_type = match backend {
            sea_orm::DatabaseBackend::Postgres => "UUID",
            // SQLite is typeless; TEXT stores UUID strings portably.
            _ => "TEXT",
        };

        for table in OWNER_CHILD_TABLES {
            db.execute_unprepared(&format!(
                "ALTER TABLE {table} ADD COLUMN owner_tenant_id {col_type}"
            ))
            .await?;
            db.execute_unprepared(&format!(
                "ALTER TABLE {table} ADD COLUMN owner_id {col_type}"
            ))
            .await?;
        }

        // ------------------------------------------------------------------ //
        // Step C — Backfill owner columns in parent-to-child chain order      //
        // ------------------------------------------------------------------ //

        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                // messages ← sessions
                db.execute_unprepared(
                    "UPDATE messages m \
                     SET owner_tenant_id = s.tenant_id, \
                         owner_id        = s.user_id \
                     FROM sessions s \
                     WHERE m.session_id = s.session_id",
                )
                .await?;

                let orphans: i64 = db
                    .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                        backend,
                        "SELECT COUNT(*) AS cnt FROM messages \
                         WHERE owner_tenant_id IS NULL",
                    ))
                    .await?
                    .map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                    .unwrap_or(0);
                if orphans > 0 {
                    return Err(DbErr::Custom(format!(
                        "Migration aborted: {orphans} row(s) in `messages` have no \
                         matching session (orphan rows). Remove or fix them before \
                         running this migration."
                    )));
                }

                // message_parts ← messages
                db.execute_unprepared(
                    "UPDATE message_parts mp \
                     SET owner_tenant_id = m.owner_tenant_id, \
                         owner_id        = m.owner_id \
                     FROM messages m \
                     WHERE mp.message_id = m.message_id",
                )
                .await?;

                let orphans: i64 = db
                    .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                        backend,
                        "SELECT COUNT(*) AS cnt FROM message_parts \
                         WHERE owner_tenant_id IS NULL",
                    ))
                    .await?
                    .map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                    .unwrap_or(0);
                if orphans > 0 {
                    return Err(DbErr::Custom(format!(
                        "Migration aborted: {orphans} orphan row(s) in `message_parts`."
                    )));
                }

                // citation / reference tables ← message_parts
                for table in ["file_citations", "link_citations", "link_references"] {
                    db.execute_unprepared(&format!(
                        "UPDATE {table} ct \
                         SET owner_tenant_id = mp.owner_tenant_id, \
                             owner_id        = mp.owner_id \
                         FROM message_parts mp \
                         WHERE ct.message_part_id = mp.id"
                    ))
                    .await?;

                    let orphans: i64 = db
                        .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                            backend,
                            format!(
                                "SELECT COUNT(*) AS cnt FROM {table} \
                                 WHERE owner_tenant_id IS NULL"
                            ),
                        ))
                        .await?
                        .map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                        .unwrap_or(0);
                    if orphans > 0 {
                        return Err(DbErr::Custom(format!(
                            "Migration aborted: {orphans} orphan row(s) in `{table}`."
                        )));
                    }
                }

            }
            sea_orm::DatabaseBackend::Sqlite => {
                // SQLite does not support UPDATE ... FROM; use correlated subquery.

                // messages ← sessions
                db.execute_unprepared(
                    "UPDATE messages SET \
                     owner_tenant_id = (SELECT tenant_id FROM sessions WHERE sessions.session_id = messages.session_id), \
                     owner_id        = (SELECT user_id   FROM sessions WHERE sessions.session_id = messages.session_id)",
                )
                .await?;

                let orphans: i64 = db
                    .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                        backend,
                        "SELECT COUNT(*) AS cnt FROM messages WHERE owner_tenant_id IS NULL",
                    ))
                    .await?
                    .map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                    .unwrap_or(0);
                if orphans > 0 {
                    return Err(DbErr::Custom(format!(
                        "Migration aborted: {orphans} orphan row(s) in `messages`."
                    )));
                }

                // message_parts ← messages
                db.execute_unprepared(
                    "UPDATE message_parts SET \
                     owner_tenant_id = (SELECT owner_tenant_id FROM messages WHERE messages.message_id = message_parts.message_id), \
                     owner_id        = (SELECT owner_id        FROM messages WHERE messages.message_id = message_parts.message_id)",
                )
                .await?;

                let orphans: i64 = db
                    .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                        backend,
                        "SELECT COUNT(*) AS cnt FROM message_parts WHERE owner_tenant_id IS NULL",
                    ))
                    .await?
                    .map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                    .unwrap_or(0);
                if orphans > 0 {
                    return Err(DbErr::Custom(format!(
                        "Migration aborted: {orphans} orphan row(s) in `message_parts`."
                    )));
                }

                // citation / reference tables ← message_parts
                for table in ["file_citations", "link_citations", "link_references"] {
                    db.execute_unprepared(&format!(
                        "UPDATE {table} SET \
                         owner_tenant_id = (SELECT owner_tenant_id FROM message_parts WHERE message_parts.id = {table}.message_part_id), \
                         owner_id        = (SELECT owner_id        FROM message_parts WHERE message_parts.id = {table}.message_part_id)"
                    ))
                    .await?;

                    let orphans: i64 = db
                        .query_one(sea_orm_migration::sea_orm::Statement::from_string(
                            backend,
                            format!(
                                "SELECT COUNT(*) AS cnt FROM {table} WHERE owner_tenant_id IS NULL"
                            ),
                        ))
                        .await?
                        .map(|r| r.try_get::<i64>("", "cnt").unwrap_or(0))
                        .unwrap_or(0);
                    if orphans > 0 {
                        return Err(DbErr::Custom(format!(
                            "Migration aborted: {orphans} orphan row(s) in `{table}`."
                        )));
                    }
                }

            }
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(
                    "Migration m20260417_000006 does not support MySQL (ADR-0019).".into(),
                ));
            }
        }

        // ------------------------------------------------------------------ //
        // Step D — Set NOT NULL and add indexes                               //
        // ------------------------------------------------------------------ //

        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                for table in OWNER_CHILD_TABLES {
                    db.execute_unprepared(&format!(
                        "ALTER TABLE {table} ALTER COLUMN owner_tenant_id SET NOT NULL"
                    ))
                    .await?;
                    db.execute_unprepared(&format!(
                        "ALTER TABLE {table} ALTER COLUMN owner_id SET NOT NULL"
                    ))
                    .await?;
                }
            }
            sea_orm::DatabaseBackend::Sqlite => {
                // SQLite cannot add NOT NULL to an existing column via ALTER TABLE.
                // NOT NULL is enforced only on new inserts through application-level
                // constraints (Phase 2 Scopable derive) and the backfill above.
            }
            sea_orm::DatabaseBackend::MySql => {}
        }

        // Named composite indexes on owner pair for each child table.
        let index_specs: [(&str, &str); 5] = [
            ("idx_messages_owner", "messages"),
            ("idx_parts_owner", "message_parts"),
            ("idx_file_citations_owner", "file_citations"),
            ("idx_link_citations_owner", "link_citations"),
            ("idx_link_references_owner", "link_references"),
        ];

        for (index_name, table) in index_specs {
            manager
                .create_index(
                    Index::create()
                        .name(index_name)
                        .table(Alias::new(table))
                        .col(Alias::new("owner_tenant_id"))
                        .col(Alias::new("owner_id"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // This migration is irreversible. up() performs an in-place text→UUID
        // cast on sessions.tenant_id / sessions.user_id that cannot be safely
        // undone: converting the UUID columns back to TEXT would break any
        // downstream code written against the UUID type. Dropping the owner
        // columns and indexes while leaving that cast in place would report a
        // successful rollback while leaving the schema in an inconsistent,
        // half-reverted state. Fail loudly instead of doing partial work.
        Err(DbErr::Custom(
            "m20260417_000006_authz_owner_columns is irreversible: the \
             text→UUID cast on sessions.tenant_id / sessions.user_id cannot be \
             safely reverted"
                .to_owned(),
        ))
    }
}
