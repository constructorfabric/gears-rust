use super::*;

#[test]
fn test_default_config() {
    let cfg = TypesRegistryConfig::default();
    assert_eq!(cfg.entity_id_fields, vec!["$id", "gtsId", "id"]);
    assert_eq!(cfg.schema_id_fields, vec!["$schema", "gtsTid", "type"]);
}

#[test]
fn test_to_gts_config() {
    let cfg = TypesRegistryConfig::default();
    let gts_cfg = cfg.to_gts_config();
    assert_eq!(gts_cfg.entity_id_fields, cfg.entity_id_fields);
    assert_eq!(gts_cfg.schema_id_fields, cfg.schema_id_fields);
}
