//! The pricing feature must pull products into the server's actual registrator set.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(feature = "bss-pricing")]

#[path = "../src/registered_gears.rs"]
mod registered_gears;

#[test]
fn pricing_feature_registers_both_gears() {
    let registry = toolkit::registry::GearRegistry::discover_and_build().unwrap();
    assert!(registry.get_gear("bss-pricing").is_some());
    assert!(registry.get_gear("bss-products").is_some());
}
