use super::*;

/// Strip the `gts://` URI prefix that schema `$id` fields use.
fn strip_gts_uri(id: &str) -> &str {
    id.strip_prefix("gts://").unwrap_or(id)
}

#[test]
fn catalog_returns_exactly_21_entities() {
    let entities = oagw_gts_entities();
    assert_eq!(
        entities.len(),
        21,
        "expected 21 entities (7 schemas + 14 instances)"
    );
}

#[test]
fn all_entities_have_valid_id_fields() {
    for entity in &oagw_gts_entities() {
        let raw_id = entity["$id"]
            .as_str()
            .unwrap_or_else(|| panic!("entity missing $id: {entity}"));
        let id = strip_gts_uri(raw_id);
        assert!(
            id.starts_with("gts."),
            "GTS ID should start with 'gts.': {id}"
        );
    }
}

#[test]
fn schemas_use_gts_uri_prefix() {
    for entity in &oagw_gts_entities() {
        if entity.get("$schema").is_some() {
            let id = entity["$id"].as_str().unwrap();
            assert!(
                id.starts_with("gts://"),
                "Schema $id must use gts:// URI format: {id}"
            );
        }
    }
}

#[test]
fn seven_schemas_and_thirteen_instances() {
    let entities = oagw_gts_entities();
    let schemas: Vec<_> = entities
        .iter()
        .filter(|e| e.get("$schema").is_some())
        .collect();
    let instances: Vec<_> = entities
        .iter()
        .filter(|e| e.get("$schema").is_none())
        .collect();

    assert_eq!(schemas.len(), 7, "expected 7 schemas");
    assert_eq!(instances.len(), 14, "expected 14 instances");
}

#[test]
fn all_gts_ids_have_valid_segment_format() {
    for entity in &oagw_gts_entities() {
        let raw_id = entity["$id"].as_str().unwrap();
        let id = strip_gts_uri(raw_id);
        // Validate via the gts crate
        gts::GtsID::new(id).unwrap_or_else(|e| panic!("invalid GTS ID '{id}': {e}"));
    }
}
