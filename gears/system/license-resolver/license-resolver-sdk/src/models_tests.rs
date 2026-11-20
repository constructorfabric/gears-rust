//! Unit tests for the license resolver value objects.

use toolkit_gts::gts_id;

use super::{
    IsLicensedRequest, LicenseCheckContext, LicenseCheckRequest, LicenseDecision,
    PlatformIsLicensedRequest, PrincipalSubject, Resource, Subject, SubjectInput,
};

const SUBJECT_CONTRACT: &str = gts_id!("cf.core.lic.subj.v1~cf.genai.llm_gateway.user.v1~");
const RESOURCE_CONTRACT: &str = gts_id!("cf.core.lic.res.v1~cf.genai.llm_gateway.model_usage.v1~");

#[test]
fn subject_round_trips() {
    let json = format!(
        r#"{{"type":"{SUBJECT_CONTRACT}","id":"acme-admin","metadata":{{"category":"internal"}}}}"#
    );
    let subject: Subject = serde_json::from_str(&json).expect("valid subject");

    assert_eq!(subject.gts_type.as_ref(), SUBJECT_CONTRACT);
    assert_eq!(subject.id.as_deref(), Some("acme-admin"));
    assert_eq!(
        subject.metadata.get("category").and_then(|v| v.as_str()),
        Some("internal")
    );

    let wire = serde_json::to_value(&subject).unwrap();
    assert_eq!(
        wire.get("type").and_then(|v| v.as_str()),
        Some(SUBJECT_CONTRACT)
    );
    let back: Subject = serde_json::from_value(wire).unwrap();
    assert_eq!(back, subject);
}

#[test]
fn resource_whole_type_omits_id() {
    let json = format!(r#"{{"type":"{RESOURCE_CONTRACT}","metadata":{{}}}}"#);
    let resource: Resource = serde_json::from_str(&json).expect("valid whole-type resource");
    assert!(resource.id.is_none());

    let wire = serde_json::to_value(&resource).unwrap();
    assert!(
        wire.get("id").is_none(),
        "absent id must be omitted for a whole-type check, got: {wire}"
    );
    assert_eq!(
        wire.get("type").and_then(|v| v.as_str()),
        Some(RESOURCE_CONTRACT)
    );
}

#[test]
fn resource_specific_instance_carries_id() {
    let json = format!(
        r#"{{"type":"{RESOURCE_CONTRACT}","id":"gpt-4o","metadata":{{"model_vendor":"openai"}}}}"#
    );
    let resource: Resource = serde_json::from_str(&json).expect("valid resource");
    assert_eq!(resource.id.as_deref(), Some("gpt-4o"));
    assert_eq!(
        resource
            .metadata
            .get("model_vendor")
            .and_then(|v| v.as_str()),
        Some("openai")
    );
}

#[test]
fn decision_defaults_to_not_granted_with_empty_diagnostics() {
    let decision = LicenseDecision::default();
    assert!(!decision.granted);
    assert!(decision.diagnostics.is_empty());
}

#[test]
fn request_round_trips() {
    let request = LicenseCheckRequest::new(
        uuid::Uuid::from_u128(42),
        serde_json::from_str(&format!(
            r#"{{"type":"{SUBJECT_CONTRACT}","metadata":{{}}}}"#
        ))
        .unwrap(),
        serde_json::from_str(&format!(
            r#"{{"type":"{RESOURCE_CONTRACT}","metadata":{{}}}}"#
        ))
        .unwrap(),
        LicenseCheckContext::default(),
    );
    let back: LicenseCheckRequest =
        serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap();
    assert_eq!(back, request);
}

#[test]
fn subject_input_is_tagged_by_kind() {
    let principal = PrincipalSubject::new("user", uuid::Uuid::from_u128(7));
    let wire = serde_json::to_value(SubjectInput::from(principal.clone())).unwrap();
    assert_eq!(
        wire,
        serde_json::json!({ "kind": "principal", "type": "user", "id": uuid::Uuid::from_u128(7) })
    );
    assert_eq!(
        serde_json::from_value::<SubjectInput>(wire).unwrap(),
        SubjectInput::Principal(principal)
    );

    let license: SubjectInput = serde_json::from_str(&format!(
        r#"{{"kind":"license","type":"{SUBJECT_CONTRACT}","metadata":{{}}}}"#
    ))
    .unwrap();
    assert!(matches!(license, SubjectInput::License(_)));
}

#[test]
fn nonconforming_subject_inputs_do_not_deserialize() {
    for (case, raw) in [
        (
            "no kind",
            format!(r#"{{"type":"{SUBJECT_CONTRACT}","metadata":{{}}}}"#),
        ),
        (
            "principal with metadata",
            r#"{"kind":"principal","type":"user","id":"00000000-0000-0000-0000-000000000007","metadata":{}}"#
                .to_owned(),
        ),
        (
            "principal with a non-uuid id",
            r#"{"kind":"principal","type":"user","id":"u1"}"#.to_owned(),
        ),
        (
            "license subject with an unknown field",
            format!(r#"{{"kind":"license","type":"{SUBJECT_CONTRACT}","metadata":{{}},"bogus":1}}"#),
        ),
    ] {
        assert!(
            serde_json::from_str::<SubjectInput>(&raw).is_err(),
            "`{case}` must not deserialize: {raw}"
        );
    }
}

#[test]
fn nonconforming_resource_payloads_do_not_deserialize() {
    for (case, raw) in [
        (
            "missing metadata",
            format!(r#"{{"type":"{RESOURCE_CONTRACT}"}}"#),
        ),
        (
            "unknown top-level field",
            format!(r#"{{"type":"{RESOURCE_CONTRACT}","metadata":{{}},"bogus":1}}"#),
        ),
        (
            "non-object metadata",
            format!(r#"{{"type":"{RESOURCE_CONTRACT}","metadata":"invalid"}}"#),
        ),
        ("missing type", r#"{"metadata":{}}"#.to_owned()),
    ] {
        assert!(
            serde_json::from_str::<Resource>(&raw).is_err(),
            "`{case}` must not be laundered into a conforming Resource: {raw}"
        );
    }
}

#[test]
fn nonconforming_subject_payloads_do_not_deserialize() {
    for (case, raw) in [
        (
            "unknown top-level field",
            format!(r#"{{"type":"{SUBJECT_CONTRACT}","metadata":{{}},"bogus":1}}"#),
        ),
        (
            "misspelled id",
            format!(r#"{{"type":"{SUBJECT_CONTRACT}","idd":"acme-admin","metadata":{{}}}}"#),
        ),
    ] {
        assert!(
            serde_json::from_str::<Subject>(&raw).is_err(),
            "`{case}` must not be laundered into a conforming Subject: {raw}"
        );
    }
}

#[test]
fn misspelled_resource_id_is_rejected_instead_of_widening_the_check() {
    let raw = format!(r#"{{"type":"{RESOURCE_CONTRACT}","idd":"gpt-4o","metadata":{{}}}}"#);
    assert!(
        serde_json::from_str::<Resource>(&raw).is_err(),
        "a misspelled id must not silently become a whole-type check: {raw}"
    );
}

#[test]
fn envelope_ignores_unknown_fields() {
    let tenant = uuid::Uuid::from_u128(0x1d2f_6e3a);
    let raw = format!(
        concat!(
            r#"{{"tenant_id":"{tenant}","#,
            r#""subject":{{"kind":"license","type":"{subject}","metadata":{{}}}},"#,
            r#""resource":{{"type":"{resource}","metadata":{{}}}},"#,
            r#""context":{{"future_contextual_input":true}},"#,
            r#""future_request_input":1}}"#
        ),
        subject = SUBJECT_CONTRACT,
        resource = RESOURCE_CONTRACT,
        tenant = tenant
    );

    let request: PlatformIsLicensedRequest = serde_json::from_str(&raw)
        .expect("a newer producer's request must stay readable by an older reader");
    assert_eq!(request.tenant_id, tenant);
    assert!(
        matches!(&request.subject, SubjectInput::License(s) if s.gts_type.as_ref() == SUBJECT_CONTRACT)
    );
}

#[test]
fn context_is_optional_on_the_wire() {
    let raw = format!(r#"{{"resource":{{"type":"{RESOURCE_CONTRACT}","metadata":{{}}}}}}"#);
    let request: IsLicensedRequest = serde_json::from_str(&raw).expect("context may be omitted");
    assert_eq!(request.context, LicenseCheckContext::default());
}

#[test]
fn subject_input_schema_is_a_discriminated_union_of_closed_branches() {
    use utoipa::openapi::RefOr;
    use utoipa::openapi::schema::Schema;
    use utoipa::{PartialSchema, ToSchema};

    let RefOr::T(Schema::OneOf(one_of)) = SubjectInput::schema() else {
        panic!("SubjectInput is a oneOf");
    };
    let mapping = one_of.discriminator.expect("tagged by kind").mapping;
    assert_eq!(
        mapping.get("license").map(String::as_str),
        Some("#/components/schemas/LicenseSubjectInput")
    );
    assert_eq!(
        mapping.get("principal").map(String::as_str),
        Some("#/components/schemas/PrincipalSubjectInput")
    );

    let mut components = Vec::new();
    SubjectInput::schemas(&mut components);
    for ((name, branch), (expected, fields)) in components.iter().zip([
        (
            "LicenseSubjectInput",
            ["kind", "type", "id", "metadata"].as_slice(),
        ),
        ("PrincipalSubjectInput", ["kind", "type", "id"].as_slice()),
    ]) {
        assert_eq!(name, expected);
        let RefOr::T(Schema::Object(object)) = branch else {
            panic!("`{name}` is one closed object, not an allOf");
        };
        for field in fields {
            assert!(
                object.properties.contains_key(*field),
                "`{name}` must declare `{field}`"
            );
        }
        assert!(object.required.iter().any(|r| r == "kind"));
    }
}

#[test]
fn decision_ignores_unknown_fields() {
    let decision: LicenseDecision =
        serde_json::from_str(r#"{"granted":true,"diagnostics":{},"grant_status":"active"}"#)
            .expect("granted is authoritative on its own");
    assert!(decision.granted);
}
