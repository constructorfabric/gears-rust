//! Read-only repo methods: `resolve_for_get`, `find_own`, `inventory`,
//! `scope_includes_tenant`.

use credstore_sdk::{OwnerId, SecretRef, SharingMode, TenantId};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, QuerySelect};
use toolkit_db::secure::{ScopeError, SecureEntityExt};
use toolkit_security::access_scope::ScopeFilter;
use toolkit_security::{AccessScope, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::SecretCounts;
use crate::domain::secret::model::{SecretRow, SecretStatus};
use crate::infra::canonical_mapping::classify_db_err_to_domain;
use crate::infra::storage::entity;
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, entity_to_model, map_scope_err, sharing_from_i16, sharing_to_i16,
};

/// Minimal projection for inventory aggregate rows.
#[derive(Debug, FromQueryResult)]
struct InventoryRow {
    sharing: i16,
    status: i16,
    c: i64,
}

/// Minimal projection for distinct-tenant count.
#[derive(Debug, FromQueryResult)]
struct TenantCount {
    t: i64,
}

fn scope_err_to_domain(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Db(db) => classify_db_err_to_domain(db),
        other => map_scope_err(other),
    }
}

pub(super) async fn resolve_for_get(
    repo: &SecretRepoImpl,
    req_tenant: TenantId,
    subject: OwnerId,
    key: &SecretRef,
    chain: &[Uuid],
) -> Result<Option<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    let req = req_tenant.0;
    // resolve_for_get applies its own chain + sharing predicates;
    // PDP authorization runs upstream. allow_all skips the scope WHERE clamp.
    let rows = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(entity::secrets::Column::Reference.eq(key.as_ref())))
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Status.eq(SecretStatus::Active.as_smallint())),
        )
        // Expired secrets resolve as not-found (write paths still see the
        // row: overwrite refreshes it, delete revokes it, the reaper sweeps it).
        .filter(
            Condition::any()
                .add(entity::secrets::Column::ExpiresAt.is_null())
                .add(entity::secrets::Column::ExpiresAt.gt(time::OffsetDateTime::now_utc())),
        )
        .filter(Condition::all().add(entity::secrets::Column::TenantId.is_in(chain.to_vec())))
        // Visibility by sharing class, within the ancestor `chain`:
        //   * Private — own tenant + owner only (`tenant_id == req AND
        //     owner_id == subject`). Private is owner-scoped per DESIGN §4.3 and
        //     never inherited, so we pin it to `req` here rather than lean on
        //     the authn-layer invariant that a `subject_id` belongs to a single
        //     tenant. That keeps disclosure closed even if a `subject_id` is
        //     ever reused across tenants or a cross-tenant principal is
        //     introduced (matches `find_own`/`find_for_write`, which already
        //     pin the tenant). `Shared` remains the sole inheritance vector.
        //   * Shared — inherited down the chain (any tenant in `chain`).
        //   * Tenant — own tenant only (`tenant_id == req`), never inherited.
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(
                            entity::secrets::Column::Sharing
                                .eq(sharing_to_i16(SharingMode::Private)),
                        )
                        .add(entity::secrets::Column::TenantId.eq(req))
                        .add(entity::secrets::Column::OwnerId.eq(subject.0)),
                )
                .add(entity::secrets::Column::Sharing.eq(sharing_to_i16(SharingMode::Shared)))
                .add(
                    Condition::all()
                        .add(
                            entity::secrets::Column::Sharing
                                .eq(sharing_to_i16(SharingMode::Tenant)),
                        )
                        .add(entity::secrets::Column::TenantId.eq(req)),
                ),
        )
        .all(&conn)
        .await
        .map_err(scope_err_to_domain)?;

    // Winner: closest tenant in chain; private beats non-private at same level.
    let pos = |t: Uuid| chain.iter().position(|c| *c == t).unwrap_or(usize::MAX);
    let best = rows.into_iter().min_by(|a, b| {
        pos(a.tenant_id).cmp(&pos(b.tenant_id)).then(
            (a.sharing != sharing_to_i16(SharingMode::Private))
                .cmp(&(b.sharing != sharing_to_i16(SharingMode::Private))),
        )
    });
    best.map(entity_to_model).transpose()
}

pub(super) async fn find_own(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    tenant: TenantId,
    subject: OwnerId,
    key: &SecretRef,
) -> Result<Option<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    // Active rows plus deprovisioning ones — a DELETE retry must be able to
    // resume a stuck delete saga. Provisioning rows stay invisible.
    let rows = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Reference.eq(key.as_ref()))
                .add(entity::secrets::Column::TenantId.eq(tenant.0))
                .add(entity::secrets::Column::Status.is_in([
                    SecretStatus::Active.as_smallint(),
                    SecretStatus::Deprovisioning.as_smallint(),
                ]))
                .add(
                    Condition::any()
                        .add(
                            Condition::all()
                                .add(
                                    entity::secrets::Column::Sharing
                                        .eq(sharing_to_i16(SharingMode::Private)),
                                )
                                .add(entity::secrets::Column::OwnerId.eq(subject.0)),
                        )
                        .add(entity::secrets::Column::Sharing.is_in([
                            sharing_to_i16(SharingMode::Tenant),
                            sharing_to_i16(SharingMode::Shared),
                        ])),
                ),
        )
        .all(&conn)
        .await
        .map_err(scope_err_to_domain)?;

    // Prefer the private row if both exist.
    let best = rows
        .into_iter()
        .min_by_key(|r| i32::from(r.sharing != sharing_to_i16(SharingMode::Private)));
    best.map(entity_to_model).transpose()
}

pub(super) async fn find_for_write(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    tenant: TenantId,
    subject: OwnerId,
    key: &SecretRef,
    sharing: SharingMode,
) -> Result<Option<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    // Address the row by the target sharing class only — the same identity the
    // partial unique indexes enforce. A private write must NOT match a coexisting
    // tenant/shared row (and vice-versa); they are distinct secrets per design.
    let class = if sharing == SharingMode::Private {
        Condition::all()
            .add(entity::secrets::Column::Sharing.eq(sharing_to_i16(SharingMode::Private)))
            .add(entity::secrets::Column::OwnerId.eq(subject.0))
    } else {
        Condition::all().add(entity::secrets::Column::Sharing.is_in([
            sharing_to_i16(SharingMode::Tenant),
            sharing_to_i16(SharingMode::Shared),
        ]))
    };
    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Reference.eq(key.as_ref()))
                .add(entity::secrets::Column::TenantId.eq(tenant.0))
                .add(entity::secrets::Column::Status.eq(SecretStatus::Active.as_smallint()))
                .add(class),
        )
        .one(&conn)
        .await
        .map_err(scope_err_to_domain)?;
    row.map(entity_to_model).transpose()
}

#[allow(clippy::cognitive_complexity)]
pub(super) async fn inventory(repo: &SecretRepoImpl) -> Result<SecretCounts, DomainError> {
    let conn = repo.db.conn()?;

    // Aggregate counts grouped by sharing + status.
    let rows: Vec<InventoryRow> = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .project_all(&conn, |q| {
            q.select_only()
                .column(entity::secrets::Column::Sharing)
                .column(entity::secrets::Column::Status)
                .column_as(entity::secrets::Column::Id.count(), "c")
                .group_by(entity::secrets::Column::Sharing)
                .group_by(entity::secrets::Column::Status)
                .into_model::<InventoryRow>()
        })
        .await
        .map_err(scope_err_to_domain)?;

    let mut counts = SecretCounts::default();
    for r in rows {
        // Decode through the typed enums (not magic numbers) so a future encoding
        // change can't silently miscount; an unknown encoding is logged, not dropped.
        match SecretStatus::from_smallint(r.status) {
            Some(SecretStatus::Provisioning) => counts.provisioning += r.c,
            Some(SecretStatus::Deprovisioning) => counts.deprovisioning += r.c,
            Some(SecretStatus::Active) => match sharing_from_i16(r.sharing) {
                Some(SharingMode::Private) => counts.private += r.c,
                Some(SharingMode::Tenant) => counts.tenant += r.c,
                Some(SharingMode::Shared) => counts.shared += r.c,
                None => tracing::warn!(
                    sharing = r.sharing,
                    "inventory: unknown sharing encoding, row not counted"
                ),
            },
            None => tracing::warn!(
                status = r.status,
                "inventory: unknown status encoding, row not counted"
            ),
        }
    }

    // Distinct-tenant count for active rows.
    let tenant_rows: Vec<TenantCount> = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Status.eq(SecretStatus::Active.as_smallint())),
        )
        .project_all(&conn, |q| {
            q.select_only()
                .column_as(
                    Expr::col(entity::secrets::Column::TenantId).count_distinct(),
                    "t",
                )
                .into_model::<TenantCount>()
        })
        .await
        .map_err(scope_err_to_domain)?;

    if let Some(row) = tenant_rows.into_iter().next() {
        counts.tenants = row.t;
    }
    Ok(counts)
}

pub(super) async fn scope_includes_tenant(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    tenant: Uuid,
) -> Result<bool, DomainError> {
    if scope.is_unconstrained() {
        return Ok(true);
    }
    if scope.is_deny_all() {
        return Ok(false);
    }
    // Fail-closed tenant-membership check. A scope's constraints are OR-ed
    // (alternative grants) and the filters within a constraint are AND-ed, so
    // a constraint admits `tenant` only when *every* one of its filters is a
    // tenant-level predicate on `OWNER_TENANT_ID` satisfied by `tenant`. Any
    // sibling filter that narrows below tenant granularity (`owner_id`,
    // `resource_id`, group membership, …) or any filter this gate cannot
    // evaluate makes the whole constraint non-admitting. That way a scope
    // stricter than tenant granularity fails closed (403) instead of being
    // silently widened to the whole tenant on a lone `OWNER_TENANT_ID` match.
    // The `InTenantSubtree` closure lookup hits `tenant_closure` directly, so
    // an empty `credstore_secrets` table never produces a false-negative.
    let conn = repo.db.conn()?;
    'constraints: for constraint in scope.constraints() {
        // An empty constraint matches everything (mirrors SecureORM's
        // `build_constraint_condition`, which compiles it to `WHERE true`).
        for filter in constraint.filters() {
            // Only `OWNER_TENANT_ID` predicates can affirm tenant-level access.
            if filter.property() != pep_properties::OWNER_TENANT_ID {
                continue 'constraints;
            }
            let admits = match filter {
                ScopeFilter::Eq(_) | ScopeFilter::In(_) => {
                    filter.values().iter().any(|v| v.as_uuid() == Some(tenant))
                }
                ScopeFilter::InTenantSubtree(sf) => match sf.root_tenant_id().as_uuid() {
                    None => false,
                    Some(root) => {
                        let mut closure_cond = Condition::all()
                            .add(entity::tenant_closure::Column::AncestorId.eq(root))
                            .add(entity::tenant_closure::Column::DescendantId.eq(tenant));
                        if sf.respect_barriers() {
                            closure_cond =
                                closure_cond.add(entity::tenant_closure::Column::Barrier.eq(0_i16));
                        }
                        entity::tenant_closure::Entity::find()
                            .secure()
                            .scope_with(&AccessScope::allow_all())
                            .filter(closure_cond)
                            .one(&conn)
                            .await
                            .map_err(scope_err_to_domain)?
                            .is_some()
                    }
                },
                // Group membership over `OWNER_TENANT_ID` is not a plain
                // tenant predicate this gate resolves — fail closed.
                ScopeFilter::InGroup(_) | ScopeFilter::InGroupSubtree(_) => false,
            };
            if !admits {
                continue 'constraints;
            }
        }
        // Every filter affirmed `tenant` (or the constraint was empty).
        return Ok(true);
    }
    Ok(false)
}
