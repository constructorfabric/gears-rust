//! Shared authorized commercial boundary over immutable versioned receipts.
mod check;
pub mod errors;
mod fulfilment;
pub mod wire;
use crate::{
    api::rest::authoring::AuthoringState,
    authz::{self, OwnerTenant, ResourceRef, actions, resource_types},
    config::SellerHoldPolicy,
    infra::{
        clock::Clock,
        storage::{RepoError, repo::acceptance_repo},
    },
};
use authz_resolver_sdk::PolicyEnforcer;
use bss_pricing_sdk::acceptance::{AcceptanceQuery, AcceptanceReceipt, CommercialReason};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// One service shared by the sellability and acceptance capabilities.
pub struct CommercialTermsService {
    state: Arc<AuthoringState>,
    enforcer: Arc<PolicyEnforcer>,
    clock: Arc<dyn Clock>,
    policy: SellerHoldPolicy,
}
impl CommercialTermsService {
    /// Attach runtime dependencies. Gear startup validates the policy first.
    #[must_use]
    pub fn new(
        state: Arc<AuthoringState>,
        enforcer: Arc<PolicyEnforcer>,
        clock: Arc<dyn Clock>,
        policy: SellerHoldPolicy,
    ) -> Self {
        Self {
            state,
            enforcer,
            clock,
            policy,
        }
    }
    pub(crate) async fn scope(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        action: &str,
        id: Option<Uuid>,
    ) -> Result<AccessScope, CanonicalError> {
        if !crate::api::rest::authoring::support::authenticated(ctx) {
            return Err(CanonicalError::unauthenticated()
                .with_reason("AUTHENTICATION_REQUIRED")
                .create());
        }
        authz::access_scope(
            &self.enforcer,
            ctx,
            &resource_types::ACCEPTANCE,
            action,
            Some(OwnerTenant(tenant)),
            id.map(ResourceRef),
        )
        .await
        .map_err(errors::authorization)
    }
    pub(crate) async fn acceptance(
        &self,
        ctx: &SecurityContext,
        query: AcceptanceQuery,
    ) -> Result<AcceptanceReceipt, CanonicalError> {
        let scope = self
            .scope(
                ctx,
                query.catalog.tenant_id,
                actions::READ,
                Some(query.acceptance_id),
            )
            .await?;
        let conn = self
            .state
            .db
            .conn()
            .map_err(|e| errors::storage(RepoError::from(e)))?;
        let row =
            acceptance_repo::find(&conn, &scope, query.catalog.tenant_id, query.acceptance_id)
                .await
                .map_err(errors::storage)?
                .ok_or_else(|| CanonicalError::from(CommercialReason::ReceiptNotFound))?;
        wire::decode_acceptance(&row.receipt_json).map_err(errors::storage)
    }
}
