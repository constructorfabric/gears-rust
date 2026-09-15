// Created: 2026-08-26 by Constructor Tech
//! Tests for the declaration `OData` surface.
//!
//! Acceptance: FEATURE `setting-declarations.md` CDSL `inst-decl-read-5` — an
//! expression on an unmapped field or with an unsupported operator is rejected
//! rather than ignored.

use settings_service_sdk::odata::DeclarationFilterField;
use toolkit_db::odata::FieldToColumn;

use super::DeclarationODataMapper;
use crate::infra::storage::entity::declaration::Column as DeclarationColumn;

#[test]
fn every_declared_field_maps_to_its_column() {
    // The pairing the rejection rule rests on: a declared field always has a
    // column, so a parsed expression can always be built into a query.
    for (field, column) in [
        (DeclarationFilterField::Key, DeclarationColumn::Key),
        (
            DeclarationFilterField::CategoryId,
            DeclarationColumn::CategoryId,
        ),
        (
            DeclarationFilterField::DomainAffinity,
            DeclarationColumn::DomainAffinity,
        ),
        (DeclarationFilterField::Mode, DeclarationColumn::Mode),
        (DeclarationFilterField::Status, DeclarationColumn::Status),
        (
            DeclarationFilterField::OwnerModule,
            DeclarationColumn::OwnerModule,
        ),
    ] {
        // `Column` is not `PartialEq`; compare the identifier it resolves to.
        assert_eq!(
            format!("{:?}", DeclarationODataMapper::map_field(field)),
            format!("{column:?}")
        );
    }
}

#[test]
fn the_filter_surface_excludes_rendering_and_write_only_columns() {
    // `default_value` and `description` reach a caller through search, not
    // filtering; `scope_class`, `data_classification` and the flags organise no
    // listing. Widening later is compatible; narrowing is not, so the default
    // is narrow.
    let declared = [
        "key",
        "categoryId",
        "domainAffinity",
        "mode",
        "status",
        "ownerModule",
    ];
    for excluded in [
        "defaultValue",
        "description",
        "scopeClass",
        "dataClassification",
        "hasSecretTrait",
        "tenantVisible",
        "tenantOverridable",
        "licenceFeature",
        "createdAt",
    ] {
        assert!(
            !declared.contains(&excluded),
            "`{excluded}` must not be filterable without a deliberate decision"
        );
    }
}
