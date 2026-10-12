//! Pure unit tests for the `RecognitionRunner` entry construction (Group D):
//! the balanced `DR CONTRACT_LIABILITY / CR REVENUE` shape (same stream both
//! legs, equal amount, `Σ DR == Σ CR`), the `RECOGNITION` idempotency key
//! (`schedule_id:segment_no`), the schedule currency on the entry + lines, and
//! the natural-period `effective_at`. The atomic-release / over-recognition /
//! idempotent-replay behaviours need a database and are Group F4 testcontainers
//! tests (NOT here) — see the note at the foot of this file.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bss_ledger_sdk::{AccountClass, Side, SourceDocType};
use chrono::{Datelike, NaiveDate};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::infra::posting::service::decimal_tests::money;

fn segment() -> ReleasableSegment {
    ReleasableSegment {
        schedule_id: "sched-7".to_owned(),
        segment_no: 3,
        period_id: "202607".to_owned(),
        amount: money("25", "USD", 2),
        revenue_stream: "recurring".to_owned(),
    }
}

#[test]
fn business_id_is_schedule_colon_segment() {
    assert_eq!(recognition_business_id("sched-7", 3), "sched-7:3");
}

#[test]
fn entry_is_recognition_keyed_on_schedule_segment() {
    let ctx = SecurityContext::anonymous();
    let tenant = Uuid::from_u128(1);
    let entry = build_recognition_entry(&ctx, tenant, &segment());

    assert_eq!(entry.source_doc_type, SourceDocType::Recognition);
    assert_eq!(entry.source_business_id, "sched-7:3");
    assert_eq!(entry.tenant_id, tenant);
    assert_eq!(entry.entry_currency, "USD");
    // A forward release reverses nothing.
    assert!(entry.reverses_entry_id.is_none());
    assert!(entry.reverses_period_id.is_none());
}

#[test]
fn entry_is_dr_contract_liability_cr_revenue_same_stream_equal_amount() {
    let ctx = SecurityContext::anonymous();
    let entry = build_recognition_entry(&ctx, Uuid::from_u128(1), &segment());

    assert_eq!(entry.lines.len(), 2, "exactly two legs");

    let dr = entry
        .lines
        .iter()
        .find(|l| l.side == Side::Debit)
        .expect("a DR leg");
    let cr = entry
        .lines
        .iter()
        .find(|l| l.side == Side::Credit)
        .expect("a CR leg");

    // DR CONTRACT_LIABILITY (draw down the deferred balance).
    assert_eq!(dr.account_class, AccountClass::ContractLiability);
    // CR REVENUE (recognize).
    assert_eq!(cr.account_class, AccountClass::Revenue);

    // Both legs carry the SAME stream (per-stream disaggregation, §4.5).
    assert_eq!(dr.revenue_stream.as_deref(), Some("recurring"));
    assert_eq!(cr.revenue_stream.as_deref(), Some("recurring"));

    // Equal amounts ⇒ balanced (Σ DR == Σ CR), the schedule currency on both.
    assert_eq!(dr.money, money("25", "USD", 2));
    assert_eq!(cr.money, money("25", "USD", 2));
    assert_eq!(dr.money.currency().code(), "USD");
    assert_eq!(cr.money.currency().code(), "USD");

    // Lines are bound from the chart later — the builder emits the nil placeholder.
    assert_eq!(dr.account_id, Uuid::nil());
    assert_eq!(cr.account_id, Uuid::nil());

    // A recognition leg carries no AR/invoice/tax dims.
    assert!(dr.invoice_id.is_none());
    assert!(cr.invoice_id.is_none());
    assert!(dr.tax_jurisdiction.is_none());
    assert!(cr.ar_status.is_none());
}

#[test]
fn entry_posts_to_the_segments_period() {
    let ctx = SecurityContext::anonymous();
    let entry = build_recognition_entry(&ctx, Uuid::from_u128(1), &segment());
    assert_eq!(entry.period_id, "202607");
    // effective_at is the first day of that period (Group D natural-period rule).
    assert_eq!(
        entry.effective_at,
        NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
    );
    assert_eq!(entry.effective_at.day(), 1);
    assert_eq!(entry.effective_at.month(), 7);
}

#[test]
fn first_day_of_period_parses_yyyymm() {
    assert_eq!(
        first_day_of_period("202607"),
        NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
    );
    assert_eq!(
        first_day_of_period("202612"),
        NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()
    );
}

#[test]
fn first_day_of_period_malformed_falls_back_to_a_gate_rejectable_sentinel() {
    // A malformed period yields `NaiveDate::MIN` — the foundation OPEN-period
    // gate rejects it; it never silently posts to a wrong date.
    let bad = first_day_of_period("oops");
    assert_eq!(bad, NaiveDate::MIN);
    // Out-of-range month is also rejected.
    assert_eq!(first_day_of_period("202613"), NaiveDate::MIN);
}

#[test]
fn reversal_business_id_is_schedule_colon_segment_colon_reversal() {
    // Distinct from the forward-release key (`sched-7:3`), so a reversal is its
    // own at-most-once unit and never collides with the original DONE release.
    assert_eq!(reversal_business_id("sched-7", 3), "sched-7:3:reversal");
    assert_ne!(
        reversal_business_id("sched-7", 3),
        recognition_business_id("sched-7", 3)
    );
}

fn schedule_state() -> ScheduleState {
    ScheduleState {
        tenant_id: Uuid::from_u128(1),
        schedule_id: "sched-7".to_owned(),
        payer_tenant_id: Uuid::from_u128(2),
        source_invoice_id: "INV-1".to_owned(),
        source_invoice_item_ref: "item-1".to_owned(),
        po_allocation_group: None,
        subscription_ref: None,
        revenue_stream: "recurring".to_owned(),
        total_deferred: money("100", "USD", 2),
        recognized: money("25", "USD", 2),
        policy_ref: "policy".to_owned(),
        ssp_snapshot_ref: None,
        vc_estimate_ref: None,
        vc_method_ref: None,
        status: "ACTIVE".to_owned(),
        version: 1,
    }
}

fn segment_state() -> SegmentState {
    SegmentState {
        tenant_id: Uuid::from_u128(1),
        schedule_id: "sched-7".to_owned(),
        segment_no: 3,
        period_id: "202607".to_owned(),
        amount: money("25", "USD", 2),
        version: 1,
        status: "DONE".to_owned(),
        recognized_at: None,
        run_id: None,
    }
}

fn release_line(class: &str, side: &str, amount: &str) -> crate::domain::model::LineRecord {
    crate::domain::model::LineRecord {
        line_id: Uuid::now_v7(),
        entry_id: Uuid::from_u128(9),
        tenant_id: Uuid::from_u128(1),
        period_id: "202607".to_owned(),
        payer_tenant_id: Uuid::nil(),
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::now_v7(),
        account_class: class.to_owned(),
        gl_code: None,
        side: side.to_owned(),
        money: money(amount, "USD", 2),
        invoice_id: None,
        due_date: None,
        revenue_stream: Some("recurring".to_owned()),
        mapping_status: "RESOLVED".to_owned(),
        functional_money: None,
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
    }
}

fn original(lines: Vec<crate::domain::model::LineRecord>) -> crate::domain::model::EntryRecord {
    crate::domain::model::EntryRecord {
        entry_id: Uuid::from_u128(9),
        tenant_id: Uuid::from_u128(1),
        legal_entity_id: Uuid::nil(),
        period_id: "202607".to_owned(),
        entry_currency: "USD".to_owned(),
        source_doc_type: "RECOGNITION".to_owned(),
        source_business_id: "sched-7:3".to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: time::OffsetDateTime::UNIX_EPOCH,
        effective_at: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: Uuid::nil(),
        correlation_id: Uuid::nil(),
        rounding_evidence: serde_json::json!({}),
        created_seq: 1,
        lines,
    }
}

/// The original-release check sums the stored release by parsed class and side:
/// a DR liability / CR revenue pair equal to the segment passes.
#[test]
fn original_release_with_the_segment_amount_passes() {
    let entry = original(vec![
        release_line("CONTRACT_LIABILITY", "DR", "25"),
        release_line("REVENUE", "CR", "25"),
        // Lines of other classes are ignored.
        release_line("AR", "DR", "999"),
    ]);
    assert!(validate_original_release(&entry, &schedule_state(), &segment_state()).is_ok());
}

/// An unknown stored side or class is an invariant failure, never a silent
/// credit; swapped sides make the totals disagree with the segment.
#[test]
fn original_release_with_unknown_or_swapped_side_or_class_is_internal() {
    for lines in [
        vec![
            release_line("CONTRACT_LIABILITY", "XX", "25"),
            release_line("REVENUE", "CR", "25"),
        ],
        vec![
            release_line("CONTRACT_LIABILITY", "DR", "25"),
            release_line("NOT_A_CLASS", "CR", "25"),
        ],
        vec![
            release_line("CONTRACT_LIABILITY", "CR", "25"),
            release_line("REVENUE", "DR", "25"),
        ],
        vec![
            release_line("CONTRACT_LIABILITY", "DR", "25"),
            release_line("REVENUE", "CR", "24"),
        ],
    ] {
        let entry = original(lines);
        assert!(matches!(
            validate_original_release(&entry, &schedule_state(), &segment_state()),
            Err(DomainError::Internal(_))
        ));
    }
}

// ── NOTE — Group F4 testcontainers coverage (NOT in this pure-unit file) ──
// The integration/concurrency tests for the release + reversal live in
// `tests/postgres_recognition_run.rs` (Group F4, design §11), driving the REAL
// `RecognitionRunService` against a testcontainer Postgres. They cover:
//   * atomic release: `DR CL / CR Revenue` + the `recognized_minor += amount`
//     bump + the segment `→ DONE` stamp all commit in ONE txn;
//   * at-most-once: a re-run of the same `(schedule, segment)` replays the prior
//     entry (no second credit) via the `RECOGNITION` idempotency claim + the
//     `status = DONE` / `UNIQUE (schedule, period_id)` guards;
//   * over-recognition: a release pushing `recognized_minor` past
//     `total_deferred_minor` is blocked at the per-schedule cap CHECK and maps to
//     `OverRecognition` (409) — even when a sibling schedule keeps the per-stream
//     `CONTRACT_LIABILITY` account aggregate positive;
//   * reversal: `release_reversal` posts `DR Revenue / CR CL`, decrements
//     `recognized_minor`, and the reversed segment stays `DONE`;
//   * racing runs on the same / different segments → no double-credit;
//   * ordering: period N released before N-1 is DONE → N parked QUEUED, then
//     drained by a later run once N-1 commits.
