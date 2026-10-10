//! The plugin through its plugin client and the real resolver SDK PEP/compiler.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Fixed test fixtures must fail loudly.
use authz_resolver_sdk::pep::ResourceType;
use authz_resolver_sdk::{
    AuthZResolverApi, AuthZResolverPluginClient, EnforcerError, EvaluationRequest,
    EvaluationResponse, PolicyEnforcer,
};
use rules_authz_plugin::config::RulesAuthZPluginConfig;
use rules_authz_plugin::domain::Service;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

struct ViaPluginClient(Arc<dyn AuthZResolverPluginClient>);
#[async_trait::async_trait]
impl AuthZResolverApi for ViaPluginClient {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.0
            .evaluate(request)
            .await
            .map_err(|_| CanonicalError::service_unavailable().create())
    }
}

static THING: ResourceType = ResourceType::from_static(
    "gts.cf.example.thing.v1~",
    &["seller_tenant_id", "payer_tenant_id", "id"],
);

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn caller(subject: u128) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(u(subject))
        .subject_tenant_id(u(500))
        .subject_type("gts.cf.core.security.subject_user.v1~")
        .build()
        .unwrap()
}
fn enforcer(policy: serde_json::Value) -> PolicyEnforcer {
    let config: RulesAuthZPluginConfig = serde_json::from_value(policy).unwrap();
    let plugin: Arc<dyn AuthZResolverPluginClient> =
        Arc::new(Service::from_config(&config).unwrap());
    PolicyEnforcer::new(Arc::new(ViaPluginClient(plugin)))
}

#[tokio::test]
async fn compiled_scope_keeps_paths_and_unknown_properties_fail_closed() {
    let pep = enforcer(serde_json::json!({
        "vendor": "constructorfabric", "priority": 10, "policy_revision": "rev-sdk",
        "rules": [
            {"id": "seller", "subject": {"id": u(1)}, "resource_type": "gts.cf.example.thing.v1~",
             "actions": ["read"], "paths": [
                {"predicates": [{"property": "seller_tenant_id", "values": [u(2)]}]},
                {"predicates": [{"property": "id", "values": [u(9)]},
                                {"property": "payer_tenant_id", "values": [u(3)]}]}]},
            {"id": "owner-shape", "subject": {"id": u(4)}, "resource_type": "gts.cf.example.thing.v1~",
             "actions": ["read"], "paths": [
                {"predicates": [{"property": "owner_tenant_id", "values": [u(2)]}]}]}
        ]
    }));
    let scope = pep
        .access_scope(&caller(1), &THING, "read", None)
        .await
        .unwrap();
    assert_eq!(scope.constraints().len(), 2);
    assert!(scope.contains_uuid("seller_tenant_id", u(2)));
    // A path on a property the PEP does not support fails compilation: no broadened scope.
    assert!(matches!(
        pep.access_scope(&caller(4), &THING, "read", None).await,
        Err(EnforcerError::CompileFailed(_))
    ));
    // An unknown subject is an explicit PDP denial with a reason.
    match pep.access_scope(&caller(5), &THING, "read", None).await {
        Err(EnforcerError::Denied { deny_reason }) => {
            assert_eq!(deny_reason.unwrap().error_code, "no_matching_rule");
        }
        other => panic!("expected denial, got {other:?}"),
    }
}
