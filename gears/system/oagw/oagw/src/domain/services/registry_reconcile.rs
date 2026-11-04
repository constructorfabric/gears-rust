//! The reconcile of registry-managed upstreams and routes with the instances
//! in the types registry (`cpt-cf-oagw-flow-domain-registry-provisioning`,
//! ADR-0018 *Provisioning from the Types Registry*), through the
//! [`RegistryProvisioner`] only the gear holds.
//!
//! One rule decides every failure, *reject or fatal*: after a pure content
//! check, a write the Control Plane refuses skips that instance (logged,
//! counted, its stored row keeps serving), unless something every write
//! depends on is down ([`DomainError::Internal`], an outage-like error, a
//! call timeout) or the tenant resolver refuses OAGW itself, which abort the
//! run. Rejected writes are retried in the next pass while the
//! previous pass changed something, so hand-overs resolve in one run in any
//! listing order; a swap never makes progress and is skipped.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use oagw_sdk::reason;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::management::{
    overlapping_method, registry_route_overlaps, route_drift, upstream_drift,
    validate_route_request,
};
pub(crate) use super::system_identity::provisioning_ctx;
use super::{RegistryProvisioner, UpstreamCheck};
use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, ListQuery, ManagedBy, Route, UpdateRouteRequest,
    UpdateUpstreamRequest, Upstream,
};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::{provisioning_kind as kind, provisioning_reason};
use crate::domain::repo::{DeleteOutcome, RowKey, one_wait_at_the_end};
use crate::domain::type_provisioning::{ProvisionedRoute, ProvisionedUpstream};

/// What [`reconcile_registry`] did to one kind of row.
#[domain_model]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tally {
    /// Rows created.
    pub(crate) created: usize,
    /// Rows updated (a route moved to another upstream counts here).
    pub(crate) updated: usize,
    /// Rows already matching the registry.
    pub(crate) unchanged: usize,
    /// Rows removed.
    pub(crate) removed: usize,
    /// No longer in the registry but kept: routes still use the upstream
    /// (Management API routes, or registry routes the run skipped). Always 0
    /// for routes.
    pub(crate) kept: usize,
    /// Instances skipped: their content failed the check or the Control
    /// Plane rejected their write. A stored row keeps serving.
    pub(crate) failed: usize,
}

/// Counts reported by [`reconcile_registry`].
#[domain_model]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ProvisioningCounts {
    /// What happened to upstreams.
    pub(crate) upstreams: Tally,
    /// What happened to routes.
    pub(crate) routes: Tally,
    /// Write passes over upstreams and routes together.
    pub(crate) passes: usize,
    /// The run stopped before the end (shutdown, or the lease was lost).
    pub(crate) stopped: bool,
}

impl ProvisioningCounts {
    /// Nothing skipped, nothing kept, and the run went to the end: the
    /// stored rows match the registry content.
    pub(crate) fn is_clean(&self) -> bool {
        !self.stopped
            && self.upstreams.failed == 0
            && self.routes.failed == 0
            && self.upstreams.kept == 0
    }
}

/// Between the writes of a run: whether to go on.
#[async_trait]
pub(crate) trait RunControl: Send + Sync {
    /// `true` stops the run (shutdown).
    fn cancelled(&self) -> bool {
        false
    }

    /// Called after the route removals, after each dropped upstream, and
    /// after every pass: `Ok(false)` stops the run (the lease was lost); an
    /// error aborts it.
    async fn renew(&self) -> anyhow::Result<bool> {
        Ok(true)
    }

    /// Called before every write: `Ok(false)` stops the run (shutdown, or a
    /// renewal due by time found the lease lost); an error aborts it.
    async fn before_write(&self) -> anyhow::Result<bool> {
        Ok(!self.cancelled())
    }
}

/// How long one call of a run may take: a dependency, a registry-state
/// statement, or one write. A third of the lease TTL: a write starts at
/// most a third of the TTL after a renewal and ends at most a third later,
/// so it lands inside the lease. A call that times out aborts the run.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// A call of a run that took longer than [`CALL_TIMEOUT`]. Its future was
/// dropped, which stops only this side: a statement already sent (an
/// autocommit `DELETE` waiting on a row lock, a `COMMIT`) can still complete
/// on the database. A run that ends on one keeps its lease until the TTL
/// lapses, so the late statement lands under it.
#[domain_model]
#[derive(Debug, thiserror::Error)]
#[error("{what} timed out after {timeout:?}")]
pub(crate) struct CallTimedOut {
    what: String,
    timeout: Duration,
}

/// `call`, or [`CallTimedOut`] once it has taken [`CALL_TIMEOUT`]. Not
/// cancel-safe: see [`CallTimedOut`].
pub(crate) async fn bounded<T>(what: &str, call: impl Future<Output = T>) -> anyhow::Result<T> {
    tokio::time::timeout(CALL_TIMEOUT, call).await.map_err(|_| {
        anyhow::Error::new(CallTimedOut {
            what: what.to_owned(),
            timeout: CALL_TIMEOUT,
        })
    })
}

/// A write bounded by [`CALL_TIMEOUT`]; a timeout aborts the run.
async fn bounded_write<T>(
    kind: Kind,
    key: RowKey,
    write: impl Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    match bounded(&format!("the write of {} {}", kind.label(), key.id), write).await {
        Ok(result) => result,
        Err(timeout) => Err(Failure::Fatal(timeout)),
    }
}

/// A run nothing else coordinates (the in-memory backend).
#[domain_model]
pub(crate) struct Unattended;

impl RunControl for Unattended {}

/// Make the registry-managed upstreams and routes match the instances other
/// gears registered in types-registry (`cpt-cf-oagw-flow-domain-registry-provisioning`).
///
/// Phase 1 loads the stored registry rows; phase 2 checks the content
/// without reading anything and skips bad instances; phase 3 removes rows the
/// registry no longer has (a dropped upstream that Management API routes use
/// is kept, never cascaded); phases 4 and 5 write upstreams, then routes, in
/// passes. Row IDs are the GTS instance UUIDs. Only an infrastructure error
/// returns `Err`.
///
/// No client waits to see the run's writes on another replica, so the writes
/// do not each wait for their change to be published; the run waits once,
/// at its end (ADR-0020 *Keeping the Cache Fresh*).
pub(crate) async fn reconcile_registry(
    cp: &dyn RegistryProvisioner,
    metrics: &dyn OagwMetricsPort,
    control: &dyn RunControl,
    upstreams: &[ProvisionedUpstream],
    routes: &[ProvisionedRoute],
    root_tenant_id: Uuid,
) -> anyhow::Result<ProvisioningCounts> {
    // Boxed: the run's future is large, and the scope would otherwise move
    // it on the stack.
    one_wait_at_the_end(Box::pin(reconcile(
        cp,
        metrics,
        control,
        upstreams,
        routes,
        root_tenant_id,
    )))
    .await
}

/// [`reconcile_registry`], with each write's wait left to the caller.
async fn reconcile(
    cp: &dyn RegistryProvisioner,
    metrics: &dyn OagwMetricsPort,
    control: &dyn RunControl,
    upstreams: &[ProvisionedUpstream],
    routes: &[ProvisionedRoute],
    root_tenant_id: Uuid,
) -> anyhow::Result<ProvisioningCounts> {
    let mut run = Run {
        cp,
        metrics,
        control,
        counts: ProvisioningCounts::default(),
    };

    // -- Phase 1: load --
    let mut stored_upstreams: HashMap<RowKey, Upstream> = bounded(
        "listing registry-managed upstreams",
        cp.registry_upstreams(),
    )
    .await?
    .map_err(|e| anyhow::anyhow!("Failed to list registry-managed upstreams: {e}"))?
    .into_iter()
    .map(|u| (key_of(u.tenant_id, u.id), u))
    .collect();
    let mut stored_routes: HashMap<RowKey, Route> =
        bounded("listing registry-managed routes", cp.registry_routes())
            .await?
            .map_err(|e| anyhow::anyhow!("Failed to list registry-managed routes: {e}"))?
            .into_iter()
            .map(|r| (key_of(r.tenant_id, r.id), r))
            .collect();

    // -- Phase 2: content check --
    let checked = run.content_check(
        upstreams,
        routes,
        root_tenant_id,
        &stored_upstreams,
        &stored_routes,
    );

    // -- Phase 3: removals --
    let recreated = run
        .remove(&checked, &mut stored_upstreams, &mut stored_routes)
        .await?;
    if run.counts.stopped {
        return Ok(run.counts);
    }

    // -- Phase 4: upstreams --
    let applied = run.apply_upstreams(&checked, &stored_upstreams).await?;
    if run.counts.stopped {
        return Ok(run.counts);
    }

    // -- Phase 5: routes --
    run.apply_routes(&checked, &stored_routes, &applied, &recreated)
        .await?;
    Ok(run.counts)
}

/// The registry content that passed the check, and every instance key.
struct Checked<'a> {
    /// Upstreams to write, with the alias each would store.
    upstreams: Vec<(RowKey, &'a ProvisionedUpstream, String)>,
    /// Routes to write.
    routes: Vec<(RowKey, &'a ProvisionedRoute)>,
    /// Every upstream instance with an ID, skipped or not: never removed.
    wanted_upstreams: HashSet<RowKey>,
    /// Every route instance with an ID, skipped or not: never removed.
    wanted_routes: HashSet<RowKey>,
}

/// What deleting one registry route did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removal {
    Deleted,
    /// Already gone.
    Gone,
    /// Rejected and skipped: the row stays.
    Refused,
}

/// What applying one registry instance did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applied {
    Created,
    Updated,
    Unchanged,
}

impl Applied {
    fn count(self, tally: &mut Tally) {
        match self {
            Self::Created => tally.created += 1,
            Self::Updated => tally.updated += 1,
            Self::Unchanged => tally.unchanged += 1,
        }
    }
}

/// The kind of a registry instance.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An upstream instance.
    Upstream,
    /// A route instance.
    Route,
}

impl Kind {
    /// The `kind` label of `registry_provisioning_failures_total`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Upstream => kind::UPSTREAM,
            Self::Route => kind::ROUTE,
        }
    }
}

/// How the reconcile treats a failed write.
enum Failure {
    /// The Control Plane refused this instance: skip it, go on.
    Rejected {
        error: Box<DomainError>,
        reason: &'static str,
    },
    /// Something every write depends on is down: abort the run.
    Fatal(anyhow::Error),
}

/// Reject or fatal, by the rule in the module doc. An `unexpected` error is
/// logged with its variant: the skip logs only `Display`, which for some
/// variants is the detail alone.
fn classify(kind: Kind, key: RowKey, error: DomainError) -> Failure {
    match reason_of(&error) {
        Some(reason) => {
            if reason == provisioning_reason::UNEXPECTED {
                error!(
                    kind = kind.label(),
                    id = %key.id,
                    tenant_id = %key.tenant_id,
                    error = ?error,
                    "Control Plane write returned a proxy-time error"
                );
            }
            Failure::Rejected {
                reason,
                error: Box::new(error),
            }
        }
        None => Failure::Fatal(provisioning_failed(kind, key, &error)),
    }
}

/// The `reason` label of a rejected write, or `None` for an error that
/// aborts the run. Every variant is listed, so a new one needs a decision.
fn reason_of(error: &DomainError) -> Option<&'static str> {
    use provisioning_reason as why;
    match error {
        // Something every write depends on is down. A Control Plane write
        // returns only `Internal` of these; the rest would mean the same.
        DomainError::Internal { .. }
        | DomainError::DownstreamError { .. }
        | DomainError::LinkUnavailable { .. }
        | DomainError::CircuitBreakerOpen { .. }
        | DomainError::ConnectionTimeout { .. }
        | DomainError::RequestTimeout { .. }
        | DomainError::IdleTimeout { .. }
        | DomainError::StreamAborted { .. }
        | DomainError::ProtocolError { .. } => None,
        DomainError::Forbidden { reason, .. }
            if reason == reason::permission::TENANT_RESOLVER_UNAUTHORIZED =>
        {
            None
        }
        DomainError::Forbidden { .. } => Some(why::FORBIDDEN),
        DomainError::Conflict { .. } => Some(why::CONFLICT),
        DomainError::NotFound { .. } => Some(why::NOT_FOUND),
        DomainError::Validation { .. } => Some(why::VALIDATION),
        DomainError::RegistryManaged { .. } => Some(why::REGISTRY_MANAGED),
        DomainError::SecretRefNotAccessible { .. } | DomainError::SecretNotFound { .. } => {
            Some(why::SECRET_NOT_ACCESSIBLE)
        }
        DomainError::PluginNotFound { .. } | DomainError::PluginInUse { .. } => Some(why::PLUGIN),
        // Proxy-time errors: about a request, never a Control Plane write.
        DomainError::UpstreamDisabled { .. }
        | DomainError::MissingTargetHost { .. }
        | DomainError::InvalidTargetHost { .. }
        | DomainError::UnknownTargetHost { .. }
        | DomainError::AuthenticationFailed { .. }
        | DomainError::PayloadTooLarge { .. }
        | DomainError::RateLimitExceeded { .. }
        | DomainError::GuardRejected { .. }
        | DomainError::CorsOriginNotAllowed { .. }
        | DomainError::CorsMethodNotAllowed { .. } => Some(why::UNEXPECTED),
    }
}

/// A rejection that names no write error (the content check, a lookup).
fn rejected(reason: &'static str, detail: impl Into<String>) -> Failure {
    Failure::Rejected {
        error: Box::new(DomainError::validation(detail)),
        reason,
    }
}

/// One reconcile run: the collaborators and what it did so far.
struct Run<'a> {
    cp: &'a dyn RegistryProvisioner,
    metrics: &'a dyn OagwMetricsPort,
    control: &'a dyn RunControl,
    counts: ProvisioningCounts,
}

impl Run<'_> {
    fn tally(&mut self, kind: Kind) -> &mut Tally {
        match kind {
            Kind::Upstream => &mut self.counts.upstreams,
            Kind::Route => &mut self.counts.routes,
        }
    }

    /// Skip an instance: log it, count it, nothing else. Dependents fail on
    /// their own write.
    fn skip(
        &mut self,
        kind: Kind,
        key: Option<RowKey>,
        reason: &'static str,
        detail: &str,
        serving: bool,
    ) {
        let what = kind.label();
        error!(
            kind = what,
            id = ?key.map(|k| k.id),
            tenant_id = ?key.map(|k| k.tenant_id),
            reason,
            detail,
            stored_row_serves = serving,
            "Skipped {what} from types-registry"
        );
        self.metrics
            .record_registry_provisioning_failure(what, reason);
        self.tally(kind).failed += 1;
    }

    /// Phase 2: the checks that read nothing; every failure skips.
    fn content_check<'c>(
        &mut self,
        upstreams: &'c [ProvisionedUpstream],
        routes: &'c [ProvisionedRoute],
        root_tenant_id: Uuid,
        stored_upstreams: &HashMap<RowKey, Upstream>,
        stored_routes: &HashMap<RowKey, Route>,
    ) -> Checked<'c> {
        let mut checked = Checked {
            upstreams: Vec::new(),
            routes: Vec::new(),
            wanted_upstreams: HashSet::new(),
            wanted_routes: HashSet::new(),
        };

        for u in upstreams {
            let Some(key) = instance_key(u.request.id, u.tenant_id, root_tenant_id) else {
                self.skip(
                    Kind::Upstream,
                    None,
                    provisioning_reason::INVALID_INSTANCE,
                    "upstream instance has no ID",
                    false,
                );
                continue;
            };
            checked.wanted_upstreams.insert(key);
            match self.cp.check_registry_upstream(&u.request) {
                Ok(alias) => checked.upstreams.push((key, u, alias)),
                Err(e) => self.skip(
                    Kind::Upstream,
                    Some(key),
                    provisioning_reason::INVALID_REQUEST,
                    &e.to_string(),
                    stored_upstreams.contains_key(&key),
                ),
            }
        }
        // Two instances of one tenant on one alias: neither can be written.
        let mut by_alias: HashMap<(Uuid, &str), usize> = HashMap::new();
        for (key, _, alias) in &checked.upstreams {
            *by_alias.entry((key.tenant_id, alias.as_str())).or_default() += 1;
        }
        let duplicated: Vec<(RowKey, String)> = checked
            .upstreams
            .iter()
            .filter(|(key, _, alias)| by_alias[&(key.tenant_id, alias.as_str())] > 1)
            .map(|(key, _, alias)| (*key, alias.clone()))
            .collect();
        for (key, alias) in &duplicated {
            self.skip(
                Kind::Upstream,
                Some(*key),
                provisioning_reason::DUPLICATE_ALIAS,
                &format!(
                    "several upstream instances of tenant {} derive alias '{alias}'",
                    key.tenant_id
                ),
                stored_upstreams.contains_key(key),
            );
        }
        checked
            .upstreams
            .retain(|(key, _, _)| !duplicated.iter().any(|(k, _)| k == key));

        for r in routes {
            let Some(key) = instance_key(r.request.id, r.tenant_id, root_tenant_id) else {
                self.skip(
                    Kind::Route,
                    None,
                    provisioning_reason::INVALID_INSTANCE,
                    "route instance has no ID",
                    false,
                );
                continue;
            };
            checked.wanted_routes.insert(key);
            let serving = stored_routes.contains_key(&key);
            if let Err(e) = validate_route_request(&r.request) {
                self.skip(
                    Kind::Route,
                    Some(key),
                    provisioning_reason::INVALID_REQUEST,
                    &e.to_string(),
                    serving,
                );
                continue;
            }
            let upstream = key_of(key.tenant_id, r.request.upstream_id);
            if !checked.wanted_upstreams.contains(&upstream) {
                self.skip(
                    Kind::Route,
                    Some(key),
                    provisioning_reason::UNKNOWN_UPSTREAM,
                    &format!(
                        "upstream {} is not an upstream instance of tenant {} in types-registry",
                        r.request.upstream_id, key.tenant_id
                    ),
                    serving,
                );
                continue;
            }
            checked.routes.push((key, r));
        }
        let requests: Vec<(RowKey, &CreateRouteRequest)> = checked
            .routes
            .iter()
            .map(|(key, r)| (*key, &r.request))
            .collect();
        let mut overlapping: HashMap<RowKey, String> = HashMap::new();
        for (a, b, method) in registry_route_overlaps(&requests) {
            let detail = format!(
                "types-registry routes {} and {} overlap on method {method:?}",
                a.id, b.id
            );
            overlapping.entry(a).or_insert_with(|| detail.clone());
            overlapping.entry(b).or_insert(detail);
        }
        let mut skipped: Vec<_> = overlapping.into_iter().collect();
        skipped.sort();
        for (key, detail) in &skipped {
            self.skip(
                Kind::Route,
                Some(*key),
                provisioning_reason::OVERLAPPING_REGISTRY_ROUTES,
                detail,
                stored_routes.contains_key(key),
            );
        }
        checked
            .routes
            .retain(|(key, _)| !skipped.iter().any(|(k, _)| k == key));
        checked
    }

    /// Whether the next write may go ahead; marks the run stopped if not.
    async fn may_write(&mut self) -> anyhow::Result<bool> {
        if !self.control.before_write().await? {
            self.counts.stopped = true;
            return Ok(false);
        }
        Ok(true)
    }

    /// Stop here if the run was cancelled or lost its lease.
    async fn checkpoint(&mut self) -> anyhow::Result<bool> {
        if self.control.cancelled() || !self.control.renew().await? {
            self.counts.stopped = true;
            return Ok(false);
        }
        Ok(true)
    }

    /// Phase 3: delete registry routes the registry no longer has, then
    /// each registry upstream it no longer has: its registry routes that
    /// phase 5 writes again first (ours to delete), then the upstream only if
    /// nothing uses it. A skipped registry route stays, and keeps the
    /// upstream. Returns the routes deleted with their upstream, re-created
    /// in phase 5.
    async fn remove(
        &mut self,
        checked: &Checked<'_>,
        stored_upstreams: &mut HashMap<RowKey, Upstream>,
        stored_routes: &mut HashMap<RowKey, Route>,
    ) -> anyhow::Result<HashSet<RowKey>> {
        // Routes phase 5 writes again: they passed the check, and the
        // upstream they name will exist (written in phase 4, or stored). A
        // skipped route is not among them, nor one whose upstream the check
        // skipped: deleting it here would leave it on no upstream.
        let new_upstreams: HashSet<RowKey> =
            checked.upstreams.iter().map(|(key, _, _)| *key).collect();
        let writable: HashSet<RowKey> = checked
            .routes
            .iter()
            .filter(|(key, r)| {
                let target = key_of(key.tenant_id, r.request.upstream_id);
                new_upstreams.contains(&target) || stored_upstreams.contains_key(&target)
            })
            .map(|(key, _)| *key)
            .collect();
        let mut dropped_upstreams: Vec<RowKey> = stored_upstreams
            .keys()
            .filter(|key| !checked.wanted_upstreams.contains(key))
            .copied()
            .collect();
        dropped_upstreams.sort();

        // A dropped upstream that a route this run neither deletes nor writes
        // again still uses (a Management API route, a skipped registry route)
        // stays. Routes leaving it are then not deleted first: phase 5 moves
        // them in place, and a rejected move keeps them serving here.
        let mut staying: HashSet<RowKey> = HashSet::new();
        for &key in &dropped_upstreams {
            let leaving: HashSet<Uuid> = stored_routes
                .iter()
                .filter(|(k, r)| {
                    r.tenant_id == key.tenant_id
                        && r.upstream_id == key.id
                        && (writable.contains(k) || !checked.wanted_routes.contains(k))
                })
                .map(|(k, _)| k.id)
                .collect();
            if self.used_by_others(key, &leaving).await? {
                staying.insert(key);
            }
        }
        // A route moving to another tenant keeps its ID, so it can be created
        // there only once this row is gone.
        let wanted_route_ids: HashSet<Uuid> = checked.wanted_routes.iter().map(|k| k.id).collect();

        let mut dropped_routes: Vec<RowKey> = stored_routes
            .iter()
            .filter(|(key, r)| {
                !checked.wanted_routes.contains(key)
                    && !(wanted_route_ids.contains(&key.id)
                        && staying.contains(&key_of(r.tenant_id, r.upstream_id)))
            })
            .map(|(key, _)| *key)
            .collect();
        dropped_routes.sort();
        for key in dropped_routes {
            if !self.may_write().await? {
                return Ok(HashSet::new());
            }
            match self.delete_route(key).await? {
                Removal::Deleted => {
                    info!(id = %key.id, tenant_id = %key.tenant_id, "Removed route no longer in types-registry");
                    self.counts.routes.removed += 1;
                    stored_routes.remove(&key);
                }
                Removal::Gone => {
                    stored_routes.remove(&key);
                }
                // Still on its upstream, which it keeps.
                Removal::Refused => {}
            }
        }
        if !self.checkpoint().await? {
            return Ok(HashSet::new());
        }

        let mut recreated = HashSet::new();
        for key in dropped_upstreams {
            let mut on_it: Vec<RowKey> = stored_routes
                .iter()
                .filter(|(k, r)| {
                    !staying.contains(&key)
                        && r.tenant_id == key.tenant_id
                        && r.upstream_id == key.id
                        && writable.contains(k)
                })
                .map(|(k, _)| *k)
                .collect();
            on_it.sort();
            for route in on_it {
                if !self.may_write().await? {
                    return Ok(recreated);
                }
                // A refused delete leaves it here; phase 5 moves it in place.
                if self.delete_route(route).await? != Removal::Refused {
                    info!(
                        id = %route.id,
                        tenant_id = %route.tenant_id,
                        upstream_id = %key.id,
                        "Deleted route with its dropped upstream; re-created on its new upstream next"
                    );
                    stored_routes.remove(&route);
                    recreated.insert(route);
                }
            }
            if !self.may_write().await? {
                return Ok(recreated);
            }
            let ctx = provisioning_ctx(key.tenant_id)?;
            match bounded(
                "the delete of a dropped registry upstream",
                self.cp.delete_registry_upstream_if_unused(&ctx, key.id),
            )
            .await?
            {
                Ok(DeleteOutcome::Deleted) => {
                    info!(id = %key.id, tenant_id = %key.tenant_id, "Removed upstream no longer in types-registry");
                    self.counts.upstreams.removed += 1;
                    stored_upstreams.remove(&key);
                }
                Ok(DeleteOutcome::NotFound) => {
                    stored_upstreams.remove(&key);
                }
                Ok(DeleteOutcome::InUse) => {
                    let mut registry: Vec<Uuid> = stored_routes
                        .values()
                        .filter(|r| r.tenant_id == key.tenant_id && r.upstream_id == key.id)
                        .map(|r| r.id)
                        .collect();
                    registry.sort();
                    self.keep(&ctx, key, &registry).await?;
                }
                Err(e) => match classify(Kind::Upstream, key, e) {
                    Failure::Fatal(e) => return Err(e),
                    Failure::Rejected { error, reason } => {
                        self.skip(Kind::Upstream, Some(key), reason, &error.to_string(), true);
                    }
                },
            }
            if !self.checkpoint().await? {
                return Ok(recreated);
            }
        }
        Ok(recreated)
    }

    /// Whether a route other than `leaving` (the routes this run deletes
    /// or writes again) is on the dropped upstream `key`. Routes list in ID
    /// order, so one more than `leaving` holds such a route if any exists.
    async fn used_by_others(&self, key: RowKey, leaving: &HashSet<Uuid>) -> anyhow::Result<bool> {
        let ctx = provisioning_ctx(key.tenant_id)?;
        let top = u32::try_from(leaving.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let routes = bounded(
            "listing the routes of a dropped upstream",
            self.cp
                .list_routes(&ctx, Some(key.id), &ListQuery { top, skip: 0 }),
        )
        .await?
        .map_err(|e| provisioning_failed(Kind::Upstream, key, &e))?;
        Ok(routes.iter().any(|r| !leaving.contains(&r.id)))
    }

    /// A dropped upstream that routes still use stays, with its last
    /// registry configuration and its alias, until they are gone: Management
    /// API routes, or registry routes (`registry`, every one still on it)
    /// that the content check skipped or whose delete was refused.
    async fn keep(
        &mut self,
        ctx: &SecurityContext,
        key: RowKey,
        registry: &[Uuid],
    ) -> anyhow::Result<()> {
        // Only for the log: the API routes among its first ten routes by ID.
        let users = bounded(
            "listing the routes of a kept upstream",
            self.cp
                .list_routes(ctx, Some(key.id), &ListQuery { top: 10, skip: 0 }),
        )
        .await?
        .map_err(|e| provisioning_failed(Kind::Upstream, key, &e))?;
        let api: Vec<String> = users
            .iter()
            .filter(|r| r.managed_by == ManagedBy::Api)
            .map(|r| r.id.to_string())
            .collect();
        let api = api.join(", ");
        if registry.is_empty() {
            warn!(
                id = %key.id,
                tenant_id = %key.tenant_id,
                routes = %api,
                "Kept upstream no longer in types-registry: Management API routes use it; \
                 it is removed by the first reconcile after they are deleted"
            );
        } else {
            let registry: Vec<String> = registry.iter().map(Uuid::to_string).collect();
            warn!(
                id = %key.id,
                tenant_id = %key.tenant_id,
                registry_routes = %registry.join(", "),
                api_routes = %api,
                "Kept upstream no longer in types-registry: types-registry routes are still \
                 on it (skipped, or their delete was refused: see the errors above); it is \
                 removed by the first reconcile after they are gone (and any Management API \
                 routes are deleted)"
            );
        }
        self.metrics.record_registry_provisioning_failure(
            Kind::Upstream.label(),
            provisioning_reason::KEPT_IN_USE,
        );
        self.counts.upstreams.kept += 1;
        Ok(())
    }

    /// Delete one registry route. A rejection skips it, and its row stays.
    async fn delete_route(&mut self, key: RowKey) -> anyhow::Result<Removal> {
        let ctx = provisioning_ctx(key.tenant_id)?;
        match bounded(
            "the delete of a registry route",
            self.cp.delete_registry_route(&ctx, key.id),
        )
        .await?
        {
            Ok(()) => Ok(Removal::Deleted),
            Err(DomainError::NotFound { .. }) => Ok(Removal::Gone),
            Err(e) => match classify(Kind::Route, key, e) {
                Failure::Fatal(e) => Err(e),
                Failure::Rejected { error, reason } => {
                    self.skip(Kind::Route, Some(key), reason, &error.to_string(), true);
                    Ok(Removal::Refused)
                }
            },
        }
    }

    /// Phase 4: create or update upstreams in passes. Returns every upstream
    /// written or confirmed in this run.
    async fn apply_upstreams(
        &mut self,
        checked: &Checked<'_>,
        stored: &HashMap<RowKey, Upstream>,
    ) -> anyhow::Result<HashSet<RowKey>> {
        // Stored instances first: one whose alias changed usually frees it
        // before a new one takes it, so most hand-overs need one pass.
        let (old, new): (Vec<_>, Vec<_>) = checked
            .upstreams
            .iter()
            .partition(|(key, _, _)| stored.contains_key(key));
        let mut pending: Vec<&(RowKey, &ProvisionedUpstream, String)> =
            old.into_iter().chain(new).collect();
        let mut applied = HashSet::new();
        if pending.is_empty() {
            return Ok(applied);
        }
        let mut last_error: HashMap<RowKey, (Box<DomainError>, &'static str)> = HashMap::new();
        loop {
            self.counts.passes += 1;
            let mut rejected = Vec::new();
            let mut changed = false;
            for item in pending {
                let (key, u, _) = item;
                if !self.may_write().await? {
                    return Ok(applied);
                }
                match bounded_write(
                    Kind::Upstream,
                    *key,
                    apply_upstream(self.cp, *key, u, stored.get(key)),
                )
                .await
                {
                    Ok(outcome) => {
                        applied.insert(*key);
                        changed |= outcome != Applied::Unchanged;
                        outcome.count(&mut self.counts.upstreams);
                    }
                    Err(Failure::Rejected { error, reason }) => {
                        last_error.insert(*key, (error, reason));
                        rejected.push(item);
                    }
                    Err(Failure::Fatal(e)) => return Err(e),
                }
            }
            if !self.checkpoint().await? {
                return Ok(applied);
            }
            pending = rejected;
            if pending.is_empty() || !changed {
                break;
            }
        }
        for (key, _, alias) in &pending {
            let Some((error, reason)) = last_error.remove(key) else {
                continue;
            };
            // A conflict on an alias another still-pending registry upstream
            // holds: a swap or a longer cycle, which no order resolves, or a
            // hand-over from an upstream rejected for another reason.
            let cycle = matches!(*error, DomainError::Conflict { .. })
                && pending.iter().any(|(other, _, _)| {
                    other != key && stored.get(other).is_some_and(|s| s.alias == *alias)
                });
            if cycle {
                self.skip(
                    Kind::Upstream,
                    Some(*key),
                    provisioning_reason::ALIAS_CYCLE,
                    &format!(
                        "alias '{alias}' is held by another registry upstream this run could not \
                         move either; if they swap aliases, split the change across two releases, \
                         otherwise fix that upstream ({error})"
                    ),
                    stored.contains_key(key),
                );
            } else {
                self.skip(
                    Kind::Upstream,
                    Some(*key),
                    reason,
                    &error.to_string(),
                    stored.contains_key(key),
                );
            }
        }
        Ok(applied)
    }

    /// Phase 5: create, update or move routes in passes.
    async fn apply_routes(
        &mut self,
        checked: &Checked<'_>,
        stored: &HashMap<RowKey, Route>,
        applied_upstreams: &HashSet<RowKey>,
        recreated: &HashSet<RowKey>,
    ) -> anyhow::Result<()> {
        let mut pending: Vec<&(RowKey, &ProvisionedRoute)> = checked.routes.iter().collect();
        if pending.is_empty() {
            return Ok(());
        }
        let mut last_error: HashMap<RowKey, (Box<DomainError>, &'static str)> = HashMap::new();
        loop {
            self.counts.passes += 1;
            let mut rejected = Vec::new();
            let mut changed = false;
            for item in pending {
                let (key, r) = item;
                if !self.may_write().await? {
                    return Ok(());
                }
                let upstream = key_of(key.tenant_id, r.request.upstream_id);
                let check = if applied_upstreams.contains(&upstream) {
                    UpstreamCheck::KnownRegistry
                } else {
                    UpstreamCheck::Read
                };
                match bounded_write(
                    Kind::Route,
                    *key,
                    apply_route(self.cp, *key, r, stored.get(key), check),
                )
                .await
                {
                    Ok(outcome) => {
                        changed |= outcome != Applied::Unchanged;
                        let outcome = match outcome {
                            // Deleted with its dropped upstream, now on its new one.
                            Applied::Created if recreated.contains(key) => Applied::Updated,
                            outcome => outcome,
                        };
                        outcome.count(&mut self.counts.routes);
                    }
                    Err(Failure::Rejected { error, reason }) => {
                        last_error.insert(*key, (error, reason));
                        rejected.push(item);
                    }
                    Err(Failure::Fatal(e)) => return Err(e),
                }
            }
            if !self.checkpoint().await? {
                return Ok(());
            }
            pending = rejected;
            if pending.is_empty() || !changed {
                break;
            }
        }
        for (key, r) in &pending {
            let Some((error, reason)) = last_error.remove(key) else {
                continue;
            };
            // An overlap with the stored row of another still-pending
            // registry route: a swap or a longer cycle.
            let cycle = matches!(*error, DomainError::Conflict { .. })
                && pending.iter().any(|(other, _)| {
                    other != key
                        && stored
                            .get(other)
                            .is_some_and(|s| overlaps_stored(&r.request, s))
                });
            // A route deleted with its dropped upstream has no row left.
            let serving = stored.contains_key(key);
            if recreated.contains(key) {
                error!(
                    id = %key.id,
                    tenant_id = %key.tenant_id,
                    "Route deleted with its dropped upstream was not re-created on its new \
                     upstream; it serves no requests until the write below succeeds"
                );
            }
            if cycle {
                self.skip(
                    Kind::Route,
                    Some(*key),
                    provisioning_reason::PATH_CYCLE,
                    &format!(
                        "its path is held by another registry route this run could not move \
                         either; if they swap paths, split the change across two releases, \
                         otherwise fix that route ({error})"
                    ),
                    serving,
                );
            } else {
                self.skip(Kind::Route, Some(*key), reason, &error.to_string(), serving);
            }
        }
        Ok(())
    }
}

/// Whether the route `req` would write overlaps stored route `stored`.
fn overlaps_stored(req: &CreateRouteRequest, stored: &Route) -> bool {
    if !req.enabled || !stored.enabled || stored.upstream_id != req.upstream_id {
        return false;
    }
    let (Ok(rules), Some(stored_http)) = (validate_route_request(req), &stored.match_rules.http)
    else {
        return false;
    };
    rules.http.as_ref().is_some_and(|http| {
        overlapping_method(http, req.priority, stored_http, stored.priority).is_some()
    })
}

/// Create or update one upstream instance.
async fn apply_upstream(
    cp: &dyn RegistryProvisioner,
    key: RowKey,
    u: &ProvisionedUpstream,
    stored: Option<&Upstream>,
) -> Result<Applied, Failure> {
    let ctx = provisioning_ctx(key.tenant_id).map_err(Failure::Fatal)?;
    let found;
    let stored = match stored {
        Some(stored) => stored,
        None => match cp.create_registry_upstream(&ctx, u.request.clone()).await {
            Ok(created) => {
                info!(
                    id = %key.id,
                    tenant_id = %key.tenant_id,
                    alias = %created.alias,
                    "Provisioned upstream from types-registry"
                );
                return Ok(Applied::Created);
            }
            // The ID or the alias is held: by our own row (another replica
            // wrote it), by an API row, or by another row's alias.
            Err(conflict @ DomainError::Conflict { .. }) => {
                match cp.get_upstream(&ctx, key.id).await {
                    Ok(row) if row.managed_by == ManagedBy::Registry => {
                        found = row;
                        &found
                    }
                    Ok(_) => {
                        return Err(rejected(
                            provisioning_reason::ID_HELD_BY_API,
                            format!("upstream {} was created through the Management API", key.id),
                        ));
                    }
                    Err(DomainError::NotFound { .. }) => {
                        return Err(Failure::Rejected {
                            reason: provisioning_reason::CONFLICT,
                            error: Box::new(conflict),
                        });
                    }
                    Err(e) => return Err(classify(Kind::Upstream, key, e)),
                }
            }
            Err(e) => return Err(classify(Kind::Upstream, key, e)),
        },
    };
    let drift = upstream_drift(stored, &u.request).map_err(|e| classify(Kind::Upstream, key, e))?;
    if drift.is_empty() {
        debug!(id = %key.id, tenant_id = %key.tenant_id, "upstream from types-registry is up to date");
        return Ok(Applied::Unchanged);
    }
    cp.update_registry_upstream(&ctx, key.id, upstream_update(&u.request))
        .await
        .map_err(|e| classify(Kind::Upstream, key, e))?;
    info!(id = %key.id, tenant_id = %key.tenant_id, ?drift, "Updated upstream from types-registry");
    Ok(Applied::Updated)
}

/// Create, update or move one route instance.
async fn apply_route(
    cp: &dyn RegistryProvisioner,
    key: RowKey,
    r: &ProvisionedRoute,
    stored: Option<&Route>,
    check: UpstreamCheck,
) -> Result<Applied, Failure> {
    let ctx = provisioning_ctx(key.tenant_id).map_err(Failure::Fatal)?;
    let found;
    let stored = match stored {
        Some(stored) => stored,
        None => match cp
            .create_registry_route(&ctx, r.request.clone(), check)
            .await
        {
            Ok(_) => {
                info!(id = %key.id, tenant_id = %key.tenant_id, "Provisioned route from types-registry");
                return Ok(Applied::Created);
            }
            // The ID is held (our own row, or an API row), or the route
            // overlaps a stored one.
            Err(conflict @ DomainError::Conflict { .. }) => {
                match cp.get_route(&ctx, key.id).await {
                    Ok(row) if row.managed_by == ManagedBy::Registry => {
                        found = row;
                        &found
                    }
                    Ok(_) => {
                        return Err(rejected(
                            provisioning_reason::ID_HELD_BY_API,
                            format!("route {} was created through the Management API", key.id),
                        ));
                    }
                    Err(DomainError::NotFound { .. }) => {
                        return Err(Failure::Rejected {
                            reason: provisioning_reason::CONFLICT,
                            error: Box::new(conflict),
                        });
                    }
                    Err(e) => return Err(classify(Kind::Route, key, e)),
                }
            }
            Err(e) => return Err(classify(Kind::Route, key, e)),
        },
    };
    let drift = route_drift(stored, &r.request);
    if drift.is_empty() {
        debug!(id = %key.id, tenant_id = %key.tenant_id, "route from types-registry is up to date");
        return Ok(Applied::Unchanged);
    }
    let moved = stored.upstream_id != r.request.upstream_id;
    cp.update_registry_route(
        &ctx,
        key.id,
        r.request.upstream_id,
        route_update(&r.request),
        check,
    )
    .await
    .map_err(|e| classify(Kind::Route, key, e))?;
    if moved {
        info!(
            id = %key.id,
            tenant_id = %key.tenant_id,
            from = %stored.upstream_id,
            to = %r.request.upstream_id,
            "Moved route to another upstream from types-registry"
        );
    } else {
        info!(id = %key.id, tenant_id = %key.tenant_id, ?drift, "Updated route from types-registry");
    }
    Ok(Applied::Updated)
}

fn key_of(tenant_id: Uuid, id: Uuid) -> RowKey {
    RowKey { tenant_id, id }
}

/// The row key of a registry instance: its GTS instance UUID in its tenant,
/// or the root tenant when the instance names none. `None` without an ID.
fn instance_key(id: Option<Uuid>, tenant_id: Option<Uuid>, root_tenant_id: Uuid) -> Option<RowKey> {
    Some(key_of(tenant_id.unwrap_or(root_tenant_id), id?))
}

/// The full replacement that makes a stored upstream match `req`.
fn upstream_update(req: &CreateUpstreamRequest) -> UpdateUpstreamRequest {
    UpdateUpstreamRequest {
        server: req.server.clone(),
        protocol: req.protocol.clone(),
        alias: req.alias.clone(),
        auth: req.auth.clone(),
        headers: req.headers.clone(),
        plugins: req.plugins.clone(),
        rate_limit: req.rate_limit.clone(),
        cors: req.cors.clone(),
        tags: req.tags.clone(),
        enabled: req.enabled,
    }
}

/// The full replacement that makes a stored route match `req` (its upstream
/// goes separately).
fn route_update(req: &CreateRouteRequest) -> UpdateRouteRequest {
    UpdateRouteRequest {
        match_rules: req.match_rules.clone(),
        plugins: req.plugins.clone(),
        rate_limit: req.rate_limit.clone(),
        cors: req.cors.clone(),
        tags: req.tags.clone(),
        priority: req.priority,
        enabled: req.enabled,
    }
}

fn provisioning_failed(kind: Kind, key: RowKey, error: &DomainError) -> anyhow::Error {
    let what = kind.label();
    anyhow::anyhow!(
        "Failed to provision {what} {} (tenant={}): {error}",
        key.id,
        key.tenant_id
    )
}

/// Log what a run did ("Type provisioning complete").
pub(crate) fn log_counts(counts: &ProvisioningCounts) {
    info!(
        upstreams_created = counts.upstreams.created,
        upstreams_updated = counts.upstreams.updated,
        upstreams_unchanged = counts.upstreams.unchanged,
        upstreams_removed = counts.upstreams.removed,
        upstreams_kept = counts.upstreams.kept,
        upstreams_failed = counts.upstreams.failed,
        routes_created = counts.routes.created,
        routes_updated = counts.routes.updated,
        routes_unchanged = counts.routes.unchanged,
        routes_removed = counts.routes.removed,
        routes_failed = counts.routes.failed,
        passes = counts.passes,
        stopped = counts.stopped,
        "Type provisioning complete"
    );
}

#[cfg(test)]
#[path = "registry_reconcile_tests.rs"]
pub(crate) mod tests;
