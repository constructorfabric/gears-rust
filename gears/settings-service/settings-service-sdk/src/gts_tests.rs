// Created: 2026-08-12 by Constructor Tech
//! Tests for the GTS resource-type vocabulary.

use super::{CATEGORY_SCHEMA, DECLARATION_SCHEMA, Resource, VALUE_SCHEMA};

#[test]
fn every_constant_round_trips_through_the_typed_view() {
    for wire in [DECLARATION_SCHEMA, VALUE_SCHEMA, CATEGORY_SCHEMA] {
        let typed = Resource::from_wire(wire);
        assert_eq!(
            typed.as_wire(),
            wire,
            "`{wire}` must survive a round trip through the typed view"
        );
        assert!(
            !matches!(typed, Resource::Unknown(_)),
            "`{wire}` is a modelled resource and must not fall through to Unknown"
        );
    }
}

#[test]
fn an_unmodelled_resource_is_preserved_not_discarded() {
    // A resource type this SDK does not know must survive intact, so a consumer
    // can still report it and a later version can model it without data loss.
    let typed = Resource::from_wire("gts.cf.toolkit.settings.apply_bundle.v1~");
    assert_eq!(
        typed,
        Resource::Unknown("gts.cf.toolkit.settings.apply_bundle.v1~".to_owned())
    );
    assert_eq!(typed.as_wire(), "gts.cf.toolkit.settings.apply_bundle.v1~");
}

#[test]
fn declaration_and_value_are_different_resources() {
    // The projection leans on this distinction to tell "no such setting" from
    // "no credential configured"; if these ever collapsed, that would break.
    assert_ne!(
        Resource::from_wire(DECLARATION_SCHEMA),
        Resource::from_wire(VALUE_SCHEMA)
    );
}
