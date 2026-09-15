// Created: 2026-09-06 by Constructor Tech
//! Tests for the composed setting-type schema.

use serde_json::json;
use settings_service_sdk::SettingKey;
use settings_service_sdk::gts::SETTING_TYPE_BASE;

use super::setting_type_schema;

#[test]
fn a_setting_type_derives_from_the_base_and_narrows_the_payload() {
    let key = SettingKey::contributed(
        "cf",
        "settings_demo",
        "network",
        "proxy_enabled",
        std::num::NonZeroU32::new(1).expect("non-zero"),
    )
    .expect("key");
    let schema = setting_type_schema(&key, "gts.cf.toolkit.settings.type_bool_flag.v1~");
    assert_eq!(schema["$id"], json!(format!("gts://{key}")));
    assert_eq!(
        schema["allOf"],
        json!([{ "$ref": format!("gts://{SETTING_TYPE_BASE}") }])
    );
    assert_eq!(
        schema["properties"]["payload"],
        json!({ "$ref": "gts://gts.cf.toolkit.settings.type_bool_flag.v1~" })
    );
}

#[test]
fn a_setting_type_carries_no_default() {
    // The Schema Default lives in the declaration alone; a `default` here would
    // be a second home the two could drift between.
    let key = SettingKey::compose("acme", "network", "enable_proxy").expect("key");
    let schema = setting_type_schema(&key, "gts.cf.toolkit.settings.type_bool_flag.v1~");
    assert!(schema.get("default").is_none());
    assert!(schema["properties"]["payload"].get("default").is_none());
}
