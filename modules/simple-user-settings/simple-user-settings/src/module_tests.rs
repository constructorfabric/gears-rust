use super::SettingsModule;

#[test]
fn test_settings_module_default() {
    let module = SettingsModule::default();
    assert!(module.service.get().is_none());
}

#[test]
fn test_settings_module_multiple_defaults_empty_service() {
    let module = SettingsModule::default();
    let other = SettingsModule::default();
    assert!(other.service.get().is_none());
    assert!(module.service.get().is_none());
}
