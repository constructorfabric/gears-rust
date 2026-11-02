//! Publisher adapter for `#[toolkit::gear(gts(publisher = types_registry_sdk::gts_publisher))]`.
//!
//! The runtime wires [`Registry`] of [`PROVIDER`] for the publication
//! (through [`ResolvingClient`] when the registry is remote) and calls
//! [`publish`] from the runtime's application preparation. Retry semantics stay here,
//! in [`publish_until_admitted`]. This module does not depend on `toolkit`.

use std::error::Error;
use std::fmt;

use tokio_util::sync::CancellationToken;

use crate::{GtsId, PublicationError, PublicationRetry, PublisherContext, publish_until_admitted};

/// The registry contract the publication uses: the client-hub key.
pub type Registry = dyn crate::PlatformTypesRegistryApi;

/// The directory name of the registry provider.
pub const PROVIDER: &str = "types-registry";

/// The resolving REST client wired when no local registry is present.
#[cfg(feature = "rest-client")]
pub use crate::PlatformTypesRegistryApiRestResolvingClient as ResolvingClient;

/// Reconcile `documents` as `publisher_name`/`publisher_version` until every
/// one is admitted. Retryable conditions (an absent dependency, a conflict, an
/// unavailable registry) are retried with [`PublicationRetry::default`] and
/// reported to `on_pending`; cancellation ends the wait.
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
    publish_until_admitted(
        registry,
        &publisher,
        &desired,
        &PublicationRetry::default(),
        cancel,
        |error| on_pending(&error.to_string()),
    )
    .await
    .map(drop)
    .map_err(GtsPublishError::Publication)
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

#[cfg(test)]
#[path = "gts_publisher_tests.rs"]
mod tests;
