//! `PaymentApi` contract definition and Contract IR.
//!
//! The Rust trait is the source of truth. The `#[toolkit::contract]` macro
//! derives the Contract IR, static descriptor, and `Contract` impl.
//!
//! Trait-name suffix `Api` (PRD #1536 D6) marks this as a *provided*,
//! remote-capable contract.

use std::pin::Pin;

use futures_core::Stream;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::models::{ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentSummary};

/// Boxed stream type returned by streaming `PaymentApi` methods.
pub type PaymentStream<T> = Pin<Box<dyn Stream<Item = Result<T, CanonicalError>> + Send + 'static>>;

/// Payment API contract — the same trait for local and remote consumption.
///
/// All parameter types are owned and `'static`-compatible.
/// Registered in `ClientHub` as `Arc<dyn PaymentApi>`.
#[toolkit::contract(gear = "api-contracts", version = "v1")]
pub trait PaymentApi: Send + Sync {
    /// Charge a payment. Non-idempotent write.
    ///
    /// # Errors
    ///
    /// Returns a `CanonicalError` if the charge fails (e.g., invalid amount,
    /// payment processor error).
    #[idempotency(NonIdempotentWrite)]
    async fn charge(
        &self,
        ctx: SecurityContext,
        req: ChargeRequest,
    ) -> Result<ChargeResponse, CanonicalError>;

    /// Get an invoice by ID. Safe read.
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

    /// List payments as a server-streaming response.
    #[idempotency(SafeRead)]
    #[streaming]
    fn list_payments(
        &self,
        ctx: SecurityContext,
        filter: ListPaymentsFilter,
    ) -> Result<PaymentSummary, CanonicalError>;
}
