//! Conformance test between `docs/usage-collector-v1.yaml` and the
//! `OpenAPI` surface that `OperationBuilder` actually registers.
//!
//! The registry is built by calling the two per-resource route registrars
//! directly, so this needs no server, no database, and no `Service`.
//!
//! Enforced: the set of operations, their methods, operation ids, tags,
//! parameters (name / location / required), request bodies (media type,
//! `required`, referenced schema), 2xx responses (status, media type,
//! referenced schema), the canonical error surface on both sides,
//! per-operation authentication posture, that every component the
//! runtime publishes is documented, and that every `$ref` in the
//! document resolves.
//!
//! Component naming: the document names a component after the wire shape
//! it describes, while the Rust DTO behind it carries a `Dto` suffix by
//! convention (`UsageRecord` / `UsageRecordDto`, `Page_UsageRecord` /
//! `Page_UsageRecordDto`). That suffix is the *only* admitted difference
//! — [`contract_name`] strips it and everything else must match
//! character for character, so there is no per-name exemption list that
//! can grow a row per divergence, and
//! [`every_registered_component_is_documented`] holds the nested
//! components to the same rule. The document additionally declares
//! finer-grained documentary components (`Timestamp`, `UsageValue`,
//! `IdempotencyKey`, …) that the generated document inlines into the
//! fields using them; the runtime has no component for those, so nothing
//! here compares them.
//!
//! Deliberately NOT enforced: descriptions, summaries, examples,
//! field-level schema contents, response headers (`ResponseSpec` carries
//! no header information), `info`, and `servers`.

use std::collections::{BTreeMap, BTreeSet};

use axum::Router;
use serde_json::Value;
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit::api::operation_builder::{OperationSpec, ParamLocation, RequestBodySchema};

use super::{usage_records, usage_types};

/// The contract document, embedded at compile time. Moving or deleting it
/// is a build error rather than a silently skipped test.
const YAML: &str = include_str!("../../../../../docs/usage-collector-v1.yaml");

/// Build a registry populated by the gear's full REST surface.
fn registry() -> OpenApiRegistryImpl {
    let reg = OpenApiRegistryImpl::new();
    let router = usage_types::register_usage_type_routes(Router::new(), &reg);
    let _router = usage_records::register_usage_record_routes(router, &reg);
    reg
}

fn spec_doc() -> Value {
    serde_saphyr::from_str(YAML).expect("usage-collector-v1.yaml must be valid YAML")
}

/// Canonical operation key shared by both sides, e.g.
/// `GET /usage-collector/v1/records`.
fn op_key(method: &str, path: &str) -> String {
    format!("{} {}", method.to_uppercase(), path)
}

fn registry_ops(reg: &OpenApiRegistryImpl) -> BTreeMap<String, OperationSpec> {
    reg.operation_specs
        .iter()
        .map(|entry| {
            let spec = entry.value().clone();
            (op_key(spec.method.as_str(), &spec.path), spec)
        })
        .collect()
}

const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

fn yaml_ops(doc: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let paths = doc["paths"].as_object().expect("`paths` must be a map");
    for (path, item) in paths {
        let item = item.as_object().expect("path item must be a map");
        // Path-item-level `parameters:` (e.g. the shared `{id}` / `{gts_id}`
        // declared once per path) apply to every operation under that path
        // per the `OpenAPI` spec -- *unless* the operation redeclares the
        // same (name, in) pair, in which case OpenAPI 3.x says the
        // operation-level entry overrides the path-level one rather than
        // adding a second, conflicting parameter. Keying the merge on the
        // resolved (name, in) pair and inserting path-level entries before
        // operation-level ones (so the latter overwrite on a collision)
        // implements exactly that override rule.
        let path_params = item.get("parameters").and_then(Value::as_array).cloned();
        for (method, op) in item {
            if HTTP_METHODS.contains(&method.as_str()) {
                let mut op = op.clone();
                if let Some(path_params) = &path_params {
                    let mut merged: BTreeMap<(String, String), Value> = BTreeMap::new();
                    for param in path_params {
                        merged.insert(param_key(doc, param), param.clone());
                    }
                    if let Some(op_params) = op.get("parameters").and_then(Value::as_array) {
                        for param in op_params {
                            merged.insert(param_key(doc, param), param.clone());
                        }
                    }
                    op["parameters"] = Value::Array(merged.into_values().collect());
                }
                out.insert(op_key(method, path), op);
            }
        }
    }
    out
}

/// `(name, in)` for a parameter entry, resolving a local `$ref` first if
/// present. Used only to key the path/operation-level override merge above;
/// the merged output keeps each parameter's original (possibly `$ref`) form,
/// so `yaml_params`'s own `$ref`-resolution path is still exercised.
fn param_key(doc: &Value, param: &Value) -> (String, String) {
    name_and_location(deref(doc, param))
}

#[test]
fn harness_sees_the_whole_rest_surface() {
    let reg = registry();
    let doc = spec_doc();

    assert_eq!(
        registry_ops(&reg).len(),
        9,
        "expected 9 registered operations across both registrars",
    );
    assert_eq!(yaml_ops(&doc).len(), 9, "expected 9 documented operations");
}

#[test]
fn operation_identity_matches() {
    let reg = registry();
    let doc = spec_doc();
    let registered = registry_ops(&reg);
    let documented = yaml_ops(&doc);

    let registered_keys: BTreeSet<&String> = registered.keys().collect();
    let documented_keys: BTreeSet<&String> = documented.keys().collect();
    assert_eq!(
        registered_keys, documented_keys,
        "the set of routes must match the set of documented operations",
    );

    for (key, op) in &documented {
        let spec = spec_for(&registered, key);

        assert_eq!(
            op["operationId"].as_str(),
            spec.operation_id.as_deref(),
            "{key}: operationId mismatch",
        );

        let documented_tags: BTreeSet<String> = op["tags"]
            .as_array()
            .expect("every operation must declare tags")
            .iter()
            .map(|t| t.as_str().expect("tag must be a string").to_owned())
            .collect();
        let registered_tags: BTreeSet<String> = spec.tags.iter().cloned().collect();
        assert_eq!(documented_tags, registered_tags, "{key}: tag mismatch");
    }
}

/// The registered operation for a documented one.
///
/// Every test walks the documented operations and looks the registered
/// side up, so a yaml-only operation reaches this function. Indexing
/// would report `no entry found for key` — no path, no method, no file.
/// Tests run independently, so `operation_identity_matches` cannot be
/// relied on to fail first (`cargo test parameters_match` never runs it).
fn spec_for<'a>(registered: &'a BTreeMap<String, OperationSpec>, key: &str) -> &'a OperationSpec {
    registered.get(key).unwrap_or_else(|| {
        panic!(
            "{key}: documented in usage-collector-v1.yaml but no route registers it \
             (see `register_usage_record_routes` / `register_usage_type_routes`)"
        )
    })
}

/// `(name, location, required)` — the machine-checkable part of a parameter.
type ParamTriple = (String, String, bool);

/// Resolve a local `$ref` such as `#/components/parameters/Limit`, delegating
/// to `Value::pointer` (which handles RFC 6901 `~0`/`~1` escapes correctly)
/// and panicking with the offending pointer if it dangles, rather than
/// silently walking off into `Value::Null` and failing later with a message
/// that names neither the `$ref` nor the operation it came from.
fn resolve_ref<'a>(doc: &'a Value, pointer: &str) -> &'a Value {
    doc.pointer(pointer.trim_start_matches('#'))
        .unwrap_or_else(|| panic!("dangling $ref in the contract: {pointer}"))
}

/// Resolve a node's `$ref`, if it has one, to the object it points at;
/// otherwise return the node unchanged. Applies to every position where
/// the document may substitute a reference for an inline object —
/// parameters, request bodies, and responses.
fn deref<'a>(doc: &'a Value, node: &'a Value) -> &'a Value {
    node.get("$ref")
        .and_then(Value::as_str)
        .map_or(node, |r| resolve_ref(doc, r))
}

/// Collect every `$ref` pointer in the document, at any depth.
fn collect_refs(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(fields) => {
            for (key, value) in fields {
                match (key.as_str(), value.as_str()) {
                    ("$ref", Some(pointer)) => out.push(pointer.to_owned()),
                    _ => collect_refs(value, out),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        _ => {}
    }
}

/// Every `$ref` in the document points at something that exists.
///
/// `deref` and `ref_name` only reach the references hanging directly off
/// an operation, which is the minority of them: the rest are nested
/// inside `components` (`Page_UsageRecord.items`, every field of
/// `UsageRecord`), and a dangling reference there makes the document
/// undereferenceable just as completely.
#[test]
fn every_ref_resolves() {
    let doc = spec_doc();
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);

    for pointer in &refs {
        let _ = resolve_ref(&doc, pointer);
    }

    assert!(
        !refs.is_empty(),
        "no $ref found in the document; collect_refs is broken",
    );
}

/// `(name, in)` for an already-`$ref`-resolved parameter object.
fn name_and_location(param: &Value) -> (String, String) {
    (
        param["name"].as_str().expect("parameter name").to_owned(),
        param["in"].as_str().expect("parameter location").to_owned(),
    )
}

fn yaml_params(doc: &Value, op: &Value) -> BTreeSet<ParamTriple> {
    let Some(params) = op.get("parameters").and_then(Value::as_array) else {
        return BTreeSet::new();
    };
    params
        .iter()
        .map(|param| {
            let param = deref(doc, param);
            let (name, location) = name_and_location(param);
            let required = param
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            (name, location, required)
        })
        .collect()
}

fn registry_params(spec: &OperationSpec) -> BTreeSet<ParamTriple> {
    spec.params
        .iter()
        .map(|param| {
            let location = match param.location {
                ParamLocation::Path => "path",
                ParamLocation::Query => "query",
                ParamLocation::Header => "header",
                ParamLocation::Cookie => "cookie",
            };
            (param.name.clone(), location.to_owned(), param.required)
        })
        .collect()
}

#[test]
fn parameters_match() {
    let reg = registry();
    let doc = spec_doc();
    let registered = registry_ops(&reg);

    for (key, op) in yaml_ops(&doc) {
        let spec = spec_for(&registered, &key);
        assert_eq!(
            yaml_params(&doc, &op),
            registry_params(spec),
            "{key}: documented parameters differ from the registered ones",
        );
    }
}

/// The document's name for the component the server registers as
/// `registered`.
///
/// The generated document names every component after the Rust DTO behind
/// it, and the DTO layer suffixes those types with `Dto`; the hand-written
/// contract names the wire shape. Stripping that suffix is the entire
/// translation between the two — a renamed DTO, or a document schema
/// renamed out from under one, fails the body comparison with both names
/// in the message instead of silently earning an exemption.
fn contract_name(registered: &str) -> String {
    registered
        .strip_suffix("Dto")
        .unwrap_or(registered)
        .to_owned()
}

/// Last path segment of a local `$ref`, if the value is a `$ref` at all.
///
/// The pointer is resolved before its name is taken, so a `$ref` whose
/// target is missing from `components.schemas` fails here rather than
/// shipping a document that no `OpenAPI` tool can dereference. Without
/// that step the comparison is pure string manipulation on the pointer
/// and nothing in this file ever reads `components.schemas` at all —
/// renaming a component would leave every test green. Parameter `$ref`s
/// have always been resolved this way (`deref`).
fn ref_name(doc: &Value, schema: &Value) -> Option<String> {
    let pointer = schema.get("$ref").and_then(Value::as_str)?;
    let _ = resolve_ref(doc, pointer);
    pointer.rsplit('/').next().map(ToOwned::to_owned)
}

/// The single `(media type, media object)` pair of a `content` block.
///
/// `RequestBodySpec` and `ResponseSpec` each carry exactly one
/// `content_type`, so a block declaring several media types has no
/// representation on the registry side. Reducing it to its first entry
/// would compare half a body; panicking says which operation to fix.
fn single_media_type<'a>(key: &str, what: &str, content: &'a Value) -> (String, &'a Value) {
    let entries = content
        .as_object()
        .unwrap_or_else(|| panic!("{key}: the {what} `content` block must be a map"));
    let mut iter = entries.iter();
    match (iter.next(), iter.next()) {
        (Some((content_type, media)), None) => (content_type.clone(), media),
        (None, _) => panic!("{key}: the {what} declares an empty `content` block"),
        (Some(_), Some(_)) => panic!(
            "{key}: the {what} declares {n} media types; the registry models exactly \
             one per body, so this cannot be checked — split the operation or extend \
             `ResponseSpec`",
            n = entries.len(),
        ),
    }
}

/// `(media type, required, component)` — the machine-checkable part of a
/// request body.
type RequestBodyTriple = (String, bool, String);

fn yaml_request_body(doc: &Value, key: &str, op: &Value) -> Option<RequestBodyTriple> {
    let body = deref(doc, op.get("requestBody")?);
    let content = body
        .get("content")
        .unwrap_or_else(|| panic!("{key}: `requestBody` carries no `content` block"));
    let (content_type, media) = single_media_type(key, "request body", content);
    // OpenAPI's default for `requestBody.required` is `false`; every
    // builder helper that attaches a body defaults it to `true`, so an
    // omitted `required:` in the document is a real disagreement.
    let required = body
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let schema = media
        .get("schema")
        .unwrap_or_else(|| panic!("{key}: the request body declares no `schema`"));
    let name = ref_name(doc, schema).unwrap_or_else(|| {
        panic!(
            "{key}: the request body schema must be a `$ref` into \
             `components.schemas` — an inline schema has no counterpart in \
             `RequestBodySchema::Ref` and cannot be compared"
        )
    });
    Some((content_type, required, name))
}

/// The registered request body, or `None` when the route registers none.
///
/// A non-`Ref` schema variant panics instead of collapsing to `None`:
/// `None` is also what "this route has no request body at all" returns, so
/// mapping `Multipart` / `Binary` / `InlineObject` onto it would let a
/// route grow a multipart body, lose its `requestBody:` block in the
/// document, and stay green.
fn registry_request_body(key: &str, spec: &OperationSpec) -> Option<RequestBodyTriple> {
    let body = spec.request_body.as_ref()?;
    let schema_name = match &body.schema {
        RequestBodySchema::Ref { schema_name } => contract_name(schema_name),
        other => panic!(
            "{key}: the route registers a {other:?} request body, which the contract \
             document has no way to express. Extend both sides rather than leaving \
             the body unchecked."
        ),
    };
    Some((body.content_type.to_owned(), body.required, schema_name))
}

/// `(status, media type, component)` — the machine-checkable part of a
/// response. `None` media type is a body-less response (`204`); `None`
/// component is a body with no named schema.
type ResponseTriple = (u16, Option<String>, Option<String>);

fn yaml_success_responses(doc: &Value, key: &str, op: &Value) -> BTreeSet<ResponseTriple> {
    op["responses"]
        .as_object()
        .expect("every operation must declare responses")
        .iter()
        .filter_map(|(status, body)| {
            let status: u16 = status.parse().ok()?;
            if !(200..300).contains(&status) {
                return None;
            }
            // A response with no `content` advertises no body. Keeping
            // that distinct from "a body whose schema is unnamed" is the
            // point: `OpenAPI` code generators treat a `204` carrying a
            // `content` block as returning a payload, which is exactly
            // what `no_content_response` exists to avoid.
            let body = deref(doc, body);
            let Some(content) = body.get("content") else {
                return Some((status, None, None));
            };
            let (content_type, media) =
                single_media_type(key, &format!("{status} response"), content);
            let schema = media.get("schema").map(|schema| {
                ref_name(doc, schema).unwrap_or_else(|| {
                    panic!(
                        "{key}: the {status} response schema must be a `$ref` into \
                         `components.schemas`; `ResponseSpec` carries a component \
                         name, not an inline schema"
                    )
                })
            });
            Some((status, Some(content_type), schema))
        })
        .collect()
}

fn registry_success_responses(spec: &OperationSpec) -> BTreeSet<ResponseTriple> {
    spec.responses
        .iter()
        .filter(|r| (200..300).contains(&r.status))
        .map(|r| {
            // `no_content_response` marks a body-less response with an
            // empty content type.
            let content_type = (!r.content_type.is_empty()).then(|| r.content_type.to_owned());
            (
                r.status,
                content_type,
                r.schema_name.as_deref().map(contract_name),
            )
        })
        .collect()
}

#[test]
fn body_schemas_match() {
    let reg = registry();
    let doc = spec_doc();
    let registered = registry_ops(&reg);

    for (key, op) in yaml_ops(&doc) {
        let spec = spec_for(&registered, &key);

        assert_eq!(
            yaml_request_body(&doc, &key, &op),
            registry_request_body(&key, spec),
            "{key}: request body mismatch (media type, `required`, schema)",
        );
        assert_eq!(
            yaml_success_responses(&doc, &key, &op),
            registry_success_responses(spec),
            "{key}: 2xx response mismatch (status, media type, schema)",
        );
    }
}

/// Kept in sync with `OperationBuilder::standard_errors` in
/// `libs/toolkit/src/api/operation_builder.rs`.
const STANDARD_ERROR_STATUSES: &[u16] = &[400, 401, 403, 404, 409, 429, 500];

const PROBLEM_JSON: &str = "application/problem+json";

/// The standard-error statuses the route does *not* register as
/// `application/problem+json`.
fn missing_standard_errors(spec: &OperationSpec) -> Vec<u16> {
    STANDARD_ERROR_STATUSES
        .iter()
        .copied()
        .filter(|status| {
            !spec
                .responses
                .iter()
                .any(|r| r.status == *status && r.content_type == PROBLEM_JSON)
        })
        .collect()
}

/// Every operation carries the canonical error surface on both sides.
///
/// This is deliberately an unconditional requirement rather than a
/// yaml↔code agreement check. Any handler on a canonical route can
/// produce the full 400/401/403/404/409/429/500 set — the gateway
/// middleware alone accounts for 401, 403, and 429 — so an operation
/// without it is wrong, not merely undocumented. Phrasing it as
/// agreement would leave the check disarmed by the one edit most likely
/// to happen: dropping `.standard_errors(openapi)` from a route and
/// `default:` from its operation together, after which "both sides
/// agree" holds vacuously and the document promises nothing.
///
/// A route that genuinely cannot produce these has to say so by editing
/// this test.
#[test]
fn every_operation_declares_the_standard_error_set() {
    let reg = registry();
    let doc = spec_doc();
    let registered = registry_ops(&reg);

    for (key, op) in yaml_ops(&doc) {
        let spec = spec_for(&registered, &key);

        assert!(
            op["responses"].get("default").is_some(),
            "{key}: the operation must declare \
             `default: $ref: '#/components/responses/Problem'` — that is how the \
             document carries the canonical error envelope",
        );

        let missing = missing_standard_errors(spec);
        assert!(
            missing.is_empty(),
            "{key}: the route must register every standard error \
             {STANDARD_ERROR_STATUSES:?} as `application/problem+json` (that is \
             what `.standard_errors(openapi)` does) — missing {missing:?}",
        );
    }
}

/// Whether an operation requires authentication under the document's own
/// rules: an operation-level `security` array *replaces* the root-level
/// one, and an empty array means "this operation is public".
fn yaml_operation_authenticated(doc: &Value, key: &str, op: &Value) -> bool {
    let requirements = match op.get("security") {
        Some(value) => value
            .as_array()
            .unwrap_or_else(|| panic!("{key}: operation-level `security` must be an array")),
        None => doc["security"]
            .as_array()
            .expect("the contract must declare a root-level `security` array"),
    };

    // A requirement naming an undeclared scheme is as broken as a dangling
    // `$ref`, and `every_ref_resolves` cannot see it: security requirements
    // name schemes by key, not by pointer.
    for requirement in requirements {
        let names = requirement
            .as_object()
            .unwrap_or_else(|| panic!("{key}: each `security` entry must be a map"));
        for scheme in names.keys() {
            assert!(
                doc.pointer("/components/securitySchemes")
                    .and_then(|schemes| schemes.get(scheme))
                    .is_some(),
                "{key}: `security` names `{scheme}`, which \
                 `components.securitySchemes` does not declare",
            );
        }
    }

    !requirements.is_empty()
}

/// Every component the server registers is declared here under the same
/// name.
///
/// `body_schemas_match` only reaches the components an operation *body*
/// references, which leaves every nested one — `ResourceRef`,
/// `AggregationOp`, the element type of each `Page` — free to drift. This
/// is what makes the `Dto`-suffix rule a rule instead of a comment.
///
/// Containment is deliberately one-way: the document also declares
/// documentary refinements (`Timestamp`, `UsageValue`, the two
/// `CreateUsageRecordResult` branches, …) that the runtime emits inline
/// and so has no component for.
#[test]
fn every_registered_component_is_documented() {
    let reg = registry();
    let doc = spec_doc();

    let documented = doc
        .pointer("/components/schemas")
        .and_then(Value::as_object)
        .expect("`components.schemas` must be a map");

    let components = reg.components_registry.load();
    assert!(
        !components.is_empty(),
        "the registry produced no component schemas; this check would be vacuous",
    );

    let missing: Vec<String> = components
        .keys()
        .map(|name| contract_name(name))
        .filter(|name| !documented.contains_key(name))
        .collect();

    assert!(
        missing.is_empty(),
        "the runtime document publishes components the contract does not declare: \
         {missing:?} (the contract name is the registered name minus any `Dto` \
         suffix)",
    );
}

/// Per-operation authentication posture matches `.authenticated()`.
///
/// The root-level block alone is not enough: an operation carrying
/// `security: []` is public in the published document no matter what the
/// root says, and the route registering it would still be
/// `.authenticated()`.
#[test]
fn security_matches_authenticated_routes() {
    let reg = registry();
    let doc = spec_doc();
    let registered = registry_ops(&reg);

    assert!(
        doc["security"]
            .as_array()
            .is_some_and(|schemes| !schemes.is_empty()),
        "the contract must declare a root-level `security` requirement",
    );

    for (key, op) in yaml_ops(&doc) {
        let spec = spec_for(&registered, &key);
        assert_eq!(
            yaml_operation_authenticated(&doc, &key, &op),
            spec.authenticated,
            "{key}: the operation's effective `security` and the route's \
             `.authenticated()` posture disagree",
        );
    }
}
