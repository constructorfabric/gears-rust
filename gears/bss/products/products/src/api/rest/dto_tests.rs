#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::sku::SkuPatch;
use serde_json::json;

#[test]
fn nullable_patch_values_and_invalid_enums_keep_their_fields() {
    let absent: SkuPatchRequest = serde_json::from_value(json!({})).unwrap();
    assert_eq!(SkuPatch::try_from(absent).unwrap().gl_code, None);
    let clear: SkuPatchRequest =
        serde_json::from_value(json!({"gl_code":null,"billing_timing":null,"usage_type_ref":null}))
            .unwrap();
    let clear = SkuPatch::try_from(clear).unwrap();
    assert_eq!(clear.gl_code, Some(None));
    assert_eq!(clear.billing_timing, Some(None));
    assert_eq!(clear.usage_type_ref, Some(None));
    let bad: SkuPatchRequest =
        serde_json::from_value(json!({"type":"bad","lifecycle":"bad","billing_timing":"bad"}))
            .unwrap();
    let report = SkuPatch::try_from(bad).unwrap_err();
    for field in ["type", "lifecycle", "billing_timing"] {
        assert!(
            report
                .violations()
                .iter()
                .any(|v| v.subject == field && v.code == "VALIDATION")
        );
    }
}

/// P-D-196: `category_id` is optional on create (omitted is null, never a default) and three-state
/// on the draft PATCH: omitted keeps it, null clears it, a value sets it.
#[test]
fn the_category_is_optional_on_create_and_three_state_on_patch() {
    use crate::domain::sku::NewSku;
    use uuid::Uuid;
    for body in [
        json!({"code":"A","name":"A","type":"recurring"}),
        json!({"code":"A","name":"A","type":"recurring","category_id":null}),
    ] {
        let create: SkuRequest = serde_json::from_value(body).unwrap();
        assert_eq!(NewSku::try_from(create).unwrap().category_id, None);
    }
    let id = Uuid::new_v4();
    for (body, want) in [
        (json!({}), None),
        (json!({"category_id":null}), Some(None)),
        (json!({"category_id":id}), Some(Some(id))),
    ] {
        let patch: SkuPatchRequest = serde_json::from_value(body).unwrap();
        assert_eq!(SkuPatch::try_from(patch).unwrap().category_id, want);
    }
}
