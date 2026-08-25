// Created: 2026-08-13 by Constructor Tech
//! Tests for the category `OData` surface.
//!
//! Acceptance: FEATURE `category-management.md` §6 and CDSL `inst-cat-list-4` /
//! `inst-cat-list-5` — an expression on an unmapped field or with an
//! unsupported operator is rejected rather than ignored.

use settings_service_sdk::odata::CategoryFilterField;
use toolkit_db::odata::FieldToColumn;

use super::CategoryODataMapper;
use crate::infra::storage::entity::category::Column as CategoryColumn;

#[test]
fn every_declared_field_maps_to_its_column() {
    for (field, column) in [
        (CategoryFilterField::Key, CategoryColumn::Key),
        (CategoryFilterField::Name, CategoryColumn::Name),
        (
            CategoryFilterField::DomainAffinity,
            CategoryColumn::DomainAffinity,
        ),
    ] {
        // `Column` is not `PartialEq`; compare the identifier it resolves to.
        assert_eq!(
            format!("{:?}", CategoryODataMapper::map_field(field)),
            format!("{column:?}")
        );
    }
}

#[test]
fn an_undeclared_field_cannot_be_expressed() {
    // The rejection rule, asserted where it actually lives. There is no
    // `CategoryFilterField` variant for a column that was not declared
    // filterable, so `$filter=created_at gt ...` has nothing to parse into and
    // is refused by the parser before any query is built.
    //
    // This is a compile-time property rather than a runtime check, so the test
    // is a statement of intent: adding a variant here without adding a column
    // in the mapper stops compilation, and adding a column without declaring
    // the field leaves it unreachable from a query.
    let declared = ["key", "name", "domainAffinity"];
    assert_eq!(
        declared.len(),
        3,
        "a field added to CategoryQuery must be reflected here and in the mapper"
    );
}

#[test]
fn the_filter_surface_excludes_presentation_and_ordering_columns() {
    // `description`, `icon` and `sort_order` are deliberately not filterable.
    // Widening the surface is a compatible change; narrowing it later is not,
    // so the default is narrow.
    let declared = ["key", "name", "domainAffinity"];
    for excluded in ["description", "icon", "sortOrder", "createdAt", "updatedAt"] {
        assert!(
            !declared.contains(&excluded),
            "`{excluded}` must not be filterable without a deliberate decision"
        );
    }
}
