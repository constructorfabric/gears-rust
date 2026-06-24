//! Local (in-process) client for `PaymentService`.

use std::sync::Arc;

use api_contracts_sdk::contract::{PaymentApi, PaymentStream};
use api_contracts_sdk::models::{
    ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentSummary,
};
use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::ContractError;
use toolkit_contract::ir::contract::{Idempotency, MethodKind};
use toolkit_contract::policy::{PolicyContext, PolicyStack};
use toolkit_security::SecurityContext;

use crate::domain::service::PaymentDomainService;

/// Direct in-process adapter — zero serialization, zero network.
///
/// Wraps each call with the [`PolicyStack`] for tracing/metrics.
/// Implements [`PaymentApi`] (PRD #1536 D1: consumer always depends on the base contract).
pub struct PaymentLocalClient {
    service: Arc<PaymentDomainService>,
    policies: Arc<PolicyStack>,
}

impl PaymentLocalClient {
    /// Create a local client wrapping the domain service.
    #[must_use]
    pub fn new(service: Arc<PaymentDomainService>, policies: Arc<PolicyStack>) -> Self {
        Self { service, policies }
    }
}

/// Map a policy stack error to `CanonicalError`.
///
/// Takes by value because `PolicyStack::execute` requires `fn(ContractError) -> E`.
#[allow(
    clippy::needless_pass_by_value,
    reason = "required by fn pointer signature"
)]
fn policy_err(e: ContractError) -> CanonicalError {
    CanonicalError::internal(e.to_string()).create()
}

#[async_trait]
impl PaymentApi for PaymentLocalClient {
    async fn charge(
        &self,
        ctx: SecurityContext,
        req: ChargeRequest,
    ) -> Result<ChargeResponse, CanonicalError> {
        let pc = PolicyContext {
            service: "PaymentApi",
            method: "charge",
            idempotency: Idempotency::NonIdempotentWrite,
            kind: MethodKind::Unary,
        };
        let svc = Arc::clone(&self.service);
        self.policies
            .execute(&pc, || async move { svc.charge(&ctx, &req) }, policy_err)
            .await
    }

    async fn get_invoice(
        &self,
        ctx: SecurityContext,
        invoice_id: String,
    ) -> Result<Invoice, CanonicalError> {
        let pc = PolicyContext {
            service: "PaymentApi",
            method: "get_invoice",
            idempotency: Idempotency::SafeRead,
            kind: MethodKind::Unary,
        };
        let svc = Arc::clone(&self.service);
        self.policies
            .execute(
                &pc,
                || async move { svc.get_invoice(&ctx, &invoice_id) },
                policy_err,
            )
            .await
    }

    fn list_payments(
        &self,
        ctx: SecurityContext,
        filter: ListPaymentsFilter,
    ) -> PaymentStream<PaymentSummary> {
        // Streaming: policies are not applied per-item. Per-call hooks would
        // wrap the stream construction; left to a future iteration.
        self.service.list_payments(&ctx, &filter)
    }
}
