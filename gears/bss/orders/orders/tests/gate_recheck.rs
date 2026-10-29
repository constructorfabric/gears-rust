//! The activation re-check's frozen decision vectors (03 §3.6, D-127, D-144; S3-13 Orders-side)
//! against the pure decision. Workflow #4787 adopts `activation-recheck-v1.json` as its
//! conformance set; the vectors are authored from the algorithm text, not from this code.
//!
//! The fixture is string-typed; `input_of` converts it to `RecheckInput` and refuses a missing
//! or unknown member, an unregistered token and a value the typed field cannot hold.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a corrupt frozen fixture must fail loudly; `allow`, not `expect`, because not every \
              test in the file unwraps or expects"
)]
use bss_orders::domain::gate::answers::{
    LimitProvenance, MaxConcurrentActive, Occupancy, OccupancyObservation, OccupancyQuery,
    PayerProfile,
};
use bss_orders::domain::gate::ports::{PortAnswer, Unavailable};
use bss_orders::domain::gate::recheck::{
    NotDispatchable, Recheck, RecheckInput, StoredLine, StoredVersion, recheck,
};
use bss_orders_sdk::catalog::{OrderState, Reason};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

fn vectors() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("fixtures/activation-recheck-v1.json")).unwrap()
        ["vectors"]
        .as_array()
        .unwrap()
        .clone()
}

/// The object's members, refused unless they are exactly `keys`.
fn exact<'a>(v: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>, String> {
    let object = v.as_object().ok_or_else(|| format!("not an object: {v}"))?;
    let have: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let want: BTreeSet<&str> = keys.iter().copied().collect();
    if have == want {
        Ok(object)
    } else {
        Err(format!("members {have:?}, expected {want:?}"))
    }
}
fn text<'a>(o: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    o[key]
        .as_str()
        .ok_or_else(|| format!("`{key}` is not a string"))
}
fn parse<T>(
    o: &Map<String, Value>,
    key: &str,
    f: impl FnOnce(&str) -> Option<T>,
) -> Result<T, String> {
    f(text(o, key)?).ok_or_else(|| format!("`{key}` does not parse"))
}
fn nullable<T>(
    o: &Map<String, Value>,
    key: &str,
    f: impl FnOnce(&str) -> Option<T>,
) -> Result<Option<T>, String> {
    match &o[key] {
        Value::Null => Ok(None),
        _ => parse(o, key, f).map(Some),
    }
}
fn uuid(s: &str) -> Option<Uuid> {
    Uuid::parse_str(s).ok()
}
fn int(s: &str) -> Option<u32> {
    s.parse().ok()
}
fn instant(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}
fn state(s: &str) -> Option<OrderState> {
    OrderState::ALL.iter().copied().find(|st| st.token() == s)
}
fn unavailable(s: &str) -> Option<Unavailable> {
    Some(match s {
        "outage" => Unavailable::Outage,
        "denied" => Unavailable::Denied,
        "deadline" => Unavailable::Deadline,
        "malformed" => Unavailable::Malformed,
        "missing" => Unavailable::Missing,
        _ => return None,
    })
}
fn provenance(s: &str) -> Option<LimitProvenance> {
    serde_json::from_value(json!(s)).ok()
}

/// `{"answered": …}` or `{"unavailable": "<kind>"}`.
fn port<T>(
    v: &Value,
    answered: impl FnOnce(&Value) -> Result<T, String>,
) -> Result<PortAnswer<T>, String> {
    let object = v.as_object().ok_or("port answer is not an object")?;
    match (
        object.len(),
        object.get("answered"),
        object.get("unavailable"),
    ) {
        (1, Some(a), None) => answered(a).map(PortAnswer::Answered),
        (1, None, Some(_)) => {
            parse(object, "unavailable", unavailable).map(PortAnswer::Unavailable)
        }
        _ => Err(format!("port answer members: {object:?}")),
    }
}
/// The identity answer carries the two facts the re-check reads; the profile's other facts
/// are not inputs of the decision and are filled with fixed values.
fn profile_of(v: &Value) -> Result<PayerProfile, String> {
    let o = exact(v, &["currency", "region"])?;
    Ok(PayerProfile {
        axes_valid: true,
        currency: text(o, "currency")?.to_owned(),
        region: text(o, "region")?.to_owned(),
        in_relationship_with_seller: true,
        in_relationship_with_resource: true,
        profile_version: "fixture".to_owned(),
    })
}
fn occupancy_of(v: &Value) -> Result<Vec<(OccupancyQuery, OccupancyObservation)>, String> {
    v.as_array()
        .ok_or("occupancy answer is not an array")?
        .iter()
        .map(|row| {
            let o = exact(
                row,
                &[
                    "overlap_scope_key",
                    "active_count",
                    "count_scope",
                    "max_concurrent_active",
                    "provenance",
                ],
            )?;
            Ok((
                legacy_query(text(o, "overlap_scope_key")?),
                Occupancy {
                    active_count: parse(o, "active_count", int)?,
                    count_scope: serde_json::from_value(o["count_scope"].clone())
                        .map_err(|e| e.to_string())?,
                    max_concurrent_active: nullable(o, "max_concurrent_active", int)?
                        .map_or(MaxConcurrentActive::Unlimited, MaxConcurrentActive::Limit),
                    provenance: parse(o, "provenance", provenance)?,
                }
                .into(),
            ))
        })
        .collect()
}
fn line_of(v: &Value) -> Result<StoredLine, String> {
    let o = exact(v, &["line_id", "overlap_scope_key", "binding_deadline"])?;
    Ok(StoredLine {
        overlap_item_keys: None,
        catalog_subscription_product_key: None,
        line_id: parse(o, "line_id", uuid)?,
        overlap_scope_key: nullable(o, "overlap_scope_key", |s| Some(s.to_owned()))?,
        binding_deadline: nullable(o, "binding_deadline", instant)?,
    })
}
fn stored_of(v: &Value) -> Result<StoredVersion, String> {
    let o = exact(
        v,
        &[
            "state",
            "current_version",
            "market_currency",
            "market_region",
            "payer_tenant_id",
            "lines",
        ],
    )?;
    Ok(StoredVersion {
        order_id: None,
        resource_tenant_id: None,
        state: parse(o, "state", state)?,
        current_version: parse(o, "current_version", int)?,
        market_currency: text(o, "market_currency")?.to_owned(),
        market_region: text(o, "market_region")?.to_owned(),
        payer_tenant_id: parse(o, "payer_tenant_id", uuid)?,
        lines: o["lines"]
            .as_array()
            .ok_or("`lines` is not an array")?
            .iter()
            .map(line_of)
            .collect::<Result<_, _>>()?,
    })
}
/// The fixture's string-typed input as the decision's typed input.
fn input_of(v: &Value) -> Result<RecheckInput, String> {
    let o = exact(
        v,
        &[
            "requested_version",
            "stored",
            "identity",
            "occupancy",
            "now",
        ],
    )?;
    Ok(RecheckInput {
        requested_version: parse(o, "requested_version", int)?,
        stored: stored_of(&o["stored"])?,
        identity: port(&o["identity"], profile_of)?,
        occupancy: port(&o["occupancy"], occupancy_of)?,
        now: parse(o, "now", instant)?,
    })
}
/// The decision in the fixture's `expect` form.
fn expect_form(outcome: &Recheck) -> Value {
    match outcome {
        Recheck::Proceed => json!({"outcome": "proceed"}),
        Recheck::Reject { lines } => json!({
            "outcome": "reject",
            "lines": lines
                .iter()
                .map(|(id, reason)| json!({"line_id": id.to_string(), "reason": reason.token()}))
                .collect::<Vec<_>>(),
        }),
        Recheck::NotDispatchable(NotDispatchable::Held) => {
            json!({"outcome": "not-dispatchable", "kind": "held"})
        }
        Recheck::NotDispatchable(NotDispatchable::Terminal) => {
            json!({"outcome": "not-dispatchable", "kind": "terminal"})
        }
        Recheck::NotDispatchable(NotDispatchable::Superseded { current_version }) => json!({
            "outcome": "not-dispatchable",
            "kind": "superseded",
            "current_version": current_version.to_string(),
        }),
        Recheck::Defer(reason) => json!({"outcome": "defer", "reason": reason.token()}),
    }
}

#[test]
fn recheck_reproduces_every_frozen_vector() {
    let mut failures = Vec::new();
    for vector in vectors() {
        let id = vector["id"].as_str().unwrap();
        let input = input_of(&vector["input"]).unwrap_or_else(|e| panic!("{id}: {e}"));
        let got = expect_form(&recheck(&input));
        if got != vector["expect"] {
            failures.push(format!("{id}: got {got}, expected {}", vector["expect"]));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn vectors_cover_every_outcome_and_reason() {
    let all = vectors();
    let ids: BTreeSet<&str> = all.iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), all.len(), "vector ids are unique");
    for required in [
        "proceed-two-lines",
        "not-dispatchable-held",
        "not-dispatchable-terminal-cancelled",
        "not-dispatchable-superseded",
        "defer-identity-outage",
        "defer-identity-deadline",
        "reject-market-currency",
        "reject-market-region",
        "defer-occupancy-outage",
        "defer-occupancy-key-missing",
        "reject-overlap-collision-one-key",
        "proceed-unlimited-limit",
        "proceed-at-the-limit",
        "proceed-line-without-overlap-key",
        "reject-binding-expired",
        "proceed-binding-not-yet-expired",
        "held-wins-over-identity-outage",
        "market-wins-over-collision",
        "collision-wins-over-binding-expired",
        "terminal-wins-over-superseded",
        "held-wins-over-superseded",
        "proceed-no-keys-occupancy-unavailable",
    ] {
        assert!(ids.contains(required), "missing vector {required}");
    }
    let outcomes: BTreeSet<&str> = all
        .iter()
        .map(|v| v["expect"]["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(
        outcomes,
        BTreeSet::from(["proceed", "reject", "not-dispatchable", "defer"])
    );
    let kinds: BTreeSet<&str> = all
        .iter()
        .filter_map(|v| v["expect"]["kind"].as_str())
        .collect();
    assert_eq!(kinds, BTreeSet::from(["held", "terminal", "superseded"]));
    let reasons: BTreeSet<&str> = all
        .iter()
        .flat_map(|v| {
            let e = &v["expect"];
            e["reason"].as_str().into_iter().chain(
                e["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|l| l["reason"].as_str().unwrap()),
            )
        })
        .collect();
    let want: BTreeSet<&str> = [
        Reason::IdentityPartyUnavailable,
        Reason::MarketDivergence,
        Reason::OverlapPresenceUnevaluable,
        Reason::OverlapCollision,
        Reason::OrderBindingExpired,
    ]
    .iter()
    .map(|r| r.token())
    .collect();
    assert_eq!(reasons, want);
}

#[test]
fn converter_refuses_unknown_and_missing_members() {
    let base = vectors()[0]["input"].clone();
    let mut unknown = base.clone();
    unknown["stored"]["lines"][0]["extra"] = json!("x");
    assert!(input_of(&unknown).is_err());
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("now");
    assert!(input_of(&missing).is_err());
    let mut bad_state = base;
    bad_state["stored"]["state"] = json!("paused");
    assert!(input_of(&bad_state).is_err());
}

#[test]
fn recheck_counts_only_own_pending_lines_for_both_declared_scopes() {
    for vector in vectors() {
        let mut input = input_of(&vector["input"]).unwrap();
        let expected = recheck(&input);
        if let PortAnswer::Answered(rows) = &mut input.occupancy {
            for (_, answer) in rows {
                let OccupancyObservation::LegacyActiveV1(answer) = answer else {
                    panic!()
                };
                answer.count_scope =
                    bss_orders::domain::gate::answers::OccupancyCountScope::FullTuple;
            }
        }
        assert_eq!(recheck(&input), expected, "{}", vector["id"]);
    }
    let row = json!({"overlap_scope_key": "k", "active_count": "0", "max_concurrent_active": "1", "provenance": "catalog"});
    assert!(occupancy_of(&json!([row])).is_err());
    let mut unknown = row;
    unknown["count_scope"] = json!("unknown");
    assert!(occupancy_of(&json!([unknown])).is_err());
}

#[test]
fn collection_recheck_checks_all_scopes_once_per_line_and_rejects_mixed_rows() {
    use bss_orders::domain::gate::{
        answers::OverlapKeyLine,
        item_keys::{ClassificationProvenance, ItemOverlapKey},
    };
    let vector = vectors()
        .into_iter()
        .find(|v| v["id"] == "proceed-no-keys-occupancy-unavailable")
        .unwrap();
    let mut input = input_of(&vector["input"]).unwrap();
    let line = input.stored.lines[0].line_id;
    let item = |id, scope: &str| ItemOverlapKey {
        profile: "item_keys_v1".into(),
        selection: OverlapKeyLine {
            line_id: line,
            plan_revision_id: Uuid::from_u128(2),
            item_id: Uuid::from_u128(id),
            dimension_value: None,
            paid_recurring_sku: Uuid::from_u128(3),
            sku_version: 7,
        },
        catalog_subscription_product_key: format!("product-{id}"),
        overlap_scope_key: scope.into(),
        classification: ClassificationProvenance {
            source: "test-provider".into(),
            reference: "classification".into(),
            version: "1".into(),
        },
    };
    input.stored.lines[0].overlap_item_keys =
        Some(vec![item(10, "a"), item(11, "a"), item(12, "b")]);
    let occupancy = |n| {
        Occupancy {
            active_count: 0,
            count_scope: bss_orders::domain::gate::answers::OccupancyCountScope::FullTuple,
            max_concurrent_active: MaxConcurrentActive::Limit(n),
            provenance: LimitProvenance::Catalog,
        }
        .into()
    };
    input.occupancy = PortAnswer::Answered(vec![
        (legacy_query("a"), occupancy(1)),
        (legacy_query("b"), occupancy(1)),
    ]);
    assert_eq!(
        recheck(&input),
        Recheck::Proceed,
        "repeated items on scope a demand one line unit"
    );
    input.occupancy = PortAnswer::Answered(vec![
        (legacy_query("a"), occupancy(1)),
        (legacy_query("b"), occupancy(0)),
    ]);
    assert_eq!(
        recheck(&input),
        Recheck::Reject {
            lines: vec![(line, Reason::OverlapCollision)]
        }
    );
    input.occupancy = PortAnswer::Answered(vec![(legacy_query("a"), occupancy(1))]);
    assert_eq!(
        recheck(&input),
        Recheck::Defer(Reason::OverlapPresenceUnevaluable)
    );
    input.stored.lines[0].catalog_subscription_product_key = Some("legacy-product".into());
    assert_eq!(
        recheck(&input),
        Recheck::Defer(Reason::OverlapPresenceUnevaluable)
    );
}

fn legacy_query(key: &str) -> OccupancyQuery {
    OccupancyQuery {
        own_context: None,
        payer_tenant_id: Uuid::nil(),
        resource_tenant_id: None,
        overlap_scope_key: key.into(),
    }
}
