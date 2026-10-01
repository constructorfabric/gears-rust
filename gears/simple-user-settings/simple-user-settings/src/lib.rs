//! Settings Gear Implementation
//!
//! The public API is defined in `simple_user_settings-sdk` and re-exported here.

pub use simple_user_settings_sdk::{
    NamedSetting, NamedSettingsClientV1, SimpleUserSettings, SimpleUserSettingsClientV1,
    SimpleUserSettingsPatch, SimpleUserSettingsUpdate,
};

pub mod gear;
pub use gear::SettingsGear;

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
