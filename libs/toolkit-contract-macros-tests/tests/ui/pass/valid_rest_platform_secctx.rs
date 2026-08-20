//! A REST projection whose first argument is `&PlatformSecurityContext` is a
//! platform-plane method: the marker is accepted, kept off the wire, and the
//! generated client routes it to the transport-injected internal token
//! (`X-ToolKit-Internal-Token`) rather than `Authorization: Bearer`
//! (`cpt-cf-adr-two-plane-auth`). It must compile — the historic compile-time
//! rejection of platform-plane REST projections is gone.

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
    async fn register(
        &self,
        ctx: &PlatformSecurityContext,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

fn main() {}
