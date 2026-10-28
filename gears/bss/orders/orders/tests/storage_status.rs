//! The storage status enums carry exactly the tokens the schema's CHECK constraints admit.
//!
//! `src/infra/storage/status.rs` has no crate dependencies, so it is included here unchanged
//! (as the D-184 UI tests include the maintenance capability module) and compared with the
//! generated `schema-inventory.json`, the frozen record of the migrations' constraints.
#![expect(
    clippy::expect_used,
    reason = "a malformed frozen inventory must fail loudly"
)]
#[allow(
    dead_code,
    reason = "only the token and set members are compared; the helpers serve the storage code"
)]
#[path = "../src/infra/storage/status.rs"]
mod status;

use status::{AttemptStatus, ControlStatus, RegistryStatus};

const INVENTORY: &str = include_str!("../src/infra/storage/migrations/schema-inventory.json");

/// The text of `constraint` on the inventory table `table`.
fn constraint(table: &str, constraint: &str) -> String {
    inventory_entry(table, "constraints", constraint)
}

/// The text of `constraint` in the `section` map of the inventory table `table`.
fn inventory_entry(table: &str, section: &str, constraint: &str) -> String {
    let inventory: serde_json::Value = serde_json::from_str(INVENTORY).expect("inventory JSON");
    let tables = inventory["tables"].as_array().expect("tables array");
    let entry = tables
        .iter()
        .find(|t| t["name"] == table)
        .unwrap_or_else(|| panic!("table {table} in the inventory"));
    entry[section][constraint]
        .as_str()
        .unwrap_or_else(|| panic!("constraint {constraint} on {table}"))
        .to_owned()
}

/// `status IN ('a','b',...)` as the inventory spells it.
fn status_in(tokens: impl IntoIterator<Item = &'static str>) -> String {
    let list: Vec<String> = tokens.into_iter().map(|t| format!("'{t}'")).collect();
    format!("status IN ({})", list.join(","))
}

#[test]
fn registry_status_matches_its_check() {
    assert_eq!(
        constraint("idempotency", "ck_bss_orders__idempotency__0"),
        format!(
            "CHECK ({})",
            status_in(RegistryStatus::ALL.map(RegistryStatus::token))
        )
    );
    for status in RegistryStatus::ALL {
        assert_eq!(RegistryStatus::from_token(status.token()), Some(status));
    }
}

#[test]
fn attempt_status_matches_its_checks() {
    assert_eq!(
        constraint("commercial_attempt", "ck_bss_orders__commercial_attempt__1"),
        format!(
            "CHECK ({})",
            status_in(AttemptStatus::ALL.map(AttemptStatus::token))
        )
    );
    // The lease/terminal CHECK splits the set into the unresolved half and the rest.
    let lease = constraint("commercial_attempt", "ck_bss_orders__commercial_attempt__6");
    let terminal = AttemptStatus::ALL
        .into_iter()
        .filter(|s| !s.is_unresolved())
        .map(AttemptStatus::token);
    assert!(lease.starts_with(&format!(
        "CHECK (({} AND lease_until IS NOT NULL",
        status_in(AttemptStatus::unresolved_tokens())
    )));
    assert!(lease.contains(&format!(
        "OR ({} AND lease_until IS NULL",
        status_in(terminal)
    )));
    for status in AttemptStatus::ALL {
        assert_eq!(AttemptStatus::from_token(status.token()), Some(status));
    }
}

#[test]
fn control_status_matches_its_checks() {
    assert_eq!(
        constraint(
            "fulfillment_control",
            "ck_bss_orders__fulfillment_control__1"
        ),
        format!(
            "CHECK ({})",
            status_in(ControlStatus::ALL.map(ControlStatus::token))
        )
    );
    let lease = constraint(
        "fulfillment_control",
        "ck_bss_orders__fulfillment_control__3",
    );
    let terminal = ControlStatus::ALL
        .into_iter()
        .filter(|s| !s.is_unresolved())
        .map(ControlStatus::token);
    assert!(lease.starts_with(&format!(
        "CHECK (({} AND terminal_at IS NULL",
        status_in(ControlStatus::unresolved_tokens())
    )));
    assert!(lease.contains(&format!(
        "OR ({} AND terminal_at IS NOT NULL",
        status_in(terminal)
    )));
    // One unresolved control per order.
    assert_eq!(
        inventory_entry(
            "fulfillment_control",
            "indexes",
            "uq_bss_orders__fulfillment_control__2"
        ),
        format!(
            "UNIQUE (order_id) WHERE {}",
            status_in(ControlStatus::unresolved_tokens())
        )
    );
    for status in ControlStatus::ALL {
        assert_eq!(ControlStatus::from_token(status.token()), Some(status));
    }
    // Progress order: prepared, awaiting, barrier_ready; terminal statuses have no rank.
    let ranks: Vec<_> = ControlStatus::ALL.map(ControlStatus::rank).into();
    assert_eq!(ranks, [Some(0), Some(1), Some(2), None, None, None]);
}
