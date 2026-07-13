//! `MetadataService` — domain orchestrator for the tenant-metadata
//! subsystem.
//!
//! Implements FEATURE `tenant-metadata` (see
//! `gears/system/account-management/docs/features/feature-tenant-metadata.md`).
//!
//! Five operations, each PEP-gated through [`MetadataService::authorize`]
//! against [`pep::METADATA`]:
//!
//! * [`MetadataService::list_metadata`] — cursor-paginated direct-on-
//!   tenant listing (NO ancestor walk per FEATURE §3.1). Accepts an
//!   [`ODataQuery`] for filter / order / cursor / limit; returns a
//!   [`toolkit_odata::Page<MetadataEntry>`].
//! * [`MetadataService::get_metadata`] — single-entry read; surfaces
//!   the unified metadata 404 (both "schema unknown to registry"
//!   and "entry missing for tenant" collapse to
//!   [`DomainError::MetadataEntryNotFound`]).
//! * [`MetadataService::upsert_metadata`] — upsert at
//!   `(tenant_id, schema_uuid)`; returns the post-write
//!   [`MetadataEntry`]. The insert-vs-update discriminator is recorded
//!   on the `am.events:metadata_upserted` audit line (`outcome=created`
//!   / `updated`) and on the internal [`UpsertOutcome`] returned by
//!   the repo seam — the SDK contract surfaces only the entry per
//!   FEATURE §6 AC line 393, leaving REST status-code mapping
//!   (HTTP 200 vs 201) to the handler (or collapsing to a uniform 200
//!   per RFC 7231 PUT semantics).
//! * [`MetadataService::delete_metadata`] — idempotent delete: returns
//!   `Ok(())` whether the row existed or not (mirrors `delete_user`
//!   deprovision idempotency). Tenant-existence and
//!   schema-registration gates still surface their own 404 codes.
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
use authz_resolver_sdk::pep::ResourceType;
use gts::GtsTypeId;
use std::collections::HashMap;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metadata::registry::{InheritancePolicy, MetadataSchemaRegistry};
use crate::domain::metadata::repo::MetadataRepo;
use crate::domain::metadata::type_id::ParsedTypeId;
use crate::domain::metadata::{MetadataRow, UpsertOutcome};
use crate::domain::tenant::model::{TenantModel, TenantStatus};
use crate::domain::tenant::repo::TenantRepo;

/// PEP descriptors for the tenant-metadata resource.
///
/// Mirrors the `pep::TENANT` declaration on `TenantService` (DESIGN
/// §4.2). The resource type name pins
/// [`account_management_sdk::TENANT_METADATA_RESOURCE_TYPE`]; the
/// literal duplication is cross-checked against the SDK constant in
/// [`crate::domain::error_tests`] so a divergence trips at test time.
pub(crate) mod pep {
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
    /// * [`TYPE_ID`] — AM-local PEP attribute carrying the chained
    ///   metadata schema id (`gts.cf.core.am.tenant_metadata.v1~vendor.app.foo.v1~`)
    ///   per DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`
    ///   and PRD §"Metadata steward". Set on every
    ///   `get_metadata` / `upsert_metadata` / `delete_metadata` /
    ///   `resolve_metadata` authorize call so per-schema policy grants
    ///   (`Metadata.read` for branding but not billing) can match. The
    ///   `list_metadata` flow authorises once at tenant scope to gate
    ///   the listing operation itself, then re-authorises per row
    ///   with the row's resolved `type_id` to drop entries the
    ///   caller cannot read — the PRD line 1848 contract.
    pub const METADATA: ResourceType = ResourceType::from_static(
        account_management_sdk::TENANT_METADATA_RESOURCE_TYPE,
        &[
            pep_properties::OWNER_TENANT_ID,
            pep_properties::RESOURCE_ID,
            TYPE_ID,
        ],
    );

    /// AM-local PEP attribute name for the chained metadata schema id.
    /// Carries the wire-shaped `gts.…~vendor.…~` chain (the validated
    /// id obtained via
    /// [`crate::domain::metadata::type_id::ParsedTypeId::as_str`])
    /// so a PDP rule can match on the exact registered schema rather
    /// than the storage-internal `schema_uuid`. Lives on the AM service
    /// gear rather than [`toolkit_security::pep_properties`] because
    /// no other gear currently carries a per-schema attribute; if a
    /// second consumer appears the const is the natural place to
    /// promote upstream.
    pub const TYPE_ID: &str = "type_id";

    /// Action vocabulary mirroring DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`
    /// (`Metadata.read`, `Metadata.write`, `Metadata.list`,
    /// `Metadata.delete`). The `Metadata.` prefix lives on the resource
    /// type ([`METADATA::name`]); these constants carry only the
    /// action verb the PDP rule body matches against.
    ///
    /// The inheritance-aware `/resolved` endpoint reuses [`READ`]
    /// rather than carrying a separate `resolve` verb: the resolved
    /// value is the caller's effective config for their schema slot
    /// (the inheritance walk is bounded by `self_managed` barriers
    /// and the schema's policy), so it is logically still a read of
    /// that slot. A caller with `Metadata.read` grant on the schema
    /// should be able to call both `GET /metadata/{type_id}` and
    /// `GET /metadata/{type_id}/resolved` without an additional
    /// per-endpoint permission. Per-row source disambiguation belongs
    /// in the response surface (the [`ResolvedTenantMetadataDto::source_tenant_id`](account_management_sdk)
    /// field is reserved for surfacing the ancestor that produced the
    /// value) and audit log enrichment, not in the policy verb.
    pub mod actions {
        pub const READ: &str = "read";
        pub const LIST: &str = "list";
        /// `Metadata.write` per DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`.
        /// Used by the upsert flow; PUT and the future PATCH share
        /// the same action — distinguishing them is up to the PDP,
        /// not AM.
        pub const WRITE: &str = "write";
        pub const DELETE: &str = "delete";
    }
}

/// Clock seam: production uses `OffsetDateTime::now_utc`; tests
/// override via [`MetadataService::with_now_fn`] to pin timestamps.
type NowFn = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// Central tenant-metadata orchestrator. Deps are `Arc<dyn ...>` so
/// prod wiring and unit tests share one constructor; [`PolicyEnforcer`]
/// is by-value (it is `Clone`).
#[domain_model]
pub struct MetadataService {
    metadata_repo: Arc<dyn MetadataRepo>,
    tenant_repo: Arc<dyn TenantRepo>,
    schema_registry: Arc<dyn MetadataSchemaRegistry>,
    enforcer: PolicyEnforcer,
    now_fn: NowFn,
    /// Per-deployment `$top` cap; override via
    /// [`Self::with_listing_max_top`] from `cfg.listing.max_top`.
    /// Default = [`DEFAULT_MAX_LISTING_TOP`].
    max_listing_top: u32,
}

/// Default `$top` cap surfaced by [`MetadataService::max_listing_top`]
/// when the production wiring does not call [`MetadataService::with_listing_max_top`]
/// to override it. Mirrors the platform-wide listing cap baked into
/// the metadata-repo listing config.
const DEFAULT_MAX_LISTING_TOP: u32 = 200;

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
            max_listing_top: DEFAULT_MAX_LISTING_TOP,
        }
    }

    /// Operator-tunable per-deployment listing cap. The gear bootstrap
    /// passes `cfg.listing.max_top` so the metadata listing surface
    /// stays uniform with the tenant / conversion listing caps. Mirrors
    /// [`crate::domain::tenant::service::TenantService::max_list_children_top`].
    #[must_use]
    pub const fn with_listing_max_top(mut self, max_top: u32) -> Self {
        self.max_listing_top = max_top;
        self
    }

    /// Per-deployment `$top` cap. The REST handler calls
    /// [`crate::api::rest::handlers::common::clamp_listing_top`] with
    /// this value so a deployment that tightened
    /// `cfg.listing.max_top` below the repo-level absolute ceiling
    /// (200) sees the tighter cap take effect uniformly across every
    /// AM listing endpoint.
    #[must_use]
    pub const fn max_listing_top(&self) -> u32 {
        self.max_listing_top
    }

    /// PEP gate. Calls the platform-side `PolicyEnforcer`, returns
    /// the [`AccessScope`] the storage layer forwards through
    /// `toolkit_db`'s secure builders.
    ///
    /// Mirrors `TenantService::authorize`:
    ///
    /// * `OWNER_TENANT_ID = tenant_id` — the row's owning tenant.
    /// * `RESOURCE_ID = tenant_id` — the same id; `tenant_metadata`
    ///   has no policy-visible resource id beyond its tenant scope.
    /// * `TYPE_ID = type_id` (when supplied) — the chained
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
    /// `type_id` is `None` exclusively for the `list_metadata`
    /// outer gate (the operation-level decision: "is the caller
    /// allowed to *list* on this tenant at all?"). Per-row schema
    /// filtering on the listing page happens through a separate
    /// [`Self::caller_allows_schema_read`] pass.
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        tenant_id: Uuid,
        type_id: Option<&str>,
    ) -> Result<AccessScope, DomainError> {
        // Delegates to [`crate::domain::authz::authz_scope`] for the
        // uniform PEP-gate shape; the metadata-specific `TYPE_ID`
        // property is layered on via the `extend` closure so the
        // shared helper does not have to know about per-service
        // AccessRequest attributes.
        //
        // `&str` is passed verbatim to keep the per-row `list_metadata`
        // path from double-allocating the schema id.
        crate::domain::authz::authz_scope(
            &self.enforcer,
            ctx,
            &pep::METADATA,
            action,
            tenant_id,
            Some(tenant_id),
            |req| match type_id {
                Some(sid) => req.resource_property(pep::TYPE_ID, sid),
                None => req,
            },
        )
        .await
    }

    /// Per-row schema-scoped authorization helper used by
    /// [`Self::list_metadata`] to drop entries the caller is not
    /// permitted to read. Returns `true` iff the PDP allows
    /// `Metadata.read` on `(tenant_id, type_id)` under the same
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
        type_id: &str,
    ) -> Result<bool, DomainError> {
        match self
            .authorize(ctx, pep::actions::READ, tenant_id, Some(type_id))
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
    /// `toolkit_db::odata::paginate_odata`. Stable tiebreaker on
    /// `schema_uuid` keeps cursor re-reads deterministic.
    ///
    /// Each returned [`MetadataEntry`] carries the public chained
    /// `type_id` re-hydrated from the registry per FEATURE §2 step
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
    /// * [`DomainError::Internal`] — orphan row: a stored
    ///   `schema_uuid` no longer resolves to a chained id in the
    ///   types registry. This is a data-integrity signal (the
    ///   chained id was deleted from the registry while metadata
    ///   rows still reference it); the service logs a `warn!` and
    ///   surfaces `Internal` rather than leaking the bare UUID
    ///   through a public 404 envelope.
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

        // Reverse-hydrate the chained `type_id` for the page rows in
        // one batch call. The registry adapter resolves all uuids in a
        // single round-trip and the lookup below is a pure map read.
        // Rows whose `schema_uuid` is no longer registered are an
        // integrity-pipeline signal — operators see them on the
        // `am.metadata` warn line and the caller gets a loud
        // `Internal` (HTTP 500). We do NOT surface this through the
        // public 404 path: leaking the bare `schema_uuid` as a
        // `resource_name` exposes an AM-internal storage key (the
        // chain that the caller would recognise is unknown by
        // definition here), and silently skipping the row would mask
        // a data-integrity drift behind a partial page.
        let uuids: Vec<Uuid> = page.items.iter().map(|r| r.schema_uuid).collect();
        let id_by_uuid: HashMap<Uuid, GtsTypeId> =
            self.schema_registry.resolve_ids_by_uuid(&uuids).await?;

        // Per-row schema-scoped authorization. Per PRD §1848 ("list
        // responses omit entries the actor is not permitted to read")
        // and DESIGN §`cpt-cf-account-management-fr-tenant-metadata-permissions`
        // the listing surface MUST filter entries by per-schema
        // `Metadata.read` policy — otherwise a caller granted only
        // `branding` could enumerate `billing` rows just by listing.
        // Outer `LIST` already passed above; this loop is the
        // per-row `READ` gate on the hydrated rows. `CrossTenantDenied`
        // for a row is silently dropped (the documented "omit"
        // semantic); any other PDP failure propagates so transport /
        // policy-load errors fail closed.
        let mut items: Vec<MetadataEntry> = Vec::with_capacity(page.items.len());
        for row in page.items {
            let Some(gts_type_id) = id_by_uuid.get(&row.schema_uuid).cloned() else {
                // Orphan row: `schema_uuid` is in `tenant_metadata`
                // but its chained id is gone from the types registry.
                // Loud `warn!` so the integrity-check pipeline can
                // correlate, and `Internal` so the client sees an
                // explicit "AM is broken" signal rather than a
                // partial page.
                tracing::warn!(
                    target: "am.metadata",
                    tenant_id = %tenant_id,
                    schema_uuid = %row.schema_uuid,
                    "metadata list: orphan row references a schema_uuid not present in the \
                     types registry; surfacing Internal so the data-integrity drift is \
                     observable"
                );
                return Err(DomainError::Internal {
                    diagnostic: format!(
                        "metadata list: orphan row for tenant {tenant_id} references \
                         schema_uuid {} which the types registry no longer recognises",
                        row.schema_uuid
                    ),
                    cause: None,
                });
            };
            if !self
                .caller_allows_schema_read(ctx, tenant_id, gts_type_id.as_ref())
                .await?
            {
                continue;
            }
            items.push(project_to_entry(row, gts_type_id));
        }

        // Per-row deny filtering can return items.len() <
        // page_info.limit — pagination is still correct because the
        // cursor advances over the unfiltered set. Consumers MUST use
        // page_info.next_cursor (not items.len()) as the termination
        // signal.
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

    /// Single-entry read keyed by `(tenant_id, type_id)`.
    ///
    /// Implements `cpt-cf-account-management-flow-tenant-metadata-get`.
    ///
    /// Both "schema unknown to the registry" and "schema known but no
    /// row at `(tenant_id, schema_uuid)`" surface as the same
    /// [`DomainError::MetadataEntryNotFound`] — AM does not
    /// distinguish them on the wire. The canonical envelope carries
    /// `resource_type = gts.cf.core.am.tenant_metadata.v1~` and
    /// `resource_name` = the chained `type_id` the caller supplied.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] — tenant is `Provisioning` or
    ///   `Deleted` (Active + Suspended pass per FEATURE spec).
    /// * [`DomainError::MetadataEntryNotFound`] — see above.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-get:p1:inst-flow-mdget-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-get-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-get-service
    #[tracing::instrument(skip(self), fields(tenant_id = %tenant_id, type_id = %type_id))]
    pub async fn get_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        type_id: GtsTypeId,
    ) -> Result<MetadataEntry, DomainError> {
        let parsed = ParsedTypeId::parse(type_id.as_ref())?;
        let scope = self
            .authorize(ctx, pep::actions::READ, tenant_id, Some(parsed.as_str()))
            .await?;

        // Tenant existence + status guard runs BEFORE the registry call.
        let _tenant = self.resolve_visible_tenant(&scope, tenant_id).await?;

        // Existence gate: registry resolves the policy AND signals
        // unregistered. We don't need the policy on the GET path but
        // the same RPC is the cheapest existence check (one round-trip
        // serves both reads and writes). The error variant carries
        // `type_id` verbatim so the canonical envelope can surface
        // the requested id without re-parsing the path.
        let _policy = self
            .schema_registry
            .resolve_inheritance_policy(parsed.as_gts())
            .await?;

        // UUIDv5 derivation cached on `ParsedTypeId` (matches the
        // upstream `gts::GtsId::to_uuid()` namespace per
        // `dod-tenant-metadata-schema-registration-and-uuid-derivation`).
        let schema_uuid = parsed.uuid();

        let row = self
            .metadata_repo
            .get_for_tenant(&scope, tenant_id, schema_uuid)
            .await?
            .ok_or_else(|| DomainError::MetadataEntryNotFound {
                detail: format!("no metadata entry for tenant {tenant_id} at schema {type_id}"),
                entry: type_id.to_string(),
            })?;

        Ok(project_to_entry(row, type_id))
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-get-service
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
    /// Guard ordering — pure → PEP → topology → schema → write:
    /// 1. `ParsedTypeId::parse` — pure input validation.
    /// 2. `value.is_null()` — pure input validation.
    /// 3. `authorize` — PEP gate carrying the `TYPE_ID` attribute.
    /// 4. `resolve_visible_tenant` — tenant topology gate.
    /// 5. `schema_registry.resolve_inheritance_policy` — schema-existence gate.
    /// 6. `schema_registry.validate_value` — body validation before any DB write.
    /// 7. `metadata_repo.upsert_for_tenant` — the actual write.
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
    /// * [`DomainError::MetadataEntryNotFound`] — schema not
    ///   in the registry (unified metadata 404); no row written.
    /// * [`DomainError::ServiceUnavailable`] — types-registry transport
    ///   failure; no row written.
    /// * [`DomainError::Internal`] — registered schema is not a valid
    ///   JSON Schema (catalog drift); no row written.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-put:p1:inst-flow-mdput-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-put-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-put-service
    #[tracing::instrument(
        skip(self, input),
        fields(tenant_id = %tenant_id, type_id = %input.type_id, actor_uuid = %ctx.subject_id())
    )]
    pub async fn upsert_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        input: UpsertMetadataRequest,
    ) -> Result<MetadataEntry, DomainError> {
        let UpsertMetadataRequest {
            type_id,
            value,
            expected_version,
            ..
        } = input;
        let parsed = ParsedTypeId::parse(type_id.as_ref())?;

        // Service-side null gate — SDK accepts plain JSON without
        // validation, so the gate runs here and surfaces as
        // `DomainError::Validation` (HTTP 400) at the canonical
        // boundary.
        if value.is_null() {
            return Err(DomainError::MetadataValidation {
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
        // an unregistered-schema PUT still surfaces 404, not 400.
        // The registry's local-client cache amortizes the second
        // round-trip in the steady state.
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
        let entry = project_to_entry(row, type_id.clone());

        // Audit line preserves the insert-vs-update split
        // (outcome=created/updated) that the SDK return type
        // intentionally hides.
        tracing::info!(
            target: "am.events",
            event = "metadata_upserted",
            tenant_id = %tenant_id,
            type_id = %type_id,
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
    /// Idempotent on missing rows: returns `Ok(())` whether the row
    /// existed and was removed or was already absent (mirrors the
    /// `delete_user` deprovision idempotency contract). The
    /// tenant-existence and schema-registration gates still run upstream
    /// — those `NotFound` paths surface their own 404 codes.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] — tenant is `Provisioning` or
    ///   `Deleted` (Active + Suspended pass per FEATURE spec).
    /// * [`DomainError::MetadataEntryNotFound`] — schema not
    ///   in the registry (unified metadata 404); no DB write issued.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-delete:p1:inst-flow-mddel-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-crud-contract:p1:inst-dod-crud-delete-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-delete-service
    #[tracing::instrument(
        skip(self),
        fields(tenant_id = %tenant_id, type_id = %type_id, actor_uuid = %ctx.subject_id())
    )]
    pub async fn delete_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        type_id: GtsTypeId,
    ) -> Result<(), DomainError> {
        let parsed = ParsedTypeId::parse(type_id.as_ref())?;
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

        self.metadata_repo
            .delete_for_tenant(&scope, tenant_id, schema_uuid)
            .await?;

        tracing::info!(
            target: "am.events",
            event = "metadata_deleted",
            tenant_id = %tenant_id,
            type_id = %type_id,
            actor_uuid = %actor,
            outcome = "ok",
            "am tenant metadata deleted"
        );

        Ok(())
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation:p1:inst-dod-schema-registration-delete-service
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
    /// [`DomainError::MetadataEntryNotFound`]. The REST handler
    /// surfaces `Ok(None)` as HTTP 200 with an empty response.
    ///
    /// # Errors
    ///
    /// * [`DomainError::NotFound`] — `tenant_id` does not resolve.
    /// * [`DomainError::Validation`] — tenant is `Provisioning` or
    ///   `Deleted` (Active + Suspended pass per FEATURE spec).
    /// * [`DomainError::MetadataEntryNotFound`] — schema not in the
    ///   registry (unified metadata 404); no walk performed.
    // @cpt-begin:cpt-cf-account-management-flow-tenant-metadata-resolve:p1:inst-flow-mdres-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-inheritance-resolution-contract:p1:inst-dod-inheritance-resolve-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-metadata-application-only-enforcement:p1:inst-dod-app-only-resolve-service
    #[tracing::instrument(skip(self), fields(tenant_id = %tenant_id, type_id = %type_id))]
    pub async fn resolve_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        type_id: GtsTypeId,
    ) -> Result<Option<MetadataEntry>, DomainError> {
        let parsed = ParsedTypeId::parse(type_id.as_ref())?;
        // Reuse the READ action: the resolved value is the caller's
        // effective config for their schema slot, and the inheritance
        // walk is bounded by `self_managed` barriers and the schema's
        // policy — see the `pep::actions` gear docs for why the
        // `/resolved` flow does not carry a separate verb.
        let scope = self
            .authorize(ctx, pep::actions::READ, tenant_id, Some(parsed.as_str()))
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

        Ok(row.map(|r| project_to_entry(r, type_id)))
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
    ///    suspended ancestor ⇒ skip-traverse (or stop if it is the
    ///    self-managed domain root); otherwise read and return on hit.
    ///    A `self_managed` ancestor is the inclusive top of the
    ///    inheritance domain — its value is read like any other, then
    ///    the walk stops *after* it (never crossing to its parent),
    ///    matching the `tenant_closure` barrier rule (VHP-1672).
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

        // PEP authorised the caller on start_tenant. Ancestor walk is
        // structural — results are projected through the start
        // tenant's visibility, never disclosed directly. Reusing the
        // narrowed InTenantSubtree scope would clamp to descendants of
        // start_tenant, turning every ancestor read into a miss.
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

            // Step 9 — barrier classification (`inst-algo-walk-ancestor-barrier`).
            // A `self_managed` ancestor is the INCLUSIVE top of the
            // descendant's inheritance domain — its own value IS
            // inheritable; only crossing ABOVE it is blocked. This
            // mirrors the authoritative `tenant_closure` barrier rule:
            // `barrier(A, D) = 1` iff some tenant on `(A, D]` is
            // self_managed (ancestor excluded, descendant included —
            // DESIGN §3.2 / `closure.rs` invariant 3), so
            // `barrier(self_managed, direct_descendant) = 0` and the
            // self_managed tenant sits inside its descendants' domain.
            // VHP-1672: this previously returned empty BEFORE the read,
            // over-blocking by one level vs the closure/scope barrier
            // and leaving tenants inside a self-managed org unable to
            // inherit defaults from their own domain root.
            let is_domain_root = ancestor.self_managed;

            // Step 10 — suspended ancestor: never consult its value —
            // suspension is a lifecycle state, not a barrier
            // (`inst-algo-walk-suspended-continue`). If this ancestor is
            // also the self_managed domain root the walk still stops
            // here (the barrier above it blocks crossing regardless of
            // status); otherwise traverse on to its parent.
            if matches!(ancestor.status, TenantStatus::Suspended) {
                if is_domain_root {
                    return Ok(None);
                }
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

            // Step 13 — self_managed domain root reached with no value:
            // stop *after* it per the barrier rule
            // (`inst-algo-walk-ancestor-barrier-return`) — do not cross
            // to its parent. A non-barrier ancestor with no value loops
            // on to its parent (`inst-algo-walk-loop`).
            if is_domain_root {
                return Ok(None);
            }
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
    /// Forward the PEP scope through `tenants.find_by_id` so an
    /// overly-permissive PDP still fails closed at the DB clamp
    /// (`InTenantSubtree` compiles against `tenants.id`).
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

/// Project a [`MetadataRow`] + its public chained `type_id` into
/// the [`MetadataEntry`] surface returned by every read-flow.
fn project_to_entry(row: MetadataRow, type_id: GtsTypeId) -> MetadataEntry {
    MetadataEntry::new(type_id, row.value, row.updated_at, row.version)
}
