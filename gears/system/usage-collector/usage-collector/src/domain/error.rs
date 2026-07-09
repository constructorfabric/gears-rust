//! Domain errors for the usage-collector module.
//!
//! Bridges the public SDK envelope ([`UsageCollectorError`]), the plugin-side
//! vocabulary ([`UsageCollectorPluginError`]), and registry / `ClientHub` /
//! plugin-selection failures into the internal [`DomainError`]. The RFC-9457
//! `Problem` lift lives on the REST surface — this module only normalizes
//! failures.
//!
//! The catalog error vocabulary is keyed by `gts_id: UsageTypeGtsId`
//! end-to-end (no UUID derivation; `gts_id` is the catalog PK).
//! Validation failures (typed SDK variants — `NegativeCounterValue`,
//! `InvalidUsageTypeGtsId`, `InvalidResourceRef`, etc.) flow back to the
//! caller verbatim and are not re-classified through `DomainError`.

use toolkit_macros::domain_model;
use usage_collector_sdk::{UsageCollectorError, UsageCollectorPluginError, UsageTypeGtsId};
use uuid::Uuid;

/// Internal domain errors for the usage-collector host.
#[domain_model]
#[derive(thiserror::Error, Debug, Clone)]
pub enum DomainError {
    #[error("types registry is not available: {0}")]
    TypesRegistryUnavailable(String),

    #[error("no storage plugin instances found for vendor '{vendor}'")]
    PluginNotFound { vendor: String },

    #[error("invalid plugin instance content for '{gts_id}': {reason}")]
    InvalidPluginInstance { gts_id: String, reason: String },

    /// Structural readiness failure on the storage plugin: the selector
    /// resolved an instance but the scoped client is not registered, or
    /// the SDK envelope crossed back into the host without an instance id
    /// in scope. `gts_id` is `Some` only on the cold-path internal call
    /// site that knows which instance was resolved; SDK-envelope lifts
    /// leave it `None` rather than synthesising a placeholder.
    #[error(
        "storage plugin not available{}: {reason}",
        gts_id.as_ref().map(|g| format!(" for '{g}'")).unwrap_or_default()
    )]
    PluginUnavailable {
        gts_id: Option<String>,
        reason: String,
    },

    /// Retryable plugin-reported transient backend failure (downstream
    /// timeout, connection reset, upstream 5xx). Lifts to
    /// `UsageCollectorError::ServiceUnavailable`, preserving the optional
    /// `retry_after_seconds` hint end-to-end.
    #[error("storage plugin transient failure: {detail}")]
    PluginTransient {
        detail: String,
        retry_after_seconds: Option<u64>,
    },

    /// PDP-supplied deny on the requested operation. Surfaces both an explicit
    /// `EnforcerError::Denied` and the fail-closed `EnforcerError::CompileFailed`
    /// branch; both collapse to the same deterministic platform authorization
    /// deny envelope and never derive a permissive fallback.
    #[error("authorization denied{}", reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default())]
    AuthorizationDenied { reason: Option<String> },

    /// The PDP transport failed — `authz-resolver` is unreachable or the
    /// evaluation RPC timed out. The collector fails closed and never serves a
    /// cached or permissive decision, per
    /// `cpt-cf-usage-collector-principle-pdp-centric-authorization`.
    #[error("authorization service unavailable: {0}")]
    AuthorizationUnavailable(String),

    /// The referenced `gts_id` is absent from the plugin-owned catalog
    /// (catalog-admin op, ingestion, or aggregated-query reference) per
    /// ADR-0012.
    #[error("usage type not found: {gts_id}")]
    UsageTypeNotFound { gts_id: UsageTypeGtsId },

    /// `create_usage_type` was called with a `gts_id` whose row is already
    /// present in `usage_type_catalog` and whose payload differs from the
    /// stored row.
    #[error("usage type already exists: {gts_id}")]
    UsageTypeAlreadyExists { gts_id: UsageTypeGtsId },

    /// `delete_usage_type` rejected because the usage type is still
    /// referenced by usage samples (ADR-0012 §"Consequences").
    #[error("usage type {gts_id} is still referenced by {sample_ref_count} samples")]
    UsageTypeReferenced {
        gts_id: UsageTypeGtsId,
        sample_ref_count: u64,
    },

    /// Ingestion supplied a `metadata` map carrying a key that is not a
    /// member of the referenced usage type's declared `metadata_fields` list
    /// per ADR-0012 (closed shape, keyed by `gts_id`).
    #[error("unknown metadata key '{key}' for usage type {gts_id}")]
    UnknownMetadataKey { gts_id: UsageTypeGtsId, key: String },

    /// Idempotency conflict on a usage submission: the supplied
    /// `idempotency_key` is already bound to a different usage submission
    /// (sdk-trait.md §"`DedupOutcome`"). Carries the UUID of the previously
    /// persisted record bound to the key.
    #[error("idempotency conflict: key {idempotency_key} already bound to record {existing_uuid}")]
    IdempotencyConflict {
        idempotency_key: String,
        existing_uuid: Uuid,
    },

    /// `deactivate_usage_record` referenced an `id` that does not exist
    /// within the visible scope.
    #[error("usage record not found: {id}")]
    UsageRecordNotFound { id: Uuid },

    /// `deactivate_usage_record` referenced an `id` that was already
    /// `inactive` (one-way latch).
    #[error("usage record already inactive: {id}")]
    UsageRecordAlreadyInactive { id: Uuid },

    #[error("internal error: {0}")]
    Internal(String),
}

// NOTE(DE1302): `DomainError::Internal` / `TypesRegistryUnavailable` only carry
// a String, so these From impls intentionally stringify the source error.
//
// A `TypesRegistryClient::list_instances` failure is a registry-availability
// failure on the lazy storage-plugin resolution path, so it maps to
// `TypesRegistryUnavailable` (not `Internal`): the selector cache stays empty
// and the next dispatch retries the resolve, per the Plugin Host binding algo.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<types_registry_sdk::TypesRegistryError> for DomainError {
    fn from(e: types_registry_sdk::TypesRegistryError) -> Self {
        Self::TypesRegistryUnavailable(e.to_string())
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<toolkit::client_hub::ClientHubError> for DomainError {
    fn from(e: toolkit::client_hub::ClientHubError) -> Self {
        Self::Internal(e.to_string())
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<serde_json::Error> for DomainError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<toolkit::plugins::ChoosePluginError> for DomainError {
    fn from(e: toolkit::plugins::ChoosePluginError) -> Self {
        match e {
            toolkit::plugins::ChoosePluginError::InvalidPluginInstance { gts_id, reason } => {
                Self::InvalidPluginInstance { gts_id, reason }
            }
            toolkit::plugins::ChoosePluginError::PluginNotFound { vendor, .. } => {
                Self::PluginNotFound { vendor }
            }
        }
    }
}

// Fail-closed: `CompileFailed` collapses to `AuthorizationDenied` (the
// collector calls the enforcer without `require_constraints(true)`, so a
// non-deny `CompileFailed` is unexpected — but we keep the mapping
// fail-closed). `EvaluationFailed` is the only path to
// `AuthorizationUnavailable`.
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-entity-pdp-decision:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-fail-closed:p2
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        use authz_resolver_sdk::EnforcerError;
        match e {
            // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-deny
            // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-deny
            // Reason is captured by field extraction (NOT `{r:?}`) so a future
            // `DenyReason` field addition cannot leak struct internals into
            // operator logs. The wire envelope drops the reason; this string
            // only feeds the `tracing` Display path.
            EnforcerError::Denied { deny_reason } => Self::AuthorizationDenied {
                reason: deny_reason.map(|r| match r.details {
                    Some(details) => format!("{}: {details}", r.error_code),
                    None => r.error_code,
                }),
            },
            EnforcerError::CompileFailed(err) => Self::AuthorizationDenied {
                reason: Some(err.to_string()),
            },
            // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-deny
            // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-deny
            // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-resolver-catch
            // @cpt-begin:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-fail-closed
            // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-catch
            // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-fail-closed
            EnforcerError::EvaluationFailed(err) => Self::AuthorizationUnavailable(err.to_string()),
            // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-fail-closed
            // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-catch
            // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-fail-closed
            // @cpt-end:cpt-cf-usage-collector-flow-foundation-pdp-authorize:p1:inst-pdp-resolver-catch
        }
    }
}

// `UsageCollectorPluginError` is `#[non_exhaustive]`. The catch-all arm exists
// only for future variant growth; the `is_*_exhaustive_today` debug_assert
// fires in tests if a new variant is added without extending the match.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<UsageCollectorPluginError> for DomainError {
    fn from(e: UsageCollectorPluginError) -> Self {
        debug_assert!(is_plugin_error_exhaustive_today(&e));
        match e {
            // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-spi-catch
            // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1:inst-cascade-fail
            UsageCollectorPluginError::Transient {
                detail,
                retry_after_seconds,
            } => Self::PluginTransient {
                detail,
                retry_after_seconds,
            },
            UsageCollectorPluginError::Internal(detail) => Self::Internal(detail),
            // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-cascade:p1:inst-cascade-fail
            // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-spi-catch
            UsageCollectorPluginError::UsageTypeNotFound { gts_id } => {
                Self::UsageTypeNotFound { gts_id }
            }
            UsageCollectorPluginError::UsageTypeAlreadyExists { gts_id } => {
                Self::UsageTypeAlreadyExists { gts_id }
            }
            UsageCollectorPluginError::UsageTypeReferenced {
                gts_id,
                sample_ref_count,
            } => Self::UsageTypeReferenced {
                gts_id,
                sample_ref_count,
            },
            UsageCollectorPluginError::IdempotencyConflict {
                idempotency_key,
                existing_uuid,
            } => Self::IdempotencyConflict {
                idempotency_key,
                existing_uuid,
            },
            UsageCollectorPluginError::UsageRecordNotFound { id } => {
                Self::UsageRecordNotFound { id }
            }
            UsageCollectorPluginError::UsageRecordAlreadyInactive { id } => {
                Self::UsageRecordAlreadyInactive { id }
            }
            other => Self::Internal(other.to_string()),
        }
    }
}

#[allow(dead_code)]
fn is_plugin_error_exhaustive_today(e: &UsageCollectorPluginError) -> bool {
    matches!(
        e,
        UsageCollectorPluginError::Transient { .. }
            | UsageCollectorPluginError::Internal(_)
            | UsageCollectorPluginError::UsageTypeNotFound { .. }
            | UsageCollectorPluginError::UsageTypeAlreadyExists { .. }
            | UsageCollectorPluginError::UsageTypeReferenced { .. }
            | UsageCollectorPluginError::IdempotencyConflict { .. }
            | UsageCollectorPluginError::UsageRecordNotFound { .. }
            | UsageCollectorPluginError::UsageRecordAlreadyInactive { .. }
    )
}

// The public envelope → DomainError direction is intentionally absent:
// the compacted `UsageCollectorError` is a terminal caller-facing shape and
// nothing re-classifies it back into the host-internal vocabulary. The
// host always flows DomainError → UsageCollectorError (below).

impl From<DomainError> for UsageCollectorError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::PluginNotFound { .. } | DomainError::PluginUnavailable { .. } => {
                Self::plugin_unavailable()
            }
            DomainError::PluginTransient {
                detail,
                retry_after_seconds,
            } => Self::service_unavailable(detail, retry_after_seconds),
            DomainError::AuthorizationDenied { reason } => {
                Self::permission_denied(reason.unwrap_or_else(|| "denied".to_owned()))
            }
            DomainError::AuthorizationUnavailable(reason) => {
                Self::service_unavailable(reason, None)
            }
            DomainError::UsageTypeNotFound { gts_id } => Self::usage_type_not_found(&gts_id),
            DomainError::UsageTypeAlreadyExists { gts_id } => {
                Self::usage_type_already_exists(&gts_id)
            }
            DomainError::UnknownMetadataKey { gts_id, key } => {
                Self::unknown_metadata_key(&gts_id, &key)
            }
            DomainError::UsageTypeReferenced {
                gts_id,
                sample_ref_count,
            } => Self::usage_type_referenced(&gts_id, sample_ref_count),
            DomainError::IdempotencyConflict {
                idempotency_key,
                existing_uuid,
            } => Self::idempotency_conflict(&idempotency_key, existing_uuid),
            DomainError::UsageRecordNotFound { id } => Self::usage_record_not_found(id),
            DomainError::UsageRecordAlreadyInactive { id } => Self::already_inactive(id),
            DomainError::InvalidPluginInstance { gts_id, reason } => {
                Self::internal(format!("invalid plugin instance '{gts_id}': {reason}"))
            }
            DomainError::TypesRegistryUnavailable(_) => Self::types_registry_unavailable(),
            DomainError::Internal(reason) => Self::internal(reason),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
