#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not \
              `expect`, because not every test in the file unwraps or expects"
)]
use super::*;
use bss_orders_sdk::{
    OrdersV1,
    models::{CallMeta, DraftRevision, IdempotencyKey, OrderVersion},
};
use serde_json::{Value, json};
use toolkit::HealthcheckStatus;
use toolkit::contracts::{DatabaseCapability, RestApiCapability};
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;
/// The shared Orders container start (bounded port-publication retry and TCP readiness).
#[path = "../infra/storage/tests/pg_container.rs"]
#[allow(
    clippy::duplicate_mod,
    reason = "test-only harness file shared by private test modules that cannot reach each other"
)]
mod pg_container;
#[path = "test_registry.rs"]
mod test_registry;

struct Config(Option<Value>);
impl toolkit::config::ConfigProvider for Config {
    fn get_gear_config(&self, _: &str) -> Option<&Value> {
        self.0.as_ref()
    }
}
fn config() -> Value {
    json!({"lock_route":"direct","idempotency_lease_seconds":30,"dependency_timeout_ms":2000,
        "service_principals":[{"role":"workflow","subject_id":"00000000-0000-0000-0000-000000000901","tenant_id":"00000000-0000-0000-0000-000000000001"}],
        "events":{"producer_subject_id":"00000000-0000-0000-0000-00000000044c","producer_tenant_id":"00000000-0000-0000-0000-00000000044d"},
        "audit_minimization":{"key_id":"test-1","key":TEST_KEY}})
}
/// A test-only 32-byte audit minimization key (D-204).
const TEST_KEY: &str = "unit-test-key-0123456789abcdefgh";
/// A complete maintenance block with every class connection, for config tests.
fn maintenance_block() -> Value {
    json!({
        "actor_subject_id": "00000000-0000-0000-0000-000000000903",
        "actor_tenant_id": "00000000-0000-0000-0000-000000000001",
        "tasks": ["retention_purge", "idempotency_cleanup", "audit_verification", "audit_checkpoint", "state_expiry", "draft_auto_void"],
        "connections": {
            "discovery": "postgres://d:secret-d@127.0.0.1:5432/postgres",
            "maintenance": "postgres://m:secret-m@127.0.0.1:5432/postgres",
            "retention": "postgres://r:secret-r@127.0.0.1:5432/postgres",
            "verifier": "postgres://v:secret-v@127.0.0.1:5432/postgres",
            "checkpoint": "postgres://c:secret-c@127.0.0.1:5432/postgres"
        }
    })
}
fn context(config: Option<Value>, hub: Arc<toolkit::ClientHub>) -> GearCtx {
    GearCtx::new(
        "bss-orders",
        Uuid::nil(),
        Arc::new(Config(config.map(|value| json!({"config": value})))),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
}
struct Resolver;
#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Resolver {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        _: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        panic!("the lifecycle tests never evaluate business authorization")
    }
}
fn dependencies(mode: &'static str) -> Arc<toolkit::ClientHub> {
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn authz_resolver_sdk::AuthZResolverApi>(Arc::new(Resolver));
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(
        test_registry::Registry { mode },
    ));
    hub
}

#[test]
fn config_rejects_unsafe_bounds_routes_and_typographical_errors() {
    assert!(serde_json::from_value::<OrdersConfig>(json!({})).is_err());
    // Each bad value is the valid config with only the field under test changed, so the
    // refusal is that field's: an unsafe lock route, then an unknown (typographical) member.
    let mut transaction_pool = config();
    transaction_pool["lock_route"] = json!("transaction_pool");
    let mut unknown = config();
    unknown["enabled"] = json!(true);
    for (case, value) in [
        ("lock_route", transaction_pool),
        ("unknown member", unknown),
    ] {
        assert!(
            serde_json::from_value::<OrdersConfig>(value).is_err(),
            "{case}"
        );
    }
    for (key, bad) in [
        ("idempotency_lease_seconds", 0),
        ("idempotency_lease_seconds", 86401),
        ("dependency_timeout_ms", 0),
        ("dependency_timeout_ms", 60001),
    ] {
        let mut value = config();
        value[key] = json!(bad);
        assert!(
            serde_json::from_value::<OrdersConfig>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    serde_json::from_value::<OrdersConfig>(config())
        .unwrap()
        .validate()
        .unwrap();
}

/// D-200: the producer principal is explicit; there is no event-less mode and no identity
/// shared with other principals.
#[test]
fn events_config_is_required_and_names_a_distinct_producer() {
    let mut missing = config();
    missing.as_object_mut().unwrap().remove("events");
    assert!(serde_json::from_value::<OrdersConfig>(missing).is_err());
    let mut unknown = config();
    unknown["events"]["queue"] = json!("other");
    assert!(serde_json::from_value::<OrdersConfig>(unknown).is_err());
    for (key, bad) in [
        (
            "producer_subject_id",
            json!("00000000-0000-0000-0000-000000000000"),
        ),
        (
            "producer_subject_id",
            json!("00000000-0000-0000-0000-000000000901"),
        ),
    ] {
        let mut value = config();
        value["events"][key] = bad;
        let parsed = serde_json::from_value::<OrdersConfig>(value).unwrap();
        assert!(parsed.validate().is_err(), "{key}");
    }
    let settings = serde_json::from_value::<OrdersConfig>(config())
        .unwrap()
        .producer_settings()
        .unwrap();
    assert_eq!(
        (settings.subject_id, settings.tenant_id),
        (
            Uuid::parse_str("00000000-0000-0000-0000-00000000044c").unwrap(),
            Uuid::parse_str("00000000-0000-0000-0000-00000000044d").unwrap(),
        )
    );
}

/// D-204: the audit minimization key is required configuration. A missing block, an unresolved
/// secret reference, a short key or a malformed key ID fails startup; the key never appears in
/// `Debug` output or in any startup error.
#[test]
fn audit_minimization_key_is_required_redacted_and_validated() {
    let mut missing = config();
    missing
        .as_object_mut()
        .unwrap()
        .remove("audit_minimization");
    assert!(serde_json::from_value::<OrdersConfig>(missing).is_err());
    let mut unknown = config();
    unknown["audit_minimization"]["algorithm"] = json!("sha256");
    assert!(serde_json::from_value::<OrdersConfig>(unknown).is_err());
    for (field, bad) in [
        (
            "key",
            json!("${ORDERS_AUDIT_MINIMIZATION_KEY_UNSET_CANARY_0123456789}"),
        ),
        ("key", json!("short-key-0123456789abcdef01234")),
        ("key", json!("")),
        ("key_id", json!("")),
        ("key_id", json!("k:1")),
    ] {
        let mut value = config();
        value["audit_minimization"][field] = bad;
        let parsed = serde_json::from_value::<OrdersConfig>(value).unwrap();
        let err = parsed.validate().unwrap_err().to_string();
        assert!(err.contains("audit_minimization"), "{field}: {err}");
        assert!(
            !err.contains("0123456789abcdef"),
            "no key material in errors: {err}"
        );
    }
    let parsed = serde_json::from_value::<OrdersConfig>(config()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(parsed.admin_text_key().unwrap().key_id(), "test-1");
    let debug = format!("{parsed:?} {:?}", parsed.admin_text_key().unwrap());
    assert!(
        !debug.contains(TEST_KEY),
        "Debug never prints the key: {debug}"
    );
}

/// D-115: the audit actor class comes only from configured identities; a missing or malformed
/// service-principal configuration fails startup instead of defaulting a class.
#[test]
fn service_principal_identities_are_required_and_fail_closed() {
    let mut missing = config();
    missing
        .as_object_mut()
        .unwrap()
        .remove("service_principals");
    assert!(serde_json::from_value::<OrdersConfig>(missing).is_err());
    let p = |role: &str, subject: &str, tenant: &str| json!({"role": role, "subject_id": subject, "tenant_id": tenant});
    let (a, b, t) = (
        "00000000-0000-0000-0000-000000000901",
        "00000000-0000-0000-0000-000000000902",
        "00000000-0000-0000-0000-000000000001",
    );
    let maintenance =
        json!({"actor_subject_id": a, "actor_tenant_id": t, "tasks": ["retention_purge"]});
    for (principals, maint) in [
        (json!([]), None),
        (json!([p("billing", b, t)]), None),
        (json!([p("workflow", a, t), p("billing", a, t)]), None),
        (
            json!([p("workflow", "00000000-0000-0000-0000-000000000000", t)]),
            None,
        ),
        (json!([p("workflow", a, t)]), Some(maintenance)),
    ] {
        let mut value = config();
        value["service_principals"] = principals;
        if let Some(m) = maint {
            value["maintenance"] = m;
        }
        let parsed = serde_json::from_value::<OrdersConfig>(value).unwrap();
        assert!(parsed.validate().is_err());
    }
    for bad in [
        json!([{"role": "auditor", "subject_id": a, "tenant_id": t}]),
        json!([{"role": "workflow", "subject_id": a, "tenant_id": t, "class": "system"}]),
    ] {
        let mut value = config();
        value["service_principals"] = bad;
        assert!(serde_json::from_value::<OrdersConfig>(value).is_err());
    }
}

/// A gear block with distinct maintenance, service and producer identities parses, validates
/// and classifies them: the maintenance actor is `system`, each configured principal `service`,
/// and every other subject, including a lookalike subject ID in another tenant, `user` (D-115).
#[test]
fn configured_identities_classify_system_service_and_user_distinctly() {
    use crate::domain::audit::AuditActorClass;
    let tenant = "0e0e0000-0000-4000-8000-0000000000aa";
    let principal = |role: &str, n: &str| json!({"role": role, "subject_id": format!("0e0e0000-0000-4000-8000-000000000{n}"), "tenant_id": tenant});
    let mut value = config();
    value["service_principals"] = json!([
        principal("workflow", "002"),
        principal("subscriptions", "003"),
        principal("billing", "004")
    ]);
    value["events"]["producer_subject_id"] = json!("0e0e0000-0000-4000-8000-000000000005");
    value["events"]["producer_tenant_id"] = json!(tenant);
    value["maintenance"] = json!({
        "actor_subject_id": "0e0e0000-0000-4000-8000-000000000001",
        "actor_tenant_id": tenant,
        "tasks": ["retention_purge"],
        "connections": {"retention": "postgres://r:secret-r@127.0.0.1:5432/postgres"}
    });
    let cfg = serde_json::from_value::<OrdersConfig>(value).unwrap();
    cfg.validate().unwrap();
    let ids = cfg.actor_identities().unwrap();
    let ctx = |s: &str, t: &str| {
        SecurityContext::builder()
            .subject_id(Uuid::parse_str(s).unwrap())
            .subject_tenant_id(Uuid::parse_str(t).unwrap())
            .build()
            .unwrap()
    };
    let class = |s: &str, t: &str| ids.classify(&ctx(s, t)).unwrap().class();
    assert_eq!(
        class("0e0e0000-0000-4000-8000-000000000001", tenant),
        AuditActorClass::System
    );
    for service in ["002", "003", "004"] {
        let subject = format!("0e0e0000-0000-4000-8000-000000000{service}");
        assert_eq!(class(&subject, tenant), AuditActorClass::Service);
        // A lookalike subject in another tenant is an ordinary user.
        assert_eq!(
            class(&subject, "00000000-df51-5b42-9538-d2b56b7ee953"),
            AuditActorClass::User
        );
    }
    assert_eq!(
        class(
            "11111111-6a88-4768-9dfc-6bcd5187d9ed",
            "00000000-df51-5b42-9538-d2b56b7ee953"
        ),
        AuditActorClass::User
    );
}

#[tokio::test]
async fn missing_prerequisites_never_publish_a_client_or_routes() {
    let hub = Arc::new(toolkit::ClientHub::new());
    let gear = BssOrdersGear::default();
    for (cfg, expected) in [
        (None, "explicit config"),
        (Some(config()), "AuthZResolverApi"),
    ] {
        let ctx = context(cfg, Arc::clone(&hub));
        assert!(
            gear.init(&ctx)
                .await
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        assert!(hub.get::<dyn OrdersV1>().is_err());
    }
    hub.register::<dyn authz_resolver_sdk::AuthZResolverApi>(Arc::new(Resolver));
    let ctx = context(Some(config()), Arc::clone(&hub));
    assert!(
        gear.init(&ctx)
            .await
            .unwrap_err()
            .to_string()
            .contains("TypesRegistryClient")
    );
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(
        test_registry::Registry { mode: "complete" },
    ));
    assert!(
        gear.init(&ctx)
            .await
            .unwrap_err()
            .to_string()
            .contains("database required")
    );
    assert!(hub.get::<dyn OrdersV1>().is_err());
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = gear
        .register_rest(&ctx, axum::Router::new(), &openapi)
        .unwrap();
    assert!(!router.has_routes());
    let document = serde_json::to_value(
        openapi
            .build_openapi(&toolkit::api::OpenApiInfo::default())
            .unwrap(),
    )
    .unwrap();
    assert!(document["paths"].as_object().unwrap().is_empty());
    assert!(!gear.migrations().is_empty());
    assert_eq!(
        gear.healthcheck(&ctx).unwrap().check().await.status,
        HealthcheckStatus::Unhealthy
    );
}

#[tokio::test]
async fn registry_refusal_and_incomplete_ack_fail_closed() {
    assert!(
        crate::gts::register(&test_registry::Registry { mode: "incomplete" })
            .await
            .is_err()
    );
    crate::gts::register(&test_registry::Registry { mode: "complete" })
        .await
        .unwrap();
    assert!(
        crate::gts::register(&test_registry::Registry { mode: "refused" })
            .await
            .is_err()
    );
    crate::gts::validate_catalog().unwrap();
}

/// Real PostgreSQL startup and shutdown; registry/PDP are explicit recording doubles.
/// This is not the S1-03 custom-property authorization proof.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_boot_registers_unavailable_sdk_and_shutdown_is_cooperative() -> anyhow::Result<()>
{
    let (_container, port) = pg_container::start_postgres().await?;
    let db = toolkit_db::connect_db(
        &format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
        toolkit_db::ConnectOpts::default(),
    )
    .await?;
    let hub = dependencies("complete");
    let ctx =
        context(Some(config()), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db.clone()));
    let gear = Arc::new(BssOrdersGear::default());
    // The host's DB phase runs the gear's migrations before init.
    toolkit_db::migration_runner::run_migrations_for_testing(&db, gear.migrations()).await?;
    gear.init(&ctx).await?;
    assert!(gear.init(&ctx).await.is_err(), "double init is rejected");
    let caller = SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .subject_type(toolkit_gts::gts_id!("cf.core.security.subject_user.v1~"))
        .build()?;
    let client = hub.get::<dyn OrdersV1>()?;
    let result = client
        .submit(
            &caller,
            Uuid::from_u128(3),
            DraftRevision::try_from(0)?,
            CallMeta {
                expected_version: OrderVersion::try_from(1)?,
                idempotency_key: IdempotencyKey::try_from("key".to_owned())?,
                correlation_id: None,
                delegation_proof_ref: None,
            },
        )
        .await;
    assert_eq!(
        crate::api::rest::problem(result.unwrap_err()).status,
        Some(503)
    );
    // S2-09 mounts exactly the five delivered authoring operations and the early-S6 package
    // the three draft read operations; nothing else.
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = gear.register_rest(&ctx, axum::Router::new(), &openapi)?;
    let document =
        serde_json::to_value(openapi.build_openapi(&toolkit::api::OpenApiInfo::default())?)?;
    let mut operations: Vec<String> = document["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, item)| {
            item.as_object()
                .unwrap()
                .keys()
                .map(move |method| format!("{} {path}", method.to_uppercase()))
        })
        .collect();
    operations.sort();
    assert_eq!(
        operations,
        [
            "DELETE /bss-orders/v1/orders/{orderId}/lines/{lineId}",
            "GET /bss-orders/v1/orders",
            "GET /bss-orders/v1/orders/{orderId}",
            "GET /bss-orders/v1/orders/{orderId}/lines",
            "PATCH /bss-orders/v1/orders/{orderId}",
            "PATCH /bss-orders/v1/orders/{orderId}/lines/{lineId}",
            "POST /bss-orders/v1/orders",
            "POST /bss-orders/v1/orders/{orderId}/lines",
        ]
    );
    assert_delivered_census(&router, &openapi, &document, &operations, &caller).await?;
    // Before `serve` binds the engine the routes answer unavailable: no write before readiness.
    let request = axum::http::Request::post("/bss-orders/v1/orders")
        .header("idempotency-key", "k-1")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({"resource_tenant_id": Uuid::from_u128(10), "payer_tenant_id": Uuid::from_u128(30),
                   "seller_tenant_id": Uuid::from_u128(20),
                   "category": "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1"})
            .to_string(),
        ))?;
    let response = tower::ServiceExt::oneshot(
        router.clone().layer(axum::Extension(caller.clone())),
        request,
    )
    .await?;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    // Reads answer unavailable before readiness too (no read before the parts are bound), while
    // their input validation still precedes everything.
    let request =
        axum::http::Request::get("/bss-orders/v1/orders").body(axum::body::Body::empty())?;
    let response = tower::ServiceExt::oneshot(
        router.clone().layer(axum::Extension(caller.clone())),
        request,
    )
    .await?;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    let request =
        axum::http::Request::get("/bss-orders/v1/orders?limit=0").body(axum::body::Body::empty())?;
    let response = tower::ServiceExt::oneshot(
        router.clone().layer(axum::Extension(caller.clone())),
        request,
    )
    .await?;
    assert_eq!(response.status().as_u16(), 400);
    // A missing If-Match is 428 at the boundary, before readiness or authorization.
    let request =
        axum::http::Request::patch(format!("/bss-orders/v1/orders/{}", Uuid::from_u128(3)))
            .header("idempotency-key", "k-2")
            .body(axum::body::Body::from(r#"{"fields":{"contract_id":null}}"#))?;
    let response =
        tower::ServiceExt::oneshot(router.layer(axum::Extension(caller.clone())), request).await?;
    assert_eq!(response.status().as_u16(), 428);
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(Arc::clone(&gear).serve(cancel.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !gear.running.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    // No Event Broker provider in this host: the producer is not ready and traffic stays off.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while gear.producer_readiness() == ProducerReadiness::Starting {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(
        gear.producer_readiness(),
        ProducerReadiness::Unavailable(crate::infra::broker::BindFailure::BrokerUnavailable)
    );
    assert!(gear.event_sink().is_none());
    let health = gear.healthcheck(&ctx).unwrap().check().await;
    assert_eq!(health.status, HealthcheckStatus::Unhealthy);
    assert_eq!(
        health.code.as_deref(),
        Some("event-broker-unavailable"),
        "{health:?}"
    );
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), task).await???;
    assert_eq!(gear.producer_readiness(), ProducerReadiness::Stopped);
    assert!(gear.runtime.load().is_none());
    assert!(!gear.running.load(Ordering::Acquire));
    // A failed registry acknowledgement on a real DB cannot install the local provider.
    let hub = dependencies("incomplete");
    let ctx =
        context(Some(config()), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db.clone()));
    assert!(BssOrdersGear::default().init(&ctx).await.is_err());
    assert!(hub.get::<dyn OrdersV1>().is_err());
    let hub = dependencies("hanging");
    let mut cfg = config();
    cfg["dependency_timeout_ms"] = json!(1);
    let ctx = context(Some(cfg), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db));
    let error = BssOrdersGear::default().init(&ctx).await.unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(hub.get::<dyn OrdersV1>().is_err());
    Ok(())
}

/// S2-12 route census over the mounted router and its `OpenAPI` document: the mounted set is
/// exactly `DELIVERED_OPERATIONS`, every delivered write binds the enforced identity-keyed
/// caller zone (D-185) and documents 429, no read is throttled, every undelivered catalog path
/// is unrouted (404/405) so a service-only action is unreachable from any mounted path, and the
/// request-body declarations follow D-206.
async fn assert_delivered_census(
    router: &axum::Router,
    openapi: &toolkit::api::OpenApiRegistryImpl,
    document: &Value,
    operations: &[String],
    caller: &SecurityContext,
) -> anyhow::Result<()> {
    // S2-12 census: the mounted set is exactly `DELIVERED_OPERATIONS`; every delivered write
    // binds the enforced identity-keyed caller zone (D-185) and documents 429; no read is
    // throttled; the undelivered catalog paths (submit, cancel, hold, the workflow seams, …)
    // are unrouted, so a service-only action is unreachable from any mounted path.
    let delivered: std::collections::BTreeSet<String> = crate::api::rest::DELIVERED_OPERATIONS
        .iter()
        .map(|id| {
            let route = crate::api::rest::wire::route(id).unwrap();
            format!("{} {}", route.method, route.path)
        })
        .collect();
    assert_eq!(
        operations
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        delivered
    );
    for spec in &openapi.operation_specs {
        let spec = spec.value();
        let op = crate::api::rest::wire::ROUTES
            .iter()
            .find(|op| op.method == spec.method.as_str() && op.path == spec.path)
            .unwrap();
        assert_eq!(
            spec.auth,
            toolkit::api::RouteAuth::Authenticated,
            "{}",
            op.id
        );
        if crate::api::rest::DELIVERED_WRITES.contains(&op.id) {
            let throttling = spec.throttling.as_ref().expect(op.id);
            assert_eq!(
                throttling.rate_limit_zone.as_deref(),
                Some(crate::api::rest::throttle::CALLER_WRITE_ZONE),
                "{}",
                op.id
            );
            assert!(
                throttling.require_security_context && !throttling.dry_run,
                "{}",
                op.id
            );
            assert!(
                spec.responses.iter().any(|r| r.status == 429),
                "{} documents 429",
                op.id
            );
        } else {
            assert!(
                spec.throttling.is_none(),
                "{} is not engine-entering",
                op.id
            );
        }
    }
    for op in crate::api::rest::wire::ROUTES
        .iter()
        .filter(|op| !crate::api::rest::DELIVERED_OPERATIONS.contains(&op.id))
    {
        let path = op
            .path
            .replace("{orderId}", &Uuid::from_u128(3).to_string())
            .replace("{lineId}", &Uuid::from_u128(4).to_string())
            .replace("{version}", "1");
        let request = axum::http::Request::builder()
            .method(op.method)
            .uri(path)
            .header("idempotency-key", "k-x")
            .header("if-match", "\"1\"")
            .body(axum::body::Body::empty())?;
        let response = tower::ServiceExt::oneshot(
            router.clone().layer(axum::Extension(caller.clone())),
            request,
        )
        .await?;
        // 404 for a path nothing serves; 405 where only other methods of that path are
        // mounted (e.g. `POST /orders/preview` falls on `GET|PATCH /orders/{orderId}`).
        assert!(
            matches!(response.status().as_u16(), 404 | 405),
            "{} is not mounted: {}",
            op.id,
            response.status()
        );
    }
    crate::api::rest::validate_delivered()?;
    // Only line removal has an optional body (D-206: it carries only `expected_draft_revision`).
    let paths = &document["paths"];
    let body_required = |path: &str, method: &str| {
        paths[path][method]["requestBody"]["required"].as_bool() == Some(true)
    };
    assert!(!body_required(
        "/bss-orders/v1/orders/{orderId}/lines/{lineId}",
        "delete"
    ));
    assert!(body_required(
        "/bss-orders/v1/orders/{orderId}/lines",
        "post"
    ));
    assert!(body_required("/bss-orders/v1/orders", "post"));
    // Each mounted path is the catalog's operation with its method.
    for op in crate::api::rest::wire::ROUTES.iter().filter(|op| {
        [
            "create",
            "patch_order",
            "add_line",
            "patch_line",
            "remove_line",
            "get",
            "list",
            "list_lines",
        ]
        .contains(&op.id)
    }) {
        assert!(
            operations.contains(&format!("{} {}", op.method, op.path)),
            "{}",
            op.id
        );
    }
    Ok(())
}

/// S2-10 at boot (DESIGN 02 §3.7/§3.8): the configured date policy is promoted with the
/// deployment, a repeated boot is a no-op, and a missing platform default refuses startup
/// without publishing a client.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn boot_promotes_the_date_policy_and_refuses_a_missing_default() -> anyhow::Result<()> {
    use sea_orm::ConnectionTrait;
    let (_container, port) = pg_container::start_postgres().await?;
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = toolkit_db::connect_db(&url, toolkit_db::ConnectOpts::default()).await?;
    let raw = sea_orm::Database::connect(url).await?;
    let gear = Arc::new(BssOrdersGear::default());
    toolkit_db::migration_runner::run_migrations_for_testing(&db, gear.migrations()).await?;
    let mut cfg = config();
    cfg["date_policy"] = json!({
        "platform_default": {"service_activation_required": false, "acceptance_due_required": true},
        "overrides": [{"resource_tenant_id": "00000000-0000-0000-0000-00000000000a",
            "service_activation_required": true, "acceptance_due_required": false}]
    });
    let state = || async {
        raw.query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT string_agg(concat_ws(':', resource_tenant_id, service_activation_required, acceptance_due_required, revision), ',' ORDER BY resource_tenant_id NULLS FIRST) AS s FROM bss_orders__date_policy",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "s")
        .unwrap()
    };
    for _ in 0..2 {
        let hub = dependencies("complete");
        let ctx = context(Some(cfg.clone()), Arc::clone(&hub))
            .with_db(toolkit_db::DBProvider::new(db.clone()));
        BssOrdersGear::default().init(&ctx).await?;
        // Default 1 -> 2, the override at 3, the default high-water mark 4; unchanged on reboot.
        assert_eq!(
            state().await,
            "f:t:4,00000000-0000-0000-0000-00000000000a:t:f:3"
        );
    }
    // An invalid promotion refuses startup before any write.
    let mut bad = cfg.clone();
    bad["date_policy"]["retired_overrides"] = json!(["00000000-0000-0000-0000-00000000000a"]);
    let hub = dependencies("complete");
    let ctx = context(Some(bad), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db.clone()));
    assert!(BssOrdersGear::default().init(&ctx).await.is_err());
    assert!(hub.get::<dyn OrdersV1>().is_err());
    // Without the platform default (removable only by bypassing the guards) startup fails.
    raw.execute_unprepared("ALTER TABLE bss_orders__date_policy DISABLE TRIGGER USER; DELETE FROM bss_orders__date_policy WHERE resource_tenant_id IS NULL; ALTER TABLE bss_orders__date_policy ENABLE TRIGGER USER").await?;
    for config in [config(), cfg] {
        let hub = dependencies("complete");
        let ctx = context(Some(config), Arc::clone(&hub))
            .with_db(toolkit_db::DBProvider::new(db.clone()));
        let error = BssOrdersGear::default().init(&ctx).await.unwrap_err();
        assert!(error.to_string().contains("date policy"), "{error}");
        assert!(hub.get::<dyn OrdersV1>().is_err());
    }
    Ok(())
}

#[tokio::test]
async fn unsupported_database_cannot_publish_a_client() -> anyhow::Result<()> {
    let hub = dependencies("complete");
    let db = toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default()).await?;
    let ctx = context(Some(config()), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db));
    let error = BssOrdersGear::default().init(&ctx).await.unwrap_err();
    assert!(error.to_string().contains("PostgreSQL is required"));
    assert!(hub.get::<dyn OrdersV1>().is_err());
    Ok(())
}

/// The real path: with the Event Broker gear's client and the platform root source in the hub,
/// `serve` binds the managed producer (registration, eleven schemas, worker handle), exposes the
/// sink and the transition engine built over it, and reports the producer ready; shutdown
/// withdraws both and stops the library workers. The gear stays unhealthy overall because no
/// business operation is delivered.
#[cfg(orders_event_broker_test_support)] // D-229: `with_types_registry_client` is #5381.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn serve_binds_the_real_producer_and_stops_its_workers() -> anyhow::Result<()> {
    let (_container, port) = pg_container::start_postgres().await?;
    let db = toolkit_db::connect_db(
        &format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
        toolkit_db::ConnectOpts::default(),
    )
    .await?;
    let gear = Arc::new(BssOrdersGear::default());
    toolkit_db::migration_runner::run_migrations_for_testing(&db, gear.migrations()).await?;
    let harness = event_broker::test_support::EventBrokerHarness::builder()
        .with_types_registry_client(
            crate::infra::events::test_support::registry_client(),
            &[(crate::infra::events::TOPIC, 4)],
        )
        .build()
        .await;
    let hub = dependencies("complete");
    hub.register::<dyn event_broker_sdk::EventBrokerApi>(harness.broker());
    hub.register::<dyn tenant_resolver_sdk::TenantResolverClient>(
        crate::infra::events::test_support::root(1_000, None),
    );
    let ctx = context(Some(config()), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db));
    gear.init(&ctx).await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(Arc::clone(&gear).serve(cancel.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while gear.producer_readiness() != ProducerReadiness::Ready {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    let sink = gear
        .event_sink()
        .expect("a ready producer exposes its sink");
    assert_eq!(sink.root().id(), Uuid::from_u128(1_000));
    assert!(
        gear.engine().is_some(),
        "a ready producer exposes the engine"
    );
    // S2-12: with the producer bound, the engine and reads mounted and the store answering, the
    // draft milestone is ready; the message names what later packages still withhold.
    let health = gear.healthcheck(&ctx).unwrap().check().await;
    assert_eq!(health.status, HealthcheckStatus::Healthy, "{health:?}");
    assert!(
        health
            .message
            .as_deref()
            .is_some_and(|m| m.contains("later packages answer unavailable")),
        "{health:?}"
    );
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), task).await???;
    assert_eq!(gear.producer_readiness(), ProducerReadiness::Stopped);
    assert!(gear.event_sink().is_none());
    assert!(gear.engine().is_none());
    // After shutdown the instance reports not ready again.
    let health = gear.healthcheck(&ctx).unwrap().check().await;
    assert_eq!(health.status, HealthcheckStatus::Unhealthy);
    Ok(())
}

/// S2-12 readiness loss (DESIGN §3.8): losing the store makes the running instance **not
/// ready** (code `orders-store-unavailable`) while its lifecycle task keeps running — traffic
/// stops without any restart loop — and readiness returns once the store answers again.
#[cfg(orders_event_broker_test_support)] // D-229: `with_types_registry_client` is #5381.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn store_loss_stops_readiness_without_a_restart_loop() -> anyhow::Result<()> {
    let (container, port) = pg_container::start_postgres().await?;
    let db = toolkit_db::connect_db(
        &format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
        toolkit_db::ConnectOpts {
            acquire_timeout: Some(std::time::Duration::from_millis(500)),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await?;
    let gear = Arc::new(BssOrdersGear::default());
    toolkit_db::migration_runner::run_migrations_for_testing(&db, gear.migrations()).await?;
    let harness = event_broker::test_support::EventBrokerHarness::builder()
        .with_types_registry_client(
            crate::infra::events::test_support::registry_client(),
            &[(crate::infra::events::TOPIC, 4)],
        )
        .build()
        .await;
    let hub = dependencies("complete");
    hub.register::<dyn event_broker_sdk::EventBrokerApi>(harness.broker());
    hub.register::<dyn tenant_resolver_sdk::TenantResolverClient>(
        crate::infra::events::test_support::root(1_000, None),
    );
    let ctx = context(Some(config()), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db));
    gear.init(&ctx).await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(Arc::clone(&gear).serve(cancel.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while gear.producer_readiness() != ProducerReadiness::Ready {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    let check = gear.healthcheck(&ctx).unwrap();
    assert_eq!(check.check().await.status, HealthcheckStatus::Healthy);
    // The store goes away (the container is paused, so its port mapping survives and every
    // connection attempt times out): not ready, still running, nothing restarted.
    container.pause().await?;
    let lost = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let result = check.check().await;
            if result.status == HealthcheckStatus::Unhealthy {
                return result;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await?;
    assert_eq!(
        lost.code.as_deref(),
        Some("orders-store-unavailable"),
        "{lost:?}"
    );
    assert!(gear.running.load(Ordering::Acquire));
    assert!(
        !task.is_finished(),
        "the lifecycle task survives the store loss"
    );
    assert_eq!(gear.producer_readiness(), ProducerReadiness::Ready);
    // The store returns: ready again without a restart.
    container.unpause().await?;
    let back = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let result = check.check().await;
            if result.status == HealthcheckStatus::Healthy {
                return result;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    })
    .await?;
    assert_eq!(back.status, HealthcheckStatus::Healthy);
    assert!(!task.is_finished());
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), task).await???;
    Ok(())
}

/// The code binds the D-185 gateway zones by name, and the gear-local edge limiter's settings
/// default to the baseline when absent and are bounded.
#[test]
fn throttle_zone_bindings_and_edge_limiter_settings_are_bounded() {
    assert_eq!(
        crate::api::rest::throttle::caller_write_throttling()
            .rate_limit_zone
            .as_deref(),
        Some("rl_orders_caller_write")
    );
    assert_eq!(
        crate::api::rest::throttle::workflow_write_throttling()
            .rate_limit_zone
            .as_deref(),
        Some("rl_orders_workflow_write")
    );
    // The edge limiter's settings are the baseline when absent and are bounded.
    let cfg = serde_json::from_value::<OrdersConfig>(config()).unwrap();
    assert_eq!(
        cfg.throttle_settings().unwrap(),
        crate::api::rest::throttle::ThrottleSettings::default()
    );
    for (field, bad) in [
        ("per_order_per_minute", json!(0)),
        ("per_order_per_minute", json!(21)),
        ("per_order_max_keys", json!(0)),
    ] {
        let mut value = config();
        value["throttling"] = json!({ field: bad });
        let error = serde_json::from_value::<OrdersConfig>(value)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(error.contains(field), "{field}: {error}");
    }
    let mut unknown = config();
    unknown["throttling"] = json!({"per_caller_per_minute": 200});
    assert!(serde_json::from_value::<OrdersConfig>(unknown).is_err());
}

/// S2-11: every allowlisted task needs its restricted class connection, references must be
/// resolved PostgreSQL DSNs, cadences stay inside their design bounds, and no DSN is printed.
#[test]
fn maintenance_connections_and_schedule_are_validated_and_fail_closed() {
    use crate::infra::workers::{RoleClass, WorkerSettings};
    let mut value = config();
    value["maintenance"] = maintenance_block();
    let parsed = serde_json::from_value::<OrdersConfig>(value.clone()).unwrap();
    parsed.validate().unwrap();
    let connections = parsed.maintenance_connections().unwrap();
    assert_eq!(connections.len(), 5);
    assert!(connections.contains_key(&RoleClass::Checkpoint));
    assert_eq!(
        parsed.worker_settings().unwrap(),
        WorkerSettings::baseline()
    );
    let debug = format!("{parsed:?}");
    assert!(!debug.contains("secret-"), "no DSN in Debug: {debug}");
    // A task without its class connection fails closed; the message names the class.
    let mut missing = value.clone();
    missing["maintenance"]["connections"]
        .as_object_mut()
        .unwrap()
        .remove("verifier");
    let error = serde_json::from_value::<OrdersConfig>(missing)
        .unwrap()
        .validate()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("maintenance.connections.verifier"),
        "{error}"
    );
    assert!(!error.contains("secret-"), "{error}");
    // Only the needed classes are required: retention alone needs only its own login.
    let mut only_retention = value.clone();
    only_retention["maintenance"]["tasks"] = json!(["retention_purge"]);
    only_retention["maintenance"]["connections"] =
        json!({"retention": "postgres://r:secret-r@127.0.0.1:5432/postgres"});
    let only = serde_json::from_value::<OrdersConfig>(only_retention).unwrap();
    only.validate().unwrap();
    assert_eq!(
        only.maintenance_connections()
            .unwrap()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [RoleClass::Retention]
    );
    for (field, bad) in [
        ("retention", json!("${ORDERS_RETENTION_DSN}")),
        ("retention", json!("mysql://r:x@127.0.0.1/postgres")),
        ("retention", json!("")),
    ] {
        let mut v = value.clone();
        v["maintenance"]["connections"][field] = bad;
        let error = serde_json::from_value::<OrdersConfig>(v)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("maintenance.connections.retention"),
            "{error}"
        );
    }
    let mut unknown = value.clone();
    unknown["maintenance"]["connections"]["host"] = json!("postgres://x");
    assert!(serde_json::from_value::<OrdersConfig>(unknown).is_err());
    for (field, bad) in [
        ("cleanup_interval_seconds", 0),
        ("cleanup_interval_seconds", 3601),
        ("cleanup_batch", 501),
        ("retention_interval_seconds", 59),
        ("retention_interval_seconds", 86_401),
        ("retention_batch", 5001),
        ("retention_batches_per_pass", 0),
        ("checkpoint_interval_seconds", 86_401),
        ("verification_orders_per_pass", 501),
        ("sweep_batch", 5001),
    ] {
        let mut v = value.clone();
        v["maintenance"]["schedule"] = json!({ field: bad });
        let error = serde_json::from_value::<OrdersConfig>(v)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(error.contains(field), "{field}: {error}");
    }
    let mut unknown = value;
    unknown["maintenance"]["schedule"] = json!({"purge_everything": true});
    assert!(serde_json::from_value::<OrdersConfig>(unknown).is_err());
}

/// S2-11 at boot on real PostgreSQL: the class connections are opened and attested at init
/// (a misprovisioned one refuses startup without publishing the client), the configured
/// workers are scheduled by `serve` under the host's advisory keys, run their passes through
/// their class connections, and stop with the lifecycle.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn postgres_boot_attests_class_connections_and_schedules_the_workers() -> anyhow::Result<()> {
    use crate::infra::workers::{PassResult, WorkerKind};
    use sea_orm::ConnectionTrait;
    let (_container, port) = pg_container::start_postgres().await?;
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = toolkit_db::connect_db(&url, toolkit_db::ConnectOpts::default()).await?;
    let raw = sea_orm::Database::connect(url.clone()).await?;
    let gear = Arc::new(BssOrdersGear::default());
    toolkit_db::migration_runner::run_migrations_for_testing(&db, gear.migrations()).await?;
    for class in ["maintenance", "retention", "verifier", "checkpoint"] {
        raw.execute_unprepared(&format!(
            "CREATE ROLE e2e_{class} LOGIN PASSWORD 'fixture' INHERIT; GRANT bss_orders_{class} TO e2e_{class}"
        ))
        .await?;
    }
    let dsn = |user: &str, password: &str| {
        json!(format!(
            "postgres://{user}:{password}@127.0.0.1:{port}/postgres"
        ))
    };
    let mut cfg = config();
    cfg["maintenance"] = json!({
        "actor_subject_id": "00000000-0000-0000-0000-000000000903",
        "actor_tenant_id": "00000000-0000-0000-0000-000000000001",
        "tasks": ["retention_purge", "idempotency_cleanup", "audit_verification", "audit_checkpoint"],
        "connections": {
            "maintenance": dsn("e2e_maintenance", "fixture"),
            "retention": dsn("e2e_retention", "fixture"),
            "verifier": dsn("e2e_verifier", "fixture"),
            "checkpoint": dsn("e2e_checkpoint", "fixture")
        },
        "schedule": {"cleanup_interval_seconds": 1, "verification_interval_seconds": 1}
    });
    // A misprovisioned class (the superuser as verifier) refuses init; nothing is published.
    let mut excess = cfg.clone();
    excess["maintenance"]["connections"]["verifier"] = dsn("postgres", "postgres");
    let hub = dependencies("complete");
    let ctx =
        context(Some(excess), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db.clone()));
    let error = gear.init(&ctx).await.unwrap_err().to_string();
    assert!(
        error.contains("`verifier`") && error.contains("outside"),
        "{error}"
    );
    assert!(
        !error.contains("fixture") && !error.contains("postgres:postgres"),
        "{error}"
    );
    assert!(hub.get::<dyn OrdersV1>().is_err());
    // A wrong-role login lacks its grants and refuses init too.
    let mut wrong = cfg.clone();
    wrong["maintenance"]["connections"]["retention"] = dsn("e2e_maintenance", "fixture");
    let ctx =
        context(Some(wrong), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db.clone()));
    let error = gear.init(&ctx).await.unwrap_err().to_string();
    assert!(
        error.contains("`retention`") && error.contains("lacks"),
        "{error}"
    );
    // Proper provisioning boots and schedules exactly the configured workers.
    let hub = dependencies("complete");
    let ctx = context(Some(cfg), Arc::clone(&hub)).with_db(toolkit_db::DBProvider::new(db.clone()));
    gear.init(&ctx).await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(Arc::clone(&gear).serve(cancel.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while gear.worker_status().last(WorkerKind::IdempotencyCleanup)
            != Some(PassResult::Completed)
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await?;
    assert_eq!(
        gear.scheduled_workers(),
        [
            WorkerKind::IdempotencyCleanup,
            WorkerKind::RetentionPurge,
            WorkerKind::Audit
        ]
    );
    // The daily purge ran its first pass at start; no expiry worker was scheduled.
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while gear.worker_status().last(WorkerKind::RetentionPurge) != Some(PassResult::Completed) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await?;
    assert_eq!(gear.worker_status().last(WorkerKind::Expiry), None);
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), task).await???;
    assert!(gear.scheduled_workers().is_empty());
    // Every key was released with the lifecycle.
    for key in ["idempotency-cleanup", "retention-purge"] {
        let guard = db
            .try_lock(
                crate::infra::workers::LOCK_NAMESPACE,
                key,
                toolkit_db::LockConfig {
                    max_retries: Some(0),
                    ..toolkit_db::LockConfig::default()
                },
            )
            .await?;
        guard.expect("released key").release().await?;
    }
    Ok(())
}

/// Review finding 44: the bind retry delay is fully jittered over `(0, backoff]`, and replicas
/// drawing it do not all land on the same instant.
#[test]
fn bind_backoff_is_fully_jittered_within_the_bound() {
    for backoff in [
        Duration::from_millis(250),
        Duration::from_secs(4),
        BIND_BACKOFF_MAX,
    ] {
        let draws: Vec<Duration> = (0..64).map(|_| jittered(backoff)).collect();
        assert!(draws.iter().all(|d| *d > Duration::ZERO && *d <= backoff));
        let first = draws[0];
        assert!(draws.iter().any(|d| *d != first), "jitter must vary");
    }
}
