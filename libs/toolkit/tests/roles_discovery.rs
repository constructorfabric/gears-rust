#![allow(clippy::unwrap_used, clippy::expect_used)]

//! `cpt-cf-adr-instance-addressable-discovery` section 1: a role-split gear's
//! closed role set is auto-discovered from its `#[toolkit::gear(roles = [...])]`
//! declaration via the `DeclaredRoles` inventory, so `OoP` bootstrap can enforce
//! the role / front-door split with no `main.rs` wiring.
//!
//! This test lives in its own binary so the process-global `inventory` set holds
//! exactly the gear declared here.

use anyhow::Result;
use async_trait::async_trait;
use toolkit::{contracts::Gear, gear};

#[derive(Default)]
#[gear(name = "evbk", roles = ["evbk", "evbk-ingest", "evbk-delivery"])]
struct RoleSplitGear;

#[async_trait]
impl Gear for RoleSplitGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}

#[test]
fn macro_emits_roles_const() {
    assert_eq!(
        RoleSplitGear::ROLES,
        &["evbk", "evbk-ingest", "evbk-delivery"]
    );
}

#[test]
fn declared_roles_are_discovered_from_inventory() {
    // The gear's declared set is discoverable without any `main.rs` forwarding.
    let roles = toolkit::registry::discovered_role_names();
    for expected in ["evbk", "evbk-ingest", "evbk-delivery"] {
        assert!(
            roles.iter().any(|r| r == expected),
            "discovered_role_names() should contain {expected:?}, got {roles:?}"
        );
    }
}
