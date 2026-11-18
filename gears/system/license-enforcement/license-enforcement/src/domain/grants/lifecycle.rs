//! Lifecycle verbs of grants, packs, and delegations (DESIGN §3.1, Lifecycle).

use std::collections::HashMap;

use license_enforcement_sdk::models::{TerminationReason, Window};
use license_enforcement_sdk::{DelegationId, Intakes};

use super::{
    Binding, DbTx, DomainError, Grant, GrantId, GrantRepo, GrantService, GrantState, Missing,
    OffsetDateTime, Pack, PackId, Prepared, Serialize, Validity, WindowInput, actions, not_found,
    resources, stored_window, window_violations,
};
use crate::domain::grants::{AccessScope, admits, members_admitted, subject_loses_grant};
use crate::domain::window::{end, inside};
use license_enforcement_sdk::dto::{Delegation, DelegationGrants};
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// A lifecycle verb without parameters. `renew` carries a window and has its
/// own entry points. The serde names enter stored idempotency hashes: a
/// rename would refuse a replay after a deploy.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Verb {
    #[serde(rename = "Accept")]
    Accept,
    #[serde(rename = "Reject")]
    Reject,
    #[serde(rename = "Suspend")]
    Suspend,
    #[serde(rename = "Reinstate")]
    Reinstate,
    #[serde(rename = "Revoke")]
    Revoke,
    #[serde(rename = "Return")]
    Return,
    #[serde(rename = "Recall")]
    Recall,
}

impl Verb {
    fn action(self) -> &'static str {
        match self {
            Self::Accept => actions::ACCEPT,
            Self::Reject => actions::REJECT,
            Self::Suspend => actions::SUSPEND,
            Self::Reinstate => actions::REINSTATE,
            Self::Revoke => actions::REVOKE,
            Self::Return => actions::RETURN,
            Self::Recall => actions::RECALL,
        }
    }

    /// Verbs of the vendor on a root: they need the `issue` intake, and a pack
    /// member takes them only through its pack.
    fn of_vendor(self) -> bool {
        matches!(self, Self::Suspend | Self::Reinstate | Self::Revoke)
    }

    /// Verbs of the holder that a pack takes on all its members at once.
    fn of_pack_holder(self) -> bool {
        matches!(self, Self::Accept | Self::Reject)
    }

    /// Verbs of the grantor, who holds the parent and not the grant.
    fn of_grantor(self) -> bool {
        self == Self::Recall
    }

    /// Whether `verb` changes the live grant `g`: `Ok(false)` when `g` is
    /// already where the verb leads, `Err` with the reason when the verb is
    /// not allowed.
    fn applies(self, g: &Grant) -> Result<bool, &'static str> {
        let root = g.parent_grant_id.is_none();
        match (self, g.state) {
            (Self::Suspend | Self::Reinstate | Self::Revoke, _) if !root => {
                Err("the verb applies to a root only")
            }
            (Self::Return | Self::Recall, _) if root => Err("the verb applies to a sub-grant only"),
            (Self::Accept | Self::Reject, GrantState::AwaitingAcceptance)
            | (Self::Suspend, GrantState::Active)
            | (Self::Reinstate, GrantState::Suspended)
            | (Self::Revoke | Self::Recall, _) => Ok(true),
            (Self::Return, GrantState::Active) => match g.binding {
                Binding::None => Ok(true),
                _ => Err("the sub-grant is bound"),
            },
            (Self::Accept | Self::Reinstate, GrantState::Active)
            | (Self::Suspend, GrantState::Suspended) => Ok(false),
            (Self::Accept | Self::Reject, _) => Err("the grant does not await acceptance"),
            (Self::Suspend, _) => Err("the grant is not active"),
            (Self::Reinstate, _) => Err("the grant is not suspended"),
            (Self::Return, _) => Err("the sub-grant is not active"),
        }
    }
}

fn bump(g: &mut Grant) {
    g.version += 1;
}

/// `g` ended at `now`: the end of its window, grace included, has passed.
pub(super) fn ended(g: &Grant, now: OffsetDateTime) -> bool {
    end(g.validity.window()) <= now
}

/// Write grants held by any tenant in one store call, so each pack is written
/// once. The caller decided access on the grant that the verb addresses; the
/// rest of the operation follows from it.
pub(super) async fn write_all<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    grants: &[Grant],
    now: OffsetDateTime,
) -> Result<(), DomainError> {
    if grants.is_empty() {
        return Ok(());
    }
    // SAFETY(cross-tenant write): see above.
    store
        .update_grants(tx, &AccessScope::allow_all(), grants, now)
        .await
}

/// Terminate `subtree`, the locked live subtree of `top`, with `reason`, into
/// `out`. When the subject [loses the grant](subject_loses_grant) by `reason`,
/// a subject binding in it becomes none.
/// Returns `top` as it is to be written.
pub(super) fn terminate_rows(
    subtree: Vec<Grant>,
    top: GrantId,
    reason: TerminationReason,
    out: &mut Vec<Grant>,
) -> Result<Grant, DomainError> {
    let mut written = None;
    for mut g in subtree {
        g.state = GrantState::Terminated;
        g.termination_reason = Some(reason);
        if subject_loses_grant(reason) && matches!(g.binding, Binding::Subject { .. }) {
            g.binding = Binding::None;
        }
        bump(&mut g);
        if g.id == top {
            written = Some(g.clone());
        }
        out.push(g);
    }
    written.ok_or_else(|| DomainError::transition("the grant is terminated"))
}

/// The binding of the parent of each of `subs` becomes none, if it still
/// names that sub-grant; the parents are read in one query and written in one
/// store call.
async fn release_parents<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    subs: &[Grant],
    now: OffsetDateTime,
) -> Result<(), DomainError> {
    let mut out = Vec::new();
    let parents: Vec<GrantId> = subs.iter().filter_map(|g| g.parent_grant_id).collect();
    // SAFETY(cross-tenant): the parents are held by the grantor; the verb on
    // `subs` releases them.
    for mut p in store
        .grants(tx, &AccessScope::allow_all(), &parents, true)
        .await?
    {
        if matches!(p.binding, Binding::Delegated { grant_id, .. }
            if subs.iter().any(|s| s.id == grant_id && s.parent_grant_id == Some(p.id)))
        {
            p.binding = Binding::None;
            bump(&mut p);
            out.push(p);
        }
    }
    write_all(tx, store, &out, now).await
}

/// The verb ends a sub-grant and gives its slot back to the parent.
fn releases(verb: Verb) -> bool {
    matches!(verb, Verb::Reject | Verb::Return | Verb::Recall)
}

/// Apply `verb` to the locked live grant `g`, which `applies` allowed, and
/// release the parent where the verb ends a sub-grant.
async fn apply<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    verb: Verb,
    g: Grant,
    now: OffsetDateTime,
) -> Result<Grant, DomainError> {
    let subtree = if matches!(verb, Verb::Revoke | Verb::Recall) {
        store.live_subtree(tx, g.id, true).await?
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    let done = apply_one(verb, g, subtree, &mut out)?;
    write_all(tx, store, &out, now).await?;
    if releases(verb) {
        release_parents(tx, store, std::slice::from_ref(&done), now).await?;
    }
    Ok(done)
}

/// [`apply`] into `out`, without the write and the release of the parent. For
/// Revoke and Recall, `subtree` is the locked live subtree of `g`.
fn apply_one(
    verb: Verb,
    mut g: Grant,
    subtree: Vec<Grant>,
    out: &mut Vec<Grant>,
) -> Result<Grant, DomainError> {
    match verb {
        Verb::Accept | Verb::Suspend | Verb::Reinstate => {
            g.state = match verb {
                Verb::Suspend => GrantState::Suspended,
                _ => GrantState::Active,
            };
            bump(&mut g);
            out.push(g.clone());
            Ok(g)
        }
        Verb::Reject | Verb::Return => {
            g.state = GrantState::Terminated;
            g.termination_reason = Some(if verb == Verb::Reject {
                TerminationReason::Rejected
            } else {
                TerminationReason::Returned
            });
            bump(&mut g);
            out.push(g.clone());
            Ok(g)
        }
        Verb::Revoke => terminate_rows(subtree, g.id, TerminationReason::Revoked, out),
        Verb::Recall => terminate_rows(subtree, g.id, TerminationReason::Recalled, out),
    }
}

/// `Ok(true)` when `new` renews `current`: it contains it and does not expire
/// earlier. `Ok(false)` when it is the same window. Renewing a grant and
/// renewing a pack hold to it.
fn renews(new: &Window, current: &Window) -> Result<bool, &'static str> {
    if new == current {
        Ok(false)
    } else if inside(new, current) && new.expires_at >= current.expires_at {
        Ok(true)
    } else {
        Err("renew extends the window; the new window does not contain it")
    }
}

/// The locked members of pack `id` of `holder`, and the pack at
/// `expected_version`. Every pack writer locks the members (the roots) before
/// it reads the version, so the read sees the commit of the writer before it.
pub(super) async fn locked_pack<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    scope: &AccessScope,
    holder: Uuid,
    id: PackId,
    expected_version: Option<u64>,
) -> Result<(Vec<Grant>, Pack), DomainError> {
    let members = store.pack_grants(tx, holder, id, true).await?;
    let pack = store
        .pack(tx, scope, id)
        .await?
        .ok_or_else(|| not_found(Missing::Pack, id.0))?;
    if expected_version.is_some_and(|v| v != pack.version) {
        return Err(DomainError::VersionMismatch);
    }
    Ok((members, pack))
}

/// Give `top` the window `window`, and each live descendant in `subtree`, the
/// locked live subtree of `top`, the same window; put them into `out`.
fn renew_one(
    top: &Grant,
    subtree: Vec<Grant>,
    window: Window,
    out: &mut Vec<Grant>,
) -> Result<Grant, DomainError> {
    match renews(&window, top.validity.window()) {
        Ok(true) => {}
        Ok(false) => return Err(DomainError::transition("the grant already has this window")),
        Err(why) => return Err(DomainError::transition(why)),
    }
    let mut renewed = None;
    for mut g in subtree {
        g.validity = if g.id == top.id {
            Validity::Own { window }
        } else {
            Validity::Inherited { window }
        };
        bump(&mut g);
        if g.id == top.id {
            renewed = Some(g.clone());
        }
        out.push(g);
    }
    renewed.ok_or_else(|| DomainError::Internal(format!("grant {} is not live", top.id)))
}

/// The grant `id` for a verb: through the PDP scope for the holder and the
/// vendor; for the grantor, a sub-grant that `tenant` delegated, in a scope
/// that allows `tenant`.
async fn authorize<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    scope: &AccessScope,
    id: GrantId,
    by_grantor: bool,
    tenant: Uuid,
    lock: bool,
) -> Result<Grant, DomainError> {
    let g = if by_grantor {
        store
            .grantor_grant(tx, tenant, id, lock)
            .await?
            .filter(|_| admits(scope, tenant, id.0))
    } else {
        store.grant(tx, scope, id, lock).await?
    };
    g.ok_or_else(|| not_found(Missing::Grant, id.0))
}

/// The order of every operation that changes a tree: `read` without locks
/// decides access, the roots of the grants that `ids` names in its result are
/// locked, and `read` with locks gives the rows to act on. `read` refuses what
/// the caller may not act on, so no row of another tenant is locked before
/// access is decided.
pub(super) async fn read_locked<R: GrantRepo, T, F>(
    tx: &DbTx<'_>,
    store: R,
    ids: impl FnOnce(&T) -> Vec<GrantId>,
    read: impl Fn(bool) -> F,
) -> Result<T, DomainError>
where
    F: std::future::Future<Output = Result<T, DomainError>>,
{
    let unlocked = read(false).await?;
    store.lock_roots(tx, &ids(&unlocked)).await?;
    read(true).await
}

/// [`authorize`] under [`read_locked`].
async fn authorize_locked<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    scope: &AccessScope,
    id: GrantId,
    by_grantor: bool,
    tenant: Uuid,
) -> Result<Grant, DomainError> {
    read_locked(
        tx,
        store,
        |_| vec![id],
        |lock| authorize(tx, store, scope, id, by_grantor, tenant, lock),
    )
    .await
}

/// A pack member takes a vendor verb, accept, and reject through its pack.
fn not_a_pack_member(g: &Grant) -> Result<(), DomainError> {
    if g.pack_id.is_some() {
        return Err(DomainError::transition(
            "a pack member takes this verb through its pack",
        ));
    }
    Ok(())
}

/// A vendor verb on the root `g`: the plugin serves the `issue` intake, and
/// `g` is not a pack member.
fn vendor_verb_on(g: &Grant, issue: bool) -> Result<(), DomainError> {
    if !issue {
        return Err(DomainError::IntakeNotServed);
    }
    not_a_pack_member(g)
}

/// The rule of a verb over several members (pack or delegation): terminated
/// members and members already there are skipped; one member that the verb
/// does not allow refuses the whole operation; nothing to change refuses it.
fn converge(
    members: &[Grant],
    mut changes: impl FnMut(&Grant) -> Result<bool, &'static str>,
) -> Result<Vec<&Grant>, DomainError> {
    let mut apply = Vec::new();
    let mut blocked = Vec::new();
    for m in members.iter().filter(|m| m.state != GrantState::Terminated) {
        match changes(m) {
            Ok(true) => apply.push(m),
            Ok(false) => {}
            Err(why) => blocked.push(format!("{}: {why}", m.id)),
        }
    }
    if !blocked.is_empty() {
        return Err(DomainError::transition(format!(
            "members block the verb: {}",
            blocked.join("; ")
        )));
    }
    if apply.is_empty() {
        return Err(DomainError::transition("no member changes"));
    }
    Ok(apply)
}

/// The delegation of these sub-grants as `tenant` sees it: the recipient its
/// sub-grants, the grantor its own grants that it delegated. The grantor
/// reads no row of the recipient.
async fn delegation_of<R: GrantRepo, C: DBRunner>(
    runner: &C,
    store: R,
    tenant: Uuid,
    id: DelegationId,
    (sub_grants, version): (Vec<Grant>, u64),
) -> Result<Delegation, DomainError> {
    let first = sub_grants
        .first()
        .ok_or_else(|| not_found(Missing::Delegation, id.0))?;
    let grantor = first
        .grantor_tenant_id
        .ok_or_else(|| DomainError::Internal(format!("sub-grant {} has no grantor", first.id)))?;
    let recipient = first.holder_tenant_id;
    let grants = if tenant == grantor {
        let parents: Vec<GrantId> = sub_grants
            .iter()
            .filter_map(|g| g.parent_grant_id)
            .collect();
        // SAFETY(own tenant): the grantor reads the grants that it delegated,
        // after the PDP allowed it the delegation; these rows are the answer.
        let mut own: HashMap<GrantId, Grant> = store
            .grants(runner, &AccessScope::for_tenant(tenant), &parents, false)
            .await?
            .into_iter()
            .map(|g| (g.id, g))
            .collect();
        let grants = parents
            .iter()
            .map(|p| {
                own.remove(p).ok_or_else(|| {
                    DomainError::Internal(format!("grant {p} of the grantor is missing"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        DelegationGrants::Given { grants }
    } else {
        DelegationGrants::Received { grants: sub_grants }
    };
    Ok(Delegation::new(id, grantor, recipient, grants, version))
}

/// The grant that a verb on `g` answers with: the grantor gets its own grant,
/// not the row of the recipient.
async fn answer<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    tenant: Uuid,
    as_grantor: bool,
    g: Grant,
) -> Result<Grant, DomainError> {
    match g.parent_grant_id {
        Some(p) if as_grantor => own_grant(tx, store, tenant, p).await,
        _ => Ok(g),
    }
}

/// `expected_version` against the row that the caller sees in the answer:
/// for the grantor its own grant, the parent of `g`. The grantor does not
/// read the rows of the recipient, so neither their version.
async fn check_version<R: GrantRepo>(
    tx: &DbTx<'_>,
    store: R,
    tenant: Uuid,
    as_grantor: bool,
    g: &Grant,
    expected: Option<u64>,
) -> Result<(), DomainError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let seen = match g.parent_grant_id {
        Some(p) if as_grantor => own_grant(tx, store, tenant, p).await?.version,
        _ => g.version,
    };
    if seen == expected {
        Ok(())
    } else {
        Err(DomainError::VersionMismatch)
    }
}

/// A grant that `tenant` holds and the caller reached through its own
/// delegation: its absence is a broken invariant.
async fn own_grant<R: GrantRepo, C: DBRunner>(
    runner: &C,
    store: R,
    tenant: Uuid,
    id: GrantId,
) -> Result<Grant, DomainError> {
    // SAFETY(own tenant): the grantor reads its own grant that it delegated,
    // after the PDP allowed it the verb on the sub-grant.
    store
        .grant(runner, &AccessScope::for_tenant(tenant), id, false)
        .await?
        .ok_or_else(|| DomainError::Internal(format!("grant {id} of the grantor is missing")))
}

impl<R: GrantRepo> GrantService<R> {
    fn intakes(&self) -> Intakes {
        self.plugin.intakes()
    }

    /// A plugin that verifies evidence takes the window from the evidence, so
    /// a window renewed in the rows would not apply (DESIGN §3.1, `renew`).
    fn renews_in_place(&self) -> Result<(), DomainError> {
        if self.plugin.info().verifies_evidence {
            Err(DomainError::RenewNeedsNewEvidence)
        } else {
            Ok(())
        }
    }

    /// A verb on one grant.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn grant_verb(
        &self,
        ctx: &super::SecurityContext,
        id: GrantId,
        verb: Verb,
        token: String,
        expected_version: Option<u64>,
    ) -> Result<Grant, DomainError> {
        let op = format!("{}_grant", verb.action());
        let (scope, hash) = match self
            .prepare(
                ctx,
                Vec::new(),
                &op,
                &(&id, &expected_version),
                &token,
                &resources::GRANT,
                verb.action(),
                Some(id.0),
                !verb.of_vendor(),
            )
            .await?
        {
            Prepared::Replayed(g, _) => return Ok(g),
            Prepared::Fresh { scope, hash } => (scope, hash),
        };
        let issue = self.intakes().issue;
        let tenant = ctx.subject_tenant_id();
        self.commit(ctx, token, hash, move |tx, store| {
            Box::pin(async move {
                let g = authorize_locked(tx, store, &scope, id, verb.of_grantor(), tenant).await?;
                check_version(tx, store, tenant, verb.of_grantor(), &g, expected_version).await?;
                if verb.of_vendor() {
                    vendor_verb_on(&g, issue)?;
                } else if verb.of_pack_holder() {
                    not_a_pack_member(&g)?;
                }
                if g.state == GrantState::Terminated {
                    return Err(DomainError::transition("the grant is terminated"));
                }
                match verb.applies(&g) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(DomainError::transition(
                            "the grant is already in that state",
                        ));
                    }
                    Err(why) => return Err(DomainError::transition(why)),
                }
                let done = apply(tx, store, verb, g, OffsetDateTime::now_utc()).await?;
                answer(tx, store, tenant, verb.of_grantor(), done).await
            })
        })
        .await
    }

    /// Renew a root: the vendor extends its window, and its sub-grants follow.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn renew_grant(
        &self,
        ctx: &super::SecurityContext,
        id: GrantId,
        mut window: WindowInput,
        token: String,
        expected_version: Option<u64>,
    ) -> Result<Grant, DomainError> {
        self.renews_in_place()?;
        stored_window(&mut window);
        let mut violations = Vec::new();
        window_violations(
            "window",
            &window,
            OffsetDateTime::now_utc(),
            &mut violations,
        );
        let tenant = ctx.subject_tenant_id();
        let (scope, hash) = match self
            .prepare(
                ctx,
                violations,
                "renew_grant",
                &(&id, &window, &expected_version),
                &token,
                &resources::GRANT,
                actions::RENEW,
                Some(id.0),
                false,
            )
            .await?
        {
            Prepared::Replayed(g, _) => return Ok(g),
            Prepared::Fresh { scope, hash } => (scope, hash),
        };
        let issue = self.intakes().issue;
        let window: Window = window.into();
        self.commit(ctx, token, hash, move |tx, store| {
            Box::pin(async move {
                let g = authorize_locked(tx, store, &scope, id, false, tenant).await?;
                // A sub-grant takes the window of its parent.
                if g.parent_grant_id.is_some() {
                    return Err(DomainError::transition("the verb applies to a root only"));
                }
                if expected_version.is_some_and(|v| v != g.version) {
                    return Err(DomainError::VersionMismatch);
                }
                if g.state == GrantState::Terminated {
                    return Err(DomainError::transition("the grant is terminated"));
                }
                vendor_verb_on(&g, issue)?;
                let subtree = store.live_subtree(tx, g.id, true).await?;
                let mut out = Vec::new();
                let renewed = renew_one(&g, subtree, window, &mut out)?;
                write_all(tx, store, &out, OffsetDateTime::now_utc()).await?;
                Ok(renewed)
            })
        })
        .await
    }

    /// A verb on every member of a pack in one transaction: of the vendor, or
    /// accept and reject of the holder.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn pack_verb(
        &self,
        ctx: &super::SecurityContext,
        id: PackId,
        verb: Verb,
        token: String,
        expected_version: Option<u64>,
    ) -> Result<Pack, DomainError> {
        self.pack_change(ctx, id, PackChange::Verb(verb), token, expected_version)
            .await
    }

    /// Renew every member of a pack in one transaction.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn renew_pack(
        &self,
        ctx: &super::SecurityContext,
        id: PackId,
        mut window: WindowInput,
        token: String,
        expected_version: Option<u64>,
    ) -> Result<Pack, DomainError> {
        self.renews_in_place()?;
        stored_window(&mut window);
        self.pack_change(ctx, id, PackChange::Renew(window), token, expected_version)
            .await
    }

    async fn pack_change(
        &self,
        ctx: &super::SecurityContext,
        id: PackId,
        change: PackChange,
        token: String,
        expected_version: Option<u64>,
    ) -> Result<Pack, DomainError> {
        let (op, action) = match change {
            PackChange::Verb(v) if v.of_vendor() || v.of_pack_holder() => {
                (format!("{}_pack", v.action()), v.action())
            }
            PackChange::Verb(_) => {
                return Err(DomainError::transition("the verb does not apply to a pack"));
            }
            PackChange::Renew(_) => ("renew_pack".to_owned(), actions::RENEW),
        };
        // The holder acts in its own tenant, as on one grant.
        let by_holder = matches!(change, PackChange::Verb(v) if v.of_pack_holder());
        let mut violations = Vec::new();
        if let PackChange::Renew(w) = &change {
            window_violations("window", w, OffsetDateTime::now_utc(), &mut violations);
        }
        let (scope, hash) = match self
            .prepare(
                ctx,
                violations,
                &op,
                &(&id, &change, &expected_version),
                &token,
                &resources::PACK,
                action,
                Some(id.0),
                by_holder,
            )
            .await?
        {
            Prepared::Replayed(p, _) => return Ok(p),
            Prepared::Fresh { scope, hash } => (scope, hash),
        };
        if !by_holder && !self.intakes().issue {
            return Err(DomainError::IntakeNotServed);
        }
        let grants_scope = self
            .scope(ctx, &resources::GRANT, action, None, by_holder)
            .await?;
        self.commit(ctx, token, hash, move |tx, store| {
            Box::pin(async move {
                let holder = store
                    .pack(tx, &scope, id)
                    .await?
                    .ok_or_else(|| not_found(Missing::Pack, id.0))?
                    .holder_tenant_id;
                // A caller refused on the members locks none of them. A member
                // stays in its pack (I9) and the members end together, so the
                // locked read holds no member this check did not see.
                members_admitted(
                    &grants_scope,
                    &store.pack_grants(tx, holder, id, false).await?,
                )?;
                let (members, pack) =
                    locked_pack(tx, store, &scope, holder, id, expected_version).await?;
                let now = OffsetDateTime::now_utc();
                match change {
                    PackChange::Verb(verb) => {
                        let apply_to = converge(&members, |m| verb.applies(m))?;
                        let mut subtrees = if verb == Verb::Revoke {
                            let ids: Vec<GrantId> = apply_to.iter().map(|m| m.id).collect();
                            store.live_subtrees(tx, &ids, true).await?
                        } else {
                            HashMap::new()
                        };
                        let mut out = Vec::new();
                        for m in apply_to {
                            let subtree = subtrees.remove(&m.id).unwrap_or_default();
                            apply_one(verb, m.clone(), subtree, &mut out)?;
                        }
                        write_all(tx, store, &out, now).await?;
                    }
                    PackChange::Renew(w) => {
                        let window: Window = w.into();
                        let apply_to =
                            converge(&members, |m| renews(&window, m.validity.window()))?;
                        let ids: Vec<GrantId> = apply_to.iter().map(|m| m.id).collect();
                        let mut subtrees = store.live_subtrees(tx, &ids, true).await?;
                        let mut out = Vec::new();
                        for m in apply_to {
                            let subtree = subtrees.remove(&m.id).unwrap_or_default();
                            renew_one(m, subtree, window, &mut out)?;
                        }
                        write_all(tx, store, &out, now).await?;
                    }
                }
                store
                    .update_pack_version(tx, &scope, id, pack.version + 1)
                    .await?;
                store
                    .pack(tx, &scope, id)
                    .await?
                    .ok_or_else(|| not_found(Missing::Pack, id.0))
            })
        })
        .await
    }

    /// A verb on every sub-grant of a delegation in one transaction.
    ///
    /// # Errors
    ///
    /// See DESIGN §3.3, Error model.
    pub async fn delegation_verb(
        &self,
        ctx: &super::SecurityContext,
        id: DelegationId,
        verb: Verb,
        token: String,
        expected_version: Option<u64>,
    ) -> Result<Delegation, DomainError> {
        if !matches!(
            verb,
            Verb::Accept | Verb::Reject | Verb::Return | Verb::Recall
        ) {
            return Err(DomainError::transition(
                "the verb does not apply to a delegation",
            ));
        }
        let op = format!("{}_delegation", verb.action());
        let (scope, hash) = match self
            .prepare(
                ctx,
                Vec::new(),
                &op,
                &(&id, &expected_version),
                &token,
                &resources::DELEGATION,
                verb.action(),
                Some(id.0),
                true,
            )
            .await?
        {
            Prepared::Replayed(d, _) => return Ok(d),
            Prepared::Fresh { scope, hash } => (scope, hash),
        };
        let tenant = ctx.subject_tenant_id();
        if !admits(&scope, tenant, id.0) {
            return Err(not_found(Missing::Delegation, id.0));
        }
        let grants_scope = self
            .scope(ctx, &resources::GRANT, verb.action(), None, true)
            .await?;
        self.commit(ctx, token, hash, move |tx, store| {
            Box::pin(async move {
                let grants_scope = &grants_scope;
                // The side of the verb acts on it: the grantor recalls; the
                // recipient accepts, rejects, and returns.
                let acts = |members: &[Grant]| {
                    !members.is_empty()
                        && members.iter().all(|g| {
                            let actor = if verb.of_grantor() {
                                g.grantor_tenant_id
                            } else {
                                Some(g.holder_tenant_id)
                            };
                            actor == Some(tenant)
                        })
                };
                // The other side gets `NotFound` before it locks the trees.
                let (members, version) = read_locked(
                    tx,
                    store,
                    |(m, _): &(Vec<Grant>, u64)| m.iter().map(|g| g.id).collect(),
                    |lock| async move {
                        let read = store.delegation_grants(tx, tenant, id, lock).await?;
                        if !acts(&read.0) {
                            return Err(not_found(Missing::Delegation, id.0));
                        }
                        side_admitted(grants_scope, tenant, &read.0)?;
                        Ok(read)
                    },
                )
                .await?;
                if expected_version.is_some_and(|v| v != version) {
                    return Err(DomainError::VersionMismatch);
                }
                let now = OffsetDateTime::now_utc();
                // Members sit in disjoint subtrees (each under its own parent
                // of the grantor), so a verb on one does not change another;
                // the version check of each write would catch it if it did.
                let apply_to = converge(&members, |m| verb.applies(m))?;
                let ids: Vec<GrantId> = apply_to.iter().map(|m| m.id).collect();
                let mut subtrees = if verb == Verb::Recall {
                    store.live_subtrees(tx, &ids, true).await?
                } else {
                    HashMap::new()
                };
                let mut released = Vec::with_capacity(apply_to.len());
                let mut out = Vec::new();
                for m in apply_to {
                    let subtree = subtrees.remove(&m.id).unwrap_or_default();
                    released.push(apply_one(verb, m.clone(), subtree, &mut out)?);
                }
                write_all(tx, store, &out, now).await?;
                if releases(verb) {
                    release_parents(tx, store, &released, now).await?;
                }
                store.set_delegation_version(tx, id, version + 1).await?;
                let members = store.delegation_grants(tx, tenant, id, false).await?;
                delegation_of(tx, store, tenant, id, members).await
            })
        })
        .await
    }

    /// Read a delegation as its grantor or its recipient.
    ///
    /// # Errors
    ///
    /// `NotFound` for another caller, or outside the caller's scope.
    pub async fn get_delegation(
        &self,
        ctx: &super::SecurityContext,
        id: DelegationId,
    ) -> Result<Delegation, DomainError> {
        let scope = self
            .scope(ctx, &resources::DELEGATION, actions::GET, Some(id.0), true)
            .await?;
        let tenant = ctx.subject_tenant_id();
        if !admits(&scope, tenant, id.0) {
            return Err(not_found(Missing::Delegation, id.0));
        }
        let grants_scope = self
            .scope(ctx, &resources::GRANT, actions::GET, None, true)
            .await?;
        let store = self.store;
        // One snapshot: a verb that commits between the two reads would join
        // rows of two states.
        self.db
            .transaction_with_config(self.store.snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    let members = store.delegation_grants(tx, tenant, id, false).await?;
                    side_admitted(&grants_scope, tenant, &members.0)?;
                    delegation_of(tx, store, tenant, id, members).await
                })
            })
            .await
    }
}

/// A delegation verb or read reaches the grants of the caller's side: the
/// sub-grants that the recipient holds, the grants that the grantor
/// delegated. `scope`, the decision on grants, must allow each live one, as
/// on a pack.
fn side_admitted(scope: &AccessScope, tenant: Uuid, members: &[Grant]) -> Result<(), DomainError> {
    let refused = members
        .iter()
        .filter(|m| m.state != GrantState::Terminated)
        .find(|m| {
            let own = if m.holder_tenant_id == tenant {
                Some(m.id)
            } else {
                m.parent_grant_id
            };
            !own.is_some_and(|id| admits(scope, tenant, id.0))
        });
    match refused {
        None => Ok(()),
        Some(m) => {
            tracing::info!(grant = %m.id, "license-enforcement: the grant decision does not allow a grant of the delegation");
            Err(DomainError::PermissionDenied)
        }
    }
}
/// What a pack verb does. Its serde names enter stored idempotency hashes.
#[derive(Debug, Clone, Copy, Serialize)]
enum PackChange {
    #[serde(rename = "Verb")]
    Verb(Verb),
    #[serde(rename = "Renew")]
    Renew(WindowInput),
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
