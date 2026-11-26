use std::path::Path;

use gts::GtsSchema;
use serde_json::{Value, json};

use super::{
    QUOTA_METRIC_TYPE_ID, QUOTA_SUBJECT_TYPE_ID, QUOTA_TENANT_SUBJECT_TYPE_ID, QuotaMetricV1,
    QuotaResolverPluginSpecV1, QuotaSubjectV1,
};

fn documented(file: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/schemas")
        .join(file);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("documented schema is JSON")
}

fn declared(type_id: &str) -> Value {
    let entry = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
        .into_iter()
        .find(|entry| entry.type_id == type_id)
        .unwrap_or_else(|| panic!("{type_id} is not in the inventory"));
    serde_json::from_str(&(entry.schema_fn)()).expect("declared schema is JSON")
}

#[test]
fn type_ids_are_canonical() {
    assert_eq!(QuotaSubjectV1::TYPE_ID, QUOTA_SUBJECT_TYPE_ID);
    assert_eq!(QuotaMetricV1::<()>::TYPE_ID, QUOTA_METRIC_TYPE_ID);
    assert_eq!(QUOTA_SUBJECT_TYPE_ID, "gts.cf.core.quota.subj.v1~");
    assert_eq!(
        QUOTA_TENANT_SUBJECT_TYPE_ID,
        "gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~"
    );
    assert_eq!(QUOTA_METRIC_TYPE_ID, "gts.cf.core.quota.metric.v1~");
    assert_eq!(
        QuotaResolverPluginSpecV1::TYPE_ID,
        "gts.cf.toolkit.plugins.plugin.v1~cf.core.quota_resolver.plugin.v1~"
    );
}

#[test]
fn the_subject_base_matches_the_documented_schema() {
    assert_eq!(
        declared(QUOTA_SUBJECT_TYPE_ID),
        documented("gts.cf.core.quota.subj.v1~.schema.json")
    );
}

#[test]
fn the_tenant_subject_type_matches_the_documented_schema() {
    assert_eq!(
        declared(QUOTA_TENANT_SUBJECT_TYPE_ID),
        documented("gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~.schema.json")
    );
}

// The macro emits the `metadata` slot of a base as a bare `{"type": "object"}`
// and has no way to add a description to it.
#[test]
fn the_metric_base_matches_the_documented_schema_but_for_the_metadata_description() {
    let mut documented = documented("gts.cf.core.quota.metric.v1~.schema.json");
    documented["properties"]["metadata"]
        .as_object_mut()
        .expect("metadata is an object schema")
        .remove("description");
    assert_eq!(declared(QUOTA_METRIC_TYPE_ID), documented);
}

#[test]
fn the_metric_base_declares_its_trait_defaults() {
    let traits = &declared(QUOTA_METRIC_TYPE_ID)["x-gts-traits-schema"];
    assert_eq!(traits["required"], json!(["kind"]));
    assert_eq!(
        traits["properties"]["admitted_subjects"]["default"],
        json!([])
    );
    assert_eq!(
        traits["properties"]["enforcement"]["default"],
        json!("quota_gated")
    );
}

#[test]
fn the_bases_are_abstract_and_the_tenant_type_is_final() {
    for type_id in [QUOTA_SUBJECT_TYPE_ID, QUOTA_METRIC_TYPE_ID] {
        assert_eq!(
            declared(type_id)["x-gts-abstract"],
            json!(true),
            "{type_id}"
        );
    }
    assert_eq!(
        declared(QUOTA_TENANT_SUBJECT_TYPE_ID)["x-gts-final"],
        json!(true)
    );
}

#[test]
fn the_crate_collector_holds_every_declaration() {
    let ids: Vec<String> = crate::gts_declarations()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    for type_id in [
        QUOTA_SUBJECT_TYPE_ID,
        QUOTA_TENANT_SUBJECT_TYPE_ID,
        QUOTA_METRIC_TYPE_ID,
        QuotaResolverPluginSpecV1::TYPE_ID,
    ] {
        assert!(ids.iter().any(|id| id == type_id), "{type_id} in {ids:?}");
    }
}
