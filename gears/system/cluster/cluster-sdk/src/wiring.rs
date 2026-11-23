//! Cluster's hand-written `ConsumerRegistration` — how a deployed consumer finds
//! the cluster gear (DESIGN.md).
//!
//! The SDK submits one inventory registration on behalf of the gears that link
//! it with the `grpc-client` feature. In normal and out-of-process startup, the
//! host wires registrations involving local system gears after system `init`
//! and before system `post_init` and `start`. It wires other consumers after
//! application `init` and before application `post_init` and `start`. Registry
//! clients can wire earlier. Facade resolution uses the client visible to the
//! caller and can also invoke DNS registration if wiring leaves the hub empty.
//!
//! # Why the SDK submits its own registration
//!
//! The generated gRPC consumer resolves a service through the directory and
//! applies a per-call deadline. Cluster shares one channel across its four
//! services and supports calls without a deadline, including server-side lock
//! waits and watches. Its synchronous closure uses the same transport-agnostic
//! inventory mechanism and can build a client from Kubernetes DNS convention.
//!
//! # Client selection and host addressing
//!
//! The local-only lookup yields `WireOutcome::Local` on a hit. Otherwise a
//! separate lookup for any visible client yields `WireOutcome::RemoteGrpc` for
//! `cluster.profile.v1.ClusterProfileApi` on a hit. It can see a newly registered
//! local client or a proxy, including a root proxy inherited by an owner view.
//! Composition chooses the endpoint and credentials for a proxy it supplies.
//! Both early returns skip client construction and descriptor prefetch.
//!
//! For that gRPC outcome the host looks up
//! `gears.cluster-sdk.clients.consumes.cluster.grpc_endpoint`. A static gRPC
//! address satisfies this registration's address requirement without a probe.
//! A configured `endpoint` also supplies gRPC addressing when `grpc_endpoint`
//! is absent. Without a static address the host tracks a directory target for
//! the gRPC service and probes when a directory is available. Any outstanding
//! target for the dependency still gates host readiness. A static address says
//! only that addressing is available. Consumers can use the SDK readiness
//! contributor to check their profile requirements and descriptor health.
//!
//! After both misses, the fallback helper checks visibility again. On its own
//! miss, it uses [`derive_endpoint`] to build the Kubernetes URI from `POD_NAMESPACE`
//! and the SDK port convention, then registers a lazy proxy without consulting
//! host overrides or the directory. An unusable namespace or URI is a wiring error.
//! When the helper succeeds, wiring schedules descriptor prefetch on the returned
//! client and returns the legacy `WireOutcome::Remote`. The host uses its REST
//! addressing rules for this outcome. Connection and descriptor I/O are deferred.

use std::sync::Arc;

use toolkit::client_hub::ClientHub;
use toolkit::discovery::{ConsumerRegistration, EndpointResolver, WireOutcome};
use toolkit_contract::runtime::config::InternalTokenProvider;
use toolkit_contract::wiring::ClientTuning;

use crate::client::ClusterClient;
use crate::client::remote::RemoteClusterClient;
use crate::error::ClusterError;
use crate::profile::registered_profiles;

/// The gear name cluster's Service is published under, and the first label of the
/// derived DNS name.
///
/// The same string as `#[toolkit::gear(name = "cluster")]` and as the `dep_gear`
/// below, because they are the same thing seen from three places: the registry
/// name, the readiness dependency, and the Kubernetes Service.
pub const CLUSTER_GEAR: &str = "cluster";

/// The port cluster's coordination plane is reached on, by convention.
///
/// The Kubernetes Service must expose this port for convention-derived
/// addresses. A deployment that changes the port needs a client supplied by
/// composition to use that address.
///
/// It matches the platform's conventional gRPC port, which an operator
/// reading `grpc: { port: 50051 }` in the values file expects.
pub const CLUSTER_GRPC_PORT: u16 = 50051;

/// The downward-API variable the namespace is read from (§4.5 source 2).
///
/// The only environment input on this path, and it is the platform's own variable
/// rather than a cluster-specific one — which keeps it outside invariant
/// I9's prohibition on cluster-side client configuration.
pub const POD_NAMESPACE_ENV: &str = "POD_NAMESPACE";

/// Builds the endpoint of the deployed cluster gear from Kubernetes convention.
///
/// `http://cluster.{namespace}.svc.cluster.local:{CLUSTER_GRPC_PORT}`, with the
/// namespace from [`POD_NAMESPACE_ENV`].
///
/// # Why a missing namespace is an error rather than a default
///
/// Defaulting to `default` would build a *syntactically valid* endpoint pointing
/// at the wrong pod, and the symptom would be a connection failure at the first
/// coordination call — indistinguishable from cluster being down, and diagnosed
/// nowhere near the cause. A named error at wiring time says exactly what is
/// missing. This is the §4.7 permanent/transient split applied to configuration:
/// a missing deployment variable cannot fix itself.
///
/// # Errors
/// [`ClusterError::InvalidConfig`] naming [`POD_NAMESPACE_ENV`] when it is unset
/// or empty, or when the namespace is not a usable DNS label.
pub fn derive_endpoint() -> Result<String, ClusterError> {
    let namespace = std::env::var(POD_NAMESPACE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ClusterError::InvalidConfig {
            reason: format!(
                "cannot derive the cluster endpoint: {POD_NAMESPACE_ENV} is unset or empty. \
                 In Kubernetes it comes from the downward API; cluster owns no endpoint \
                 configuration key of its own"
            ),
        })?;

    // A namespace lands inside a DNS name, so a value carrying `/`, `:` or a dot
    // would silently produce a different host than intended. The cluster name rule
    // is the same character class a DNS label allows, minus `_`, and it is already
    // the rule every cluster identifier satisfies - reusing it keeps one rule.
    if !crate::profile::is_valid_cluster_name(&namespace) || namespace.contains('_') {
        return Err(ClusterError::InvalidConfig {
            reason: format!(
                "cannot derive the cluster endpoint: {POD_NAMESPACE_ENV}=`{namespace}` is not a \
                 DNS label"
            ),
        });
    }

    Ok(format!(
        "http://{CLUSTER_GEAR}.{namespace}.svc.cluster.local:{CLUSTER_GRPC_PORT}"
    ))
}

/// Reuses a visible cluster client or registers a lazy DNS proxy in the supplied hub.
///
/// Consumer wiring and facade binding share this helper so they use the same
/// endpoint convention. The SDK inventory registration uses the process-wide
/// hub when `cluster-sdk` is absent from the gear registry. Facade binding
/// passes the caller's hub view. That view sees the shared local implementation,
/// then its own proxy, then any root proxy.
///
/// The independent `try_get` guard lets either caller reuse all visible clients.
/// The lookup and insertion are separate operations. Concurrent misses can
/// each build a proxy.
///
/// `internal_token_provider` is the process's platform-plane credential source,
/// forwarded to [`RemoteClusterClient::connect_lazy`] so every outbound call
/// carries `X-ToolKit-Internal-Token`. `None` attaches nothing.
///
/// # Errors
/// [`ClusterError::InvalidConfig`] from [`derive_endpoint`], or from an endpoint
/// tonic cannot parse.
pub fn register_remote_client(
    hub: &ClientHub,
    internal_token_provider: Option<&InternalTokenProvider>,
) -> Result<Arc<dyn ClusterClient>, ClusterError> {
    if let Some(existing) = hub.try_get::<dyn ClusterClient>() {
        return Ok(existing);
    }
    let endpoint = derive_endpoint()?;
    let client: Arc<dyn ClusterClient> = Arc::new(RemoteClusterClient::connect_lazy(
        &endpoint,
        internal_token_provider,
    )?);
    tracing::info!(
        endpoint = %endpoint,
        "cluster: registered a remote client (derived from Kubernetes DNS convention)"
    );
    // Keep the local map separate so later wiring can report RemoteGrpc
    // for the visible proxy and still give a co-located implementation priority.
    hub.register_remote_proxy::<dyn ClusterClient>(Arc::clone(&client));
    Ok(client)
}

/// Schedules descriptor prefetch on the client returned by the wiring fallback.
///
/// The detached task reads the inventoried profiles to report ones that are
/// unbound or unavailable at startup. Without markers it calls
/// `descriptor("")` once on the returned client.
///
/// Binding has its own bounded descriptor lookup and the readiness contributor
/// checks recorded requirements independently. Prefetch supplies neither the
/// host's address decision nor a prerequisite for `start` or `resolve()`.
fn spawn_descriptor_prefetch(client: Arc<dyn ClusterClient>) {
    let expected = registered_profiles();
    tokio::spawn(async move {
        if expected.is_empty() {
            // Nothing declared `register_cluster_profile!`. Prefetch anyway - the
            // markers are a diagnostic aid, not the mechanism - but say so, since
            // in a real consumer it means the macro was forgotten.
            tracing::debug!(
                "cluster prefetch: no profile markers registered in this process; \
                 fetching the bound set anyway"
            );
        }
        for profile in &expected {
            match client.descriptor(profile).await {
                Ok(descriptor) => tracing::debug!(
                    profile = %profile,
                    health = ?descriptor.health,
                    "cluster prefetch: descriptor cached"
                ),
                Err(err) => tracing::warn!(
                    profile = %profile,
                    error = %err,
                    "cluster prefetch: the cluster gear does not serve a profile this \
                     process declared, or is not reachable yet; readiness will report it"
                ),
            }
        }
        if expected.is_empty() {
            // One unfiltered call so the accessors are warm even with no markers.
            drop(client.descriptor("").await);
        }
    });
}

/// The wiring closure the framework replays. See the [module docs](self).
///
/// The SDK chooses a visible client or derives its own DNS URI. The host retains
/// responsibility for interpreting the returned addressing outcome.
///
/// The signature uses the toolkit's `anyhow::Result` re-export to avoid a
/// direct dependency on `anyhow` for this closure.
// The runtime's platform-plane credential is used when constructing a DNS
// proxy. The client attaches `X-ToolKit-Internal-Token` through its interceptor
// when a provider is configured. An existing client keeps its credential setup.
fn wire(
    hub: &ClientHub,
    _resolver: Arc<dyn EndpointResolver>,
    tuning: ClientTuning,
) -> toolkit::Result<WireOutcome> {
    // Cluster registers its local client in init before consumer wiring runs.
    if hub.try_get_local::<dyn ClusterClient>().is_some() {
        tracing::debug!(
            "cluster wiring: a local cluster client is registered in this process; \
             no channel built"
        );
        return Ok(WireOutcome::Local);
    }

    // A gear's hub view can inherit the process-wide proxy.
    if hub.try_get::<dyn ClusterClient>().is_some() {
        return Ok(WireOutcome::RemoteGrpc {
            service: crate::grpc::stubs::profile::cluster_profile_api_server::SERVICE_NAME,
        });
    }

    let internal_token_provider = tuning.internal_token_provider;
    let client = register_remote_client(hub, internal_token_provider.as_ref())?;
    spawn_descriptor_prefetch(client);

    Ok(WireOutcome::Remote)
}

toolkit::inventory::submit! {
    ConsumerRegistration {
        // The SDK owns this registration for every linked consumer. The host
        // reads configuration under this owner even when the gear registry has
        // no matching entry. In that case wiring uses the process-wide hub.
        owner_gear: "cluster-sdk",
        // The host tracks addressing for the provider under its gear name.
        // This SDK registration declares the dependency for readiness.
        dep_gear: CLUSTER_GEAR,
        wire,
    }
}

#[cfg(test)]
#[path = "wiring_tests.rs"]
mod wiring_tests;
