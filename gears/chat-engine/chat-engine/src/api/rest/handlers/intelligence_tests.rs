use super::*;
use crate::domain::error::ChatEngineError;
use serde_json::json;

#[test]
fn patch_body_deserializes_none_policy() {
    let body: PatchRetentionPolicyBody = serde_json::from_value(json!({"type": "none"})).unwrap();
    assert!(matches!(body.policy, RetentionPolicy::None));
}

#[test]
fn patch_body_deserializes_age_based() {
    let body: PatchRetentionPolicyBody =
        serde_json::from_value(json!({"type": "age_based", "max_age_days": 7})).unwrap();
    assert!(matches!(
        body.policy,
        RetentionPolicy::AgeBased { max_age_days: 7 }
    ));
}

#[test]
fn patch_body_deserializes_count_based() {
    let body: PatchRetentionPolicyBody =
        serde_json::from_value(json!({"type": "count_based", "max_message_count": 100})).unwrap();
    assert!(matches!(
        body.policy,
        RetentionPolicy::CountBased {
            max_message_count: 100
        }
    ));
}

#[test]
fn patch_body_rejects_unknown_type() {
    let res: std::result::Result<PatchRetentionPolicyBody, _> =
        serde_json::from_value(json!({"type": "no_such"}));
    assert!(res.is_err(), "unknown discriminator must be rejected");
}

#[test]
fn patch_body_rejects_anti_spoof_fields() {
    // The body deserializes fine; the handler-level guard then
    // rejects the request with BadRequest. Mirror the Phase 4 guard
    // shape so a regression of the deserializer doesn't accidentally
    // gain tenant_id/user_id setters.
    let body: PatchRetentionPolicyBody = serde_json::from_value(json!({
        "type": "none",
        "tenant_id": "spoof",
        "user_id": "spoof",
    }))
    .unwrap();
    assert!(body.tenant_id.is_some());
    assert!(body.user_id.is_some());
    let err = reject_body_identity(&body.tenant_id, &body.user_id).unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}
