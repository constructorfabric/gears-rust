// Created: 2026-08-13 by Constructor Tech
//! Mapping the declared category query surface onto columns.
//!
//! The SDK declares *which* fields are filterable; this says *where* each one
//! lives. Both halves are needed for the rejection rule to hold: a field with
//! no variant cannot be parsed, and a variant with no column would not compile.

use toolkit_db::odata::{FieldToColumn, ODataFieldMapping};

use settings_service_sdk::odata::CategoryFilterField;

use crate::infra::storage::entity::category::{
    Column as CategoryColumn, Entity as CategoryEntity, Model as CategoryModel,
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
        }
    }
}

#[cfg(test)]
#[path = "odata_mapper_tests.rs"]
mod odata_mapper_tests;
