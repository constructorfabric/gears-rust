//! A handler must implement every manual method.

use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::manual::ManualCallContext;
use toolkit_security::SecurityContext;

#[toolkit::contract(gear = "local", version = "v1")]
pub trait LocalApi: Send + Sync {
    async fn one(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
    async fn two(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
}

#[toolkit::rest_contract(base_path = "/local/v1")]
pub trait LocalApiRest: LocalApi {
    #[get("/one")]
    #[client_manual]
    #[server_manual]
    async fn one(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;

    #[get("/two")]
    #[client_manual]
    #[server_manual]
    async fn two(&self, ctx: SecurityContext) -> Result<u32, CanonicalError>;
}

pub struct Local;

impl local_api_rest_manual_sealed::Sealed for Local {}

#[async_trait::async_trait]
impl LocalApiRestManual for Local {
    async fn one(
        &self,
        _call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
    ) -> Result<u32, CanonicalError> {
        Ok(1)
    }
}

fn main() {}
