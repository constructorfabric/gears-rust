use super::StoragePluginConfig;

#[test]
fn default_config_selects_the_platform_vendor_and_validates() {
    let cfg = StoragePluginConfig::default();
    assert_eq!(cfg.vendor, "constructorfabric");
    assert_eq!(cfg.priority, 100);
    assert_eq!(cfg.config_refresh(), std::time::Duration::from_secs(30));
    cfg.validate().expect("default config is valid");
}

#[test]
fn blank_vendor_is_rejected() {
    let cfg = StoragePluginConfig {
        vendor: " ".to_owned(),
        priority: 1,
        ..StoragePluginConfig::default()
    };
    let err = cfg.validate().expect_err("blank vendor rejected");
    assert!(err.to_string().contains("vendor"), "{err}");
}

#[test]
fn unknown_keys_are_rejected() {
    assert!(serde_json::from_str::<StoragePluginConfig>(r#"{ "dsn": "x" }"#).is_err());
}

#[test]
fn a_zero_config_refresh_is_rejected() {
    let cfg = StoragePluginConfig {
        config_refresh_secs: 0,
        ..StoragePluginConfig::default()
    };
    let err = cfg.validate().expect_err("zero refresh rejected");
    assert!(err.to_string().contains("config_refresh_secs"), "{err}");
}
