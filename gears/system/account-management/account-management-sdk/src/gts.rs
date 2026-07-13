//! GTS resource type identifiers for Account Management.
//!
//! Single source of truth for the AM resource-type strings used in:
//!
//! * PEP `ResourceType.name` for authorization decisions (consumed by
//!   `service::pep::TENANT` and friends in the impl crate).
//! * `resource_type` field on [`crate::AccountManagementError`]
//!   variants (`NotFound`, `FailedPrecondition`) and on the canonical
//!   envelope they lift to at the REST boundary.
//! * Future cross-gear event consumers and sibling gears that
//!   pattern-match on AM-emitted events (event-bus contract TBD) —
//!   depending on this SDK instead of the impl crate keeps consumer
//!   build graphs slim.
//!
//! Strings follow the AM-specific GTS namespace convention from
//! `gears/system/account-management/docs/DESIGN.md` (PEP table).
//!
//! Mirrors the `gts` gear layout used by `resource-group-sdk` —
//! see `account_management_sdk::lib` rationale for the SDK split.
//!
//! # Note on `#[resource_error]` macro arguments
//!
//! Use the marker form `#[resource_error(gts_id!("..."))]` in impl
//! crates so IDs flow through the same toolkit GTS macro path.

/// AM Tenant resource. Used for PEP authorization on the `tenants`
/// table and as the `resource_type` field on tenant-scoped canonical
/// errors (e.g. `tenant {id} not found` → 404).
pub const TENANT_RESOURCE_TYPE: &str = gts_id!("cf.core.am.tenant.v1~");

/// AM `TenantMetadata` resource. Used for every canonical error
/// raised by the metadata surface — both "schema not registered" and
/// "entry not found" collapse to this single `resource_type` per the
/// unified-404 contract; `resource_name` carries the chained
/// `type_id` string the caller supplied so consumers can still see
/// **which** schema was involved without needing a separate
/// `resource_type` discriminator.
pub const TENANT_METADATA_RESOURCE_TYPE: &str = gts_id!("cf.core.am.tenant_metadata.v1~");

/// AM `ConversionRequest` resource. Used for canonical errors raised
/// by the conversion-request feature and for the future PEP gate on
/// conversion read / approve / reject endpoints.
pub const CONVERSION_REQUEST_RESOURCE_TYPE: &str = gts_id!("cf.core.am.conversion_request.v1~");

/// AM `IdpUser` resource projection. Mirror of the
/// `gts.cf.core.am.user.v1~` JSON Schema declared in
/// `gears/system/account-management/docs/schemas/user.v1.schema.json`
/// and produced by [`crate::IdpUser`]. Surfaces as the `resource_type`
/// on user-scoped canonical errors raised by the user-operations
/// feature (`feature-idp-user-operations-contract`).
pub const USER_RESOURCE_TYPE: &str = gts_id!("cf.core.am.user.v1~");

// ---------------------------------------------------------------------------
// IdpUser-groups feature -- two flavours of identifiers
// ---------------------------------------------------------------------------
//
// Two related identifier forms:
// - AM resource-type names (for PEP / canonical envelopes).
// - RG-prefixed type codes (required by RG's `validate_type_code`).
//
// Both are exported as crate constants so sibling crates import them by
// name instead of hard-coding strings.

/// RG type-registry code for the AM user-group **container** type.
///
/// Used by:
///
/// * `ResourceGroupClient::list_groups($filter=type eq <this>)` -- to
///   list user-groups (optionally combined with `tenant_id eq <t>`).
/// * `ResourceGroupClient::create_group({code: <this>, ...})` -- to
///   create a new user-group instance.
/// * AM's `register_user_group_types` at gear init.
///
/// The string lives in RG's type-registry namespace
/// (`gts.cf.core.rg.type.v1~` prefix) as required by RG's
/// `validate_type_code`.
pub const USER_GROUP_RG_TYPE_CODE: &str = gts_id!("cf.core.rg.type.v1~cf.core.am.user_group.v1~");

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
pub const USER_RG_TYPE_CODE: &str = gts_id!("cf.core.rg.type.v1~cf.core.am.user.v1~");

// ---------------------------------------------------------------------------
// AM base envelope schema registration
// ---------------------------------------------------------------------------
//
// Registered AM-owned base envelopes (boot-time, via the link-time
// `toolkit_gts` inventory the `#[gts_type_schema]` macro populates):
//
//   * `gts.cf.core.am.tenant_metadata.v1~` and
//     `gts.cf.core.am.tenant_type.v1~` -- the abstract trait-carrier
//     envelopes (below in this file) so vendor metadata / tenant_type
//     schemas deriving from them are admitted by `register_type_schemas`.
//     They use the gts-rust 0.10.0 `traits_schema` + `gts_abstract` macro
//     params to emit `x-gts-traits-schema` / `x-gts-abstract` from Rust
//     trait structs (no more hand-authored inline JSON).
//   * `gts.cf.core.am.user.v1~` -- registered via [`UserV1`] below.
//     `domain::gts_validation::validate_new_user_payload_via_gts` is
//     fail-closed and returns `ServiceUnavailable` if this envelope is
//     absent at boot.
//
// Registered for RBAC authorizability (PEP target types), id-only bodies:
//   * `tenant.v1~` -- registered via [`TenantV1`] below so RBAC can name it
//     in a role `target_type`. Body is `id`-only (empty for authorization
//     purposes), so `domain::gts_validation::validate_tenant_name_via_gts`
//     stays effectively fail-open (no name constraints in this schema; the
//     DB-level `CHECK (length(name) BETWEEN ...)` remains the backstop).
//   * `conversion_request.v1~` -- registered via [`ConversionRequestV1`] below.
//
// Not on the registration list (no vendor-derived extensions planned):
//   * `user_group.v1~` -- no consumer (the SDK constant was removed in
//     this PR); the JSON file may be deletable separately.

// ---------------------------------------------------------------------------
// AM resource type schema mirrors
// ---------------------------------------------------------------------------

use schemars::JsonSchema;
use toolkit::gts::PluginV1;
use toolkit_gts::{GtsTraitsSchema, gts_id, gts_type_schema};

/// GTS Type Schema mirror for the AM `IdpUser` resource type
/// (`gts.cf.core.am.user.v1~`).
///
/// This struct exists solely to register the AM user envelope into
/// the process-wide `toolkit_gts` inventory at compile time so the
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
/// GtsTypeId`) field; the real `IdpUser.id` is the IdP-issued UUID
/// per `cpt-cf-account-management-adr-idp-user-identity-source-of-truth`
/// and the AM-side validator does not inspect `id`, so the
/// generated-schema vs hand-authored docs divergence on the `id`
/// sub-schema has no functional impact.
///
/// Structural constraints (`minLength` / `maxLength` on the string
/// fields, `format: email`) are carried into the generated schema via
/// `#[schemars(...)]` field helpers below: `gts-macros` builds the
/// document from `schemars::schema_for!`, so the helpers ride straight
/// through (the derive is injected at struct level, so `legacy_derive_helpers`
/// never fires — see <https://github.com/GlobalTypeSystem/gts-rust/issues/86>).
/// `user_schema_constraints_tests` guards against regression.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.am.user.v1~"),
    description = "Account Management user resource — IdP-issued user identity projection",
    properties = "id,username,email,display_name,first_name,last_name",
    base = true
)]
pub struct UserV1 {
    /// Required by `gts-macros` base-struct contract; not the same as
    /// the IdP-issued UUID carried by [`crate::IdpUser`].
    pub id: gts::GtsInstanceId,
    /// Login identifier.
    #[schemars(length(min = 1, max = 255))]
    pub username: String,
    /// Optional contact email.
    #[schemars(extend("format" = "email"))]
    pub email: Option<String>,
    /// Optional display name.
    #[schemars(length(min = 1, max = 255))]
    pub display_name: Option<String>,
    /// Optional given name (Keycloak `firstName`, OIDC `given_name`).
    #[schemars(length(min = 1, max = 255))]
    pub first_name: Option<String>,
    /// Optional family name (Keycloak `lastName`, OIDC `family_name`).
    #[schemars(length(min = 1, max = 255))]
    pub last_name: Option<String>,
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
/// `cf-gears-authn-resolver-sdk::gts` so plugin discovery is
/// uniform across the Gears plugin contracts (`IdpPluginClient`,
/// `AuthNResolverPluginClient`, `TenantResolverPluginClient`, …).
///
/// # Instance ID Format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~<vendor>.<package>.idp.plugin.v1~
/// ```
///
/// # Example
///
/// ```ignore
/// use account_management_sdk::IdpPluginSpecV1;
/// use toolkit::gts::PluginV1;
///
/// // Plugin generates its instance ID
/// let instance_id = IdpPluginSpecV1::gts_make_instance_id(
///     "cf.builtin.keycloak_idp.plugin.v1",
/// );
///
/// // Plugin builds the registration record
/// let instance = PluginV1::<IdpPluginSpecV1> {
///     id: instance_id.clone(),
///     vendor: "constructorfabric".to_owned(),
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
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.idp.plugin.v1~"),
    description = "IdP provider plugin specification",
    properties = "",
)]
pub struct IdpPluginSpecV1;

// ---------------------------------------------------------------------------
// PEP resource-type registrations (RBAC target_type authorizability)
// ---------------------------------------------------------------------------

/// GTS type-schema mirror for the AM **tenant** PEP resource
/// (`gts.cf.core.am.tenant.v1~`, enforced by `TenantService`'s `pep::TENANT`).
///
/// Registered so the RBAC role-definitions API can validate `target_type`
/// against the types-registry and the PDP can compile a tenant-scoped grant;
/// without it every tenant operation is denied (403) because no role can name
/// the type. The authorization tenant-scope binding comes from the impl
/// crate's `ResourceType` (`OWNER_TENANT_ID`), not from this schema body.
///
/// Declares `name` (beyond the contract-required `id`) because
/// [`account-management::domain::gts_validation::validate_tenant_name_via_gts`]
/// validates the tenant `name` against this schema's `properties.name` on
/// every `create_tenant` — it is fail-closed, so a missing `name` property
/// surfaces as Internal/500. Bounds mirror `docs/schemas/tenant.v1.schema.json`
/// (and the DB `CHECK (length(name) BETWEEN 1 AND 255)`); guarded by
/// `tenant_schema_constraints_tests`.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.am.tenant.v1~"),
    description = "Account Management tenant resource — RBAC/PEP target type",
    properties = "id,name",
    base = true
)]
pub struct TenantV1 {
    /// Required by the `gts-macros` base-struct contract; inert for
    /// authorization (RBAC only needs the type id known to the registry).
    pub id: gts::GtsInstanceId,
    /// Tenant display name. Validated by AM's fail-closed
    /// `validate_tenant_name_via_gts` on `create_tenant`.
    #[schemars(length(min = 1, max = 255))]
    pub name: String,
}

/// GTS type-schema mirror for the AM **conversion request** PEP resource
/// (`gts.cf.core.am.conversion_request.v1~`, enforced by `ConversionService`'s
/// `pep::CONVERSION`). See [`TenantV1`] for the RBAC-authorizability rationale.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.am.conversion_request.v1~"),
    description = "Account Management tenant conversion-request resource — RBAC/PEP target type",
    properties = "id",
    base = true
)]
pub struct ConversionRequestV1 {
    /// Required by the `gts-macros` base-struct contract; inert for authorization.
    pub id: gts::GtsInstanceId,
}

// ---------------------------------------------------------------------------
// tenant_type.v1~ envelope
// ---------------------------------------------------------------------------

// Behavioural traits for `gts.cf.core.am.tenant_type.v1~` — emitted as
// the envelope's `x-gts-traits-schema`. NOTE: a `///` doc comment here
// would surface as a `description` on the emitted `x-gts-traits-schema`
// object (which the documented contract omits), so this stays a plain
// `//` comment. Field `///` docs ARE wanted — they become the per-trait
// `description`; `#[serde(default)]` yields the `default` keyword and
// drops the trait from `required`.
// Field docs are verbatim copies of the documented `docs/schemas/` trait
// contract (the drift guard asserts byte-equality), so they are prose,
// not rustdoc — suppress the markdown-backtick lint.
// A single `allowed_parent_types` entry: the GTS type identifier of a
// tenant type permitted as parent. The `x-gts-ref` extension makes the
// gts store validate each value is a `tenant_type.v1~`-derived type id
// when a derived tenant-type schema is registered. The prefix also
// permits same-type nesting (a type listing its own id); `/$id`
// self-reference is a different, exact-match construct and is not what
// we want here.
// `transparent` so the schema is just the inner string + the extension
// (no wrapper title) and the value serialises as a bare string.
#[allow(dead_code)]
#[derive(serde::Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-ref" = gts_id!("cf.core.am.tenant_type.v1~")))]
pub struct TenantTypeRef(pub String);

#[allow(clippy::doc_markdown)]
#[derive(JsonSchema, serde::Serialize, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct TenantTypeTraits {
    /// GTS instance identifiers of tenant types allowed as parent. Empty array means the type is root-only or leaf-only. The root tenant type has allowed_parent_types: [] by convention.
    #[serde(default)]
    pub allowed_parent_types: Vec<TenantTypeRef>,
    /// Whether tenants of this type typically require dedicated IdP-side resources. Account Management still calls IdpPluginClient::provision_tenant and deprovision_tenant; provider implementations may use this trait to decide whether to create resources or complete as a no-op.
    #[serde(default)]
    pub idp_provisioning: bool,
}

/// Base tenant-type envelope (abstract). Carries no instance data — the
/// `id` field exists only to satisfy the `gts-macros` base-struct
/// contract (a base must declare an `id: GtsInstanceId` or `gts_type:
/// GtsTypeId`) and is inert (see gear docs).
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.am.tenant_type.v1~"),
    description = "Base tenant type schema for Account Management. Derived tenant type schemas resolve behavioral traits via x-gts-traits. Traits configure system behavior for processing tenants of each type - they are not part of the tenant instance data model.",
    properties = "id",
    traits_schema = inline(TenantTypeTraits),
    // Inline-only `x-gts-traits` defaults (not in docs/schemas). gts's
    // OP#13 `validate_entity_traits` rejects an `x-gts-traits-schema` with
    // no values in the chain — even for abstract types — so the abstract
    // base MUST carry defaults or types-registry ready-commit fails and the
    // server cannot boot. Mirrors the trait-schema `default`s; guarded by
    // `sync_tests::tenant_type_envelope_trait_contract_matches_docs`.
    traits = serde_json::json!({ "allowed_parent_types": [], "idp_provisioning": false }),
    gts_abstract = true
)]
pub struct TenantTypeEnvelopeV1 {
    /// Required by the `gts-macros` base-struct contract; envelopes carry
    /// no instance data.
    pub id: gts::GtsInstanceId,
}

// ---------------------------------------------------------------------------
// tenant_metadata.v1~ envelope
// ---------------------------------------------------------------------------

// How `MetadataService` resolves a metadata schema's values across the
// tenant hierarchy. Serialises snake_case to match the wire tokens read
// by `metadata_schema_registry`. NOTE: variant `///` docs would make
// schemars emit a `oneOf` of `const` subschemas instead of the flat
// `enum: ["override_only", "inherit"]` the documented contract uses, so
// the variants are left undocumented here.
// Variants exist for schema generation (`enum` values); only `OverrideOnly`
// is constructed in Rust (via `Default`).
#[allow(dead_code)]
#[derive(JsonSchema, serde::Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MetadataInheritancePolicy {
    #[default]
    OverrideOnly,
    Inherit,
}

// Behavioural traits for `gts.cf.core.am.tenant_metadata.v1~` — emitted
// as the envelope's `x-gts-traits-schema` (plain `//`; see
// `TenantTypeTraits`).
#[allow(clippy::doc_markdown)]
#[derive(JsonSchema, serde::Serialize, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct TenantMetadataTraits {
    /// How MetadataService resolves values across the tenant hierarchy for this schema, per MetadataInheritancePolicy. 'override_only' (MetadataInheritancePolicy::OverrideOnly) returns the tenant's own entry or empty; 'inherit' (MetadataInheritancePolicy::Inherit) walks ancestors via parent_id, includes the nearest self-managed ancestor, then stops.
    #[serde(default)]
    pub inheritance_policy: MetadataInheritancePolicy,
}

/// Base tenant-metadata envelope (abstract). See [`TenantTypeEnvelopeV1`]
/// for the inert-`id` rationale.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.am.tenant_metadata.v1~"),
    description = "Base tenant metadata schema for Account Management. Derived metadata schemas resolve behavioral traits via x-gts-traits. Traits configure how MetadataService treats entries of each schema - they are not part of the metadata payload.",
    properties = "id",
    traits_schema = inline(TenantMetadataTraits),
    // Inline-only `x-gts-traits` default — see `TenantTypeEnvelopeV1` for
    // the OP#13 rationale. Guarded by
    // `sync_tests::tenant_metadata_envelope_trait_contract_matches_docs`.
    traits = serde_json::json!({ "inheritance_policy": "override_only" }),
    gts_abstract = true
)]
pub struct TenantMetadataEnvelopeV1 {
    /// Required by the `gts-macros` base-struct contract; envelopes carry
    /// no instance data.
    pub id: gts::GtsInstanceId,
}

#[cfg(test)]
mod resource_type_tests {
    use toolkit_gts::all_inventory_type_schemas;

    /// Both PEP resource types AM enforces on but never published must now be
    /// in the link-time type-schema inventory, so RBAC can validate a role's
    /// `target_type` against them (without registration every AM op is 403).
    #[test]
    fn tenant_and_conversion_request_type_schemas_are_registered() {
        let blob = serde_json::to_string(
            &all_inventory_type_schemas().expect("collect inventory type schemas"),
        )
        .expect("serialize inventory type schemas");
        assert!(
            blob.contains(super::TENANT_RESOURCE_TYPE),
            "tenant.v1~ type schema not registered in inventory"
        );
        assert!(
            blob.contains(super::CONVERSION_REQUEST_RESOURCE_TYPE),
            "conversion_request.v1~ type schema not registered in inventory"
        );
    }

    /// The generated `TYPE_ID` const is the source-of-truth tie between the
    /// registered schema and the SDK resource-type literal.
    #[test]
    fn registered_type_ids_match_resource_type_consts() {
        use gts::GtsSchema;
        assert_eq!(super::TenantV1::TYPE_ID, super::TENANT_RESOURCE_TYPE);
        assert_eq!(
            super::ConversionRequestV1::TYPE_ID,
            super::CONVERSION_REQUEST_RESOURCE_TYPE
        );
    }
}

#[cfg(test)]
mod user_schema_constraints_tests {
    //! Drift guard: the macro-generated `UserV1` schema — the one that
    //! reaches the Types Registry at boot and that
    //! `account-management::domain::gts_validation::validate_new_user_payload_via_gts`
    //! enforces fail-closed — MUST carry the `minLength` / `maxLength` /
    //! `format` constraints documented in `docs/schemas/user.v1.schema.json`.
    //! Until <https://github.com/GlobalTypeSystem/gts-rust/issues/86> was
    //! understood these were silently dropped; schemars field helpers on
    //! `UserV1` now forward them verbatim through `schemars::schema_for!`.

    use serde_json::Value;

    use super::UserV1;

    fn user_schema() -> Value {
        serde_json::from_str(&UserV1::gts_schema_with_refs_as_string())
            .expect("UserV1 schema must be valid JSON")
    }

    /// Property subschema for `field`; panics with the full schema dump
    /// so a structural surprise (e.g. an unexpected `Option` wrapping) is
    /// immediately visible.
    fn property<'a>(schema: &'a Value, field: &str) -> &'a Value {
        schema
            .get("properties")
            .and_then(|p| p.get(field))
            .unwrap_or_else(|| {
                panic!(
                    "property `{field}` missing from UserV1 schema:\n{}",
                    serde_json::to_string_pretty(schema).unwrap()
                )
            })
    }

    /// Look up a constraint keyword on a property, descending into
    /// `anyOf` / `oneOf` / `allOf` branches so `Option<String>` fields
    /// (which schemars may model as a nullable union) are handled.
    fn constraint<'a>(prop: &'a Value, key: &str) -> Option<&'a Value> {
        if let Some(v) = prop.get(key) {
            return Some(v);
        }
        for branch in ["anyOf", "oneOf", "allOf"] {
            if let Some(arr) = prop.get(branch).and_then(Value::as_array) {
                for sub in arr {
                    if let Some(v) = sub.get(key) {
                        return Some(v);
                    }
                }
            }
        }
        None
    }

    #[test]
    fn username_carries_length_bounds() {
        let schema = user_schema();
        let username = property(&schema, "username");
        assert_eq!(
            constraint(username, "minLength").and_then(Value::as_u64),
            Some(1),
            "username minLength missing; schema:\n{}",
            serde_json::to_string_pretty(&schema).unwrap()
        );
        assert_eq!(
            constraint(username, "maxLength").and_then(Value::as_u64),
            Some(255),
            "username maxLength missing; schema:\n{}",
            serde_json::to_string_pretty(&schema).unwrap()
        );
    }

    #[test]
    fn email_carries_email_format() {
        let schema = user_schema();
        let email = property(&schema, "email");
        assert_eq!(
            constraint(email, "format").and_then(Value::as_str),
            Some("email"),
            "email `format: email` missing; schema:\n{}",
            serde_json::to_string_pretty(&schema).unwrap()
        );
    }

    #[test]
    fn optional_profile_fields_carry_length_bounds() {
        let schema = user_schema();
        for field in ["display_name", "first_name", "last_name"] {
            let prop = property(&schema, field);
            assert_eq!(
                constraint(prop, "minLength").and_then(Value::as_u64),
                Some(1),
                "{field} minLength missing; schema:\n{}",
                serde_json::to_string_pretty(&schema).unwrap()
            );
            assert_eq!(
                constraint(prop, "maxLength").and_then(Value::as_u64),
                Some(255),
                "{field} maxLength missing; schema:\n{}",
                serde_json::to_string_pretty(&schema).unwrap()
            );
        }
    }
}

#[cfg(test)]
mod tenant_schema_constraints_tests {
    //! Drift guard: the macro-generated `TenantV1` schema (registered as
    //! `gts.cf.core.am.tenant.v1~`) is the type
    //! `account-management::domain::gts_validation::validate_tenant_name_via_gts`
    //! validates the tenant `name` against. It MUST declare `name` with the
    //! `minLength`/`maxLength` bounds from `docs/schemas/tenant.v1.schema.json`.
    //! If `name` is absent, the fail-closed validator returns Internal/500 on
    //! every `create_tenant` ("schema does not declare property `name`").

    use serde_json::Value;

    use super::TenantV1;

    #[test]
    fn tenant_schema_declares_name_with_length_bounds() {
        let schema: Value = serde_json::from_str(&TenantV1::gts_schema_with_refs_as_string())
            .expect("TenantV1 schema must be valid JSON");
        let name = schema
            .get("properties")
            .and_then(|p| p.get("name"))
            .unwrap_or_else(|| {
                panic!(
                    "TenantV1 schema missing `name` property:\n{}",
                    serde_json::to_string_pretty(&schema).unwrap()
                )
            });
        assert_eq!(
            name.get("minLength").and_then(Value::as_u64),
            Some(1),
            "tenant name minLength missing; schema:\n{}",
            serde_json::to_string_pretty(&schema).unwrap()
        );
        assert_eq!(
            name.get("maxLength").and_then(Value::as_u64),
            Some(255),
            "tenant name maxLength missing; schema:\n{}",
            serde_json::to_string_pretty(&schema).unwrap()
        );
    }
}

#[cfg(test)]
mod sync_tests {
    //! Drift guard: the macro-generated envelope's **trait contract**
    //! (`x-gts-traits-schema` shape + the envelope's `x-gts-traits`
    //! defaults) MUST agree with the documented
    //! `docs/schemas/<name>.v1.schema.json`. The macro additionally emits
    //! a data-type top-level shape (`id`/`properties`/`required`/
    //! `additionalProperties`) that the docs omit by design and that is
    //! inert at runtime (see gear docs), so only the trait subtrees are
    //! compared.

    use std::fs;
    use std::path::PathBuf;

    use gts::GtsSchema;
    use serde_json::Value;

    use super::{TenantMetadataEnvelopeV1, TenantTypeEnvelopeV1};

    fn read_disk_schema(file_name: &str) -> Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("docs")
            .join("schemas")
            .join(file_name);
        let body = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        serde_json::from_str(&body)
            .unwrap_or_else(|err| panic!("parse {} as JSON: {err}", path.display()))
    }

    /// Assert the macro-generated `x-gts-traits-schema` equals the
    /// documented one, that the envelope is marked abstract, and that it
    /// declares the `x-gts-traits` **default values** required for gts's
    /// OP#13 entity-trait completeness check.
    ///
    /// `gts::store::validate_entity_traits` (run for every schema at
    /// types-registry ready-commit) rejects any chain that declares an
    /// `x-gts-traits-schema` without supplying matching `x-gts-traits`
    /// values — and, unlike `validate_schema_traits`, it does NOT exempt
    /// abstract types. So the abstract base MUST still carry default trait
    /// values or the whole types-registry post-init fails (server cannot
    /// boot). These defaults are inline-only — they are intentionally not
    /// part of `docs/schemas/*.json`, matching the pre-macro envelope
    /// registration — so they are asserted separately from the disk diff.
    fn assert_trait_contract(
        generated: &Value,
        disk: &Value,
        type_id: &str,
        expected_traits: &Value,
    ) {
        assert_eq!(
            generated.get("x-gts-traits-schema"),
            disk.get("x-gts-traits-schema"),
            "macro x-gts-traits-schema for {type_id} drifted from docs/schemas/ -- \
             re-sync the trait struct with the documented trait contract",
        );
        assert_eq!(
            generated.get("x-gts-abstract"),
            Some(&Value::Bool(true)),
            "envelope {type_id} must be x-gts-abstract: true (pure trait carrier)",
        );
        assert_eq!(
            generated.get("x-gts-abstract"),
            disk.get("x-gts-abstract"),
            "envelope {type_id} x-gts-abstract drifted from docs/schemas/",
        );
        assert_eq!(
            generated.get("x-gts-traits"),
            Some(expected_traits),
            "envelope {type_id} must declare x-gts-traits default values to satisfy gts \
             OP#13 (validate_entity_traits rejects an x-gts-traits-schema with no values \
             in the chain, even for abstract types). These defaults were dropped in the \
             gts-0.10.0 macro migration; restore them via the macro `traits = ...` param.",
        );
    }

    #[test]
    fn tenant_type_envelope_trait_contract_matches_docs() {
        let generated = TenantTypeEnvelopeV1::gts_schema_with_refs();
        assert_trait_contract(
            &generated,
            &read_disk_schema("tenant_type.v1.schema.json"),
            TenantTypeEnvelopeV1::TYPE_ID,
            &serde_json::json!({ "allowed_parent_types": [], "idp_provisioning": false }),
        );
        // `type_id` and the `allowed_parent_types` `x-gts-ref` are separate
        // attribute literals (Rust attributes can't reference a const), so
        // guard that they agree: the ref must equal this envelope's TYPE_ID
        // so derived chains validate against the correct base prefix.
        let xref = generated
            .pointer("/x-gts-traits-schema/properties/allowed_parent_types/items/x-gts-ref");
        assert_eq!(
            xref.and_then(Value::as_str),
            Some(TenantTypeEnvelopeV1::TYPE_ID),
            "allowed_parent_types x-gts-ref literal must match the envelope's type_id literal",
        );
    }

    #[test]
    fn tenant_metadata_envelope_trait_contract_matches_docs() {
        assert_trait_contract(
            &TenantMetadataEnvelopeV1::gts_schema_with_refs(),
            &read_disk_schema("tenant_metadata.v1.schema.json"),
            TenantMetadataEnvelopeV1::TYPE_ID,
            &serde_json::json!({ "inheritance_policy": "override_only" }),
        );
    }
}
