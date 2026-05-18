//! `MetadataService` — domain orchestrator for the tenant-metadata
//! subsystem.
//!
//! Implements FEATURE `tenant-metadata` (see
//! `modules/system/account-management/docs/features/feature-tenant-metadata.md`).
//!
//! Five operations, each PEP-gated through [`MetadataService::authorize`]
//! against [`pep::METADATA`]:
//!
//! * [`MetadataService::list_metadata`] — cursor-paginated direct-on-
//!   tenant listing (NO ancestor walk per FEATURE §3.1). Accepts an
//!   [`ODataQuery`] for filter / order / cursor / limit; returns a
//!   [`modkit_odata::Page<MetadataEntry>`].
//! * [`MetadataService::get_metadata`] — single-entry read; surfaces
//!   the distinct-404 split (`metadata_schema_not_registered` vs
//!   `metadata_entry_not_found`).
//! * [`MetadataService::upsert_metadata`] — upsert at
//!   `(tenant_id, schema_uuid)`; returns the post-write
//!   [`MetadataEntry`]. The insert-vs-update discriminator is recorded
//!   on the `am.events:metadata_upserted` audit line (`outcome=created`
//!   / `updated`) and on the internal [`UpsertOutcome`] returned by
//!   the repo seam — the SDK contract surfaces only the entry per
//!   FEATURE §6 AC line 393, leaving REST status-code mapping
//!   (HTTP 200 vs 201) to the handler (or collapsing to a uniform 200
//!   per RFC 7231 PUT semantics).
//! * [`MetadataService::delete_metadata`] — non-idempotent delete;
//!   missing rows surface as `MetadataEntryNotFound` per
//!   `dod-tenant-metadata-distinct-404-codes`.
//! * [`MetadataService::resolve_metadata`] — barrier-aware walk-up
//!   resolution per `algo-tenant-metadata-resolve-walk-up` and
//!   ADR-0002.
//!
//! # Layering invariant — application-only enforcement (ADR-0002)
//!
//! Inheritance semantics live exclusively in [`MetadataService`]. The
//! storage layer carries only directly-written rows; there is no DB
//! trigger, no materialized inheritance column, no walk-up SQL view.
//! Any SQL reader bypassing this service therefore sees only the
//! direct values for a given tenant — consumers that need inherited
//! values MUST go through this entry point or the future
//! `/api/.../resolved` REST endpoint.

use std::sync::Arc;

use account_management_sdk::{MetadataEntry, UpsertMetadataRequest};
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use gts::GtsSchemaId;
use modkit_macros::domain_model;
use modkit_odata::{ODataQuery, Page};
use modkit_security::{AccessScope, SecurityContext, pep_properties};
use std::collections::HashMap;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metadata::registry::{InheritancePolicy, MetadataSchemaRegistry};
use crate::domain::metadata::repo::MetadataRepo;
use crate::domain::metadata::schema_id::ParsedSchemaId;
use crate::domain::metadata::{MetadataRow, UpsertOutcome};
use crate::domain::tenant::model::{TenantModel, TenantStatus};
use crate::domain::tenant::repo::TenantRepo;

/// PEP descriptors for the tenant-metadata resource.
///
/// Mirrors the `pep::TENANT` declaration on `TenantService` (DESIGN
/// §4.2). The resource type name pins
/// [`account_management_sdk::TENANT_METADATA_RESOURCE_TYPE`]; the
/// duplicated string literal is required because `ResourceType.name`
/// is a `&'static str` consumed at compile time, and a
/// `&'static str = CONST_FROM_SDK` would require const-promotion the
/// SDK does not yet expose. The literal is asserted against the SDK
/// constant in
/// [`crate::domain::error_tests`] so a divergence trips at test time.
pub(super) mod pep {
    use super::{ResourceType, pep_properties};

    /// Resource declaration for `tenant_metadata`. Supported PEP
    /// properties:
    ///
    /// * `OWNER_TENANT_ID` — the tenant the metadata row belongs to;
    ///   ownership-style policies consume this.
    /// * `RESOURCE_ID` — set to the tenant id (`tenant_metadata` does
    ///   not carry an independent resource id of its own; the
    ///   `schema_uuid` is internal storage detail). The
    ///   `InTenantSubtree` constraint compiled by the PDP narrows the
    ///   read to the caller's subtree against `tenant_metadata.tenant_id`
    ///   via the entity's `Scopable(tenant_col = "tenant_id", ...)`
    ///   declaration.
    /// * [`SCHEMA_ID`] — AM-local PEP attribute carrying the chained
    ///   metadata schema id (`gts.cf.core.am.tenant_metadata.v1~vendor.app.foo.v1~`)
    ///   per DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`
    ///   and PRD §"Metadata steward". Set on every
    ///   `get_metadata` / `upsert_metadata` / `delete_metadata` /
    ///   `resolve_metadata` authorize call so per-schema policy grants
    ///   (`Metadata.read` for branding but not billing) can match. The
    ///   `list_metadata` flow authorises once at tenant scope to gate
    ///   the listing operation itself, then re-authorises per row
    ///   with the row's resolved `schema_id` to drop entries the
    ///   caller cannot read — the PRD line 1848 contract.
    pub const METADATA: ResourceType = ResourceType {
        name: "gts.cf.core.am.tenant_metadata.v1~",
        supported_properties: &[
            pep_properties::OWNER_TENANT_ID,
            pep_properties::RESOURCE_ID,
            SCHEMA_ID,
        ],
    };

    /// AM-local PEP attribute name for the chained metadata schema id.
    /// Carries the wire-shaped `gts.…~vendor.…~` chain (the validated
    /// id obtained via
    /// [`crate::domain::metadata::schema_id::ParsedSchemaId::as_str`])
    /// so a PDP rule can match on the exact registered schema rather
    /// than the storage-internal `schema_uuid`. Lives on the AM service
    /// module rather than [`modkit_security::pep_properties`] because
    /// no other module currently carries a per-schema attribute; if a
    /// second consumer appears the const is the natural place to
    /// promote upstream.
    pub const SCHEMA_ID: &str = "schema_id";

    /// Action vocabulary mirroring DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`
    /// (`Metadata.read`, `Metadata.write`, `Metadata.list`,
    /// `Metadata.delete`) plus the AM-specific `resolve` action for
    /// the walk-up path. The `Metadata.` prefix lives on the resource
    /// type ([`METADATA::name`]); these constants carry only the
    /// action verb the PDP rule body matches against. `resolve` is
    /// AM-local: a future PDP rule can deny only inheritance reads
    /// without blocking direct ones.
    pub mod actions {
        pub const READ: &str = "read";
        pub const LIST: &str = "list";
        pub const RESOLVE: &str = "resolve";
        /// `Metadata.write` per DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`.
        /// Used by the upsert flow; PUT and the future PATCH share
        /// the same action — distinguishing them is up to the PDP,
        /// not AM.
        pub const WRITE: &str = "write";
        pub const DELETE: &str = "delete";
    }
}

/// Shared clock seam. Produced by [`MetadataService::new`] from
/// `OffsetDateTime::now_utc` and overridable in tests via
/// [`MetadataService::with_now_fn`]. Mirrors the
/// [`crate::domain::conversion::service::ConversionService`] convention
/// so the unit tests can pin `created_at` / `updated_at` for repeatable
/// idempotency assertions.
type NowFn = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// Central AM domain service for tenant metadata.
///
/// Construction mirrors
/// [`crate::domain::conversion::service::ConversionService`] — every
/// dependency is `Arc<dyn ...>` so production wiring (`module.rs`)
/// and tests (`FakeMetadataRepo` + `FakeTenantRepo` +
/// `StubMetadataSchemaRegistry`) share the same constructor surface.
///
/// The [`PolicyEnforcer`] is owned by-value (it is `Clone`); the
/// module wiring clones it from the shared instance used by
/// `TenantService`.
#[domain_model]
pub struct MetadataService {
    metadata_repo: Arc<dyn MetadataRepo>,
    tenant_repo: Arc<dyn TenantRepo>,
    schema_registry: Arc<dyn MetadataSchemaRegistry>,
    enforcer: PolicyEnforcer,
    now_fn: NowFn,
}

impl MetadataService {
    /// Construct a fully-wired service with the production clock
    /// (`OffsetDateTime::now_utc`).
    #[must_use]
    pub fn new(
        metadata_repo: Arc<dyn MetadataRepo>,
        tenant_repo: Arc<dyn TenantRepo>,
        schema_registry: Arc<dyn MetadataSchemaRegistry>,
        enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            metadata_repo,
            tenant_repo,
            schema_registry,
            enforcer,
            now_fn: Arc::new(OffsetDateTime::now_utc),
        }
    }

    /// PEP gate. Calls the platform-side `PolicyEnforcer`, returns
    /// the [`AccessScope`] the storage layer forwards through
    /// `modkit_db`'s secure builders.
    ///
    /// Mirrors `TenantService::authorize`:
    ///
    /// * `OWNER_TENANT_ID = tenant_id` — the row's owning tenant.
    /// * `RESOURCE_ID = tenant_id` — the same id; `tenant_metadata`
    ///   has no policy-visible resource id beyond its tenant scope.
    /// * `SCHEMA_ID = schema_id` (when supplied) — the chained
    ///   `gts.…~vendor.…~` id of the metadata schema this call
    ///   targets. Per DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`
    ///   policies may restrict `Metadata.read` / `Metadata.write` /
    ///   `Metadata.delete` / `Metadata.list` per-schema; without
    ///   this attribute the PDP would receive only the tenant scope
    ///   and a per-schema grant could not match.
    /// * `require_constraints(true)` — a PDP returning `decision: true,
    ///   constraints: []` fails closed via `CompileFailed →
    ///   CrossTenantDenied` rather than silently widening the read.
    ///
    /// `schema_id` is `None` exclusively for the `list_metadata`
    /// outer gate (the operation-level decision: "is the caller
    /// allowed to *list* on this tenant at all?"). Per-row schema
    /// filtering on the listing page happens through a separate
    /// [`Self::caller_allows_schema_read`] pass.
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        tenant_id: Uuid,
        schema_id: Option<&str>,
    ) -> Result<AccessScope, DomainError> {
        let mut request = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
            .resource_property(pep_properties::RESOURCE_ID, tenant_id)
            .require_constraints(true);
        if let Some(sid) = schema_id {
            // The `IntoPropertyValue: From<&str>` impl on
            // `AccessRequest::resource_property` clones into the
            // wire envelope itself — passing the borrow avoids the
            // double allocation that an intermediate `to_owned()`
            // would incur on the per-row loop (up to 200 rows per
            // page).
            request = request.resource_property(pep::SCHEMA_ID, sid);
        }
        let scope = self
            .enforcer
            .access_scope_with(ctx, &pep::METADATA, action, Some(tenant_id), &request)
            .await?;
        Ok(scope)
    }

    /// Per-row schema-scoped authorization helper used by
    /// [`Self::list_metadata`] to drop entries the caller is not
    /// permitted to read. Returns `true` iff the PDP allows
    /// `Metadata.read` on `(tenant_id, schema_id)` under the same
    /// `SecurityContext` that gated the outer list operation; a
    /// `CrossTenantDenied` decision (the boundary error
    /// [`PolicyEnforcer::access_scope_with`] surfaces for "decision:
    /// false") is silently dropped — that is the contract per
    /// PRD §1848 ("list responses omit entries the actor is not
    /// permitted to read"). Any other error is propagated so the
    /// caller can fail closed on PDP transport failures.
    ///
    /// Performance note: invoked once per row on the listing page.
    /// The platform-side `PolicyEnforcer` is expected to cache
    /// decisions for the duration of a request (per the resolver
    /// contract). Page sizes are capped at 200 by the
    /// `METADATA_LIMIT_CFG` constant in
    /// `crate::infra::storage::repo_impl::metadata` (private item;
    /// path quoted for navigation only, not an intra-doc link), so
    /// the worst-case fan-out is bounded.
    async fn caller_allows_schema_read(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: &str,
    ) -> Result<bool, DomainError> {
        match self
            .authorize(ctx, pep::actions::READ, tenant_id, Some(schema_id))
            .await
        {
            Ok(_) => Ok(true),
            Err(DomainError::CrossTenantDenied { .. }) => Ok(false),
            Err(other) => Err(other),
        }
    }

    /// Override the wall-clock function used to stamp `created_at`
    /// / `updated_at` on the upsert path. Mirrors
    /// [`crate::domain::conversion::service::ConversionService::with_now_fn`].
    #[must_use]
    pub fn with_now_fn(mut self, now_fn: NowFn) -> Self {
        self.now_fn = now_fn;
        self
    }

    /// Snapshot the current wall-clock through the configured
    /// `now_fn`.
    fn now(&self) -> OffsetDateTime {
        (self.now_fn)()
    }

    // ----------------------------------------------------------------
    // list_metadata
    // ----------------------------------------------------------------

    /// Cursor-paginated direct-on-tenant listing.
    ///
    /// Implements `cpt-cf-account-management-flow-tenant-metadata-list`.
    /// The query MUST NOT walk ancestors per FEATURE §3.1 — clients
    /// reading effective values use [`Self::resolve_metadata`].
    ///
    /// `query` carries `$filter` / `$orderby` / `$cursor` / `$limit`
    /// per the SDK contract; the repo layer pushes them into SQL via
    /// `modkit_db::odata::paginate_odata`. Stable tiebreaker on
    /// `schema_uuid` keeps cursor re-reads deterministic.
    ///
    /// Each returned [`MetadataEntry`] carries the public chained
    /// `schema_id` re-hydrated from the registry per FEATURE §2 step
    /// 4 — `dbtable-tenant-metadata` MUST NOT retain the public id
    /// per `dod-tenant-metadata-schema-registration-and-uuid-derivation`.
    ///
    /// # Errors
    ///
    /// * [`DomainError::CrossTenantDenied`] — PDP denies the caller.
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] — tenant is `Provisioning` or
    ///   `Deleted` (Active + Suspended pass per FEATURE spec), or
    ///   the `ODataQuery` carries an unsupported filter / cursor
    ///   shape.
    /// * [`DomainError::MetadataSchemaNotRegistered`] — a stored row
    ///   carries a `schema_uuid` whose chained id is missing from the
    ///   registry. This is a data-integrity signal; in practice
    ///   schemas are removed from the registry only after every
    ///   tenant has dropped its row, but the service surfaces the
    ///   condition rather than swallowing it.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-list:p1:inst-flow-mdlist-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-list-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-application-only-enforcement:p1:inst-dod-app-only-list-service
    #[tracing::instrument(skip(self, query), fields(tenant_id = %tenant_id))]
    pub async fn list_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<MetadataEntry>, DomainError> {
        // Outer gate: caller must be allowed to LIST on the tenant at
        // all. Per-row schema filtering happens after the page is
        // hydrated (`caller_allows_schema_read` below).
        let scope = self
            .authorize(ctx, pep::actions::LIST, tenant_id, None)
            .await?;

        // Tenant existence + status guard runs BEFORE any DB read on
        // `tenant_metadata` per FEATURE §2 list error scenarios.
        let _tenant = self.resolve_visible_tenant(&scope, tenant_id).await?;

        // Direct-on-tenant only — NO ancestor walk per FEATURE §3.1.
        let page = self
            .metadata_repo
            .list_for_tenant(&scope, tenant_id, query)
            .await?;

        // Reverse-hydrate the chained `schema_id` for the page rows in
        // one batch call. The registry adapter resolves all uuids in a
        // single round-trip and the lookup below is a pure map read.
        // Rows whose `schema_uuid` is no longer registered are an
        // integrity-pipeline signal — operators get a precise
        // `MetadataSchemaNotRegistered` rather than a panic.
        let uuids: Vec<Uuid> = page.items.iter().map(|r| r.schema_uuid).collect();
        let id_by_uuid: HashMap<Uuid, GtsSchemaId> =
            self.schema_registry.resolve_ids_by_uuid(&uuids).await?;

        // Per-row schema-scoped authorization. Per PRD §1848 ("list
        // responses omit entries the actor is not permitted to read")
        // and DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`
        // the listing surface MUST filter entries by per-schema
        // `Metadata.read` policy — otherwise a caller granted only
        // `branding` could enumerate `billing` rows just by listing.
        // Outer `LIST` already passed at line 322; this loop is the
        // per-row `READ` gate on the hydrated rows. `CrossTenantDenied`
        // for a row is silently dropped (the documented "omit"
        // semantic); any other PDP failure propagates so transport /
        // policy-load errors fail closed.
        let mut items: Vec<MetadataEntry> = Vec::with_capacity(page.items.len());
        for row in page.items {
            let gts_schema_id = id_by_uuid.get(&row.schema_uuid).cloned().ok_or_else(|| {
                DomainError::MetadataSchemaNotRegistered {
                    detail: format!("schema_uuid {} not registered", row.schema_uuid),
                    schema: row.schema_uuid.to_string(),
                }
            })?;
            if !self
                .caller_allows_schema_read(ctx, tenant_id, gts_schema_id.as_ref())
                .await?
            {
                continue;
            }
            items.push(project_to_entry(row, gts_schema_id));
        }

        // NOTE on cursor stability: filtering rows out of the
        // hydrated page returns a `Page` whose `items.len()` can be
        // less than `page_info.limit`. The cursor itself is computed
        // by the repo over the un-filtered set, so the next page
        // still advances correctly — callers walking the listing
        // observe gaps in cardinality but no missing rows. Returning
        // `items` smaller than `limit` is the same contract the
        // `OData` platform helpers honour when the underlying row
        // count is less than the requested limit.
        //
        // Terminal-page case: when the repo has no further rows
        // (`page_info.next_cursor = None`), the caller stops walking
        // regardless of how many entries the per-row schema-deny
        // filter dropped — the cursor presence (or absence) is the
        // termination signal, not `items.len() == limit`. A page
        // ending with `next_cursor = None` and zero passing rows is
        // a legitimate "no readable metadata on this tenant" reply
        // rather than a stuck-pagination bug.
        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-application-only-enforcement:p1:inst-dod-app-only-list-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-list-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-metadata-list:p1:inst-flow-mdlist-service

    // ----------------------------------------------------------------
    // get_for_tenant
    // ----------------------------------------------------------------

    /// Single-entry read keyed by `(tenant_id, schema_id)`.
    ///
    /// Implements `cpt-cf-account-management-flow-tenant-metadata-get`.
    ///
    /// Distinct-404 disambiguation per
    /// `dod-tenant-metadata-distinct-404-codes`:
    ///
    /// * Schema unknown to the registry →
    ///   [`DomainError::MetadataSchemaNotRegistered`] (HTTP 404,
    ///   `code=metadata_schema_not_registered`).
    /// * Schema known but no row at `(tenant_id, schema_uuid)` →
    ///   [`DomainError::MetadataEntryNotFound`] (HTTP 404,
    ///   `code=metadata_entry_not_found`).
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] — tenant is `Provisioning` or
    ///   `Deleted` (Active + Suspended pass per FEATURE spec).
    /// * [`DomainError::MetadataSchemaNotRegistered`] — see above.
    /// * [`DomainError::MetadataEntryNotFound`] — see above.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-get:p1:inst-flow-mdget-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-get-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-distinct-404-codes:p1:inst-dod-distinct-404-get-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-get-service
    #[tracing::instrument(skip(self), fields(tenant_id = %tenant_id, schema_id = %schema_id))]
    pub async fn get_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<MetadataEntry, DomainError> {
        let parsed = ParsedSchemaId::parse(schema_id.as_ref())?;
        let scope = self
            .authorize(ctx, pep::actions::READ, tenant_id, Some(parsed.as_str()))
            .await?;

        // Tenant existence + status guard runs BEFORE the registry call.
        let _tenant = self.resolve_visible_tenant(&scope, tenant_id).await?;

        // Existence gate: registry resolves the policy AND signals
        // unregistered. We don't need the policy on the GET path but
        // the same RPC is the cheapest existence check (one round-trip
        // serves both reads and writes). The error variant carries
        // `schema_id` verbatim so the canonical envelope can surface
        // the requested id without re-parsing the path.
        let _policy = self
            .schema_registry
            .resolve_inheritance_policy(parsed.as_gts())
            .await?;

        // UUIDv5 derivation cached on `ParsedSchemaId` (matches the
        // upstream `gts::GtsID::to_uuid()` namespace per
        // `dod-tenant-metadata-schema-registration-and-uuid-derivation`).
        let schema_uuid = parsed.uuid();

        let row = self
            .metadata_repo
            .get_for_tenant(&scope, tenant_id, schema_uuid)
            .await?
            .ok_or_else(|| DomainError::MetadataEntryNotFound {
                detail: format!("no metadata entry for tenant {tenant_id} at schema {schema_id}"),
                entry: format!("({tenant_id}, {schema_id})"),
            })?;

        Ok(project_to_entry(row, schema_id))
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-get-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-distinct-404-codes:p1:inst-dod-distinct-404-get-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-get-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-metadata-get:p1:inst-flow-mdget-service

    // ----------------------------------------------------------------
    // put_for_tenant
    // ----------------------------------------------------------------

    /// Upsert the row at `(tenant_id, schema_uuid)`.
    ///
    /// Implements `cpt-cf-account-management-flow-tenant-metadata-put`.
    /// The return value is the post-write [`MetadataEntry`]; the
    /// insert-vs-update discriminator is recorded on the
    /// `am.events:metadata_upserted` audit line (`outcome=created` /
    /// `updated`) and on the internal [`UpsertOutcome`] crossing the
    /// repo seam. The SDK contract surfaces only the entry per
    /// FEATURE §6 AC line 393 — REST status-code mapping (200 vs 201)
    /// is the handler's call.
    ///
    /// Guard ordering (full, matches FEATURE §6 AC):
    /// 1. `ParsedSchemaId::parse(input.schema_id)` — pure input
    ///    validation. Reads no external state; rejecting malformed
    ///    GTS syntax / wrong root segment / instance-id shapes here
    ///    does not leak tenant topology because the error depends
    ///    solely on the caller-supplied bytes.
    /// 2. `value.is_null()` — pure input validation; same rationale
    ///    as step 1. The SDK boundary deliberately allows
    ///    `Value::Null` (see `upsert_metadata_request_accepts_any_non_missing_value`
    ///    in `metadata_tests.rs`); the service-side rejection
    ///    surfaces as [`DomainError::Validation`] before any state
    ///    read.
    /// 3. `authorize` — PEP gate. Carries the `SCHEMA_ID` attribute
    ///    so per-schema policies (`Metadata.write` on branding but
    ///    not billing) can match.
    /// 4. `resolve_visible_tenant` — `NotFound` / Provisioning /
    ///    Deleted collapses BEFORE any registry lookup so tenant
    ///    topology does not leak through a registry-call error.
    ///    Suspended tenants pass — metadata writes on suspended
    ///    tenants are explicitly allowed by the FEATURE spec.
    /// 5. `schema_registry.resolve_inheritance_policy` — the
    ///    schema-existence gate. Unregistered schemas surface as
    ///    [`DomainError::MetadataSchemaNotRegistered`] without ever
    ///    touching the validator.
    /// 6. `schema_registry.validate_value` — GTS body validation
    ///    against the registered JSON Schema. Payload-fail surfaces
    ///    as [`DomainError::Validation`] BEFORE any DB write,
    ///    fingerprinting `dod-tenant-metadata-crud-contract` line 393.
    /// 7. `metadata_repo.upsert_for_tenant` — the actual write.
    ///
    /// Steps 1–3 run before the tenant-existence gate intentionally:
    /// (1, 2) are pure input checks whose error variant cannot encode
    /// any server-side state, and (3) is the PEP gate which by design
    /// MUST run for every request regardless of resource state so
    /// PDP-deny is uniformly 403. Steps 4–7 carry the topology
    /// ordering: tenant existence is checked before registry / DB
    /// operations so an unregistered-schema or missing-row error
    /// cannot surface on a non-existent tenant.
    ///
    /// `requested_by` is recorded on the success-side `am.events`
    /// line for audit correlation.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] — tenant is `Provisioning` or
    ///   `Deleted` (Active + Suspended pass per FEATURE spec), or
    ///   `value` violates the registered JSON Schema body.
    /// * [`DomainError::MetadataSchemaNotRegistered`] — schema not
    ///   in the registry; no row written.
    /// * [`DomainError::ServiceUnavailable`] — types-registry transport
    ///   failure; no row written.
    /// * [`DomainError::Internal`] — registered schema is not a valid
    ///   JSON Schema (catalog drift); no row written.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-put:p1:inst-flow-mdput-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-put-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-put-service
    #[tracing::instrument(
        skip(self, input),
        fields(tenant_id = %tenant_id, schema_id = %input.schema_id, actor_uuid = %ctx.subject_id())
    )]
    pub async fn upsert_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        input: UpsertMetadataRequest,
    ) -> Result<MetadataEntry, DomainError> {
        let UpsertMetadataRequest {
            schema_id,
            value,
            expected_version,
            ..
        } = input;
        let parsed = ParsedSchemaId::parse(schema_id.as_ref())?;

        // Non-null `value` was previously enforced by
        // `UpsertMetadataRequest::new` (returning the now-removed
        // `MetadataValidationError::EmptyValue`). Now that the SDK
        // ships a plain JSON shape with no validation, the gate runs
        // service-side and surfaces as `DomainError::Validation`
        // (HTTP 400) at the canonical boundary.
        if value.is_null() {
            return Err(DomainError::Validation {
                detail: "metadata value must not be null".into(),
            });
        }

        let actor = ctx.subject_id();
        let scope = self
            .authorize(ctx, pep::actions::WRITE, tenant_id, Some(parsed.as_str()))
            .await?;

        let _tenant = self.resolve_visible_tenant(&scope, tenant_id).await?;

        let _policy = self
            .schema_registry
            .resolve_inheritance_policy(parsed.as_gts())
            .await?;

        // GTS body validation. Runs AFTER the existence gate above so
        // an unregistered-schema PUT still surfaces 404, not 400, per
        // `dod-tenant-metadata-distinct-404-codes`. The registry's
        // local-client cache amortizes the second round-trip in the
        // steady state.
        self.schema_registry
            .validate_value(parsed.as_gts(), &value)
            .await?;

        let schema_uuid = parsed.uuid();
        let now = self.now();

        let outcome = self
            .metadata_repo
            .upsert_for_tenant(&scope, tenant_id, schema_uuid, value, now, expected_version)
            .await?;

        let was_inserted = outcome.was_inserted();
        let row = match outcome {
            UpsertOutcome::Inserted(row) | UpsertOutcome::Updated(row) => row,
        };
        let entry = project_to_entry(row, schema_id.clone());

        // Audit emission with `schema_id` on the structured log. The
        // `outcome` field carries `created` / `updated` so a downstream
        // aggregator counting by (event, outcome) can still distinguish
        // the insert vs rewrite path even though the public SDK
        // contract collapses both into a single `MetadataEntry`
        // return.
        tracing::info!(
            target: "am.events",
            event = "metadata_upserted",
            tenant_id = %tenant_id,
            schema_id = %schema_id,
            actor_uuid = %actor,
            outcome = if was_inserted { "created" } else { "updated" },
            "am tenant metadata upserted"
        );

        Ok(entry)
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-put-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-put-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-metadata-put:p1:inst-flow-mdput-service

    // ----------------------------------------------------------------
    // delete_for_tenant
    // ----------------------------------------------------------------

    /// Delete the row at `(tenant_id, schema_uuid)`.
    ///
    /// Implements `cpt-cf-account-management-flow-tenant-metadata-delete`.
    ///
    /// DELETE is intentionally NOT idempotent-success on missing rows:
    /// the distinct-404 contract per
    /// `dod-tenant-metadata-distinct-404-codes` makes the signal
    /// observable to clients. Missing rows surface as
    /// [`DomainError::MetadataEntryNotFound`].
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] — tenant is `Provisioning` or
    ///   `Deleted` (Active + Suspended pass per FEATURE spec).
    /// * [`DomainError::MetadataSchemaNotRegistered`] — schema not
    ///   registered; no DB write issued.
    /// * [`DomainError::MetadataEntryNotFound`] — schema known but no
    ///   row at `(tenant_id, schema_uuid)`.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-delete:p1:inst-flow-mddel-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-delete-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-distinct-404-codes:p1:inst-dod-distinct-404-delete-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-delete-service
    #[tracing::instrument(
        skip(self),
        fields(tenant_id = %tenant_id, schema_id = %schema_id, actor_uuid = %ctx.subject_id())
    )]
    pub async fn delete_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<(), DomainError> {
        let parsed = ParsedSchemaId::parse(schema_id.as_ref())?;
        let actor = ctx.subject_id();
        let scope = self
            .authorize(ctx, pep::actions::DELETE, tenant_id, Some(parsed.as_str()))
            .await?;

        let _tenant = self.resolve_visible_tenant(&scope, tenant_id).await?;

        let _policy = self
            .schema_registry
            .resolve_inheritance_policy(parsed.as_gts())
            .await?;

        let schema_uuid = parsed.uuid();

        // The repo's `delete_for_tenant` returns
        // [`DomainError::MetadataEntryNotFound`] on missing rows,
        // satisfying the distinct-404 contract without an additional
        // service-side existence probe. Remap to use the public
        // `schema_id` in `detail` / `entry` so the wire shape matches
        // `get_metadata`'s NotFound projection (which the repo cannot
        // synthesise because it only sees the internal `schema_uuid`).
        // Without the remap, GET and DELETE on the same missing entry
        // would surface two different `entry` payloads, breaking
        // aggregators keyed on that field.
        self.metadata_repo
            .delete_for_tenant(&scope, tenant_id, schema_uuid)
            .await
            .map_err(|e| match e {
                DomainError::MetadataEntryNotFound { .. } => DomainError::MetadataEntryNotFound {
                    detail: format!(
                        "no metadata entry for tenant {tenant_id} at schema {schema_id}"
                    ),
                    entry: format!("({tenant_id}, {schema_id})"),
                },
                other => other,
            })?;

        tracing::info!(
            target: "am.events",
            event = "metadata_deleted",
            tenant_id = %tenant_id,
            schema_id = %schema_id,
            actor_uuid = %actor,
            outcome = "ok",
            "am tenant metadata deleted"
        );

        Ok(())
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-delete-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-distinct-404-codes:p1:inst-dod-distinct-404-delete-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-delete-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-metadata-delete:p1:inst-flow-mddel-service

    // ----------------------------------------------------------------
    // resolve_for_tenant
    // ----------------------------------------------------------------

    /// Barrier-aware effective-value resolution.
    ///
    /// Implements `cpt-cf-account-management-flow-tenant-metadata-resolve`
    /// + `cpt-cf-account-management-algo-tenant-metadata-resolve-walk-up`.
    ///
    /// Empty resolution is `Ok(None)` — the normal terminal state of
    /// an unsuccessful walk per FEATURE §3 / DESIGN §3.2.3, NOT
    /// [`DomainError::MetadataEntryNotFound`]. Per
    /// `dod-tenant-metadata-distinct-404-codes` the future REST
    /// handler surfaces `Ok(None)` as HTTP 200 with an empty
    /// response.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] — tenant is `Provisioning` or
    ///   `Deleted` (Active + Suspended pass per FEATURE spec).
    /// * [`DomainError::MetadataSchemaNotRegistered`] — schema not
    ///   registered; no walk performed.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-resolve:p1:inst-flow-mdres-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-inheritance-resolution-contract:p1:inst-dod-inheritance-resolve-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-application-only-enforcement:p1:inst-dod-app-only-resolve-service
    #[tracing::instrument(skip(self), fields(tenant_id = %tenant_id, schema_id = %schema_id))]
    pub async fn resolve_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<Option<MetadataEntry>, DomainError> {
        let parsed = ParsedSchemaId::parse(schema_id.as_ref())?;
        let scope = self
            .authorize(ctx, pep::actions::RESOLVE, tenant_id, Some(parsed.as_str()))
            .await?;

        let start_tenant = self.resolve_visible_tenant(&scope, tenant_id).await?;

        // The walk-up algorithm consumes the resolved policy as its
        // sole controller per the DoD; unregistered schemas surface
        // here BEFORE any walk is attempted.
        let policy = self
            .schema_registry
            .resolve_inheritance_policy(parsed.as_gts())
            .await?;

        let schema_uuid = parsed.uuid();

        let row = self
            .resolve_walk_up(&scope, &start_tenant, schema_uuid, policy)
            .await?;

        Ok(row.map(|r| project_to_entry(r, schema_id)))
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-application-only-enforcement:p1:inst-dod-app-only-resolve-service
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-inheritance-resolution-contract:p1:inst-dod-inheritance-resolve-service
    // @cpt-end:cpt-cf-account-management-flow-tenant-metadata-resolve:p1:inst-flow-mdres-service

    /// Barrier-aware ancestor walk-up per FEATURE §3
    /// `algo-tenant-metadata-resolve-walk-up`.
    ///
    /// Step ordering MUST match the FEATURE doc verbatim:
    ///
    /// 1. Read direct entry at `(start.id, schema_uuid)`. Hit ⇒ return.
    /// 2. `OverrideOnly` ⇒ return empty.
    /// 3. Start tenant `self_managed == true` ⇒ return empty
    ///    (start-tenant barrier).
    /// 4. Walk loop: advance to `parent_id`; null ⇒ root-empty;
    ///    self-managed ancestor ⇒ barrier-empty BEFORE reading;
    ///    suspended ancestor ⇒ skip-traverse; otherwise read and
    ///    return on hit, loop on miss.
    ///
    /// Application-only-enforcement contract: this is a pure
    /// service-layer computation. No DB trigger, no materialized
    /// inheritance column, no walk-up SQL view.
    // @cpt-begin:cpt-cf-account-management-algo-tenant-metadata-resolve-walk-up:p1:inst-algo-walk-up-service
    async fn resolve_walk_up(
        &self,
        scope: &AccessScope,
        start_tenant: &TenantModel,
        schema_uuid: Uuid,
        policy: InheritancePolicy,
    ) -> Result<Option<MetadataRow>, DomainError> {
        // Hard cap on the ancestor walk to bound an integrity-violating
        // `parent_id` cycle (A->B->A) — without this guard the loop
        // below would issue one DB round-trip per hop forever. The
        // integrity-check pipeline can detect and repair cycles but
        // does not prevent them atomically, so the walk-up must be
        // defensive. Mirrors `MAX_ANCESTOR_WALK_HOPS` in
        // `infra::storage::repo_impl::updates`; well above any
        // realistic tenant hierarchy depth.
        const MAX_WALK_HOPS: usize = 64;

        // Step 1 — own row first. Direct hit is returned regardless
        // of the start tenant's `self_managed` flag (the barrier only
        // blocks INHERITANCE from ancestors; own values are always
        // surfaced per `inst-algo-walk-own-return`).
        if let Some(row) = self
            .metadata_repo
            .get_for_tenant(scope, start_tenant.id, schema_uuid)
            .await?
        {
            return Ok(Some(row));
        }

        // Step 2 — override_only short-circuits before any tenant-row
        // load. `inst-algo-walk-override-return`.
        if matches!(policy, InheritancePolicy::OverrideOnly) {
            return Ok(None);
        }

        // Step 3 — start-tenant barrier. A self-managed tenant never
        // inherits from ancestors above its barrier per
        // `principle-barrier-as-data` /
        // `inst-algo-walk-start-barrier-return`.
        if start_tenant.self_managed {
            return Ok(None);
        }

        // Walk init (`inst-algo-walk-init`): `current = start`. We
        // already loaded the start row through `resolve_visible_tenant`
        // so the loop body is structured around `current.parent_id`
        // null-check first per the FEATURE-doc step ordering (step 7
        // gates step 8).
        //
        // Note on suspended start: `resolve_visible_tenant` accepts
        // both `Active` and `Suspended` (the FEATURE spec allows
        // metadata flows on suspended tenants). The `self_managed`
        // barrier check above and the suspended-skip rule applied
        // inside the loop both run uniformly regardless of the
        // start tenant's status — a suspended self-managed start
        // still short-circuits at step 3, and a suspended
        // non-self-managed start with an own row still surfaces it
        // via step 1.
        let mut current_parent = start_tenant.parent_id;

        // allow_all for the ancestor walk:
        //
        // The PEP gate above already authorised the caller on
        // `start_tenant` (the resource the caller actually named).
        // Once that gate has passed, walking up the ancestor chain
        // for `Inherit`-policy inheritance is a STRUCTURAL read --
        // the result is projected through the start tenant's
        // visibility, never disclosed directly. The
        // post-#1813 `tenants` entity declares `resource_col = "id"`
        // (and `tenant_metadata` declares `tenant_col = "tenant_id"`),
        // so reusing the caller's narrowed scope here would clamp
        // both reads to descendants of the start tenant -- and an
        // ancestor is by definition NOT in the start tenant's
        // descendant subtree. The narrowed scope would therefore
        // turn every ancestor lookup into a dangling-parent
        // `Internal` error (step 8) or a silent miss (step 11),
        // collapsing the FEATURE's inheritance semantics. Mirrors
        // the saga-internal `allow_all` reads in `TenantService`
        // (`create_tenant`, `update_tenant`, `delete_tenant` parent /
        // structural-precondition reads).
        let walk_scope = AccessScope::allow_all();

        // Hop counter for the `MAX_WALK_HOPS` cycle guard. Declared
        // at the function top (see the `const` near the entry) so
        // Clippy's `items_after_statements` stays happy; this
        // mutable binding has to live next to the loop.
        let mut hops: usize = 0;

        loop {
            // Step 7 — root reached without a value.
            // `inst-algo-walk-root-return`.
            let Some(parent_id) = current_parent else {
                return Ok(None);
            };

            // Cycle / overflow guard: a `parent_id` cycle in the
            // `tenants` table would otherwise spin this loop
            // indefinitely. `>=` so the cap counts the upcoming hop.
            if hops >= MAX_WALK_HOPS {
                return Err(DomainError::internal(format!(
                    "metadata walk-up exceeded {MAX_WALK_HOPS} hops from start tenant {} at parent {parent_id}; possible parent_id cycle",
                    start_tenant.id,
                )));
            }
            hops += 1;

            // Step 8 — load the ancestor row.
            let ancestor = self
                .tenant_repo
                .find_by_id(&walk_scope, parent_id)
                .await?
                .ok_or_else(|| {
                    // A stored `parent_id` referencing a missing
                    // tenant row is a hierarchy-integrity violation.
                    // Surface it as `Internal` so the integrity-check
                    // pipeline can surface the dangling-parent
                    // signal; the walk does not silently terminate
                    // because that would mask the data-integrity
                    // signal under an empty-resolved response.
                    DomainError::internal(format!(
                        "metadata walk-up: parent tenant {parent_id} is missing (dangling parent_id reference)"
                    ))
                })?;

            // Step 9 — barrier-stop ancestor: return empty BEFORE
            // reading the ancestor's value per `inst-algo-walk-ancestor-barrier-return`.
            if ancestor.self_managed {
                return Ok(None);
            }

            // Step 10 — suspended ancestor: skip the read but
            // continue the walk to its parent. Suspension is a
            // lifecycle state, not a barrier per
            // `inst-algo-walk-suspended-continue`.
            if matches!(ancestor.status, TenantStatus::Suspended) {
                current_parent = ancestor.parent_id;
                continue;
            }

            // Step 11 — read ancestor's direct entry through
            // `walk_scope` (`allow_all`). Same structural-read
            // rationale as the ancestor `find_by_id` above: the
            // caller's narrowed `scope` was already enforced on the
            // start-tenant own-row read at the top of this function;
            // ancestors live outside that subtree by definition and
            // must be reached via `walk_scope` to honour the
            // `Inherit` policy.
            if let Some(row) = self
                .metadata_repo
                .get_for_tenant(&walk_scope, ancestor.id, schema_uuid)
                .await?
            {
                // Step 12 — return the ancestor's value.
                return Ok(Some(row));
            }

            // Step 13 — loop back to root-reached check with the new
            // `current`. `inst-algo-walk-loop`.
            current_parent = ancestor.parent_id;
        }
    }
    // @cpt-end:cpt-cf-account-management-algo-tenant-metadata-resolve-walk-up:p1:inst-algo-walk-up-service

    // ----------------------------------------------------------------
    // helpers
    // ----------------------------------------------------------------

    /// Resolve `tenant_id` to a visible tenant for the metadata
    /// flows. Accepts both [`TenantStatus::Active`] **and**
    /// [`TenantStatus::Suspended`]; rejects `Provisioning` and
    /// `Deleted`.
    ///
    /// Deliberately wider than
    /// [`crate::domain::user::service::UserService::resolve_active_tenant`]:
    /// the FEATURE spec
    /// (`docs/features/feature-tenant-metadata.md`) and DESIGN row
    /// "Metadata steward" allow `Metadata.{read,write,delete,list}`
    /// on visible tenants regardless of suspension state — a
    /// suspended tenant can still display its branding, billing,
    /// and configuration metadata to operators. `resolve_metadata`
    /// in particular needs this so the walk-up algorithm can apply
    /// the documented suspended-skip rule from a suspended start
    /// tenant (and so the algorithm's `self_managed` short-circuit
    /// can fire on a suspended start). User-side flows (provision /
    /// list users etc.) keep the stricter active-only gate because
    /// the `IdP` plugin contract requires an Active tenant.
    ///
    /// # Scope on the start-tenant read
    ///
    /// The PEP-compiled `scope` is forwarded to the `tenants`
    /// `find_by_id` so a misconfigured / overly-permissive PDP that
    /// returns `decision: true` for an out-of-subtree `tenant_id`
    /// (e.g. with only an `InTenantSubtree(caller_subtree)`
    /// constraint) still fails closed at the database. The
    /// post-#1813 `tenants` entity declares `resource_col = "id"`,
    /// so the `InTenantSubtree` predicate compiles to
    /// `tenants.id IN tenant_closure(caller_subtree)` and collapses
    /// an out-of-subtree row to `NotFound`. The `OWNER_TENANT_ID`
    /// predicate carried by the same scope does not appear on
    /// `tenants` and resolves to `None` at the per-filter
    /// compilation step — the surviving `RESOURCE_ID` /
    /// `InTenantSubtree` predicate keeps the clamp intact
    /// (fail-closed at the filter level; OR-of-constraints
    /// semantics across the two `Constraint` alternatives the PDP
    /// emits).
    async fn resolve_visible_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<TenantModel, DomainError> {
        let tenant = self
            .tenant_repo
            .find_by_id(scope, tenant_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                detail: format!("tenant {tenant_id} not found"),
                resource: tenant_id.to_string(),
            })?;

        match tenant.status {
            TenantStatus::Active | TenantStatus::Suspended => Ok(tenant),
            TenantStatus::Provisioning | TenantStatus::Deleted => Err(DomainError::Validation {
                detail: format!(
                    "tenant {} is not visible to metadata flows (status={})",
                    tenant.id,
                    tenant.status.as_str()
                ),
            }),
        }
    }
}

/// Project a [`MetadataRow`] + its public chained `schema_id` into
/// the [`MetadataEntry`] surface returned by every read-flow.
fn project_to_entry(row: MetadataRow, schema_id: GtsSchemaId) -> MetadataEntry {
    MetadataEntry::new(schema_id, row.value, row.updated_at, row.version)
}
