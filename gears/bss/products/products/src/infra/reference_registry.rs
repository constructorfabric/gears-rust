//! Trusted in-process transport. Ownership is selected only by Products wiring.
use crate::api::rest::{
    self, ApiState, TxError, closed_sets::ProductsReferenceState, governance as g,
    references as service,
};
use crate::authz::{actions, resource_types};
use crate::domain::{error::DomainError, references::RefKind};
use crate::infra::storage::{RepoError, RepoRefusal, repo};
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use bss_products_sdk::{
    PRICING_SYSTEM_ACTOR, ReferenceKind, ReferenceRegistryV1, ReferenceState, ReservationReceipt,
    Sku, SkuVersion,
};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Bound owner awaiting the provider's runtime dependencies.
pub struct ReferenceRegistryOwner(String);
impl ReferenceRegistryOwner {
    /// Attach the same dependencies as the REST reference handlers.
    #[must_use]
    pub fn with_runtime(
        self,
        state: Arc<ApiState>,
        enforcer: Arc<PolicyEnforcer>,
    ) -> LocalReferenceRegistry {
        LocalReferenceRegistry {
            owner: self.0,
            state,
            enforcer,
        }
    }
}
/// Same-binary trust boundary; this constructor is called by Products, not request input.
pub struct LocalReferenceRegistry {
    owner: String,
    state: Arc<ApiState>,
    enforcer: Arc<PolicyEnforcer>,
}
impl LocalReferenceRegistry {
    /// Bind a consumer owner before attaching the Products runtime.
    #[must_use]
    pub fn for_owner(owner: &str) -> ReferenceRegistryOwner {
        ReferenceRegistryOwner(owner.into())
    }
    /// The caller's scope, and whether it is the trusted system act (P-D-222).
    ///
    /// **The in-process trust.** This registry is reached only through the `ClientHub`
    /// (`PricingReferenceRegistry`); no REST door calls it, and every REST door asks the PDP for
    /// every caller, whatever subject type the caller's token asserts. The one trusted principal
    /// is pricing's system actor (`bss-pricing.system`, `PRICING_SYSTEM_ACTOR`) on the registry
    /// bound to the `pricing` owner, in the caller's own tenant: in-process code of the same
    /// binary is trusted, as it is with the database. Pricing's doors hand this registry their
    /// caller's context, so the REST edge of both gears refuses that actor, in either half, 403
    /// `SYSTEM_ACTOR_RESERVED` (`require_authenticated`, fix run W1c): only in-process code
    /// reaches this branch as it. A missing subject type is a human
    /// principal (the static-authn default, third-party OIDC tokens): it goes through the PDP
    /// like any other. Only the system branch interprets the subject type, and it checks it
    /// itself.
    async fn scope(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        action: &str,
    ) -> Result<(AccessScope, bool), CanonicalError> {
        if tenant != ctx.subject_tenant_id() || tenant.is_nil() || ctx.subject_id().is_nil() {
            return Err(service::forbidden(
                ctx.subject_id(),
                ctx.subject_tenant_id(),
                "the call names another tenant than the caller's",
            )
            .into());
        }
        if ctx.subject_type().is_some_and(|s| s.ends_with(".system")) {
            if self.owner != "pricing"
                || ctx.subject_type() != Some("bss-pricing.system")
                || ctx.subject_id() != PRICING_SYSTEM_ACTOR
            {
                return Err(service::forbidden(
                    ctx.subject_id(),
                    tenant,
                    "a system subject other than pricing's on its own registry",
                )
                .into());
            }
            return Ok((AccessScope::for_tenant(tenant), true));
        }
        Ok((
            g::scope(&self.enforcer, ctx, &resource_types::SKU, action).await?,
            false,
        ))
    }
    async fn change(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
        release: bool,
    ) -> Result<(), CanonicalError> {
        let (scope, system) = self.scope(ctx, tenant, actions::REFERENCE).await?;
        let ctx = ctx.clone();
        let owner = self.owner.clone();
        let ttl = self.state.fence_ttl_minutes;
        self.state
            .db
            .db()
            .transaction_with_retry(
                rest::category_tx_config(&self.state),
                rest::contention_db_err,
                move |tx| {
                    let (scope, ctx, owner) = (scope.clone(), ctx.clone(), owner.clone());
                    Box::pin(async move {
                        let acting = service::Acting { ctx: &ctx, system };
                        if release {
                            service::release_tx(tx, &scope, acting, &owner, id, ttl).await?;
                        } else {
                            service::confirm_tx(tx, &scope, acting, &owner, id, ttl).await?;
                        }
                        Ok(())
                    })
                },
            )
            .await
            .map_err(rest::tx_to_canonical)
    }
}
/// A stored reference state, read back through its closed set: a token outside it names the row
/// and the token in the logged 500 (RS-33).
fn state(token: &str, id: Uuid) -> Result<ReferenceState, CanonicalError> {
    ProductsReferenceState::stored(token, &format_args!("reference {id} state"))
        .map(Into::into)
        .map_err(|e| rest::repo_error_to_canonical(&e))
}
#[async_trait]
impl ReferenceRegistryV1 for LocalReferenceRegistry {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_id: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        let (scope, system) = self.scope(ctx, tenant, actions::REFERENCE).await?;
        let kind = match kind {
            ReferenceKind::PriceBookEntry => RefKind::PriceBookEntry,
            ReferenceKind::PlanItem => RefKind::PlanItem,
            ReferenceKind::SoldAs => RefKind::SoldAs,
        };
        // A unique loser retries once; a second loss is the 409 after the loop (RS-05).
        for _ in 0..2 {
            let (scope, ctx, owner, ttl) = (
                scope.clone(),
                ctx.clone(),
                self.owner.clone(),
                self.state.fence_ttl_minutes,
            );
            let result = self
                .state
                .db
                .db()
                .transaction_with_retry(
                    rest::category_tx_config(&self.state),
                    rest::contention_db_err,
                    move |tx| {
                        let (scope, ctx, owner) = (scope.clone(), ctx.clone(), owner.clone());
                        Box::pin(async move {
                            let acting = service::Acting { ctx: &ctx, system };
                            service::reserve_tx(
                                tx, &scope, acting, &owner, sku_id, kind, ref_id, ttl,
                            )
                            .await
                        })
                    },
                )
                .await;
            match result {
                Err(TxError::Repo(RepoError::Refused(RepoRefusal::ReferenceExists))) => {}
                other => {
                    let (row, _) = other.map_err(rest::tx_to_canonical)?;
                    return Ok(ReservationReceipt {
                        reservation_id: row.id,
                        state: state(&row.state, row.id)?,
                    });
                }
            }
        }
        Err(rest::tx_to_canonical(g::conflict(
            "REFERENCE_EXISTS",
            "logical reference changed; retry",
        )))
    }
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.change(ctx, tenant, id, false).await
    }
    async fn release(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.change(ctx, tenant, id, true).await
    }
    async fn states(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        let (scope, _) = self.scope(ctx, tenant, actions::REFERENCE).await?;
        let db = self.state.db.db();
        let conn = db.conn().map_err(|e| rest::tx_to_canonical(e.into()))?;
        // One read per thousand ids, the owner checked per row (RS-13): pricing's reconcile
        // asks a whole batch of receipts each pass. The first id in order that is missing or
        // held by another owner decides the answer, as when each id was read on its own.
        let mut rows = std::collections::HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(1000) {
            for row in repo::find_references(&conn, &scope, tenant, chunk)
                .await
                .map_err(|e| rest::repo_error_to_canonical(&e))?
            {
                rows.insert(row.id, row);
            }
        }
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(row) = rows.get(id) else {
                return Err(DomainError::NotFound {
                    what: "reference",
                    id: *id,
                }
                .into());
            };
            if row.owner_gear != self.owner {
                return Err(service::forbidden(
                    ctx.subject_id(),
                    tenant,
                    "the reference belongs to another owner",
                )
                .into());
            }
            result.push((*id, state(&row.state, row.id)?));
        }
        Ok(result)
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        let (scope, _) = self.scope(ctx, tenant, actions::READ).await?;
        let db = self.state.db.db();
        let conn = db.conn().map_err(|e| rest::tx_to_canonical(e.into()))?;
        g::find(&conn, &scope, tenant, id)
            .await
            .map_err(rest::tx_to_canonical)
    }
    async fn skus_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_ids: &[Uuid],
    ) -> Result<Vec<Sku>, CanonicalError> {
        let (scope, _) = self.scope(ctx, tenant, actions::READ).await?;
        let mut seen = std::collections::BTreeSet::new();
        let mut ordered = Vec::new();
        for id in sku_ids {
            if seen.insert(*id) {
                ordered.push(*id);
            }
        }
        if ordered.is_empty() {
            return Ok(Vec::new());
        }
        let db = self.state.db.db();
        let backend = db.backend();
        let conn = self
            .state
            .db
            .conn()
            .map_err(|e| rest::tx_to_canonical(e.into()))?;
        let found = repo::find_skus(&conn, backend, &scope, tenant, &ordered)
            .await
            .map_err(|e| rest::repo_error_to_canonical(&e))?;
        let mut by_id: std::collections::HashMap<Uuid, Sku> =
            found.into_iter().map(|sku| (sku.id, sku)).collect();
        Ok(ordered
            .into_iter()
            .filter_map(|id| by_id.remove(&id))
            .collect())
    }
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        let (scope, _) = self.scope(ctx, tenant, actions::READ).await?;
        let db = self.state.db.db();
        let conn = db.conn().map_err(|e| rest::tx_to_canonical(e.into()))?;
        g::find(&conn, &scope, tenant, id)
            .await
            .map_err(rest::tx_to_canonical)?;
        repo::version_as_of(&conn, &scope, tenant, id, date)
            .await
            .map_err(|e| rest::repo_error_to_canonical(&e))
    }
}

#[cfg(test)]
#[path = "reference_registry_tests.rs"]
mod reference_registry_tests;
