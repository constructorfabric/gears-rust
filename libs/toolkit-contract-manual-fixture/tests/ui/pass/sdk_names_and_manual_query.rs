//! An SDK type named like a generic parameter is not shadowed by the
//! generated handler parameter, and a fully manual method is free of the
//! generated query restrictions.

use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::manual::ManualCallContext;
use toolkit_security::SecurityContext;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct H(pub String);

#[toolkit::contract(gear = "local", version = "v1")]
pub trait LocalApi: Send + Sync {
    async fn find(&self, ctx: SecurityContext, a: String, b: String) -> Result<H, CanonicalError>;
    async fn echo(&self, ctx: SecurityContext, value: H) -> Result<H, CanonicalError>;
}

#[toolkit::rest_contract(base_path = "/local/v1", manual_handler = Local)]
pub trait LocalApiRest: LocalApi {
    #[get("/find")]
    #[client_manual]
    #[server_manual]
    async fn find(&self, ctx: SecurityContext, a: String, b: String) -> Result<H, CanonicalError>;

    #[post("/echo")]
    #[client_manual]
    #[server_manual]
    async fn echo(&self, ctx: SecurityContext, value: H) -> Result<H, CanonicalError>;
}

#[derive(Default)]
pub struct Local;

impl local_api_rest_manual_sealed::Sealed for Local {}

#[async_trait::async_trait]
impl LocalApiRestManual for Local {
    async fn find(
        &self,
        _call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
        a: String,
        b: String,
    ) -> Result<H, CanonicalError> {
        Ok(H(a + &b))
    }

    async fn echo(
        &self,
        _call: &ManualCallContext<'_>,
        _ctx: SecurityContext,
        value: H,
    ) -> Result<H, CanonicalError> {
        Ok(value)
    }
}

fn main() {}
