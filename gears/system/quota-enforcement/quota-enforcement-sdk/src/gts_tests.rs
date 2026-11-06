#![allow(clippy::expect_used)]

use gts::{GtsRefValidation, GtsStore};
use serde_json::Value;

use super::{
    CONSTRAINT_BASE, METRIC_BASE_TYPE, REQUEST_BASE, RESOURCE_BASE, SCOPE_TYPE, SCOPE_USER,
    SUBJECT_BASE, owned_definitions,
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

/// A store holding the QE definitions and the `llm_gateway` examples: the
/// repository's semantic check of the reviewed schema files. The registry
/// validates with the same `gts` library and pattern-only entity-reference policy
/// at admission time.
fn store_with_examples() -> GtsStore {
    let mut store = GtsStore::new();
    for def in owned_definitions().iter().filter(|d| d.id.ends_with('~')) {
        store
            .register_schema(def.id, &def.document)
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
    for def in owned_definitions().iter().filter(|d| !d.id.ends_with('~')) {
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

#[test]
fn a_concrete_derived_type_leaving_a_required_trait_unresolved_is_rejected() {
    let mut store = store_with_examples();
    let id = "gts.cf.core.qe.request.v1~cf.qetest.llm.missing_trait.v1~";
    // Resolves `metric` but not `constraint_contract`, which the base requires.
    let derived = serde_json::json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-gts-traits": {
            "metric": format!("{METRIC_BASE_TYPE}cf.qetest.metric.other.v1")
        },
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{REQUEST_BASE}") }]
    });
    store
        .register_schema(id, &derived)
        .expect("the derived schema registers");
    let err = store
        .validate_schema_with(id, GtsRefValidation::None)
        .expect_err("an unresolved required trait must be rejected");
    assert!(err.to_string().contains("constraint_contract"), "{err}");
}
