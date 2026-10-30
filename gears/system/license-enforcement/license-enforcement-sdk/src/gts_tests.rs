use gts::GtsSchema;
use serde_json::Value;

use super::RecipientProjectionV1;

#[test]
fn recipient_projection_type_id_and_properties() {
    assert_eq!(
        RecipientProjectionV1::TYPE_ID,
        toolkit_gts::gts_id!("cf.core.license_enforcement.recipient.v1~")
    );
    let schema: Value =
        serde_json::from_str(&RecipientProjectionV1::gts_schema_with_refs_as_string()).unwrap();
    let props = schema["properties"].as_object().unwrap();
    for key in ["type", "id", "tenant_type", "status"] {
        assert!(props.contains_key(key), "{key} missing:\n{schema:#}");
    }
}
