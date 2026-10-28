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
/// `SecureORM` skips a filter on a column the model leaves `NotSet`, so every column is listed
/// below and only the ones these facts cannot supply are `NotSet`. Of those, `audit_tenant_id`
/// is the one other `pep_prop` property: it is the immutable audit-chain namespace and grants no
/// access (D-104), so no order PDP constrains it. A scope that does anyway cannot be decided from
/// these facts, and skipping its filter would admit more than the PDP allowed, so it is denied
/// (fail closed). The remaining `NotSet` columns are not scope properties.
///
/// A decision whose constraints exclude the prefetched target is a denial for that target: it
/// takes the same follow-up and non-disclosing mapping as an explicit deny (08 §3.6 item 2).
pub fn scope_admits(scope: &AccessScope, order_id: Option<Uuid>, proposed: Arrangement) -> bool {
    use sea_orm::ActiveValue::{NotSet, Set};
    use toolkit_db::secure::ScopableEntity;
    let constrains_audit_namespace = scope.constraints().iter().any(|constraint| {
        constraint.filters().iter().any(|filter| {
            matches!(
                order::Entity::resolve_property(filter.property()),
                Some(order::Column::AuditTenantId)
            )
        })
    });
    if constrains_audit_namespace {
        return false;
    }
    // Definition order. Set: the `resource_col` when known and the three business axes the PDP
    // constrains. `audit_tenant_id`: denied above when constrained. The rest: not scope
    // properties.
    let model = order::ActiveModel {
        order_id: order_id.map_or(NotSet, Set),
        order_number: NotSet,
        category: NotSet,
        resource_tenant_id: Set(proposed.resource_tenant_id),
        audit_tenant_id: NotSet,
        payer_tenant_id: Set(proposed.payer_tenant_id),
        seller_tenant_id: Set(proposed.seller_tenant_id),
        initiating_actor: NotSet,
        sales_path: NotSet,
        contract_id: NotSet,
        state: NotSet,
        state_entered_at: NotSet,
        current_version: NotSet,
        version_allocation_high_water: NotSet,
        draft_revision: NotSet,
        pre_hold_state: NotSet,
        resume_count: NotSet,
        amendment_count: NotSet,
        fulfillment_control_generation: NotSet,
        fulfillment_control_pending: NotSet,
        spawn_signal_at: NotSet,
        authorization_failure_tolerated_at: NotSet,
        compensation_evidence: NotSet,
        audit_sequence: NotSet,
        created_at: NotSet,
    };
    order::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .is_ok()
}

/// The approved minimal point-read prefetch (08 §3.6): restricted to the requested target ID,
/// read-only, no row lock, returning only the authorization axes as PDP inputs.
///
/// It takes the `Db` rather than a caller's runner because it owns its transaction: the prefetch
/// runs before the PDP decision and the authorized snapshot starts after it (08 §3.6), so the
/// prefetch is its own read-only transaction, finished before the PDP call; a caller's runner
/// could keep one transaction (and its snapshot) open across the decision.
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
    /// Visible with unchanged authorization facts: the current row.
    Current(Box<order::Model>),
    /// Authorization-relevant facts changed after the decision: restart authorization.
    FactsChanged,
    /// Not visible through the decided scope: the non-disclosing not-found arm.
    NotVisible,
}

/// Lock outcome for an authorized existing-order mutation.
pub enum AuthorizedLock<'a, T: TransactionRunner> {
    /// Locked with unchanged authorization facts.
    Locked(Box<LockedOrder<'a, T>>),
    /// The order is still visible but its authorization facts changed: conflict, no rebase.
    /// Never settles or changes an idempotency key; the refusal reason is the domain's
    /// ([`stale_refusal`](crate::domain::transition::stale_refusal)).
    Stale {
        /// The locked row's current version, observed under the row lock.
        current_version: i32,
    },
    /// Access lost after the decision: preserve the non-disclosing `order-not-found`.
    AccessLost,
}

/// Lock the current aggregate under the PDP-produced scope and compare the authorization facts
/// used for the decision, through commit (the row lock holds them).
///
/// The sealed `TransactionRunner` bound is deliberate, not `&impl DBRunner`: the lock is a
/// `SELECT ... FOR UPDATE`, which must run inside the mutation's transaction to hold the facts
/// through commit; on a connection it would end with its own autocommit statement.
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
        return Ok(AuthorizedLock::Stale {
            current_version: locked.row().current_version,
        });
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
/// `order × create`. Every security property is populated by the model.
///
/// # Errors
/// `Denied` when the model is not the authorized arrangement or the scope rejects it.
pub async fn insert_authorized_order(
    tx: &impl DBRunner,
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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the engine admits a create through its own preparation; Preview (S3) is the next \
                  caller, the authz storage tests prove it now"
    )
)]
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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the delivered list reads page through the read repository; the authz storage tests \
                  prove this collection-scope helper"
    )
)]
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
    /// The underlying access scope.
    pub(crate) fn access_scope(&self) -> &AccessScope {
        &self.0
    }
}

/// Stable registry principal from the authenticated context only (never body, token or proof):
/// the platform-asserted subject tenant and subject identifier (Foundation §4.2).
///
/// # Errors
/// An anonymous or nil identity has no stable scope and must not reach the registry.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the engine derives the principal inside its registry gate; the authz storage tests \
                  pin this standalone derivation"
    )
)]
pub fn principal_scope(
    caller: &Caller,
) -> Result<crate::domain::idempotency::PrincipalScope, crate::domain::idempotency::RegistryRuleError>
{
    crate::domain::idempotency::PrincipalScope::from_context(caller.ctx())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axes() -> Arrangement {
        Arrangement {
            resource_tenant_id: Uuid::from_u128(1),
            seller_tenant_id: Uuid::from_u128(2),
            payer_tenant_id: Uuid::from_u128(3),
        }
    }

    fn path(filters: Vec<ScopeFilter>) -> ScopeConstraint {
        ScopeConstraint::new(filters)
    }

    #[test]
    fn a_scope_constraining_the_audit_namespace_is_denied() {
        let facts = axes();
        let resource = || ScopeFilter::eq("resource_tenant_id", facts.resource_tenant_id);
        let audit = || ScopeFilter::eq("audit_tenant_id", facts.resource_tenant_id);
        // The axes alone decide an ordinary scope.
        assert!(scope_admits(
            &AccessScope::single(path(vec![resource()])),
            None,
            facts
        ));
        assert!(!scope_admits(
            &AccessScope::single(path(vec![ScopeFilter::eq(
                "resource_tenant_id",
                Uuid::from_u128(9)
            )])),
            None,
            facts
        ));
        // A filter on the never-supplied audit namespace is not skipped: denied, alone, beside
        // a matching axis, and even when another path would admit the facts.
        assert!(!scope_admits(
            &AccessScope::single(path(vec![audit()])),
            Some(Uuid::from_u128(4)),
            facts
        ));
        assert!(!scope_admits(
            &AccessScope::single(path(vec![resource(), audit()])),
            None,
            facts
        ));
        assert!(!scope_admits(
            &AccessScope::from_constraints(vec![path(vec![resource()]), path(vec![audit()])]),
            None,
            facts
        ));
    }
}
