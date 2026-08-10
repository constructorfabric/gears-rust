use super::*;

#[test]
fn kc_rest_detail_format_matches_design() {
    let e = PluginError::KcRest {
        method: "GET",
        path_template: "/admin/realms/{realm}".into(),
        status: KcStatusKind::Http(403),
        body_first_2kb: "forbidden".into(),
    };
    assert_eq!(
        format!("{e}"),
        "kc:GET /admin/realms/{realm} -> 403: forbidden"
    );
}

#[test]
fn kc_rest_with_transport_status() {
    let e = PluginError::KcRest {
        method: "POST",
        path_template: "/admin/realms".into(),
        status: KcStatusKind::Transport,
        body_first_2kb: "connection refused".into(),
    };
    assert_eq!(
        format!("{e}"),
        "kc:POST /admin/realms -> transport: connection refused"
    );
}

#[test]
fn kc_rest_with_timeout_status() {
    let e = PluginError::KcRest {
        method: "PUT",
        path_template: "/admin/realms/{realm}".into(),
        status: KcStatusKind::Timeout,
        body_first_2kb: "deadline exceeded".into(),
    };
    assert_eq!(
        format!("{e}"),
        "kc:PUT /admin/realms/{realm} -> timeout: deadline exceeded"
    );
}

#[test]
fn credstore_read_detail_format() {
    let e = PluginError::CredStoreRead {
        detail: "timeout reading vp-idp-bootstrap-admin-secret".into(),
    };
    assert_eq!(
        format!("{e}"),
        "internal:CredStoreReader: timeout reading vp-idp-bootstrap-admin-secret"
    );
}

#[test]
fn metadata_decode_propagates_via_from() {
    use crate::domain::metadata_codec::DecodeError;
    let dec_err = DecodeError::MissingVersion;
    let plugin_err: PluginError = dec_err.into();
    assert!(matches!(plugin_err, PluginError::MetadataDecode(_)));
    assert!(format!("{plugin_err}").contains("internal:MetadataCodec:"));
}

#[test]
fn bootstrap_perms_missing_detail_format() {
    let e = PluginError::BootstrapPermsMissing {
        realm: "partner-acme".into(),
    };
    assert_eq!(
        format!("{e}"),
        "bootstrap admin lacks view-realm/query-groups in partner-acme — see Phase 0 IaC checklist"
    );
}

#[test]
fn created_realm_exists_detail_format() {
    let e = PluginError::CreatedRealmExists {
        realm: "partner-acme".into(),
    };
    assert_eq!(
        format!("{e}"),
        "mode = created but realm 'partner-acme' already exists"
    );
}

#[test]
fn ambiguous_created_kc_realm_create_format() {
    let e = PluginError::AmbiguousCreated {
        stage: AmbiguousStage::KcRealmCreate,
        detail: "kc:POST /admin/realms -> 500: boom".into(),
    };
    assert_eq!(
        format!("{e}"),
        "ambig:kc_realm_create: kc:POST /admin/realms -> 500: boom"
    );
}

#[test]
fn ambiguous_created_openbao_format() {
    let e = PluginError::AmbiguousCreated {
        stage: AmbiguousStage::OpenBaoPutAfterKcSuccess,
        detail: "credstore: ServiceUnavailable(nope)".into(),
    };
    assert_eq!(
        format!("{e}"),
        "ambig:openbao_put_after_kc_success: credstore: ServiceUnavailable(nope)"
    );
}

#[test]
fn deprovision_not_found_detail_format() {
    let e = PluginError::DeprovisionNotFound {
        realm_name: "partner-acme".into(),
    };
    assert_eq!(
        format!("{e}"),
        "deprovision target not found in realm 'partner-acme'"
    );
}

#[test]
fn deprovision_retryable_detail_format() {
    let e = PluginError::DeprovisionRetryable {
        detail: "kc:DELETE /admin/realms/{realm}/groups/{tenant_group_id} -> 503: boom".into(),
    };
    assert_eq!(
        format!("{e}"),
        "deprovision retryable: kc:DELETE /admin/realms/{realm}/groups/{tenant_group_id} -> 503: boom"
    );
}

#[test]
fn deprovision_terminal_detail_format() {
    let e = PluginError::DeprovisionTerminal {
        detail: "metadata corrupt".into(),
    };
    assert_eq!(format!("{e}"), "deprovision terminal: metadata corrupt");
}

#[test]
fn user_op_rejected_detail_format() {
    let e = PluginError::UserOpRejected {
        detail: "user already exists".into(),
    };
    assert_eq!(format!("{e}"), "user op rejected: user already exists");
}

#[test]
fn user_op_unavailable_detail_format() {
    let e = PluginError::UserOpUnavailable {
        detail: "kc:POST /admin/realms/{realm}/users -> 503: boom".into(),
    };
    assert_eq!(
        format!("{e}"),
        "user op unavailable: kc:POST /admin/realms/{realm}/users -> 503: boom"
    );
}

#[test]
fn user_op_unsupported_detail_format() {
    let e = PluginError::UserOpUnsupported {
        detail: "provider is read-only".into(),
    };
    assert_eq!(format!("{e}"), "user op unsupported: provider is read-only");
}

#[test]
fn redact_secrets_masks_client_secret_kv() {
    let body = r#"{"error":"invalid_request","client_secret":"sensitive-value-123"}"#;
    let out = crate::domain::error::redact_secrets(body);
    assert!(!out.contains("sensitive-value-123"), "secret leaked: {out}");
    assert!(out.contains("<redacted>"), "redaction marker absent: {out}");
}

#[test]
fn redact_secrets_masks_password_kv() {
    let body = "password=hunter2&grant_type=password";
    let out = crate::domain::error::redact_secrets(body);
    assert!(!out.contains("hunter2"), "password leaked: {out}");
}

#[test]
fn redact_secrets_masks_authorization_header_echo() {
    let body = "received Authorization: Bearer eyJhbGciOi...";
    let out = crate::domain::error::redact_secrets(body);
    assert!(!out.contains("eyJhbGciOi"), "bearer token leaked: {out}");
    assert!(out.contains("<redacted>"));
    // Pin the HTTP-header echo shape — no `=` injected (which would
    // produce the malformed `Bearer=<redacted>` form).
    assert!(
        out.contains("Bearer <redacted>"),
        "expected `Bearer <redacted>` (space separator); got: {out}"
    );
}

#[test]
fn redact_secrets_passes_through_non_matching_body() {
    let body = r#"{"error":"not_found","message":"realm not found"}"#;
    let out = crate::domain::error::redact_secrets(body);
    assert_eq!(out, body);
}

#[test]
fn redact_secrets_preserves_trailing_json_fields_after_client_secret() {
    // Regression: a greedy `\S+` value bound on `kv_re` previously
    // consumed `"abc","other":"y"}` in one match, so every diagnostic
    // field after the secret was lost from `body_first_2kb`. The bound
    // must stop at the JSON closing quote so the rest of the body
    // round-trips.
    let body = r#"{"error":"x","client_secret":"abc","other":"y"}"#;
    let out = crate::domain::error::redact_secrets(body);
    assert!(!out.contains("\"abc\""), "client_secret leaked: {out}");
    assert!(out.contains("<redacted>"), "redaction marker absent: {out}");
    assert!(
        out.contains("\"other\":\"y\""),
        "trailing JSON field after client_secret was clobbered by greedy regex: {out}"
    );
    assert!(
        out.ends_with("\"other\":\"y\"}"),
        "JSON closing brace clobbered: {out}"
    );
}

#[test]
fn redact_secrets_preserves_trailing_query_params_after_password() {
    // Form-body shape: `password=...&grant_type=...&...`. The value
    // bound must stop at `&` so the remaining query params survive.
    let body = "grant_type=password&password=hunter2&client_id=foo";
    let out = crate::domain::error::redact_secrets(body);
    assert!(!out.contains("hunter2"), "password leaked: {out}");
    assert!(out.contains("<redacted>"), "redaction marker absent: {out}");
    assert!(
        out.contains("client_id=foo"),
        "trailing form param after password was clobbered: {out}"
    );
}

#[test]
fn redact_secrets_masks_kc_camel_case_keys() {
    // KC error / discovery responses use camelCase: `clientSecret`,
    // `refreshToken`, `idToken`, plus the bare `secret` field on
    // `ClientRepresentation`. snake_case-only matching let these slip
    // through the defence-in-depth layer.
    let body = r#"{"clientId":"foo","clientSecret":"hunter2","refreshToken":"rt-x"}"#;
    let out = crate::domain::error::redact_secrets(body);
    assert!(!out.contains("hunter2"), "clientSecret leaked: {out}");
    assert!(!out.contains("rt-x"), "refreshToken leaked: {out}");
    assert!(out.contains("<redacted>"));
}

#[test]
fn redact_secrets_masks_kc_bare_secret_key() {
    let body = r#"{"clientId":"foo","secret":"shh"}"#;
    let out = crate::domain::error::redact_secrets(body);
    assert!(!out.contains("\"shh\""), "bare `secret` key leaked: {out}");
    assert!(out.contains("<redacted>"));
}

#[test]
fn redact_secrets_does_not_match_secret_inside_compound_key() {
    // `client_secrets` (plural) and `secretary` MUST NOT trigger the
    // `\bsecret\b` arm — otherwise we redact unrelated harmless fields.
    let body = r#"{"client_secrets_managed":true,"secretary":"alice"}"#;
    let out = crate::domain::error::redact_secrets(body);
    // `client_secrets_managed` is NOT one of the redacted keys; the
    // `client_secret` arm has no `:` immediately after `_secret`, so no
    // false-positive. `secretary` likewise shouldn't trigger.
    assert!(
        out.contains("\"client_secrets_managed\":true") && out.contains("\"secretary\":\"alice\""),
        "compound key false-positive: {out}"
    );
}

#[test]
fn redact_secrets_is_case_insensitive() {
    let body = r#"Client_Secret = "x""#;
    let out = crate::domain::error::redact_secrets(body);
    assert!(
        !out.contains("\"x\""),
        "case-insensitive match failed: {out}"
    );
}

#[test]
fn ambiguous_stage_display_strings_are_stable() {
    // These strings are part of the `ambig:{stage}:` prefix contract
    // (DESIGN §4.2 / §5.5 line 482) and MUST NOT silently change — Phase
    // 15 boundary + operator runbooks key on them.
    assert_eq!(
        AmbiguousStage::KcRealmCreate.to_string(),
        "ambig:kc_realm_create"
    );
    assert_eq!(
        AmbiguousStage::KcClientCreate.to_string(),
        "ambig:kc_client_create"
    );
    assert_eq!(
        AmbiguousStage::KcRoleMapping.to_string(),
        "ambig:kc_role_mapping"
    );
    assert_eq!(
        AmbiguousStage::KcClientSecretRead.to_string(),
        "ambig:kc_client_secret_read"
    );
    assert_eq!(
        AmbiguousStage::AdminUserBind.to_string(),
        "ambig:admin_user_bind"
    );
    assert_eq!(
        AmbiguousStage::OpenBaoPutAfterKcSuccess.to_string(),
        "ambig:openbao_put_after_kc_success"
    );
}

#[test]
fn ambiguous_stage_timeout_displays_with_ambig_prefix() {
    assert_eq!(AmbiguousStage::Timeout.to_string(), "ambig:timeout");
}

#[test]
fn sp_error_variants_have_stable_labels() {
    let invalid = PluginError::SpInvalidInput {
        detail: "d".into(),
        field: Some("name".into()),
    };
    let not_found = PluginError::SpNotFound { detail: "d".into() };
    assert_eq!(failure_variant_label(&invalid), "sp_invalid_input");
    assert_eq!(failure_variant_label(&not_found), "sp_not_found");
}

// SP-saga wire detail strings: part of the ambig:{stage} prefix contract; must not change silently.
#[test]
fn sp_ambiguous_stages_display() {
    assert_eq!(
        AmbiguousStage::KcSaAttrSet.to_string(),
        "ambig:kc_sa_attr_set"
    );
    assert_eq!(
        AmbiguousStage::KcClientScopeAttach.to_string(),
        "ambig:kc_client_scope_attach"
    );
    assert_eq!(
        AmbiguousStage::KcClientSecretRotate.to_string(),
        "ambig:kc_client_secret_rotate"
    );
    assert_eq!(
        AmbiguousStage::KcClientDelete.to_string(),
        "ambig:kc_client_delete"
    );
}

#[test]
fn sp_invalid_input_detail_format() {
    let e = PluginError::SpInvalidInput {
        detail: "name too long".into(),
        field: Some("name".into()),
    };
    assert_eq!(format!("{e}"), "sp invalid input: name too long");
}

#[test]
fn sp_not_found_detail_format() {
    let e = PluginError::SpNotFound {
        detail: "client abc not found".into(),
    };
    assert_eq!(format!("{e}"), "sp not found: client abc not found");
}
