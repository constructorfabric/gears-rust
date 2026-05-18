//! [`BootstrapService`] — orchestrates the platform-bootstrap saga.
//!
//! Implements FEATURE `platform-bootstrap` (see
//! `modules/system/account-management/docs/features/feature-platform-bootstrap.md`).
//!
//! The saga has three observable phases (FEATURE §3):
//!
//! 1. **Idempotency classification** — `find_by_id(root_id)` drives the
//!    branch decision. Active root → no-op skip; Provisioning root →
//!    in-band synchronous compensation when age > stuck threshold
//!    (see [`BootstrapService::attempt_stuck_row_compensation`]) and
//!    otherwise resume the peer-wait loop; Suspended/Deleted root →
//!    fail-fast `Internal` (illegal pre-existing state — operator
//!    intervention required); no row → fresh insert.
//! 2. **`IdP` provision with backoff** — `provision_tenant` is itself
//!    the readiness signal. A `IdpProvisionFailure::CleanFailure` during
//!    the provision call while the deadline still has budget
//!    reschedules the saga after compensating; the bootstrap deadline
//!    (`idp_wait_timeout`) is the wall-clock cap. Backoff doubles from
//!    `idp_retry_backoff_initial` up to `idp_retry_backoff_max` per
//!    FEATURE §3.
//! 3. **Finalization** — single short transaction that flips the root
//!    row from `Provisioning` to `Active` and writes the self-row in
//!    `tenant_closure` via `TenantRepo::activate_tenant` (closure
//!    helpers in [`crate::domain::tenant::closure`]).
//!
//! Compensation rules per FEATURE §3 `algo-platform-bootstrap-finalization-saga`:
//!
//! * `IdpProvisionFailure::CleanFailure` → delete the provisioning row and
//!   surface `idp_unavailable` (retry-safe).
//! * `IdpProvisionFailure::Ambiguous` → leave the provisioning row in
//!   place for the reaper; surface `Internal` (NOT retry-safe).
//! * `IdpProvisionFailure::UnsupportedOperation` → delete the provisioning
//!   row and surface `idp_unsupported_operation`.

use std::sync::Arc;
use std::time::Duration;

use modkit_macros::domain_model;
use modkit_security::AccessScope;
use time::OffsetDateTime;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use account_management_sdk::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpPluginClient, IdpProvisionFailure,
    IdpProvisionTenantRequest, IdpTenantContext,
};
use serde_json::Value;

use crate::domain::bootstrap::config::BootstrapConfig;
use crate::domain::error::DomainError;
use crate::domain::metrics::{AM_BOOTSTRAP_LIFECYCLE, MetricKind, emit_metric};
use crate::domain::tenant::TenantContext;
use crate::domain::tenant::closure::build_activation_rows;
use crate::domain::tenant::model::{NewTenant, TenantModel, TenantStatus};
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::util::backoff::compute_next_backoff;
use types_registry_sdk::TypesRegistryClient;

/// Internal classification produced by `BootstrapService::classify`.
///
/// `TenantModel` is ~120 bytes; the previous `Box<TenantModel>` arms
/// existed solely to placate `clippy::large_enum_variant`. Bootstrap
/// runs at most once per process, so the per-call heap traffic is not
/// worth the indirection — allow the lint instead.
#[allow(clippy::large_enum_variant)]
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
enum BootstrapClassification {
    /// No root row exists — proceed with the fresh-insert + saga path.
    NoRoot,
    /// Active root already present — skip (idempotent re-run).
    ActiveRootExists(TenantModel),
    /// Root row in `Provisioning` observed at classify time. The
    /// saga does NOT re-run `IdP` + activate from this branch:
    /// instead it falls through to the retry loop (no preflight) so
    /// a peer replica currently mid-saga can finalize its own
    /// attempt and we observe the outcome. If the row's age exceeds
    /// `2 × idp_wait_timeout` (the FEATURE-§3 stuck threshold) the
    /// branch first attempts in-band synchronous compensation
    /// (`deprovision_tenant` + row cleanup) and only surfaces
    /// `Internal` (`deferred_to_reaper`) when the `IdP` does not
    /// confirm cleanup; if the bootstrap deadline expires while
    /// waiting, the branch surfaces `IdpUnavailable`.
    ProvisioningRootResume(TenantModel),
    /// Root row in `Suspended` or `Deleted` — illegal pre-existing
    /// state. Fail-fast.
    InvariantViolation { observed_status: TenantStatus },
}

/// Bound on consecutive `AlreadyExists` retries during the saga's
/// `Insert` step. A configured `root_id` that drifted away from the
/// actual DB root collides with `ux_tenants_single_root` on every
/// insert while the next `classify` (filtered by configured id)
/// keeps returning `NoRoot`. Without a cap that pair would loop
/// forever; the cap escalates a drifted config to a clean
/// `Internal` error instead of spinning init.
const MAX_ALREADY_EXISTS_STREAK: u32 = 3;

/// Side-effect-free description of "what the saga is about to do
/// next". Each variant maps 1:1 to a `step_*` method on
/// `BootstrapService`; the `step` dispatcher performs the IO and
/// returns the next state. `Terminal` carries the final outcome
/// for `run()`'s caller.
///
/// `TenantModel` payload on `Finalize` is ~120 bytes; bootstrap
/// runs at most once per process so the per-call heap traffic is
/// not worth the indirection — allow the lint instead.
#[allow(clippy::large_enum_variant)]
#[domain_model]
enum BootstrapState {
    /// Entry: classify before the retry loop. Decides between the
    /// idempotent terminal fast-paths (`Active` /
    /// `InvariantViolation` / stuck-`Provisioning` that fails
    /// in-band compensation) and entering the loop (in-flight
    /// `Provisioning` resume → flag takeover; fresh `NoRoot` → run
    /// preflight first).
    InitialClassify,

    /// Initial-`NoRoot` bridge: run `preflight_root_tenant_type` +
    /// `preflight_root_gts_config` once before entering the loop.
    /// Skipped on the `ProvisioningRootResume` path — the peer is
    /// performing those checks.
    InitialPreflight,

    /// Top of the retry loop: re-classify and dispatch.
    LoopClassify,

    /// Takeover bridge: the loop saw `NoRoot` AND
    /// `pending_takeover_precheck` is set (we entered the loop via
    /// `ProvisioningRootResume` and the peer's row vanished). Run
    /// the preflight pair once before insert; the flag flips off
    /// so a same-run subsequent `NoRoot` does not re-pay the
    /// precheck cost.
    TakeoverPreflight,

    /// Call `insert_root_provisioning(scope)`; dispatch on
    /// `Ok(_) | Err(AlreadyExists) | Err(other)`.
    Insert,

    /// Call `finalize(scope, provisioning_root, deadline)`; dispatch
    /// on `Ok | Err(IdpUnavailable) | Err(other)`.
    Finalize { provisioning_root: TenantModel },

    /// Sleep + backoff bump, then return to `LoopClassify`. The
    /// deadline-trip terminal is decided BEFORE entering this state
    /// (so once we are here the deadline is known not to have
    /// tripped). `reason` does not drive anything today but pins
    /// the call site for future per-arm telemetry without breaking
    /// the state shape.
    Sleep { reason: SleepReason },

    /// Final outcome ready for `run()` to return.
    Terminal(Result<TenantModel, DomainError>),
}

/// Why we are about to sleep. Surfaced as a static label on the
/// `am.bootstrap` trace event emitted by `step_sleep` so operators
/// driving incident response from logs can attribute a wake-up
/// cycle to the call site that scheduled it (peer-wait vs.
/// `IdP`-retry vs. `AlreadyExists` race) without correlating
/// against the deadline-trip metric, which only fires on the
/// terminal arm.
#[domain_model]
#[derive(Debug, Clone, Copy)]
enum SleepReason {
    /// `Provisioning` row exists, age ≤ stuck, deadline not
    /// tripped: peer is finalizing; we sleep and re-classify.
    PeerInProgress,
    /// `finalize` returned `IdpUnavailable`, deadline not tripped:
    /// `compensate_provisioning` already ran inside `finalize`, so
    /// the next `classify` lands on `NoRoot` and the saga restarts
    /// step 2.
    IdpRetryOnFinalize,
    /// `insert_root_provisioning` lost the unique-root race, streak
    /// under cap, deadline not tripped: re-classify to see if the
    /// winner finalized.
    AlreadyExistsRetry,
}

impl SleepReason {
    /// Stable string label for log/dashboard filtering. Kept
    /// separate from `Debug` so a derive change cannot silently
    /// rename the value operators key on.
    fn as_label(self) -> &'static str {
        match self {
            Self::PeerInProgress => "peer_in_progress",
            Self::IdpRetryOnFinalize => "idp_retry_on_finalize",
            Self::AlreadyExistsRetry => "already_exists_retry",
        }
    }
}

/// Output of `classify_with_terminal_dispatch`. Folds the idempotent
/// terminal fast-paths (`Active` / `InvariantViolation` / stuck
/// `Provisioning` after a failed in-band compensation) plus a
/// classify error into a single `Terminal` arm, leaving only the two
/// context-divergent variants (`Resume`, `NoRoot`) for the call site
/// to dispatch on. Owns `TenantModel` on `Resume` for the same
/// reason `BootstrapState` does — the saga runs at most once per
/// process so heap-traffic cost is irrelevant; allow the lint.
#[allow(clippy::large_enum_variant)]
#[domain_model]
enum ClassifyOutcome {
    /// One of the idempotent terminal fast-paths fired (`Active`
    /// root / `InvariantViolation` / stuck `Provisioning` whose
    /// in-band compensation did not confirm IdP-side cleanup) or
    /// the underlying `classify` call returned an error. The caller
    /// returns this `BootstrapState` directly.
    Terminal(BootstrapState),
    /// A peer's `Provisioning` row younger than `stuck_threshold`
    /// was observed. The caller decides between flagging takeover
    /// (initial classify) or running the deadline-then-sleep dance
    /// (loop classify).
    Resume(TenantModel),
    /// No root row exists (either originally, or because in-band
    /// stuck-row compensation just dropped it). The caller decides
    /// between bridging through `InitialPreflight` (initial
    /// `NoRoot`) and gating on `pending_takeover_precheck` to drive
    /// `TakeoverPreflight` or direct `Insert` (loop `NoRoot`).
    NoRoot,
}

/// Mutable saga state threaded through `step_*` calls by `&mut`.
/// Keeps the `step` signatures uniform and isolates every piece of
/// state the loop mutates (deadline / backoff / streak / takeover
/// flag) so they cannot drift between branches.
#[domain_model]
struct RunCtx {
    scope: AccessScope,
    deadline: Instant,
    cap: Duration,
    stuck_threshold: time::Duration,
    backoff: Duration,
    pending_takeover_precheck: bool,
    already_exists_streak: u32,
}

/// Platform-bootstrap saga.
///
/// Owns the root-tenant lifecycle from `absent` (or
/// `stuck-provisioning`) to `active`. Holds no async state across calls
/// — every invocation re-reads the current root row from the repo.
#[domain_model]
pub struct BootstrapService<R: TenantRepo> {
    repo: Arc<R>,
    idp: Arc<dyn IdpPluginClient>,
    types_registry: Option<Arc<dyn TypesRegistryClient>>,
    cfg: BootstrapConfig,
    /// Mirrors `cfg.idp.required` from the parent
    /// `AccountManagementConfig`. Threaded in via
    /// [`Self::with_idp_required`] so the step-3 compensator can
    /// treat `IdpDeprovisionFailure::UnsupportedOperation` symmetrically
    /// with `TenantService::compensate_failed_activation`: under
    /// `idp.required = false` the variant means "no plugin wired,
    /// nothing to clean" and the local row may be deleted; under
    /// `idp.required = true` it means "the real plugin can't
    /// deprovision a tenant it provisioned" and the row MUST be
    /// left for the reaper to avoid orphaning vendor-side state.
    /// Defaults to `false` so test embedders inherit the
    /// Phase-1/2 default without having to thread the flag through.
    idp_required: bool,
    /// Cooperative cancellation token. When cancelled, every
    /// `tokio::time::sleep` in the saga (step-sleep backoff) is
    /// interrupted via `tokio::select!`, and the saga surfaces
    /// `DomainError::Internal` with a "cancelled" diagnostic.
    /// Threaded in via [`Self::with_cancel`]; defaults to a
    /// standalone token that is never cancelled, preserving the
    /// pre-cancel API contract for existing callers and tests.
    cancel: CancellationToken,
}

impl<R: TenantRepo> BootstrapService<R> {
    /// Construct a fully-wired bootstrap service.
    ///
    /// # Caller contract
    ///
    /// Callers **MUST** call [`BootstrapConfig::validate`] before
    /// `new()`. The check below is a `debug_assert!` (stripped from
    /// release builds), so a nil-UUID config slips through release
    /// silently and breaks `fr-bootstrap-idempotency` on the next
    /// platform restart (`BootstrapConfig` uses `serde(default)`, so
    /// an empty TOML table deserialises to `Uuid::nil()`; see
    /// `feature-platform-bootstrap.md` lines 23-25).
    ///
    /// Module wiring (`module.rs::init`) is the canonical validate
    /// call site because it owns the strict/non-strict branching: a
    /// strict-mode validation failure is lifecycle-fatal, whereas a
    /// non-strict failure logs and skips bootstrap entirely. Tests
    /// and future embedders that construct the saga directly inherit
    /// the same obligation.
    ///
    /// # Panics
    ///
    /// In debug builds, panics if `cfg.validate()` returns `Err` — the
    /// caller violated the validate-before-construct contract above.
    /// Release builds strip the assertion, so the contract MUST be
    /// honored at every call site (production path is honored by
    /// `module.rs::init`).
    #[must_use]
    pub fn new(repo: Arc<R>, idp: Arc<dyn IdpPluginClient>, cfg: BootstrapConfig) -> Self {
        // Single `validate()` call so the assertion message and the
        // boolean predicate cannot disagree (a previous shape called
        // `validate()` twice and could in principle produce different
        // error text on the second call once any non-pure check is
        // added; the form below stays correct under refactor).
        if cfg!(debug_assertions)
            && let Err(err) = cfg.validate()
        {
            panic!(
                "BootstrapConfig must be validated before constructing BootstrapService; \
                 module wiring is the canonical validate call site (see module.rs::init). \
                 Validation failed: {err}"
            );
        }
        Self {
            repo,
            idp,
            types_registry: None,
            cfg,
            idp_required: false,
            cancel: CancellationToken::new(),
        }
    }

    /// Attach the GTS Types Registry client used for root-tenant-type
    /// preflight. Tests that exercise non-GTS paths may omit this; module
    /// wiring supplies it when `ClientHub` resolves the registry client.
    #[must_use]
    pub fn with_types_registry(mut self, types_registry: Arc<dyn TypesRegistryClient>) -> Self {
        self.types_registry = Some(types_registry);
        self
    }

    /// Mirror the parent `AccountManagementConfig.idp.required` flag
    /// into the saga so step-3 compensation can apply the same
    /// `UnsupportedOperation` policy `TenantService::compensate_failed_activation`
    /// uses: refuse to delete the local row when a real plugin returns
    /// Unsupported under `idp.required = true`, because vendor-side
    /// state may exist that AM cannot reach. Module wiring sets this;
    /// tests that operate against `NoopIdpProvider` (or that don't care
    /// about the orphan-prevention path) can omit it and inherit the
    /// `idp.required = false` default.
    #[must_use]
    pub fn with_idp_required(mut self, idp_required: bool) -> Self {
        self.idp_required = idp_required;
        self
    }

    /// Attach a cooperative shutdown token so the saga's sleep
    /// loops can be interrupted on SIGTERM / lifecycle drain. When
    /// the token fires, every `tokio::time::sleep` in the saga is
    /// interrupted and the saga returns
    /// `DomainError::Internal { diagnostic: "bootstrap cancelled …" }`.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Run the bootstrap saga to terminal state.
    ///
    /// # Returned tenant status
    ///
    /// On `Ok(_)` the returned [`TenantModel`] is always
    /// [`TenantStatus::Active`] — the saga either created and
    /// activated a fresh root, observed an already-active one, or
    /// waited for a peer replica's saga to flip the row to active.
    /// A row stuck in `Provisioning` whose in-band compensation
    /// failed to confirm IdP-side cleanup, or a deadline-exhausted
    /// peer-wait surfaces as `Err(_)` rather than `Ok(Provisioning)`
    /// — the strict-mode `init` gate in
    /// `module::run_bootstrap_phase` decides whether to abort or
    /// proceed without an active root.
    ///
    /// # Errors
    ///
    /// * [`DomainError::IdpUnavailable`] when every `provision_tenant`
    ///   retry returned `CleanFailure` within the deadline or the
    ///   deadline elapsed while peer-waiting.
    /// * [`DomainError::UnsupportedOperation`] when the `IdP` plugin signals
    ///   it cannot perform root provisioning at all (compensated).
    /// * [`DomainError::Internal`] for ambiguous `IdP` outcomes (provisioning
    ///   row left for reaper) and for invariant-violation root states.
    #[tracing::instrument(skip_all, fields(root_id = %self.cfg.root_id))]
    pub async fn run(&self) -> Result<TenantModel, DomainError> {
        // Initial classification runs BEFORE any IdP work so the
        // idempotent fast-paths (already-Active root, invariant-
        // violation status) decide without contacting the IdP. A
        // restart with an already-active root therefore succeeds
        // even when the IdP is down. Only `NoRoot` and
        // `ProvisioningRootResume`-in-flight enter the retry loop;
        // a stuck `ProvisioningRootResume` is reaped synchronously
        // in-band (see `attempt_stuck_row_compensation`) before
        // either restarting the saga path on confirmed cleanup or
        // surfacing the `deferred_to_reaper` terminal. The state
        // machine encodes that ordering: every path from
        // `InitialClassify` to `Insert` either dies in `Terminal`
        // first or transits through `InitialPreflight` /
        // `TakeoverPreflight`.
        //
        // `BootstrapConfig::validate` caps `idp_wait_timeout` at
        // `MAX_IDP_WAIT_TIMEOUT` (1h), so both
        // `Instant::checked_add` and the `i64::try_from(secs * 2)`
        // cast below are safe by construction. The `Err` arms here
        // are defensive: they surface a clean `Internal` if the
        // validate-before-construct contract was violated rather
        // than panicking on a misconfiguration.
        let deadline = Instant::now()
            .checked_add(self.cfg.idp_wait_timeout)
            .ok_or_else(|| DomainError::Internal {
                diagnostic: format!(
                    "bootstrap deadline overflow: Instant::now() + {:?} exceeds platform Instant range; idp_wait_timeout must be validated <= {:?}",
                    self.cfg.idp_wait_timeout,
                    crate::domain::bootstrap::config::MAX_IDP_WAIT_TIMEOUT,
                ),
                cause: None,
            })?;
        // `2 × idp_wait_timeout` is the FEATURE-§3 stuck threshold
        // for distinguishing a crashed previous attempt from one
        // currently mid-saga on a peer replica (the peer's saga
        // budget is bounded by `idp_wait_timeout`, so anything
        // older than 2x is by definition not in flight).
        let stuck_secs = i64::try_from(self.cfg.idp_wait_timeout.as_secs().saturating_mul(2))
            .map_err(|_| DomainError::Internal {
                diagnostic: format!(
                    "bootstrap stuck-threshold overflow: 2 * {:?} does not fit in i64; idp_wait_timeout must be validated <= {:?}",
                    self.cfg.idp_wait_timeout,
                    crate::domain::bootstrap::config::MAX_IDP_WAIT_TIMEOUT,
                ),
                cause: None,
            })?;
        let mut ctx = RunCtx {
            scope: AccessScope::allow_all(),
            deadline,
            cap: self.cfg.idp_retry_backoff_max,
            stuck_threshold: time::Duration::seconds(stuck_secs),
            backoff: self.cfg.idp_retry_backoff_initial,
            // `pending_takeover_precheck` defers the preflight pair
            // when we enter the loop via `ProvisioningRootResume`.
            // The peer-mid-saga branch deliberately skips those
            // checks (the peer is doing them), but if the peer or
            // reaper later removes the row, the loop's `NoRoot` arm
            // would otherwise jump straight to
            // `insert_root_provisioning` + `finalize` without ever
            // validating the root tenant type. Run them lazily on
            // the first NoRoot observation in that case.
            pending_takeover_precheck: false,
            already_exists_streak: 0,
        };
        let mut state = BootstrapState::InitialClassify;
        loop {
            state = self.step(state, &mut ctx).await;
            if let BootstrapState::Terminal(result) = state {
                return result;
            }
        }
    }

    /// Driver dispatch -- one match arm per `BootstrapState` variant.
    /// Each arm calls a dedicated `step_*` method that performs the
    /// IO and returns the next state. The dispatcher itself owns
    /// nothing: every piece of mutable saga state lives in
    /// `RunCtx`, threaded through by `&mut`.
    async fn step(&self, state: BootstrapState, ctx: &mut RunCtx) -> BootstrapState {
        match state {
            BootstrapState::InitialClassify => self.step_initial_classify(ctx).await,
            BootstrapState::InitialPreflight => self.step_initial_preflight(ctx).await,
            BootstrapState::LoopClassify => self.step_loop_classify(ctx).await,
            BootstrapState::TakeoverPreflight => self.step_takeover_preflight(ctx).await,
            BootstrapState::Insert => self.step_insert(ctx).await,
            BootstrapState::Finalize { provisioning_root } => {
                self.step_finalize(ctx, provisioning_root).await
            }
            BootstrapState::Sleep { reason } => self.step_sleep(ctx, reason).await,
            BootstrapState::Terminal(_) => {
                unreachable!("Terminal must short-circuit `run()` before re-entering `step`",)
            }
        }
    }

    /// Dispatches `classify` and folds the three idempotent
    /// terminal fast-paths (`Active` / `InvariantViolation` /
    /// stuck `Provisioning`) plus a classify error into a
    /// `Terminal` outcome the call site returns directly. Hoists
    /// the shared match shape between `step_initial_classify` and
    /// `step_loop_classify` so the per-context divergence stays in
    /// the call site rather than being duplicated under each arm.
    async fn classify_with_terminal_dispatch(&self, ctx: &RunCtx) -> ClassifyOutcome {
        let cls = match self.classify(&ctx.scope).await {
            Ok(c) => c,
            Err(e) => return ClassifyOutcome::Terminal(BootstrapState::Terminal(Err(e))),
        };
        match cls {
            BootstrapClassification::ActiveRootExists(root) => {
                ClassifyOutcome::Terminal(BootstrapState::Terminal(Ok(handle_skip(root))))
            }
            BootstrapClassification::InvariantViolation { observed_status } => {
                ClassifyOutcome::Terminal(BootstrapState::Terminal(Err(
                    handle_invariant_violation(observed_status),
                )))
            }
            BootstrapClassification::ProvisioningRootResume(existing) => {
                let age = OffsetDateTime::now_utc() - existing.created_at;
                if age > ctx.stuck_threshold {
                    // Stuck row: try one synchronous
                    // `deprovision_tenant` + `compensate_provisioning`
                    // pass in-band before declaring the row a reaper
                    // problem. On confirmed teardown the local row is
                    // dropped and the saga restarts on a clean
                    // `NoRoot`; on any non-clean outcome we fall
                    // through to the existing `deferred_to_reaper`
                    // terminal — the row stays in place and the
                    // reaper picks it up on its next tick.
                    //
                    // Observability is owned end-to-end by
                    // `attempt_stuck_row_compensation`: every IdP
                    // outcome (`ok` / `already_absent` /
                    // `unsupported_noop` / `unsupported_required` /
                    // `terminal` / `retryable` / `timeout`) emits a
                    // labelled `phase=stuck_recovery` counter plus a
                    // redacted warn. No metric is emitted here so the
                    // per-variant breakdown does not double-count.
                    if self
                        .attempt_stuck_row_compensation(&existing, ctx.deadline)
                        .await
                        .is_ok()
                    {
                        info!(
                            target: "am.bootstrap",
                            root_id = %existing.id,
                            "stuck Provisioning row compensated in-band; saga restarts on NoRoot"
                        );
                        ClassifyOutcome::NoRoot
                    } else {
                        ClassifyOutcome::Terminal(BootstrapState::Terminal(Err(
                            handle_deferred_to_reaper_stuck(&existing),
                        )))
                    }
                } else {
                    ClassifyOutcome::Resume(existing)
                }
            }
            BootstrapClassification::NoRoot => ClassifyOutcome::NoRoot,
        }
    }

    async fn step_initial_classify(&self, ctx: &mut RunCtx) -> BootstrapState {
        match self.classify_with_terminal_dispatch(ctx).await {
            ClassifyOutcome::Terminal(state) => state,
            ClassifyOutcome::Resume(_) => {
                // peer-mid-saga: fall through to the retry loop
                // WITHOUT preflight; the peer is performing those
                // steps and we just observe its outcome. If the row
                // vanishes mid-wait the loop's NoRoot arm will run
                // preflight once before taking over (gated by
                // `pending_takeover_precheck`).
                ctx.pending_takeover_precheck = true;
                BootstrapState::LoopClassify
            }
            ClassifyOutcome::NoRoot => BootstrapState::InitialPreflight,
        }
    }

    async fn step_initial_preflight(&self, ctx: &mut RunCtx) -> BootstrapState {
        // Permanent type-misconfiguration fails fast before any
        // saga IO.
        if let Err(e) = self.preflight_root_tenant_type(ctx.deadline).await {
            return BootstrapState::Terminal(Err(e));
        }
        // Registry-backed config validation for `root_name` /
        // `root_tenant_metadata`.
        if let Err(e) = self.preflight_root_gts_config(ctx.deadline).await {
            return BootstrapState::Terminal(Err(e));
        }
        BootstrapState::LoopClassify
    }

    async fn step_loop_classify(&self, ctx: &mut RunCtx) -> BootstrapState {
        match self.classify_with_terminal_dispatch(ctx).await {
            ClassifyOutcome::Terminal(state) => state,
            ClassifyOutcome::Resume(existing) => {
                ctx.already_exists_streak = 0;
                if Instant::now() >= ctx.deadline {
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[
                            ("phase", "provisioning_wait"),
                            ("classification", "in_progress_elsewhere"),
                            ("outcome", "timeout"),
                        ],
                    );
                    warn!(
                        target: "am.bootstrap",
                        root_id = %existing.id,
                        "bootstrap deadline exhausted while peer was finalizing root; surfacing idp_unavailable"
                    );
                    return BootstrapState::Terminal(Err(DomainError::IdpUnavailable {
                        detail:
                            "peer replica did not finalize the root within the bootstrap deadline"
                                .to_owned(),
                    }));
                }
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "provisioning_wait"),
                        ("classification", "in_progress_elsewhere"),
                        ("outcome", "retry"),
                    ],
                );
                BootstrapState::Sleep {
                    reason: SleepReason::PeerInProgress,
                }
            }
            ClassifyOutcome::NoRoot => {
                // Bootstrap deadline guard: a timed-out finalize, a
                // sleep that consumed the last budget, or a slow
                // takeover-preflight can leave the loop here past the
                // wall-clock cap. Without this check the loop would
                // still advance into `TakeoverPreflight` or
                // `Insert`, mutating state (and potentially leaving a
                // fresh `Provisioning` row) after `idp_wait_timeout`
                // has elapsed. Mirror the deadline-trip terminal used
                // by the `Resume`, `Finalize`, and `AlreadyExists`
                // branches so every loop arm honors the same cap.
                if Instant::now() >= ctx.deadline {
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[
                            ("phase", "idp_waiting"),
                            ("classification", "no_root_post_deadline"),
                            ("outcome", "timeout"),
                        ],
                    );
                    warn!(
                        target: "am.bootstrap",
                        "bootstrap deadline exhausted before reaching insert/takeover; surfacing idp_unavailable"
                    );
                    return BootstrapState::Terminal(Err(DomainError::IdpUnavailable {
                        detail: "bootstrap deadline exhausted before root insert".to_owned(),
                    }));
                }
                // Takeover path: the row we initially observed in
                // `Provisioning` was removed by the peer's
                // compensation or by the reaper, leaving us as the
                // first writer. Run the same preflight pair the
                // initial-NoRoot path performs once, then proceed
                // to insert. The flag flips off so a subsequent
                // NoRoot iteration in the same `run()` (after
                // another classify cycle) does not re-pay the
                // precheck cost.
                if ctx.pending_takeover_precheck {
                    BootstrapState::TakeoverPreflight
                } else {
                    BootstrapState::Insert
                }
            }
        }
    }

    async fn step_takeover_preflight(&self, ctx: &mut RunCtx) -> BootstrapState {
        if let Err(e) = self.preflight_root_tenant_type(ctx.deadline).await {
            return BootstrapState::Terminal(Err(e));
        }
        if let Err(e) = self.preflight_root_gts_config(ctx.deadline).await {
            return BootstrapState::Terminal(Err(e));
        }
        ctx.pending_takeover_precheck = false;
        BootstrapState::Insert
    }

    async fn step_insert(&self, ctx: &mut RunCtx) -> BootstrapState {
        match self.insert_root_provisioning(&ctx.scope).await {
            Ok(inserted) => {
                ctx.already_exists_streak = 0;
                BootstrapState::Finalize {
                    provisioning_root: inserted,
                }
            }
            // `ux_tenants_single_root` partial unique index surfaces
            // a concurrent winner OR a root-id drift (configured
            // `root_id` doesn't match the existing root row) as
            // `AlreadyExists`. The first case resolves on the next
            // `classify`; the second case produces an oscillating
            // `NoRoot → AlreadyExists → NoRoot` loop because the
            // classify is filtered by the configured id. Cap the
            // consecutive streak so a drifted config escalates to a
            // clean invariant error instead of spinning init.
            Err(DomainError::AlreadyExists { .. }) => {
                ctx.already_exists_streak += 1;
                if ctx.already_exists_streak >= MAX_ALREADY_EXISTS_STREAK {
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[
                            ("phase", "failed"),
                            ("classification", "root_id_drift"),
                            ("outcome", "failure"),
                        ],
                    );
                    warn!(
                        target: "am.bootstrap",
                        streak = ctx.already_exists_streak,
                        root_id = %self.cfg.root_id,
                        "configured root_id does not match the existing platform root; aborting init"
                    );
                    return BootstrapState::Terminal(Err(DomainError::internal(format!(
                        "platform root already exists with a different id; configured root_id={} cannot be inserted (likely a config drift between platform restarts)",
                        self.cfg.root_id
                    ))));
                }
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "root_creating"),
                        ("classification", "race_loser"),
                        ("outcome", "reclassify"),
                    ],
                );
                info!(
                    target: "am.bootstrap",
                    streak = ctx.already_exists_streak,
                    "concurrent root insert observed; re-classifying on next loop iteration"
                );
                // Yield once before the next classify so a legitimate
                // concurrent-replica race does not burn through
                // `MAX_ALREADY_EXISTS_STREAK` in microseconds. The
                // other retry arms in this loop sleep on the same
                // backoff accumulator, so re-using it here keeps the
                // per-invocation envelope symmetric.
                if Instant::now() >= ctx.deadline {
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[
                            ("phase", "root_creating"),
                            ("classification", "race_loser"),
                            ("outcome", "timeout"),
                        ],
                    );
                    return BootstrapState::Terminal(Err(DomainError::IdpUnavailable {
                        detail: "bootstrap deadline exhausted while losing concurrent-root inserts"
                            .to_owned(),
                    }));
                }
                BootstrapState::Sleep {
                    reason: SleepReason::AlreadyExistsRetry,
                }
            }
            Err(err) => BootstrapState::Terminal(Err(err)),
        }
    }

    // @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-idp-wait-ordering:p1:inst-dod-bootstrap-retry-envelope
    //
    // Implements the bootstrap retry-envelope DoD: bounds the saga's
    // `provision_tenant` re-attempts by `idp_wait_timeout` (deadline
    // computed in `run()`), with backoff doubling from
    // `idp_retry_backoff_initial` up to `idp_retry_backoff_max` (cap
    // applied by `step_sleep` via `compute_next_backoff`). On
    // `CleanFailure` the finalize step has already compensated the
    // provisioning row, so the retry path is row-clean by
    // construction; retry exhaustion surfaces `IdpUnavailable`
    // (mapped to 503 at the canonical-errors boundary) with no
    // partial row left behind.
    async fn step_finalize(
        &self,
        ctx: &mut RunCtx,
        provisioning_root: TenantModel,
    ) -> BootstrapState {
        match self
            .finalize(&ctx.scope, provisioning_root, ctx.deadline)
            .await
        {
            Ok(root) => BootstrapState::Terminal(Ok(root)),
            Err(err) if matches!(err, DomainError::IdpUnavailable { .. }) => {
                if Instant::now() >= ctx.deadline {
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[("phase", "idp_waiting"), ("outcome", "timeout")],
                    );
                    warn!(
                        target: "am.bootstrap",
                        "bootstrap idp wait exhausted; surfacing idp_unavailable"
                    );
                    return BootstrapState::Terminal(Err(err));
                }
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[("phase", "idp_waiting"), ("outcome", "retry")],
                );
                BootstrapState::Sleep {
                    reason: SleepReason::IdpRetryOnFinalize,
                }
            }
            Err(err) => BootstrapState::Terminal(Err(err)),
        }
    }
    // @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-idp-wait-ordering:p1:inst-dod-bootstrap-retry-envelope

    async fn step_sleep(&self, ctx: &mut RunCtx, reason: SleepReason) -> BootstrapState {
        let sleep_for = bounded_sleep(ctx.backoff, ctx.deadline);
        tracing::trace!(
            target: "am.bootstrap",
            reason = reason.as_label(),
            sleep_ms = u64::try_from(sleep_for.as_millis()).unwrap_or(u64::MAX),
            "bootstrap saga sleeping before re-classify",
        );
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => {
                return BootstrapState::Terminal(Err(DomainError::Internal {
                    diagnostic: "bootstrap cancelled by shutdown signal".into(),
                    cause: None,
                }));
            }
            () = tokio::time::sleep(sleep_for) => {}
        }
        ctx.backoff = compute_next_backoff(ctx.backoff, ctx.cap);
        BootstrapState::LoopClassify
    }

    // The previous `try_bootstrap_once` / `try_bootstrap_once_locked`
    // forwarders were folded into the unified retry loop in `run()`
    // above. Keeping a one-shot attempt helper would have re-classified
    // on every iteration and called `preflight_root_tenant_type` more
    // than once for a clean NoRoot path, both of which are unwanted.

    async fn preflight_root_tenant_type(&self, deadline: Instant) -> Result<(), DomainError> {
        let Some(registry) = &self.types_registry else {
            emit_metric(
                AM_BOOTSTRAP_LIFECYCLE,
                MetricKind::Counter,
                &[
                    ("phase", "gts_preflight"),
                    ("classification", "service_unavailable"),
                    ("outcome", "failure"),
                ],
            );
            return Err(DomainError::service_unavailable(
                "types-registry client not attached",
            ));
        };

        // Bound the registry call by the bootstrap deadline so a stalled
        // types-registry cannot hang the saga indefinitely. Mirrors the
        // `provision_tenant` timeout used in `finalize`. A timeout
        // surfaces as `service_unavailable` (the registry is treated as
        // a transient infrastructure dependency); operators see the
        // `gts_preflight / service_unavailable` counter classification
        // and can correlate with the deadline.
        let entity = match tokio::time::timeout_at(
            deadline,
            registry.get_type_schema(self.cfg.root_tenant_type.as_ref()),
        )
        .await
        {
            Ok(Ok(entity)) => entity,
            Ok(Err(err)) => {
                if err.is_not_found() {
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[
                            ("phase", "gts_preflight"),
                            ("classification", "invalid_tenant_type"),
                            ("outcome", "failure"),
                        ],
                    );
                    return Err(DomainError::InvalidTenantType {
                        detail: self.cfg.root_tenant_type.to_string(),
                    });
                }
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "gts_preflight"),
                        ("classification", "service_unavailable"),
                        ("outcome", "failure"),
                    ],
                );
                return Err(DomainError::service_unavailable(format!(
                    "types-registry: {err}"
                )));
            }
            Err(_elapsed) => {
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "gts_preflight"),
                        ("classification", "service_unavailable"),
                        ("outcome", "timeout"),
                    ],
                );
                return Err(DomainError::service_unavailable(
                    "types-registry preflight timed out",
                ));
            }
        };

        // `GtsTypeSchema` is by construction a type-schema (the
        // `is_schema` axis is implicit in the type), so the only check
        // remaining is the AM-tenant-type chain prefix on `type_id`.
        let entity_type_id = entity.type_id.as_ref();
        if !entity_type_id.starts_with("gts.cf.core.am.tenant_type.v1~") {
            emit_metric(
                AM_BOOTSTRAP_LIFECYCLE,
                MetricKind::Counter,
                &[
                    ("phase", "gts_preflight"),
                    ("classification", "invalid_tenant_type"),
                    ("outcome", "failure"),
                ],
            );
            return Err(DomainError::InvalidTenantType {
                detail: format!("{entity_type_id} is not an AM tenant type"),
            });
        }

        // `effective_traits` resolves the GTS chain (own + ancestors)
        // so a root type that inherits a non-empty
        // `allowed_parent_types` from a base schema is correctly
        // rejected. Reading `entity.raw_schema` directly only sees the
        // type's own declarations and would silently accept an
        // inherited rule, breaking the root-eligibility contract —
        // the same effective-trait resolution that
        // `GtsTenantTypeChecker` already uses for child create.
        let allowed = extract_allowed_parent_types_from_effective(&entity.effective_traits())?;
        if !allowed.is_empty() {
            emit_metric(
                AM_BOOTSTRAP_LIFECYCLE,
                MetricKind::Counter,
                &[
                    ("phase", "gts_preflight"),
                    ("classification", "type_not_allowed"),
                    ("outcome", "failure"),
                ],
            );
            return Err(DomainError::TypeNotAllowed {
                detail: format!(
                    "root tenant type {} has allowed_parent_types={allowed:?}",
                    self.cfg.root_tenant_type
                ),
            });
        }

        emit_metric(
            AM_BOOTSTRAP_LIFECYCLE,
            MetricKind::Counter,
            &[("phase", "gts_preflight"), ("outcome", "success")],
        );
        Ok(())
    }

    /// Read the configured root id and classify the bootstrap state.
    // @cpt-begin:cpt-cf-account-management-algo-platform-bootstrap-idempotency-detection:p1:inst-algo-idem-classify-root
    // @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-idempotency:p1:inst-dod-bootstrap-idempotency-classify
    async fn classify(&self, scope: &AccessScope) -> Result<BootstrapClassification, DomainError> {
        let existing = self.repo.find_by_id(scope, self.cfg.root_id).await?;
        Ok(match existing {
            None => BootstrapClassification::NoRoot,
            Some(t) => match t.status {
                TenantStatus::Active => BootstrapClassification::ActiveRootExists(t),
                TenantStatus::Provisioning => BootstrapClassification::ProvisioningRootResume(t),
                other => BootstrapClassification::InvariantViolation {
                    observed_status: other,
                },
            },
        })
    }
    // @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-idempotency:p1:inst-dod-bootstrap-idempotency-classify
    // @cpt-end:cpt-cf-account-management-algo-platform-bootstrap-idempotency-detection:p1:inst-algo-idem-classify-root

    /// Registry-backed validation of the operator-configured root
    /// tenant metadata. Runs in the preflight phase (BEFORE the
    /// saga loop opens its first `provision_tenant` call) so a bad
    /// `root_name` or `root_tenant_metadata` fails fast as
    /// `Validation` instead of burning the full `idp_wait_timeout`
    /// budget on retries.
    ///
    /// Mirrors the call sites in `TenantService::create_tenant` (and
    /// the `cf-resource-group::validate_metadata_via_gts` posture):
    /// when the registry has the schema the JSON-Schema bounds
    /// (`minLength`, `maxLength`) gate the call; when the schema is
    /// not yet registered the helper short-circuits to `Ok(())` and
    /// the DB `CHECK (length(name) BETWEEN 1 AND 255)` constraint
    /// serves as the last-line guard for `root_name`. Without these
    /// checks running at preflight, a misconfigured `root_name`
    /// would land in `tenants.name` after passing only the
    /// trim-non-empty check in [`BootstrapConfig::validate`].
    ///
    /// # Deadline contract
    ///
    /// Both calls are live registry calls — a stalled types-registry
    /// would otherwise hang the bootstrap saga past
    /// `idp_wait_timeout`. Each await is wrapped in
    /// `tokio::time::timeout_at(deadline, …)` so the saga's
    /// deadline-bound contract (already enforced for
    /// `preflight_root_tenant_type` and `provision_tenant`) extends
    /// to these too. A timeout maps to `service_unavailable` and the
    /// bootstrap retry loop handles it the same way as a transient
    /// registry error.
    async fn preflight_root_gts_config(&self, deadline: Instant) -> Result<(), DomainError> {
        let Some(registry) = self.types_registry.as_ref() else {
            return Ok(());
        };
        match tokio::time::timeout_at(
            deadline,
            crate::domain::gts_validation::validate_tenant_name_via_gts(
                &self.cfg.root_name,
                registry.as_ref(),
            ),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(err),
            Err(_elapsed) => {
                return Err(DomainError::service_unavailable(
                    "bootstrap: validate_tenant_name_via_gts timed out against types-registry",
                ));
            }
        }
        // No plugin-input metadata shape validation here: the IdP
        // plugin owns its own `provisioning_metadata` shape end-to-
        // end (AM treats the value as opaque per the IdP-metadata
        // isolation contract). A misshaped payload surfaces as a
        // downstream plugin error during `provision_tenant`, which
        // the saga classifies via the standard `IdpProvisionFailure`
        // ladder; no AM-side fast-fail on the request shape.
        //
        // Size-only cap is enforced here, BEFORE the first
        // `provision_tenant` call, so an oversize
        // `root_tenant_metadata` fails fast as `Validation` instead
        // of being shipped over the wire (and reshipped on every
        // subsequent `IdpPluginClient` call via
        // `TenantContext::metadata`).
        crate::domain::tenant::service::check_idp_metadata_size(
            "bootstrap.root_tenant_metadata",
            self.cfg.root_tenant_metadata.as_ref(),
        )?;
        Ok(())
    }

    /// Saga step 1 — insert the root row in `provisioning` status.
    // @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-root-creation:p1:inst-dod-bootstrap-root-insert
    async fn insert_root_provisioning(
        &self,
        scope: &AccessScope,
    ) -> Result<TenantModel, DomainError> {
        // GTS-config validation runs in the preflight phase
        // (`preflight_root_gts_config`) so a bad `root_name` /
        // `root_tenant_metadata` fails BEFORE the first
        // `provision_tenant` call. By the time we reach this method
        // the configured values have already been gated.
        //
        // The repo enforces the per-id uniqueness invariant; we accept
        // its `Conflict` mapping if a concurrent replica beat us to it.
        // Root tenants are the unique row with `parent_id = None` per
        // the migration's `ck_tenants_root_depth` constraint
        // (`parent_id IS NULL AND depth = 0`) and the
        // `ux_tenants_single_root` partial unique index.
        // Derive `tenant_type_uuid` from the configured GTS id via
        // `gts::GtsID::to_uuid()` — the same canonical V5-UUID
        // algorithm `create_tenant` uses, so the bootstrap and
        // child-create paths produce identical FK values for the
        // same `root_tenant_type`. `GtsID::new` additionally
        // validates the chain shape, surfacing
        // `DomainError::InvalidTenantType` early on a malformed
        // configuration rather than at the FK insert.
        let tenant_type_uuid = gts::GtsID::new(self.cfg.root_tenant_type.as_ref())
            .map_err(|e| DomainError::InvalidTenantType {
                detail: format!(
                    "invalid root_tenant_type chain `{}`: {e}",
                    self.cfg.root_tenant_type
                ),
            })?
            .to_uuid();
        let root_tenant = NewTenant {
            id: self.cfg.root_id,
            parent_id: None,
            name: self.cfg.root_name.clone(),
            self_managed: false,
            tenant_type_uuid,
            depth: 0,
        };
        let inserted = self.repo.insert_provisioning(scope, &root_tenant).await?;
        // Emit the success counter only after the insert returned
        // `Ok(_)`; emitting before the await would record a phantom
        // success on every `AlreadyExists` race-loser pass.
        emit_metric(
            AM_BOOTSTRAP_LIFECYCLE,
            MetricKind::Counter,
            &[("phase", "root_creating"), ("outcome", "success")],
        );
        Ok(inserted)
    }
    // @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-root-creation:p1:inst-dod-bootstrap-root-insert

    /// Saga steps 2 + 3 — `IdP` provision + activate-with-closure-self-row.
    // @cpt-begin:cpt-cf-account-management-algo-platform-bootstrap-finalization-saga:p1:inst-algo-bootstrap-finalization
    // @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-root-creation:p1:inst-dod-bootstrap-root-finalize
    // @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-idp-linking:p1:inst-dod-bootstrap-idp-link
    async fn finalize(
        &self,
        scope: &AccessScope,
        provisioning_root: TenantModel,
        deadline: Instant,
    ) -> Result<TenantModel, DomainError> {
        // The `idp_provisioning` success counter is emitted from
        // `handle_provision_success` after `provision_tenant` returns
        // `Ok`; emitting it here would record a success on every IdP
        // call attempt regardless of outcome.

        // Root tenants are the canonical bootstrap target per
        // `dod-platform-bootstrap-root-creation` — the SDK enum
        // names this branch explicitly so plugin authors see
        // "this is the root, not a missing parent".
        let mut req = IdpProvisionTenantRequest::for_root(
            provisioning_root.id,
            self.cfg.root_name.clone(),
            self.cfg.root_tenant_type.clone(),
        );
        if let Some(meta) = self.cfg.root_tenant_metadata.clone() {
            req = req.with_metadata(meta);
        }
        // Cap the IdP call at the bootstrap deadline so a hung
        // provider cannot stretch `module::init` past
        // `idp_wait_timeout`. Treat a timeout as
        // `IdpUnavailable` after compensating the row — the saga
        // retry loop handles that variant the same way it handles a
        // `CleanFailure`.
        match tokio::time::timeout_at(deadline, self.idp.provision_tenant(&req)).await {
            Ok(Ok(result)) => {
                let metadata = result.metadata;
                match self
                    .handle_provision_success(scope, provisioning_root.id, metadata.as_ref())
                    .await
                {
                    Ok(activated) => Ok(activated),
                    Err(err) => {
                        // Step-3 (activate_tenant) failed AFTER the IdP
                        // already created the tenant. Mirror the
                        // child-create saga: bound the IdP-side
                        // deprovision attempt by the bootstrap
                        // deadline, then run the local compensation
                        // ONLY if the IdP confirmed cleanup. On any
                        // other outcome (timeout, retryable, terminal,
                        // unsupported) leave the `Provisioning` row in
                        // place so the reaper picks it up — deleting
                        // locally while the vendor tenant is still
                        // alive turns step-3 rollback into an external
                        // orphan with no AM-side handle to clean it up.
                        //
                        // Forward the IdP-returned metadata into the
                        // deprovision call so the plugin sees its own
                        // per-tenant state for vendor-side teardown.
                        self.compensate_step3_failure(
                            scope,
                            provisioning_root.id,
                            metadata.as_ref(),
                            deadline,
                        )
                        .await;
                        Err(err)
                    }
                }
            }
            Ok(Err(failure)) => Err(self
                .handle_provision_failure(scope, provisioning_root.id, failure)
                .await),
            Err(_elapsed) => {
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "failed"),
                        ("classification", "idp_timeout"),
                        ("outcome", "failure"),
                    ],
                );
                // Do NOT compensate on timeout. `tokio::time::timeout_at`
                // only stops *local* waiting; it does not prove the
                // `provision_tenant` request never reached the `IdP` or
                // never completed. Deleting the local Provisioning row
                // here would let the retry loop re-insert and call
                // `provision_tenant` again, potentially duplicating or
                // orphaning vendor-side state when the original request
                // succeeded after the deadline. Treat this like
                // `IdpProvisionFailure::Ambiguous`: leave the row in place
                // so the provisioning reaper can take ownership and
                // classify it on its own tick.
                warn!(
                    target: "am.bootstrap",
                    root_id = %provisioning_root.id,
                    "bootstrap IdP provision_tenant exceeded the bootstrap deadline; leaving Provisioning row for the reaper (timeout is ambiguous, not a confirmed cancellation)"
                );
                Err(DomainError::IdpUnavailable {
                    detail: "bootstrap IdP provision_tenant timed out".to_owned(),
                })
            }
        }
    }
    // @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-idp-linking:p1:inst-dod-bootstrap-idp-link
    // @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-root-creation:p1:inst-dod-bootstrap-root-finalize
    // @cpt-end:cpt-cf-account-management-algo-platform-bootstrap-finalization-saga:p1:inst-algo-bootstrap-finalization

    // @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-root-creation:p1:inst-dod-bootstrap-root-activate
    // @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-success-telemetry
    async fn handle_provision_success(
        &self,
        scope: &AccessScope,
        root_id: uuid::Uuid,
        idp_metadata: Option<&Value>,
    ) -> Result<TenantModel, DomainError> {
        emit_metric(
            AM_BOOTSTRAP_LIFECYCLE,
            MetricKind::Counter,
            &[("phase", "idp_provisioning"), ("outcome", "success")],
        );
        // No AM-side schema validation of the IdP-returned blob —
        // the plugin owns its shape end-to-end per the IdP-metadata
        // isolation contract. The value is opaque to AM; whatever
        // the plugin produced is persisted verbatim in
        // `tenant_idp_metadata` and replayed via
        // `TenantContext::metadata` on every subsequent IdP call.
        //
        // Size-only cap on the plugin-returned blob: same
        // `MAX_IDP_METADATA_BYTES` budget as the create-child saga.
        // An oversize blob propagates as `DomainError::Validation`
        // and the bootstrap caller (`finalize`) wraps the Err in
        // `compensate_step3_failure`, so vendor-side state is torn
        // down before the row is left for the reaper.
        crate::domain::tenant::service::check_idp_metadata_size(
            "bootstrap.idp_returned_metadata",
            idp_metadata,
        )?;
        // Persist the plugin-private metadata BEFORE `activate_tenant`
        // opens its SERIALIZABLE TX so the provisioning reaper can
        // recover vendor-side state even if the activation fails.
        // Mirrors the same fix on the create-child saga path. See
        // `tenant::service::create_tenant` for the rationale on why
        // the up-front upsert is load-bearing.
        //
        // # `?` on the upsert is safe here
        //
        // Unlike the create-child saga (where the `?` would skip the
        // local compensation rung and we have to call
        // `compensate_failed_activation` explicitly), the bootstrap
        // caller — `BootstrapService::finalize` lines 1208-1242 —
        // already wraps every `Err` from `handle_provision_success`
        // in `compensate_step3_failure`, which runs the bounded
        // `deprovision_tenant` + row-cleanup pipeline. A transient
        // DB error on this line therefore propagates to that
        // wrapper and the IdP-side teardown still runs. The
        // bootstrap caller needs the saga `deadline` (an `Instant`
        // not in scope here) to bound the IdP call, so inlining the
        // compensation at this site would either require a
        // signature change or skip the deadline guard.
        self.repo
            .upsert_idp_metadata(scope, root_id, idp_metadata)
            .await?;
        // Root tenant has no strict ancestors; closure rows
        // collapse to the self-row.
        let closure_rows = build_activation_rows(root_id, TenantStatus::Active, false, &[]);
        let activated = self
            .repo
            .activate_tenant(scope, root_id, &closure_rows, idp_metadata)
            .await?;
        emit_metric(
            AM_BOOTSTRAP_LIFECYCLE,
            MetricKind::Counter,
            &[
                ("phase", "completed"),
                ("classification", "fresh"),
                ("outcome", "success"),
            ],
        );
        info!(
            target: "am.bootstrap",
            root_id = %activated.id,
            classification = "fresh",
            "platform-bootstrap saga completed"
        );
        Ok(activated)
    }
    // @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-success-telemetry
    // @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-root-creation:p1:inst-dod-bootstrap-root-activate

    // @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-failure-telemetry
    async fn handle_provision_failure(
        &self,
        scope: &AccessScope,
        root_id: uuid::Uuid,
        failure: IdpProvisionFailure,
    ) -> DomainError {
        // Emit the same `phase=failed` metric for every terminal arm so
        // dashboards count failures symmetrically. The `classification`
        // label is the typed `IdpProvisionFailure::as_metric_label()` token
        // (avoids hand-rolled strings drifting between this site and
        // the saga's failure path in `tenant::service`).
        emit_metric(
            AM_BOOTSTRAP_LIFECYCLE,
            MetricKind::Counter,
            &[
                ("phase", "failed"),
                ("classification", failure.as_metric_label()),
                ("outcome", "failure"),
            ],
        );
        match failure {
            IdpProvisionFailure::CleanFailure { detail } => {
                self.compensate(scope, root_id, "clean-failure").await;
                // The provider `detail` is logged via the
                // `phase=failed` metric label above; the public
                // envelope wraps it in `IdpUnavailable` whose
                // canonical mapping redacts vendor strings before
                // they reach the public Problem body.
                DomainError::IdpUnavailable { detail }
            }
            IdpProvisionFailure::Ambiguous { detail } => {
                // Leave the provisioning row in place — the reaper
                // compensates per FEATURE §3 step 8.2. Redact the
                // provider detail before placing it in the
                // `Internal::diagnostic` field: that field is
                // exposed verbatim through `CanonicalError::internal`
                // to API callers, so a vendor stack trace or token-
                // bearing string would otherwise leak through. The
                // raw detail is logged via the `phase=failed`
                // counter's classification label and the `am.idp`
                // log channel.
                let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                warn!(
                    target: "am.idp",
                    classification = "ambiguous",
                    detail_digest = digest,
                    detail_len_chars = len,
                    "idp provision returned Ambiguous; provisioning row left for reaper"
                );
                DomainError::internal(format!(
                    "idp provision ambiguous outcome (detail digest: {digest:016x}, len: {len})"
                ))
            }
            IdpProvisionFailure::UnsupportedOperation { detail } => {
                self.compensate(scope, root_id, "unsupported").await;
                DomainError::UnsupportedOperation { detail }
            }
            other => {
                // SDK marks `IdpProvisionFailure` as `#[non_exhaustive]`;
                // any new variant added later that hits the bootstrap
                // path with no dedicated AM mapping surfaces as a
                // conservative `Internal` so a failure-mode review is
                // forced before the new variant goes live.
                // Format via `Debug` so the actual variant + payload
                // appear in the diagnostic — `type_name_of_val` only
                // prints the type name (`IdpProvisionFailure`), which is
                // useless for triage.
                self.compensate(scope, root_id, "unknown").await;
                DomainError::internal(format!(
                    "unknown IdpProvisionFailure variant in bootstrap saga: {other:?}"
                ))
            }
        }
    }
    // @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-failure-telemetry

    /// Best-effort compensation for the bootstrap saga. Compensation
    /// failure is intentionally swallowed (logged at warn-level, no
    /// error returned): the provisioning reaper
    /// (`algo-provisioning-reaper-compensation`) sweeps any row left
    /// behind on its next tick, so propagating the comp failure here
    /// would only surface a duplicate `Internal` to the caller without
    /// changing the eventual end state.
    async fn compensate(&self, scope: &AccessScope, root_id: uuid::Uuid, label: &str) {
        // `expected_claimed_by = None` selects the saga-compensation
        // fence (`claimed_by IS NULL`): bootstrap holds exclusive
        // ownership of the row up to the IdP call boundary, so the
        // provisioning reaper has not yet had an opportunity to claim
        // it. If the reaper *does* race in (RETENTION_CLAIM_TTL
        // expired during a slow IdP round-trip), the row is left for
        // the reaper to compensate on its own tick.
        if let Err(comp_err) = self
            .repo
            .compensate_provisioning(scope, root_id, None)
            .await
        {
            warn!(
                target: "am.bootstrap",
                error = %comp_err,
                label,
                "bootstrap compensation failed; deferring to reaper"
            );
        }
    }

    /// Compensation for a step-3 (`activate_tenant`) failure that
    /// landed AFTER the `IdP` successfully provisioned the tenant.
    /// Mirrors the child-create saga's step-3 handling, with the
    /// `idp_clean` gate that decides whether the local row deletion
    /// is safe:
    ///
    /// * `Ok(())` / `NotFound` / `UnsupportedOperation` — `IdP`-side
    ///   cleanup is confirmed (or trivially complete because the
    ///   plugin has nothing to manage). The local
    ///   [`Self::compensate`] runs and the row is removed.
    /// * `Terminal` / `Retryable` / unknown variant / deadline
    ///   timeout — the vendor tenant may still exist. Skip the
    ///   local compensation and leave the `Provisioning` row in
    ///   place so the provisioning reaper can take ownership on a
    ///   subsequent tick. Deleting the local row in that state
    ///   would orphan the vendor resource with no AM-side handle to
    ///   reconcile it.
    ///
    /// The deprovision attempt is bounded by the bootstrap deadline
    /// so a hung provider cannot stretch `module::init` past the
    /// configured wall-clock cap.
    #[allow(
        clippy::cognitive_complexity,
        reason = "best-effort step-3 compensation: each IdpDeprovisionFailure variant logs a distinct outcome (clean / unsupported_required / non-clean / timeout) and decides whether to run the local row-delete; collapsing the arms would obscure the per-variant contract that mirrors `TenantService::compensate_failed_activation`"
    )]
    async fn compensate_step3_failure(
        &self,
        scope: &AccessScope,
        root_id: uuid::Uuid,
        idp_metadata: Option<&Value>,
        deadline: Instant,
    ) {
        // Build the AM-internal `TenantContext` from the saga's
        // in-scope facts: `root_id`, the configured root name/type,
        // and whatever the plugin returned from `provision_tenant`
        // (we just got it in `finalize`). `cfg.root_tenant_type` is
        // the typed `GtsSchemaId` the saga already passed into the
        // provision call, so we forward the same value here without
        // re-parsing. Convert to the SDK `IdpTenantContext` at the
        // plugin-SPI boundary.
        let tenant_context = TenantContext::new(
            root_id,
            self.cfg.root_name.clone(),
            self.cfg.root_tenant_type.clone(),
            idp_metadata.cloned(),
        );
        let req = IdpDeprovisionTenantRequest::new(IdpTenantContext::from(&tenant_context));
        let idp_clean = match tokio::time::timeout_at(deadline, self.idp.deprovision_tenant(&req))
            .await
        {
            // `Ok(())` confirmed cleanup; `NotFound` is the
            // vendor-side already-absent success-equivalent per
            // the SDK doc — both are unconditionally safe.
            Ok(Ok(()) | Err(IdpDeprovisionFailure::NotFound { .. })) => true,
            Ok(Err(IdpDeprovisionFailure::UnsupportedOperation { .. })) => {
                // Symmetric with
                // `TenantService::compensate_failed_activation`:
                // `UnsupportedOperation` is only safe to treat as
                // "no IdP-side state retained" when the deployment
                // explicitly opted out of an `IdP` via
                // `cfg.idp.required = false` (the wired-in
                // `NoopIdpProvider` path). A real plugin returning
                // this variant under `idp.required = true` signals
                // that vendor-side state may exist but the plugin
                // can't deprovision it — deleting the local row
                // would orphan that state with no AM-side handle
                // to reconcile it. Defer to the reaper instead.
                if self.idp_required {
                    warn!(
                        target: "am.bootstrap",
                        outcome = "unsupported_required",
                        "bootstrap step-3 compensation: IdP plugin returned UnsupportedOperation but idp.required=true; refusing to orphan vendor-side state, leaving Provisioning row for the reaper"
                    );
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[
                            ("phase", "step3_compensation"),
                            ("classification", "unsupported_required"),
                            ("outcome", "deferred_to_reaper"),
                        ],
                    );
                    false
                } else {
                    true
                }
            }
            Ok(Err(deprov_err)) => {
                // Non-clean variant. `Ok` / `NotFound` /
                // `UnsupportedOperation` are caught by the prior
                // arms, so this only reaches `Terminal` /
                // `Retryable` / a future `#[non_exhaustive]`
                // variant. Redact the vendor detail before
                // logging so DSN / hostname / token strings do
                // not reach the `am.bootstrap` log channel —
                // mirrors the redaction policy applied in
                // `handle_provision_failure::Ambiguous`.
                let label = deprov_err.as_metric_label();
                let detail = match &deprov_err {
                    IdpDeprovisionFailure::Terminal { detail }
                    | IdpDeprovisionFailure::Retryable { detail } => detail.as_str(),
                    // SDK-side `as_metric_label` is itself an
                    // exhaustive `const fn`, so a new
                    // `IdpDeprovisionFailure` variant is a compile-
                    // time event — this wildcard is dead code
                    // today and only exists as forward-compat
                    // scaffolding for the rare case where a new
                    // variant lands without an SDK rebuild here
                    // (vendored fork, dependency-resolution
                    // skew). Pattern matches the
                    // `reaper::classify_deprovision` and
                    // `retention::process_single_hard_delete`
                    // wildcards.
                    #[allow(
                        unreachable_patterns,
                        reason = "IdpDeprovisionFailure is #[non_exhaustive]; the wildcard guards against future SDK variants"
                    )]
                    _ => "",
                };
                let (digest, len) = crate::domain::idp::redact_provider_detail(detail);
                warn!(
                    target: "am.bootstrap",
                    outcome = label,
                    detail_digest = digest,
                    detail_len_chars = len,
                    "bootstrap step-3 compensation: idp deprovision_tenant returned a non-clean failure; leaving Provisioning row for the reaper"
                );
                // Mirror the `AM_BOOTSTRAP_LIFECYCLE` counter
                // shape used elsewhere in the saga (phase /
                // classification / outcome): step-3 deprovision
                // failures are now observable on the same metric
                // family that already covers `idp_provisioning`,
                // `idp_waiting`, etc. — operators do not need a
                // separate dashboard to spot the case where the
                // saga left a Provisioning row behind because
                // the IdP returned a non-clean variant.
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "step3_compensation"),
                        ("classification", label),
                        ("outcome", "deferred_to_reaper"),
                    ],
                );
                false
            }
            Err(_elapsed) => {
                // Emit the `phase=step3_compensation` counter on this
                // arm too. The other two non-clean arms above already
                // do (`unsupported_required` + `<terminal/retryable>`),
                // so leaving the timeout arm silent makes the
                // "step-3 deferred to reaper" view on
                // `AM_BOOTSTRAP_LIFECYCLE` blind to the timeout case
                // and operators can't disambiguate "vendor down" from
                // "vendor terminal" without parsing the warn-log.
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "step3_compensation"),
                        ("classification", "timeout"),
                        ("outcome", "deferred_to_reaper"),
                    ],
                );
                warn!(
                    target: "am.bootstrap",
                    "bootstrap step-3 compensation: idp deprovision_tenant exceeded the bootstrap deadline; leaving Provisioning row for the reaper"
                );
                false
            }
        };
        if idp_clean {
            self.compensate(scope, root_id, "step3-failure").await;
        }
    }

    /// Synchronous in-band compensation for a stuck `Provisioning`
    /// root observed at bootstrap-classify time. Mirrors the body of
    /// `TenantService::reap_stuck_provisioning` for a single row:
    /// load the tenant's plugin-private metadata, build a
    /// [`TenantContext`], call
    /// [`account_management_sdk::IdpPluginClient::deprovision_tenant`],
    /// and on confirmed cleanup
    /// ([`IdpDeprovisionFailure::NotFound`] or `Ok`) issue
    /// `TenantRepo::compensate_provisioning` to drop the local row.
    ///
    /// Symmetric with [`Self::compensate_step3_failure`] for the
    /// `UnsupportedOperation` arm: when `idp.required = false` (the
    /// `NoopIdpProvider` fallback) the variant means "no plugin
    /// wired, nothing to clean" and the local row may be deleted;
    /// when `idp.required = true` it means the real plugin can't
    /// deprovision a tenant it provisioned and the row MUST be left
    /// for the reaper to avoid orphaning vendor-side state.
    ///
    /// # Observability
    ///
    /// Mirrors [`Self::compensate_step3_failure`]: every per-variant
    /// outcome is a distinct
    /// `am.bootstrap.lifecycle{phase=stuck_recovery,
    /// classification=<variant>, outcome=<compensated|deferred_to_reaper>}`
    /// counter sample plus a redacted warn line on `am.bootstrap`.
    /// Operators triaging a deferred-to-reaper terminal can
    /// distinguish `terminal`/`retryable`/`unsupported_required`/`timeout`
    /// without reading the plugin's own logs. The caller does NOT
    /// re-emit a metric — observability is owned end-to-end here.
    ///
    /// # Errors
    ///
    /// Returns `Ok(())` when the row was successfully compensated
    /// (next classify will see `NoRoot`); `Err(_)` when the `IdP`
    /// did not confirm cleanup or compensation failed (the caller
    /// falls through to the existing `deferred_to_reaper` terminal).
    /// Bounded by `deadline` so a hung provider cannot stretch the
    /// saga past its wall-clock cap.
    #[allow(
        clippy::cognitive_complexity,
        reason = "flat dispatch over five IdP-deprovision outcomes; splitting hides the per-branch metric label + redacted warn log that mirrors `compensate_step3_failure`"
    )]
    async fn attempt_stuck_row_compensation(
        &self,
        row: &TenantModel,
        deadline: Instant,
    ) -> Result<(), DomainError> {
        let scope = AccessScope::allow_all();
        let metadata = self.repo.find_idp_metadata(&scope, row.id).await?;
        let tenant_context = TenantContext::new(
            row.id,
            row.name.clone(),
            self.cfg.root_tenant_type.clone(),
            metadata,
        );
        let req = IdpDeprovisionTenantRequest::new(IdpTenantContext::from(&tenant_context));
        let idp_clean = match tokio::time::timeout_at(deadline, self.idp.deprovision_tenant(&req))
            .await
        {
            // `Ok(())` confirmed teardown; `NotFound` is the
            // vendor-side already-absent success-equivalent per
            // the SDK doc — both are unconditionally safe.
            Ok(Ok(())) => {
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "stuck_recovery"),
                        ("classification", "ok"),
                        ("outcome", "compensated"),
                    ],
                );
                true
            }
            Ok(Err(IdpDeprovisionFailure::NotFound { .. })) => {
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "stuck_recovery"),
                        ("classification", "already_absent"),
                        ("outcome", "compensated"),
                    ],
                );
                true
            }
            Ok(Err(IdpDeprovisionFailure::UnsupportedOperation { .. })) => {
                if self.idp_required {
                    // Same posture as `compensate_step3_failure`:
                    // a real plugin returning `UnsupportedOperation`
                    // under `idp.required = true` cannot deprovision
                    // a tenant it provisioned. Leaving the local row
                    // in place forces operator intervention before
                    // the reaper can take over.
                    warn!(
                        target: "am.bootstrap",
                        root_id = %row.id,
                        outcome = "unsupported_required",
                        "stuck-row in-band compensation: IdP plugin returned UnsupportedOperation \
                         but idp.required=true; refusing to orphan vendor-side state, leaving \
                         Provisioning row for the reaper"
                    );
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[
                            ("phase", "stuck_recovery"),
                            ("classification", "unsupported_required"),
                            ("outcome", "deferred_to_reaper"),
                        ],
                    );
                    false
                } else {
                    emit_metric(
                        AM_BOOTSTRAP_LIFECYCLE,
                        MetricKind::Counter,
                        &[
                            ("phase", "stuck_recovery"),
                            ("classification", "unsupported_noop"),
                            ("outcome", "compensated"),
                        ],
                    );
                    true
                }
            }
            Ok(Err(deprov_err)) => {
                // Non-clean variant. `Ok` / `NotFound` /
                // `UnsupportedOperation` are caught by the prior
                // arms, so this only reaches `Terminal` /
                // `Retryable` / a future `#[non_exhaustive]`
                // variant. Mirror the redaction policy applied in
                // `compensate_step3_failure`: log digest + length
                // (never raw vendor `detail`) and label the metric
                // with the SDK-defined variant token.
                let label = deprov_err.as_metric_label();
                let detail = match &deprov_err {
                    IdpDeprovisionFailure::Terminal { detail }
                    | IdpDeprovisionFailure::Retryable { detail } => detail.as_str(),
                    #[allow(
                        unreachable_patterns,
                        reason = "IdpDeprovisionFailure is #[non_exhaustive]; the wildcard guards against future SDK variants"
                    )]
                    _ => "",
                };
                let (digest, len) = crate::domain::idp::redact_provider_detail(detail);
                warn!(
                    target: "am.bootstrap",
                    root_id = %row.id,
                    outcome = label,
                    detail_digest = digest,
                    detail_len_chars = len,
                    "stuck-row in-band compensation: idp deprovision_tenant returned a non-clean \
                     failure; leaving Provisioning row for the reaper"
                );
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "stuck_recovery"),
                        ("classification", label),
                        ("outcome", "deferred_to_reaper"),
                    ],
                );
                false
            }
            Err(_elapsed) => {
                // `tokio::time::timeout_at` only stops local
                // waiting; it does NOT prove the deprovision call
                // never reached the IdP. Treat as ambiguous: leave
                // the row for the reaper rather than risk a
                // false-clean compensation that orphans
                // vendor-side state.
                warn!(
                    target: "am.bootstrap",
                    root_id = %row.id,
                    "stuck-row in-band compensation: idp deprovision_tenant exceeded the bootstrap \
                     deadline; leaving Provisioning row for the reaper"
                );
                emit_metric(
                    AM_BOOTSTRAP_LIFECYCLE,
                    MetricKind::Counter,
                    &[
                        ("phase", "stuck_recovery"),
                        ("classification", "timeout"),
                        ("outcome", "deferred_to_reaper"),
                    ],
                );
                false
            }
        };
        if !idp_clean {
            return Err(DomainError::IdpUnavailable {
                detail: "stuck-row in-band compensation: IdP did not confirm cleanup".to_owned(),
            });
        }
        self.repo
            .compensate_provisioning(&scope, row.id, None)
            .await
    }
}

/// Idempotent skip path — emit the completed-skipped lifecycle metric
/// + info log, then return the existing active root.
// @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-idempotency:p1:inst-dod-bootstrap-skip-existing
// @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-skip-telemetry
fn handle_skip(root: TenantModel) -> TenantModel {
    emit_metric(
        AM_BOOTSTRAP_LIFECYCLE,
        MetricKind::Counter,
        &[
            ("phase", "completed"),
            ("classification", "skipped"),
            ("outcome", "success"),
        ],
    );
    info!(
        target: "am.bootstrap",
        root_id = %root.id,
        classification = "skipped",
        "platform-bootstrap saga skipped: root tenant already active"
    );
    root
}
// @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-skip-telemetry
// @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-idempotency:p1:inst-dod-bootstrap-skip-existing

// @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-idempotency:p1:inst-dod-bootstrap-defer-reaper
// @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-defer-telemetry
/// Surface a `Provisioning` root older than `2 × idp_wait_timeout`
/// (the FEATURE-§3 stuck threshold) as a non-success signal: the
/// previous attempt crashed mid-saga, the provisioning reaper will
/// compensate, and module init is NOT complete. Returning an error
/// (rather than `Ok(_)` with the still-Provisioning model) lets the
/// strict-mode init gate in `module::run_bootstrap_phase` decide
/// whether to abort or proceed without an active root.
fn handle_deferred_to_reaper_stuck(root: &TenantModel) -> DomainError {
    emit_metric(
        AM_BOOTSTRAP_LIFECYCLE,
        MetricKind::Counter,
        &[
            ("phase", "failed"),
            ("classification", "deferred_to_reaper"),
            ("outcome", "failure"),
        ],
    );
    warn!(
        target: "am.bootstrap",
        root_id = %root.id,
        classification = "deferred_to_reaper",
        "platform-bootstrap found stuck provisioning root; reaper will compensate, init not complete"
    );
    DomainError::internal(format!(
        "platform-bootstrap deferred to reaper for stuck provisioning root {} (created_at={}); init not complete",
        root.id, root.created_at
    ))
}
// @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-defer-telemetry
// @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-idempotency:p1:inst-dod-bootstrap-defer-reaper

/// Cap `requested` at the time remaining until `deadline` so the
/// caller never sleeps past its own budget. Returns
/// `Duration::ZERO` if the deadline has already elapsed; the caller
/// is expected to check the deadline immediately after the sleep
/// and surface a timeout.
fn bounded_sleep(requested: Duration, deadline: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(Instant::now());
    requested.min(remaining)
}

// @cpt-begin:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-invariant-telemetry
fn handle_invariant_violation(observed_status: TenantStatus) -> DomainError {
    emit_metric(
        AM_BOOTSTRAP_LIFECYCLE,
        MetricKind::Counter,
        &[
            ("phase", "failed"),
            ("classification", "invariant_violation"),
            ("outcome", "failure"),
        ],
    );
    DomainError::internal(format!(
        "bootstrap invariant violation: root tenant in unexpected state {observed_status:?}"
    ))
}
// @cpt-end:cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics:p1:inst-dod-bootstrap-invariant-telemetry

/// Extract `allowed_parent_types` from a flat effective-traits map
/// (the shape returned by `GtsTypeSchema::effective_traits`), which
/// keys traits directly rather than nesting them under
/// `x-gts-traits` / `allOf`.
fn extract_allowed_parent_types_from_effective(
    effective: &serde_json::Value,
) -> Result<Vec<String>, DomainError> {
    let Some(value) = effective.get("allowed_parent_types") else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(DomainError::InvalidTenantType {
            detail: "allowed_parent_types trait must be an array".to_owned(),
        });
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| DomainError::InvalidTenantType {
                    detail: "allowed_parent_types trait must contain only strings".to_owned(),
                })
        })
        .collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
