#![allow(clippy::expect_used)]

use gts::{
    GtsTypeId, ResolvedType, SELF_ID_REF, X_GTS_INSTANCE_REF, X_GTS_PATTERN_REF, X_GTS_TYPE_REF,
};
use quota_enforcement_sdk::{METRIC_BASE_TYPE, SCOPE_USER};
use serde_json::{Value, json};

use super::CompiledContract;
use crate::test_support::{
    DRAFT7, LLM_TOKEN_REQUEST, METRIC_TOKENS, MIXIN_REQUEST, TYPED_MODEL_BASE, TYPED_REQUEST,
    llm_gateway_documents, mixin_request_document, mixin_type_document, resolve_documents,
    typed_request_document,
};

const MODEL_GPT: &str = "gts.cf.qetest.models.model.v1~cf.qetest.models.gpt.v1";
const MODEL_CLAUDE: &str = "gts.cf.qetest.models.model.v1~cf.qetest.models.claude.v1";

fn compiled(id: &str, documents: &[serde_json::Value]) -> CompiledContract {
    let registered = resolve_documents(documents)
        .into_iter()
        .find(|r| r.id.as_ref() == id)
        .expect("fixture resolves");
    CompiledContract::new(registered.payload.expect("compiles"), registered.schema)
}

/// Compiles `schema` as the resolved body of the concrete type `id`.
fn compile(id: &str, schema: Value) -> Result<CompiledContract, String> {
    let resolved = ResolvedType {
        id: GtsTypeId::new(id),
        is_abstract: false,
        is_final: false,
        schema,
        effective_traits: Value::Null,
        effective_traits_schema: Value::Null,
    };
    resolved
        .compile_payload_validator()
        .map(|validator| CompiledContract::new(validator, resolved.schema))
        .map_err(|e| e.to_string())
}

#[test]
fn the_token_request_contract_validates_the_envelope_not_the_inner_object() {
    let contract = compiled(LLM_TOKEN_REQUEST, &llm_gateway_documents());
    assert_eq!(contract.type_id().as_ref(), LLM_TOKEN_REQUEST);
    contract
        .validate(&json!({ "type": LLM_TOKEN_REQUEST, "metadata": { "region": "eu-west-1" } }))
        .expect("a conforming envelope");
    let missing = contract
        .validate(&json!({ "type": LLM_TOKEN_REQUEST, "metadata": {} }))
        .expect_err("region is required");
    assert!(missing.iter().any(|m| m.contains("region")), "{missing:?}");
    let bare = contract
        .validate(&json!({ "region": "eu-west-1" }))
        .expect_err("the inner object alone misses the base's required keys");
    assert!(!bare.is_empty());
    let extra = contract
        .validate(&json!({ "type": LLM_TOKEN_REQUEST, "metadata": { "region": "eu", "x": 1 } }))
        .expect_err("the owner closed the metadata object");
    assert!(!extra.is_empty());
}

#[test]
fn entity_refs_and_formats_are_enforced_on_metadata_values() {
    let contract = compiled(TYPED_REQUEST, &[typed_request_document(METRIC_TOKENS)]);
    let envelope = |model: &str| json!({ "type": TYPED_REQUEST, "metadata": { "model": model } });

    contract
        .validate(&envelope(
            "gts.cf.qetest.models.model.v1~cf.qetest.models.gpt.v1",
        ))
        .expect("an instance under the declared base");
    let malformed = contract
        .validate(&envelope("not-an-id"))
        .expect_err("x-gts-instance-ref rejects a value that is no GTS id");
    assert!(!malformed.is_empty());
    let a_type = contract
        .validate(&envelope(TYPED_MODEL_BASE))
        .expect_err("x-gts-instance-ref rejects a type id");
    assert!(!a_type.is_empty());
    let other_base = contract
        .validate(&envelope(METRIC_TOKENS))
        .expect_err("a well-formed id under another base fails x-gts-instance-ref");
    assert!(
        other_base
            .iter()
            .any(|m| m.contains("gts.cf.qetest.models.model.v1~")),
        "{other_base:?}"
    );
    let bad_time = contract
        .validate(&json!({
            "type": TYPED_REQUEST,
            "metadata": { "model": "gts.cf.qetest.models.model.v1~cf.qetest.models.gpt.v1", "when": "yesterday" }
        }))
        .expect_err("date-time is asserted under Draft-07");
    assert!(!bad_time.is_empty());
}

#[test]
fn local_and_mixin_references_apply_after_resolution() {
    let contract = compiled(
        MIXIN_REQUEST,
        &[mixin_type_document(), mixin_request_document(METRIC_TOKENS)],
    );
    contract
        .validate(&json!({
            "type": MIXIN_REQUEST,
            "metadata": { "region": "eu", "when": "2026-09-08T10:00:00Z" }
        }))
        .expect("conforming");
    assert!(
        contract
            .validate(&json!({ "type": MIXIN_REQUEST, "metadata": { "region": "mars" } }))
            .is_err(),
        "the mixin's enum applies"
    );
    assert!(
        contract
            .validate(&json!({ "type": MIXIN_REQUEST, "metadata": { "region": "eu", "when": 3 } }))
            .is_err(),
        "the local definition applies"
    );
}

#[test]
fn a_schema_that_does_not_compile_is_reported() {
    let err = compile(
        LLM_TOKEN_REQUEST,
        json!({ "$schema": crate::test_support::DRAFT7, "type": "object", "properties": 5 }),
    )
    .expect_err("properties must be an object");
    assert!(!err.is_empty());
}

/// A one-field contract whose `value` carries the schema under test.
fn contract_over(value_schema: &Value) -> CompiledContract {
    compile(
        TYPED_REQUEST,
        json!({
            "$schema": DRAFT7,
            "type": "object",
            "properties": { "value": value_schema },
            "required": ["value"],
        }),
    )
    .expect("compiles")
}

fn check(contract: &CompiledContract, value: &Value) -> Result<(), Vec<String>> {
    contract.validate(&json!({ "value": value }))
}

#[test]
fn one_of_branches_without_gts_constraints_are_judged_by_their_own_schema() {
    let contract =
        contract_over(&json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] }));
    check(&contract, &json!("text")).expect("only the string branch matches");
    check(&contract, &json!(7)).expect("only the integer branch matches");
    check(&contract, &json!(true)).expect_err("no branch matches");
}

#[test]
fn one_of_branches_are_told_apart_by_their_entity_ref() {
    let contract = contract_over(&json!({ "oneOf": [
        { "type": "string", X_GTS_INSTANCE_REF: TYPED_MODEL_BASE },
        { "type": "string", X_GTS_INSTANCE_REF: METRIC_BASE_TYPE },
    ] }));
    check(&contract, &json!(MODEL_GPT)).expect("exactly the model branch");
    check(&contract, &json!(METRIC_TOKENS)).expect("exactly the metric branch");
    check(&contract, &json!(SCOPE_USER)).expect_err("an instance under neither base");
    check(&contract, &json!("plain")).expect_err("not a GTS id");
}

#[test]
fn one_of_branches_are_told_apart_by_the_reference_kind() {
    let contract = contract_over(&json!({ "oneOf": [
        { "type": "string", X_GTS_TYPE_REF: TYPED_MODEL_BASE },
        { "type": "string", X_GTS_INSTANCE_REF: TYPED_MODEL_BASE },
    ] }));
    check(&contract, &json!(TYPED_MODEL_BASE)).expect("exactly the type branch");
    check(&contract, &json!(MODEL_GPT)).expect("exactly the instance branch");
}

#[test]
fn each_keyword_admits_only_its_kind() {
    let types = contract_over(&json!({ "type": "string", X_GTS_TYPE_REF: "gts.cf.*" }));
    check(&types, &json!(TYPED_MODEL_BASE)).expect("a type id");
    let wrong = check(&types, &json!(MODEL_GPT)).expect_err("an instance id");
    assert!(
        wrong.iter().any(|m| m.contains(X_GTS_TYPE_REF)),
        "{wrong:?}"
    );

    let instances = contract_over(&json!({ "type": "string", X_GTS_INSTANCE_REF: "gts.cf.*" }));
    check(&instances, &json!(MODEL_GPT)).expect("an instance id");
    let wrong = check(&instances, &json!(TYPED_MODEL_BASE)).expect_err("a type id");
    assert!(
        wrong.iter().any(|m| m.contains(X_GTS_INSTANCE_REF)),
        "{wrong:?}"
    );
}

#[test]
fn an_instance_ref_may_name_one_concrete_instance() {
    let contract = contract_over(&json!({ "type": "string", X_GTS_INSTANCE_REF: MODEL_GPT }));
    check(&contract, &json!(MODEL_GPT)).expect("the named instance");
    check(&contract, &json!(MODEL_CLAUDE)).expect_err("a sibling instance");
}

#[test]
fn any_of_falls_back_to_a_branch_without_gts_constraints() {
    let contract = contract_over(&json!({ "anyOf": [
        { "type": "string", X_GTS_INSTANCE_REF: TYPED_MODEL_BASE },
        { "type": "string", "maxLength": 5 },
    ] }));
    check(&contract, &json!(MODEL_GPT)).expect("the GTS branch");
    check(&contract, &json!("short")).expect("the plain branch");
    check(&contract, &json!(METRIC_TOKENS)).expect_err("wrong base and too long");
}

#[test]
fn all_of_combines_an_entity_ref_with_ordinary_keywords() {
    let contract = contract_over(&json!({ "allOf": [
        { "type": "string", X_GTS_INSTANCE_REF: TYPED_MODEL_BASE },
        { "pattern": "gpt" },
    ] }));
    check(&contract, &json!(MODEL_GPT)).expect("both hold");
    check(&contract, &json!(MODEL_CLAUDE)).expect_err("the pattern fails");
    // GTS reference violations are reported once every other constraint holds.
    let gpt_metric = format!("{METRIC_BASE_TYPE}cf.qetest.models.gpt.v1");
    let wrong_base = check(&contract, &json!(gpt_metric)).expect_err("the reference fails");
    assert!(
        wrong_base.iter().any(|m| m.contains(TYPED_MODEL_BASE)),
        "{wrong_base:?}"
    );
}

#[test]
fn the_self_id_operand_resolves_to_the_validated_type() {
    let contract = compile(
        TYPED_MODEL_BASE,
        json!({
            "$schema": DRAFT7,
            "$id": format!("gts://{TYPED_MODEL_BASE}"),
            "type": "object",
            "properties": {
                "value": { "type": "string", X_GTS_INSTANCE_REF: SELF_ID_REF },
                "kind": { "type": "string", X_GTS_TYPE_REF: SELF_ID_REF },
            },
        }),
    )
    .expect("compiles");
    check(&contract, &json!(MODEL_GPT)).expect("under the validated type");
    check(&contract, &json!(METRIC_TOKENS)).expect_err("another base");
    contract
        .validate(&json!({ "kind": TYPED_MODEL_BASE }))
        .expect("the validated type itself");
    contract
        .validate(&json!({ "kind": MODEL_GPT }))
        .expect_err("an instance is not a type");
}

#[test]
fn a_pattern_ref_admits_only_the_patterns_its_operand_covers() {
    let contract = contract_over(
        &json!({ "type": "string", X_GTS_PATTERN_REF: format!("{TYPED_MODEL_BASE}*") }),
    );
    check(&contract, &json!(MODEL_GPT)).expect("a concrete id under the operand");
    check(&contract, &json!(format!("{TYPED_MODEL_BASE}cf.qetest.*"))).expect("a narrower pattern");
    check(&contract, &json!("gts.cf.*")).expect_err("a wider pattern");
    check(&contract, &json!("plain")).expect_err("not a GTS pattern");
}

#[test]
fn an_entity_ref_constrains_strings_and_leaves_other_values_to_type() {
    let untyped = contract_over(&json!({ X_GTS_INSTANCE_REF: TYPED_MODEL_BASE }));
    check(&untyped, &json!(3)).expect("no string to match, no type keyword to fail");
    check(&untyped, &json!(METRIC_BASE_TYPE)).expect_err("a string must match");
    let typed = contract_over(&json!({ "type": "string", X_GTS_INSTANCE_REF: TYPED_MODEL_BASE }));
    check(&typed, &json!(3)).expect_err("type rejects the integer");
}

#[test]
fn a_malformed_entity_ref_fails_compilation() {
    let bad_operands = [
        json!(5),
        json!("not.a.pattern"),
        json!("/nowhere"),
        json!("gts.cf.*.qe.*"),
    ];
    for keyword in [X_GTS_TYPE_REF, X_GTS_INSTANCE_REF] {
        for bad in &bad_operands {
            let err = compile(
                TYPED_REQUEST,
                json!({
                    "$schema": DRAFT7,
                    "type": "object",
                    "properties": { "value": { keyword: bad } },
                }),
            )
            .expect_err("the constraint is rejected at compile time");
            assert!(err.contains(keyword), "{keyword} {bad}: {err}");
        }
    }
}

#[test]
fn a_type_ref_naming_an_instance_fails_compilation() {
    let err = compile(
        TYPED_REQUEST,
        json!({
            "$schema": DRAFT7,
            "type": "object",
            "properties": { "value": { X_GTS_TYPE_REF: MODEL_GPT } },
        }),
    )
    .expect_err("an instance id selects no types");
    assert!(err.contains(X_GTS_TYPE_REF), "{err}");
}
