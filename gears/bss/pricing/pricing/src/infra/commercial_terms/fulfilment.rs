//! Fresh original-binding eligibility and atomic, replayable first holds.
use super::{CommercialTermsService, check::failure, errors, wire};
use crate::{
    api::{
        pricing_read::PricingReadProvider,
        rest::authoring::support::{self, DoorError},
    },
    authz::{actions, resource_types},
    domain::commercial_terms::{
        validate_activation_window, validate_commercial_terms, validate_hold_time,
    },
    infra::{
        reference_registry,
        storage::{
            RepoError,
            entity::{commercial_command, price},
            repo::{
                acceptance_repo,
                commercial_command_repo::{self, CommandScope},
                hold_repo, price_repo,
            },
        },
    },
};
use bss_pricing_sdk::{
    acceptance::{
        AcceptanceReceipt, CommandMeta, CommercialReason as R, FulfilmentEligibility,
        FulfilmentQuery, HeldBindings,
    },
    digest::fulfilment_digest,
};
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;

struct Request {
    scope: AccessScope,
    related_scope: AccessScope,
    digest: bss_pricing_sdk::Digest,
    query: FulfilmentQuery,
    key: Option<CommandScope>,
}
struct Capture {
    receipt: AcceptanceReceipt,
    prices: Vec<(AccessScope, price::Model)>,
}
impl CommercialTermsService {
    pub(crate) async fn hold(
        &self,
        ctx: &SecurityContext,
        query: FulfilmentQuery,
        meta: CommandMeta,
    ) -> Result<HeldBindings, CanonicalError> {
        let (r, db, tenant) = self.opened(ctx, query, Some(meta)).await?;
        let key = r.key.clone().ok_or_else(|| {
            CanonicalError::internal("a hold opened without its command").create()
        })?;
        support::retry_unit_capture(&db, || async {
            let conn = self.state.db.conn().map_err(RepoError::from)?;
            if let Some(held) = replay(&conn, &r).await? {
                return Ok(held);
            }
            let capture = Arc::new(self.capture(ctx, &r).await?);
            let r = r.clone();
            let clock = self.clock.clone();
            let key = key.clone();
            support::unit_transaction_observed_with_events(&db, &self.state.outbox, move |tx, _| {
                let r = r.clone();
                let capture = capture.clone();
                let clock = clock.clone();
                let key = key.clone();
                Box::pin(async move {
                    if let Some(held) = replay(tx, &r).await? {
                        return Ok(held);
                    }
                    prices_unchanged(tx, tenant, &capture).await?;
                    let now = clock.now();
                    let _eligibility = eligible(&r.query, &capture, now)?;
                    same_activation(tx, &r, tenant, &capture).await?;
                    let candidate = HeldBindings {
                        hold_id: Uuid::now_v7(),
                        acceptance_id: capture.receipt.acceptance_id,
                        terms_digest: capture.receipt.terms_digest,
                        activation_at: r.query.activation_at,
                        bindings: capture.receipt.bindings.clone(),
                    };
                    let row = hold_repo::from_receipt(tenant, &candidate, key.caller_id, now)?;
                    let winner = hold_repo::insert_or_get(tx, &r.related_scope, row).await?;
                    commercial_command_repo::insert_or_get(
                        tx,
                        &r.related_scope,
                        commercial_command::Model {
                            id: Uuid::now_v7(),
                            tenant_id: tenant,
                            caller_tenant_id: key.caller_tenant_id,
                            caller_id: key.caller_id,
                            operation: key.operation.clone(),
                            idempotency_key: key.idempotency_key.clone(),
                            request_digest: crate::infra::usage_policy_wire::digest_text(r.digest),
                            receipt_kind: "hold".into(),
                            receipt_id: winner.id,
                            acceptance_id: None,
                            hold_id: Some(winner.id),
                        },
                    )
                    .await?;
                    Ok(wire::decode_hold(&winner.receipt_json)?)
                })
            })
            .await
        })
        .await
        .map_err(failure)
    }
    pub(crate) async fn check_fulfilment(
        &self,
        ctx: &SecurityContext,
        query: FulfilmentQuery,
    ) -> Result<FulfilmentEligibility, CanonicalError> {
        let (r, db, tenant) = self.opened(ctx, query, None).await?;
        support::retry_unit_capture(&db, || async {
            let conn = self.state.db.conn().map_err(RepoError::from)?;
            // The acceptance must still be visible. A read has no command to replay.
            replay(&conn, &r).await?;
            let capture = Arc::new(self.capture(ctx, &r).await?);
            let r = r.clone();
            let clock = self.clock.clone();
            support::unit_transaction_observed_with_events(&db, &self.state.outbox, move |tx, _| {
                let r = r.clone();
                let capture = capture.clone();
                let clock = clock.clone();
                Box::pin(async move {
                    replay(tx, &r).await?;
                    prices_unchanged(tx, tenant, &capture).await?;
                    let now = clock.now();
                    let eligibility = eligible(&r.query, &capture, now)?;
                    same_activation(tx, &r, tenant, &capture).await?;
                    Ok(eligibility)
                })
            })
            .await
        })
        .await
        .map_err(failure)
    }
    async fn opened(
        &self,
        ctx: &SecurityContext,
        mut query: FulfilmentQuery,
        meta: Option<CommandMeta>,
    ) -> Result<(Arc<Request>, toolkit_db::Db, Uuid), CanonicalError> {
        let tenant = query.tenant_axes.seller_tenant_id;
        let scope = self
            .scope(
                ctx,
                tenant,
                if meta.is_some() {
                    actions::HOLD
                } else {
                    actions::READ
                },
                Some(query.acceptance.acceptance_id),
            )
            .await?;
        if let Some(meta) = &meta {
            crate::api::rest::preconditions::validate_idempotency_key(&meta.idempotency_key)?;
        }
        query.activation_at = query.activation_at.to_offset(time::UtcOffset::UTC);
        let related_scope = scope.tenant_only();
        let r = Arc::new(Request {
            scope,
            related_scope,
            digest: fulfilment_digest(&query),
            query,
            key: meta.map(|m| CommandScope {
                tenant_id: tenant,
                caller_tenant_id: ctx.subject_tenant_id(),
                caller_id: ctx.subject_id(),
                operation: "hold".into(),
                idempotency_key: m.idempotency_key,
            }),
        });
        Ok((r, self.state.db.db(), tenant))
    }
    async fn capture(&self, ctx: &SecurityContext, r: &Request) -> Result<Capture, DoorError> {
        let tenant = r.query.tenant_axes.seller_tenant_id;
        let conn = self.state.db.conn().map_err(RepoError::from)?;
        let row = acceptance_repo::find(&conn, &r.scope, tenant, r.query.acceptance.acceptance_id)
            .await?
            .ok_or_else(|| CanonicalError::from(R::ReceiptNotFound))?;
        let receipt = wire::decode_acceptance(&row.receipt_json)?;
        if receipt.terms_digest != r.query.acceptance.terms_digest
            || receipt.query.tenant_axes != r.query.tenant_axes
        {
            return Err(CanonicalError::from(R::AcceptanceMismatch).into());
        }
        if receipt.query.market != r.query.current_market {
            return Err(CanonicalError::from(R::MarketChanged).into());
        }
        validate_hold_time(self.clock.now(), receipt.hold_until).map_err(CanonicalError::from)?;
        validate_activation_window(
            receipt.query.start_at,
            r.query.activation_at,
            receipt.hold_until,
        )
        .map_err(CanonicalError::from)?;
        // Check compatibility of the frozen inputs; never replace BillingTerms or descriptors.
        validate_commercial_terms(&receipt.query, &receipt.bindings)
            .map_err(CanonicalError::from)?;
        let reader = PricingReadProvider::new(self.state.clone(), self.enforcer.clone());
        let mut prices = Vec::with_capacity(receipt.bindings.len());
        for binding in &receipt.bindings {
            let scope = reader
                .scope(ctx, tenant, &resource_types::PRICE, binding.price.price_id)
                .await?;
            let row = price_repo::find(&conn, &scope, tenant, binding.price.price_id)
                .await?
                .ok_or_else(|| CanonicalError::from(R::PermissionDenied))?;
            prices.push((scope, row));
        }
        let registry = reference_registry::resolve(&self.state.hub)
            .map_err(|e| support::registry_unavailable(&e))?;
        let ids: Vec<Uuid> = receipt.bindings.iter().map(|b| b.sku_id).collect();
        let found = registry
            .skus_for_write(ctx, tenant, &ids)
            .await
            .map_err(errors::products)?;
        let by_id: std::collections::BTreeMap<_, _> =
            found.into_iter().map(|sku| (sku.id, sku)).collect();
        for binding in &receipt.bindings {
            let sku = match by_id.get(&binding.sku_id).cloned() {
                Some(sku) => sku,
                None => registry
                    .sku_for_write(ctx, tenant, binding.sku_id)
                    .await
                    .map_err(errors::products)?,
            };
            if sku.lifecycle == bss_products_sdk::models::Lifecycle::Retired {
                return Err(CanonicalError::from(R::SkuRetired).into());
            }
        }
        Ok(Capture { receipt, prices })
    }
}
async fn prices_unchanged(
    tx: &impl DBRunner,
    tenant: Uuid,
    capture: &Capture,
) -> Result<(), DoorError> {
    for (scope, observed) in &capture.prices {
        if price_repo::find(tx, scope, tenant, observed.id)
            .await?
            .as_ref()
            != Some(observed)
        {
            return Err(DoorError::SelectionMoved);
        }
    }
    Ok(())
}
async fn same_activation(
    tx: &impl DBRunner,
    r: &Request,
    tenant: Uuid,
    capture: &Capture,
) -> Result<(), DoorError> {
    if let Some(existing) =
        hold_repo::find_acceptance(tx, &r.related_scope, tenant, capture.receipt.acceptance_id)
            .await?
        && wire::decode_hold(&existing.receipt_json)?.activation_at != r.query.activation_at
    {
        return Err(CanonicalError::from(R::AcceptanceMismatch).into());
    }
    Ok(())
}
fn eligible(
    q: &FulfilmentQuery,
    capture: &Capture,
    now: OffsetDateTime,
) -> Result<FulfilmentEligibility, CanonicalError> {
    let a = &capture.receipt;
    validate_hold_time(now, a.hold_until)?;
    validate_activation_window(a.query.start_at, q.activation_at, a.hold_until)?;
    let mut valid_before = a.hold_until;
    for (_, p) in &capture.prices {
        // effective_to alone describes successor selection, never accepted-binding expiry.
        let end = p
            .temporary_until
            .or_else(|| p.effective_to.filter(|_| p.closed_explicitly));
        let from = p.effective_from.midnight().assume_utc();
        if p.state != "approved"
            || (p.closed_explicitly && end.is_none())
            || now < from
            || q.activation_at < from
        {
            return Err(R::PriceClosed.into());
        }
        if let Some(end) = end {
            let end = end.midnight().assume_utc();
            if now >= end || q.activation_at >= end {
                return Err(R::PriceClosed.into());
            }
            valid_before = valid_before.min(end);
        }
    }
    Ok(FulfilmentEligibility {
        acceptance_id: a.acceptance_id,
        checked_at: now,
        valid_before,
    })
}
async fn replay(tx: &impl DBRunner, r: &Request) -> Result<Option<HeldBindings>, DoorError> {
    // The PDP scope names the acceptance, not the generated hold/command primary keys.
    // Prove the exact parent is visible before using its derived tenant scope for children.
    acceptance_repo::find(
        tx,
        &r.scope,
        r.query.tenant_axes.seller_tenant_id,
        r.query.acceptance.acceptance_id,
    )
    .await?
    .ok_or_else(|| CanonicalError::from(R::ReceiptNotFound))?;
    let Some(key) = &r.key else {
        return Ok(None);
    };
    let Some(command) = commercial_command_repo::find_scope(tx, &r.related_scope, key).await?
    else {
        return Ok(None);
    };
    if command.request_digest != crate::infra::usage_policy_wire::digest_text(r.digest) {
        return Err(CanonicalError::from(R::IdempotencyConflict).into());
    }
    let row = hold_repo::find(tx, &r.related_scope, key.tenant_id, command.receipt_id)
        .await?
        .ok_or_else(|| RepoError::CorruptRow("hold command receipt absent".into()))?;
    if row.acceptance_id != r.query.acceptance.acceptance_id {
        return Err(RepoError::CorruptRow("hold command parent mismatch".into()).into());
    }
    Ok(Some(wire::decode_hold(&row.receipt_json)?))
}
