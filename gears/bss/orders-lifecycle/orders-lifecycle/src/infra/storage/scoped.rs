//! Authorization-bound storage adapters (S2-03).
//!
//! Business persistence reaches the scoped repositories only through PEP-issued authorization
//! types ([`TargetAuthorization`], [`ProposedAuthorization`], [`CollectionScope`]). The only
//! locally built scopes are the approved point prefetch (target ID only, read-only, returns
//! facts and never a row) and the bounded private-persistence scopes below, each derived from an
//! already authorized order or the authenticated caller. None of them widens business access.
use super::entity::order;
use super::repo::{self, LockedOrder, TransactionRunner};
use crate::authz::{
    Arrangement, AuthorizationFacts, Caller, CollectionScope, Prefetch, ProposedAuthorization,
    TargetAuthorization,
};
use bss_orders_lifecycle_sdk::catalog::Reason;
use sea_orm::EntityTrait;
use toolkit_db::Db;
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, TxConfig};
use toolkit_security::access_scope::{AccessScope, ScopeConstraint, ScopeFilter};
use uuid::Uuid;

fn arrangement(row: &order::Model) -> Arrangement {
    Arrangement {
        resource_tenant_id: row.resource_tenant_id,
        seller_tenant_id: row.seller_tenant_id,
        payer_tenant_id: row.payer_tenant_id,
    }
}

/// Whether a PDP-compiled scope admits the given order facts, decided by `SecureORM`'s own
/// insert-time predicate evaluation over the aggregate's `pep_prop` mappings (OR over paths,
/// AND within a path). An unknown `order_id` (a not-yet-created order) is left unset, so only the
/// three axes are checked here; the complete model is checked again before persistence.
///
/// A decision whose constraints exclude the prefetched target is a denial for that target: it
/// takes the same follow-up and non-disclosing mapping as an explicit deny (08 §3.6 item 2).
pub fn scope_admits(scope: &AccessScope, order_id: Option<Uuid>, proposed: Arrangement) -> bool {
    use sea_orm::ActiveValue::{NotSet, Set};
    let model = order::ActiveModel {
        order_id: order_id.map_or(NotSet, Set),
        resource_tenant_id: Set(proposed.resource_tenant_id),
        payer_tenant_id: Set(proposed.payer_tenant_id),
        seller_tenant_id: Set(proposed.seller_tenant_id),
        ..Default::default()
    };
    order::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .is_ok()
}

/// The approved minimal point-read prefetch (08 §3.6): restricted to the requested target ID,
/// read-only, no row lock, returning only the authorization axes as PDP inputs.
///
/// # Errors
/// Store failure; the caller maps it to a sanitized unavailable/read-store failure.
pub async fn prefetch(db: &Db, order_id: Uuid) -> anyhow::Result<Prefetch> {
    db.transaction_ref_mapped_with_config(TxConfig::read_only(), move |tx| {
        Box::pin(async move {
            let row = order::Entity::find_by_id(order_id)
                .secure()
                .scope_with(&AccessScope::for_resource(order_id))
                .one(tx)
                .await?;
            Ok(row.map_or(Prefetch::Missing(order_id), |row| {
                Prefetch::Found(AuthorizationFacts::observed(order_id, arrangement(&row)))
            }))
        })
    })
    .await
}

/// Constrained re-read under the PDP-produced current-order scope. A row whose authorization
/// axes differ from the decided facts is not returned: authorization must restart.
///
/// # Errors
/// Scope/store failure.
pub async fn read_authorized(
    runner: &impl DBRunner,
    target: &TargetAuthorization,
) -> Result<AuthorizedRead, ScopeError> {
    let facts = target.facts();
    let row = repo::find_order(runner, target.access_scope(), facts.order_id()).await?;
    Ok(match row {
        None => AuthorizedRead::NotVisible,
        Some(row) if arrangement(&row) != facts.arrangement() => AuthorizedRead::FactsChanged,
        Some(row) => AuthorizedRead::Current(Box::new(row)),
    })
}

/// Outcome of a constrained point re-read.
#[derive(Debug)]
pub enum AuthorizedRead {
    Current(Box<order::Model>),
    /// Authorization-relevant facts changed after the decision: restart authorization.
    FactsChanged,
    /// Not visible through the decided scope: the non-disclosing not-found arm.
    NotVisible,
}

/// Facts changed between decision and lock. Never settles or changes an idempotency key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleAuthorization {
    current_version: i32,
}
impl StaleAuthorization {
    /// `version-conflict` where the caller's expected version is safely observable as stale,
    /// otherwise `authorization-context-changed` (409) without new target facts.
    #[must_use]
    pub fn reason(self, expected_version: Option<i32>) -> Reason {
        match expected_version {
            Some(expected) if expected != self.current_version => Reason::VersionConflict,
            _ => Reason::AuthorizationContextChanged,
        }
    }
}

/// Lock outcome for an authorized existing-order mutation.
pub enum AuthorizedLock<'a, T: TransactionRunner> {
    Locked(Box<LockedOrder<'a, T>>),
    /// The order is still visible but its authorization facts changed: conflict, no rebase.
    Stale(StaleAuthorization),
    /// Access lost after the decision: preserve the non-disclosing `order-not-found`.
    AccessLost,
}

/// Lock the current aggregate under the PDP-produced scope and compare the authorization facts
/// used for the decision, through commit (the row lock holds them).
///
/// # Errors
/// Scope/store failure.
pub async fn lock_authorized<'a, T: TransactionRunner>(
    tx: &'a T,
    target: &TargetAuthorization,
) -> Result<AuthorizedLock<'a, T>, ScopeError> {
    let facts = target.facts();
    let Some(locked) =
        LockedOrder::lock_current(tx, target.access_scope(), facts.order_id()).await?
    else {
        return Ok(AuthorizedLock::AccessLost);
    };
    if arrangement(locked.row()) != facts.arrangement() {
        return Ok(AuthorizedLock::Stale(StaleAuthorization {
            current_version: locked.row().current_version,
        }));
    }
    Ok(AuthorizedLock::Locked(Box::new(locked)))
}

/// Persist exactly the authorized proposed arrangement on a locked order. The complete proposed
/// model is validated against the proposed-side PDP scope (no `NotSet` axis can skip it); the
/// update itself stays restricted by the current-side scope captured at lock time.
///
/// # Errors
/// `Denied` when the model's axes are not the authorized arrangement or belong to another
/// order, or when the proposed scope does not admit the complete model.
pub async fn apply_authorized_arrangement<T: TransactionRunner>(
    locked: &mut LockedOrder<'_, T>,
    proposed: &ProposedAuthorization,
    model: order::Model,
) -> Result<(), ScopeError> {
    if proposed.order_id() != Some(model.order_id) || arrangement(&model) != proposed.arrangement()
    {
        return Err(ScopeError::Denied(
            "proposed arrangement differs from authorization",
        ));
    }
    locked.replace(proposed.access_scope(), model).await
}

/// Insert a newly created aggregate whose complete arrangement was authorized by
/// `order × create` (payer use included). Every security property is populated by the model.
///
/// # Errors
/// `Denied` when the model is not the authorized arrangement or the scope rejects it.
pub async fn insert_authorized_order<T: TransactionRunner>(
    tx: &T,
    created: &ProposedAuthorization,
    model: order::Model,
) -> Result<order::Model, ScopeError> {
    if created.order_id().is_some() || arrangement(&model) != created.arrangement() {
        return Err(ScopeError::Denied(
            "created arrangement differs from authorization",
        ));
    }
    repo::insert_order(tx, created.access_scope(), model).await
}

/// Validate a proposed create/preview model before any registry probe or commercial read.
///
/// # Errors
/// `Denied` when the proposed scope does not admit the complete arrangement.
pub fn admit_proposed(
    proposed: &ProposedAuthorization,
    model: &order::Model,
) -> Result<(), ScopeError> {
    if arrangement(model) != proposed.arrangement() {
        return Err(ScopeError::Denied(
            "proposed arrangement differs from authorization",
        ));
    }
    repo::validate_order(model, proposed.access_scope())
}

/// Collection reads apply the PDP scope to the current aggregate; filters only narrow it.
///
/// # Errors
/// Scope/store failure.
pub async fn find_in_collection(
    runner: &impl DBRunner,
    scope: &CollectionScope,
    order_id: Uuid,
) -> Result<Option<order::Model>, ScopeError> {
    repo::find_order(runner, scope.access_scope(), order_id).await
}

/// Bounded private persistence authority (08 §3.5): restricted service database role plus a
/// scope bound to an authorized order, the authenticated principal or the refusal subject.
/// It is not a PDP grant and never authorizes a business read or another order.
pub struct PrivateScope(AccessScope);
impl PrivateScope {
    /// Committed audit/idempotency evidence of the locked, authorized order.
    #[must_use]
    pub fn for_locked_order<T: TransactionRunner>(locked: &LockedOrder<'_, T>) -> Self {
        Self(AccessScope::for_resource(locked.row().order_id))
    }
    /// Refusal evidence under the authenticated subject tenant (D-104); no target lookup.
    #[must_use]
    pub fn for_refusal(caller: &Caller) -> Self {
        Self(AccessScope::single(ScopeConstraint::new(vec![
            ScopeFilter::eq(
                crate::gts::permissions::properties::SUBJECT_TENANT_ID,
                caller.ctx().subject_tenant_id(),
            ),
        ])))
    }
    /// Read access-log evidence bound to the authenticated subject (08 §3.7: `actor` is the
    /// immutable subject UUID rendered as text; the insert validator compares UUID-shaped text
    /// as a UUID); no target lookup and no business authority (S6-04).
    #[must_use]
    pub fn for_read_log(caller: &Caller) -> Self {
        Self(AccessScope::single(ScopeConstraint::new(vec![
            ScopeFilter::eq("actor", caller.ctx().subject_id()),
        ])))
    }
    /// The authenticated principal's own idempotency records.
    #[must_use]
    pub fn for_principal(principal_scope: &str) -> Self {
        Self(AccessScope::single(ScopeConstraint::new(vec![
            ScopeFilter::eq("principal_scope", principal_scope.to_owned()),
        ])))
    }
    pub(crate) fn access_scope(&self) -> &AccessScope {
        &self.0
    }
}

/// Stable registry principal from the authenticated context only (never body, token or proof):
/// the platform-asserted subject tenant and subject identifier (Foundation §4.2).
///
/// # Errors
/// An anonymous or nil identity has no stable scope and must not reach the registry.
pub fn principal_scope(
    caller: &Caller,
) -> Result<crate::domain::idempotency::PrincipalScope, crate::domain::idempotency::RegistryRuleError>
{
    crate::domain::idempotency::PrincipalScope::from_context(caller.ctx())
}
