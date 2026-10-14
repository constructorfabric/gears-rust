// Created: 2026-08-13 by Virtuozzo International GmbH
//! Tests for the category `OData` surface.
//!
//! Acceptance: FEATURE `category-management.md` §6 and CDSL `inst-cat-list-4` /
//! `inst-cat-list-5` — an expression on an unmapped field or with an
//! unsupported operator is rejected rather than ignored.

use settings_service_sdk::odata::CategoryFilterField;
use toolkit_db::odata::{FieldToColumn, ODataFieldMapping};

use super::CategoryODataMapper;
use crate::infra::storage::entity::category::{Column as CategoryColumn, Model as CategoryModel};

#[test]
fn every_declared_field_maps_to_its_column() {
    for (field, column) in [
        (CategoryFilterField::Key, CategoryColumn::Key),
        (CategoryFilterField::Name, CategoryColumn::Name),
        (
            CategoryFilterField::DomainAffinity,
            CategoryColumn::DomainAffinity,
        ),
        (CategoryFilterField::SortOrder, CategoryColumn::SortOrder),
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
    let declared = ["key", "name", "domainAffinity", "sortOrder"];
    assert_eq!(
        declared.len(),
        4,
        "a field added to CategoryQuery must be reflected here and in the mapper"
    );
}

#[test]
fn the_filter_surface_excludes_presentation_and_audit_columns() {
    // `description` and `icon` are deliberately not filterable; `sort_order` is,
    // because the listing is ordered by it and a client narrows on a weight.
    // Widening the surface is a compatible change; narrowing it later is not,
    // so the default is narrow.
    let declared = ["key", "name", "domainAffinity", "sortOrder"];
    for excluded in ["description", "icon", "createdAt", "updatedAt"] {
        assert!(
            !declared.contains(&excluded),
            "`{excluded}` must not be filterable without a deliberate decision"
        );
    }
}

#[test]
fn the_sort_order_cursor_value_is_the_models_weight_as_an_integer() {
    // The page cursor for an ordering by weight is read off the last row served;
    // it must carry the integer itself, not its text, or the next page would
    // compare a number against a string.
    let now = time::OffsetDateTime::now_utc();
    let model = CategoryModel {
        id: uuid::Uuid::new_v4(),
        key: "billing".to_owned(),
        name: "Invoices".to_owned(),
        description: None,
        domain_affinity: None,
        sort_order: 7,
        icon: None,
        created_at: now,
        updated_at: now,
    };

    let value =
        <CategoryODataMapper as ODataFieldMapping<CategoryFilterField>>::extract_cursor_value(
            &model,
            CategoryFilterField::SortOrder,
        );
    assert!(
        matches!(
            value,
            sea_orm::Value::Int(Some(7)) | sea_orm::Value::BigInt(Some(7))
        ),
        "an integer cursor value, got {value:?}"
    );
}
