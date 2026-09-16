//! The `SeaORM` entities of the core six, exercised against the real migrated
//! schema on in-memory `SQLite`.
//!
//! Enum ↔ smallint numbering is pinned by the in-source tests beside
//! `entity/enums.rs`; what *this* file proves is the part those cannot — that
//! every column name, Rust type, nullability and primary-key shape actually
//! matches the DDL. A mistyped column name or a `bool` where the DDL has a
//! smallint compiles fine and only fails here.
//!
//! The timestamp round-trip is the specific risk worth a test: `database.sql`'s
//! `timestamptz` lowers to `TEXT` on `SQLite`, so writing an `OffsetDateTime`
//! through `SeaORM` and reading it back is a real conversion, not a no-op.

// `disallowed_methods` steers production code onto `SecureSelect`, which is the
// right default and the wrong tool here: this file asserts that a column name,
// Rust type and nullability match the DDL, so it queries a bare
// `DatabaseConnection` with no scope machinery in the way. Adding a scope would
// test the scope, not the entity.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::disallowed_methods
)]

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, Database, DatabaseConnection,
    EntityTrait, QueryFilter, Statement,
};
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_gts::gts_id;
use uuid::Uuid;

use types_registry::infra::storage::Migrator;
use types_registry::infra::storage::entity::enums::{
    EntityKind, LifecycleStatus, OperationItemStatus, OperationKind, OperationStatus,
    OwnershipScope, Plane,
};
use types_registry::infra::storage::entity::{
    entity, instance, instance_revision, operation, operation_item, type_schema,
    type_schema_revision, version_family,
};

const CREATED: OffsetDateTime = datetime!(2026-08-18 09:15:30 UTC);
const UPDATED: OffsetDateTime = datetime!(2026-08-18 10:20:40 UTC);
const GTS_TYPE: &str = gts_id!("acme.crm.customer.type.v1~");
const INSTANCE_GTS_ID: &str = gts_id!("acme.crm.customer.type.v1~acme.crm.customers.first.v1");
const FAMILY_KEY: &str = "gts.acme.crm.customer.type";

async fn migrated_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        "PRAGMA foreign_keys = ON;".to_owned(),
    ))
    .await
    .expect("enable foreign keys");
    Migrator::up(&db, None).await.expect("apply the migration");
    db
}

/// One global family; returns its database-assigned id.
async fn insert_family(db: &DatabaseConnection) -> i64 {
    let model = version_family::ActiveModel {
        family_key: Set(FAMILY_KEY.to_owned()),
        ownership_scope: Set(OwnershipScope::Global),
        owner_tenant_id: Set(None),
        created_at: Set(CREATED),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert version family");
    assert_eq!(model.family_key, FAMILY_KEY);
    assert_eq!(model.ownership_scope, OwnershipScope::Global);
    model.id
}

#[tokio::test]
async fn version_family_round_trips_through_the_migrated_schema() {
    let db = migrated_db().await;
    let id = insert_family(&db).await;

    let found = version_family::Entity::find_by_id(id)
        .one(&db)
        .await
        .expect("query family")
        .expect("the row just inserted");
    assert_eq!(found.family_key, FAMILY_KEY);
    assert_eq!(found.ownership_scope, OwnershipScope::Global);
    assert_eq!(found.owner_tenant_id, None);
    // `timestamptz` lowers to TEXT on SQLite, so this is a real conversion.
    assert_eq!(found.created_at, CREATED);
}

#[tokio::test]
async fn entity_round_trips_including_its_nullable_tenant_and_tombstone_columns() {
    let db = migrated_db().await;
    let family_id = insert_family(&db).await;
    let gts_uuid = Uuid::from_u128(0xC1);

    let inserted = entity::ActiveModel {
        gts_uuid: Set(gts_uuid),
        gts_id: Set(GTS_TYPE.to_owned()),
        entity_kind: Set(EntityKind::TypeSchema),
        family_id: Set(family_id),
        ownership_scope: Set(OwnershipScope::Global),
        owner_tenant_id: Set(None),
        owning_gear: Set(Some("types-registry".to_owned())),
        lifecycle_status: Set(LifecycleStatus::Active),
        resource_version: Set(1),
        deleted_at: Set(None),
        created_at: Set(CREATED),
        updated_at: Set(UPDATED),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert entity");

    let found = entity::Entity::find()
        .filter(entity::Column::GtsId.eq(GTS_TYPE))
        .one(&db)
        .await
        .expect("query entity")
        .expect("the row just inserted");
    assert_eq!(found.id, inserted.id);
    assert_eq!(found.gts_uuid, gts_uuid);
    assert_eq!(found.entity_kind, EntityKind::TypeSchema);
    assert_eq!(found.lifecycle_status, LifecycleStatus::Active);
    assert_eq!(found.owning_gear.as_deref(), Some("types-registry"));
    assert_eq!(found.owner_tenant_id, None);
    assert_eq!(found.deleted_at, None);
    assert_eq!(found.resource_version, 1);
    assert_eq!(found.created_at, CREATED);
    assert_eq!(found.updated_at, UPDATED);
}

/// A tombstone: `lifecycle_status = deleted` with `deleted_at` set. The pair is
/// what `ck_tr_entity_lifecycle` constrains, so writing it through the entity
/// proves the mapping agrees with the constraint.
#[tokio::test]
async fn entity_writes_a_tombstone_the_lifecycle_check_accepts() {
    let db = migrated_db().await;
    let family_id = insert_family(&db).await;

    entity::ActiveModel {
        gts_uuid: Set(Uuid::from_u128(0xC2)),
        gts_id: Set(GTS_TYPE.to_owned()),
        entity_kind: Set(EntityKind::TypeSchema),
        family_id: Set(family_id),
        ownership_scope: Set(OwnershipScope::Global),
        owner_tenant_id: Set(None),
        owning_gear: Set(Some("types-registry".to_owned())),
        lifecycle_status: Set(LifecycleStatus::Deleted),
        resource_version: Set(2),
        deleted_at: Set(Some(UPDATED)),
        created_at: Set(CREATED),
        updated_at: Set(UPDATED),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("a deleted entity carrying deleted_at is admissible");
}

/// One accepted platform-plane registration and its single candidate, written
/// through the entities and read back. Covers the two `bytea` columns, the
/// `boolean`, the uuid primary key that is not auto-increment, and every
/// nullable transition timestamp.
#[tokio::test]
async fn operation_and_its_item_round_trip_together() {
    let db = migrated_db().await;
    let op_id = Uuid::from_u128(0xA1);
    let principal = Uuid::from_u128(0xB1);
    let scope_hash = vec![0x11_u8, 0x22, 0x33];
    let fingerprint = vec![0xAA_u8, 0xBB];

    operation::ActiveModel {
        id: Set(op_id),
        kind: Set(OperationKind::Registration),
        dry_run: Set(false),
        plane: Set(Plane::Platform),
        tenant_id: Set(None),
        principal_id: Set(principal),
        idempotency_key: Set("idem-1".to_owned()),
        idempotency_scope_hash: Set(scope_hash.clone()),
        request_fingerprint: Set(fingerprint.clone()),
        status: Set(OperationStatus::Pending),
        created_at: Set(CREATED),
        started_at: Set(None),
        completed_at: Set(None),
    }
    .insert(&db)
    .await
    .expect("insert operation");

    let item = operation_item::ActiveModel {
        operation_id: Set(op_id),
        item_no: Set(0),
        gts_id: Set(GTS_TYPE.to_owned()),
        dry_run: Set(false),
        kind: Set(OperationKind::Registration),
        expected_resource_version: Set(0),
        status: Set(OperationItemStatus::Pending),
        request_payload: Set(Some("{}".to_owned())),
        result_revision_no: Set(None),
        result_resource_version: Set(None),
        error_payload: Set(None),
        created_at: Set(CREATED),
        started_at: Set(None),
        completed_at: Set(None),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert operation item");

    let found_op = operation::Entity::find_by_id(op_id)
        .one(&db)
        .await
        .expect("query operation")
        .expect("the row just inserted");
    assert_eq!(found_op.kind, OperationKind::Registration);
    assert_eq!(found_op.plane, Plane::Platform);
    assert_eq!(found_op.status, OperationStatus::Pending);
    assert!(!found_op.dry_run);
    assert_eq!(found_op.idempotency_scope_hash, scope_hash);
    assert_eq!(found_op.request_fingerprint, fingerprint);
    assert_eq!(found_op.started_at, None);
    assert_eq!(found_op.completed_at, None);

    let found_item = operation_item::Entity::find_by_id(item.id)
        .one(&db)
        .await
        .expect("query item")
        .expect("the row just inserted");
    assert_eq!(found_item.operation_id, op_id);
    assert_eq!(found_item.status, OperationItemStatus::Pending);
    assert_eq!(found_item.kind, OperationKind::Registration);
    assert_eq!(found_item.request_payload.as_deref(), Some("{}"));
    assert_eq!(found_item.expected_resource_version, 0);
}

/// The item's terminal shape: `succeeded`, payload dropped, revision and
/// resource version written. `ck_tr_operation_item_state` accepts exactly this,
/// so a successful update is also evidence the entity's nullability is right.
#[tokio::test]
async fn operation_item_advances_to_a_succeeded_terminal_shape() {
    let db = migrated_db().await;
    let op_id = Uuid::from_u128(0xA2);
    operation::ActiveModel {
        id: Set(op_id),
        kind: Set(OperationKind::Registration),
        dry_run: Set(false),
        plane: Set(Plane::Platform),
        tenant_id: Set(None),
        principal_id: Set(Uuid::from_u128(0xB1)),
        idempotency_key: Set("idem-2".to_owned()),
        idempotency_scope_hash: Set(vec![0x01]),
        request_fingerprint: Set(vec![0x02]),
        status: Set(OperationStatus::Pending),
        created_at: Set(CREATED),
        started_at: Set(None),
        completed_at: Set(None),
    }
    .insert(&db)
    .await
    .expect("insert operation");

    let item = operation_item::ActiveModel {
        operation_id: Set(op_id),
        item_no: Set(0),
        gts_id: Set(GTS_TYPE.to_owned()),
        dry_run: Set(false),
        kind: Set(OperationKind::Registration),
        expected_resource_version: Set(0),
        status: Set(OperationItemStatus::Pending),
        request_payload: Set(Some("{}".to_owned())),
        created_at: Set(CREATED),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert item");

    let mut terminal: operation_item::ActiveModel = item.into();
    terminal.status = Set(OperationItemStatus::Succeeded);
    terminal.request_payload = Set(None);
    terminal.result_revision_no = Set(Some(1));
    terminal.result_resource_version = Set(Some(1));
    terminal.started_at = Set(Some(CREATED));
    terminal.completed_at = Set(Some(UPDATED));
    let updated = terminal
        .update(&db)
        .await
        .expect("ck_tr_operation_item_state accepts a succeeded registration with its revision");
    assert_eq!(updated.status, OperationItemStatus::Succeeded);
    assert_eq!(updated.result_revision_no, Some(1));
    assert_eq!(updated.request_payload, None);
}

/// `fk_tr_instance_revision_schema` prevents a dangling schema-revision reference —
/// a provenance claim about nothing.
///
/// Foreign keys are enabled explicitly here: `SQLite` does not enforce them by
/// default, so without the pragma this would pass while proving nothing.
#[tokio::test]
async fn instance_revision_cannot_reference_a_missing_schema_revision() {
    let db = migrated_db().await;
    let family_id = insert_family(&db).await;
    let op_id = Uuid::from_u128(0xA7);

    operation::ActiveModel {
        id: Set(op_id),
        kind: Set(OperationKind::Registration),
        dry_run: Set(false),
        plane: Set(Plane::Platform),
        tenant_id: Set(None),
        principal_id: Set(Uuid::from_u128(0xB7)),
        idempotency_key: Set("idem-fk".to_owned()),
        idempotency_scope_hash: Set(vec![0x07]),
        request_fingerprint: Set(vec![0x08]),
        status: Set(OperationStatus::Completed),
        created_at: Set(CREATED),
        started_at: Set(Some(CREATED)),
        completed_at: Set(Some(UPDATED)),
    }
    .insert(&db)
    .await
    .expect("insert operation");

    let item = operation_item::ActiveModel {
        operation_id: Set(op_id),
        item_no: Set(0),
        gts_id: Set(INSTANCE_GTS_ID.to_owned()),
        dry_run: Set(false),
        kind: Set(OperationKind::Registration),
        expected_resource_version: Set(0),
        // Pending: the item's own state is irrelevant, and the CHECK pins the pairing.
        status: Set(OperationItemStatus::Pending),
        request_payload: Set(Some("{}".to_owned())),
        result_revision_no: Set(None),
        result_resource_version: Set(None),
        error_payload: Set(None),
        created_at: Set(CREATED),
        started_at: Set(None),
        completed_at: Set(None),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert operation item");

    let ent = entity::ActiveModel {
        gts_uuid: Set(Uuid::from_u128(0xC7)),
        gts_id: Set(INSTANCE_GTS_ID.to_owned()),
        entity_kind: Set(EntityKind::Instance),
        family_id: Set(family_id),
        ownership_scope: Set(OwnershipScope::Global),
        owner_tenant_id: Set(None),
        owning_gear: Set(Some("types-registry".to_owned())),
        lifecycle_status: Set(LifecycleStatus::Active),
        resource_version: Set(1),
        deleted_at: Set(None),
        created_at: Set(CREATED),
        updated_at: Set(UPDATED),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert entity");

    // No `type_schema_revision` row exists for this pair.
    let err = instance_revision::ActiveModel {
        entity_id: Set(ent.id),
        revision_no: Set(1),
        canonical_value: Set(r#"{"name":"orphan"}"#.to_owned()),
        content_hash: Set(vec![0x09]),
        type_schema_entity_id: Set(ent.id),
        type_schema_revision_no: Set(999),
        gts_spec_version: Set("0.13".to_owned()),
        gts_impl_version: Set("0.12.0".to_owned()),
        operation_item_id: Set(item.id),
        created_at: Set(CREATED),
        updated_at: Set(UPDATED),
    }
    .insert(&db)
    .await
    .expect_err("a dangling schema-revision reference must be refused");
    assert!(
        err.to_string().to_lowercase().contains("foreign key"),
        "expected a foreign-key violation, got: {err}"
    );

    // And nothing was written.
    assert!(
        instance_revision::Entity::find()
            .filter(instance_revision::Column::EntityId.eq(ent.id))
            .all(&db)
            .await
            .expect("read")
            .is_empty(),
    );
    assert!(
        instance::Entity::find()
            .filter(instance::Column::EntityId.eq(ent.id))
            .all(&db)
            .await
            .expect("read")
            .is_empty(),
    );
}

/// The composite-primary-key pair: an immutable revision and the current-state
/// pointer that references it.
#[tokio::test]
async fn type_schema_revision_and_current_pointer_round_trip() {
    let db = migrated_db().await;
    let family_id = insert_family(&db).await;
    let op_id = Uuid::from_u128(0xA3);

    operation::ActiveModel {
        id: Set(op_id),
        kind: Set(OperationKind::Registration),
        dry_run: Set(false),
        plane: Set(Plane::Platform),
        tenant_id: Set(None),
        principal_id: Set(Uuid::from_u128(0xB1)),
        idempotency_key: Set("idem-3".to_owned()),
        idempotency_scope_hash: Set(vec![0x03]),
        request_fingerprint: Set(vec![0x04]),
        status: Set(OperationStatus::Completed),
        created_at: Set(CREATED),
        started_at: Set(Some(CREATED)),
        completed_at: Set(Some(UPDATED)),
    }
    .insert(&db)
    .await
    .expect("insert operation");

    let item = operation_item::ActiveModel {
        operation_id: Set(op_id),
        item_no: Set(0),
        gts_id: Set(GTS_TYPE.to_owned()),
        dry_run: Set(false),
        kind: Set(OperationKind::Registration),
        expected_resource_version: Set(0),
        status: Set(OperationItemStatus::Succeeded),
        request_payload: Set(None),
        result_revision_no: Set(Some(1)),
        result_resource_version: Set(Some(1)),
        error_payload: Set(None),
        created_at: Set(CREATED),
        started_at: Set(Some(CREATED)),
        completed_at: Set(Some(UPDATED)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert item");

    let ent = entity::ActiveModel {
        gts_uuid: Set(Uuid::from_u128(0xC3)),
        gts_id: Set(GTS_TYPE.to_owned()),
        entity_kind: Set(EntityKind::TypeSchema),
        family_id: Set(family_id),
        ownership_scope: Set(OwnershipScope::Global),
        owner_tenant_id: Set(None),
        owning_gear: Set(Some("types-registry".to_owned())),
        lifecycle_status: Set(LifecycleStatus::Active),
        resource_version: Set(1),
        deleted_at: Set(None),
        created_at: Set(CREATED),
        updated_at: Set(UPDATED),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert entity");

    let hash = vec![0xDE_u8, 0xAD, 0xBE, 0xEF];
    type_schema_revision::ActiveModel {
        entity_id: Set(ent.id),
        revision_no: Set(1),
        raw_schema: Set(r#"{"type":"object"}"#.to_owned()),
        content_hash: Set(hash.clone()),
        gts_spec_version: Set("0.13".to_owned()),
        gts_impl_version: Set("0.12.0".to_owned()),
        compat_forced: Set(false),
        operation_item_id: Set(item.id),
        created_at: Set(CREATED),
        updated_at: Set(UPDATED),
    }
    .insert(&db)
    .await
    .expect("insert type schema revision");

    let fingerprint = vec![0x99_u8];
    type_schema::ActiveModel {
        entity_id: Set(ent.id),
        revision_no: Set(1),
        resolved_schema: Set(r#"{"type":"object"}"#.to_owned()),
        effective_traits: Set("{}".to_owned()),
        effective_traits_schema: Set("{}".to_owned()),
        resolution_fingerprint: Set(fingerprint.clone()),
        created_at: Set(CREATED),
        updated_at: Set(UPDATED),
    }
    .insert(&db)
    .await
    .expect("insert current type schema");

    // The composite key is (entity_id, revision_no) — find_by_id takes the tuple.
    let revision = type_schema_revision::Entity::find_by_id((ent.id, 1))
        .one(&db)
        .await
        .expect("query revision")
        .expect("the row just inserted");
    assert_eq!(revision.content_hash, hash);
    assert_eq!(revision.gts_spec_version, "0.13");
    assert_eq!(revision.gts_impl_version, "0.12.0");
    assert!(!revision.compat_forced);
    assert_eq!(revision.operation_item_id, item.id);

    let current = type_schema::Entity::find_by_id(ent.id)
        .one(&db)
        .await
        .expect("query current type schema")
        .expect("the row just inserted");
    assert_eq!(current.revision_no, 1);
    assert_eq!(current.resolution_fingerprint, fingerprint);
    assert_eq!(current.effective_traits, "{}");
}

// ---------------------------------------------------------------------------
// Security dimensions. `#[secure(...)]` is not optional — the `Scopable` derive
// refuses to compile without an explicit decision for every dimension — but
// *which* decision was made is a choice a future edit could change silently.
// These assertions make ceiling C6 a test rather than only a comment: flipping
// any of the six to a tenant column without the `PolicyEnforcer` work fails
// here, which is the point at which someone should read the `ponytail:` note.
// ---------------------------------------------------------------------------

#[test]
fn every_core_entity_is_declared_unrestricted_while_ceiling_c6_stands() {
    use toolkit_db::secure::ScopableEntity;

    fn assert_unrestricted<E: ScopableEntity>(table: &str) {
        assert!(
            E::IS_UNRESTRICTED,
            "{table} is no longer `#[secure(unrestricted)]`; ceiling C6 (no PDP) says the \
             switch to `tenant_col` comes with the PolicyEnforcer work, not before it"
        );
        // `IS_UNRESTRICTED` makes every dimension column `None`; asserting it
        // here catches a half-migration that sets a column but leaves the flag.
        assert!(
            E::tenant_col().is_none(),
            "{table} declares a tenant column"
        );
        assert!(
            E::resource_col().is_none(),
            "{table} declares a resource column"
        );
        assert!(E::owner_col().is_none(), "{table} declares an owner column");
        assert!(E::type_col().is_none(), "{table} declares a type column");
    }

    assert_unrestricted::<version_family::Entity>("version_family");
    assert_unrestricted::<entity::Entity>("entity");
    assert_unrestricted::<type_schema_revision::Entity>("type_schema_revision");
    assert_unrestricted::<type_schema::Entity>("type_schema");
    assert_unrestricted::<operation::Entity>("operation");
    assert_unrestricted::<operation_item::Entity>("operation_item");
}
