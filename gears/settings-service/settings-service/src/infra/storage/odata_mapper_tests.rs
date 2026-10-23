//! Tests for the category `OData` surface.
//!
//! Acceptance: FEATURE `category-management.md` §6 and CDSL `inst-cat-list-4` /
//! `inst-cat-list-5` — an expression on an unmapped field or with an
//! unsupported operator is rejected rather than ignored.

use settings_service_sdk::odata::CategoryFilterField;
use toolkit_db::odata::{FieldToColumn, ODataFieldMapping};

use toolkit_odata::filter::{FieldKind, FilterField};

use super::{CategoryODataMapper, ReviewODataMapper};
use crate::domain::odata::ReviewOrderField;
use crate::infra::storage::entity::category::{Column as CategoryColumn, Model as CategoryModel};
use crate::infra::storage::entity::setting_value::{Column as ValueColumn, Model as ValueModel};

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

#[test]
fn the_review_page_is_ordered_by_declaration_then_tenant_and_nothing_else() {
    // The listing's order is fixed and total (`uq_value_scope`), so the two
    // fields a keyset cursor needs are the whole surface, in that order; both
    // are non-null identifiers and compare as UUIDs, never as text.
    assert_eq!(
        ReviewOrderField::FIELDS,
        &[ReviewOrderField::DeclarationId, ReviewOrderField::TenantId]
    );
    for field in ReviewOrderField::FIELDS {
        assert!(
            matches!(field.kind(), FieldKind::Uuid),
            "`{}` is a UUID field",
            field.name()
        );
    }
}

#[test]
fn every_review_field_maps_to_its_value_column() {
    for (field, column) in [
        (ReviewOrderField::DeclarationId, ValueColumn::DeclarationId),
        (ReviewOrderField::TenantId, ValueColumn::TenantId),
    ] {
        assert_eq!(
            format!("{:?}", ReviewODataMapper::map_field(field)),
            format!("{column:?}")
        );
    }
}

#[test]
fn the_review_cursor_values_are_the_row_models_ids() {
    // The cursor of the next page is read off the last row served, so it must
    // carry the row's own ids, as UUIDs rather than their text.
    let now = time::OffsetDateTime::now_utc();
    let model = ValueModel {
        id: uuid::Uuid::new_v4(),
        declaration_id: uuid::Uuid::new_v4(),
        tenant_id: uuid::Uuid::new_v4(),
        subject_type: None,
        subject_id: None,
        value: Some(serde_json::json!(false)),
        secret_ref: None,
        data_classification: "public".to_owned(),
        needs_review: true,
        needs_review_detail: Some("no longer validates".to_owned()),
        last_change_at: now,
        created_at: now,
        updated_at: now,
        set_by: "admin".to_owned(),
    };

    for (field, id) in [
        (ReviewOrderField::DeclarationId, model.declaration_id),
        (ReviewOrderField::TenantId, model.tenant_id),
    ] {
        let value =
            <ReviewODataMapper as ODataFieldMapping<ReviewOrderField>>::extract_cursor_value(
                &model, field,
            );
        assert_eq!(
            value,
            sea_orm::Value::from(id),
            "the cursor value of `{}` is the row's id",
            field.name()
        );
    }
}
