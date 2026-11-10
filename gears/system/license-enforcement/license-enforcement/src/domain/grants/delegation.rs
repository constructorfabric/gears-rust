//! `delegate` (DESIGN §3.1, Lifecycle; §3.6, Delegate and Recall).

use std::collections::HashSet;

use license_enforcement_sdk::DelegationId;
use license_enforcement_sdk::dto::{DelegateRequest, Delegation, DelegationGrants};
use license_enforcement_sdk::limits::MAX_GRANTS_PER_DELEGATION;
use license_enforcement_sdk::models::Permissions;

use super::lifecycle::{ended, read_locked, write_all};
use super::{
    AccessScope, Binding, DomainError, FieldViolation, Grant, GrantId, GrantRepo, GrantService,
    GrantState, Missing, OffsetDateTime, Prepared, SecurityContext, Uuid, Validity, actions,
    empty_rules, not_found, permissions_within, reason, resources, too_large,
};

fn not_a_subset(field: &str, detail: &str) -> DomainError {
    DomainError::invalid(field, detail, reason::NOT_A_SUBSET)
}

fn request_violations(req: &DelegateRequest) -> Vec<FieldViolation> {
    let mut v = Vec::new();
    let grant_ids = &req.grant_ids;
    if grant_ids.is_empty() {
        v.push(FieldViolation::new(
            "grant_ids",
            "no grant to delegate",
            reason::REQUIRED,
        ));
    }
    too_large(
        "grant_ids",
        grant_ids.len(),
        MAX_GRANTS_PER_DELEGATION,
        &mut v,
    );
    let named: HashSet<_> = grant_ids.iter().collect();
    if named.len() != grant_ids.len() {
        v.push(FieldViolation::new(
            "grant_ids",
            "a grant is named twice",
            reason::DUPLICATE,
        ));
    }
    too_large(
        "expected_versions",
        req.expected_versions.len(),
        MAX_GRANTS_PER_DELEGATION,
        &mut v,
    );
    if req.expected_versions.keys().any(|id| !named.contains(id)) {
        v.push(FieldViolation::new(
            "expected_versions",
            "a version is stated for a grant outside grant_ids",
            reason::NOT_IN_REQUEST,
        ));
    }
    v
}

/// Why `parent` cannot be delegated now, or `None`. The caller holds its lock
/// and gives its chain up to the root and its live child, if any.
fn refusal(
    parent: &Grant,
    chain: &[Grant],
    child: Option<&Grant>,
    now: OffsetDateTime,
) -> Result<Option<&'static str>, DomainError> {
    if parent.state != GrantState::Active {
        return Ok(Some("the grant is not active"));
    }
    if parent.permissions.max_depth == 0 {
        return Ok(Some("the grant is not delegable"));
    }
    if parent.binding_fixed {
        return Ok(Some("the binding was fixed at admission"));
    }
    if ended(parent, now) {
        return Ok(Some("the window of the grant has ended"));
    }
    match parent.binding {
        Binding::None => {}
        Binding::Delegated { .. } => {
            if child.is_some() {
                return Ok(Some("the grant is delegated"));
            }
        }
        _ => return Ok(Some("the grant is bound")),
    }
    match chain.last() {
        Some(root) if root.parent_grant_id.is_none() && root.state == GrantState::Active => {}
        // The live chain ends short of a terminated root.
        _ => return Ok(Some("the root is not active")),
    }
    // ponytail: recipient rules need the recipient projection, which is not
    // built; a grant with rules is not delegated until it is.
    if !empty_rules(parent.recipient_rules.as_ref()) {
        return Err(DomainError::NotImplemented("recipient rules"));
    }
    Ok(None)
}

/// The sub-grant of `parent` for `recipient`.
fn sub_grant(
    parent: &Grant,
    recipient: Uuid,
    delegation: DelegationId,
    req: &DelegateRequest,
) -> Result<Grant, DomainError> {
    let validity = Validity::Inherited {
        window: *parent.validity.window(),
    };
    let max_depth = parent
        .permissions
        .max_depth
        .checked_sub(1)
        .ok_or_else(|| DomainError::transition("the grant is not delegable"))?;
    let permissions = match req.permissions {
        Some(p) => {
            let asked = Permissions {
                may_bind: p.may_bind,
                max_depth,
                requires_acceptance: p.requires_acceptance,
            };
            if !permissions_within(asked, parent.permissions) {
                return Err(not_a_subset(
                    "permissions",
                    "the permissions are wider than those of the parent",
                ));
            }
            asked
        }
        None => Permissions {
            max_depth,
            ..parent.permissions
        },
    };
    // What the sub-grant inherits is named here: a new field of `Grant`
    // starts at its default, not at the value of the parent.
    let mut sub = Grant::new(
        GrantId(Uuid::now_v7()),
        recipient,
        parent.resource.clone(),
        parent.subject_type.clone(),
        validity,
        permissions,
        parent.evidence_id,
    );
    sub.conditions.clone_from(&parent.conditions);
    sub.recipient_rules.clone_from(&parent.recipient_rules);
    sub.parent_grant_id = Some(parent.id);
    sub.grantor_tenant_id = Some(parent.holder_tenant_id);
    sub.delegation_id = Some(delegation);
    if permissions.requires_acceptance {
        sub.state = GrantState::AwaitingAcceptance;
    }
    Ok(sub)
}

impl<R: GrantRepo> GrantService<R> {
    /// Delegate grants of the caller's tenant to a descendant tenant.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn delegate(
        &self,
        ctx: &SecurityContext,
        req: DelegateRequest,
    ) -> Result<Delegation, DomainError> {
        // ponytail: the evaluator has no union of conditions, so a sub-grant
        // takes the conditions of its parent until it has one.
        if !empty_rules(req.conditions.as_ref()) {
            return Err(DomainError::NotImplemented("delegated conditions"));
        }
        let violations = request_violations(&req);
        let token = req.idempotency_token.clone();
        let (scope, hash) = match self
            .prepare(
                ctx,
                violations,
                "delegate",
                &req,
                &token,
                &resources::GRANT,
                actions::DELEGATE,
                None,
                true,
            )
            .await?
        {
            Prepared::Replayed(d, _) => return Ok(d),
            Prepared::Fresh { scope, hash } => (scope, hash),
        };
        let holder = ctx.subject_tenant_id();
        let recipient = req.recipient_tenant_id;
        if recipient == holder || !self.is_descendant(ctx, holder, recipient).await? {
            return Err(DomainError::transition(
                "the recipient is not a descendant of the holder, or is behind a barrier",
            ));
        }
        let delegation = DelegationId(Uuid::now_v7());
        self.commit(ctx, token, hash, move |tx, store| {
            Box::pin(async move {
                let now = OffsetDateTime::now_utc();
                let grant_ids = &req.grant_ids;
                // Access first: no row of another tenant is locked for a
                // grant that the caller cannot reach.
                let reached = |rows: Vec<Grant>| {
                    let mut by_id: std::collections::HashMap<GrantId, Grant> = rows
                        .into_iter()
                        .filter(|g| g.holder_tenant_id == holder)
                        .map(|g| (g.id, g))
                        .collect();
                    grant_ids
                        .iter()
                        .map(|id| {
                            by_id
                                .remove(id)
                                .ok_or_else(|| not_found(Missing::Grant, id.0))
                        })
                        .collect::<Result<Vec<_>, _>>()
                };
                let (scope, reached) = (&scope, &reached);
                let parents = read_locked(
                    tx,
                    store,
                    |_| grant_ids.clone(),
                    |lock| async move { reached(store.grants(tx, scope, grant_ids, lock).await?) },
                )
                .await?;
                if parents.iter().any(|g| {
                    req.expected_versions
                        .get(&g.id)
                        .is_some_and(|v| *v != g.version)
                }) {
                    return Err(DomainError::VersionMismatch);
                }
                let mut chains = store.chains_of(tx, grant_ids).await?;
                let children: Vec<GrantId> = parents
                    .iter()
                    .filter_map(|g| match g.binding {
                        Binding::Delegated { grant_id, .. } => Some(grant_id),
                        _ => None,
                    })
                    .collect();
                let subtrees = store.live_subtrees(tx, &children, true).await?;
                let mut roots = Vec::with_capacity(parents.len());
                for g in &parents {
                    let chain = chains.remove(&g.id).unwrap_or_default();
                    let child = match g.binding {
                        Binding::Delegated { grant_id, .. } => {
                            subtrees.get(&grant_id).and_then(|t| t.first())
                        }
                        _ => None,
                    };
                    if let Some(why) = refusal(g, &chain, child, now)? {
                        return Err(DomainError::transition(format!("grant {}: {why}", g.id)));
                    }
                    roots.push(chain.last().map(|r| r.id));
                }
                let mut subs = Vec::with_capacity(parents.len());
                let mut delegated = Vec::with_capacity(parents.len());
                for (mut parent, root) in parents.into_iter().zip(roots) {
                    let sub = sub_grant(&parent, recipient, delegation, &req)?;
                    let root = root.ok_or_else(|| {
                        DomainError::Internal(format!(
                            "grant {} of delegation {delegation} has no root",
                            parent.id
                        ))
                    })?;
                    parent.binding = Binding::delegated(sub.id, recipient);
                    parent.version += 1;
                    subs.push((sub, root));
                    delegated.push(parent);
                }
                // SAFETY(cross-tenant write): the hierarchy check made the
                // recipient a descendant of the holder.
                store
                    .insert_sub_grants(tx, &AccessScope::for_tenant(recipient), &subs, now)
                    .await?;
                write_all(tx, store, &delegated, now).await?;
                Ok(Delegation::new(
                    delegation,
                    holder,
                    recipient,
                    DelegationGrants::Given { grants: delegated },
                    1,
                ))
            })
        })
        .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "delegation_tests.rs"]
mod delegation_tests;
