//! Deployment publication: publish every linked gear's manifest to an explicit
//! remote registry, without running any gear.
//!
//! The job uses only the [`GtsPublicationDescriptor`]s and the effective
//! configuration: no gear instance, `init`, database, bootstrap or plugin
//! binding. Each descriptor's registry wiring is bound to `registry_endpoint`
//! with the given client tuning, which carries the platform credential. All
//! publications run concurrently under one deadline: a waiting derived-type
//! publisher never delays a base-type one in the same job.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_contract::wiring::ClientTuning;

use super::{
    GtsPublicationDescriptor, GtsPublicationError, PreparedGtsPublication, check_ownership,
};
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;
use crate::discovery::StaticEndpointResolver;

/// How one publisher ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GtsDeploymentOutcome {
    /// Every document admitted (or nothing to publish).
    Published { documents: usize },
    /// A terminal refusal; the message names the publisher and GTS ids.
    Refused(String),
    /// Not confirmed before the deadline. `unconfirmed` lists the manifest's
    /// GTS ids: some of them may already be admitted.
    DeadlineExceeded { unconfirmed: Vec<String> },
    /// The caller cancelled the job first; `unconfirmed` as above.
    Cancelled { unconfirmed: Vec<String> },
}

/// The result of a deployment publication, one entry per publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GtsDeploymentReport {
    pub publishers: Vec<(&'static str, GtsDeploymentOutcome)>,
}

impl GtsDeploymentReport {
    /// Whether every publisher succeeded.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.publishers
            .iter()
            .all(|(_, outcome)| matches!(outcome, GtsDeploymentOutcome::Published { .. }))
    }
}

impl fmt::Display for GtsDeploymentReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (gear, outcome) in &self.publishers {
            match outcome {
                GtsDeploymentOutcome::Published { documents } => {
                    writeln!(f, "{gear}: published ({documents} documents)")?;
                }
                GtsDeploymentOutcome::Refused(message) => {
                    writeln!(f, "{gear}: refused: {message}")?;
                }
                GtsDeploymentOutcome::DeadlineExceeded { unconfirmed } => writeln!(
                    f,
                    "{gear}: deadline exceeded; not confirmed (some may be admitted): {}",
                    unconfirmed.join(", ")
                )?,
                GtsDeploymentOutcome::Cancelled { unconfirmed } => writeln!(
                    f,
                    "{gear}: cancelled; not confirmed (some may be admitted): {}",
                    unconfirmed.join(", ")
                )?,
            }
        }
        Ok(())
    }
}

/// Publish the manifests of `descriptors` for `config` to `registry_endpoint`,
/// concurrently, within `deadline`. Cancelling `cancel`, or dropping the
/// future, ends every publication: they run in this future, not spawned.
///
/// # Errors
/// A setup error before any publication: a manifest that does not build, a
/// GTS id with two publishers, or registry wiring that fails. Publication
/// results, refusals, the deadline and cancellation included, are in the report.
pub async fn publish(
    descriptors: &[&'static GtsPublicationDescriptor],
    config: &dyn ConfigProvider,
    registry_endpoint: &str,
    tuning: &ClientTuning,
    deadline: Duration,
    cancel: &CancellationToken,
) -> anyhow::Result<GtsDeploymentReport> {
    let hub = ClientHub::new();
    let resolver: Arc<dyn crate::discovery::EndpointResolver> =
        Arc::new(StaticEndpointResolver::new(registry_endpoint));
    let mut prepared = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        (descriptor.wire)(&hub, Arc::clone(&resolver), tuning.clone())
            .map_err(|e| e.context(format!("{}: registry wiring", descriptor.gear)))?;
        prepared.push((
            descriptor.gear,
            PreparedGtsPublication::prepare(descriptor, config, &hub)?,
        ));
    }
    check_ownership(prepared.iter().map(|(_, plan)| plan.gts_manifest()))?;

    let prepared: Vec<PreparedGtsPublication> =
        prepared.into_iter().map(|(_, plan)| plan).collect();

    // Cancelled at the deadline or with the caller's token.
    let stop = cancel.child_token();
    let deadline_passed = AtomicBool::new(false);
    let all = super::publish_all(
        &prepared,
        &stop,
        Arc::new(|_: &'static str, _: &str| {}),
        false,
    );
    tokio::pin!(all);
    let results = tokio::select! {
        results = &mut all => results,
        () = tokio::time::sleep(deadline) => {
            // Every unfinished publication ends: past the deadline.
            deadline_passed.store(true, Ordering::SeqCst);
            stop.cancel();
            all.await
        }
    };
    let publishers = prepared
        .iter()
        .zip(results)
        .map(|(plan, (gear, result))| {
            let unconfirmed = || {
                plan.gts_manifest()
                    .documents()
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect()
            };
            let outcome = match result {
                Ok(()) => GtsDeploymentOutcome::Published {
                    documents: plan.gts_manifest().documents().len(),
                },
                Err(GtsPublicationError::Cancelled { .. })
                    if deadline_passed.load(Ordering::SeqCst) =>
                {
                    GtsDeploymentOutcome::DeadlineExceeded {
                        unconfirmed: unconfirmed(),
                    }
                }
                Err(GtsPublicationError::Cancelled { .. }) => GtsDeploymentOutcome::Cancelled {
                    unconfirmed: unconfirmed(),
                },
                Err(e) => GtsDeploymentOutcome::Refused(format!("{e:#}")),
            };
            (gear, outcome)
        })
        .collect();
    Ok(GtsDeploymentReport { publishers })
}

#[cfg(test)]
#[path = "gts_publication_deployment_tests.rs"]
mod gts_publication_deployment_tests;
