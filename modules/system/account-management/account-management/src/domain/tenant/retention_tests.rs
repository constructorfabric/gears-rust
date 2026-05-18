//! Companion tests for `domain::tenant::retention` — kept out-of-line
//! so the inline test block does not exceed DE1101's 100-line limit.

#![allow(
    clippy::duration_suboptimal_units,
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use super::*;

fn ts(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).expect("valid epoch")
}

#[test]
fn is_due_crosses_boundary_inclusive() {
    let deleted = ts(1_000_000);
    let win = Duration::from_secs(60);
    // Before boundary — not due.
    assert!(!is_due(ts(1_000_000 + 59), deleted, win));
    // On boundary — due (inclusive).
    assert!(is_due(ts(1_000_000 + 60), deleted, win));
    // Past boundary — due.
    assert!(is_due(ts(1_000_000 + 61), deleted, win));
}

#[test]
fn is_due_rejects_invalid_retention_window() {
    let deleted = ts(1_000_000);
    assert!(!is_due(ts(1_000_001), deleted, Duration::MAX));
}

#[test]
fn order_batch_leaf_first_sorts_depth_desc() {
    let a = TenantRetentionRow {
        id: Uuid::from_u128(0x1),
        depth: 1,
        deleted_at: ts(100),
        retention_window: Duration::from_secs(60),
        claimed_by: Uuid::nil(),
    };
    let b = TenantRetentionRow {
        id: Uuid::from_u128(0x2),
        depth: 3,
        deleted_at: ts(100),
        retention_window: Duration::from_secs(60),
        claimed_by: Uuid::nil(),
    };
    let c = TenantRetentionRow {
        id: Uuid::from_u128(0x3),
        depth: 2,
        deleted_at: ts(100),
        retention_window: Duration::from_secs(60),
        claimed_by: Uuid::nil(),
    };
    let d = TenantRetentionRow {
        id: Uuid::from_u128(0x4),
        // tie with `c` on depth=2; id order decides
        depth: 2,
        deleted_at: ts(100),
        retention_window: Duration::from_secs(60),
        claimed_by: Uuid::nil(),
    };
    let ordered = order_batch_leaf_first(vec![a.clone(), b.clone(), c.clone(), d.clone()]);
    assert_eq!(
        ordered.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![b.id, c.id, d.id, a.id]
    );
}

#[test]
fn order_batch_leaf_first_uses_deleted_at_before_id_at_same_depth() {
    // Anti-starvation contract: among siblings at the same depth,
    // the tenant soft-deleted earliest must reclaim first regardless
    // of UUID ordering. A row with a *later* `deleted_at` but a
    // numerically *smaller* uuid would beat an earlier-deleted peer
    // if `deleted_at` were not the secondary key.
    let early_late_uuid = TenantRetentionRow {
        id: Uuid::from_u128(0xFF),
        depth: 2,
        deleted_at: ts(100),
        retention_window: Duration::from_secs(60),
        claimed_by: Uuid::nil(),
    };
    let late_early_uuid = TenantRetentionRow {
        id: Uuid::from_u128(0x01),
        depth: 2,
        deleted_at: ts(200),
        retention_window: Duration::from_secs(60),
        claimed_by: Uuid::nil(),
    };
    let ordered = order_batch_leaf_first(vec![late_early_uuid.clone(), early_late_uuid.clone()]);
    assert_eq!(
        ordered.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![early_late_uuid.id, late_early_uuid.id],
        "row deleted at ts(100) must precede row deleted at ts(200) regardless of uuid"
    );
}

#[test]
fn hard_delete_result_tally_counts_correctly() {
    let mut r = HardDeleteResult::default();
    r.tally(&HardDeleteOutcome::Cleaned);
    r.tally(&HardDeleteOutcome::DeferredChildPresent);
    r.tally(&HardDeleteOutcome::CascadeTerminal);
    r.tally(&HardDeleteOutcome::NotEligible);
    // `IdpUnsupported` reports a successful DB teardown with an
    // `IdP` no-op; the contract folds it into `cleaned`. The
    // distinct metric label preserves the observability axis.
    r.tally(&HardDeleteOutcome::IdpUnsupported);
    assert_eq!(r.processed, 5);
    assert_eq!(r.cleaned, 2);
    assert_eq!(r.deferred, 1);
    assert_eq!(r.failed, 1);
}
