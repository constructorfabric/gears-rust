//! Guards the surface a **gear** compiles.
//!
//! Pricing's `FixtureGate` takes this crate with `default-features = false`: it
//! asks the registry whether a `(kind, variant)` pair is green and needs nothing
//! else. That narrow build is easy to break without noticing, because every
//! other build in the workspace turns the `corpus` feature on — so this test
//! compiles the narrow surface on purpose and exercises exactly what the gate
//! does.
//!
//! Run: `cargo test -p cf-gears-bss-fixtures --no-default-features --test production_surface`

use bss_fixtures::{ModelKind, Registry, Variant};

const SAMPLE: &str = r#"
[[variants]]
kind    = "graduated"
variant = "model_kind"
oracle  = true
publish = true
rating  = false
"#;

#[test]
fn the_gate_answers_from_the_registry_alone() {
    let reg: Registry = toml::from_str(SAMPLE).expect("registry parses");

    assert!(reg.gate_open_for(ModelKind::Graduated, Variant::ModelKind));
    assert!(!reg.gate_open_for(ModelKind::Package, Variant::ModelKind));
    // The variant axis is part of the narrow surface too: a kind green on its
    // own fixture is not thereby green on a cross-cutting one.
    assert!(!reg.gate_open_for(ModelKind::Graduated, Variant::LevelAggregation));
}

#[test]
fn an_empty_registry_opens_nothing() {
    let reg = Registry::default();

    for kind in ModelKind::ALL {
        for variant in Variant::ALL {
            assert!(
                !reg.gate_open_for(kind, variant),
                "{kind:?}/{} must not be open",
                variant.wire()
            );
        }
    }
}
