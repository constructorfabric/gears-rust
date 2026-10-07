//! The `plan_revision` approval subject (spec §6): one draft revision of a plan, published on
//! approval with its predecessor superseded and the plan's `published_rev` advanced, in the
//! door's serializable transaction.
//!
//! The fingerprinted `after` is the revision's business content only: its book, its sale date
//! and its items (SKU and entry, D-467), in SKU order —
//! never a version, a lock or an item's reference columns, which the reference machine moves
//! while the unit is pending. The item SKUs' current descriptors are read fresh for the reviewer
//! and kept beside `after`, never in it: a GL change must not refresh a pending unit (D-408).
//! Submit and apply judge the revision with the checks of `GET /plan-revisions/{id}/checks`,
//! built by the same function from the same fresh reads.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-revision-unit:p1
use crate::{
    api::rest::authoring::{
        plans,
        support::{self, DoorError},
    },
    domain::plan::{self, RevisionState},
    infra::{
        events::{self, PlanRevisionPublished, TxOutbox},
        reference_ticker::system_actor,
        storage::{
            RepoError,
            entity::{plan_item, plan_revision},
            repo::{approval_repo, plan_item_repo, plan_repo, plan_revision_repo},
        },
    },
};
use bss_approval::{ApprovalError, ApprovalSubject, ItemRef, Unit};
use bss_products_sdk::models::Sku;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, PoisonError},
};
use time::{Date, OffsetDateTime};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{
    DbTx,
    secure::{AccessScope, DBRunner},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The approval kind of one plan revision.
pub const KIND_PLAN_REVISION: &str = crate::domain::plan::KIND_PLAN_REVISION;
/// A `plan_revision` unit references its revision.
pub const REF_TYPE: &str = "plan_revision";
/// The one item of a `plan_revision` unit is the revision.
pub const ITEM_TYPE: &str = "plan_revision";
/// The honest answer for the subscriptions a unit touches: pricing cannot count them before
/// the Subscriptions integration reports its pins.
pub const SUBSCRIPTIONS_UNAVAILABLE: &str = "unavailable until the Subscriptions integration";

/// The stored revisions as the effective-state rule reads them (D-447), in their order; a state
/// outside the closed set is a corrupt row naming the revision (D-439).
/// # Errors
/// `CorruptRow`.
pub fn stored_revisions(
    rows: &[plan_revision::Model],
) -> Result<Vec<plan::StoredRevision>, RepoError> {
    rows.iter()
        .map(|m| {
            Ok(plan::StoredRevision {
                id: m.id,
                plan_id: m.plan_id,
                rev_no: m.rev_no,
                state: m.state.parse::<RevisionState>().map_err(|_| {
                    RepoError::CorruptRow(format!(
                        "revision {} state: {:?} is not a revision state",
                        m.id, m.state
                    ))
                })?,
                available_from: m.available_from,
                published_at: m.published_at,
            })
        })
        .collect()
}
/// The revisions as they read on `today` (D-447), in the order of `rows`: a due scheduled
/// revision reads published, its plan's stored-published one superseded. A read derives; it never
/// writes.
/// # Errors
/// `CorruptRow` for a state outside the closed set.
pub fn effective_revisions(
    rows: &[plan_revision::Model],
    today: Date,
) -> Result<Vec<plan::EffectiveRevision>, RepoError> {
    Ok(plan::effective(&stored_revisions(rows)?, today))
}
/// The item SKUs of the published revision in effect among ONE plan's `revisions` as they read on
/// a day (D-447): the SKUs a deprecated SKU may be carried from (D-408) and added again (D-465).
/// # Errors
/// Storage failures.
pub async fn in_effect_skus(
    tx: &impl DBRunner,
    children: &AccessScope,
    tenant: Uuid,
    revisions: &[plan::EffectiveRevision],
) -> Result<Vec<Uuid>, RepoError> {
    Ok(match plan::in_effect(revisions) {
        Some(published) => plan_item_repo::for_revision(tx, children, tenant, published.id)
            .await?
            .into_iter()
            .map(|i| i.sku_id)
            .collect(),
        None => Vec::new(),
    })
}
/// The item SKUs of the published revision in effect on `today` of the plan `plan_id` (D-465):
/// a deprecated SKU among them may be added to its draft again, as the checks carry it (D-408).
/// The item door and the create op's SKU re-read judge by it; a clone is a new plan, with none.
/// # Errors
/// Storage failures; `CorruptRow` for a state outside the closed set.
pub async fn published_skus(
    tx: &impl DBRunner,
    tenant: Uuid,
    plan_id: Uuid,
    today: Date,
) -> Result<Vec<Uuid>, RepoError> {
    let children = AccessScope::for_tenant(tenant);
    let revisions = effective_revisions(
        &plan_revision_repo::for_plan(tx, &children, tenant, plan_id).await?,
        today,
    )?;
    in_effect_skus(tx, &children, tenant, &revisions).await
}
/// What a revision's publication touches that pricing can measure: nothing yet, since
/// publishing a revision moves no existing pin (D-394).
#[must_use]
pub fn impact() -> Value {
    json!({ "subscriptions": SUBSCRIPTIONS_UNAVAILABLE })
}

/// Who a `PlanRevisionPublished` of a persisted switch names as its actor (plan rev 2 H1): the
/// actor of the unit's latest approving decision that is neither stale nor of an older
/// generation, else the unit's submitter — at quorum 0 the unit applies at its submit and records
/// no decision.
#[must_use]
pub fn switch_actor(unit: &Unit, decisions: &[bss_approval::Decision]) -> Uuid {
    decisions
        .iter()
        .filter(|d| {
            !d.stale
                && d.generation == unit.generation
                && d.verdict == bss_approval::Verdict::Approve
        })
        .max_by_key(|d| (d.at, d.actor))
        .map_or(unit.submitted_by, |d| d.actor)
}

/// Persist a plan's due switch and announce it, in the caller's transaction (D-450, D-451):
/// [`plan_revision_repo::switch_due`], then, only when it switched, `PlanRevisionPublished` (the
/// approving unit, [`switch_actor`], the revision it superseded) and the audit row
/// `plan_revision.switch` under pricing's system actor. The switch job calls it for each due
/// plan, and the copy, clone and unschedule doors call it before they judge anything; whichever
/// switches announces, so a switch is announced exactly once. Answers what it switched.
/// # Errors
/// Storage failures, kept typed so a serializable transaction retries; a scheduled revision whose
/// unit is gone is a corrupt row.
pub async fn catch_up(
    tx: &(impl DBRunner + Sync),
    outbox: &TxOutbox,
    tenant: Uuid,
    plan_id: Uuid,
    now: OffsetDateTime,
    correlation: Uuid,
) -> Result<Option<plan_revision_repo::Switched>, DoorError> {
    let scope = AccessScope::for_tenant(tenant);
    let Some(switched) = plan_revision_repo::switch_due(tx, &scope, tenant, plan_id, now).await?
    else {
        return Ok(None);
    };
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-2
    let lost = |what: &str| {
        RepoError::CorruptRow(format!(
            "switched plan revision {} lost its {what}",
            switched.revision_id
        ))
    };
    let unit = approval_repo::find_unit(tx, &scope, tenant, switched.unit_id)
        .await
        .map_err(support::approval_failure)?
        .ok_or_else(|| lost("unit"))?;
    let decisions = approval_repo::decisions_of(tx, &scope, tenant, unit.id).await?;
    let event = PlanRevisionPublished {
        tenant_id: tenant,
        plan_id,
        revision_id: switched.revision_id,
        rev_no: switched.rev_no,
        book_id: switched.book_id,
        superseded_revision_id: switched.superseded_revision_id,
        unit_id: switched.unit_id,
        actor_ref: switch_actor(&unit, &decisions),
    };
    events::enqueue(outbox, tx, &event, now).await?;
    let published = plan_revision_repo::find(tx, &scope, tenant, switched.revision_id)
        .await?
        .ok_or_else(|| lost("row"))?;
    support::audit(
        tx,
        &system_actor(tenant)?,
        correlation,
        "plan_revision.switch",
        published.id,
        published.version,
    )
    .await?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-2
    Ok(Some(switched))
}

/// A revision's business content, the unit's `after` (and `before` for the published one): each
/// item is its SKU and its entry (D-467). A unit submitted before D-467 fingerprinted each item's
/// treatment and quantities too, so its first vote or reject after D-467 finds its content changed
/// and refreshes it once, at the next generation (`bss_approval`'s stale refresh).
#[must_use]
pub fn content(
    revision: &plan_revision::Model,
    items: &[plan_item::Model],
    entries: &[crate::infra::storage::entity::price_book_entry::Model],
) -> Value {
    let mut items: Vec<&plan_item::Model> = items.iter().collect();
    items.sort_by_key(|i| i.sku_id);
    json!({
        "book_id": revision.book_id,
        "available_from": revision.available_from.map(|d| d.to_string()),
        "items": items
            .iter()
            .map(|i| {
                let mut item = json!({
                    "sku_id": i.sku_id,
                    "price_book_entry_id": i.price_book_entry_id,
                });
                if let Some(entry) = entries.iter().find(|e| Some(e.id) == i.price_book_entry_id)
                    && let Some(id) = entry.usage_policy_id
                {
                    item["usage_policy"] = json!({"policy_id": id,
                        "version": entry.usage_policy_version.map(|v| v.to_string()),
                        "digest": entry.usage_policy_digest});
                    if let Some(version) = entry.usage_sku_version {
                        item["usage_sku_version"] = json!(version);
                    }
                }
                item
            })
            .collect::<Vec<_>>(),
    })
}

/// One SKU's current descriptors, shown to the reviewer for information only (D-408).
#[must_use]
pub fn descriptors(sku: &Sku) -> Value {
    json!({
        "sku_id": sku.id,
        "gl_code": sku.gl_code,
        "tax_category": sku.tax_category,
        "invoice_line_template": sku.invoice_line_template,
        "billing_timing": sku.billing_timing,
    })
}

/// The snapshot's `descriptors` when Products could not answer their read (D-416).
pub const DESCRIPTORS_UNAVAILABLE: &str = "unavailable";

/// The snapshot's `descriptors` from a best-effort SKU read (D-416): each SKU's descriptors, or
/// `"unavailable"` when the registry could not answer or refused the caller. Descriptors are
/// information, never content: their read never refuses a submit, a vote or a reject.
#[must_use]
pub fn descriptors_or_unavailable(read: Result<Vec<Sku>, CanonicalError>) -> Value {
    read.map_or_else(
        |_| Value::from(DESCRIPTORS_UNAVAILABLE),
        |skus| Value::Array(skus.iter().map(descriptors).collect()),
    )
}

/// What changes against the published revision: the book, the sale date, and the items added,
/// removed or changed, by SKU.
fn diff(before: Option<&Value>, after: &Value) -> Value {
    let by_sku = |content: Option<&Value>| -> BTreeMap<String, Value> {
        content
            .and_then(|c| c["items"].as_array())
            .map(|items| {
                items
                    .iter()
                    .map(|i| (i["sku_id"].to_string(), i.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (old, new) = (by_sku(before), by_sku(Some(after)));
    let field = |name: &str| {
        let was = before.map_or(Value::Null, |b| b[name].clone());
        if was == after[name] {
            Value::Null
        } else {
            json!({"before": was, "after": after[name]})
        }
    };
    json!({
        "book_id": field("book_id"),
        "available_from": field("available_from"),
        "added": new.iter().filter(|(k, _)| !old.contains_key(*k)).map(|(_, v)| v).collect::<Vec<_>>(),
        "removed": old.iter().filter(|(k, _)| !new.contains_key(*k)).map(|(_, v)| v).collect::<Vec<_>>(),
        "changed": new
            .iter()
            .filter_map(|(k, v)| old.get(k).filter(|was| *was != v).map(|was| json!({"before": was, "after": v})))
            .collect::<Vec<_>>(),
    })
}

/// What `collect` and `validate_submit` gather for the synchronous `snapshot`, and what `apply`
/// leaves for the door's `PlanRevisionPublished`.
#[derive(Default)]
struct Review {
    plan_id: Option<Uuid>,
    plan_code: Option<String>,
    rev_no: Option<i32>,
    diff: Value,
    /// The item SKUs' descriptors, or `"unavailable"` when Products did not answer the read.
    descriptors: Value,
    superseded: Option<Uuid>,
    /// Whether the last apply published the revision now (a revision approved before its sale
    /// date is scheduled instead, D-449).
    published: bool,
}

/// The subject of one `plan_revision` unit.
#[derive(Clone)]
pub struct PlanRevisionSubject {
    pub meter_observations: crate::infra::meter_semantics::Observations,
    /// The caller; the fresh SKU reads are made on its behalf.
    pub ctx: SecurityContext,
    pub hub: Arc<toolkit::ClientHub>,
    pub tenant_id: Uuid,
    pub revision_id: Uuid,
    pub now: OffsetDateTime,
    /// A refusal whose body the approval error cannot carry (a Products refusal kept whole, or
    /// the red checks), for the door to answer as is.
    refused: Arc<Mutex<Option<CanonicalError>>>,
    review: Arc<Mutex<Review>>,
}

fn invalid(code: &'static str, detail: impl Into<String>) -> ApprovalError {
    ApprovalError::InvalidSubmit {
        code,
        field: ITEM_TYPE.into(),
        detail: detail.into(),
    }
}
/// Keep contention typed for the transaction's retry; a unique arbiter refuses the apply.
fn storage(error: RepoError) -> ApprovalError {
    match error {
        RepoError::Driver { source, .. } => ApprovalError::Db(source),
        RepoError::Conflict { code } => ApprovalError::ApplyRefused {
            code,
            detail: code.into(),
        },
        other => ApprovalError::Store(other.to_string()),
    }
}
/// A submit-time refusal met again at apply is an environment change.
fn applied(error: ApprovalError) -> ApprovalError {
    match error {
        ApprovalError::InvalidSubmit { code, detail, .. } => {
            ApprovalError::ApplyRefused { code, detail }
        }
        other => other,
    }
}
fn red(checks: Vec<plan::Check>) -> Vec<plan::Check> {
    checks.into_iter().filter(|c| !c.ok).collect()
}
fn codes(red: &[plan::Check]) -> String {
    red.iter().map(|c| c.code).collect::<Vec<_>>().join(", ")
}

impl PlanRevisionSubject {
    async fn content(
        &self,
        tx: &impl DBRunner,
        revision: &plan_revision::Model,
        items: &[plan_item::Model],
    ) -> Result<Value, ApprovalError> {
        let ids = items
            .iter()
            .filter_map(|i| i.price_book_entry_id)
            .collect::<Vec<_>>();
        let entries = crate::infra::storage::repo::price_book_entry_repo::find_many(
            tx,
            &self.scope(),
            self.tenant_id,
            &ids,
        )
        .await
        .map_err(storage)?;
        Ok(content(revision, items, &entries))
    }

    /// A subject for the caller's tenant.
    #[must_use]
    pub fn new(
        ctx: SecurityContext,
        hub: Arc<toolkit::ClientHub>,
        revision_id: Uuid,
        now: OffsetDateTime,
    ) -> Self {
        let tenant_id = ctx.subject_tenant_id();
        Self {
            meter_observations: crate::infra::meter_semantics::Observations::default(),
            ctx,
            hub,
            tenant_id,
            revision_id,
            now,
            refused: Arc::default(),
            review: Arc::default(),
        }
    }
    /// The refusal that ended the last judgement with a body of its own, if any.
    #[must_use]
    pub fn take_refusal(&self) -> Option<CanonicalError> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
    /// The revision the last apply superseded, if any.
    #[must_use]
    pub fn superseded(&self) -> Option<Uuid> {
        self.review
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .superseded
    }
    /// Whether the last apply published the revision now. A revision approved before its sale
    /// date was scheduled instead (D-449): its `PlanRevisionPublished` is its switch's (D-450).
    #[must_use]
    pub fn published_now(&self) -> bool {
        self.review
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .published
    }
    fn refuse(&self, error: CanonicalError) {
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
    }
    fn scope(&self) -> AccessScope {
        AccessScope::for_tenant(self.tenant_id)
    }
    async fn revision(&self, tx: &impl DBRunner) -> Result<plan_revision::Model, ApprovalError> {
        plan_revision_repo::find(tx, &self.scope(), self.tenant_id, self.revision_id)
            .await
            .map_err(storage)?
            .ok_or_else(|| {
                invalid(
                    "REVISION_NOT_FOUND",
                    format!("revision {}", self.revision_id),
                )
            })
    }
    /// The item SKUs, read fresh for the checks (D-408), a rule: the read is hard and made as the
    /// caller, and the descriptors are kept for the snapshot. Only unavailability is
    /// `REGISTRY_UNAVAILABLE`; a definite refusal is answered as Products gave it.
    fn skus(&self, ids: impl IntoIterator<Item = Uuid>) -> Result<Vec<Sku>, ApprovalError> {
        match self.meter_observations.skus(ids) {
            Ok(skus) => {
                {
                    let mut review = self.review.lock().unwrap_or_else(PoisonError::into_inner);
                    review.descriptors = Value::Array(skus.iter().map(descriptors).collect());
                }
                Ok(skus)
            }
            Err(error) if error.status_code() == 503 => Err(invalid(
                "REGISTRY_UNAVAILABLE",
                "Products reference registry",
            )),
            Err(error) => {
                self.refuse(error);
                Err(invalid("REGISTRY_REFUSED", "Products refused a SKU read"))
            }
        }
    }
    /// The checks of `GET /plan-revisions/{id}/checks` on the stored state of this transaction
    /// and fresh SKU reads, on the subject's day (its UTC date). No scheduled revision stands
    /// beside the pending one it judges (D-451), so its plan's revisions read as stored.
    async fn judge(&self, tx: &DbTx<'_>) -> Result<Vec<plan::Check>, ApprovalError> {
        let today = self.now.to_offset(time::UtcOffset::UTC).date();
        let mut context =
            plans::stored_context(tx, &self.scope(), self.tenant_id, self.revision_id, today)
                .await
                .map_err(|error| match error {
                    DoorError::Repo(e) => storage(e),
                    DoorError::Api(e) if e.status_code() == 404 => invalid(
                        "REVISION_NOT_FOUND",
                        format!("revision {}", self.revision_id),
                    ),
                    other => ApprovalError::Store(other.to_string()),
                })?;
        let selected = context
            .items
            .iter()
            .filter_map(|i| i.price_book_entry_id)
            .collect::<Vec<_>>();
        let entries = crate::infra::storage::repo::price_book_entry_repo::find_many(
            tx,
            &self.scope(),
            self.tenant_id,
            &selected,
        )
        .await
        .map_err(storage)?;
        let policies = crate::infra::storage::repo::usage_policy_repo::for_entries(
            tx,
            self.tenant_id,
            &entries,
        )
        .await
        .map_err(storage)?;
        let entry_ids: Vec<Uuid> = entries.iter().map(|e| e.id).collect();
        let prices = crate::infra::storage::repo::price_repo::by_entry(
            crate::infra::storage::repo::price_repo::for_entries(
                tx,
                &AccessScope::for_tenant(self.tenant_id),
                self.tenant_id,
                &entry_ids,
            )
            .await
            .map_err(storage)?,
        );
        for entry in entries {
            self.meter_observations.check(&entry).map_err(|error| {
                self.refuse(error);
                invalid("METER_POLICY_REFUSED", format!("entry {}", entry.id))
            })?;
            let refuses = policies.get(&entry.id).is_some_and(|p| {
                crate::domain::usage_policy::refuses_minimum_fee(&(&p.content).into())
            });
            if refuses
                && prices.get(&entry.id).is_some_and(|rows| {
                    rows.iter().any(|p| {
                        p.state == "approved"
                            && crate::infra::storage::repo::price_repo::is_price(p)
                            && p.min_fee.is_some()
                    })
                })
            {
                return Err(invalid(
                    "UNSUPPORTED_TERMS",
                    "minimum fee on CalendarHour or a resource-scoped policy",
                ));
            }
        }
        context.skus = self.skus(context.items.iter().map(|i| i.sku_id))?;
        Ok(plan::checks(&context, today))
    }
}

#[async_trait::async_trait]
impl<'a> ApprovalSubject<DbTx<'a>> for PlanRevisionSubject {
    fn kind(&self) -> &'static str {
        KIND_PLAN_REVISION
    }
    fn ref_type(&self) -> &'static str {
        REF_TYPE
    }
    /// The revision's business content, the published revision's as `before`, the diff and the
    /// item SKUs' current descriptors (best-effort, D-416); its author is the item author
    /// separation of duties excludes.
    async fn collect(&self, tx: &DbTx<'a>, _ids: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError> {
        let r = self.revision(tx).await?;
        let scope = self.scope();
        let p = plan_repo::find(tx, &scope, self.tenant_id, r.plan_id)
            .await
            .map_err(storage)?
            .ok_or_else(|| ApprovalError::Store(format!("revision {} has no plan", r.id)))?;
        let items = plan_item_repo::for_revision(tx, &scope, self.tenant_id, r.id)
            .await
            .map_err(storage)?;
        let published = plan_revision_repo::for_plan(tx, &scope, self.tenant_id, p.id)
            .await
            .map_err(storage)?
            .into_iter()
            .find(|x| x.id != r.id && x.state == RevisionState::Published.as_str());
        let before = match published {
            Some(published) => {
                let old = plan_item_repo::for_revision(tx, &scope, self.tenant_id, published.id)
                    .await
                    .map_err(storage)?;
                Some(self.content(tx, &published, &old).await?)
            }
            None => None,
        };
        let after = self.content(tx, &r, &items).await?;
        // The descriptors are information (D-416): best-effort, never a refusal. The checks'
        // hard reads are `validate_submit`'s and `apply`'s.
        let described = descriptors_or_unavailable(
            self.meter_observations.skus(items.iter().map(|i| i.sku_id)),
        );
        {
            let mut review = self.review.lock().unwrap_or_else(PoisonError::into_inner);
            review.descriptors = described;
            review.plan_id = Some(p.id);
            review.plan_code = Some(p.code);
            review.rev_no = Some(r.rev_no);
            review.diff = diff(before.as_ref(), &after);
        }
        Ok(vec![ItemRef {
            item_type: ITEM_TYPE.into(),
            item_id: r.id,
            created_by: r.created_by,
            before,
            after,
        }])
    }
    /// An unlocked draft whose checks are all green; a red check refuses with the red checks and
    /// no unit.
    async fn validate_submit(
        &self,
        tx: &DbTx<'a>,
        _items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        let r = self.revision(tx).await?;
        if r.state != RevisionState::Draft.as_str() || r.pending_unit_id.is_some() {
            return Err(invalid("REVISION_NOT_DRAFT", format!("revision {}", r.id)));
        }
        // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-4
        let red = red(self.judge(tx).await?);
        if red.is_empty() {
            return Ok(());
        }
        self.refuse(support::checks_red(&red));
        Err(invalid("REVISION_CHECKS_RED", codes(&red)))
        // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-4
    }
    /// `pending_unit_id` on an unlocked draft at the version read; a lost write is
    /// `ROW_LOCKED_PENDING` and the whole submit rolls back.
    async fn lock(
        &self,
        tx: &DbTx<'a>,
        unit_id: Uuid,
        _items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        let r = self.revision(tx).await?;
        if plan_revision_repo::try_lock(tx, &self.scope(), self.tenant_id, r.id, unit_id, r.version)
            .await
            .map_err(storage)?
        {
            Ok(())
        } else {
            Err(ApprovalError::Locked {
                item_type: ITEM_TYPE.into(),
                item_id: r.id,
            })
        }
    }
    fn snapshot(&self, items: &[ItemRef], _common_effective_date: Option<Date>) -> Value {
        let item = items.first();
        let (plan_id, plan_code, rev_no, diff, descriptors) = {
            let r = self.review.lock().unwrap_or_else(PoisonError::into_inner);
            (
                r.plan_id,
                r.plan_code.clone(),
                r.rev_no,
                r.diff.clone(),
                r.descriptors.clone(),
            )
        };
        json!({
            "plan_id": plan_id,
            "plan_code": plan_code,
            "meter_evidence": self.meter_observations.audit(),
            "revision_id": self.revision_id,
            "rev_no": rev_no,
            "before": item.and_then(|i| i.before.clone()),
            "after": item.map(|i| i.after.clone()),
            "diff": diff,
            "descriptors": descriptors,
            "impact": impact(),
            "computed_at": self
                .now
                .format(&time::format_description::well_known::Rfc3339)
                .ok(),
        })
    }
    /// The checks again with fresh reads (red: `APPLY_REFUSED`, the whole unit rolls back); then
    /// a revision whose sale date is after the apply's UTC day is scheduled (D-449): nothing is
    /// superseded or published, and `published_rev` stays. Otherwise the published revision is
    /// superseded FIRST, this one published, and the plan's `published_rev` advanced.
    async fn apply(
        &self,
        tx: &DbTx<'a>,
        unit: &Unit,
        _items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        let r = self.revision(tx).await.map_err(applied)?;
        if r.state != RevisionState::Pending.as_str() || r.pending_unit_id != Some(unit.id) {
            return Err(ApprovalError::ApplyRefused {
                code: "REVISION_NOT_PENDING",
                detail: format!("revision {}", r.id),
            });
        }
        // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-2
        let red = red(self.judge(tx).await.map_err(applied)?);
        if !red.is_empty() {
            return Err(ApprovalError::ApplyRefused {
                code: "REVISION_CHECKS_RED",
                detail: codes(&red),
            });
        }
        // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-2
        let scope = self.scope();
        // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-5
        if r.available_from
            .is_some_and(|from| from > self.now.to_offset(time::UtcOffset::UTC).date())
        {
            // D-449: approved before its sale date, the revision waits; the plan keeps selling
            // its published revision until the switch (D-450).
            plan_revision_repo::schedule(tx, &scope, self.tenant_id, r.id, unit.id, self.now)
                .await
                .map_err(storage)?;
            return Ok(());
        }
        // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-5
        let previous = plan_revision_repo::for_plan(tx, &scope, self.tenant_id, r.plan_id)
            .await
            .map_err(storage)?
            .into_iter()
            .find(|x| x.id != r.id && x.state == RevisionState::Published.as_str());
        let contended = |e: RepoError| match e {
            RepoError::Conflict { .. } => ApprovalError::Contended,
            other => storage(other),
        };
        // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-3
        // @cpt-begin:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-5
        if let Some(previous) = &previous {
            plan_revision_repo::supersede(
                tx,
                &scope,
                self.tenant_id,
                previous.id,
                previous.version,
                self.now,
            )
            .await
            .map_err(contended)?;
        }
        plan_revision_repo::publish(tx, &scope, self.tenant_id, r.id, unit.id, self.now)
            .await
            .map_err(storage)?;
        let p = plan_repo::find(tx, &scope, self.tenant_id, r.plan_id)
            .await
            .map_err(storage)?
            .ok_or_else(|| ApprovalError::Store(format!("revision {} has no plan", r.id)))?;
        plan_repo::set_published(
            tx,
            &scope,
            self.tenant_id,
            p.id,
            p.version,
            r.rev_no,
            self.now,
        )
        .await
        .map_err(contended)?;
        // @cpt-end:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-5
        // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-apply:p1:inst-plans-revision-apply-3
        {
            let mut review = self.review.lock().unwrap_or_else(PoisonError::into_inner);
            review.superseded = previous.map(|x| x.id);
            review.published = true;
        }
        Ok(())
    }
    /// Reject and withdraw return the revision to an editable draft; an approval's publish
    /// already turned the lock into `approved_by_unit_id`.
    async fn unlock(
        &self,
        tx: &DbTx<'a>,
        unit: &Unit,
        _items: &[ItemRef],
        approved: bool,
    ) -> Result<(), ApprovalError> {
        if approved {
            return Ok(());
        }
        plan_revision_repo::unlock(tx, &self.scope(), self.tenant_id, self.revision_id, unit.id)
            .await
            .map_err(|e| match e {
                RepoError::Conflict { .. } => {
                    ApprovalError::Store("revision lock is not owned by this unit".into())
                }
                other => storage(other),
            })
    }
}

#[cfg(test)]
#[path = "plan_revisions_tests.rs"]
mod plan_revisions_tests;
