//! Manual-mock pattern for [`PaymentApi`].
//!
//! This file demonstrates the convention used elsewhere in the project
//! (`examples/toolkit/users-info/users-info/src/test_support.rs`,
//! `MockEventPublisher` / `MockAuditPort`): a hand-written struct that
//! implements the trait, records calls, and returns canned responses.
//!
//! No external mocking framework — minimal dependencies. Suitable for
//! simple cases where you just need to substitute the trait at the
//! `Arc<dyn PaymentApi>` boundary.

#![allow(clippy::unwrap_used)]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::sync::Arc;

use api_contracts_sdk::contract::{PaymentApi, PaymentStream};
use api_contracts_sdk::models::{
    ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentStatus, PaymentSummary,
};
use async_trait::async_trait;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use parking_lot::Mutex;
use uuid::Uuid;

/// Manual mock for [`PaymentApi`] that records charge calls and returns
/// canned responses for `get_invoice`/`list_payments`.
struct ManualPaymentApi {
    charge_calls: Mutex<Vec<ChargeRequest>>,
    canned_charge: ChargeResponse,
    canned_invoice: Option<Invoice>,
}

impl ManualPaymentApi {
    fn new(canned_charge: ChargeResponse, canned_invoice: Option<Invoice>) -> Self {
        Self {
            charge_calls: Mutex::new(Vec::new()),
            canned_charge,
            canned_invoice,
        }
    }

    fn charge_calls(&self) -> Vec<ChargeRequest> {
        self.charge_calls.lock().clone()
    }
}

#[async_trait]
impl PaymentApi for ManualPaymentApi {
    async fn charge(
        &self,
        _ctx: SecurityContext,
        req: ChargeRequest,
    ) -> Result<ChargeResponse, CanonicalError> {
        self.charge_calls.lock().push(req);
        Ok(self.canned_charge.clone())
    }

    async fn get_invoice(
        &self,
        _ctx: SecurityContext,
        _invoice_id: String,
    ) -> Result<Invoice, CanonicalError> {
        self.canned_invoice
            .clone()
            .ok_or_else(|| CanonicalError::internal("no canned invoice configured").create())
    }

    fn list_payments(
        &self,
        _ctx: SecurityContext,
        _filter: ListPaymentsFilter,
    ) -> PaymentStream<PaymentSummary> {
        // Empty stream by default — overridden in tests that need data.
        Box::pin(futures_util::stream::empty::<
            Result<PaymentSummary, CanonicalError>,
        >())
    }
}

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

#[tokio::test]
async fn mock_records_charge_call() {
    let payment_id = Uuid::new_v4();
    let mock = ManualPaymentApi::new(
        ChargeResponse::new(payment_id, PaymentStatus::Pending),
        None,
    );

    let req = ChargeRequest::new(1000, "USD", "rent");
    let resp = mock.charge(ctx(), req.clone()).await.unwrap();

    assert_eq!(resp.payment_id, payment_id);
    assert_eq!(resp.status, PaymentStatus::Pending);
    let calls = mock.charge_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].amount_cents, 1000);
    assert_eq!(calls[0].currency, "USD");
}

#[tokio::test]
async fn mock_returns_canned_invoice() {
    let id = Uuid::new_v4();
    let invoice = Invoice::new(
        id,
        id,
        2500,
        "EUR",
        "subscription",
        PaymentStatus::Completed,
    );
    let mock = ManualPaymentApi::new(
        ChargeResponse::new(id, PaymentStatus::Pending),
        Some(invoice.clone()),
    );

    let got = mock.get_invoice(ctx(), id.to_string()).await.unwrap();
    assert_eq!(got.invoice_id, invoice.invoice_id);
    assert_eq!(got.amount_cents, 2500);
    assert_eq!(got.currency, "EUR");
}

#[tokio::test]
async fn mock_can_be_swapped_via_client_hub() {
    let id = Uuid::new_v4();
    let mock: Arc<dyn PaymentApi> = Arc::new(ManualPaymentApi::new(
        ChargeResponse::new(id, PaymentStatus::Pending),
        None,
    ));

    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn PaymentApi>(mock);

    let resolved = hub.get::<dyn PaymentApi>().unwrap();
    let req = ChargeRequest::new(99, "USD", "test");
    let resp = resolved.charge(ctx(), req).await.unwrap();
    assert_eq!(resp.payment_id, id);
}
