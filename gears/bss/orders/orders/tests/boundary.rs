//! The frozen S1-02 boundary fixture (`boundary-fixtures.json`) executed against the real Rust
//! adapters: every case kind is dispatched, and an unknown kind fails the test.
//!
//! - `etag` and `problem`: the REST `If-Match` parser and the Problem mapping.
//! - `boundary`: the fixture's abstract call metadata is carried the way REST carries it
//!   (`If-Match`, `Idempotency-Key`, `X-Delegation-Proof-Ref`, the body's
//!   `expected_draft_revision`), then each step runs through the gear's own boundary functions in
//!   the handlers' order: version, key, proof, draft revision, the handlers' body functions
//!   (envelope, REST DTO decoding, conversion into the SDK request type), `validate()`, and for
//!   `PATCH` the field classification that selects the trigger.
//! - `transition`: the compiled state table.
//! - `registry`: the mutation applied to the Rust registry builder that owns that rule.
//!
//! Cases whose rule has no Rust boundary or builder in S2 are named in an exact set below, so a
//! newly delivered decoder or a new fixture case shows up as a test change, never as a skip.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a corrupt frozen fixture must fail loudly; `allow`, not `expect`, because not every \
              test in the file unwraps or expects"
)]
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use bss_orders::api::rest;
use bss_orders::api::rest::capture;
use bss_orders::authz::{CensusError, operation_action};
use bss_orders::domain::capture::{select_header, select_line};
use bss_orders::domain::contributions::FieldClassification;
use bss_orders::domain::guards::{GUARDS, GuardDecl, GuardRegistry, GuardRegistryError};
use bss_orders::domain::state_table::{
    EVENT_ROWS, ROWS, RowId, StateTable, TableError, Target, Versioning,
};
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::catalog::{OPERATIONS, Operation, OrderState, Reason, Trigger};
use bss_orders_sdk::models::DraftRevision;
use bss_orders_sdk::reads::PageSize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn fixtures() -> Value {
    serde_json::from_str(include_str!(
        "../../orders-sdk/tests/fixtures/boundary-fixtures.json"
    ))
    .unwrap()
}
fn catalog_json() -> Value {
    serde_json::from_str(include_str!("../../orders-sdk/contracts/catalog.json")).unwrap()
}

/// `boundary` cases that reach a body rule with no Rust decoder in S2: the operation is a
/// registered catalog contract whose request type arrives with its package (`hold` and `resume`
/// S4, `record_acceptance` S5, the workflow seams `reflect_approval`, `begin_fulfillment`,
/// `report_spawn_signal` and `acknowledge` S5-08). Their header steps (version, key, proof) are
/// still executed; only the body step is not.
const UNSUPPORTED_BOUNDARY: &[&str] = &[
    "B09-key-exact-max",
    "B22-required-verdict",
    "B23-unknown-verdict",
    "B24-forbidden-denial-reason",
    "B25-missing-authority-is-later-guard",
    "B26-completed-ack",
    "B27-completed-forbids-failure",
    "B28-unknown-ack-outcome",
    "B29-payment-outcome",
    "B30-no-local-payment-tolerance",
    "B31-no-caller-acceptance-time",
    "B32-spawn-exact-roster",
    "B33-no-caller-grant",
    "B38-no-public-trigger",
    "B43-correlation-from-context",
];

/// `registry` mutations with no Rust builder: the operation list (`OPERATIONS`) and the reason
/// registry (`Reason`) are generated from `catalog.json` by `generate_catalog.py`, whose `--check`
/// and the SDK `contracts.rs` test own their uniqueness; no Rust code compiles a caller-supplied
/// list of either.
const UNSUPPORTED_REGISTRY: &[&str] = &["duplicate-operation", "duplicate-reason"];

/// What a `boundary` case produced.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The fixture token: `boundary-pass`, a reason token or the selected `PATCH` trigger.
    Token(String),
    /// The case reached a body rule with no Rust decoder.
    Unsupported,
}

fn token(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn refusal(error: OrdersError) -> Outcome {
    match error {
        OrdersError::Refused(reason) => Outcome::Token(reason.mapping().reason.to_owned()),
        other => panic!("boundary step produced a non-refusal {other:?}"),
    }
}

fn invalid() -> OrdersError {
    OrdersError::Refused(Reason::RequestInvalid)
}

/// The `If-Match` spelling of the fixture's abstract `expected_version`: a JSON integer is its
/// strong `ETag`; any other JSON value is sent as its literal text, which no `ETag` parser accepts.
fn if_match(value: Option<&Value>) -> Option<String> {
    value.map(|v| match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => format!("\"{n}\""),
        other => other.to_string(),
    })
}

/// The proof header values exactly as sent; non-ASCII text stays as raw bytes (obs-text), as a
/// client would put it on the wire.
fn proof_headers(call: &Value) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(values) = call["headers"]["x-delegation-proof-ref"].as_array() {
        for value in values {
            headers.append(
                HeaderName::from_static(rest::DELEGATION_PROOF_HEADER),
                HeaderValue::from_bytes(value.as_str().unwrap().as_bytes()).unwrap(),
            );
        }
    }
    headers
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, OrdersError> {
    serde_json::from_value(value).map_err(|_| invalid())
}

fn run_boundary(catalog: &Value, classes: &FieldClassification, call: &Value) -> Outcome {
    let operation = call["operation"].as_str().unwrap();
    let declared = catalog["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["id"] == operation)
        .unwrap_or_else(|| panic!("{operation} is not a catalog operation"));
    let meta = call["meta"].as_object().unwrap();
    let body = call["body"].as_object().unwrap();
    let wire = serde_json::to_vec(body).unwrap();
    let steps = || -> Result<Outcome, OrdersError> {
        // 1. If-Match (428 before anything else) where the operation requires it.
        if declared["expected_version"] == "required-before-authorization" {
            rest::expected_version(if_match(meta.get("expected_version")).as_deref())?;
        }
        // 2. Idempotency-Key.
        if declared["idempotency"] == "required" {
            rest::idempotency_key(meta.get("idempotency_key").and_then(Value::as_str))?;
        }
        // 3. X-Delegation-Proof-Ref (D-202).
        rest::delegation_proof(&proof_headers(call))?;
        // 4. The body's independent `expected_draft_revision` member.
        let revision = meta.get("expected_draft_revision");
        if let Some(value) = revision {
            decode::<DraftRevision>(value.clone())?;
        }
        // 5. The body into the operation's request type.
        let pass = || Ok(Outcome::Token("boundary-pass".to_owned()));
        match operation {
            "create" => {
                capture::create_body(&wire)?;
                pass()
            }
            "patch_order" => {
                let (_, patch) = capture::patch_order_body(&wire)?;
                patch.validate().map_err(|_| invalid())?;
                let selection = select_header(classes, &patch).ok_or_else(invalid)?;
                Ok(Outcome::Token(token(&selection.trigger)))
            }
            "patch_line" => {
                let (_, patch) = capture::patch_line_body(&wire)?;
                patch.validate().map_err(|_| invalid())?;
                let selection = select_line(classes, &patch).ok_or_else(invalid)?;
                Ok(Outcome::Token(token(&selection.trigger)))
            }
            "add_line" => {
                let (_, line) = capture::add_line_body(&wire)?;
                line.validate().map_err(|_| invalid())?;
                pass()
            }
            "remove_line" if body.is_empty() => pass(),
            "remove_line" => Err(invalid()),
            "submit" => {
                // REST carries the required revision inside the submit body.
                let mut submit = body.clone();
                if let Some(value) = revision {
                    submit.insert("expected_draft_revision".to_owned(), value.clone());
                }
                decode::<rest::SubmitBody>(Value::Object(submit))?;
                pass()
            }
            "list" => {
                for (name, value) in body {
                    assert_eq!(
                        name, "limit",
                        "list query member {name} is not modeled here"
                    );
                    value
                        .as_u64()
                        .and_then(|n| PageSize::try_from(n).ok())
                        .ok_or(OrdersError::Refused(Reason::PageSizeExceeded))?;
                }
                pass()
            }
            _ => Ok(Outcome::Unsupported),
        }
    };
    steps().unwrap_or_else(refusal)
}

fn state(value: &Value) -> Option<OrderState> {
    (!value.is_null()).then(|| serde_json::from_value(value.clone()).unwrap())
}

fn run_transition(table: &StateTable, case: &Value) -> Value {
    let from = state(&case["from"]);
    let trigger: Trigger = serde_json::from_value(case["trigger"].clone()).unwrap();
    let Some((_, id)) = table.expanded().find(|(key, _)| *key == (from, trigger)) else {
        return Value::Null;
    };
    let row = table.row(id).unwrap();
    let to = match row.target {
        Target::State(to) => token(&to),
        Target::Same => token(&from.unwrap()),
        Target::PreHold => "$pre_hold_state".to_owned(),
    };
    json!({
        "row": row.id.number(),
        "to": to,
        "event": row.event.map(|e| token(&e)),
        "versioning": match row.versioning {
            Versioning::Append => "append",
            Versioning::StateOnly => "none",
        },
    })
}

/// Apply a registry mutation to the Rust builder that owns the rule and return its refusal. The
/// fixture's expected text is the S1-02 Python oracle's; the Rust refusal is the typed error
/// named here for each mutation.
fn run_registry(mutation: &str, table: &StateTable) -> Option<String> {
    let with_guard = |mutant: GuardDecl| {
        let mut decls = GUARDS.to_vec();
        decls.push(mutant);
        GuardRegistry::compile(table, &decls).map(|_| ())
    };
    let mutant = GuardDecl {
        name: "fixture.mutant",
        owner: "fixture",
        rows: &[1],
        reasons: GUARDS[0].reasons,
        inputs: &[],
    };
    Some(match mutation {
        // The oracle clears the first operation's permission; `operation_action` is the census
        // step that maps an operation to its permission.
        "missing-permission" => {
            let op = Operation {
                action: "",
                ..OPERATIONS[0]
            };
            let error = operation_action(&op, None).unwrap_err();
            assert_eq!(error, CensusError::Undeclared);
            error.to_string()
        }
        // Triggers are registered by the state table rows; an invented one is an unknown token.
        "unknown-trigger" => {
            let mut rows = ROWS;
            rows[0].trigger = "invented";
            let error = StateTable::compile(&rows, &EVENT_ROWS).unwrap_err();
            assert!(
                matches!(error, TableError::UnknownToken(row, "invented") if row == RowId::of(1)),
                "{error}"
            );
            error.to_string()
        }
        // The oracle's row 999 does not fit a row number; 30 is the first row outside 1..=29.
        // The mutant keeps a guard-owned reason, since the oracle's `request-invalid` is
        // engine-owned and is refused before the row is examined.
        "unknown-guard-row" => {
            let error = with_guard(GuardDecl {
                rows: &[30],
                ..mutant
            })
            .unwrap_err();
            assert_eq!(error, GuardRegistryError::UnknownRow("fixture.mutant", 30));
            error.to_string()
        }
        "unknown-guard-reason" => {
            let error = with_guard(GuardDecl {
                reasons: &["invented"],
                ..mutant
            })
            .unwrap_err();
            assert_eq!(
                error,
                GuardRegistryError::UnknownReason("fixture.mutant", "invented")
            );
            error.to_string()
        }
        // Rows 18 and 19 are both amendment rows; giving row 19 row 18's from-states duplicates
        // the expanded (from-state, trigger) key.
        "duplicate-state-trigger" => {
            let mut rows = ROWS;
            rows[18].from = rows[17].from;
            let error = StateTable::compile(&rows, &EVENT_ROWS).unwrap_err();
            assert!(
                matches!(
                    error,
                    TableError::DuplicateKey { first, second, .. }
                        if first == RowId::of(18) && second == RowId::of(19)
                ),
                "{error}"
            );
            error.to_string()
        }
        _ => return None,
    })
}

#[test]
fn every_frozen_boundary_fixture_case_runs_against_the_rust_adapters() {
    let fixtures = fixtures();
    let catalog = catalog_json();
    let classes = FieldClassification::registered().unwrap();
    let table = StateTable::registered().unwrap();
    let mut executed = std::collections::BTreeMap::<&str, usize>::new();
    let mut unsupported_boundary = BTreeSet::new();
    let mut unsupported_registry = BTreeSet::new();
    let mut mismatches = Vec::new();
    for case in fixtures["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        match kind {
            "etag" => {
                let result = rest::expected_version(case["input"].as_str());
                if let Some(n) = case["expected"].as_i64() {
                    assert_eq!(i64::from(result.unwrap()), n, "{id}");
                } else {
                    assert_eq!(rest::problem(result.unwrap_err()).status, Some(428), "{id}");
                }
            }
            "problem" => {
                let reason: Reason = serde_json::from_value(case["reason"].clone()).unwrap();
                let p = serde_json::to_value(rest::problem(OrdersError::Refused(reason))).unwrap();
                for k in ["type", "title", "error_domain", "error_code"] {
                    assert_eq!(p[k], case["expected"][k], "{id}");
                }
                assert_eq!(p["status"], case["expected"]["http_status"], "{id}");
            }
            "boundary" => match run_boundary(&catalog, &classes, &case["call"]) {
                Outcome::Unsupported => {
                    unsupported_boundary.insert(id);
                    continue;
                }
                Outcome::Token(actual) => {
                    if actual != case["expected"].as_str().unwrap() {
                        mismatches
                            .push(format!("{id}: expected {}, got {actual}", case["expected"]));
                    }
                }
            },
            "transition" => {
                let actual = run_transition(&table, case);
                if actual != case["expected"] {
                    mismatches.push(format!("{id}: expected {}, got {actual}", case["expected"]));
                }
            }
            "registry" => {
                let mutation = case["mutation"].as_str().unwrap();
                if run_registry(mutation, &table).is_none() {
                    unsupported_registry.insert(mutation);
                    continue;
                }
            }
            other => panic!("{id}: unknown fixture case kind `{other}`"),
        }
        *executed.entry(kind).or_default() += 1;
    }
    assert!(
        mismatches.is_empty(),
        "fixture mismatches:\n{}",
        mismatches.join("\n")
    );
    assert_eq!(
        unsupported_boundary,
        UNSUPPORTED_BOUNDARY
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        "the boundary cases without a Rust decoder changed"
    );
    assert_eq!(
        unsupported_registry,
        UNSUPPORTED_REGISTRY
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        "the registry mutations without a Rust builder changed"
    );
    // Every executed kind is accounted for: 52 boundary (37 executed), 10 transition, 8 etag,
    // 7 registry (5 executed) and 6 problem cases.
    assert_eq!(
        executed,
        std::collections::BTreeMap::from([
            ("boundary", 37),
            ("etag", 8),
            ("problem", 6),
            ("registry", 5),
            ("transition", 10),
        ])
    );
    for golden in fixtures["golden_envelopes"].as_array().unwrap() {
        if golden["kind"] == "problem" {
            let reason = Reason::ALL
                .iter()
                .find(|r| r.mapping().code == golden["value"]["error_code"])
                .unwrap();
            assert_eq!(
                serde_json::to_value(rest::problem(OrdersError::Refused(*reason))).unwrap(),
                golden["value"]
            );
        }
    }
}

#[test]
fn submit_requires_independent_revision_and_rejects_context_fields() {
    for body in [
        r"{}",
        r#"{"expected_draft_revision":-1}"#,
        r#"{"expected_draft_revision":true}"#,
        r#"{"expected_draft_revision":0,"actor":"forged"}"#,
        r#"{"expected_draft_revision":0,"expected_version":1}"#,
    ] {
        assert!(serde_json::from_str::<rest::SubmitBody>(body).is_err());
    }
    assert!(serde_json::from_str::<rest::SubmitBody>(r#"{"expected_draft_revision":0}"#).is_ok());
    assert_eq!(
        rest::problem(rest::expected_version(None).unwrap_err()).status,
        Some(428)
    );
    assert!(rest::idempotency_key(None).is_err());
    assert!(rest::idempotency_key(Some("k")).is_ok());
}
