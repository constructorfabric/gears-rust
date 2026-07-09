//! `GtsMetadataSchemaRegistry` — production [`MetadataSchemaRegistry`]
//! wired against `types_registry_sdk::TypesRegistryClient` resolved
//! from `ClientHub`.
//!
//! Responsibilities (see
//! [`crate::domain::metadata::registry::MetadataSchemaRegistry`]):
//!
//! 1. *Existence* — surface unknown schemas as
//!    [`DomainError::MetadataEntryNotFound`] BEFORE any downstream
//!    DB read or write. AM does not distinguish "schema unknown" from
//!    "entry missing" on the wire — both collapse into the unified
//!    metadata 404.
//! 2. *Inheritance policy* — read the schema's
//!    `x-gts-traits.inheritance_policy` (effective, chain-merged) and
//!    map onto [`InheritancePolicy::Inherit`] / [`InheritancePolicy::OverrideOnly`].
//!    Default per FEATURE §3.1 is `override_only`, so a missing /
//!    null / non-`"inherit"` value collapses to `OverrideOnly`.
//! 3. *Reverse hydration* — `schema_uuid → String` via
//!    [`TypesRegistryClient::get_type_schema_by_uuid`]; the `type_id`
//!    on the resolved schema is returned verbatim as the chained
//!    `gts.cf.core.am.tenant_metadata.v1~vendor.app.foo.v1~` string.
//!
//! Determinism contract (per the trait): the adapter MUST NOT cache
//! across calls. Every invocation re-resolves through the SDK so trait
//! updates take effect immediately. The SDK's local-client cache is
//! responsible for any short-lived caching it chooses to do internally.

use std::sync::Arc;

use async_trait::async_trait;
use gts::{GtsID, GtsTypeId};
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metadata::registry::{InheritancePolicy, MetadataSchemaRegistry};

/// Top-level key in the effective trait map carrying the inheritance
/// policy enum value. Defined on the `gts.cf.core.am.tenant_metadata.v1~`
/// envelope's `x-gts-traits-schema` (see
/// `account_management_sdk::gts_envelopes::TenantMetadataTraits`, the
/// Rust source of truth the macro emits the trait schema from).
const INHERITANCE_POLICY_TRAIT: &str = "inheritance_policy";

/// Wire token for the `Inherit` policy. Anything else (missing key,
/// `null`, non-string value, unknown string) collapses to the
/// documented default ([`InheritancePolicy::OverrideOnly`]) per
/// FEATURE §3.1.
const INHERIT_POLICY_TOKEN: &str = "inherit";
const OVERRIDE_POLICY_TOKEN: &str = "override_only";

/// Production [`MetadataSchemaRegistry`] backed by the GTS Types
/// Registry.
pub struct GtsMetadataSchemaRegistry {
    registry: Arc<dyn TypesRegistryClient>,
}

impl GtsMetadataSchemaRegistry {
    /// Construct a new registry adapter around a `TypesRegistryClient`
    /// resolved from `ClientHub`.
    #[must_use]
    pub fn new(registry: Arc<dyn TypesRegistryClient>) -> Self {
        Self { registry }
    }
}

/// Map a `CanonicalError` onto the appropriate `DomainError` for
/// the schema-registry seam:
///
/// * `CanonicalError::NotFound` → [`DomainError::MetadataEntryNotFound`]
///   (HTTP 404 with the unified `resource_type =
///   gts.cf.core.am.tenant_metadata.v1~`).
/// * any other transport / registry error → `ServiceUnavailable`.
fn map_registry_err(err: CanonicalError, schema_token: &str) -> DomainError {
    match err {
        CanonicalError::NotFound { .. } => DomainError::MetadataEntryNotFound {
            detail: format!("schema {schema_token} is not registered in the types registry"),
            entry: schema_token.to_owned(),
        },
        other => DomainError::service_unavailable(format!("types-registry: {other}")),
    }
}

/// Compute the deterministic `schema_uuid` for an already-validated
/// `type_id` string. Mirrors the helper in
/// [`crate::domain::metadata::registry`] — both rely on
/// `gts::GtsID::to_uuid()` for the canonical namespace.
///
/// # Panics
///
/// Panics if `type_id` is not parseable as a GTS id. Callers MUST
/// pass strings already validated by
/// [`crate::domain::metadata::type_id::ParsedTypeId::parse`] (the
/// service-layer guard runs before the registry sees the value).
#[allow(
    clippy::expect_used,
    reason = "callers validate via ParsedTypeId before invoking registry; \
              an unparseable input here is a service-layer contract break"
)]
fn uuid_for_registered_schema(type_id: &str) -> Uuid {
    GtsID::new(type_id)
        .expect(
            "types-registry returned a type_id that does not parse as a GTS id - \
             upstream SDK contract break",
        )
        .to_uuid()
}

#[async_trait]
impl MetadataSchemaRegistry for GtsMetadataSchemaRegistry {
    async fn resolve_inheritance_policy(
        &self,
        type_id: &GtsTypeId,
    ) -> Result<InheritancePolicy, DomainError> {
        // Forward the public chained id to the registry. The registry's
        // local-client cache will resolve this against the in-memory
        // schema map (no extra hop in the steady state).
        let schema_str = type_id.as_ref();
        let schema = self
            .registry
            .get_type_schema(schema_str)
            .await
            .map_err(|err| map_registry_err(err, schema_str))?;

        // `effective_traits` walks self → ancestors with rightmost-wins
        // semantics, then layers schema-declared `default` values from
        // every `x-gts-traits-schema` block in the chain. The base
        // envelope (`gts.cf.core.am.tenant_metadata.v1~`) declares
        // `inheritance_policy: { default: "override_only" }` so a
        // properly-derived schema always carries a string value here.
        let effective = schema.effective_traits();
        let raw = effective.get(INHERITANCE_POLICY_TRAIT);
        let policy = match raw {
            // Explicit `"inherit"` opt-in.
            Some(v) if v.as_str() == Some(INHERIT_POLICY_TOKEN) => InheritancePolicy::Inherit,
            // Explicit `"override_only"` OR an absent / null trait
            // (FEATURE §3.1 defaults to override-only when the trait
            // is unspecified). Both are valid wire shapes; treat as
            // the documented default silently.
            Some(v) if v.as_str() == Some(OVERRIDE_POLICY_TOKEN) => InheritancePolicy::OverrideOnly,
            None => InheritancePolicy::OverrideOnly,
            Some(v) if v.is_null() => InheritancePolicy::OverrideOnly,
            // Truly unknown shape: typo, future variant, wrong wire
            // type. Warn so the operator sees the drift before users
            // observe unexpected walk-up behaviour, then default to
            // override-only (the safe-default in the walk-up
            // algorithm -- an unrecognised value cannot accidentally
            // promote a schema to inheriting from ancestors).
            Some(other) => {
                tracing::warn!(
                    target: "am.metadata",
                    type_id = %type_id,
                    raw_value = ?other,
                    "unknown inheritance_policy value; defaulting to override_only"
                );
                InheritancePolicy::OverrideOnly
            }
        };
        Ok(policy)
    }

    async fn resolve_ids_by_uuid(
        &self,
        schema_uuids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, GtsTypeId>, DomainError> {
        // The SDK has no native multi-id call; loop through
        // `resolve_id_by_uuid` and rely on the SDK's local-client
        // snapshot cache for amortisation (every steady-state hit is a
        // pure `HashMap::get` after the first cold load). Page size is
        // bounded by `IdpUserPagination::MAX_TOP`, so the worst-case cold
        // page is a small constant of round-trips.
        let mut out = std::collections::HashMap::with_capacity(schema_uuids.len());
        for &uuid in schema_uuids {
            match self.resolve_id_by_uuid(uuid).await {
                Ok(id) => {
                    out.insert(uuid, id);
                }
                Err(DomainError::MetadataEntryNotFound { .. }) => {
                    // Page-poisoning guard: omit unknowns; service layer
                    // decides per-row how to surface the miss.
                }
                Err(other) => return Err(other),
            }
        }
        Ok(out)
    }

    async fn resolve_id_by_uuid(&self, schema_uuid: Uuid) -> Result<GtsTypeId, DomainError> {
        let schema = self
            .registry
            .get_type_schema_by_uuid(schema_uuid)
            .await
            .map_err(|err| map_registry_err(err, &schema_uuid.to_string()))?;

        // The resolved `schema.type_id` is a `GtsTypeId` that the
        // registry has already validated. Defense-in-depth: re-derive
        // the UUID from the resolved `type_id` and confirm it matches
        // the input. The SDK guarantees this on its own, but a future
        // SDK bug that mapped an arbitrary schema to a `schema_uuid`
        // would otherwise let a List flow re-hydrate the wrong public
        // id alongside a tenant's stored row. Surface as `Internal` so
        // the bug is loud.
        //
        // We do NOT re-validate the chain-prefix shape here (the
        // retired SDK `MetadataSchemaId::try_from` used to do that):
        // the only callers of this registry are the metadata flows,
        // and a non-tenant-metadata schema in the storage row could
        // only have landed there via a manual write that bypassed
        // `MetadataService::upsert_metadata` (which validates via
        // `ParsedTypeId::parse`). The UUID round-trip check below
        // catches the same class of bug end-to-end without coupling
        // the registry adapter to the AM root-segment constant.
        let raw = schema.type_id.as_ref();
        if uuid_for_registered_schema(raw) != schema_uuid {
            return Err(DomainError::Internal {
                diagnostic: format!(
                    "types-registry returned schema {raw} for uuid {schema_uuid} but the AM-side \
                     UUIDv5 derivation does not round-trip; possible SDK bug or schema renaming \
                     mid-flight"
                ),
                cause: None,
            });
        }
        Ok(GtsTypeId::new(raw))
    }

    async fn validate_value(&self, type_id: &GtsTypeId, value: &Value) -> Result<(), DomainError> {
        // Mirrors the `validate_provision_input_metadata_via_gts` pattern
        // from `domain::gts_validation` (the canonical AM seam for GTS
        // body validation) — single round-trip to the registry, then
        // `jsonschema::validator_for` on the effective (chain-merged)
        // schema, then `iter_errors` to collect every violation into one
        // `DomainError::MetadataValidation` detail.
        //
        // The error-shape differs by one variant vs the IdP helper:
        // a missing schema here surfaces as
        // [`DomainError::MetadataEntryNotFound`] (HTTP 404 with the
        // unified metadata `resource_type`), not `Internal` — metadata
        // schemas are caller-named so an unregistered chain is a
        // public 404, not a deploy-prerequisite failure.
        let schema_str = type_id.as_ref();
        let schema = self
            .registry
            .get_type_schema(schema_str)
            .await
            .map_err(|err| map_registry_err(err, schema_str))?;
        let resolved = schema.effective_schema();
        // The base AM metadata envelope declares `$schema:
        // "http://json-schema.org/draft-07/schema#"`, and derived metadata
        // schemas carry `$schema: "gts://gts.cf.core.am.tenant_metadata.v1~"`
        // -- the GTS chain identifier that AM uses for inheritance. The
        // generic `jsonschema::validator_for` auto-detects the draft from
        // `$schema` and fails closed when the URI is not a registered
        // meta-schema (which custom GTS chain ids never are). Pin the
        // draft to 7 explicitly so the chain id is treated as opaque
        // metadata and validation runs against the well-known draft the
        // envelope was authored for.
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .build(&resolved)
            .map_err(|err| {
                // Catalog drift: the schema body in the registry is not a
                // valid JSON Schema. Operator action required; surface as
                // `Internal` so the public envelope does not pretend the
                // caller's payload is the problem.
                DomainError::Internal {
                    diagnostic: format!(
                        "GTS metadata schema `{type_id}` is not a valid JSON Schema \
                         (catalog drift): {err}"
                    ),
                    cause: None,
                }
            })?;
        let errors: Vec<String> = validator
            .iter_errors(value)
            .map(|e| e.to_string())
            .collect();
        if !errors.is_empty() {
            return Err(DomainError::MetadataValidation {
                detail: format!(
                    "metadata value violates registered schema `{type_id}`: {}",
                    errors.join("; ")
                ),
            });
        }
        Ok(())
    }
}
