//! The gear builds, exposes a migrator, and reports the `db` capability.

#[test]
fn gear_exposes_migrator_via_db_capability() {
    use toolkit::contracts::DatabaseCapability;
    // The gear constructs and its `db` capability exposes the registered
    // migration chain (non-empty; later phases push more onto it).
    let gear = bss_ledger::module::BssLedgerGear::default();
    assert!(!gear.migrations().is_empty());
}
