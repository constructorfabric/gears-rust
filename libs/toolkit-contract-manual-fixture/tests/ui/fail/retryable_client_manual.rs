//! The generated retry loop never runs for a manual method.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

#[toolkit::contract(gear = "local", version = "v1")]
pub trait LocalApi: Send + Sync {
    async fn one(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
}

#[toolkit::rest_contract(base_path = "/local/v1")]
pub trait LocalApiRest: LocalApi {
    #[get("/one")]
    #[retryable]
    #[client_manual]
    #[server_manual]
    async fn one(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
}

fn main() {}
