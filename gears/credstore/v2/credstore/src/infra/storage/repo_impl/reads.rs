// Updated: 2026-10-06 by Constructor Tech
//! Read-only repo methods: `resolve_for_get`, `find_own`, `find_for_write`,
//! `scope_includes_tenant`, the create-time downward type check
//! (`list_tenants_with_other_type`), and the collection read's candidate queries
//! (`list_candidate_references`, `list_candidates_for_references` —
//! ADR-0005, ADR-0010).
//!
//! The three queries that read a record row for a secret read or a write
//! (`resolve_for_get`, `find_own`, `find_for_write`) also return the two heal
//! flags (ADR-0006, "Heal on access") in the same statement: correlated
//! `EXISTS` point lookups on `(tenant_id, record_id)`, never a `COUNT`.

use credstore_sdk::{OwnerId, SecretRef, SharingMode, TenantId};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, Condition, DbBackend, EntityTrait, FromQueryResult, Iterable, QueryOrder,
    QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, ScopeError, Scoped, SecureEntityExt, SecureSelect};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::authz::scope_admits_tenant;
use crate::domain::error::DomainError;
use crate::domain::secret::model::{HealFlags, SecretRow};
use crate::infra::canonical_mapping::classify_db_err_to_domain;
use crate::infra::storage::entity;
use crate::infra::storage::entity::enums::{DbFallback, DbSharing, DbStatus};
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, db_now_sql, entity_to_model, map_scope_err,
};

fn scope_err_to_domain(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Db(db) => classify_db_err_to_domain(db),
        other => map_scope_err(other),
    }
}

/// A `credstore_secrets` row with the two heal flags computed beside it.
#[derive(FromQueryResult)]
struct RowWithFlags {
    id: Uuid,
    tenant_id: Uuid,
    reference: String,
    sharing: DbSharing,
    owner_id: Uuid,
    status: DbStatus,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    version: i64,
    secret_type_uuid: Uuid,
    expires_at: Option<OffsetDateTime>,
    value_version: Option<String>,
    fallback: DbFallback,
    has_debts: bool,
    has_expired_intents: bool,
}

impl RowWithFlags {
    fn into_row(self) -> SecretRow {
        let heal = HealFlags {
            debts: self.has_debts,
            expired_intents: self.has_expired_intents,
        };
        let row = entity_to_model(entity::secrets::Model {
            id: self.id,
            tenant_id: self.tenant_id,
            reference: self.reference,
            sharing: self.sharing,
            owner_id: self.owner_id,
            status: self.status,
            created_at: self.created_at,
            updated_at: self.updated_at,
            version: self.version,
            secret_type_uuid: self.secret_type_uuid,
            expires_at: self.expires_at,
            value_version: self.value_version,
            fallback: self.fallback,
        });
        SecretRow { heal, ..row }
    }
}

/// Runs `select` projecting every column of the row plus the heal flags:
/// "this record has pending debts" and "this record has expired intents"
/// (`lease_until < now()` on the database clock), as correlated `EXISTS`
/// point lookups served by the `(tenant_id, record_id)` indexes of the two
/// bookkeeping tables. One statement: the scope clamp of `select` still
/// applies to the row.
async fn rows_with_flags(
    select: SecureSelect<entity::secrets::Entity, Scoped>,
    runner: &impl DBRunner,
    backend: DbBackend,
) -> Result<Vec<RowWithFlags>, DomainError> {
    let has_debts = Expr::cust(
        "EXISTS (SELECT 1 FROM credstore_store_cleanup c \
         WHERE c.tenant_id = credstore_secrets.tenant_id AND c.record_id = credstore_secrets.id)",
    );
    let has_expired_intents = Expr::cust(format!(
        "EXISTS (SELECT 1 FROM credstore_write_intents i \
         WHERE i.tenant_id = credstore_secrets.tenant_id AND i.record_id = credstore_secrets.id \
         AND i.lease_until < {})",
        db_now_sql(backend)
    ));
    select
        .project_all(runner, |sel| {
            sel.select_only()
                .columns(entity::secrets::Column::iter())
                .column_as(has_debts, "has_debts")
                .column_as(has_expired_intents, "has_expired_intents")
                .into_model::<RowWithFlags>()
        })
        .await
        .map_err(scope_err_to_domain)
}

/// Resolution-eligible statuses (ADR-0008, Resolution): `active` (expired
/// or not — expiry applies to the secret, not to the record, so an expired
/// record stays the decisive one and the service refuses to serve its
/// secret), or `declared` with `fallback = none` (a suppressing row that
/// competes and blocks regardless of any stale `expires_at` it carries — a
/// suppression policy does not expire). A `declared`/`inherit` row is
/// excluded by construction — it is simply not one of these two `(status,
/// fallback)` shapes.
fn resolution_eligible_condition() -> Condition {
    Condition::any()
        .add(Condition::all().add(entity::secrets::Column::Status.eq(DbStatus::Active)))
        .add(
            Condition::all()
                .add(entity::secrets::Column::Status.eq(DbStatus::Declared))
                .add(entity::secrets::Column::Fallback.eq(DbFallback::None)),
        )
}

/// Sharing-visibility predicate for `tenant`'s own rows: private rows only
/// for `subject`, tenant/shared rows visible to the whole tenant. Shared by
/// [`resolve_for_get`], [`resolve_candidates`] and (indirectly) `find_own`.
fn own_tenant_visibility_condition(tenant: Uuid, subject: Uuid) -> Condition {
    Condition::any()
        .add(
            Condition::all()
                .add(entity::secrets::Column::Sharing.eq(DbSharing::Private))
                .add(entity::secrets::Column::TenantId.eq(tenant))
                .add(entity::secrets::Column::OwnerId.eq(subject)),
        )
        .add(
            Condition::all()
                .add(entity::secrets::Column::Sharing.is_in([DbSharing::Tenant, DbSharing::Shared]))
                .add(entity::secrets::Column::TenantId.eq(tenant)),
        )
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
    // The predicate is `status = active OR (status = declared AND fallback =
    // none)` (ADR-0008, Resolution): a suppressing row competes and, when
    // nearest, wins — the walk stops there rather than falling through.
    let select = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(entity::secrets::Column::Reference.eq(key.as_ref())))
        // An expired `active` row stays eligible: it is the decisive record
        // and the service answers `SecretExpired` for its secret rather than
        // walking on to an ancestor's value.
        .filter(resolution_eligible_condition())
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
                        .add(entity::secrets::Column::Sharing.eq(DbSharing::Private))
                        .add(entity::secrets::Column::TenantId.eq(req))
                        .add(entity::secrets::Column::OwnerId.eq(subject.0)),
                )
                .add(entity::secrets::Column::Sharing.eq(DbSharing::Shared))
                .add(
                    Condition::all()
                        .add(entity::secrets::Column::Sharing.eq(DbSharing::Tenant))
                        .add(entity::secrets::Column::TenantId.eq(req)),
                ),
        );
    let rows = rows_with_flags(select, &conn, repo.db.db().backend()).await?;

    // Winner: closest tenant in chain; private beats non-private at same level.
    let pos = |t: Uuid| chain.iter().position(|c| *c == t).unwrap_or(usize::MAX);
    let best = rows.into_iter().min_by(|a, b| {
        pos(a.tenant_id)
            .cmp(&pos(b.tenant_id))
            .then((a.sharing != DbSharing::Private).cmp(&(b.sharing != DbSharing::Private)))
    });
    Ok(best.map(RowWithFlags::into_row))
}

/// Create-time upward type check: the record `req_tenant`'s reference
/// resolves to among **non-private** rows only — a `shared` row anywhere in
/// `chain` or a `tenant` row of `req_tenant` itself, under the same
/// resolution predicate as [`resolve_for_get`]. Private rows (the caller's
/// own included) never take part: they are exempt from type consistency.
/// Closest tenant in `chain` wins. Unscoped internal lookup, like
/// [`resolve_for_get`].
pub(super) async fn resolve_non_private(
    repo: &SecretRepoImpl,
    req_tenant: TenantId,
    key: &SecretRef,
    chain: &[Uuid],
) -> Result<Option<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    let req = req_tenant.0;
    let select = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(entity::secrets::Column::Reference.eq(key.as_ref())))
        .filter(resolution_eligible_condition())
        .filter(Condition::all().add(entity::secrets::Column::TenantId.is_in(chain.to_vec())))
        .filter(
            Condition::any()
                .add(entity::secrets::Column::Sharing.eq(DbSharing::Shared))
                .add(
                    Condition::all()
                        .add(entity::secrets::Column::Sharing.eq(DbSharing::Tenant))
                        .add(entity::secrets::Column::TenantId.eq(req)),
                ),
        );
    let rows = rows_with_flags(select, &conn, repo.db.db().backend()).await?;

    let pos = |t: Uuid| chain.iter().position(|c| *c == t).unwrap_or(usize::MAX);
    Ok(rows
        .into_iter()
        .min_by_key(|r| pos(r.tenant_id))
        .map(RowWithFlags::into_row))
}

/// Visibility predicate shared by [`resolve_candidates`] and the collection
/// read's candidate queries (ADR-0005): in the caller's own tenant, every
/// sharing-visible row of any status (so the record view can report a
/// `declared` own row's `status`/`fallback`/validator even though it never
/// resolves); in ancestor tenants, `shared` rows only, and only those that
/// pass [`resolution_eligible_condition`] — an ancestor's `declared`/
/// `inherit` row is invisible here exactly as it is to a value read.
fn chain_visibility_condition(req: Uuid, subject: Uuid, ancestors: &[Uuid]) -> Condition {
    let mut visibility = Condition::any().add(
        Condition::all()
            .add(entity::secrets::Column::TenantId.eq(req))
            .add(own_tenant_visibility_condition(req, subject)),
    );
    if !ancestors.is_empty() {
        visibility = visibility.add(
            Condition::all()
                .add(entity::secrets::Column::TenantId.is_in(ancestors.to_vec()))
                .add(entity::secrets::Column::Sharing.eq(DbSharing::Shared))
                .add(resolution_eligible_condition()),
        );
    }
    visibility
}

pub(super) async fn resolve_candidates(
    repo: &SecretRepoImpl,
    req_tenant: TenantId,
    subject: OwnerId,
    key: &SecretRef,
    chain: &[Uuid],
) -> Result<Vec<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    let req = req_tenant.0;
    let ancestors: Vec<Uuid> = chain.iter().copied().filter(|t| *t != req).collect();
    let visibility = chain_visibility_condition(req, subject.0, &ancestors);

    let rows = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(entity::secrets::Column::Reference.eq(key.as_ref())))
        .filter(Condition::all().add(entity::secrets::Column::TenantId.is_in(chain.to_vec())))
        .filter(visibility)
        .all(&conn)
        .await
        .map_err(scope_err_to_domain)?;

    Ok(rows.into_iter().map(entity_to_model).collect())
}

/// Row-shape helper for a `SELECT DISTINCT reference` projection.
#[derive(FromQueryResult)]
struct ReferenceRow {
    reference: String,
}

/// Row-shape helper for a `SELECT DISTINCT tenant_id` projection.
#[derive(FromQueryResult)]
struct TenantRow {
    tenant_id: Uuid,
}

/// Create-time downward type check: the distinct tenants other than
/// `exclude_tenant` holding `reference` with a type other than
/// `requested_type`, any status and owner, non-private rows only (private
/// records are exempt from type consistency). Unscoped (internal
/// lookup, like the create's own-row lookup), served by
/// `idx_credstore_lookup`; keyset-paged by `tenant_id`, never a `COUNT`.
pub(super) async fn list_tenants_with_other_type(
    repo: &SecretRepoImpl,
    reference: &SecretRef,
    requested_type: Uuid,
    exclude_tenant: TenantId,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<Uuid>, DomainError> {
    let conn = repo.db.conn()?;
    let mut filter = Condition::all()
        .add(entity::secrets::Column::Reference.eq(reference.as_ref()))
        .add(entity::secrets::Column::SecretTypeUuid.ne(requested_type))
        .add(entity::secrets::Column::Sharing.ne(DbSharing::Private))
        .add(entity::secrets::Column::TenantId.ne(exclude_tenant.0));
    if let Some(after) = after {
        filter = filter.add(entity::secrets::Column::TenantId.gt(after));
    }
    let rows: Vec<TenantRow> = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(filter)
        .project_all(&conn, |sel| {
            sel.select_only()
                .column(entity::secrets::Column::TenantId)
                .distinct()
                .order_by_asc(entity::secrets::Column::TenantId)
                .limit(limit)
                .into_model::<TenantRow>()
        })
        .await
        .map_err(scope_err_to_domain)?;
    Ok(rows.into_iter().map(|r| r.tenant_id).collect())
}

/// Collection read, step 1 (ADR-0005): candidate **references** visible
/// across `chain`, under [`chain_visibility_condition`], clamped by an exact
/// `reference` set when the caller's `$filter` named one, and by
/// `type_scope` - a type-only scope (`secret_type IN (...)`) the service
/// derived from ONE PDP decision on the base credential type, already
/// intersected with the caller's own `$filter type in (…)` (ADR-0010). The
/// scope is applied through the secure ORM, so the type predicate is
/// compiled to SQL exactly like any other PDP constraint. Both clamps are
/// invariant across a reference's chain, so both are sound SQL clamps
/// (ADR-0005 §"What stays out of the filter"). `DISTINCT reference`, ordered
/// by `reference` (`desc` when `desc`), keyset-paginated by `cursor`
/// (exclusive: `reference > cursor` ascending, `reference < cursor`
/// descending). Fetches at most `limit` references — the caller passes
/// `page_limit + 1` to detect a next page; the count reflects only
/// references admitted by the permitted-type clamp.
#[allow(
    clippy::too_many_arguments,
    reason = "every clamp the collection read's step 1 query supports, named rather than \
              bundled into an ad-hoc struct only this call site would use"
)]
pub(super) async fn list_candidate_references(
    repo: &SecretRepoImpl,
    req_tenant: TenantId,
    subject: OwnerId,
    chain: &[Uuid],
    reference_in: Option<&[String]>,
    type_scope: &AccessScope,
    cursor: Option<&str>,
    desc: bool,
    limit: u64,
) -> Result<Vec<String>, DomainError> {
    let conn = repo.db.conn()?;
    let req = req_tenant.0;
    let ancestors: Vec<Uuid> = chain.iter().copied().filter(|t| *t != req).collect();
    let visibility = chain_visibility_condition(req, subject.0, &ancestors);

    let mut filter = Condition::all()
        .add(entity::secrets::Column::TenantId.is_in(chain.to_vec()))
        .add(visibility);
    if let Some(refs) = reference_in {
        filter = filter.add(entity::secrets::Column::Reference.is_in(refs.to_vec()));
    }
    if let Some(after) = cursor {
        filter = filter.add(if desc {
            entity::secrets::Column::Reference.lt(after)
        } else {
            entity::secrets::Column::Reference.gt(after)
        });
    }

    let rows: Vec<ReferenceRow> = entity::secrets::Entity::find()
        .secure()
        .scope_with(type_scope)
        .filter(filter)
        .project_all(&conn, |sel| {
            let sel = sel
                .select_only()
                .column(entity::secrets::Column::Reference)
                .distinct();
            let sel = if desc {
                sel.order_by_desc(entity::secrets::Column::Reference)
            } else {
                sel.order_by_asc(entity::secrets::Column::Reference)
            };
            sel.limit(limit).into_model::<ReferenceRow>()
        })
        .await
        .map_err(scope_err_to_domain)?;

    Ok(rows.into_iter().map(|r| r.reference).collect())
}

/// Collection read, step 2 (ADR-0005): every visible row of `references`,
/// whole and unclamped by type, exactly as [`resolve_candidates`] would
/// return for each reference individually — so reduction sees every row a
/// value read would see, never fewer because a `$filter=type…`/authorization
/// clamp narrowed the candidate set before reduction ran.
pub(super) async fn list_candidates_for_references(
    repo: &SecretRepoImpl,
    req_tenant: TenantId,
    subject: OwnerId,
    chain: &[Uuid],
    references: &[String],
) -> Result<Vec<SecretRow>, DomainError> {
    if references.is_empty() {
        return Ok(Vec::new());
    }
    let conn = repo.db.conn()?;
    let req = req_tenant.0;
    let ancestors: Vec<Uuid> = chain.iter().copied().filter(|t| *t != req).collect();
    let visibility = chain_visibility_condition(req, subject.0, &ancestors);

    let rows = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(entity::secrets::Column::Reference.is_in(references.to_vec())))
        .filter(Condition::all().add(entity::secrets::Column::TenantId.is_in(chain.to_vec())))
        .filter(visibility)
        .all(&conn)
        .await
        .map_err(scope_err_to_domain)?;

    Ok(rows.into_iter().map(entity_to_model).collect())
}

pub(super) async fn find_own(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    tenant: TenantId,
    subject: OwnerId,
    key: &SecretRef,
) -> Result<Option<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    // Active or declared — there is no delete saga to resume any more
    // (ADR-0006: `delete_by_id` is one transaction), but a `declared` row is
    // a legitimate "own record" `patch`/`delete` must be able to find
    // (ADR-0004).
    let select = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Reference.eq(key.as_ref()))
                .add(entity::secrets::Column::TenantId.eq(tenant.0))
                .add(entity::secrets::Column::Status.is_in([DbStatus::Active, DbStatus::Declared]))
                .add(
                    Condition::any()
                        .add(
                            Condition::all()
                                .add(entity::secrets::Column::Sharing.eq(DbSharing::Private))
                                .add(entity::secrets::Column::OwnerId.eq(subject.0)),
                        )
                        .add(
                            entity::secrets::Column::Sharing
                                .is_in([DbSharing::Tenant, DbSharing::Shared]),
                        ),
                ),
        );
    let rows = rows_with_flags(select, &conn, repo.db.db().backend()).await?;

    // Prefer the private row if both exist.
    let best = rows
        .into_iter()
        .min_by_key(|r| i32::from(r.sharing != DbSharing::Private));
    Ok(best.map(RowWithFlags::into_row))
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
            .add(entity::secrets::Column::Sharing.eq(DbSharing::Private))
            .add(entity::secrets::Column::OwnerId.eq(subject.0))
    } else {
        Condition::all()
            .add(entity::secrets::Column::Sharing.is_in([DbSharing::Tenant, DbSharing::Shared]))
    };
    let select = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Reference.eq(key.as_ref()))
                .add(entity::secrets::Column::TenantId.eq(tenant.0))
                .add(entity::secrets::Column::Status.is_in([DbStatus::Active, DbStatus::Declared]))
                .add(class),
        )
        .limit(1);
    let rows = rows_with_flags(select, &conn, repo.db.db().backend()).await?;
    Ok(rows.into_iter().next().map(RowWithFlags::into_row))
}

pub(super) fn scope_includes_tenant(scope: &AccessScope, tenant: Uuid) -> bool {
    // Fail-closed own-tenant gate shared with the domain's row clamp: row
    // predicates (type, reference) narrow rows (applied by SQL), never the tenant.
    scope_admits_tenant(scope, tenant)
}
