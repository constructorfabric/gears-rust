//! The [`GatewayProvider`] abstraction.

use async_trait::async_trait;

use crate::error::GatewayError;
use crate::types::{Endpoint, GearInstance, GearName, OpenApiSpec};

/// One instance's inputs for [`GatewayProvider::apply_snapshot`]: its identity,
/// `OpenAPI` document, and upstream endpoint.
pub struct InstanceSpec<'a> {
    /// Stable gear name.
    pub gear: GearName,
    /// Opaque, unique instance identifier.
    pub instance_id: String,
    /// The instance's `OpenAPI` document (public routes are extracted from it).
    pub spec: OpenApiSpec<'a>,
    /// Upstream endpoint serving this instance's routes.
    pub endpoint: Endpoint,
}

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
    /// Registers (or replaces) the public routes for a specific `instance_id`
    /// of `gear`, backed by that instance's HTTP `endpoint`. Only operations
    /// marked public on the visibility axis in `spec` are exposed at the edge.
    ///
    /// Implementations must be idempotent: registering an instance that is
    /// already registered replaces its previous route set atomically, leaving
    /// other instances of the same gear untouched.
    ///
    /// # Errors
    /// Returns [`GatewayError`] if `spec` or `endpoint` is invalid. A provider
    /// backed by an external gateway may also surface backend failures here.
    async fn register_routes(
        &self,
        gear: &GearName,
        instance_id: &str,
        spec: OpenApiSpec<'_>,
        endpoint: &Endpoint,
    ) -> Result<(), GatewayError>;

    /// Removes all routes previously registered for the given `instance_id` of
    /// `gear`.
    ///
    /// Deregistering an instance that is not registered is **not** an error.
    /// Other instances of the same gear remain registered.
    ///
    /// # Errors
    /// Infallible for the built-in in-process provider; the fallible signature
    /// is retained for providers backed by an external gateway.
    async fn deregister_routes(
        &self,
        gear: &GearName,
        instance_id: &str,
    ) -> Result<(), GatewayError>;

    /// Returns the gear instances currently registered with this provider.
    ///
    /// The directory-sync loop uses this to compute which registered instances
    /// have disappeared from a directory snapshot (and must be deregistered).
    /// A provider that keeps no local view of its registrations may return an
    /// empty list, in which case the loop never prunes on its behalf.
    fn registered_instances(&self) -> Vec<GearInstance>;

    /// Reconciles a full directory snapshot: registers (or replaces) every
    /// instance in `instances` and deregisters every instance in `removals`.
    ///
    /// This is the single entry point the directory-sync loop drives, so the
    /// loop can hold an `Arc<dyn GatewayProvider>` and remain agnostic to the
    /// concrete edge (built-in reverse proxy, Kong, Tyk, ...).
    ///
    /// The default implementation applies each change independently via
    /// [`register_routes`](Self::register_routes) /
    /// [`deregister_routes`](Self::deregister_routes), logging and skipping
    /// per-instance failures so one bad gear cannot stall the rest. Providers
    /// that can apply a whole snapshot atomically (e.g. a single router rebuild)
    /// should override this to avoid per-instance churn.
    async fn apply_snapshot<'a>(
        &self,
        instances: Vec<InstanceSpec<'a>>,
        removals: &[GearInstance],
    ) {
        for inst in instances {
            if let Err(err) = self
                .register_routes(&inst.gear, &inst.instance_id, inst.spec, &inst.endpoint)
                .await
            {
                tracing::warn!(
                    gear = %inst.gear,
                    instance_id = %inst.instance_id,
                    error = %err,
                    "skipping gear: failed to register routes at the edge",
                );
            }
        }
        for instance in removals {
            if let Err(err) = self
                .deregister_routes(instance.gear(), instance.instance_id())
                .await
            {
                tracing::warn!(
                    gear = %instance.gear(),
                    instance_id = %instance.instance_id(),
                    error = %err,
                    "failed to deregister stale gear instance at the edge",
                );
            }
        }
    }
}
