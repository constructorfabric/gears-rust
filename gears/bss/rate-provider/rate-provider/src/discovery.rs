//! `DiscoveringRateProvider` — the composite the ledger resolves.
//!
//! Lazily discovers the registered source plugins (types-registry instances of
//! `RateProviderSourcePluginSpecV1`, filtered by vendor), orders them by
//! `priority` (lower = tried first), resolves each scoped `RateProviderV1` from
//! `ClientHub`, and delegates ordered fallback to a [`CompositeRateProvider`].
//!
//! Discovery runs on the FIRST `fetch_latest` and is cached in a `OnceCell`. That
//! call happens in the ledger's serve phase — after every gear's `init()`, so all
//! source plugins have registered by then; no init-ordering constraint is needed.
//! A failed discovery is NOT cached (the `OnceCell` stays empty), so a later tick
//! retries and self-heals. `provider_id()` delegates to the cached composite once
//! warm and otherwise returns the configured id (never `"none"`).

use std::sync::Arc;

use async_trait::async_trait;
use bss_ledger_sdk::{CurrencyPair, ProviderRate, RateProviderError, RateProviderV1};
use bss_rate_provider_sdk::RateProviderSourcePluginSpecV1;
use tokio::sync::OnceCell;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit_security::SecurityContext;
use types_registry_sdk::{GtsInstance, InstanceQuery, TypesRegistryClient};

use crate::composite::CompositeRateProvider;

/// A `RateProviderV1` that composes the discovered source plugins on demand.
pub struct DiscoveringRateProvider {
    hub: Arc<ClientHub>,
    source_vendor: String,
    id: String,
    composite: OnceCell<CompositeRateProvider>,
}

impl DiscoveringRateProvider {
    /// Build over the shared `ClientHub`, the source-plugin vendor filter, and the
    /// pre-discovery `provider_id` fallback.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, source_vendor: String, id: String) -> Self {
        Self {
            hub,
            source_vendor,
            id,
            composite: OnceCell::new(),
        }
    }

    /// Get (or lazily build + cache) the composite over the discovered sources.
    async fn composite(&self) -> Result<&CompositeRateProvider, RateProviderError> {
        self.composite.get_or_try_init(|| self.discover()).await
    }

    /// Discover source plugins from types-registry, order by priority, and build
    /// the composite.
    ///
    /// # Errors
    /// [`RateProviderError`] if types-registry is unavailable or no source plugin
    /// is registered for the configured vendor.
    async fn discover(&self) -> Result<CompositeRateProvider, RateProviderError> {
        let registry = self
            .hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| RateProviderError::Internal(format!("types-registry unavailable: {e}")))?;
        let type_id = RateProviderSourcePluginSpecV1::gts_type_id();
        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{type_id}*")))
            .await
            .map_err(|e| {
                RateProviderError::Unreachable(format!("list rate-provider source plugins: {e}"))
            })?;

        let mut chosen = self.select_matching_sources(&instances);
        chosen.sort_by_key(|(priority, _)| *priority);
        warn_on_duplicate_priorities(&chosen);

        let sources: Vec<Arc<dyn RateProviderV1>> =
            chosen.into_iter().map(|(_, src)| src).collect();
        if sources.is_empty() {
            return Err(RateProviderError::Unreachable(
                "no FX rate-provider source plugins registered for vendor".to_owned(),
            ));
        }
        Ok(CompositeRateProvider::new(sources))
    }

    /// Read vendor + priority straight off each `PluginV1` instance JSON, keep
    /// the matching-vendor sources, and resolve their scoped clients. A vendor
    /// match with no scoped client registered is logged and excluded, not
    /// fatal — unless it drops the whole set to empty, in which case
    /// `discover` reports that.
    fn select_matching_sources(
        &self,
        instances: &[GtsInstance],
    ) -> Vec<(i16, Arc<dyn RateProviderV1>)> {
        let mut chosen = Vec::new();
        for inst in instances {
            if inst
                .object
                .get("vendor")
                .and_then(serde_json::Value::as_str)
                != Some(self.source_vendor.as_str())
            {
                continue;
            }
            let priority = inst
                .object
                .get("priority")
                .and_then(serde_json::Value::as_i64)
                .and_then(|p| i16::try_from(p).ok())
                .unwrap_or(i16::MAX);
            let scope = ClientScope::gts_id(inst.id.as_ref());
            if let Some(src) = self.hub.try_get_scoped::<dyn RateProviderV1>(&scope) {
                chosen.push((priority, src));
            } else {
                tracing::warn!(
                    instance_id = %inst.id.as_ref(),
                    "bss-rate-provider: source plugin instance has no scoped RateProviderV1 \
                     client registered; excluding it from the fallback chain"
                );
            }
        }
        chosen
    }
}

/// A duplicate `priority` doesn't fail discovery — `sort_by_key` is stable, so
/// ties keep whatever order `list_instances` returned them in — but it's
/// logged, since fallback order silently depending on registry return order is
/// worth knowing about. `chosen` must already be sorted by priority.
fn warn_on_duplicate_priorities(chosen: &[(i16, Arc<dyn RateProviderV1>)]) {
    for pair in chosen.windows(2) {
        if pair[0].0 == pair[1].0 {
            tracing::warn!(
                priority = pair[0].0,
                "bss-rate-provider: two or more source plugins share the same priority; \
                 fallback order among them is not deterministic"
            );
        }
    }
}

#[async_trait]
impl RateProviderV1 for DiscoveringRateProvider {
    fn provider_id(&self) -> &str {
        match self.composite.get() {
            Some(composite) => composite.provider_id(),
            None => &self.id,
        }
    }

    async fn fetch_latest(
        &self,
        ctx: &SecurityContext,
        pairs: &[CurrencyPair],
        request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        self.composite()
            .await?
            .fetch_latest(ctx, pairs, request_id)
            .await
    }

    async fn health(
        &self,
        ctx: &SecurityContext,
        request_id: &str,
    ) -> Result<(), RateProviderError> {
        self.composite().await?.health(ctx, request_id).await
    }
}
