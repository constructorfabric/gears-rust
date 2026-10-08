//! Reconcile explicit desired documents; no inventory discovery or deletion (SPEC §10.1).
//! Read unsettled content, settle active matches, submit differences with observed preconditions.
//! Transport retries reuse key/request; new passes re-read and use new keys (ADR-0012).
//! Dependencies, conflicts and call refusals stay pending; item refusals are terminal.
//! Until T41, equal content cannot confirm a newer publisher version (D18).

use std::collections::{BTreeMap, HashMap};
use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use gts::GtsId;
use tokio_util::sync::CancellationToken;
use toolkit::tokio::time::{Instant, sleep_until};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::contract::PlatformTypesRegistryApi;
use crate::ext::{MAX_BATCH_GET_KEYS, await_registration, bounded, deadline_from_now, jittered};
use crate::gts::TypeResource;
use crate::item_failure::{AdmissionFailure, AdmissionFailureReason as Reason};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetItem, CandidateStatus, EntityField, EntityKey, EntityLookup,
    FieldSelection, IdempotencyKey, JsonDocument, LifecycleStatus, Origin, Projection,
    PublisherContext, RegisterEntitiesRequest, RegisterItem,
};

/// How a submitted batch is retried at the transport level, key and request unchanged.
const TRANSPORT_ATTEMPTS: u32 = 3;

/// Initial transport retry pause; doubles with jitter under the call deadline.
const TRANSPORT_RETRY_BACKOFF: Duration = Duration::from_millis(100);

/// Reconciliation tuning; initialize with Default, then adjust fields.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReconcileOptions {
    /// Candidates per submission; oversized synchronous refusals are bisected to meet registry
    /// limits.
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

/// What one call achieved.
#[derive(Debug, Clone)]
pub enum Reconciliation {
    /// Every desired document already matched; nothing was submitted.
    UpToDate,
    /// Per-identifier outcomes when work was submitted or remains undecided; keys retain caller
    /// spelling.
    Reconciled(BTreeMap<String, ReconcileOutcome>),
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
    /// Call-level refusal (e.g. auth/permission); retry after its cause is fixed.
    Refused(CanonicalError),
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

/// Reconcile desired documents under `publisher`; callers reach it through
/// `PlatformTypesRegistryApiExt::reconcile_entities_and_await`, which documents it.
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
    I: IntoIterator<Item = &'d (String, JsonDocument)>,
{
    let call = Call {
        api,
        ctx,
        publisher,
        deadline: deadline_from_now(options.deadline)?,
        cancel,
    };
    let (mut wanted, mut outcomes) = validate(desired);
    let mut submitted = false;
    let mut backoff = options.retry_backoff.min(options.retry_backoff_max);

    for pass in 0..options.passes.get() {
        if cancel.is_cancelled() {
            return Err(TypeResource::cancelled().create());
        }
        if pass > 0 {
            let wait = jittered(backoff);
            tracing::debug!(
                gear = %publisher.name,
                pass,
                unsettled = wanted.len(),
                backoff = ?wait,
                "reconciliation retries the unsettled identifiers"
            );
            if let Err(e) = pause(wait, call.deadline, cancel).await? {
                mark_unsettled(
                    &wanted,
                    &mut outcomes,
                    &ReconcilePendingCause::Unavailable(e),
                );
                break;
            }
            backoff = backoff.saturating_mul(2).min(options.retry_backoff_max);
        }

        let current = match call.read(wanted.values().map(|(id, _)| id)).await {
            Ok(current) => current,
            Err(e) if matches!(e, CanonicalError::Cancelled { .. }) => return Err(e),
            Err(e) => {
                mark_unsettled(&wanted, &mut outcomes, &ReconcilePendingCause::of(e));
                break;
            }
        };

        let candidates = compare(&mut wanted, &mut outcomes, &current);
        if candidates.is_empty() {
            wanted.clear();
            break;
        }
        submitted = true;

        let decided = call
            .submit_all(candidates, options.batch_size.get())
            .await?;
        record(decided, &mut wanted, &mut outcomes);
        if wanted.is_empty() {
            break;
        }
    }

    // Whatever stopped the passes, nothing desired leaves without an outcome.
    for key in wanted.keys() {
        outcomes.entry(key.clone()).or_insert_with(|| {
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(
                CanonicalError::internal("reconciliation ended before this identifier was decided")
                    .create(),
            ))
        });
    }
    if !submitted
        && outcomes
            .values()
            .all(|o| matches!(o, ReconcileOutcome::Admitted))
    {
        return Ok(Reconciliation::UpToDate);
    }
    Ok(Reconciliation::Reconciled(outcomes))
}

struct Call<'c, A: ?Sized> {
    api: &'c A,
    ctx: &'c PlatformSecurityContext,
    publisher: &'c PublisherContext,
    deadline: Instant,
    cancel: &'c CancellationToken,
}

/// Deadline-bounded pause: inner Err means expiry, outer Err cancellation.
async fn pause(
    backoff: Duration,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Result<(), CanonicalError>, CanonicalError> {
    let wake = Instant::now()
        .checked_add(backoff)
        .map_or(deadline, |wake| wake.min(deadline));
    match bounded(deadline, cancel, None, sleep_until(wake)).await {
        Ok(()) => Ok(Ok(())),
        Err(e) if matches!(e, CanonicalError::Cancelled { .. }) => Err(e),
        Err(e) => Ok(Err(e)),
    }
}

/// Settle active content matches immediately; return only remaining candidates, avoiding
/// resubmission/downgrade.
fn compare(
    wanted: &mut Wanted<'_>,
    outcomes: &mut BTreeMap<String, ReconcileOutcome>,
    current: &HashMap<String, Current>,
) -> Vec<RegisterItem> {
    let mut candidates = Vec::new();
    wanted.retain(|key, (id, content)| match current.get(key) {
        Some(found) if found.lifecycle == LifecycleStatus::Active && found.content == **content => {
            outcomes.insert(key.clone(), ReconcileOutcome::Admitted);
            false
        }
        found => {
            candidates.push(RegisterItem {
                gts_id: id.clone(),
                content: (*content).clone(),
                expected_resource_version: found.map(|f| f.resource_version),
                force: false,
            });
            true
        }
    });
    candidates
}

/// Record decisions and remove settled identifiers.
fn record(
    decided: Vec<(GtsId, ReconcileOutcome)>,
    wanted: &mut Wanted<'_>,
    outcomes: &mut BTreeMap<String, ReconcileOutcome>,
) {
    for (id, outcome) in decided {
        let key = id.id().to_owned();
        if outcome.is_settled() {
            wanted.remove(&key);
        }
        outcomes.insert(key, outcome);
    }
}

type Wanted<'d> = BTreeMap<String, (GtsId, &'d JsonDocument)>;

struct Current {
    content: JsonDocument,
    resource_version: u64,
    lifecycle: LifecycleStatus,
}

/// Reject invalid identifiers and conflicting duplicates before transport.
fn validate<'d>(
    desired: impl IntoIterator<Item = &'d (String, JsonDocument)>,
) -> (Wanted<'d>, BTreeMap<String, ReconcileOutcome>) {
    let mut wanted: Wanted<'d> = BTreeMap::new();
    let mut outcomes = BTreeMap::new();
    let mut conflicting = Vec::new();
    for (raw, content) in desired {
        // Require canonical spelling (SPEC §8.1): try_new trims, which would change response keys.
        let parsed = GtsId::try_new(raw)
            .map_err(|e| e.to_string())
            .and_then(|id| {
                if id.id() == raw {
                    Ok(id)
                } else {
                    Err("not the canonical spelling".to_owned())
                }
            });
        let id = match parsed {
            Ok(id) => id,
            Err(why) => {
                let error = TypeResource::invalid_argument()
                    .with_field_violation(
                        crate::field::GTS_ID_FIELD,
                        format!("'{raw}': {why}"),
                        crate::field::INVALID_GTS_ID,
                    )
                    .create();
                outcomes.insert(raw.clone(), ReconcileOutcome::Rejected(error));
                continue;
            }
        };
        match wanted.get(id.id()) {
            Some((_, existing)) if *existing != content => conflicting.push(id.id().to_owned()),
            Some(_) => {}
            None => {
                wanted.insert(id.id().to_owned(), (id, content));
            }
        }
    }
    for id in conflicting {
        wanted.remove(&id);
        let error = TypeResource::invalid_argument()
            .with_field_violation(
                crate::field::GTS_ID_FIELD,
                format!("'{id}' is declared twice with different documents"),
                crate::field::VALIDATION_FAILED,
            )
            .create();
        outcomes.insert(id, ReconcileOutcome::Rejected(error));
    }
    (wanted, outcomes)
}

/// Every identifier still wanted is unsettled; record the latest cause.
fn mark_unsettled(
    wanted: &Wanted<'_>,
    outcomes: &mut BTreeMap<String, ReconcileOutcome>,
    cause: &ReconcilePendingCause,
) {
    for key in wanted.keys() {
        outcomes.insert(key.clone(), ReconcileOutcome::Pending(cause.clone()));
    }
}

impl<A: PlatformTypesRegistryApi + ?Sized> Call<'_, A> {
    /// Reads `ids` in bounded batches, selecting `content` and `origin`.
    async fn read<'a>(
        &self,
        ids: impl Iterator<Item = &'a GtsId>,
    ) -> Result<HashMap<String, Current>, CanonicalError> {
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
            let mut lookups = bounded(
                self.deadline,
                self.cancel,
                None,
                self.api.batch_get_entities(self.ctx, request),
            )
            .await??;
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
                            return Err(incomplete_read(
                                id,
                                "a found entity lacks a selected field",
                            ));
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
                        return Err(incomplete_read(
                            id,
                            "an unconditional read answered unchanged",
                        ));
                    }
                    None => return Err(incomplete_read(id, "the read did not answer this key")),
                }
            }
        }
        Ok(current)
    }

    /// Submit bounded batches, bisect synchronous refusals, and return every candidate’s outcome.
    async fn submit_all(
        &self,
        candidates: Vec<RegisterItem>,
        batch_size: usize,
    ) -> Result<Vec<(GtsId, ReconcileOutcome)>, CanonicalError> {
        let mut decided = Vec::with_capacity(candidates.len());
        let mut queue: Vec<Vec<RegisterItem>> = Vec::new();
        let mut rest = candidates.into_iter().peekable();
        while rest.peek().is_some() {
            queue.push(rest.by_ref().take(batch_size).collect());
        }
        queue.reverse();
        while let Some(batch) = queue.pop() {
            match self.submit_batch(batch).await {
                Submitted::Decided(outcomes) => decided.extend(outcomes),
                Submitted::Refused(error, mut batch) if batch.len() > 1 => {
                    tracing::debug!(size = batch.len(), %error, "registry refused a batch; bisecting it");
                    let right = batch.split_off(batch.len() >> 1);
                    queue.push(right);
                    queue.push(batch);
                }
                Submitted::Refused(error, batch) => {
                    decided.extend(
                        batch
                            .into_iter()
                            .map(|c| (c.gts_id, ReconcileOutcome::Rejected(error.clone()))),
                    );
                }
                Submitted::Unavailable(error, batch) => {
                    if matches!(error, CanonicalError::Cancelled { .. }) {
                        return Err(error);
                    }
                    let cause = ReconcilePendingCause::of(error);
                    decided.extend(
                        batch
                            .into_iter()
                            .map(|c| (c.gts_id, ReconcileOutcome::Pending(cause.clone()))),
                    );
                }
            }
        }
        Ok(decided)
    }

    /// New key per batch; transport retries preserve it and use jittered pauses under the deadline.
    async fn submit_batch(&self, items: Vec<RegisterItem>) -> Submitted {
        let key = IdempotencyKey::generate();
        let request = RegisterEntitiesRequest {
            items,
            dry_run: false,
            publisher: self.publisher.clone(),
        };
        let mut backoff = TRANSPORT_RETRY_BACKOFF;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let error = match await_registration(
                self.api,
                self.ctx,
                key.clone(),
                request.clone(),
                self.deadline,
                self.cancel,
            )
            .await
            {
                Ok(operation) => {
                    return Submitted::Decided(cover(&request.items, operation.items));
                }
                Err(e @ CanonicalError::InvalidArgument { .. }) => {
                    return Submitted::Refused(e, request.items);
                }
                Err(e) => e,
            };
            if !retryable(&error) {
                return Submitted::Unavailable(error, request.items);
            }
            if attempt >= TRANSPORT_ATTEMPTS {
                tracing::warn!(
                    gear = %self.publisher.name,
                    attempts = attempt,
                    size = request.items.len(),
                    %error,
                    "a submission failed every transport attempt; it stays pending"
                );
                return Submitted::Unavailable(error, request.items);
            }
            let wait = jittered(backoff);
            tracing::debug!(attempt, %error, backoff = ?wait, "retrying a submission under its key");
            match pause(wait, self.deadline, self.cancel).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Submitted::Unavailable(error, request.items),
                Err(cancelled) => return Submitted::Unavailable(cancelled, request.items),
            }
            backoff = backoff.saturating_mul(2);
        }
    }
}

fn incomplete_read(id: &GtsId, why: &str) -> CanonicalError {
    tracing::warn!(gts_id = %id, why, "registry read answered incompletely; nothing is submitted");
    CanonicalError::internal(format!(
        "the registry answered a reconciliation read of '{id}' incompletely: {why}"
    ))
    .create()
}

enum Submitted {
    Decided(Vec<(GtsId, ReconcileOutcome)>),
    /// A synchronous refusal of the whole batch, which is handed back.
    Refused(CanonicalError, Vec<RegisterItem>),
    /// Undecided call: unavailable, refused, expired or cancelled; returns the batch.
    Unavailable(CanonicalError, Vec<RegisterItem>),
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
            .push(classify(item.status, item.error));
    }
    submitted
        .iter()
        .map(|candidate| {
            let outcome = match reported.remove(candidate.gts_id.id()) {
                Some(mut outcomes) if outcomes.len() == 1 => outcomes.remove(0),
                found => {
                    tracing::warn!(
                        gts_id = %candidate.gts_id,
                        reported = found.map_or(0, |o| o.len()),
                        "a completed operation did not report this candidate exactly once"
                    );
                    ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(
                        CanonicalError::internal(
                            "the registry's operation did not report a submitted candidate exactly once",
                        )
                        .create(),
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

/// The outcome policy, keyed by the exact reason (SPEC §10.1).
fn classify(status: CandidateStatus, error: Option<CanonicalError>) -> ReconcileOutcome {
    match status {
        CandidateStatus::Succeeded | CandidateStatus::Unchanged => ReconcileOutcome::Admitted,
        CandidateStatus::Pending | CandidateStatus::Running => {
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(
                CanonicalError::internal("a completed operation left a candidate undecided")
                    .create(),
            ))
        }
        CandidateStatus::Failed => {
            let error = error.unwrap_or_else(|| {
                CanonicalError::internal("a failed candidate carried no error").create()
            });
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
                // Revalidation exhaustion is contention with concurrent writers, not a verdict
                // on the document (SPEC §8.4): the next pass re-reads and resubmits.
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
#[path = "reconcile_tests.rs"]
mod reconcile_tests;
