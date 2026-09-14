use std::sync::Arc;

use parking_lot::Mutex;

use account_management_sdk::idp_service_account::IdpProvisionServiceAccountRequest;
use secrecy::ExposeSecret as _;
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::config::ServiceAccountConfig;
use crate::domain::credstore::CredStoreReader;
use crate::domain::error::{AmbiguousStage, PluginError};
use crate::domain::kc::factory::KeycloakAdminClientFactory;
use crate::domain::kc::transport::KcTransport;
// `purge_tenant_accounts` is a trait method (the tenant-deprovision cascade
// port), so the trait has to be in scope to call it on the concrete facade.
use crate::domain::ports::metrics::{
    FailureMetricsPort, FailureVariant, PluginOp, SaOp, SaOpMetricsPort,
};
use crate::domain::ports::purge::TenantAccountPurgeHook;
use crate::domain::service_account_facade::{ServiceAccountFacade, build_client_id, validate_name};
use crate::domain::system_actor::build_system_ctx;
use crate::domain::test_support::{
    StubCredStore, keycloak_cfg, noop_failure_metrics, noop_sa_metrics,
};
use crate::infra::kc_http::ReqwestKcTransport;

/// Build the resolved tenant context the AM service layer forwards on every
/// service-account call. The facade reads only `tenant_id`; `tenant_name`,
/// `tenant_type`, and the plugin-private `metadata` are present because the
/// contract carries them, not because these tests exercise them.
fn sa_tenant_ctx(tenant_id: Uuid) -> account_management_sdk::IdpTenantContext {
    account_management_sdk::IdpTenantContext::new(
        tenant_id,
        "sa-facade-test",
        gts::GtsTypeId::new(toolkit_gts::gts_id!(
            "cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"
        )),
        None,
    )
}

// -----------------------------------------------------------------------
// Recording metric stubs for emission tests
// -----------------------------------------------------------------------

struct RecordingSpMetrics {
    calls: Mutex<Vec<(SaOp, f64)>>,
}

impl RecordingSpMetrics {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(vec![]),
        })
    }

    fn recorded(&self) -> Vec<(SaOp, f64)> {
        self.calls.lock().clone()
    }
}

impl SaOpMetricsPort for RecordingSpMetrics {
    fn sa_op_duration(&self, op: SaOp, secs: f64) {
        self.calls.lock().push((op, secs));
    }
}

struct RecordingFailureMetrics {
    calls: Mutex<Vec<(PluginOp, String)>>,
}

impl RecordingFailureMetrics {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(vec![]),
        })
    }

    fn recorded(&self) -> Vec<(PluginOp, String)> {
        self.calls.lock().clone()
    }
}

impl FailureMetricsPort for RecordingFailureMetrics {
    fn failure(&self, op: PluginOp, variant: FailureVariant) {
        self.calls.lock().push((op, variant.as_str().to_owned()));
    }
}

fn build_facade_with_metrics(
    server: &MockServer,
    cfg: ServiceAccountConfig,
    sa_metrics: Arc<dyn SaOpMetricsPort>,
    failure_metrics: Arc<dyn FailureMetricsPort>,
) -> ServiceAccountFacade {
    let kc_cfg = keycloak_cfg(&server.uri());
    let cs_reader = CredStoreReader::new(Arc::new(StubCredStore), build_system_ctx(Uuid::nil()));
    let transport: Arc<dyn KcTransport> = Arc::new(ReqwestKcTransport::new(reqwest::Client::new()));
    let factory = Arc::new(KeycloakAdminClientFactory::new(
        kc_cfg,
        transport,
        cs_reader,
        Arc::new(crate::domain::test_support::NoopMetrics),
    ));
    ServiceAccountFacade::new(cfg, factory, sa_metrics, failure_metrics)
}

// -----------------------------------------------------------------------
// Pure-helper tests (Step 1)
// -----------------------------------------------------------------------

#[test]
fn client_id_is_namespaced_by_tenant() {
    let t = Uuid::nil();
    assert_eq!(build_client_id(t, "metrics"), format!("svc-{t}-metrics"));
}

#[test]
fn secret_rep_deserializes_value_into_redacting_wrapper() {
    let rep: super::SecretRep =
        serde_json::from_str(r#"{"value":"must-not-debug"}"#).expect("valid secret response");

    assert_eq!(rep.value.expose_secret(), "must-not-debug");
    assert!(!format!("{:?}", rep.value).contains("must-not-debug"));
}

#[test]
fn name_validation_rejects_bad_input() {
    for bad in ["", "UPPER", "with space", "dot.ted", &"x".repeat(41)] {
        let err = validate_name(bad).expect_err("must reject");
        assert!(
            matches!(err, PluginError::SaInvalidInput { field: Some(ref f), .. } if f == "name")
        );
    }
    assert!(validate_name("metrics-push-1").is_ok());
}

// -----------------------------------------------------------------------
// Wiremock integration tests (Step 5)
// -----------------------------------------------------------------------

const CLIENT_UUID: &str = "11111111-1111-1111-1111-111111111111";
const SA_USER_UUID: &str = "22222222-2222-2222-2222-222222222222";
const SCOPE_UUID: &str = "33333333-3333-3333-3333-333333333333";

fn sa_cfg() -> ServiceAccountConfig {
    ServiceAccountConfig {
        scope_allowlist: vec!["mon:metrics:write".into()],
        ..Default::default()
    }
}

fn build_facade(server: &MockServer, cfg: ServiceAccountConfig) -> ServiceAccountFacade {
    let kc_cfg = keycloak_cfg(&server.uri());
    let cs_reader = CredStoreReader::new(Arc::new(StubCredStore), build_system_ctx(Uuid::nil()));
    let transport: Arc<dyn KcTransport> = Arc::new(ReqwestKcTransport::new(reqwest::Client::new()));
    let factory = Arc::new(KeycloakAdminClientFactory::new(
        kc_cfg,
        transport,
        cs_reader,
        Arc::new(crate::domain::test_support::NoopMetrics),
    ));
    ServiceAccountFacade::new(cfg, factory, noop_sa_metrics(), noop_failure_metrics())
}

async fn mount_bootstrap_token(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/realms/master/protocol/openid-connect/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "t-master",
            "expires_in": 600
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn create_provisions_client_sa_attrs_scope_and_returns_secret() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // quota lookup (search=true) → empty
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    // scopes list (pre-flight, before POST)
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/client-scopes"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id": SCOPE_UUID, "name": "mon:metrics:write"}])),
        )
        .mount(&server)
        .await;

    // create → 201; assert full confidential SA-only shape + ownership attr
    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .and(body_partial_json(json!({
            "clientId": client_id,
            "publicClient": false,
            "serviceAccountsEnabled": true,
            "standardFlowEnabled": false,
            "directAccessGrantsEnabled": false,
            "implicitFlowEnabled": false,
            "authorizationServicesEnabled": false,
            "clientAuthenticatorType": "client-secret",
            "attributes": { "cf.provisioning.tenant_id": tenant.to_string() },
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    // exact lookup → rep
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": CLIENT_UUID}])))
        .mount(&server)
        .await;

    // SA user fetch
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
        })))
        .mount(&server)
        .await;

    // SA attrs PUT — assert tenant_id + user_type attributes
    Mock::given(method("PUT"))
        .and(path(format!("/admin/realms/platform/users/{SA_USER_UUID}")))
        .and(body_partial_json(json!({
            "attributes": {
                "tenant_id": [tenant.to_string()],
                "user_type": ["gts.cf.core.security.subject_service.v1~"],
            }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    // scope attach (uuid already resolved from pre-flight)
    Mock::given(method("PUT"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/default-client-scopes/{SCOPE_UUID}"
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    // secret read
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "secret", "value": "s3cr3t"})),
        )
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req = IdpProvisionServiceAccountRequest::new(
        sa_tenant_ctx(tenant),
        "metrics".into(),
        vec!["mon:metrics:write".into()],
    );
    let creds = facade
        .create_inner(&ctx, &req)
        .await
        .expect("create must succeed");
    assert_eq!(creds.client_id, client_id);
    assert_eq!(creds.client_secret.expose_secret(), "s3cr3t");
    assert!(
        creds
            .token_url
            .ends_with("/realms/platform/protocol/openid-connect/token")
    );
    assert_eq!(creds.subject_id.to_string(), SA_USER_UUID);
}

#[tokio::test]
async fn create_rejects_scope_outside_allowlist() {
    let server = MockServer::start().await;
    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req = IdpProvisionServiceAccountRequest::new(
        sa_tenant_ctx(Uuid::new_v4()),
        "metrics".into(),
        vec!["admin:everything".into()],
    );
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("must reject");
    assert!(matches!(err, PluginError::SaInvalidInput { field: Some(ref f), .. } if f == "scopes"));
}

#[tokio::test]
async fn create_rejects_when_quota_reached() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let prefix = format!("svc-{tenant}-");
    let existing: Vec<_> = (0..10)
        .map(|i| {
            json!({
                "id": Uuid::new_v4(),
                "clientId": format!("{prefix}sa{i}"),
                "attributes": { "cf.provisioning.tenant_id": [tenant.to_string()] },
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(existing)))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let err = facade.create_inner(&ctx, &req).await.expect_err("quota");
    assert!(matches!(err, PluginError::SaInvalidInput { .. }));
}

/// 409 on POST /clients is a strict reject even when the existing client
/// is owned by the same tenant: two consumers colliding on a name must
/// not silently share credentials or graft scopes onto each other's
/// client. Recovery from an Ambiguous half-create is revoke → create.
#[tokio::test]
async fn create_409_yields_invalid_input_even_for_same_tenant_owner() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // quota lookup → empty
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    // POST → 409
    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(409))
        .expect(1)
        .mount(&server)
        .await;

    // No ownership reconcile: the exact-clientId lookup must never fire,
    // even though it would report this tenant as the owner.
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": CLIENT_UUID,
            "attributes": { "cf.provisioning.tenant_id": [tenant.to_string()] },
        }])))
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("name collision must reject");
    assert!(
        matches!(
            &err,
            PluginError::SaInvalidInput { detail, field: Some(f) }
                if f == "name" && detail.contains("already exists")
        ),
        "expected SaInvalidInput 'already exists', got {err:?}"
    );
}

/// A 401 on the post-create uuid lookup must reach the reactive-401
/// wrapper (token re-mint + one retry) instead of being swallowed into
/// `AmbiguousCreated` inside the closure.
#[tokio::test]
async fn create_lookup_401_after_post_is_reminted_and_retried() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // quota lookup → empty
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    // POST → 201
    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    // exact lookup: first attempt → 401 (expired token), retry → rep.
    // Lower priority number = higher precedence in wiremock-rs.
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(401).set_body_string("token expired"))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": CLIENT_UUID}])))
        .mount(&server)
        .await;

    // SA user fetch
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
        })))
        .mount(&server)
        .await;

    // SA attrs PUT
    Mock::given(method("PUT"))
        .and(path(format!("/admin/realms/platform/users/{SA_USER_UUID}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    // secret read
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "secret", "value": "s3cr3t"})),
        )
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let creds = facade
        .create_inner(&ctx, &req)
        .await
        .expect("401 on lookup must re-mint and retry, not fail Ambiguous");
    assert_eq!(creds.client_secret.expose_secret(), "s3cr3t");
}

/// Mirror of `bind_admin_user_to_tenant`'s hijack guard: refuse to
/// overwrite a foreign `tenant_id` already present on the SA user
/// (hand-edited / inconsistent KC state).
#[tokio::test]
async fn create_refuses_to_overwrite_foreign_sa_user_tenant_id() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let other = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // quota lookup → empty
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    // POST → 201
    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    // exact lookup → rep
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": CLIENT_UUID}])))
        .mount(&server)
        .await;

    // SA user carries a FOREIGN tenant_id
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
            "attributes": { "tenant_id": [other.to_string()] },
        })))
        .mount(&server)
        .await;

    // The guard must reject BEFORE any PUT
    Mock::given(method("PUT"))
        .and(path(format!("/admin/realms/platform/users/{SA_USER_UUID}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("foreign SA tenant_id must refuse");
    assert!(
        matches!(
            &err,
            PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                detail,
            } if detail.contains("refusing to overwrite")
        ),
        "expected KcSaAttrSet guard refusal, got {err:?}"
    );
}

#[tokio::test]
async fn create_409_foreign_owner_yields_invalid_input() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();

    // quota lookup → empty
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    // No scopes — skip pre-flight list fetch

    // POST → 409 (strict reject — no ownership reconcile follows)
    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(409))
        .expect(1)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("foreign owner must reject");
    assert!(
        matches!(err, PluginError::SaInvalidInput { field: Some(ref f), .. } if f == "name"),
        "unexpected err: {err:?}"
    );
}

/// A transient-looking response to the create POST is still ambiguous: KC may
/// have committed the client before an intermediary produced the 503. The
/// transport must not replay the POST and turn that uncertainty into a 409
/// invalid-input rejection.
#[tokio::test]
async fn create_503_then_409_is_not_replayed_and_remains_ambiguous() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(409).set_body_string("already exists"))
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("uncertain create must remain ambiguous");

    assert!(
        matches!(
            err,
            PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcClientCreate,
                ..
            }
        ),
        "expected KcClientCreate ambiguity, got {err:?}"
    );
}

#[tokio::test]
async fn create_sa_attr_put_failure_is_ambiguous_with_stage() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // quota lookup → empty
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    // No scopes — skip pre-flight

    // POST → 201
    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    // exact lookup → rep
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": CLIENT_UUID}])))
        .mount(&server)
        .await;

    // SA user fetch → ok
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
        })))
        .mount(&server)
        .await;

    // SA attrs PUT → 500
    Mock::given(method("PUT"))
        .and(path(format!("/admin/realms/platform/users/{SA_USER_UUID}")))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("must fail");
    assert!(
        matches!(
            err,
            PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                ref detail,
            } if detail.contains(&client_id)
        ),
        "expected AmbiguousCreated{{stage: KcSaAttrSet, detail contains client_id}}; got {err:?}",
    );
}

#[tokio::test]
async fn create_allowlisted_scope_missing_in_realm_is_config_error_preflight() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();

    // quota lookup → empty
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    // scopes list → does NOT contain "mon:metrics:write"
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/client-scopes"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"id": SCOPE_UUID, "name": "other:scope"}])),
        )
        .mount(&server)
        .await;

    // POST must NOT be called
    let post_mock = Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount_as_scoped(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req = IdpProvisionServiceAccountRequest::new(
        sa_tenant_ctx(tenant),
        "metrics".into(),
        vec!["mon:metrics:write".into()],
    );
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("must fail pre-flight");
    assert!(
        matches!(err, PluginError::Config { .. }),
        "expected Config error, got {err:?}"
    );
    // Verify no POST happened — wiremock checks expect(0) on drop.
    drop(post_mock);
}

// -----------------------------------------------------------------------
// rotate / revoke / list tests (Task 5)
// -----------------------------------------------------------------------

/// Mount an exact clientId lookup that returns the given owner in attributes.
async fn mount_owned_client_lookup(server: &MockServer, client_id: &str, owner: Uuid) {
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": CLIENT_UUID,
            "clientId": client_id,
            "attributes": { "cf.provisioning.tenant_id": [owner.to_string()] },
        }])))
        .mount(server)
        .await;
}

#[tokio::test]
async fn rotate_returns_new_secret_for_owned_client() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    mount_owned_client_lookup(&server, &client_id, tenant).await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "secret", "value": "n3w"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Healthy account: SA attrs already bound → no repair PUT.
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
            "attributes": {
                "tenant_id": [tenant.to_string()],
                "user_type": ["gts.cf.core.security.subject_service.v1~"],
            },
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path(format!("/admin/realms/platform/users/{SA_USER_UUID}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let creds = facade
        .rotate_secret_inner(&ctx, tenant, &client_id)
        .await
        .expect("rotate must succeed");
    assert_eq!(creds.client_secret.expose_secret(), "n3w");
    assert_eq!(creds.client_id, client_id);
    assert_eq!(creds.subject_id.to_string(), SA_USER_UUID);
    assert!(
        creds
            .token_url
            .ends_with("/realms/platform/protocol/openid-connect/token")
    );
}

/// Deletion race: client vanished between the ownership lookup and the
/// SA-user read → terminal `NotFound`, not a retryable-looking `CleanFailure`
/// (mirrors the 404/410 mapping of the secret POST and revoke paths).
#[tokio::test]
async fn rotate_sa_user_404_yields_sa_not_found() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    mount_owned_client_lookup(&server, &client_id, tenant).await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .rotate_secret_inner(&ctx, tenant, &client_id)
        .await
        .expect_err("deleted client must yield not-found");
    assert!(
        matches!(err, PluginError::SaNotFound { .. }),
        "expected SaNotFound, got {err:?}"
    );
}

/// Rotate on a half-created account (create failed before the SA-attr
/// bind) refuses to hand out credentials. Rotate cannot reconstruct the
/// originally requested scopes, so the documented recovery is revoke → create.
#[tokio::test]
async fn rotate_rejects_unbound_sa_attributes() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    mount_owned_client_lookup(&server, &client_id, tenant).await;

    // SA user WITHOUT bound attributes — half-created account.
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path(format!("/admin/realms/platform/users/{SA_USER_UUID}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "secret", "value": "n3w"})),
        )
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .rotate_secret_inner(&ctx, tenant, &client_id)
        .await
        .expect_err("rotate must reject an incomplete account");
    assert!(
        matches!(err, PluginError::SaInvalidInput { field: Some(ref f), .. } if f == "client_id"),
        "expected SaInvalidInput{{field: client_id}}, got {err:?}"
    );
}

/// Migration safety: an account provisioned under a previous subject-type
/// constant must stay rotatable.
///
/// `user_type` is a deployment-wide classification constant, so its value
/// says nothing about whether *this* account was fully provisioned — only
/// its presence does. Keying account health on equality with the current
/// constant would mean a future rename retroactively marks every existing
/// account corrupt, and since only `rotate_secret` consults the flag the
/// breakage is lopsided: rotate fails fleet-wide while list and revoke keep
/// working, with re-issuing every live credential as the documented
/// recovery. The literal below is the pre-rename value this guards.
#[tokio::test]
async fn rotate_tolerates_a_legacy_user_type_value() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    mount_owned_client_lookup(&server, &client_id, tenant).await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
            "attributes": {
                "tenant_id": [tenant.to_string()],
                // Stale value from before the subject-type rename.
                "user_type": ["gts.cf.core.security.subject_service_principal.v1~"],
            },
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "secret", "value": "n3w"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let creds = facade
        .rotate_secret_inner(&ctx, tenant, &client_id)
        .await
        .expect("a legacy user_type value must not make an account unrotatable");
    assert_eq!(creds.client_secret.expose_secret(), "n3w");
}

/// The presence check still has teeth: a blank marker is not a marker, so it
/// reads as a half-created account rather than as a legacy value.
#[tokio::test]
async fn rotate_rejects_blank_user_type() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    mount_owned_client_lookup(&server, &client_id, tenant).await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
            "attributes": {
                "tenant_id": [tenant.to_string()],
                "user_type": ["   "],
            },
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "secret", "value": "n3w"})),
        )
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .rotate_secret_inner(&ctx, tenant, &client_id)
        .await
        .expect_err("a blank user_type must not count as bound");
    assert!(
        matches!(err, PluginError::SaInvalidInput { field: Some(ref f), .. } if f == "client_id"),
        "expected SaInvalidInput{{field: client_id}}, got {err:?}"
    );
}

/// The `tenant_id` half is still an equality check, not a presence check —
/// relaxing `user_type` must not relax the ownership marker with it.
#[tokio::test]
async fn rotate_rejects_sa_user_bound_to_another_tenant() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    mount_owned_client_lookup(&server, &client_id, tenant).await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
            "attributes": {
                // Present and well-formed, but a different tenant.
                "tenant_id": [Uuid::new_v4().to_string()],
                "user_type": ["gts.cf.core.security.subject_service.v1~"],
            },
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "secret", "value": "n3w"})),
        )
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .rotate_secret_inner(&ctx, tenant, &client_id)
        .await
        .expect_err("a foreign tenant_id marker must still refuse");
    assert!(
        matches!(err, PluginError::SaInvalidInput { field: Some(ref f), .. } if f == "client_id"),
        "expected SaInvalidInput{{field: client_id}}, got {err:?}"
    );
}

#[tokio::test]
async fn rotate_foreign_tenant_yields_sa_not_found() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let other = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // Owned by a DIFFERENT tenant
    mount_owned_client_lookup(&server, &client_id, other).await;

    // POST must never be called
    Mock::given(method("POST"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"value": "should-not-matter"})),
        )
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .rotate_secret_inner(&ctx, tenant, &client_id)
        .await
        .expect_err("foreign tenant must yield not-found");
    assert!(
        matches!(err, PluginError::SaNotFound { .. }),
        "expected SaNotFound, got {err:?}"
    );
}

#[tokio::test]
async fn rotate_non_service_account_client_yields_sa_not_found() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = "realm-admin-client";

    mount_owned_client_lookup(&server, client_id, tenant).await;

    Mock::given(method("POST"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": "nope"})))
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .rotate_secret_inner(&ctx, tenant, client_id)
        .await
        .expect_err("non-SP client id must yield not-found");
    assert!(
        matches!(err, PluginError::SaNotFound { .. }),
        "expected SaNotFound, got {err:?}"
    );
}

#[tokio::test]
async fn rotate_unknown_client_yields_sa_not_found() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // Exact lookup returns empty array
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .rotate_secret_inner(&ctx, tenant, &client_id)
        .await
        .expect_err("unknown client must yield not-found");
    assert!(
        matches!(err, PluginError::SaNotFound { .. }),
        "expected SaNotFound, got {err:?}"
    );
}

#[tokio::test]
async fn revoke_deletes_owned_client_and_maps_kc_404_to_not_found() {
    // Case 1: owned client, DELETE → 204 ⇒ Ok(())
    {
        let server = MockServer::start().await;
        mount_bootstrap_token(&server).await;
        let tenant = Uuid::new_v4();
        let client_id = format!("svc-{tenant}-metrics");

        mount_owned_client_lookup(&server, &client_id, tenant).await;

        Mock::given(method("DELETE"))
            .and(path(format!(
                "/admin/realms/platform/clients/{CLIENT_UUID}"
            )))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let facade = build_facade(&server, sa_cfg());
        let ctx = build_system_ctx(Uuid::nil());
        facade
            .revoke_inner(&ctx, tenant, &client_id)
            .await
            .expect("revoke must succeed on 204");
    }

    // Case 2: owned client, DELETE → 404 ⇒ SaNotFound with "already absent"
    {
        let server = MockServer::start().await;
        mount_bootstrap_token(&server).await;
        let tenant = Uuid::new_v4();
        let client_id = format!("svc-{tenant}-metrics");

        mount_owned_client_lookup(&server, &client_id, tenant).await;

        Mock::given(method("DELETE"))
            .and(path(format!(
                "/admin/realms/platform/clients/{CLIENT_UUID}"
            )))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let facade = build_facade(&server, sa_cfg());
        let ctx = build_system_ctx(Uuid::nil());
        let err = facade
            .revoke_inner(&ctx, tenant, &client_id)
            .await
            .expect_err("404 from KC must map to SaNotFound");
        assert!(
            matches!(
                &err,
                PluginError::SaNotFound { detail } if detail.contains("already absent")
            ),
            "expected SaNotFound with 'already absent', got {err:?}"
        );
    }
}

#[tokio::test]
async fn list_returns_tenant_accounts_with_scopes() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let prefix = format!("svc-{tenant}-");

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": CLIENT_UUID,
                "clientId": format!("{prefix}metrics"),
                "enabled": true,
                "defaultClientScopes": ["mon:metrics:write"],
                "attributes": { "cf.provisioning.tenant_id": [tenant.to_string()] },
            },
            {
                "id": "44444444-4444-4444-4444-444444444444",
                "clientId": "unrelated-client",
                "enabled": true,
                "defaultClientScopes": [],
            }
        ])))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let list = facade
        .list_inner(&ctx, tenant)
        .await
        .expect("list must succeed");
    assert_eq!(list.len(), 1, "substring-match noise must be filtered out");
    assert_eq!(list[0].client_id, format!("{prefix}metrics"));
    assert!(list[0].enabled);
    assert_eq!(list[0].scopes, vec!["mon:metrics:write"]);
}

#[tokio::test]
async fn list_is_empty_for_tenant_without_accounts() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let list = facade
        .list_inner(&ctx, tenant)
        .await
        .expect("list must succeed");
    assert!(list.is_empty());
}

#[tokio::test]
async fn create_quota_lookup_5xx_is_kc_rest() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();

    // quota GET → 503
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("5xx must fail");
    assert!(
        matches!(err, PluginError::KcRest { .. }),
        "expected KcRest error, got {err:?}"
    );
}

#[tokio::test]
async fn revoke_foreign_tenant_yields_sa_not_found() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let other = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // Owned by a DIFFERENT tenant
    mount_owned_client_lookup(&server, &client_id, other).await;

    // DELETE must never be called
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}"
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .revoke_inner(&ctx, tenant, &client_id)
        .await
        .expect_err("foreign tenant must yield not-found");
    assert!(
        matches!(err, PluginError::SaNotFound { .. }),
        "expected SaNotFound, got {err:?}"
    );
}

#[tokio::test]
async fn revoke_non_service_account_client_yields_sa_not_found() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = "realm-admin-client";

    mount_owned_client_lookup(&server, client_id, tenant).await;

    Mock::given(method("DELETE"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}"
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let err = facade
        .revoke_inner(&ctx, tenant, client_id)
        .await
        .expect_err("non-SP client id must yield not-found");
    assert!(
        matches!(err, PluginError::SaNotFound { .. }),
        "expected SaNotFound, got {err:?}"
    );
}

// -----------------------------------------------------------------------
// purge (tenant-deprovision cascade) tests
// -----------------------------------------------------------------------

/// Deprovision cascade: deletes every owned `svc-<tenant>-` client,
/// tolerates the concurrent-delete 404, skips substring/foreign noise.
#[tokio::test]
async fn purge_deletes_owned_clients_and_tolerates_races() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let other = Uuid::new_v4();
    let prefix = format!("svc-{tenant}-");
    let owned_a = "55555555-5555-5555-5555-555555555555";
    let owned_b = "66666666-6666-6666-6666-666666666666";
    let foreign = "77777777-7777-7777-7777-777777777777";

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": owned_a,
                "clientId": format!("{prefix}metrics"),
                "attributes": { "cf.provisioning.tenant_id": [tenant.to_string()] },
            },
            {
                "id": owned_b,
                "clientId": format!("{prefix}push"),
                "attributes": { "cf.provisioning.tenant_id": [tenant.to_string()] },
            },
            {
                // substring noise: prefix matches but owner is foreign
                "id": foreign,
                "clientId": format!("{prefix}stray"),
                "attributes": { "cf.provisioning.tenant_id": [other.to_string()] },
            },
        ])))
        .mount(&server)
        .await;

    Mock::given(method("DELETE"))
        .and(path(format!("/admin/realms/platform/clients/{owned_a}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    // Concurrent revoke already removed this one — tolerated.
    Mock::given(method("DELETE"))
        .and(path(format!("/admin/realms/platform/clients/{owned_b}")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/admin/realms/platform/clients/{foreign}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let deleted = facade
        .purge_tenant_accounts(tenant)
        .await
        .expect("purge must succeed");
    assert_eq!(deleted, 1);
}

/// A KC failure mid-purge propagates so the deprovision caller retries.
#[tokio::test]
async fn purge_propagates_kc_failures() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(503).set_body_string("kc down"))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let err = facade
        .purge_tenant_accounts(tenant)
        .await
        .expect_err("kc failure must propagate");
    assert!(
        matches!(err, PluginError::KcRest { .. }),
        "expected KcRest, got {err:?}"
    );
}

// -----------------------------------------------------------------------
// Owned-client pagination tests
// -----------------------------------------------------------------------

/// The owned-client walk MUST page to exhaustion.
///
/// Without an explicit `first`/`max` the query is bounded by Keycloak's own
/// server-side default, and every caller of `list_owned_reps` truncates
/// silently: the quota gate under-counts, `list` hides accounts, and the
/// deprovision purge leaves live credentials behind a torn-down tenant.
///
/// The fixture serves a FULL first page (`max` rows) so the walk cannot stop
/// early, then a partial second page that terminates it.
#[tokio::test]
async fn list_owned_reps_pages_past_the_first_full_page() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let prefix = format!("svc-{tenant}-");

    // Page size is the production constant; keep the fixture keyed to it so
    // a change to one forces a look at the other.
    let page_size = 100usize;
    let first_page: Vec<_> = (0..page_size)
        .map(|i| {
            json!({
                "id": Uuid::new_v4(),
                "clientId": format!("{prefix}acct{i}"),
                "attributes": { "cf.provisioning.tenant_id": [tenant.to_string()] },
            })
        })
        .collect();
    let tail_id = "88888888-8888-8888-8888-888888888888";

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .and(query_param("first", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(first_page)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .and(query_param("first", page_size.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": tail_id,
                "clientId": format!("{prefix}tail"),
                "attributes": { "cf.provisioning.tenant_id": [tenant.to_string()] },
            },
        ])))
        .expect(1)
        .mount(&server)
        .await;

    // Every listed account must be deletable, including the one that only the
    // second page reveals — that is the credential a truncating walk strands.
    Mock::given(method("DELETE"))
        .and(path_regex(r"^/admin/realms/platform/clients/.+$"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let deleted = facade
        .purge_tenant_accounts(tenant)
        .await
        .expect("purge must succeed");
    assert_eq!(
        deleted,
        page_size + 1,
        "purge must delete the second page too; a truncating walk would report {page_size}"
    );
}

// -----------------------------------------------------------------------
// URL component encoding tests
// -----------------------------------------------------------------------

/// A realm name reaches `admin_base` as a PATH segment, and `cfg.realm` is
/// free-form operator config. The query-style encoder this replaced turned a
/// space into `+`, which is a literal `+` in a path — so the adapter would
/// address a realm that does not exist while looking like it worked.
#[tokio::test]
async fn realm_with_reserved_characters_is_percent_encoded_in_the_path() {
    let server = MockServer::start().await;
    let tenant = Uuid::new_v4();

    let mut cfg = sa_cfg();
    cfg.realm = "tenant realm/one".into();

    // `%20`, not `+`; `%2F`, not a path separator that would split the segment.
    let encoded_realm = "tenant%20realm%2Fone";
    mount_bootstrap_token(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("/admin/realms/{encoded_realm}/clients")))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let facade = build_facade(&server, cfg);
    facade
        .purge_tenant_accounts(tenant)
        .await
        .expect("the encoded realm path must be the one requested");
}

// -----------------------------------------------------------------------
// Metric emission tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn create_happy_path_emits_sa_op_duration_no_failure() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": CLIENT_UUID}])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": SA_USER_UUID,
            "username": format!("service-account-{client_id}"),
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path(format!("/admin/realms/platform/users/{SA_USER_UUID}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/client-secret"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type": "secret", "value": "s3cr3t"})),
        )
        .mount(&server)
        .await;

    let sa_rec = RecordingSpMetrics::new();
    let fail_rec = RecordingFailureMetrics::new();
    let facade = build_facade_with_metrics(
        &server,
        ServiceAccountConfig::default(),
        Arc::clone(&sa_rec) as _,
        Arc::clone(&fail_rec) as _,
    );
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    facade.create_inner(&ctx, &req).await.expect("must succeed");

    let sa_calls = sa_rec.recorded();
    assert_eq!(sa_calls.len(), 1, "exactly one sa_op_duration call");
    assert_eq!(sa_calls[0].0, SaOp::Create);
    assert!(sa_calls[0].1 >= 0.0);
    assert!(fail_rec.recorded().is_empty(), "no failure on success path");
}

#[tokio::test]
async fn create_quota_reject_emits_failure_sa_create_sa_invalid_input() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let prefix = format!("svc-{tenant}-");
    let existing: Vec<_> = (0..10)
        .map(|i| {
            json!({
                "id": Uuid::new_v4(),
                "clientId": format!("{prefix}sa{i}"),
                "attributes": { "cf.provisioning.tenant_id": [tenant.to_string()] },
            })
        })
        .collect();

    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(existing)))
        .mount(&server)
        .await;

    let sa_rec = RecordingSpMetrics::new();
    let fail_rec = RecordingFailureMetrics::new();
    let facade = build_facade_with_metrics(
        &server,
        ServiceAccountConfig::default(),
        Arc::clone(&sa_rec) as _,
        Arc::clone(&fail_rec) as _,
    );
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("quota must reject");

    // Duration is recorded on failures too (mirrors user_op_duration) so
    // outage-time latency stays visible in the histogram.
    let sa_calls = sa_rec.recorded();
    assert_eq!(sa_calls.len(), 1, "duration recorded on the error path");
    assert_eq!(sa_calls[0].0, SaOp::Create);
    let failures = fail_rec.recorded();
    assert_eq!(failures.len(), 1);
    assert!(matches!(failures[0].0, PluginOp::SaCreate));
    assert_eq!(failures[0].1, "sa_invalid_input");
}

/// Persistent 401 on SA-attrs GET (both wrapper attempts) → AmbiguousCreated{KcSaAttrSet}.
/// The wrapper retries once on 401; the SA user endpoint returns 401 unconditionally,
/// so the post-mutation escape is re-tagged Ambiguous at the `create_inner` call site.
#[tokio::test]
async fn create_sa_attrs_persistent_401_is_ambiguous() {
    let server = MockServer::start().await;
    mount_bootstrap_token(&server).await;
    let tenant = Uuid::new_v4();
    let client_id = format!("svc-{tenant}-metrics");

    // quota → empty
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("search", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    // POST → 201
    Mock::given(method("POST"))
        .and(path("/admin/realms/platform/clients"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    // exact lookup → rep
    Mock::given(method("GET"))
        .and(path("/admin/realms/platform/clients"))
        .and(query_param("clientId", client_id.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id": CLIENT_UUID}])))
        .mount(&server)
        .await;

    // SA user GET → always 401 (both wrapper attempts)
    Mock::given(method("GET"))
        .and(path(format!(
            "/admin/realms/platform/clients/{CLIENT_UUID}/service-account-user"
        )))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let facade = build_facade(&server, sa_cfg());
    let ctx = build_system_ctx(Uuid::nil());
    let req =
        IdpProvisionServiceAccountRequest::new(sa_tenant_ctx(tenant), "metrics".into(), vec![]);
    let err = facade
        .create_inner(&ctx, &req)
        .await
        .expect_err("persistent 401 must yield Ambiguous");
    assert!(
        matches!(
            err,
            PluginError::AmbiguousCreated {
                stage: AmbiguousStage::KcSaAttrSet,
                ..
            }
        ),
        "expected AmbiguousCreated{{stage: KcSaAttrSet}}; got {err:?}",
    );
}
