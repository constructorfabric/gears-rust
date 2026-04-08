use tracing::info;

use crate::api::rest::dto::{
    AddressDto, CityDto, CreateCityReq, PutAddressReq, UpdateCityReq, UpdateUserReq, UserDto,
    UserEvent, UserFullDto,
};

use modkit::api::prelude::*;
use modkit::api::select::{apply_select, page_to_projected_json};

use modkit::SseBroadcaster;

use modkit_security::SecurityContext;

mod addresses;
mod cities;
mod events;
mod users;

// ==================== User Handlers ====================

pub(crate) use users::create_user;
pub(crate) use users::delete_user;
pub(crate) use users::get_user;
pub(crate) use users::list_users;
pub(crate) use users::update_user;

// ==================== Event Handlers (SSE) ====================

pub(crate) use events::users_events;

// ==================== City Handlers ====================

pub(crate) use cities::create_city;
pub(crate) use cities::delete_city;
pub(crate) use cities::get_city;
pub(crate) use cities::list_cities;
pub(crate) use cities::update_city;

// ==================== Address Handlers ====================

pub(crate) use addresses::delete_user_address;
pub(crate) use addresses::get_user_address;
pub(crate) use addresses::put_user_address;
