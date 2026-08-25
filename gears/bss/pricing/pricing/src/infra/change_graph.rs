//! What the publish path has to read before it can judge a plan-change contract
//! (`design/06-consumer-contracts.md` `inst-pc-targets`, `inst-pc-mutual`, D-54).
//!
//! [`ChangeGraphAuthorable`](crate::domain::contracts::ChangeGraphAuthorable) is
//! a statement about **other plans** — are the targets published, do they carry
//! a rank, and does anyone already point here — and a walk over one
//! [`PlanShape`](crate::domain::plan_shape::PlanShape) cannot answer any of the
//! three. So the facts are resolved once, before the rule set runs, and handed
//! in: the same shape `infra::bundle::referencing_markets` takes for
//! `inst-bc-taxbasis`'s reverse half, and for the same two reasons — the rule is
//! a property of another aggregate, and the set runs **twice** on one publish
//! (approval approves content, the commit re-validates state) with the same
//! answer required both times.
//!
//! # Both directions, and the inbound one is D-54
//!
//! The outbound half is the obvious one: for each target this plan names, is
//! there a published revision and what rank does it carry.
//!
//! The inbound half is the guard D-54 added after the fact, and it reads the
//! opposite way — **which published plans name *this* plan**. Without it a plan
//! could re-publish with its `comparability_rank` dropped to NULL while every
//! edge pointing at it stayed published, leaving those edges unclassifiable at
//! read time. That is the same read-time drift D-23 cut rule-based targets to
//! avoid, arriving through the other end of the edge; D-24 covers only the
//! *retirement* case, which is a different fact and is Subscriptions' to re-check
//! at change time.
//!
//! # Published revisions only, and `retired` is not one of them
//!
//! A target is resolvable when the plan has a `published` revision. `retired` is
//! deliberately **not** admitted: D-24 makes an edge to a retired plan *inert*
//! rather than invalid — Subscriptions re-checks lifecycle at change time — so a
//! publish must not be refused for it, and this resolver must not report the
//! plan as a live target either. Both readings are wrong in opposite directions,
//! and the one that matters is that a retired plan cannot become the *reason* a
//! source plan's publish fails. Since a retired plan's revision is `retired` and
//! not `published`, it simply does not enter the index — and the source plan's
//! edge to it is refused as unpublished.
//!
//! **That last clause is a known divergence from D-24 and it is reported rather
//! than papered over.** D-24 says a retired target's edge is inert; this
//! resolver makes it a `CHANGE_TARGET_UNPUBLISHED` refusal on the *source's next
//! publish*. The gap is narrow — an edge authored to a live plan keeps working
//! until the source re-publishes — but it is real, and closing it needs the
//! index to distinguish "never published" from "published then retired", which is
//! a second lifecycle token in the map and a rule arm to read it. Recorded in the
//! hand-back rather than built, because D-24's own text puts the re-check in
//! Subscriptions and no code in this gear says which way a *publish* should go.

use std::collections::{BTreeMap, BTreeSet};

use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use uuid::Uuid;

use crate::domain::contracts::ChangeTargetIndex;
use crate::domain::lifecycle::LifecycleState;
use crate::domain::scope_key::PlanId;
use crate::infra::storage::RepoError;
use crate::infra::storage::entity::plan;

/// One published revision, reduced to the two facts the change graph needs.
struct PublishedPlan {
    /// The rank it publishes, when it carries one.
    rank: Option<i32>,
    /// The `planId`s its own `allowedChangeTargets` names.
    edges: BTreeSet<Uuid>,
}

/// Resolve both halves of the change graph around `subject`.
///
/// # Errors
/// [`RepoError::Db`] if the read fails; [`RepoError::CorruptRow`] if a stored
/// edge list is not an array of `planId`s.
pub async fn resolve(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    subject: PlanId,
    targets: &[Uuid],
) -> Result<ChangeTargetIndex, RepoError> {
    let published = published_plans(runner, scope, tenant_id).await?;
    let subject_id = subject.get();

    // The outbound half is narrowed to the edges this publish actually names.
    // Handing the rule the tenant's whole plan set would work and would make the
    // index's size a function of the catalog rather than of the contract under
    // judgement.
    let named: BTreeSet<Uuid> = targets.iter().copied().collect();
    let published_targets: BTreeMap<Uuid, Option<i32>> = published
        .iter()
        .filter(|(plan_id, _)| named.contains(*plan_id))
        .map(|(plan_id, entry)| (*plan_id, entry.rank))
        .collect();

    // The inbound half: every published plan whose own edge list names the
    // subject. Read off the same rows rather than with a second query -- the
    // edge list is a column of the revision already in hand, and a `jsonb`
    // containment filter would have to be written once per engine.
    //
    // The subject excludes **itself**: a self-edge is a plan naming its own
    // published revision, and reading it as an inbound reference would make a
    // plan unable to ever drop its own rank.
    let inbound_edges: BTreeSet<Uuid> = published
        .iter()
        .filter(|(plan_id, entry)| **plan_id != subject_id && entry.edges.contains(&subject_id))
        .map(|(plan_id, _)| *plan_id)
        .collect();

    Ok(ChangeTargetIndex::new(published_targets, inbound_edges))
}

/// Every `published` revision of the tenant, by plan.
///
/// **`published` only.** `retired` is excluded deliberately; the module doc
/// carries the argument and the divergence from D-24 it leaves open.
async fn published_plans(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<BTreeMap<Uuid, PublishedPlan>, RepoError> {
    let rows = plan::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(plan::Column::TenantId.eq(tenant_id))
                .add(plan::Column::LifecycleState.eq(LifecycleState::Published.as_str())),
        )
        .all(runner)
        .await
        .map_err(|e| RepoError::Db(format!("read the tenant's published plans: {e}")))?;

    let mut out = BTreeMap::new();
    for row in rows {
        let edges = read_edges(row.plan_id, row.allowed_change_targets.as_ref())?;
        out.insert(
            row.plan_id,
            PublishedPlan {
                rank: row.comparability_rank,
                edges,
            },
        );
    }
    Ok(out)
}

/// The stored edge list as a set, refusing anything the writer cannot produce.
///
/// A malformed value is **corruption**, not an absent edge list: reading it as
/// empty would silently drop an inbound reference and let the plan it points at
/// revoke its rank -- which is the whole of what D-54 exists to stop.
fn read_edges(
    plan_id: Uuid,
    stored: Option<&sea_orm::JsonValue>,
) -> Result<BTreeSet<Uuid>, RepoError> {
    let Some(value) = stored else {
        return Ok(BTreeSet::new());
    };
    let sea_orm::JsonValue::Array(items) = value else {
        return Err(RepoError::CorruptRow(format!(
            "pricing_plan {plan_id}: allowed_change_targets holds {value}, which is not an array"
        )));
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| {
                    RepoError::CorruptRow(format!(
                        "pricing_plan {plan_id}: allowed_change_targets holds {item}, which is \
                         not a planId"
                    ))
                })
        })
        .collect()
}
