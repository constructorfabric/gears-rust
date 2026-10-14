// Created: 2026-10-14 by Virtuozzo International GmbH
//! Tests for the declared `OData` surfaces of the SDK.

use toolkit_odata::filter::{FieldKind, FilterField};

use super::CategoryFilterField;

#[test]
fn the_category_surface_offers_sort_order_as_an_integer() {
    // The listing is ordered by the weight, and a client narrows on it with
    // `$filter=sort_order eq N`; both need the field declared, and as a number
    // so the comparison is not against text.
    let field = CategoryFilterField::from_name("sort_order");
    assert!(field.is_some(), "`sort_order` must be a declared field");
    assert_eq!(field.map(|f| f.kind()), Some(FieldKind::I64));
}

#[test]
fn the_category_surface_lists_exactly_its_declared_fields() {
    let names: Vec<&str> = CategoryFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .collect();
    assert_eq!(
        names,
        vec!["key", "name", "domain_affinity", "sort_order"],
        "widening the surface is deliberate and lands here"
    );
}
