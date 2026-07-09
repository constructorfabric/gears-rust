//! Unit tests for the `UnrealizedRevaluationRun` pure helpers (idempotency
//! business-id shape, period-end effective date, scope→account-class mapping).
//! The end-to-end run (cache scan → remeasure → post → dual-column trigger) is a
//! testcontainer test (Group J).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn business_id_is_period_colon_scope_colon_payer() {
    let payer = uuid::uuid!("11111111-1111-1111-1111-111111111111");
    assert_eq!(
        business_id("202606", RevaluationScope::Ar, payer),
        "202606:AR:11111111-1111-1111-1111-111111111111"
    );
    assert_eq!(
        business_id("202612", RevaluationScope::Unallocated, payer),
        "202612:UNALLOCATED:11111111-1111-1111-1111-111111111111"
    );
    // The reversal lookup prefix is `period:scope:` (every payer).
    assert_eq!(
        business_id_prefix("202601", RevaluationScope::ReusableCredit),
        "202601:REUSABLE_CREDIT:"
    );
    assert!(
        business_id("202601", RevaluationScope::ReusableCredit, payer).starts_with(
            &business_id_prefix("202601", RevaluationScope::ReusableCredit)
        )
    );
}

#[test]
fn period_end_naive_is_last_day_of_month() {
    assert_eq!(
        period_end_naive("202606"),
        NaiveDate::from_ymd_opt(2026, 6, 30).unwrap()
    );
    // December rolls over: end of 2026-12 is 2026-12-31.
    assert_eq!(
        period_end_naive("202612"),
        NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()
    );
    // February (non-leap) ends on the 28th.
    assert_eq!(
        period_end_naive("202602"),
        NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()
    );
}

#[test]
fn scope_account_class_maps_each_scope() {
    assert_eq!(scope_account_class(RevaluationScope::Ar), AccountClass::Ar);
    assert_eq!(
        scope_account_class(RevaluationScope::Unallocated),
        AccountClass::Unallocated
    );
    assert_eq!(
        scope_account_class(RevaluationScope::ReusableCredit),
        AccountClass::ReusableCredit
    );
}
