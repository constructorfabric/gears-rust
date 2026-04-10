//! `#[contract]` accepts the explicit `#[secctx]` parameter attribute as a
//! future-proof alternative to the `ctx: SecurityContext` name+type
//! heuristic. The attribute is consumed by the macro and must not leak into
//! the emitted trait.

use toolkit_contract::contract;

pub struct AuthInfo {
    pub _placeholder: u32,
}

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    /// `auth` is marked as a server-injected SecurityContext via the
    /// explicit attribute — the protogen back-end will filter it from the
    /// wire schema based on the IR's FieldRole.
    async fn ping(
        &self,
        #[secctx] auth: AuthInfo,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

fn main() {}
