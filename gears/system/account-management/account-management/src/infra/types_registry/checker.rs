//! Production [`TenantTypeChecker`] over `types_registry_sdk::TypesRegistryApi`.
//! Each check uses the request's context without retaining it (P33).
//!
//! For Feature 2.3 (`algo-allowed-parent-types-evaluation`,
//! `algo-same-type-nesting-admission`):
//!
//! 1. Batch-read the child and, if different, parent schemas' `effective_traits`.
//! 2. Require the `gts.cf.core.am.tenant_type.v1~` envelope.
//! 3. Admit only when the effective `allowed_parent_types` contains the parent's
//!    chained ID, including the type's own ID for same-type nesting.
//!
//! Admission materializes traits with leaf values taking precedence over
//! deepest-base defaults. Missing selected traits are a contract break.
//! Unregistered types map to [`DomainError::InvalidTenantType`], disallowed
//! pairings to [`DomainError::TypeNotAllowed`]; registry faults use the caller
//! error classifier.
//!
//! No adapter cache (`dod-tenant-type-enforcement-gts-availability-surface`);
//! short-lived caching belongs to the SDK client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use toolkit_gts::gts_id;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_security::SecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsId, LifecycleStatus, Projection, ReadOptions, TypeSchema,
    TypesRegistryApi, TypesRegistryApiExt,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, MetricKind, emit_metric};
use crate::domain::tenant_type::checker::TenantTypeChecker;

/// GTS identifier of the AM tenant-type envelope. Children that do not
/// descend from this schema cannot legitimately resolve an
/// `allowed_parent_types` trait — they're rejected as
/// [`DomainError::InvalidTenantType`] regardless of what the
/// effective-trait merge produces.
const TENANT_TYPE_BASE_GTS_ID: &str = gts_id!("cf.core.am.tenant_type.v1~");

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
    registry: Arc<dyn TypesRegistryApi>,
    probe_timeout: Duration,
}

impl GtsTenantTypeChecker {
    /// Construct a new checker around a registry client resolved from
    /// `ClientHub`, using the default probe timeout.
    #[must_use]
    pub fn new(registry: Arc<dyn TypesRegistryApi>) -> Self {
        Self::with_timeout(registry, DEFAULT_PROBE_TIMEOUT_MS)
    }

    /// Construct a checker with an explicit GTS probe timeout (ms).
    #[must_use]
    pub fn with_timeout(registry: Arc<dyn TypesRegistryApi>, probe_timeout_ms: u64) -> Self {
        Self {
            registry,
            probe_timeout: Duration::from_millis(probe_timeout_ms.max(1)),
        }
    }

    /// Whether the schema's GTS chain starts at the AM tenant-type envelope.
    fn descends_from_tenant_type_envelope(schema: &TypeSchema) -> bool {
        GtsId::try_new(schema.type_id.as_ref())
            .ok()
            .and_then(|id| id.chain_ids().into_iter().next())
            .is_some_and(|root| root == TENANT_TYPE_BASE_GTS_ID)
    }

    /// Extract `allowed_parent_types` from an effective-trait map, or
    /// `None` when the entry is missing or not an array of type-schema
    /// identifiers (a single malformed entry collapses the whole list).
    fn extract_allowed_parents(traits: &Value) -> Option<Vec<String>> {
        let arr = traits.get(ALLOWED_PARENT_TYPES_TRAIT)?.as_array()?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let s = item.as_str()?;
            if !GtsId::try_new(s).is_ok_and(|id| id.is_type()) {
                return None;
            }
            out.push(s.to_owned());
        }
        Some(out)
    }

    fn emit_probe(outcome: &'static str) {
        emit_metric(
            AM_DEPENDENCY_HEALTH,
            MetricKind::Counter,
            &[
                ("target", "types_registry"),
                ("op", "get_type_schemas_by_uuid"),
                ("outcome", outcome),
            ],
        );
    }
}

#[async_trait]
impl TenantTypeChecker for GtsTenantTypeChecker {
    async fn check_parent_child(
        &self,
        ctx: &SecurityContext,
        parent_type: Uuid,
        child_type: Uuid,
    ) -> Result<(), DomainError> {
        let mut requested = vec![child_type];
        if parent_type != child_type {
            requested.push(parent_type);
        }
        let projection = Projection::Select(FieldSelection::with(&[EntityField::EffectiveTraits]));

        let map = match tokio::time::timeout(
            self.probe_timeout,
            self.registry.batch_get_type_schemas_by_uuid(
                ctx,
                &requested,
                ReadOptions {
                    projection,
                    ..ReadOptions::default()
                },
            ),
        )
        .await
        {
            Err(_elapsed) => {
                Self::emit_probe("timeout");
                return Err(DomainError::service_unavailable(
                    "types-registry: timeout exceeded",
                ));
            }
            Ok(Err(err)) => {
                Self::emit_probe("error");
                return Err(DomainError::caller_registry_failure(err, "types-registry"));
            }
            Ok(Ok(map)) => {
                Self::emit_probe("success");
                map
            }
        };

        let child_schema = resolve_schema(&map, child_type, "child")?;

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

        let effective = child_schema.effective_traits.as_ref().ok_or_else(|| {
            DomainError::internal(format!(
                "types-registry answered child tenant type {} without the selected \
                 effective_traits",
                child_schema.type_id.as_ref(),
            ))
        })?;
        let allowed = Self::extract_allowed_parents(effective).ok_or_else(|| {
            DomainError::InvalidTenantType {
                detail: format!(
                    "child tenant type {} effective trait `{}` is missing or not an array of strings",
                    child_schema.type_id.as_ref(),
                    ALLOWED_PARENT_TYPES_TRAIT,
                ),
            }
        })?;

        let parent_id_string: String = if parent_type == child_type {
            child_schema.type_id.as_ref().to_owned()
        } else {
            let parent_schema = resolve_schema(&map, parent_type, "parent")?;
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
}

/// Look up one requested UUID: an unanswered key is a contract break of
/// the registry client (service unavailable), `None` is an unregistered
/// tenant type, and a deleted one admits no new pairing either — exact
/// reads answer tombstones, which the legacy catalogue never held.
fn resolve_schema<'a>(
    map: &'a HashMap<Uuid, Option<TypeSchema>>,
    uuid: Uuid,
    role: &'static str,
) -> Result<&'a TypeSchema, DomainError> {
    match map.get(&uuid) {
        None => Err(DomainError::service_unavailable(format!(
            "types-registry: {role} uuid {uuid} missing from response"
        ))),
        Some(None) => Err(DomainError::InvalidTenantType {
            detail: format!("{role} tenant type {uuid} not registered in GTS"),
        }),
        Some(Some(schema)) if schema.lifecycle_status != LifecycleStatus::Active => {
            Err(DomainError::InvalidTenantType {
                detail: format!(
                    "{role} tenant type {} ({uuid}) is deleted in GTS",
                    schema.type_id.as_ref()
                ),
            })
        }
        Some(Some(schema)) => Ok(schema),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]
#[path = "checker_tests.rs"]
mod checker_tests;
