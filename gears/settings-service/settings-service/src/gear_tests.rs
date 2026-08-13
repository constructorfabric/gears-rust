// Created: 2026-08-12 by Constructor Tech
//! Tests for the gear scaffold's startup contract.
//!
//! `Gear::init` needs a live `GearCtx` (database capability, config provider),
//! so its happy path belongs to the integration suite. What is pinned here is
//! the part that must hold before any of that: the gear must not hand out
//! resources it has not acquired.

use super::SettingsService;

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_config() {
    // Returning a default here instead of an error is exactly the failure the
    // fail-closed bootstrap exists to prevent, one layer up.
    let gear = SettingsService::default();
    assert!(gear.config().is_err());
}

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_the_database() {
    let gear = SettingsService::default();
    assert!(gear.db().is_err());
}

#[test]
fn the_accessor_errors_name_the_gear() {
    // Startup failures are read in aggregated logs where the message may be the
    // only clue which gear produced it.
    let gear = SettingsService::default();
    let err = gear.config().expect_err("uninitialized");
    assert!(err.to_string().contains("settings-service"), "got `{err}`");
}

#[test]
fn the_gear_hands_its_migrations_to_the_capability() {
    // The capability is the only route by which ToolKit learns there is a schema
    // to apply. A harness that existed but was never handed over would leave the
    // gear starting cleanly against a database it had never migrated.
    use sea_orm_migration::MigratorTrait;
    use toolkit::DatabaseCapability;

    let gear = SettingsService::default();
    let handed_over: Vec<String> = gear
        .migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let harness: Vec<String> = crate::infra::storage::migrations::Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();

    assert!(!handed_over.is_empty());
    assert_eq!(
        handed_over, harness,
        "the capability must expose the harness itself, not a separate list"
    );
}

#[test]
fn an_uninitialized_gear_refuses_to_hand_out_the_enforcer() {
    // Handing back a permissive default here would turn every unenforced
    // handler into an allow. There is no default to hand back.
    let gear = SettingsService::default();
    assert!(gear.enforcer().is_err());
}
