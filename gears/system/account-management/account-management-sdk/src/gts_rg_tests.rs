//! Tests for the AM Resource Group type schemas declared in [`crate::gts`].

use super::{USER_GROUP_RG_TYPE_CODE, USER_RG_TYPE_CODE, UserGroupRgTypeV1, UserRgTypeV1};
use serde_json::{Value, json};
use toolkit_gts::GtsSchema;

const RG_BASE_ID: &str = "gts.cf.core.rg.type.v1~";

fn inventory_schema(gts_id: &str) -> Value {
    let want = json!(format!("gts://{gts_id}"));
    toolkit_gts::all_inventory_type_schemas()
        .expect("inventory schemas parse")
        .into_iter()
        .find(|s| s["$id"] == want)
        .unwrap_or_else(|| panic!("{gts_id} not in inventory"))
}

fn user_traits() -> Value {
    json!({"can_be_root": true, "allowed_parent_types": [], "allowed_membership_types": []})
}

fn group_traits() -> Value {
    json!({
        "can_be_root": true,
        "allowed_parent_types": [USER_GROUP_RG_TYPE_CODE],
        "allowed_membership_types": [USER_RG_TYPE_CODE]
    })
}

#[test]
fn constants_match_declared_type_ids() {
    assert_eq!(USER_RG_TYPE_CODE, UserRgTypeV1::TYPE_ID);
    assert_eq!(USER_GROUP_RG_TYPE_CODE, UserGroupRgTypeV1::TYPE_ID);
}

#[test]
fn inventory_carries_exact_traits() {
    assert_eq!(
        inventory_schema(USER_RG_TYPE_CODE)["x-gts-traits"],
        user_traits()
    );
    assert_eq!(
        inventory_schema(USER_GROUP_RG_TYPE_CODE)["x-gts-traits"],
        group_traits()
    );
}

#[test]
fn store_validates_against_rg_base() {
    let mut store = gts::GtsStore::new();
    for id in [RG_BASE_ID, USER_RG_TYPE_CODE, USER_GROUP_RG_TYPE_CODE] {
        store
            .register_schema(id, &inventory_schema(id))
            .unwrap_or_else(|e| panic!("register {id}: {e}"));
    }
    for (id, want) in [
        (USER_RG_TYPE_CODE, user_traits()),
        (USER_GROUP_RG_TYPE_CODE, group_traits()),
    ] {
        let resolved = store
            .validate_schema(id)
            .unwrap_or_else(|e| panic!("validate {id}: {e}"));
        assert_eq!(resolved.effective_traits, want, "{id}");
        let metadata = &resolved.schema["allOf"][1]["properties"]["metadata"];
        assert_eq!(metadata["additionalProperties"], json!(false), "{id}");
        assert_eq!(metadata["properties"], json!({}), "{id}");
    }
}
