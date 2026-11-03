//! Publish a gear's declared documents through reconciliation.
//!
//! The runtime wires [`Registry`] from [`PROVIDER`] and calls [`publish_with_deadline`].
//! This module prepares the publisher and documents, supplies the platform context and
//! budget, and formats publication diagnostics. The reconciliation layer owns admission,
//! progress and retries. This module does not depend on `toolkit`.

use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::reconciliation::error_fields;
use crate::{
    EntityKind, GtsId, JsonDocument, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
    PublisherContext, ReconcileOptions, ReconcileOutcome, ReconcilePendingCause, ReconcileReport,
    Reconciliation, STARTUP_PUBLICATION_DEADLINE,
};

/// The registry contract the publication uses: the client-hub key.
pub type Registry = dyn crate::PlatformTypesRegistryApi;

/// The directory name of the registry provider.
pub const PROVIDER: &str = "types-registry";

/// The resolving REST client wired when no local registry is present.
#[cfg(feature = "rest-client")]
pub use crate::PlatformTypesRegistryApiRestResolvingClient as ResolvingClient;

/// Reconcile `documents` as `publisher_name`/`publisher_version` until every
/// one is admitted. Retryable conditions (an absent dependency, a conflict, an
/// unavailable registry) are retried with jittered exponential backoff (1 s up to 30 s) and
/// reported to `on_pending`, within the shared startup budget. Cancellation ends the wait.
///
/// # Errors
/// [`GtsPublishError::Input`] for an invalid id or version;
/// [`GtsPublishError::Publication`] for a terminal refusal or cancellation.
pub async fn publish(
    registry: &Registry,
    publisher_name: &str,
    publisher_version: &str,
    documents: &[(String, serde_json::Value)],
    cancel: &CancellationToken,
    on_pending: impl FnMut(&str) + Send,
) -> Result<(), GtsPublishError> {
    publish_with_deadline(
        registry,
        publisher_name,
        publisher_version,
        documents,
        cancel,
        None,
        on_pending,
    )
    .await
}

/// Publish within the caller's absolute deadline, or the default startup budget.
/// The deadline covers every reconciliation pass; it is never renewed on a retry.
///
/// # Errors
/// Invalid input, a terminal refusal, cancellation, or an expired publication budget.
pub async fn publish_with_deadline(
    registry: &Registry,
    publisher_name: &str,
    publisher_version: &str,
    documents: &[(String, serde_json::Value)],
    cancel: &CancellationToken,
    deadline: Option<tokio::time::Instant>,
    mut on_pending: impl FnMut(&str) + Send,
) -> Result<(), GtsPublishError> {
    let publisher = PublisherContext {
        name: publisher_name.to_owned(),
        version: publisher_version.parse().map_err(|e| {
            GtsPublishError::Input(format!(
                "{publisher_name}: publisher version {publisher_version}: {e}"
            ))
        })?,
    };
    let desired = documents
        .iter()
        .map(|(id, document)| {
            GtsId::try_new(id)
                .map(|id| (id, document.clone()))
                .map_err(|e| GtsPublishError::Input(format!("{publisher_name}: GTS id {id}: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let options = ReconcileOptions {
        passes: NonZeroU32::MAX,
        retry_backoff: Duration::from_secs(1),
        retry_backoff_max: Duration::from_secs(30),
        deadline: deadline.map_or(crate::STARTUP_PUBLICATION_DEADLINE, |deadline| {
            deadline.saturating_duration_since(tokio::time::Instant::now())
        }),
        absolute_deadline: deadline,
        ..ReconcileOptions::default()
    };
    crate::reconciliation::reconcile_with_progress(
        registry,
        &PlatformSecurityContext::outbound_marker(),
        &publisher,
        &desired,
        &options,
        cancel,
        |report| {
            let error = report
                .clone()
                .into_canonical()
                .unwrap_or_else(|error| error);
            let error = PublicationError::new(&publisher, &desired, error);
            tracing::warn!(
                publisher = %publisher.name,
                error = %error,
                "startup publication not admitted yet; retrying"
            );
            on_pending(&error.to_string());
        },
    )
    .await
    .map(drop)
    .map_err(|error| {
        GtsPublishError::Publication(PublicationError::new(&publisher, &desired, error))
    })
}

/// Why [`publish`] did not succeed.
#[derive(Debug)]
pub enum GtsPublishError {
    /// The manifest has an invalid GTS id or publisher version.
    Input(String),
    /// A terminal refusal or cancellation.
    Publication(PublicationError),
}

impl fmt::Display for GtsPublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(message) => f.write_str(message),
            Self::Publication(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl Error for GtsPublishError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Input(_) => None,
            Self::Publication(error) => Some(error),
        }
    }
}

/// Reconcile a gear's documents and await admission before binding dependent clients.
///
/// Uses the platform outbound marker and [`STARTUP_PUBLICATION_DEADLINE`], retrying
/// within that budget. Batching and backoff use [`ReconcileOptions`] defaults. `Ok` means every identifier is admitted or
/// already up to date. The publisher's name and version belong to the calling gear.
///
/// Runs in the current task, without spawning or changing lifecycle phases. Call only
/// when the registry client is available; application preparation wires it before `init`.
/// For custom tuning or a different security context, use
/// [`crate::PlatformTypesRegistryApiExt::reconcile_entities_and_await`] directly.
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
        passes: NonZeroU32::MAX,
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
