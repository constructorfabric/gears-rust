#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

//! Common test utilities for types-registry integration tests.
//!
//! `dead_code` is allowed because each test binary includes this module whole and
//! uses only the helpers it needs.

use std::sync::Arc;

use gts::GtsConfig;
use types_registry::{
    config::TypesRegistryConfig, domain::service::TypesRegistryService,
    infra::InMemoryGtsRepository,
};

pub fn default_config() -> GtsConfig {
    TypesRegistryConfig::default().to_gts_config()
}

pub fn create_service() -> Arc<TypesRegistryService> {
    let repo = Arc::new(InMemoryGtsRepository::new(default_config()));
    Arc::new(TypesRegistryService::new(
        repo,
        TypesRegistryConfig::default(),
    ))
}

// ---------------------------------------------------------------------------
// Database harness
// ---------------------------------------------------------------------------

use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};

/// In-memory `SQLite` with the managed-state migration applied.
///
/// `max_conns = 1`: a bare `sqlite::memory:` gives every pooled connection its
/// own empty database, so a second connection would see no tables at all.
pub async fn test_db() -> Arc<DBProvider<DbError>> {
    provider_for("sqlite::memory:", 1).await
}

/// File-backed `SQLite` with a real pool, for the tests that need more than one
/// connection against the same database. The caller owns the temporary
/// directory and drops it to clean up.
pub async fn test_db_file(path: &std::path::Path) -> Arc<DBProvider<DbError>> {
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    provider_for(&dsn, 4).await
}

/// Any DSN with the managed-state migration applied. The `integration` suite
/// hands this a container DSN so the `PostgreSQL` and `MySQL` repository tests
/// exercise the same code path as the `SQLite` ones.
pub async fn provider_for(dsn: &str, max_conns: u32) -> Arc<DBProvider<DbError>> {
    let opts = ConnectOpts {
        max_conns: Some(max_conns),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, migrations())
        .await
        .expect("run migrations");
    Arc::new(DBProvider::new(db))
}

fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
    use sea_orm_migration::MigratorTrait;
    types_registry::infra::storage::Migrator::migrations()
}

/// The database-backed persistence ports, as the gear wires them. Tests that
/// drive `accept` / `run_operation` / `RegistryService` pass this: the domain names
/// only its ports, so the adapter is chosen here exactly as `init()` chooses it.
pub fn stores() -> Arc<dyn types_registry::domain::ports::Stores> {
    Arc::new(types_registry::infra::storage::Repos)
}

/// The scope every P0 read and write runs under. P0 has no PDP (ceiling C6) and
/// the entities are `#[secure(unrestricted)]`, so `allow_all` is the honest
/// value: a legitimate authorization outcome with no row-level filtering, not a
/// bypass. `AccessScope::default()` is deny-all, which is what an unset scope
/// would give.
pub fn allow_all() -> AccessScope {
    AccessScope::allow_all()
}

// ---------------------------------------------------------------------------
// Managed-state fixtures
// ---------------------------------------------------------------------------
//
// `type_schema` and `type_schema_revision` have no repository writer: those
// writes belong to the admission worker (T8), which is their first caller. Until
// then the tests that need admitted rows — the transient store (T5) and the
// backend suites — write them through their `ActiveModel`s, and share the
// boilerplate here rather than each carrying its own copy of the
// operation → item → revision → current-pointer chain.

use sea_orm::ActiveValue::Set;
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, secure_insert};
use types_registry::infra::storage::entity::enums::{
    OperationItemStatus, OperationKind, OperationStatus, Plane,
};
use types_registry::infra::storage::entity::{
    operation, operation_item, type_schema, type_schema_revision,
};
use uuid::Uuid;

/// A completed registration operation and its single item, which every revision
/// row needs: `type_schema_revision.operation_item_id` is a `RESTRICT` foreign
/// key pinning the admitting provenance. Returns the item id.
pub async fn seed_operation_item(
    runner: &impl DBRunner,
    gts_id: &str,
    revision_no: i32,
    now: OffsetDateTime,
) -> i64 {
    let scope = allow_all();
    let op_id = Uuid::new_v4();
    secure_insert::<operation::Entity>(
        operation::ActiveModel {
            id: Set(op_id),
            kind: Set(OperationKind::Registration),
            dry_run: Set(false),
            plane: Set(Plane::Platform),
            tenant_id: Set(None),
            principal_id: Set(Uuid::from_u128(0xB1)),
            idempotency_key: Set(format!("idem-{op_id}")),
            idempotency_scope_hash: Set(vec![0x01]),
            request_fingerprint: Set(vec![0x02]),
            status: Set(OperationStatus::Completed),
            created_at: Set(now),
            started_at: Set(Some(now)),
            completed_at: Set(Some(now)),
        },
        &scope,
        runner,
    )
    .await
    .expect("insert operation");

    let item = secure_insert::<operation_item::Entity>(
        operation_item::ActiveModel {
            operation_id: Set(op_id),
            item_no: Set(0),
            gts_id: Set(gts_id.to_owned()),
            dry_run: Set(false),
            kind: Set(OperationKind::Registration),
            expected_resource_version: Set(0),
            status: Set(OperationItemStatus::Succeeded),
            request_payload: Set(None),
            result_revision_no: Set(Some(revision_no)),
            result_resource_version: Set(Some(i64::from(revision_no))),
            error_payload: Set(None),
            created_at: Set(now),
            started_at: Set(Some(now)),
            completed_at: Set(Some(now)),
            ..Default::default()
        },
        &scope,
        runner,
    )
    .await
    .expect("insert operation item");
    item.id
}

/// One immutable authored revision.
pub async fn seed_type_schema_revision(
    runner: &impl DBRunner,
    entity_id: i64,
    revision_no: i32,
    operation_item_id: i64,
    raw_schema: &str,
    now: OffsetDateTime,
) {
    secure_insert::<type_schema_revision::Entity>(
        type_schema_revision::ActiveModel {
            entity_id: Set(entity_id),
            revision_no: Set(revision_no),
            raw_schema: Set(raw_schema.to_owned()),
            content_hash: Set(vec![u8::try_from(revision_no).expect("small revision")]),
            gts_spec_version: Set(gts::GTS_SPECIFICATION_VERSION.to_owned()),
            gts_impl_version: Set(gts::GTS_IMPLEMENTATION_VERSION.to_owned()),
            compat_forced: Set(false),
            operation_item_id: Set(operation_item_id),
            created_at: Set(now),
            updated_at: Set(now),
        },
        &allow_all(),
        runner,
    )
    .await
    .expect("insert type schema revision");
}

/// The current-state row pointing at a revision. The resolved artifacts are
/// placeholders: nothing under test reads them, because the transient store
/// resolves from the *authored* document (D3).
pub async fn seed_current_type_schema(
    runner: &impl DBRunner,
    entity_id: i64,
    revision_no: i32,
    resolved_schema: &str,
    now: OffsetDateTime,
) {
    secure_insert::<type_schema::Entity>(
        type_schema::ActiveModel {
            entity_id: Set(entity_id),
            revision_no: Set(revision_no),
            resolved_schema: Set(resolved_schema.to_owned()),
            effective_traits: Set("{}".to_owned()),
            effective_traits_schema: Set("{}".to_owned()),
            resolution_fingerprint: Set(vec![0x11]),
            created_at: Set(now),
            updated_at: Set(now),
        },
        &allow_all(),
        runner,
    )
    .await
    .expect("insert current type schema");
}
