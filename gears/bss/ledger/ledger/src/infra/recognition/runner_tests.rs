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

fn segment() -> ReleasableSegment {
    ReleasableSegment {
        schedule_id: "sched-7".to_owned(),
        segment_no: 3,
        period_id: "202607".to_owned(),
        amount_minor: 2_500,
        revenue_stream: "recurring".to_owned(),
        currency: "USD".to_owned(),
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
    assert_eq!(dr.amount_minor, 2_500);
    assert_eq!(cr.amount_minor, 2_500);
    assert_eq!(dr.currency, "USD");
    assert_eq!(cr.currency, "USD");

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

#[test]
fn reversal_entry_is_dr_revenue_cr_contract_liability_same_stream_equal_amount() {
    let ctx = SecurityContext::anonymous();
    let entry = build_reversal_entry(&ctx, Uuid::from_u128(1), &segment());

    assert_eq!(entry.source_doc_type, SourceDocType::Recognition);
    assert_eq!(entry.source_business_id, "sched-7:3:reversal");
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

    // The MIRROR of the release: DR REVENUE (give back the recognized revenue) /
    // CR CONTRACT_LIABILITY (restore the deferred balance).
    assert_eq!(dr.account_class, AccountClass::Revenue);
    assert_eq!(cr.account_class, AccountClass::ContractLiability);

    // Both legs carry the same stream + currency; equal amounts ⇒ balanced.
    assert_eq!(dr.revenue_stream.as_deref(), Some("recurring"));
    assert_eq!(cr.revenue_stream.as_deref(), Some("recurring"));
    assert_eq!(dr.amount_minor, 2_500);
    assert_eq!(cr.amount_minor, 2_500);
    assert_eq!(dr.currency, "USD");
    assert_eq!(cr.currency, "USD");

    // A reversal reverses nothing via the header's reverse-link (it is a fresh
    // compensating entry keyed on the `:reversal` business id, not a strict
    // line-negation reversal); account ids are bound from the chart later.
    assert!(entry.reverses_entry_id.is_none());
    assert_eq!(dr.account_id, Uuid::nil());
    assert_eq!(cr.account_id, Uuid::nil());
}

#[test]
fn due_pending_segment_projects_into_releasable() {
    let due = DuePendingSegment {
        schedule_id: "s1".to_owned(),
        segment_no: 1,
        period_id: "202606".to_owned(),
        amount_minor: 100,
        revenue_stream: "usage".to_owned(),
        currency: "EUR".to_owned(),
        total_deferred_minor: 1_200,
        recognized_minor: 0,
    };
    let r: ReleasableSegment = due.into();
    assert_eq!(r.schedule_id, "s1");
    assert_eq!(r.segment_no, 1);
    assert_eq!(r.revenue_stream, "usage");
    assert_eq!(r.currency, "EUR");
    assert_eq!(r.amount_minor, 100);
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
