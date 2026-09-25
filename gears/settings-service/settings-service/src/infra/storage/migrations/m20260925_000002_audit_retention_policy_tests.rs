// Created: 2026-09-25 by Virtuozzo International GmbH
//! The minimum the policy table and the trigger floor are written against.

/// The migration's own source, read at compile time.
const SOURCE: &str = include_str!("m20260925_000002_audit_retention_policy.rs");

#[test]
fn the_policy_and_the_trigger_never_go_below_the_platform_minimum() {
    // The check on the row and the `GREATEST` in the trigger both write the
    // minimum out; the configuration refuses anything below it. If it moves, a
    // new migration moves these with it — this is what says so.
    let up = SOURCE.split("async fn down").next().expect("the up half");
    let minimum = crate::audit::MIN_RETENTION_DAYS;
    assert!(
        up.contains(&format!("CHECK (retention_days >= {minimum})")),
        "the row's check is the minimum"
    );
    assert!(
        up.contains(&format!(
            "GREATEST({minimum}, COALESCE(MAX(retention_days), {minimum}))"
        )),
        "the trigger's floor is the greater of the minimum and the row"
    );
}
