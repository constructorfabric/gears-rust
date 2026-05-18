//! GTS resource type identifiers for Account Management.
//!
//! Single source of truth for the AM resource-type strings used in:
//!
//! * PEP `ResourceType.name` for authorization decisions (consumed by
//!   `service::pep::TENANT` and friends in the impl crate).
//! * `resource_type` field on [`crate::AccountManagementError`]
//!   variants (`NotFound`, `FailedPrecondition`) and on the canonical
//!   envelope they lift to at the REST boundary.
//! * Future cross-module event consumers and sibling modules that
//!   pattern-match on AM-emitted events (event-bus contract TBD) —
//!   depending on this SDK instead of the impl crate keeps consumer
//!   build graphs slim.
//!
//! Strings follow the AM-specific GTS namespace convention from
//! `modules/system/account-management/docs/DESIGN.md` (PEP table):
//! `gts.cf.core.am.{resource}.v1~`. The trailing `~` is the GTS
//! terminator and is part of the identifier.
//!
//! Mirrors the `gts` module layout used by `resource-group-sdk` —
//! see `account_management_sdk::lib` rationale for the SDK split.
//!
//! # Note on `#[resource_error]` macro arguments
//!
//! The `modkit_canonical_errors::resource_error` proc-macro takes a
//! literal string at expansion time and cannot resolve constants —
//! the impl-crate sites that call the macro therefore duplicate
//! these literals. The `domain::error_tests` module asserts the
//! impl-crate strings match the constants below, so a divergence
//! trips at test time, not in production.

/// AM Tenant resource. Used for PEP authorization on the `tenants`
/// table and as the `resource_type` field on tenant-scoped canonical
/// errors (e.g. `tenant {id} not found` → 404).
pub const TENANT_RESOURCE_TYPE: &str = "gts.cf.core.am.tenant.v1~";

/// AM `TenantMetadata` resource. Used for canonical errors raised
/// by the metadata feature (e.g. `MetadataSchemaNotRegistered`,
/// `MetadataEntryNotFound`) and for the future PEP gate on
/// metadata reads / writes.
pub const TENANT_METADATA_RESOURCE_TYPE: &str = "gts.cf.core.am.tenant_metadata.v1~";

/// AM `ConversionRequest` resource. Used for canonical errors raised
/// by the conversion-request feature and for the future PEP gate on
/// conversion read / approve / reject endpoints.
pub const CONVERSION_REQUEST_RESOURCE_TYPE: &str = "gts.cf.core.am.conversion_request.v1~";

/// AM `IdpUser` resource projection. Mirror of the
/// `gts.cf.core.am.user.v1~` JSON Schema declared in
/// `modules/system/account-management/docs/schemas/user.v1.schema.json`
/// and produced by [`crate::IdpUser`]. Surfaces as the `resource_type`
/// on user-scoped canonical errors raised by the user-operations
/// feature (`feature-idp-user-operations-contract`).
pub const USER_RESOURCE_TYPE: &str = "gts.cf.core.am.user.v1~";

// ---------------------------------------------------------------------------
// IdpUser-groups feature -- two flavours of identifiers
// ---------------------------------------------------------------------------
//
// The user-groups feature delegates storage to the Resource Group module
// (see DECOMPOSITION §2.6: "consumers call `ResourceGroupClient` directly
// per the Delegation-to-RG principle"). That delegation produces TWO
// related-but-distinct strings per resource:
//
// * The **AM resource-type identifier** (`gts.cf.core.am.*`) -- the
//   semantic AM type used by PEP / canonical-error envelopes / future
//   event-bus consumers. Mirrors the pattern of `TENANT_RESOURCE_TYPE`
//   et al above.
//
// * The **RG-prefixed type code** (`gts.cf.core.rg.type.v1~cf.core.am.*`)
//   -- the form RG's `validate_type_code` requires for entries in
//   `gts_type` (the type-registry namespace). AM registers these codes
//   at module init via `register_user_group_types`; sibling modules
//   (RBAC, UI gateways, batch jobs) that interact with RG directly
//   need the RG-prefixed strings to filter `list_groups` /
//   `add_membership` / `remove_membership` calls.
//
// Sibling modules MUST import these constants instead of hard-coding
// the strings; the impl crate re-exports them so the AM-internal call
// sites stay aligned with the public SDK contract.
//
// TODO(#1930): drop `USER_RG_TYPE_CODE` / `USER_GROUP_RG_TYPE_CODE`
// once the RG-side fix in PR cyberfabric/cyberware-rust#1929 lands.
// That PR removes the `gts.cf.core.rg.type.v1~` prefix requirement
// from `allowed_membership_types` validation, so AM can stop wrapping
// external types and use plain `gts.cf.core.am.*` codes everywhere.
// See https://github.com/cyberfabric/cyberware-rust/issues/1930 for
// the AM-side cleanup checklist.

/// RG type-registry code for the AM user-group **container** type.
///
/// Used by:
///
/// * `ResourceGroupClient::list_groups($filter=type eq <this>)` -- to
///   list user-groups (optionally combined with `tenant_id eq <t>`).
/// * `ResourceGroupClient::create_group({code: <this>, ...})` -- to
///   create a new user-group instance.
/// * AM's `register_user_group_types` at module init.
///
/// The string lives in RG's type-registry namespace
/// (`gts.cf.core.rg.type.v1~` prefix) as required by RG's
/// `validate_type_code`.
pub const USER_GROUP_RG_TYPE_CODE: &str = "gts.cf.core.rg.type.v1~cf.core.am.user_group.v1~";

/// RG type-registry code for the AM user **member-handle** type.
///
/// Used by:
///
/// * `ResourceGroupClient::add_membership(group_id, <this>, user_uuid)`
///   -- to add an AM user as a member of a user-group.
/// * `ResourceGroupClient::remove_membership(group_id, <this>, user_uuid)`
///   -- to remove a user from a group.
/// * `ResourceGroupClient::list_memberships($filter=resource_type eq <this>)`
///   -- to enumerate user→group links (e.g. "what groups is user X in").
///
/// This is a type-registry-only entry; AM users themselves live in
/// AM's tables + `IdP`, never as RG groups. Wraps
/// [`USER_RESOURCE_TYPE`] in the RG type-registry namespace.
pub const USER_RG_TYPE_CODE: &str = "gts.cf.core.rg.type.v1~cf.core.am.user.v1~";

// ---------------------------------------------------------------------------
// AM base envelope schema registration -- TODO
// ---------------------------------------------------------------------------
//
// TODO(am-envelope-registration): the AM-owned base envelope JSON schemas
// in `docs/schemas/` are not yet all emitted into
// `all_inventory_type_schemas()` at boot. Two still need to land.
//
// Tracked: cyberfabric/cyberware-rust#1928. The clean Rust-struct path
// is blocked on upstream GlobalTypeSystem/gts-rust#85
// (`x-gts-traits-schema` macro support); a direct `inventory::submit!`
// + `include_str!` workaround is available today (see the issue).
//
// 1. `gts.cf.core.am.tenant_metadata.v1~`
//    (`docs/schemas/tenant_metadata.v1.schema.json`) -- vendor metadata
//    schemas extend this envelope as derived `allOf` children. Without it
//    in the registry, `register_type_schemas` rejects them as orphans
//    and the default `inheritance_policy = "override_only"` from
//    `x-gts-traits-schema` never reaches `effective_traits()`. See the
//    long-form rationale at
//    `account-management::infra::types_registry::metadata_schema_registry`.
//
// 2. `gts.cf.core.am.tenant_type.v1~`
//    (`docs/schemas/tenant_type.v1.schema.json`) -- product roadmap calls
//    for vendor-defined tenant types (derived from this base envelope).
//    Same orphan-rejection failure mode applies once the first vendor
//    tenant_type ships.
//
// Already registered (this PR):
//   * `gts.cf.core.am.user.v1~` -- via [`UserV1`] below. Required because
//     `domain::gts_validation::validate_new_user_payload_via_gts` is
//     fail-closed and returns `ServiceUnavailable` if the envelope is
//     absent at boot.
//
// Not on the registration list (no vendor-derived extensions planned):
//   * `tenant.v1~` -- consumed by
//     `domain::gts_validation::validate_tenant_name_via_gts`, but that
//     validator is fail-OPEN (short-circuits when the schema is absent
//     and defers to the DB-level `CHECK (length(name) BETWEEN ...)`
//     backstop). Schema registration is "nice to have" but not required
//     for create_tenant to function.
//   * `user_group.v1~` -- no consumer (the SDK constant was removed in
//     this PR); the JSON file may be deletable separately.

// ---------------------------------------------------------------------------
// AM resource type schema mirrors
// ---------------------------------------------------------------------------

use modkit::gts::PluginV1;
use modkit_gts::gts_type_schema;

/// GTS Type Schema mirror for the AM `IdpUser` resource type
/// (`gts.cf.core.am.user.v1~`).
///
/// This struct exists solely to register the AM user envelope into
/// the process-wide `modkit_gts` inventory at compile time so the
/// `types-registry` boot path picks it up via
/// `all_inventory_type_schemas()`.
///
/// The envelope is consumed by
/// [`account-management::domain::gts_validation::validate_new_user_payload_via_gts`]
/// on every `create_user`, which is **fail-closed**: without this
/// schema in the Types Registry the validator returns
/// `ServiceUnavailable` and `create_user` is unavailable until the
/// catalog is seeded.
///
/// Field shape mirrors `docs/schemas/user.v1.schema.json` for
/// `username` / `email` / `display_name` — the fields AM actually
/// validates on the create-user payload. The `id` field is typed
/// [`gts::GtsInstanceId`] only because `gts-macros` requires base
/// structs to declare an `id: GtsInstanceId` (or a `gts_type:
/// GtsSchemaId`) field; the real `IdpUser.id` is the IdP-issued UUID
/// per `cpt-cf-account-management-adr-idp-user-identity-source-of-truth`
/// and the AM-side validator does not inspect `id`, so the
/// generated-schema vs hand-authored docs divergence on the `id`
/// sub-schema has no functional impact.
///
/// TODO(am-user-schema-constraints): the schemars-generated schema
/// currently lacks the `minLength` / `maxLength` / `format: email`
/// constraints present in `docs/schemas/user.v1.schema.json`.
/// `validate_new_user_payload_via_gts` therefore accepts any
/// non-empty string for `username` and `display_name` and any string
/// for `email`. Adding `#[schemars(length(min = 1, max = 255))]` and
/// `#[schemars(email)]` is blocked by an attribute-helper / derive
/// ordering interaction with `#[gts_type_schema]` in gts-macros
/// 0.9.3 (the derive helpers must be visible before the wrapper
/// attribute macro expands, but adding `#[derive(JsonSchema)]`
/// explicitly conflicts with the auto-added derive from the macro).
/// Re-evaluate when gts-macros gains direct constraint-attribute
/// support or when the version bumps.
#[gts_type_schema(
    dir_path = "schemas",
    schema_id = "gts.cf.core.am.user.v1~",
    description = "Account Management user resource — IdP-issued user identity projection",
    properties = "id,username,email,display_name",
    base = true
)]
pub struct UserV1 {
    /// Required by `gts-macros` base-struct contract; not the same as
    /// the IdP-issued UUID carried by [`crate::IdpUser`].
    pub id: gts::GtsInstanceId,
    /// Login identifier.
    pub username: String,
    /// Optional contact email.
    pub email: Option<String>,
    /// Optional display name.
    pub display_name: Option<String>,
}

// ---------------------------------------------------------------------------
// IdP provider plugin spec
// ---------------------------------------------------------------------------

/// GTS type definition for `IdP` provider plugin instances.
///
/// Each `IdP` plugin registers an instance of this type with its
/// vendor-specific instance ID. AM resolves the active plugin
/// through `ClientHub` keyed by the schema id below per
/// `cpt-cf-account-management-adr-idp-contract-separation` (ADR-0001).
///
/// Mirrors the established `AuthNResolverPluginSpecV1` pattern from
/// `cyberware-authn-resolver-sdk::gts` so plugin discovery is
/// uniform across the Cyber Ware plugin contracts (`IdpPluginClient`,
/// `AuthNResolverPluginClient`, `TenantResolverPluginClient`, …).
///
/// # Instance ID Format
///
/// ```text
/// gts.cf.modkit.plugins.plugin.v1~<vendor>.<package>.idp.plugin.v1~
/// ```
///
/// # Example
///
/// ```ignore
/// use account_management_sdk::IdpPluginSpecV1;
/// use modkit::gts::PluginV1;
///
/// // Plugin generates its instance ID
/// let instance_id = IdpPluginSpecV1::gts_make_instance_id(
///     "cf.builtin.keycloak_idp.plugin.v1",
/// );
///
/// // Plugin builds the registration record
/// let instance = PluginV1::<IdpPluginSpecV1> {
///     id: instance_id.clone(),
///     vendor: "cyberfabric".to_owned(),
///     priority: 100,
///     properties: IdpPluginSpecV1,
/// };
///
/// // Register with types-registry
/// // registry.register(vec![serde_json::to_value(&instance)?]).await?;
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    schema_id = "gts.cf.modkit.plugins.plugin.v1~cf.core.idp.plugin.v1~",
    description = "IdP provider plugin specification",
    properties = "",
)]
pub struct IdpPluginSpecV1;
