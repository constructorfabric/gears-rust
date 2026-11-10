//! `manual_handler` without any `#[client_manual]` method.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

#[toolkit::contract(gear = "local", version = "v1")]
pub trait LocalApi: Send + Sync {
    async fn one(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
}

#[toolkit::rest_contract(base_path = "/local/v1", manual_handler = Local)]
pub trait LocalApiRest: LocalApi {
    #[get("/one")]
    #[server_manual]
    async fn one(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
}

fn main() {}
