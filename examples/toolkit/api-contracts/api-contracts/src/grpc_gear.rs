//! The gear that serves `PaymentApi` v1 over gRPC (feature `grpc-server`).
//!
//! It is a gear of its own so that `api-contracts` keeps one literal
//! `#[toolkit::gear]` whatever the features, and so that only a binary built
//! with `grpc-server` declares the `grpc` capability and links grpc-hub.
//!
//! grpc-hub hosts the service and publishes it in the directory under
//! [`api_contracts_sdk::PAYMENT_API_GRPC_SERVICE`]. A consumer declared with
//! `#[toolkit::consumes(contract = PaymentApi, from = "api-contracts",
//! transport = grpc)]` resolves it by that service name. The service works on
//! the same domain store as the REST routes of `api-contracts`.
//!
//! grpc-hub checks only the platform-plane internal token, and a tenant-plane
//! call carries only the user's bearer. So a hub with `internal_auth` must run
//! with `internal_auth_enforcement: permissive`, and the service authenticates
//! the bearer itself, with the in-process `AuthN` resolver.

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverBearerAuthenticator, AuthNResolverClient};
use toolkit::contracts::{GrpcServiceCapability, RegisterGrpcServiceFn};
use toolkit::{Gear, GearCtx};
use toolkit_security::DynBearerAuthenticator;

use crate::api::grpc::PaymentApiGrpcService;
use crate::gear::domain_service;

/// Hosts the `PaymentApi` v1 gRPC service through grpc-hub.
#[toolkit::gear(name = "api-contracts-grpc", capabilities = [grpc])]
#[derive(Default)]
pub struct ApiContractsGrpc;

#[async_trait]
impl Gear for ApiContractsGrpc {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl GrpcServiceCapability for ApiContractsGrpc {
    async fn get_grpc_services(&self, ctx: &GearCtx) -> anyhow::Result<Vec<RegisterGrpcServiceFn>> {
        let authn = ctx
            .client_hub()
            .get::<dyn AuthNResolverClient>()
            .map_err(|e| anyhow::anyhow!("gRPC PaymentApi needs the AuthN resolver: {e}"))?;
        let server = PaymentApiGrpcService::new(domain_service())
            .with_authenticator(DynBearerAuthenticator::new(
                AuthNResolverBearerAuthenticator::new(authn),
            ))
            .into_server();

        Ok(vec![RegisterGrpcServiceFn {
            service_name: api_contracts_sdk::PAYMENT_API_GRPC_SERVICE,
            register: Box::new(move |routes| {
                routes.add_service(server.clone());
            }),
        }])
    }
}
