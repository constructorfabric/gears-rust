//! Domain service for the usage-collector module.
//!
//! The `Service` is the sole owner of the lazy storage-plugin binding.
//! Plugin discovery is resolved on the first dispatch via the embedded
//! `GtsPluginSelector` (single-flight `get_or_init`); the resolved
//! `GtsInstanceId` is cached for the `Service`'s lifetime, so binding
//! changes require a module restart. The structural readiness fact
//! (selector cached AND the scoped `dyn UsageCollectorPluginV1` client is
//! registered in `ClientHub`) is computed per dispatch — the SPI exposes
//! no plugin-side `ready()` probe.
//!
//! Authorization is a direct PDP call per operation through
//! [`crate::domain::authz`]; the resource definitions and action
//! vocabularies all live there so the PEP declarations stay in one place.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use futures::StreamExt;
use futures::stream;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page as ODataPage};
use toolkit_security::SecurityContext;
use tracing::info;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient, TypesRegistryError};
use usage_collector_sdk::{
    AggregationResult, AggregationSpec, MetadataFilter, UsageCollectorError,
    UsageCollectorPluginError, UsageCollectorPluginSpecV1, UsageCollectorPluginV1, UsageRecord,
    UsageType, UsageTypeGtsId,
};
use uuid::Uuid;

use crate::domain::authz::{self, AttributionTupleKey, usage_record, usage_type};
use crate::domain::query::{compose_query_with_scope, require_bounded_time_window};
use crate::domain::validation::{
    SemanticsOutcome, validate_record_semantics, validate_submit_record_metadata,
    verify_l1_corrects_id,
};

use super::error::DomainError;

/// Maximum number of records accepted in a single `create_usage_records`
/// invocation, enforced at the SDK-facing service entry per
/// `cpt-cf-usage-collector-dod-usage-emission-nfr-batch-and-report-timing`.
/// The REST handler is a thin wrapper over this entry, so the cap is the
/// same on both surfaces; `usage-collector-v1.yaml` documents it as
/// `CreateUsageRecordsRequest.records.maxItems` on the wire.
pub const MAX_BATCH_RECORDS: usize = 100;

/// Concurrency cap for the per-distinct-attribution-tuple PDP fan-out in
/// `create_usage_records`. Sized to match the platform's established
/// external-call posture (8) so a worst-case all-distinct
/// [`MAX_BATCH_RECORDS`] batch takes `ceil(100 / 8) × PDP_RTT` wall-clock
/// without overwhelming the PDP transport pool. Bounds the
/// `inst-algo-attrib-bounded-fanout` step of
/// `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`.
const PDP_CONCURRENCY: usize = 8;

/// Concurrency cap for the per-distinct-`gts_id` `get_usage_type` SPI
/// fan-out in `create_usage_records`. Bounds plugin-side pressure for
/// the catalog lookup pre-pass; sized identically to [`PDP_CONCURRENCY`]
/// (the two fan-outs run sequentially, not concurrently, so the
/// effective in-flight ceiling against the bound storage plugin stays
/// at 8). Bounds the `inst-algo-catalog-bounded-fanout` step of
/// `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`.
const CATALOG_FANOUT_CONCURRENCY: usize = 8;

/// Concurrency cap for the per-distinct-`corrects_id` `get_usage_record`
/// L1 lookup fan-out in `create_usage_records`. Bounds plugin-side
/// pressure for the compensation referential-check pre-pass; same value
/// as [`CATALOG_FANOUT_CONCURRENCY`] because both pre-passes hit the
/// same plugin handle. Bounds the `inst-algo-semantics-l1-bounded-fanout`
/// step of
/// `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`.
const L1_LOOKUP_FANOUT_CONCURRENCY: usize = 8;

/// One PDP fan-out outcome: the input indices that share an attribution
/// tuple plus the `Result<(), DomainError>` returned for that tuple's
/// representative call. Decision projection (success / deny / unavailable
/// → per-index `results[index]` slot) reads this shape.
type PdpGroupDecision = (Vec<usize>, Result<(), DomainError>);

/// Cached catalog lookup result per distinct `gts_id`, lifted into
/// [`DomainError`] (Clone-able) so a single SPI outcome can be
/// projected to every record sharing the id without re-issuing the
/// `get_usage_type` call.
type CatalogCache = HashMap<UsageTypeGtsId, Result<UsageType, DomainError>>;

/// Cached L1 referential lookup per distinct `corrects_id`, lifted into
/// [`DomainError`] so the variant identity of
/// `UsageRecordNotFound { id }` survives the cache (and is reclassified
/// to `UsageCollectorError::NotFound` on the per-record
/// projection — same lift that the in-loop code path used).
type L1LookupCache = HashMap<Uuid, Result<UsageRecord, DomainError>>;

/// A per-record validation outcome deferred to the post-loop L1 pre-pass:
/// `(input_index, the record itself, the corrects_id to fetch)`. Records
/// only end up here when they passed PDP, the catalog cache lookup, AND
/// semantics validation reported `NeedsL1Lookup`.
type PendingL1Lookup = (usize, UsageRecord, Uuid);

/// Log a host-invariant breach (cache miss, SPI size mismatch, unfilled
/// result slot) and build the typed `Internal` returned for it, so each
/// breach site stays a one-liner and never panics the request thread.
fn invariant_breach(detail: String) -> UsageCollectorError {
    tracing::error!(detail = %detail, "usage-collector host-invariant breach");
    UsageCollectorError::internal(detail)
}

/// Collapse a PDP denial into `NotFound` so the by-id surfaces (`get` /
/// `deactivate`) never act as an existence oracle; every other error
/// (notably `ServiceUnavailable`, which leaks nothing) is preserved.
fn collapse_deny_to_not_found(
    err: impl Into<UsageCollectorError>,
    id: Uuid,
) -> UsageCollectorError {
    match err.into() {
        UsageCollectorError::PermissionDenied { .. } => {
            UsageCollectorError::usage_record_not_found(id)
        }
        other => other,
    }
}

/// Resolve every deferred L1 referential check from
/// [`Service::create_usage_records`]'s validation loop.
///
/// Builds a request-local `Map<corrects_id, Result<UsageRecord, _>>` via
/// a bounded `get_usage_record` fan-out
/// (`inst-algo-semantics-l1-dedup` / `inst-algo-semantics-l1-bounded-fanout`),
/// then for every input index in `pending` runs
/// [`verify_l1_corrects_id`] and the deferred metadata check, projecting
/// the outcome into `results` (rejection) or `eligible` (verified).
///
/// Extracted from the host body to keep `create_usage_records` under the
/// cognitive-complexity cap without losing the explicit
/// `semantics → L1 → metadata` error-priority ordering described in the
/// algorithm.
// @cpt-algo:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1
// @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-dedup
// @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-bounded-fanout
// @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-lookup
async fn resolve_l1_lookups(
    plugin: &dyn UsageCollectorPluginV1,
    pending: Vec<PendingL1Lookup>,
    catalog_cache: &CatalogCache,
    results: &mut [Option<Result<UsageRecord, UsageCollectorError>>],
    eligible: &mut Vec<(usize, UsageRecord)>,
) {
    if pending.is_empty() {
        return;
    }

    let distinct_ids: HashSet<Uuid> = pending.iter().map(|(_, _, id)| *id).collect();

    let l1_cache: L1LookupCache =
        stream::iter(distinct_ids.into_iter().map(|corrects_id| async move {
            let outcome = plugin
                .get_usage_record(corrects_id)
                .await
                .map_err(DomainError::from);
            (corrects_id, outcome)
        }))
        .buffer_unordered(L1_LOOKUP_FANOUT_CONCURRENCY)
        .collect()
        .await;

    for (index, record, corrects_id) in pending {
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-not-found
        // The L1 pre-pass populates the cache for every pending
        // corrects_id, so a missing entry here is a host-invariant
        // breach. Surface it as a typed `Internal` per-record error
        // rather than `unreachable!()` — request paths must not panic
        // on an invariant failure, matching the SPI-size-mismatch arm
        // in `create_usage_records_inner`.
        let referenced = match l1_cache.get(&corrects_id) {
            Some(Ok(r)) => r,
            Some(Err(DomainError::UsageRecordNotFound { .. })) => {
                results[index] = Some(Err(UsageCollectorError::corrects_id_not_found(corrects_id)));
                continue;
            }
            Some(Err(e)) => {
                results[index] = Some(Err(UsageCollectorError::from(e.clone())));
                continue;
            }
            None => {
                results[index] = Some(Err(invariant_breach(format!(
                    "L1 pre-pass cache miss for corrects_id {corrects_id}"
                ))));
                continue;
            }
        };
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-not-found

        if let Err(e) = verify_l1_corrects_id(&record, corrects_id, referenced) {
            results[index] = Some(Err(e));
            continue;
        }

        // Metadata check deferred behind L1 to preserve the
        // `semantics → L1 → metadata` error-priority ordering the pre-A3
        // in-loop code exposed. A missing catalog entry here is a
        // host-invariant breach (the pre-pass covers every PDP-allowed
        // record's gts_id); surface it as a typed `Internal` rather than
        // panic the request thread.
        let Some(Ok(usage_type)) = catalog_cache.get(&record.gts_id) else {
            results[index] = Some(Err(invariant_breach(format!(
                "catalog pre-pass cache miss for gts_id {} before L1 metadata check",
                record.gts_id,
            ))));
            continue;
        };
        if let Err(e) = validate_submit_record_metadata(usage_type, &record.metadata) {
            results[index] = Some(Err(e));
            continue;
        }

        eligible.push((index, record));
    }
}
// @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-lookup
// @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-bounded-fanout
// @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-dedup

/// `usage-collector` domain service.
///
/// Discovers the bound storage plugin via `types-registry` and delegates
/// durable state to it. Owns the lazy binding resolution.
// @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-component-usage-type-catalog:p2
// @cpt-state:cpt-cf-usage-collector-state-usage-type-lifecycle-usage-type-registration-lifecycle:p2
// @cpt-state:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2
#[domain_model]
pub struct Service {
    hub: Arc<ClientHub>,

    /// Vendor selector read once at `Gear::init`; changing it requires a
    /// gear restart.
    vendor: String,

    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-component-plugin-host:p2
    selector: GtsPluginSelector,

    /// PEP boundary. The PDP is a hard dependency per ADR-0001; the host
    /// fails init if no resolver client is registered, so this field is
    /// always populated at runtime.
    enforcer: PolicyEnforcer,
}

impl Service {
    /// Storage-plugin resolution is lazy: no `types-registry` query happens
    /// here, it is deferred to the first dispatch.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, vendor: String, enforcer: PolicyEnforcer) -> Self {
        Self {
            hub,
            vendor,
            selector: GtsPluginSelector::new(),
            enforcer,
        }
    }

    /// Register a new `UsageType` in the plugin-owned `usage_type_catalog`
    /// per `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`.
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::PermissionDenied`] /
    ///   [`UsageCollectorError::ServiceUnavailable`] when the PDP denies or is
    ///   unavailable.
    /// * [`UsageCollectorError::AlreadyExists`] when the plugin's
    ///   `UNIQUE(gts_id)` constraint fires.
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-usage-type-registration:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-seq-register-usage-type:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-entity-usage-type:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-counter-semantics:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-gauge-semantics:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-constraint-no-business-logic:p2
    pub async fn create_usage_type(
        &self,
        ctx: &SecurityContext,
        input: UsageType,
    ) -> Result<UsageType, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-pdp
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-pdp-deny
        authz::authorize(
            &self.enforcer,
            ctx,
            &usage_type::RESOURCE,
            usage_type::actions::CREATE,
        )
        .await
        .map_err(UsageCollectorError::from)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-pdp-deny
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-pdp

        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-spi-insert
        // @cpt-begin:cpt-cf-usage-collector-state-usage-type-lifecycle-usage-type-registration-lifecycle:p2:inst-state-usage-type-lifecycle-registered
        match plugin.create_usage_type(input).await {
            Ok(record) => Ok(record),
            // @cpt-end:cpt-cf-usage-collector-state-usage-type-lifecycle-usage-type-registration-lifecycle:p2:inst-state-usage-type-lifecycle-registered
            // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-spi-insert
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-spi-catch
            Err(plugin_err) => {
                Err(match plugin_err {
                    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-duplicate
                    UsageCollectorPluginError::UsageTypeAlreadyExists { gts_id } => {
                        UsageCollectorError::usage_type_already_exists(&gts_id)
                    }
                    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-duplicate
                    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-spi-fail
                    other => UsageCollectorError::from(DomainError::from(other)),
                    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-spi-fail
                })
            } // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-spi-catch
        }
    }

    /// Create a single `UsageRecord` through the ingestion path per
    /// `cpt-cf-usage-collector-flow-usage-emission-emit-record`. No
    /// in-process catalog cache — the referenced `UsageType` is resolved
    /// from the bound storage plugin on each call.
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::PermissionDenied`] /
    ///   [`UsageCollectorError::ServiceUnavailable`] when the PDP denies or
    ///   is unavailable.
    /// * [`UsageCollectorError::NotFound`] when the referenced
    ///   `gts_id` is absent from the plugin-owned catalog.
    /// * [`UsageCollectorError::InvalidArgument`] /
    ///   [`UsageCollectorError::InvalidArgument`] on a malformed
    ///   `metadata` payload.
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-emission-compensation:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-ingestion:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-record-metadata:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-resource-attribution:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-subject-attribution:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-ingestion-authorization:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-usage-type-existence-and-semantics:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-tenant-attribution:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-compensation-flow:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-principle-fail-closed:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-principle-pluggable-storage:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-constraint-no-business-logic:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-component-ingestion-gateway:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-usage-type:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-idempotency:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-principle-idempotency-by-key:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-adr-mandatory-idempotency:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-adr-caller-supplied-attribution:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-seq-emit-usage:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-usage-record:p1
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-submit
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-missing-ctx
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-submit
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-missing-ctx
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-receive-ctx
    pub async fn create_usage_record(
        &self,
        ctx: &SecurityContext,
        record: UsageRecord,
    ) -> Result<UsageRecord, UsageCollectorError> {
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-receive-ctx
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-missing-ctx
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-submit
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-missing-ctx
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-submit
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-attrib-authz
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-pdp-deny
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-attrib-authz
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-pdp-deny
        authz::authorize_usage_record(&self.enforcer, ctx, &record, usage_record::actions::CREATE)
            .await
            .map_err(UsageCollectorError::from)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-pdp-deny
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-attrib-authz
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-pdp-deny
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-attrib-authz

        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-catalog-lookup
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-usage-type-not-found
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-catalog-lookup
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-usage-type-not-found
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-read-input
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-spi-dispatch
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-spi-fail
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-not-found
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-found
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-resolve-fields
        let usage_type = match plugin.get_usage_type(record.gts_id.clone()).await {
            Ok(ut) => ut,
            Err(UsageCollectorPluginError::UsageTypeNotFound { gts_id }) => {
                return Err(UsageCollectorError::usage_type_not_found(&gts_id));
            }
            Err(e) => return Err(UsageCollectorError::from(DomainError::from(e))),
        };
        // @cpt-end:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-resolve-fields
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-found
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-not-found
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-spi-fail
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-spi-dispatch
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-read-input
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-usage-type-not-found
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-catalog-lookup
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-usage-type-not-found
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-catalog-lookup

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-semantics-check
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-semantics-invalid
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-validate
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-validate-fail
        if let SemanticsOutcome::NeedsL1Lookup { corrects_id } =
            validate_record_semantics(&usage_type, &record)?
        {
            // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-lookup
            let referenced = match plugin.get_usage_record(corrects_id).await {
                Ok(row) => row,
                // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-not-found
                Err(UsageCollectorPluginError::UsageRecordNotFound { .. }) => {
                    return Err(UsageCollectorError::corrects_id_not_found(corrects_id));
                }
                // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-not-found
                Err(e) => return Err(UsageCollectorError::from(DomainError::from(e))),
            };
            // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-lookup
            verify_l1_corrects_id(&record, corrects_id, &referenced)?;
        }
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-validate-fail
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-validate
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-semantics-invalid
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-semantics-check

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-metadata-closed-shape
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-metadata-cap
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-metadata-too-large
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-metadata-closed-shape
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-metadata-cap
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-metadata-too-large
        validate_submit_record_metadata(&usage_type, &record.metadata)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-metadata-too-large
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-metadata-cap
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-metadata-closed-shape
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-metadata-too-large
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-metadata-cap
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-metadata-closed-shape

        // @cpt-begin:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2:inst-state-usage-record-validated
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-spi-dispatch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-spi-catch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-spi-fail
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-conflict
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-accepted
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-spi-dispatch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-spi-catch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-spi-fail
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-conflict
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-accepted
        // @cpt-begin:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2:inst-state-usage-record-persisted
        // @cpt-begin:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2:inst-state-usage-record-spi-error
        // @cpt-begin:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2:inst-state-usage-record-rejected-validation
        plugin
            .create_usage_record(record)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))
        // @cpt-end:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2:inst-state-usage-record-rejected-validation
        // @cpt-end:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2:inst-state-usage-record-spi-error
        // @cpt-end:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2:inst-state-usage-record-persisted
        // @cpt-end:cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle:p2:inst-state-usage-record-validated
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-accepted
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-conflict
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-spi-fail
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-spi-catch
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-compensation:p1:inst-compensation-spi-dispatch
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-accepted
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-conflict
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-spi-fail
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-spi-catch
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-record:p1:inst-emit-record-spi-dispatch
    }

    /// Create a batch of `UsageRecord`s through the ingestion path per
    /// `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`.
    ///
    /// Per-record stages mirror [`Self::create_usage_record`] and run
    /// independently for each input; eligible records carry their
    /// caller-supplied `created_at` through verbatim and are dispatched
    /// together. Per-record validation /
    /// SPI failures surface in the result vector at their input index —
    /// the outer `Err` is reserved for batch-level failures (plugin
    /// handle resolution, outer SPI dispatch, and the structural batch-size
    /// cap below).
    ///
    /// The SDK-facing batch cap of `1..=`[`MAX_BATCH_RECORDS`] is enforced
    /// here (not at the REST handler, which is a thin wrapper over this
    /// entry): an empty submission OR a submission exceeding
    /// [`MAX_BATCH_RECORDS`] surfaces as
    /// [`UsageCollectorError::InvalidArgument`] — the canonical lift
    /// renders it as the structural-validation `Problem` envelope (HTTP 400).
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::InvalidArgument`] when the input violates
    ///   the `1..=`[`MAX_BATCH_RECORDS`] cap.
    /// * [`UsageCollectorError::ServiceUnavailable`] when the storage plugin
    ///   handle cannot be resolved or the outer SPI dispatch fails.
    /// * Any other [`UsageCollectorError`] variant lifted from a batch-level
    ///   plugin transport / persistence failure.
    ///
    /// Per-record failures (authorization denial, missing usage type,
    /// malformed metadata, SPI errors against individual records) surface in
    /// the per-index `Result` entries of the returned vector rather than the
    /// outer `Err`.
    ///
    /// # Post-condition
    ///
    /// On `Ok`, the returned vector has length equal to `records.len()` and
    /// preserves input order: index `i` of the output corresponds to index
    /// `i` of the input batch.
    //
    // Realizes the batch flow `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`;
    // DoDs already attributed to the file via `create_usage_record` above
    // (fr-ingestion, fr-ingestion-authorization, fr-record-metadata,
    // fr-usage-type-existence-and-semantics, principle-fail-closed,
    // principle-pluggable-storage, component-ingestion-gateway, seq-emit-usage)
    // are not re-declared here (one `@cpt-dod` per id per file). Workload-
    // isolation is batch-specific so its marker lands here.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-nfr-workload-isolation:p1
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-receive-ctx
    pub async fn create_usage_records(
        &self,
        ctx: &SecurityContext,
        records: Vec<UsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorError>>, UsageCollectorError> {
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-receive-ctx
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-cap-check
        let actual = records.len();
        if actual == 0 || actual > MAX_BATCH_RECORDS {
            return Err(UsageCollectorError::invalid_batch_size(
                actual,
                1,
                MAX_BATCH_RECORDS,
            ));
        }
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-cap-check

        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;

        let mut results: Vec<Option<Result<UsageRecord, UsageCollectorError>>> =
            (0..records.len()).map(|_| None).collect();
        let mut eligible: Vec<(usize, UsageRecord)> = Vec::new();
        let mut pending_l1: Vec<PendingL1Lookup> = Vec::new();

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-pdp
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-dedup-tuple-key
        let mut distinct_tuples: HashMap<AttributionTupleKey, Vec<usize>> = HashMap::new();
        for (index, record) in records.iter().enumerate() {
            distinct_tuples
                .entry(AttributionTupleKey::from_record(
                    record,
                    usage_record::actions::CREATE,
                ))
                .or_default()
                .push(index);
        }
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-dedup-tuple-key

        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-bounded-fanout
        // The fan-out passes the tuple `key` (not a `&UsageRecord`) into the
        // PDP composer. This is the load-bearing safety property of the
        // dedup: `authorize_attribution_tuple` has no syntactic access to
        // any record field outside the key, so two records that
        // hash-equal under `AttributionTupleKey` cannot diverge in PDP
        // payload — they share the SAME `AccessRequest` by construction.
        // `action` is part of the key (hash/eq), so a future caller that
        // mixes actions in one batch cannot collapse onto a single PDP
        // decision.
        let pdp_decisions: Vec<PdpGroupDecision> =
            stream::iter(distinct_tuples.into_iter().map(|(key, indices)| {
                let enforcer = &self.enforcer;
                async move {
                    let decision = authz::authorize_attribution_tuple(enforcer, ctx, &key).await;
                    (indices, decision)
                }
            }))
            .buffer_unordered(PDP_CONCURRENCY)
            .collect()
            .await;
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-bounded-fanout

        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-pdp-deny
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-pdp-allow
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-pdp-projected-deny
        let mut pdp_allowed: Vec<bool> = vec![true; records.len()];
        for (indices, decision) in pdp_decisions {
            if let Err(e) = decision {
                // A PDP-transport failure (`AuthorizationUnavailable`) and a
                // plugin `Transient` both lift to `ServiceUnavailable`; their
                // curated `detail` strings keep them distinguishable for
                // operator triage without a separate per-record origin tag.
                for index in indices {
                    results[index] = Some(Err(UsageCollectorError::from(e.clone())));
                    pdp_allowed[index] = false;
                }
            }
        }
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-pdp-projected-deny
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-pdp-allow
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-pdp-deny
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-pdp

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-catalog
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-dedup-gts-id
        let distinct_gts_ids: HashSet<UsageTypeGtsId> = records
            .iter()
            .enumerate()
            .filter(|(idx, _)| pdp_allowed[*idx])
            .map(|(_, r)| r.gts_id.clone())
            .collect();
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-dedup-gts-id

        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-bounded-fanout
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-spi-dispatch
        // The fan-out lifts each per-id outcome to `DomainError` eagerly so
        // the cached value is Clone and a single SPI response can be
        // projected to every input index that references the id without
        // re-issuing the `get_usage_type` call.
        let catalog_cache: CatalogCache =
            stream::iter(distinct_gts_ids.into_iter().map(|gts_id| {
                let plugin = plugin.as_ref();
                async move {
                    let outcome = plugin
                        .get_usage_type(gts_id.clone())
                        .await
                        .map_err(DomainError::from);
                    (gts_id, outcome)
                }
            }))
            .buffer_unordered(CATALOG_FANOUT_CONCURRENCY)
            .collect()
            .await;
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-spi-dispatch
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-bounded-fanout
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-catalog

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-foreach-validate
        for (index, record) in records.into_iter().enumerate() {
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-pdp
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-deny
            if !pdp_allowed[index] {
                continue;
            }
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-deny
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-pdp

            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-catalog
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-unknown-usage-type
            // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-read-input
            // @cpt-begin:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-resolve-fields
            // The catalog pre-pass populated `catalog_cache` with a Clone
            // outcome per distinct gts_id; every PDP-allowed record's
            // gts_id is guaranteed to be present.
            let usage_type = match catalog_cache.get(&record.gts_id) {
                // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-found
                Some(Ok(ut)) => ut.clone(),
                // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-found
                // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-not-found
                // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-spi-fail
                Some(Err(e)) => {
                    results[index] = Some(Err(UsageCollectorError::from(e.clone())));
                    continue;
                }
                // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-spi-fail
                // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-not-found
                // Host-invariant breach (catalog pre-pass populated by
                // `distinct_gts_ids`); typed `Internal` per-record error
                // rather than `unreachable!()` so a future refactor cannot
                // turn an invariant slip into a request-thread panic.
                None => {
                    results[index] = Some(Err(invariant_breach(format!(
                        "catalog pre-pass cache miss for gts_id {} during record dispatch",
                        record.gts_id,
                    ))));
                    continue;
                }
            };
            // @cpt-end:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-resolve-fields
            // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup:p1:inst-algo-catalog-read-input
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-unknown-usage-type
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-catalog

            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-semantics
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-semantics-invalid
            let semantics_outcome = match validate_record_semantics(&usage_type, &record) {
                Ok(outcome) => outcome,
                Err(e) => {
                    results[index] = Some(Err(e));
                    continue;
                }
            };
            if let SemanticsOutcome::NeedsL1Lookup { corrects_id } = semantics_outcome {
                // L1 lookup is deferred to a post-loop dedup + bounded
                // fan-out pre-pass (`inst-algo-semantics-l1-dedup` /
                // `inst-algo-semantics-l1-bounded-fanout`); the metadata
                // check runs after L1 succeeds so the existing
                // semantics→L1→metadata error-priority ordering is
                // preserved end-to-end.
                pending_l1.push((index, record, corrects_id));
                continue;
            }
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-semantics-invalid
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-semantics

            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-metadata-closed-shape
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-metadata
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-metadata-too-large
            if let Err(e) = validate_submit_record_metadata(&usage_type, &record.metadata) {
                results[index] = Some(Err(e));
                continue;
            }
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-metadata-too-large
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-metadata
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-metadata-closed-shape

            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-eligible
            // (caller-supplied `record.created_at` is materialized verbatim on the persisted record)
            eligible.push((index, record));
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-eligible
        }

        resolve_l1_lookups(
            plugin.as_ref(),
            pending_l1,
            &catalog_cache,
            &mut results,
            &mut eligible,
        )
        .await;

        // The L1 phase pushes verified-compensation records to `eligible`
        // after the input-order foreach has completed, so the vec is no
        // longer guaranteed in input-index order. Sort once before the
        // plugin SPI dispatch; per-record results are still routed back
        // via the input index, so this only affects the order in which
        // the plugin sees the records.
        eligible.sort_by_key(|(index, _)| *index);
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-foreach-validate

        if !eligible.is_empty() {
            let (indices, dispatched): (Vec<usize>, Vec<UsageRecord>) =
                eligible.into_iter().unzip();
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-spi-dispatch
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-spi-catch
            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-spi-fail-mark
            let spi_results = plugin
                .create_usage_records(dispatched)
                .await
                .map_err(|e| UsageCollectorError::from(DomainError::from(e)))?;
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-spi-fail-mark
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-spi-catch
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-spi-dispatch

            if spi_results.len() != indices.len() {
                return Err(invariant_breach(format!(
                    "plugin returned {} per-record results for {} dispatched records",
                    spi_results.len(),
                    indices.len()
                )));
            }

            // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-foreach-spi
            for (index, spi_result) in indices.into_iter().zip(spi_results) {
                // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-accepted
                // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-conflict
                // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-spi-err
                results[index] =
                    Some(spi_result.map_err(|e| UsageCollectorError::from(DomainError::from(e))));
                // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-spi-err
                // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-conflict
                // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-record-accepted
            }
            // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:p1:inst-emit-batch-foreach-spi
        }

        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-return
        // Every per-record slot is filled by the PDP / catalog / semantics
        // / metadata / SPI-fanout passes above; an empty slot here is a
        // host-invariant breach. Yield a typed `Internal` for that slot so
        // the request thread cannot panic.
        Ok(results
            .into_iter()
            .enumerate()
            .map(|(slot_index, opt)| {
                opt.unwrap_or_else(|| {
                    Err(invariant_breach(format!(
                        "per-record slot {slot_index} was not populated before return"
                    )))
                })
            })
            .collect())
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization:p1:inst-algo-attrib-return
    }

    /// Deactivate a previously-emitted `UsageRecord` by `uuid`.
    ///
    /// The handler first fetches the target row via Plugin SPI Method 10
    /// `get_usage_record(id)` so PDP can authorize over the full attribution
    /// tuple (`tenant_id`, `resource_ref`, optional `subject_ref`). It then
    /// dispatches Plugin SPI Method 5 `deactivate_usage_record(id)` exactly
    /// once; the plugin performs the atomic depth-1 cascade in one backend
    /// transaction, and on `Ok(())` every affected row's `status` column is
    /// now `inactive`.
    ///
    /// Existence-oracle guard: the pre-PDP fetch would otherwise let an
    /// unauthorized caller tell "no such record" (`NotFound`) from "exists
    /// but denied" (`PermissionDenied`). A PDP denial is therefore collapsed
    /// into the same `NotFound` the missing-row path returns, so the two are
    /// indistinguishable on this by-id surface.
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::NotFound`] when the targeted record does not
    ///   exist (raised by the pre-PDP fetch or a race where the row
    ///   disappears before SPI Method 5 dispatch), or when the PDP denies
    ///   (collapsed, see above).
    /// * [`UsageCollectorError::ServiceUnavailable`] when the PDP is
    ///   unavailable.
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1
    // @cpt-flow:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1
    // @cpt-algo:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1
    // @cpt-algo:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-event-deactivation-component-deactivation-handler:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-event-deactivation-nfr-availability:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-event-deactivation-principle-fail-closed:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-event-deactivation-entity-usage-record:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-event-deactivation-entity-security-context:p1
    pub async fn deactivate_usage_record(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-resolve-plugin
        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-resolve-plugin

        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-prefetch
        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-prefetch-not-found
        let record = plugin
            .get_usage_record(id)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))?;
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-prefetch-not-found
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-prefetch

        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-pdp
        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-pdp-deny
        authz::authorize_usage_record(
            &self.enforcer,
            ctx,
            &record,
            usage_record::actions::DEACTIVATE,
        )
        .await
        .map_err(|e| collapse_deny_to_not_found(e, id))?;
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-pdp-deny
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-pdp

        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-spi-dispatch
        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1:inst-cascade-receive-id
        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1:inst-cascade-spi-call
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-spi-call
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-await
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-catch
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-propagate-error
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-return-outcome
        // @cpt-begin:cpt-cf-usage-collector-state-event-deactivation-record-lifecycle:p1:inst-state-active-to-inactive
        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-outcome-map
        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-already-inactive
        plugin
            .deactivate_usage_record(id)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-already-inactive
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-outcome-map
        // @cpt-end:cpt-cf-usage-collector-state-event-deactivation-record-lifecycle:p1:inst-state-active-to-inactive
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-return-outcome
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-propagate-error
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-catch
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-await
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch:p1:inst-algo-dispatch-spi-call
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1:inst-cascade-spi-call
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1:inst-cascade-receive-id
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-spi-dispatch
    }

    /// Read a single `UsageRecord` by `uuid` from the bound storage plugin.
    ///
    /// The handler first fetches the target row via Plugin SPI Method 10
    /// `get_usage_record(id)` so PDP can authorize over the full attribution
    /// tuple (`tenant_id`, `resource_ref`, optional `subject_ref`). A PDP
    /// denial is collapsed into the same `NotFound` the missing-row path
    /// returns, so an unauthorized caller cannot use this by-id surface as an
    /// existence oracle (mirrors `deactivate_usage_record`).
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::NotFound`] when the targeted record does not
    ///   exist (raised by the pre-PDP fetch), or when the PDP denies
    ///   (collapsed, see above).
    /// * [`UsageCollectorError::ServiceUnavailable`] when the PDP is
    ///   unavailable.
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-emission-get-record:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-api-get-records-id:p1
    pub async fn get_usage_record(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<UsageRecord, UsageCollectorError> {
        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-prefetch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-prefetch-not-found
        let record = plugin
            .get_usage_record(id)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-prefetch-not-found
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-prefetch

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-pdp
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-pdp-deny
        authz::authorize_usage_record(&self.enforcer, ctx, &record, usage_record::actions::GET)
            .await
            .map_err(|e| collapse_deny_to_not_found(e, id))?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-pdp-deny
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-pdp

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-success
        Ok(record)
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-success
    }

    /// Read a single `UsageType` from the bound storage plugin's catalog.
    ///
    /// A plugin `UsageTypeNotFound` is surfaced verbatim through the
    /// dispatch-boundary translation as
    /// [`UsageCollectorError::NotFound`].
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::PermissionDenied`] /
    ///   [`UsageCollectorError::ServiceUnavailable`] when the PDP denies or
    ///   is unavailable.
    /// * [`UsageCollectorError::NotFound`] when the catalog has no
    ///   row for `gts_id`.
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-nfr-availability:p1
    pub async fn get_usage_type(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
    ) -> Result<UsageType, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-pdp
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-pdp-deny
        authz::authorize(
            &self.enforcer,
            ctx,
            &usage_type::RESOURCE,
            usage_type::actions::GET,
        )
        .await
        .map_err(UsageCollectorError::from)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-pdp-deny
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-pdp
        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-repo-find-by-id
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-not-found
        plugin
            .get_usage_type(gts_id)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-not-found
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-repo-find-by-id
    }

    /// List `UsageType` records from the bound storage plugin's catalog.
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::PermissionDenied`] /
    ///   [`UsageCollectorError::ServiceUnavailable`] when the PDP denies or
    ///   is unavailable.
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1
    pub async fn list_usage_types(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<ODataPage<UsageType>, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-pdp
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-pdp-deny
        authz::authorize(
            &self.enforcer,
            ctx,
            &usage_type::RESOURCE,
            usage_type::actions::LIST,
        )
        .await
        .map_err(UsageCollectorError::from)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-pdp-deny
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-pdp
        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-plugin-read
        plugin
            .list_usage_types(query)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-plugin-read
    }

    /// Keyset-paginated list of `UsageRecord`s from the bound storage
    /// plugin's table, narrowed by the PDP-returned constraints.
    ///
    /// Three responsibilities live here per
    /// `cpt-cf-usage-collector-flow-usage-query-query-raw`:
    ///
    /// 1. **Authorize** the request via [`authz::authorize_list_usage_records`].
    ///    The PEP request is pre-row (no per-record attribution attributes)
    ///    because the caller has not yet named a specific row — the PDP
    ///    responds with row-scope narrowing via the [`AccessScope`]
    ///    constraints, not via a tuple match. It runs under
    ///    `require_constraints(true)`, so the PDP MUST return row-scope
    ///    narrowing (a platform admin resolves to the full tenant set, never
    ///    `allow_all`); a degenerate unconstrained permit is denied in
    ///    composition by [`authz::scope_to_odata_filter`], not read as "all
    ///    tenants".
    /// 2. **Compose** the PDP constraints into the user-supplied `OData`
    ///    filter via [`authz::scope_to_odata_filter`]. The composition is
    ///    intersection-only (`composed = user_filter AND constraints`) per
    ///    `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`
    ///    — no widening is permitted. `gts_id` stays a typed parameter
    ///    and is NOT touched here. The `[from, to)` time window flows
    ///    through `query.filter` as a `created_at` predicate (see
    ///    [`usage_collector_sdk::UsageRecordFilterField`]); the gateway
    ///    no longer accepts a separate `TimeWindow`.
    /// 3. **Delegate** to the bound storage plugin's
    ///    `list_usage_records` SPI with the composed filter.
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::PermissionDenied`] /
    ///   [`UsageCollectorError::ServiceUnavailable`] when the PDP denies
    ///   or is unavailable, or when the PDP returns a constraint shape
    ///   this gear cannot honour (tree predicates on a flat resource,
    ///   unknown PEP property, type mismatch on a value).
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-query-query-raw:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-fr-query-raw:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-fr-tenant-isolation:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-nfr-authorization:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-principle-pdp-centric-authorization:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-principle-fail-closed:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-constraint-no-business-logic:p1
    pub async fn list_usage_records(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
    ) -> Result<ODataPage<UsageRecord>, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-pdp-delegate
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-attribution
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-pdp-deny-return
        let scope = authz::authorize_list_usage_records(&self.enforcer, ctx)
            .await
            .map_err(UsageCollectorError::from)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-pdp-deny-return
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-attribution
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-pdp-delegate

        // Reject an unbounded query before composition / dispatch so an
        // authorized caller cannot drive a full-table scan. Runs after
        // authz to preserve the PDP-first posture (an unauthorized caller
        // is denied regardless of window shape).
        require_bounded_time_window(query)?;

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-constraint-composition
        let composed = compose_query_with_scope(query, &scope)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-constraint-composition

        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-plugin-dispatch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-plugin-catch
        plugin
            .list_usage_records(gts_id, &composed, metadata_filter)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-plugin-catch
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-plugin-dispatch
    }

    /// Aggregated read over `UsageRecord`s, narrowed by the PDP-returned
    /// constraints and executed server-side by the bound storage plugin.
    ///
    /// Mirrors [`Self::list_usage_records`] in posture — the same three
    /// responsibilities live here per
    /// `cpt-cf-usage-collector-flow-usage-query-query-aggregated`:
    ///
    /// 1. **Authorize** the request via [`authz::authorize_list_usage_records`]
    ///    (the PEP shape is shared: pre-row, no per-record attribution, with
    ///    `require_constraints(true)` so the PDP MUST return row-scope
    ///    narrowing). A constrained permit narrows the user filter; a
    ///    degenerate unconstrained permit is denied in composition by
    ///    [`authz::scope_to_odata_filter`], not left unscoped across tenants.
    /// 2. **Compose** the PDP constraints into the user-supplied `OData`
    ///    filter via [`compose_query_with_scope`]. The composition is
    ///    intersection-only per
    ///    `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`.
    /// 3. **Delegate** to the bound storage plugin's
    ///    `query_aggregated_usage_records` SPI with the composed filter,
    ///    the typed `gts_id`, the metadata side-channel, and the
    ///    [`AggregationSpec`]. The plugin executes `SUM` / `COUNT` /
    ///    `MIN` / `MAX` / `AVG` and any `group_by` dimensions
    ///    server-side per `plugin-spi.md` Method 3.
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::PermissionDenied`] /
    ///   [`UsageCollectorError::ServiceUnavailable`] when the PDP denies
    ///   or is unavailable, or when the PDP returns a constraint shape
    ///   this gear cannot honour (tree predicates on a flat resource,
    ///   unknown PEP property, type mismatch on a value).
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-fr-query-aggregation:p1
    pub async fn query_aggregated_usage_records(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
        aggregation: AggregationSpec,
    ) -> Result<AggregationResult, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-pdp-delegate
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-attribution
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-pdp-deny-return
        let scope = authz::authorize_list_usage_records(&self.enforcer, ctx)
            .await
            .map_err(UsageCollectorError::from)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-pdp-deny-return
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-attribution
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-pdp-delegate

        // Reject an unbounded query before composition / dispatch so an
        // authorized caller cannot drive a full-table aggregation. The
        // aggregate path has no `$top` ceiling, so the bounded window is
        // its only scan bound. Runs after authz (PDP-first posture).
        require_bounded_time_window(query)?;

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-constraint-composition
        let composed = compose_query_with_scope(query, &scope)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-constraint-composition

        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;

        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-plugin-dispatch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-plugin-catch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-plugin-catch-return
        plugin
            .query_aggregated_usage_records(gts_id, &composed, metadata_filter, aggregation)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-plugin-catch-return
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-plugin-catch
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-plugin-dispatch
    }

    /// Delete a `UsageType` row from the bound storage plugin's catalog.
    ///
    /// The plugin surfaces FK-rejection as
    /// [`UsageCollectorError::Conflict`] and a missing target as
    /// [`UsageCollectorError::NotFound`].
    ///
    /// # Errors
    ///
    /// * [`UsageCollectorError::PermissionDenied`] /
    ///   [`UsageCollectorError::ServiceUnavailable`] when the PDP denies or
    ///   is unavailable.
    /// * [`UsageCollectorError::NotFound`] when no catalog row
    ///   matches `gts_id`.
    /// * [`UsageCollectorError::Conflict`] when active records
    ///   still reference the target.
    /// * Any other [`UsageCollectorError`] variant lifted from a plugin
    ///   transport / persistence failure.
    // @cpt-flow:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-usage-type-deletion:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-seq-delete-usage-type:p1
    pub async fn delete_usage_type(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
    ) -> Result<(), UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-pdp-authorize
        authz::authorize(
            &self.enforcer,
            ctx,
            &usage_type::RESOURCE,
            usage_type::actions::DELETE,
        )
        .await
        .map_err(UsageCollectorError::from)?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-pdp-authorize
        let plugin = self.get_plugin().await.map_err(UsageCollectorError::from)?;
        // The plugin SPI catch is expressed as a composed `From` chain
        // (`UsageCollectorPluginError` → `DomainError` → `UsageCollectorError`).
        // Variant-specific routing for `UsageTypeNotFound` and
        // `UsageTypeReferenced` lives in `infra::sdk_error_mapping`, where
        // each canonical-lift arm carries its own instruction marker.
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-dispatch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-catch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-fail
        plugin
            .delete_usage_type(gts_id)
            .await
            .map_err(|e| UsageCollectorError::from(DomainError::from(e)))?;
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-fail
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-catch
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-dispatch
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-delete-return
        // @cpt-begin:cpt-cf-usage-collector-state-usage-type-lifecycle-usage-type-registration-lifecycle:p2:inst-state-usage-type-lifecycle-not-registered
        Ok(())
        // @cpt-end:cpt-cf-usage-collector-state-usage-type-lifecycle-usage-type-registration-lifecycle:p2:inst-state-usage-type-lifecycle-not-registered
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-delete-return
    }

    /// Lazily resolves and returns the bound storage-plugin client.
    ///
    /// On the first dispatch the embedded `GtsPluginSelector` resolves the
    /// instance id single-flight via [`Self::resolve_plugin`] and caches it for
    /// the `Service`'s lifetime; warm calls reuse the cached id with no further
    /// `types-registry` round-trip. The resolved scoped client is looked up via
    /// `ClientHub::try_get_scoped`; the structural readiness fact (selector
    /// cached AND the scoped client registered) governs whether the dispatch
    /// proceeds.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::TypesRegistryUnavailable`] when the registry call
    /// fails (the selector stays uncached so the next dispatch retries),
    /// [`DomainError::PluginNotFound`] when no instance matches the configured
    /// vendor, [`DomainError::InvalidPluginInstance`] on malformed instance
    /// content, and [`DomainError::PluginUnavailable`] when the scoped client is
    /// not registered under the resolved scope.
    // @cpt-flow:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1
    // @cpt-algo:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-fr-pluggable-storage:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-entity-plugin-binding:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-nfr-availability:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-pluggable-storage:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-plugin-resolution-via-client-hub:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-fail-closed:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-adr-pluggable-storage:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-constraint-vendor-pluggable:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-constraint-plugin-contract-stability:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-constraint-nfr-thresholds:p2
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-contract-storage-plugin:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-contract-gts-registry:p1
    pub async fn get_plugin(&self) -> Result<Arc<dyn UsageCollectorPluginV1>, DomainError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-enter-selector
        // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-cold-path
        // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-cold-path
        // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-cache-instance-id
        // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-cache-instance-id
        // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-warm-path
        // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-warm-path
        let instance_id = match self.selector.get_or_init(|| self.resolve_plugin()).await {
            Ok(instance_id) => instance_id,
            // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-warm-path
            // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-warm-path
            // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-cache-instance-id
            // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-cache-instance-id
            // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-catch
            // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-plugin-unavailable-cold
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    vendor = %self.vendor,
                    "usage-collector plugin selector resolution failed"
                );
                return Err(e);
            } // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-plugin-unavailable-cold
              // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-catch
        };
        // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-cold-path
        // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-cold-path
        // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-enter-selector

        // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-try-get-scoped
        // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-try-get-scoped
        let scope = ClientScope::gts_id(instance_id.as_ref());
        let client = self
            .hub
            .try_get_scoped::<dyn UsageCollectorPluginV1>(&scope);
        // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-try-get-scoped
        // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-try-get-scoped

        // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-return-handle
        // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-return
        if let Some(client) = client {
            return Ok(client);
        }

        tracing::warn!(
            plugin_gts_id = %instance_id,
            vendor = %self.vendor,
            "usage-collector storage plugin client not registered yet"
        );
        Err(DomainError::PluginUnavailable {
            gts_id: Some(instance_id.to_string()),
            reason: "client not registered yet".into(),
        })
        // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-return
        // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-return-handle
    }

    /// Resolves the bound storage-plugin instance id from `types-registry`.
    // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-lazy-resolve
    // @cpt-begin:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-resolve-plugin
    #[tracing::instrument(skip_all, fields(vendor = %self.vendor))]
    async fn resolve_plugin(&self) -> Result<String, DomainError> {
        info!("Resolving usage-collector storage plugin");

        let registry = self
            .hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))?;

        let plugin_type_id = UsageCollectorPluginSpecV1::gts_type_id().clone();

        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{plugin_type_id}*")))
            .await
            .map_err(TypesRegistryError::from)?;

        let gts_id = choose_plugin_instance::<UsageCollectorPluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.id.as_ref(), &e.object)),
        )?;

        info!(plugin_gts_id = %gts_id, "Selected usage-collector storage plugin instance");

        Ok(gts_id)
    }
    // @cpt-end:cpt-cf-usage-collector-algo-foundation-plugin-host-binding:p2:inst-algo-binding-resolve-plugin
    // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-lazy-resolve

    /// Test-only: clear the cached binding so the next dispatch re-resolves.
    #[cfg(test)]
    pub(crate) async fn selector_reset_for_test(&self) -> bool {
        self.selector.reset().await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
