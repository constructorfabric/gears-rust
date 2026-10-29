#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
use bss_orders::domain::gate::answers::*;
use bss_orders::domain::gate::pricing_map::SelectedItem;
use bss_orders::domain::gate::vector::{ProducerResult, Verdict};
use bss_orders_sdk::gate_report::CatalogSelection;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use time::{Date, Month, OffsetDateTime};
use uuid::Uuid;

fn id(b: u8) -> Uuid {
    Uuid::from_bytes([b; 16])
}
fn rt<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
    let j = serde_json::to_value(v).unwrap();
    let back: T = serde_json::from_value(j).unwrap();
    assert_eq!(&back, v);
}
fn money(n: &str, d: &str) -> ExactMoney {
    ExactMoney::new(n, d, "EUR", 2).unwrap()
}
fn amount() -> Amount {
    Amount {
        exact: money("-1", "3"),
        display: "-0.33".into(),
    }
}
fn at() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
}
fn day() -> Date {
    Date::from_calendar_date(2026, Month::October, 22).unwrap()
}
fn totals() -> Totals {
    Totals {
        gross: amount(),
        net: amount(),
        discount: Availability::Unavailable,
    }
}

#[test]
fn identity_contract_round_trip() {
    rt(&PayerProfileQuery {
        payer_tenant_id: id(1),
        seller_tenant_id: id(2),
        resource_tenant_id: id(3),
    });
    rt(&PayerProfile {
        axes_valid: true,
        currency: "EUR".into(),
        region: "EU".into(),
        in_relationship_with_seller: true,
        in_relationship_with_resource: false,
        profile_version: "v1".into(),
    });
    rt(&ContractQuery {
        contract_id: id(1),
        payer_tenant_id: id(2),
        seller_tenant_id: id(3),
    });
    for status in [ContractStatus::Active, ContractStatus::Inactive] {
        rt(&ContractFacts {
            status,
            payer_eligible: true,
            effective_at: Some(at()),
            acceptance_required: Some(false),
            version: "3".into(),
        });
    }
    rt(&ContractFacts {
        status: ContractStatus::Active,
        payer_eligible: false,
        effective_at: None,
        acceptance_required: None,
        version: "1".into(),
    });
}

#[test]
fn overlap_and_occupancy_round_trip() {
    rt(&OverlapKeyLine {
        item_id: id(9),
        dimension_value: None,
        sku_version: 1,
        line_id: id(1),
        plan_revision_id: id(2),
        paid_recurring_sku: id(3),
    });
    rt(&OverlapKey {
        item_key: None,
        line_id: id(1),
        catalog_subscription_product_key: "k".into(),
        overlap_scope_key: "s".into(),
    });
    rt(&OccupancyQuery {
        own_context: None,
        payer_tenant_id: id(1),
        resource_tenant_id: Some(id(2)),
        overlap_scope_key: "s".into(),
    });
    rt(&OccupancyQuery {
        own_context: None,
        payer_tenant_id: id(1),
        resource_tenant_id: None,
        overlap_scope_key: "s".into(),
    });
    for p in [
        LimitProvenance::Catalog,
        LimitProvenance::Contract,
        LimitProvenance::PlatformDefault,
    ] {
        rt(&Occupancy {
            active_count: 2,
            count_scope: bss_orders::domain::gate::answers::OccupancyCountScope::PerPayer,
            max_concurrent_active: MaxConcurrentActive::Limit(3),
            provenance: p,
        });
    }
    rt(&Occupancy {
        active_count: 0,
        count_scope: bss_orders::domain::gate::answers::OccupancyCountScope::PerPayer,
        max_concurrent_active: MaxConcurrentActive::Unlimited,
        provenance: LimitProvenance::Catalog,
    });
}

#[test]
fn max_concurrent_active_encoding() {
    assert_eq!(
        serde_json::to_value(MaxConcurrentActive::Unlimited).unwrap(),
        json!(null)
    );
    assert_eq!(
        serde_json::to_value(MaxConcurrentActive::Limit(3)).unwrap(),
        json!(3)
    );
    assert_eq!(
        serde_json::from_value::<MaxConcurrentActive>(json!(null)).unwrap(),
        MaxConcurrentActive::Unlimited
    );
    assert_eq!(
        serde_json::from_value::<MaxConcurrentActive>(json!(3)).unwrap(),
        MaxConcurrentActive::Limit(3)
    );
    assert!(serde_json::from_value::<MaxConcurrentActive>(json!(-1)).is_err());
    assert!(serde_json::from_value::<MaxConcurrentActive>(json!("3")).is_err());
}

#[test]
fn occupancy_requires_count_limit_provenance_and_scope() {
    for field in [
        "max_concurrent_active",
        "active_count",
        "provenance",
        "count_scope",
    ] {
        let mut wire = json!({
            "active_count": 0,
            "count_scope": "per_payer",
            "max_concurrent_active": null,
            "provenance": "catalog"
        });
        wire.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<Occupancy>(wire).is_err(),
            "missing {field} must not become an unlimited occupancy answer"
        );
    }
}

#[test]
fn occupancy_preserves_explicit_limits_and_rejects_malformed_limits() {
    for (wire_limit, expected) in [
        (json!(null), MaxConcurrentActive::Unlimited),
        (json!(0), MaxConcurrentActive::Limit(0)),
        (json!(3), MaxConcurrentActive::Limit(3)),
    ] {
        let occupancy: Occupancy = serde_json::from_value(json!({
            "active_count": 0,
            "count_scope": "per_payer",
            "max_concurrent_active": wire_limit,
            "provenance": "contract"
        }))
        .unwrap();
        assert_eq!(occupancy.max_concurrent_active, expected);
        assert_eq!(occupancy.active_count, 0);
        assert_eq!(occupancy.provenance, LimitProvenance::Contract);
    }
    for wire_limit in [json!(-1), json!("3"), json!(1.5), json!(true)] {
        assert!(
            serde_json::from_value::<Occupancy>(json!({
                "active_count": 0,
                "count_scope": "per_payer",
                "max_concurrent_active": wire_limit,
                "provenance": "catalog"
            }))
            .is_err()
        );
    }
}

#[test]
fn malformed_occupancy_uses_existing_unevaluable_recheck_path() {
    use bss_orders::domain::gate::ports::{PortAnswer, Unavailable};
    use bss_orders::domain::gate::recheck::{
        Recheck, RecheckInput, StoredLine, StoredVersion, recheck,
    };
    use bss_orders_sdk::catalog::{OrderState, Reason};

    // No producer adapter is delivered yet. Decode its malformed answer at that boundary,
    // using the existing typed unavailability rather than inventing an unlimited value.
    for wire in [
        json!({"active_count": 0, "count_scope": "per_payer", "provenance": "catalog"}),
        json!({"active_count": 0, "max_concurrent_active": 1, "provenance": "catalog"}),
        json!({"active_count": 0, "max_concurrent_active": 1, "provenance": "catalog", "count_scope": "unknown"}),
    ] {
        let decoded = serde_json::from_value::<Occupancy>(wire);
        let occupancy = match decoded {
            Ok(answer) => PortAnswer::Answered(vec![(
                OccupancyQuery {
                    own_context: None,
                    payer_tenant_id: id(1),
                    resource_tenant_id: None,
                    overlap_scope_key: "scope".into(),
                },
                answer.into(),
            )]),
            Err(_) => PortAnswer::Unavailable(Unavailable::Malformed),
        };
        let input = RecheckInput {
            requested_version: 1,
            stored: StoredVersion {
                order_id: None,
                resource_tenant_id: None,
                state: OrderState::Submitted,
                current_version: 1,
                market_currency: "EUR".into(),
                market_region: "EU".into(),
                payer_tenant_id: id(1),
                lines: vec![StoredLine {
                    overlap_item_keys: None,
                    catalog_subscription_product_key: None,
                    line_id: id(2),
                    overlap_scope_key: Some("scope".into()),
                    binding_deadline: None,
                }],
            },
            identity: PortAnswer::Answered(PayerProfile {
                axes_valid: true,
                currency: "EUR".into(),
                region: "EU".into(),
                in_relationship_with_seller: true,
                in_relationship_with_resource: true,
                profile_version: "v1".into(),
            }),
            occupancy,
            now: at(),
        };
        assert_eq!(
            recheck(&input),
            Recheck::Defer(Reason::OverlapPresenceUnevaluable)
        );
    }
}

#[test]
fn terms_and_policy_round_trip() {
    for intent in [
        TermIntentInput::Fixed { months: 12 },
        TermIntentInput::Rolling,
        TermIntentInput::SellerPolicy,
    ] {
        rt(&BillingTermsQuery {
            line_id: id(1),
            payer_tenant_id: id(2),
            seller_tenant_id: id(3),
            plan_revision_id: id(4),
            intent,
            contract_id: Some(id(5)),
        });
    }
    for provenance in [
        TermsProvenance::ExplicitOrder,
        TermsProvenance::Contract {
            contract_id: id(1),
            version: "2".into(),
        },
        TermsProvenance::SellerPolicy {
            version: "3".into(),
        },
        TermsProvenance::PlatformDefault,
    ] {
        rt(&ResolvedTerms {
            line_id: id(1),
            billing_terms: BillingTermsRef {
                digest: "sha256:ab".into(),
            },
            term: EvaluationTerm::FixedPeriods { count: 12 },
            earliest_start: Some(at()),
            provenance,
            snapshot: None,
        });
    }
    for source in [
        TermsSnapshotSource::ExplicitOrder,
        TermsSnapshotSource::SellerPolicy {
            id: id(9),
            version: 3,
        },
    ] {
        rt(&ResolvedTerms {
            line_id: id(1),
            billing_terms: BillingTermsRef {
                digest: "ab".repeat(32),
            },
            term: EvaluationTerm::Rolling,
            earliest_start: None,
            provenance: TermsProvenance::ExplicitOrder,
            snapshot: Some(BillingTermsSnapshot {
                schema_version: 1,
                cycle: EvaluationCycle::Month,
                anchor: TermsAnchor::SubscriptionStart,
                anchor_at: at(),
                timezone: "UTC".into(),
                source,
            }),
        });
    }
    rt(&AcceptancePolicyQuery {
        seller_tenant_id: id(1),
    });
    rt(&AcceptancePolicy {
        version: "1".into(),
        hold_seconds: 900,
    });
}

#[test]
fn assessment_round_trip() {
    let line = AssessmentLine {
        line_id: id(1),
        plan_id: id(2),
        revision_id: id(3),
        selections: vec![SelectedItem {
            item_id: id(4),
            quantity: "1".into(),
            selected_dim_value: None,
        }],
        market: Some(Market {
            currency: "EUR".into(),
            region: "EU".into(),
        }),
        term: Some(EvaluationTerm::Rolling),
        cycle: Some(EvaluationCycle::Year),
    };
    let q = AssessmentQuery {
        assessment_id: id(9),
        seller_tenant_id: id(8),
        resolve_date: day(),
        profile: "orders-gate/1".into(),
        lines: vec![line],
    };
    rt(&q);
    assert_eq!(
        serde_json::to_value(&q).unwrap()["resolve_date"],
        json!("2026-10-22")
    );
    rt(&AssessmentAnswer {
        profile: "p".into(),
        observed_at: at(),
        results: vec![ProducerResultWithKey {
            line_id: id(1),
            item_id: Some(id(4)),
            selection: CatalogSelection::Selected("c".into()),
            result: ProducerResult {
                producer: "pricing".into(),
                profile: "p".into(),
                predicate: "x".into(),
                verdict: Verdict::Passed,
                reason: None,
                detail: None,
            },
        }],
    });
}

/// C11: a producer answer keyed `has_catalog_selection: false` with a scope key names no
/// selection, so it never decodes into a keyed result (malformed coverage, D-195); the three
/// valid two-member spellings decode and re-encode byte for byte.
#[test]
fn producer_result_with_a_key_but_no_selection_refuses() {
    let entry = |members: serde_json::Value| {
        let mut wire = json!({
            "line_id": id(1),
            "item_id": id(4),
            "result": {
                "producer": "catalog",
                "profile": "p",
                "predicate": "catalog.dimension-market",
                "verdict": "passed",
                "reason": null,
                "detail": null
            }
        });
        wire.as_object_mut()
            .unwrap()
            .extend(members.as_object().unwrap().clone());
        wire
    };
    assert!(
        serde_json::from_value::<ProducerResultWithKey>(entry(
            json!({"has_catalog_selection": false, "catalog_scope_key": "c"})
        ))
        .is_err()
    );
    for members in [
        json!({"has_catalog_selection": false}),
        json!({"has_catalog_selection": true}),
        json!({"has_catalog_selection": true, "catalog_scope_key": "c"}),
    ] {
        let wire = entry(members);
        let r: ProducerResultWithKey = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap(), wire);
    }
}

#[test]
fn evaluation_round_trip() {
    for t in [
        EvaluationTerm::Rolling,
        EvaluationTerm::FixedPeriods { count: 3 },
    ] {
        rt(&t);
    }
    for c in [EvaluationCycle::Month, EvaluationCycle::Year] {
        rt(&c);
    }
    let req = OrderEvaluationRequest {
        tenant_axes: None,
        assessed_at: None,
        acceptance_policy: None,
        assessment_id: id(1),
        resolve_date: day(),
        market: Market {
            currency: "EUR".into(),
            region: "EU".into(),
        },
        contract_id: None,
        lines: vec![EvaluationLine {
            plan_id: None,
            start_at: None,
            resolved_terms: None,
            line_id: id(2),
            plan_revision_id: id(3),
            term: EvaluationTerm::Rolling,
            cycle: EvaluationCycle::Month,
            items: vec![EvaluationItem {
                item_id: id(4),
                quantity: "2".into(),
                chains: vec![EvaluationChain {
                    dim_value: Some("v".into()),
                    binding: EvaluationBinding {
                        native_binding: None,
                        item_id: id(4),
                        price_book_entry_id: id(5),
                        price_id: id(8),
                        money_digest: "ab".repeat(32),
                        currency: "EUR".into(),
                        currency_scale: 2,
                        sku_id: id(6),
                        sku_version: 7,
                        dimension_key: Some("k".into()),
                        dimension_value: Some("v".into()),
                        kind: EvaluationChargeKind::Recurring,
                        recurring_period: Some(EvaluationCycle::Month),
                        via_default: true,
                    },
                }],
            }],
        }],
    };
    rt(&req);
    rt(&LineEvaluation {
        line_id: id(2),
        totals: totals(),
        usage_flagged: true,
        committed_components: None,
        declaration: None,
        usage_declaration: None,
    });
    let ev = OrderEvaluation {
        assessment_id: id(1),
        estimate: true,
        currency: "EUR".into(),
        currency_scale: 2,
        lines: vec![LineEvaluation {
            line_id: id(2),
            totals: totals(),
            usage_flagged: false,
            committed_components: None,
            declaration: None,
            usage_declaration: None,
        }],
        order_committed_components: None,
        order_tcv_carrier: None,
        order_totals: Totals {
            gross: amount(),
            net: amount(),
            discount: Availability::Available(amount()),
        },
        order_declaration: None,
        order_usage_declaration: None,
        tcv: Some(Tcv {
            exact: money("0", "1"),
            display: "0.00".into(),
            posting_estimate: "0".into(),
            basis: None,
            evidence: None,
        }),
        promotion_status: Availability::Unavailable,
        request_digest: "d".into(),
    };
    rt(&ev);
    let mut none = ev;
    none.tcv = None;
    rt(&none);
}

#[test]
fn tax_round_trip() {
    rt(&TaxQuery {
        payer_tenant_id: id(1),
        seller_tenant_id: id(2),
        lines: vec![LineAmount {
            line_id: id(3),
            amount: amount(),
        }],
    });
    rt(&IndicativeTax {
        per_line: vec![LineAmount {
            line_id: id(3),
            amount: amount(),
        }],
        total: amount(),
        explicit_no_tax: false,
    });
}

#[test]
fn indicative_tax_line_tuples_are_refused() {
    let amt = serde_json::to_value(amount()).unwrap();
    let tuple = json!([id(3), amt]);
    assert!(serde_json::from_value::<LineAmount>(tuple.clone()).is_err());
    assert!(
        serde_json::from_value::<IndicativeTax>(
            json!({"per_line": [tuple], "total": amt, "explicit_no_tax": false})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<TaxQuery>(
            json!({"payer_tenant_id": id(1), "seller_tenant_id": id(2), "lines": [tuple]})
        )
        .is_err()
    );
}

#[test]
fn indicative_tax_lines_are_named_objects() {
    let tax = IndicativeTax {
        per_line: vec![LineAmount {
            line_id: id(3),
            amount: amount(),
        }],
        total: amount(),
        explicit_no_tax: false,
    };
    let wire = serde_json::to_value(&tax).unwrap();
    assert_eq!(wire["per_line"][0]["line_id"], json!(id(3)));
    assert_eq!(wire["per_line"][0]["amount"]["display"], json!("-0.33"));
    assert!(wire["per_line"][0].is_object());
    let mut unknown = wire;
    unknown["per_line"][0]["extra"] = json!(1);
    assert!(serde_json::from_value::<IndicativeTax>(unknown).is_err());
    rt(&tax);
}

#[test]
fn availability_encoding() {
    assert_eq!(
        serde_json::to_value(Availability::Unavailable).unwrap(),
        json!({"status": "unavailable"})
    );
    let a = serde_json::to_value(Availability::Available(amount())).unwrap();
    assert_eq!(a["status"], json!("available"));
    assert_eq!(a["display"], json!("-0.33"));
    assert_eq!(a["exact"]["numerator"], json!("-1"));
    rt(&Availability::Available(amount()));
    rt(&Availability::Unavailable);
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "json! yields owned values at every call site"
)]
fn exact(n: serde_json::Value, d: serde_json::Value) -> Result<ExactMoney, serde_json::Error> {
    serde_json::from_value(
        json!({"numerator": n, "denominator": d, "currency": "EUR", "currency_scale": 2}),
    )
}

#[test]
fn exact_money_accepts_valid() {
    assert!(exact(json!("-1"), json!("3")).is_ok());
    assert!(exact(json!("0"), json!("1")).is_ok());
    assert!(exact(json!("123456789012345678901234567890"), json!("11")).is_ok());
    rt(&money("-1", "3"));
}

#[test]
fn exact_money_rejects_invalid() {
    assert!(exact(json!("1e3"), json!("1")).is_err());
    assert!(exact(json!(1), json!("1")).is_err());
    assert!(exact(json!("1"), json!(1)).is_err());
    assert!(exact(json!("01"), json!("1")).is_err());
    assert!(exact(json!("-01"), json!("1")).is_err());
    assert!(exact(json!("1"), json!("0")).is_err());
    assert!(exact(json!("1"), json!("03")).is_err());
    assert!(exact(json!("1"), json!("-3")).is_err());
    assert!(exact(json!(""), json!("1")).is_err());
    assert!(exact(json!("1"), json!("")).is_err());
    assert!(exact(json!("-"), json!("1")).is_err());
    assert!(exact(json!("-0"), json!("1")).is_err());
    assert!(exact(json!("+1"), json!("1")).is_err());
    assert!(exact(json!(" 1"), json!("1")).is_err());
    assert!(ExactMoney::new("1e3", "1", "EUR", 2).is_err());
    assert!(ExactMoney::new("1", "0", "EUR", 2).is_err());
}

#[test]
fn max_concurrent_active_absent_is_a_decode_error_without_field_attributes() {
    #[derive(serde::Deserialize, Debug)]
    struct S {
        m: MaxConcurrentActive,
    }
    assert!(serde_json::from_value::<S>(json!({})).is_err());
    assert_eq!(
        serde_json::from_value::<S>(json!({"m": null})).unwrap().m,
        MaxConcurrentActive::Unlimited
    );
    assert_eq!(
        serde_json::from_value::<S>(json!({"m": 3})).unwrap().m,
        MaxConcurrentActive::Limit(3)
    );
    assert!(serde_json::from_value::<S>(json!({"m": 4_294_967_296_u64})).is_err());
    assert!(serde_json::from_value::<S>(json!({"m": -1})).is_err());
    assert!(serde_json::from_value::<S>(json!({"m": "3"})).is_err());
}

#[test]
fn exact_money_rejects_noncanonical_parts() {
    for (n, d, expected) in [
        ("1e3", "1", ExactMoneyError::Numerator),
        ("01", "1", ExactMoneyError::Numerator),
        ("-0", "1", ExactMoneyError::Numerator),
        ("+1", "1", ExactMoneyError::Numerator),
        ("", "1", ExactMoneyError::Numerator),
        ("1", "0", ExactMoneyError::Denominator),
        ("1", "03", ExactMoneyError::Denominator),
        ("1", "-3", ExactMoneyError::Denominator),
        ("1", "", ExactMoneyError::Denominator),
        ("2", "4", ExactMoneyError::NotReduced),
        ("-6", "9", ExactMoneyError::NotReduced),
        ("0", "2", ExactMoneyError::NotReduced),
    ] {
        assert_eq!(ExactMoney::new(n, d, "EUR", 2), Err(expected), "{n}/{d}");
        assert!(
            exact(json!(n), json!(d)).is_err(),
            "serde must agree for {n}/{d}"
        );
    }
    let ok = ExactMoney::new("-1", "3", "EUR", 2).unwrap();
    assert_eq!(
        (
            ok.numerator(),
            ok.denominator(),
            ok.currency(),
            ok.currency_scale()
        ),
        ("-1", "3", "EUR", 2)
    );
}

#[test]
fn occupancy_refuses_missing_or_unknown_count_scope() {
    for scope in [None, Some(json!("unknown")), Some(json!(null))] {
        let mut wire =
            json!({"active_count": 0, "max_concurrent_active": 1, "provenance": "catalog"});
        if let Some(scope) = scope {
            wire["count_scope"] = scope;
        }
        assert!(
            serde_json::from_value::<Occupancy>(wire).is_err(),
            "missing/unknown count scope must fail closed"
        );
    }
}

#[test]
fn observation_never_decodes_unknown_or_undiscriminated_protected_evidence_as_legacy() {
    for extra in [
        json!({"profile":"unknown"}),
        json!({"retained_suspended_count":1}),
        json!({"own_membership":null}),
    ] {
        let mut value = json!({"active_count":0,"count_scope":"full_tuple","max_concurrent_active":null,"provenance":"catalog"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(serde_json::from_value::<Occupancy>(value).is_err());
    }
}

/// Integration tier. The total-row vocabulary has one spelling: the serde wire and the stored
/// token agree, and every token parses back.
#[test]
fn total_tokens_match_their_serde_spelling() {
    use bss_orders::domain::gate::totals::{AmountStatus, TotalScope};
    for kind in [
        EvaluationChargeKind::Recurring,
        EvaluationChargeKind::Usage,
        EvaluationChargeKind::OneTime,
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.token());
        assert_eq!(EvaluationChargeKind::from_token(kind.token()), Some(kind));
    }
    for basis in [
        TcvBasis::FiniteTerm,
        TcvBasis::RollingAnnualized,
        TcvBasis::Mixed,
    ] {
        assert_eq!(serde_json::to_value(basis).unwrap(), basis.token());
    }
    for scope in [TotalScope::Line, TotalScope::Order] {
        assert_eq!(TotalScope::from_token(scope.token()), Some(scope));
    }
    for status in [AmountStatus::Committed, AmountStatus::UncommittedUsage] {
        assert_eq!(AmountStatus::from_token(status.token()), Some(status));
    }
    assert_eq!(EvaluationChargeKind::from_token("Recurring"), None);
    assert_eq!(TotalScope::from_token(""), None);
    assert_eq!(AmountStatus::from_token("usage"), None);
}

/// R12 (`SUBSCRIPTIONS_CONTRACT.md`: unknown struct members refuse): a renamed or misspelled member
/// never decodes, so a misspelled `item_key` is never read as a legacy echo.
#[test]
fn overlap_key_refuses_unknown_members() {
    let key = OverlapKey {
        item_key: None,
        line_id: id(1),
        catalog_subscription_product_key: "k".into(),
        overlap_scope_key: "s".into(),
    };
    let wire = serde_json::to_value(&key).unwrap();
    assert!(serde_json::from_value::<OverlapKey>(wire.clone()).is_ok());
    for member in ["item_keys", "itemKey", "extra"] {
        let mut unknown = wire.clone();
        unknown[member] = json!({"profile": "item_keys_v1"});
        assert!(
            serde_json::from_value::<OverlapKey>(unknown).is_err(),
            "{member}"
        );
    }
}
