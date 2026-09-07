//! Ontology analysis: derivation chains, trait resolution and chain
//! validation (DESIGN § 3.1 Base Ontology GTS Schemas).
//!
//! Pure logic over schemas — no I/O. Both the domain services and the store
//! implementations use it, so what registration validated is exactly what
//! ingest later enforces.
//!
//! Pattern semantics are the platform's (`gts` crate), evaluated by set
//! resolution — a pattern is never compiled into SQL text, so no identifier
//! ever reaches a `LIKE` pattern.

use std::collections::BTreeMap;

use graph_storage_sdk::models::{EffectiveTraits, GtsTypeId, TypeKind};
use gts::{GtsId, GtsIdPattern};
use serde_json::Value;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The nine base-ontology schemas, embedded from the crate's `schemas/` so the
/// boot registration and the published schema files cannot drift apart. They
/// live inside the crate rather than under `docs/` because `cargo package`
/// ships nothing outside the crate directory.
pub const BASE_SCHEMAS: [(&str, &str); 9] = [
    (
        graph_storage_sdk::gts::NODE_BASE_TYPE,
        include_str!("../../schemas/gts.cf.core.graph.node.v1~.schema.json"),
    ),
    (
        graph_storage_sdk::gts::EDGE_BASE_TYPE,
        include_str!("../../schemas/gts.cf.core.graph.edge.v1~.schema.json"),
    ),
    (
        graph_storage_sdk::gts::ATTRIBUTE_BASE_TYPE,
        include_str!("../../schemas/gts.cf.core.graph.attribute.v1~.schema.json"),
    ),
    (
        graph_storage_sdk::gts::OWNED_NODE_TYPE,
        include_str!(
            "../../schemas/gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~.schema.json"
        ),
    ),
    (
        graph_storage_sdk::gts::REFERENCE_NODE_TYPE,
        include_str!(
            "../../schemas/gts.cf.core.graph.node.v1~cf.core.graph.reference_node.v1~.schema.json"
        ),
    ),
    (
        graph_storage_sdk::gts::PHANTOM_NODE_TYPE,
        include_str!(
            "../../schemas/gts.cf.core.graph.node.v1~cf.core.graph.phantom_node.v1~.schema.json"
        ),
    ),
    (
        graph_storage_sdk::gts::STATIC_EDGE_TYPE,
        include_str!(
            "../../schemas/gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~.schema.json"
        ),
    ),
    (
        graph_storage_sdk::gts::ANALYSIS_EDGE_TYPE,
        include_str!(
            "../../schemas/gts.cf.core.graph.edge.v1~cf.core.graph.analysis_edge.v1~.schema.json"
        ),
    ),
    (
        graph_storage_sdk::gts::PROVENANCE_ATTRIBUTE_TYPE,
        include_str!(
            "../../schemas/gts.cf.core.graph.attribute.v1~cf.core.graph.provenance.v1~.schema.json"
        ),
    ),
];

/// The platform's GTS keyword vocabulary. The gear registers **no** extension
/// keyword of its own, so this list is exactly what `gts` defines; anything
/// else spelled `x-*` is rejected rather than ignored, because an annotation
/// silently skipped is a constraint the producer believes exists.
const KNOWN_EXTENSIONS: [&str; 5] = [
    "x-gts-abstract",
    "x-gts-final",
    "x-gts-ref",
    "x-gts-traits",
    "x-gts-traits-schema",
];

/// Everything registration derives from one schema.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeDescriptor {
    pub type_id: GtsTypeId,
    pub type_uuid: Uuid,
    pub kind: TypeKind,
    pub is_abstract: bool,
    pub effective_traits: EffectiveTraits,
    /// The `index` trait resolved against the chain's schemas: each declared
    /// pointer with the scalar kind the schema gives it. Registration refuses
    /// a pointer that lands nowhere or on a non-scalar, so a projection can
    /// trust that every admitted path has a kind (DEVIATIONS D-104).
    pub index_paths: Vec<IndexedPath>,
    pub schema: Value,
}

/// The scalar type of one declared `index` path, taken from the type's own
/// schema. The extraction expression, the comparison semantics and the cursor
/// codec all depend on it: `'10' < '9'` as text, `10 > 9` as a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarKind {
    String,
    Number,
    Integer,
    Boolean,
    /// A `string` with `format: date-time`. Compared as text in this
    /// iteration, which is exact for RFC 3339 timestamps in one offset and
    /// approximate across offsets (see D-104).
    DateTime,
}

impl ScalarKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::DateTime => "date-time",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "integer" => Some(Self::Integer),
            "boolean" => Some(Self::Boolean),
            "date-time" => Some(Self::DateTime),
            _ => None,
        }
    }
}

/// One declared `index` path with its resolved kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedPath {
    /// JSON pointer from the node document root, e.g. `/payload/severity`.
    pub pointer: String,
    pub kind: ScalarKind,
}

/// The characters a pointer token may use. Declared paths are rendered into
/// SQL text (an extraction expression has to match its index expression
/// byte for byte), so the alphabet is closed here rather than escaped there.
fn token_is_plain(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Resolve every `index` pointer of a type against the chain's schemas.
///
/// The walk descends `properties` from the document root, leaf schema first
/// (a derived type's refinement is the authority on its own members), then
/// each ancestor; `allOf` branches of one schema are searched as well. The
/// first `type` found decides. A pointer that resolves nowhere, or to an
/// object or array, is refused: an index over it would serve no query.
pub fn resolve_index_paths(
    type_id: &str,
    chain_schemas: &[&Value],
    pointers: &[String],
) -> Result<Vec<IndexedPath>, DomainError> {
    let mut out = Vec::with_capacity(pointers.len());
    for pointer in pointers {
        let Some(rest) = pointer.strip_prefix("/payload/") else {
            return Err(invalid_type(
                type_id,
                format!("index path `{pointer}` must point below `/payload`"),
            ));
        };
        let tokens: Vec<&str> = rest.split('/').collect();
        if tokens.iter().any(|t| !token_is_plain(t)) {
            return Err(invalid_type(
                type_id,
                format!(
                    "index path `{pointer}` has a token outside `[A-Za-z0-9_.-]`; \
                     declared paths are rendered into index expressions"
                ),
            ));
        }
        let mut found = None;
        for schema in chain_schemas.iter().rev() {
            if let Some(kind) = scalar_kind_at(schema, &["payload"], &tokens) {
                found = Some(kind);
                break;
            }
        }
        match found {
            Some(Ok(kind)) => out.push(IndexedPath {
                pointer: pointer.clone(),
                kind,
            }),
            Some(Err(seen)) => {
                return Err(invalid_type(
                    type_id,
                    format!("index path `{pointer}` resolves to `{seen}`, not a scalar"),
                ));
            }
            None => {
                return Err(invalid_type(
                    type_id,
                    format!(
                        "index path `{pointer}` does not resolve to a declared property in the \
                         type's schema chain"
                    ),
                ));
            }
        }
    }
    Ok(out)
}

/// Walk `schema` along `prefix ++ tokens` through `properties` (and `allOf`
/// branches), returning the scalar kind of the property reached, `Err` with
/// the non-scalar type name, or `None` when the path is not declared here.
fn scalar_kind_at(
    schema: &Value,
    prefix: &[&str],
    tokens: &[&str],
) -> Option<Result<ScalarKind, String>> {
    let mut path: Vec<&str> = prefix.to_vec();
    path.extend_from_slice(tokens);
    property_at(schema, &path).map(kind_of_property)
}

fn property_at<'a>(schema: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let Some((head, tail)) = path.split_first() else {
        return Some(schema);
    };
    if let Some(next) = schema
        .get("properties")
        .and_then(|p| p.get(head))
        .and_then(|next| property_at(next, tail))
    {
        return Some(next);
    }
    schema
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|branch| property_at(branch, path))
}

fn kind_of_property(property: &Value) -> Result<ScalarKind, String> {
    let declared = match property.get("type") {
        Some(Value::String(t)) => Some(t.as_str()),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null"),
        _ => None,
    };
    let declared = declared.or_else(|| {
        // An `enum` of strings without an explicit `type` is a string.
        property
            .get("enum")
            .and_then(Value::as_array)
            .filter(|values| values.iter().all(Value::is_string))
            .map(|_| "string")
    });
    match declared {
        Some("string") => {
            if property.get("format").and_then(Value::as_str) == Some("date-time") {
                Ok(ScalarKind::DateTime)
            } else {
                Ok(ScalarKind::String)
            }
        }
        Some("number") => Ok(ScalarKind::Number),
        Some("integer") => Ok(ScalarKind::Integer),
        Some("boolean") => Ok(ScalarKind::Boolean),
        Some(other) => Err(other.to_owned()),
        None => Err("an untyped schema".to_owned()),
    }
}

/// The derivation chain of a GTS identifier, outermost base first, the
/// identifier itself last. `a.v1~b.v1~c.v1~` -> `[a.v1~, a.v1~b.v1~, ...]`.
#[must_use]
pub fn ancestors(type_id: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let mut end = 0usize;
    for (index, ch) in type_id.char_indices() {
        if ch == '~' {
            end = index + 1;
            chain.push(type_id[..end].to_string());
        }
    }
    // A well-formed id ends in '~', so the last prefix is the id itself.
    if end != type_id.len() {
        chain.push(type_id.to_owned());
    }
    chain
}

fn invalid_type(type_id: &str, message: impl std::fmt::Display) -> DomainError {
    DomainError::invalid(format!("type `{type_id}`: {message}"))
}

/// Which base the chain is rooted in.
#[must_use]
pub fn kind_of(type_id: &str) -> Option<TypeKind> {
    let root = ancestors(type_id).into_iter().next()?;
    match root.as_str() {
        graph_storage_sdk::gts::NODE_BASE_TYPE => Some(TypeKind::Node),
        graph_storage_sdk::gts::EDGE_BASE_TYPE => Some(TypeKind::Edge),
        graph_storage_sdk::gts::ATTRIBUTE_BASE_TYPE => Some(TypeKind::Attribute),
        _ => None,
    }
}

fn traits_object(schema: &Value) -> Option<&serde_json::Map<String, Value>> {
    schema.get("x-gts-traits").and_then(Value::as_object)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Merge trait values down the chain: base defaults first (from the base's
/// `x-gts-traits-schema`), then every `x-gts-traits` from the outermost
/// ancestor to the leaf. A registered type stores this resolution, so a
/// 10,000-item batch validates without re-walking the chain.
fn resolve_traits(chain_schemas: &[&Value]) -> EffectiveTraits {
    let mut merged: BTreeMap<String, Value> = BTreeMap::new();

    if let Some(base) = chain_schemas.first()
        && let Some(declared) = base
            .get("x-gts-traits-schema")
            .and_then(|s| s.get("properties"))
            .and_then(Value::as_object)
    {
        for (name, spec) in declared {
            if let Some(default) = spec.get("default") {
                merged.insert(name.clone(), default.clone());
            }
        }
    }

    for schema in chain_schemas {
        if let Some(traits) = traits_object(schema) {
            for (name, value) in traits {
                merged.insert(name.clone(), value.clone());
            }
        }
    }

    EffectiveTraits {
        family: merged
            .get("family")
            .and_then(Value::as_str)
            .map(str::to_owned),
        scope_managed: merged
            .get("scope_managed")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        emit_events: merged
            .get("emit_events")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        index: string_array(merged.get("index")),
        full_text_search: string_array(merged.get("full_text_search")),
        vector_search: string_array(merged.get("vector_search")),
        src_types: string_array(merged.get("src_types")),
        dst_types: string_array(merged.get("dst_types")),
    }
}

/// Analyze one schema against its (already validated) ancestor schemas.
///
/// `ancestor_schemas` is ordered outermost base first and covers every
/// ancestor of `type_id`; the caller resolves them from the current batch and
/// the registered set.
pub fn analyze(
    type_id: &str,
    schema: &Value,
    ancestor_schemas: &[&Value],
    max_chain_depth: usize,
) -> Result<TypeDescriptor, DomainError> {
    let parsed = GtsId::try_new(type_id)
        .map_err(|error| invalid_type(type_id, format!("not a valid GTS identifier: {error}")))?;

    let kind = kind_of(type_id).ok_or_else(|| {
        invalid_type(
            type_id,
            "does not derive from a graph-storage base (node / edge / attribute)",
        )
    })?;

    let chain = ancestors(type_id);
    // The platform guideline (GTS.md § 9) recommends two derivations, which
    // is 3 segments (base -> family -> producer type) and the default. It is
    // a design recommendation, not a capability of the type system: nothing
    // below depends on the length, so a deployment mirroring a deeper domain
    // hierarchy raises `ontology_max_chain_depth` (DEVIATIONS D-029).
    if chain.len() > max_chain_depth {
        return Err(invalid_type(
            type_id,
            format!(
                "derivation chain has {} segments; this deployment admits at most {max_chain_depth} \
                 (`ontology_max_chain_depth`)",
                chain.len()
            ),
        ));
    }
    if ancestor_schemas.len() + 1 != chain.len() {
        return Err(invalid_type(
            type_id,
            format!(
                "expected {} ancestor schema(s), got {}",
                chain.len() - 1,
                ancestor_schemas.len()
            ),
        ));
    }

    if let Some(object) = schema.as_object() {
        for key in object.keys() {
            if key.starts_with("x-") && !KNOWN_EXTENSIONS.contains(&key.as_str()) {
                return Err(invalid_type(
                    type_id,
                    format!(
                        "unknown extension keyword `{key}`; the gear registers none of its own"
                    ),
                ));
            }
        }
    } else {
        return Err(invalid_type(type_id, "schema is not a JSON object"));
    }

    // The $id must agree with the identifier the type is registered under.
    if let Some(id) = schema.get("$id").and_then(Value::as_str) {
        let expected = format!("gts://{type_id}");
        if id != expected {
            return Err(invalid_type(
                type_id,
                format!("$id `{id}` does not match `{expected}`"),
            ));
        }
    }

    // Derivation from the final (non-abstract) phantom type is refused.
    if chain.len() > 1 {
        let parent = &chain[chain.len() - 2];
        if parent == graph_storage_sdk::gts::PHANTOM_NODE_TYPE {
            return Err(invalid_type(
                type_id,
                "cannot derive from the phantom node type",
            ));
        }
        // A node or edge type derives from a family, never a base directly.
        // The base enforces it structurally (`family` is required with no
        // default), and the analysis names it instead of failing opaquely.
        // Attributes have no families — they are payload fragments, not
        // storable rows — so the rule does not apply to them.
        let parent_is_base = chain.len() == 2 && kind != TypeKind::Attribute;
        let parent_fixes_family = ancestor_schemas
            .last()
            .and_then(|s| traits_object(s))
            .is_some_and(|t| t.contains_key("family"));
        if parent_is_base && schema.get("x-gts-abstract").and_then(Value::as_bool) != Some(true) {
            let own_fixes_family = traits_object(schema).is_some_and(|t| t.contains_key("family"));
            if !parent_fixes_family && !own_fixes_family {
                return Err(invalid_type(
                    type_id,
                    "derives directly from a base without fixing `family`; derive from a family type",
                ));
            }
        }
    }

    let mut chain_schemas: Vec<&Value> = ancestor_schemas.to_vec();
    chain_schemas.push(schema);
    let effective_traits = resolve_traits(&chain_schemas);
    let index_paths = if kind == TypeKind::Node {
        resolve_index_paths(type_id, &chain_schemas, &effective_traits.index)?
    } else {
        Vec::new()
    };

    let is_abstract = schema.get("x-gts-abstract").and_then(Value::as_bool) == Some(true);
    if !is_abstract && kind != TypeKind::Attribute && effective_traits.family.is_none() {
        return Err(invalid_type(
            type_id,
            "resolves no `family` trait; only abstract types may leave it open",
        ));
    }

    Ok(TypeDescriptor {
        type_id: type_id.to_owned(),
        type_uuid: parsed.to_uuid(),
        kind,
        is_abstract,
        effective_traits,
        index_paths,
        schema: schema.clone(),
    })
}

/// Does `candidate` match any of `patterns`? Platform pattern semantics,
/// never text matching.
pub fn matches_any_pattern(candidate: &str, patterns: &[String]) -> Result<bool, DomainError> {
    let id = GtsId::try_new(candidate)
        .map_err(|error| DomainError::invalid(format!("`{candidate}`: {error}")))?;
    for pattern in patterns {
        let compiled = GtsIdPattern::try_new(pattern)
            .map_err(|error| DomainError::invalid(format!("pattern `{pattern}`: {error}")))?;
        if id.matches_pattern(&compiled) {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// Chain validation of instances
// ---------------------------------------------------------------------------

/// A compiled validator for one registered type: the leaf schema with every
/// ancestor resolvable through its `gts://` references, so validating the
/// leaf validates the whole chain (each `allOf` branch evaluates
/// independently).
pub struct ChainValidator {
    validator: jsonschema::Validator,
}

struct MapRetriever {
    schemas: BTreeMap<String, Value>,
}

impl jsonschema::Retrieve for MapRetriever {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        self.schemas
            .get(uri.as_str())
            .cloned()
            .ok_or_else(|| format!("unresolved schema reference `{uri}`").into())
    }
}

impl ChainValidator {
    /// Compile the leaf schema, resolving `gts://` references from the given
    /// chain (ancestors and the leaf itself, in any order).
    pub fn compile(
        leaf: &Value,
        chain: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<Self, DomainError> {
        let schemas: BTreeMap<String, Value> = chain
            .into_iter()
            .map(|(id, schema)| (format!("gts://{id}"), schema))
            .collect();
        let validator = jsonschema::options()
            .with_retriever(MapRetriever { schemas })
            .build(leaf)
            .map_err(|error| DomainError::invalid(format!("schema does not compile: {error}")))?;
        Ok(Self { validator })
    }

    /// Validate one instance envelope, reporting **every** violation with its
    /// JSON pointer — a producer fixes a batch in one round trip, not one
    /// error at a time.
    #[must_use]
    pub fn validate(&self, instance: &Value) -> Vec<(String, String)> {
        self.validator
            .iter_errors(instance)
            .map(|error| (error.instance_path().to_string(), error.to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform posture: base -> family -> producer type.
    const DEFAULT_DEPTH: usize = 3;

    #[allow(clippy::needless_pass_by_value)]
    fn owned_leaf(leaf: &str, traits: Value, payload_properties: Value) -> (String, Value) {
        let id = format!("{}{leaf}", graph_storage_sdk::gts::OWNED_NODE_TYPE);
        let schema = serde_json::json!({
            "$id": format!("gts://{id}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": traits,
            "type": "object",
            "allOf": [
                { "$ref": format!("gts://{}", graph_storage_sdk::gts::OWNED_NODE_TYPE) },
                { "type": "object", "properties": { "payload": {
                    "type": "object", "properties": payload_properties } } }
            ]
        });
        (id, schema)
    }

    fn owned_ancestors() -> Vec<Value> {
        vec![
            base_schema(graph_storage_sdk::gts::NODE_BASE_TYPE),
            base_schema(graph_storage_sdk::gts::OWNED_NODE_TYPE),
        ]
    }

    #[test]
    fn an_index_path_resolves_its_scalar_kind_from_the_schema() {
        let (id, schema) = owned_leaf(
            "acme.dm._.finding.v1~",
            serde_json::json!({ "index": [
                "/payload/severity", "/payload/score", "/payload/count",
                "/payload/open", "/payload/seen_at", "/payload/loc/line" ] }),
            serde_json::json!({
                "severity": { "enum": ["low", "high"] },
                "score": { "type": "number" },
                "count": { "type": ["integer", "null"] },
                "open": { "type": "boolean" },
                "seen_at": { "type": "string", "format": "date-time" },
                "loc": { "type": "object", "properties": { "line": { "type": "integer" } } }
            }),
        );
        let ancestors = owned_ancestors();
        let refs: Vec<&Value> = ancestors.iter().collect();
        let descriptor =
            analyze(&id, &schema, &refs, DEFAULT_DEPTH).unwrap_or_else(|e| panic!("{e}"));
        let kinds: Vec<(&str, ScalarKind)> = descriptor
            .index_paths
            .iter()
            .map(|p| (p.pointer.as_str(), p.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("/payload/severity", ScalarKind::String),
                ("/payload/score", ScalarKind::Number),
                ("/payload/count", ScalarKind::Integer),
                ("/payload/open", ScalarKind::Boolean),
                ("/payload/seen_at", ScalarKind::DateTime),
                ("/payload/loc/line", ScalarKind::Integer),
            ]
        );
    }

    #[test]
    fn an_index_path_onto_an_object_or_nowhere_is_refused() {
        let ancestors = owned_ancestors();
        let refs: Vec<&Value> = ancestors.iter().collect();

        let (id, schema) = owned_leaf(
            "acme.dm._.thing.v1~",
            serde_json::json!({ "index": ["/payload/meta"] }),
            serde_json::json!({ "meta": { "type": "object" } }),
        );
        let error = analyze(&id, &schema, &refs, DEFAULT_DEPTH).expect_err("object refused");
        assert!(error.to_string().contains("not a scalar"), "{error}");

        let (id, schema) = owned_leaf(
            "acme.dm._.other.v1~",
            serde_json::json!({ "index": ["/payload/ghost"] }),
            serde_json::json!({ "real": { "type": "string" } }),
        );
        let error = analyze(&id, &schema, &refs, DEFAULT_DEPTH).expect_err("undeclared refused");
        assert!(error.to_string().contains("does not resolve"), "{error}");

        let (id, schema) = owned_leaf(
            "acme.dm._.third.v1~",
            serde_json::json!({ "index": ["/name"] }),
            serde_json::json!({}),
        );
        let error =
            analyze(&id, &schema, &refs, DEFAULT_DEPTH).expect_err("outside payload refused");
        assert!(error.to_string().contains("below `/payload`"), "{error}");
    }

    /// A domain hierarchy mirrored into the chain: family -> managed object
    /// -> document -> requirement. The default depth refuses it, a raised one
    /// admits it, an ancestor pattern selects it, and a trait declared on the
    /// intermediate type reaches the leaf.
    #[test]
    fn a_deeper_chain_is_a_policy_decision_not_a_capability() {
        let family = graph_storage_sdk::gts::OWNED_NODE_TYPE;
        let managed = format!("{family}acme.dm.core.managed_object.v1~");
        let document = format!("{managed}acme.dm.core.document.v1~");
        let requirement = format!("{document}acme.dm.sdlc.requirement.v1~");

        let intermediate = |id: &str, parent: &str, traits: Value, props: Value| {
            serde_json::json!({
                "$id": format!("gts://{id}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "x-gts-abstract": true,
                "x-gts-traits": traits,
                "type": "object",
                "allOf": [
                    { "$ref": format!("gts://{parent}") },
                    { "type": "object", "properties": { "payload": {
                        "type": "object", "properties": props } } }
                ]
            })
        };
        let managed_schema = intermediate(
            &managed,
            family,
            serde_json::json!({ "index": ["/payload/status"], "full_text_search": ["/name", "/payload/title"] }),
            serde_json::json!({ "status": { "type": "string" }, "title": { "type": "string" } }),
        );
        let document_schema = intermediate(
            &document,
            &managed,
            serde_json::json!({}),
            serde_json::json!({ "url": { "type": "string" } }),
        );
        let mut requirement_schema = intermediate(
            &requirement,
            &document,
            serde_json::json!({}),
            serde_json::json!({ "priority": { "type": "integer" } }),
        );
        requirement_schema
            .as_object_mut()
            .and_then(|o| o.remove("x-gts-abstract"));

        let mut chain = owned_ancestors();
        chain.push(managed_schema);
        chain.push(document_schema);
        let refs: Vec<&Value> = chain.iter().collect();

        let error = analyze(&requirement, &requirement_schema, &refs, DEFAULT_DEPTH)
            .expect_err("the platform posture refuses five segments");
        assert!(
            error.to_string().contains("ontology_max_chain_depth"),
            "{error}"
        );

        let descriptor =
            analyze(&requirement, &requirement_schema, &refs, 8).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(descriptor.effective_traits.family.as_deref(), Some("owned"));
        assert_eq!(descriptor.effective_traits.index, vec!["/payload/status"]);
        assert_eq!(
            descriptor.index_paths,
            vec![IndexedPath {
                pointer: "/payload/status".into(),
                kind: ScalarKind::String
            }]
        );
        assert!(!descriptor.is_abstract);

        assert_eq!(
            matches_any_pattern(&requirement, &[format!("{managed}*")]).ok(),
            Some(true),
            "an ancestor pattern selects the whole subtree"
        );
        assert_eq!(
            matches_any_pattern(&requirement, &[format!("{document}*")]).ok(),
            Some(true)
        );
        assert_eq!(
            matches_any_pattern(&requirement, &[format!("{family}acme.dm.core.other.v1~*")]).ok(),
            Some(false)
        );
    }

    /// The endpoint check is only safe because of how the platform matcher
    /// reads a pattern: a base identifier admits everything derived from it.
    ///
    /// The default constraint on every edge type is the bare node base, so
    /// enabling the check constrains nothing that used to pass; a narrower
    /// family identifier is what gives it teeth. Both halves are the
    /// platform's behaviour, not ours, so they are pinned here.
    #[test]
    fn a_pattern_admits_what_derives_from_it_and_nothing_else() {
        let commit =
            "gts.cf.core.graph.node.v1~cf.core.graph.reference_node.v1~acme.scm._.commit.v1~";

        assert_eq!(
            matches_any_pattern(commit, &["gts.cf.core.graph.node.v1~".to_owned()]).ok(),
            Some(true),
            "the base every node type derives from admits them all"
        );
        assert_eq!(
            matches_any_pattern(
                commit,
                &["gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~".to_owned()]
            )
            .ok(),
            Some(false),
            "a sibling family does not admit a reference node"
        );
    }

    fn base_schema(id: &str) -> Value {
        let (_, raw) = BASE_SCHEMAS
            .iter()
            .find(|(schema_id, _)| *schema_id == id)
            .unwrap_or_else(|| panic!("missing base schema {id}"));
        serde_json::from_str(raw).unwrap_or_else(|e| panic!("{id} does not parse: {e}"))
    }

    #[test]
    fn every_embedded_base_schema_parses_and_analyzes() {
        for (type_id, _) in BASE_SCHEMAS {
            let schema = base_schema(type_id);
            let chain = ancestors(type_id);
            let ancestor_values: Vec<Value> = chain[..chain.len() - 1]
                .iter()
                .map(|a| base_schema(a))
                .collect();
            let ancestor_refs: Vec<&Value> = ancestor_values.iter().collect();
            let descriptor = analyze(type_id, &schema, &ancestor_refs, DEFAULT_DEPTH)
                .unwrap_or_else(|e| panic!("{type_id}: {e}"));
            // The three bases and the node/edge families are abstract. The
            // two concrete ones are deliberate: the phantom type, which the
            // gear itself instantiates, and the provenance attribute, which
            // producers embed in analysis-edge payloads.
            let expected_abstract = type_id != graph_storage_sdk::gts::PHANTOM_NODE_TYPE
                && type_id != graph_storage_sdk::gts::PROVENANCE_ATTRIBUTE_TYPE;
            assert_eq!(
                descriptor.is_abstract, expected_abstract,
                "{type_id} abstractness"
            );
        }
    }

    #[test]
    fn family_types_resolve_their_family() {
        let schema = base_schema(graph_storage_sdk::gts::OWNED_NODE_TYPE);
        let base = base_schema(graph_storage_sdk::gts::NODE_BASE_TYPE);
        let descriptor = analyze(
            graph_storage_sdk::gts::OWNED_NODE_TYPE,
            &schema,
            &[&base],
            DEFAULT_DEPTH,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(descriptor.effective_traits.family.as_deref(), Some("owned"));
        assert!(descriptor.effective_traits.scope_managed);
        assert_eq!(descriptor.kind, TypeKind::Node);
    }

    #[test]
    fn an_unknown_extension_keyword_is_rejected() {
        let mut schema = base_schema(graph_storage_sdk::gts::OWNED_NODE_TYPE);
        schema
            .as_object_mut()
            .and_then(|o| o.insert("x-gts-indexed".into(), serde_json::json!(["/payload/x"])));
        let base = base_schema(graph_storage_sdk::gts::NODE_BASE_TYPE);
        let error = analyze(
            graph_storage_sdk::gts::OWNED_NODE_TYPE,
            &schema,
            &[&base],
            DEFAULT_DEPTH,
        )
        .expect_err("unknown extension must be rejected");
        assert!(error.to_string().contains("x-gts-indexed"), "{error}");
    }

    #[test]
    fn ancestors_walk_the_chain_outermost_first() {
        let leaf = format!(
            "{}acme.sec._.finding.v1~",
            graph_storage_sdk::gts::OWNED_NODE_TYPE
        );
        assert_eq!(
            ancestors(&leaf),
            vec![
                graph_storage_sdk::gts::NODE_BASE_TYPE.to_owned(),
                graph_storage_sdk::gts::OWNED_NODE_TYPE.to_owned(),
                leaf,
            ]
        );
    }
}
