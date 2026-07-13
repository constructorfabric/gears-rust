//! Internal domain models for the Types Registry gear.
//!
//! These types are NOT part of the public SDK surface — external gears
//! consume the typed [`GtsTypeSchema`](types_registry_sdk::GtsTypeSchema) /
//! [`GtsInstance`](types_registry_sdk::GtsInstance) views. The kind-agnostic
//! `GtsEntity` and `ListQuery` types are kept here as the contract between
//! the domain service and the storage layer (`GtsRepository`).

use gts::GtsIdSegment;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// A registered GTS entity, kind-agnostic.
///
/// Used internally between the storage layer and the domain service. The
/// service maps it to typed [`GtsTypeSchema`](types_registry_sdk::GtsTypeSchema)
/// or [`GtsInstance`](types_registry_sdk::GtsInstance) before exposing values
/// to external callers.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct GtsEntity<C = serde_json::Value> {
    /// Deterministic UUID v5 derived from the GTS ID.
    pub uuid: Uuid,
    /// The full GTS identifier string.
    pub gts_id: String,
    /// All parsed segments from the GTS ID.
    pub segments: Vec<GtsIdSegment>,
    /// Whether this entity is a type-schema (GTS ID ends with `~`).
    pub is_type_schema: bool,
    /// The entity content (schema body for type-schemas, object for instances).
    pub content: C,
    /// Optional description.
    pub description: Option<String>,
}

/// Type alias for dynamic GTS entities using `serde_json::Value` as content.
pub type DynGtsEntity = GtsEntity<serde_json::Value>;

impl<C> GtsEntity<C> {
    /// Creates a new `GtsEntity` with the given components.
    #[must_use]
    pub fn new(
        uuid: Uuid,
        gts_id: impl Into<String>,
        segments: Vec<GtsIdSegment>,
        is_type_schema: bool,
        content: C,
        description: Option<String>,
    ) -> Self {
        Self {
            uuid,
            gts_id: gts_id.into(),
            segments,
            is_type_schema,
            content,
            description,
        }
    }

    /// Returns `true` if this entity is a type-schema definition.
    #[must_use]
    pub const fn is_type(&self) -> bool {
        self.is_type_schema
    }

    /// Returns `true` if this entity is an instance.
    #[must_use]
    pub const fn is_instance(&self) -> bool {
        !self.is_type_schema
    }

    /// Returns the primary segment (first segment in the chain).
    #[must_use]
    pub fn primary_segment(&self) -> Option<&GtsIdSegment> {
        self.segments.first()
    }

    /// Returns the vendor from the primary segment.
    #[must_use]
    pub fn vendor(&self) -> Option<&str> {
        self.primary_segment().map(GtsIdSegment::vendor)
    }

    /// Returns the package from the primary segment.
    #[must_use]
    pub fn package(&self) -> Option<&str> {
        self.primary_segment().map(GtsIdSegment::package)
    }

    /// Returns the namespace from the primary segment.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.primary_segment().map(GtsIdSegment::namespace)
    }
}

/// Controls which segments of a chained GTS id the `vendor`, `package`, and
/// `namespace` filters in [`ListQuery`] are matched against.
#[domain_model]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SegmentMatchScope {
    /// Match filters against only the primary (first) GTS id segment.
    Primary,
    /// Match filters against any segment in the GTS id chain.
    #[default]
    Any,
}

/// Query parameters for listing GTS entities (kind-agnostic).
///
/// Internal to the parent gear. SDK callers use [`TypeSchemaQuery`] /
/// [`InstanceQuery`] (pattern-only) and the service translates those to this
/// struct. The REST handler builds the full struct directly so the wire
/// contract retains the per-segment `vendor` / `package` / `namespace`
/// filters.
///
/// [`TypeSchemaQuery`]: types_registry_sdk::TypeSchemaQuery
/// [`InstanceQuery`]: types_registry_sdk::InstanceQuery
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQuery {
    /// Optional wildcard pattern for GTS ID matching. Supports `*`.
    pub pattern: Option<String>,
    /// Filter for entity kind: `true` for schemas, `false` for instances.
    pub is_type: Option<bool>,
    /// Filter by vendor. Which segments this applies to is controlled by
    /// [`Self::segment_scope`].
    pub vendor: Option<String>,
    /// Filter by package. Which segments this applies to is controlled by
    /// [`Self::segment_scope`].
    pub package: Option<String>,
    /// Filter by namespace. Which segments this applies to is controlled by
    /// [`Self::segment_scope`].
    pub namespace: Option<String>,
    /// Controls which chain segments the `vendor` / `package` / `namespace`
    /// filters match against. Defaults to [`SegmentMatchScope::Any`].
    pub segment_scope: SegmentMatchScope,
}

impl ListQuery {
    /// Creates a new empty `ListQuery`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the pattern filter.
    #[must_use]
    pub fn with_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.pattern = Some(pattern.into());
        self
    }

    /// Sets the `is_type` filter.
    #[must_use]
    pub const fn with_is_type(mut self, is_type: bool) -> Self {
        self.is_type = Some(is_type);
        self
    }

    /// Sets the vendor filter.
    #[must_use]
    pub fn with_vendor(mut self, vendor: impl Into<String>) -> Self {
        self.vendor = Some(vendor.into());
        self
    }

    /// Sets the package filter.
    #[must_use]
    pub fn with_package(mut self, package: impl Into<String>) -> Self {
        self.package = Some(package.into());
        self
    }

    /// Sets the namespace filter.
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    /// Sets the segment match scope.
    #[must_use]
    pub const fn with_segment_scope(mut self, scope: SegmentMatchScope) -> Self {
        self.segment_scope = scope;
        self
    }

    /// Returns `true` if no filters are set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pattern.is_none()
            && self.is_type.is_none()
            && self.vendor.is_none()
            && self.package.is_none()
            && self.namespace.is_none()
    }

    /// Builds an internal `ListQuery` from an SDK `TypeSchemaQuery` (kind = schema).
    #[must_use]
    pub fn from_type_schema_query(q: types_registry_sdk::TypeSchemaQuery) -> Self {
        Self {
            pattern: q.pattern,
            is_type: Some(true),
            ..Self::default()
        }
    }

    /// Builds an internal `ListQuery` from an SDK `InstanceQuery` (kind = instance).
    #[must_use]
    pub fn from_instance_query(q: types_registry_sdk::InstanceQuery) -> Self {
        Self {
            pattern: q.pattern,
            is_type: Some(false),
            ..Self::default()
        }
    }
}
