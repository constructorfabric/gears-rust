//! Gear Manager - tracks and manages all live gear instances in the runtime

use dashmap::DashMap;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Represents an endpoint where a gear instance can be reached
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub uri: String,
}

/// Typed view of an endpoint for parsing and matching
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointKind {
    /// TCP endpoint with resolved socket address
    Tcp(std::net::SocketAddr),
    /// Unix domain socket with file path
    Uds(std::path::PathBuf),
    /// Other/unparsed endpoint URI
    Other(String),
}

impl Endpoint {
    pub fn from_uri<S: Into<String>>(s: S) -> Self {
        Self { uri: s.into() }
    }

    pub fn uds(path: impl AsRef<std::path::Path>) -> Self {
        Self {
            uri: format!("unix://{}", path.as_ref().display()),
        }
    }

    #[must_use]
    pub fn http(host: &str, port: u16) -> Self {
        Self {
            uri: format!("http://{host}:{port}"),
        }
    }

    #[must_use]
    pub fn https(host: &str, port: u16) -> Self {
        Self {
            uri: format!("https://{host}:{port}"),
        }
    }

    /// Parse the endpoint URI into a typed view
    #[must_use]
    pub fn kind(&self) -> EndpointKind {
        if let Some(rest) = self.uri.strip_prefix("unix://") {
            return EndpointKind::Uds(std::path::PathBuf::from(rest));
        }
        if let Some(rest) = self.uri.strip_prefix("http://")
            && let Ok(addr) = rest.parse::<std::net::SocketAddr>()
        {
            return EndpointKind::Tcp(addr);
        }
        if let Some(rest) = self.uri.strip_prefix("https://")
            && let Ok(addr) = rest.parse::<std::net::SocketAddr>()
        {
            return EndpointKind::Tcp(addr);
        }
        EndpointKind::Other(self.uri.clone())
    }
}

/// Observable state of an instance, *derived* from its lease and reported
/// state at read time (`cpt-cf-adr-directory-persistence-and-replication`).
/// The directory never writes this; it is computed by
/// [`GearInstance::observed_state_at`] from the
/// [`ReportedState`] the instance last declared and whether its lease is still
/// valid.
///
/// Only [`Healthy`](Self::Healthy) (and the deprecated [`Ready`](Self::Ready),
/// kept for wire compatibility and still read as serving) are **serving**.
/// [`Quarantined`](Self::Quarantined) now means *the lease expired* (missed
/// heartbeats), not a failing readiness check — a failing readiness check is
/// reported as `Starting` and reads as [`Registered`](Self::Registered). A
/// record whose lease expired more than `grace` ago is not observable at all
/// (see [`observed_state_at`](GearInstance::observed_state_at)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceState {
    Registered,
    /// Deprecated: no longer produced by the directory, kept in the enum for
    /// wire compatibility and still treated as serving when read.
    Ready,
    Healthy,
    Quarantined,
    Draining,
}

/// What an instance declares about itself in `RegisterInstance` / `Heartbeat`.
/// The directory derives the observable [`InstanceState`] from this
/// plus the lease; it is the only liveness signal an instance writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReportedState {
    /// Not yet serving (starting up, or failing its readiness check).
    #[default]
    Starting,
    /// Serving and heartbeating.
    Serving,
    /// Gracefully draining; upstreams should stop routing new work.
    Draining,
}

/// The directory's lease policy: how long a registration/heartbeat keeps an
/// instance observable (`ttl`), and the extra window it reads as
/// [`InstanceState::Quarantined`] before becoming unobservable (`grace`).
#[derive(Clone, Copy, Debug)]
struct LeasePolicy {
    ttl: Duration,
    grace: Duration,
}

/// Runtime liveness of an instance (guarded by `RwLock` for safe mutation).
///
/// Holds the instance's **lease**: `lease_expires_at` is set to `now + ttl` by
/// every registration and heartbeat, and the observable state is computed from
/// it at read time rather than written by a background task.
#[derive(Clone, Debug)]
struct InstanceRuntimeState {
    /// Instant the lease expires: last registration/heartbeat `+ ttl`.
    lease_expires_at: Instant,
    /// Window after `lease_expires_at` during which the record reads as
    /// [`InstanceState::Quarantined`] before becoming unobservable.
    grace: Duration,
    /// The state the instance last reported about itself.
    reported: ReportedState,
}

/// Represents a single instance of a gear
#[derive(Debug)]
#[must_use]
pub struct GearInstance {
    pub gear: String,
    pub instance_id: Uuid,
    pub control: Option<Endpoint>,
    pub grpc_services: HashMap<String, Endpoint>,
    pub version: Option<String>,
    pub rest_endpoint: Option<Endpoint>,
    pub openapi_spec: Option<String>,
    /// Stable addressing labels (k8s `matchLabels` style) advertised by this
    /// instance, used for label-based instance selection.
    pub labels: BTreeMap<String, String>,
    /// State the registrant declares about itself, or `None` when unspecified
    /// (`optional reported_state`). On a *new* registration `None`
    /// means [`ReportedState::Starting`]; on a *re-registration* `None` means
    /// "keep whatever the record already reports", so a periodic self-heal that
    /// omits the field never knocks a serving instance back to `Starting`.
    pub reported_state: Option<ReportedState>,
    inner: Arc<parking_lot::RwLock<InstanceRuntimeState>>,
}

impl Clone for GearInstance {
    fn clone(&self) -> Self {
        Self {
            gear: self.gear.clone(),
            instance_id: self.instance_id,
            control: self.control.clone(),
            grpc_services: self.grpc_services.clone(),
            version: self.version.clone(),
            rest_endpoint: self.rest_endpoint.clone(),
            openapi_spec: self.openapi_spec.clone(),
            labels: self.labels.clone(),
            reported_state: self.reported_state,
            inner: Arc::clone(&self.inner),
        }
    }
}

impl GearInstance {
    /// Build a new instance record using `other`'s metadata but preserving
    /// `self`'s `inner` runtime-state lock. This makes re-registration atomic:
    /// a concurrent `heartbeat` keeps writing the same state object instead of
    /// racing with a copied/swapped `InstanceRuntimeState`.
    ///
    /// One process registers an instance from two places: the gRPC hub with
    /// its services, the host with its REST endpoint and `OpenAPI` spec. Each
    /// writes only its own half, and they race, so an absent half keeps the
    /// stored one instead of clearing it, as labels do.
    fn with_metadata_of(&self, other: &GearInstance) -> GearInstance {
        GearInstance {
            gear: other.gear.clone(),
            instance_id: other.instance_id,
            control: other.control.clone(),
            grpc_services: if other.grpc_services.is_empty() {
                self.grpc_services.clone()
            } else {
                other.grpc_services.clone()
            },
            version: other.version.clone(),
            rest_endpoint: other
                .rest_endpoint
                .clone()
                .or_else(|| self.rest_endpoint.clone()),
            openapi_spec: other
                .openapi_spec
                .clone()
                .or_else(|| self.openapi_spec.clone()),
            // Empty incoming labels mean "keep the stored set", not "clear"; a
            // non-empty set replaces it. See `RegisterInstanceInfo::with_labels`
            // for why (self-heal / REST-augmentation re-registers omit labels).
            labels: if other.labels.is_empty() {
                self.labels.clone()
            } else {
                other.labels.clone()
            },
            // The declared state rides on the merged record so
            // `register_instance` can apply it (or preserve the stored one when
            // `None`) after rebuilding the instance.
            reported_state: other.reported_state,
            inner: Arc::clone(&self.inner),
        }
    }
}

impl GearInstance {
    pub fn new(gear: impl Into<String>, instance_id: Uuid) -> Self {
        Self {
            gear: gear.into(),
            instance_id,
            control: None,
            grpc_services: HashMap::new(),
            version: None,
            rest_endpoint: None,
            openapi_spec: None,
            labels: BTreeMap::new(),
            reported_state: None,
            // Placeholder lease: already expired, so an instance that is never
            // registered is never observable. `register_instance` stamps the
            // real lease (`now + ttl`) and grace from the manager policy.
            inner: Arc::new(parking_lot::RwLock::new(InstanceRuntimeState {
                lease_expires_at: Instant::now(),
                grace: Duration::ZERO,
                reported: ReportedState::Starting,
            })),
        }
    }

    pub fn with_control(mut self, ep: Endpoint) -> Self {
        self.control = Some(ep);
        self
    }

    pub fn with_version(mut self, v: impl Into<String>) -> Self {
        self.version = Some(v.into());
        self
    }

    pub fn with_grpc_service(mut self, name: impl Into<String>, ep: Endpoint) -> Self {
        self.grpc_services.insert(name.into(), ep);
        self
    }

    pub fn with_rest_endpoint(mut self, ep: Endpoint) -> Self {
        self.rest_endpoint = Some(ep);
        self
    }

    pub fn with_openapi_spec(mut self, spec: impl Into<String>) -> Self {
        self.openapi_spec = Some(spec.into());
        self
    }

    pub fn with_labels(mut self, labels: BTreeMap<String, String>) -> Self {
        self.labels = labels;
        self
    }

    /// Set the state the registrant declares about itself.
    pub fn with_reported_state(mut self, reported: ReportedState) -> Self {
        self.reported_state = Some(reported);
        self
    }

    /// The observable [`InstanceState`] derived from the lease and reported
    /// state at `now`, or `None` when the record has expired more than `grace`
    /// past its lease and is therefore no longer observable (the sweeper will
    /// remove it; reads hide it in the meantime).
    ///
    /// | Condition | Returns |
    /// |---|---|
    /// | lease valid, reported `Starting` | `Registered` |
    /// | lease valid, reported `Serving` | `Healthy` |
    /// | lease valid, reported `Draining` | `Draining` |
    /// | lease expired, less than `grace` ago | `Quarantined` |
    /// | lease expired `grace` ago or more | `None` |
    #[must_use]
    pub fn observed_state_at(&self, now: Instant) -> Option<InstanceState> {
        let s = self.inner.read();
        if now <= s.lease_expires_at {
            Some(match s.reported {
                ReportedState::Starting => InstanceState::Registered,
                ReportedState::Serving => InstanceState::Healthy,
                ReportedState::Draining => InstanceState::Draining,
            })
        } else if now <= s.lease_expires_at + s.grace {
            Some(InstanceState::Quarantined)
        } else {
            None
        }
    }

    /// The current observable state (evaluated against the wall clock now).
    ///
    /// A record past its grace window is no longer observable; callers that
    /// enumerate instances filter those out, so this collapses the
    /// unobservable case onto `Quarantined` for the rare direct caller.
    #[must_use]
    pub fn state(&self) -> InstanceState {
        self.observed_state_at(Instant::now())
            .unwrap_or(InstanceState::Quarantined)
    }
}

/// Central registry that tracks all running gear instances in the system.
/// Provides discovery, health tracking, and round-robin load balancing.
///
/// Always shared as an `Arc<GearManager>` and NOT `Clone`: a by-value clone
/// would deep-copy the `DashMap` directories while sharing the guards, so the
/// registration lock would no longer serialize the map it guards.
#[must_use]
pub struct GearManager {
    inner: DashMap<String, Vec<Arc<GearInstance>>>,
    rr_counters: DashMap<String, usize>,
    /// Lease policy (ttl + grace), behind a lock so the `service-discovery`
    /// gear can apply its configured `lease` settings onto the
    /// host-owned manager after it is constructed and shared.
    lease: parking_lot::RwLock<LeasePolicy>,
    /// Serializes gRPC-service-name ownership check + insert in
    /// [`register_instance`](Self::register_instance) so two
    /// gears cannot both pass an "unowned" check and then both commit the same
    /// name. `DashMap` locks per entry, giving no boundary across the
    /// cross-gear ownership scan and the single-entry write; this gate provides
    /// it.
    ///
    /// Contract: every operation that *creates* a name→gear binding must hold
    /// this lock — today `register_instance`, `set_grpc_service_owners`, and
    /// `merge_authoritative_grpc_service_owners`. `deregister` / `sweep_expired`
    /// only *free* names and `heartbeat` only renews a lease, so they cannot
    /// create a second owner and need not take it. Any future mutator that can
    /// bind a name must.
    reg_lock: parking_lot::Mutex<()>,
    /// Authoritative gRPC-service-name -> owning-gear map. When a name appears
    /// here, ownership is fixed to the named gear: only that gear may advertise
    /// it and no other gear can ever claim it, regardless of which registered
    /// first — the stable ownership source the dynamic registration scan is
    /// not. Populated by the runtime at startup (see
    /// [`set_grpc_service_owners`](Self::set_grpc_service_owners)); names absent
    /// from the map fall back to first-registration ownership.
    service_owners: parking_lot::RwLock<HashMap<String, String>>,
}

/// Returned by [`GearManager::register_instance`] when an instance
/// advertises a gRPC service name already owned by a *different* gear. The
/// store is left unchanged; the caller maps this onto a gRPC status (see
/// `DirectoryServiceNameConflict`), whose code depends on [`recoverable`].
///
/// [`recoverable`]: Self::recoverable
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcServiceNameConflict {
    /// The gRPC service name that is already owned.
    pub service_name: String,
    /// The gear that currently owns `service_name`.
    pub owner: String,
    /// Whether waiting could clear the conflict: `false` when the name is pinned
    /// to `owner` by the authoritative ownership map (permanent), `true` when
    /// `owner` merely currently advertises it (clears when it deregisters). Set
    /// at the two rejection sites in [`GearManager::register_instance`].
    pub recoverable: bool,
}

impl std::fmt::Display for GrpcServiceNameConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gRPC service name '{}' is already owned by gear '{}'",
            self.service_name, self.owner
        )
    }
}

impl std::error::Error for GrpcServiceNameConflict {}

impl std::fmt::Debug for GearManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let gears: Vec<String> = self.inner.iter().map(|e| e.key().clone()).collect();
        let lease = *self.lease.read();
        f.debug_struct("GearManager")
            .field("instances_count", &self.inner.len())
            .field("gears", &gears)
            .field("lease_ttl", &lease.ttl)
            .field("lease_grace", &lease.grace)
            .finish_non_exhaustive()
    }
}

impl GearManager {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
            rr_counters: DashMap::new(),
            // Lease defaults (`cpt-cf-adr-directory-persistence-and-replication`):
            // a crashed instance stops being preferred within `ttl` and becomes
            // unobservable within `ttl + grace`.
            lease: parking_lot::RwLock::new(LeasePolicy {
                ttl: Duration::from_secs(30),
                grace: Duration::from_secs(30),
            }),
            reg_lock: parking_lot::Mutex::new(()),
            service_owners: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    /// Set the lease policy, builder-style. Equivalent to
    /// [`set_lease_policy`](Self::set_lease_policy) before the manager is shared.
    pub fn with_lease_policy(self, ttl: Duration, grace: Duration) -> Self {
        self.set_lease_policy(ttl, grace);
        self
    }

    /// Set the lease policy at runtime. Used by the
    /// `service-discovery` gear to apply its configured `lease` settings onto
    /// the host-owned manager after construction. Only affects leases stamped
    /// by registrations/heartbeats after this call; already-stamped records
    /// keep the grace they were given.
    pub fn set_lease_policy(&self, ttl: Duration, grace: Duration) {
        *self.lease.write() = LeasePolicy { ttl, grace };
    }

    /// The lease TTL this directory grants, so `RegisterInstance` can return
    /// `lease_ttl_ms` and a worker can heartbeat at `ttl / 3`.
    #[must_use]
    pub fn lease_ttl(&self) -> Duration {
        self.lease.read().ttl
    }

    /// Install the authoritative gRPC-service-name -> owning-gear map.
    ///
    /// A listed name may be advertised only by its owner (enforced in
    /// [`register_instance`](Self::register_instance)), closing the hole where a
    /// gear could squat another's service name by registering first; unlisted
    /// names keep first-registration ownership. Seeded from operator config, then
    /// layered with the compiled-in owners via
    /// [`merge_authoritative_grpc_service_owners`](Self::merge_authoritative_grpc_service_owners) —
    /// as each group's installers become available. Configure application/`OoP` names
    /// explicitly to protect them before initialization and registration.
    ///
    /// Taken under `reg_lock` so it can't interleave with an in-flight
    /// [`register_instance`](Self::register_instance) check + commit.
    pub fn set_grpc_service_owners(&self, owners: HashMap<String, String>) {
        let _gate = self.reg_lock.lock();
        *self.service_owners.write() = owners;
    }

    /// Merge a compiled-in `service_name -> gear` map into the ownership map,
    /// overriding any conflicting operator-configured entry.
    ///
    /// The compiled binary is ground truth — a gear physically *provides* the
    /// service — so config must not reassign that name elsewhere (that would
    /// re-open the squat); a disagreeing entry is overridden and warned. Names
    /// absent here (e.g. remote / out-of-process gears) keep their configured
    /// owner.
    ///
    /// Runs before each group's registration, under `reg_lock`.
    pub fn merge_authoritative_grpc_service_owners(&self, authoritative: HashMap<String, String>) {
        let _gate = self.reg_lock.lock();
        let mut owners = self.service_owners.write();
        for (service_name, gear) in authoritative {
            if let Some(configured) = owners.get(&service_name)
                && configured != &gear
            {
                tracing::warn!(
                    service = %service_name,
                    configured_owner = %configured,
                    compiled_owner = %gear,
                    "grpc-service-owners: operator config assigned a compiled-in service name to a \
                     different gear; the compiled-in provider is authoritative and overrides it"
                );
            }
            owners.insert(service_name, gear);
        }
    }

    /// Register or update a gear instance, enforcing single-gear ownership of
    /// every gRPC service name it advertises — atomically.
    ///
    /// The ownership check ([`check_grpc_service_ownership`](Self::check_grpc_service_ownership))
    /// and the insert run under one registration lock, so two gears cannot both
    /// pass an "unowned" check and then both commit the same name — a
    /// check-then-write race the per-entry `DashMap` locks do not prevent. On a
    /// conflict the store is left untouched.
    ///
    /// Registration **renews the lease**: `lease_expires_at` is set
    /// to `now + ttl`, including on re-registration of an existing record.
    ///
    /// Re-registering an existing `instance_id` is otherwise an idempotent
    /// endpoint / metadata refresh that preserves the same
    /// `Arc<RwLock<InstanceRuntimeState>>`, so a concurrent `heartbeat` keeps
    /// writing the same lease. The declared [`ReportedState`]
    /// ([`GearInstance::reported_state`]) is applied when present; when `None`
    /// (a self-heal re-register that omits the field) the record *keeps* the
    /// state it already reports, so a serving instance is never knocked back to
    /// `Starting`. On a brand-new record `None` defaults to
    /// [`ReportedState::Starting`].
    ///
    /// # Errors
    /// Returns [`GrpcServiceNameConflict`] if an advertised gRPC service name is
    /// owned by, or currently advertised by, another gear.
    pub fn register_instance(
        &self,
        instance: Arc<GearInstance>,
    ) -> Result<(), GrpcServiceNameConflict> {
        let _gate = self.reg_lock.lock();
        self.check_grpc_service_ownership(&instance)?;

        let now = Instant::now();
        let declared = instance.reported_state;
        let gear = instance.gear.clone();
        let mut vec = self.inner.entry(gear).or_default();
        // replace by instance_id if it already exists
        let record = if let Some(pos) = vec
            .iter()
            .position(|i| i.instance_id == instance.instance_id)
        {
            // A record whose lease has lapsed past its grace window is
            // unobservable and may already have had its gRPC service names
            // reclaimed by another gear (`check_grpc_service_ownership` skips
            // dead records). Merging would restore those names from the stored
            // half — names the ownership check above never saw, as it only
            // validates `instance`'s own services — and revive the dead lease,
            // leaving two gears advertising one name. Treat it as a fresh
            // registration so only the just-validated half survives.
            if vec[pos].observed_state_at(now).is_none() {
                vec[pos] = Arc::clone(&instance);
                instance
            } else {
                let merged = Arc::new(vec[pos].with_metadata_of(&instance));
                vec[pos] = Arc::clone(&merged);
                merged
            }
        } else {
            vec.push(Arc::clone(&instance));
            instance
        };

        // Renew the lease and apply the declared state. On a re-register that
        // omits the state, keep the stored one (see the doc above).
        let policy = *self.lease.read();
        let mut s = record.inner.write();
        s.lease_expires_at = now + policy.ttl;
        s.grace = policy.grace;
        if let Some(reported) = declared {
            s.reported = reported;
        }
        Ok(())
    }

    /// Authorize every gRPC service name `instance` advertises, resolving
    /// ownership in two tiers:
    ///
    /// 1. **Declared (authoritative).** If a name appears in the configured
    ///    ownership map (see
    ///    [`set_grpc_service_owners`](Self::set_grpc_service_owners)), only the
    ///    declared owner may advertise it — any other gear is rejected even if
    ///    it registers first. This stops a gear from squatting a name it was
    ///    never assigned.
    /// 2. **First-registration (fallback).** For names absent from that map, the
    ///    first gear to register the name owns it.
    ///
    /// Authorization is necessary but not sufficient: a name currently
    /// advertised by a *different* gear always conflicts, so even the declared
    /// owner is refused while a stale advertiser still holds it (e.g. one that
    /// registered under a previous ownership map before
    /// [`set_grpc_service_owners`](Self::set_grpc_service_owners) reassigned the
    /// name). This keeps ownership from splitting across two advertisers; the
    /// declared owner succeeds once the stale holder deregisters or expires. A
    /// name held by the *same* gear is always fine (re-register / additional
    /// instance).
    ///
    /// Must be called with `reg_lock` held so the check stays atomic with the
    /// caller's subsequent insert.
    fn check_grpc_service_ownership(
        &self,
        instance: &GearInstance,
    ) -> Result<(), GrpcServiceNameConflict> {
        // Tier 1: reject a declared name claimed by a non-owner; the rest (the
        // declared owner, and undeclared names) still need the tier-2 scan.
        let declared = self.service_owners.read();
        let mut to_scan: Vec<&str> = Vec::new();
        for service_name in instance.grpc_services.keys() {
            match declared.get(service_name) {
                Some(owner) if *owner != instance.gear => {
                    // Tier 1: the name is pinned to another gear by the
                    // authoritative map — permanent, waiting cannot clear it.
                    return Err(GrpcServiceNameConflict {
                        service_name: service_name.clone(),
                        owner: owner.clone(),
                        recoverable: false,
                    });
                }
                _ => to_scan.push(service_name),
            }
        }
        drop(declared);
        if to_scan.is_empty() {
            return Ok(());
        }

        // Tier 2: one pass over the directory, checking every candidate name per
        // instance visited rather than re-scanning once per name. A name already
        // held by a *different* gear conflicts. An instance whose lease expired
        // past its grace window is unobservable and no longer holds
        // its names, so it is skipped — a successor can claim the name even
        // before the sweeper removes the dead record.
        let now = Instant::now();
        for entry in &self.inner {
            if entry.key() == &instance.gear {
                continue; // same gear: re-register / additional instance is fine
            }
            for existing in entry.value() {
                if existing.observed_state_at(now).is_none() {
                    continue; // dead record: releases its gRPC names
                }
                if let Some(&name) = to_scan
                    .iter()
                    .find(|&&n| existing.grpc_services.contains_key(n))
                {
                    // Tier 2: a *different* gear currently advertises the name —
                    // recoverable, it clears when that holder deregisters/expires.
                    return Err(GrpcServiceNameConflict {
                        service_name: name.to_owned(),
                        owner: entry.key().clone(),
                        recoverable: true,
                    });
                }
            }
        }
        Ok(())
    }

    /// Renew an instance's lease and apply the state it reports.
    ///
    /// Called for every heartbeat. `reported` is `None` when the heartbeat
    /// omits a state, which counts as [`ReportedState::Serving`] (so an old
    /// worker that only ever heartbeats reads as `Healthy`). Returns `false`
    /// when no such `(gear, instance_id)` is registered, letting the caller
    /// answer `NOT_FOUND`.
    ///
    /// A record whose lease expired past its grace window is treated as absent:
    /// it is no longer observable, and a successor gear may already have claimed
    /// the gRPC service names it used to hold (ownership scans skip it, see
    /// [`check_grpc_service_ownership`](Self::check_grpc_service_ownership)).
    /// Renewing it here would revive a second advertiser of the same name, so a
    /// heartbeat for such a record returns `false`. The caller re-registers,
    /// which runs the ownership check in
    /// [`register_instance`](Self::register_instance) and rejects the revival if
    /// the name moved on.
    #[must_use]
    pub fn heartbeat(
        &self,
        gear: &str,
        instance_id: Uuid,
        reported: Option<ReportedState>,
    ) -> bool {
        if let Some(vec) = self.inner.get(gear)
            && let Some(inst) = vec.iter().find(|i| i.instance_id == instance_id)
        {
            let policy = *self.lease.read();
            // Read the expiry and renew under the *same* write lock, with a
            // timestamp taken after acquiring it: the "is it still observable"
            // decision and the renewing write must be one critical section.
            // Checking against a `now` captured before the lock would let the
            // grace boundary pass in between, reviving a record a concurrent
            // `register_instance` may already have handed its gRPC name to a
            // successor. Past the grace window the record is unobservable, so
            // return `false` and force the caller onto the re-registration path
            // (which re-checks ownership).
            let mut s = inst.inner.write();
            let now = Instant::now();
            if now > s.lease_expires_at + s.grace {
                return false;
            }
            s.lease_expires_at = now + policy.ttl;
            s.grace = policy.grace;
            s.reported = reported.unwrap_or(ReportedState::Serving);
            return true;
        }
        false
    }

    /// Remove an instance from the directory
    pub fn deregister(&self, gear: &str, instance_id: Uuid) {
        if let Some(mut vec) = self.inner.get_mut(gear) {
            vec.value_mut()
                .retain(|inst| inst.instance_id != instance_id);
        }

        // Drop the gear entry only if it is *still* empty: a concurrent
        // `register_instance` can insert a fresh instance between the retain
        // above and this removal, and an unconditional `remove` would delete it.
        // `remove_if` re-checks under the entry lock (same pattern as
        // `sweep_expired`).
        if self
            .inner
            .remove_if(gear, |_, vec| vec.is_empty())
            .is_some()
        {
            self.rr_counters.remove(gear);
            self.rr_counters.remove(&format!("rest:{gear}"));
        }
    }

    /// Get all currently-observable instances of a specific gear.
    ///
    /// A record whose lease expired past its grace window is filtered out: it
    /// is no longer observable, so callers never see it even before
    /// [`sweep_expired`](Self::sweep_expired) removes it.
    #[must_use]
    pub fn instances_of(&self, gear: &str) -> Vec<Arc<GearInstance>> {
        let now = Instant::now();
        self.inner
            .get(gear)
            .map(|v| {
                v.iter()
                    .filter(|i| i.observed_state_at(now).is_some())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all currently-observable instances across all gears.
    #[must_use]
    pub fn all_instances(&self) -> Vec<Arc<GearInstance>> {
        let now = Instant::now();
        self.inner
            .iter()
            .flat_map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|i| i.observed_state_at(now).is_some())
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Remove records whose lease expired more than `grace` ago.
    ///
    /// This is the `memory`-mode sweeper: a record is no longer observable once
    /// its lease lapses past the grace window, so this only garbage-collects —
    /// reads already hide such records. Removing a gear's last instance also
    /// frees its round-robin counters and the gRPC service names it held.
    pub fn sweep_expired(&self, now: Instant) {
        let mut empty_gears = Vec::new();

        for mut entry in self.inner.iter_mut() {
            let gear = entry.key().clone();
            let vec = entry.value_mut();
            vec.retain(|inst| inst.observed_state_at(now).is_some());
            if vec.is_empty() {
                empty_gears.push(gear);
            }
        }

        // Remove each emptied gear only if it is *still* empty: a
        // `register_instance` can insert a fresh instance between the retain
        // above and this removal, and an unconditional `remove` would delete it.
        // `remove_if` re-checks under the entry lock, atomically with any
        // concurrent registration on the same entry.
        for gear in empty_gears {
            if self
                .inner
                .remove_if(&gear, |_, vec| vec.is_empty())
                .is_some()
            {
                self.rr_counters.remove(&gear);
                self.rr_counters.remove(&format!("rest:{gear}"));
            }
        }
    }

    /// From a candidate set, return the serving subset (`Healthy`/`Ready`); if
    /// none are serving, fall back to the not-yet-serving instances
    /// (`Registered`/`Quarantined`) rather than failing closed. A `Draining`
    /// instance is never a routing target, even as a fallback — draining means
    /// "stop routing new work" — so a gear whose only instances are draining
    /// resolves to nothing. `context` names the pool for the cold-path `debug!`.
    fn prefer_serving(candidates: Vec<Arc<GearInstance>>, context: &str) -> Vec<Arc<GearInstance>> {
        let now = Instant::now();
        // Keep only routable instances: a fully-lapsed lease is unobservable and
        // a `Draining` instance is shutting down — neither may be routed to, even
        // as a fallback.
        let routable: Vec<Arc<GearInstance>> = candidates
            .into_iter()
            .filter(|inst| {
                !matches!(
                    inst.observed_state_at(now),
                    None | Some(InstanceState::Draining)
                )
            })
            .collect();
        let serving: Vec<Arc<GearInstance>> = routable
            .iter()
            .filter(|inst| {
                matches!(
                    inst.observed_state_at(now),
                    Some(InstanceState::Healthy | InstanceState::Ready)
                )
            })
            .cloned()
            .collect();
        if serving.is_empty() {
            if !routable.is_empty() {
                tracing::debug!(
                    context,
                    "no serving (Ready/Healthy) instance available; round-robining over the \
                     not-serving set instead of returning None"
                );
            }
            routable
        } else {
            serving
        }
    }

    /// Pick an instance using round-robin selection, preferring serving instances
    #[must_use]
    pub fn pick_instance_round_robin(&self, gear: &str) -> Option<Arc<GearInstance>> {
        let instances_entry = self.inner.get(gear)?;
        let instances = instances_entry.value();

        let candidates = Self::prefer_serving(instances.clone(), gear);
        if candidates.is_empty() {
            return None;
        }

        let len = candidates.len();
        let mut counter = self.rr_counters.entry(gear.to_owned()).or_insert(0);
        let idx = *counter % len;
        *counter = (*counter + 1) % len;

        candidates.get(idx).cloned()
    }

    /// Pick a service endpoint using round-robin, returning (gear, instance, endpoint).
    /// Prefers healthy/ready instances and automatically rotates among them.
    #[must_use]
    pub fn pick_service_round_robin(
        &self,
        service_name: &str,
    ) -> Option<(String, Arc<GearInstance>, Endpoint)> {
        // Collect the `Arc` of every instance that provides this service, then
        // apply the shared serving-preference fallback so gRPC resolution
        // survives an all-not-ready gear instead of failing closed. Cloning an
        // `Arc` is a refcount bump; the gear name and endpoint are cloned once,
        // for the winner, after the index is chosen.
        let mut providing: Vec<Arc<GearInstance>> = Vec::new();
        for entry in &self.inner {
            for inst in entry.value() {
                if inst.grpc_services.contains_key(service_name) {
                    providing.push(inst.clone());
                }
            }
        }

        if providing.is_empty() {
            return None;
        }

        let mut candidates = Self::prefer_serving(providing, service_name);
        if candidates.is_empty() {
            return None;
        }

        // Use a counter keyed by service name for round-robin
        let len = candidates.len();
        let service_key = service_name.to_owned();
        let mut counter = self.rr_counters.entry(service_key).or_insert(0);
        let idx = *counter % len;
        *counter = (*counter + 1) % len;

        let inst = candidates.swap_remove(idx);
        let endpoint = inst.grpc_services.get(service_name)?.clone();
        let gear = inst.gear.clone();
        Some((gear, inst, endpoint))
    }

    /// Resolve a REST endpoint for a gear using round-robin over instances that
    /// expose one, preferring healthy/ready instances.
    #[must_use]
    pub fn pick_rest_endpoint_round_robin(&self, gear: &str) -> Option<Endpoint> {
        let instances_entry = self.inner.get(gear)?;
        let instances = instances_entry.value();

        // Only instances that actually expose a REST endpoint are candidates.
        let with_rest: Vec<_> = instances
            .iter()
            .filter(|inst| inst.rest_endpoint.is_some())
            .cloned()
            .collect();

        if with_rest.is_empty() {
            return None;
        }

        let candidates = Self::prefer_serving(with_rest, gear);
        if candidates.is_empty() {
            return None;
        }

        let len = candidates.len();
        let rr_key = format!("rest:{gear}");
        let mut counter = self.rr_counters.entry(rr_key).or_insert(0);
        let idx = *counter % len;
        *counter = (*counter + 1) % len;

        candidates
            .get(idx)
            .and_then(|inst| inst.rest_endpoint.clone())
    }

    /// Return a gear that currently advertises the gRPC service `service_name`,
    /// if any (any instance state counts); in the misconfigured multi-owner case
    /// the first match in iteration order.
    ///
    /// Test-only: [`register_instance`](Self::register_instance) enforces single
    /// ownership with its own inline scan, so this is not part of the public
    /// surface — it exists purely to let the tests below assert who owns a name.
    #[cfg(test)]
    #[must_use]
    fn grpc_service_owner(&self, service_name: &str) -> Option<String> {
        let now = Instant::now();
        for entry in &self.inner {
            if entry.value().iter().any(|inst| {
                inst.observed_state_at(now).is_some()
                    && inst.grpc_services.contains_key(service_name)
            }) {
                return Some(entry.key().clone());
            }
        }
        None
    }

    /// Retrieve the `OpenAPI` spec of a gear, taken from the first registered
    /// instance that published one.
    #[must_use]
    pub fn openapi_spec_of(&self, gear: &str) -> Option<String> {
        let instances_entry = self.inner.get(gear)?;
        instances_entry
            .value()
            .iter()
            .find_map(|inst| inst.openapi_spec.clone())
    }
}

impl Default for GearManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_register_and_retrieve_instances() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();
        let instance = Arc::new(
            GearInstance::new("test_gear", instance_id)
                .with_control(Endpoint::http("localhost", 8080))
                .with_version("1.0.0"),
        );

        seed(&dir, instance);

        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].instance_id, instance_id);
        assert_eq!(instances[0].gear, "test_gear");
        assert_eq!(instances[0].version, Some("1.0.0".to_owned()));
    }

    /// Seed the registry in a test. The fixture is assumed to advertise no
    /// conflicting gRPC service name, so a conflict is a bug in the test, not
    /// an expected outcome — surface it loudly rather than swallowing it.
    #[track_caller]
    fn seed(mgr: &GearManager, instance: Arc<GearInstance>) {
        mgr.register_instance(instance)
            .expect("test fixture must not create a gRPC service-name conflict");
    }

    /// Force a seeded instance's lease to have expired `ago` before now, with
    /// the given grace window. Registration stamps a fresh lease, so this is
    /// called *after* seeding to simulate a worker that stopped heartbeating.
    #[track_caller]
    fn expire_lease(inst: &GearInstance, ago: Duration, grace: Duration) {
        let mut s = inst.inner.write();
        s.lease_expires_at = Instant::now()
            .checked_sub(ago)
            .expect("test lease subtraction should not underflow");
        s.grace = grace;
    }

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn reregister_without_labels_preserves_stored_labels() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();

        // Initial registration carries a shard label.
        seed(
            &dir,
            Arc::new(
                GearInstance::new("shard-gear", instance_id).with_labels(labels(&[("shard", "7")])),
            ),
        );

        // A label-less re-registration (e.g. periodic self-heal / REST
        // augmentation) must NOT wipe the stored labels.
        seed(
            &dir,
            Arc::new(GearInstance::new("shard-gear", instance_id).with_version("2.0.0")),
        );

        let registered = dir.instances_of("shard-gear");
        assert_eq!(registered.len(), 1);
        assert_eq!(
            registered[0].labels.get("shard"),
            Some(&"7".to_owned()),
            "label-less re-registration must preserve stored labels"
        );
        assert_eq!(registered[0].version, Some("2.0.0".to_owned()));
    }

    /// The hub's gRPC record and the host's REST record of one instance, in
    /// either order: each keeps the other's half.
    #[test]
    fn grpc_and_rest_halves_of_an_instance_survive_each_other() {
        let grpc = |id| {
            GearInstance::new("both", id)
                .with_grpc_service("pkg.Svc", Endpoint::from_uri("http://127.0.0.1:50051"))
        };
        let rest = |id| {
            GearInstance::new("both", id)
                .with_rest_endpoint(Endpoint::from_uri("http://127.0.0.1:8080"))
                .with_openapi_spec("{}")
        };
        for grpc_first in [true, false] {
            let dir = GearManager::new();
            let id = Uuid::new_v4();
            let (first, second) = if grpc_first {
                (grpc(id), rest(id))
            } else {
                (rest(id), grpc(id))
            };
            seed(&dir, Arc::new(first));
            seed(&dir, Arc::new(second));

            let registered = dir.instances_of("both");
            assert_eq!(registered.len(), 1);
            assert!(
                registered[0].grpc_services.contains_key("pkg.Svc"),
                "gRPC kept (grpc_first={grpc_first})"
            );
            assert_eq!(
                registered[0].rest_endpoint.as_ref().map(|e| e.uri.as_str()),
                Some("http://127.0.0.1:8080"),
                "REST kept (grpc_first={grpc_first})"
            );
            assert_eq!(registered[0].openapi_spec.as_deref(), Some("{}"));
        }
    }

    /// A present half still replaces the stored one.
    #[test]
    fn a_present_half_replaces_the_stored_one() {
        let dir = GearManager::new();
        let id = Uuid::new_v4();
        seed(
            &dir,
            Arc::new(
                GearInstance::new("moved", id)
                    .with_rest_endpoint(Endpoint::from_uri("http://127.0.0.1:8080")),
            ),
        );
        seed(
            &dir,
            Arc::new(
                GearInstance::new("moved", id)
                    .with_rest_endpoint(Endpoint::from_uri("http://127.0.0.1:9090")),
            ),
        );
        assert_eq!(
            dir.instances_of("moved")[0]
                .rest_endpoint
                .as_ref()
                .map(|e| e.uri.as_str()),
            Some("http://127.0.0.1:9090")
        );
    }

    #[test]
    fn reregister_with_labels_replaces_stored_labels() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();

        seed(
            &dir,
            Arc::new(
                GearInstance::new("shard-gear", instance_id).with_labels(labels(&[("shard", "7")])),
            ),
        );
        // An explicit non-empty set replaces the stored one wholesale.
        seed(
            &dir,
            Arc::new(
                GearInstance::new("shard-gear", instance_id).with_labels(labels(&[("shard", "8")])),
            ),
        );

        let registered = dir.instances_of("shard-gear");
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].labels.get("shard"), Some(&"8".to_owned()));
    }

    #[test]
    fn test_register_multiple_instances() {
        let dir = GearManager::new();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let instance1 = Arc::new(GearInstance::new("test_gear", id1));
        let instance2 = Arc::new(GearInstance::new("test_gear", id2));

        seed(&dir, instance1);
        seed(&dir, instance2);

        let registered = dir.instances_of("test_gear");
        assert_eq!(registered.len(), 2);

        let ids: Vec<_> = registered.iter().map(|i| i.instance_id).collect();
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[test]
    fn test_update_existing_instance() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();

        let initial_instance =
            Arc::new(GearInstance::new("test_gear", instance_id).with_version("1.0.0"));
        seed(&dir, initial_instance);

        let updated_instance =
            Arc::new(GearInstance::new("test_gear", instance_id).with_version("2.0.0"));
        seed(&dir, updated_instance);

        let registered = dir.instances_of("test_gear");
        assert_eq!(registered.len(), 1, "Should not duplicate instance");
        assert_eq!(registered[0].version, Some("2.0.0".to_owned()));
    }

    #[test]
    fn test_reregistration_preserves_liveness_state() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();

        // Register, then heartbeat so the instance is Healthy (routable).
        seed(
            &dir,
            Arc::new(GearInstance::new("test_gear", instance_id).with_version("1.0.0")),
        );
        assert!(dir.heartbeat("test_gear", instance_id, None));
        assert!(matches!(
            dir.instances_of("test_gear")[0].state(),
            InstanceState::Healthy
        ));

        // A self-heal re-registration that omits the reported state must NOT
        // reset liveness back to Registered — otherwise the instance drops out
        // of gRPC round-robin until the next heartbeat (the "split-brain" flap).
        seed(
            &dir,
            Arc::new(GearInstance::new("test_gear", instance_id).with_version("2.0.0")),
        );

        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert!(
            matches!(instances[0].state(), InstanceState::Healthy),
            "re-registration must preserve the Healthy state"
        );
        assert_eq!(
            instances[0].version,
            Some("2.0.0".to_owned()),
            "re-registration must still refresh metadata/endpoints"
        );
    }

    #[test]
    fn test_concurrent_reregister_and_heartbeat_preserves_state() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();

        let initial = Arc::new(GearInstance::new("test_gear", instance_id).with_version("1.0.0"));
        seed(&dir, initial);
        assert!(dir.heartbeat("test_gear", instance_id, None));
        assert!(matches!(
            dir.instances_of("test_gear")[0].state(),
            InstanceState::Healthy
        ));

        std::thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..1000 {
                    // The record always exists here, so each heartbeat renews it.
                    assert!(dir.heartbeat("test_gear", instance_id, None));
                }
            });
            s.spawn(|| {
                for i in 0..1000 {
                    let version = if i % 2 == 0 { "2.0.0" } else { "3.0.0" };
                    let reinst = Arc::new(
                        GearInstance::new("test_gear", instance_id)
                            .with_version(version)
                            .with_rest_endpoint(Endpoint::http(
                                "127.0.0.1",
                                8000u16 + u16::try_from(i % 10).expect("i % 10 fits in u16"),
                            )),
                    );
                    seed(&dir, reinst);
                }
            });
        });

        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert!(
            matches!(instances[0].state(), InstanceState::Healthy),
            "concurrent re-registration must not reset Healthy state"
        );
    }

    #[test]
    fn register_with_reported_serving_reads_healthy() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();
        // A registrant that already reports Serving is immediately Healthy,
        // without waiting for a heartbeat.
        seed(
            &dir,
            Arc::new(
                GearInstance::new("test_gear", instance_id)
                    .with_reported_state(ReportedState::Serving),
            ),
        );

        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert!(matches!(instances[0].state(), InstanceState::Healthy));
    }

    #[test]
    fn heartbeat_renews_lease_and_reads_serving() {
        let dir = GearManager::new();
        let instance_id = Uuid::new_v4();
        seed(&dir, Arc::new(GearInstance::new("test_gear", instance_id)));

        // No heartbeat yet: reported Starting reads as Registered.
        assert!(matches!(
            dir.instances_of("test_gear")[0].state(),
            InstanceState::Registered
        ));

        // A heartbeat with no reported state counts as Serving -> Healthy.
        assert!(dir.heartbeat("test_gear", instance_id, None));
        assert!(matches!(
            dir.instances_of("test_gear")[0].state(),
            InstanceState::Healthy
        ));

        // A heartbeat for an unknown instance returns false (NOT_FOUND).
        assert!(!dir.heartbeat("test_gear", Uuid::new_v4(), None));
    }

    #[test]
    fn heartbeat_on_a_lapsed_lease_returns_not_found() {
        let grace = Duration::from_millis(50);
        let dir = GearManager::new().with_lease_policy(Duration::from_millis(50), grace);

        let instance = Arc::new(GearInstance::new("test_gear", Uuid::new_v4()));
        seed(&dir, Arc::clone(&instance));

        // Within grace the record is still observable, so a heartbeat renews it.
        expire_lease(&instance, Duration::from_millis(10), grace);
        assert!(dir.heartbeat("test_gear", instance.instance_id, None));

        // Past grace the record is unobservable: a heartbeat must return false
        // (NOT_FOUND) instead of reviving it, so the caller re-registers and
        // ownership is re-checked.
        expire_lease(&instance, grace + Duration::from_millis(10), grace);
        assert!(
            !dir.heartbeat("test_gear", instance.instance_id, None),
            "a heartbeat for a record whose lease lapsed past grace must not renew it"
        );
    }

    #[test]
    fn a_lapsed_owner_cannot_reclaim_its_grpc_name_by_heartbeat() {
        // gear X owns a gRPC service name, then its lease lapses past grace.
        let grace = Duration::from_millis(50);
        let dir = GearManager::new().with_lease_policy(Duration::from_millis(50), grace);
        let service = "pkg.Svc";

        let x = Arc::new(
            GearInstance::new("x", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
        );
        seed(&dir, Arc::clone(&x));
        expire_lease(&x, grace + Duration::from_millis(10), grace);

        // A successor gear claims the now-free name.
        let y = Arc::new(
            GearInstance::new("y", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
        );
        dir.register_instance(y)
            .expect("the lapsed owner's gRPC name must be claimable by a successor");

        // X's heartbeat must NOT revive it: that would leave two gears
        // advertising the same gRPC name.
        assert!(
            !dir.heartbeat("x", x.instance_id, None),
            "a lapsed owner must not reclaim its name via a bare heartbeat"
        );
        assert_eq!(
            dir.grpc_service_owner(service).as_deref(),
            Some("y"),
            "the successor keeps sole ownership of the name"
        );
    }

    #[test]
    fn a_lapsed_owner_rest_reregister_does_not_restore_its_grpc_name() {
        // gear X owns a gRPC name, then its lease lapses past grace.
        let grace = Duration::from_millis(50);
        let dir = GearManager::new().with_lease_policy(Duration::from_millis(50), grace);
        let service = "pkg.Svc";

        let x_id = Uuid::new_v4();
        let x = Arc::new(
            GearInstance::new("x", x_id)
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
        );
        seed(&dir, Arc::clone(&x));
        expire_lease(&x, grace + Duration::from_millis(10), grace);

        // A successor gear claims the now-free name.
        let y = Arc::new(
            GearInstance::new("y", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
        );
        dir.register_instance(y)
            .expect("the lapsed owner's gRPC name must be claimable by a successor");

        // X re-registers a REST-only half (empty gRPC map), which passes the
        // ownership check. The merge must NOT restore X's old gRPC name from
        // the lapsed record — that would leave two gears advertising one name.
        dir.register_instance(Arc::new(
            GearInstance::new("x", x_id)
                .with_rest_endpoint(Endpoint::from_uri("http://127.0.0.1:8080")),
        ))
        .expect("a REST-only re-registration carries no conflicting gRPC name");

        let registered = dir.instances_of("x");
        assert_eq!(registered.len(), 1);
        assert!(
            !registered[0].grpc_services.contains_key(service),
            "a lapsed owner's REST-only re-register must not restore its gRPC name"
        );
        assert_eq!(
            dir.grpc_service_owner(service).as_deref(),
            Some("y"),
            "the successor keeps sole ownership of the name"
        );
    }

    #[test]
    fn test_all_instances() {
        let dir = GearManager::new();

        let instance1 = Arc::new(GearInstance::new("gear_a", Uuid::new_v4()));
        let instance2 = Arc::new(GearInstance::new("gear_b", Uuid::new_v4()));
        let instance3 = Arc::new(GearInstance::new("gear_a", Uuid::new_v4()));

        seed(&dir, instance1);
        seed(&dir, instance2);
        seed(&dir, instance3);

        let all = dir.all_instances();
        assert_eq!(all.len(), 3);

        let gears: Vec<_> = all.iter().map(|i| i.gear.as_str()).collect();
        assert_eq!(gears.iter().filter(|&m| *m == "gear_a").count(), 2);
        assert_eq!(gears.iter().filter(|&m| *m == "gear_b").count(), 1);
    }

    #[test]
    fn test_pick_instance_round_robin() {
        let dir = GearManager::new();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let instance1 = Arc::new(GearInstance::new("test_gear", id1));
        let instance2 = Arc::new(GearInstance::new("test_gear", id2));

        seed(&dir, instance1);
        seed(&dir, instance2);

        // Pick three times to verify round-robin behavior
        let picked1 = dir.pick_instance_round_robin("test_gear").unwrap();
        let picked2 = dir.pick_instance_round_robin("test_gear").unwrap();
        let picked3 = dir.pick_instance_round_robin("test_gear").unwrap();

        let ids = [
            picked1.instance_id,
            picked2.instance_id,
            picked3.instance_id,
        ];

        // With 2 instances, we expect round-robin pattern like A, B, A
        // Check that both instance IDs appear and that at least one repeats
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
        // First and third pick should be the same (round-robin wraps)
        assert_eq!(picked1.instance_id, picked3.instance_id);
        // Second pick should be different from the first
        assert_ne!(picked1.instance_id, picked2.instance_id);
    }

    #[test]
    fn test_pick_instance_none_available() {
        let dir = GearManager::new();
        let picked = dir.pick_instance_round_robin("nonexistent_gear");
        assert!(picked.is_none());
    }

    #[test]
    fn test_endpoint_creation() {
        let plain_ep = Endpoint::http("localhost", 8080);
        assert_eq!(plain_ep.uri, "http://localhost:8080");

        let secure_ep = Endpoint::https("localhost", 8443);
        assert_eq!(secure_ep.uri, "https://localhost:8443");

        let uds_ep = Endpoint::uds("/tmp/socket.sock");
        assert!(uds_ep.uri.starts_with("unix://"));
        assert!(uds_ep.uri.contains("socket.sock"));

        let custom_ep = Endpoint::from_uri("http://example.com");
        assert_eq!(custom_ep.uri, "http://example.com");
    }

    #[test]
    fn test_endpoint_kind() {
        let plain_ep = Endpoint::http("127.0.0.1", 8080);
        match plain_ep.kind() {
            EndpointKind::Tcp(addr) => {
                assert_eq!(addr.ip().to_string(), "127.0.0.1");
                assert_eq!(addr.port(), 8080);
            }
            _ => panic!("Expected TCP endpoint for http"),
        }

        let secure_ep = Endpoint::https("127.0.0.1", 8443);
        match secure_ep.kind() {
            EndpointKind::Tcp(addr) => {
                assert_eq!(addr.ip().to_string(), "127.0.0.1");
                assert_eq!(addr.port(), 8443);
            }
            _ => panic!("Expected TCP endpoint for https"),
        }

        let uds_ep = Endpoint::uds("/tmp/test.sock");
        match uds_ep.kind() {
            EndpointKind::Uds(path) => {
                assert!(path.to_string_lossy().contains("test.sock"));
            }
            _ => panic!("Expected UDS endpoint"),
        }

        let other_ep = Endpoint::from_uri("grpc://example.com");
        match other_ep.kind() {
            EndpointKind::Other(uri) => {
                assert_eq!(uri, "grpc://example.com");
            }
            _ => panic!("Expected Other endpoint"),
        }
    }

    #[test]
    fn test_gear_instance_builder() {
        let instance_id = Uuid::new_v4();
        let instance = GearInstance::new("test_gear", instance_id)
            .with_control(Endpoint::http("localhost", 8080))
            .with_version("1.2.3")
            .with_grpc_service("service1", Endpoint::http("localhost", 8082))
            .with_grpc_service("service2", Endpoint::http("localhost", 8083));

        assert_eq!(instance.gear, "test_gear");
        assert_eq!(instance.instance_id, instance_id);
        assert!(instance.control.is_some());
        assert_eq!(instance.version, Some("1.2.3".to_owned()));
        assert_eq!(instance.grpc_services.len(), 2);
        assert!(instance.grpc_services.contains_key("service1"));
        assert!(instance.grpc_services.contains_key("service2"));
        // The builder declares no reported state until a registration supplies
        // one; the observable state only exists once the directory stamps a
        // lease in `register_instance`.
        assert_eq!(instance.reported_state, None);
    }

    #[test]
    fn test_quarantine_and_sweep() {
        let grace = Duration::from_millis(50);
        let dir = GearManager::new().with_lease_policy(Duration::from_millis(50), grace);

        let instance = Arc::new(GearInstance::new("test_gear", Uuid::new_v4()));
        seed(&dir, Arc::clone(&instance));

        // Lease expired 10ms ago, still within the 50ms grace -> Quarantined,
        // observable, so the sweeper keeps it.
        expire_lease(&instance, Duration::from_millis(10), grace);
        dir.sweep_expired(Instant::now());
        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert!(matches!(instances[0].state(), InstanceState::Quarantined));

        // Past grace: no longer observable, so the sweeper removes it.
        expire_lease(&instance, grace + Duration::from_millis(10), grace);
        dir.sweep_expired(Instant::now());
        assert!(dir.instances_of("test_gear").is_empty());
    }

    #[test]
    fn test_instances_of_empty() {
        let dir = GearManager::new();
        let instances = dir.instances_of("nonexistent");
        assert!(instances.is_empty());
    }

    #[test]
    fn test_rr_prefers_healthy() {
        let dir = GearManager::new();

        // Create two instances: one healthy, one quarantined
        let healthy_id = Uuid::new_v4();
        let healthy = Arc::new(GearInstance::new("test_gear", healthy_id));
        seed(&dir, healthy);
        assert!(dir.heartbeat("test_gear", healthy_id, None));

        let quarantined_id = Uuid::new_v4();
        let quarantined = Arc::new(GearInstance::new("test_gear", quarantined_id));
        seed(&dir, Arc::clone(&quarantined));
        // Lease expired but still within grace -> Quarantined (not serving).
        expire_lease(
            &quarantined,
            Duration::from_millis(10),
            Duration::from_secs(30),
        );

        // RR should only pick the healthy instance
        for _ in 0..5 {
            let picked = dir.pick_instance_round_robin("test_gear").unwrap();
            assert_eq!(picked.instance_id, healthy_id);
        }
    }

    #[test]
    fn test_pick_rest_endpoint_and_openapi() {
        let dir = GearManager::new();
        let id = Uuid::new_v4();
        let instance = Arc::new(
            GearInstance::new("billing", id)
                .with_rest_endpoint(Endpoint::http("billing", 8080))
                .with_openapi_spec("{\"openapi\":\"3.1.0\"}"),
        );
        seed(&dir, instance);

        let rest = dir.pick_rest_endpoint_round_robin("billing").unwrap();
        assert_eq!(rest.uri, "http://billing:8080");

        let spec = dir.openapi_spec_of("billing").unwrap();
        assert!(spec.contains("openapi"));
    }

    #[test]
    fn test_pick_rest_endpoint_none_when_absent() {
        let dir = GearManager::new();
        let id = Uuid::new_v4();
        // Instance exposes only a gRPC service, no REST endpoint.
        let instance = Arc::new(
            GearInstance::new("grpc_only", id)
                .with_grpc_service("some.Service", Endpoint::http("127.0.0.1", 9000)),
        );
        seed(&dir, instance);

        assert!(dir.pick_rest_endpoint_round_robin("grpc_only").is_none());
        assert!(dir.openapi_spec_of("grpc_only").is_none());
        assert!(dir.pick_rest_endpoint_round_robin("missing").is_none());
    }

    #[test]
    fn test_pick_rest_endpoint_round_robin_rotates() {
        let dir = GearManager::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let inst1 = Arc::new(
            GearInstance::new("web", id1).with_rest_endpoint(Endpoint::http("127.0.0.1", 8001)),
        );
        let inst2 = Arc::new(
            GearInstance::new("web", id2).with_rest_endpoint(Endpoint::http("127.0.0.1", 8002)),
        );
        seed(&dir, inst1);
        seed(&dir, inst2);
        assert!(dir.heartbeat("web", id1, None));
        assert!(dir.heartbeat("web", id2, None));

        let ep1 = dir.pick_rest_endpoint_round_robin("web").unwrap();
        let ep2 = dir.pick_rest_endpoint_round_robin("web").unwrap();
        let ep3 = dir.pick_rest_endpoint_round_robin("web").unwrap();

        assert_ne!(ep1, ep2);
        assert_eq!(ep1, ep3);
    }

    #[test]
    fn test_pick_service_round_robin() {
        let dir = GearManager::new();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        // Register two instances providing the same service
        let inst1 = Arc::new(
            GearInstance::new("test_gear", id1)
                .with_grpc_service("test.Service", Endpoint::http("127.0.0.1", 8001)),
        );
        let inst2 = Arc::new(
            GearInstance::new("test_gear", id2)
                .with_grpc_service("test.Service", Endpoint::http("127.0.0.1", 8002)),
        );

        seed(&dir, inst1);
        seed(&dir, inst2);

        // Mark both as healthy
        assert!(dir.heartbeat("test_gear", id1, None));
        assert!(dir.heartbeat("test_gear", id2, None));

        // Pick should rotate between instances
        let pick1 = dir.pick_service_round_robin("test.Service");
        let pick2 = dir.pick_service_round_robin("test.Service");
        let pick3 = dir.pick_service_round_robin("test.Service");

        assert!(pick1.is_some());
        assert!(pick2.is_some());
        assert!(pick3.is_some());

        let (_, inst1, ep1) = pick1.unwrap();
        let (_, inst2, ep2) = pick2.unwrap();
        let (_, inst3, _) = pick3.unwrap();

        // First and third should be the same (round-robin)
        assert_eq!(inst1.instance_id, inst3.instance_id);
        // First and second should be different
        assert_ne!(inst1.instance_id, inst2.instance_id);
        // Endpoints should differ
        assert_ne!(ep1, ep2);
    }

    #[test]
    fn pick_service_falls_back_to_not_ready() {
        // A gear whose only service-providing instance is not yet serving
        // (Registered, no heartbeat) must still resolve, mirroring the REST and
        // instance pickers' not-ready fallback rather than failing closed.
        let dir = GearManager::new();
        let id = Uuid::new_v4();
        seed(
            &dir,
            Arc::new(
                GearInstance::new("worker", id)
                    .with_grpc_service("worker.Svc", Endpoint::http("127.0.0.1", 9000)),
            ),
        );
        // No heartbeat: the instance stays Registered (not serving).
        assert!(matches!(
            dir.instances_of("worker")[0].state(),
            InstanceState::Registered
        ));

        let picked = dir.pick_service_round_robin("worker.Svc");
        assert!(
            picked.is_some(),
            "gRPC service resolution must fall back to the not-ready instance"
        );
        let (gear, _, ep) = picked.unwrap();
        assert_eq!(gear, "worker");
        assert_eq!(ep, Endpoint::http("127.0.0.1", 9000));
    }

    #[test]
    fn pick_service_never_falls_back_to_a_draining_instance() {
        // A gear whose only instance is draining resolves to nothing: draining
        // means "stop routing new work", so it is not a fallback target.
        let dir = GearManager::new();
        let id = Uuid::new_v4();
        seed(
            &dir,
            Arc::new(
                GearInstance::new("worker", id)
                    .with_grpc_service("worker.Svc", Endpoint::http("127.0.0.1", 9000)),
            ),
        );
        assert!(dir.heartbeat("worker", id, Some(ReportedState::Draining)));
        assert!(matches!(
            dir.instances_of("worker")[0].state(),
            InstanceState::Draining
        ));

        assert!(
            dir.pick_service_round_robin("worker.Svc").is_none(),
            "a draining-only gear must not be resolved, even as a fallback"
        );
    }

    #[test]
    fn grpc_service_owner_reports_owning_gear_regardless_of_health() {
        let dir = GearManager::new();

        // A freshly-registered (not-yet-serving) instance still owns its name.
        seed(
            &dir,
            Arc::new(
                GearInstance::new("authz-resolver", Uuid::new_v4()).with_grpc_service(
                    "cf.authz.v1.AuthzService",
                    Endpoint::http("127.0.0.1", 9000),
                ),
            ),
        );

        assert_eq!(
            dir.grpc_service_owner("cf.authz.v1.AuthzService")
                .as_deref(),
            Some("authz-resolver"),
            "the advertising gear owns the name even while only Registered"
        );
        assert!(
            dir.grpc_service_owner("unowned.Service").is_none(),
            "an unadvertised name has no owner"
        );
    }

    /// The production invariant behind the above: a `Registered`-but-not-healthy
    /// owner still blocks another gear's claim of the same name through
    /// `register_instance` (ownership does not wait on the owner becoming healthy).
    #[test]
    fn registered_but_unhealthy_owner_blocks_another_gears_claim() {
        let dir = GearManager::new();
        let owner_id = Uuid::new_v4();

        // Owner claims the name and never heartbeats -> stays Registered.
        dir.register_instance(Arc::new(
            GearInstance::new("authz-resolver", owner_id).with_grpc_service(
                "cf.authz.v1.AuthzService",
                Endpoint::http("127.0.0.1", 9000),
            ),
        ))
        .expect("first claim of an unowned name succeeds");
        assert!(matches!(
            dir.instances_of("authz-resolver")[0].state(),
            InstanceState::Registered
        ));

        // A different gear is rejected via the production path, health regardless.
        let conflict = dir
            .register_instance(Arc::new(
                GearInstance::new("impostor", Uuid::new_v4()).with_grpc_service(
                    "cf.authz.v1.AuthzService",
                    Endpoint::http("127.0.0.1", 9001),
                ),
            ))
            .unwrap_err();
        assert_eq!(conflict.owner, "authz-resolver");
    }

    #[test]
    fn deregister_releases_grpc_service_name_to_another_gear() {
        let dir = GearManager::new();
        let service = "cf.authz.v1.AuthzService";
        let owner_id = Uuid::new_v4();

        seed(
            &dir,
            Arc::new(
                GearInstance::new("authz-resolver", owner_id)
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
            ),
        );

        // While the owner is registered, a different gear cannot claim the name.
        dir.register_instance(Arc::new(
            GearInstance::new("successor", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
        ))
        .unwrap_err();

        // Deregistering the owner releases the name...
        dir.deregister("authz-resolver", owner_id);
        assert!(
            dir.grpc_service_owner(service).is_none(),
            "the name is unowned once its owner deregisters"
        );

        // ...so a different gear may now claim it.
        dir.register_instance(Arc::new(
            GearInstance::new("successor", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
        ))
        .expect("a released gRPC service name must be claimable by another gear");
        assert_eq!(
            dir.grpc_service_owner(service).as_deref(),
            Some("successor")
        );
    }

    #[test]
    fn lease_releases_grpc_service_name_only_after_grace() {
        let grace = Duration::from_millis(50);
        let dir = GearManager::new().with_lease_policy(Duration::from_millis(50), grace);
        let service = "cf.authz.v1.AuthzService";

        let owner = Arc::new(
            GearInstance::new("authz-resolver", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
        );
        seed(&dir, Arc::clone(&owner));
        // Expired but still within grace: the owner reads as Quarantined.
        expire_lease(&owner, Duration::from_millis(10), grace);

        // A quarantined-but-still-observable owner keeps blocking its gRPC name,
        // so a successor is rejected (recoverable conflict).
        assert!(matches!(
            dir.instances_of("authz-resolver")[0].state(),
            InstanceState::Quarantined
        ));
        let conflict = dir
            .register_instance(Arc::new(
                GearInstance::new("successor", Uuid::new_v4())
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
            ))
            .unwrap_err();
        assert_eq!(
            conflict.owner, "authz-resolver",
            "a quarantined-but-observable owner still holds the name"
        );

        // Past the grace window the owner is unobservable and releases the name,
        // so another gear may take it over — even before the sweeper runs.
        expire_lease(&owner, grace + Duration::from_millis(10), grace);
        assert!(
            dir.grpc_service_owner(service).is_none(),
            "an expired owner hands the name back once the grace period lapses"
        );
        dir.register_instance(Arc::new(
            GearInstance::new("successor", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9002)),
        ))
        .expect("an expired owner's gRPC name must be claimable by another gear");
        assert_eq!(
            dir.grpc_service_owner(service).as_deref(),
            Some("successor")
        );
    }

    #[test]
    fn test_deregister_clears_rr_counters() {
        let dir = GearManager::new();
        let id = Uuid::new_v4();
        let instance = Arc::new(
            GearInstance::new("web", id).with_rest_endpoint(Endpoint::http("127.0.0.1", 8001)),
        );
        seed(&dir, instance);
        assert!(dir.heartbeat("web", id, None));

        // Exercise both round-robin counters so the keys are created.
        assert!(dir.pick_instance_round_robin("web").is_some());
        assert!(dir.pick_rest_endpoint_round_robin("web").is_some());

        assert!(dir.rr_counters.contains_key("web"));
        assert!(dir.rr_counters.contains_key("rest:web"));

        dir.deregister("web", id);

        assert!(!dir.rr_counters.contains_key("web"));
        assert!(!dir.rr_counters.contains_key("rest:web"));
    }

    #[test]
    fn test_sweep_expired_clears_rr_counters() {
        let grace = Duration::from_millis(50);
        let dir = GearManager::new().with_lease_policy(Duration::from_millis(50), grace);

        let id = Uuid::new_v4();
        let instance = Arc::new(
            GearInstance::new("web", id).with_rest_endpoint(Endpoint::http("127.0.0.1", 8001)),
        );
        seed(&dir, Arc::clone(&instance));
        assert!(dir.pick_rest_endpoint_round_robin("web").is_some());
        assert!(dir.rr_counters.contains_key("rest:web"));

        // Expired but within grace: still observable, sweep keeps it.
        expire_lease(&instance, Duration::from_millis(10), grace);
        dir.sweep_expired(Instant::now());
        let instances = dir.instances_of("web");
        assert_eq!(instances.len(), 1);
        assert!(matches!(instances[0].state(), InstanceState::Quarantined));

        // Past grace: sweep removes it and frees the round-robin counters.
        expire_lease(&instance, grace + Duration::from_millis(10), grace);
        dir.sweep_expired(Instant::now());

        assert!(dir.instances_of("web").is_empty());
        assert!(!dir.rr_counters.contains_key("web"));
        assert!(!dir.rr_counters.contains_key("rest:web"));
    }

    #[test]
    fn register_instance_rejects_cross_gear_grpc_name() {
        let mgr = GearManager::new();
        let service = "cf.authz.v1.AuthzService";

        mgr.register_instance(Arc::new(
            GearInstance::new("authz-resolver", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
        ))
        .expect("claiming an unowned service name must succeed");

        // A different gear claiming the same name is rejected, store untouched.
        let conflict = mgr
            .register_instance(Arc::new(
                GearInstance::new("evil", Uuid::new_v4())
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
            ))
            .unwrap_err();
        assert_eq!(conflict.service_name, service);
        assert_eq!(conflict.owner, "authz-resolver");
        assert!(mgr.instances_of("evil").is_empty());

        // The owning gear may add another instance under the same name.
        mgr.register_instance(Arc::new(
            GearInstance::new("authz-resolver", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9002)),
        ))
        .expect("the owning gear may add another instance for its own service");
    }

    #[test]
    fn declared_ownership_beats_registration_order() {
        let mgr = GearManager::new();
        let service = "cf.authz.v1.AuthzService";

        // Config declares the name is owned by `authz`.
        mgr.set_grpc_service_owners(HashMap::from([(service.to_owned(), "authz".to_owned())]));

        // A squatter registering *first* cannot claim a declared name; it is
        // rejected with the declared owner, and the store is untouched.
        let conflict = mgr
            .register_instance(Arc::new(
                GearInstance::new("evil", Uuid::new_v4())
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
            ))
            .unwrap_err();
        assert_eq!(conflict.service_name, service);
        assert_eq!(conflict.owner, "authz");
        assert!(
            !conflict.recoverable,
            "a declared-owner conflict is pinned by config (permanent)"
        );
        assert!(mgr.instances_of("evil").is_empty());

        // The declared owner is always admitted, even coming in second.
        mgr.register_instance(Arc::new(
            GearInstance::new("authz", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
        ))
        .expect("the declared owner must be admitted");
        assert_eq!(mgr.grpc_service_owner(service).as_deref(), Some("authz"));
    }

    /// A gear can own an undeclared name via first-registration, after which a
    /// `set_grpc_service_owners` call reassigns that name to a different declared
    /// owner. The declared owner is authorized, but must not begin advertising
    /// the name while the original holder is still serving it — that would split
    /// ownership between two advertisers. It is refused until the stale holder is
    /// gone, then admitted.
    #[test]
    fn declared_owner_refused_while_a_stale_advertiser_still_holds_the_name() {
        let mgr = GearManager::new();
        let service = "cf.authz.v1.AuthzService";
        let holder = Uuid::new_v4();

        // `first` claims the name via first-registration (not yet declared).
        mgr.register_instance(Arc::new(
            GearInstance::new("first", holder)
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
        ))
        .expect("claiming an undeclared, unowned name must succeed");

        // The map is (re)installed, declaring the name is owned by `authz`.
        mgr.set_grpc_service_owners(HashMap::from([(service.to_owned(), "authz".to_owned())]));

        // The declared owner is authorized, but `first` is still advertising the
        // name: admitting `authz` now would split ownership, so it is rejected.
        let conflict = mgr
            .register_instance(Arc::new(
                GearInstance::new("authz", Uuid::new_v4())
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
            ))
            .unwrap_err();
        assert_eq!(conflict.service_name, service);
        assert_eq!(
            conflict.owner, "first",
            "the current holder is the conflicting owner"
        );
        assert!(
            conflict.recoverable,
            "a stale-advertiser conflict clears once the holder deregisters"
        );
        assert!(mgr.instances_of("authz").is_empty());

        // Once the stale holder is gone, the declared owner is admitted and owns
        // the name outright — no split.
        mgr.deregister("first", holder);
        mgr.register_instance(Arc::new(
            GearInstance::new("authz", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9002)),
        ))
        .expect("the declared owner must be admitted once the stale holder is gone");
        assert_eq!(mgr.grpc_service_owner(service).as_deref(), Some("authz"));
    }

    /// The compiled-in (authoritative) map overrides a conflicting operator
    /// entry — the binary that *provides* a service name wins over config that
    /// tries to reassign it — while operator entries for names the binary does
    /// not know about are preserved. A subsequent registration is governed by
    /// the merged result.
    #[test]
    fn authoritative_owners_override_conflicting_config_and_keep_the_rest() {
        let mgr = GearManager::new();

        // Operator config: one name that a compiled gear also provides (a
        // reassignment attempt), and one the binary does not compile in.
        mgr.set_grpc_service_owners(HashMap::from([
            ("cf.authz.v1.AuthzService".to_owned(), "impostor".to_owned()),
            ("remote.only.Service".to_owned(), "remote-gear".to_owned()),
        ]));

        // Compiled registry says authz-resolver provides the authz service.
        mgr.merge_authoritative_grpc_service_owners(HashMap::from([(
            "cf.authz.v1.AuthzService".to_owned(),
            "authz-resolver".to_owned(),
        )]));

        // Compiled-in provider wins the conflict; the remote-only entry stays.
        let conflict = mgr
            .register_instance(Arc::new(
                GearInstance::new("impostor", Uuid::new_v4()).with_grpc_service(
                    "cf.authz.v1.AuthzService",
                    Endpoint::http("127.0.0.1", 9000),
                ),
            ))
            .unwrap_err();
        assert_eq!(conflict.owner, "authz-resolver");
        assert!(
            !conflict.recoverable,
            "a pinned compiled-in owner is permanent"
        );

        mgr.register_instance(Arc::new(
            GearInstance::new("authz-resolver", Uuid::new_v4()).with_grpc_service(
                "cf.authz.v1.AuthzService",
                Endpoint::http("127.0.0.1", 9001),
            ),
        ))
        .expect("the compiled-in provider owns its service name");

        // The operator entry for the name absent from the compiled map is intact.
        let remote_conflict = mgr
            .register_instance(Arc::new(
                GearInstance::new("squatter", Uuid::new_v4())
                    .with_grpc_service("remote.only.Service", Endpoint::http("127.0.0.1", 9002)),
            ))
            .unwrap_err();
        assert_eq!(remote_conflict.owner, "remote-gear");
    }

    #[test]
    fn names_absent_from_declared_map_keep_first_registration_ownership() {
        let mgr = GearManager::new();
        // The map declares one name but says nothing about `worker.Svc`.
        mgr.set_grpc_service_owners(HashMap::from([(
            "cf.authz.v1.AuthzService".to_owned(),
            "authz".to_owned(),
        )]));
        let service = "worker.Svc";

        mgr.register_instance(Arc::new(
            GearInstance::new("worker-a", Uuid::new_v4())
                .with_grpc_service(service, Endpoint::http("127.0.0.1", 9000)),
        ))
        .expect("claiming an undeclared, unowned name must succeed");

        let conflict = mgr
            .register_instance(Arc::new(
                GearInstance::new("worker-b", Uuid::new_v4())
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", 9001)),
            ))
            .unwrap_err();
        assert_eq!(conflict.owner, "worker-a");
        assert!(
            conflict.recoverable,
            "a first-registration conflict clears if the holder leaves"
        );
    }

    /// Under contention, the atomic check+insert admits exactly one owner for a
    /// gRPC service name: N gears race to claim the same unowned name and only
    /// one commits, closing the check-then-write race the per-entry `DashMap`
    /// locks leave open.
    #[test]
    fn concurrent_register_admits_exactly_one_owner() {
        use std::sync::Barrier;

        let mgr = Arc::new(GearManager::new());
        let service = "cf.authz.v1.AuthzService";
        let racers = 16;
        let barrier = Arc::new(Barrier::new(racers));

        #[expect(
            clippy::needless_collect,
            reason = "a lazy iterator would join before all racers spawn, deadlocking the Barrier"
        )]
        let handles: Vec<_> = (0..racers)
            .map(|i| {
                let mgr = Arc::clone(&mgr);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let gear = format!("gear-{i}");
                    let instance = Arc::new(
                        GearInstance::new(gear.clone(), Uuid::new_v4()).with_grpc_service(
                            service,
                            Endpoint::http("127.0.0.1", 9000 + u16::try_from(i).unwrap()),
                        ),
                    );
                    // Release all threads at once to maximise contention.
                    barrier.wait();
                    (gear, mgr.register_instance(instance))
                })
            })
            .collect();

        let results: Vec<(String, Result<(), GrpcServiceNameConflict>)> = handles
            .into_iter()
            .map(|h| h.join().expect("registration thread must not panic"))
            .collect();

        // Exactly one gear wins the race.
        let winners: Vec<&str> = results
            .iter()
            .filter(|(_, r)| r.is_ok())
            .map(|(gear, _)| gear.as_str())
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "exactly one competing registration must win"
        );
        let winner = winners[0];

        // Every loser is rejected with a conflict naming the winning gear — the
        // `owner` surfaced in the server-side warn log and the operator's
        // remediation hint, so it must be the gear that actually committed, not
        // just any error.
        for (gear, result) in &results {
            if gear == winner {
                continue;
            }
            let conflict = result
                .as_ref()
                .expect_err("a losing registration must be rejected, not silently dropped");
            assert_eq!(conflict.service_name, service);
            assert_eq!(
                conflict.owner.as_str(),
                winner,
                "every loser's conflict must name the single winning gear as owner"
            );
        }

        // ...and the store resolves the contested name to exactly that winner.
        assert_eq!(
            mgr.grpc_service_owner(service).as_deref(),
            Some(winner),
            "the contested name must resolve to the one gear that won"
        );
    }
}
