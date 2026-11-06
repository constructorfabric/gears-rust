//! Publish gear manifests through reconciliation, using [`Registry`] from [`PROVIDER`].
//! Supplies publisher identity, platform context, budget and diagnostics;
//! reconciliation owns admission and retries. Independent of `toolkit`.

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

/// Reconcile one publisher's documents until all are admitted.
///
/// Uses the platform outbound marker in the current task. Missing dependencies,
/// conflicts and registry outages retry with jittered exponential backoff
/// (default 1–30 s) and [`ReconcileOptions`]' default batch size.
/// See [`Self::retry_backoff`] and [`Self::batch_size`].
///
/// With `start` the time `publish` begins, deadline `D` and timeout `T`:
///
/// | `D` | `T` | Publication ends at |
/// |-----|-----|---------------------|
/// | none | none | `start + `[`STARTUP_PUBLICATION_DEADLINE`] |
/// | `D` | none | `D`, even beyond the default budget |
/// | none | `T` | `start + T` |
/// | `D` | `T` | `min(D, start + T)` |
///
/// Retries never renew the deadline.
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
            version: publisher_version
                .parse()
                .map_err(|source| PublicationInputError {
                    publisher: publisher_name.to_owned(),
                    version: publisher_version.to_owned(),
                    source,
                })?,
        };
        Ok(Self::new(registry, publisher, documents.to_vec()))
    }
}

impl<'a, F> Publication<'a, F> {
    /// Use an absolute deadline for all passes, replacing the default budget
    /// even when longer. Retries never renew it. `None` restores the default.
    pub fn deadline(mut self, deadline: impl Into<Option<tokio::time::Instant>>) -> Self {
        self.deadline = deadline.into();
        self
    }

    /// Limit publication to `timeout` from `publish`, capped by [`Self::deadline`].
    /// `None` removes this cap. An unrepresentable timeout uses the caller's deadline
    /// if supplied; otherwise `publish` fails before any request.
    pub fn timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.timeout = timeout.into();
        self
    }

    /// Retry with jittered exponential pauses from `initial` up to `max`.
    ///
    /// Unlike runtime configuration, this accepts zero pauses (immediate retries)
    /// and clamps `initial` to `max`.
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
            // A publisher's desired set may wait on another writer's base type.
            stop_when_stalled: false,
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

/// An invalid publisher version in a manifest, found before any request; the
/// parse error is its [`Error::source`].
#[derive(Debug)]
pub struct PublicationInputError {
    publisher: String,
    version: String,
    source: crate::PublisherVersionError,
}

impl fmt::Display for PublicationInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: publisher version {}", self.publisher, self.version)
    }
}

impl Error for PublicationInputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
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

    /// Decoded per-identifier outcomes and stop reason, when the error carries them.
    #[must_use]
    pub fn report(&self) -> Option<&ReconcileReport> {
        self.report.as_ref()
    }

    /// Whether every unresolved identifier could succeed unchanged after a dependency,
    /// concurrent-write or availability issue clears. Rejections, call-level refusals,
    /// invalid input and cancellation are terminal.
    ///
    /// Uses per-identifier [`report`](Self::report) outcomes: the shared
    /// `FailedPrecondition` category cannot distinguish these cases.
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

impl PublicationError {
    /// Whether the budget expired: the report's stop is
    /// [`ReconcileStop::Deadline`](crate::ReconcileStop::Deadline).
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.report
            .as_ref()
            .is_some_and(|report| report.stop == crate::ReconcileStop::Deadline)
    }
}

/// [`PublicationError::is_expired`], in the publisher-adapter interface of
/// `toolkit::gts_publication`.
#[must_use]
pub fn expired(error: &PublicationError) -> bool {
    error.is_expired()
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
