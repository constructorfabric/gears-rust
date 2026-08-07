//! ADR-0007 end-to-end: two major contract versions served side by side.
//!
//! A breaking change ships as a *parallel* trait (`PaymentApiV2`), not as an
//! edit to `PaymentApi`. This test proves the three claims the ADR makes about
//! that arrangement, plus the one property that made us reject the alternative
//! spelling (module-per-version):
//!
//! 1. both versions answer on **one** router, with different request shapes;
//! 2. `ClientHub` resolves each version independently (distinct `dyn` keys);
//! 3. every `operationId` in the shared `OpenAPI` document is **unique** — the
//!    trap that identically-named traits in different modules would fall into,
//!    silently, since the registry keys operations by `METHOD:path`;
//! 4. v2's `charge` is genuinely idempotent and its dedupe is **scoped to the
//!    caller**, so one tenant cannot replay another tenant's key.

#![allow(clippy::unwrap_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::collections::HashSet;
use std::sync::Arc;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::contract_v2::PaymentApiV2;
use api_contracts_sdk::models::{ChargeRequest, ChargeV2Request};
use api_contracts_sdk::rest::PaymentApiRestClient;
use api_contracts_sdk::rest_v2::PaymentApiV2RestClient;
use axum::Router;
use toolkit::ClientHub;
use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
use toolkit_contract::policy::PolicyStack;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use cf_api_contracts::api::rest::routes::{register_routes, register_v2_routes};
use cf_api_contracts::client::local::{PaymentApiV2LocalClient, PaymentLocalClient};
use cf_api_contracts::domain::service::PaymentDomainService;

const V1_CHARGE_PATH: &str = "/api-contracts/v1/payments/charge";
const V2_CHARGE_PATH: &str = "/api-contracts/v2/payments/charge";

fn empty_policies() -> Arc<PolicyStack> {
    Arc::new(PolicyStack::new())
}

fn anon_ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

/// A context for a *different* caller than [`anon_ctx`], to prove the v2
/// idempotency store is scoped by identity rather than by the raw key.
fn other_tenant_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .subject_type("user")
        .build()
        .unwrap()
}

/// Build both local clients over **one** domain store, mirroring how the gear
/// wires them (`gear.rs::domain_service`).
fn local_clients() -> (Arc<dyn PaymentApi>, Arc<dyn PaymentApiV2>) {
    let svc = Arc::new(PaymentDomainService::new());
    let v1: Arc<dyn PaymentApi> =
        Arc::new(PaymentLocalClient::new(Arc::clone(&svc), empty_policies()));
    let v2: Arc<dyn PaymentApiV2> = Arc::new(PaymentApiV2LocalClient::new(svc, empty_policies()));
    (v1, v2)
}

/// Serve v1 and v2 on one router (same composition the gear performs) and
/// return the base URL plus the `OpenAPI` document both versions registered into.
async fn start_dual_version_server() -> (String, serde_json::Value) {
    let (v1, v2) = local_clients();

    // The gateway would normally inject the SecurityContext; tests materialize
    // an anonymous one per request, as the other integration tests do.
    let secctx_layer = axum::middleware::from_fn(
        |mut req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| async move {
            req.extensions_mut().insert(SecurityContext::anonymous());
            next.run(req).await
        },
    );

    let openapi = OpenApiRegistryImpl::new();
    let router = register_routes(Router::new(), &openapi, v1);
    let app = register_v2_routes(router, &openapi, v2).layer(secctx_layer);

    let doc = openapi.build_openapi(&OpenApiInfo::default()).unwrap();
    let spec = serde_json::to_value(&doc).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), spec)
}

/// Every `operationId` in the document, across all paths and verbs.
#[allow(
    clippy::expect_used,
    reason = "Test-only helper: a document without a `paths` object means route registration itself broke, so failing loudly here is the desired outcome."
)]
fn operation_ids(spec: &serde_json::Value) -> Vec<String> {
    spec["paths"]
        .as_object()
        .expect("paths object")
        .values()
        .filter_map(serde_json::Value::as_object)
        .flat_map(|path_item| path_item.values())
        .filter_map(|op| op.get("operationId")?.as_str())
        .map(str::to_owned)
        .collect()
}

/// Claim 1: both versions answer on one server, each with its own body shape.
#[tokio::test]
async fn both_versions_are_served_from_one_router() {
    let (base_url, _spec) = start_dual_version_server().await;

    let v1_client = PaymentApiRestClient::new(ClientConfig::new(base_url.clone())).unwrap();
    let v1_resp = PaymentApi::charge(
        &v1_client,
        anon_ctx(),
        ChargeRequest::new(1_000, "USD", "v1 payment"),
    )
    .await
    .expect("v1 charge must succeed");

    let v2_client = PaymentApiV2RestClient::new(ClientConfig::new(base_url)).unwrap();
    let v2_resp = PaymentApiV2::charge(
        &v2_client,
        anon_ctx(),
        ChargeV2Request::new(2_000, "EUR", "idem-key-1"),
    )
    .await
    .expect("v2 charge must succeed");

    // Distinct payments, and v2 echoes back the fields its breaking change added.
    assert_ne!(v1_resp.payment_id, v2_resp.payment_id);
    assert_eq!(v2_resp.amount_minor, 2_000);
    assert_eq!(v2_resp.currency, "EUR");
}

/// Claim 1b: a payment created through v2 is readable through v2's carried-over
/// `get_invoice`, i.e. both versions sit on one domain store.
#[tokio::test]
async fn v2_charge_is_visible_to_v2_get_invoice() {
    let (base_url, _spec) = start_dual_version_server().await;
    let client = PaymentApiV2RestClient::new(ClientConfig::new(base_url)).unwrap();

    let charged = PaymentApiV2::charge(
        &client,
        anon_ctx(),
        ChargeV2Request::new(500, "USD", "idem-invoice"),
    )
    .await
    .unwrap();

    let invoice = PaymentApiV2::get_invoice(&client, anon_ctx(), charged.payment_id.to_string())
        .await
        .expect("invoice for a v2-created payment must be found");
    assert_eq!(invoice.payment_id, charged.payment_id);
}

/// Claim 2: the hub keys the two versions separately, so a consumer picks its
/// version by type at compile time.
#[test]
fn client_hub_resolves_each_version_independently() {
    let (v1, v2) = local_clients();
    let hub = ClientHub::new();
    hub.register::<dyn PaymentApi>(v1);
    hub.register::<dyn PaymentApiV2>(v2);

    assert!(hub.get::<dyn PaymentApi>().is_ok());
    assert!(hub.get::<dyn PaymentApiV2>().is_ok());
}

/// Claim 3: both versions' paths land in one document and no `operationId`
/// repeats.
///
/// This is the regression guard for the naming decision: identically-named
/// projection traits in different modules would emit the same
/// `{trait_snake}_{method}` id for `/v1/...` and `/v2/...`, and because the
/// registry keys operations by `METHOD:path` both would be kept — an invalid
/// `OpenAPI` document, with no warning.
#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "Test-only: a document without a `paths` object means route registration itself broke, so failing loudly here is the desired outcome."
)]
async fn openapi_document_covers_both_versions_with_unique_operation_ids() {
    let (_base_url, spec) = start_dual_version_server().await;

    let paths = spec["paths"].as_object().expect("paths object");
    assert!(
        paths.contains_key(V1_CHARGE_PATH),
        "v1 charge path missing; paths: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
    assert!(
        paths.contains_key(V2_CHARGE_PATH),
        "v2 charge path missing; paths: {:?}",
        paths.keys().collect::<Vec<_>>()
    );

    let ids = operation_ids(&spec);
    assert!(!ids.is_empty(), "document registered no operations");
    let unique: HashSet<&String> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "duplicate operationId in the shared OpenAPI document: {ids:?}"
    );
}

/// Claim 4a: replaying the same idempotency key returns the original payment
/// instead of charging twice — what makes v2's `charge` safe to mark
/// `#[retryable]`.
#[tokio::test]
async fn v2_charge_is_idempotent_for_the_same_key() {
    let (base_url, _spec) = start_dual_version_server().await;
    let client = PaymentApiV2RestClient::new(ClientConfig::new(base_url)).unwrap();

    let req = ChargeV2Request::new(1_500, "USD", "idem-repeat");
    let first = PaymentApiV2::charge(&client, anon_ctx(), req.clone())
        .await
        .unwrap();
    let replay = PaymentApiV2::charge(&client, anon_ctx(), req)
        .await
        .unwrap();
    assert_eq!(
        first.payment_id, replay.payment_id,
        "replaying an idempotency key must not create a second payment"
    );

    // A different key from the same caller is a genuinely new charge.
    let other = PaymentApiV2::charge(
        &client,
        anon_ctx(),
        ChargeV2Request::new(1_500, "USD", "idem-different"),
    )
    .await
    .unwrap();
    assert_ne!(first.payment_id, other.payment_id);
}

/// Claim 4b: the dedupe store is scoped by caller identity — the same key used
/// by a different tenant must NOT return the first tenant's payment.
#[tokio::test]
async fn v2_idempotency_is_scoped_per_caller() {
    // Exercised through the local client so each call carries its own
    // SecurityContext (the HTTP test layer injects an anonymous one for every
    // request, which cannot distinguish tenants).
    let (_v1, v2) = local_clients();

    let shared_key = ChargeV2Request::new(700, "USD", "same-key-two-tenants");
    let mine = v2.charge(anon_ctx(), shared_key.clone()).await.unwrap();
    let theirs = v2
        .charge(other_tenant_ctx(), shared_key)
        .await
        .expect("another tenant reusing the key must be charged separately");

    assert_ne!(
        mine.payment_id, theirs.payment_id,
        "idempotency key must be scoped by (tenant, subject); a cross-tenant \
         replay would leak the first caller's ChargeV2Response"
    );
}
