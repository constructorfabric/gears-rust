//! Infrastructure layer mapping from type-safe `FilterNode` to `SeaORM` Conditions.
//!
//! This gear is the ONLY place where we map from DTO-level filter fields to
//! `SeaORM` Column types. The API and domain layers work exclusively with `FilterNode`
//! and do not know about `SeaORM` Column enums.
//!
//! This gear provides the complete `OData` mapping including filtering, ordering,
//! and cursor extraction - all using the type-safe `FilterField` approach.

use sea_orm::Condition;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, ODataFieldMapping, filter_node_to_condition,
};
use toolkit_odata::filter::FilterNode;

use crate::infra::storage::entity::{
    Column, Entity, Model,
    address::{Column as AddressColumn, Entity as AddressEntity, Model as AddressModel},
    city::{Column as CityColumn, Entity as CityEntity, Model as CityModel},
};
use users_info_sdk::odata::{AddressFilterField, CityFilterField, UserFilterField};

/// Complete `OData` mapper for `users_info`.
///
/// This is the only users_info-specific code needed for `OData` operations.
/// It maps `UserFilterField` to database columns and extracts cursor values.
pub struct UserODataMapper;

impl FieldToColumn<UserFilterField> for UserODataMapper {
    type Column = Column;

    fn map_field(field: UserFilterField) -> Column {
        match field {
            UserFilterField::Id => Column::Id,
            UserFilterField::Email => Column::Email,
            UserFilterField::CreatedAt => Column::CreatedAt,
        }
    }
}

impl ODataFieldMapping<UserFilterField> for UserODataMapper {
    type Entity = Entity;

    fn extract_cursor_value(model: &Model, field: UserFilterField) -> sea_orm::Value {
        match field {
            UserFilterField::Id => sea_orm::Value::Uuid(Some(model.id)),
            UserFilterField::Email => sea_orm::Value::String(Some(model.email.clone())),
            UserFilterField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.created_at))
            }
        }
    }
}

/// Map a `FilterNode`<UserFilterField> to a `SeaORM` Condition.
///
/// This function is provided for compatibility but is no longer needed
/// if you use `paginate_odata` directly, which handles filtering internally.
///
/// # Arguments
///
/// * `filter` - The type-safe filter node from the API/domain layer
///
/// # Returns
///
/// A `SeaORM` Condition that can be applied to a query
pub fn filter_to_condition(filter: &FilterNode<UserFilterField>) -> Result<Condition, String> {
    filter_node_to_condition::<UserFilterField, UserODataMapper>(filter)
}

/// Complete `OData` mapper for cities.
pub struct CityODataMapper;

impl FieldToColumn<CityFilterField> for CityODataMapper {
    type Column = CityColumn;

    fn map_field(field: CityFilterField) -> CityColumn {
        match field {
            CityFilterField::Id => CityColumn::Id,
            CityFilterField::Name => CityColumn::Name,
            CityFilterField::Country => CityColumn::Country,
            CityFilterField::CreatedAt => CityColumn::CreatedAt,
        }
    }
}

impl ODataFieldMapping<CityFilterField> for CityODataMapper {
    type Entity = CityEntity;

    fn extract_cursor_value(model: &CityModel, field: CityFilterField) -> sea_orm::Value {
        match field {
            CityFilterField::Id => sea_orm::Value::Uuid(Some(model.id)),
            CityFilterField::Name => sea_orm::Value::String(Some(model.name.clone())),
            CityFilterField::Country => sea_orm::Value::String(Some(model.country.clone())),
            CityFilterField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.created_at))
            }
        }
    }
}

/// Complete `OData` mapper for addresses.
pub struct AddressODataMapper;

impl FieldToColumn<AddressFilterField> for AddressODataMapper {
    type Column = AddressColumn;

    fn map_field(field: AddressFilterField) -> AddressColumn {
        match field {
            AddressFilterField::Id => AddressColumn::Id,
            AddressFilterField::UserId => AddressColumn::UserId,
            AddressFilterField::CityId => AddressColumn::CityId,
            AddressFilterField::Street => AddressColumn::Street,
            AddressFilterField::PostalCode => AddressColumn::PostalCode,
            AddressFilterField::CreatedAt => AddressColumn::CreatedAt,
        }
    }
}

impl ODataFieldMapping<AddressFilterField> for AddressODataMapper {
    type Entity = AddressEntity;

    fn extract_cursor_value(model: &AddressModel, field: AddressFilterField) -> sea_orm::Value {
        match field {
            AddressFilterField::Id => sea_orm::Value::Uuid(Some(model.id)),
            AddressFilterField::UserId => sea_orm::Value::Uuid(Some(model.user_id)),
            AddressFilterField::CityId => sea_orm::Value::Uuid(Some(model.city_id)),
            AddressFilterField::Street => sea_orm::Value::String(Some(model.street.clone())),
            AddressFilterField::PostalCode => {
                sea_orm::Value::String(Some(model.postal_code.clone()))
            }
            AddressFilterField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.created_at))
            }
        }
    }
}
