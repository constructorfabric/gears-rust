//! Await a gear's startup publications with shared tuning and diagnostics.

use std::error::Error;
use std::fmt;

use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::reconcile::error_fields;
use crate::{
    EntityKind, GtsId, JsonDocument, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
    PublisherContext, ReconcileOptions, ReconcileOutcome, ReconcilePendingCause, ReconcileReport,
    Reconciliation, STARTUP_PUBLICATION_DEADLINE,
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
