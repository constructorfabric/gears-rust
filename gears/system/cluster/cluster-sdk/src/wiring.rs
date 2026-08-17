// Created: 2026-08-13 by Constructor Tech
//! Cluster's hand-written `ConsumerRegistration` — how a deployed consumer finds
//! the cluster gear (DESIGN-DEPLOYABLE-GEAR §4.9.2, §4.9.3, §12.15).
//!
//! Nothing here is called from consumer code. The `inventory::submit!` at the
//! bottom of this file is replayed by the framework's proxy-wiring phase, once,
//! before any gear's `start` — so by the time a consumer resolves a facade in its
//! own `start`, the process's `dyn ClusterClient` is already in the hub.
//!
//! # Why it is hand-written
//!
//! `#[toolkit::consumes]` generates a `<Contract>RestResolvingClient` and there is
//! no gRPC counterpart planned. Cluster's data plane is gRPC (§2.2.1), so the
//! macro cannot generate cluster's client — but `ConsumerRegistration` is a plain
//! transport-agnostic struct, so cluster reuses the *mechanism* and writes only
//! the one thing the macro cannot: the closure. That is a smaller deviation than
//! it sounds, and its owner has confirmed the registration needs no rework when a
//! generated gRPC client eventually lands (§4.9.2).
//!
//! # The endpoint is derived, not resolved
//!
//! [`derive_endpoint`] builds `cluster.{namespace}.svc.cluster.local:{port}` and
//! the framework's `EndpointResolver` is ignored. Three reasons, in descending
//! order of how load-bearing they are:
//!
//! - **It resolves the wrong plane.** `DirectoryEndpointResolver` calls
//!   `resolve_rest_service`, which answers with a REST base URI. Cluster's
//!   coordination plane is gRPC on a different port; handing that URI to
//!   `RemoteClusterClient::connect_lazy` would build a channel to the wrong
//!   listener.
//! - **It cannot be consulted here.** `EndpointResolver::resolve_endpoint` is
//!   `async` and `ConsumerRegistration::wire` is a sync `fn`. Blocking on the
//!   future would block a runtime worker inside the wiring phase.
//! - **Cluster knows the port and a framework resolver does not** (§4.5). DNS
//!   returns a name, not a URI, so a generic resolver needs a port convention
//!   that does not exist yet — decision 21, and a platform-PRD question.
//!
//! **The cost, stated rather than buried**: the ADR-0004 static override
//! (`gears.<owner>.config.consumer_wiring.<dep>`) therefore does not reach
//! cluster, and §4.5 names that override as the current mechanism for a
//! directory-less Kubernetes deployment. For cluster, DNS convention *is* that
//! mechanism — which is the intended end state and needs no directory, so nothing
//! is lost in Profile 3. What is lost is the ability to point a consumer at an
//! arbitrary cluster endpoint from config, and by invariant I9 that is deliberate:
//! there is no cluster-side endpoint key, in any form.
//!
//! **Only the *endpoint* half is inert, though — the key is still read, and it
//! silently drops a readiness gate.** `static_endpoint_override`
//! (`host_runtime.rs:83-94`) does not consult `known_gears`, so a
//! `gears.cluster-sdk.config.consumer_wiring.cluster` value is picked up even
//! though `owner_gear: "cluster-sdk"` names no registered gear. The phase then
//! takes its `is_static` branch and calls `dep_checker.mark_resolved("cluster")`
//! **with no probe**, while `wire` below still builds its own DNS-derived
//! channel. Measured: with the key set, a consumer's `unresolved_deps()` at
//! `start` is `[]`; without it, `["cluster"]`. So the override cannot redirect
//! cluster, but it *can* put a consumer into rotation against a cluster that was
//! never resolved — and the framework's own `warn!` on that path ("the … static
//! override will never resolve") is, in this one respect, false. Recorded as
//! `CFG-1` in `docs/AUDIT-DEPLOYABLE-GEAR.md`; the durable repair is framework-
//! side, skipping overrides whose `owner_gear` is unknown.
//!
//! # What this closure must never do
//!
//! Await, connect, or fail on cluster being absent (invariant I6, ADR-0005).
//! `connect_lazy` touches no network, the descriptor prefetch is spawned, and the
//! only error it can return is a genuinely permanent one: a namespace it cannot
//! determine, or an endpoint that is not a URI.

use std::sync::Arc;

use toolkit::client_hub::ClientHub;
use toolkit::discovery::{ConsumerRegistration, EndpointResolver, WireOutcome};

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
/// An SDK **constant**, not configuration (invariant I9), and the convention half
/// of §4.5's "the name is derivable; the port is not". Cluster can write this
/// where a framework resolver cannot, because cluster is what chooses the port —
/// but only as a convention, so `D2`'s Helm chart must render the Service on this
/// port and a deployment that changes it has no way to tell a consumer.
///
/// It matches the platform's conventional gRPC port, which is what an operator
/// reading `grpc: { port: 50051 }` in the values file expects.
pub const CLUSTER_GRPC_PORT: u16 = 50051;

/// The downward-API variable the namespace is read from (§4.5 source 2).
///
/// The only environment input on this path, and it is the platform's own variable
/// rather than a cluster-specific one — which is what keeps it outside invariant
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

/// Builds and registers the process's remote cluster client, unless one is
/// already there.
///
/// Shared by the wiring closure below and by `binding::process_client`'s
/// self-construction arm, so the two cannot drift: whichever runs first is the one
/// that builds the channel, and the other finds it.
///
/// The `try_get` guard is what keeps this from ever overwriting a client that is
/// already there, local or remote: the hub's `register` is last-write-wins with no
/// if-absent form, so a check-then-register is the only available shape. Combined
/// with the caller's local-wins probe, that is what keeps invariant I4's "exactly
/// one client, local winning, decided at registration time" true. `try_get` and not
/// `try_get_local` here on purpose - a remote proxy some other consumer's wiring
/// registered is a perfectly good client, and building a second channel beside it
/// would be waste, not safety.
///
/// # Errors
/// [`ClusterError::InvalidConfig`] from [`derive_endpoint`], or from an endpoint
/// tonic cannot parse.
pub fn register_remote_client(hub: &ClientHub) -> Result<Arc<dyn ClusterClient>, ClusterError> {
    if let Some(existing) = hub.try_get::<dyn ClusterClient>() {
        return Ok(existing);
    }
    let endpoint = derive_endpoint()?;
    let client: Arc<dyn ClusterClient> = Arc::new(RemoteClusterClient::connect_lazy(&endpoint)?);
    tracing::info!(
        endpoint = %endpoint,
        "cluster: registered a remote client (derived from Kubernetes DNS convention)"
    );
    // `register_remote_proxy`, not `register`: it is what makes `try_get_local`
    // answer `None` for this registration, which is how a second consumer wiring
    // the same contract in one process correctly reports `Remote` instead of
    // mistaking this proxy for a co-located implementation.
    hub.register_remote_proxy::<dyn ClusterClient>(Arc::clone(&client));
    Ok(client)
}

/// Fetches the whole bound-profile set in the background, so the synchronous
/// descriptor accessors are populated before a consumer reads them (§5.5).
///
/// Spawned, never awaited: this gates `/readyz` only, never `start` and never
/// `resolve()` (§4.9.3 step 3). `resolve()` awaits the descriptor itself on a
/// bounded timeout, so a prefetch that has not landed costs latency at the first
/// resolve and nothing else.
///
/// The inventoried profile markers are what make the log line useful: they are the
/// profiles this *process* declared, so a profile the server does not bind is a
/// configuration mismatch worth naming — and naming it here, at startup, is much
/// earlier than the `CapabilityNotMet`/`ProfileNotBound` a consumer would
/// otherwise meet at its first call. The prefetch itself needs no filter, because
/// `DescribeProfiles` with an empty one answers with the entire bound set.
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
/// `resolver` is deliberately unused — see the module docs for the three reasons,
/// of which the binding one is that it resolves REST endpoints and cluster's
/// coordination plane is gRPC.
///
/// The return type is spelled `toolkit::Result` rather than `anyhow::Result`, which
/// it is: `ConsumerRegistration::wire` is typed in terms of `anyhow`, and going
/// through the toolkit's own re-export means this crate needs no `anyhow`
/// dependency of its own for one signature.
fn wire(hub: &ClientHub, _resolver: Arc<dyn EndpointResolver>) -> toolkit::Result<WireOutcome> {
    // 1. Local wins, and `try_get_local` is the right probe: it ignores anything
    //    registered through `register_remote_proxy`, so a second consumer in this
    //    process cannot mistake the proxy *this* closure registered for a
    //    co-located cluster gear. A co-located gear registers its
    //    `LocalClusterClient` in `init`, one phase before this runs, precisely so
    //    that this branch is taken in every process that hosts cluster - including
    //    the `cluster-oop` binary itself, which would otherwise wire a remote
    //    client to its own socket.
    if hub.try_get_local::<dyn ClusterClient>().is_some() {
        tracing::debug!(
            "cluster wiring: a local cluster client is registered in this process; \
             no channel built"
        );
        return Ok(WireOutcome::Local);
    }

    // 2. Build and register the remote client. Pure - no I/O, nothing awaited.
    let client = register_remote_client(hub)?;

    // 3. Warm the descriptor cache in the background. Readiness only.
    spawn_descriptor_prefetch(client);

    Ok(WireOutcome::Remote)
}

toolkit::inventory::submit! {
    ConsumerRegistration {
        // Neither name is a free choice, and the first one is a known mismatch
        // with the framework's model. `owner_gear` is meant to be the consuming
        // gear's registry name, so that `gears.<owner>.config.consumer_wiring.*`
        // resolves - but this registration is submitted by an *SDK*, once per
        // process, on behalf of however many consumer gears link it. There is no
        // single owner to name. The framework logs a `warn!` when `owner_gear` is
        // not a registered gear, saying the static override will never resolve;
        // for cluster that statement is true and harmless, since the override is
        // unreachable here anyway (see the module docs). Recorded as a finding for
        // ADR-0004 (`D-40`), which already asks that an SDK-submitted
        // registration be specified.
        owner_gear: "cluster-sdk",
        // This one is exact: it is the provider gear's registry name, and the
        // framework uses it as the readiness dependency key - so a consumer's
        // `/readyz` gates on `cluster` resolving, which is what `deps = [cluster]`
        // already means.
        dep_gear: CLUSTER_GEAR,
        wire,
    }
}

#[cfg(test)]
#[path = "wiring_tests.rs"]
mod wiring_tests;
