use super::*;

#[test]
fn parse_format_defaults_to_json() {
    assert_eq!(parse_format(None).unwrap(), ExportFormat::Json);
}

#[test]
fn parse_format_accepts_known_values() {
    assert_eq!(parse_format(Some("json")).unwrap(), ExportFormat::Json);
    assert_eq!(
        parse_format(Some("markdown")).unwrap(),
        ExportFormat::Markdown
    );
    assert_eq!(parse_format(Some("md")).unwrap(), ExportFormat::Markdown);
}

#[test]
fn parse_format_rejects_unknown_value() {
    let err = parse_format(Some("yaml")).unwrap_err();
    assert!(matches!(err, ChatEngineError::BadRequest { .. }));
}

#[test]
fn map_share_error_emits_410_for_expired_conflict() {
    let err = ChatEngineError::Conflict {
        reason: "share token expired".into(),
    };
    let response = map_share_error(err);
    assert_eq!(response.status(), StatusCode::GONE);
}

#[test]
fn map_share_error_passes_through_unrelated_conflicts() {
    let err = ChatEngineError::Conflict {
        reason: "invalid lifecycle transition".into(),
    };
    let response = map_share_error(err);
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[test]
fn revoke_share_response_serializes_as_empty_object() {
    let body = serde_json::to_string(&RevokeShareResponse {}).unwrap();
    assert_eq!(body, "{}");
}

#[test]
fn create_share_body_anti_spoof_fields_default_none() {
    let body: CreateShareBody = serde_json::from_str("{}").unwrap();
    assert!(body.expires_in_hours.is_none());
    assert!(body.tenant_id.is_none());
    assert!(body.user_id.is_none());
}

#[test]
fn create_share_body_rejects_spoofed_identity() {
    let body: CreateShareBody = serde_json::from_str(r#"{"tenant_id": "x"}"#).unwrap();
    let result = reject_body_identity(&body.tenant_id, &body.user_id);
    assert!(matches!(result, Err(ChatEngineError::BadRequest { .. })));
}

#[test]
fn export_session_query_defaults() {
    let q: ExportSessionQuery = serde_json::from_str("{}").unwrap();
    assert!(q.format.is_none());
    assert!(q.include_plugin_metadata.is_none());
}
