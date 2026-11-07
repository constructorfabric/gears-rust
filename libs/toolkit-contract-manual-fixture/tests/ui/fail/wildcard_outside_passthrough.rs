//! `{*name}` wildcards belong to `#[passthrough]` templates only.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

#[toolkit::contract(gear = "local", version = "v1")]
pub trait LocalApi: Send + Sync {
    async fn one(&self, ctx: SecurityContext, rest: String) -> Result<u32, CanonicalError>;
}

#[toolkit::rest_contract(base_path = "/local/v1")]
pub trait LocalApiRest: LocalApi {
    #[get("/files/{*rest}")]
    #[server_manual]
    async fn one(&self, ctx: SecurityContext, rest: String) -> Result<u32, CanonicalError>;
}

fn main() {}
