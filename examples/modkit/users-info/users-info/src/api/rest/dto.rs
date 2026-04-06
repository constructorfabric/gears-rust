/// REST DTO for user representation with serde/utoipa
use time::OffsetDateTime;
use users_info_sdk::{Address, City, NewAddress, NewCity, NewUser, User, UserFull, UserPatch};
use uuid::Uuid;

/// REST DTO for user representation with serde/utoipa
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct UserDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub display_name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// REST DTO for creating a new user
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct CreateUserReq {
    /// Optional ID for the user. If not provided, a UUID v7 will be generated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub email: String,
    pub display_name: String,
}

/// REST DTO for updating a user (partial)
#[derive(Debug, Clone, Default)]
#[modkit_macros::api_dto(request)]
pub struct UpdateUserReq {
    pub email: Option<String>,
    pub display_name: Option<String>,
}

/// REST DTO for aggregated user response with related entities
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct UserFullDto {
    pub user: UserDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<AddressDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<CityDto>,
}

// Conversion implementations between REST DTOs and contract models
impl From<User> for UserDto {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            tenant_id: user.tenant_id,
            email: user.email,
            display_name: user.display_name,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
}

impl From<CreateUserReq> for NewUser {
    fn from(req: CreateUserReq) -> Self {
        Self {
            id: req.id,
            tenant_id: req.tenant_id,
            email: req.email,
            display_name: req.display_name,
        }
    }
}

impl From<UpdateUserReq> for UserPatch {
    fn from(req: UpdateUserReq) -> Self {
        Self {
            email: req.email,
            display_name: req.display_name,
        }
    }
}

impl From<UserFull> for UserFullDto {
    fn from(user_full: UserFull) -> Self {
        Self {
            user: UserDto::from(user_full.user),
            address: user_full.address.map(AddressDto::from),
            city: user_full.city.map(CityDto::from),
        }
    }
}

// ==================== City DTOs ====================

/// REST DTO for city representation
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct CityDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub country: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// REST DTO for creating a new city
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct CreateCityReq {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub name: String,
    pub country: String,
}

/// REST DTO for updating a city (partial)
#[derive(Debug, Clone, Default)]
#[modkit_macros::api_dto(request)]
pub struct UpdateCityReq {
    pub name: Option<String>,
    pub country: Option<String>,
}

impl From<City> for CityDto {
    fn from(city: City) -> Self {
        Self {
            id: city.id,
            tenant_id: city.tenant_id,
            name: city.name,
            country: city.country,
            created_at: city.created_at,
            updated_at: city.updated_at,
        }
    }
}

impl From<CreateCityReq> for NewCity {
    fn from(req: CreateCityReq) -> Self {
        Self {
            id: req.id,
            tenant_id: req.tenant_id,
            name: req.name,
            country: req.country,
        }
    }
}

impl From<UpdateCityReq> for users_info_sdk::CityPatch {
    fn from(req: UpdateCityReq) -> Self {
        Self {
            name: req.name,
            country: req.country,
        }
    }
}

// ==================== Address DTOs ====================

/// REST DTO for address representation
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
pub struct AddressDto {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub city_id: Uuid,
    pub street: String,
    pub postal_code: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// REST DTO for creating/upserting an address
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct PutAddressReq {
    pub city_id: Uuid,
    pub street: String,
    pub postal_code: String,
}

impl From<Address> for AddressDto {
    fn from(address: Address) -> Self {
        Self {
            id: address.id,
            tenant_id: address.tenant_id,
            user_id: address.user_id,
            city_id: address.city_id,
            street: address.street,
            postal_code: address.postal_code,
            created_at: address.created_at,
            updated_at: address.updated_at,
        }
    }
}

impl PutAddressReq {
    #[must_use]
    pub fn into_new_address(self, user_id: Uuid) -> NewAddress {
        NewAddress {
            id: None,
            tenant_id: Uuid::nil(),
            user_id,
            city_id: self.city_id,
            street: self.street,
            postal_code: self.postal_code,
        }
    }
}

/// Transport-level SSE payload.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request, response)]
#[schema(title = "UserEvent", description = "Server-sent user event")]
pub struct UserEvent {
    pub kind: String,
    pub id: Uuid,
    #[schema(format = "date-time")]
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

impl From<&crate::domain::events::UserDomainEvent> for UserEvent {
    fn from(e: &crate::domain::events::UserDomainEvent) -> Self {
        use crate::domain::events::UserDomainEvent::{Created, Deleted, Updated};
        match e {
            Created { id, at } => Self {
                kind: "created".into(),
                id: *id,
                at: *at,
            },
            Updated { id, at } => Self {
                kind: "updated".into(),
                id: *id,
                at: *at,
            },
            Deleted { id, at } => Self {
                kind: "deleted".into(),
                id: *id,
                at: *at,
            },
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "dto_tests.rs"]
mod tests;
