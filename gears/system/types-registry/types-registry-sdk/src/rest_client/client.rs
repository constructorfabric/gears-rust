//! [`TypesRegistryApi`] over the tenant reads (SPEC §10.1, D17).
//!
//! The platform client's reads, on the tenant routes and under the caller's credential:
//! every attempt sends the bearer of the `SecurityContext` it was called with, which the
//! listener validates again, and never the process's internal token. One client serves
//! every caller; it holds no request's context.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_http::{HttpClient, HttpError};
use toolkit_security::SecurityContext;

use super::transport::{Credential, Transport, client_span, traced};
use super::wire::{BatchGetBody, LookupsWire, PageWire, discovery_query, parse};
use crate::contract::TypesRegistryApi;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, ListEntitiesRequest, ListEntitiesResponse,
};

/// The tenant reads' base path; T38 promotes them to `/types-registry/v1`.
pub const TENANT_BASE_PATH: &str = crate::paths::TENANT;

/// The `rpc.service` of every span, as a generated client names its trait.
const SERVICE: &str = "TypesRegistryApi";

/// The tenant contract over REST.
///
/// Both methods are safe reads, so every attempt may be retried under
/// [`ClientConfig::retry`], each within [`ClientConfig::timeout`].
/// [`ClientConfig::internal_token_provider`] is never consulted.
pub struct TypesRegistryApiRestClient {
    transport: Transport,
}

impl TypesRegistryApiRestClient {
    /// A client over the default `toolkit-http` transport, as a generated client builds it.
    ///
    /// # Errors
    /// Whatever the `toolkit-http` builder returns.
    pub fn new(config: ClientConfig) -> Result<Self, HttpError> {
        let transport = Transport::new(SERVICE, TENANT_BASE_PATH, config)?;
        Ok(Self { transport })
    }

    /// A client over a caller-built transport; only `config`'s call-time fields apply.
    #[must_use]
    pub fn with_http_client(http: HttpClient, config: ClientConfig) -> Self {
        let transport = Transport::with_http_client(http, TENANT_BASE_PATH, config);
        Self { transport }
    }
}

/// The span a generated client opens for one tenant method.
macro_rules! span {
    ($method:literal, $http_method:literal, $route:literal) => {
        client_span!(
            "TypesRegistryApi",
            crate::paths::tenant_path!(),
            $method,
            $http_method,
            $route
        )
    };
}

#[async_trait]
impl TypesRegistryApi for TypesRegistryApiRestClient {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let span = span!("batch_get_entities", "POST", "/entities:batchGet");
        traced(span, async {
            let body = BatchGetBody::encode(&request)?;
            let url = self.transport.url("/entities:batchGet");
            let answer = self
                .transport
                .exchange(
                    "batch_get_entities",
                    Credential::Bearer(ctx.bearer_token()),
                    |http| {
                        http.post(&url)
                            .json(&body)
                            .map_err(TransportError::serialization)
                    },
                )
                .await?;
            parse::<LookupsWire>("batch read", &answer.body)?.decode()
        })
        .await
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        let span = span!("list_entities", "GET", "/entities");
        traced(span, async {
            let query = toolkit_contract::query::to_query_string(&discovery_query(&request))
                .map_err(TransportError::serialization)?;
            let url = match query {
                Some(query) => format!("{}?{query}", self.transport.url("/entities")),
                None => self.transport.url("/entities"),
            };
            let answer = self
                .transport
                .exchange(
                    "list_entities",
                    Credential::Bearer(ctx.bearer_token()),
                    |http| Ok(http.get(&url)),
                )
                .await?;
            parse::<PageWire>("discovery page", &answer.body)?.decode()
        })
        .await
    }
}
