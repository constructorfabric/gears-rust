use std::sync::Arc;

use quota_resolver_sdk::models::{PlatformDebitRequest, PrincipalSubject, SubjectInput};
use quota_resolver_sdk::{PlatformQuotaResolverApi, QuotaResolverApi};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::{OpenApiInfo, OpenApiRegistry, OpenApiRegistryImpl};
use toolkit::client_hub::ClientHubError;
use toolkit::config::ConfigProvider;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use super::{QuotaResolver, refuse_remote_wiring};
use crate::domain::test_support::*;

struct StaticConfigProvider {
    root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

fn ctx_for(env: &Env, mapping: &[(&str, &str)]) -> GearCtx {
    let mapping: serde_json::Map<String, serde_json::Value> = mapping
        .iter()
        .map(|(k, v)| ((*k).to_owned(), json!(v)))
        .collect();
    let cfg = json!({ "quota-resolver": { "config": { "vendor": VENDOR, "principal_subject_types": mapping } } });
    GearCtx::new(
        QuotaResolver::MODULE_NAME,
        Uuid::from_u128(1),
        Arc::new(StaticConfigProvider { root: cfg }),
        env.hub.clone(),
        CancellationToken::new(),
    )
}

fn debit_request() -> PlatformDebitRequest {
    PlatformDebitRequest {
        tenant_id: tenant(),
        subjects: vec![SubjectInput::Principal(PrincipalSubject {
            subject_type: USER_TYPE.to_owned(),
            id: principal_id(),
        })],
        items: vec![item()],
        idempotency_key: "k".to_owned(),
    }
}

#[tokio::test]
async fn init_registers_both_contracts_and_the_plugin_is_resolved() {
    let env = Env::new();
    let ctx = ctx_for(&env, &[(USER_TYPE, USER_SUBJECT)]);

    QuotaResolver.init(&ctx).await.expect("init must succeed");

    let platform = ctx
        .client_hub()
        .get::<dyn PlatformQuotaResolverApi>()
        .expect("platform contract registered");
    ctx.client_hub()
        .get::<dyn QuotaResolverApi>()
        .expect("tenant contract registered");
    assert!(
        ctx.client_hub()
            .try_get::<crate::domain::QuotaResolverLocalClient>()
            .is_none(),
        "the client is held in the hub only while wiring"
    );
    let decision = platform
        .debit(PlatformSecurityContext::outbound_marker(), debit_request())
        .await
        .unwrap();
    assert!(decision.allowed);
    assert_eq!(
        env.plugin.debits.lock().unwrap()[0].subjects[0].subject_type,
        USER_SUBJECT
    );
}

#[tokio::test]
async fn init_fails_on_an_invalid_principal_mapping() {
    let env = Env::new();
    let ctx = ctx_for(&env, &[(USER_TYPE, "gts.cf.core.quota.subj.v1~")]);

    assert!(QuotaResolver.init(&ctx).await.is_err());
}

#[tokio::test]
async fn register_rest_mounts_both_projections() {
    let env = Env::new();
    let ctx = ctx_for(&env, &[]);
    QuotaResolver.init(&ctx).await.unwrap();

    let registry = OpenApiRegistryImpl::new();
    let _router = QuotaResolver
        .register_rest(&ctx, axum::Router::new(), &registry)
        .expect("register_rest must mount the routes");

    let spec =
        serde_json::to_value(registry.build_openapi(&OpenApiInfo::default()).unwrap()).unwrap();
    assert!(spec["paths"]["/quota-resolver/v1/platform/debit"]["post"].is_object());
    assert_eq!(
        spec["paths"]["/quota-resolver/v1/quota-state"]["post"]["x-throttling-rate-limit-zone"],
        json!(quota_resolver_sdk::rest::QUOTA_STATE_RATE_LIMIT_ZONE)
    );
}

#[test]
fn register_rest_fails_before_init() {
    let env = Env::new();
    let openapi: &dyn OpenApiRegistry = &OpenApiRegistryImpl::new();
    let err = QuotaResolver
        .register_rest(&ctx_for(&env, &[]), axum::Router::new(), openapi)
        .expect_err("no api in the hub, no route");
    assert!(
        matches!(
            err.downcast_ref::<ClientHubError>(),
            Some(ClientHubError::NotFound { .. })
        ),
        "got: {err:?}"
    );
}

fn provider(gear_config: serde_json::Value) -> StaticConfigProvider {
    let mut root = serde_json::Map::new();
    root.insert("quota-resolver".to_owned(), gear_config);
    StaticConfigProvider {
        root: serde_json::Value::Object(root),
    }
}

#[test]
fn a_remote_binding_for_a_provided_contract_is_refused() {
    for key in ["platform_quota_resolver_api", "quota_resolver_api"] {
        let cfg = provider(json!({
            "clients": { "provides": { key: { "transport": "rest", "endpoint": "http://r:1" } } }
        }));
        let err = refuse_remote_wiring(&cfg, "quota-resolver")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("clients.provides.{key}")),
            "names the entry: {err}"
        );
    }
}

#[test]
fn local_or_absent_wiring_is_accepted() {
    for cfg in [
        provider(json!({ "config": { "vendor": VENDOR } })),
        provider(json!({
            "clients": { "provides": { "quota_resolver_api": { "transport": "local" } } }
        })),
    ] {
        refuse_remote_wiring(&cfg, "quota-resolver").unwrap();
    }
}
