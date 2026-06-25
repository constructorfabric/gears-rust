//! REST projection of [`PaymentApi`].
//!
//! Carries HTTP method/path annotations consumed by `#[toolkit::rest_contract]`.
//! When the `rest-client` feature is enabled the macro also emits a generated
//! `PaymentApiRestClient` struct that implements [`PaymentApi`] over HTTP.
//!
//! The transport-error → `CanonicalError` conversion required by the
//! generated client lives in `toolkit-canonical-errors` behind the
//! `contract-transport` feature, which the SDK's `rest-client` feature
//! turns on.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::contract::PaymentApi;
use crate::models::{ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentSummary};

/// HTTP projection of [`PaymentApi`].
#[toolkit::rest_contract(base_path = "/api/api-contracts/v1")]
pub trait PaymentApiRest: PaymentApi {
    #[post("/payments/charge")]
    async fn charge(
        &self,
        ctx: SecurityContext,
        req: ChargeRequest,
    ) -> Result<ChargeResponse, CanonicalError>;

    #[get("/invoices/{invoice_id}")]
    #[retryable]
    async fn get_invoice(
        &self,
        ctx: SecurityContext,
        invoice_id: String,
    ) -> Result<Invoice, CanonicalError>;

    // `#[server_manual]`: the SSE/streaming server route is registered by hand
    // via `OperationBuilder` in the server crate (see `register_manual_routes`).
    // The generated client + IR still cover this method; only the server-side
    // route generation skips it. Demonstrates that the manual OperationBuilder
    // path remains first-class and composes with the generated routes.
    #[get("/payments")]
    #[streaming]
    #[server_manual]
    fn list_payments(
        &self,
        ctx: SecurityContext,
        filter: ListPaymentsFilter,
    ) -> Result<PaymentSummary, CanonicalError>;
}
