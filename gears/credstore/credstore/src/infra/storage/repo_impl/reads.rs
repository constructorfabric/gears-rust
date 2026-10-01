//! Read-only repo methods: `resolve_for_get`, `find_own`, `find_for_write`,
//! `scope_includes_tenant`, and the collection read's candidate queries
//! (`list_candidate_references`, `list_candidates_for_references` —
//! ADR-0005, ADR-0010).

use credstore_sdk::{OwnerId, SecretRef, SharingMode, TenantId};
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, QueryOrder, QuerySelect};
use toolkit_db::secure::{ScopeError, SecureEntityExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::authz::scope_admits_tenant;
use crate::domain::error::DomainError;
use crate::domain::secret::model::{Fallback, SecretRow, SecretStatus};
use crate::infra::canonical_mapping::classify_db_err_to_domain;
use crate::infra::storage::entity;
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, entity_to_model, map_scope_err, sharing_to_i16,
};

fn scope_err_to_domain(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Db(db) => classify_db_err_to_domain(db),
        other => map_scope_err(other),
    }
}

/// Resolution-eligible statuses (ADR-0004, Suppression): `active` (expired
/// or not — expiry applies to the secret, not to the record, so an expired
/// record stays the decisive one and the service refuses to serve its
/// secret), or `declared` with `fallback = none` (a suppressing row that
/// competes and blocks regardless of any stale `expires_at` it carries — a
/// suppression policy does not expire). A `declared`/`inherit` row is
/// excluded by construction — it is simply not one of these two `(status,
/// fallback)` shapes.
fn resolution_eligible_condition() -> Condition {
    Condition::any()
        .add(
            Condition::all()
                .add(entity::secrets::Column::Status.eq(SecretStatus::Active.as_smallint())),
        )
        .add(
            Condition::all()
                .add(entity::secrets::Column::Status.eq(SecretStatus::Declared.as_smallint()))
                .add(entity::secrets::Column::Fallback.eq(Fallback::None.as_smallint())),
        )
}

/// Sharing-visibility predicate for `tenant`'s own rows: private rows only
/// for `subject`, tenant/shared rows visible to the whole tenant. Shared by
/// [`resolve_for_get`], [`resolve_candidates`] and (indirectly) `find_own`.
fn own_tenant_visibility_condition(tenant: Uuid, subject: Uuid) -> Condition {
    Condition::any()
        .add(
            Condition::all()
                .add(entity::secrets::Column::Sharing.eq(sharing_to_i16(SharingMode::Private)))
                .add(entity::secrets::Column::TenantId.eq(tenant))
                .add(entity::secrets::Column::OwnerId.eq(subject)),
        )
        .add(
            Condition::all()
                .add(entity::secrets::Column::Sharing.is_in([
                    sharing_to_i16(SharingMode::Tenant),
                    sharing_to_i16(SharingMode::Shared),
                ]))
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
    // none)` (ADR-0004, Suppression): a suppressing row competes and, when
    // nearest, wins — the walk stops there rather than falling through.
    let rows = entity::secrets::Entity::find()
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
                .add(entity::secrets::Column::Sharing.eq(sharing_to_i16(SharingMode::Shared)))
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

    rows.into_iter().map(entity_to_model).collect()
}

/// Row-shape helper for a `SELECT DISTINCT reference` projection.
#[derive(FromQueryResult)]
struct ReferenceRow {
    reference: String,
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
/// `page_limit + 1` in metadata mode to detect a next page, or
/// `secret_mode_cap + 1` in secret mode (no cursor, always ascending); both
/// counts reflect only references admitted by the permitted-type clamp.
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

    rows.into_iter().map(entity_to_model).collect()
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
    let rows = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Reference.eq(key.as_ref()))
                .add(entity::secrets::Column::TenantId.eq(tenant.0))
                .add(entity::secrets::Column::Status.is_in([
                    SecretStatus::Active.as_smallint(),
                    SecretStatus::Declared.as_smallint(),
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
                .add(entity::secrets::Column::Status.is_in([
                    SecretStatus::Active.as_smallint(),
                    SecretStatus::Declared.as_smallint(),
                ]))
                .add(class),
        )
        .one(&conn)
        .await
        .map_err(scope_err_to_domain)?;
    row.map(entity_to_model).transpose()
}

pub(super) fn scope_includes_tenant(scope: &AccessScope, tenant: Uuid) -> bool {
    // Fail-closed own-tenant gate shared with the domain's row clamp: row
    // predicates (type, reference) narrow rows (applied by SQL), never the tenant.
    scope_admits_tenant(scope, tenant)
}
