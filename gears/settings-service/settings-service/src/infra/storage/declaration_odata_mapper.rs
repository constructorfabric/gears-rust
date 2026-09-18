// Created: 2026-08-26 by Constructor Tech
//! Mapping the declared declaration query surface onto columns.
//!
//! The SDK declares *which* fields are filterable; this says *where* each one
//! lives. Both halves are needed for the rejection rule to hold: a field with no
//! variant cannot be parsed, and a variant with no column would not compile.

use toolkit_db::odata::{FieldToColumn, ODataFieldMapping};

use settings_service_sdk::odata::DeclarationFilterField;

use crate::infra::storage::entity::declaration::{
    Column as DeclarationColumn, Entity as DeclarationEntity, Model as DeclarationModel,
};

/// `OData` mapper for declarations.
pub struct DeclarationODataMapper;

impl FieldToColumn<DeclarationFilterField> for DeclarationODataMapper {
    type Column = DeclarationColumn;

    fn map_field(field: DeclarationFilterField) -> DeclarationColumn {
        // Exhaustive with no wildcard: a field added to the declared surface
        // must be given a column here or this stops compiling, rather than
        // falling through to something arbitrary.
        match field {
            DeclarationFilterField::Key => DeclarationColumn::Key,
            DeclarationFilterField::CategoryId => DeclarationColumn::CategoryId,
            DeclarationFilterField::DomainAffinity => DeclarationColumn::DomainAffinity,
            DeclarationFilterField::Mode => DeclarationColumn::Mode,
            DeclarationFilterField::Status => DeclarationColumn::Status,
            DeclarationFilterField::OwnerModule => DeclarationColumn::OwnerModule,
        }
    }
}

impl ODataFieldMapping<DeclarationFilterField> for DeclarationODataMapper {
    type Entity = DeclarationEntity;

    fn extract_cursor_value(
        model: &DeclarationModel,
        field: DeclarationFilterField,
    ) -> sea_orm::Value {
        // Read from the same model the page returned, so a cursor always
        // describes a row that was actually served.
        match field {
            DeclarationFilterField::Key => model.key.clone().into(),
            DeclarationFilterField::CategoryId => model.category_id.into(),
            DeclarationFilterField::DomainAffinity => model.domain_affinity.clone().into(),
            DeclarationFilterField::Mode => model.mode.clone().into(),
            DeclarationFilterField::Status => model.status.clone().into(),
            DeclarationFilterField::OwnerModule => model.owner_module.clone().into(),
        }
    }
}

#[cfg(test)]
#[path = "declaration_odata_mapper_tests.rs"]
mod declaration_odata_mapper_tests;
