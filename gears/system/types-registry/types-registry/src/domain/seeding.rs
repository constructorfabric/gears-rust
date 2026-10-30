//! Admit inventory and configured seeds before publishing the database APIs
//! (T31, SPEC D6/D11/§8.4).
//!
//! Reconcile through [`LocalClient`] with CAS, batches bounded by
//! `limits.batch_candidates`, and retries for contention or later dependencies.
//! Equal active documents retain revisions; omitted documents are not deleted.
//! One monotonic deadline bounds the seed set.

use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    AdmissionFailure, GtsId, PlatformTypesRegistryApiExt as _, PublisherContext,
    PublisherVersionError, ReconcileOptions, ReconcileOutcome, ReconcilePendingCause,
    ReconcileReport, ReconcileStop, Reconciliation,
};

use crate::domain::admission::fingerprint::canonical_text;
use crate::domain::local_client::LocalClient;
use crate::domain::registry_service::RegistryService;

/// The `gts://` prefix of a Type Schema's `$id`.
const GTS_ID_URI_PREFIX: &str = "gts://";

/// First pause between two passes; each later one doubles up to the maximum.
const BACKOFF_INITIAL: Duration = Duration::from_millis(50);
const BACKOFF_MAX: Duration = Duration::from_secs(1);

/// The registry's own seed documents, by canonical identifier.
#[domain_model]
#[derive(Clone, Debug, Default)]
pub struct SeedSet {
    documents: BTreeMap<String, Value>,
}

impl SeedSet {
    /// Identify each document by the first of `id_fields` it carries, as the
    /// legacy catalogue does. Identical duplicates collapse; differing ones refuse,
    /// byte-level distinctions such as `-0.0` against `0.0` included.
    ///
    /// # Errors
    /// [`SeedError::MissingIdentifier`] or [`SeedError::ConflictingDeclarations`].
    pub fn collect(
        documents: impl IntoIterator<Item = Value>,
        id_fields: &[String],
    ) -> Result<Self, SeedError> {
        let mut set: BTreeMap<String, Value> = BTreeMap::new();
        for (index, document) in documents.into_iter().enumerate() {
            let gts_id =
                identifier(&document, id_fields).ok_or_else(|| SeedError::MissingIdentifier {
                    index,
                    fields: id_fields.to_vec(),
                })?;
            match set.get(&gts_id) {
                Some(existing) if canonical_text(existing) != canonical_text(&document) => {
                    return Err(SeedError::ConflictingDeclarations { gts_id });
                }
                Some(_) => {}
                None => {
                    set.insert(gts_id, document);
                }
            }
        }
        Ok(Self { documents: set })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// The reconciliation input; an identifier that is not a canonical GTS
    /// identifier refuses the whole set.
    fn desired(&self) -> Result<Vec<(GtsId, Value)>, SeedError> {
        self.documents
            .iter()
            .map(|(gts_id, document)| match GtsId::try_new(gts_id) {
                Ok(id) if id.id() == gts_id => Ok((id, document.clone())),
                Ok(_) => Err(SeedError::InvalidIdentifier {
                    gts_id: gts_id.clone(),
                    cause: "not in canonical form".to_owned(),
                }),
                Err(error) => Err(SeedError::InvalidIdentifier {
                    gts_id: gts_id.clone(),
                    cause: error.to_string(),
                }),
            })
            .collect()
    }
}

fn identifier(document: &Value, id_fields: &[String]) -> Option<String> {
    id_fields.iter().find_map(|field| {
        let id = document.get(field)?.as_str()?;
        let id = if field == "$id" {
            id.strip_prefix(GTS_ID_URI_PREFIX).unwrap_or(id)
        } else {
            id
        };
        Some(id.to_owned())
    })
}

/// One seed that did not reach the database, and why.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedFailure {
    pub gts_id: String,
    /// A diagnostic code, not one vocabulary: the admission failure reason's
    /// wire code when the outcome carries one, otherwise the outcome's kind —
    /// `rejected`, `dependency`, `conflict`, `unavailable`, `refused`,
    /// `skipped` — or `deadline_exceeded` when no report named the seed.
    pub reason: String,
    pub message: String,
}

impl std::fmt::Display for SeedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.gts_id, self.reason, self.message)
    }
}

fn list(failures: &[SeedFailure]) -> String {
    failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Why seeding did not admit the whole set. Every variant fails boot.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("seed document #{index} names no identifier in any of {fields:?}")]
    MissingIdentifier { index: usize, fields: Vec<String> },
    #[error("seed '{gts_id}' is declared twice with different documents")]
    ConflictingDeclarations { gts_id: String },
    #[error("seed '{gts_id}' is not a valid GTS identifier: {cause}")]
    InvalidIdentifier { gts_id: String, cause: String },
    #[error("{} seed(s) were not admitted: {}", .0.len(), list(.0))]
    Rejected(Vec<SeedFailure>),
    #[error("seeding did not finish within {budget:?}; undecided: {}", list(.undecided))]
    Deadline {
        budget: Duration,
        undecided: Vec<SeedFailure>,
    },
    #[error("seeding stopped ({}) with {} seed(s) unsettled: {}", .stop.as_str(), .failures.len(), list(.failures))]
    Unsettled {
        stop: ReconcileStop,
        failures: Vec<SeedFailure>,
    },
    #[error("seeding was cancelled")]
    Cancelled,
    #[error("the seed budget {budget:?} is too large to schedule")]
    BudgetTooLarge { budget: Duration },
    #[error("the registry's own publisher version is refused: {0}")]
    PublisherVersion(#[from] PublisherVersionError),
    #[error("the registry failed while seeding: {0}")]
    Registry(#[source] CanonicalError),
}

/// What a successful seed did.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedSummary {
    pub documents: usize,
    /// Every seed was already stored, active and equal: nothing was submitted.
    pub up_to_date: bool,
}

/// Admit all of `set` within one `budget`, checked before, during and after
/// reconciliation. Expiry or cancellation always fails, naming unadmitted seeds.
///
/// # Errors
/// See [`SeedError`].
pub async fn seed(
    registry: &Arc<RegistryService>,
    set: &SeedSet,
    budget: Duration,
    cancel: &CancellationToken,
) -> Result<SeedSummary, SeedError> {
    let deadline = Instant::now()
        .checked_add(budget)
        .ok_or(SeedError::BudgetTooLarge { budget })?;
    let desired = set.desired()?;
    let publisher = PublisherContext {
        name: "types-registry".to_owned(),
        version: env!("CARGO_PKG_VERSION").parse()?,
    };
    let client = LocalClient::new(Arc::clone(registry));
    // The SDK takes a relative deadline and starts its own clock a moment later; the
    // guard holds the call to this absolute one, whoever is ready first.
    let ctx = PlatformSecurityContext::outbound_marker();
    let options = options(registry, deadline);
    let call = client.reconcile_entities_and_await(&ctx, &publisher, &desired, &options, cancel);
    let reconciled = match guarded(deadline, cancel, call).await {
        Ok(reconciled) => reconciled,
        Err(Stop::Cancelled) => return Err(SeedError::Cancelled),
        Err(Stop::Deadline) => return Err(expired(budget, set)),
    };
    match reconciled {
        Ok(reconciliation) => {
            confirm(deadline, budget, set, cancel)?;
            Ok(SeedSummary {
                documents: set.len(),
                up_to_date: matches!(reconciliation, Reconciliation::UpToDate),
            })
        }
        Err(_) if cancel.is_cancelled() => Err(SeedError::Cancelled),
        Err(error) => Err(refusal(error, budget)),
    }
}

/// What ended a guarded call before its own answer.
#[domain_model]
#[derive(Debug, PartialEq, Eq)]
enum Stop {
    Cancelled,
    Deadline,
}

/// Poll `call` under the absolute seed deadline and cancellation token, checking
/// both first on every poll, even when `call` is ready.
///
/// Dropping reconciliation abandons waits, not accepted writes: the durable outbox
/// may process or redeliver them after its partition lease. APIs remain unpublished
/// until `seed` succeeds; the next boot reconciles again.
async fn guarded<F: Future>(
    deadline: Instant,
    cancel: &CancellationToken,
    call: F,
) -> Result<F::Output, Stop> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Stop::Cancelled),
        () = sleep_until(deadline) => Err(Stop::Deadline),
        output = call => Ok(output),
    }
}

/// Fit batches to the registry limit. Allow unbounded passes for later-batch
/// dependencies and lost CAS, bounded by the seed deadline. Since the set is
/// complete, stop when a pass admits nothing and only dependencies remain.
fn options(registry: &RegistryService, deadline: Instant) -> ReconcileOptions {
    let mut options = ReconcileOptions::default();
    options.batch_size =
        NonZeroUsize::new(registry.limits().batch_candidates).unwrap_or(NonZeroUsize::MIN);
    options.passes = NonZeroU32::MAX;
    options.stop_when_stalled = true;
    options.retry_backoff = BACKOFF_INITIAL;
    options.retry_backoff_max = BACKOFF_MAX;
    options.deadline = deadline.saturating_duration_since(Instant::now());
    options
}

/// Success only while the budget still holds: the reconciliation may finish without
/// any bounded wait (an empty or already equal set), or after the deadline passed.
fn confirm(
    deadline: Instant,
    budget: Duration,
    set: &SeedSet,
    cancel: &CancellationToken,
) -> Result<(), SeedError> {
    if cancel.is_cancelled() {
        return Err(SeedError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(expired(budget, set));
    }
    Ok(())
}

/// The deadline error when no reconciliation report names the undecided seeds.
fn expired(budget: Duration, set: &SeedSet) -> SeedError {
    SeedError::Deadline {
        budget,
        undecided: set
            .documents
            .keys()
            .map(|gts_id| SeedFailure {
                gts_id: gts_id.clone(),
                reason: "deadline_exceeded".to_owned(),
                message: "not confirmed before the seed deadline; it may still be admitted, \
                          and the next boot reconciles it"
                    .to_owned(),
            })
            .collect(),
    }
}

/// The seed error for a failed reconciliation, naming every seed not admitted.
fn refusal(error: CanonicalError, budget: Duration) -> SeedError {
    let Some(report) = ReconcileReport::from_canonical(&error) else {
        return SeedError::Registry(error);
    };
    let rejected = report
        .outcomes
        .values()
        .any(|outcome| matches!(outcome, ReconcileOutcome::Rejected(_)));
    let mut failures: Vec<SeedFailure> = report
        .outcomes
        .iter()
        .filter_map(|(id, outcome)| failure(id, outcome))
        .collect();
    failures.sort_by(|left, right| left.gts_id.cmp(&right.gts_id));
    match report.stop {
        ReconcileStop::Deadline => SeedError::Deadline {
            budget,
            undecided: failures,
        },
        _ if rejected => SeedError::Rejected(failures),
        stop => SeedError::Unsettled { stop, failures },
    }
}

fn failure(id: &GtsId, outcome: &ReconcileOutcome) -> Option<SeedFailure> {
    let (kind, error) = match outcome {
        ReconcileOutcome::Admitted => return None,
        ReconcileOutcome::Rejected(error) => ("rejected", Some(error)),
        ReconcileOutcome::Pending(cause) => match cause {
            ReconcilePendingCause::Dependency(error) => ("dependency", Some(error)),
            ReconcilePendingCause::Conflict(error) => ("conflict", Some(error)),
            ReconcilePendingCause::Unavailable(error) => ("unavailable", Some(error)),
            ReconcilePendingCause::Refused(error) => ("refused", Some(error)),
            ReconcilePendingCause::Skipped => ("skipped", None),
        },
    };
    let (reason, message) = match error {
        Some(error) => match AdmissionFailure::from_canonical(error) {
            Some(failure) => (failure.reason.as_wire().to_owned(), failure.message),
            None => (kind.to_owned(), error.to_string()),
        },
        None => (kind.to_owned(), "not judged".to_owned()),
    };
    Some(SeedFailure {
        gts_id: id.id().to_owned(),
        reason,
        message,
    })
}

#[cfg(test)]
#[path = "seeding_tests.rs"]
mod seeding_tests;
