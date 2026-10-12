use simple_user_settings_sdk::models::{NamedSetting, SimpleUserSettings, SimpleUserSettingsPatch};
use uuid::Uuid;

#[derive(Debug)]
#[toolkit_macros::api_dto(request, response)]
pub struct SimpleUserSettingsDto {
    #[schema(value_type = String)]
    pub user_id: Uuid,
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub theme: Option<String>,
    pub language: Option<String>,
}

impl From<SimpleUserSettings> for SimpleUserSettingsDto {
    fn from(settings: SimpleUserSettings) -> Self {
        Self {
            user_id: settings.user_id,
            tenant_id: settings.tenant_id,
            theme: settings.theme,
            language: settings.language,
        }
    }
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateSimpleUserSettingsRequest {
    pub theme: String,
    pub language: String,
}

#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct PatchSimpleUserSettingsRequest {
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

impl From<PatchSimpleUserSettingsRequest> for SimpleUserSettingsPatch {
    fn from(req: PatchSimpleUserSettingsRequest) -> Self {
        Self {
            theme: req.theme,
            language: req.language,
        }
    }
}

/// One named setting on the wire.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct NamedSettingDto {
    pub key: String,
    /// Any JSON value.
    pub value: serde_json::Value,
}

impl From<NamedSetting> for NamedSettingDto {
    fn from(setting: NamedSetting) -> Self {
        Self {
            key: setting.key,
            value: setting.value,
        }
    }
}

/// Every named setting the caller has, ordered by key.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct NamedSettingsListDto {
    pub settings: Vec<NamedSettingDto>,
}

/// Body of `PUT /named-settings/{key}`: the value to store under the key.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
pub struct PutNamedSettingRequest {
    /// Any JSON value.
    pub value: serde_json::Value,
}
