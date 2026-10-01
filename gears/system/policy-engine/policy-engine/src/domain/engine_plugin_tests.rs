//! The plugin's mapping of decisions and failures onto the admission-control
//! vocabulary, over the in-memory decision stack.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use admission_control_sdk::{AdmissionEnginePluginClientV1, EngineFailureCondition, EngineResult};
use policy_engine_sdk::{ADMISSION_ENGINE_INSTANCE_ID, reason};
use toolkit_security::SecurityContext;

use super::{PolicyEngineAdmissionPlugin, registration};
use crate::domain::decision::test_support::{
    CHILD, DENY_ALL, DENY_NONE, NOT_BOOLEAN, SIBLING, Stack, ctx_in, request, widget_binding,
};
use crate::domain::ports::PortError;

fn plugin(stack: Stack) -> (PolicyEngineAdmissionPlugin, Stack) {
    (
        PolicyEngineAdmissionPlugin::new(Arc::clone(&stack.service)),
        stack,
    )
}

#[tokio::test]
async fn a_permit_carries_the_shadow_denials() {
    let shadow = widget_binding(2, CHILD, false, DENY_ALL);
    let shadow_document = shadow.version.documents[0].id.0;
    let (plugin, _stack) = plugin(Stack::new(vec![
        widget_binding(1, CHILD, true, DENY_NONE),
        shadow,
    ]));
    let result = plugin
        .evaluate(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    match result {
        EngineResult::Permit { shadow_denials } => {
            assert_eq!(shadow_denials.len(), 1);
            assert_eq!(shadow_denials[0].document_id, shadow_document);
            assert_eq!(shadow_denials[0].document_name, "doc2");
        }
        other @ EngineResult::Deny { .. } => panic!("expected a permit: {other:?}"),
    }
}

#[tokio::test]
async fn denials_name_every_denying_document() {
    let first = widget_binding(1, CHILD, true, DENY_ALL);
    let second = widget_binding(2, CHILD, true, DENY_ALL);
    let mut expected = [
        first.version.documents[0].id.0,
        second.version.documents[0].id.0,
    ];
    expected.sort();
    let (plugin, _stack) = plugin(Stack::new(vec![first, second]));

    let result = plugin
        .evaluate(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap();
    match result {
        EngineResult::Deny {
            reason_code,
            denials,
            shadow_denials,
        } => {
            assert_eq!(reason_code, reason::POLICY_DENIED);
            let mut documents: Vec<_> = denials.iter().map(|d| d.document_id).collect();
            documents.sort();
            assert_eq!(documents, expected);
            assert!(shadow_denials.is_empty());
        }
        other @ EngineResult::Permit { .. } => panic!("expected a denial: {other:?}"),
    }
}

#[tokio::test]
async fn the_tenant_boundary_is_a_denial_without_documents() {
    let (plugin, _stack) = plugin(Stack::new(vec![widget_binding(1, CHILD, true, DENY_ALL)]));
    let result = plugin
        .evaluate(&ctx_in(SIBLING), &request("create", CHILD))
        .await
        .unwrap();
    match result {
        EngineResult::Deny {
            reason_code,
            denials,
            ..
        } => {
            assert_eq!(reason_code, reason::TENANT_BOUNDARY);
            assert!(denials.is_empty());
        }
        other @ EngineResult::Permit { .. } => panic!("expected a denial: {other:?}"),
    }
}

#[tokio::test]
async fn failures_are_never_denials() {
    let (plugin, stack) = plugin(Stack::new(vec![widget_binding(
        1,
        CHILD,
        true,
        NOT_BOOLEAN,
    )]));

    let invalid = plugin
        .evaluate(&SecurityContext::anonymous(), &request("create", CHILD))
        .await
        .unwrap_err();
    assert_eq!(invalid.condition, EngineFailureCondition::InvalidRequest);

    let internal = plugin
        .evaluate(&ctx_in(CHILD), &request("create", CHILD))
        .await
        .unwrap_err();
    assert_eq!(internal.condition, EngineFailureCondition::Internal);

    *stack.hierarchy.fault.lock() = Some(PortError::Timeout);
    let timeout = plugin
        .evaluate(&ctx_in(SIBLING), &request("create", CHILD))
        .await
        .unwrap_err();
    assert_eq!(timeout.condition, EngineFailureCondition::Timeout);

    *stack.hierarchy.fault.lock() = Some(PortError::Unavailable("down".to_owned()));
    let unavailable = plugin
        .evaluate(&ctx_in(SIBLING), &request("create", CHILD))
        .await
        .unwrap_err();
    assert_eq!(unavailable.condition, EngineFailureCondition::Unavailable);
}

#[test]
fn registration_uses_the_well_known_instance() {
    let (id, payload) = registration("acme", 7).unwrap();
    assert!(id.as_ref().starts_with(ADMISSION_ENGINE_INSTANCE_ID));
    assert!(payload.to_string().contains("acme"));
}
