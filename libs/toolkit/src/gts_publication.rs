//! GTS publication capability.
//!
//! A gear declares its publication in its gear attribute:
//!
//! ```ignore
//! #[toolkit::gear(
//!     name = "my-gear",
//!     gts(
//!         crates = [crate, my_gear_sdk],
//!         publisher = types_registry_sdk::gts_publisher,
//!         entities = Self::configured_entities, // optional
//!     ),
//! )]
//! pub struct MyGear { /* ... */ }
//! ```
//!
//! Each crate in `crates` invokes `toolkit_gts::declare_gts_inventory!()`. The
//! macro generates the [`GtsPublicationDescriptor`], the
//! [`GtsPublicationCapability`] and the registry-client wiring of the
//! publisher adapter. The gear writes no task, retry loop or readiness code.
//!
//! The runtime builds every manifest from the configuration before any gear
//! runs, then — once the infrastructure (the registry among it) started and
//! the application migrations ran — publishes them all concurrently
//! ([`publish_all`]) and waits until each one is admitted. No application
//! `init` runs before that barrier. A gear reads its admitted manifest in
//! `init` ([`GearCtx::gts_publication_manifest`]): a plugin binds its scoped
//! clients to the instances it lists. A pending publication of one gear does
//! not delay another publication; it delays every application `init`.
//!
//! [`GearCtx::gts_publication_manifest`]: crate::context::GearCtx::gts_publication_manifest
//!
//! # Publisher adapter
//!
//! The `publisher` path names a module with this interface:
//!
//! ```ignore
//! pub type Registry = dyn SomeRegistryApi;          // client-hub key
//! pub const PROVIDER: &str = "provider-gear";       // directory name
//! pub use some::ResolvingClient;                    // ::new(resolver, PROVIDER, tuning)
//! pub async fn publish(
//!     registry: &Registry,
//!     publisher_name: &str,
//!     publisher_version: &str,
//!     documents: &[(String, serde_json::Value)],
//!     cancel: &CancellationToken,
//!     on_pending: impl FnMut(&str) + Send,
//! ) -> Result<(), impl std::error::Error + Send + Sync + 'static>;
//! ```
//!
//! `publish` returns `Ok` only after every document is admitted. It retries
//! retryable conditions itself and returns an error only for a terminal
//! refusal or cancellation. Toolkit does not depend on the adapter's crate.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;

use crate::client_hub::ClientHub;
use crate::config::{ConfigError, ConfigProvider, gear_config_or_default, gear_config_required};

pub use toolkit_gts::local::DeclarationSource;

/// Config-only inputs of an `entities` function.
///
/// It gives the effective gear configuration and nothing else: no initialized
/// gear state, database or remote client. Equal effective configuration gives
/// an equal manifest, in the runtime and in a deployment job.
pub struct GtsPublicationInputs<'a> {
    gear: &'a str,
    config: &'a dyn ConfigProvider,
}

impl<'a> GtsPublicationInputs<'a> {
    #[must_use]
    pub fn new(gear: &'a str, config: &'a dyn ConfigProvider) -> Self {
        Self { gear, config }
    }

    /// The gear's config section, as [`GearCtx::config`].
    ///
    /// # Errors
    /// The config is missing or does not deserialize.
    pub fn config<T: DeserializeOwned>(&self) -> Result<T, ConfigError> {
        gear_config_required(self.config, self.gear)
    }

    /// The gear's config section or its default, as [`GearCtx::config_or_default`].
    ///
    /// # Errors
    /// The config does not deserialize.
    pub fn config_or_default<T: DeserializeOwned + Default>(&self) -> Result<T, ConfigError> {
        gear_config_or_default(self.config, self.gear)
    }

    /// As [`GearCtx::config_expanded_or_default`].
    ///
    /// # Errors
    /// The config does not deserialize or a variable does not expand.
    pub fn config_expanded_or_default<T>(&self) -> Result<T, ConfigError>
    where
        T: DeserializeOwned + Default + crate::var_expand::ExpandVars,
    {
        let mut cfg: T = self.config_or_default()?;
        cfg.expand_vars().map_err(|e| ConfigError::VarExpand {
            gear: self.gear.to_owned(),
            cause: e,
        })?;
        Ok(cfg)
    }
}

/// The documents one publisher intends to register, sorted by GTS id.
#[derive(Debug, Clone, PartialEq)]
pub struct GtsManifest {
    publisher_name: String,
    publisher_version: String,
    documents: Vec<(String, serde_json::Value)>,
}

impl GtsManifest {
    /// Merge `sources` into a manifest. An id declared twice with the same
    /// canonical content is kept once.
    ///
    /// # Errors
    /// Invalid JSON, or an id declared with different content.
    pub fn build(
        publisher_name: impl Into<String>,
        publisher_version: impl Into<String>,
        sources: impl IntoIterator<Item = DeclarationSource>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            publisher_name: publisher_name.into(),
            publisher_version: publisher_version.into(),
            documents: toolkit_gts::local::merge_declarations(sources)?,
        })
    }

    #[must_use]
    pub fn publisher_name(&self) -> &str {
        &self.publisher_name
    }

    #[must_use]
    pub fn publisher_version(&self) -> &str {
        &self.publisher_version
    }

    /// `(GTS id, authored document)`, sorted by id.
    #[must_use]
    pub fn documents(&self) -> &[(String, serde_json::Value)] {
        &self.documents
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

/// Check that no GTS id is in the manifests of two publishers. One process,
/// or one deployment job, gives each id one owner; equal content does not
/// make a second owner valid.
///
/// # Errors
/// Lists every id with its publishers.
pub fn check_ownership<'a>(
    manifests: impl IntoIterator<Item = &'a GtsManifest>,
) -> anyhow::Result<()> {
    let mut owners: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for manifest in manifests {
        for (id, _) in manifest.documents() {
            owners
                .entry(id)
                .or_default()
                .push(manifest.publisher_name());
        }
    }
    let conflicts: Vec<String> = owners
        .into_iter()
        .filter(|(_, publishers)| publishers.len() > 1)
        .map(|(id, mut publishers)| {
            publishers.sort_unstable();
            format!("{id} is published by {}", publishers.join(" and "))
        })
        .collect();
    if conflicts.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "GTS ids with more than one publisher:\n- {}",
            conflicts.join("\n- ")
        )
    }
}

/// What a [`GtsPublicationBackend`] receives for one publication.
pub struct GtsPublicationRequest {
    manifest: Arc<GtsManifest>,
    cancel: CancellationToken,
    progress: Arc<dyn Fn(&str) + Send + Sync>,
}

impl GtsPublicationRequest {
    #[must_use]
    pub fn new(
        manifest: Arc<GtsManifest>,
        cancel: CancellationToken,
        progress: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        Self {
            manifest,
            cancel,
            progress,
        }
    }

    #[must_use]
    pub fn gts_manifest(&self) -> &GtsManifest {
        &self.manifest
    }

    #[must_use]
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Record why the publication is not complete yet.
    pub fn report_pending(&self, detail: &str) {
        (self.progress)(detail);
    }
}

type GtsPublishFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;

/// Executes publications: the boundary between the executor and a publisher
/// adapter. The stateless backend calls the adapter directly; a durable
/// backend can wrap the same adapter.
#[derive(Clone)]
pub struct GtsPublicationBackend(
    Arc<dyn Fn(GtsPublicationRequest) -> GtsPublishFuture + Send + Sync>,
);

impl GtsPublicationBackend {
    pub fn new<F, Fut>(publish: F) -> Self
    where
        F: Fn(GtsPublicationRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        Self(Arc::new(move |request| Box::pin(publish(request))))
    }

    /// Publish until admitted. `Err` is a terminal refusal or cancellation.
    ///
    /// # Errors
    /// See above.
    pub async fn publish(&self, request: GtsPublicationRequest) -> anyhow::Result<()> {
        (self.0)(request).await
    }
}

impl fmt::Debug for GtsPublicationBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GtsPublicationBackend")
    }
}

/// An `entities` function: config-only documents as `(GTS id, document)`.
pub type GtsEntitiesFn =
    fn(&GtsPublicationInputs<'_>) -> anyhow::Result<Vec<(String, serde_json::Value)>>;

/// Static description of a gear's publication, generated by
/// `#[toolkit::gear(gts(...))]`. The runtime uses it before `init`, and a
/// deployment job without the gear instance: neither calls `init`.
pub struct GtsPublicationDescriptor {
    /// The gear name, which is also the publisher name.
    pub gear: &'static str,
    /// The publisher version: the version of the gear's crate.
    pub publisher_version: &'static str,
    /// The crate-local collectors of the `crates` list.
    pub declarations: fn() -> Vec<DeclarationSource>,
    /// The optional config-only `entities` function.
    pub entities: Option<GtsEntitiesFn>,
    /// Binds the adapter to the registry client in `hub`.
    pub backend: fn(&ClientHub) -> anyhow::Result<GtsPublicationBackend>,
    /// The registry provider's directory name.
    pub provider: &'static str,
    /// Registers the adapter's registry client in a hub, as proxy wiring does;
    /// a deployment job calls it with its explicit registry endpoint.
    pub wire: crate::discovery::WireFn,
}

impl GtsPublicationDescriptor {
    /// The manifest for the effective configuration `config`.
    ///
    /// # Errors
    /// The `entities` function fails, or the declarations conflict.
    pub fn build_gts_manifest(&self, config: &dyn ConfigProvider) -> anyhow::Result<GtsManifest> {
        let mut sources = (self.declarations)();
        if let Some(entities) = self.entities {
            let inputs = GtsPublicationInputs::new(self.gear, config);
            let documents = entities(&inputs)?
                .into_iter()
                .map(|(id, document)| (id, document.to_string()))
                .collect();
            sources.push(DeclarationSource::new("entities", documents));
        }
        GtsManifest::build(self.gear, self.publisher_version, sources)
    }
}

impl fmt::Debug for GtsPublicationDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GtsPublicationDescriptor")
            .field("gear", &self.gear)
            .field("publisher_version", &self.publisher_version)
            .finish_non_exhaustive()
    }
}

/// Every linked publication, for a deployment job.
pub struct GtsPublicationRegistration(pub &'static GtsPublicationDescriptor);

inventory::collect!(GtsPublicationRegistration);

/// Every publication descriptor linked into this binary, sorted by gear.
#[must_use]
pub fn linked_descriptors() -> Vec<&'static GtsPublicationDescriptor> {
    let mut out: Vec<_> = inventory::iter::<GtsPublicationRegistration>
        .into_iter()
        .map(|r| r.0)
        .collect();
    out.sort_by_key(|d| d.gear);
    out
}

/// The runtime side of a gear's publication, generated by
/// `#[toolkit::gear(gts(...))]`.
pub trait GtsPublicationCapability: Send + Sync {
    fn descriptor(&self) -> &'static GtsPublicationDescriptor;
}

/// Why a publication did not complete.
#[derive(Debug, thiserror::Error)]
pub enum GtsPublicationError {
    /// The manifest could not be built from the configuration.
    #[error("{gear}: GTS manifest: {source:#}")]
    GtsManifest {
        gear: &'static str,
        source: anyhow::Error,
    },
    /// The publisher adapter could not bind its registry client.
    #[error("{gear}: GTS publisher: {source:#}")]
    Backend {
        gear: &'static str,
        source: anyhow::Error,
    },
    /// The registry refused the publication.
    #[error("{gear}: GTS publication refused: {source:#}")]
    Refused {
        gear: &'static str,
        source: anyhow::Error,
    },
    /// Cancelled before admission.
    #[error("{gear}: GTS publication cancelled")]
    Cancelled { gear: &'static str },
}

/// A gear's publication, ready to run: the manifest and bound backend.
/// Every publication is prepared before any runs, so a manifest or wiring
/// error fails before the first registry write.
pub struct PreparedGtsPublication {
    gear: &'static str,
    manifest: Arc<GtsManifest>,
    /// `None` for an empty manifest: nothing to send.
    backend: Option<GtsPublicationBackend>,
}

impl PreparedGtsPublication {
    /// Build the manifest from `config` and bind the backend from `hub`.
    ///
    /// # Errors
    /// [`GtsPublicationError::GtsManifest`] or [`GtsPublicationError::Backend`].
    pub fn prepare(
        descriptor: &'static GtsPublicationDescriptor,
        config: &dyn ConfigProvider,
        hub: &ClientHub,
    ) -> Result<Self, GtsPublicationError> {
        let gear = descriptor.gear;
        let manifest = descriptor
            .build_gts_manifest(config)
            .map_err(|source| GtsPublicationError::GtsManifest { gear, source })?;
        Self::bind(descriptor, Arc::new(manifest), hub)
    }

    /// Bind the backend of an already built `manifest` from `hub`. An empty
    /// manifest needs no backend, so no registry.
    ///
    /// # Errors
    /// [`GtsPublicationError::Backend`].
    pub fn bind(
        descriptor: &'static GtsPublicationDescriptor,
        manifest: Arc<GtsManifest>,
        hub: &ClientHub,
    ) -> Result<Self, GtsPublicationError> {
        let gear = descriptor.gear;
        let backend = if manifest.is_empty() {
            None
        } else {
            Some(
                (descriptor.backend)(hub)
                    .map_err(|source| GtsPublicationError::Backend { gear, source })?,
            )
        };
        Ok(Self {
            gear,
            manifest,
            backend,
        })
    }

    #[must_use]
    pub fn gear(&self) -> &'static str {
        self.gear
    }

    #[must_use]
    pub fn gts_manifest(&self) -> &GtsManifest {
        &self.manifest
    }

    /// The manifest, shared.
    #[must_use]
    pub fn shared_gts_manifest(&self) -> Arc<GtsManifest> {
        Arc::clone(&self.manifest)
    }

    /// Publish until admitted or refused. An empty manifest succeeds at once.
    ///
    /// # Errors
    /// [`GtsPublicationError::Refused`] or [`GtsPublicationError::Cancelled`].
    pub async fn publish(
        &self,
        cancel: &CancellationToken,
        progress: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<(), GtsPublicationError> {
        let gear = self.gear;
        if cancel.is_cancelled() {
            return Err(GtsPublicationError::Cancelled { gear });
        }
        let Some(backend) = &self.backend else {
            return Ok(());
        };
        let request =
            GtsPublicationRequest::new(Arc::clone(&self.manifest), cancel.clone(), progress);
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(GtsPublicationError::Cancelled { gear }),
            result = backend.publish(request) => result,
        };
        match result {
            Ok(()) => Ok(()),
            Err(_) if cancel.is_cancelled() => Err(GtsPublicationError::Cancelled { gear }),
            Err(source) => Err(GtsPublicationError::Refused { gear, source }),
        }
    }
}

/// Reports why a gear's publication is not admitted yet: `(gear, detail)`.
pub type GtsPublicationProgress = Arc<dyn Fn(&'static str, &str) + Send + Sync>;

/// Publish every prepared publication concurrently: a derived-type publisher
/// waiting for its base never delays the base publisher. Returns one result
/// per publication, in the order of `prepared`, once each one ended.
///
/// With `stop_on_refusal`, the first refusal cancels the others: the batch
/// failed and their admission no longer matters. The publications run in
/// this future, not spawned: dropping it, or cancelling `cancel`, ends them.
pub async fn publish_all(
    prepared: &[PreparedGtsPublication],
    cancel: &CancellationToken,
    progress: GtsPublicationProgress,
    stop_on_refusal: bool,
) -> Vec<(&'static str, Result<(), GtsPublicationError>)> {
    use futures_util::StreamExt as _;

    let stop = cancel.child_token();
    let mut running: futures_util::stream::FuturesUnordered<_> = prepared
        .iter()
        .enumerate()
        .map(|(index, publication)| {
            let stop = stop.clone();
            let gear = publication.gear();
            let progress = Arc::clone(&progress);
            async move {
                let report: Arc<dyn Fn(&str) + Send + Sync> =
                    Arc::new(move |detail: &str| progress(gear, detail));
                (index, publication.publish(&stop, report).await)
            }
        })
        .collect();
    let mut results: Vec<Option<Result<(), GtsPublicationError>>> =
        prepared.iter().map(|_| None).collect();
    while let Some((index, result)) = running.next().await {
        if stop_on_refusal && matches!(result, Err(GtsPublicationError::Refused { .. })) {
            stop.cancel();
        }
        results[index] = Some(result);
    }
    prepared
        .iter()
        .zip(results)
        .map(|(publication, result)| {
            let gear = publication.gear();
            (
                gear,
                result.unwrap_or(Err(GtsPublicationError::Cancelled { gear })),
            )
        })
        .collect()
}

#[path = "gts_publication_deployment.rs"]
pub mod deployment;

#[cfg(test)]
#[path = "gts_publication_tests.rs"]
mod gts_publication_tests;
