use serde_json::json;

use super::{BindRequest, CheckExplanation, VerbRequest};
use crate::reason::DenialReason;

#[test]
fn verb_request_carries_token_and_version() {
    let bind: BindRequest = serde_json::from_value(json!({
        "subject": {"type": "gts.cf.core.lic.subj.v1~cf.core.lic.tenant.v1~", "id": "u1"},
        "idempotency_token": "t1",
        "expected_version": 4
    }))
    .unwrap();
    assert_eq!(bind.expected_version, Some(4));

    let accept: VerbRequest = serde_json::from_value(json!({"idempotency_token": "t2"})).unwrap();
    assert_eq!(accept.expected_version, None);
}

#[test]
fn verb_request_rejects_unknown_fields_and_missing_token() {
    for body in [
        json!({"idempotency_token": "t", "extra": 1}),
        json!({"verb": "accept", "idempotency_token": "t"}),
        json!({}),
    ] {
        assert!(
            serde_json::from_value::<VerbRequest>(body.clone()).is_err(),
            "{body}"
        );
    }
}

#[test]
fn check_explanation_serializes_reason_as_wire_string() {
    let explanation = CheckExplanation {
        granted: false,
        reason: Some(DenialReason::SubjectNotBound),
        termination_reason: None,
        grant_id: None,
        grace: false,
        diagnostics: std::collections::HashMap::default(),
    };
    let json = serde_json::to_value(&explanation).unwrap();
    assert_eq!(json["reason"], "subject_not_bound");
    assert_eq!(
        serde_json::from_value::<CheckExplanation>(json).unwrap(),
        explanation
    );
}

/// The keys must be the ones that `toolkit::api::odata::ODataParams` binds.
#[test]
fn list_query_uses_odata_wire_keys() {
    let query = super::ListQuery {
        filter: Some("state eq 'active'".to_owned()),
        orderby: Some("resource_type asc".to_owned()),
        limit: Some(10),
        cursor: Some("c1".to_owned()),
    };
    let encoded = toolkit_contract::query::to_query_string(&query)
        .unwrap()
        .unwrap();
    let keys: Vec<&str> = encoded
        .split('&')
        .filter_map(|pair| pair.split('=').next())
        .collect();
    assert_eq!(keys, ["%24filter", "%24orderby", "limit", "cursor"]);
}
