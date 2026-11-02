//! Await a gear's startup publications with shared tuning and diagnostics.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::reconcile::error_fields;
use crate::{
    EntityKind, GtsId, JsonDocument, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
    PublisherContext, ReconcileOptions, ReconcileOutcome, ReconcilePendingCause, ReconcileReport,
    ReconcileStop, Reconciliation, STARTUP_PUBLICATION_DEADLINE,
};

/// Reconcile a gear's documents and await admission before binding dependent clients.
///
/// Uses the platform outbound marker and [`STARTUP_PUBLICATION_DEADLINE`], with the
/// other [`ReconcileOptions`] defaults. `Ok` means every identifier is admitted or
/// already up to date. The publisher's name and version belong to the calling gear.
///
/// Runs in the current task, without spawning or changing lifecycle phases. Call only
/// when the registry client is available: a remote client is wired after `init`.
/// For custom tuning or a different security context, use
/// [`PlatformTypesRegistryApiExt::reconcile_entities_and_await`] directly.
///
/// # Errors
/// Returns [`PublicationError`] on refusal, incomplete admission, deadline or
/// cancellation, preserving the original canonical error and any reconciliation report.
/// Cancellation or dropping the future does not roll back accepted writes.
pub async fn publish_startup_entities(
    registry: &dyn PlatformTypesRegistryApi,
    publisher: &PublisherContext,
    desired: &[(GtsId, JsonDocument)],
    cancel: &CancellationToken,
) -> Result<Reconciliation, PublicationError> {
    let options = ReconcileOptions {
        deadline: STARTUP_PUBLICATION_DEADLINE,
        ..ReconcileOptions::default()
    };
    registry
        .reconcile_entities_and_await(
            &PlatformSecurityContext::outbound_marker(),
            publisher,
            desired,
            &options,
            cancel,
        )
        .await
        .map_err(|error| PublicationError::new(publisher, desired, error))
}

/// Pauses between [`publish_until_admitted`] attempts: `initial`, doubling up to `max`.
#[derive(Debug, Clone, Copy)]
pub struct PublicationRetry {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for PublicationRetry {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

impl PublicationRetry {
    /// The pause before each retry, in order.
    pub(crate) fn pauses(self) -> impl Iterator<Item = Duration> {
        std::iter::successors(Some(self.initial.min(self.max)), move |pause| {
            Some(pause.saturating_mul(2).min(self.max))
        })
    }
}

/// [`publish_startup_entities`] again and again, until every identifier is admitted
/// or an attempt fails for good.
///
/// An attempt failing with a [transient](PublicationError::is_transient) error is
/// reported to `on_retry` and repeated after a pause; each attempt has the whole
/// [`STARTUP_PUBLICATION_DEADLINE`]. Meant for a supervised task: it awaits the registry
/// for as long as it takes, so never call it from a lifecycle hook directly.
///
/// An identifier admitted by one attempt stays admitted: later attempts submit only
/// the others, so a later change by another writer is not overwritten. Errors, those
/// passed to `on_retry` included, cover the whole `desired` set, earlier admissions
/// included. `Ok` once every identifier has been admitted.
///
/// # Errors
/// The first non-transient [`PublicationError`]: a rejection, a call-level refusal or
/// invalid input; or caller cancellation, as a `Cancelled` error, whenever it comes.
pub async fn publish_until_admitted(
    registry: &dyn PlatformTypesRegistryApi,
    publisher: &PublisherContext,
    desired: &[(GtsId, JsonDocument)],
    retry: &PublicationRetry,
    cancel: &CancellationToken,
    mut on_retry: impl FnMut(&PublicationError) + Send,
) -> Result<Reconciliation, PublicationError> {
    let mut admitted: HashMap<GtsId, ReconcileOutcome> = HashMap::new();
    let mut remaining: Vec<(GtsId, JsonDocument)> = desired.to_vec();
    let mut pauses = retry.pauses();
    loop {
        let error = match publish_startup_entities(registry, publisher, &remaining, cancel).await {
            Ok(Reconciliation::UpToDate) if admitted.is_empty() => {
                return Ok(Reconciliation::UpToDate);
            }
            Ok(reconciliation) => {
                let outcomes = match reconciliation {
                    Reconciliation::UpToDate => HashMap::new(),
                    Reconciliation::Reconciled(outcomes) => outcomes,
                };
                admitted.extend(outcomes);
                for (id, _) in remaining {
                    admitted.entry(id).or_insert(ReconcileOutcome::Admitted);
                }
                return Ok(Reconciliation::Reconciled(admitted));
            }
            Err(error) => error,
        };
        if let Some(report) = error.report() {
            for (id, outcome) in &report.outcomes {
                if matches!(outcome, ReconcileOutcome::Admitted) {
                    admitted.insert(id.clone(), ReconcileOutcome::Admitted);
                }
            }
        }
        remaining.retain(|(id, _)| !admitted.contains_key(id));
        let error = error.covering(desired, &admitted);
        if !error.is_transient() {
            return Err(error);
        }
        if cancel.is_cancelled() {
            return Err(PublicationError::cancelled(publisher, desired));
        }
        let pause = pauses.next().unwrap_or(retry.max);
        tracing::warn!(
            publisher = %publisher.name,
            error = %error,
            retry_in_ms = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
            "startup publication not admitted yet; retrying"
        );
        on_retry(&error);
        tokio::select! {
            () = cancel.cancelled() => return Err(PublicationError::cancelled(publisher, desired)),
            () = tokio::time::sleep(pause) => {}
        }
    }
}

/// A startup publication failure with the original canonical error as its source.
///
/// Display names the publisher, stop reason and each rejected or pending identifier,
/// sorted by identifier, with machine-readable causes rather than document contents.
/// Without a report (e.g. cancellation), it names every requested identifier instead.
#[derive(Debug)]
pub struct PublicationError {
    publisher: String,
    requested: Vec<GtsId>,
    error: Box<CanonicalError>,
    report: Option<ReconcileReport>,
}

impl PublicationError {
    fn new(
        publisher: &PublisherContext,
        desired: &[(GtsId, JsonDocument)],
        error: CanonicalError,
    ) -> Self {
        let mut requested: Vec<_> = desired.iter().map(|(id, _)| id.clone()).collect();
        requested.sort_unstable_by(|left, right| left.id().cmp(right.id()));
        requested.dedup();
        let report = ReconcileReport::from_canonical(&error);
        Self {
            publisher: publisher.name.clone(),
            requested,
            error: Box::new(error),
            report,
        }
    }

    /// The original error, also available through [`Error::source`].
    #[must_use]
    pub fn canonical_error(&self) -> &CanonicalError {
        &self.error
    }

    /// Caller cancellation of a publication of `desired`.
    fn cancelled(publisher: &PublisherContext, desired: &[(GtsId, JsonDocument)]) -> Self {
        Self::new(
            publisher,
            desired,
            crate::gts::OperationResource::cancelled().create(),
        )
    }

    /// This error, about part of `desired`, as one about all of it: requested
    /// identifiers are all of `desired`, and the report adds the `admitted` ones, in
    /// [`report`](Self::report) and the canonical error alike.
    fn covering(
        mut self,
        desired: &[(GtsId, JsonDocument)],
        admitted: &HashMap<GtsId, ReconcileOutcome>,
    ) -> Self {
        let mut requested: Vec<_> = desired.iter().map(|(id, _)| id.clone()).collect();
        requested.sort_unstable_by(|left, right| left.id().cmp(right.id()));
        requested.dedup();
        self.requested = requested;
        if let Some(report) = &mut self.report {
            for id in admitted.keys() {
                report
                    .outcomes
                    .entry(id.clone())
                    .or_insert(ReconcileOutcome::Admitted);
            }
            // Encoding a decoded report succeeds; keep the attempt's error otherwise.
            if let Ok(error) = report.clone().into_canonical() {
                *self.error = error;
            }
        }
        self
    }

    /// Decoded per-identifier outcomes and stop reason, when the error carries them.
    #[must_use]
    pub fn report(&self) -> Option<&ReconcileReport> {
        self.report.as_ref()
    }

    /// Whether a later attempt could succeed with nothing changed on the publisher's
    /// side: every identifier not admitted waits on a dependency, a concurrent write or
    /// the registry's availability. A rejection, a call-level refusal (authentication,
    /// permission, an invalid request), invalid input and cancellation are not transient.
    ///
    /// Decided per identifier from the [`report`](Self::report): the error's own
    /// category (`FailedPrecondition` for every incomplete reconciliation) says nothing.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match &self.report {
            Some(report) => {
                report.stop != ReconcileStop::InvalidInput
                    && report.outcomes.values().all(|outcome| {
                        matches!(
                            outcome,
                            ReconcileOutcome::Admitted
                                | ReconcileOutcome::Pending(
                                    ReconcilePendingCause::Dependency(_)
                                        | ReconcilePendingCause::Conflict(_)
                                        | ReconcilePendingCause::Unavailable(_)
                                )
                        )
                    })
            }
            None => matches!(
                ReconcilePendingCause::of((*self.error).clone()),
                ReconcilePendingCause::Unavailable(_)
            ),
        }
    }
}

impl Error for PublicationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.as_ref())
    }
}

impl fmt::Display for PublicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: publishing ", self.publisher)?;
        if let [id] = self.requested.as_slice() {
            let kind = match EntityKind::of(id) {
                EntityKind::TypeSchema => "type schema",
                EntityKind::Instance => "instance",
            };
            write!(f, "{kind} {id}")?;
        } else {
            f.write_str("entities")?;
        }
        write!(f, " failed: {}", error_fields(&self.error).category)?;
        let Some(report) = &self.report else {
            for id in &self.requested {
                write!(f, "; requested: {id}")?;
            }
            return Ok(());
        };
        write!(f, "; {report}")?;
        let mut outcomes: Vec<_> = report.outcomes.iter().collect();
        outcomes.sort_unstable_by(|(left, _), (right, _)| left.id().cmp(right.id()));
        for (id, outcome) in outcomes {
            let (status, error) = match outcome {
                ReconcileOutcome::Admitted => continue,
                ReconcileOutcome::Rejected(error) => ("rejected", Some(error)),
                ReconcileOutcome::Pending(cause) => match cause {
                    ReconcilePendingCause::Dependency(error) => ("pending.dependency", Some(error)),
                    ReconcilePendingCause::Conflict(error) => ("pending.conflict", Some(error)),
                    ReconcilePendingCause::Unavailable(error) => {
                        ("pending.unavailable", Some(error))
                    }
                    ReconcilePendingCause::Refused(error) => ("pending.refused", Some(error)),
                    ReconcilePendingCause::Skipped => ("pending.skipped", None),
                },
            };
            write!(f, "; {id}: {status}")?;
            if let Some(error) = error {
                let fields = error_fields(error);
                write!(f, " ({})", fields.category)?;
                if let Some(reason) = fields.reason {
                    write!(f, " [{reason}]")?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod tests;
