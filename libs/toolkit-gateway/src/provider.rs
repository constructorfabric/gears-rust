//! The [`GatewayProvider`] abstraction.

use async_trait::async_trait;

use crate::error::GatewayError;
use crate::types::{Endpoint, GearName, OpenApiSpec};

/// Abstraction over an edge gateway that can register and deregister a gear's
/// externally-visible (public) routes.
///
/// The framework ships one in-tree implementation, `ToolKitGatewayProvider`,
/// which reverse-proxies through the built-in `api-gateway`. Out-of-tree
/// adapters (Kong, Tyk, ...) implement the same trait for Mode B deployments.
///
/// The trait is object-safe (via [`macro@async_trait`]) so the `OoP` bootstrap
/// can hold it as `Arc<dyn GatewayProvider>` and inject the concrete provider at
/// startup based on the deployment profile.
///
/// # Stability
/// This trait is **unstable**: it may change in a minor release while the `OoP`
/// gateway story stabilizes (see PRD § 7.1).
#[async_trait]
pub trait GatewayProvider: Send + Sync {
    /// Registers (or replaces) the public routes for `gear`, backed by the
    /// gear's HTTP `endpoint`. Only operations marked public on the visibility
    /// axis in `spec` are exposed at the edge.
    ///
    /// Implementations must be idempotent: registering a gear that is already
    /// registered replaces its previous route set atomically.
    ///
    /// # Errors
    /// Returns [`GatewayError`] if `spec` or `endpoint` is invalid, or if the
    /// gateway backend rejects the registration.
    async fn register_routes(
        &self,
        gear: &GearName,
        spec: OpenApiSpec<'_>,
        endpoint: &Endpoint,
    ) -> Result<(), GatewayError>;

    /// Removes all routes previously registered for `gear`.
    ///
    /// Deregistering a gear that is not registered is **not** an error.
    ///
    /// # Errors
    /// Returns [`GatewayError`] if the gateway backend rejects the request.
    async fn deregister_routes(&self, gear: &GearName) -> Result<(), GatewayError>;
}
