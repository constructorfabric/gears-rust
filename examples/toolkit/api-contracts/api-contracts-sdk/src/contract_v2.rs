//! `PaymentApiV2` — the **v2** major version of the payment contract.
//!
//! ADR-0007: additive changes ship inside the current major version; a
//! **breaking** change ships as a *parallel* trait that is served alongside v1
//! for the duration of the migration window. This trait is that parallel
//! version — [`PaymentApi`](crate::contract::PaymentApi) stays untouched and
//! keeps working for existing callers.
//!
//! What v2 changes relative to v1:
//!
//! | v1 | v2 | why |
//! |----|----|-----|
//! | `charge(ChargeRequest) -> ChargeResponse` | `charge(ChargeV2Request) -> ChargeV2Response` | **breaking**: `amount_cents` renamed to `amount_minor`, `idempotency_key` is newly required |
//! | `get_invoice(..) -> Invoice` | unchanged, `Invoice` reused | carried over as-is |
//! | `list_payments(..)` (streaming) | **removed** | a method removal is breaking, hence the new major |
//!
//! The trailing `V2` does not disturb the contract-type suffix rule (DESIGN D6):
//! the macro strips a trailing major-version marker before classifying, so
//! `PaymentApiV2` is an `Api` contract exactly like `PaymentApi`.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::models::{ChargeV2Request, ChargeV2Response, Invoice};

/// Payment API contract, v2 — the same trait for local and remote consumption.
///
/// Registered in `ClientHub` as `Arc<dyn PaymentApiV2>`, which is a key distinct
/// from `Arc<dyn PaymentApi>` (the hub keys by fully-qualified type name), so
/// both versions coexist in one process and a consumer's version choice is a
/// compile-time property.
#[toolkit::contract(gear = "api-contracts", version = "v2")]
pub trait PaymentApiV2: Send + Sync {
    /// Charge a payment. **Idempotent** write: repeating a call with the same
    /// [`ChargeV2Request::idempotency_key`] returns the original outcome instead
    /// of charging twice. This is what v2's newly required key buys, and it is
    /// why the REST projection marks this method `#[retryable]` — a charge whose
    /// response is lost to a transient failure is safe to re-send.
    ///
    /// Implementations MUST scope the dedupe store by the caller's identity —
    /// `(tenant, subject, idempotency_key)` from `ctx`, never the raw
    /// client-supplied string — otherwise one tenant could replay another
    /// tenant's key and receive their `ChargeV2Response`.
    ///
    /// # Errors
    ///
    /// Returns a `CanonicalError` if the charge fails (e.g., invalid amount,
    /// payment processor error).
    #[idempotency(IdempotentWrite)]
    async fn charge(
        &self,
        ctx: SecurityContext,
        req: ChargeV2Request,
    ) -> Result<ChargeV2Response, CanonicalError>;

    /// Get an invoice by ID. Safe read. Unchanged from v1.
    ///
    /// # Errors
    ///
    /// Returns a `CanonicalError` if the invoice is not found or access is
    /// denied.
    #[idempotency(SafeRead)]
    async fn get_invoice(
        &self,
        ctx: SecurityContext,
        invoice_id: String,
    ) -> Result<Invoice, CanonicalError>;
}
