//! Reconcile explicit documents by reading current content and submitting differences.
//! Transport retries reuse the key and request; new passes re-read and use new keys.
//! Dependencies, conflicts and call refusals stay pending; item refusals are terminal.
//! Permanent submission refusals stop the call; hinted `ResourceExhausted` waits under the
//! deadline. Success requires all identifiers admitted. Matching content does not confirm the
//! publisher version.

use std::collections::{BTreeMap, HashMap};
use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use gts::GtsId;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use toolkit_security::PlatformSecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

use crate::contract::PlatformTypesRegistryApi;
use crate::error::TypesRegistryError;
use crate::gts::{OperationResource, TypeResource};
use crate::item_failure::{AdmissionFailure, AdmissionFailureReason as Reason};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetItem, EntityField, EntityKey, EntityLookup, FieldSelection,
    IdempotencyKey, JsonDocument, LifecycleStatus, MAX_BATCH_GET_KEYS, Origin, Projection,
    PublisherContext, RegisterEntitiesRequest, RegisterItem, RegistrationOutcome,
};

pub mod submit;
use submit::{Failure, Stop, await_registration, bounded, deadline_after, jittered};

mod telemetry;
use telemetry::{Run, StopReason, error_fields, operation_id_of};

/// Retryable transport failure limit per batch, excluding hinted throttling.
const TRANSPORT_ATTEMPTS: u32 = 3;

/// Initial transport retry pause; doubles with jitter under the call deadline.
const TRANSPORT_RETRY_BACKOFF: Duration = Duration::from_millis(100);

/// Reconciliation tuning; initialize with Default, then adjust fields.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReconcileOptions {
    /// Maximum candidates per submission; must fit the registry's limit, or the call stops.
    pub batch_size: NonZeroUsize,
    /// Read–compare–submit passes in one call.
    pub passes: NonZeroU32,
    /// Initial pass backoff; clamp to [`Self::retry_backoff_max`], then double with jitter.
    pub retry_backoff: Duration,
    /// Longest pause between two passes.
    pub retry_backoff_max: Duration,
    /// Budget of the whole call, every pass and poll included.
    pub deadline: Duration,
}

impl Default for ReconcileOptions {
    fn default() -> Self {
        Self {
            batch_size: NonZeroUsize::new(100).unwrap_or(NonZeroUsize::MIN),
            passes: NonZeroU32::new(5).unwrap_or(NonZeroU32::MIN),
            retry_backoff: Duration::from_millis(200),
            retry_backoff_max: Duration::from_secs(5),
            deadline: Duration::from_secs(60),
        }
    }
}

/// Successful reconciliation: all desired identifiers are admitted.
#[derive(Debug, Clone)]
pub enum Reconciliation {
    /// Every desired document already matched; nothing was submitted.
    UpToDate,
    /// All outcomes are [`ReconcileOutcome::Admitted`] after submitting work.
    Reconciled(HashMap<GtsId, ReconcileOutcome>),
}

mod report;
pub use report::ReconcileReport;

/// Reconciliation stop reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReconcileStop {
    /// No identifiers remain to submit.
    Completed,
    /// Pass limit reached with pending identifiers.
    PassesExhausted,
    /// Whole-call deadline reached.
    Deadline,
    /// Entity read failed.
    ReadFailed,
    /// A permanent submission refusal; no further batches or passes run.
    InvalidInput,
}

impl ReconcileStop {
    /// Parse a wire stop label; unknown labels return `None`.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "completed" => Some(Self::Completed),
            "passes_exhausted" => Some(Self::PassesExhausted),
            "deadline" => Some(Self::Deadline),
            "read_failed" => Some(Self::ReadFailed),
            "invalid_input" => Some(Self::InvalidInput),
            _ => None,
        }
    }

    /// Stable `reconcile_finished.stop` label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::PassesExhausted => "passes_exhausted",
            Self::Deadline => "deadline",
            Self::ReadFailed => "read_failed",
            Self::InvalidInput => "invalid_input",
        }
    }
}

/// One desired identifier's outcome.
#[derive(Debug, Clone)]
pub enum ReconcileOutcome {
    /// Registered, or already present with the desired content.
    Admitted,
    /// Terminal refusal; decode registry reasons with [`AdmissionFailure::from_canonical`].
    Rejected(CanonicalError),
    /// Not settled by this call.
    Pending(ReconcilePendingCause),
}

/// Why an identifier is still pending.
#[derive(Debug, Clone)]
pub enum ReconcilePendingCause {
    /// Something it depends on is not registered yet.
    Dependency(CanonicalError),
    /// A concurrent writer changed it; the next pass re-reads.
    Conflict(CanonicalError),
    /// The registry could not be reached or did not answer in time.
    Unavailable(CanonicalError),
    /// Call-level refusal (e.g. auth/permission); this call does not submit it again, a
    /// later one may once its cause is fixed.
    Refused(CanonicalError),
    /// Unjudged after invalid input; used only when no earlier outcome is known.
    Skipped,
}

impl ReconcilePendingCause {
    /// Transport/availability failure maps to Unavailable; other call failures to Refused.
    pub(crate) fn of(error: CanonicalError) -> Self {
        if transport_failure(&error) {
            Self::Unavailable(error)
        } else {
            Self::Refused(error)
        }
    }
}

impl ReconcileOutcome {
    /// Whether another call could change this outcome.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        !matches!(self, Self::Pending(_))
    }
}

/// Reconcile desired documents, tracing the call and each submitted batch.
pub async fn reconcile<'d, A, I>(
    api: &A,
    ctx: &PlatformSecurityContext,
    publisher: &PublisherContext,
    desired: I,
    options: &ReconcileOptions,
    cancel: &CancellationToken,
) -> Result<Reconciliation, CanonicalError>
where
    A: PlatformTypesRegistryApi + ?Sized,
    I: IntoIterator<Item = &'d (GtsId, JsonDocument)>,
{
    // The whole-call budget starts now, before any preparation.
    let started = Instant::now();
    let deadline = deadline_after(started, options.deadline);
    let span = tracing::info_span!(
        "reconcile",
        reconcile_id = %Uuid::new_v4(),
        publisher = %publisher.name,
        publisher_version = %publisher.version,
        inputs = tracing::field::Empty,
        desired = tracing::field::Empty,
        passes = options.passes.get(),
        batch_size = options.batch_size.get(),
        deadline_ms = u64::try_from(options.deadline.as_millis()).unwrap_or(u64::MAX),
    );
    let (wanted, outcomes) = span.in_scope(|| validate(desired));
    let prepared = Prepared {
        started,
        deadline,
        wanted,
        outcomes,
    };
    run(api, ctx, publisher, cancel, options, prepared)
        .instrument(span)
        .await
}

/// A call's input, validated, and its budget.
struct Prepared<'d> {
    started: Instant,
    deadline: Result<Instant, CanonicalError>,
    wanted: Wanted<'d>,
    outcomes: Outcomes,
}

async fn run<A: PlatformTypesRegistryApi + ?Sized>(
    api: &A,
    ctx: &PlatformSecurityContext,
    publisher: &PublisherContext,
    cancel: &CancellationToken,
    options: &ReconcileOptions,
    prepared: Prepared<'_>,
) -> Result<Reconciliation, CanonicalError> {
    // The guard reports the call's end, whichever way it ends, dropped included.
    let desired = prepared.wanted.len() + prepared.outcomes.len();
    let mut run = Run::new(prepared.started, desired, prepared.outcomes);
    let mut wanted = prepared.wanted;
    tracing::info!(log_kind = "reconcile_started", "reconciliation started");
    let deadline = match prepared.deadline {
        Ok(deadline) => deadline,
        Err(error) => {
            run.stop(StopReason::InvalidDeadline);
            return Err(error);
        }
    };
    let mut call = Call {
        api,
        ctx,
        publisher,
        deadline,
        cancel,
        batches: 0,
    };
    let mut backoff = options.retry_backoff.min(options.retry_backoff_max);
    let mut stop = ReconcileStop::PassesExhausted;
    for pass in 1..=options.passes.get() {
        run.passes = pass;
        let end = if pass > 1
            && let Err(stopped) = pause(jittered(backoff), deadline, cancel).await
        {
            stopped_between_passes(stopped, &wanted, &mut run)
        } else {
            call.pass(pass, options.batch_size.get(), &mut wanted, &mut run)
                .await
        };
        // Each later pause doubles, up to the maximum.
        if pass > 1 {
            backoff = backoff.saturating_mul(2).min(options.retry_backoff_max);
        }
        match end {
            PassEnd::Next => {}
            PassEnd::Stop(reason) => {
                stop = reason;
                break;
            }
            PassEnd::Cancelled => {
                run.stop(StopReason::Cancelled);
                return Err(Stop::Cancelled.into_error());
            }
        }
    }
    // The last pass ran out of time rather than of passes: the local budget, not a remote
    // `DeadlineExceeded` answered while it still had some.
    if stop == ReconcileStop::PassesExhausted && Instant::now() >= deadline {
        stop = ReconcileStop::Deadline;
    }
    conclude(run, &wanted, call.batches > 0, stop)
}

/// The pause before a later pass, ended by the caller or the deadline.
fn stopped_between_passes(stopped: Stop, wanted: &Wanted<'_>, run: &mut Run) -> PassEnd {
    match stopped {
        Stop::Cancelled => PassEnd::Cancelled,
        Stop::Deadline(_) => {
            let cause = ReconcilePendingCause::Unavailable(stopped.into_error());
            mark_unsettled(wanted, &mut run.outcomes, &cause);
            PassEnd::Stop(ReconcileStop::Deadline)
        }
    }
}

/// How one pass ended.
enum PassEnd {
    Next,
    Stop(ReconcileStop),
    Cancelled,
}

/// Fill missing outcomes and return success only if all identifiers are admitted.
fn conclude(
    mut run: Run,
    wanted: &Wanted<'_>,
    submitted: bool,
    stop: ReconcileStop,
) -> Result<Reconciliation, CanonicalError> {
    // Whatever stopped the passes, nothing desired leaves without an outcome.
    for (key, (id, _)) in wanted {
        run.outcomes.entry(key.clone()).or_insert_with(|| {
            (
                id.clone(),
                ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(protocol_error(
                    "reconciliation ended before this identifier was decided",
                ))),
            )
        });
    }
    let admitted = run
        .outcomes
        .values()
        .all(|(_, o)| matches!(o, ReconcileOutcome::Admitted));
    if admitted && !submitted {
        run.finish(StopReason::UpToDate);
        return Ok(Reconciliation::UpToDate);
    }
    let outcomes = run.finish(stop.into()).into_values().collect();
    if admitted {
        Ok(Reconciliation::Reconciled(outcomes))
    } else {
        Err(ReconcileReport { stop, outcomes }.into_canonical()?)
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

struct Call<'c, A: ?Sized> {
    api: &'c A,
    ctx: &'c PlatformSecurityContext,
    publisher: &'c PublisherContext,
    deadline: Instant,
    cancel: &'c CancellationToken,
    /// Payloads submitted so far; numbers each `reconcile_batch` span.
    batches: u32,
}

/// A pause of `backoff`, cut short by the deadline or the caller's cancellation.
async fn pause(
    backoff: Duration,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<(), Stop> {
    let wake = Instant::now()
        .checked_add(backoff)
        .map_or(deadline, |wake| wake.min(deadline));
    bounded(deadline, cancel, None, sleep_until(wake)).await
}

/// Settle active content matches immediately; return only remaining candidates, avoiding
/// resubmission/downgrade.
fn compare(
    wanted: &mut Wanted<'_>,
    outcomes: &mut Outcomes,
    current: &HashMap<String, Current>,
) -> Vec<RegisterItem> {
    let mut candidates = Vec::new();
    wanted.retain(|key, (id, content)| match current.get(key) {
        Some(found) if found.lifecycle == LifecycleStatus::Active && found.content == **content => {
            outcomes.insert(key.clone(), (id.clone(), ReconcileOutcome::Admitted));
            false
        }
        found => {
            candidates.push(RegisterItem {
                gts_id: id.clone(),
                content: (*content).clone(),
                expected_resource_version: found.map(|f| f.resource_version),
                skip_compatibility_check: false,
            });
            true
        }
    });
    candidates
}

/// Record decisions and remove the identifiers this call will not submit again.
fn record(
    decided: impl IntoIterator<Item = (GtsId, ReconcileOutcome)>,
    wanted: &mut Wanted<'_>,
    outcomes: &mut Outcomes,
) {
    for (id, outcome) in decided {
        let key = id.id().to_owned();
        // Request refusals need a later call after their cause is fixed.
        if outcome.is_settled()
            || matches!(
                outcome,
                ReconcileOutcome::Pending(ReconcilePendingCause::Refused(_))
            )
        {
            wanted.remove(&key);
        }
        outcomes.insert(key, (id, outcome));
    }
}

/// Desired documents not yet settled, by canonical identifier: submission order is stable.
type Wanted<'d> = BTreeMap<String, (GtsId, &'d JsonDocument)>;

/// Outcomes so far, by canonical identifier.
pub type Outcomes = BTreeMap<String, (GtsId, ReconcileOutcome)>;

struct Current {
    content: JsonDocument,
    resource_version: u64,
    lifecycle: LifecycleStatus,
}

/// Collapse identical duplicates and reject conflicting ones before transport. A `GtsId` is
/// valid and canonical by construction, so nothing else is checked here.
fn validate<'d>(
    desired: impl IntoIterator<Item = &'d (GtsId, JsonDocument)>,
) -> (Wanted<'d>, Outcomes) {
    let mut wanted: Wanted<'d> = BTreeMap::new();
    let mut outcomes = Outcomes::new();
    let mut conflicting = Vec::new();
    let mut inputs = 0_usize;
    for (id, content) in desired {
        inputs += 1;
        match wanted.get(id.id()) {
            Some((_, existing)) if *existing != content => conflicting.push(id.clone()),
            Some(_) => {}
            None => {
                wanted.insert(id.id().to_owned(), (id.clone(), content));
            }
        }
    }
    for id in conflicting {
        wanted.remove(id.id());
        let error = TypeResource::invalid_argument()
            .with_field_violation(
                crate::field::GTS_ID_FIELD,
                format!("'{id}' is declared twice with different documents"),
                crate::field::VALIDATION_FAILED,
            )
            .create();
        outcomes.insert(id.id().to_owned(), (id, ReconcileOutcome::Rejected(error)));
    }
    let span = tracing::Span::current();
    span.record("inputs", inputs);
    span.record("desired", wanted.len() + outcomes.len());
    (wanted, outcomes)
}

/// Every identifier still wanted is unsettled; record the latest cause.
fn mark_unsettled(wanted: &Wanted<'_>, outcomes: &mut Outcomes, cause: &ReconcilePendingCause) {
    for (key, (id, _)) in wanted {
        outcomes.insert(
            key.clone(),
            (id.clone(), ReconcileOutcome::Pending(cause.clone())),
        );
    }
}

impl<A: PlatformTypesRegistryApi + ?Sized> Call<'_, A> {
    /// One read–compare–submit pass; outcomes are recorded in `run` as they are decided.
    async fn pass(
        &mut self,
        pass: u32,
        batch_size: usize,
        wanted: &mut Wanted<'_>,
        run: &mut Run,
    ) -> PassEnd {
        if self.cancel.is_cancelled() {
            return PassEnd::Cancelled;
        }
        // Only the caller's token ends the call; a `Cancelled` from the registry is an answer.
        let current = match self.read(wanted.values().map(|(id, _)| id)).await {
            Ok(Ok(current)) => current,
            Err(Stop::Cancelled) => return PassEnd::Cancelled,
            Err(stop @ Stop::Deadline(_)) => {
                let cause = ReconcilePendingCause::of(stop.into_error());
                mark_unsettled(wanted, &mut run.outcomes, &cause);
                return PassEnd::Stop(ReconcileStop::Deadline);
            }
            Ok(Err(e)) => {
                mark_unsettled(wanted, &mut run.outcomes, &ReconcilePendingCause::of(e));
                return PassEnd::Stop(ReconcileStop::ReadFailed);
            }
        };
        let candidates = compare(wanted, &mut run.outcomes, &current);
        if candidates.is_empty() {
            wanted.clear();
            return PassEnd::Stop(ReconcileStop::Completed);
        }
        if let Err(end) = self
            .submit_all(candidates, batch_size, pass, wanted, run)
            .await
        {
            return end;
        }
        if wanted.is_empty() {
            PassEnd::Stop(ReconcileStop::Completed)
        } else {
            PassEnd::Next
        }
    }

    /// Reads `ids` in bounded batches, selecting `content` and `origin`. The outer `Err` is
    /// this call's own stop, the inner one the registry's failure or a protocol fault.
    async fn read<'a>(
        &self,
        ids: impl Iterator<Item = &'a GtsId>,
    ) -> Result<Result<HashMap<String, Current>, CanonicalError>, Stop> {
        let projection = Projection::Select(FieldSelection::with(&[
            EntityField::Content,
            EntityField::Origin,
        ]));
        let ids: Vec<&GtsId> = ids.collect();
        let mut current = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(MAX_BATCH_GET_KEYS) {
            let request = BatchGetEntitiesRequest {
                items: chunk
                    .iter()
                    .map(|id| BatchGetItem::from(EntityKey::GtsId((*id).clone())))
                    .collect(),
                projection: projection.clone(),
                fresh: true,
            };
            let mut lookups = match bounded(
                self.deadline,
                self.cancel,
                None,
                self.api.batch_get_entities(self.ctx, request),
            )
            .await?
            {
                Ok(lookups) => lookups,
                Err(error) => return Ok(Err(error)),
            };
            // Only explicit NotFound permits creation; missing, Unchanged, or incomplete Found is a
            // protocol fault.
            for id in chunk {
                let key = EntityKey::GtsId((*id).clone());
                match lookups.0.remove(&key) {
                    Some(EntityLookup::NotFound) => {}
                    Some(EntityLookup::Found {
                        entity: snapshot, ..
                    }) => {
                        let (
                            Some(Origin::Managed {
                                resource_version, ..
                            }),
                            Some(content),
                        ) = (snapshot.origin, snapshot.content)
                        else {
                            return Ok(Err(incomplete_read(
                                id,
                                "a found entity lacks a selected field",
                            )));
                        };
                        current.insert(
                            id.id().to_owned(),
                            Current {
                                content,
                                resource_version,
                                lifecycle: snapshot.lifecycle_status,
                            },
                        );
                    }
                    Some(EntityLookup::Unchanged { .. }) => {
                        return Ok(Err(incomplete_read(
                            id,
                            "an unconditional read answered unchanged",
                        )));
                    }
                    None => {
                        return Ok(Err(incomplete_read(id, "the read did not answer this key")));
                    }
                }
            }
        }
        Ok(Ok(current))
    }

    /// Submit batches, preserving each result. Stop on cancellation or invalid input.
    async fn submit_all(
        &mut self,
        candidates: Vec<RegisterItem>,
        batch_size: usize,
        pass: u32,
        wanted: &mut Wanted<'_>,
        run: &mut Run,
    ) -> Result<(), PassEnd> {
        let mut rest = candidates.into_iter();
        loop {
            let batch: Vec<RegisterItem> = rest.by_ref().take(batch_size).collect();
            if batch.is_empty() {
                return Ok(());
            }
            let key = IdempotencyKey::generate();
            self.batches += 1;
            let span = tracing::info_span!(
                "reconcile_batch",
                batch = self.batches,
                pass,
                size = batch.len(),
                idempotency_key = key.as_str(),
                attempt = tracing::field::Empty,
                operation_id = tracing::field::Empty,
            );
            let submitted = self.submit_batch(key, batch).instrument(span.clone()).await;
            // Synchronously in the batch's span: its refusal events carry its fields.
            let Decision { decided, invalid } = span
                .in_scope(|| decide(submitted))
                .map_err(|_| PassEnd::Cancelled)?;
            record(decided, wanted, &mut run.outcomes);
            if let Some(unjudged) = invalid {
                skip(unjudged.into_iter().chain(rest), &mut run.outcomes);
                return Err(PassEnd::Stop(ReconcileStop::InvalidInput));
            }
        }
    }

    /// Submit one payload under `key`; transport retries preserve it and use jittered pauses
    /// under the deadline. Runs inside the batch's span.
    async fn submit_batch(&self, key: IdempotencyKey, items: Vec<RegisterItem>) -> Submitted {
        let request = RegisterEntitiesRequest {
            items,
            dry_run: false,
            publisher: self.publisher.clone(),
        };
        // Before anything is sent: the key reaches an INFO log even if no answer ever does.
        tracing::info!(log_kind = "batch_submitted", "submitting a batch");
        let mut backoff = TRANSPORT_RETRY_BACKOFF;
        let mut attempt = 0;
        let mut transport_failures = 0;
        loop {
            attempt += 1;
            tracing::Span::current().record("attempt", attempt);
            let (error, retry_after) = match await_registration(
                self.api,
                self.ctx,
                key.clone(),
                request.clone(),
                self.deadline,
                self.cancel,
            )
            .await
            {
                Ok(Ok(operation)) => {
                    return Submitted::Decided(cover(&request.items, operation.items));
                }
                Ok(Err(Failure::Submit(error))) => {
                    if permanent(&error) {
                        return Submitted::Refused(error, request.items);
                    }
                    let retry_after = throttle_hint(&error);
                    (error, retry_after)
                }
                Ok(Err(failure)) => (failure.into_error(), None),
                Err(Stop::Cancelled) => return Submitted::Cancelled,
                Err(stop @ Stop::Deadline(_)) => (stop.into_error(), None),
            };
            if let Some(operation_id) = operation_id_of(&error) {
                tracing::Span::current()
                    .record("operation_id", tracing::field::display(operation_id));
            }
            if retry_after.is_none() && !retryable(&error) {
                return Submitted::Unavailable(error, request.items);
            }
            let fields = error_fields(&error);
            if retry_after.is_none() {
                transport_failures += 1;
                if transport_failures >= TRANSPORT_ATTEMPTS {
                    telemetry::transport_exhausted(transport_failures, &fields);
                    return Submitted::Unavailable(error, request.items);
                }
            }
            let wait = jittered(backoff).max(retry_after.unwrap_or_default());
            telemetry::transport_retry(&fields, millis(wait));
            match pause(wait, self.deadline, self.cancel).await {
                Ok(()) => {}
                Err(Stop::Deadline(_)) => return Submitted::Unavailable(error, request.items),
                Err(Stop::Cancelled) => return Submitted::Cancelled,
            }
            backoff = backoff.saturating_mul(2);
        }
    }
}

/// Submission refusals that repeating the same request cannot cure, by canonical category.
fn permanent(error: &CanonicalError) -> bool {
    match error {
        CanonicalError::InvalidArgument { .. }
        | CanonicalError::FailedPrecondition { .. }
        | CanonicalError::Unauthenticated { .. }
        | CanonicalError::PermissionDenied { .. }
        | CanonicalError::NotFound { .. }
        | CanonicalError::AlreadyExists { .. }
        | CanonicalError::OutOfRange { .. }
        | CanonicalError::Cancelled { .. } => true,
        CanonicalError::ResourceExhausted { .. } => throttle_hint(error).is_none(),
        _ => false,
    }
}

/// The longest quota `retry_after_seconds` of a `ResourceExhausted`: throttling to wait out.
fn throttle_hint(error: &CanonicalError) -> Option<Duration> {
    let CanonicalError::ResourceExhausted { ctx, .. } = error else {
        return None;
    };
    ctx.violations
        .iter()
        .filter_map(|v| v.retry_after_seconds)
        .max()
        .map(Duration::from_secs)
}

struct Decision {
    decided: Vec<(GtsId, ReconcileOutcome)>,
    /// On invalid input, contains unjudged candidates and stops submission.
    invalid: Option<Vec<RegisterItem>>,
}

/// Mark unjudged candidates skipped without overwriting known outcomes.
fn skip(unjudged: impl IntoIterator<Item = RegisterItem>, outcomes: &mut Outcomes) {
    for candidate in unjudged {
        outcomes
            .entry(candidate.gts_id.id().to_owned())
            .or_insert_with(|| {
                (
                    candidate.gts_id,
                    ReconcileOutcome::Pending(ReconcilePendingCause::Skipped),
                )
            });
    }
}

/// Classify results; cancellation returns `Err`.
fn decide(submitted: Submitted) -> Result<Decision, Stop> {
    Ok(match submitted {
        Submitted::Decided(decided) => Decision {
            decided,
            invalid: None,
        },
        Submitted::Cancelled => return Err(Stop::Cancelled),
        Submitted::Unavailable(error, batch) => Decision {
            decided: pending(batch, &ReconcilePendingCause::of(error)),
            invalid: None,
        },
        Submitted::Refused(error, batch) => refused(error, batch),
    })
}

fn pending(
    batch: Vec<RegisterItem>,
    cause: &ReconcilePendingCause,
) -> Vec<(GtsId, ReconcileOutcome)> {
    batch
        .into_iter()
        .map(|c| (c.gts_id, ReconcileOutcome::Pending(cause.clone())))
        .collect()
}

/// Reject a named candidate, or keep the request error pending; either way the call stops.
fn refused(error: CanonicalError, mut batch: Vec<RegisterItem>) -> Decision {
    let fields = error_fields(&error);
    if let Some(index) = refused_candidate(&error, &batch) {
        let refused = batch.remove(index);
        telemetry::candidate_refused(&refused.gts_id, batch.len(), &fields);
        return Decision {
            decided: vec![(refused.gts_id, ReconcileOutcome::Rejected(error))],
            invalid: Some(batch),
        };
    }
    telemetry::batch_refused(&fields);
    Decision {
        decided: pending(batch, &ReconcilePendingCause::Refused(error)),
        invalid: Some(Vec::new()),
    }
}

fn incomplete_read(id: &GtsId, why: &str) -> CanonicalError {
    tracing::warn!(
        log_kind = "incomplete_read",
        gts_id = %id,
        why,
        "registry read answered incompletely; nothing is submitted"
    );
    protocol_error(format!(
        "the registry answered a reconciliation read of '{id}' incompletely: {why}"
    ))
}

fn protocol_error(detail: impl Into<String>) -> CanonicalError {
    OperationResource::aborted(detail)
        .with_reason(crate::reason::aborted::RECONCILIATION_PROTOCOL_ERROR)
        .create()
}

/// The request fields that belong to one candidate.
const CANDIDATE_FIELDS: [&str; 4] = [
    crate::field::GTS_ID_FIELD,
    crate::field::ENTITY_FIELD,
    crate::field::EXPECTED_RESOURCE_VERSION_FIELD,
    crate::field::SKIP_COMPATIBILITY_CHECK_FIELD,
];

/// Identify a candidate's validation or policy refusal; others refuse the whole request.
fn refused_candidate(error: &CanonicalError, batch: &[RegisterItem]) -> Option<usize> {
    if let TypesRegistryError::PolicyRefused { gts_id, .. } =
        TypesRegistryError::from(error.clone())
    {
        return batch.iter().position(|c| c.gts_id.id() == gts_id);
    }
    let CanonicalError::InvalidArgument {
        ctx: InvalidArgument::FieldViolations { field_violations },
        resource_name: Some(name),
        ..
    } = error
    else {
        return None;
    };
    let own = !field_violations.is_empty()
        && field_violations
            .iter()
            .all(|v| CANDIDATE_FIELDS.contains(&v.field.as_str()));
    if !own {
        return None;
    }
    batch.iter().position(|c| c.gts_id.id() == name)
}

enum Submitted {
    Decided(Vec<(GtsId, ReconcileOutcome)>),
    /// A permanent submission refusal with its batch.
    Refused(CanonicalError, Vec<RegisterItem>),
    /// Undecided call: unavailable, refused or expired; returns the batch.
    Unavailable(CanonicalError, Vec<RegisterItem>),
    /// The caller's token stopped the submission.
    Cancelled,
}

/// Require exactly one outcome per submitted candidate; missing/duplicates stay pending, extras are
/// ignored.
fn cover(
    submitted: &[RegisterItem],
    items: Vec<crate::models::RegistrationItemResult>,
) -> Vec<(GtsId, ReconcileOutcome)> {
    let mut reported: HashMap<String, Vec<ReconcileOutcome>> = HashMap::new();
    for item in items {
        reported
            .entry(item.gts_id.id().to_owned())
            .or_default()
            .push(classify(item.outcome));
    }
    submitted
        .iter()
        .map(|candidate| {
            let outcome = match reported.remove(candidate.gts_id.id()) {
                Some(mut outcomes) if outcomes.len() == 1 => outcomes.remove(0),
                found => {
                    tracing::warn!(
                        log_kind = "item_unreported",
                        gts_id = %candidate.gts_id,
                        reported = found.map_or(0, |o| o.len()),
                        "a completed operation did not report this candidate exactly once"
                    );
                    ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(
                        protocol_error(
                            "the registry's operation did not report a submitted candidate exactly once",
                        ),
                    ))
                }
            };
            (candidate.gts_id.clone(), outcome)
        })
        .collect()
}

/// Transport/registry availability failures retain [`ReconcilePendingCause::Unavailable`].
fn transport_failure(error: &CanonicalError) -> bool {
    retryable(error)
        || matches!(
            error,
            CanonicalError::DeadlineExceeded { .. } | CanonicalError::ResourceExhausted { .. }
        )
}

/// Transport failures worth repeating under the same key and request.
fn retryable(error: &CanonicalError) -> bool {
    matches!(
        error,
        CanonicalError::Aborted { .. }
            | CanonicalError::ServiceUnavailable { .. }
            | CanonicalError::Internal { .. }
            | CanonicalError::Unknown { .. }
    )
}

/// Classify candidate outcomes by admission-failure reason.
fn classify(outcome: RegistrationOutcome) -> ReconcileOutcome {
    match outcome {
        RegistrationOutcome::Succeeded { .. } | RegistrationOutcome::Unchanged { .. } => {
            ReconcileOutcome::Admitted
        }
        RegistrationOutcome::Pending | RegistrationOutcome::Running => {
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(protocol_error(
                "a completed operation left a candidate undecided",
            )))
        }
        // Reconciliation never submits a dry run: a preview is not a write.
        RegistrationOutcome::WouldSucceed => {
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(protocol_error(
                "the registry answered a committing registration with a dry-run preview",
            )))
        }
        RegistrationOutcome::Failed { error } => {
            let Some(failure) = AdmissionFailure::from_canonical(&error) else {
                return ReconcileOutcome::Rejected(error);
            };
            match failure.reason {
                Reason::DependencyNotFound
                | Reason::BlockedByDependency
                | Reason::BlockedByPredecessor
                | Reason::MissingPredecessor => {
                    ReconcileOutcome::Pending(ReconcilePendingCause::Dependency(error))
                }
                // Contention with concurrent writers: re-read and resubmit next pass.
                Reason::AlreadyExists | Reason::PreconditionFailed | Reason::RevalidationExhausted => {
                    ReconcileOutcome::Pending(ReconcilePendingCause::Conflict(error))
                }
                Reason::SystemFailure => {
                    ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(error))
                }
                Reason::ActivationWriteSetExceeded
                | Reason::BaselineUnresolvable
                | Reason::CompatibilityUndecidable
                | Reason::DependencyDeleted
                | Reason::DependentInvalid
                | Reason::DialectChanged
                | Reason::EntityDeleted
                | Reason::FamilyKindConflict
                | Reason::HasRegisteredDependents
                | Reason::FamilyShapeConflict
                | Reason::IncompatibleWithBaseline
                | Reason::InstanceOfMajorZero
                | Reason::InvalidDocument
                | Reason::InvalidIdentifier
                | Reason::InvalidSchema
                | Reason::InvalidValue
                | Reason::NotActive
                | Reason::PublisherMismatch
                | Reason::ResolutionClosureExceeded
                | Reason::ResolvedDocumentTooLarge
                | Reason::StableDerivesFromMajorZero
                | Reason::StableRefsMajorZero
                | Reason::Superseded
                | Reason::UnparsablePayload
                | Reason::UnreadableVersion
                | Reason::UnrecognizedPayload
                // A reason this build does not know is not retried on a guess.
                | Reason::Unknown(_) => ReconcileOutcome::Rejected(error),
            }
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;

#[cfg(test)]
mod telemetry_tests;

#[cfg(test)]
mod telemetry_capture;

#[cfg(test)]
mod report_tests;
