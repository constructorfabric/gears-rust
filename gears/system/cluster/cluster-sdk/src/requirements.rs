//! What `resolve()` recorded, and the readiness verdict computed from it
//! (DESIGN.md).
//!
//! Invariant I5 is the guarantee this module exists to keep: **no consumer serves
//! traffic against an unmet requirement.** `resolve()` enforces it directly when a
//! descriptor is in hand — an unmet capability is `Err(CapabilityNotMet)` at the
//! call site. When the descriptor was not obtainable in time, `resolve()` returns
//! `Ok` and the guarantee has to be kept somewhere else. This is that somewhere:
//! every resolve records what it required, and readiness re-checks it as descriptors
//! land.
//!
//! # Unfeatured, and that is load-bearing
//!
//! Profile 1 needs this. `K3`'s consumer registration never runs in an embedded
//! process — no forwarding feature, nothing inventoried to replay — so a contributor
//! reachable only from the remote branch would leave the profile where a mistake is
//! *always* a build or config error with no backstop at all (§4.9.1). The verdict
//! that fires only in Profile 1 (verdict 2 below) is the clearest case.
//!
//! # The re-validation is the *same closure*, not a reconstruction
//!
//! [`Recorded`] holds the very `validate` closure `binding::bind` would have run
//! inline. Nothing is re-derived, so a deferred verdict cannot drift from the inline
//! error it stands in for — the diagnostic is byte-identical because it is produced
//! by identical code over identical requirements. That is what makes §4.7's "the
//! guarantee and the error text do not vary" checkable rather than aspirational, and
//! it is why this module knows nothing about the three capability types.
//!
//! # What this module cannot do, and what a consumer must therefore write
//!
//! §4.9.1 says the registry "registers itself as a readiness contributor on first
//! use". **It cannot.** The only path a `Healthcheck` reaches `/readyz` is
//! `RestApiCapability::healthcheck()` on a *gear*: the framework collects them per
//! gear during router composition (`host_runtime.rs:1376`), and there is no
//! inventory collection and no `ClientHub` lookup for healthchecks. An SDK has no
//! way in.
//!
//! So a consumer wires it, in one line that is the same in both deployment
//! profiles — which keeps invariant I1 (the source does not vary by profile) while
//! giving up §4.9.1's "no consumer code":
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use toolkit::{GearCtx, Healthcheck};
//! # struct MyGear;
//! # impl MyGear {
//! fn healthcheck(&self, ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
//!     Some(cluster_sdk::cluster_readiness(ctx.client_hub()))
//! }
//! # }
//! ```
//!
//! A consumer that omits it gets no enforcement on the deferred path, which is
//! exactly the gap I5 exists to close — so [`Recorded`] tracks whether the
//! contributor has ever been polled and `resolve()` warns when requirements were
//! recorded and nothing ever asked. A silent gap becomes a log line.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use crate::dto::{ProfileDescriptor, ProfileHealth};
use crate::error::ClusterError;

/// How long a *permanent* verdict must persist before escalation is due (§4.7).
///
/// A permanent error does not resolve on its own, and a never-ready pod is a quiet
/// failure a rollout can sit behind indefinitely. The window is what keeps that
/// apart from a transient dependency failure, which never escalates however long it
/// lasts.
pub const PERMANENT_GRACE: Duration = Duration::from_mins(5);

/// How long a process may have recorded requirements with no cluster client before
/// that reads as a misconfiguration rather than a cold start (§4.9.1).
///
/// Short is safe on both sides. In Profile 1 the cluster gear claims the client in
/// its `init`, before any consumer's `start`, so a resolve that found nothing is
/// already terminal. In Profile 3 `resolve()` self-constructs, so an absent client
/// means the endpoint could not be derived — also terminal. The window exists only
/// so a programmatic fixture that wires out of order is not reported broken, and it
/// sits well inside [`PERMANENT_GRACE`].
pub const NOT_WIRED_GRACE: Duration = Duration::from_secs(30);

/// How long a resolve waits for the contributor to be polled before warning that
/// nobody wired it.
///
/// Generous on purpose: the framework composes its router and starts probing well
/// inside this, so a warning means the `healthcheck()` line is genuinely missing
/// rather than merely late.
pub const CONTRIBUTOR_SILENCE_WARNING: Duration = Duration::from_mins(1);

/// How stale a descriptor may be before the contributor forces a refresh (§5.5).
///
/// Health moves without a configuration change, so a client that only read its
/// descriptor cache would never observe a profile degrading — `descriptor()`
/// answers from the cache whenever it is populated, by design. This is what makes
/// verdict 3 reachable *and* self-clearing.
pub const DESCRIPTOR_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// The readiness code reported when a recorded requirement is unmet, or nothing is
/// wired. Permanent: it cannot fix itself.
pub const CODE_MISCONFIGURED: &str = "cluster-misconfigured";
/// The readiness code reported when a recorded profile is non-serving server-side.
/// Transient: it clears when the backend recovers.
pub const CODE_PROFILE_DEGRADED: &str = "cluster-profile-degraded";
/// The readiness code reported while descriptors are still landing.
pub const CODE_STARTING: &str = "cluster-starting";

/// The health enum as a word, because `clippy::use_debug` is denied and this string
/// reaches an operator through `/health`.
const fn health_name(health: ProfileHealth) -> &'static str {
    match health {
        ProfileHealth::Serving => "serving",
        // Also the `_UNSPECIFIED = 0` reading, deliberately: an unspecified health
        // pulls consumers out of rotation rather than keeping them in (§4.4).
        ProfileHealth::Degraded => "degraded",
    }
}

/// The stored form of a resolve's capability validator.
///
/// Named because `clippy::type_complexity` requires it, and it earns the name: this
/// is the *same* closure `binding::bind` runs inline, shared with the registry rather
/// than reconstructed, which keeps the deferred diagnostic identical to the
/// inline one (see the module docs).
pub type RecordedValidator =
    Box<dyn Fn(&ProfileDescriptor) -> Result<(), ClusterError> + Send + Sync>;

/// One `resolve()` that happened, and how to re-check it.
struct Recorded {
    /// The profile resolved. Interned by the resolver, so `&'static`.
    profile: &'static str,
    /// `"cache"`, `"lock"` or `"leader"` — the same string `bind` logs.
    primitive: &'static str,
    /// The inline validator, stored rather than reconstructed. See the module docs.
    validate: RecordedValidator,
}

/// A readiness verdict, with the permanence the escalation rule turns on.
///
/// Permanence is carried here and **not** on the framework's `HealthcheckResult`,
/// which has no such notion — §12.13's sketch tags verdict 3 with a `.transient()`
/// method that does not exist. The distinction is cluster's, and it exists for
/// exactly one purpose: deciding whether a verdict may escalate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Every recorded requirement is satisfied by a descriptor in hand.
    Ready,
    /// Descriptors have not landed yet. Ordinary cold start.
    Starting(String),
    /// A recorded profile is non-serving server-side. Clears on recovery.
    ProfileDegraded(String),
    /// A recorded requirement is unmet, or nothing was ever wired. Cannot clear.
    Misconfigured(String),
}

impl Verdict {
    /// Whether this verdict can fix itself without an operator.
    ///
    /// Only the negative answer escalates (§4.7). A degraded backend must never
    /// crash-loop the fleet, so it is a distinct variant rather than a
    /// differently-worded `Misconfigured`.
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        matches!(*self, Self::Misconfigured(_))
    }

    /// The stable `/health` code for this verdict.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match *self {
            Self::Ready => "ok",
            Self::Starting(_) => CODE_STARTING,
            Self::ProfileDegraded(_) => CODE_PROFILE_DEGRADED,
            Self::Misconfigured(_) => CODE_MISCONFIGURED,
        }
    }

    /// The verdict as the framework's result type.
    ///
    /// **Every not-ready verdict is `Unhealthy`, including the degraded one**, and
    /// that is deliberate rather than lazy. The framework maps `Unhealthy` to
    /// `state: starting` / **503** and `Degraded` to **200 with `ready: true`**
    /// (`runtime/readiness.rs`), so reporting `Degraded` here would leave the
    /// consumer *in* rotation — the opposite of §4.4's requirement that a degraded
    /// profile pull its consumers out of it.
    ///
    /// Note this is the mirror image of the **cluster gear's** own check, which
    /// reports `Degraded` for an unreachable profile precisely so the pod stays in
    /// rotation (evicting it would take coordination down for every profile, and
    /// `DescribeProfiles` is how a consumer learns its profile is degraded). Same
    /// word, opposite decision, correct on both sides: the server must keep serving
    /// what it can, the consumer must stop serving what it cannot.
    #[must_use]
    pub fn into_result(self) -> toolkit::HealthcheckResult {
        match self {
            Self::Ready => toolkit::HealthcheckResult::healthy(),
            Self::Starting(message) => {
                toolkit::HealthcheckResult::unhealthy(message).with_code(CODE_STARTING)
            }
            Self::ProfileDegraded(message) => {
                toolkit::HealthcheckResult::unhealthy(message).with_code(CODE_PROFILE_DEGRADED)
            }
            Self::Misconfigured(message) => {
                toolkit::HealthcheckResult::unhealthy(message).with_code(CODE_MISCONFIGURED)
            }
        }
    }
}

/// Process-global record of what every `resolve()` in this process required.
///
/// Reached through [`requirements`], never constructed by a caller: the whole point
/// is that there is exactly one per process, because the guarantee it keeps is a
/// property of the process rather than of any one facade.
#[derive(Default)]
pub struct RequirementRegistry {
    recorded: Mutex<Vec<Recorded>>,
    /// When the first resolve happened, which is when the grace windows start.
    first_resolve_at: OnceLock<Instant>,
    /// Whether any `resolve()` ever found (or built) a `dyn ClusterClient`.
    client_seen: AtomicBool,
    /// Whether a readiness contributor has ever been polled — the check that the
    /// consumer wired one at all.
    contributor_polled: AtomicBool,
    /// Whether the contributor-silence one-shot has been armed. Per-registry, not
    /// process-global, so each test drives its own (see `requirements_tests`).
    silence_warning_armed: AtomicBool,
    /// Whether the missing-`healthcheck()` warning has already been emitted. One
    /// per process: the message is a static instruction, so repeating it is noise.
    silence_warned: AtomicBool,
    /// When the current run of permanent verdicts began, or `None`.
    permanent_since: Mutex<Option<Instant>>,
}

/// The process's requirement registry.
pub fn requirements() -> &'static RequirementRegistry {
    static REGISTRY: OnceLock<RequirementRegistry> = OnceLock::new();
    REGISTRY.get_or_init(RequirementRegistry::default)
}

impl std::fmt::Debug for RequirementRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequirementRegistry")
            .field("recorded", &self.recorded_count())
            .field("client_seen", &self.client_seen.load(Ordering::Relaxed))
            .field(
                "contributor_polled",
                &self.contributor_polled.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl RequirementRegistry {
    /// Records a resolve and its validator (§4.7.1).
    ///
    /// Called by `binding::bind` on **every** resolve, not only the deferred ones:
    /// §5.6's descriptor refresh re-validates against the recorded set, so an inline
    /// success still has to be recorded or a profile that degrades after startup
    /// would never be re-checked.
    pub fn record(
        &self,
        profile: &'static str,
        primitive: &'static str,
        validate: RecordedValidator,
    ) {
        // Already set means an earlier resolve won the race, which is the value we
        // want - the grace windows run from the *first* resolve.
        let _first = self.first_resolve_at.set(Instant::now());
        self.recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Recorded {
                profile,
                primitive,
                validate,
            });
        self.warn_if_nobody_is_listening();
    }

    /// Notes whether this resolve found a client. Sticky-true: one success means the
    /// process is wired, and a later miss is a different fault.
    pub fn set_client_seen(&self, seen: bool) {
        if seen {
            self.client_seen.store(true, Ordering::Relaxed);
        }
    }

    /// Whether any resolve ever found a client.
    #[must_use]
    pub fn client_seen(&self) -> bool {
        self.client_seen.load(Ordering::Relaxed)
    }

    /// How many resolves have been recorded.
    #[must_use]
    pub fn recorded_count(&self) -> usize {
        self.recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// The distinct profiles this process resolved, in a stable order.
    #[must_use]
    pub fn recorded_profiles(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self
            .recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|entry| entry.profile)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Marks the contributor as polled, so the missing-`healthcheck()` warning stops.
    pub fn note_contributor_polled(&self) {
        self.contributor_polled.store(true, Ordering::Relaxed);
    }

    /// The verdict, given a way to read each recorded profile's descriptor.
    ///
    /// `descriptor` returns `None` for a profile whose descriptor has not landed.
    /// The four verdicts are evaluated in §12.13's order, and the order is the
    /// specification rather than an implementation detail: a misconfiguration must
    /// win over a cold start, or a permanently broken consumer would report
    /// `Starting` forever and never escalate.
    pub fn verdict(&self, descriptor: &impl Fn(&str) -> Option<ProfileDescriptor>) -> Verdict {
        let verdict = self.classify(descriptor);
        self.track_permanence(&verdict);
        verdict
    }

    /// [`verdict`](Self::verdict)'s decision, without the permanence bookkeeping.
    ///
    /// Split out because `verdict` would otherwise exceed the workspace's
    /// cognitive-complexity limit, and because the classification is what the tests
    /// want to drive directly.
    fn classify(&self, descriptor: &impl Fn(&str) -> Option<ProfileDescriptor>) -> Verdict {
        let recorded = self.recorded.lock().unwrap_or_else(PoisonError::into_inner);

        // Nothing resolved: this process does not use cluster, so it has no cluster
        // readiness to report. Not `Starting` -- a consumer that never resolved must
        // not be held out of rotation by a dependency it does not have.
        if recorded.is_empty() {
            return Verdict::Ready;
        }

        // 1. A recorded requirement a landed descriptor does not satisfy. Permanent,
        //    and the message is the inline error verbatim (see the module docs).
        for entry in recorded.iter() {
            if let Some(profile) = descriptor(entry.profile)
                && let Err(err) = (entry.validate)(&profile)
            {
                return Verdict::Misconfigured(format!(
                    "profile `{}` ({}): {err}",
                    entry.profile, entry.primitive
                ));
            }
        }

        // 2. Nothing wired at all, past the grace window. In Profile 1 this is
        //    "`cluster` is not linked" or "the `profiles` block is missing"; in
        //    Profile 3 it is a client that could not be built. `resolve()` cannot
        //    tell either from a cold start, so enforcement is here
        //    (§4.7, §4.9.1).
        if !self.client_seen() && self.grace_elapsed(NOT_WIRED_GRACE) {
            return Verdict::Misconfigured(format!(
                "no cluster client is registered in this process, but {} resolve(s) recorded \
                 requirements: is the `cluster` gear linked, or the SDK's client feature enabled?",
                recorded.len()
            ));
        }

        // 3. A recorded profile the server reports as non-serving. Transient: it
        //    clears itself when the backend recovers, so it must never escalate.
        for entry in recorded.iter() {
            if let Some(profile) = descriptor(entry.profile)
                && profile.health != ProfileHealth::Serving
            {
                return Verdict::ProfileDegraded(format!(
                    "profile `{}` is {} server-side",
                    entry.profile,
                    health_name(profile.health)
                ));
            }
        }

        // 4. Descriptors still landing. Ordinary cold start, and the only verdict
        //    that is expected during normal startup.
        if let Some(entry) = recorded
            .iter()
            .find(|entry| descriptor(entry.profile).is_none())
        {
            return Verdict::Starting(format!(
                "profile `{}`: descriptor has not arrived yet",
                entry.profile
            ));
        }

        Verdict::Ready
    }

    /// Starts, sustains or clears the permanent-verdict timer.
    fn track_permanence(&self, verdict: &Verdict) {
        let mut since = self
            .permanent_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if verdict.is_permanent() {
            if since.is_none() {
                *since = Some(Instant::now());
            }
        } else {
            *since = None;
        }
    }

    /// Whether a permanent verdict has persisted past [`PERMANENT_GRACE`], and the
    /// reason if so (§4.7).
    ///
    /// **This reports the decision; it does not act on it.** Wiring it to
    /// `std::process::exit` is deliberately not done here — that escalation would
    /// owe the platform's ADR-0005 an amendment recording it as a considered
    /// exception, which is unresolved. An SDK terminating its host process is the
    /// platform's call to sanction, not cluster's to assume.
    #[must_use]
    pub fn escalation_due(&self) -> bool {
        self.permanent_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|since| since.elapsed() >= PERMANENT_GRACE)
    }

    /// Whether `extra` has elapsed since the first resolve.
    fn grace_elapsed(&self, extra: Duration) -> bool {
        self.first_resolve_at
            .get()
            .is_some_and(|at| at.elapsed() >= extra)
    }

    /// Warns once a resolve has recorded requirements and no contributor has ever
    /// been polled — the consumer forgot the `healthcheck()` line.
    ///
    /// A pure level test, evaluated from two places: the one-shot
    /// [`arm_contributor_silence_warning`](Self::arm_contributor_silence_warning)
    /// spawns (which makes it reachable at all), and [`record`](Self::record),
    /// which keeps a genuinely late resolve reporting immediately rather than not
    /// at all if the one-shot could not be armed.
    fn warn_if_nobody_is_listening(&self) {
        if !self.contributor_polled.load(Ordering::Relaxed)
            && self.grace_elapsed(CONTRIBUTOR_SILENCE_WARNING)
            // Last, and only once: the instruction is static, so a long-lived
            // process must not repeat it on every later resolve.
            && !self.silence_warned.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                "cluster: requirements were recorded but no readiness contributor has been \
                 polled in this process. Return `cluster_sdk::cluster_readiness(ctx.client_hub())` \
                 from your gear's `RestApiCapability::healthcheck`, or an unmet requirement \
                 discovered after startup will not gate traffic (invariant I5)"
            );
        }
    }

    /// Arms the one-shot that reports a forgotten `healthcheck()` line.
    ///
    /// # Why this needs a task at all, when the module deliberately has none
    ///
    /// [`warn_if_nobody_is_listening`](Self::warn_if_nobody_is_listening) tests a
    /// **level** condition — "a minute has passed and nothing has polled" — and
    /// until this existed the only thing that evaluated it was
    /// [`record`](Self::record), an **edge**. The two cannot coincide: a consumer
    /// resolves its facades inside `start()` milliseconds apart and never resolves
    /// again, so the trigger has stopped firing a minute before the condition
    /// becomes true, and I5's only mitigation for a missing contributor was
    /// unreachable.
    ///
    /// There is no poll to hang it off instead. In the exact scenario this detects
    /// the contributor is *absent*, so nothing calls [`verdict`](Self::verdict) or
    /// [`escalation_due`](Self::escalation_due) — they have no other call site.
    /// Hanging the check off either would move the bug rather than fix it.
    ///
    /// # And why this does not cost the "nothing to cancel at shutdown" property
    ///
    /// That property is about the §5.5 descriptor *refresh*: a recurring pump
    /// would need a `CancellationToken` threaded in from the wiring and a join at
    /// stop, so it became an interval on the contributor instead. None
    /// of that applies here. This task is armed at most once per process, captures
    /// nothing but `&'static self` (no client, no hub, no channel), holds no
    /// resource, and **ends by itself** within [`CONTRIBUTOR_SILENCE_WARNING`].
    /// Dropping the runtime drops a parked `sleep`, so there is still nothing to
    /// cancel. The sibling precedent is cluster's own:
    /// `wiring::spawn_descriptor_prefetch` is a detached, unjoined startup
    /// one-shot on this very path.
    ///
    /// Callers need no runtime: outside one this is a no-op, which lets
    /// `record()` stay callable from a synchronous test.
    pub fn arm_contributor_silence_warning(&'static self) {
        // Per-registry latch, so fifty resolves arm one task rather than fifty.
        if self.silence_warning_armed.swap(true, Ordering::Relaxed) {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        // An **absolute** deadline off `first_resolve_at`, not a relative sleep:
        // it is the same instant `grace_elapsed` measures from, so the wake cannot
        // land before the condition it tests can be true, and a task that is not
        // polled promptly still wakes at the right time rather than a late one.
        let Some(deadline) = self
            .first_resolve_at
            .get()
            .map(|at| *at + CONTRIBUTOR_SILENCE_WARNING)
        else {
            // Armed before any resolve. Unreachable from `bind`, which records
            // first; defensive only, and it un-latches so a later arm still takes.
            self.silence_warning_armed.store(false, Ordering::Relaxed);
            return;
        };
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            self.warn_if_nobody_is_listening();
        });
    }

    /// Whether a contributor has ever been polled. Test-only.
    #[cfg(test)]
    pub(crate) fn contributor_polled_for_test(&self) -> bool {
        self.contributor_polled.load(Ordering::Relaxed)
    }
}

/// The readiness contributor a consumer gear returns from its own `healthcheck()`.
///
/// See the [module docs](self) for why a consumer has to wire this rather than the
/// SDK registering itself, and for the one line that does it.
///
/// It holds the hub rather than a client, because in Profile 3 the client may be
/// registered after this is constructed (the wiring phase and a consumer's `start`
/// are different phases) and because the hub is what a `GearCtx` hands out.
pub struct ClusterReadinessContributor {
    hub: std::sync::Arc<toolkit::client_hub::ClientHub>,
    /// When descriptors were last force-refreshed, so health changes are observed
    /// (§5.5). `None` until the first poll.
    last_refresh: Mutex<Option<Instant>>,
}

/// The contributor over `hub`, as a gear's `healthcheck()` returns it.
#[must_use]
pub fn cluster_readiness(
    hub: std::sync::Arc<toolkit::client_hub::ClientHub>,
) -> std::sync::Arc<dyn toolkit::Healthcheck> {
    std::sync::Arc::new(ClusterReadinessContributor {
        hub,
        last_refresh: Mutex::new(None),
    })
}

impl std::fmt::Debug for ClusterReadinessContributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterReadinessContributor")
            .finish_non_exhaustive()
    }
}

impl ClusterReadinessContributor {
    /// Whether a forced descriptor refresh is due, marking it done if so.
    fn refresh_due(&self) -> bool {
        let mut last = self
            .last_refresh
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let due = last.is_none_or(|at| at.elapsed() >= DESCRIPTOR_REFRESH_INTERVAL);
        if due {
            *last = Some(Instant::now());
        }
        due
    }
}

#[toolkit::async_trait]
impl toolkit::Healthcheck for ClusterReadinessContributor {
    fn name(&self) -> &'static str {
        "cluster-requirements"
    }

    async fn check(&self) -> toolkit::HealthcheckResult {
        let registry = requirements();
        registry.note_contributor_polled();

        // `try_get`, never `binding::process_client`: that helper *self-constructs* a
        // remote client when the hub is empty, which would make the nothing-wired
        // verdict unreachable by the very act of checking for it.
        let client = self.hub.try_get::<dyn crate::client::ClusterClient>();

        // Force a refresh on the interval, because `descriptor()` answers from the
        // cache whenever it is populated -- so without this a profile that degrades
        // after startup would never be observed and verdict 3 could not fire.
        if let Some(client) = client.as_ref()
            && self.refresh_due()
            && let Err(err) = client.refresh_descriptors().await
        {
            // Not a verdict of its own: an unreachable cluster is transient. Before
            // any descriptor has landed the classification below reports `Starting`
            // anyway, because there is nothing cached to classify. Once one has
            // landed, a failed refresh leaves the last good set standing and this
            // pod keeps serving on it -- a failing poll must not evict every
            // consumer in the fleet each time the cluster gear rolls (invariant I6's
            // runtime counterpart). The cost is that the set can then be arbitrarily
            // stale; bounding that is a design question, not this call site's.
            tracing::debug!(error = %err, "cluster readiness: descriptor refresh failed");
        }

        // Collect each recorded profile's descriptor once, so the classification is
        // a pure function over a snapshot rather than issuing I/O inside a lock.
        let mut descriptors = std::collections::BTreeMap::new();
        if let Some(client) = client.as_ref() {
            for profile in registry.recorded_profiles() {
                if let Ok(descriptor) = client.descriptor(profile).await {
                    descriptors.insert(profile, descriptor);
                }
            }
        }

        let verdict = registry.verdict(&|profile: &str| descriptors.get(profile).cloned());

        // The escalation decision is reported, not acted on -- see `escalation_due`.
        if registry.escalation_due() {
            tracing::error!(
                verdict = verdict.code(),
                "cluster readiness: a permanent configuration error has persisted past the \
                 grace window; this pod cannot become ready without an operator"
            );
        }
        verdict.into_result()
    }
}

#[cfg(test)]
#[path = "requirements_tests.rs"]
mod requirements_tests;
