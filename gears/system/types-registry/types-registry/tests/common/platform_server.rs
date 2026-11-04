//! The gear's REST routes on a real TCP listener behind the `OoP` route-auth stack
//! (`layer_route_auth`, policy derived from the registered specs), with a shared-secret
//! internal authenticator and a stub bearer authenticator.

use std::sync::Arc;

use axum::Router;
use secrecy::SecretString;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_http_middleware::{RouteAuthPolicy, layer_route_auth};
use toolkit_security::{
    AuthNError, BearerAuthenticator, DynBearerAuthenticator, DynInternalAuthenticator,
    SecurityContext, SharedSecretInternalAuthenticator,
};

use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::RegistryService;
use types_registry::domain::service::TypesRegistryService;
use types_registry::infra::InMemoryGtsRepository;
use types_registry::infra::outbox::OutboxDispatch;

/// The internal token the listener validates.
pub const INTERNAL_TOKEN: &str = "types-registry-test-internal-token";
/// The bearer the stub tenant authenticator accepts.
pub const TENANT_BEARER: &str = "types-registry-test-tenant-bearer";

/// Accepts [`TENANT_BEARER`] only.
struct StubBearer;

impl BearerAuthenticator for StubBearer {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        if token == TENANT_BEARER {
            Ok(SecurityContext::builder()
                .subject_id(uuid::Uuid::from_u128(1))
                .subject_tenant_id(uuid::Uuid::from_u128(2))
                .build()
                .expect("a tenant context"))
        } else {
            Err(AuthNError::InvalidToken)
        }
    }
}

/// A served registry; dropping it stops nothing the test still needs.
pub struct PlatformServer {
    pub base_url: String,
    pub service: Arc<RegistryService>,
    _outbox: toolkit_db::outbox::OutboxHandle,
    _dir: super::TestDir,
}

/// Serve the routes over `SQLite` with running admission workers.
pub async fn serve() -> PlatformServer {
    serve_with(|app| app).await
}

/// [`serve`], with `wrap` applied outermost, ahead of authentication: it sees every
/// request as sent.
pub async fn serve_with(wrap: impl FnOnce(Router) -> Router) -> PlatformServer {
    serve_on(wrap, Some(DynBearerAuthenticator::new(StubBearer)), true).await
}

/// [`serve`] as a standalone host with no tenant authenticator in its `ClientHub`:
/// `oop_serve` then installs no tenant plane.
#[allow(dead_code, reason = "used by the auth matrix only")]
pub async fn serve_without_tenant_authenticator() -> PlatformServer {
    serve_on(|app| app, None, true).await
}

/// [`serve`] as a host whose `oop_http` configures no `internal_auth`: nothing can admit a
/// platform caller.
#[allow(dead_code, reason = "used by the auth matrix only")]
pub async fn serve_without_internal_authenticator() -> PlatformServer {
    serve_on(
        |app| app,
        Some(DynBearerAuthenticator::new(StubBearer)),
        false,
    )
    .await
}

async fn serve_on(
    wrap: impl FnOnce(Router) -> Router,
    bearer: Option<DynBearerAuthenticator>,
    internal: bool,
) -> PlatformServer {
    let dir = super::TestDir::new("tr-platform-tcp");
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("platform.db").display()
    );
    let db = super::provider_for_with_outbox(&dsn, 8).await;
    let config = TypesRegistryConfig::default();
    let legacy = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config.clone(),
    ));
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        super::stores(),
        RegistrationPolicy::default(),
        config,
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        super::metrics(),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .expect("start the admission outbox");

    let openapi = OpenApiRegistryImpl::new();
    let router = types_registry::api::rest::routes::register_routes(
        Router::new(),
        &openapi,
        legacy,
        Some(Arc::clone(&service)),
    );
    let policy: RouteAuthPolicy = openapi
        .operation_specs
        .iter()
        .map(|entry| {
            let spec = entry.value();
            (spec.method.clone(), spec.path.clone(), spec.auth)
        })
        .collect();
    let internal_authenticator = SharedSecretInternalAuthenticator::try_new(
        SecretString::from(INTERNAL_TOKEN.to_owned()),
        "types-registry-test".to_owned(),
    )
    .expect("a usable shared secret");
    let app = wrap(layer_route_auth(
        router,
        policy,
        bearer,
        internal.then(|| DynInternalAuthenticator::new(internal_authenticator)),
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a local port");
    let addr = listener.local_addr().expect("a bound address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    PlatformServer {
        base_url: format!("http://{addr}"),
        service,
        _outbox: outbox,
        _dir: dir,
    }
}
