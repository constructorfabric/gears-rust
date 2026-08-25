//! `SeaORM`-backed implementation of [`MetadataRepo`].
//!
//! Mirrors the conventions established by the sibling [`TenantRepoImpl`]
//! split (`reads`, `lifecycle`, `updates`, `retention`,
//! [`ConversionRepoImpl`]): every method on the [`MetadataRepo`] trait is
//! dispatched to a `pub(super)` free function in this gear, every DB
//! call forwards the caller's [`AccessScope`] through `SecureORM` (the
//! `tenant_metadata` entity is declared `Scopable(tenant_col =
//! "tenant_id", no_resource, no_owner, no_type)` so a caller-built
//! `InTenantSubtree` scope clamps reads / writes to the caller's tenant
//! subtree via the secure-ORM closure subquery), and DB errors are
//! routed through the canonical-mapping classifier so domain code never
//! sees a raw `DbErr`.
//!
//! Two repo-specific behaviours are pinned here:
//!
//! * `upsert_for_tenant` runs a single SERIALIZABLE retry transaction
//!   that performs a SELECT-then-INSERT-or-UPDATE on the composite key
//!   `(tenant_id, schema_uuid)`. The path is engine-portable (`SeaORM`'s
//!   `OnConflict` builder is dialect-agnostic but UPSERT semantics
//!   require us to read back the post-write row anyway, so the SELECT-
//!   first form is simpler). On insert `created_at == updated_at ==
//!   now`; on update `created_at` is preserved and `updated_at` is
//!   bumped to `now`.
//! * `delete_for_tenant` is **idempotent** on missing rows: a
//!   `rows_affected == 0` outcome returns `Ok(())` (mirrors
//!   `delete_user` deprovision idempotency). The tenant-existence and
//!   schema-registration gates run upstream in the service layer, so a
//!   0-row outcome here unambiguously means "no direct entry at
//!   `(tenant_id, schema_uuid)`".
//!
//! Cascade-delete on tenant removal is owned by
//! `TenantRepoImpl::hard_delete_one`: that path issues a single in-TX
//! `delete_many` against `tenant_metadata` (dialect-portable; works on
//! PG and `SQLite` regardless of `PRAGMA foreign_keys`). The metadata
//! repo deliberately exposes no cascade-cleanup method.
//!
//! [`TenantRepoImpl`]: crate::infra::storage::repo_impl::TenantRepoImpl
//! [`ConversionRepoImpl`]: crate::infra::storage::repo_impl::ConversionRepoImpl
//! [`MetadataRepo`]: crate::domain::metadata::repo::MetadataRepo

use std::sync::Arc;

use account_management_sdk::MetadataEntryFilterField;
use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, paginate_odata,
};
use toolkit_db::secure::{
    DbTx, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metadata::repo::MetadataRepo;
use crate::domain::metadata::{MetadataRow, UpsertOutcome};
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::infra::storage::entity::tenant_metadata;

use super::AmDbProvider;
use super::helpers::{TxError, map_scope_err, map_scope_to_tx, with_serializable_retry};

/// `OData` mapper for `tenant_metadata`. Maps the public SDK filter
/// fields (`MetadataEntryFilterField` — `UpdatedAt` and `SchemaUuid`)
/// onto the underlying `SeaORM` columns and surfaces cursor values
/// for `paginate_odata`'s tiebreaker logic.
///
/// Mirrors the RG pattern in
/// `resource_group::infra::storage::odata_mapper`. The chained
/// `type_id` is not a filter field — exact-schema lookups go through
/// `get_metadata` instead; `SchemaUuid` is exposed so callers that
/// already hold the derived `UUIDv5` can pin a row directly.
struct MetadataODataMapper;

impl FieldToColumn<MetadataEntryFilterField> for MetadataODataMapper {
    type Column = tenant_metadata::Column;

    fn map_field(field: MetadataEntryFilterField) -> tenant_metadata::Column {
        match field {
            MetadataEntryFilterField::UpdatedAt => tenant_metadata::Column::UpdatedAt,
            MetadataEntryFilterField::SchemaUuid => tenant_metadata::Column::SchemaUuid,
        }
    }
}

impl ODataFieldMapping<MetadataEntryFilterField> for MetadataODataMapper {
    type Entity = tenant_metadata::Entity;

    fn extract_cursor_value(
        model: &tenant_metadata::Model,
        field: MetadataEntryFilterField,
    ) -> sea_orm::Value {
        match field {
            MetadataEntryFilterField::UpdatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.updated_at))
            }
            MetadataEntryFilterField::SchemaUuid => sea_orm::Value::Uuid(Some(model.schema_uuid)),
        }
    }
}

/// Pagination limits for the tenant-metadata listing surface.
///
/// `default = 50` mirrors the SDK's `IdpUserPagination::DEFAULT_TOP`
/// and `ListChildrenQuery::DEFAULT_TOP` so the AM listing endpoints
/// share one fallback page size. `max = 200` matches
/// `IdpUserPagination::MAX_TOP` to keep the platform-wide ceiling
/// uniform across CRUD surfaces.
const METADATA_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

/// Retry budget for the SELECT-then-INSERT race when
/// `with_serializable_retry` cannot absorb it (PG READ COMMITTED /
/// `SQLite` surface 23505/2067 raw, not as 40001). Next iteration's
/// SELECT sees the peer's row → UPDATE path.
const MAX_UPSERT_UNIQUE_VIOLATION_RETRIES: u8 = 3;

/// `SeaORM` repository adapter for [`MetadataRepo`].
///
/// Decision rule: a separate struct from [`super::TenantRepoImpl`] /
/// [`super::ConversionRepoImpl`] because `MetadataRepo` is a disjoint
/// trait — there is no shared state to factor and the storage layout is
/// independent (composite PK `(tenant_id, schema_uuid)`, no closure
/// touchpoints).
pub struct MetadataRepoImpl {
    db: Arc<AmDbProvider>,
}

impl MetadataRepoImpl {
    /// Build a new repo adapter over the shared AM DB provider.
    #[must_use]
    pub fn new(db: Arc<AmDbProvider>) -> Self {
        Self { db }
    }
}

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

/// Lift a [`tenant_metadata::Model`] row into the domain
/// [`MetadataRow`]. Pure projection — every column is preserved
/// verbatim, the only translation is `Json` → [`serde_json::Value`].
fn entity_to_row(row: tenant_metadata::Model) -> MetadataRow {
    MetadataRow {
        tenant_id: row.tenant_id,
        schema_uuid: row.schema_uuid,
        value: row.value,
        created_at: row.created_at,
        updated_at: row.updated_at,
        version: row.version,
    }
}

/// Build a `Condition` matching a metadata row by composite key.
fn pk_eq(tenant_id: Uuid, schema_uuid: Uuid) -> Condition {
    Condition::all()
        .add(tenant_metadata::Column::TenantId.eq(tenant_id))
        .add(tenant_metadata::Column::SchemaUuid.eq(schema_uuid))
}

// ---------------------------------------------------------------------------
// Free functions implementing each MetadataRepo method.
// ---------------------------------------------------------------------------

async fn list_for_tenant(
    repo: &MetadataRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    query: &ODataQuery,
) -> Result<Page<MetadataRow>, DomainError> {
    let conn = repo.db.conn()?;

    // Build the base SELECT: scoped through the secure-ORM seam
    // (clamps to the caller's tenant subtree via
    // `tenant_metadata.tenant_id IN (SELECT descendant FROM
    // tenant_closure ...)`) AND additionally filtered to
    // `tenant_id = <path-param>` so the listing surface is direct-on-
    // this-tenant only. The OData `$filter` (currently `updated_at`
    // only) is applied on top by `paginate_odata`.
    let base = tenant_metadata::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(tenant_metadata::Column::TenantId.eq(tenant_id)));

    // Stable tiebreaker on `schema_uuid ASC` keeps cursor re-reads
    // deterministic even when callers omit `$orderby` or order by
    // `updated_at` (where collisions are possible on the same wall
    // clock). The `paginate_odata` helper merges the tiebreaker into
    // the effective order automatically when absent.
    let page = paginate_odata::<MetadataEntryFilterField, MetadataODataMapper, _, _, _, _>(
        base,
        &conn,
        query,
        ("schema_uuid", SortDir::Asc),
        METADATA_LIMIT_CFG,
        entity_to_row,
    )
    .await
    .map_err(|e| DomainError::Validation {
        detail: format!("metadata list query rejected: {e}"),
    })?;

    Ok(page)
}

async fn get_for_tenant(
    repo: &MetadataRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    schema_uuid: Uuid,
) -> Result<Option<MetadataRow>, DomainError> {
    let conn = repo.db.conn()?;
    let row = tenant_metadata::Entity::find()
        .secure()
        // Same scope-forwarding posture as `list_for_tenant` — caller's
        // `InTenantSubtree` scope clamps the SELECT to the caller's
        // tenant subtree.
        .scope_with(scope)
        .filter(pk_eq(tenant_id, schema_uuid))
        .one(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(row.map(entity_to_row))
}

// @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-put:p1:inst-storage-upsert-impl
// @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-upsert-storage
async fn upsert_for_tenant(
    repo: &MetadataRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    schema_uuid: Uuid,
    value: Value,
    now: OffsetDateTime,
    expected_version: Option<i64>,
) -> Result<UpsertOutcome, DomainError> {
    // Two retry layers wrap this:
    // 1. Inner — `with_serializable_retry` absorbs lock contention
    //    (40001 / deadlock / BUSY).
    // 2. Outer — this loop re-enters on `AlreadyExists` (the
    //    SELECT-then-INSERT first-write race the inner helper does
    //    not classify); the next iteration's SELECT finds the peer's
    //    row and dispatches to UPDATE.
    let mut last_already_exists_detail: Option<String> = None;
    for attempt in 0..=MAX_UPSERT_UNIQUE_VIOLATION_RETRIES {
        match upsert_for_tenant_once(
            repo,
            scope,
            tenant_id,
            schema_uuid,
            value.clone(),
            now,
            expected_version,
        )
        .await
        {
            Ok(outcome) => return Ok(outcome),
            Err(DomainError::AlreadyExists { detail }) => {
                last_already_exists_detail = Some(detail);
                if attempt < MAX_UPSERT_UNIQUE_VIOLATION_RETRIES {
                    // Metric makes misclassification observable
                    // instead of silent looping.
                    emit_metric(
                        AM_DEPENDENCY_HEALTH,
                        MetricKind::Counter,
                        &[
                            ("target", "metadata_upsert"),
                            ("op", "unique_violation_race"),
                            ("outcome", "retry"),
                        ],
                    );
                    tracing::debug!(
                        target: "am.metadata",
                        tenant_id = %tenant_id,
                        schema_uuid = %schema_uuid,
                        attempt = attempt + 1,
                        "metadata upsert: unique-violation race, retrying as UPDATE-path"
                    );
                    continue;
                }
                // Final attempt also raced: budget exhausted. Break
                // out so the post-loop counter + error get emitted.
                break;
            }
            Err(e) => return Err(e),
        }
    }

    // Exhausted retries: surface AlreadyExists as the last seen
    // signal. Reaching this branch implies sustained concurrent
    // first-writes for the same `(tenant_id, schema_uuid)` — the
    // operator-visible counter captures that the retry budget did
    // not absorb the race, which is itself the actionable signal.
    tracing::warn!(
        target: "am.metadata",
        tenant_id = %tenant_id,
        schema_uuid = %schema_uuid,
        attempts = u32::from(MAX_UPSERT_UNIQUE_VIOLATION_RETRIES) + 1,
        "metadata upsert retry budget exhausted on unique-violation race"
    );
    emit_metric(
        AM_DEPENDENCY_HEALTH,
        MetricKind::Counter,
        &[
            ("target", "metadata_upsert"),
            ("op", "unique_violation_race"),
            ("outcome", "retries_exhausted"),
        ],
    );
    let last_detail = last_already_exists_detail.unwrap_or_else(|| "<unknown>".to_owned());
    Err(DomainError::AlreadyExists {
        detail: format!(
            "metadata upsert for ({tenant_id}, {schema_uuid}) failed after \
             {MAX_UPSERT_UNIQUE_VIOLATION_RETRIES} retry attempts on unique-constraint race; \
             last inner detail: {last_detail}"
        ),
    })
}

/// Single SELECT-then-INSERT-or-UPDATE pass under one
/// `with_serializable_retry` envelope. Extracted so the outer
/// unique-violation retry loop in `upsert_for_tenant` can re-call it
/// with a fresh transaction.
async fn upsert_for_tenant_once(
    repo: &MetadataRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    schema_uuid: Uuid,
    value: Value,
    now: OffsetDateTime,
    expected_version: Option<i64>,
) -> Result<UpsertOutcome, DomainError> {
    let scope_owned = scope.clone();
    let value_owned = value;
    with_serializable_retry(&repo.db, move || {
        let scope = scope_owned.clone();
        let value = value_owned.clone();
        Box::new(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                let existing = tenant_metadata::Entity::find()
                    .secure()
                    // Caller's scope (typically `InTenantSubtree`)
                    // clamps the SELECT to the caller's subtree; the
                    // upsert TX inherits the same authz fence used by
                    // `list_for_tenant`.
                    .scope_with(&scope)
                    .filter(pk_eq(tenant_id, schema_uuid))
                    .one(tx)
                    .await
                    .map_err(map_scope_to_tx)?;

                if let Some(existing) = existing {
                    // Optimistic-lock precondition: the caller asked
                    // for `expected_version`, so the stored row's
                    // version must match. Mismatch surfaces as
                    // `MetadataVersionMismatch` (HTTP 409) — distinct
                    // from `SerializationConflict` because the caller
                    // must re-read + decide, not blindly retry.
                    if let Some(expected) = expected_version
                        && existing.version != expected
                    {
                        return Err(TxError::Domain(DomainError::MetadataVersionMismatch {
                            entry: schema_uuid.to_string(),
                            expected,
                            current: existing.version,
                        }));
                    }
                    // UPDATE pins on version as a belt-and-braces
                    // guard — SERIALIZABLE already protects, but the
                    // filter keeps writes deterministic when the
                    // helper's retry budget is engaged.
                    let new_version = existing.version + 1;
                    let value_for_row = value.clone();
                    let res = tenant_metadata::Entity::update_many()
                        .col_expr(tenant_metadata::Column::Value, Expr::value(value))
                        .col_expr(tenant_metadata::Column::UpdatedAt, Expr::value(now))
                        .col_expr(tenant_metadata::Column::Version, Expr::value(new_version))
                        .filter(pk_eq(tenant_id, schema_uuid))
                        .filter(tenant_metadata::Column::Version.eq(existing.version))
                        .secure()
                        // Same caller-scope forwarding as the SELECT above.
                        .scope_with(&scope)
                        .exec(tx)
                        .await
                        .map_err(map_scope_to_tx)?;
                    if res.rows_affected == 0 {
                        // rows_affected == 0 ⇒ concurrent hard-delete
                        // or version drift; surface Internal so the
                        // timing-collision is operator-visible.
                        tracing::warn!(
                            target: "am.metadata",
                            tenant_id = %tenant_id,
                            schema_uuid = %schema_uuid,
                            "metadata upsert UPDATE affected 0 rows; concurrent hard-delete or version drift suspected"
                        );
                        return Err(TxError::Domain(DomainError::Internal {
                            diagnostic: format!(
                                "metadata upsert UPDATE affected 0 rows for ({tenant_id}, \
                                 {schema_uuid}); concurrent hard-delete or version drift suspected"
                            ),
                            cause: None,
                        }));
                    }
                    Ok(UpsertOutcome::Updated(MetadataRow {
                        tenant_id,
                        schema_uuid,
                        value: value_for_row,
                        created_at: existing.created_at,
                        updated_at: now,
                        version: new_version,
                    }))
                } else {
                    // INSERT path. `expected_version = Some(v != 0)`
                    // means the caller thought a row already existed —
                    // surface a mismatch with `current = 0` so the
                    // caller can re-read and either back off (the
                    // row was concurrently deleted) or seed a fresh
                    // entry (with `expected_version = None` /
                    // `Some(0)`).
                    if let Some(expected) = expected_version
                        && expected != 0
                    {
                        return Err(TxError::Domain(DomainError::MetadataVersionMismatch {
                            entry: schema_uuid.to_string(),
                            expected,
                            current: 0,
                        }));
                    }
                    // INSERT path: stamp `created_at == updated_at == now`,
                    // seed `version = 1`.
                    let am = tenant_metadata::ActiveModel {
                        tenant_id: ActiveValue::Set(tenant_id),
                        schema_uuid: ActiveValue::Set(schema_uuid),
                        value: ActiveValue::Set(value),
                        created_at: ActiveValue::Set(now),
                        updated_at: ActiveValue::Set(now),
                        version: ActiveValue::Set(1),
                    };
                    // scope_unchecked — PEP already authorised;
                    // secure-orm's scope_with_model would deny because
                    // InTenantSubtree scope yields no value()s on
                    // validate_insert. Mirrors
                    // lifecycle::insert_provisioning.
                    let model = tenant_metadata::Entity::insert(am)
                        .secure()
                        .scope_unchecked(&scope)
                        .map_err(map_scope_to_tx)?
                        .exec_with_returning(tx)
                        .await
                        .map_err(map_scope_to_tx)?;
                    Ok(UpsertOutcome::Inserted(entity_to_row(model)))
                }
            })
        })
    })
    .await
}
// @cpt-end:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-upsert-storage
// @cpt-end:cpt-cf-account-management-flow-tenant-metadata-put:p1:inst-storage-upsert-impl

// @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-delete:p1:inst-storage-delete-impl
async fn delete_for_tenant(
    repo: &MetadataRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
    schema_uuid: Uuid,
) -> Result<(), DomainError> {
    let conn = repo.db.conn()?;
    // Idempotent on missing rows: 0 rows_affected returns `Ok(())`.
    // The tenant-existence and schema-registration gates run upstream
    // in the service layer, so a 0-row outcome here unambiguously
    // means "no direct entry at (tenant_id, schema_uuid)".
    tenant_metadata::Entity::delete_many()
        .filter(pk_eq(tenant_id, schema_uuid))
        .secure()
        .scope_with(scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(())
}
// @cpt-end:cpt-cf-account-management-flow-tenant-metadata-delete:p1:inst-storage-delete-impl

// ---------------------------------------------------------------------------
// Trait dispatch.
// ---------------------------------------------------------------------------

#[async_trait]
impl MetadataRepo for MetadataRepoImpl {
    async fn list_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<MetadataRow>, DomainError> {
        list_for_tenant(self, scope, tenant_id, query).await
    }

    async fn get_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
    ) -> Result<Option<MetadataRow>, DomainError> {
        get_for_tenant(self, scope, tenant_id, schema_uuid).await
    }

    async fn upsert_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
        value: Value,
        now: OffsetDateTime,
        expected_version: Option<i64>,
    ) -> Result<UpsertOutcome, DomainError> {
        upsert_for_tenant(
            self,
            scope,
            tenant_id,
            schema_uuid,
            value,
            now,
            expected_version,
        )
        .await
    }

    async fn delete_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
    ) -> Result<(), DomainError> {
        delete_for_tenant(self, scope, tenant_id, schema_uuid).await
    }
}
