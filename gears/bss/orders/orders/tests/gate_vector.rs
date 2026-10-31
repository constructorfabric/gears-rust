#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
use bss_orders::domain::gate::predicates::{Origin, PREDICATES, PredicateId, Scope};
use bss_orders::domain::gate::vector::KeyError;
use bss_orders::domain::gate::vector::*;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::CatalogSelection;
use std::cmp::Ordering;
use uuid::Uuid;

#[expect(
    clippy::option_option,
    reason = "absent / selected NULL / selected value is the selection model under test"
)]
fn key(
    p: PredicateId,
    line: Option<u8>,
    item: Option<u8>,
    sel: Option<Option<&str>>,
) -> OutcomeKey {
    OutcomeKey {
        predicate: p,
        line_id: line.map(|b| Uuid::from_bytes([b; 16])),
        item_id: item.map(|b| Uuid::from_bytes([b; 16])),
        selection: sel.map_or(CatalogSelection::NotSelected, |k| {
            k.map_or(CatalogSelection::SelectedNull, |k| {
                CatalogSelection::Selected(k.to_owned())
            })
        }),
    }
}
fn outcome(k: OutcomeKey, v: Verdict, r: Option<Reason>, a: Applicability) -> Outcome {
    Outcome {
        key: k,
        verdict: v,
        reason: r,
        applicability: a,
        producer_results: vec![],
        upstream_detail: None,
    }
}

#[test]
fn registry_declares_eight_adopted_then_nine_delta_then_pin() {
    let origins: Vec<Origin> = PREDICATES.iter().map(|p| p.origin).collect();
    assert_eq!(origins.iter().filter(|o| **o == Origin::Adopted).count(), 8);
    assert_eq!(origins.iter().filter(|o| **o == Origin::Delta).count(), 9);
    assert_eq!(origins.last(), Some(&Origin::Pin));
    assert!(
        origins.windows(2).all(|w| w[0] <= w[1]),
        "adopted < delta < pin"
    );
    assert_eq!(PREDICATES[8].id, PredicateId::AxisValidity);
    assert_eq!(PREDICATES[8].token, "orders.axis-validity");
    assert_eq!(PREDICATES[8].scope, Scope::Order);
}

#[test]
fn ordering_is_predicate_then_line_then_item_then_presence_then_bytes_with_null_first() {
    let p = PredicateId::CatalogSlotCoverage;
    let ordered = vec![
        key(PredicateId::CatalogRevisionCurrent, Some(9), None, None),
        key(p, None, None, None),
        key(p, Some(1), None, None),
        key(p, Some(1), Some(1), None),
        key(p, Some(1), Some(1), Some(None)),
        key(p, Some(1), Some(1), Some(Some("a"))),
        key(p, Some(1), Some(1), Some(Some("b"))),
        key(p, Some(1), Some(2), None),
        key(p, Some(2), None, None),
    ];
    let mut shuffled = ordered.clone();
    shuffled.reverse();
    shuffled.sort();
    assert_eq!(shuffled, ordered);
}

#[test]
fn selected_null_and_literal_default_are_distinct_identities() {
    let a = key(
        PredicateId::CatalogSlotCoverage,
        Some(1),
        Some(1),
        Some(None),
    );
    let b = key(
        PredicateId::CatalogSlotCoverage,
        Some(1),
        Some(1),
        Some(Some("default")),
    );
    assert_ne!(a, b);
    let v = Vector::build(vec![
        outcome(a, Verdict::Passed, None, Applicability::Required),
        outcome(b, Verdict::Passed, None, Applicability::Required),
    ])
    .unwrap();
    assert_eq!(v.outcomes().len(), 2);
}

#[test]
fn build_rejects_duplicates_missing_reasons_and_invalid_identities() {
    let k = key(PredicateId::AxisValidity, None, None, None);
    let dup = Vector::build(vec![
        outcome(k.clone(), Verdict::Passed, None, Applicability::Required),
        outcome(k.clone(), Verdict::Passed, None, Applicability::Required),
    ]);
    assert!(matches!(dup, Err(VectorError::DuplicateIdentity(_))));
    let no_reason = Vector::build(vec![outcome(
        k.clone(),
        Verdict::Failed,
        None,
        Applicability::Required,
    )]);
    assert!(matches!(no_reason, Err(VectorError::ReasonMissing(..))));
    // A passed outcome never carries a reason (`StoredOutcome::reason` is `None` exactly then).
    let reason_on_pass = Vector::build(vec![outcome(
        k.clone(),
        Verdict::Passed,
        Some(Reason::AxisInvalid),
        Applicability::Required,
    )]);
    assert_eq!(reason_on_pass, Err(VectorError::ReasonOnPass(k)));
    // item without line; selection without item; key without selection
    let item_no_line = key(PredicateId::CatalogSlotCoverage, None, Some(1), None);
    assert_eq!(item_no_line.validate(), Err(KeyError::ComponentWithoutLine));
    assert_eq!(
        key(PredicateId::CatalogSlotCoverage, Some(1), None, Some(None)).validate(),
        Err(KeyError::SelectionWithoutItem)
    );
    let invalid = Vector::build(vec![outcome(
        item_no_line,
        Verdict::Passed,
        None,
        Applicability::Required,
    )]);
    assert!(matches!(
        invalid,
        Err(VectorError::InvalidIdentity(
            _,
            KeyError::ComponentWithoutLine
        ))
    ));
    // A key without a selection is no longer representable; its stored members refuse to
    // decode (C11, `selection_order_matches_the_frozen_tuple_order`).
    let mut k2 = serde_json::to_value(key(
        PredicateId::CatalogSlotCoverage,
        Some(1),
        Some(1),
        None,
    ))
    .unwrap();
    k2["catalog_scope_key"] = serde_json::json!("x");
    let refused = serde_json::from_value::<OutcomeKey>(k2).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains(&KeyError::KeyWithoutSelection.to_string()),
        "{refused}"
    );
}

#[test]
fn primary_is_first_unavailable_in_the_blocking_set_else_first_failed() {
    // (d) No definitive local failure (D-234): the D-195 rule holds.
    let v = Vector::build(vec![
        outcome(
            key(PredicateId::CatalogItemMembership, Some(1), Some(1), None),
            Verdict::Failed,
            Some(Reason::CatalogPredicateFailed),
            Applicability::Required,
        ),
        outcome(
            key(PredicateId::MarketConsistency, Some(1), None, None),
            Verdict::Failed,
            Some(Reason::MarketInconsistent),
            Applicability::Required,
        ),
        outcome(
            key(PredicateId::OverlapUniqueness, Some(1), None, None),
            Verdict::Unevaluable,
            Some(Reason::OverlapPresenceUnevaluable),
            Applicability::Required,
        ),
        outcome(
            key(PredicateId::PinComposition, Some(1), None, None),
            Verdict::Unevaluable,
            Some(Reason::CatalogPinCompositionUnavailable),
            Applicability::TcvOnly,
        ),
    ])
    .unwrap();
    assert_eq!(
        v.primary(Operation::Submit).unwrap().reason,
        Some(Reason::OverlapPresenceUnevaluable)
    );
    assert_eq!(v.failures().count(), 4);
    let only_failed = Vector::build(vec![
        outcome(
            key(PredicateId::MarketConsistency, Some(2), None, None),
            Verdict::Failed,
            Some(Reason::MarketInconsistent),
            Applicability::Required,
        ),
        outcome(
            key(PredicateId::CatalogItemMembership, Some(1), Some(1), None),
            Verdict::Failed,
            Some(Reason::CatalogPredicateFailed),
            Applicability::Required,
        ),
    ])
    .unwrap();
    assert_eq!(
        only_failed.primary(Operation::Submit).unwrap().reason,
        Some(Reason::CatalogPredicateFailed),
        "declared predicate order, not insertion order"
    );
}

#[test]
fn definitive_local_flags_exactly_the_five_d230_predicates() {
    let local: Vec<PredicateId> = PREDICATES
        .iter()
        .filter(|p| p.definitive_local)
        .map(|p| p.id)
        .collect();
    assert_eq!(
        local,
        [
            PredicateId::AxisValidity,
            PredicateId::QuantityFloor,
            PredicateId::ReferenceResolution,
            PredicateId::SingleCurrency,
            PredicateId::DatesResolvable,
        ]
    );
}

#[test]
fn d230_failed_local_refusal_outranks_an_earlier_unevaluable_catalog_row() {
    // (a) A failed quantity floor on line 2 beats an unevaluable catalog row on line 1.
    let v = Vector::build(vec![
        outcome(
            key(PredicateId::CatalogRevisionAvailable, Some(1), None, None),
            Verdict::Unevaluable,
            Some(Reason::CatalogPredicatesUnavailable),
            Applicability::Required,
        ),
        outcome(
            key(PredicateId::QuantityFloor, Some(2), None, None),
            Verdict::Failed,
            Some(Reason::QuantityBelowFloor),
            Applicability::Required,
        ),
    ])
    .unwrap();
    for op in [Operation::Submit, Operation::Preview, Operation::Amendment] {
        let p = v.primary(op).unwrap();
        assert_eq!(p.key.predicate, PredicateId::QuantityFloor, "{op:?}");
        assert_eq!(p.reason, Some(Reason::QuantityBelowFloor), "{op:?}");
    }
}

#[test]
fn d230_failed_catalog_row_keeps_the_d195_order() {
    // (b) A failed provider-backed row never outranks an unevaluable one (D-195 kept).
    let v = Vector::build(vec![
        outcome(
            key(PredicateId::CatalogItemMembership, Some(1), Some(1), None),
            Verdict::Failed,
            Some(Reason::CatalogPredicateFailed),
            Applicability::Required,
        ),
        outcome(
            key(PredicateId::CatalogEligibilityRules, Some(1), None, None),
            Verdict::Unevaluable,
            Some(Reason::CatalogPredicatesUnavailable),
            Applicability::Required,
        ),
    ])
    .unwrap();
    assert_eq!(
        v.primary(Operation::Submit).unwrap().reason,
        Some(Reason::CatalogPredicatesUnavailable)
    );
}

#[test]
fn d230_withheld_row_with_predicate_1_reason_outranks_catalog_unavailability() {
    // (c) An unevaluable withheld row carrying `payer-relationship-unconfirmed` (D-232 item 6)
    // beats an earlier unevaluable catalog row.
    let v = Vector::build(vec![
        outcome(
            key(
                PredicateId::CatalogDimensionMarket,
                Some(1),
                Some(1),
                Some(None),
            ),
            Verdict::Unevaluable,
            Some(Reason::CatalogPredicatesUnavailable),
            Applicability::Required,
        ),
        outcome(
            key(PredicateId::ContractActive, None, None, None),
            Verdict::Unevaluable,
            Some(Reason::PayerRelationshipUnconfirmed),
            Applicability::Required,
        ),
    ])
    .unwrap();
    for op in [Operation::Submit, Operation::Preview] {
        let p = v.primary(op).unwrap();
        assert_eq!(p.key.predicate, PredicateId::ContractActive, "{op:?}");
        assert_eq!(p.reason, Some(Reason::PayerRelationshipUnconfirmed));
    }
}

#[test]
fn tcv_only_blocks_submit_but_not_preview() {
    let o = outcome(
        key(PredicateId::PinComposition, Some(1), None, None),
        Verdict::Unevaluable,
        Some(Reason::CatalogPinCompositionUnavailable),
        Applicability::TcvOnly,
    );
    assert!(blocking(Operation::Submit, &o));
    assert!(!blocking(Operation::Preview, &o));
    let v = Vector::build(vec![o]).unwrap();
    assert!(
        v.primary(Operation::Preview).is_none(),
        "a successful Preview with tcvWithheld"
    );
}

#[test]
fn registry_index_matches_declaration_position() {
    for (i, p) in PREDICATES.iter().enumerate() {
        assert_eq!(p.id.index(), i);
        assert_eq!(p.id.spec().id, p.id);
    }
}

#[test]
fn preview_forecast_never_blocks_and_tcv_only_blocks_amendment() {
    let mk = |a| {
        outcome(
            key(PredicateId::PinComposition, Some(1), None, None),
            Verdict::Failed,
            Some(Reason::PinUnresolvable),
            a,
        )
    };
    let forecast = mk(Applicability::PreviewForecast);
    for op in [Operation::Preview, Operation::Submit, Operation::Amendment] {
        assert!(!blocking(op, &forecast), "{op:?}");
    }
    assert!(blocking(Operation::Amendment, &mk(Applicability::TcvOnly)));
}

#[test]
fn predicate_id_serializes_as_its_stored_token() {
    let s = serde_json::to_string(&PredicateId::AxisValidity).unwrap();
    assert_eq!(s, "\"orders.axis-validity\"");
    assert_eq!(
        serde_json::from_str::<PredicateId>(&s).unwrap(),
        PredicateId::AxisValidity
    );
    for p in &PREDICATES {
        let j = serde_json::to_string(&p.id).unwrap();
        assert_eq!(serde_json::from_str::<PredicateId>(&j).unwrap(), p.id);
    }
    assert!(serde_json::from_str::<PredicateId>("\"orders.nope\"").is_err());
}

/// S3-10: the stored projection the diagnostics writer maps 1:1 into `gate_outcome` rows.
#[test]
fn stored_projection_keeps_vector_order_and_its_evidence_fields_carry_no_money_or_credentials() {
    use bss_orders::domain::gate::evidence::UpstreamDetail;
    let producer = ProducerResult {
        producer: "pricing".into(),
        profile: "catalog".into(),
        predicate: "revision-current".into(),
        verdict: Verdict::Unevaluable,
        reason: Some("provider-unavailable".into()),
        detail: Some(UpstreamDetail {
            revision_id: Some(Uuid::from_bytes([7; 16])),
            upstream_code: Some("unavailable".into()),
            ..UpstreamDetail::default()
        }),
    };
    let mut remote = outcome(
        key(PredicateId::CatalogRevisionCurrent, Some(1), None, None),
        Verdict::Unevaluable,
        Some(Reason::PricingRevisionUnavailable),
        Applicability::Required,
    );
    remote.producer_results = vec![producer];
    remote.upstream_detail = Some(UpstreamDetail {
        plan_id: Some(Uuid::from_bytes([8; 16])),
        ..UpstreamDetail::default()
    });
    let evidence = |predicate: &str, code: &str| ProducerResult {
        producer: "pricing".into(),
        profile: "catalog".into(),
        predicate: predicate.into(),
        verdict: Verdict::Unevaluable,
        reason: Some("provider-unavailable".into()),
        detail: Some(UpstreamDetail {
            upstream_code: Some(code.into()),
            ..UpstreamDetail::default()
        }),
    };
    let detail = || {
        Some(UpstreamDetail {
            sku_id: Some(Uuid::from_bytes([9; 16])),
            upstream_code: Some("x".into()),
            ..UpstreamDetail::default()
        })
    };
    // The rows whose evidence could carry an amount: a TCV-only row and an indicative-tax row
    // (the registry has no tax predicate; the vocabulary's tax reason rides a forecast row).
    let mut tcv = outcome(
        key(PredicateId::PinComposition, Some(3), None, None),
        Verdict::Unevaluable,
        Some(Reason::CatalogPinCompositionUnavailable),
        Applicability::TcvOnly,
    );
    tcv.producer_results = vec![evidence("pin-composition", "x")];
    tcv.upstream_detail = detail();
    let mut tax = outcome(
        key(PredicateId::PinComposition, Some(4), None, None),
        Verdict::Unevaluable,
        Some(Reason::IndicativeTaxUnavailable),
        Applicability::PreviewForecast,
    );
    tax.producer_results = vec![evidence("pin-composition", "unavailable")];
    tax.upstream_detail = detail();
    let v = Vector::build(vec![
        tcv,
        tax,
        outcome(
            key(PredicateId::QuantityFloor, Some(2), None, None),
            Verdict::Failed,
            Some(Reason::QuantityBelowFloor),
            Applicability::Required,
        ),
        outcome(
            key(PredicateId::AxisValidity, None, None, None),
            Verdict::Passed,
            None,
            Applicability::Required,
        ),
        outcome(
            key(
                PredicateId::CatalogSlotCoverage,
                Some(1),
                Some(1),
                Some(Some("eu")),
            ),
            Verdict::Passed,
            None,
            Applicability::PreviewForecast,
        ),
        remote,
    ])
    .unwrap();
    let rows = v.stored_projection().unwrap();
    assert_eq!(rows.len(), v.outcomes().len());
    for (row, o) in rows.iter().zip(v.outcomes()) {
        assert_eq!(row.predicate, o.key.predicate.token(), "vector order");
        assert_eq!(row.line_id, o.key.line_id);
        assert_eq!(row.item_id, o.key.item_id);
        assert_eq!(
            row.has_catalog_selection,
            o.key.selection.has_catalog_selection()
        );
        assert_eq!(
            row.catalog_scope_key.as_deref(),
            o.key.selection.catalog_scope_key()
        );
        assert_eq!(row.verdict, o.verdict.token());
        assert_eq!(row.reason, o.reason.map(Reason::token));
        assert_eq!(row.applicability, o.applicability.token());
        if o.verdict == Verdict::Passed {
            assert_eq!(row.reason, None, "a passed row carries no reason");
        }
        if o.producer_results.is_empty() {
            assert_eq!(row.producer_results, serde_json::json!([]), "local row");
        }
        assert_eq!(row.upstream_detail.is_some(), o.upstream_detail.is_some());
        let evidence_fields = format!(
            "{} {} {}",
            row.producer_results,
            serde_json::to_string(&row.upstream_detail).unwrap(),
            serde_json::to_string(&row.catalog_scope_key).unwrap()
        )
        .to_ascii_lowercase();
        for forbidden in ["total", "tcv", "tax", "token", "bearer", "amount"] {
            assert!(
                !evidence_fields.contains(forbidden),
                "{forbidden} in stored evidence: {evidence_fields}"
            );
        }
    }
    let remote_row = rows.iter().find(|r| r.upstream_detail.is_some()).unwrap();
    assert_eq!(
        remote_row.producer_results[0]["producer"],
        serde_json::json!("pricing")
    );
}

/// C11: frozen vectors are sorted by `OutcomeKey`, so the selection must order exactly as the
/// `(has_catalog_selection, catalog_scope_key)` tuple did: not selected, then selected NULL,
/// then selected values by bytes. The key's serde form stays the five flat members with
/// explicit nulls, and a key without a selection refuses to decode.
#[test]
fn selection_order_matches_the_frozen_tuple_order() {
    let k = |sel| key(PredicateId::CatalogSlotCoverage, Some(1), Some(1), sel);
    let ordered = [
        k(None),
        k(Some(None)),
        k(Some(Some("a"))),
        k(Some(Some("b"))),
    ];
    for w in ordered.windows(2) {
        assert!(w[0] < w[1], "{:?} < {:?}", w[0], w[1]);
    }
    let mut shuffled = vec![
        ordered[3].clone(),
        ordered[1].clone(),
        ordered[0].clone(),
        ordered[2].clone(),
    ];
    shuffled.sort();
    assert_eq!(shuffled, ordered);
    let wire = serde_json::to_value(&ordered[1]).unwrap();
    assert_eq!(
        wire,
        serde_json::json!({
            "predicate": "catalog.slot-coverage",
            "line_id": Uuid::from_bytes([1; 16]),
            "item_id": Uuid::from_bytes([1; 16]),
            "has_catalog_selection": true,
            "catalog_scope_key": null
        })
    );
    assert_eq!(
        serde_json::from_value::<OutcomeKey>(wire.clone()).unwrap(),
        ordered[1]
    );
    let mut keyless = wire;
    keyless["has_catalog_selection"] = serde_json::json!(false);
    keyless["catalog_scope_key"] = serde_json::json!("x");
    assert!(serde_json::from_value::<OutcomeKey>(keyless).is_err());
}

/// C5 (n80): `Ord` agrees with `Eq`: two keys differing only in one field are never `Equal`,
/// whichever field it is (the `cmp` destructures `Self`, so a new field fails to compile there).
#[test]
fn outcome_key_order_agrees_with_eq() {
    let base = key(
        PredicateId::CatalogSlotCoverage,
        Some(1),
        Some(1),
        Some(None),
    );
    let variants = [
        key(
            PredicateId::CatalogSkuSellable,
            Some(1),
            Some(1),
            Some(None),
        ),
        key(
            PredicateId::CatalogSlotCoverage,
            Some(2),
            Some(1),
            Some(None),
        ),
        key(
            PredicateId::CatalogSlotCoverage,
            Some(1),
            Some(2),
            Some(None),
        ),
        key(PredicateId::CatalogSlotCoverage, Some(1), Some(1), None),
        key(
            PredicateId::CatalogSlotCoverage,
            Some(1),
            Some(1),
            Some(Some("a")),
        ),
    ];
    assert_eq!(base.cmp(&base.clone()), Ordering::Equal);
    for v in &variants {
        assert_ne!(base, *v);
        assert_ne!(base.cmp(v), Ordering::Equal, "{v:?}");
        assert_eq!(base.cmp(v), v.cmp(&base).reverse(), "{v:?}");
    }
}
