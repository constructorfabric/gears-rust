// Created: 2026-09-24 by Virtuozzo International GmbH
//! The retention floor the trigger enforces.

/// The migration's own source, read at compile time.
const SOURCE: &str = include_str!("m20260924_000002_audit_records_retention_floor.rs");

#[test]
fn the_trigger_floor_is_the_platform_minimum_retention() {
    // The migration writes the floor out; the configuration refuses anything
    // below `MIN_RETENTION_DAYS`. If the minimum moves, a new migration must
    // move the trigger with it — this is what says so.
    let up = SOURCE.split("async fn down").next().expect("the up half");
    let floor = format!("interval '{} days'", crate::audit::MIN_RETENTION_DAYS);
    assert!(up.contains(&floor), "the trigger's floor is `{floor}`");
}
