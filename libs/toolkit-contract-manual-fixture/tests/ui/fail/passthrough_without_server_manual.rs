//! Neither side of a passthrough can be generated.

use toolkit_canonical_errors::CanonicalError;
use toolkit_contract_manual_fixture::RawBody;
use toolkit_security::SecurityContext;

#[toolkit::contract(gear = "local", version = "v1")]
pub trait LocalApi: Send + Sync {
    async fn proxy(
        &self,
        ctx: SecurityContext,
        req: http::Request<RawBody>,
    ) -> Result<http::Response<RawBody>, CanonicalError>;
}

#[toolkit::rest_contract(base_path = "/local/v1")]
pub trait LocalApiRest: LocalApi {
    #[passthrough("/proxy/{*path}", methods = [get])]
    #[client_manual]
    async fn proxy(
        &self,
        ctx: SecurityContext,
        req: http::Request<RawBody>,
    ) -> Result<http::Response<RawBody>, CanonicalError>;
}

fn main() {}
