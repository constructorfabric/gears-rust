//! Management reads the catalog as its caller and preserves registry refusals.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{SUBJECT, TENANT, WIDGET, gear_ctx_over, registry};
use policy_engine::PolicyEngine;
use policy_engine_sdk::PolicyManagementClientV1;
use policy_engine_sdk::management::{DocumentSpec, ManagementError, NewBundle, VersionContent};
use toolkit::{ClientHub, Gear};
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing_platform::{Call, Fault};

#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.core.types_registry.entity.v1~"))]
struct RegistryResource;

#[tokio::test]
async fn validation_and_activation_keep_caller_context_and_do_not_retry_refusals_as_platform() {
    for (refusal, expected, status) in [
        (
            CanonicalError::unauthenticated()
                .with_reason("CALLER_AUTH")
                .create(),
            ManagementError::Unauthenticated,
            401,
        ),
        (
            RegistryResource::permission_denied()
                .with_reason("CALLER_DENIED")
                .create(),
            ManagementError::RegistryReadDenied,
            403,
        ),
    ] {
        let hub = Arc::new(ClientHub::new());
        let registry = registry();
        registry.install(&hub);
        let ctx = gear_ctx_over(&serde_json::json!({}), Arc::clone(&hub)).await;
        PolicyEngine::default().init(&ctx).await.unwrap();
        let api = hub.get::<dyn PolicyManagementClientV1>().unwrap();
        let caller = SecurityContext::builder()
            .subject_id(SUBJECT)
            .subject_tenant_id(TENANT)
            .subject_type("operator")
            .token_scopes(vec!["*".to_owned(), "registry.read".to_owned()])
            .bearer_token("opaque-test-token")
            .build()
            .unwrap();
        let bundle = api
            .create_bundle(&caller, NewBundle::new("checked"))
            .await
            .unwrap();
        let version = api
            .create_draft_version(&caller, bundle.id, None)
            .await
            .unwrap();
        api.replace_draft_content(
            &caller,
            bundle.id,
            version.id,
            VersionContent {
                documents: vec![DocumentSpec {
                    name: "guard".to_owned(),
                    content: "package p\ndeny := false".to_owned(),
                    resource_types: vec![WIDGET.to_owned()],
                    actions: Vec::new(),
                }],
            },
        )
        .await
        .unwrap();
        let platform_reads = registry.platform_calls(Call::BatchGet);
        let mut upstream: Problem = refusal.into();
        upstream.detail = "private upstream detail".to_owned();
        let refusal = CanonicalError::try_from(upstream).unwrap();
        registry.inject(Fault::on(Call::BatchGet).fail(refusal));
        assert_eq!(
            api.validate_version(&caller, bundle.id, version.id)
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(
            api.activate_version(&caller, bundle.id, version.id)
                .await
                .unwrap_err(),
            expected
        );
        let observed = registry.caller_contexts(Call::BatchGet);
        assert_eq!(observed.len(), 2);
        for seen in observed {
            assert_eq!(seen.subject_id(), caller.subject_id());
            assert_eq!(seen.subject_tenant_id(), caller.subject_tenant_id());
            assert_eq!(seen.subject_type(), caller.subject_type());
            assert_eq!(seen.token_scopes(), caller.token_scopes());
            assert!(seen.bearer_token().is_some());
        }
        assert_eq!(registry.platform_calls(Call::BatchGet), platform_reads);
        let problem: Problem = expected.into();
        assert_eq!(problem.status, Some(status));
        assert!(!problem.detail.contains("private upstream detail"));
    }
}
