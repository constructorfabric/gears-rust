//! Platform-plane REST projections are rejected at compile time (H-1): the
//! generated client cannot yet source the internal token, so it would emit an
//! UNAUTHENTICATED request. Until token injection lands the macro must reject
//! `PlatformSecurityContext` methods rather than silently generate a broken
//! client. Serve platform-plane contracts over gRPC or a manual client.

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
