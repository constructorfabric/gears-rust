use super::{RECORD_BASE_SCHEMA, RECORD_BASE_TYPE, RECORD_TYPES};

fn schema(index: usize) -> serde_json::Value {
    serde_json::from_str(RECORD_TYPES[index].schema).expect("the schema file is JSON")
}

#[test]
fn every_schema_file_declares_the_type_it_is_registered_under() {
    for (index, record_type) in RECORD_TYPES.iter().enumerate() {
        assert_eq!(
            schema(index)["$id"],
            format!("gts://{}", record_type.type_id)
        );
    }
}

#[test]
fn only_the_base_is_abstract_and_every_other_type_derives_from_it() {
    assert_eq!(schema(0)["x-gts-abstract"], true);
    for (index, record_type) in RECORD_TYPES.iter().enumerate().skip(1) {
        let type_id = record_type.type_id;
        assert!(type_id.starts_with(RECORD_BASE_TYPE), "{type_id}");
        assert_eq!(
            schema(index)["allOf"][0]["$ref"],
            format!("gts://{RECORD_BASE_TYPE}"),
            "{type_id}"
        );
        assert_ne!(schema(index)["x-gts-abstract"], true, "{type_id}");
    }
}

#[test]
fn every_record_type_is_in_the_link_time_inventory() {
    let registered: Vec<&str> = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>()
        .map(|schema| schema.type_id)
        .collect();
    for record_type in RECORD_TYPES {
        assert!(
            registered.contains(&record_type.type_id),
            "{} is not submitted",
            record_type.type_id
        );
    }
}

#[test]
fn the_base_schema_constant_is_the_base_entry() {
    let base = RECORD_TYPES
        .iter()
        .find(|record_type| record_type.type_id == RECORD_BASE_TYPE)
        .expect("the base is registered");
    assert_eq!(base.schema, RECORD_BASE_SCHEMA);
}
