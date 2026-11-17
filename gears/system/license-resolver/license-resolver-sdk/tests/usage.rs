#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests demonstrating how a consuming Gear builds a license check.
//!
//! They compile from outside the crate, which carries a guarantee no unit test
//! can: `LicenseCheckContext`, the requests and `LicenseDecision` are
//! `#[non_exhaustive]` so a future contextual input stays additive, which puts
//! struct literals out of reach for consumers and makes the constructors the
//! whole public surface. If a growth-surface field ever becomes unreachable
//! through a constructor, this stops compiling.

use license_resolver_sdk::{
    IsLicensedRequest, LicenseCheckContext, LicenseDecision, PlatformIsLicensedRequest,
    PrincipalSubject, Resource, Subject, SubjectInput,
};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const SUBJECT_CONTRACT: &str = gts_id!("cf.core.lic.subj.v1~cf.genai.llm_gateway.user.v1~");
const RESOURCE_CONTRACT: &str = gts_id!("cf.core.lic.res.v1~cf.genai.llm_gateway.model_usage.v1~");

fn subject() -> Subject {
    serde_json::from_str(&format!(
        r#"{{"type":"{SUBJECT_CONTRACT}","metadata":{{"category":"internal"}}}}"#
    ))
    .expect("valid subject")
}

fn resource() -> Resource {
    serde_json::from_str(&format!(
        r#"{{"type":"{RESOURCE_CONTRACT}","id":"gpt-4o","metadata":{{"model_vendor":"openai"}}}}"#
    ))
    .expect("valid resource")
}

#[test]
fn a_consumer_can_check_its_user_on_the_platform_plane() {
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(7))
        .subject_tenant_id(Uuid::from_u128(42))
        .subject_type("user")
        .build()
        .expect("valid security context");

    let request = PlatformIsLicensedRequest::for_caller(&ctx, resource());

    assert_eq!(request.tenant_id, Uuid::from_u128(42));
    assert_eq!(
        request.subject,
        SubjectInput::Principal(PrincipalSubject::new("user", Uuid::from_u128(7)))
    );
    assert_eq!(request.context, LicenseCheckContext::default());
}

#[test]
fn a_consumer_can_name_a_subject_contract_object() {
    let request = PlatformIsLicensedRequest::new(Uuid::from_u128(42), subject(), resource());

    assert!(matches!(request.subject, SubjectInput::License(_)));
    assert_eq!(request.resource.id.as_deref(), Some("gpt-4o"));
}

#[test]
fn a_tenant_plane_request_names_only_the_resource_and_context() {
    let request = IsLicensedRequest::new(resource());

    assert_eq!(request.resource.id.as_deref(), Some("gpt-4o"));
    assert_eq!(request.context, LicenseCheckContext::default());
}

#[test]
fn a_plugin_can_answer_without_a_struct_literal() {
    let denied = LicenseDecision::default();
    assert!(!denied.granted, "the default answer is fail-closed");

    let granted = LicenseDecision::new(true).with_diagnostic("backend", "static-license-plugin");
    assert!(granted.granted);
    assert_eq!(
        granted.diagnostics.get("backend").and_then(|v| v.as_str()),
        Some("static-license-plugin"),
        "diagnostics stay advisory but must survive the constructor"
    );
}
