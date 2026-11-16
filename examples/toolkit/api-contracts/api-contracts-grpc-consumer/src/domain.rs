//! Forwards requests to the `PaymentApi` contract resolved from the `ClientHub`.

use std::sync::Arc;
use std::time::Duration;

use api_contracts_sdk::PaymentApi;
use api_contracts_sdk::contract::PaymentStream;
use api_contracts_sdk::models::{
    ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentSummary,
};
use futures_util::TryStreamExt as _;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// How long collecting the payment stream may take. The generated client's
/// timeout covers opening the stream, not reading it to the end.
pub const LIST_DEADLINE: Duration = Duration::from_secs(10);

/// Resolves `PaymentApi` from the hub per call. Over gRPC the hub holds the
/// directory-resolving client wired by `#[toolkit::consumes]`.
pub struct PaymentProxyService {
    hub: Arc<ClientHub>,
}

impl PaymentProxyService {
    /// Build the service over a `ClientHub` handle (typically `ctx.client_hub()`).
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }

    fn api(&self) -> Result<Arc<dyn PaymentApi>, CanonicalError> {
        // The error names only a Rust type path, so it is safe in the Problem.
        self.hub.get::<dyn PaymentApi>().map_err(|err| {
            CanonicalError::service_unavailable()
                .with_detail(format!("PaymentApi not resolvable from ClientHub: {err}"))
                .create()
        })
    }

    /// Forward a charge.
    ///
    /// # Errors
    /// `ServiceUnavailable` when `PaymentApi` is not wired, otherwise whatever
    /// the provider returns.
    pub async fn charge(
        &self,
        ctx: SecurityContext,
        req: ChargeRequest,
    ) -> Result<ChargeResponse, CanonicalError> {
        self.api()?.charge(ctx, req).await
    }

    /// Fetch one invoice.
    ///
    /// # Errors
    /// `ServiceUnavailable` when `PaymentApi` is not wired, otherwise whatever
    /// the provider returns (`NotFound` for an unknown invoice).
    pub async fn invoice(
        &self,
        ctx: SecurityContext,
        invoice_id: String,
    ) -> Result<Invoice, CanonicalError> {
        self.api()?.get_invoice(ctx, invoice_id).await
    }

    /// Read the caller's payments from the provider's server stream.
    ///
    /// # Errors
    /// `ServiceUnavailable` when `PaymentApi` is not wired or the stream does
    /// not finish within [`LIST_DEADLINE`], otherwise the first error the
    /// stream yields.
    pub async fn payments(
        &self,
        ctx: SecurityContext,
    ) -> Result<Vec<PaymentSummary>, CanonicalError> {
        let stream = self
            .api()?
            .list_payments(ctx, ListPaymentsFilter::default());
        tokio::time::timeout(LIST_DEADLINE, stream.try_collect())
            .await
            .unwrap_or_else(|_| {
                Err(CanonicalError::service_unavailable()
                    .with_detail("payment stream did not finish in time")
                    .create())
            })
    }

    /// Open the provider's payment feed (`stream_payments`, a fallible open).
    /// The caller forwards the items as they arrive.
    ///
    /// # Errors
    /// `ServiceUnavailable` when `PaymentApi` is not wired, otherwise the
    /// provider's open-time rejection (`InvalidArgument` for a bad currency,
    /// `FailedPrecondition` for `status = failed`), before any item.
    pub async fn open_feed(
        &self,
        ctx: SecurityContext,
        filter: ListPaymentsFilter,
    ) -> Result<PaymentStream<PaymentSummary>, CanonicalError> {
        self.api()?.stream_payments(ctx, filter).await
    }
}
