//! The REST authoring bodies decode through the API DTOs (`api::rest::dto`) and convert into the
//! SDK request types (TOOLKIT-CORE-001). This pins that move to the wire it replaced: for a corpus
//! of bodies per operation, the gear's body functions (`api::rest::capture::*_body`) and the
//! previous pipeline (the same envelope steps, then the SDK type's own `Deserialize`) either both
//! accept with the identical SDK value and draft revision, or both refuse `request-invalid`.
//!
//! The corpus is the frozen boundary fixture's bodies for the four operations plus generated
//! edge cases: every top-level and nested member replaced by every value of a shared pool,
//! removed, set to `null` and joined by an unknown member.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a broken corpus must fail loudly; `allow`, not `expect`, because not every \
              test in the file unwraps or expects"
)]
use bss_orders::api::rest::capture;
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::{AddLine, Category, CreateOrder, HeaderPatch, LinePatch};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::models::DraftRevision;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

// The previous request pipeline, verbatim apart from the error type: the envelope steps are
// unchanged by the move, and the last step decoded the SDK type directly.

fn old_object(body: &[u8]) -> Result<Map<String, Value>, ()> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(()),
    }
}

fn old_revision(map: &mut Map<String, Value>) -> Result<Option<DraftRevision>, ()> {
    match map.remove("expected_draft_revision") {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .and_then(|v| DraftRevision::try_from(v).ok())
            .map(Some)
            .ok_or(()),
        Some(_) => Err(()),
    }
}

fn old_fields(mut map: Map<String, Value>) -> Result<Value, ()> {
    let fields = map.remove("fields").ok_or(())?;
    if !map.is_empty() {
        return Err(());
    }
    match &fields {
        Value::Object(named) if !named.is_empty() => Ok(fields),
        _ => Err(()),
    }
}

fn old_decode<T: DeserializeOwned>(value: Value) -> Result<T, ()> {
    old_nested_members(&value)?;
    serde_json::from_value(value).map_err(|_| ())
}

/// The previous SDK `SelectedItem` and `AuthoredTerm` refused unknown members
/// (`deny_unknown_fields`). Both now tolerate them because they are part of the stored
/// `OrderView` replayed across builds, so the frozen pipeline keeps that rule here: an unknown
/// member in a selected item or in an authored term is refused, as it always was on the wire.
fn old_nested_members(value: &Value) -> Result<(), ()> {
    let only = |object: &Map<String, Value>, known: &[&str]| {
        if object.keys().all(|key| known.contains(&key.as_str())) {
            Ok(())
        } else {
            Err(())
        }
    };
    if let Some(Value::Array(items)) = value.get("selected_items") {
        for item in items {
            if let Value::Object(item) = item {
                only(item, &["item_id", "quantity", "selected_dim_value"])?;
            }
        }
    }
    if let Some(Value::Object(term)) = value.get("term_duration") {
        // serde never applied `deny_unknown_fields` to the unit variant of an internally tagged
        // enum, so `{"kind": "rolling", ...}` with extra members was always accepted (and still
        // is by `OrdersAuthoredTerm`).
        let known: &[&str] = match term.get("kind").and_then(Value::as_str) {
            Some("periods") => &["kind", "count"],
            Some("calendar") => &["kind", "years", "months", "days", "microseconds"],
            _ => return Ok(()),
        };
        only(term, known)?;
    }
    Ok(())
}

fn old_create(body: &[u8]) -> Result<CreateOrder, ()> {
    old_decode(Value::Object(old_object(body)?))
}

fn old_patch_order(body: &[u8]) -> Result<(Option<DraftRevision>, HeaderPatch), ()> {
    let mut map = old_object(body)?;
    let revision = old_revision(&mut map)?;
    Ok((revision, old_decode(old_fields(map)?)?))
}

fn old_add_line(body: &[u8]) -> Result<(Option<DraftRevision>, AddLine), ()> {
    let mut map = old_object(body)?;
    let revision = old_revision(&mut map)?;
    Ok((revision, old_decode(Value::Object(map))?))
}

fn old_patch_line(body: &[u8]) -> Result<(Option<DraftRevision>, LinePatch), ()> {
    let mut map = old_object(body)?;
    let revision = old_revision(&mut map)?;
    Ok((revision, old_decode(old_fields(map)?)?))
}

/// Accepted and refused counts of one operation's corpus.
#[derive(Debug, Default)]
struct Tally {
    accepted: usize,
    refused: usize,
}

/// Both paths accept with the same value, or both refuse; the new refusal is `request-invalid`.
fn same<T: PartialEq + std::fmt::Debug>(
    tally: &mut Tally,
    body: &[u8],
    old: Result<T, ()>,
    new: Result<T, OrdersError>,
) {
    let shown = String::from_utf8_lossy(body);
    match (old, new) {
        (Ok(old), Ok(new)) => {
            assert_eq!(old, new, "different SDK value for {shown}");
            tally.accepted += 1;
        }
        (Err(()), Err(OrdersError::Refused(Reason::RequestInvalid))) => tally.refused += 1,
        (old, new) => panic!("behaviour differs for {shown}: old {old:?}, new {new:?}"),
    }
}

const UUID: &str = "11111111-1111-4111-8111-111111111111";

/// Values every member is tried with: each JSON type, and every scalar spelling the wire rules
/// care about, valid and invalid.
fn pool() -> Vec<Value> {
    let mut values = vec![
        json!(null),
        json!(true),
        json!(0),
        json!(1),
        json!(-1),
        json!(2.5),
        json!(4_294_967_296_u64),
        json!(""),
        json!("x"),
        json!({}),
        json!([]),
        json!(UUID),
        json!("11111111111141118111111111111111"),
        json!("urn:uuid:11111111-1111-4111-8111-111111111111"),
        json!("{11111111-1111-4111-8111-111111111111}"),
        json!(Category::NEW_SALE),
        json!(Category::CHANGE),
        json!("gts.cf.bss.orders.category.v1~cf.bss.orders.other.v1"),
        json!("EUR"),
        json!("USD"),
        json!("eur"),
        json!("EURO"),
        json!("EU"),
        json!("2026-11-01"),
        json!("0000-01-01"),
        json!("9999-12-31"),
        json!("10000-01-01"),
        json!("-0001-01-01"),
        json!("+2026-11-01"),
        json!("2026-1-1"),
        json!("2026-02-30"),
        json!("2026-11-01T00:00:00Z"),
        json!(" 2026-11-01"),
        json!("2.5"),
        json!("2.50"),
        json!("02.5"),
        json!("1"),
        json!("1.0"),
        json!("0"),
        json!("0.0"),
        json!("-1"),
        json!("1e2"),
        json!(".5"),
        json!("0.0000000000000000000000000001"),
        json!("99999999999999999999999999999"),
        json!("month"),
        json!("year"),
        json!("Month"),
        json!("week"),
        json!({"month": null}),
        json!({"year": null}),
        json!({"month": 1}),
        json!({"month": null, "year": null}),
        json!(["month"]),
        json!("eu-west"),
        json!("a\u{0}b"),
        json!("\u{e9}".repeat(201)),
        json!("x".repeat(2001)),
    ];
    values.extend(terms());
    values.extend([
        json!([item()]),
        json!([item(), item()]),
        json!([[UUID]]),
        json!([[UUID, "2", "eu"]]),
        json!([{"item_id": UUID, "quantity": "0"}]),
        json!([{"item_id": UUID, "extra": 1}]),
        json!([null]),
        item(),
    ]);
    values
}

/// Authored terms: every kind, valid and invalid, plus the sequence forms a derived internally
/// tagged enum accepts.
fn terms() -> Vec<Value> {
    vec![
        json!({"kind": "rolling"}),
        json!({"kind": "rolling", "count": 1}),
        json!({"kind": "periods", "count": 12}),
        json!({"kind": "periods", "count": 0}),
        json!({"kind": "periods", "count": 178_956_971}),
        json!({"kind": "periods", "count": 178_956_970}),
        json!({"kind": "periods", "count": "12"}),
        json!({"kind": "periods", "count": 1.0}),
        json!({"kind": "periods", "count": -1}),
        json!({"kind": "periods"}),
        json!({"kind": "periods", "count": 1, "years": 1}),
        json!({"kind": "calendar", "years": 1, "months": 0, "days": 0, "microseconds": 0}),
        json!({"kind": "calendar", "years": 0, "months": 0, "days": 0, "microseconds": 0}),
        json!({"kind": "calendar", "years": 0, "months": 0, "days": 0, "microseconds": 1}),
        json!({"kind": "calendar", "years": 0, "months": 0, "days": 4_294_967_295_u32, "microseconds": 0}),
        json!({"kind": "calendar", "years": 4_294_967_295_u32, "months": 0, "days": 0, "microseconds": 0}),
        json!({"kind": "calendar", "years": 0, "months": 0, "days": 0, "microseconds": u64::MAX}),
        json!({"kind": "calendar", "years": 1, "months": 0, "days": 0}),
        json!({"kind": "calendar", "years": 1, "months": 0, "days": 0, "microseconds": 0, "x": 0}),
        json!({"kind": "Rolling"}),
        json!({"kind": "fixed"}),
        json!({"kind": null}),
        json!({"count": 3}),
        json!(["rolling"]),
        json!(["periods", 3]),
        json!(["calendar", 1, 0, 0, 0]),
        json!({"rolling": null}),
        json!("rolling"),
    ]
}

fn item() -> Value {
    json!({"item_id": UUID, "quantity": "2.5", "selected_dim_value": "eu-west"})
}

/// Bodies derived from `base` (members of `target`, an object inside it found by `path`): each
/// member replaced by every pool value, removed, and an unknown member added.
fn mutations(base: &Value, path: &[&str], pool: &[Value]) -> Vec<Value> {
    let target = |body: &mut Value| -> Option<Map<String, Value>> {
        let mut at = &mut *body;
        for step in path {
            at = match step.parse::<usize>() {
                Ok(index) => at.get_mut(index)?,
                Err(_) => at.get_mut(*step)?,
            };
        }
        at.as_object().cloned()
    };
    let put = |body: &Value, members: Map<String, Value>| {
        let mut body = body.clone();
        let mut at = &mut body;
        for step in path {
            at = match step.parse::<usize>() {
                Ok(index) => &mut at[index],
                Err(_) => &mut at[*step],
            };
        }
        *at = Value::Object(members);
        body
    };
    let members = target(&mut base.clone()).expect("path names an object");
    let mut out = Vec::new();
    for name in members.keys() {
        for value in pool {
            let mut changed = members.clone();
            changed.insert(name.clone(), value.clone());
            out.push(put(base, changed));
        }
        let mut removed = members.clone();
        removed.remove(name);
        out.push(put(base, removed));
    }
    let mut unknown = members;
    unknown.insert("unknown_member".to_owned(), json!(1));
    out.push(put(base, unknown));
    out
}

fn fixture_bodies(operation: &str) -> Vec<Value> {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../orders-sdk/tests/fixtures/boundary-fixtures.json"
    ))
    .unwrap();
    fixtures["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["kind"] == "boundary" && case["call"]["operation"] == operation)
        .map(|case| case["call"]["body"].clone())
        .collect()
}

/// Raw bodies the envelope step refuses or reads before any member is decoded.
fn raw_bodies() -> Vec<Vec<u8>> {
    [
        "",
        "null",
        "[]",
        "1",
        "\"x\"",
        "{",
        "{}",
        "{\"fields\": {}}",
        "{\"fields\": []}",
        "{\"fields\": null}",
        "{\"fields\": [\"x\"]}",
        "{\"fields\": {\"display_label\": \"a\"}, \"display_label\": \"b\"}",
        "{\"fields\": {\"display_label\": \"a\"}, \"fields\": {\"display_label\": \"b\"}}",
        "{\"fields\": {\"display_label\": \"a\", \"display_label\": null}}",
    ]
    .iter()
    .map(|body| body.as_bytes().to_vec())
    .collect()
}

/// Draft revision spellings tried on every body that has room for one.
fn with_revisions(bodies: Vec<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    for body in bodies {
        if let Value::Object(map) = &body {
            for revision in [json!(0), json!(7), json!(-1), json!("1"), json!(null)] {
                let mut map = map.clone();
                map.insert("expected_draft_revision".to_owned(), revision);
                out.push(Value::Object(map));
            }
        }
        out.push(body);
    }
    out
}

fn bytes(bodies: &[Value]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = bodies
        .iter()
        .map(|body| serde_json::to_vec(body).unwrap())
        .collect();
    out.extend(raw_bodies());
    out
}

fn create_corpus() -> Vec<Vec<u8>> {
    let pool = pool();
    let base = json!({
        "resource_tenant_id": UUID, "payer_tenant_id": UUID, "seller_tenant_id": UUID,
        "category": Category::NEW_SALE, "contract_id": UUID,
    });
    let mut bodies = fixture_bodies("create");
    bodies.push(base.clone());
    bodies.extend(mutations(&base, &[], &pool));
    let mut drafted = base;
    drafted["expected_draft_revision"] = json!(0);
    bodies.push(drafted);
    bytes(&bodies)
}

fn add_line_bodies() -> Vec<Value> {
    let pool = pool();
    let base = json!({
        "plan_id": UUID, "plan_revision_id": UUID, "selected_items": [item()],
        "currency": "EUR", "contract_effective_date": "2026-11-01",
        "service_activation_date": "2026-11-02", "acceptance_due_date": "2026-11-03",
        "term_duration": {"kind": "periods", "count": 12}, "billing_cycle": "month",
    });
    let mut bodies = fixture_bodies("add_line");
    bodies.push(base.clone());
    bodies.extend(mutations(&base, &[], &pool));
    bodies.extend(mutations(&base, &["selected_items", "0"], &pool));
    bodies.extend(mutations(&base, &["term_duration"], &pool));
    for term in terms() {
        let mut body = base.clone();
        body["term_duration"] = term;
        bodies.push(body);
    }
    let mut external = base;
    external["external_reference"] = json!("PO-1");
    bodies.push(external);
    bodies
}

/// `{ "fields": { name: value } }` for every name and pool value, plus multi-field and nested
/// mutations of a full `fields` object.
fn patch_bodies(operation: &str, full: &Value, extra_names: &[&str]) -> Vec<Value> {
    let pool = pool();
    let mut bodies = fixture_bodies(operation);
    let names: Vec<String> = full
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .chain(extra_names.iter().map(|name| (*name).to_owned()))
        .collect();
    for name in &names {
        for value in &pool {
            bodies.push(json!({ "fields": { name.as_str(): value } }));
        }
    }
    let base = json!({ "fields": full });
    bodies.push(base.clone());
    bodies.extend(mutations(&base, &["fields"], &pool));
    if full.get("selected_items").is_some() {
        bodies.extend(mutations(&base, &["fields", "selected_items", "0"], &pool));
    }
    if full.get("term_duration").is_some() {
        bodies.extend(mutations(&base, &["fields", "term_duration"], &pool));
    }
    bodies
}

fn header_fields() -> Value {
    json!({
        "category": Category::NEW_SALE, "payer_tenant_id": UUID, "contract_id": UUID,
        "resource_tenant_id": UUID, "seller_tenant_id": UUID, "external_reference": "PO-1",
        "display_label": "label", "internal_notes": "notes",
    })
}

fn line_fields() -> Value {
    json!({
        "plan_id": UUID, "plan_revision_id": UUID, "selected_items": [item()],
        "currency": "EUR", "contract_effective_date": "2026-11-01",
        "service_activation_date": "2026-11-02", "acceptance_due_date": "2026-11-03",
        "term_duration": {"kind": "calendar", "years": 1, "months": 2, "days": 3, "microseconds": 4},
        "billing_cycle": "year", "external_reference": "PO-1", "display_label": "label",
        "internal_notes": "notes",
    })
}

#[test]
fn create_bodies_decode_as_before() {
    let mut tally = Tally::default();
    for body in create_corpus() {
        same(
            &mut tally,
            &body,
            old_create(&body),
            capture::create_body(&body),
        );
    }
    assert!(tally.accepted > 20 && tally.refused > 400, "{tally:?}");
}

#[test]
fn add_line_bodies_decode_as_before() {
    let mut tally = Tally::default();
    for body in bytes(&with_revisions(add_line_bodies())) {
        same(
            &mut tally,
            &body,
            old_add_line(&body),
            capture::add_line_body(&body),
        );
    }
    assert!(tally.accepted > 300 && tally.refused > 7_000, "{tally:?}");
}

#[test]
fn patch_order_bodies_decode_as_before() {
    let bodies = patch_bodies("patch_order", &header_fields(), &["currency", "plan_id"]);
    let mut tally = Tally::default();
    for body in bytes(&with_revisions(bodies)) {
        same(
            &mut tally,
            &body,
            old_patch_order(&body),
            capture::patch_order_body(&body),
        );
    }
    assert!(tally.accepted > 900 && tally.refused > 9_000, "{tally:?}");
}

#[test]
fn patch_line_bodies_decode_as_before() {
    let bodies = patch_bodies(
        "patch_line",
        &line_fields(),
        &["category", "payer_tenant_id"],
    );
    let mut tally = Tally::default();
    for body in bytes(&with_revisions(bodies)) {
        same(
            &mut tally,
            &body,
            old_patch_line(&body),
            capture::patch_line_body(&body),
        );
    }
    assert!(
        tally.accepted > 1_300 && tally.refused > 17_000,
        "{tally:?}"
    );
}

/// The presence rules, spelled out: `null` on a non-nullable `PATCH` field is refused, on a
/// nullable one it names the field and clears it, and an omitted field is not named.
#[test]
fn patch_presence_rules_hold_on_the_new_path() {
    let line = |fields: Value| {
        capture::patch_line_body(&serde_json::to_vec(&json!({ "fields": fields })).unwrap())
    };
    for name in ["plan_id", "plan_revision_id", "selected_items", "currency"] {
        assert!(line(json!({ name: null })).is_err(), "{name}");
    }
    let (_, cleared) = line(json!({"billing_cycle": null, "term_duration": null})).unwrap();
    assert_eq!(cleared.billing_cycle, Some(None));
    assert_eq!(cleared.term_duration, Some(None));
    assert_eq!(cleared.named_fields(), ["term_duration", "billing_cycle"]);
    let header = |fields: Value| {
        capture::patch_order_body(&serde_json::to_vec(&json!({ "fields": fields })).unwrap())
    };
    for name in [
        "category",
        "payer_tenant_id",
        "resource_tenant_id",
        "seller_tenant_id",
    ] {
        assert!(header(json!({ name: null })).is_err(), "{name}");
    }
    let (revision, cleared) = header(json!({"contract_id": null})).unwrap();
    assert_eq!(revision, None);
    assert_eq!(cleared.contract_id, Some(None));
    assert_eq!(cleared.named_fields(), ["contract_id"]);
}
