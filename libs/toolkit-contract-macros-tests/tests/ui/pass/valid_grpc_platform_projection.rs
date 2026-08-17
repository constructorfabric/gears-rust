//! A **platform-plane** gRPC projection compiles.
//!
//! `cpt-cf-binding-constraint-security-context` permits either plane — the tenant
//! `SecurityContext` or the platform `PlatformSecurityContext` — and
//! `rest_contract` rejects the platform form outright, telling the author to
//! "serve this contract over gRPC". This is the test that the advice is true.
//!
//! Two things must hold, and only the first is visible here:
//!
//! - the context is classified as a security context, so it is excluded from the
//!   wire payload and the *body* parameter is the one after it. Misclassify it and
//!   `body_param_ident` picks the context, leaving the real payload as a surplus
//!   parameter;
//! - the generated client attaches **nothing** from it.
//!   `PlatformSecurityContext` holds a validated identity, not a credential, so
//!   there is no bearer to attach; the credential is process-level and is attached
//!   at the channel by `InternalAuthInterceptor`. That half is asserted by
//!   `platform_client_attaches_no_bearer` in the macros crate, because it is a
//!   property of the emitted body rather than of whether it compiles.
//!
//! The client struct itself is gated on the `grpc-client` feature, so the tonic
//! stubs need not exist for this to compile — same as `valid_grpc_projection.rs`.

use toolkit_contract::{contract, grpc_contract};
use toolkit_security::PlatformSecurityContext;

#[contract(gear = "directory", version = "v1")]
pub trait DirectoryRegistrationApi: Send + Sync {
    async fn register(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

#[grpc_contract(
    package = "directory.registration.v1",
    service = "DirectoryRegistrationApi",
    stubs_module = "crate::stubs"
)]
pub trait DirectoryRegistrationApiGrpc: DirectoryRegistrationApi {
    #[rpc(name = "Register")]
    async fn register(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

/// The by-value form is equally valid, and the `ctx:`-name heuristic must not be
/// what carries it: `PlatformSecurityContext`'s last path segment is not
/// `SecurityContext`, so classification comes from the type, not the name.
#[contract(gear = "directory", version = "v1")]
pub trait DirectoryHeartbeatApi: Send + Sync {
    async fn heartbeat(
        &self,
        platform: PlatformSecurityContext,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

#[grpc_contract(
    package = "directory.heartbeat.v1",
    service = "DirectoryHeartbeatApi",
    stubs_module = "crate::stubs"
)]
pub trait DirectoryHeartbeatApiGrpc: DirectoryHeartbeatApi {
    #[rpc(name = "Heartbeat")]
    async fn heartbeat(
        &self,
        platform: PlatformSecurityContext,
        body: String,
    ) -> Result<u32, std::io::Error>;
}

fn main() {}
