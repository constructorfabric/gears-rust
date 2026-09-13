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
///
/// The error parameter defaults to `CanonicalError`, which every streaming
/// method declares.
///
/// The `E` describes stream *items* over every framing. Over `multipart/mixed`
/// (the `stream_payments` REST projection) a post-open failure is carried as a
/// typed **error part** — one `application/problem+json` part whose body is the
/// serialized `CanonicalError` — so it arrives as `Err(CanonicalError)` with its
/// category preserved, exactly like the SSE `event: error` frame and the gRPC
/// trailing `Status`. It is *not* flattened to a bare transport error. The one
/// residual gap is fidelity of a *typed variant's payload fields* (see
/// `stream_payments`), which the wire `Problem` can carry but `CanonicalError`
/// cannot yet reconstruct — deferred to the canonical-error-macro work (#4734).
pub type PaymentStream<T, E = CanonicalError> =
    Pin<Box<dyn Stream<Item = Result<T, E>> + Send + 'static>>;

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

    /// The same payments, streamed through a **fallible open** (#4734 D2).
    ///
    /// The `async fn` is the whole difference from
    /// [`list_payments`](Self::list_payments): the open becomes a distinct,
    /// awaited operation returning `Result<Stream, E>`, so a failure *before any
    /// item exists* — here, a filter the server will not accept — reaches the
    /// caller's open-time error handling instead of arriving as the first item
    /// of a stream it has already been handed. The authored return type is
    /// unchanged; only `async` moves.
    ///
    /// **Both projections carry it, over different wire framings.** The REST
    /// projection declares `#[streaming(multipart_mixed)]`, so its generated
    /// client speaks `multipart/mixed`, one JSON item per body part; the gRPC
    /// projection needs no framing selector, because gRPC's framing is fixed by
    /// the transport. Both express the fallible open natively — over HTTP it is
    /// the response status, over gRPC the initial `Status` — which is why one
    /// base method can serve both.
    ///
    /// Its error type is `CanonicalError`, like every other method. A fallible
    /// open that wants to hand back a *typed variant with payload fields* — so a
    /// consumer's open-time state machine can branch on, say, which partitions
    /// still need seeding — needs a richer error than `CanonicalError` can
    /// currently carry (it has no field for `error_code`, `error_domain` or
    /// `context["data"]`, so a 400/409 degrades to `Internal` / 500 on the
    /// wire). That enrichment is deferred to a future change to the canonical
    /// error macro; until then this method reports open failures as plain
    /// `CanonicalError`, consistent with the rest of the contract. See #4734.
    ///
    /// # Errors
    ///
    /// Returns a `CanonicalError` from the *open* when the filter is not
    /// acceptable, or when the requested partitions have no committed position.
    /// Failures after a successful open arrive as `Err` items of the returned
    /// stream.
    #[idempotency(SafeRead)]
    #[streaming(open = fallible)]
    async fn stream_payments(
        &self,
        ctx: SecurityContext,
        filter: ListPaymentsFilter,
    ) -> Result<PaymentSummary, CanonicalError>;
}
