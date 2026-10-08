//! D-439: every closed set on a RESPONSE schema is an `enum` in the served spec, holding exactly
//! the stored tokens; request bodies keep `string`, so each field's refusal code stays (D-403);
//! the fields no CHECK guards stay `string` on the responses too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::Value;
use std::collections::BTreeSet;
use toolkit::api::OpenApiInfo;

pub mod rest_support;

const CHARGE_KIND: &[&str] = &["recurring", "usage", "one_time"];
const PERIOD: &[&str] = &["month", "year"];
const MODEL: &[&str] = &["flat", "per_unit", "graduated", "volume", "package"];
const ENTRY_REFERENCE: &[&str] = &["confirmation_pending", "confirmed", "lost", "released"];
const ELIGIBILITY: &[&str] = &["all", "new"];
const PRICE_STATE: &[&str] = &["draft", "pending", "approved", "rejected", "cancelled"];
const CHANGE_KIND: &[&str] = &["set", "cancel", "end"];
const PRICE_STATUS: &[&str] = &[
    "draft",
    "pending",
    "rejected",
    "scheduled",
    "active",
    "superseded",
    "cancelled",
];
/// A pinned price's status (D-520, amended 2026-10-04): only `cancelled` is ever served.
const PINNED_PRICE_STATUS: &[&str] = &["cancelled"];
/// Where a SKU's entry stands today (D-486). Not a price's display status.
const SKU_ENTRY_STATUS: &[&str] = &["priced", "scheduled", "unpriced"];
const ITEM_REFERENCE: &[&str] = &["unreserved", "confirmation_pending", "confirmed", "lost"];
const REVISION: &[&str] = &["draft", "pending", "scheduled", "published", "superseded"];
/// How a plan's list row is changing (D-485).
const PLAN_CHANGE: &[&str] = &["none", "draft", "pending", "scheduled"];
const RESOLVED_REVISION: &[&str] = &["published", "superseded", "scheduled"];
const OP_KIND: &[&str] = &["create", "delete", "rereserve", "attach", "release"];
const OP_STATE: &[&str] = &["reserving", "written", "cancelling", "releasing", "done"];
const OP_REF_KIND: &[&str] = &["price_book_entry", "plan_item"];
const UNIT_STATE: &[&str] = &["pending", "approved", "rejected", "withdrawn"];
const UNIT_KIND: &[&str] = &["prices", "plan_revision"];
const DECISION: &[&str] = &["approve", "reject"];
const VOTE_OUTCOME: &[&str] = &["pending", "applied", "rejected", "withdrawn"];
const TIMING: &[&str] = &["advance", "arrears"];
const SOURCE: &[&str] = &["entry", "sku", "tenant"];

/// (schema, field, the exact values in order, nullable).
type Closed = (&'static str, &'static str, &'static [&'static str], bool);
const CLOSED: &[Closed] = &[
    (
        "PricingPriceBookEntryDto",
        "charge_kind",
        CHARGE_KIND,
        false,
    ),
    ("PricingPriceBookEntryDto", "period", PERIOD, true),
    ("PricingPriceBookEntryDto", "model", MODEL, false),
    (
        "PricingPriceBookEntryDto",
        "reference_state",
        ENTRY_REFERENCE,
        false,
    ),
    ("PricingPriceDto", "model", MODEL, false),
    ("PricingPriceDto", "eligibility", ELIGIBILITY, false),
    ("PricingPriceDto", "state", PRICE_STATE, false),
    ("PricingPriceDto", "change_kind", CHANGE_KIND, false),
    ("PricingPriceDto", "status", PRICE_STATUS, false),
    ("PricingSkuEntryDto", "status", SKU_ENTRY_STATUS, false),
    (
        "PricingPlanItemDto",
        "reference_state",
        ITEM_REFERENCE,
        false,
    ),
    ("PricingPlanItemReadDto", "state", REVISION, false),
    ("PricingPlanRevisionHeader", "state", REVISION, false),
    ("PricingPlanDto", "change", PLAN_CHANGE, false),
    ("PricingPlanRevisionDto", "state", REVISION, false),
    ("PricingPlanEntrySummary", "charge_kind", CHARGE_KIND, false),
    ("PricingPlanEntrySummary", "period", PERIOD, true),
    ("PricingPlanEntrySummary", "model", MODEL, false),
    (
        "PricingPlanReservationItemDto",
        "reference_state",
        ITEM_REFERENCE,
        false,
    ),
    ("PricingEffectivePolicyDto", "kind", UNIT_KIND, false),
    ("PricingReferenceOpDto", "kind", OP_KIND, false),
    ("PricingReferenceOpDto", "state", OP_STATE, false),
    ("PricingReferenceOpDto", "ref_kind", OP_REF_KIND, false),
    ("PricingApprovalUnitDto", "state", UNIT_STATE, false),
    // The phase 9 review's theme C: no CHECK holds the kind, but the repository reads it through
    // its closed set, so a row outside it is a corrupt row (500), never served.
    ("PricingApprovalUnitDto", "kind", UNIT_KIND, false),
    ("PricingDecisionDto", "decision", DECISION, false),
    ("PricingVoteReceipt", "outcome", VOTE_OUTCOME, false),
    ("PricingSettingsDto", "default_timing", TIMING, false),
    ("PricingResolveDto", "state", RESOLVED_REVISION, false),
    ("PricingResolveItemDto", "charge_kind", CHARGE_KIND, true),
    ("PricingResolveItemDto", "period", PERIOD, true),
    ("PricingResolveItemDto", "model", MODEL, true),
    ("PricingResolveInputDto", "source", SOURCE, true),
    (
        "PricingResolveBindingDto",
        "eligibility",
        ELIGIBILITY,
        false,
    ),
    ("PricingPinnedPriceDto", "charge_kind", CHARGE_KIND, false),
    ("PricingPinnedPriceDto", "period", PERIOD, true),
    ("PricingPinnedPriceDto", "model", MODEL, false),
    ("PricingPinnedPriceDto", "eligibility", ELIGIBILITY, false),
    // D-520, amended: `cancelled` on a cancelled price, absent on an approved one (D-422). Its own
    // one-value set: the read never serves another status.
    ("PricingPinnedPriceDto", "status", PINNED_PRICE_STATUS, true),
];

/// Response fields that stay `string` (D-439): no CHECK guards the stored set (`default_rounding`
/// and the resolve's copy of it, D-437; the approval unit's `ref_type`), or the value is a code
/// vocabulary or an open value rather than a state (`code`, `chain`).
const KEPT_STRING: &[(&str, &str)] = &[
    ("PricingSettingsDto", "default_rounding"),
    ("PricingResolveDto", "rounding_policy"),
    ("PricingApprovalUnitDto", "ref_type"),
    ("PricingPlanCheckDto", "code"),
    ("PricingProposedPrice", "chain"),
];

/// Request fields over the same sets: `string`, so the door's own code refuses a bad value.
const REQUEST_STRING: &[(&str, &str)] = &[
    ("PricingPriceBookEntryCreate", "model"),
    ("PricingPriceBookEntryCreate", "period"),
    ("PricingPriceCreate", "eligibility"),
    ("PricingPricePatch", "eligibility"),
    ("PricingSettingsPut", "default_timing"),
    ("PricingSettingsPut", "default_rounding"),
    ("PricingApprovalPolicyPut", "kind"),
];

async fn served() -> Value {
    let harness = rest_support::Harness::new().await.unwrap();
    let (_, openapi) = harness.router(axum::Router::new()).unwrap();
    serde_json::to_value(openapi.build_openapi(&OpenApiInfo::default()).unwrap()).unwrap()
}

fn component<'a>(api: &'a Value, name: &str) -> &'a Value {
    &api["components"]["schemas"][name]
}

/// The property `field` of component `schema`: its own, or an inline `allOf` part's (a flatten).
fn property<'a>(api: &'a Value, schema: &str, field: &str) -> Option<&'a Value> {
    let s = component(api, schema);
    std::iter::once(s)
        .chain(s["allOf"].as_array().into_iter().flatten())
        .find_map(|part| part["properties"].get(field))
}

fn referenced<'a>(api: &'a Value, node: &'a Value) -> &'a Value {
    node["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("#/components/schemas/"))
        .map_or(node, |name| component(api, name))
}

/// The values of the `enum` a property names (inline, by `$ref`, or by `allOf`/`oneOf`/`anyOf`
/// beside `null`) and whether it admits `null`; an error names what the property is instead.
fn enum_of(api: &Value, p: &Value) -> Result<(Vec<String>, bool), String> {
    let mut nullable = p["type"]
        .as_array()
        .is_some_and(|t| t.iter().any(|t| t == "null"));
    let mut target = None;
    for key in ["oneOf", "anyOf", "allOf"] {
        for branch in p[key].as_array().into_iter().flatten() {
            if branch["type"] == "null" {
                nullable = true;
            } else {
                target = Some(referenced(api, branch));
            }
        }
    }
    let target = target.unwrap_or_else(|| referenced(api, p));
    let values = target["enum"]
        .as_array()
        .ok_or_else(|| format!("no enum: {p}"))?;
    let string = target["type"] == "string"
        || target["type"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t == "string"));
    if !string {
        return Err(format!("the enum is not a string: {target}"));
    }
    Ok((
        values
            .iter()
            .map(|v| v.as_str().unwrap_or("<not a string>").to_owned())
            .collect(),
        nullable,
    ))
}

/// A plain `string` property: no `enum` and no reference, only `string` (and `null`).
fn plain_string(p: &Value) -> bool {
    let string = p["type"] == "string"
        || p["type"].as_array().is_some_and(|t| {
            t.iter().any(|t| t == "string") && t.iter().all(|t| t == "string" || t == "null")
        });
    string && p.get("enum").is_none() && p.get("$ref").is_none()
}

#[tokio::test]
async fn every_closed_set_on_a_response_schema_is_an_enum_of_its_stored_tokens() {
    let api = served().await;
    let mut wrong = Vec::new();
    for &(schema, field, values, nullable) in CLOSED {
        let Some(p) = property(&api, schema, field) else {
            wrong.push(format!("{schema}.{field}: no such property"));
            continue;
        };
        match enum_of(&api, p) {
            Ok((served, served_nullable)) => {
                if served != values || served_nullable != nullable {
                    wrong.push(format!(
                        "{schema}.{field}: {served:?} (nullable {served_nullable}), want \
                         {values:?} (nullable {nullable})"
                    ));
                }
            }
            Err(why) => wrong.push(format!("{schema}.{field}: {why}")),
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} closed sets are not their enum:\n{}",
        wrong.len(),
        CLOSED.len(),
        wrong.join("\n")
    );
}

#[tokio::test]
async fn the_fields_no_check_guards_stay_strings_on_the_responses() {
    let api = served().await;
    for &(schema, field) in KEPT_STRING {
        let p = property(&api, schema, field).unwrap_or_else(|| panic!("{schema}.{field}"));
        assert!(plain_string(p), "{schema}.{field}: {p}");
    }
}

fn refs(node: &Value, out: &mut BTreeSet<String>) {
    match node {
        Value::Object(map) => {
            if let Some(r) = map.get("$ref").and_then(Value::as_str) {
                out.insert(r.trim_start_matches("#/components/schemas/").to_owned());
            }
            map.values().for_each(|v| refs(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| refs(v, out)),
        _ => {}
    }
}
fn has_enum(node: &Value) -> bool {
    match node {
        Value::Object(map) => map.contains_key("enum") || map.values().any(has_enum),
        Value::Array(items) => items.iter().any(has_enum),
        _ => false,
    }
}

#[tokio::test]
async fn request_bodies_keep_strings_so_the_doors_keep_their_codes() {
    let api = served().await;
    let mut pending = BTreeSet::new();
    for op in api["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|ops| ops.as_object().unwrap().values())
    {
        refs(&op["requestBody"], &mut pending);
    }
    let mut seen = BTreeSet::new();
    while let Some(name) = pending.pop_first() {
        if seen.insert(name.clone()) {
            let mut next = BTreeSet::new();
            refs(component(&api, &name), &mut next);
            pending.extend(next.difference(&seen).cloned());
        }
    }
    let enums: Vec<_> = seen
        .iter()
        .filter(|name| has_enum(component(&api, name)))
        .collect();
    assert_eq!(
        enums.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        [
            "AggregationScope",
            "Fold",
            "PartialWindow",
            "RatingWindow",
            "Reset",
            "Timezone"
        ],
        "D-502 alone introduces closed policy request enums; existing request codes stay unchanged"
    );
    for &(schema, field) in REQUEST_STRING {
        assert!(seen.contains(schema), "{schema} is a request body");
        let p = property(&api, schema, field).unwrap_or_else(|| panic!("{schema}.{field}"));
        assert!(plain_string(p), "{schema}.{field}: {p}");
    }
}

/// D-467: a plan item is a SKU and its entry. No plan item schema, request or response, and no
/// resolved item carries `treatment`, `included_qty` or `qty_min`; the treatment's closed set is
/// gone from the spec. The create requires `sku_id`; `price_book_entry_id` may be absent (D-512).
#[tokio::test]
async fn no_plan_item_schema_carries_treatment_or_the_quantities() {
    let api = served().await;
    for schema in [
        "PricingPlanItemCreate",
        "PricingPlanItemPatch",
        "PricingPlanItemDto",
        "PricingPlanItemReadDto",
        "PricingResolveItemDto",
    ] {
        assert!(!component(&api, schema).is_null(), "{schema} is served");
        for key in ["treatment", "included_qty", "qty_min"] {
            assert!(property(&api, schema, key).is_none(), "{schema}.{key}");
        }
    }
    assert!(
        api["components"]["schemas"]
            .get("PricingTreatment")
            .is_none(),
        "no schema names a treatment"
    );
    let required = component(&api, "PricingPlanItemCreate")["required"]
        .as_array()
        .unwrap();
    assert!(required.contains(&Value::from("sku_id")), "{required:?}");
    assert!(
        !required.contains(&Value::from("price_book_entry_id")),
        "the entry may be absent: {required:?}"
    );
}
