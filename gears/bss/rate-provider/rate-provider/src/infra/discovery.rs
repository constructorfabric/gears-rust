//! `DiscoveringRateProvider` — the composite the ledger resolves.
//!
//! Lazily discovers the registered source plugins (types-registry instances of
//! `RateProviderSourcePluginSpecV1`, filtered by vendor), orders them by
//! `priority` (lower = tried first), resolves each scoped `RateProviderV1` from
//! `ClientHub`, and delegates ordered fallback to a [`CompositeRateProvider`].
//!
//! Discovery re-runs on EVERY `fetch_latest`/`health` call and nothing is cached
//! between calls, so a source plugin's registration, removal, or `priority`
//! change takes effect on the very next ledger sync tick with no gear restart.
//! Cheap enough to do unconditionally: one `list_instances` call plus N in-memory
//! `ClientHub` lookups, on a background job that ticks hourly by default and
//! never runs on the posting path.

use std::sync::Arc;

use async_trait::async_trait;
use bss_ledger_sdk::{CurrencyPair, ProviderRate, RateProviderError, RateProviderV1};
use bss_rate_provider_sdk::RateProviderSourcePluginSpecV1;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit_security::SecurityContext;
use types_registry_sdk::{GtsInstance, InstanceQuery, TypesRegistryClient};

use crate::domain::composite::CompositeRateProvider;

/// A `RateProviderV1` that composes the discovered source plugins on demand.
pub struct DiscoveringRateProvider {
    hub: Arc<ClientHub>,
    source_vendor: String,
    id: String,
}

/// A source that will take part in the fallback chain.
struct ChosenSource {
    /// Lower is tried first.
    priority: i16,
    /// The plugin's GTS instance id — the tiebreak when two sources share a
    /// `priority`. Registry list order is not a stable contract, so ordering on it
    /// would let the serving source (and the stored provenance) change between
    /// ticks with no configuration change at all.
    instance_id: String,
    client: Arc<dyn RateProviderV1>,
}

/// One discovery scan: the usable sources, plus the vendors that were filtered
/// out — carried so an empty result can name what the registry actually held,
/// not only what was missing.
struct SourceScan {
    /// Matching sources, unsorted.
    chosen: Vec<ChosenSource>,
    /// Distinct `vendor` values seen on instances dropped by the vendor filter.
    excluded_vendors: Vec<String>,
}

impl DiscoveringRateProvider {
    /// Build over the shared `ClientHub`, the source-plugin vendor filter, and
    /// this composite's own stable id.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, source_vendor: String, id: String) -> Self {
        Self {
            hub,
            source_vendor,
            id,
        }
    }

    /// Discover source plugins from types-registry, order by priority, and build
    /// the composite for this call.
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

        let scan = self.select_matching_sources(&instances);
        let mut chosen = scan.chosen;
        // Instance id as the secondary key, so equal priorities do not fall back
        // to whatever order `list_instances` happened to return.
        chosen.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.instance_id.cmp(&b.instance_id))
        });
        warn_on_duplicate_priorities(&chosen);

        let sources: Vec<Arc<dyn RateProviderV1>> = chosen.into_iter().map(|c| c.client).collect();
        if sources.is_empty() {
            return Err(RateProviderError::Unreachable(no_sources_message(
                &self.source_vendor,
                instances.len(),
                &scan.excluded_vendors,
            )));
        }
        Ok(CompositeRateProvider::new(self.id.clone(), sources))
    }

    /// Deserialize each instance as a typed `PluginV1` to read its `vendor` and
    /// `priority`, keep the matching-vendor sources, and resolve their scoped
    /// clients.
    ///
    /// Typed rather than ad-hoc `serde_json::Value` field pokes: this and the
    /// ledger's own `toolkit::plugins::choose_plugin_instance` consume the same
    /// wire format, so reading it through the same struct keeps the two from
    /// drifting, and a format change surfaces as a logged per-instance anomaly
    /// instead of silently filtering every source out.
    ///
    /// Nothing here is fatal on its own — a malformed instance, a vendor mismatch,
    /// or a vendor match with no scoped client is logged and excluded. `discover`
    /// reports a failure only if that empties the whole set.
    fn select_matching_sources(&self, instances: &[GtsInstance]) -> SourceScan {
        let mut chosen = Vec::new();
        let mut excluded_vendors: Vec<String> = Vec::new();
        for inst in instances {
            match self.classify(inst) {
                Candidate::Source(priority, src) => chosen.push(ChosenSource {
                    priority,
                    instance_id: inst.id.as_ref().to_owned(),
                    client: src,
                }),
                // Deduplicated: N plugins under one wrong vendor should read as
                // one wrong vendor in the error, not as N repetitions.
                Candidate::OtherVendor(vendor) => {
                    if !excluded_vendors.contains(&vendor) {
                        excluded_vendors.push(vendor);
                    }
                }
                Candidate::Excluded => {}
            }
        }
        SourceScan {
            chosen,
            excluded_vendors,
        }
    }

    /// Decide what a single registry instance contributes to the scan, logging
    /// each exclusion at the point it is detected.
    fn classify(&self, inst: &GtsInstance) -> Candidate {
        let Some(spec) = parse_spec(inst) else {
            return Candidate::Excluded;
        };
        if spec.vendor != self.source_vendor {
            log_vendor_mismatch(inst, &spec.vendor, &self.source_vendor);
            return Candidate::OtherVendor(spec.vendor);
        }
        self.resolve_scoped(inst, spec.priority)
    }

    /// Look up the scoped `RateProviderV1` published alongside this instance.
    ///
    /// A published instance with no client is the partial-registration state a
    /// plugin is briefly in during startup — excluded, not fatal, so a healthy
    /// sibling still serves this tick and the next tick can pick it up.
    fn resolve_scoped(&self, inst: &GtsInstance, priority: i16) -> Candidate {
        let scope = ClientScope::gts_id(inst.id.as_ref());
        if let Some(src) = self.hub.try_get_scoped::<dyn RateProviderV1>(&scope) {
            Candidate::Source(priority, src)
        } else {
            tracing::warn!(
                instance_id = %inst.id.as_ref(),
                "bss-rate-provider: source plugin instance has no scoped RateProviderV1 \
                 client registered; excluding it from the fallback chain"
            );
            Candidate::Excluded
        }
    }
}

/// Read the instance's typed `PluginV1` envelope, or log and yield `None`.
fn parse_spec(inst: &GtsInstance) -> Option<PluginV1<RateProviderSourcePluginSpecV1>> {
    match serde_json::from_value(inst.object.clone()) {
        Ok(spec) => Some(spec),
        Err(e) => {
            tracing::warn!(
                instance_id = %inst.id.as_ref(),
                error = %e,
                "bss-rate-provider: source plugin instance does not deserialize as PluginV1; \
                 excluding it from the fallback chain"
            );
            None
        }
    }
}

/// Record a vendor-filtered instance.
///
/// Not an anomaly on its own: one registry can hold source plugins belonging to
/// another composite. `debug`, not `warn`, because discovery re-runs every tick,
/// so a warning per foreign instance per tick would be pure noise. It is still
/// logged (and the vendor still travels back to the caller) because a `vendor`
/// typo passes every startup check by design — discovery is lazy, so plugin
/// registration order never has to be enforced — leaving this the only place the
/// mismatch is visible.
fn log_vendor_mismatch(inst: &GtsInstance, vendor: &str, expected: &str) {
    tracing::debug!(
        instance_id = %inst.id.as_ref(),
        vendor,
        expected_vendor = expected,
        "bss-rate-provider: source plugin instance advertises a different vendor; \
         excluding it from the fallback chain"
    );
}

/// What one registry instance contributes to a scan.
enum Candidate {
    /// A usable source, to be ordered by its `priority`.
    Source(i16, Arc<dyn RateProviderV1>),
    /// Dropped by the vendor filter; carries the `vendor` it advertised, which
    /// is the one exclusion reason worth reporting back to the caller.
    OtherVendor(String),
    /// Dropped for a reason already logged where it was detected.
    Excluded,
}

/// Spell out what the scan saw when it found nothing usable.
///
/// The `vendor` string has to agree in three independently configured places —
/// this gear's `source_vendor`, each source plugin's `vendor`, and the ledger's
/// `fx.provider_vendor`. A typo in any of them fails no startup check, so it
/// first surfaces here, on the ledger's first rate-sync tick. Naming the expected
/// vendor and the ones actually present makes that a one-line diagnosis.
fn no_sources_message(
    expected: &str,
    instances_seen: usize,
    excluded_vendors: &[String],
) -> String {
    if excluded_vendors.is_empty() {
        format!(
            "no FX rate-provider source plugins registered for vendor {expected:?} \
             ({instances_seen} source-plugin instance(s) in the types-registry)"
        )
    } else {
        let found = excluded_vendors.join(", ");
        format!(
            "no FX rate-provider source plugins registered for vendor {expected:?}; \
             of {instances_seen} source-plugin instance(s) in the types-registry, none \
             matched — vendors present: {found}. Check that this gear's `source_vendor` \
             equals each source plugin's `vendor`."
        )
    }
}

/// A duplicate `priority` doesn't fail discovery — the instance-id tiebreak keeps
/// the order deterministic — but it is logged, because which tied source serves is
/// then decided by an id nobody chose for that purpose. `chosen` must already be
/// sorted.
fn warn_on_duplicate_priorities(chosen: &[ChosenSource]) {
    for pair in chosen.windows(2) {
        if pair[0].priority == pair[1].priority {
            tracing::warn!(
                priority = pair[0].priority,
                first = %pair[0].instance_id,
                second = %pair[1].instance_id,
                "bss-rate-provider: two or more source plugins share the same priority; \
                 order among them falls back to instance id. Set distinct priorities to \
                 choose it deliberately"
            );
        }
    }
}

#[async_trait]
impl RateProviderV1 for DiscoveringRateProvider {
    fn provider_id(&self) -> &str {
        &self.id
    }

    async fn fetch_latest(
        &self,
        ctx: &SecurityContext,
        pairs: &[CurrencyPair],
        request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        self.discover()
            .await?
            .fetch_latest(ctx, pairs, request_id)
            .await
    }

    async fn health(
        &self,
        ctx: &SecurityContext,
        request_id: &str,
    ) -> Result<(), RateProviderError> {
        self.discover().await?.health(ctx, request_id).await
    }
}
