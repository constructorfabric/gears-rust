//! Types-registry integration port for tenant metadata schemas.
//!
//! [`MetadataSchemaRegistry`] is the read-only abstraction the
//! [`crate::domain::metadata::service::MetadataService`] uses to look
//! up GTS-registry state on every per-schema operation. Three
//! responsibilities:
//!
//! 1. *Existence* — surface unknown schemas as
//!    [`DomainError::MetadataEntryNotFound`] BEFORE any DB read or
//!    write. AM no longer distinguishes "schema unknown to registry"
//!    from "entry absent for this tenant" on the wire — both
//!    collapse into a single 404 with `resource_type =
//!    gts.cf.core.am.tenant_metadata.v1~`.
//! 2. *Inheritance policy* — resolve the schema's `inheritance_policy`
//!    trait (from `x-gts-traits`, default `override_only`). The
//!    walk-up algorithm consumes this to decide whether to walk
//!    `parent_id` ancestors or short-circuit to an empty result.
//! 3. *Reverse hydration* — map the storage-side `schema_uuid` (PK
//!    component) back onto its public chained `type_id` string for
//!    list responses per FEATURE §2 step 4. The list flow needs this
//!    because `dbtable-tenant-metadata` MUST NOT retain the public
//!    `type_id` per `dod-tenant-metadata-schema-registration-and-uuid-derivation`.
//!
//! Two implementations exist:
//!
//! * [`StubMetadataSchemaRegistry`] — in-memory test fake. Mirrors
//!   [`crate::domain::tenant_type::checker::InertTenantTypeChecker`]
//!   in shape but accepts pre-seeded `(type_id, InheritancePolicy)`
//!   pairs so per-schema policy can be scripted from service-level
//!   unit tests. Reverse lookup uses the same map keyed by the
//!   deterministic `UUIDv5` derivation via upstream
//!   [`gts::GtsId::to_uuid`].
//! * `GtsMetadataSchemaRegistry` — the production implementation
//!   backed by `types_registry_sdk::TypesRegistryClient`.
//!
//! # Schema-id type contract
//!
//! All forward methods accept `&gts::GtsTypeId` — the platform-standard
//! marker for "this string is a GTS schema id". Callers
//! (`MetadataService`) parse + validate via
//! [`crate::domain::metadata::type_id::ParsedTypeId::parse`]
//! BEFORE invoking the registry, then hand off the typed view via
//! [`crate::domain::metadata::type_id::ParsedTypeId::as_gts`], so by
//! the time the trait method runs the id is guaranteed to be a
//! well-formed AM tenant-metadata schema id. Reverse methods return
//! [`gts::GtsTypeId`] directly — the consumer (typically the list-flow
//! projection) lowers it to `String` for the SDK wire shape only at the
//! `MetadataEntry` boundary.
//!
//! # Determinism contract
//!
//! Like [`crate::domain::tenant_type::checker::TenantTypeChecker`], the
//! registry MUST NOT cache results across calls — every invocation
//! re-resolves so trait updates take effect immediately.

#![allow(
    dead_code,
    reason = "Stub registry exposes constructors that not every test wires; the surface mirrors InertTenantTypeChecker so future tests can opt in without redefining the type."
)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use gts::{GtsId, GtsTypeId};
use parking_lot::Mutex;
use serde_json::Value;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Resolved value of a metadata schema's `inheritance_policy` trait.
///
/// FEATURE §3 / `algo-tenant-metadata-resolve-walk-up` describes only
/// these two values — the `override_only` default plus the explicit
/// `inherit` opt-in. Future values (`merge`, `readonly`, `computed`)
/// are deliberately deferred per FEATURE §7. The enum is
/// `#[non_exhaustive]` so additional variants stay SemVer-additive.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InheritancePolicy {
    /// Default per DESIGN §3.1: the tenant's own row is the only
    /// source. No ancestor walk; `resolve` returns own value or empty.
    OverrideOnly,
    /// Walk `parent_id` ancestors, stopping at the nearest self-managed
    /// barrier per ADR-0002 + `principle-barrier-as-data`. Suspended
    /// ancestors on the path are skipped (suspension is a lifecycle
    /// state, not a barrier).
    Inherit,
}

impl InheritancePolicy {
    /// Lower into a stable lowercase token for audit / tracing fields.
    /// Pinned here so the wire shape is independent of any future
    /// derive changes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OverrideOnly => "override_only",
            Self::Inherit => "inherit",
        }
    }
}

/// Read-only port the metadata service uses to consult the GTS Types
/// Registry on every per-schema operation.
///
/// # Errors
///
/// * [`DomainError::MetadataEntryNotFound`] — the supplied chained
///   `type_id` (or `schema_uuid` for reverse lookup) is not
///   registered. The service layer maps this to HTTP 404 with
///   `resource_type = gts.cf.core.am.tenant_metadata.v1~` — the
///   "schema unknown" and "entry missing" cases collapse to the
///   same wire shape per the unified-not-found contract.
/// * [`DomainError::ServiceUnavailable`] — registry transport
///   failure. The service layer surfaces this unchanged so the
///   feature-errors-observability envelope can attach a retry-after
///   hint.
#[async_trait]
pub trait MetadataSchemaRegistry: Send + Sync {
    /// Resolve the `inheritance_policy` trait for `type_id`. The
    /// returned value drives the walk-up algorithm: `OverrideOnly`
    /// short-circuits to own-or-empty; `Inherit` triggers the
    /// barrier-aware ancestor walk.
    ///
    /// Implementations MUST also use this method as the existence
    /// gate — an unregistered schema surfaces
    /// [`DomainError::MetadataEntryNotFound`] without falling back
    /// to the `OverrideOnly` default; otherwise reads against an
    /// unregistered schema would silently succeed with an empty
    /// projection.
    async fn resolve_inheritance_policy(
        &self,
        type_id: &GtsTypeId,
    ) -> Result<InheritancePolicy, DomainError>;

    /// Reverse-lookup the public chained `type_id` for a stored
    /// `schema_uuid`. Used by the per-row resolve path (e.g. the
    /// `/resolved` walk-up) to re-hydrate the public identifier from a
    /// single storage row.
    ///
    /// Returns [`DomainError::MetadataEntryNotFound`] when no schema
    /// in the registry hashes to the supplied `schema_uuid`. Note that
    /// in production this signals a true orphan row (chain was deleted
    /// from the registry after metadata was written) — the LIST flow
    /// in `MetadataService::list_metadata` treats this case as
    /// `Internal` rather than surfacing the UUID through a public
    /// envelope; this variant lets the registry stay schema-typed even
    /// when the row-level caller knows better.
    async fn resolve_id_by_uuid(&self, schema_uuid: Uuid) -> Result<GtsTypeId, DomainError>;

    /// Batch reverse-lookup for the LIST flow: resolve a slice of
    /// `schema_uuid` values to their public chained ids in a single
    /// round-trip. The returned map contains an entry only for
    /// uuids that ARE registered — the caller surfaces
    /// [`DomainError::MetadataEntryNotFound`] (or, in the LIST flow,
    /// `Internal`) for any row whose `schema_uuid` is missing from
    /// the map, so a single unregistered row does not poison the
    /// whole page.
    ///
    /// Default impl is N round-trips per page; production adapter
    /// overrides to a single registry call to amortise per-page.
    ///
    /// # Errors
    ///
    /// * [`DomainError::ServiceUnavailable`] — registry transport
    ///   failure (uniform with the single-row variant).
    async fn resolve_ids_by_uuid(
        &self,
        schema_uuids: &[Uuid],
    ) -> Result<HashMap<Uuid, GtsTypeId>, DomainError> {
        let mut out = HashMap::with_capacity(schema_uuids.len());
        for &uuid in schema_uuids {
            match self.resolve_id_by_uuid(uuid).await {
                Ok(id) => {
                    out.insert(uuid, id);
                }
                Err(DomainError::MetadataEntryNotFound { .. }) => {
                    // Page-poisoning guard: omit unknowns; caller
                    // decides per-row how to surface the miss.
                }
                Err(other) => return Err(other),
            }
        }
        Ok(out)
    }

    /// Validate `value` against the registered JSON Schema body for
    /// `type_id`. Fingerprints
    /// `dod-tenant-metadata-crud-contract` (AC §6 line 393) — the PUT
    /// flow MUST reject body-schema violations with `Validation` BEFORE
    /// any DB write.
    ///
    /// # Errors
    ///
    /// * [`DomainError::MetadataEntryNotFound`] — schema is not
    ///   registered. The PUT handler surfaces this as a uniform 404
    ///   (the unified-not-found contract: AM no longer distinguishes
    ///   "schema unknown" from "entry missing" on the wire).
    /// * [`DomainError::ServiceUnavailable`] — registry transport
    ///   failure.
    /// * [`DomainError::Internal`] — the registered schema is itself
    ///   not a valid JSON Schema (catalog drift; operator action
    ///   required).
    /// * [`DomainError::Validation`] — `value` violates the schema;
    ///   the PUT handler maps this to HTTP 400 `code=validation`.
    async fn validate_value(&self, type_id: &GtsTypeId, value: &Value) -> Result<(), DomainError>;
}

/// Compute the deterministic `schema_uuid` for an already-validated
/// `type_id` string. AM-internal helper shared between Stub and
/// production registries — both rely on `gts::GtsId::to_uuid()` for
/// the canonical namespace.
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
fn uuid_for_registered_schema(type_id: &GtsTypeId) -> Uuid {
    GtsId::try_new(type_id.as_ref())
        .expect(
            "registry was given a type_id that does not parse as a GTS id - \
             caller (service layer) is contract-broken",
        )
        .to_uuid()
}

/// In-memory test fake.
///
/// State is `HashMap<GtsTypeId, InheritancePolicy>` plus a derived
/// `HashMap<Uuid, GtsTypeId>` reverse index keyed by
/// [`uuid_for_registered_schema`]. Both are kept in sync inside the
/// same `Mutex` so cloned handles share state.
#[domain_model]
#[derive(Clone)]
pub struct StubMetadataSchemaRegistry {
    inner: Arc<Mutex<StubState>>,
}

#[domain_model]
struct StubState {
    by_id: HashMap<GtsTypeId, InheritancePolicy>,
    by_uuid: HashMap<Uuid, GtsTypeId>,
    /// Schemas for which [`StubMetadataSchemaRegistry::validate_value`]
    /// surfaces [`DomainError::Validation`] regardless of the supplied
    /// `value`. Lets a negative service test pin the
    /// "schema registered AND payload rejected" branch without standing
    /// up a real JSON Schema validator.
    fail_validation: HashSet<GtsTypeId>,
}

impl StubState {
    fn new() -> Self {
        Self {
            by_id: HashMap::new(),
            by_uuid: HashMap::new(),
            fail_validation: HashSet::new(),
        }
    }

    fn register(&mut self, type_id: GtsTypeId, policy: InheritancePolicy) {
        let uuid = uuid_for_registered_schema(&type_id);
        self.by_uuid.insert(uuid, type_id.clone());
        self.by_id.insert(type_id, policy);
    }
}

impl StubMetadataSchemaRegistry {
    /// Build an empty stub — every lookup surfaces
    /// [`DomainError::MetadataEntryNotFound`]. Useful for tests that
    /// pin the unified 404 contract on an unknown schema.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StubState::new())),
        }
    }

    /// Build a stub seeded with `(type_id, policy)` pairs. Mirrors
    /// the `FakeMetadataRepo::with_seed` ergonomic.
    #[must_use]
    pub fn with_seed(entries: Vec<(GtsTypeId, InheritancePolicy)>) -> Self {
        let stub = Self::new();
        {
            let mut state = stub.inner.lock();
            for (schema, policy) in entries {
                state.register(schema, policy);
            }
        }
        stub
    }

    /// Register a schema after construction. Last-write-wins on
    /// duplicate `type_id`.
    pub fn register(&self, type_id: GtsTypeId, policy: InheritancePolicy) {
        self.inner.lock().register(type_id, policy);
    }

    /// Mark `type_id` so [`Self::validate_value`] surfaces
    /// [`DomainError::Validation`] on every call against it. Used by
    /// negative service tests to pin the "schema registered AND body
    /// rejected" branch without invoking the real JSON Schema validator.
    pub fn fail_validation_for(&self, type_id: GtsTypeId) {
        self.inner.lock().fail_validation.insert(type_id);
    }
}

impl Default for StubMetadataSchemaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MetadataSchemaRegistry for StubMetadataSchemaRegistry {
    async fn resolve_inheritance_policy(
        &self,
        type_id: &GtsTypeId,
    ) -> Result<InheritancePolicy, DomainError> {
        let state = self.inner.lock();
        state
            .by_id
            .get(type_id)
            .copied()
            .ok_or_else(|| DomainError::MetadataEntryNotFound {
                detail: format!("schema {type_id} is not registered in the types registry"),
                entry: type_id.to_string(),
            })
    }

    async fn resolve_id_by_uuid(&self, schema_uuid: Uuid) -> Result<GtsTypeId, DomainError> {
        let state = self.inner.lock();
        state
            .by_uuid
            .get(&schema_uuid)
            .cloned()
            .ok_or_else(|| DomainError::MetadataEntryNotFound {
                detail: format!("schema_uuid {schema_uuid} not registered in the types registry"),
                entry: schema_uuid.to_string(),
            })
    }

    async fn validate_value(&self, type_id: &GtsTypeId, _value: &Value) -> Result<(), DomainError> {
        let state = self.inner.lock();
        // Existence gate first — keeps the stub honest about the
        // unified-404 contract: validate_value against an unregistered
        // schema MUST surface `MetadataEntryNotFound`, not collapse to
        // `Ok(())`. Mirrors `resolve_inheritance_policy`.
        if !state.by_id.contains_key(type_id) {
            return Err(DomainError::MetadataEntryNotFound {
                detail: format!("schema {type_id} is not registered in the types registry"),
                entry: type_id.to_string(),
            });
        }
        if state.fail_validation.contains(type_id) {
            return Err(DomainError::MetadataValidation {
                detail: format!("stub: configured to reject every payload for schema `{type_id}`"),
            });
        }
        Ok(())
    }
}
