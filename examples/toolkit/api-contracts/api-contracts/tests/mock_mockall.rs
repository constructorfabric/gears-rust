//! `mockall`-based mocks for [`PaymentApi`].
//!
//! Demonstrates expectation-based mocking with the `mockall` crate. Useful
//! when you need to assert specific call counts, argument predicates, or
//! ordered sequences. For simpler stubs, prefer the manual-mock pattern in
//! `mock_manual.rs`.

#![allow(clippy::unwrap_used)]
#![cfg_attr(coverage_nightly, coverage(off))]

use api_contracts_sdk::contract::{PaymentApi, PaymentStream};
use api_contracts_sdk::models::{
    ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentStatus, PaymentSummary,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream;
use mockall::mock;
use mockall::predicate::eq;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

mock! {
    pub PaymentApiMock {}

    #[async_trait]
    impl PaymentApi for PaymentApiMock {
        async fn charge(
            &self,
            ctx: SecurityContext,
            req: ChargeRequest,
        ) -> Result<ChargeResponse, CanonicalError>;

        async fn get_invoice(
            &self,
            ctx: SecurityContext,
            invoice_id: String,
        ) -> Result<Invoice, CanonicalError>;

        fn list_payments(
            &self,
            ctx: SecurityContext,
            filter: ListPaymentsFilter,
        ) -> PaymentStream<PaymentSummary>;
    }
}

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

#[tokio::test]
async fn mockall_expects_charge_then_get_invoice() {
    let payment_id = Uuid::new_v4();
    let mut mock = MockPaymentApiMock::new();

    mock.expect_charge()
        .withf(|_, req: &ChargeRequest| req.amount_cents == 1000 && req.currency == "USD")
        .times(1)
        .returning(move |_, _| Ok(ChargeResponse::new(payment_id, PaymentStatus::Pending)));

    let invoice_id_string = payment_id.to_string();
    mock.expect_get_invoice()
        .with(mockall::predicate::always(), eq(invoice_id_string.clone()))
        .times(1)
        .returning(move |_, _| {
            Ok(Invoice::new(
                payment_id,
                payment_id,
                1000,
                "USD",
                "rent",
                PaymentStatus::Pending,
            ))
        });

    // Drive both calls in order. Mockall enforces `times(1)` on each.
    let resp = mock
        .charge(ctx(), ChargeRequest::new(1000, "USD", "rent"))
        .await
        .unwrap();
    assert_eq!(resp.payment_id, payment_id);

    let invoice = mock.get_invoice(ctx(), invoice_id_string).await.unwrap();
    assert_eq!(invoice.payment_id, payment_id);
    assert_eq!(invoice.amount_cents, 1000);
}

#[tokio::test]
async fn mockall_streaming_via_box_pin() {
    let mut mock = MockPaymentApiMock::new();

    mock.expect_list_payments().times(1).returning(|_, _| {
        let items = vec![
            Ok(PaymentSummary::new(
                Uuid::nil(),
                100,
                "USD",
                PaymentStatus::Pending,
            )),
            Ok(PaymentSummary::new(
                Uuid::nil(),
                200,
                "EUR",
                PaymentStatus::Completed,
            )),
        ];
        Box::pin(stream::iter(items))
    });

    let stream = mock.list_payments(ctx(), ListPaymentsFilter::default());
    let collected: Vec<_> = stream.collect::<Vec<_>>().await;
    assert_eq!(collected.len(), 2);
    let first = collected[0].as_ref().unwrap();
    assert_eq!(first.amount_cents, 100);
    assert_eq!(first.currency, "USD");
}

#[tokio::test]
async fn mockall_call_count_assertion() {
    let mut mock = MockPaymentApiMock::new();

    let payment_id = Uuid::new_v4();
    mock.expect_charge()
        .times(2)
        .returning(move |_, _| Ok(ChargeResponse::new(payment_id, PaymentStatus::Pending)));

    mock.charge(ctx(), ChargeRequest::new(100, "USD", "first"))
        .await
        .unwrap();
    mock.charge(ctx(), ChargeRequest::new(200, "USD", "second"))
        .await
        .unwrap();
    // Drop'd at end of scope: mockall enforces `times(2)` matched exactly.
}
