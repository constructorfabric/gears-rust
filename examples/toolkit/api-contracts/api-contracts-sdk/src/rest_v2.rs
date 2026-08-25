//! REST projection of [`PaymentApiV2`].
//!
//! Mounted at `/api-contracts/v2`, alongside v1's `/api-contracts/v1`.
//! Both projections generate their own `register_*_routes()` function, and the
//! server composes them onto a single `axum::Router` — that is how two major
//! versions are served simultaneously during a migration window (ADR-0007).
//!
//! `require_full_coverage` opts into the generated coverage assertion: it checks
//! that the base trait and this projection cover exactly the same method set,
//! and that the contract's `version = "v2"` really appears as a segment of
//! `base_path` (so a version bump cannot land in one place only).
//!
//! Because the trait name carries the version (`PaymentApiV2Rest`), every
//! generated identifier is distinct from v1's — `operation_id`s
//! (`payment_api_v2_rest_*`), `register_payment_api_v2_rest_routes`, the client
//! struct, and the `OTel` `rpc.service` field. Naming the v2 trait identically
//! to v1 in a different module would instead emit duplicate `operationId`s into
//! the shared `OpenAPI` document and collide in `#[toolkit::provides]`.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::contract_v2::PaymentApiV2;
use crate::models::{ChargeV2Request, ChargeV2Response, Invoice};

/// HTTP projection of [`PaymentApiV2`].
#[toolkit::rest_contract(base_path = "/api-contracts/v2", require_full_coverage)]
pub trait PaymentApiV2Rest: PaymentApiV2 {
    // `#[retryable]` is sound here only because v2's `charge` is an idempotent
    // write keyed by `idempotency_key` (see the base trait) — the client may
    // re-send a charge whose response was lost without double-charging.
    #[post("/payments/charge")]
    #[retryable]
    async fn charge(
        &self,
        ctx: SecurityContext,
        req: ChargeV2Request,
    ) -> Result<ChargeV2Response, CanonicalError>;

    #[get("/invoices/{invoice_id}")]
    #[retryable]
    async fn get_invoice(
        &self,
        ctx: SecurityContext,
        invoice_id: String,
    ) -> Result<Invoice, CanonicalError>;
}
