//! Reject anonymous platform methods; internal-token auth cannot be dropped.
//!
//! Contract: `cpt-cf-adr-two-plane-auth`.

use toolkit_contract::{contract, rest_contract};
use toolkit_security::PlatformSecurityContext;

#[contract(gear = "directory", version = "v1")]
pub trait DirectoryRegistrationBackend: Send + Sync {
    async fn register(
        &self,
        ctx: &PlatformSecurityContext,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

#[rest_contract(base_path = "/api/directory/v1")]
pub trait DirectoryRegistrationBackendRest: DirectoryRegistrationBackend {
    #[post("/register")]
    #[anonymous]
    async fn register(
        &self,
        ctx: &PlatformSecurityContext,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

fn main() {}
