//! Publish a gear's declared documents through reconciliation.
//!
//! The runtime wires [`Registry`] from [`PROVIDER`] and publishes each manifest with
//! [`Publication`]. This module prepares the publisher and documents, supplies the platform context and
//! budget, and formats publication diagnostics. The reconciliation layer owns admission,
//! progress and retries. This module does not depend on `toolkit`.

use std::error::Error;
use std::fmt;
use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::reconciliation::error_fields;
use crate::{
    EntityKind, GtsId, JsonDocument, PublisherContext, ReconcileOptions, ReconcileOutcome,
    ReconcilePendingCause, ReconcileReport, Reconciliation, STARTUP_PUBLICATION_DEADLINE,
};

/// The registry contract the publication uses: the client-hub key.
pub type Registry = dyn crate::PlatformTypesRegistryApi;

/// The directory name of the registry provider.
pub const PROVIDER: &str = "types-registry";

/// The resolving REST client wired when no local registry is present.
#[cfg(feature = "rest-client")]
pub use crate::PlatformTypesRegistryApiRestResolvingClient as ResolvingClient;

/// One publisher's documents, reconciled until every one is admitted.
///
/// The required data — registry, publisher and documents — go to [`Self::new`] or
/// [`Self::from_manifest`]; the rest is optional. [`Self::publish`] retries retryable
/// conditions (an absent dependency, a conflict, an unavailable registry) with jittered
/// exponential backoff, by default from 1 s up to 30 s ([`Self::retry_backoff`]), in
/// batches of the [`ReconcileOptions`] default size ([`Self::batch_size`]), until the
/// publication budget ends. It uses the platform outbound marker and runs in the current
/// task.
///
/// The budget, with `start` the instant `publish` starts, the absolute
/// [`Self::deadline`] `D` and the [`Self::timeout`] duration `T`:
///
/// | `D`  | `T`  | the publication ends at                                    |
/// |------|------|------------------------------------------------------------|
/// | none | none | `start + `[`STARTUP_PUBLICATION_DEADLINE`]                 |
/// | `D`  | none | `D`, even past `start + STARTUP_PUBLICATION_DEADLINE`      |
/// | none | `T`  | `start + T`                                                |
/// | `D`  | `T`  | `min(D, start + T)`                                        |
///
/// `D` is never renewed; `T` only caps it.
///
/// ```ignore
/// Publication::new(registry, publisher, documents)
///     .deadline(deadline)
///     .timeout(Duration::from_secs(60))
///     .on_pending(|pending| tracing::info!(%pending))
///     .publish(&cancel)
///     .await?;
/// ```
#[must_use = "a publication does nothing until `publish` is awaited"]
pub struct Publication<'a, F = fn(&PublicationError)> {
    registry: &'a Registry,
    publisher: PublisherContext,
    documents: Vec<(GtsId, JsonDocument)>,
    deadline: Option<tokio::time::Instant>,
    timeout: Option<Duration>,
    retry_backoff: Duration,
    retry_backoff_max: Duration,
    batch_size: NonZeroUsize,
    on_pending: F,
}

impl<'a> Publication<'a> {
    /// Publish `documents` as `publisher`, the calling gear's own identity.
    pub fn new(
        registry: &'a Registry,
        publisher: PublisherContext,
        documents: impl Into<Vec<(GtsId, JsonDocument)>>,
    ) -> Self {
        Self {
            registry,
            publisher,
            documents: documents.into(),
            deadline: None,
            timeout: None,
            retry_backoff: Duration::from_secs(1),
            retry_backoff_max: Duration::from_secs(30),
            batch_size: ReconcileOptions::default().batch_size,
            on_pending: |_| {},
        }
    }

    /// Publish a manifest, as the `gts_publication` runtime does: its GTS ids are
    /// already parsed, the publisher version is not.
    ///
    /// # Errors
    /// [`PublicationInputError`] for an invalid publisher version; nothing is sent to
    /// the registry.
    pub fn from_manifest(
        registry: &'a Registry,
        publisher_name: &str,
        publisher_version: &str,
        documents: &[(GtsId, JsonDocument)],
    ) -> Result<Self, PublicationInputError> {
        let publisher = PublisherContext {
            name: publisher_name.to_owned(),
            version: publisher_version.parse().map_err(|e| {
                PublicationInputError(format!(
                    "{publisher_name}: publisher version {publisher_version}: {e}"
                ))
            })?,
        };
        Ok(Self::new(registry, publisher, documents.to_vec()))
    }
}

impl<'a, F> Publication<'a, F> {
    /// End the publication at the caller's absolute `deadline` instead of the default
    /// budget. It covers every pass and may exceed the default; it is never renewed on
    /// a retry. `None` keeps the default.
    pub fn deadline(mut self, deadline: impl Into<Option<tokio::time::Instant>>) -> Self {
        self.deadline = deadline.into();
        self
    }

    /// End the publication `timeout` after the start of `publish`, or at the
    /// [`Self::deadline`] if that comes first: the timeout caps the caller's deadline and
    /// never extends it. `None` keeps no cap. Without a deadline, a `timeout` no clock can
    /// represent fails `publish` before any request; with one, the deadline applies.
    pub fn timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.timeout = timeout.into();
        self
    }

    /// Pause `initial` before the first retrying pass, then double the pause with jitter
    /// up to `max`.
    ///
    /// These are the raw reconciliation values: an `initial` above `max` is clamped to
    /// `max`, and a zero pause retries at once, as often as the budget allows. The
    /// `gts_publication` runtime configuration rejects both before it gets here.
    pub fn retry_backoff(mut self, initial: Duration, max: Duration) -> Self {
        self.retry_backoff = initial;
        self.retry_backoff_max = max;
        self
    }

    /// Submit at most `batch_size` documents per registry call. A batch above the
    /// registry's limit is refused, which ends the publication.
    pub fn batch_size(mut self, batch_size: NonZeroUsize) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// Call `on_pending` before each retrying pass with what is not admitted yet.
    pub fn on_pending<G>(self, on_pending: G) -> Publication<'a, G>
    where
        G: FnMut(&PublicationError) + Send,
    {
        Publication {
            registry: self.registry,
            publisher: self.publisher,
            documents: self.documents,
            deadline: self.deadline,
            timeout: self.timeout,
            retry_backoff: self.retry_backoff,
            retry_backoff_max: self.retry_backoff_max,
            batch_size: self.batch_size,
            on_pending,
        }
    }

    /// Reconcile the documents and await their admission.
    ///
    /// `Ok` means every identifier is admitted or already up to date.
    ///
    /// # Errors
    /// [`PublicationError`] on refusal, incomplete admission, the end of the budget or
    /// cancellation, preserving the original canonical error and any reconciliation
    /// report. Cancellation or dropping the future does not roll back accepted writes.
    pub async fn publish(
        self,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation, PublicationError>
    where
        F: FnMut(&PublicationError) + Send,
    {
        let Self {
            registry,
            publisher,
            documents,
            deadline,
            timeout,
            retry_backoff,
            retry_backoff_max,
            batch_size,
            mut on_pending,
        } = self;
        let started = tokio::time::Instant::now();
        // An absolute end only for a caller's deadline; otherwise reconciliation measures
        // the budget from its start and refuses one it cannot represent.
        let (budget, absolute_deadline) = match (deadline, timeout) {
            (None, None) => (STARTUP_PUBLICATION_DEADLINE, None),
            (None, Some(timeout)) => (timeout, None),
            (Some(deadline), timeout) => {
                let end = timeout
                    .and_then(|timeout| started.checked_add(timeout))
                    .map_or(deadline, |capped| capped.min(deadline));
                (end.saturating_duration_since(started), Some(end))
            }
        };
        let options = ReconcileOptions {
            batch_size,
            passes: NonZeroU32::MAX,
            retry_backoff,
            retry_backoff_max,
            deadline: budget,
            absolute_deadline,
            ..ReconcileOptions::default()
        };
        crate::reconciliation::reconcile_with_progress(
            registry,
            &PlatformSecurityContext::outbound_marker(),
            &publisher,
            &documents,
            &options,
            cancel,
            |report| {
                let error = report
                    .clone()
                    .into_canonical()
                    .unwrap_or_else(|error| error);
                let error = PublicationError::new(&publisher, &documents, error);
                tracing::warn!(
                    publisher = %publisher.name,
                    error = %error,
                    "startup publication not admitted yet; retrying"
                );
                on_pending(&error);
            },
        )
        .await
        .map_err(|error| PublicationError::new(&publisher, &documents, error))
    }
}

/// An invalid publisher version or GTS id in a manifest, found before any request.
#[derive(Debug)]
pub struct PublicationInputError(String);

impl fmt::Display for PublicationInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for PublicationInputError {}

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
            Some(report) => report.is_transient(),
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
