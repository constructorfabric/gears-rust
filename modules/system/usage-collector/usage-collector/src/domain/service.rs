//! Domain service for the usage-collector gateway.
//!
//! Resolves the configured GTS storage plugin lazily on first call,
//! wraps each plugin invocation in a [`CircuitBreaker`] with a per-call
//! timeout, and exposes ingest, module-config, and query operations
//! returning [`DomainError`].
//!
//! For the query API, [`Service::query_aggregated`] and [`Service::query_raw`]
//! perform the gateway-side PDP authorization (`authorize_and_compile_scope`)
//! and embed the resulting [`modkit_security::AccessScope`] into the query
//! before delegating the plugin call.

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::AuthZResolverClient;
use modkit::client_hub::{ClientHub, ClientScope};
use modkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use modkit::telemetry::ThrottledLog;
use modkit_macros::domain_model;
use modkit_security::{AccessScope, SecurityContext};
use tokio::time::timeout;
use tracing::{error, info, warn};
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};
use usage_collector_sdk::authz::{USAGE_RECORD, actions};
use usage_collector_sdk::{
    AggregationResult, AllowedMetric, ModuleConfig, Page, UsageCollectorError,
    UsageCollectorPluginClientV1, UsageCollectorPluginSpecV1, UsageRecord, UsageRecordError,
};

use super::authz::authorize_and_compile_scope;
use super::circuit_breaker::CircuitBreaker;
use super::error::DomainError;
use super::query::{AggregationQueryRequest, RawQueryRequest};
use crate::config::UsageCollectorConfig;

/// Throttle interval for unavailable plugin warnings.
const UNAVAILABLE_LOG_THROTTLE: Duration = Duration::from_secs(10);

/// Usage-collector gateway domain service.
#[domain_model]
pub struct Service {
    hub: Arc<ClientHub>,
    config: UsageCollectorConfig,
    selector: GtsPluginSelector,
    unavailable_log_throttle: ThrottledLog,
    circuit_breaker: CircuitBreaker,
    authz: Arc<dyn AuthZResolverClient>,
}

impl Service {
    #[must_use]
    pub fn new(
        config: UsageCollectorConfig,
        hub: Arc<ClientHub>,
        authz: Arc<dyn AuthZResolverClient>,
    ) -> Self {
        let circuit_breaker = CircuitBreaker::new(config.circuit_breaker.clone());
        Self {
            hub,
            config,
            selector: GtsPluginSelector::new(),
            unavailable_log_throttle: ThrottledLog::new(UNAVAILABLE_LOG_THROTTLE),
            circuit_breaker,
            authz,
        }
    }

    /// Lazily resolves and returns the storage plugin client.
    ///
    /// # Errors
    ///
    /// - [`DomainError::PluginNotFound`] if no plugin matches the configured vendor.
    /// - [`DomainError::PluginUnavailable`] if the plugin client is not yet registered.
    /// - [`DomainError::TypesRegistryUnavailable`] if the registry call fails.
    async fn get_plugin(&self) -> Result<Arc<dyn UsageCollectorPluginClientV1>, DomainError> {
        let instance_id = self.selector.get_or_init(|| self.resolve_plugin()).await?;
        let scope = ClientScope::gts_id(instance_id.as_ref());

        if let Some(client) = self
            .hub
            .try_get_scoped::<dyn UsageCollectorPluginClientV1>(&scope)
        {
            Ok(client)
        } else {
            if self.unavailable_log_throttle.should_log() {
                warn!(
                    plugin_gts_id = %instance_id,
                    vendor = %self.config.vendor,
                    "Plugin client not registered yet"
                );
            }
            Err(DomainError::PluginUnavailable {
                gts_id: instance_id.to_string(),
                reason: "client not registered yet".into(),
            })
        }
    }

    #[tracing::instrument(skip_all, fields(vendor = %self.config.vendor))]
    async fn resolve_plugin(&self) -> Result<String, DomainError> {
        info!("Resolving usage-collector plugin");

        let registry = self.hub.get::<dyn TypesRegistryClient>()?;

        let plugin_type_id = UsageCollectorPluginSpecV1::gts_schema_id().clone();

        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{plugin_type_id}*")))
            .await?;

        let gts_id = choose_plugin_instance::<UsageCollectorPluginSpecV1>(
            &self.config.vendor,
            instances.iter().map(|e| (e.id.as_ref(), &e.object)),
        )?;
        info!(plugin_gts_id = %gts_id, "Selected usage-collector plugin instance");

        Ok(gts_id)
    }

    /// Run a plugin call under the circuit breaker with the configured timeout.
    async fn call_plugin<F, Fut, T>(&self, f: F) -> Result<T, DomainError>
    where
        F: FnOnce(Arc<dyn UsageCollectorPluginClientV1>) -> Fut,
        Fut: std::future::Future<Output = Result<T, UsageCollectorError>>,
    {
        self.circuit_breaker
            .execute(|| async {
                let plugin = self.get_plugin().await?;
                match timeout(self.config.plugin_timeout, f(plugin)).await {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(canonical)) => Err(DomainError::Plugin(canonical)),
                    Err(_) => Err(DomainError::Timeout),
                }
            })
            .await
    }

    /// Forward a usage record to the storage plugin.
    ///
    /// # Errors
    ///
    /// - [`DomainError::CircuitOpen`] when the circuit breaker is rejecting calls.
    /// - [`DomainError::Timeout`] when the plugin call exceeds the configured timeout.
    /// - [`DomainError::Plugin`] when the plugin returns an error.
    /// - Plugin-resolution errors (see [`Service::get_plugin`]).
    pub async fn create_usage_record(&self, record: UsageRecord) -> Result<(), DomainError> {
        self.call_plugin(|plugin| async move { plugin.create_usage_record(record).await })
            .await
    }

    // @cpt-flow:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2
    // @cpt-algo:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2
    /// Return the static per-module metric configuration.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::ModuleNotConfigured`] when the module has no allowed metrics.
    pub fn get_module_config(&self, module_name: &str) -> Result<ModuleConfig, DomainError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-1
        // Authentication is enforced by the ModKit pipeline (`.authenticated()` in routes.rs);
        // unauthenticated requests are rejected before this function is reached.
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-1

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-2
        let allowed_metrics: Vec<AllowedMetric> = self
            .config
            .metrics
            .iter()
            .filter(|(_, cfg)| {
                cfg.modules
                    .as_ref()
                    .is_none_or(|mods| mods.iter().any(|m| m == module_name))
            })
            .map(|(name, cfg)| AllowedMetric {
                name: name.clone(),
                kind: cfg.kind,
            })
            .collect();
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-2

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-3
        if allowed_metrics.is_empty() {
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-3a
            return Err(DomainError::ModuleNotConfigured {
                module: module_name.to_owned(),
            });
            // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-3a
        }
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-3

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-4
        Ok(ModuleConfig {
            allowed_metrics,
            max_metadata_bytes: self.config.max_metadata_bytes,
        })
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config:p2:inst-cfg-p-4
    }

    /// Authorize and execute an aggregated usage query.
    ///
    /// Calls the PDP via [`authorize_and_compile_scope`] (`USAGE_RECORD`/`list`).
    /// On `Err(PermissionDenied)` the plugin is NOT invoked. On success, the
    /// compiled [`modkit_security::AccessScope`] is embedded into `query.scope`
    /// and the query is delegated to the storage plugin.
    ///
    /// # Errors
    ///
    /// - [`DomainError::PermissionDenied`] when the PDP denies the request or
    ///   returns any non-Denied error (fail-closed; see `inst-authz-3a`/`-3b`).
    /// - Plugin-call errors: [`DomainError::Timeout`], [`DomainError::Plugin`],
    ///   [`DomainError::CircuitOpen`], and plugin-resolution errors.
    pub async fn query_aggregated(
        &self,
        ctx: &SecurityContext,
        request: AggregationQueryRequest,
    ) -> Result<Vec<AggregationResult>, DomainError> {
        let scope = self.authorize_query(ctx).await?;
        let query = request.with_scope(scope);
        self.call_plugin(|plugin| async move { plugin.query_aggregated(query).await })
            .await
    }

    /// Authorize and execute a raw paginated usage query.
    ///
    /// Calls the PDP via [`authorize_and_compile_scope`] (`USAGE_RECORD`/`list`).
    /// On `Err(PermissionDenied)` the plugin is NOT invoked. On success, the
    /// compiled [`modkit_security::AccessScope`] is embedded into `query.scope`
    /// and the query is delegated to the storage plugin.
    ///
    /// # Errors
    ///
    /// - [`DomainError::PermissionDenied`] when the PDP denies the request or
    ///   returns any non-Denied error (fail-closed; see `inst-authz-3a`/`-3b`).
    /// - Plugin-call errors: [`DomainError::Timeout`], [`DomainError::Plugin`],
    ///   [`DomainError::CircuitOpen`], and plugin-resolution errors.
    pub async fn query_raw(
        &self,
        ctx: &SecurityContext,
        request: RawQueryRequest,
    ) -> Result<Page<UsageRecord>, DomainError> {
        let scope = self.authorize_query(ctx).await?;
        let query = request.with_scope(scope);
        self.call_plugin(|plugin| async move { plugin.query_raw(query).await })
            .await
    }

    /// Run the gateway-side PDP authorization for the query API and return the
    /// compiled [`modkit_security::AccessScope`]. Shared between
    /// [`Service::query_aggregated`] and [`Service::query_raw`] so the
    /// fail-closed / `USAGE_RECORD`+`LIST` audit shape stays in lockstep.
    ///
    /// The PDP call is wrapped in `tokio::time::timeout(self.config.authz_timeout, …)`
    /// so a slow or hung `AuthZResolverClient` cannot hang the request task.
    /// Timeout elapsed is treated as a non-Denied PDP error and maps to
    /// `PermissionDenied` (fail-closed; see `inst-authz-3b`).
    async fn authorize_query(&self, ctx: &SecurityContext) -> Result<AccessScope, DomainError> {
        let call =
            authorize_and_compile_scope(ctx, Arc::clone(&self.authz), &USAGE_RECORD, actions::LIST);
        if let Ok(result) = timeout(self.config.authz_timeout, call).await {
            result.map_err(DomainError::PermissionDenied)
        } else {
            error!(
                subject_id = %ctx.subject_id(),
                authz_timeout_ms = u64::try_from(self.config.authz_timeout.as_millis()).unwrap_or(u64::MAX),
                "PDP call timed out; access denied (fail-closed)",
            );
            Err(DomainError::PermissionDenied(
                UsageRecordError::permission_denied()
                    .with_reason("AUTHORIZATION_DENIED")
                    .create(),
            ))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
