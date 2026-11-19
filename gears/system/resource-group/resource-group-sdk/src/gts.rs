// Created: 2026-04-16 by Constructor Tech
//! GTS schema definitions for the Resource Group type system.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use toolkit_gts::local::gts_type_schema;
use toolkit_gts::{GtsTraitsSchema, gts_id};

/// One `allowed_parent_types` entry: the GTS type identifier of an RG type.
///
/// `x-gts-type-ref` makes registry admission check every value is a type
/// derived from `gts.cf.core.rg.type.v1~` (a type may list its own id for
/// same-type nesting).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-type-ref" = gts_id!("cf.core.rg.type.v1~")))]
pub struct RgTypeRef(pub String);

/// One `allowed_membership_types` entry: the GTS type identifier of a member
/// resource type. Any registered GTS type; it need not be an RG type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent, extend("x-gts-type-ref" = "gts.*"))]
pub struct MembershipTypeRef(pub String);

/// Placement traits of an RG type (`x-gts-traits-schema` of
/// [`ResourceGroupTypeV1`]).
///
/// The registry materializes them as a derived type's `effective_traits`
/// (leaf values over the base defaults); resource-group reads that
/// materialization and deserializes it into this struct.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, GtsTraitsSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ResourceGroupTypeTraitsV1 {
    /// Whether groups of this type may be roots (no parent).
    #[serde(default)]
    pub can_be_root: bool,
    /// RG types a group of this type may be placed under.
    #[serde(default)]
    pub allowed_parent_types: Vec<RgTypeRef>,
    /// Resource types that may be members of a group of this type.
    #[serde(default)]
    pub allowed_membership_types: Vec<MembershipTypeRef>,
}

impl ResourceGroupTypeTraitsV1 {
    /// Starts a builder requiring all three placement policy choices.
    ///
    /// Setters may be called in any order. `false` and empty lists are valid
    /// explicit choices; registry admission validates the supplied identifiers.
    /// JSON-authored declarations retain their schema defaults.
    ///
    /// ```
    /// use cf_gears_resource_group_sdk::gts::ResourceGroupTypeTraitsV1;
    /// let traits = ResourceGroupTypeTraitsV1::builder()
    ///     .with_can_be_root(true)
    ///     .with_allowed_parent_types(Vec::<String>::new())
    ///     .with_allowed_membership_types(["gts.cf.core.am.user.v1~"])
    ///     .build();
    /// assert!(traits.can_be_root);
    /// ```
    ///
    /// Omitting any policy choice prevents building:
    ///
    /// ```compile_fail
    /// use cf_gears_resource_group_sdk::gts::ResourceGroupTypeTraitsV1;
    /// let _ = ResourceGroupTypeTraitsV1::builder()
    ///     .with_allowed_parent_types(Vec::<String>::new())
    ///     .with_allowed_membership_types(Vec::<String>::new())
    ///     .build();
    /// ```
    ///
    /// ```compile_fail
    /// use cf_gears_resource_group_sdk::gts::ResourceGroupTypeTraitsV1;
    /// let _ = ResourceGroupTypeTraitsV1::builder()
    ///     .with_can_be_root(false)
    ///     .with_allowed_membership_types(Vec::<String>::new())
    ///     .build();
    /// ```
    ///
    /// ```compile_fail
    /// use cf_gears_resource_group_sdk::gts::ResourceGroupTypeTraitsV1;
    /// let _ = ResourceGroupTypeTraitsV1::builder()
    ///     .with_can_be_root(false)
    ///     .with_allowed_parent_types(Vec::<String>::new())
    ///     .build();
    /// ```
    #[must_use]
    pub const fn builder() -> ResourceGroupTypeTraitsV1Builder {
        ResourceGroupTypeTraitsV1Builder {
            can_be_root: (),
            allowed_parent_types: (),
            allowed_membership_types: (),
        }
    }
}

/// Builds [`ResourceGroupTypeTraitsV1`] after all three policy setters are called.
///
/// Each type parameter holds its value or `()` while the choice is missing.
pub struct ResourceGroupTypeTraitsV1Builder<R = (), P = (), M = ()> {
    can_be_root: R,
    allowed_parent_types: P,
    allowed_membership_types: M,
}

impl<R, P, M> ResourceGroupTypeTraitsV1Builder<R, P, M> {
    /// Sets whether groups may be roots (required).
    #[must_use]
    pub fn with_can_be_root(
        self,
        can_be_root: bool,
    ) -> ResourceGroupTypeTraitsV1Builder<bool, P, M> {
        ResourceGroupTypeTraitsV1Builder {
            can_be_root,
            allowed_parent_types: self.allowed_parent_types,
            allowed_membership_types: self.allowed_membership_types,
        }
    }

    /// Sets allowed parent type IDs (required); an empty list allows no parents.
    #[must_use]
    pub fn with_allowed_parent_types(
        self,
        types: impl IntoIterator<Item = impl Into<String>>,
    ) -> ResourceGroupTypeTraitsV1Builder<R, Vec<RgTypeRef>, M> {
        ResourceGroupTypeTraitsV1Builder {
            can_be_root: self.can_be_root,
            allowed_parent_types: types.into_iter().map(|id| RgTypeRef(id.into())).collect(),
            allowed_membership_types: self.allowed_membership_types,
        }
    }

    /// Sets allowed member type IDs (required); an empty list allows no members.
    #[must_use]
    pub fn with_allowed_membership_types(
        self,
        types: impl IntoIterator<Item = impl Into<String>>,
    ) -> ResourceGroupTypeTraitsV1Builder<R, P, Vec<MembershipTypeRef>> {
        ResourceGroupTypeTraitsV1Builder {
            can_be_root: self.can_be_root,
            allowed_parent_types: self.allowed_parent_types,
            allowed_membership_types: types
                .into_iter()
                .map(|id| MembershipTypeRef(id.into()))
                .collect(),
        }
    }
}

impl ResourceGroupTypeTraitsV1Builder<bool, Vec<RgTypeRef>, Vec<MembershipTypeRef>> {
    /// Builds the traits after all three placement policy choices are supplied.
    #[must_use]
    pub fn build(self) -> ResourceGroupTypeTraitsV1 {
        ResourceGroupTypeTraitsV1 {
            can_be_root: self.can_be_root,
            allowed_parent_types: self.allowed_parent_types,
            allowed_membership_types: self.allowed_membership_types,
        }
    }
}

/// GTS base type for Resource Group types: `gts.cf.core.rg.type.v1~`.
///
/// The persisted types registry is the only catalogue of RG types. Owners
/// publish derived types (`gts.cf.core.rg.type.v1~<vendor>...~`) before
/// application `init`; resource-group publishes this base.
///
/// * Placement traits: [`ResourceGroupTypeTraitsV1`], defaults `false`/`[]`/`[]`.
/// * Group payload: the schema describes a group as resource-group validates
///   it, an object with an optional `metadata` object. A derived type
///   constrains `metadata` (as `properties.metadata`, or through the generic
///   parameter of a Rust-declared derived type).
///
/// "Is this type a tenant?" is **not** a trait — it is derived from the type
/// code: any type whose GTS chain starts with one of the configured tenant
/// type prefixes (default: [`TENANT_RG_TYPE_PATH`]) is a tenant type.
///
/// # Compatibility with the previously published `v1~`
///
/// The previous SDK published this id with the instance properties `id`,
/// `can_be_root`, `allowed_parent_types` and `allowed_membership_types`, all
/// required, and a persisted registry keeps that revision as the baseline of
/// `v1~`. Removing a property from a closed schema is not backward compatible,
/// so they stay declared (with their previous docs, which are part of the
/// emitted schema) but are no longer required and are never read: placement
/// comes only from the traits. `metadata` is an added optional property.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.rg.type.v1~"),
    description = "Resource Group base type: placement traits and the group metadata container",
    properties = "id,can_be_root,allowed_parent_types,allowed_membership_types,metadata",
    base = true,
    traits_schema = inline(ResourceGroupTypeTraitsV1),
    traits = ResourceGroupTypeTraitsV1::default()
)]
#[schemars(extend("required" = []))]
pub struct ResourceGroupTypeV1<M> {
    /// GTS type path (schema identifier).
    pub id: gts::GtsInstanceId,
    /// Whether groups of this type can be root nodes (no parent). Default `false`.
    pub can_be_root: bool,
    /// GTS type paths of allowed parent types.
    pub allowed_parent_types: Vec<String>,
    /// GTS type paths of allowed membership resource types.
    pub allowed_membership_types: Vec<String>,
    /// The group's type-specific metadata; constrained by derived types.
    pub metadata: M,
}

/// Canonical GTS resource type for a resource **group** as a resource.
///
/// Lands in `CanonicalError::{NotFound,AlreadyExists}.ctx.resource_type`
/// for every group-attributable error and backs the impl crate's
/// `#[resource_error("…")]` REST marker. The macro literal there cannot
/// reference this const (proc-macros can't resolve consts), so the
/// round-trip tests in [`crate::error`] assert the two stay equal.
///
/// Match it against the resource-scoped projection variants
/// ([`crate::ResourceGroupError::NotFound`] /
/// [`crate::ResourceGroupError::AlreadyExists`]).
// `rg` namespace, NOT the crate-name-derived `resource_group`: this
// string is the PEP-evaluated resource type for every group CRUD gate,
// and every deployed role grant is written against the documented
// `gts.cf.core.rg.*` family (siblings: `rg.group_membership.v1~`,
// `rg.type.v1~`). The gts-rust v0.11.0 migration mechanically swapped
// the literal for a crate-derived id and silently renamed the type —
// which 403'd every existing grant (tenant members could no longer
// create groups, AM's ownership probe on tenant delete failed closed).
pub const GROUP_RESOURCE_TYPE: &str = gts_id!("cf.core.rg.group.v1~");

/// Canonical GTS resource type for a resource-group membership link.
pub const GROUP_MEMBERSHIP_RESOURCE_TYPE: &str = gts_id!("cf.core.rg.group_membership.v1~");

/// GTS identifier of the RG base type (`gts.cf.core.rg.type.v1~`): the
/// prefix every RG type identifier starts with.
pub const TYPE_RESOURCE_TYPE: &str = gts_id!("cf.core.rg.type.v1~");

/// Default GTS type path for the tenant resource-group type.
///
/// Any RG type whose code **starts with** a configured tenant-type path is
/// considered a tenant type — creating a group of such a type starts a new
/// tenant scope (`tenant_id = group.id`). Non-tenant types inherit
/// `tenant_id` from their parent. There is no explicit `is_tenant` boolean on
/// the type record; the prefix is the single source of truth.
///
/// This constant is the default of the resource-group gear's
/// `tenant_type_paths` and the rg-tr-plugin's `tenant_types` config; a
/// deployment with its own vendor overrides both lists (e.g.
/// `gts.cf.core.rg.type.v1~acme.core._.tenant.v1~`).
///
/// The tenant RG type itself is seeded externally (via API/config) with
/// `can_be_root: true` so root tenants are valid placements.
pub const TENANT_RG_TYPE_PATH: &str = gts_id!("cf.core.rg.type.v1~cf.core._.tenant.v1~");

#[cfg(test)]
#[path = "gts_tests.rs"]
mod gts_tests;
