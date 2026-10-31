//! The gate in front of the registry reconcile on a database backend
//! (ADR-0018 *Provisioning from the Types Registry*): the deployment generation, the content
//! hash and the lease, all in the one registry-state row.
//!
//! A pod whose generation is older than the stored one writes nothing. A
//! pod whose registry content hashes to the stored hash writes nothing (it
//! claims its newer generation, if any). Otherwise one pod takes the lease,
//! claiming its generation in the same statement, and reconciles; the others
//! poll until the hash matches, a newer generation appears or the lease
//! frees. The hash is recorded only after a clean run.

use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use toolkit_security::crypto::hash::Sha256;
use tracing::{info, warn};
use uuid::Uuid;

use super::RegistryProvisioner;
use super::registry_reconcile::{
    CALL_TIMEOUT, CallTimedOut, ProvisioningCounts, RunControl, bounded, log_counts,
    reconcile_registry,
};
use crate::domain::model::{
    AuthConfig, BudgetConfig, BurstConfig, CorsConfig, CreateRouteRequest, CreateUpstreamRequest,
    Endpoint, GrpcMatch, HeadersConfig, HttpMatch, MatchRules, PluginBinding, PluginsConfig,
    RateLimitConfig, RequestHeaderRules, ResponseHeaderRules, Server, SustainedRate,
};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::provisioning_outcome;
use crate::domain::repo::RegistryStateStore;
use crate::domain::type_provisioning::{ProvisionedRoute, ProvisionedUpstream};

// ---------------------------------------------------------------------------
// Content hash
// ---------------------------------------------------------------------------

/// Hex SHA-256 of OAGW's version and the registry content, as the reconcile
/// would write it (the materialized requests, not the raw GTS JSON, so a
/// change in OAGW's parse defaults changes the hash too).
///
/// The encoding is canonical and hand-written, as types-registry's
/// `fingerprint.rs`: every field is length-prefixed (so `("ab", "c")` and
/// `("a", "bc")` differ), map entries are sorted by key, `f64` is encoded by
/// its bits, enums by their variant names, and instances are sorted by
/// `(tenant_id, id)`, so neither listing order nor `HashMap` order matters.
/// SHA-256 comes from the toolkit-security facade and its selected backend.
pub(crate) fn content_hash(
    upstreams: &[ProvisionedUpstream],
    routes: &[ProvisionedRoute],
    root_tenant_id: Uuid,
) -> String {
    let mut e = Encoder(Sha256::new());
    e.str("oagw-registry-content-v1");
    e.str(env!("CARGO_PKG_VERSION"));

    let key = |tenant: Option<Uuid>, id: Option<Uuid>| (tenant.unwrap_or(root_tenant_id), id);
    let mut upstreams: Vec<&ProvisionedUpstream> = upstreams.iter().collect();
    upstreams.sort_by_key(|u| key(u.tenant_id, u.request.id));
    e.len(upstreams.len());
    for ProvisionedUpstream { tenant_id, request } in upstreams {
        let (tenant, id) = key(*tenant_id, request.id);
        e.uuid(tenant);
        e.opt(id.as_ref(), |e, id| e.uuid(*id));
        e.upstream(request);
    }
    let mut routes: Vec<&ProvisionedRoute> = routes.iter().collect();
    routes.sort_by_key(|r| key(r.tenant_id, r.request.id));
    e.len(routes.len());
    for ProvisionedRoute { tenant_id, request } in routes {
        let (tenant, id) = key(*tenant_id, request.id);
        e.uuid(tenant);
        e.opt(id.as_ref(), |e, id| e.uuid(*id));
        e.route(request);
    }

    let digest = e.0.finalize();
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The canonical encoder behind [`content_hash`].
struct Encoder(Sha256);

impl Encoder {
    fn bytes(&mut self, bytes: &[u8]) {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
    }

    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_be_bytes());
    }

    fn len(&mut self, n: usize) {
        self.u64(n as u64);
    }

    fn i64(&mut self, v: i64) {
        self.bytes(&v.to_be_bytes());
    }

    fn bool(&mut self, v: bool) {
        self.bytes(&[u8::from(v)]);
    }

    fn uuid(&mut self, v: Uuid) {
        self.bytes(v.as_bytes());
    }

    fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }

    /// A field-less enum, by its variant name.
    fn variant(&mut self, v: &impl Debug) {
        self.str(&format!("{v:?}"));
    }

    fn opt<T>(&mut self, v: Option<&T>, f: impl FnOnce(&mut Self, &T)) {
        match v {
            None => self.bool(false),
            Some(v) => {
                self.bool(true);
                f(self, v);
            }
        }
    }

    fn strs(&mut self, items: &[String]) {
        self.len(items.len());
        for item in items {
            self.str(item);
        }
    }

    fn map(&mut self, map: &HashMap<String, String>) {
        let mut entries: Vec<(&String, &String)> = map.iter().collect();
        entries.sort();
        self.len(entries.len());
        for (k, v) in entries {
            self.str(k);
            self.str(v);
        }
    }

    // Every encoder below destructures its struct with no `..`, so a new
    // field fails to compile until it is hashed or named as skipped.

    fn upstream(&mut self, u: &CreateUpstreamRequest) {
        let CreateUpstreamRequest {
            id: _, // encoded by the caller, with the tenant
            server: Server { endpoints },
            protocol,
            alias,
            auth,
            headers,
            plugins,
            rate_limit,
            cors,
            tags,
            enabled,
        } = u;
        self.len(endpoints.len());
        for Endpoint { scheme, host, port } in endpoints {
            self.variant(scheme);
            self.str(host);
            self.u64(u64::from(*port));
        }
        self.str(protocol);
        self.opt(alias.as_ref(), |e, a| e.str(a));
        self.opt(auth.as_ref(), Self::auth);
        self.opt(headers.as_ref(), Self::headers);
        self.opt(plugins.as_ref(), Self::plugins);
        self.opt(rate_limit.as_ref(), Self::rate_limit);
        self.opt(cors.as_ref(), Self::cors);
        self.strs(tags);
        self.bool(*enabled);
    }

    fn route(&mut self, r: &CreateRouteRequest) {
        let CreateRouteRequest {
            id: _, // encoded by the caller, with the tenant
            upstream_id,
            match_rules,
            plugins,
            rate_limit,
            cors,
            tags,
            priority,
            enabled,
        } = r;
        self.uuid(*upstream_id);
        self.match_rules(match_rules);
        self.opt(plugins.as_ref(), Self::plugins);
        self.opt(rate_limit.as_ref(), Self::rate_limit);
        self.opt(cors.as_ref(), Self::cors);
        self.strs(tags);
        self.i64(i64::from(*priority));
        self.bool(*enabled);
    }

    fn auth(&mut self, a: &AuthConfig) {
        let AuthConfig {
            plugin_type,
            sharing,
            config,
        } = a;
        self.str(plugin_type);
        self.variant(sharing);
        self.opt(config.as_ref(), Self::map);
    }

    fn headers(&mut self, h: &HeadersConfig) {
        let HeadersConfig { request, response } = h;
        self.opt(request.as_ref(), |e, r| {
            let RequestHeaderRules {
                set,
                add,
                remove,
                passthrough,
                passthrough_allowlist,
            } = r;
            e.map(set);
            e.map(add);
            e.strs(remove);
            e.variant(passthrough);
            e.strs(passthrough_allowlist);
        });
        self.opt(response.as_ref(), |e, r| {
            let ResponseHeaderRules { set, add, remove } = r;
            e.map(set);
            e.map(add);
            e.strs(remove);
        });
    }

    fn plugins(&mut self, p: &PluginsConfig) {
        let PluginsConfig { sharing, items } = p;
        self.variant(sharing);
        self.len(items.len());
        for PluginBinding { plugin_ref, config } in items {
            self.str(plugin_ref);
            self.map(config);
        }
    }

    fn rate_limit(&mut self, r: &RateLimitConfig) {
        let RateLimitConfig {
            sharing,
            algorithm,
            sustained: SustainedRate { rate, window },
            burst,
            budget,
            scope,
            strategy,
            cost,
            response_headers,
            pool_owner_id: _, // set by the hierarchical merge, never by provisioning
        } = r;
        self.variant(sharing);
        self.variant(algorithm);
        self.u64(u64::from(*rate));
        self.variant(window);
        self.opt(burst.as_ref(), |e, BurstConfig { capacity }| {
            e.u64(u64::from(*capacity));
        });
        self.opt(budget.as_ref(), |e, b| {
            let BudgetConfig {
                mode,
                total,
                overcommit_ratio,
            } = b;
            e.variant(mode);
            e.opt(total.as_ref(), |e, t| e.u64(u64::from(*t)));
            e.opt(overcommit_ratio.as_ref(), |e, r| e.f64(*r));
        });
        self.variant(scope);
        self.variant(strategy);
        self.u64(u64::from(*cost));
        self.bool(*response_headers);
    }

    fn cors(&mut self, c: &CorsConfig) {
        let CorsConfig {
            sharing,
            enabled,
            allowed_origins,
            allowed_methods,
            expose_headers,
            allow_credentials,
        } = c;
        self.variant(sharing);
        self.bool(*enabled);
        self.strs(allowed_origins);
        self.len(allowed_methods.len());
        for m in allowed_methods {
            self.variant(m);
        }
        self.strs(expose_headers);
        self.bool(*allow_credentials);
    }

    fn match_rules(&mut self, m: &MatchRules) {
        let MatchRules { http, grpc } = m;
        self.opt(http.as_ref(), |e, h| {
            let HttpMatch {
                methods,
                path,
                query_allowlist,
                path_suffix_mode,
            } = h;
            e.len(methods.len());
            for method in methods {
                e.variant(method);
            }
            e.str(path);
            e.strs(query_allowlist);
            e.variant(path_suffix_mode);
        });
        self.opt(grpc.as_ref(), |e, g| {
            let GrpcMatch { service, method } = g;
            e.str(service);
            e.str(method);
        });
    }
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// `interval` ±20%, so pods that started or failed together spread their
/// checks, retries and lease polls.
pub(crate) fn jitter(interval: Duration) -> Duration {
    // A random v4 UUID is 122 random bits; any spread source will do.
    let unit = (Uuid::new_v4().as_u128() % 10_000) as f64 / 10_000.0;
    interval.mul_f64(0.8 + 0.4 * unit)
}

/// How long a held lease lasts without renewal.
pub(crate) const LEASE_TTL: Duration = Duration::from_secs(60);
/// How often a pod that lost the lease looks again.
pub(crate) const LEASE_POLL: Duration = Duration::from_secs(1);

/// This pod's side of the registry-state row.
#[domain_model]
#[derive(Clone)]
pub(crate) struct Gate {
    /// The registry-state row.
    pub(crate) state: std::sync::Arc<dyn RegistryStateStore>,
    /// This process, as the lease owner.
    pub(crate) owner: Uuid,
    /// The deployment generation, when the deployment supplies one.
    pub(crate) generation: Option<u64>,
    /// [`LEASE_TTL`] outside tests.
    pub(crate) lease_ttl: Duration,
    /// [`LEASE_POLL`] outside tests.
    pub(crate) poll: Duration,
    /// Spreads the lease polls ([`jitter`]; identity in tests).
    pub(crate) jitter: fn(Duration) -> Duration,
}

/// What one gated run did.
#[domain_model]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunOutcome {
    /// The stored hash matched.
    UpToDate {
        /// This pod polled another pod's lease before the hash matched.
        waited: bool,
    },
    /// The stored hash matched, and this pod's newer generation was claimed.
    Claimed,
    /// A newer deployment generation owns the registry: nothing written.
    Stale {
        /// The stored generation.
        stored: u64,
    },
    /// Reconciled under the lease.
    Reconciled(ProvisioningCounts),
    /// Cancelled while waiting for the lease.
    Cancelled,
}

impl RunOutcome {
    /// The `outcome` label of `registry_provisioning_runs_total`.
    pub(crate) fn metric_label(&self) -> &'static str {
        match self {
            Self::UpToDate { .. } | Self::Claimed => provisioning_outcome::UP_TO_DATE,
            Self::Stale { .. } => provisioning_outcome::STALE,
            Self::Reconciled(counts) if counts.is_clean() => provisioning_outcome::CLEAN,
            Self::Reconciled(_) | Self::Cancelled => provisioning_outcome::NOT_CLEAN,
        }
    }

    /// Whether the stored rows match this pod's content, or belong to a
    /// newer generation: nothing is left for this pod to do.
    pub(crate) fn is_settled(&self) -> bool {
        match self {
            Self::UpToDate { .. } | Self::Claimed | Self::Stale { .. } => true,
            Self::Reconciled(counts) => counts.is_clean(),
            Self::Cancelled => false,
        }
    }
}

// See `CALL_TIMEOUT`.
const _: () = assert!(CALL_TIMEOUT.as_secs() * 3 <= LEASE_TTL.as_secs());

/// The lease as the reconcile sees it: renewed after every phase and pass,
/// and before a write once a third of the TTL has passed since the last
/// renewal; a lost lease or a cancellation stops the writes.
struct LeaseControl<'a> {
    gate: &'a Gate,
    cancel: &'a CancellationToken,
    /// When the last renewal (or the acquisition) was sent.
    renewed_at: parking_lot::Mutex<Instant>,
}

#[async_trait]
impl RunControl for LeaseControl<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    async fn renew(&self) -> anyhow::Result<bool> {
        let sent = Instant::now();
        let held = bounded(
            "renewing the registry lease",
            self.gate.state.renew(self.gate.owner, self.gate.lease_ttl),
        )
        .await?
        .map_err(|e| anyhow::anyhow!("Failed to renew the registry lease: {e}"))?;
        if held {
            *self.renewed_at.lock() = sent;
        } else {
            warn!(
                owner = %self.gate.owner,
                "Lost the registry lease (this pod stalled past its TTL); stopping the reconcile"
            );
        }
        Ok(held)
    }

    async fn before_write(&self) -> anyhow::Result<bool> {
        if self.cancelled() {
            return Ok(false);
        }
        let due = self.renewed_at.lock().elapsed() >= self.gate.lease_ttl / 3;
        if due { self.renew().await } else { Ok(true) }
    }
}

fn state_failed(what: &str, e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("Failed to {what} the registry state: {e}")
}

/// One gated run: the gate (see the module doc), then, holding the lease,
/// the reconcile, then the finish. Only an infrastructure error returns
/// `Err`; the lease is released on the way out, except after a timeout.
pub(crate) async fn gated_reconcile(
    gate: &Gate,
    cp: &dyn RegistryProvisioner,
    metrics: &dyn OagwMetricsPort,
    cancel: &CancellationToken,
    upstreams: &[ProvisionedUpstream],
    routes: &[ProvisionedRoute],
    root_tenant_id: Uuid,
) -> anyhow::Result<RunOutcome> {
    let hash = content_hash(upstreams, routes, root_tenant_id);
    let mut waited = false;
    let acquired_at = loop {
        let stored = bounded("reading the registry state", gate.state.read())
            .await?
            .map_err(|e| state_failed("read", e))?;
        if let Some(mine) = gate.generation
            && let Some(theirs) = stored.generation
            && theirs > mine
        {
            warn!(
                stored = theirs,
                mine,
                "Registry generation {theirs} is newer than this pod's {mine}; not reconciling"
            );
            return Ok(RunOutcome::Stale { stored: theirs });
        }
        if stored.content_hash.as_deref() == Some(hash.as_str()) {
            if let Some(mine) = gate.generation
                && stored.generation.is_none_or(|theirs| theirs < mine)
            {
                bounded("claiming the registry generation", gate.state.claim(mine))
                    .await?
                    .map_err(|e| state_failed("claim", e))?;
                return Ok(RunOutcome::Claimed);
            }
            return Ok(RunOutcome::UpToDate { waited });
        }
        let sent = Instant::now();
        if bounded(
            "taking the registry lease",
            gate.state
                .try_acquire(gate.owner, gate.lease_ttl, gate.generation),
        )
        .await?
        .map_err(|e| state_failed("lease", e))?
        {
            break sent;
        }
        // A live lease (or a newer generation, seen on the next read).
        if !waited {
            info!("Another pod holds the registry lease; waiting for it");
        }
        waited = true;
        tokio::select! {
            () = cancel.cancelled() => return Ok(RunOutcome::Cancelled),
            () = tokio::time::sleep((gate.jitter)(gate.poll)) => {}
        }
    };

    let control = LeaseControl {
        gate,
        cancel,
        renewed_at: parking_lot::Mutex::new(acquired_at),
    };
    let mut counts = match reconcile_registry(
        cp,
        metrics,
        &control,
        upstreams,
        routes,
        root_tenant_id,
    )
    .await
    {
        Ok(counts) => counts,
        Err(e) if e.is::<CallTimedOut>() => {
            // See `CallTimedOut`.
            warn!(
                error = %e,
                "Keeping the registry lease until its TTL lapses: the call that timed out may still complete"
            );
            return Err(e);
        }
        Err(e) => {
            // Best effort: the TTL frees the lease if this fails too.
            match bounded(
                "releasing the registry lease",
                gate.state.finish(gate.owner, None),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(release)) => {
                    warn!(error = %release, "Failed to release the registry lease")
                }
                Err(timeout) => warn!(error = %timeout, "Failed to release the registry lease"),
            }
            return Err(e);
        }
    };
    let record = counts.is_clean().then_some(hash.as_str());
    let held = bounded(
        "finishing the registry run",
        gate.state.finish(gate.owner, record),
    )
    .await?
    .map_err(|e| state_failed("finish", e))?;
    if record.is_some() && !held {
        // Another pod may have written meanwhile.
        warn!(
            "Lost the registry lease before recording the content hash; the next check reconciles again"
        );
        counts.stopped = true;
    }
    Ok(RunOutcome::Reconciled(counts))
}

/// Log what a gated run did ("Type provisioning complete" for a reconcile).
pub(crate) fn log_outcome(outcome: &RunOutcome, generation: Option<u64>) {
    match outcome {
        RunOutcome::Reconciled(counts) => {
            info!(?generation, gate = "reconciled", "Registry gate opened");
            log_counts(counts);
        }
        RunOutcome::UpToDate { waited } => info!(
            ?generation,
            gate = if *waited { "waited" } else { "up_to_date" },
            "Registry content up to date"
        ),
        RunOutcome::Claimed => info!(
            ?generation,
            gate = "claimed",
            "Registry content up to date; claimed this pod's generation"
        ),
        RunOutcome::Stale { stored } => info!(
            ?generation,
            stored,
            gate = "stale",
            "Registry owned by a newer generation"
        ),
        RunOutcome::Cancelled => info!(?generation, gate = "cancelled", "Registry run cancelled"),
    }
}

#[cfg(test)]
#[path = "registry_state_tests.rs"]
mod tests;
