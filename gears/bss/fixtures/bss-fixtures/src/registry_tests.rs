use super::*;
use crate::kinds::ModelKind;
use crate::variant::Variant;

const SAMPLE: &str = r#"
[[variants]]
kind    = "graduated"
variant = "model_kind"
oracle  = true
publish = true
rating  = false

[[variants]]
kind    = "graduated"
variant = "level_aggregation"
oracle  = true
publish = false
rating  = false

[[variants]]
kind    = "volume"
variant = "model_kind"
oracle  = true
publish = false
rating  = false
"#;

#[test]
fn the_gate_opens_only_on_oracle_and_publish() {
    let reg: Registry = toml::from_str(SAMPLE).expect("registry parses");

    // The `rating` flag is not part of the gate: rating has no code yet, and
    // requiring it would block every publish at launch.
    assert!(reg.gate_open_for(ModelKind::Graduated, Variant::ModelKind));
    // Publish half not yet earned.
    assert!(!reg.gate_open_for(ModelKind::Volume, Variant::ModelKind));
    // Never registered at all — an absent kind is never open.
    assert!(!reg.gate_open_for(ModelKind::Package, Variant::ModelKind));
}

#[test]
fn a_row_is_keyed_by_kind_and_variant_not_by_kind_alone() {
    // The finding, at the level the registry can state it. `graduated` is green
    // on its own fixture and unearned on the level-aggregation one, and those
    // two answers must not collapse into one — a `peak` row publishing on the
    // strength of a `sum` row's fixture is exactly what keying by kind alone did.
    let reg: Registry = toml::from_str(SAMPLE).expect("registry parses");

    assert!(reg.gate_open_for(ModelKind::Graduated, Variant::ModelKind));
    assert!(!reg.gate_open_for(ModelKind::Graduated, Variant::LevelAggregation));
    // Not registered under this kind at all, which is also never open.
    assert!(!reg.gate_open_for(ModelKind::Graduated, Variant::SupersessionContinuity));
    assert!(!reg.gate_open_for(ModelKind::Graduated, Variant::Reserved));
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

// Reading the committed file needs the corpus layout, which is the feature's
// business; the gate itself never does this.
#[cfg(feature = "corpus")]
#[test]
fn the_committed_registry_parses() {
    Registry::load(&crate::Corpus::corpus_root().join("registry.toml"))
        .expect("committed registry must parse");
}
