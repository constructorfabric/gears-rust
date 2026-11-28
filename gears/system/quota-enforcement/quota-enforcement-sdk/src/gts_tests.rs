#![allow(clippy::expect_used)]

use gts::{GTS_ID_PREFIX, GTS_ID_URI_PREFIX, GtsRefValidation, GtsSchema, GtsStore};
use serde_json::Value;

use super::{
    CONSTRAINT_BASE, LEASE_RESOURCE, METRIC_BASE_TYPE, OPERATION_RESOURCE, POLICY_RESOURCE,
    QUOTA_RESOURCE, QuotaEnforcementStoragePluginSpecV1, QuotaNotificationSinkSpecV1, REQUEST_BASE,
    RESOURCE_BASE, SCOPE_TENANT, SCOPE_TYPE, SCOPE_USER, SUBJECT_BASE,
};

/// The reviewed owner examples of ADR-0007: one complete `llm_gateway` set.
const EXAMPLES: [(&str, &str); 7] = [
    (
        "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~",
        include_str!(
            "../../docs/schemas/examples/gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~.schema.json"
        ),
    ),
    (
        "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.tenant.v1~",
        include_str!(
            "../../docs/schemas/examples/gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.tenant.v1~.schema.json"
        ),
    ),
    (
        "gts.cf.core.qe.request.v1~cf.genai.llm_gateway.token.v1~",
        include_str!(
            "../../docs/schemas/examples/gts.cf.core.qe.request.v1~cf.genai.llm_gateway.token.v1~.schema.json"
        ),
    ),
    (
        "gts.cf.core.qe.request.v1~cf.genai.llm_gateway.request_count.v1~",
        include_str!(
            "../../docs/schemas/examples/gts.cf.core.qe.request.v1~cf.genai.llm_gateway.request_count.v1~.schema.json"
        ),
    ),
    (
        "gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.token_constraint.v1~",
        include_str!(
            "../../docs/schemas/examples/gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.token_constraint.v1~.schema.json"
        ),
    ),
    (
        "gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.request_count_constraint.v1~",
        include_str!(
            "../../docs/schemas/examples/gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.request_count_constraint.v1~.schema.json"
        ),
    ),
    (
        "gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~",
        include_str!(
            "../../docs/schemas/examples/gts.cf.core.qe.res.v1~cf.genai.llm_gateway.model.v1~.schema.json"
        ),
    ),
];

fn parse(raw: &str) -> Value {
    serde_json::from_str(raw).expect("reviewed schema file is valid JSON")
}

/// One declaration of this crate's collector.
struct Definition {
    id: String,
    document: Value,
}

/// The projection-contract declarations of this crate's collector: the five
/// QE types and the two scope instances (the plugin specs excluded).
fn definitions() -> Vec<Definition> {
    crate::gts_declarations()
        .into_iter()
        .filter(|(id, _)| id.starts_with(&format!("{GTS_ID_PREFIX}cf.core.qe.")))
        .map(|(id, text)| Definition {
            id,
            document: parse(&text),
        })
        .collect()
}

#[test]
fn the_storage_plugin_spec_type_id_derives_from_the_toolkit_plugin_base() {
    let storage = QuotaEnforcementStoragePluginSpecV1::TYPE_ID;
    assert!(
        storage.starts_with("gts.cf.toolkit.plugins.plugin.v1~"),
        "{storage}"
    );
    assert!(storage.ends_with('~'));
}

#[test]
fn the_notification_sink_spec_type_id_derives_from_the_toolkit_plugin_base() {
    let sink = crate::QuotaNotificationSinkSpecV1::TYPE_ID;
    assert!(
        sink.starts_with("gts.cf.toolkit.plugins.plugin.v1~"),
        "{sink}"
    );
    assert!(
        sink.ends_with("~cf.core.qe.notification_sink.v1~"),
        "{sink}"
    );
    assert_ne!(sink, QuotaEnforcementStoragePluginSpecV1::TYPE_ID);
}

#[test]
fn resource_ids_are_distinct_five_segment_type_ids() {
    let all = [
        QUOTA_RESOURCE,
        POLICY_RESOURCE,
        LEASE_RESOURCE,
        OPERATION_RESOURCE,
    ];
    for id in all {
        assert!(id.starts_with("gts.cf.core.qe."), "{id}");
        assert!(id.ends_with(".v1~"), "{id}");
    }
    let mut sorted = all.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "resource ids must be unique");
}

/// The collector declares exactly the QE-owned definitions: the five types,
/// the two scope instances and the two plugin specs; never the metric base.
#[test]
fn the_collector_declares_the_nine_qe_definitions() {
    let declared: Vec<String> = crate::gts_declarations()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let mut expected = vec![
        SUBJECT_BASE,
        RESOURCE_BASE,
        REQUEST_BASE,
        CONSTRAINT_BASE,
        SCOPE_TYPE,
        SCOPE_USER,
        SCOPE_TENANT,
        QuotaEnforcementStoragePluginSpecV1::TYPE_ID,
        QuotaNotificationSinkSpecV1::TYPE_ID,
    ];
    expected.sort_unstable();
    assert_eq!(declared, expected);
    assert!(
        !declared.iter().any(|id| id == METRIC_BASE_TYPE),
        "the metric base is deployment-owned and never declared by QE"
    );
}

#[test]
fn every_embedded_document_carries_the_id_of_its_constant() {
    for def in definitions() {
        if def.id.ends_with('~') {
            let expected = format!("{GTS_ID_URI_PREFIX}{}", def.id);
            assert_eq!(def.document["$id"], expected, "{}", def.id);
            assert_eq!(
                def.document["$schema"], "http://json-schema.org/draft-07/schema#",
                "{} must declare the Draft-07 dialect",
                def.id
            );
        } else {
            assert_eq!(def.document["id"], def.id.as_str());
            assert_eq!(def.document["type"], SCOPE_TYPE);
        }
    }
}

#[test]
fn the_four_bases_are_abstract_and_the_scope_type_is_concrete() {
    for def in definitions().iter().filter(|d| d.id.ends_with('~')) {
        let is_abstract = def.document["x-gts-abstract"] == Value::Bool(true);
        assert_eq!(is_abstract, def.id != SCOPE_TYPE, "{}", def.id);
    }
}

#[test]
fn scope_instance_payloads_equal_the_documented_files() {
    let user = parse(include_str!(
        "../../docs/schemas/gts.cf.core.qe.scope.v1~cf.core.qe.user.v1.json"
    ));
    let tenant = parse(include_str!(
        "../../docs/schemas/gts.cf.core.qe.scope.v1~cf.core.qe.tenant.v1.json"
    ));
    let defs = definitions();
    let by_id = |id: &str| {
        defs.iter()
            .find(|d| d.id == id)
            .map(|d| d.document.clone())
            .expect("instance present")
    };
    assert_eq!(by_id(SCOPE_USER), user);
    assert_eq!(by_id(SCOPE_TENANT), tenant);
}

/// A store holding the QE definitions and the `llm_gateway` examples: the
/// repository's semantic check of the reviewed schema files. The registry
/// validates with the same `gts` library and pattern-only entity-reference policy
/// at admission time.
fn store_with_examples() -> GtsStore {
    let mut store = GtsStore::new();
    for def in definitions().iter().filter(|d| d.id.ends_with('~')) {
        store
            .register_schema(&def.id, &def.document)
            .expect("QE type schema registers");
    }
    for (id, raw) in EXAMPLES {
        store
            .register_schema(id, &parse(raw))
            .expect("example schema registers");
    }
    store
}

#[test]
fn the_qe_bases_validate_as_abstract_types_with_required_value_less_traits() {
    let mut store = store_with_examples();
    for base in [SUBJECT_BASE, RESOURCE_BASE, REQUEST_BASE, CONSTRAINT_BASE] {
        let resolved = store
            .validate_schema_with(base, GtsRefValidation::None)
            .unwrap_or_else(|e| panic!("{base}: {e}"));
        assert!(resolved.is_abstract, "{base} is abstract");
    }
    let scope = store
        .validate_schema_with(SCOPE_TYPE, GtsRefValidation::None)
        .expect("scope type validates");
    assert!(!scope.is_abstract, "the scope type is concrete");
}

#[test]
fn the_owner_examples_validate_as_concrete_derived_contracts_with_their_traits() {
    let mut store = store_with_examples();
    for (id, _) in EXAMPLES {
        let resolved = store
            .validate_schema_with(id, GtsRefValidation::None)
            .unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(!resolved.is_abstract, "{id} is concrete");
        assert!(
            resolved.schema["$schema"].is_string(),
            "{id}: the resolved schema keeps its dialect"
        );
    }
    let user = store
        .validate_schema_with(
            "gts.cf.core.qe.subj.v1~cf.genai.llm_gateway.user.v1~",
            GtsRefValidation::None,
        )
        .expect("user projection");
    assert_eq!(user.effective_traits["scope"], SCOPE_USER);
    assert_eq!(
        user.effective_traits["admitted_metrics"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    let token = store
        .validate_schema_with(
            "gts.cf.core.qe.request.v1~cf.genai.llm_gateway.token.v1~",
            GtsRefValidation::None,
        )
        .expect("token request contract");
    assert_eq!(
        token.effective_traits["metric"],
        format!("{METRIC_BASE_TYPE}cf.genai.llm_gateway.ai_tokens_input.v1")
    );
    assert_eq!(
        token.effective_traits["constraint_contract"],
        "gts.cf.core.qe.constraint.v1~cf.genai.llm_gateway.token_constraint.v1~"
    );
}

#[test]
fn scope_instances_validate_against_the_scope_type_and_bases_take_no_instances() {
    let mut store = store_with_examples();
    for def in definitions().iter().filter(|d| !d.id.ends_with('~')) {
        store
            .validate_payload_with(SCOPE_TYPE, &def.document, GtsRefValidation::None)
            .unwrap_or_else(|e| panic!("{}: {e}", def.id));
    }
    let err = store
        .validate_payload_with(
            SUBJECT_BASE,
            &serde_json::json!({ "type": SUBJECT_BASE }),
            GtsRefValidation::None,
        )
        .expect_err("an abstract base takes no direct instances");
    assert!(err.to_string().contains("abstract"), "{err}");
}

#[test]
fn the_token_request_envelope_validates_against_its_resolved_contract() {
    let mut store = store_with_examples();
    let contract = "gts.cf.core.qe.request.v1~cf.genai.llm_gateway.token.v1~";
    store
        .validate_payload_with(
            contract,
            &serde_json::json!({ "type": contract, "metadata": { "region": "eu-west-1" } }),
            GtsRefValidation::None,
        )
        .expect("a conforming envelope validates");
    let missing_region = store
        .validate_payload_with(
            contract,
            &serde_json::json!({ "type": contract, "metadata": {} }),
            GtsRefValidation::None,
        )
        .expect_err("region is required by the owner contract");
    assert!(
        missing_region.to_string().contains("region"),
        "{missing_region}"
    );
    let bare_metadata = store
        .validate_payload_with(
            contract,
            &serde_json::json!({ "region": "eu-west-1" }),
            GtsRefValidation::None,
        )
        .expect_err("the inner object alone is not the contract envelope");
    assert!(
        bare_metadata.to_string().contains("type"),
        "{bare_metadata}"
    );
}

/// The reviewed contract in `docs/schemas/` stays the reference: every
/// generated base schema means the same as its reviewed document. Prose
/// (`description`, `title`) is not compared, and a property's single-member
/// `allOf` wrapper is unwrapped; everything else, including dialect, closure,
/// abstractness, the traits schema and every GTS reference, must match.
/// `additionalProperties: true` equals its absence.
#[test]
fn the_declared_base_schemas_match_the_reviewed_documents() {
    const REVIEWED: [(&str, &str); 5] = [
        (
            SUBJECT_BASE,
            include_str!("../../docs/schemas/gts.cf.core.qe.subj.v1~.schema.json"),
        ),
        (
            RESOURCE_BASE,
            include_str!("../../docs/schemas/gts.cf.core.qe.res.v1~.schema.json"),
        ),
        (
            REQUEST_BASE,
            include_str!("../../docs/schemas/gts.cf.core.qe.request.v1~.schema.json"),
        ),
        (
            CONSTRAINT_BASE,
            include_str!("../../docs/schemas/gts.cf.core.qe.constraint.v1~.schema.json"),
        ),
        (
            SCOPE_TYPE,
            include_str!("../../docs/schemas/gts.cf.core.qe.scope.v1~.schema.json"),
        ),
    ];
    let defs = definitions();
    for (id, reviewed) in REVIEWED {
        let declared = &defs
            .iter()
            .find(|d| d.id == id)
            .expect("declared base")
            .document;
        assert_eq!(
            semantic(declared.clone()),
            semantic(parse(reviewed)),
            "{id} drifted from docs/schemas"
        );
    }
}

/// `schema` without prose, with single-member `allOf` wrappers unwrapped and
/// `additionalProperties: true` dropped (the JSON Schema default).
fn semantic(schema: Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                if key == "description" || key == "title" {
                    continue;
                }
                if key == "additionalProperties" && value == Value::Bool(true) {
                    continue;
                }
                if key == "allOf"
                    && let Value::Array(items) = &value
                    && let [only] = items.as_slice()
                    && let Value::Object(inner) = semantic(only.clone())
                {
                    out.extend(inner);
                    continue;
                }
                out.insert(key, semantic(value));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(semantic).collect()),
        other => other,
    }
}

/// The trait values are typed GTS ids: a malformed one does not deserialize,
/// and a well-formed one round-trips as the published string.
#[test]
fn trait_values_are_typed_gts_ids() {
    use crate::{RequestTraitsV1, SubjectTraitsV1};

    let subject = serde_json::json!({
        "scope": crate::SCOPE_USER,
        "admitted_metrics": ["gts.cf.core.qe.metric_type.v1~cf.test.qe.tokens.v1"],
    });
    let parsed: SubjectTraitsV1 = serde_json::from_value(subject.clone()).expect("well-formed");
    assert_eq!(serde_json::to_value(&parsed).expect("json"), subject);
    for (field, bad) in [
        ("scope", serde_json::json!("not-an-id")),
        ("admitted_metrics", serde_json::json!(["nope"])),
    ] {
        let mut malformed = subject.clone();
        malformed[field] = bad;
        assert!(
            serde_json::from_value::<SubjectTraitsV1>(malformed).is_err(),
            "{field}"
        );
    }

    let request = serde_json::json!({
        "metric": "gts.cf.core.qe.metric_type.v1~cf.test.qe.tokens.v1",
        "constraint_contract": crate::CONSTRAINT_BASE,
    });
    let parsed: RequestTraitsV1 = serde_json::from_value(request.clone()).expect("well-formed");
    assert_eq!(serde_json::to_value(&parsed).expect("json"), request);
    for field in ["metric", "constraint_contract"] {
        let mut malformed = request.clone();
        malformed[field] = serde_json::json!("not-an-id");
        assert!(
            serde_json::from_value::<RequestTraitsV1>(malformed).is_err(),
            "{field}"
        );
    }
}
