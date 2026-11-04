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
#[non_exhaustive]
pub enum GtsDeploymentOutcome {
    /// Every document admitted (or nothing to publish).
    Published { documents: usize },
    /// A terminal refusal; the message names the publisher and GTS ids.
    Refused(String),
    /// Not confirmed before the job's deadline or the gear's own publication
    /// budget. `unconfirmed` lists the manifest's GTS ids: some of them may
    /// already be admitted.
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
/// A setup error before any publication: `registry_endpoint` is not an
/// absolute `http`/`https` URI, a manifest does not build, a GTS id has two
/// publishers, or registry wiring fails. Publication
/// results, refusals, the deadline and cancellation included, are in the report.
pub async fn publish(
    descriptors: &[&'static GtsPublicationDescriptor],
    config: &dyn ConfigProvider,
    registry_endpoint: &str,
    tuning: &ClientTuning,
    deadline: Duration,
    cancel: &CancellationToken,
) -> anyhow::Result<GtsDeploymentReport> {
    // The platform credential goes to this endpoint: refuse anything but an
    // absolute URI naming a host, before any wiring.
    let endpoint: http::Uri = registry_endpoint
        .parse()
        .map_err(|e| anyhow::anyhow!("registry endpoint '{registry_endpoint}': {e}"))?;
    anyhow::ensure!(
        matches!(endpoint.scheme_str(), Some("http" | "https"))
            && endpoint.host().is_some_and(|host| !host.is_empty()),
        "registry endpoint '{registry_endpoint}' is not an absolute http(s) URI"
    );
    let deadline_at = tokio::time::Instant::now()
        .checked_add(deadline)
        .ok_or_else(|| anyhow::anyhow!("the publication deadline cannot be represented"))?;
    let hub = ClientHub::new();
    let resolver: Arc<dyn crate::discovery::EndpointResolver> =
        Arc::new(StaticEndpointResolver::new(registry_endpoint));
    let mut prepared = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        (descriptor.wire)(&hub, Arc::clone(&resolver), tuning.clone())
            .map_err(|e| e.context(format!("{}: registry wiring", descriptor.gear)))?;
        prepared.push(
            PreparedGtsPublication::prepare(descriptor, config, &hub)?.with_deadline(deadline_at),
        );
    }
    check_ownership(prepared.iter().map(PreparedGtsPublication::gts_manifest))?;

    // Cancelled at the deadline or with the caller's token.
    let stop = cancel.child_token();
    let all = super::publish_all(
        &prepared,
        &stop,
        Arc::new(|_: &'static str, _: &str| {}),
        false,
    );
    tokio::pin!(all);
    let (results, deadline_passed) = tokio::select! {
        biased;
        () = tokio::time::sleep_until(deadline_at) => {
            // Every unfinished publication ends: past the deadline.
            stop.cancel();
            (all.await, true)
        },
        results = &mut all => (results, false),
    };
    let publishers = prepared
        .iter()
        .zip(results)
        .map(|(plan, (gear, result))| {
            let unconfirmed = || {
                plan.gts_manifest()
                    .documents()
                    .iter()
                    .map(|(id, _)| id.to_string())
                    .collect()
            };
            let outcome = match result {
                Ok(()) => GtsDeploymentOutcome::Published {
                    documents: plan.gts_manifest().documents().len(),
                },
                Err(GtsPublicationError::Expired { .. }) => {
                    GtsDeploymentOutcome::DeadlineExceeded {
                        unconfirmed: unconfirmed(),
                    }
                }
                Err(GtsPublicationError::Cancelled { .. }) if deadline_passed => {
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
