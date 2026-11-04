//! Mapping the declared category query surface onto columns.
//!
//! The SDK declares *which* fields are filterable; this says *where* each one
//! lives. Both halves are needed for the rejection rule to hold: a field with
//! no variant cannot be parsed, and a variant with no column would not compile.

use toolkit_db::odata::{FieldToColumn, ODataFieldMapping};

use settings_service_sdk::odata::CategoryFilterField;

use crate::domain::odata::ReviewOrderField;
use crate::infra::storage::entity::category::{
    Column as CategoryColumn, Entity as CategoryEntity, Model as CategoryModel,
};
use crate::infra::storage::entity::setting_value::{
    Column as ValueColumn, Entity as ValueEntity, Model as ValueModel,
};

/// `OData` mapper for categories.
pub struct CategoryODataMapper;

impl FieldToColumn<CategoryFilterField> for CategoryODataMapper {
    type Column = CategoryColumn;

    fn map_field(field: CategoryFilterField) -> CategoryColumn {
        // Exhaustive with no wildcard: a field added to the declared surface
        // must be given a column here or this stops compiling, rather than
        // falling through to something arbitrary.
        match field {
            CategoryFilterField::Key => CategoryColumn::Key,
            CategoryFilterField::Name => CategoryColumn::Name,
            CategoryFilterField::DomainAffinity => CategoryColumn::DomainAffinity,
            CategoryFilterField::SortOrder => CategoryColumn::SortOrder,
        }
    }
}

impl ODataFieldMapping<CategoryFilterField> for CategoryODataMapper {
    type Entity = CategoryEntity;

    fn extract_cursor_value(model: &CategoryModel, field: CategoryFilterField) -> sea_orm::Value {
        // Read from the same model the page returned, so a cursor always
        // describes a row that was actually served.
        match field {
            CategoryFilterField::Key => model.key.clone().into(),
            CategoryFilterField::Name => model.name.clone().into(),
            CategoryFilterField::DomainAffinity => model.domain_affinity.clone().into(),
            CategoryFilterField::SortOrder => model.sort_order.into(),
        }
    }
}

/// `OData` mapper for the review listing's keyset: the value row's declaration
/// and tenant.
pub struct ReviewODataMapper;

impl FieldToColumn<ReviewOrderField> for ReviewODataMapper {
    type Column = ValueColumn;

    fn map_field(field: ReviewOrderField) -> ValueColumn {
        match field {
            ReviewOrderField::DeclarationId => ValueColumn::DeclarationId,
            ReviewOrderField::TenantId => ValueColumn::TenantId,
        }
    }
}

impl ODataFieldMapping<ReviewOrderField> for ReviewODataMapper {
    type Entity = ValueEntity;

    fn extract_cursor_value(model: &ValueModel, field: ReviewOrderField) -> sea_orm::Value {
        match field {
            ReviewOrderField::DeclarationId => model.declaration_id.into(),
            ReviewOrderField::TenantId => model.tenant_id.into(),
        }
    }
}

#[cfg(test)]
#[path = "odata_mapper_tests.rs"]
mod odata_mapper_tests;
