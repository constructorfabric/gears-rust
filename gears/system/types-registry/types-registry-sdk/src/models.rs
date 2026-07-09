//! Public models for the `types-registry` gear.
//!
//! These are transport-agnostic data structures that define the contract
//! between the `types-registry` gear and its consumers.

use std::collections::BTreeMap;
use std::sync::Arc;

use gts::{GtsID, GtsIdSegment, GtsInstanceId};
use serde_json::{Map, Value};
use toolkit_canonical_errors::CanonicalError;

/// SDK-facing GTS type-schema identifier, re-exported from the `gts` crate.
///
/// Within the types-registry SDK, schemas are *type-schemas* and their
/// identifiers are *type ids*.
pub use gts::GtsTypeId;
use uuid::Uuid;

use crate::field;
use crate::gts::TypeResource;

/// Build the in-process `InvalidArgument` canonical error the client-side
/// `try_new` constructors emit for a malformed / kind-mismatched GTS id.
///
/// Mirrors the impl crate's `From<DomainError> for CanonicalError` mapping of
/// `InvalidGtsId` (field [`field::GTS_ID_FIELD`], reason
/// [`field::INVALID_GTS_ID`]) so the in-process and REST classifications match.
/// These constructors never cross a wire boundary (ADR 0005 "Non-Canonical
/// Methods"); emitting `CanonicalError` keeps the SDK on a single error type
/// end-to-end (it projects to
/// [`TypesRegistryError::Validation`](crate::TypesRegistryError::Validation)).
fn invalid_gts_id_error(message: impl Into<String>) -> CanonicalError {
    TypeResource::invalid_argument()
        .with_field_violation(field::GTS_ID_FIELD, message, field::INVALID_GTS_ID)
        .create()
}

/// Returns `true` if `s` is shaped like a type-schema GTS id (ends with `~`).
///
/// Type-schema ids and instance ids are lexically distinct in GTS: type-schema
/// ids end with `~`, instance ids do not. Centralizing the predicate here so
/// that callers don't sprinkle raw `ends_with('~')` checks across kind-aware
/// code (`local_client`, mocks, etc.). Pure string predicate — does not parse
/// or otherwise validate the id.
///
// TODO(#1752): drop this helper once `GtsTypeId::try_new` /
// `GtsInstanceId::try_new` land upstream in `gts-rust`. Callers should
// consume `&GtsTypeId` / `&GtsInstanceId` directly and the kind invariant
// becomes a type-system property instead of a runtime predicate.
#[must_use]
pub fn is_type_schema_id(s: &str) -> bool {
    s.ends_with('~')
}

/// A registered GTS type-schema (type definition).
///
/// In addition to the common fields, the schema-specific extensions
/// `x-gts-traits-schema` and `x-gts-traits` are extracted into top-level
/// fields, and the GTS chain parent is pre-resolved into [`Self::parent`]
/// (Arc-shared, deduplicated by the registry's local-client cache).
///
/// Use [`Self::effective_schema`], [`Self::effective_properties`],
/// [`Self::effective_required`], [`Self::effective_traits`] to inspect the schema
/// across the inheritance chain without manual walking.
///
/// `x-gts-final` / `x-gts-abstract` modifiers are intentionally not surfaced
/// here yet — support will be added later.
#[derive(Debug, Clone, PartialEq)]
pub struct GtsTypeSchema {
    /// Deterministic UUID v5 derived from the type id.
    pub type_uuid: Uuid,

    /// The full GTS type identifier. Always ends with `~`.
    pub type_id: GtsTypeId,

    /// All parsed segments from the GTS ID.
    pub segments: Vec<GtsIdSegment>,

    /// This type-schema's own raw JSON Schema body.
    ///
    /// `allOf[].$ref` references are kept verbatim — use [`Self::effective_schema`]
    /// to obtain a representation with the parent inlined.
    pub raw_schema: Value,

    /// This type-schema's own `x-gts-traits` values, if present.
    pub traits: Option<Value>,

    /// This type-schema's own `x-gts-traits-schema`, if present.
    pub traits_schema: Option<Value>,

    /// Resolved parent type-schema in the inheritance chain.
    ///
    /// `None` for root type-schemas (no parent in the chain) or when the parent
    /// hasn't been resolved by the producer.
    pub parent: Option<Arc<GtsTypeSchema>>,

    /// Optional human-readable title (`title` field of the JSON Schema).
    pub title: Option<String>,

    /// Optional human-readable description.
    pub description: Option<String>,
}

impl GtsTypeSchema {
    /// Constructs a `GtsTypeSchema` from its canonical inputs.
    ///
    /// `type_uuid` and `segments` are derived from `type_id` via gts-rust's
    /// canonical parser — there is only one source of truth (the id string).
    /// `traits` / `traits_schema` / `title` are extracted from `raw_schema`.
    /// `parent` is pre-resolved by the caller (typically the local client
    /// via its type-schema cache); presence/absence of `parent` is enforced
    /// against the chain shape of `type_id` — a derived id MUST carry its
    /// parent, a root id MUST NOT — so that
    /// [`ancestors`](Self::ancestors) / [`effective_schema`](Self::effective_schema)
    /// always observe a complete chain. A mismatched parent (chain-prefix
    /// disagreement) is also rejected.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidArgument` [`CanonicalError`] (reason
    /// [`field::INVALID_GTS_ID`]) in any of these cases:
    /// - `type_id` does not end with `~` (looks like an instance id);
    /// - `type_id` does not parse as a valid GTS identifier;
    /// - `parent` is `Some(_)` but its `type_id` does not match the chain
    ///   prefix derived from this `type_id`;
    /// - `parent` is `Some(_)` but this `type_id` is a root (no chain prefix
    ///   exists, so the schema cannot have a parent);
    /// - `parent` is `None` but this `type_id` is derived (its chain prefix
    ///   is non-empty, so the schema requires its parent to be passed in).
    pub fn try_new(
        type_id: GtsTypeId,
        raw_schema: Value,
        description: Option<String>,
        parent: Option<Arc<GtsTypeSchema>>,
    ) -> Result<Self, CanonicalError> {
        if !is_type_schema_id(type_id.as_ref()) {
            return Err(invalid_gts_id_error(format!(
                "{type_id} does not end with `~`",
            )));
        }
        match (
            parent.as_ref(),
            Self::derive_parent_type_id(type_id.as_ref()),
        ) {
            (Some(parent_schema), Some(expected)) if expected != parent_schema.type_id => {
                return Err(invalid_gts_id_error(format!(
                    "type-schema {type_id} expects parent {expected}, got {}",
                    parent_schema.type_id,
                )));
            }
            (Some(_), None) => {
                return Err(invalid_gts_id_error(format!(
                    "root type-schema {type_id} cannot have a parent",
                )));
            }
            (None, Some(expected)) => {
                return Err(invalid_gts_id_error(format!(
                    "derived type-schema {type_id} requires parent {expected}, got None",
                )));
            }
            // (Some, Some) where prefixes match  →  ok
            // (None, None) — root with no parent  →  ok
            _ => {}
        }
        let parsed =
            GtsID::new(type_id.as_ref()).map_err(|e| invalid_gts_id_error(format!("{e}")))?;
        let type_uuid = parsed.to_uuid();
        let segments = parsed.gts_id_segments;
        let traits = Self::extract_traits(&raw_schema);
        let traits_schema = Self::extract_traits_schema(&raw_schema);
        let title = Self::extract_title(&raw_schema);
        Ok(Self {
            type_uuid,
            type_id,
            segments,
            raw_schema,
            traits,
            traits_schema,
            parent,
            title,
            description,
        })
    }

    /// Derives the GTS parent's `type_id` by stripping the last `~`-segment.
    ///
    /// Mirrors gts-rust's chain semantics: for a chained type id like
    /// `gts.cf.core.events.type.v1~x.commerce.orders.order.v1.0~`, the parent
    /// is `gts.cf.core.events.type.v1~`. Returns `None` for root (single-segment)
    /// type-schemas or for ids that don't end with `~`.
    #[must_use]
    pub fn derive_parent_type_id(type_id: &str) -> Option<GtsTypeId> {
        let trimmed = type_id.strip_suffix('~')?;
        let last_tilde = trimmed.rfind('~')?;
        Some(GtsTypeId::new(&type_id[..=last_tilde]))
    }

    /// Reads `x-gts-traits` from the top level of a schema value.
    #[must_use]
    pub fn extract_traits(schema: &Value) -> Option<Value> {
        schema.get("x-gts-traits").cloned()
    }

    /// Reads `x-gts-traits-schema` from the top level of a schema value.
    #[must_use]
    pub fn extract_traits_schema(schema: &Value) -> Option<Value> {
        schema.get("x-gts-traits-schema").cloned()
    }

    /// Collects parent GTS IDs from `allOf[].$ref` (with `gts://` prefix stripped).
    #[must_use]
    pub fn extract_allof_refs(schema: &Value) -> Vec<String> {
        let Some(arr) = schema.get("allOf").and_then(|v| v.as_array()) else {
            return Vec::new();
        };
        arr.iter()
            .filter_map(|item| item.get("$ref").and_then(|r| r.as_str()))
            .map(|r| r.strip_prefix("gts://").unwrap_or(r).to_owned())
            .collect()
    }

    /// Reads the optional `title` field.
    #[must_use]
    pub fn extract_title(schema: &Value) -> Option<String> {
        schema
            .get("title")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
    }

    /// Returns the primary segment (first segment in the chain).
    #[must_use]
    pub fn primary_segment(&self) -> Option<&GtsIdSegment> {
        self.segments.first()
    }

    /// Returns the vendor from the primary segment.
    #[must_use]
    pub fn vendor(&self) -> Option<&str> {
        self.primary_segment().map(|s| s.vendor.as_str())
    }

    /// Iteration over the inheritance chain (this schema first, then parent,
    /// then grandparent, ...). Linear walk via [`Self::parent`].
    #[must_use]
    pub fn ancestors(&self) -> AncestorIter<'_> {
        AncestorIter {
            current: Some(self),
        }
    }

    /// Returns this schema's body with the GTS parent's `$ref` inlined where
    /// it appears in `allOf` (parent body expanded in place). The shape of
    /// the JSON Schema is preserved — `allOf`, `oneOf`, `anyOf`, `enum`, etc.
    /// stay valid.
    ///
    /// Non-parent `allOf[].$ref` items (mixin references) are left as-is.
    #[must_use]
    pub fn effective_schema(&self) -> Value {
        merge_schema_with_parent(&self.raw_schema, self.parent.as_deref())
    }

    /// Properties merged across the full chain. This schema wins on key
    /// collisions; parent fills in inherited keys.
    #[must_use]
    pub fn effective_properties(&self) -> BTreeMap<String, Value> {
        let mut out = self
            .parent
            .as_ref()
            .map_or_else(BTreeMap::new, |p| p.effective_properties());
        for (k, v) in collect_own_properties(&self.raw_schema) {
            out.insert(k, v);
        }
        out
    }

    /// `required` field merged across the full chain (de-duplicated, order
    /// preserved by first occurrence in pre-order walk).
    #[must_use]
    pub fn effective_required(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for ancestor in self.ancestors() {
            for r in collect_own_required(&ancestor.raw_schema) {
                if seen.insert(r.clone()) {
                    out.push(r);
                }
            }
        }
        out
    }

    /// Trait values merged across the chain.
    ///
    /// Resolution order (priority high → low):
    /// 1. Declared `x-gts-traits` values from `self` and ancestors —
    ///    rightmost wins, so a leaf's value overrides any parent's.
    /// 2. Defaults from `x-gts-traits-schema.properties[*].default`
    ///    declared anywhere in the chain. When two levels both declare a
    ///    default for the same property, the **deepest** (closest to base)
    ///    wins — mirroring gts-rust's locking rule that descendants cannot
    ///    redefine an ancestor's default during schema-trait validation.
    ///
    /// Returns `Value::Null` only when neither declared traits nor
    /// schema-declared defaults exist anywhere in the chain.
    // TODO(#1723): replace with gts-rust's resolve_schema(...).effective_traits
    // once that helper is exposed publicly.
    #[must_use]
    pub fn effective_traits(&self) -> Value {
        let mut acc: Map<String, Value> = Map::new();
        // Phase 1: declared traits. Walk own → ancestors and only insert
        // when the key is absent so own (rightmost) wins over ancestors.
        for s in self.ancestors() {
            if let Some(Value::Object(traits)) = s.traits.as_ref() {
                for (k, v) in traits {
                    acc.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        // Phase 2: defaults from x-gts-traits-schema. Walk from deepest
        // base to leaf so the **earliest** default wins on a given key,
        // matching the locking semantics gts-rust enforces during
        // validation. `or_insert_with` on the already-populated map means
        // declared values still beat defaults.
        let chain: Vec<&GtsTypeSchema> = self.ancestors().collect();
        for s in chain.iter().rev() {
            let Some(traits_schema) = s.traits_schema.as_ref() else {
                continue;
            };
            let Some(Value::Object(props)) = traits_schema.get("properties") else {
                continue;
            };
            for (k, prop) in props {
                if let Some(default) = prop.get("default") {
                    acc.entry(k.clone()).or_insert_with(|| default.clone());
                }
            }
        }
        if acc.is_empty() {
            Value::Null
        } else {
            Value::Object(acc)
        }
    }

    /// All `x-gts-traits-schema` blocks collected across the chain, ordered
    /// from deepest base to this schema. Use to compose the effective trait
    /// schema (e.g. via `allOf`) when validating trait values.
    #[must_use]
    pub fn effective_traits_schema(&self) -> Vec<Value> {
        // Pre-order is self → ancestors; reverse to get deepest-base-first.
        let mut out: Vec<Value> = self
            .ancestors()
            .filter_map(|s| s.traits_schema.clone())
            .collect();
        out.reverse();
        out
    }
}

/// Iterator over a type-schema's inheritance chain (self first, then each
/// ancestor by following [`GtsTypeSchema::parent`]).
pub struct AncestorIter<'a> {
    current: Option<&'a GtsTypeSchema>,
}

impl<'a> Iterator for AncestorIter<'a> {
    type Item = &'a GtsTypeSchema;

    fn next(&mut self) -> Option<Self::Item> {
        let curr = self.current.take()?;
        self.current = curr.parent.as_deref();
        Some(curr)
    }
}

fn collect_own_properties(schema: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    // Top-level properties.
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        for (k, v) in props {
            out.insert(k.clone(), v.clone());
        }
    }
    // Properties declared inside allOf branches that are NOT pure $refs
    // (the parent's body comes from `self.parent`). Inline overlays count as "own".
    if let Some(arr) = schema.get("allOf").and_then(|v| v.as_array()) {
        for item in arr {
            // Skip pure-$ref entries (resolved via `parent`).
            let is_pure_ref = item
                .as_object()
                .is_some_and(|m| m.len() == 1 && m.contains_key("$ref"));
            if is_pure_ref {
                continue;
            }
            if let Some(props) = item.get("properties").and_then(|v| v.as_object()) {
                for (k, v) in props {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
    }
    out
}

fn collect_own_required(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(req) = schema.get("required").and_then(|v| v.as_array()) {
        for r in req {
            if let Some(s) = r.as_str() {
                out.push(s.to_owned());
            }
        }
    }
    if let Some(arr) = schema.get("allOf").and_then(|v| v.as_array()) {
        for item in arr {
            let is_pure_ref = item
                .as_object()
                .is_some_and(|m| m.len() == 1 && m.contains_key("$ref"));
            if is_pure_ref {
                continue;
            }
            if let Some(req) = item.get("required").and_then(|v| v.as_array()) {
                for r in req {
                    if let Some(s) = r.as_str() {
                        out.push(s.to_owned());
                    }
                }
            }
        }
    }
    out
}

/// Returns `schema` with the entry `allOf[i] = {$ref: gts://parent.type_id}`
/// replaced by the merged body of the GTS parent. Other `allOf` entries
/// (non-ref overlays, mixin `$ref`s pointing elsewhere) are left as-is.
/// `$id` and `$schema` are stripped from the inlined parent to keep the
/// merged document a valid composite schema.
fn merge_schema_with_parent(schema: &Value, parent: Option<&GtsTypeSchema>) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };
    let Some(parent) = parent else {
        return Value::Object(map.clone());
    };
    let mut out = map.clone();

    if let Some(Value::Array(items)) = out.get_mut("allOf").cloned().as_ref() {
        let mut new_items = Vec::with_capacity(items.len());
        for item in items {
            let resolved = if let Some(obj) = item.as_object()
                && obj.len() == 1
                && let Some(ref_uri) = obj.get("$ref").and_then(|r| r.as_str())
                && {
                    let target = ref_uri.strip_prefix("gts://").unwrap_or(ref_uri);
                    parent.type_id == target
                } {
                let mut merged = parent.effective_schema();
                if let Value::Object(ref mut m) = merged {
                    m.remove("$id");
                    m.remove("$schema");
                }
                merged
            } else {
                item.clone()
            };
            new_items.push(resolved);
        }
        out.insert("allOf".to_owned(), Value::Array(new_items));
    }

    Value::Object(out)
}

/// A registered GTS instance.
///
/// The instance carries an `Arc`-shared reference to its [`GtsTypeSchema`],
/// pre-resolved by the registry's local client (with full ancestor chain
/// already linked). Inspect via `instance.type_schema.effective_*` directly.
#[derive(Debug, Clone, PartialEq)]
pub struct GtsInstance {
    /// Deterministic UUID v5 derived from the GTS ID.
    pub uuid: Uuid,

    /// The full GTS instance identifier. Never ends with `~`.
    pub id: GtsInstanceId,

    /// All parsed segments from the GTS ID.
    pub segments: Vec<GtsIdSegment>,

    /// The full instance object (raw `Value`).
    pub object: Value,

    /// Resolved type-schema this instance conforms to (Arc-shared with the
    /// registry's cache).
    pub type_schema: Arc<GtsTypeSchema>,

    /// Optional description of the entity.
    pub description: Option<String>,
}

impl GtsInstance {
    /// Constructs a `GtsInstance` from its canonical inputs plus a
    /// pre-resolved type-schema reference.
    ///
    /// `uuid` and `segments` are derived from `id` via gts-rust's canonical
    /// parser — there is only one source of truth (the id string). `id` must
    /// NOT end with `~` and must contain at least one `~`. The passed
    /// `type_schema.type_id` is verified to match the chain prefix derived
    /// from `id` (everything up to and including the last `~`) so a
    /// mismatched type-schema can't silently mislabel the instance.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidArgument` [`CanonicalError`] (reason
    /// [`field::INVALID_GTS_ID`]) in any of these cases:
    /// - `id` ends with `~` (looks like a type-schema id);
    /// - `id` contains no `~` at all (no type-schema chain prefix);
    /// - `id` does not parse as a valid GTS identifier;
    /// - `type_schema.type_id` does not match the chain prefix derived from `id`.
    pub fn try_new(
        id: GtsInstanceId,
        object: Value,
        description: Option<String>,
        type_schema: Arc<GtsTypeSchema>,
    ) -> Result<Self, CanonicalError> {
        if is_type_schema_id(id.as_ref()) {
            return Err(invalid_gts_id_error(format!(
                "{id} ends with `~` (looks like a type-schema id)",
            )));
        }
        let derived = Self::derive_type_id(id.as_ref()).ok_or_else(|| {
            invalid_gts_id_error(format!(
                "instance id {id} has no type-schema chain (no `~`)"
            ))
        })?;
        if derived != type_schema.type_id {
            return Err(invalid_gts_id_error(format!(
                "instance id {id} chain prefix {derived} does not match type-schema {0}",
                type_schema.type_id
            )));
        }
        let parsed = GtsID::new(id.as_ref()).map_err(|e| invalid_gts_id_error(format!("{e}")))?;
        let uuid = parsed.to_uuid();
        let segments = parsed.gts_id_segments;
        Ok(Self {
            uuid,
            id,
            segments,
            object,
            type_schema,
            description,
        })
    }

    /// `type_id` of the type-schema this instance conforms to. Always ends with `~`.
    #[must_use]
    pub fn type_id(&self) -> &GtsTypeId {
        &self.type_schema.type_id
    }

    /// Derives the type-schema (parent type) GTS ID from an instance `id`.
    ///
    /// Returns everything up to and including the last `~`. `None` when the
    /// `id` contains no `~`.
    #[must_use]
    pub fn derive_type_id(id: &str) -> Option<GtsTypeId> {
        id.rfind('~').map(|i| GtsTypeId::new(&id[..=i]))
    }

    /// Returns the primary segment (first segment in the chain).
    #[must_use]
    pub fn primary_segment(&self) -> Option<&GtsIdSegment> {
        self.segments.first()
    }

    /// Returns the vendor from the primary segment.
    #[must_use]
    pub fn vendor(&self) -> Option<&str> {
        self.primary_segment().map(|s| s.vendor.as_str())
    }
}

/// Result of registering a single GTS entity in a batch operation.
///
/// Successful registration carries only the canonical (server-normalized)
/// GTS id of the persisted entity. Callers that need a typed view of the
/// registered entity should follow up with [`TypesRegistryClient::get_type_schema`]
/// / [`TypesRegistryClient::get_instance`] — keeping registration's
/// responsibility narrow ("did it persist?") and reads' responsibility narrow
/// ("give me the resolved typed value").
#[derive(Debug, Clone)]
pub enum RegisterResult {
    /// Successfully registered.
    Ok {
        /// The canonical GTS id of the registered entity.
        gts_id: String,
    },
    /// Failed to register.
    Err {
        /// The GTS ID that was attempted, if it could be extracted from the input.
        gts_id: Option<String>,
        /// The error that occurred during registration. Project to
        /// [`TypesRegistryError`](crate::TypesRegistryError) for typed dispatch.
        error: CanonicalError,
    },
}

impl RegisterResult {
    /// Returns `true` if the registration was successful.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// Returns `true` if the registration failed.
    #[must_use]
    pub const fn is_err(&self) -> bool {
        matches!(self, Self::Err { .. })
    }

    /// Converts to `Result<&str, &CanonicalError>` — the success arm
    /// borrows the canonical `gts_id`.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a reference to the error if this is a failed registration.
    pub fn as_result(&self) -> Result<&str, &CanonicalError> {
        match self {
            Self::Ok { gts_id } => Ok(gts_id),
            Self::Err { error, .. } => Err(error),
        }
    }

    /// Converts into `Result<String, CanonicalError>` — the success arm
    /// owns the canonical `gts_id`.
    ///
    /// # Errors
    ///
    /// Returns `Err` with the error if this is a failed registration.
    pub fn into_result(self) -> Result<String, CanonicalError> {
        match self {
            Self::Ok { gts_id } => Ok(gts_id),
            Self::Err { error, .. } => Err(error),
        }
    }

    /// Returns the registered `gts_id` if successful, `None` otherwise.
    #[must_use]
    pub fn ok(self) -> Option<String> {
        match self {
            Self::Ok { gts_id } => Some(gts_id),
            Self::Err { .. } => None,
        }
    }

    /// Returns the error if failed, `None` otherwise.
    #[must_use]
    pub fn err(self) -> Option<CanonicalError> {
        match self {
            Self::Ok { .. } => None,
            Self::Err { error, .. } => Some(error),
        }
    }

    /// Returns `Ok(())` if all results are successful, or the first error.
    ///
    /// # Errors
    ///
    /// Returns the first `CanonicalError` encountered in `results`.
    pub fn ensure_all_ok(results: &[Self]) -> Result<(), CanonicalError> {
        for result in results {
            if let Self::Err { error, .. } = result {
                return Err(error.clone());
            }
        }
        Ok(())
    }
}

/// Summary of a batch registration operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterSummary {
    /// Number of successfully registered entities.
    pub succeeded: usize,
    /// Number of failed registrations.
    pub failed: usize,
}

impl RegisterSummary {
    /// Creates a new summary from a slice of register results.
    #[must_use]
    pub fn from_results(results: &[RegisterResult]) -> Self {
        let succeeded = results.iter().filter(|r| r.is_ok()).count();
        let failed = results.len() - succeeded;
        Self { succeeded, failed }
    }

    /// Returns `true` if all registrations succeeded.
    #[must_use]
    pub const fn all_succeeded(&self) -> bool {
        self.failed == 0
    }

    /// Returns `true` if all registrations failed.
    #[must_use]
    pub const fn all_failed(&self) -> bool {
        self.succeeded == 0
    }

    /// Returns the total number of items processed.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.succeeded + self.failed
    }
}

/// Query parameters for listing GTS type-schemas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeSchemaQuery {
    /// Optional GTS wildcard pattern (e.g. `gts.acme.*`).
    pub pattern: Option<String>,
}

impl TypeSchemaQuery {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.pattern = Some(pattern.into());
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pattern.is_none()
    }
}

/// Query parameters for listing GTS instances.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstanceQuery {
    /// Optional GTS wildcard pattern (e.g. `gts.acme.events.user.v1~*`).
    pub pattern: Option<String>,
}

impl InstanceQuery {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.pattern = Some(pattern.into());
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pattern.is_none()
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
