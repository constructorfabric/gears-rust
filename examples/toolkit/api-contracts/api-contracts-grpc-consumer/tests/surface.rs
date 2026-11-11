//! Every route of the gRPC consumer is published by the edge and needs a
//! caller identity: the provider authenticates the forwarded bearer.

use std::sync::Arc;

use axum::Router;
use cf_api_contracts_grpc_consumer::PaymentProxyService;
use cf_api_contracts_grpc_consumer::rest::register_routes;
use toolkit::ClientHub;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::RouteAuth;

#[test]
fn every_route_is_exposed_and_authenticated() {
    let openapi = OpenApiRegistryImpl::new();
    let service = Arc::new(PaymentProxyService::new(Arc::new(ClientHub::new())));
    let _router: Router = register_routes(Router::new(), &openapi, service);

    let mut paths: Vec<String> = openapi
        .operation_specs
        .iter()
        .map(|e| {
            let spec = e.value();
            assert!(
                spec.exposed && spec.auth == RouteAuth::Authenticated,
                "{} must be exposed and authenticated",
                spec.path
            );
            spec.path.clone()
        })
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "/api-contracts-grpc-consumer/v1/charge",
            "/api-contracts-grpc-consumer/v1/invoices/{invoice_id}",
            "/api-contracts-grpc-consumer/v1/payments",
            "/api-contracts-grpc-consumer/v1/payments/feed",
        ]
    );
}
