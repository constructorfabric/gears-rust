//! Durable acceptance: detached observations followed by one serializable commit.
use super::{CommercialTermsService, errors, wire};
use crate::{
    api::{
        pricing_read::{PricingReadProvider, project_resolution},
        rest::authoring::support::{self, DoorError},
    },
    authz::{actions, resource_types},
    domain::commercial_terms::{
        SaleObservation, validate_commercial_terms, validate_new_sale_observation,
    },
    infra::{
        meter_semantics, plan_revisions,
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
    acceptance::{AcceptanceReceipt, CommandMeta, CommercialReason as R, NewSaleQuery},
    digest::{request_digest, selected_bindings_digest, terms_digest},
    read::AcceptedBinding,
};
use std::{collections::BTreeMap, sync::Arc};
use toolkit_canonical_errors::CanonicalError;
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
                .observe(ctx, &request.query, &snapshot, &bindings)
                .await?;
            if selected_bindings_digest(&resolved, &request.query.selections)?
                != request.query.resolved_bindings_digest
            {
                return Err(CanonicalError::from(R::ResolutionChanged).into());
            }
            let capture = Arc::new(Capture {
                snapshot,
                bindings,
                captured_on,
                evidence,
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
    async fn observe(
        &self,
        ctx: &SecurityContext,
        q: &NewSaleQuery,
        s: &ReadSnapshot,
        bindings: &[AcceptedBinding],
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
            });
        }
        for observation in &observations {
            validate_new_sale_observation(observation).map_err(CanonicalError::from)?;
        }
        validate_commercial_terms(q, bindings).map_err(CanonicalError::from)?;
        Ok(serde_json::Value::Array(evidence))
    }
}
/// A complete detached observation, consumed only after its local rows still match.
struct Capture {
    snapshot: ReadSnapshot,
    bindings: Vec<AcceptedBinding>,
    captured_on: time::Date,
    evidence: serde_json::Value,
}
/// Recheck and atomically persist the winning receipt, command and audit.
async fn commit(
    tx: &impl DBRunner,
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
    let now = clock.now();
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
    let receipt = AcceptanceReceipt {
        acceptance_id: Uuid::now_v7(),
        request_digest: r.digest,
        terms_digest: terms_digest(&r.query, bindings),
        query: r.query.clone(),
        accepted_at: now,
        hold_until,
        bindings: bindings.clone(),
    };
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
