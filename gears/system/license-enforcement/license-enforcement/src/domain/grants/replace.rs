//! `replace` through the `issue` intake (DESIGN §3.1, Replacement).

use std::collections::{HashMap, HashSet};

use license_enforcement_sdk::DelegationId;
use license_enforcement_sdk::dto::{IssueResult, ReplaceRequest};
use license_enforcement_sdk::models::TerminationReason;

use super::lifecycle::{ended, locked_pack, write_all};
use super::{
    Binding, DbTx, DomainError, FieldViolation, Grant, GrantId, GrantRepo, GrantService,
    GrantState, Missing, OffsetDateTime, PackId, Prepared, SecurityContext, TermsField, Validity,
    actions, empty_rules, members_admitted, not_found, permissions_within, reason, resources,
    size_violations, store_admitted, stored_window,
};
use crate::domain::types::{covers, type_key};
use toolkit_macros::domain_model;

/// What `replace` addresses. Its serde names enter stored idempotency hashes.
#[domain_model]
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum Target {
    #[serde(rename = "Grant")]
    Grant(GrantId),
    #[serde(rename = "Pack")]
    Pack(PackId),
}

fn same_resource(a: &Grant, b: &Grant) -> bool {
    type_key(a.resource.gts_type.as_ref()) == type_key(b.resource.gts_type.as_ref())
        && a.resource.id == b.resource.id
}

/// `child` satisfies I4 against `parent` without change. Its window becomes
/// the window of `parent`. A `parent` without conditions admits any, as in
/// the check.
fn subset_of(child: &Grant, parent: &Grant) -> bool {
    same_resource(child, parent)
        && type_key(child.subject_type.as_ref()) == type_key(parent.subject_type.as_ref())
        && parent
            .conditions
            .as_ref()
            .is_none_or(|c| child.conditions.as_ref() == Some(c))
        && permissions_within(child.permissions, parent.permissions)
        && child.permissions.max_depth.checked_add(1) == Some(parent.permissions.max_depth)
}

/// Whether a successor can take a binding.
enum Fit {
    Yes,
    No,
    /// The delegation of this link does not fit the successor.
    BlockedBy(Option<DelegationId>),
}

/// `replaced` has a live grant and none that awaits acceptance. Checked before
/// admission, so that a refused target does not reach the plugin, and again
/// under the lock.
fn replaceable(replaced: &[Grant]) -> Result<(), DomainError> {
    if replaced.iter().all(|g| g.state == GrantState::Terminated) {
        return Err(DomainError::transition(
            "nothing left to replace: every grant is terminated",
        ));
    }
    let waiting: Vec<String> = replaced
        .iter()
        .filter(|g| g.state == GrantState::AwaitingAcceptance)
        .map(|g| g.id.0.to_string())
        .collect();
    if waiting.is_empty() {
        Ok(())
    } else {
        Err(DomainError::transition(format!(
            "grants await acceptance: {}",
            waiting.join(", ")
        )))
    }
}

/// The checks before admission rest on I9 (a root stays a root outside a
/// pack, a member stays in its pack) and on the members of a pack ending
/// together, so the sweeper deletes none of them alone. `locked`, the rows
/// read under the lock, confirm them against `checked`, the ids checked
/// before.
fn still_replaced(
    target: Target,
    locked: &[Grant],
    checked: &[GrantId],
) -> Result<(), DomainError> {
    let mut ids: Vec<GrantId> = locked.iter().map(|g| g.id).collect();
    let mut checked = checked.to_vec();
    ids.sort_unstable();
    checked.sort_unstable();
    let moved = matches!(target, Target::Grant(_))
        && locked
            .iter()
            .any(|g| g.parent_grant_id.is_some() || g.pack_id.is_some());
    if ids != checked || moved {
        return Err(DomainError::Internal(
            "the replaced grants changed after the checks before admission".to_owned(),
        ));
    }
    Ok(())
}

/// Whether `successor` can take the binding of `replaced` at `now`: the
/// binding stays usable, so the window of `successor` has started and not
/// ended, and a moved chain keeps I4 without change. `subtrees` holds the
/// locked live subtree of each delegated sub-grant.
fn fits(
    successor: &Grant,
    replaced: &Grant,
    subtrees: &HashMap<GrantId, Vec<Grant>>,
    now: OffsetDateTime,
) -> Fit {
    let yes = |ok: bool| if ok { Fit::Yes } else { Fit::No };
    if successor.state != GrantState::Active
        || successor.validity.window().not_before > now
        || ended(successor, now)
        || !same_resource(successor, replaced)
    {
        return Fit::No;
    }
    // A successor fixed at admission already holds the binding: it fits when
    // the subject is the same, its type compared without minor versions. A
    // fixed binding moves only to a fixed one (I8).
    if successor.binding_fixed {
        return yes(matches!(
            (&successor.binding, &replaced.binding),
            (Binding::Subject { subject: s, .. }, Binding::Subject { subject: r, .. })
                if s.id == r.id && type_key(s.gts_type.as_ref()) == type_key(r.gts_type.as_ref())
        ));
    }
    if successor.binding != Binding::None || replaced.binding_fixed {
        return Fit::No;
    }
    match &replaced.binding {
        Binding::Subject { subject, .. } => {
            yes(successor.permissions.may_bind
                && covers(&successor.subject_type, &subject.gts_type))
        }
        Binding::Delegated { grant_id, .. } => {
            let tree = subtrees.get(grant_id).map_or(&[][..], Vec::as_slice);
            match tree.first() {
                None => Fit::No,
                // As `delegate`: recipient rules are not evaluated yet, so a
                // successor with rules takes no delegation.
                Some(child) if !empty_rules(successor.recipient_rules.as_ref()) => {
                    Fit::BlockedBy(child.delegation_id)
                }
                Some(child) if !subset_of(child, successor) => Fit::BlockedBy(child.delegation_id),
                Some(_) => Fit::Yes,
            }
        }
        // An unbound grant has nothing to move and does not get here; a kind
        // of binding that this code does not know moves nowhere.
        _ => Fit::No,
    }
}

/// The locked live subtree of each delegated sub-grant of `old`. A
/// delegated binding without a live sub-grant becomes none.
async fn delegated_subtrees<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    old: &mut [Grant],
) -> Result<HashMap<GrantId, Vec<Grant>>, DomainError> {
    let delegated: Vec<GrantId> = old
        .iter()
        .filter_map(|g| match g.binding {
            Binding::Delegated { grant_id, .. } => Some(grant_id),
            _ => None,
        })
        .collect();
    let subtrees = store.live_subtrees(tx, &delegated, true).await?;
    for g in old {
        if let Binding::Delegated { grant_id, .. } = g.binding
            && subtrees.get(&grant_id).is_none_or(Vec::is_empty)
        {
            g.binding = Binding::None;
        }
    }
    Ok(subtrees)
}

/// Move `tree`, the locked live subtree of `child`, under `successor`: the new parent, the
/// evidence and the window of `successor`, into `out`; each link with its new
/// root, `successor`, into `roots`.
fn reparent(
    child: GrantId,
    tree: Vec<Grant>,
    successor: &Grant,
    out: &mut Vec<Grant>,
    roots: &mut Vec<(GrantId, GrantId)>,
) {
    let window = *successor.validity.window();
    for mut g in tree {
        if g.id == child {
            g.parent_grant_id = Some(successor.id);
        }
        g.validity = Validity::Inherited { window };
        g.evidence_id = successor.evidence_id;
        g.version += 1;
        roots.push((g.id, successor.id));
        out.push(g);
    }
}

/// Each moved link with its new root.
type NewRoots = Vec<(GrantId, GrantId)>;

/// The writes of a replacement, for one batched write: each moved subtree
/// under its successor, then the replaced grants, terminated. Also returns the
/// new root of each moved link.
fn handover(
    moves: &[(usize, Binding)],
    mut subtrees: HashMap<GrantId, Vec<Grant>>,
    successors: &[Grant],
    mut old: Vec<Grant>,
) -> (Vec<Grant>, NewRoots) {
    let mut rows = Vec::new();
    let mut roots = Vec::new();
    for (i, binding) in moves {
        if let Binding::Delegated { grant_id, .. } = binding {
            let tree = subtrees.remove(grant_id).unwrap_or_default();
            reparent(*grant_id, tree, &successors[*i], &mut rows, &mut roots);
        }
    }
    for g in &mut old {
        g.state = GrantState::Terminated;
        g.termination_reason = Some(TerminationReason::Replaced);
        g.binding = Binding::None;
        g.version += 1;
    }
    rows.append(&mut old);
    (rows, roots)
}

impl<R: GrantRepo> GrantService<R> {
    /// Replace a root without a pack, or every member of a pack, by grants
    /// that the `issue` intake admits. Each binding moves to the successor
    /// that names its grant.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn replace(
        &self,
        ctx: &SecurityContext,
        target: Target,
        mut req: ReplaceRequest,
    ) -> Result<IssueResult, DomainError> {
        if !self.plugin.intakes().issue {
            return Err(DomainError::IntakeNotServed);
        }
        let mut violations = Vec::new();
        let mut seen = HashSet::new();
        for (i, s) in req.successors.iter_mut().enumerate() {
            // Before the copy for the plugin: it sees the window that is stored.
            stored_window(&mut s.terms.window);
            if let Some(r) = s.replaces
                && !seen.insert(r)
            {
                violations.push(FieldViolation::new(
                    format!("successors[{i}].replaces"),
                    "two successors name the same grant",
                    reason::DUPLICATE,
                ));
            }
        }
        let terms: Vec<_> = req.successors.iter().map(|s| s.terms.clone()).collect();
        size_violations(&terms, TermsField::REPLACE, &mut violations);
        let (op, resource, id) = match target {
            Target::Grant(id) => ("replace_grant", &resources::GRANT, id.0),
            Target::Pack(id) => ("replace_pack", &resources::PACK, id.0),
        };
        let token = req.idempotency_token.clone();
        let (scope, hash) = match self
            .prepare(
                ctx,
                violations,
                op,
                &(&target, &req.successors, &req.expected_version),
                &token,
                resource,
                actions::REPLACE,
                Some(id),
                false,
            )
            .await?
        {
            Prepared::Replayed(result, _) => return Ok(result),
            Prepared::Fresh { scope, hash } => (scope, hash),
        };

        // The replaced grants, to check the names before admission.
        let conn = self.db.conn()?;
        let (holder, replaced, grants_scope) = match target {
            Target::Grant(id) => {
                let g = self
                    .store
                    .grant(&conn, &scope, id, false)
                    .await?
                    .ok_or_else(|| not_found(Missing::Grant, id.0))?;
                if g.parent_grant_id.is_some() {
                    return Err(DomainError::transition("only a root is replaced"));
                }
                if g.pack_id.is_some() {
                    return Err(DomainError::transition(
                        "a pack member is replaced through its pack",
                    ));
                }
                if let Err(e) = replaceable(std::slice::from_ref(&g)) {
                    // A repeat of a replace that commits meanwhile finds the
                    // grants terminated: it answers with the stored result.
                    return self.or_replayed(ctx, &token, &hash, Err(e)).await;
                }
                (g.holder_tenant_id, vec![g.id], None)
            }
            Target::Pack(id) => {
                let p = self
                    .store
                    .pack(&conn, &scope, id)
                    .await?
                    .ok_or_else(|| not_found(Missing::Pack, id.0))?;
                // The decision on the members before anything about them
                // answers or reaches the plugin.
                let grants_scope = self
                    .scope(ctx, &resources::GRANT, actions::REPLACE, None, false)
                    .await?;
                let members = self
                    .store
                    .pack_grants(&conn, p.holder_tenant_id, id, false)
                    .await?;
                members_admitted(&grants_scope, &members)?;
                if let Err(e) = replaceable(&members) {
                    return self.or_replayed(ctx, &token, &hash, Err(e)).await;
                }
                (
                    p.holder_tenant_id,
                    members.iter().map(|g| g.id).collect(),
                    Some(grants_scope),
                )
            }
        };
        let mut violations = Vec::new();
        for (i, s) in req.successors.iter().enumerate() {
            if let Some(r) = s.replaces
                && !replaced.contains(&r)
            {
                violations.push(FieldViolation::new(
                    format!("successors[{i}].replaces"),
                    format!("grant {r} is not replaced by this operation"),
                    reason::NOT_IN_REQUEST,
                ));
            }
        }
        if !violations.is_empty() {
            return Err(DomainError::InvalidArgument(violations));
        }

        // The successors, the evidence and the new pack are inserted as by `issue`.
        let as_pack = grants_scope.is_some();
        let issued = self.issue_scope(ctx, holder, as_pack).await?;
        let names: Vec<Option<GrantId>> = req.successors.iter().map(|s| s.replaces).collect();
        let admitted = self
            .admit_issue(ctx, holder, terms, as_pack, &token, TermsField::REPLACE)
            .await?;
        let plugin_id = self.plugin.info().id;
        let expected_version = req.expected_version;
        self.commit(ctx, token, hash, move |tx, store| {
            Box::pin(async move {
                let now = OffsetDateTime::now_utc();
                // The pack version is written last, after the subtree rows,
                // as every writer of a pack (DESIGN §3.7, Packs).
                let (mut old, pack_version) = match target {
                    Target::Grant(id) => {
                        let g = store
                            .grant(tx, &scope, id, true)
                            .await?
                            .ok_or_else(|| not_found(Missing::Grant, id.0))?;
                        if expected_version.is_some_and(|v| v != g.version) {
                            return Err(DomainError::VersionMismatch);
                        }
                        (vec![g], None)
                    }
                    Target::Pack(id) => {
                        let (members, p) =
                            locked_pack(tx, store, &scope, holder, id, expected_version).await?;
                        (members, Some((id, p.version + 1)))
                    }
                };
                still_replaced(target, &old, &replaced)?;
                replaceable(&old)?;
                old.retain(|g| g.state != GrantState::Terminated);

                let subtrees = delegated_subtrees(tx, store, &mut old).await?;

                // Each binding needs the successor that names its grant.
                let mut admitted = admitted;
                let mut without = Vec::new();
                let mut moves: Vec<(usize, Binding)> = Vec::new();
                for g in &old {
                    if g.binding == Binding::None {
                        continue;
                    }
                    let Some(i) = names.iter().position(|n| *n == Some(g.id)) else {
                        without.push((g.id, None));
                        continue;
                    };
                    match fits(&admitted.result.grants[i], g, &subtrees, now) {
                        Fit::Yes => moves.push((i, g.binding.clone())),
                        Fit::No => without.push((g.id, None)),
                        Fit::BlockedBy(d) => without.push((g.id, d)),
                    }
                }
                if !without.is_empty() {
                    return Err(DomainError::BindingsWithoutSuccessor(without));
                }
                // A successor fixed at admission keeps its own binding: the
                // evidence names it so, and the check compares it exactly.
                for (i, binding) in &moves {
                    let successor = &mut admitted.result.grants[*i];
                    if !successor.binding_fixed {
                        successor.binding = binding.clone();
                    }
                }
                let result =
                    store_admitted(tx, store, &issued, holder, plugin_id, admitted).await?;
                let (rows, roots) = handover(&moves, subtrees, &result.grants, old);
                write_all(tx, store, &rows, now).await?;
                store.set_roots(tx, &roots).await?;
                if let Some((id, version)) = pack_version {
                    store.update_pack_version(tx, &scope, id, version).await?;
                }
                Ok(result)
            })
        })
        .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "replace_tests.rs"]
mod replace_tests;
