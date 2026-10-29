//! Orders-only proposed protected profile conformance; no Workflow/provider adoption asserted.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
use bss_orders::domain::gate::answers::{OccupancyObservation, OccupancyQuery, PayerProfile};
use bss_orders::domain::gate::ports::PortAnswer;
use bss_orders::domain::gate::protected::{
    OwnContext, ProtectedBatch, ProtectedInvalid, validate_batch,
};
use bss_orders::domain::gate::recheck::{
    Recheck, RecheckInput, StoredLine, StoredVersion, occupancy_queries, recheck,
};
use bss_orders_sdk::catalog::{OrderState, Reason};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use time::OffsetDateTime;
use uuid::Uuid;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn context() -> OwnContext {
    OwnContext {
        order_id: id(1),
        version: 2,
        payer_tenant_id: id(2),
        resource_tenant_id: id(3),
        line_ids: vec![id(4)],
    }
}
fn query(key: &str) -> OccupancyQuery {
    OccupancyQuery {
        own_context: Some(context()),
        payer_tenant_id: id(2),
        resource_tenant_id: Some(id(3)),
        overlap_scope_key: key.into(),
    }
}
fn observation(key: &str) -> Value {
    json!({
        "profile":"protected_full_tuple_v1","demand_unit":"line_scope_v1","observation_revision":"snapshot-1",
        "scope":{"payer_tenant_id":id(2),"resource_tenant_id":id(3),"overlap_scope_key":key},
        "policy":{"source":"catalog","reference":"policy-1","version":"1"},
        "active_count":0,"retained_suspended_count":0,"order_reserved_count":1,"max_concurrent_active":1,
        "own_membership":{"complete":true,"context":context(),"rights":[{"claim_id":id(5),"line_id":id(4),"state":"order_reserved"}]}
    })
}
fn expected(q: OccupancyQuery) -> BTreeMap<String, (OccupancyQuery, BTreeSet<Uuid>)> {
    [(
        q.overlap_scope_key.clone(),
        (
            q.clone(),
            q.own_context.unwrap().line_ids.into_iter().collect(),
        ),
    )]
    .into_iter()
    .collect()
}
fn decoded(v: Value) -> OccupancyObservation {
    serde_json::from_value(v).unwrap()
}
/// The validated map of a batch that must be protected.
fn protected(
    batch: Result<ProtectedBatch, ProtectedInvalid>,
) -> BTreeMap<String, bss_orders::domain::gate::protected::ValidatedProtected> {
    match batch {
        Ok(ProtectedBatch::Protected(map)) => map,
        other => panic!("expected a protected batch, got {other:?}"),
    }
}

#[test]
fn protected_wire_requires_every_field_including_nullable_fields_and_discriminators() {
    let value = observation("a");
    for name in value.as_object().unwrap().keys() {
        let mut bad = value.clone();
        bad.as_object_mut().unwrap().remove(name);
        assert!(
            serde_json::from_value::<OccupancyObservation>(bad).is_err(),
            "missing {name}"
        );
    }
    for (path, replacement) in [
        ("profile", json!("future")),
        ("demand_unit", json!("item_v1")),
        ("active_count", json!(-1)),
        ("active_count", json!(4_294_967_296_u64)),
        ("own_membership", json!({})),
    ] {
        let mut bad = value.clone();
        bad[path] = replacement;
        assert!(
            serde_json::from_value::<OccupancyObservation>(bad).is_err(),
            "invalid {path}"
        );
    }
    for path in [
        "/scope/resource_tenant_id",
        "/policy/version",
        "/own_membership/complete",
        "/own_membership/context/order_id",
        "/own_membership/rights/0/claim_id",
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(path).unwrap() = Value::Null;
        assert!(
            serde_json::from_value::<OccupancyObservation>(bad).is_err(),
            "invalid {path}"
        );
    }
    let mut explicit_null = value;
    explicit_null["own_membership"] = Value::Null;
    explicit_null["max_concurrent_active"] = Value::Null;
    assert!(serde_json::from_value::<OccupancyObservation>(explicit_null).is_ok());
}

#[test]
fn own_line_context_echo_is_a_set_and_accepts_reordering() {
    let mut q = query("a");
    q.own_context.as_mut().unwrap().line_ids.push(id(6));
    let want = expected(q.clone());
    q.own_context.as_mut().unwrap().line_ids.reverse();
    let mut value = observation("a");
    value["own_membership"]["context"] = json!(q.own_context);
    let batch = validate_batch(&want, &[(q, decoded(value))]);
    assert!(
        batch.is_ok(),
        "line order cannot change an exact context: {batch:?}"
    );
}

#[test]
fn invalid_batches_name_their_cause() {
    let q = query("a");
    let want = expected(q.clone());
    for (path, cause, replacement) in [
        (
            "/observation_revision",
            ProtectedInvalid::InvalidBinding,
            json!(""),
        ),
        (
            "/policy/reference",
            ProtectedInvalid::InvalidBinding,
            json!("x\u{0}y"),
        ),
        (
            "/policy/version",
            ProtectedInvalid::InvalidBinding,
            json!("x".repeat(1025)),
        ),
        (
            "/scope/resource_tenant_id",
            ProtectedInvalid::InvalidBinding,
            json!(id(9)),
        ),
        (
            "/scope/payer_tenant_id",
            ProtectedInvalid::InvalidBinding,
            json!(id(9)),
        ),
        (
            "/scope/overlap_scope_key",
            ProtectedInvalid::InvalidBinding,
            json!("foreign"),
        ),
        (
            "/own_membership/complete",
            ProtectedInvalid::InvalidOwnContext,
            json!(false),
        ),
        (
            "/own_membership",
            ProtectedInvalid::MembershipMismatch,
            Value::Null,
        ),
        (
            "/own_membership/context/order_id",
            ProtectedInvalid::InvalidOwnContext,
            json!(id(9)),
        ),
        (
            "/own_membership/context/version",
            ProtectedInvalid::InvalidOwnContext,
            json!(1),
        ),
        (
            "/own_membership/context/resource_tenant_id",
            ProtectedInvalid::InvalidOwnContext,
            json!(id(9)),
        ),
        (
            "/own_membership/context/payer_tenant_id",
            ProtectedInvalid::InvalidOwnContext,
            json!(id(9)),
        ),
        (
            "/own_membership/context/line_ids",
            ProtectedInvalid::InvalidOwnContext,
            json!([id(4), id(4)]),
        ),
        (
            "/own_membership/rights/0/line_id",
            ProtectedInvalid::InvalidOwnRight,
            json!(id(9)),
        ),
        (
            "/own_membership/rights/0/claim_id",
            ProtectedInvalid::InvalidOwnRight,
            json!(Uuid::nil()),
        ),
        (
            "/own_membership/rights/0/state",
            ProtectedInvalid::InvalidOwnRight,
            json!("active"),
        ),
        (
            "/own_membership/rights/0/state",
            ProtectedInvalid::InvalidOwnRight,
            json!("retained_suspended"),
        ),
        (
            "/order_reserved_count",
            ProtectedInvalid::RightsExceedCount,
            json!(0),
        ),
    ] {
        let mut value = observation("a");
        value["max_concurrent_active"] = Value::Null;
        *value.pointer_mut(path).unwrap() = replacement;
        assert_eq!(
            validate_batch(&want, &[(q.clone(), decoded(value))]),
            Err(cause),
            "wrong cause for {path}"
        );
    }
    let row = (q.clone(), decoded(observation("a")));
    assert_eq!(
        validate_batch(&want, &[row.clone(), row.clone()]),
        Err(ProtectedInvalid::IncompleteRows)
    );
    assert_eq!(
        validate_batch(&want, &[(query("foreign"), row.1.clone())]),
        Err(ProtectedInvalid::UnexpectedKey)
    );
    let mut foreign_query = q.clone();
    foreign_query.payer_tenant_id = id(9);
    assert_eq!(
        validate_batch(&want, &[(foreign_query, row.1.clone())]),
        Err(ProtectedInvalid::InvalidBinding)
    );
    let mut no_context = q;
    no_context.own_context = None;
    assert_eq!(
        validate_batch(&expected(query("a")), &[(no_context, row.1)]),
        Err(ProtectedInvalid::InvalidBinding)
    );
}

#[test]
fn a_legacy_only_batch_is_legacy_not_protected() {
    let q = query("a");
    let want = expected(q.clone());
    let legacy = decoded(json!({
        "active_count":0,"count_scope":"full_tuple","max_concurrent_active":null,"provenance":"catalog"
    }));
    assert_eq!(
        validate_batch(&want, &[(q, legacy)]),
        Ok(ProtectedBatch::Legacy)
    );
    assert_eq!(validate_batch(&want, &[]), Ok(ProtectedBatch::Legacy));
}

#[test]
fn multi_scope_claim_reuse_mixed_profiles_and_revisions_invalidate_the_entire_batch() {
    let mut want = expected(query("a"));
    want.extend(expected(query("b")));
    let a = (query("a"), decoded(observation("a")));
    for (b, cause) in [
        (observation("b"), ProtectedInvalid::InvalidOwnRight),
        (
            {
                let mut v = observation("b");
                v["observation_revision"] = json!("snapshot-2");
                v["own_membership"]["rights"][0]["claim_id"] = json!(id(8));
                v
            },
            ProtectedInvalid::MixedRevisions,
        ),
        (
            json!({"active_count":0,"count_scope":"full_tuple","max_concurrent_active":null,"provenance":"catalog"}),
            ProtectedInvalid::MixedProfiles,
        ),
    ] {
        assert_eq!(
            validate_batch(&want, &[a.clone(), (query("b"), decoded(b))]),
            Err(cause)
        );
    }
    let mut b = observation("b");
    b["own_membership"]["rights"][0]["claim_id"] = json!(id(8));
    let valid = protected(validate_batch(&want, &[a, (query("b"), decoded(b))]));
    assert!(valid.values().all(|p| p.admits() == Ok(true)));
}

fn recheck_input(value: Value) -> RecheckInput {
    let stored = StoredVersion {
        order_id: Some(id(1)),
        resource_tenant_id: Some(id(3)),
        state: OrderState::Submitted,
        current_version: 2,
        market_currency: "EUR".into(),
        market_region: "EU".into(),
        payer_tenant_id: id(2),
        lines: vec![StoredLine {
            line_id: id(4),
            overlap_scope_key: Some("a".into()),
            catalog_subscription_product_key: None,
            overlap_item_keys: None,
            binding_deadline: None,
        }],
    };
    RecheckInput {
        requested_version: 2,
        stored,
        identity: PortAnswer::Answered(PayerProfile {
            axes_valid: true,
            currency: "EUR".into(),
            region: "EU".into(),
            in_relationship_with_seller: true,
            in_relationship_with_resource: true,
            profile_version: "1".into(),
        }),
        occupancy: PortAnswer::Answered(vec![(query("a"), decoded(value))]),
        now: OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn proposed_v2_recheck_vectors() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/activation-recheck-v2.json")).unwrap();
    for case in fixture["vectors"].as_array().unwrap() {
        let mut value = observation("a");
        for patch in case["patch"].as_array().unwrap() {
            *value.pointer_mut(patch[0].as_str().unwrap()).unwrap() = patch[1].clone();
        }
        let mut input = recheck_input(value);
        if case["missing_stored_identity"] == true {
            input.stored.order_id = None;
        }
        let got = recheck(&input);
        match case["expect"].as_str().unwrap() {
            "proceed" => assert_eq!(got, Recheck::Proceed, "{}", case["id"]),
            "defer" => assert_eq!(
                got,
                Recheck::Defer(Reason::OverlapPresenceUnevaluable),
                "{}",
                case["id"]
            ),
            "collision" => assert_eq!(
                got,
                Recheck::Reject {
                    lines: vec![(id(4), Reason::OverlapCollision)]
                },
                "{}",
                case["id"]
            ),
            _ => panic!("unknown expectation"),
        }
    }
}

#[test]
fn stored_identity_drives_queries_and_superseded_version_stops_before_observation() {
    let mut input = recheck_input(observation("a"));
    assert_eq!(occupancy_queries(&input.stored).unwrap(), vec![query("a")]);
    input.stored.current_version = 3;
    assert!(matches!(recheck(&input), Recheck::NotDispatchable(_)));
    input.stored.current_version = 2;
    input.stored.resource_tenant_id = None;
    assert_eq!(
        recheck(&input),
        Recheck::Defer(Reason::OverlapPresenceUnevaluable)
    );
    input.stored.lines[0].overlap_scope_key = None;
    input.occupancy =
        PortAnswer::Unavailable(bss_orders::domain::gate::ports::Unavailable::Missing);
    assert_eq!(recheck(&input), Recheck::Proceed);
}

#[test]
fn new_purchase_has_no_own_exemption_and_partial_conversion_checks_new_demand() {
    let mut q = query("a");
    q.own_context = None;
    let want = [("a".into(), (q.clone(), BTreeSet::from([id(4)])))]
        .into_iter()
        .collect();
    assert_eq!(
        validate_batch(&want, &[(q.clone(), decoded(observation("a")))]),
        Err(ProtectedInvalid::MembershipMismatch)
    );
    let mut value = observation("a");
    value["own_membership"] = Value::Null;
    let valid = protected(validate_batch(&want, &[(q, decoded(value))]));
    assert_eq!(valid["a"].admits(), Ok(false));

    let mut input = recheck_input(observation("a"));
    let mut second = input.stored.lines[0].clone();
    second.line_id = id(6);
    input.stored.lines.push(second);
    let q = occupancy_queries(&input.stored).unwrap().remove(0);
    let mut value = observation("a");
    value["own_membership"]["context"] = json!(q.own_context);
    value["max_concurrent_active"] = json!(0);
    input.occupancy = PortAnswer::Answered(vec![(q.clone(), decoded(value.clone()))]);
    assert_eq!(
        recheck(&input),
        Recheck::Reject {
            lines: vec![
                (id(4), Reason::OverlapCollision),
                (id(6), Reason::OverlapCollision)
            ]
        }
    );
    value["order_reserved_count"] = json!(2);
    value["own_membership"]["rights"]
        .as_array_mut()
        .unwrap()
        .push(json!({"claim_id":id(7),"line_id":id(6),"state":"order_reserved"}));
    input.occupancy = PortAnswer::Answered(vec![(q.clone(), decoded(value.clone()))]);
    assert_eq!(recheck(&input), Recheck::Proceed);
    value["own_membership"]["rights"][1]["claim_id"] = json!(id(5));
    input.occupancy = PortAnswer::Answered(vec![(q.clone(), decoded(value.clone()))]);
    assert_eq!(
        recheck(&input),
        Recheck::Defer(Reason::OverlapPresenceUnevaluable)
    );
    value["own_membership"]["rights"][1]["claim_id"] = json!(id(7));
    value["own_membership"]["rights"][1]["line_id"] = json!(id(4));
    input.occupancy = PortAnswer::Answered(vec![(q, decoded(value))]);
    assert_eq!(
        recheck(&input),
        Recheck::Defer(Reason::OverlapPresenceUnevaluable)
    );
}

#[test]
fn protected_recheck_uses_every_stored_scope_and_deduplicates_items_sharing_one_scope() {
    use bss_orders::domain::gate::answers::OverlapKeyLine;
    use bss_orders::domain::gate::item_keys::{ClassificationProvenance, ItemOverlapKey};
    let mut input = recheck_input(observation("a"));
    let line = &mut input.stored.lines[0];
    line.overlap_scope_key = None;
    line.overlap_item_keys = Some(
        [("a", 11), ("a", 12), ("b", 13)]
            .into_iter()
            .map(|(scope, item)| ItemOverlapKey {
                profile: "item_keys_v1".into(),
                selection: OverlapKeyLine {
                    line_id: id(4),
                    plan_revision_id: id(10),
                    paid_recurring_sku: id(14),
                    item_id: id(item),
                    dimension_value: None,
                    sku_version: 1,
                },
                catalog_subscription_product_key: format!("product-{item}"),
                overlap_scope_key: scope.into(),
                classification: ClassificationProvenance {
                    source: "catalog".into(),
                    reference: "policy".into(),
                    version: "1".into(),
                },
            })
            .collect(),
    );
    let queries = occupancy_queries(&input.stored).unwrap();
    assert_eq!(queries.len(), 2);
    assert!(
        queries
            .iter()
            .all(|q| q.own_context.as_ref().unwrap().line_ids == vec![id(4)])
    );
    let mut b = observation("b");
    b["own_membership"]["rights"][0]["claim_id"] = json!(id(8));
    input.occupancy = PortAnswer::Answered(vec![
        (query("a"), decoded(observation("a"))),
        (query("b"), decoded(b.clone())),
    ]);
    assert_eq!(recheck(&input), Recheck::Proceed);
    b["own_membership"]["rights"] = json!([]);
    input.occupancy = PortAnswer::Answered(vec![
        (query("a"), decoded(observation("a"))),
        (query("b"), decoded(b)),
    ]);
    assert_eq!(
        recheck(&input),
        Recheck::Reject {
            lines: vec![(id(4), Reason::OverlapCollision)]
        }
    );
}
