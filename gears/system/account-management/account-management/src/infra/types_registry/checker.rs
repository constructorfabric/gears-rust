//! `GtsTenantTypeChecker` — the production [`TenantTypeChecker`] wired
//! against `types_registry_sdk::TypesRegistryClient` resolved from
//! `ClientHub`.
//!
//! Implements `algo-allowed-parent-types-evaluation` and
//! `algo-same-type-nesting-admission` (FEATURE 2.3
//! `tenant-type-enforcement`):
//!
//! 1. Resolve the GTS [`GtsTypeSchema`] for the child (and, for
//!    different-parent calls, also the parent) via
//!    [`TypesRegistryClient::get_type_schemas_by_uuid`] — one batched
//!    round-trip.
//! 2. Reject children whose chain does not descend from the
//!    `gts.cf.core.am.tenant_type.v1~` envelope (the only schema whose
//!    `x-gts-traits-schema` defines `allowed_parent_types`).
//! 3. Read the effective `allowed_parent_types` trait via
//!    [`GtsTypeSchema::effective_traits`] (the SDK does the chain
//!    merge: leaf-declared values win, then deepest-base defaults).
//! 4. Admit iff the parent's chained GTS identifier is a member of the
//!    list. Same-type nesting (`parent == child`) is admitted iff the
//!    type's own identifier appears in its own `allowed_parent_types`
//!    per `algo-same-type-nesting-admission`.
//! 5. Map registry transport / trait-resolution failures onto
//!    [`DomainError::ServiceUnavailable`]; not-registered children or
//!    parents onto [`DomainError::InvalidTenantType`]; admitted-but-
//!    rejected pairings onto [`DomainError::TypeNotAllowed`].
//!
//! The barrier MUST NOT cache schemas across calls
//! (`dod-tenant-type-enforcement-gts-availability-surface`); every
//! invocation re-resolves against GTS so trait updates and re-types
//! take effect immediately. The SDK's `get_type_schemas_by_uuid`
//! implementation is responsible for any short-lived caching it
//! chooses to do internally.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{GtsTypeSchema, TypesRegistryClient};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::tenant_type::checker::TenantTypeChecker;

/// GTS identifier of the AM tenant-type envelope. Children that do not
/// descend from this schema cannot legitimately resolve an
/// `allowed_parent_types` trait — they're rejected as
/// [`DomainError::InvalidTenantType`] regardless of what the
/// effective-trait merge produces.
const TENANT_TYPE_BASE_GTS_ID: &str = "gts.cf.core.am.tenant_type.v1~";

/// Top-level key on the effective trait map carrying the list of
/// admitted parent GTS identifiers. Defined on
/// `gts.cf.core.am.tenant_type.v1~`'s `x-gts-traits-schema` — see
/// `docs/schemas/tenant_type.v1.schema.json`.
const ALLOWED_PARENT_TYPES_TRAIT: &str = "allowed_parent_types";

/// Default GTS probe timeout (ms). Keeps a hung registry from stalling
/// a tenant create-saga past the 503 fail-something boundary; a
/// healthy registry round-trip is well under this. Mirrors the
/// `RgResourceOwnershipChecker` default for operational consistency.
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 2_000;

/// Production [`TenantTypeChecker`] backed by the GTS Types Registry.
pub struct GtsTenantTypeChecker {
    registry: Arc<dyn TypesRegistryClient + Send + Sync>,
    probe_timeout: Duration,
}

impl GtsTenantTypeChecker {
    /// Construct a new checker around a registry client resolved from
    /// `ClientHub`, using the backward-compatible default timeout.
    #[must_use]
    pub fn new(registry: Arc<dyn TypesRegistryClient + Send + Sync>) -> Self {
        Self::with_timeout(registry, DEFAULT_PROBE_TIMEOUT_MS)
    }

    /// Construct a checker with the configured probe timeout.
    #[must_use]
    pub fn with_timeout(
        registry: Arc<dyn TypesRegistryClient + Send + Sync>,
        probe_timeout_ms: u64,
    ) -> Self {
        Self {
            registry,
            probe_timeout: Duration::from_millis(probe_timeout_ms.max(1)),
        }
    }

    /// Whether `schema` descends from
    /// [`TENANT_TYPE_BASE_GTS_ID`] in its inheritance chain. Walks
    /// `ancestors()` (self → parent → ...) so a leaf, a mid-chain, and
    /// the envelope itself all count as "under the envelope."
    fn descends_from_tenant_type_envelope(schema: &GtsTypeSchema) -> bool {
        schema
            .ancestors()
            .any(|s| s.type_id.as_ref() == TENANT_TYPE_BASE_GTS_ID)
    }

    /// Extract `allowed_parent_types` from an effective trait map.
    /// Returns `None` on any malformed shape (missing key, non-array,
    /// non-string entry, or string that fails `is_type_schema_id`) so
    /// callers refuse the whole trait list — one malformed entry must
    /// not slip through under cover of legitimate siblings. Per-entry
    /// registry probes are skipped on cost grounds; the envelope check
    /// still runs against the actual `parent_type` below.
    fn extract_allowed_parents(traits: &Value) -> Option<Vec<String>> {
        let arr = traits.get(ALLOWED_PARENT_TYPES_TRAIT)?.as_array()?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let s = item.as_str()?;
            if !types_registry_sdk::is_type_schema_id(s) {
                return None;
            }
            out.push(s.to_owned());
        }
        Some(out)
    }
}

#[async_trait]
impl TenantTypeChecker for GtsTenantTypeChecker {
    // @cpt-begin:cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation:p1:inst-algo-apte-gts-impl
    // @cpt-begin:cpt-cf-account-management-algo-tenant-type-enforcement-same-type-nesting-admission:p1:inst-algo-stn-gts-impl
    async fn check_parent_child(
        &self,
        parent_type: Uuid,
        child_type: Uuid,
    ) -> Result<(), DomainError> {
        // Same-type nesting only needs the child schema; different
        // parent needs both. Batch into one GTS round-trip either way.
        let mut requested = vec![child_type];
        if parent_type != child_type {
            requested.push(parent_type);
        }

        // `am.dependency_health` mirrors the IdP-path emission shape
        // (`target` / `op` / `outcome`) so a GTS outage shows up on
        // the unified dependency-health dashboard.
        let map = match tokio::time::timeout(
            self.probe_timeout,
            self.registry.get_type_schemas_by_uuid(requested.clone()),
        )
        .await
        {
            Err(_elapsed) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "types_registry"),
                        ("op", "get_type_schemas_by_uuid"),
                        ("outcome", "timeout"),
                    ],
                );
                return Err(DomainError::service_unavailable(
                    "types-registry: timeout exceeded",
                ));
            }
            Ok(map) => map,
        };

        // Classify the call's transport-layer outcome BEFORE
        // `resolve_schema` reshapes per-entry results. A successful
        // batch return that nonetheless contains a per-entry
        // transport-flavored error (or a missing required entry —
        // a broken-client signal) is an `error` for dashboard
        // purposes; per-entry `CanonicalError::NotFound` is a domain
        // condition (`InvalidTenantType`), not a health signal.
        let had_transport_error = requested.iter().any(|u| match map.get(u) {
            None => true,
            Some(Err(err)) => !matches!(err, CanonicalError::NotFound { .. }),
            Some(Ok(_)) => false,
        });
        emit_metric(
            AM_DEPENDENCY_HEALTH,
            MetricKind::Counter,
            &[
                ("target", "types_registry"),
                ("op", "get_type_schemas_by_uuid"),
                (
                    "outcome",
                    if had_transport_error {
                        "error"
                    } else {
                        "success"
                    },
                ),
            ],
        );

        // Resolve child schema first — trait extraction depends on it.
        let child_schema = resolve_schema(map.get(&child_type), child_type, "child")?;

        // Step 3: child must descend from the AM tenant_type envelope.
        // Anything else cannot legitimately carry `allowed_parent_types`,
        // even if the effective-trait merge happens to produce a value.
        if !Self::descends_from_tenant_type_envelope(child_schema) {
            return Err(DomainError::InvalidTenantType {
                detail: format!(
                    "child tenant type {} ({}) does not descend from {}",
                    child_schema.type_id.as_ref(),
                    child_type,
                    TENANT_TYPE_BASE_GTS_ID,
                ),
            });
        }

        // Step 4: resolve the effective trait map and pull the array.
        // The base envelope's `x-gts-traits-schema` defines a `default: []`
        // for `allowed_parent_types`, so a properly-derived child always
        // has at least an empty array; missing/null/non-array shapes mean
        // the child schema is malformed.
        let effective = child_schema.effective_traits();
        let allowed = Self::extract_allowed_parents(&effective).ok_or_else(|| {
            DomainError::InvalidTenantType {
                detail: format!(
                    "child tenant type {} effective trait `{}` is missing or not an array of strings",
                    child_schema.type_id.as_ref(),
                    ALLOWED_PARENT_TYPES_TRAIT,
                ),
            }
        })?;

        // Step 5: membership check. For same-type nesting the candidate
        // is the child's own identifier; otherwise resolve the parent
        // schema's identifier from the same batched response. The
        // parent must descend from the AM tenant_type envelope as
        // well — without that check a non-tenant-type GTS UUID could
        // be passed in and the membership comparison would fail by
        // luck (its `type_id` wouldn't appear in
        // `allowed_parent_types`) with a misleading `TypeNotAllowed`
        // message instead of the correct `InvalidTenantType`.
        let parent_id_string: String = if parent_type == child_type {
            child_schema.type_id.as_ref().to_owned()
        } else {
            let parent_schema = resolve_schema(map.get(&parent_type), parent_type, "parent")?;
            if !Self::descends_from_tenant_type_envelope(parent_schema) {
                return Err(DomainError::InvalidTenantType {
                    detail: format!(
                        "parent tenant type {} ({}) does not descend from {}",
                        parent_schema.type_id.as_ref(),
                        parent_type,
                        TENANT_TYPE_BASE_GTS_ID,
                    ),
                });
            }
            parent_schema.type_id.as_ref().to_owned()
        };

        if allowed.iter().any(|s| s == &parent_id_string) {
            Ok(())
        } else if parent_type == child_type {
            Err(DomainError::TypeNotAllowed {
                detail: format!(
                    "same-type nesting not permitted for tenant type {}",
                    child_schema.type_id.as_ref(),
                ),
            })
        } else {
            Err(DomainError::TypeNotAllowed {
                detail: format!(
                    "parent tenant type {} not in allowed_parent_types of child {}",
                    parent_id_string,
                    child_schema.type_id.as_ref(),
                ),
            })
        }
    }
    // @cpt-end:cpt-cf-account-management-algo-tenant-type-enforcement-same-type-nesting-admission:p1:inst-algo-stn-gts-impl
    // @cpt-end:cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation:p1:inst-algo-apte-gts-impl
}

/// Map a `get_type_schemas_by_uuid` per-key result onto a `DomainError`:
///
/// * missing entry → `ServiceUnavailable` (the SDK contract guarantees
///   one entry per requested UUID; a missing entry indicates a broken
///   client implementation, not a domain condition)
/// * `CanonicalError::NotFound` → `InvalidTenantType`
/// * any other transport/registry error → `ServiceUnavailable`
///
/// `CanonicalError` Display is forwarded into the detail unredacted
/// (no `redact_provider_detail` here). This is intentional: the GTS
/// Types Registry is a CF-internal sibling gear whose error surface
/// is curated and safe to expose to the caller, in contrast to the `IdP`
/// plugin path (`account_management_sdk::idp`) where the error text comes
/// from third-party vendor SDKs and must be redacted before crossing
/// the AM boundary.
fn resolve_schema<'a>(
    entry: Option<&'a Result<GtsTypeSchema, CanonicalError>>,
    uuid: Uuid,
    role: &'static str,
) -> Result<&'a GtsTypeSchema, DomainError> {
    match entry {
        None => Err(DomainError::service_unavailable(format!(
            "types-registry: {role} uuid {uuid} missing from response"
        ))),
        Some(Ok(schema)) => Ok(schema),
        Some(Err(CanonicalError::NotFound { .. })) => Err(DomainError::InvalidTenantType {
            detail: format!("{role} tenant type {uuid} not registered in GTS"),
        }),
        Some(Err(other)) => Err(DomainError::service_unavailable(format!(
            "types-registry: {other}"
        ))),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]
#[path = "checker_tests.rs"]
mod checker_tests;
