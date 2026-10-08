//! S2-12 on real PostgreSQL: the pre-engine request limiter (Foundation §3.7, D-185) and the
//! audit-failure rollback seen on the wire.
//!
//! The per-caller half runs the **real api-gateway** throttling map and middleware
//! (`api_gateway::middleware::throttling`) over the mounted Orders router, built from the very
//! `OperationSpec`s the routes register; the per-(caller, order) half is the gear-local edge
//! limiter. Both answer 429 before the engine: the registry and the audit store are unchanged by
//! a throttled attempt, and admitted retries count against the budget.
use super::capture::{BUYER, T, buyer, create, create_meta, http, line, new_order, write};
use super::*;
use crate::api::rest::throttle::{PerOrderLimiter, ThrottleSettings};
use bss_orders_lifecycle_sdk::authoring::{BillingCycle, Category};
use serde_json::{Value, json};
use std::num::NonZeroU32;
use std::sync::Arc;

const JSON: (&str, &str) = ("content-type", "application/json");

fn gateway_config(rate: &str, burst: u32) -> api_gateway::ApiGatewayConfig {
    serde_json::from_value(json!({
        "bind_addr": "127.0.0.1:0",
        "rate_limit_zones": {
            crate::api::rest::throttle::CALLER_WRITE_ZONE: {
                "rate_limit": rate,
                "burst_limit": burst,
                "response_status_code": 429,
                "response_retry_after": "auto",
                "key": {"type": "identity"},
                "max_keys": 100
            }
        }
    }))
    .unwrap()
}

/// The mounted authoring router behind the real gateway throttling middleware (post-auth
/// partition, identity-keyed), with the buyer's authenticated context, as the gateway's auth
/// layer would have placed it.
fn gateway_router(
    t: &T,
    limiter: Arc<PerOrderLimiter>,
    config: &api_gateway::ApiGatewayConfig,
) -> axum::Router {
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = crate::api::rest::capture::router(Arc::new(t.service()), limiter, &openapi);
    let specs: Vec<_> = openapi
        .operation_specs
        .iter()
        .map(|e| e.value().clone())
        .collect();
    let (map, _noauth, _pruner) =
        api_gateway::middleware::throttling::build_maps(&specs, config).unwrap();
    router
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let map = map.clone();
                api_gateway::middleware::throttling::throttling_middleware(map, req, next)
            },
        ))
        .layer(axum::Extension(buyer().ctx().clone()))
}

fn baseline() -> Arc<PerOrderLimiter> {
    PerOrderLimiter::new(ThrottleSettings::default())
}

fn per_order(per_minute: u32) -> Arc<PerOrderLimiter> {
    PerOrderLimiter::new(ThrottleSettings {
        per_order_per_minute: NonZeroU32::new(per_minute).unwrap(),
        per_order_max_keys: 100,
    })
}

fn create_body() -> Value {
    json!({"resource_tenant_id": u(10), "payer_tenant_id": u(30),
           "seller_tenant_id": u(20), "category": Category::NEW_SALE})
}

fn line_body(revision: i64) -> Value {
    let mut body = serde_json::to_value(line("EUR", Some(BillingCycle::Month))).unwrap();
    body["expected_draft_revision"] = json!(revision);
    body
}

async fn add_line(
    router: &axum::Router,
    order: Uuid,
    key: &str,
    revision: i64,
) -> (u16, axum::http::HeaderMap, Value) {
    http(
        router,
        "POST",
        &format!("/bss-orders-lifecycle/v1/orders/{order}/lines"),
        &[JSON, ("if-match", "\"1\""), ("idempotency-key", key)],
        Some(line_body(revision)),
    )
    .await
}

fn assert_gateway_429(response: &(u16, axum::http::HeaderMap, Value)) {
    assert_eq!(response.0, 429, "{:?}", response.2);
    assert_eq!(response.1["content-type"], "application/problem+json");
    assert!(response.1.contains_key("retry-after"));
    assert!(response.1.contains_key("ratelimit-policy"));
    assert_eq!(
        response.2["type"],
        "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
    );
    assert_eq!(
        response.2["context"]["violations"][0]["subject"],
        "throttling"
    );
}

/// D-185 per caller: the real gateway zone (here `1/s`, burst 2, identity-keyed) admits the
/// burst, counts a same-key replay as a request, and rejects the next attempt with the canonical
/// 429 **before** the handler: no registry row, no audit row, no order. The zone is per caller,
/// so another order of the same caller shares the budget.
#[tokio::test]
async fn the_gateway_caller_zone_rejects_before_the_engine_and_counts_retries() {
    let t = T::new().await;
    let config = gateway_config("1/s", 2);
    let router = gateway_router(&t, baseline(), &config);
    let first = http(
        &router,
        "POST",
        "/bss-orders-lifecycle/v1/orders",
        &[JSON, ("idempotency-key", "g-1")],
        Some(create_body()),
    )
    .await;
    assert_eq!(first.0, 201, "{:?}", first.2);
    assert!(first.1.contains_key("ratelimit-remaining"));
    let order = first.2["order"]["order_id"].as_str().unwrap().to_owned();
    // A legitimate retry counts against the budget (a request limiter, not a refusal limiter).
    let replay = http(
        &router,
        "POST",
        "/bss-orders-lifecycle/v1/orders",
        &[JSON, ("idempotency-key", "g-1")],
        Some(create_body()),
    )
    .await;
    assert_eq!(replay.0, 201);
    assert_eq!(replay.2["order"]["order_id"], order);
    let (audits, registry, orders) = (
        t.n("SELECT count(*) AS n FROM bss_orders__transition_audit")
            .await,
        t.registry().await,
        t.n("SELECT count(*) AS n FROM bss_orders__order").await,
    );
    assert_eq!((audits, registry, orders), (1, 1, 1));
    // Burst exhausted: the next attempt on either operation is refused at the edge.
    let third = http(
        &router,
        "POST",
        "/bss-orders-lifecycle/v1/orders",
        &[JSON, ("idempotency-key", "g-2")],
        Some(create_body()),
    )
    .await;
    assert_gateway_429(&third);
    let fourth = add_line(&router, Uuid::parse_str(&order).unwrap(), "g-3", 0).await;
    assert_gateway_429(&fourth);
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__transition_audit")
            .await,
        audits,
        "a throttled attempt writes no audit row"
    );
    assert_eq!(t.registry().await, registry, "and settles no key");
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__order").await,
        orders
    );
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__draft_content")
            .await,
        0
    );
    // A fresh zone state (another gateway replica) has its own budget: the same request is
    // admitted and enters the engine.
    let replica = gateway_router(&t, baseline(), &config);
    let admitted = add_line(&replica, Uuid::parse_str(&order).unwrap(), "g-3", 0).await;
    assert_eq!(admitted.0, 200, "{:?}", admitted.2);
    assert_eq!(t.registry().await, registry + 1);
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__transition_audit")
            .await,
        audits + 1
    );
    // The configured E2E zone (3/s, burst 20) binds the same operations: its startup validation
    // accepts them and a fresh instance admits the next write into the engine.
    let e2e = gateway_router(&t, baseline(), &gateway_config("3/s", 20));
    let admitted = add_line(&e2e, Uuid::parse_str(&order).unwrap(), "g-4", 1).await;
    assert_eq!(admitted.0, 200, "{:?}", admitted.2);
    assert!(admitted.1.contains_key("ratelimit-remaining"));
    assert_eq!(t.registry().await, registry + 2);
}

/// Q-26 fallback per (caller, order): the gear-local edge limiter (here 2 per minute) runs
/// **before boundary validation**, counts every request (admitted, refused at the boundary and
/// replayed alike), is keyed per order, never bounds create, and a rejection writes no registry,
/// audit or line row and carries the canonical problem with `Retry-After`.
#[tokio::test]
async fn the_per_order_edge_limiter_rejects_before_boundary_validation_and_writes_nothing() {
    let t = T::new().await;
    let svc = Arc::new(t.service());
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = crate::api::rest::capture::router(Arc::clone(&svc), per_order(2), &openapi)
        .layer(axum::Extension(buyer().ctx().clone()));
    // Create is bounded only by the gateway caller zone: three creates pass the edge limiter.
    let mut orders = Vec::new();
    for key in ["p-c1", "p-c2", "p-c3"] {
        let created = http(
            &router,
            "POST",
            "/bss-orders-lifecycle/v1/orders",
            &[JSON, ("idempotency-key", key)],
            Some(create_body()),
        )
        .await;
        assert_eq!(created.0, 201, "{:?}", created.2);
        orders.push(Uuid::parse_str(created.2["order"]["order_id"].as_str().unwrap()).unwrap());
    }
    let (a, b) = (orders[0], orders[1]);
    let first = add_line(&router, a, "p-1", 0).await;
    assert_eq!(first.0, 200, "{:?}", first.2);
    let second = add_line(&router, a, "p-2", 1).await;
    assert_eq!(second.0, 200, "{:?}", second.2);
    let (audits, registry, lines) = (
        t.audits(a).await,
        t.registry().await,
        t.n("SELECT count(*) AS n FROM bss_orders__draft_content")
            .await,
    );
    // Third request on A: refused at the edge with the canonical problem and no effect.
    let third = add_line(&router, a, "p-3", 2).await;
    assert_eq!(third.0, 429, "{:?}", third.2);
    assert_eq!(third.1["content-type"], "application/problem+json");
    assert!(
        third.1["retry-after"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= 1
    );
    assert!(
        !third.1.contains_key("ratelimit-policy"),
        "not a gateway zone"
    );
    assert_eq!(
        third.2["type"],
        "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
    );
    assert_eq!(third.2["context"]["violations"][0]["subject"], "throttling");
    assert_eq!(
        third.2["context"]["violations"][0]["description"],
        "per-order request limit exceeded"
    );
    // Before boundary validation: a request that would be 428 is still counted and refused 429.
    let malformed = http(
        &router,
        "PATCH",
        &format!("/bss-orders-lifecycle/v1/orders/{a}"),
        &[JSON, ("idempotency-key", "p-4")],
        Some(json!({"fields": {"contract_id": null}})),
    )
    .await;
    assert_eq!(malformed.0, 429);
    // A replay of a committed key counts too (it never reaches the registry).
    let replay = add_line(&router, a, "p-1", 0).await;
    assert_eq!(replay.0, 429);
    assert_eq!(
        (
            t.audits(a).await,
            t.registry().await,
            t.n("SELECT count(*) AS n FROM bss_orders__draft_content")
                .await
        ),
        (audits, registry, lines),
        "a throttled attempt touches no store"
    );
    let row = t.order_row(a).await;
    assert_eq!(row["draft_revision"], json!(2));
    // Keyed per order: the same caller's other order has its own budget.
    let other = add_line(&router, b, "p-5", 0).await;
    assert_eq!(other.0, 200, "{:?}", other.2);
    // And per caller: the service entry used by the SDK is not edge-limited (in-process traffic).
    let sdk = svc
        .add_line(
            &buyer(),
            a,
            line("EUR", Some(BillingCycle::Month)),
            &write("p-sdk", 1, Some(2)),
        )
        .await
        .unwrap();
    assert_eq!(sdk.status, 200, "{:?}", sdk.body);
    assert_eq!(BUYER.0, 102);
}

/// S2-12 integration: an admitted draft write whose audit append fails is rolled back as one
/// transaction on the wire — sanitized failure, no line, no registry row, no revision change — and
/// the same key succeeds once the audit store answers again. (The S2-08 outbox-failure rollback
/// is proven by `events::a_failed_enqueue_never_commits_even_if_the_error_is_dropped`; create and
/// draft mutation are eventless.)
#[tokio::test]
async fn an_audit_failure_rolls_back_the_admitted_write_on_the_wire() {
    let t = T::new().await;
    let svc = Arc::new(t.service());
    let order = create(&t, &svc, "a-c").await;
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = crate::api::rest::capture::router(Arc::clone(&svc), baseline(), &openapi)
        .layer(axum::Extension(buyer().ctx().clone()));
    let before = (
        t.audits(order).await,
        t.registry().await,
        t.n("SELECT count(*) AS n FROM bss_orders__draft_content")
            .await,
        t.n("SELECT count(*) AS n FROM bss_orders__order_line_identity")
            .await,
    );
    t.env
        .pg
        .sql("REVOKE INSERT ON bss_orders__transition_audit FROM bss_orders_runtime")
        .await
        .unwrap();
    let failed = add_line(&router, order, "a-1", 0).await;
    assert!(
        matches!(failed.0, 500 | 503),
        "sanitized infrastructure failure: {failed:?}"
    );
    assert_eq!(failed.1["content-type"], "application/problem+json");
    let text = failed.2.to_string();
    assert!(
        !text.contains("permission denied") && !text.contains("transition_audit"),
        "no store detail on the wire: {text}"
    );
    assert_eq!(
        (
            t.audits(order).await,
            t.registry().await,
            t.n("SELECT count(*) AS n FROM bss_orders__draft_content")
                .await,
            t.n("SELECT count(*) AS n FROM bss_orders__order_line_identity")
                .await
        ),
        before,
        "everything rolled back"
    );
    assert_eq!(t.order_row(order).await["draft_revision"], json!(0));
    t.env
        .pg
        .sql("GRANT INSERT ON bss_orders__transition_audit TO bss_orders_runtime")
        .await
        .unwrap();
    let retried = add_line(&router, order, "a-1", 0).await;
    assert_eq!(retried.0, 200, "{:?}", retried.2);
    assert_eq!(retried.2["draft_revision"], json!(1));
    assert_eq!(t.audits(order).await, before.0 + 1);
    assert_eq!(t.registry().await, before.1 + 1);
    let _ = (create_meta("unused"), new_order(Category::NewSale));
}

/// S2-12 boundary wiring: a malformed path parameter on any mounted write or read route is the
/// platform's canonical `invalid_argument` Problem (the toolkit `extract::Path`), never axum's
/// plain-text rejection, and it is answered before the limiter, the registry, the store and the
/// access log — nothing is counted or written.
#[tokio::test]
async fn malformed_path_parameters_are_canonical_problems_before_any_effect() {
    let t = T::new().await;
    let svc = Arc::new(t.service());
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let limiter = per_order(1);
    let router =
        crate::api::rest::capture::router(Arc::clone(&svc), Arc::clone(&limiter), &openapi)
            .merge(crate::api::rest::read::router(
                Arc::new(t.reads()),
                &openapi,
            ))
            .layer(axum::Extension(buyer().ctx().clone()));
    let before = (
        t.registry().await,
        t.n("SELECT count(*) AS n FROM bss_orders__transition_audit")
            .await,
        t.n("SELECT count(*) AS n FROM bss_orders__read_access_log")
            .await,
    );
    let write_headers = [JSON, ("if-match", "\"1\""), ("idempotency-key", "m-1")];
    for (method, path, body) in [
        (
            "PATCH",
            "/bss-orders-lifecycle/v1/orders/not-a-uuid",
            Some(json!({"fields": {"contract_id": null}})),
        ),
        (
            "POST",
            "/bss-orders-lifecycle/v1/orders/not-a-uuid/lines",
            Some(line_body(0)),
        ),
        (
            "PATCH",
            &format!(
                "/bss-orders-lifecycle/v1/orders/{}/lines/not-a-uuid",
                Uuid::from_u128(3)
            ),
            Some(json!({"fields": {"currency": "USD"}})),
        ),
        (
            "DELETE",
            "/bss-orders-lifecycle/v1/orders/not-a-uuid/lines/also-not",
            None,
        ),
        ("GET", "/bss-orders-lifecycle/v1/orders/not-a-uuid", None),
        (
            "GET",
            "/bss-orders-lifecycle/v1/orders/not-a-uuid/lines",
            None,
        ),
    ] {
        let response = http(&router, method, path, &write_headers, body).await;
        assert_eq!(response.0, 400, "{method} {path}: {:?}", response.2);
        assert_eq!(
            response.1["content-type"], "application/problem+json",
            "{method} {path}"
        );
        assert_eq!(
            response.2["type"], "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "{method} {path}"
        );
        assert_eq!(
            response.2["context"]["field_violations"][0]["field"], "path",
            "{method} {path}"
        );
        assert!(
            !response.2.to_string().contains("not-a-uuid")
                || response.2["context"]["field_violations"][0]["description"].is_string(),
            "the rejection names only the offending segment"
        );
    }
    // Rejected before the edge limiter: the (caller, order) budget is untouched, so the first
    // well-formed request on any order is still admitted by the limiter.
    assert_eq!(limiter.tracked_keys(), 0);
    assert_eq!(
        (
            t.registry().await,
            t.n("SELECT count(*) AS n FROM bss_orders__transition_audit")
                .await,
            t.n("SELECT count(*) AS n FROM bss_orders__read_access_log")
                .await
        ),
        before,
        "nothing counted, nothing written"
    );
}
