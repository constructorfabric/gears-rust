#[cfg(test)]
mod tests {
    use super::super::*;
    use simple_user_settings_sdk::models::{SimpleUserSettings, SimpleUserSettingsPatch};
    use uuid::Uuid;

    #[test]
    fn test_settings_to_dto_conversion() {
        let user_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();

        let settings = SimpleUserSettings {
            user_id,
            tenant_id,
            theme: Some("dark".to_owned()),
            language: Some("en".to_owned()),
        };

        let dto: dto::SimpleUserSettingsDto = settings.into();

        assert_eq!(dto.user_id, user_id);
        assert_eq!(dto.tenant_id, tenant_id);
        assert_eq!(dto.theme, Some("dark".to_owned()));
        assert_eq!(dto.language, Some("en".to_owned()));
    }

    #[test]
    fn test_update_request_to_dto() {
        let req = dto::UpdateSimpleUserSettingsRequest {
            theme: "light".to_owned(),
            language: "es".to_owned(),
        };

        assert_eq!(req.theme, "light".to_owned());
        assert_eq!(req.language, "es".to_owned());
    }

    #[test]
    fn test_patch_request_to_settings_patch() {
        let req = dto::PatchSimpleUserSettingsRequest {
            theme: Some("dark".to_owned()),
            language: None,
        };

        let patch: SimpleUserSettingsPatch = req.into();

        assert_eq!(patch.theme, Some("dark".to_owned()));
        assert_eq!(patch.language, None);
    }

    #[test]
    fn test_patch_request_empty() {
        let req = dto::PatchSimpleUserSettingsRequest {
            theme: None,
            language: None,
        };

        let patch: SimpleUserSettingsPatch = req.into();

        assert_eq!(patch.theme, None);
        assert_eq!(patch.language, None);
    }

    #[test]
    fn test_settings_dto_serialization() {
        let user_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();

        let dto = dto::SimpleUserSettingsDto {
            user_id,
            tenant_id,
            theme: Some("dark".to_owned()),
            language: Some("en".to_owned()),
        };

        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"theme\":\"dark\""));
        assert!(json.contains("\"language\":\"en\""));
        assert!(json.contains("\"user_id\"")); // snake_case
        assert!(json.contains("\"tenant_id\"")); // snake_case
    }

    #[test]
    fn test_update_request_deserialization() {
        let json = r#"{"theme":"light","language":"es"}"#;
        let req: dto::UpdateSimpleUserSettingsRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.theme, "light".to_owned());
        assert_eq!(req.language, "es".to_owned());
    }

    #[test]
    fn test_patch_request_deserialization_partial() {
        let json = r#"{"theme":"dark"}"#;
        let req: dto::PatchSimpleUserSettingsRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.theme, Some("dark".to_owned()));
        assert_eq!(req.language, None);
    }

    #[test]
    fn test_put_named_setting_request_takes_any_json_value() {
        let req: dto::PutNamedSettingRequest =
            serde_json::from_str(r#"{"value":{"wrap":true,"tab":4}}"#).unwrap();
        assert_eq!(req.value, serde_json::json!({"wrap": true, "tab": 4}));

        let req: dto::PutNamedSettingRequest = serde_json::from_str(r#"{"value":null}"#).unwrap();
        assert_eq!(req.value, serde_json::Value::Null);
    }

    #[test]
    fn test_named_settings_list_serialization() {
        let dto = dto::NamedSettingsListDto {
            settings: vec![dto::NamedSettingDto {
                key: "portal.projects.view".to_owned(),
                value: serde_json::json!("table"),
            }],
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"settings": [{"key": "portal.projects.view", "value": "table"}]})
        );
    }

    #[test]
    fn test_put_named_setting_request_requires_value() {
        assert!(serde_json::from_str::<dto::PutNamedSettingRequest>("{}").is_err());
        assert!(serde_json::from_str::<dto::PutNamedSettingRequest>(r#"{"val":1}"#).is_err());
    }

    /// The byte bound is not the only guard: `serde_json` refuses input nested
    /// deeper than its recursion limit (128) while the body is parsed, so a
    /// small but pathologically deep value never reaches the service.
    #[test]
    fn test_put_named_setting_request_refuses_pathological_nesting() {
        let nested =
            |depth: usize| format!(r#"{{"value":{}1{}}}"#, "[".repeat(depth), "]".repeat(depth));

        serde_json::from_str::<dto::PutNamedSettingRequest>(&nested(100))
            .expect("ordinary nesting is fine");
        assert!(
            serde_json::from_str::<dto::PutNamedSettingRequest>(&nested(1000)).is_err(),
            "1000 levels must be refused at parse time"
        );
    }
}
