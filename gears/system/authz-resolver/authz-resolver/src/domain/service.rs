//! Domain service for the `AuthZ` resolver.
//!
//! Plugin discovery is lazy: resolved on the first API call, over the
//! database-backed catalogue of `PlatformTypesRegistryApi`.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::{
    AuthZResolverPluginClient, AuthZResolverPluginSpecV1, EvaluationBatchRequest,
    EvaluationBatchResponse, EvaluationRequest, EvaluationResponse, check_batch_results,
};
use tokio::sync::Semaphore;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_registered_plugin_instance, is_discovery_outage};
use toolkit::telemetry::ThrottledLog;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::PlatformSecurityContext;
use tracing::info;
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};

use super::error::DomainError;
use crate::config::{BatchConfig, ConfigError};

/// Throttle interval for unavailable plugin warnings.
const UNAVAILABLE_LOG_THROTTLE: Duration = Duration::from_secs(10);

/// Bounds cold catalogue listing and concurrent selection waits, including REST
/// page retries. Expiry returns a retryable registry outage without caching.
const DISCOVERY_BUDGET: Duration = Duration::from_secs(30);

/// `AuthZ` resolver service.
///
/// The selection is process-wide, so it is read on the platform plane: no tenant
/// credential is needed to find the plugin that evaluates a request.
#[domain_model]
pub struct Service {
    hub: Arc<ClientHub>,
    vendor: String,
    selector: GtsPluginSelector,
    unavailable_log_throttle: ThrottledLog,
    batch: BatchLimits,
}

/// Batch bounds applied by [`Service::evaluate_batch`].
struct BatchLimits {
    max_items: usize,
    max_concurrency: NonZeroUsize,
    deadline: Duration,
    /// Process-wide bound on item evaluations in flight across all batches.
    in_flight: Semaphore,
}

impl BatchLimits {
    /// Limits from a configuration that has passed [`BatchConfig::validate`]
    /// (so `max_concurrency >= 1` and `max_in_flight` fits a semaphore).
    fn from_validated(cfg: &BatchConfig) -> Self {
        Self {
            max_items: cfg.max_items,
            max_concurrency: NonZeroUsize::new(cfg.max_concurrency).unwrap_or(NonZeroUsize::MIN),
            deadline: cfg.deadline,
            in_flight: Semaphore::new(cfg.max_in_flight),
        }
    }
}

impl Service {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, vendor: String) -> Self {
        Self {
            hub,
            vendor,
            selector: GtsPluginSelector::new(),
            unavailable_log_throttle: ThrottledLog::new(UNAVAILABLE_LOG_THROTTLE),
            // The defaults are valid (`config_tests` pins it).
            batch: BatchLimits::from_validated(&BatchConfig::default()),
        }
    }

    /// Apply batch bounds (the default [`BatchConfig`] otherwise).
    ///
    /// # Errors
    ///
    /// The first out-of-range key of `cfg`; nothing is clamped.
    pub fn with_batch_config(mut self, cfg: &BatchConfig) -> Result<Self, ConfigError> {
        cfg.validate()?;
        self.batch = BatchLimits::from_validated(cfg);
        Ok(self)
    }

    async fn get_plugin(&self) -> Result<Arc<dyn AuthZResolverPluginClient>, DomainError> {
        let instance_id = self
            .selector
            .get_or_init_with_timeout(
                DISCOVERY_BUDGET,
                || self.resolve_plugin(),
                DomainError::TypesRegistryUnavailable,
            )
            .await?;
        let scope = ClientScope::gts_id(instance_id.as_ref());

        if let Some(client) = self
            .hub
            .try_get_scoped::<dyn AuthZResolverPluginClient>(&scope)
        {
            Ok(client)
        } else {
            if self.unavailable_log_throttle.should_log() {
                tracing::warn!(
                    plugin_gts_id = %instance_id,
                    vendor = %self.vendor,
                    "Plugin client not registered yet"
                );
            }
            Err(DomainError::PluginUnavailable {
                gts_id: instance_id.to_string(),
                reason: "client not registered yet".into(),
            })
        }
    }

    /// Resolves the plugin instance from types-registry.
    ///
    /// List every page and keep only locally registered scoped clients, excluding
    /// stale instances from earlier boots. Read failures are returned without caching.
    #[tracing::instrument(skip_all, fields(vendor = %self.vendor))]
    async fn resolve_plugin(&self) -> Result<String, DomainError> {
        info!("Resolving authz_resolver plugin");

        let registry = self
            .hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))?;

        let plugin_type_id = AuthZResolverPluginSpecV1::gts_type_id();
        let pattern = GtsIdPattern::try_new(plugin_type_id.as_ref())
            .map_err(|e| DomainError::Internal(e.to_string()))?;
        let instances = registry
            .list_instances(
                &PlatformSecurityContext::outbound_marker(),
                ListEntitiesRequest::matching(pattern),
            )
            .await
            .map_err(registry_failure)?;

        let gts_id = choose_registered_plugin_instance::<
            AuthZResolverPluginSpecV1,
            dyn AuthZResolverPluginClient,
        >(
            &self.hub,
            &self.vendor,
            instances
                .iter()
                .map(|instance| (instance.id.as_ref(), instance.content.as_ref())),
        )?;
        info!(plugin_gts_id = %gts_id, "Selected authz_resolver plugin instance");

        Ok(gts_id)
    }

    /// Evaluate an authorization request via the selected plugin.
    ///
    /// # Errors
    ///
    /// - Plugin resolution errors
    /// - Plugin evaluation errors
    #[tracing::instrument(skip_all)]
    pub async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, DomainError> {
        let plugin = self.get_plugin().await?;
        plugin.evaluate(request).await.map_err(DomainError::from)
    }

    /// Evaluate a batch via the selected plugin (`AuthZEN` Access Evaluations).
    ///
    /// Under one deadline: resolve the defaults and check the item limit, then
    /// select the plugin exactly as for a single evaluation, run the plugin's
    /// batch with every in-flight item holding a process-wide permit, and
    /// check the shape of the results. Any failure fails the whole batch; no
    /// partial results are returned. Tracing records counts only, never
    /// subjects, properties, token scopes or bearer tokens.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidBatch`] for an empty batch, too many items or an
    ///   unresolvable key (no plugin lookup happens)
    /// - Plugin resolution errors
    /// - Plugin evaluation errors, for the batch or any item
    /// - [`DomainError::MalformedBatchResults`] when the results do not have the
    ///   shape the semantic allows or a denial lacks an error code
    /// - [`DomainError::BatchDeadlineExceeded`] when the deadline expires
    #[tracing::instrument(
        skip_all,
        fields(
            items = request.evaluations.len(),
            semantic = tracing::field::Empty,
            evaluated = tracing::field::Empty,
            allowed = tracing::field::Empty,
            denied = tracing::field::Empty,
        )
    )]
    pub async fn evaluate_batch(
        &self,
        request: EvaluationBatchRequest,
    ) -> Result<EvaluationBatchResponse, DomainError> {
        let deadline = self.batch.deadline;
        match tokio::time::timeout(deadline, self.run_batch(request)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(DomainError::BatchDeadlineExceeded(deadline)),
        }
    }

    // cancel-safe: dropped by the batch deadline or a caller disconnect, this
    // releases its in-flight permits through RAII and abandons only read-only
    // plugin evaluations; nothing is written.
    async fn run_batch(
        &self,
        request: EvaluationBatchRequest,
    ) -> Result<EvaluationBatchResponse, DomainError> {
        let span = tracing::Span::current();
        let (requests, semantic) = request
            .resolve(Some(self.batch.max_items))
            .map_err(DomainError::InvalidBatch)?;
        span.record("semantic", tracing::field::debug(semantic));
        let items = requests.len();

        let plugin = self.get_plugin().await?;

        // The plugin runs at most `max_concurrency` items of this batch at
        // once, and each item holds one `in_flight` permit only while it runs,
        // so freed permits go to the next waiting item of any batch. Waiting
        // for a permit counts against the batch deadline.
        let evaluations = plugin
            .evaluate_batch(
                requests,
                semantic,
                self.batch.max_concurrency,
                &self.batch.in_flight,
            )
            .await
            .map_err(DomainError::from)?;
        check_batch_results(&evaluations, items, semantic)
            .map_err(DomainError::MalformedBatchResults)?;

        let allowed = evaluations.iter().filter(|r| r.decision).count();
        span.record("evaluated", evaluations.len());
        span.record("allowed", allowed);
        span.record("denied", evaluations.len() - allowed);
        Ok(EvaluationBatchResponse { evaluations })
    }
}

/// Registry availability failures map to retryable `TypesRegistryUnavailable` (503);
/// malformed pages, credential refusals and contract faults remain internal.
fn registry_failure(error: CanonicalError) -> DomainError {
    if is_discovery_outage(&error) {
        DomainError::TypesRegistryUnavailable(error.to_string())
    } else {
        DomainError::from(error)
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;

#[cfg(test)]
#[path = "service_batch_tests.rs"]
mod service_batch_tests;
