//! Durable acceptance: detached observations followed by one serializable commit.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-acceptance:p1
use super::{CommercialTermsService, errors, wire};
use crate::{
    api::{
        pricing_read::{PricingReadProvider, project_resolution},
        rest::authoring::support::{self, DoorError},
    },
    authz::{actions, resource_types},
    domain::{
        book,
        commercial_terms::{
            SaleObservation, quota_quantity, quota_refusal, validate_commercial_terms,
            validate_new_sale_observation,
        },
        plan_quota::{RuleItem, names_definition},
    },
    infra::{
        meter_semantics, plan_quota, plan_revisions,
        pricing_reads::{self, ReadSnapshot},
        reference_registry,
        storage::{
            RepoError,
            entity::{acceptance, commercial_command},
            repo::{
                acceptance_repo, audit_repo,
                commercial_command_repo::{self, CommandScope},
                price_repo,
            },
        },
    },
};
use bss_pricing_sdk::{
    acceptance::{
        AcceptanceReceipt, AcceptedQuotaRule, AcceptedQuotaSnapshot, CommandMeta,
        CommercialReason as R, NewSaleQuery,
    },
    digest::{
        QuotaSnapshotMismatch, quota_snapshot_digest, request_digest, select_quota_rules,
        selected_bindings_digest, selected_quota_digest, terms_digest, uncovered_quotas,
        verify_quota_snapshot,
    },
    read::{AcceptedBinding, ResolvedBindings, ResolvedQuotaRule},
};
use bss_products_sdk::quota::{catalog::QuotaAvailabilityState, validation::QuotaSetValidation};
use std::{collections::BTreeMap, num::NonZeroU64, sync::Arc};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::DbTx;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Everything needed to find or attach an authenticated command, without an in-flight claim.
#[derive(Clone)]
struct Request {
    scope: AccessScope,
    plan_scope: AccessScope,
    key: CommandScope,
    digest: bss_pricing_sdk::Digest,
    query: NewSaleQuery,
    /// One id for this check: catch-up events and the acceptance audit share it.
    correlation: Uuid,
}
impl CommercialTermsService {
    pub(crate) async fn check(
        &self,
        ctx: &SecurityContext,
        mut query: NewSaleQuery,
        meta: CommandMeta,
    ) -> Result<AcceptanceReceipt, CanonicalError> {
        let tenant = query.tenant_axes.seller_tenant_id;
        let scope = self.scope(ctx, tenant, actions::CREATE, None).await?;
        let reader = PricingReadProvider::new(self.state.clone(), self.enforcer.clone());
        let plan_scope = reader
            .scope(ctx, tenant, &resource_types::PLAN, query.plan_revision_id)
            .await?;
        crate::api::rest::preconditions::validate_idempotency_key(&meta.idempotency_key)?;
        query.start_at = query.start_at.to_offset(time::UtcOffset::UTC);
        query.billing_terms.anchor_at = query
            .billing_terms
            .anchor_at
            .to_offset(time::UtcOffset::UTC);
        let request = Arc::new(Request {
            scope,
            plan_scope,
            key: CommandScope {
                tenant_id: tenant,
                caller_tenant_id: ctx.subject_tenant_id(),
                caller_id: ctx.subject_id(),
                operation: "check".into(),
                idempotency_key: meta.idempotency_key,
            },
            digest: request_digest(&query),
            query,
            correlation: Uuid::now_v7(),
        });
        let db = self.state.db.db();
        support::retry_unit_capture(&db, || async {
            // Replays precede all live dependencies and never resample a receipt deadline.
            let r = request.clone();
            if let Some(receipt) = support::unit_transaction_observed_with_events(
                &db,
                &self.state.outbox,
                move |tx, _| {
                    let r = r.clone();
                    Box::pin(async move { replay(tx, &r).await })
                },
            )
            .await?
            {
                return Ok(receipt);
            }
            let r = request.clone();
            let now = self.clock.now();
            let captured_on = now.date();
            let mut snapshot = support::unit_transaction_observed_with_events(
                &db,
                &self.state.outbox,
                move |tx, outbox| {
                    let r = r.clone();
                    Box::pin(async move {
                        // Authorize/find before catch-up's tenant-scoped related-row operations.
                        crate::infra::storage::repo::plan_revision_repo::find(
                            tx,
                            &r.plan_scope,
                            tenant,
                            r.query.plan_revision_id,
                        )
                        .await?
                        .filter(|v| v.plan_id == r.query.plan_id)
                        .ok_or_else(|| CanonicalError::from(R::NotSellable))?;
                        plan_revisions::catch_up(
                            tx,
                            &outbox,
                            tenant,
                            r.query.plan_id,
                            now,
                            r.correlation,
                        )
                        .await?;
                        pricing_reads::read_stored_at(
                            tx,
                            &r.plan_scope,
                            tenant,
                            r.query.plan_revision_id,
                            None,
                            r.query.start_at.date(),
                            now.date(),
                        )
                        .await
                    })
                },
            )
            .await?;
            pricing_reads::finish(
                &self.state,
                &mut snapshot,
                ctx,
                tenant,
                request.query.start_at.date(),
                None,
                &[],
            )
            .await?;
            let resolved = project_resolution(&snapshot)?;
            let bindings = selected(&request.query, &resolved)?;
            for binding in &bindings {
                let price_scope = reader
                    .scope(ctx, tenant, &resource_types::PRICE, binding.price.price_id)
                    .await?;
                price_repo::find(&db.conn()?, &price_scope, tenant, binding.price.price_id)
                    .await?
                    .ok_or_else(|| CanonicalError::from(R::PermissionDenied))?;
            }
            let evidence = self
                .observe(ctx, &request.query, &snapshot, &bindings, captured_on)
                .await?;
            if selected_bindings_digest(&resolved, &request.query.selections)?
                != request.query.resolved_bindings_digest
            {
                return Err(CanonicalError::from(R::ResolutionChanged).into());
            }
            // @cpt-begin:cpt-cf-bss-pricing-flow-plan-quota-rules:p1:inst-quota-accept
            let quota = self
                .quota(ctx, &request.query, &resolved, &bindings)
                .await?;
            // @cpt-end:cpt-cf-bss-pricing-flow-plan-quota-rules:p1:inst-quota-accept
            let capture = Arc::new(Capture {
                snapshot,
                bindings,
                captured_on,
                evidence,
                quota,
            });
            let r = request.clone();
            let clock = self.clock.clone();
            let policy = self.policy.clone();
            support::unit_transaction_observed_with_events(&db, &self.state.outbox, move |tx, _| {
                let r = r.clone();
                let capture = capture.clone();
                let clock = clock.clone();
                let policy = policy.clone();
                Box::pin(async move { commit(tx, &r, &capture, clock.as_ref(), &policy).await })
            })
            .await
        })
        .await
        .map_err(failure)
    }
    /// The quota half of an acceptance (D-547, D-548), before the commit transaction and outside
    /// it. A revision without rules takes no expected quota digest and freezes nothing (`None`);
    /// a revision with rules needs the preview's digest. In order: a missing or unwanted expected
    /// digest is `RESOLUTION_CHANGED`, so a caller that does not know quotas cannot buy a plan with
    /// rules; a metric of a selected item that the selection leaves without a rule is
    /// `QUOTA_RULE_MISSING`, with no catalog call; an expected digest other than the selected
    /// rules' is `RESOLUTION_CHANGED`. A sale that selects rules then needs a whole quantity from
    /// 1 (`QUANTITY_INVALID`), and Products validates the selected rules as the caller, for
    /// acceptance at that quantity, each with its binding's SKU and SKU version
    /// ([`quota_refusal`] reads its checks); an outage is 503. The snapshot freezes each selected
    /// rule with Products' final limit and the availability revision its definition showed. A
    /// sale that selects no rule (only items without rules) asks Products nothing and freezes the
    /// empty selection, whatever its quantity.
    /// # Errors
    /// The refusals above; Products' refusal as Products gave it; 503 when the catalog is down; an
    /// internal error when Products' verdict does not judge each selected rule once, in request
    /// order, against its own enabled definition with a limit, when a selected rule leaves the
    /// Products request, or when the frozen snapshot fails its own check (a final limit that is
    /// not a canonical integer is logged as Products' verdict, any other failure as Pricing's
    /// own snapshot).
    // @cpt-algo:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1
    async fn quota(
        &self,
        ctx: &SecurityContext,
        q: &NewSaleQuery,
        resolved: &ResolvedBindings,
        bindings: &[AcceptedBinding],
    ) -> Result<Option<AcceptedQuotaSnapshot>, DoorError> {
        // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-expected
        let (terms, expected) = match (&resolved.quota_terms, &q.expected_quota_digest) {
            (None, None) => return Ok(None),
            (Some(terms), Some(expected)) => (terms, expected),
            (Some(_), None) | (None, Some(_)) => {
                return Err(CanonicalError::from(R::ResolutionChanged).into());
            }
        };
        // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-expected
        // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-select
        if !uncovered_quotas(terms, &q.selections).is_empty() {
            return Err(CanonicalError::from(R::QuotaRuleMissing).into());
        }
        let selected = select_quota_rules(terms, &q.selections);
        let selected_digest = selected_quota_digest(terms, &q.selections);
        // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-select
        // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-expected
        if selected_digest != *expected {
            return Err(CanonicalError::from(R::ResolutionChanged).into());
        }
        // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-expected
        // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-freeze
        let rules = if selected.is_empty() {
            Vec::new()
        } else {
            let quantity = quota_quantity(q.quantity).map_err(CanonicalError::from)?;
            let request = acceptance_request(&selected, bindings, quantity)?;
            let verdict = plan_quota::validate(&self.state.hub, ctx, request).await?;
            if let Some(reason) = quota_refusal(&verdict.checks) {
                return Err(CanonicalError::from(reason).into());
            }
            accepted_rules(&selected, &verdict)?
        };
        let snapshot = AcceptedQuotaSnapshot {
            selected_quota_digest: selected_digest,
            quota_snapshot_digest: quota_snapshot_digest(&selected_digest, &rules, q),
            original_quantity: q.quantity,
            rules,
        };
        // The receipt's stored-terms check must accept what is issued: a limit Products did not
        // answer as a canonical integer would turn the receipt into a corrupt row later. That
        // limit is Products' verdict breaking its contract; any other mismatch is Pricing's own.
        if let Err(mismatch) = verify_quota_snapshot(q, Some(&snapshot)) {
            let cause = match mismatch {
                QuotaSnapshotMismatch::Limit => Broken::Verdict,
                QuotaSnapshotMismatch::Pairing
                | QuotaSnapshotMismatch::Selection
                | QuotaSnapshotMismatch::SelectedDigest
                | QuotaSnapshotMismatch::Quantity
                | QuotaSnapshotMismatch::SnapshotDigest => Broken::Snapshot,
            };
            return Err(contract_break(cause, mismatch).into());
        }
        // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-freeze
        Ok(Some(snapshot))
    }
    async fn observe(
        &self,
        ctx: &SecurityContext,
        q: &NewSaleQuery,
        s: &ReadSnapshot,
        bindings: &[AcceptedBinding],
        captured_on: time::Date,
    ) -> Result<serde_json::Value, DoorError> {
        let mut evidence = Vec::new();
        let registry = reference_registry::resolve(&self.state.hub)
            .map_err(|e| support::registry_unavailable(&e))?;
        let tenant = q.tenant_axes.seller_tenant_id;
        let ids: Vec<Uuid> = bindings.iter().map(|b| b.sku_id).collect();
        let found = registry
            .skus_for_write(ctx, tenant, &ids)
            .await
            .map_err(errors::products)?;
        let by_id: BTreeMap<Uuid, bss_products_sdk::models::Sku> =
            found.into_iter().map(|sku| (sku.id, sku)).collect();
        let mut observations = Vec::with_capacity(bindings.len());
        // D-524: the book sells on the acceptance day and on the start day, as a price must.
        let book_valid = book_sells_on(s, [captured_on, q.start_at.date()]);
        for b in bindings {
            let sku = match by_id.get(&b.sku_id).cloned() {
                Some(sku) => sku,
                None => registry
                    .sku_for_write(ctx, tenant, b.sku_id)
                    .await
                    .map_err(errors::products)?,
            };
            if let Some(policy) = &b.usage_rating_policy {
                let meter =
                    meter_semantics::resolve(&self.state.hub, ctx, &(&policy.content).into(), &sku)
                        .await?;
                evidence.push(serde_json::json!({"entry_id": b.price_book_entry_id, "sku_revision": sku.revision.to_string(), "meter": meter}));
            }
            observations.push(SaleObservation {
                revision_is_current: s.generation.plan.published_rev == Some(s.revision.rev_no)
                    && s.revision.state == "published",
                revision_available: s
                    .revision
                    .available_from
                    .is_none_or(|d| d <= q.start_at.date()),
                sku_active: sku.lifecycle == bss_products_sdk::models::Lifecycle::Published
                    && !sku.retire_pending,
                sku_sellable: sku.sellable,
                covered: true,
                book_valid,
            });
        }
        for observation in &observations {
            validate_new_sale_observation(observation).map_err(CanonicalError::from)?;
        }
        validate_commercial_terms(q, bindings).map_err(CanonicalError::from)?;
        Ok(serde_json::Value::Array(evidence))
    }
}
/// D-524: whether the revision's book, as the snapshot read it, sells on each of the two days.
fn book_sells_on(s: &ReadSnapshot, days: [time::Date; 2]) -> bool {
    let stored = s.generation.book();
    book::sells_on_each(stored.valid_from, stored.valid_until, &days)
}
/// What breaks an acceptance's quota freeze.
#[derive(Debug, Clone, Copy)]
enum Broken {
    /// Products' verdict does not judge the selected rules as its contract says, or answers a
    /// final limit that is not a canonical integer.
    Verdict,
    /// Pricing's own snapshot is inconsistent: a selected rule lost from the Products request, or
    /// a frozen snapshot that fails another part of its own check before the receipt is issued.
    Snapshot,
}

/// An acceptance's quota freeze is broken by `cause`: an internal error, never a default. The
/// log names the cause and the fixed reason `why` (a static text, or the snapshot check that
/// failed); the caller gets a generic 500 that carries no caller or catalog data.
fn contract_break(cause: Broken, why: impl std::fmt::Display) -> CanonicalError {
    match cause {
        Broken::Verdict => {
            tracing::error!(
                why = %why,
                "pricing: Products' quota verdict breaks the acceptance contract"
            );
            CanonicalError::internal("the quota catalog's verdict breaks its contract").create()
        }
        Broken::Snapshot => {
            tracing::error!(
                why = %why,
                "pricing: the acceptance's quota snapshot is inconsistent"
            );
            CanonicalError::internal("the acceptance's quota snapshot is inconsistent").create()
        }
    }
}
/// The Products request for a sale's selected rules (D-548): each rule with the SKU and SKU
/// version of its item's binding, for acceptance at `quantity`, a new set with no prior limit.
/// Every selected rule's item is a selected item, so each has a binding.
fn acceptance_request(
    selected: &[&ResolvedQuotaRule],
    bindings: &[AcceptedBinding],
    quantity: String,
) -> Result<bss_products_sdk::quota::validation::QuotaSetRequest, CanonicalError> {
    let items: Vec<RuleItem> = bindings
        .iter()
        .map(|b| RuleItem {
            id: b.item_id,
            sku_id: b.sku_id,
            dimension_key: None,
        })
        .collect();
    let versions: BTreeMap<Uuid, i64> =
        bindings.iter().map(|b| (b.sku_id, b.sku_version)).collect();
    let rules: Vec<_> = selected.iter().map(|r| r.rule.clone()).collect();
    let request = plan_quota::set_request(
        &rules,
        &items,
        &versions,
        plan_quota::Purpose::Acceptance { quantity },
    );
    if request.rules.len() != rules.len() {
        return Err(contract_break(
            Broken::Snapshot,
            "a selected quota rule has no binding with a SKU version",
        ));
    }
    Ok(request)
}
/// Each selected rule with Products' final limit at the accepted quantity and the availability
/// revision of the definition it names (D-548), in `rule_id` order. Products answered no check,
/// so the verdict must have one entry per selected rule, in request order (the order of
/// `selected`), each for its rule, resolving the rule's own definition version
/// (`names_definition`), enabled (Products answers a check for a version that is not), with a
/// limit; anything else breaks Products' contract.
fn accepted_rules(
    selected: &[&ResolvedQuotaRule],
    verdict: &QuotaSetValidation,
) -> Result<Vec<AcceptedQuotaRule>, CanonicalError> {
    if verdict.rules.len() != selected.len() {
        return Err(contract_break(
            Broken::Verdict,
            "the verdict does not have one entry for each selected quota rule",
        ));
    }
    selected
        .iter()
        .zip(&verdict.rules)
        .map(|(rule, judged)| {
            if judged.rule_id != rule.rule.rule_id {
                return Err(contract_break(
                    Broken::Verdict,
                    "a verdict entry is not for the selected quota rule at its place in the \
                     request",
                ));
            }
            let definition = judged
                .definition
                .as_ref()
                .filter(|definition| names_definition(&rule.rule.definition_ref, definition))
                .ok_or_else(|| {
                    contract_break(
                        Broken::Verdict,
                        "a selected quota rule is not judged against its definition",
                    )
                })?;
            if definition.availability.state != QuotaAvailabilityState::Enabled {
                return Err(contract_break(
                    Broken::Verdict,
                    "a selected quota definition is not enabled",
                ));
            }
            let limit = judged.limit.clone().ok_or_else(|| {
                contract_break(Broken::Verdict, "a selected quota rule has no final limit")
            })?;
            let availability_revision = NonZeroU64::new(definition.availability.revision.get())
                .ok_or_else(|| contract_break(Broken::Verdict, "an availability revision is 0"))?;
            Ok(AcceptedQuotaRule {
                rule: (*rule).clone(),
                limit,
                availability_revision,
            })
        })
        .collect()
}
/// A complete detached observation, consumed only after its local rows still match.
struct Capture {
    snapshot: ReadSnapshot,
    bindings: Vec<AcceptedBinding>,
    captured_on: time::Date,
    evidence: serde_json::Value,
    /// The frozen quota evidence (D-548); `None` for a revision without rules.
    quota: Option<AcceptedQuotaSnapshot>,
}
/// Recheck and atomically persist the winning receipt, command and audit.
async fn commit(
    tx: &DbTx<'_>,
    r: &Request,
    capture: &Capture,
    clock: &dyn crate::infra::clock::Clock,
    policy: &crate::config::SellerHoldPolicy,
) -> Result<AcceptanceReceipt, DoorError> {
    let snapshot = &capture.snapshot;
    let bindings = &capture.bindings;
    let captured_on = capture.captured_on;
    let tenant = r.key.tenant_id;
    if let Some(receipt) = replay(tx, r).await? {
        return Ok(receipt);
    }
    let current = pricing_reads::read_stored_at(
        tx,
        &r.plan_scope,
        tenant,
        r.query.plan_revision_id,
        None,
        r.query.start_at.date(),
        captured_on,
    )
    .await?;
    if current.generation != snapshot.generation
        || current.rows != snapshot.rows
        || current.policies != snapshot.policies
    {
        return Err(DoorError::SelectionMoved);
    }
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-freeze
    if current.quota != snapshot.quota {
        return Err(DoorError::SelectionMoved);
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-freeze
    let now = clock.now();
    // D-524 on the commit's clock, as a selected price's window is judged below.
    if !book_sells_on(snapshot, [now.date(), r.query.start_at.date()]) {
        return Err(CanonicalError::from(R::NotSellable).into());
    }
    if snapshot
        .generation
        .revisions
        .iter()
        .any(|v| v.state == "scheduled" && v.available_from.is_some_and(|d| d <= now.date()))
    {
        return Err(DoorError::SelectionMoved);
    }
    if r.query.hold_policy_version != policy.version.get() {
        return Err(CanonicalError::from(R::UnsupportedTerms).into());
    }
    for b in bindings {
        let p = snapshot
            .rows
            .get(&b.price.price_id)
            .ok_or_else(|| CanonicalError::from(R::ResolutionChanged))?;
        if p.temporary_until.is_some() {
            return Err(CanonicalError::from(R::UnsupportedTerms).into());
        }
        if p.closed_explicitly
            || p.state != "approved"
            || [now.date(), r.query.start_at.date()]
                .iter()
                .any(|d| *d < p.effective_from || p.effective_to.is_some_and(|end| *d >= end))
        {
            return Err(CanonicalError::from(R::PriceClosed).into());
        }
    }
    let hold_until = now + time::Duration::seconds(i64::from(policy.duration_seconds.get()));
    if r.query.start_at >= hold_until {
        return Err(CanonicalError::from(R::ActivationOutsideAcceptedWindow).into());
    }
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-freeze
    let receipt = AcceptanceReceipt {
        acceptance_id: Uuid::now_v7(),
        request_digest: r.digest,
        terms_digest: terms_digest(&r.query, bindings, capture.quota.as_ref()),
        query: r.query.clone(),
        accepted_at: now,
        hold_until,
        bindings: bindings.clone(),
        quota: capture.quota.clone(),
    };
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-accept:p1:inst-quota-freeze
    let row = acceptance_repo::from_receipt(&receipt, r.key.caller_id)?;
    let winner = acceptance_repo::insert_or_get(tx, &r.scope, row).await?;
    attach(tx, r, &winner).await?;
    if winner.id == receipt.acceptance_id {
        audit_repo::write_eventless_act_audit(
            tx,
            &r.scope,
            audit_repo::AuditCommon {
                audit_id: Uuid::now_v7(),
                tenant_id: tenant,
                actor_ref: r.key.caller_id,
                action: "accept".into(),
                subject_kind: "acceptance".into(),
                reason: Some(capture.evidence.to_string()),
                correlation_id: Some(r.correlation.to_string()),
                written_at: now,
            },
            winner.id,
            None,
        )
        .await?;
    }
    Ok(wire::decode_acceptance(&winner.receipt_json)?)
}
fn selected(
    q: &NewSaleQuery,
    resolved: &bss_pricing_sdk::read::ResolvedBindings,
) -> Result<Vec<AcceptedBinding>, CanonicalError> {
    let items = resolved
        .cells
        .iter()
        .map(|c| c.selection.item_id)
        .collect::<std::collections::BTreeSet<_>>();
    if resolved.plan_id != q.plan_id
        || items.len() != q.selections.len()
        || q.selections
            .iter()
            .map(|s| s.item_id)
            .collect::<std::collections::BTreeSet<_>>()
            != items
    {
        return Err(R::IncompleteSelection.into());
    }
    q.selections
        .iter()
        .map(|selection| {
            resolved
                .cells
                .iter()
                .find(|c| c.selection == *selection)
                .and_then(|c| c.binding.clone())
                .ok_or_else(|| R::ResolutionChanged.into())
        })
        .collect()
}
async fn replay(tx: &impl DBRunner, r: &Request) -> Result<Option<AcceptanceReceipt>, DoorError> {
    if let Some(c) = commercial_command_repo::find_scope(tx, &r.scope, &r.key).await? {
        if c.request_digest != crate::infra::usage_policy_wire::digest_text(r.digest) {
            return Err(CanonicalError::from(R::IdempotencyConflict).into());
        }
        let row = acceptance_repo::find(tx, &r.scope, r.key.tenant_id, c.receipt_id)
            .await?
            .ok_or_else(|| RepoError::CorruptRow("command receipt absent".into()))?;
        return Ok(Some(wire::decode_acceptance(&row.receipt_json)?));
    }
    if let Some(row) = acceptance_repo::find_business(
        tx,
        &r.scope,
        r.key.tenant_id,
        r.query.order_id,
        &r.query.order_version.to_string(),
        r.query.line_id,
    )
    .await?
    {
        if row.request_digest != crate::infra::usage_policy_wire::digest_text(r.digest) {
            return Err(CanonicalError::from(R::AcceptanceMismatch).into());
        }
        attach(tx, r, &row).await?;
        return Ok(Some(wire::decode_acceptance(&row.receipt_json)?));
    }
    Ok(None)
}
async fn attach(tx: &impl DBRunner, r: &Request, row: &acceptance::Model) -> Result<(), DoorError> {
    commercial_command_repo::insert_or_get(
        tx,
        &r.scope,
        commercial_command::Model {
            id: Uuid::now_v7(),
            tenant_id: r.key.tenant_id,
            caller_tenant_id: r.key.caller_tenant_id,
            caller_id: r.key.caller_id,
            operation: r.key.operation.clone(),
            idempotency_key: r.key.idempotency_key.clone(),
            request_digest: row.request_digest.clone(),
            receipt_kind: "acceptance".into(),
            receipt_id: row.id,
            acceptance_id: Some(row.id),
            hold_id: None,
        },
    )
    .await?;
    Ok(())
}
fn contended(error: &CanonicalError) -> bool {
    matches!(
        error,
        CanonicalError::Aborted { ctx, .. } if ctx.reason == support::UNIT_CONTENDED
    )
}
pub(super) fn failure(e: DoorError) -> CanonicalError {
    match e {
        DoorError::Repo(RepoError::Conflict {
            code: "ACCEPTANCE_MISMATCH",
        }) => R::AcceptanceMismatch.into(),
        DoorError::Repo(RepoError::Conflict {
            code: "IDEMPOTENCY_CONFLICT",
        }) => R::IdempotencyConflict.into(),
        DoorError::Repo(e) => errors::storage(e),
        DoorError::SelectionMoved => R::ResolutionChanged.into(),
        DoorError::Api(error) if contended(&error) => R::ResolutionChanged.into(),
        other => other.into(),
    }
}

#[cfg(test)]
mod failure_classify {
    use super::*;
    use crate::api::rest::authoring::support::{self, DoorError};

    fn reason(error: &CanonicalError) -> Option<String> {
        match error {
            CanonicalError::Aborted { ctx, .. } => Some(ctx.reason.clone()),
            _ => None,
        }
    }

    #[test]
    fn selection_moved_is_resolution_changed() {
        let error = failure(DoorError::SelectionMoved);
        assert_eq!(reason(&error).as_deref(), Some("ResolutionChanged"));
    }

    #[test]
    fn exhausted_unit_capture_is_resolution_changed() {
        let error = failure(DoorError::Api(support::conflict(support::UNIT_CONTENDED)));
        assert_eq!(reason(&error).as_deref(), Some("ResolutionChanged"));
    }

    #[test]
    fn a_detail_that_mentions_the_code_is_not_reclassified() {
        let misleading = support::conflict_because("PRICE_CLOSED", "note mentions UNIT_CONTENDED");
        let error = failure(DoorError::Api(misleading));
        assert_eq!(reason(&error).as_deref(), Some("PRICE_CLOSED"));
    }
}
