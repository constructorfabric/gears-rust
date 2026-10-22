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

#[test]
fn the_review_surface_is_browses_grammar_without_needs_review() {
    // Every row the review listing serves is flagged already, so the field
    // selects nothing there; it is refused as unknown rather than accepted
    // as a no-op, which the declared surface guarantees by leaving it out.
    use super::ReviewFilterField;
    let names: Vec<&str> = ReviewFilterField::FIELDS
        .iter()
        .map(FilterField::name)
        .collect();
    assert_eq!(
        names,
        vec!["key", "category_id"],
        "no `needs_review` on the review listing"
    );
    assert_eq!(ReviewFilterField::from_name("needs_review"), None);
    assert_eq!(ReviewFilterField::CategoryId.kind(), FieldKind::Uuid);
}
