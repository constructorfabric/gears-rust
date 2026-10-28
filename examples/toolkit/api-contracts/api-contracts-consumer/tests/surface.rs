//! The consumer's REST surface is mixed on purpose: `charge` is exposed (the
//! edge publishes it), `status` is internal (only the gear's own listener
//! serves it). The edge's publication is tested against exactly this.

use std::sync::Arc;

use axum::Router;
use cf_api_contracts_consumer::domain::ChargeProxyService;
use cf_api_contracts_consumer::rest::register_routes;
use toolkit::ClientHub;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::RouteAuth;

#[test]
fn charge_is_exposed_and_status_is_internal() {
    let openapi = OpenApiRegistryImpl::new();
    let service = Arc::new(ChargeProxyService::new(Arc::new(ClientHub::new())));
    let _router: Router = register_routes(Router::new(), &openapi, service);
    let spec = |path: &str| {
        openapi
            .operation_specs
            .iter()
            .find(|e| e.value().path == path)
            .map_or_else(|| panic!("no {path}"), |e| e.value().clone())
    };
    let charge = spec("/api-contracts-consumer/v1/charge");
    assert!(charge.exposed && charge.auth == RouteAuth::Authenticated);
    let status = spec("/api-contracts-consumer/v1/status");
    assert!(!status.exposed && status.auth == RouteAuth::Authenticated);
}
