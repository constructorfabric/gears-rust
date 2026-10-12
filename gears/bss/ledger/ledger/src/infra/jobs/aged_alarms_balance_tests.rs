//! The positivity gate of the aging detectors over stored balance text: a
//! negative, zero, non-canonical or garbled cached balance is never an aged
//! grain, and a refund-clearing grain it skips feeds no age gauge.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::NaiveDate;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{aged_grains, aged_refund_clearing_grains};
use crate::infra::metrics::test_harness::MetricsHarness;
use crate::infra::storage::entity::{
    account_balance, journal_entry, journal_line, unallocated_balance,
};

const TENANT: u128 = 0xA7;
const PAYER: u128 = 0xB7;

fn entry(entry_id: Uuid, posted_at: OffsetDateTime) -> journal_entry::Model {
    journal_entry::Model {
        entry_id,
        tenant_id: Uuid::from_u128(TENANT),
        legal_entity_id: Uuid::from_u128(TENANT),
        period_id: "202606".to_owned(),
        entry_currency: "USD".to_owned(),
        source_doc_type: "PAYMENT_SETTLE".to_owned(),
        source_business_id: "pay-1".to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: posted_at,
        effective_at: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: Uuid::from_u128(TENANT),
        correlation_id: Uuid::from_u128(TENANT),
        rounding_evidence: serde_json::Value::Null,
        created_seq: 1,
        row_hash: None,
        prev_hash: None,
        prev_entry_id: None,
        prev_period_id: None,
    }
}

fn line(entry_id: Uuid, account: u128, class: &str) -> journal_line::Model {
    journal_line::Model {
        line_id: Uuid::now_v7(),
        entry_id,
        tenant_id: Uuid::from_u128(TENANT),
        period_id: "202606".to_owned(),
        payer_tenant_id: Uuid::from_u128(PAYER),
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::from_u128(account),
        account_class: class.to_owned(),
        gl_code: None,
        side: "CR".to_owned(),
        amount: "10".to_owned(),
        currency: "USD".to_owned(),
        currency_scale: 2,
        invoice_id: None,
        due_date: None,
        revenue_stream: None,
        mapping_status: "RESOLVED".to_owned(),
        functional_amount: None,
        functional_currency: None,
        functional_currency_scale: None,
        tax_jurisdiction: None,
        tax_filing_period: None,
        tax_rate_ref: None,
        legal_entity_id: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
        po_allocation_group: None,
        credit_grant_event_type: None,
        ar_status: None,
        rate_snapshot_ref: None,
    }
}

fn unallocated(account: u128, balance: &str) -> unallocated_balance::Model {
    unallocated_balance::Model {
        tenant_id: Uuid::from_u128(TENANT),
        payer_tenant_id: Uuid::from_u128(PAYER),
        account_id: Uuid::from_u128(account),
        currency: "USD".to_owned(),
        currency_scale: 2,
        balance: balance.to_owned(),
        functional_balance: None,
        functional_currency: None,
        functional_currency_scale: None,
        last_entry_seq: None,
        version: 0,
    }
}

fn clearing(account: u128, balance: &str) -> account_balance::Model {
    account_balance::Model {
        tenant_id: Uuid::from_u128(TENANT),
        account_id: Uuid::from_u128(account),
        currency: "USD".to_owned(),
        currency_scale: 2,
        account_class: "REFUND_CLEARING".to_owned(),
        normal_side: "CR".to_owned(),
        balance: balance.to_owned(),
        functional_balance: None,
        functional_currency: None,
        functional_currency_scale: None,
        last_entry_seq: None,
        version: 0,
    }
}

/// Balance texts the gate must refuse: negative, zero, non-canonical, garbled,
/// off-scale.
const NOT_AGED: [&str; 5] = ["-5", "0", "10.00", "garbage", "0.001"];

#[test]
fn aged_unallocated_skips_negative_zero_and_corrupt_cached_balances() {
    let now = OffsetDateTime::now_utc();
    let cutoff = now - Duration::days(7);
    let old = Uuid::now_v7();
    let entries = vec![entry(old, now - Duration::days(30))];
    for balance in NOT_AGED {
        let lines = vec![line(old, 1, "UNALLOCATED")];
        let cache = vec![unallocated(1, balance)];
        assert!(
            aged_grains(&entries, &lines, &cache, now, cutoff).is_empty(),
            "{balance}: never an aged grain"
        );
    }
    // The control: a positive canonical balance on the same old grain is aged.
    let lines = vec![line(old, 1, "UNALLOCATED")];
    let aged = aged_grains(&entries, &lines, &[unallocated(1, "10")], now, cutoff);
    assert_eq!(aged.len(), 1);
    assert_eq!(aged[0].balance.to_string(), "10 USD");
}

#[test]
fn aged_refund_clearing_skips_negative_and_corrupt_balances_and_their_gauge() {
    let now = OffsetDateTime::now_utc();
    let warn = now - Duration::days(7);
    let page = now - Duration::days(14);
    let old = Uuid::now_v7();
    let entries = vec![entry(old, now - Duration::days(30))];
    for balance in NOT_AGED {
        let harness = MetricsHarness::new();
        let metrics = harness.metrics();
        let aged = aged_refund_clearing_grains(
            Uuid::from_u128(TENANT),
            &entries,
            &[line(old, 2, "REFUND_CLEARING")],
            &[clearing(2, balance)],
            now,
            warn,
            page,
            &metrics,
        );
        assert!(aged.is_empty(), "{balance}: never an aged grain");
        harness.force_flush();
        assert_eq!(
            harness.age_gauge_value(
                "ledger_refund_clearing_aged_seconds",
                &[("tenant", &Uuid::from_u128(TENANT).to_string())]
            ),
            None,
            "{balance}: a skipped grain feeds no age gauge"
        );
    }
    // The control: a positive canonical balance is aged, paged and gauged.
    let harness = MetricsHarness::new();
    let metrics = harness.metrics();
    let aged = aged_refund_clearing_grains(
        Uuid::from_u128(TENANT),
        &entries,
        &[line(old, 2, "REFUND_CLEARING")],
        &[clearing(2, "5")],
        now,
        warn,
        page,
        &metrics,
    );
    assert_eq!(aged.len(), 1);
    assert!(aged[0].paged);
    harness.force_flush();
    assert!(
        harness
            .age_gauge_value(
                "ledger_refund_clearing_aged_seconds",
                &[("tenant", &Uuid::from_u128(TENANT).to_string())]
            )
            .is_some()
    );
}
