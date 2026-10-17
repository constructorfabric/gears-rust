//! Tests for the RG type schemas declared in [`crate::gts`].

use super::{ResourceGroupTypeV1, TENANT_RG_TYPE_PATH, TenantResourceGroupTypeV1};
use serde_json::{Value, json};
use toolkit_gts::GtsSchema;

const BASE_ID: &str = "gts.cf.core.rg.type.v1~";

fn inventory_schema(gts_id: &str) -> Value {
    let want = json!(format!("gts://{gts_id}"));
    toolkit_gts::all_inventory_type_schemas()
        .expect("inventory schemas parse")
        .into_iter()
        .find(|s| s["$id"] == want)
        .unwrap_or_else(|| panic!("{gts_id} not in inventory"))
}

#[test]
fn tenant_path_matches_declared_type_id() {
    assert_eq!(TENANT_RG_TYPE_PATH, TenantResourceGroupTypeV1::TYPE_ID);
    assert_eq!(BASE_ID, ResourceGroupTypeV1::<()>::TYPE_ID);
}

#[test]
fn base_declares_traits_schema_and_defaults() {
    let base = inventory_schema(BASE_ID);
    let ts = &base["x-gts-traits-schema"];
    assert_eq!(ts["type"], "object");
    assert_eq!(ts["additionalProperties"], json!(false));
    let p = &ts["properties"];
    assert_eq!(p["can_be_root"]["type"], "boolean");
    assert_eq!(p["can_be_root"]["default"], json!(false));
    assert_eq!(p["allowed_parent_types"]["type"], "array");
    assert_eq!(p["allowed_parent_types"]["default"], json!([]));
    assert_eq!(p["allowed_parent_types"]["items"]["type"], "string");
    assert_eq!(p["allowed_parent_types"]["items"]["x-gts-ref"], BASE_ID);
    assert_eq!(p["allowed_membership_types"]["type"], "array");
    assert_eq!(p["allowed_membership_types"]["default"], json!([]));
    assert_eq!(p["allowed_membership_types"]["items"]["x-gts-ref"], "gts.*");
    assert_eq!(
        base["x-gts-traits"],
        json!({"can_be_root": false, "allowed_parent_types": [], "allowed_membership_types": []})
    );
    assert_eq!(base["x-gts-abstract"], json!(true));
}

#[test]
fn tenant_type_declares_exact_traits() {
    let tenant = inventory_schema(TENANT_RG_TYPE_PATH);
    assert_eq!(
        tenant["x-gts-traits"],
        json!({
            "can_be_root": true,
            "allowed_parent_types": [TENANT_RG_TYPE_PATH],
            "allowed_membership_types": []
        })
    );
}

#[test]
fn store_resolves_tenant_type() {
    let mut store = gts::GtsStore::new();
    store
        .register_schema(BASE_ID, &inventory_schema(BASE_ID))
        .expect("register base");
    store
        .register_schema(TENANT_RG_TYPE_PATH, &inventory_schema(TENANT_RG_TYPE_PATH))
        .expect("register tenant");
    let resolved = store
        .validate_schema(TENANT_RG_TYPE_PATH)
        .expect("tenant type validates against RG base");
    assert_eq!(
        resolved.effective_traits,
        json!({
            "can_be_root": true,
            "allowed_parent_types": [TENANT_RG_TYPE_PATH],
            "allowed_membership_types": []
        })
    );
    // The resolved schema is `allOf [base, derived]`; the derived part owns `metadata`.
    let props = &resolved.schema["allOf"][1]["properties"]["metadata"]["properties"];
    for key in ["status", "self_managed", "custom_domain"] {
        assert!(
            props.get(key).is_some(),
            "metadata.{key} missing: {}",
            resolved.schema
        );
    }
    assert_eq!(
        props["status"]["anyOf"][0]["enum"],
        json!(["active", "suspended", "deleted"])
    );
}
