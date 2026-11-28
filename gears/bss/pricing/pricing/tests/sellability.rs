//! Pure supported-profile contract; no database or live provider is consulted. The quota terms of
//! an acceptance (D-547, D-548), at the end, run the real acceptance service over a fixture
//! database with Products' quota catalog double.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
mod quota_sale_support;
mod quota_support;
mod seam_support;
use bss_pricing::domain::commercial_terms::validate_commercial_terms;
use bss_pricing_sdk::{
    digest::{money_digest, policy_digest},
    terms::{RatingWindow, Timezone},
};
use seam_support::{sale_query, vm_binding};

use bss_pricing::domain::{
    commercial_terms::{SaleObservation, validate_new_sale_observation, validate_stored_terms},
    money,
};
use bss_pricing_sdk::{
    acceptance::{CommercialReason as R, NewSaleQuery, Term},
    digest::{billing_terms_digest, terms_digest},
    read::{AcceptedBinding, ChargeKind, PriceModel, Tier},
    terms::{AggregationScope, BillingAnchor, BillingCycle, Rounding, TermsSource},
};
use rust_decimal::Decimal;
use uuid::Uuid;

fn certify(b: &mut AcceptedBinding) {
    b.price.money_digest = money_digest(&b.price);
    if let Some(p) = &mut b.usage_rating_policy {
        p.digest = policy_digest(&p.content);
    }
}
fn hourly() -> AcceptedBinding {
    let mut b = vm_binding();
    b.usage_rating_policy
        .as_mut()
        .unwrap()
        .content
        .rating_window = RatingWindow::CalendarHour {
        timezone: Timezone::Utc,
    };
    certify(&mut b);
    b
}
fn tiers() -> Vec<Tier> {
    vec![
        Tier {
            up_to: Some(Decimal::TEN),
            rate: "0.02".parse().unwrap(),
        },
        Tier {
            up_to: None,
            rate: "0.015".parse().unwrap(),
        },
    ]
}
fn reason(q: &NewSaleQuery, b: &[AcceptedBinding], expected: R) {
    let error = validate_commercial_terms(q, b).unwrap_err();
    assert_eq!(error.code, expected.code());
    assert_eq!(error.reason, Some(expected));
}
#[test]
fn complete_model_window_cycle_scope_matrix() {
    let models = [
        PriceModel::Flat {
            amount: Decimal::ONE,
        },
        PriceModel::PerUnit {
            unit_amount: Decimal::ONE,
        },
        PriceModel::Volume { tiers: tiers() },
        PriceModel::Graduated { tiers: tiers() },
        PriceModel::Package {
            package_size: Decimal::ONE,
            package_price: Decimal::ONE,
        },
    ];
    for kind in [
        ChargeKind::Recurring,
        ChargeKind::OneTime,
        ChargeKind::Usage,
    ] {
        for cycle in [BillingCycle::Month, BillingCycle::Year] {
            for model in &models {
                for hour in [false, true] {
                    for scope in [
                        AggregationScope::SubscriptionLine,
                        AggregationScope::Resource,
                    ] {
                        let mut q = sale_query();
                        q.billing_terms.cycle = cycle;
                        if cycle == BillingCycle::Year {
                            q.billing_terms.anchor_at =
                                seam_support::date("2026-01-01").midnight().assume_utc();
                        }
                        q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
                        let mut b = if hour { hourly() } else { vm_binding() };
                        b.kind = kind.clone();
                        b.price.model = model.clone();
                        if kind == ChargeKind::Usage {
                            b.usage_rating_policy
                                .as_mut()
                                .unwrap()
                                .content
                                .aggregation_scope = scope;
                        } else {
                            b.usage_rating_policy = None;
                        }
                        b.recurring_period = (kind == ChargeKind::Recurring).then_some(cycle);
                        certify(&mut b);
                        let supported = match kind {
                            ChargeKind::Usage => matches!(
                                model,
                                PriceModel::PerUnit { .. }
                                    | PriceModel::Volume { .. }
                                    | PriceModel::Graduated { .. }
                            ),
                            _ => matches!(
                                model,
                                PriceModel::Flat { .. } | PriceModel::PerUnit { .. }
                            ),
                        };
                        if supported {
                            assert_eq!(validate_commercial_terms(&q, &[b]), Ok(()));
                        } else {
                            reason(&q, &[b], R::UnsupportedModel);
                        }
                    }
                }
            }
        }
    }
}
/// D-525: a floor is per rating window per aggregation scope, so every window and scope takes one.
#[test]
fn floors_follow_the_entry_window_and_scope() {
    for hour in [false, true] {
        for resource in [false, true] {
            for fee in [Decimal::ZERO, Decimal::ONE] {
                let mut b = if hour { hourly() } else { vm_binding() };
                if resource {
                    b.usage_rating_policy
                        .as_mut()
                        .unwrap()
                        .content
                        .aggregation_scope = AggregationScope::Resource;
                }
                b.price.minimum_fee = Some(fee);
                certify(&mut b);
                assert_eq!(
                    validate_commercial_terms(&sale_query(), &[b]),
                    Ok(()),
                    "hour={hour} resource={resource} fee={fee}"
                );
            }
        }
    }
}
#[test]
fn activation_at_ten_thirty_with_calendar_anchor_is_supported() {
    let mut q = sale_query();
    q.start_at += time::Duration::minutes(630);
    assert!(validate_commercial_terms(&q, &[hourly()]).is_ok());
}
#[test]
fn hourly_anniversary_requires_hour_alignment_but_billing_cycle_does_not() {
    let mut q = sale_query();
    q.billing_terms.anchor = BillingAnchor::SubscriptionStart;
    q.billing_terms.anchor_at += time::Duration::minutes(630);
    q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
    reason(&q, &[hourly()], R::UnalignedBillingAnchor);
    assert!(validate_commercial_terms(&q, &[vm_binding()]).is_ok());
    q.billing_terms.anchor_at -= time::Duration::minutes(30);
    q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
    assert!(validate_commercial_terms(&q, &[hourly()]).is_ok());
}
#[test]
fn calendar_anchors_are_first_day_midnight_utc_and_year_starts_in_january() {
    for at in [
        "2026-10-02T00:00:00Z",
        "2026-10-01T00:00:01Z",
        "2026-10-01T00:00:00.000000001Z",
    ] {
        let mut q = sale_query();
        q.billing_terms.anchor_at =
            time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
                .unwrap();
        q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
        reason(&q, &[vm_binding()], R::UnalignedBillingAnchor);
    }
    let mut q = sale_query();
    q.billing_terms.cycle = BillingCycle::Year;
    q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
    reason(&q, &[vm_binding()], R::UnalignedBillingAnchor);
    q.billing_terms.anchor_at = seam_support::date("2026-01-01").midnight().assume_utc();
    q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
    assert!(validate_commercial_terms(&q, &[vm_binding()]).is_ok());
    q.billing_terms.anchor_at = q
        .billing_terms
        .anchor_at
        .to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());
    assert!(validate_commercial_terms(&q, &[vm_binding()]).is_ok());
}
#[test]
fn each_query_mutation_has_one_stable_refusal() {
    type Mutation = fn(&mut NewSaleQuery);
    let cases: &[(Mutation, R)] = &[
        (|q| q.quantity = Decimal::ZERO, R::InvalidQuantity),
        (|q| q.quantity = -Decimal::ONE, R::InvalidQuantity),
        // Over the 28-digit limit of every pricing decimal.
        (|q| q.quantity = Decimal::MAX, R::InvalidQuantity),
        (
            |q| q.term = Term::FixedPeriods { count: 0 },
            R::InvalidTermCount,
        ),
        (|q| q.selections.clear(), R::IncompleteSelection),
        (
            |q| q.selections.push(q.selections[0].clone()),
            R::IncompleteSelection,
        ),
        (
            |q| q.selections[0].item_id = Uuid::from_u128(999),
            R::IncompleteSelection,
        ),
        (
            |q| q.selections[0].dimension_value = Some("eu".into()),
            R::DimensionMismatch,
        ),
        (|q| q.market.currency = "USD".into(), R::CurrencyMismatch),
        (|q| q.market.currency = "eur".into(), R::CurrencyMismatch),
        (|q| q.market.region = Some(String::new()), R::MarketMismatch),
        (|q| q.billing_terms.schema_version = 2, R::UnsupportedTerms),
        (
            |q| q.billing_terms.digest = [9; 32],
            R::BillingTermsDigestMismatch,
        ),
        (
            |q| {
                q.billing_terms.source = TermsSource::SellerPolicy {
                    id: Uuid::nil(),
                    version: 1,
                }
            },
            R::UnsupportedTerms,
        ),
        (|q| q.order_version = 0, R::UnsupportedTerms),
        (|q| q.hold_policy_version = 0, R::UnsupportedTerms),
    ];
    for (mutate, expected) in cases {
        let mut q = sale_query();
        mutate(&mut q);
        reason(&q, &[vm_binding()], *expected);
    }
}
#[test]
fn billing_terms_digest_is_recomputed_not_trusted() {
    let mut q = sale_query();
    q.billing_terms.digest = [9; 32];
    reason(&q, &[vm_binding()], R::BillingTermsDigestMismatch);
    q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
    assert!(validate_commercial_terms(&q, &[vm_binding()]).is_ok());
}
#[test]
fn each_binding_mutation_has_one_stable_refusal() {
    type Mutation = fn(&mut AcceptedBinding);
    let cases: &[(Mutation, R)] = &[
        (|b| b.usage_rating_policy = None, R::MissingRatingPolicy),
        (|b| b.unit = None, R::MeterPolicyMismatch),
        (
            |b| b.usage_rating_policy.as_mut().unwrap().digest = [9; 32],
            R::MeterPolicyMismatch,
        ),
        (
            |b| b.usage_rating_policy.as_mut().unwrap().version = 0,
            R::MeterPolicyMismatch,
        ),
        (
            |b| b.price_book_entry_id = Uuid::from_u128(99),
            R::BindingEntryMismatch,
        ),
        (
            |b| b.price.price_book_entry_id = Uuid::from_u128(99),
            R::BindingEntryMismatch,
        ),
        (
            |b| b.price.currency = bss_money::CurrencySpec::try_new("USD".into(), 2).unwrap(),
            R::CurrencyMismatch,
        ),
        (
            |b| b.price.currency = bss_money::CurrencySpec::try_new("EUR".into(), 3).unwrap(),
            R::MoneyDigestMismatch,
        ),
        (
            |b| b.invoice.template.clear(),
            R::IncompleteCommercialInputs,
        ),
        (|b| b.invoice.gl_code.clear(), R::IncompleteCommercialInputs),
        (
            |b| b.invoice.tax_category.clear(),
            R::IncompleteCommercialInputs,
        ),
        (|b| b.sku_version = 0, R::IncompleteCommercialInputs),
        (|b| b.sku_code.clear(), R::IncompleteCommercialInputs),
        (|b| b.sku_name.clear(), R::IncompleteCommercialInputs),
        (
            |b| b.invoice.template_digest = [9; 32],
            R::TemplateDigestMismatch,
        ),
        (|b| b.price.money_digest = [9; 32], R::MoneyDigestMismatch),
        (
            |b| {
                b.price.model = PriceModel::PerUnit {
                    unit_amount: -Decimal::ONE,
                }
            },
            R::InvalidMoney,
        ),
        (
            |b| b.price.minimum_fee = Some(-Decimal::ONE),
            R::InvalidMoney,
        ),
        (
            |b| b.price.model = PriceModel::Volume { tiers: vec![] },
            R::InvalidTiers,
        ),
        (
            |b| b.recurring_period = Some(BillingCycle::Month),
            R::UnsupportedTerms,
        ),
        (
            |b| {
                b.price.model = PriceModel::Package {
                    package_size: Decimal::ONE,
                    package_price: Decimal::ONE,
                }
            },
            R::UnsupportedModel,
        ),
    ];
    for (mutate, expected) in cases {
        let mut b = vm_binding();
        mutate(&mut b);
        reason(&sale_query(), &[b], *expected);
    }
}
/// Money is checked at the bound price's own stored scale, never at the ISO table's: a EUR price
/// stored at 4 (ISO gives EUR 2) takes a minimum fee of 0.0001, its increment, and a EUR price
/// stored at 2 refuses that fee. A rate is not posted money: it may be finer than the scale, up to
/// the profile's 12 places.
#[test]
fn money_is_checked_at_the_stored_price_scale_not_the_iso_scale() {
    let eur = |scale| bss_money::CurrencySpec::try_new("EUR".into(), scale).unwrap();
    let mut b = vm_binding();
    b.price.currency = eur(4);
    b.price.minimum_fee = Some("0.0001".parse().unwrap());
    certify(&mut b);
    assert!(
        validate_commercial_terms(&sale_query(), &[b.clone()]).is_ok(),
        "a fee on the stored scale's increment is accepted, whatever ISO says"
    );
    b.price.currency = eur(2);
    certify(&mut b);
    reason(&sale_query(), &[b.clone()], R::InvalidMoney);
    b.price.minimum_fee = Some("0.01".parse().unwrap());
    certify(&mut b);
    assert!(
        validate_commercial_terms(&sale_query(), &[b.clone()]).is_ok(),
        "a fee on the increment of scale 2 is accepted"
    );
    b.price.model = PriceModel::PerUnit {
        unit_amount: "0.000000000001".parse().unwrap(),
    };
    certify(&mut b);
    assert!(
        validate_commercial_terms(&sale_query(), &[b.clone()]).is_ok(),
        "a rate with 12 places is accepted at scale 2"
    );
    b.price.model = PriceModel::PerUnit {
        unit_amount: "0.0000000000001".parse().unwrap(),
    };
    certify(&mut b);
    reason(&sale_query(), &[b], R::InvalidMoney);
}
/// A stored receipt is checked as issued: its content must still hash to its terms digest. A
/// price scale edited without its money digest is refused as other money; edited with its money
/// digest but not the terms digest, it is refused as another acceptance, as is any other field.
#[test]
fn a_stored_receipt_must_still_hash_to_its_terms_digest() {
    let q = sale_query();
    let issued = vm_binding();
    let digest = terms_digest(&q, std::slice::from_ref(&issued), None);
    assert!(validate_stored_terms(&q, std::slice::from_ref(&issued), None, &digest).is_ok());
    let stored_reason = |b: AcceptedBinding, expected: R| {
        let error = validate_stored_terms(&q, &[b], None, &digest).unwrap_err();
        assert_eq!(error.reason, Some(expected));
    };
    let mut rescaled = issued.clone();
    rescaled.price.currency = bss_money::CurrencySpec::try_new("EUR".into(), 3).unwrap();
    stored_reason(rescaled.clone(), R::MoneyDigestMismatch);
    certify(&mut rescaled);
    stored_reason(rescaled, R::AcceptanceMismatch);
    let mut renamed = issued;
    renamed.sku_name = "Another name".into();
    stored_reason(renamed, R::AcceptanceMismatch);
}
#[test]
fn recurring_cycles_must_agree_and_one_time_has_no_period() {
    let mut b = vm_binding();
    b.kind = ChargeKind::Recurring;
    b.usage_rating_policy = None;
    b.recurring_period = Some(BillingCycle::Year);
    reason(&sale_query(), &[b.clone()], R::BillingCycleMismatch);
    b.recurring_period = None;
    reason(&sale_query(), &[b.clone()], R::BillingCycleMismatch);
    b.kind = ChargeKind::OneTime;
    assert!(validate_commercial_terms(&sale_query(), &[b.clone()]).is_ok());
    b.recurring_period = Some(BillingCycle::Month);
    reason(&sale_query(), &[b], R::UnsupportedTerms);
}
#[test]
fn mixed_windows_and_different_entries_are_independently_validated() {
    let mut q = sale_query();
    let a = vm_binding();
    let mut b = hourly();
    b.item_id = Uuid::from_u128(20);
    b.price_book_entry_id = Uuid::from_u128(21);
    b.price.price_book_entry_id = b.price_book_entry_id;
    q.selections.push(bss_pricing_sdk::read::BindingSelection {
        item_id: b.item_id,
        dimension_value: None,
    });
    assert!(validate_commercial_terms(&q, &[a.clone(), b.clone()]).is_ok());
    b.usage_rating_policy = None;
    reason(&q, &[a, b], R::MissingRatingPolicy);
}
#[test]
fn completeness_requires_exactly_one_binding_per_selected_item() {
    let q = sale_query();
    let b = vm_binding();
    reason(&q, &[], R::IncompleteSelection);
    reason(&q, &[b.clone(), b.clone()], R::IncompleteSelection);
    let mut q = q;
    q.selections.push(bss_pricing_sdk::read::BindingSelection {
        item_id: Uuid::from_u128(90),
        dimension_value: None,
    });
    reason(&q, &[b.clone(), b], R::IncompleteSelection);
}
#[test]
fn region_and_default_fallback_freeze_the_requested_dimension() {
    let mut q = sale_query();
    let mut b = vm_binding();
    q.market.region = Some("eu".into());
    q.selections[0].dimension_value = Some("eu".into());
    b.dimension_key = Some("region".into());
    b.dimension_value = Some("eu".into());
    b.via_default = true;
    assert!(validate_commercial_terms(&q, &[b.clone()]).is_ok());
    q.market.region = Some("us".into());
    reason(&q, &[b.clone()], R::MarketMismatch);
    q.market.region = None;
    reason(&q, &[b.clone()], R::MarketMismatch);
    b.dimension_key = None;
    reason(&q, &[b], R::DimensionMismatch);
}
#[test]
fn hourly_tiers_reuse_half_open_math_at_exact_threshold_ten() {
    let model = money::PriceData::Tiers {
        tiers: tiers()
            .into_iter()
            .map(|t| money::Tier {
                up_to: t.up_to,
                rate: t.rate,
            })
            .collect(),
    };
    for (kind, value) in [
        (bss_pricing::domain::price_book_entry::Model::Volume, "0.15"),
        (
            bss_pricing::domain::price_book_entry::Model::Graduated,
            "0.20",
        ),
    ] {
        assert_eq!(
            money::amount_for(kind, &model, Decimal::TEN).unwrap(),
            bss_money::exact::ExactAmount::from_decimal(value.parse::<Decimal>().unwrap())
        );
    }
    for model in [
        PriceModel::Volume { tiers: tiers() },
        PriceModel::Graduated { tiers: tiers() },
    ] {
        let mut b = hourly();
        b.price.model = model;
        certify(&mut b);
        assert!(validate_commercial_terms(&sale_query(), &[b]).is_ok());
    }
}
#[test]
fn invalid_tier_shapes_are_refused_by_the_existing_interpreter() {
    for ts in [
        vec![],
        vec![Tier {
            up_to: Some(Decimal::TEN),
            rate: Decimal::ONE,
        }],
        vec![
            Tier {
                up_to: Some(Decimal::ZERO),
                rate: Decimal::ONE,
            },
            Tier {
                up_to: None,
                rate: Decimal::ONE,
            },
        ],
        vec![Tier {
            up_to: None,
            rate: -Decimal::ONE,
        }],
    ] {
        let mut b = hourly();
        b.price.model = PriceModel::Volume { tiers: ts };
        certify(&mut b);
        reason(&sale_query(), &[b], R::InvalidTiers);
    }
}
#[test]
fn live_revision_coverage_and_each_sku_lifecycle_gate_fail_closed() {
    let good = SaleObservation {
        revision_is_current: true,
        revision_available: true,
        sku_active: true,
        sku_sellable: true,
        covered: true,
        book_valid: true,
    };
    assert!(validate_new_sale_observation(&good).is_ok());
    for name in [
        "superseded",
        "unavailable",
        "retired",
        "deprecated",
        "off_sale",
        "uncovered",
        "book_closed",
    ] {
        let mut o = good.clone();
        match name {
            "superseded" => o.revision_is_current = false,
            "unavailable" => o.revision_available = false,
            "retired" | "deprecated" => o.sku_active = false,
            "off_sale" => o.sku_sellable = false,
            "uncovered" => o.covered = false,
            "book_closed" => o.book_valid = false,
            _ => unreachable!(),
        }
        assert_eq!(
            validate_new_sale_observation(&o).unwrap_err().reason,
            Some(if name == "uncovered" {
                R::ResolutionChanged
            } else {
                R::NotSellable
            }),
            "{name}"
        );
    }
}
#[test]
fn unsupported_wire_scalars_remain_typed_and_preserve_the_value() {
    let e = "quarter".parse::<BillingCycle>().unwrap_err();
    assert_eq!(
        (e.field, e.value.as_str(), e.reason),
        ("cycle", "quarter", R::UnsupportedTerms)
    );
    assert_eq!(
        "cross_line".parse::<AggregationScope>().unwrap_err().reason,
        R::UnsupportedScope
    );
    assert_eq!(
        "Europe/Madrid".parse::<Timezone>().unwrap_err().reason,
        R::UnsupportedWindow
    );
    assert_eq!(
        "half_up".parse::<Rounding>().unwrap_err().reason,
        R::UnsupportedTerms
    );
}
#[test]
fn canonical_refusals_preserve_concrete_reason_and_http_class() {
    use toolkit_canonical_errors::CanonicalError;
    for r in [
        R::MissingRatingPolicy,
        R::UnalignedBillingAnchor,
        R::UnsupportedTerms,
        R::BillingTermsDigestMismatch,
    ] {
        let error: CanonicalError = bss_pricing::domain::RuleError::from(r).into();
        assert_eq!(error.status_code(), 400);
        let problem = toolkit_canonical_errors::Problem::from(error.clone());
        assert_eq!(problem.context["field_violations"][0]["reason"], r.as_str());
        match error {
            CanonicalError::InvalidArgument { ctx, .. } => assert_eq!(
                serde_json::to_value(ctx).unwrap()["field_violations"][0]["reason"],
                r.as_str()
            ),
            _ => panic!("wrong canonical class"),
        }
    }
    for r in [
        R::ResolutionChanged,
        R::AcceptanceMismatch,
        R::HoldExpired,
        R::PriceClosed,
        R::SkuRetired,
        R::MarketChanged,
        R::NotSellable,
    ] {
        let error: CanonicalError = r.into();
        assert_eq!(error.status_code(), 409);
        match error {
            CanonicalError::Aborted { ctx, .. } => assert_eq!(ctx.reason, r.as_str()),
            _ => panic!("wrong canonical class"),
        }
    }
    assert_eq!(CanonicalError::from(R::PermissionDenied).status_code(), 403);
    assert_eq!(CanonicalError::from(R::ReceiptNotFound).status_code(), 404);
    assert_eq!(
        CanonicalError::from(bss_pricing_sdk::meter_semantics::UnconfiguredMeterSemantics)
            .status_code(),
        400
    );
    assert_eq!(
        CanonicalError::service_unavailable().create().status_code(),
        503
    );
}

/// D-547 (the owner's ruling of 2026-11-22): `QUOTA_RULE_MISSING` is its own refusal, a terms
/// refusal like its siblings: 400, an invalid argument on `commercial_terms` that carries the
/// reason, the same through a domain rule error.
#[test]
fn quota_rule_missing_is_its_own_terms_refusal() {
    use toolkit_canonical_errors::CanonicalError;
    let r = R::QuotaRuleMissing;
    assert_eq!(
        (r.code(), r.as_str()),
        ("QUOTA_RULE_MISSING", "QuotaRuleMissing")
    );
    for error in [
        CanonicalError::from(r),
        bss_pricing::domain::RuleError::from(r).into(),
    ] {
        assert_eq!(error.status_code(), 400, "{error}");
        let problem = serde_json::to_value(toolkit_canonical_errors::Problem::from(error)).unwrap();
        let violation = &problem["context"]["field_violations"][0];
        assert_eq!(violation["field"], "commercial_terms", "{problem}");
        assert_eq!(violation["reason"], "QuotaRuleMissing", "{problem}");
        assert_eq!(violation["description"], "QUOTA_RULE_MISSING", "{problem}");
    }
}

#[test]
fn billing_terms_wire_requires_explicit_values_and_rejects_unsupported_semantics() {
    use bss_pricing::infra::commercial_terms_wire::decode_billing_terms;
    let terms = sale_query().billing_terms;
    let digest = terms.digest.iter().fold(String::new(), |mut text, byte| {
        use std::fmt::Write;
        write!(text, "{byte:02x}").unwrap();
        text
    });
    let valid = serde_json::json!({"schema_version":"1", "cycle":"month", "anchor":"calendar",
        "anchor_at":"2026-10-01T00:00:00Z", "timezone":"UTC", "source":{"kind":"explicit_order"}, "digest":digest});
    assert_eq!(decode_billing_terms(&valid.to_string()).unwrap(), terms);
    for (key, value, expected) in [
        ("cycle", serde_json::json!("quarter"), R::UnsupportedTerms),
        (
            "timezone",
            serde_json::json!("Europe/Madrid"),
            R::UnsupportedWindow,
        ),
        (
            "source",
            serde_json::json!({"kind":"unknown"}),
            R::UnsupportedTerms,
        ),
        (
            "schema_version",
            serde_json::json!("2"),
            R::UnsupportedTerms,
        ),
        ("schema_version", serde_json::json!(1), R::UnsupportedTerms),
        (
            "included_quantity",
            serde_json::json!("1"),
            R::UnsupportedTerms,
        ),
        (
            "promotion",
            serde_json::json!("summer"),
            R::UnsupportedTerms,
        ),
        ("phases", serde_json::json!([]), R::UnsupportedTerms),
        ("fx", serde_json::json!("USD"), R::UnsupportedTerms),
    ] {
        let shown = value.to_string();
        let mut input = valid.clone();
        input[key] = value;
        let e = decode_billing_terms(&input.to_string()).unwrap_err();
        assert_eq!(e.reason, expected);
        if matches!(key, "cycle" | "timezone" | "schema_version") && shown.starts_with('"') {
            assert_eq!(e.field, key);
            assert!(e.value.contains(shown.trim_matches('"')), "{}", e.value);
        } else {
            assert_eq!(e.field, "billing_terms");
            assert!(!e.value.is_empty(), "{key}: {}", e.value);
        }
        assert_eq!(
            toolkit_canonical_errors::CanonicalError::from(e).status_code(),
            400
        );
    }
    assert_eq!(
        decode_billing_terms("null").unwrap_err().reason,
        R::MissingBillingTerms
    );
    let mut missing_cycle = valid.clone();
    missing_cycle.as_object_mut().unwrap().remove("cycle");
    assert_eq!(
        decode_billing_terms(&missing_cycle.to_string())
            .unwrap_err()
            .reason,
        R::UnsupportedTerms
    );
    let duplicate = valid.to_string().replacen('{', "{\"cycle\":\"year\",", 1);
    assert_eq!(
        decode_billing_terms(&duplicate).unwrap_err().reason,
        R::UnsupportedTerms
    );
}

// ------------------------------------------------------------------ quota terms (D-547, D-548)

use bss_pricing_sdk::acceptance::{AcceptedQuotaSnapshot, SellabilityV1};
use quota_sale_support::{
    Metric::{Ram, Vcpu},
    Shape, reason as refusal, rule, sale_shaped, sale_with_rules,
};

/// 10 vCPU per plan unit on the priced item, its default rule.
const TEN_VCPU: quota_sale_support::Rule = rule(1, Vcpu, None, "10");

/// The receipt's frozen snapshot passes the stored-terms check fulfilment runs, so the check and
/// the acceptance agree.
fn assert_stored_terms_pass(receipt: &bss_pricing_sdk::acceptance::AcceptanceReceipt) {
    assert_eq!(
        bss_pricing_sdk::digest::verify_quota_snapshot(&receipt.query, receipt.quota.as_ref()),
        Ok(())
    );
    assert_eq!(
        validate_stored_terms(
            &receipt.query,
            &receipt.bindings,
            receipt.quota.as_ref(),
            &receipt.terms_digest
        ),
        Ok(())
    );
}

/// The ids of the frozen rules, in their order.
fn frozen_ids(quota: &AcceptedQuotaSnapshot) -> Vec<Uuid> {
    quota.rules.iter().map(|r| r.rule.rule.rule_id).collect()
}

/// D-548: a caller that does not know quotas cannot buy a plan with rules: no expected quota
/// digest is `RESOLUTION_CHANGED`, with no receipt and no catalog call.
#[tokio::test]
async fn a_quota_bearing_plan_needs_the_expected_quota_digest() {
    let s = sale_with_rules(&[TEN_VCPU]).await;
    let mut query = s.sale(None, "1").await;
    assert!(query.expected_quota_digest.is_some());
    query.expected_quota_digest = None;
    let error = s.check(&query, "a1").await.unwrap_err();
    assert_eq!(refusal(&error), "ResolutionChanged", "{error:?}");
    assert_eq!(error.status_code(), 409);
    assert!(s.acceptance_requests().is_empty());
    assert_eq!(s.acceptances().await, 0);
}

/// D-548: a revision without rules refuses a sale that sends an expected quota digest, as
/// `RESOLUTION_CHANGED`.
#[tokio::test]
async fn a_quota_free_revision_refuses_an_expected_quota_digest() {
    let s = sale_with_rules(&[]).await;
    let mut query = s.sale(None, "1").await;
    assert_eq!(
        query.expected_quota_digest, None,
        "the preview has no quota terms"
    );
    query.expected_quota_digest = Some([7; 32]);
    let error = s.check(&query, "a1").await.unwrap_err();
    assert_eq!(refusal(&error), "ResolutionChanged", "{error:?}");
    assert_eq!(s.acceptances().await, 0);
    query.expected_quota_digest = None;
    let receipt = s.check(&query, "a2").await.unwrap();
    assert_eq!(receipt.quota, None);
}

/// D-548: an expected quota digest from another revision's preview is `RESOLUTION_CHANGED`, even
/// when every price is the same. The plan is copied to a revision that raises the cap from 10 to
/// 20 vCPU per unit and published; the order moves to it with fresh selections and a fresh
/// selected bindings digest (the same price) but keeps the old preview's quota digest. With the
/// new preview's digest the same sale is accepted.
#[tokio::test]
async fn a_stale_expected_quota_digest_is_resolution_changed_with_prices_unchanged() {
    let s = sale_with_rules(&[TEN_VCPU]).await;
    let old = s.sale(None, "1").await;
    let old_price = s.resolved().await.cells[0]
        .binding
        .clone()
        .unwrap()
        .price
        .price_id;
    let (status, copy, _) =
        s.f.call(
            "POST",
            &format!("/plans/{}/revisions", s.plan),
            serde_json::json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(status, 201, "{copy}");
    let next: Uuid = copy["id"].as_str().unwrap().parse().unwrap();
    let copied: Uuid = copy["items"][0]["id"].as_str().unwrap().parse().unwrap();
    let raised = quota_support::rule_json(1, &s.cpu, copied, "20");
    let (status, saved) = quota_sale_support::put_rules(&s.f, next, &[raised], "raise").await;
    assert_eq!(status, 200, "{saved}");
    plan_support::publish(&s.f, s.plan, next).await;
    let resolved = s.resolved_of(next).await;
    assert_eq!(
        resolved.cells[0].binding.as_ref().unwrap().price.price_id,
        old_price,
        "the same price"
    );
    let fresh = quota_sale_support::sale_of(&s.ctx, &resolved, copied, None, "1");
    let mut moved = fresh.clone();
    moved.expected_quota_digest = old.expected_quota_digest;
    assert_ne!(moved.expected_quota_digest, fresh.expected_quota_digest);
    let error = s.check(&moved, "a2").await.unwrap_err();
    assert_eq!(refusal(&error), "ResolutionChanged", "{error:?}");
    assert!(
        s.acceptance_requests().is_empty(),
        "refused before any catalog call"
    );
    let receipt = s.check(&fresh, "a3").await.unwrap();
    assert_eq!(receipt.quota.unwrap().rules[0].limit, "20");
}

/// D-548: the receipt freezes the selected rule with its evidence, its final limit at the
/// accepted quantity (10 per unit at 2 is 20) and the availability revision its definition showed,
/// the original quantity, the selected digest the preview gave and the snapshot digest, which the
/// terms digest binds. Products judged the rule for acceptance at that quantity with the
/// binding's SKU and SKU version.
#[tokio::test]
async fn acceptance_freezes_rules_quantity_and_digests_in_the_receipt() {
    let s = sale_with_rules(&[TEN_VCPU]).await;
    let query = s.sale(None, "2").await;
    let receipt = s.check(&query, "a3").await.unwrap();
    let quota = receipt.quota.clone().expect("a frozen snapshot");
    assert_eq!(quota.rules.len(), 1);
    assert_eq!(quota.rules[0].limit, "20", "10 per unit at quantity 2");
    assert_eq!(quota.rules[0].availability_revision.get(), 2);
    assert_eq!(
        quota.rules[0].rule,
        s.resolved().await.quota_terms.unwrap().rules[0],
        "the rule and the evidence the preview showed"
    );
    assert_eq!(quota.original_quantity, Decimal::from(2));
    assert_eq!(
        Some(quota.selected_quota_digest),
        query.expected_quota_digest
    );
    assert_eq!(
        quota.quota_snapshot_digest,
        bss_pricing_sdk::digest::quota_snapshot_digest(
            &quota.selected_quota_digest,
            &quota.rules,
            &receipt.query
        )
    );
    assert_eq!(
        receipt.terms_digest,
        terms_digest(&receipt.query, &receipt.bindings, Some(&quota))
    );
    assert_stored_terms_pass(&receipt);
    let asked = s.acceptance_requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].quantity.as_deref(), Some("2"));
    assert_eq!(asked[0].rules.len(), 1);
    assert_eq!(asked[0].rules[0].sku_id, s.sku);
    assert_eq!(
        i64::try_from(asked[0].rules[0].sku_version.get()).unwrap(),
        receipt.bindings[0].sku_version
    );
    assert_eq!(
        s.fake.callers().last().map(|c| c.0),
        Some(s.ctx.subject_id()),
        "Products is asked as the caller"
    );
}

/// D-548: a replay answers the stored receipt, frozen snapshot included, after the definition is
/// withdrawn and the catalog is down: no live quota term is read again.
#[tokio::test]
async fn a_replay_after_a_catalog_change_answers_the_frozen_receipt() {
    let s = sale_with_rules(&[TEN_VCPU]).await;
    let query = s.sale(None, "1").await;
    let first = s.check(&query, "a4").await.unwrap();
    assert!(first.quota.is_some());
    s.fake.withdraw_all();
    s.fake.go_down();
    let again = s.check(&query, "a4").await.unwrap();
    assert_eq!(
        again, first,
        "the replay reads the stored receipt, no live term"
    );
    let by_line = s.check(&query, "a4-other-key").await.unwrap();
    assert_eq!(
        by_line, first,
        "the same order line answers its stored receipt"
    );
    assert_eq!(s.acceptance_requests().len(), 1);
}

/// D-548: a definition that is not enabled (its qualification is pending) is `NOT_SELLABLE`, with
/// no receipt; a draft could still save the rule.
#[tokio::test]
async fn acceptance_refuses_a_definition_not_enabled() {
    let shape = Shape {
        disabled: true,
        ..Shape::default()
    };
    let s = sale_shaped(shape, &[TEN_VCPU]).await;
    let error = s.check(&s.sale(None, "1").await, "a5").await.unwrap_err();
    assert_eq!(refusal(&error), "NotSellable", "{error:?}");
    assert_eq!(error.status_code(), 409);
    assert_eq!(s.acceptance_requests().len(), 1);
    assert_eq!(s.acceptances().await, 0);
}

/// D-548: Products judges each final limit at the accepted quantity: 10 vCPU per unit at 200 is
/// 2000, above the unit's 1024, so `QUANTITY_INVALID`; at 102 it is 1020 and the sale is accepted.
#[tokio::test]
async fn per_plan_unit_limits_are_validated_at_the_accepted_quantity() {
    let s = sale_with_rules(&[TEN_VCPU]).await;
    let error = s.check(&s.sale(None, "200").await, "a6").await.unwrap_err();
    assert_eq!(refusal(&error), "InvalidQuantity", "{error:?}");
    assert_eq!(error.status_code(), 400);
    assert_eq!(s.acceptances().await, 0);
    let receipt = s.check(&s.sale(None, "102").await, "a7").await.unwrap();
    assert_eq!(receipt.quota.unwrap().rules[0].limit, "1020");
}

/// D-548: a sale that selects rules needs a whole quantity from 1: 1.5 is `QUANTITY_INVALID`
/// before any catalog call, and 2.00 is the whole quantity 2.
#[tokio::test]
async fn a_fractional_quantity_with_rules_is_invalid_quantity() {
    let s = sale_with_rules(&[TEN_VCPU]).await;
    let error = s.check(&s.sale(None, "1.5").await, "a8").await.unwrap_err();
    assert_eq!(refusal(&error), "InvalidQuantity", "{error:?}");
    assert!(s.acceptance_requests().is_empty());
    let receipt = s.check(&s.sale(None, "2.00").await, "a9").await.unwrap();
    assert_eq!(receipt.quota.unwrap().rules[0].limit, "20");
    assert_eq!(s.acceptance_requests()[0].quantity.as_deref(), Some("2"));
}

/// D-548: a quota-free sale is accepted as before: no quota member in its query or receipt, its
/// terms digest the quota-free one, its stored bytes without a quota member, and no catalog
/// call. The frozen-acceptance seam fixture and the receipt goldens are unchanged in their own
/// suites.
#[tokio::test]
async fn a_quota_free_sale_keeps_its_receipt_bytes() {
    let f = seam_support::acceptance_fixture::AcceptanceFixture::new().await;
    let fake = quota_support::install(&f.fixture);
    let receipt = f
        .sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap();
    assert_eq!(receipt.quota, None);
    assert_eq!(receipt.query.expected_quota_digest, None);
    assert_eq!(
        receipt.terms_digest,
        terms_digest(&receipt.query, &receipt.bindings, None)
    );
    let stored = bss_pricing::infra::commercial_terms::wire::encode_acceptance(&receipt).unwrap();
    assert!(!stored.contains("quota"), "{stored}");
    assert!(fake.requests().is_empty());
}

/// D-547 (the owner's ruling of 2026-11-22): a requested value with neither its own rule nor a
/// default rule of a metric is `QUOTA_RULE_MISSING` (400), with no receipt and no catalog call.
#[tokio::test]
async fn a_value_with_neither_a_rule_nor_a_default_is_quota_rule_missing() {
    let s = sale_shaped(regional(), &[rule(1, Vcpu, Some("eu"), "8")]).await;
    let error = s
        .check(&s.sale(Some("us"), "1").await, "b1")
        .await
        .unwrap_err();
    assert_eq!(refusal(&error), "QuotaRuleMissing", "{error:?}");
    assert_eq!(error.status_code(), 400);
    assert!(s.acceptance_requests().is_empty());
    assert_eq!(s.acceptances().await, 0);
}

/// D-547: a value with no rule of its own takes the item's default rule, which the receipt
/// freezes, and the receipt passes the stored-terms check.
#[tokio::test]
async fn a_value_with_only_the_default_rule_freezes_the_default() {
    let rules = [rule(1, Vcpu, Some("eu"), "8"), rule(2, Vcpu, None, "4")];
    let s = sale_shaped(regional(), &rules).await;
    let receipt = s.check(&s.sale(Some("us"), "1").await, "b2").await.unwrap();
    let quota = receipt.quota.clone().unwrap();
    assert_eq!(frozen_ids(&quota), [Uuid::from_u128(2)]);
    assert_eq!(quota.rules[0].limit, "4");
    assert_stored_terms_pass(&receipt);
}

/// D-547: the cell without a value of an item with a dimension whose rules all name a value has
/// no rule: `QUOTA_RULE_MISSING`.
#[tokio::test]
async fn the_cell_without_a_value_of_an_item_with_value_rules_only_is_quota_rule_missing() {
    let rules = [
        rule(1, Vcpu, Some("eu"), "8"),
        rule(2, Vcpu, Some("us"), "6"),
    ];
    let s = sale_shaped(regional(), &rules).await;
    let error = s.check(&s.sale(None, "1").await, "b3").await.unwrap_err();
    assert_eq!(refusal(&error), "QuotaRuleMissing", "{error:?}");
    assert!(s.acceptance_requests().is_empty());
}

/// D-547: an `eu` sale takes its own vCPU rule and the default RAM rule, both frozen; the default
/// vCPU rule is not taken beside `eu`'s own.
#[tokio::test]
async fn an_eu_sale_freezes_its_own_vcpu_rule_and_the_default_ram_rule() {
    let rules = [
        rule(1, Vcpu, Some("eu"), "8"),
        rule(2, Vcpu, None, "4"),
        rule(3, Ram, None, "16"),
    ];
    let s = sale_shaped(regional(), &rules).await;
    let receipt = s.check(&s.sale(Some("eu"), "2").await, "b4").await.unwrap();
    let quota = receipt.quota.clone().unwrap();
    assert_eq!(frozen_ids(&quota), [Uuid::from_u128(1), Uuid::from_u128(3)]);
    let limits: Vec<&str> = quota.rules.iter().map(|r| r.limit.as_str()).collect();
    assert_eq!(limits, ["16", "32"]);
    assert_stored_terms_pass(&receipt);
}

/// D-547: a quota scoped to the region sold uses a binding per region (products P-D-276), and each
/// value sells with its own rule: `eu` freezes the `compute-eu` rule only, `us` the `compute-us`
/// rule only.
#[tokio::test]
async fn per_region_bindings_each_freeze_their_own_rule() {
    let mut eu = rule(1, Vcpu, Some("eu"), "8");
    eu.binding = "compute-eu";
    let mut us = rule(2, Vcpu, Some("us"), "6");
    us.binding = "compute-us";
    let s = sale_shaped(regional(), &[eu, us]).await;
    for (value, id) in [("eu", 1), ("us", 2)] {
        let key = format!("sale-{value}");
        let receipt = s
            .check(&s.sale(Some(value), "1").await, &key)
            .await
            .unwrap();
        assert_eq!(
            frozen_ids(receipt.quota.as_ref().unwrap()),
            [Uuid::from_u128(id)]
        );
        assert_stored_terms_pass(&receipt);
    }
}

/// D-544, D-547: the checks judge the revision's sale date, so a default-chain price that starts
/// after it leaves the cell without a value unsellable there, and the checks' coverage row is
/// green although no rule covers that cell. A sale on a later day binds that price, and
/// acceptance refuses the cell as `QUOTA_RULE_MISSING`.
#[tokio::test]
async fn a_cell_without_a_value_priced_after_the_sale_date_is_refused_at_acceptance() {
    let shape = Shape {
        default_from: Some("2026-09-20"),
        available_from: Some("2026-09-01"),
        ..regional()
    };
    let rules = [
        rule(1, Vcpu, Some("eu"), "8"),
        rule(2, Vcpu, Some("us"), "6"),
    ];
    let s = sale_shaped(shape, &rules).await;
    assert_eq!(
        s.draft_checks["sale_date"], "2026-09-01",
        "{}",
        s.draft_checks
    );
    let row = s.draft_checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "QUOTA_RULE_MISSING")
        .unwrap();
    assert_eq!(row["ok"], true, "{row}");
    let query = s.sale(None, "1").await;
    assert_eq!(query.start_at.date(), seam_support::date("2026-10-01"));
    let error = s.check(&query, "b6").await.unwrap_err();
    assert_eq!(refusal(&error), "QuotaRuleMissing", "{error:?}");
    assert!(s.acceptance_requests().is_empty());
}

/// D-547, D-548: the order of the quota refusals. A caller that sends no expected digest gets
/// `RESOLUTION_CHANGED` even for a cell no rule covers; with a digest, the uncovered cell is
/// `QUOTA_RULE_MISSING` whatever the digest, before the digest is compared and before any
/// catalog call.
#[tokio::test]
async fn quota_rule_missing_follows_the_expected_digest_and_precedes_the_catalog() {
    let s = sale_shaped(regional(), &[rule(1, Vcpu, Some("eu"), "8")]).await;
    let query = s.sale(Some("us"), "1").await;
    let mut unaware = query.clone();
    unaware.expected_quota_digest = None;
    let error = s.check(&unaware, "c1").await.unwrap_err();
    assert_eq!(refusal(&error), "ResolutionChanged", "{error:?}");
    let error = s.check(&query, "c2").await.unwrap_err();
    assert_eq!(refusal(&error), "QuotaRuleMissing", "{error:?}");
    let mut other = query.clone();
    other.expected_quota_digest = Some([9; 32]);
    let error = s.check(&other, "c3").await.unwrap_err();
    assert_eq!(refusal(&error), "QuotaRuleMissing", "{error:?}");
    assert!(s.acceptance_requests().is_empty());
    assert_eq!(s.acceptances().await, 0);
}

/// D-548: a revision with rules whose sale selects none (its only rule is on an included item,
/// which no sale selects) freezes the empty selection: a snapshot with no rules whose selected
/// digest is the empty selection's, the digest the preview gave. Products is not asked and the
/// whole-quantity rule does not apply. The included item is a legacy item without an entry,
/// published past the checks: submit refuses a revision with such an item under D-467 (and
/// D-512 for an item added without an entry), so no submitted revision reaches this state. The
/// test pins the defensive branch.
#[tokio::test]
async fn a_sale_that_selects_no_rule_freezes_the_empty_selection() {
    let shape = Shape {
        included_item: true,
        ..Shape::default()
    };
    let mut on_included = rule(1, Vcpu, None, "10");
    on_included.included = true;
    let s = sale_shaped(shape, &[on_included]).await;
    let terms = s.resolved().await.quota_terms.unwrap();
    assert_eq!(terms.rules.len(), 1);
    let query = s.sale(None, "1.5").await;
    assert_eq!(
        query.expected_quota_digest,
        Some(bss_pricing_sdk::digest::selected_quota_digest(&terms, &[]))
    );
    let receipt = s.check(&query, "d1").await.unwrap();
    let quota = receipt.quota.clone().unwrap();
    assert!(quota.rules.is_empty());
    assert_eq!(
        Some(quota.selected_quota_digest),
        query.expected_quota_digest
    );
    assert_eq!(quota.original_quantity, "1.5".parse::<Decimal>().unwrap());
    assert!(s.fake.requests().iter().all(|r| r.quantity.is_none()));
    assert!(s.acceptance_requests().is_empty());
    assert_stored_terms_pass(&receipt);
}

/// D-548: an issued receipt with two frozen rules passes the stored-terms check, in the SDK and
/// through fulfilment's eligibility read, which decodes the stored receipt and runs it.
#[tokio::test]
async fn an_issued_quota_receipt_passes_the_stored_terms_check() {
    let rules = [rule(1, Vcpu, None, "10"), rule(2, Ram, None, "20")];
    let s = sale_with_rules(&rules).await;
    let receipt = s.check(&s.sale(None, "3").await, "e1").await.unwrap();
    let quota = receipt.quota.clone().unwrap();
    let limits: Vec<&str> = quota.rules.iter().map(|r| r.limit.as_str()).collect();
    assert_eq!(limits, ["30", "60"]);
    assert_stored_terms_pass(&receipt);
    let eligibility = s
        .sellability
        .check_fulfilment(
            &s.ctx,
            seam_support::acceptance_fixture::AcceptanceFixture::fulfilment_query(&receipt),
        )
        .await
        .unwrap();
    assert_eq!(eligibility.acceptance_id, receipt.acceptance_id);
}

/// D-548: a catalog outage at acceptance is the service's 503 `QUOTA_CATALOG_UNAVAILABLE`, and no
/// receipt is issued.
#[tokio::test]
async fn a_catalog_outage_at_acceptance_is_503_and_issues_no_receipt() {
    let s = sale_with_rules(&[TEN_VCPU]).await;
    let query = s.sale(None, "1").await;
    s.fake.go_down();
    let error = s.check(&query, "f1").await.unwrap_err();
    assert_eq!(error.status_code(), 503, "{error:?}");
    assert!(
        bss_pricing::infra::plan_quota::is_unavailable(&error),
        "{error:?}"
    );
    assert_eq!(s.acceptances().await, 0);
    s.fake.come_back();
    assert!(s.check(&query, "f1").await.unwrap().quota.is_some());
}

/// D-548: a verdict that answers no check but does not judge the selected rules as Products'
/// contract says is an internal error, never a default. A verdict with no final limit, with no
/// entry for the selected rule, for another definition, for a definition that is not enabled, or
/// with a second entry for the rule, and a final limit that is not a canonical integer (refused
/// by the frozen snapshot's own check), each answer a 500 that carries no caller or catalog data,
/// issue no receipt and log Products' verdict as the cause with the check that failed. Once the
/// catalog judges again, the same sale is accepted.
#[tokio::test]
async fn a_verdict_that_breaks_the_contract_is_500_and_issues_no_receipt() {
    use bss_products_sdk::quota::catalog::QuotaDefinition;
    use bss_products_sdk::quota::validation::{QuotaRuleVerdict, QuotaSetValidation};
    const LOGGED: &str = "pricing: Products' quota verdict breaks the acceptance contract";
    const ANSWERED: &str = "the quota catalog's verdict breaks its contract";
    let s = sale_with_rules(&[TEN_VCPU]).await;
    let query = s.sale(None, "1").await;
    let entry = |rule: u128, definition: &QuotaDefinition, limit: Option<&str>| QuotaRuleVerdict {
        rule_id: Uuid::from_u128(rule),
        definition: Some(definition.clone()),
        limit: limit.map(str::to_owned),
    };
    let verdict = |rules: Vec<QuotaRuleVerdict>| QuotaSetValidation {
        rules,
        checks: vec![],
    };
    let cases = [
        (
            "no final limit",
            verdict(vec![entry(1, &s.cpu, None)]),
            "a selected quota rule has no final limit",
        ),
        (
            "no entry for the rule",
            verdict(vec![entry(9, &s.cpu, Some("10"))]),
            "a verdict entry is not for the selected quota rule at its place in the request",
        ),
        (
            "another definition",
            verdict(vec![entry(1, &s.ram, Some("10"))]),
            "a selected quota rule is not judged against its definition",
        ),
        (
            "a definition that is not enabled",
            verdict(vec![entry(
                1,
                &quota_support::disabled(s.cpu.clone()),
                Some("10"),
            )]),
            "a selected quota definition is not enabled",
        ),
        (
            "a second entry for the rule",
            verdict(vec![entry(1, &s.cpu, Some("10")), entry(1, &s.ram, None)]),
            "the verdict does not have one entry for each selected quota rule",
        ),
        (
            "a limit that is not canonical",
            verdict(vec![entry(1, &s.cpu, Some("010"))]),
            "a final limit of the quota snapshot is not a canonical decimal integer",
        ),
    ];
    let asked = cases.len();
    for (n, (case, scripted, why)) in cases.into_iter().enumerate() {
        s.fake.answering(scripted);
        let (logs, guard) = quota_support::LogCapture::install();
        let answer = s.check(&query, &format!("g{n}")).await;
        drop(guard);
        let Err(error) = answer else {
            panic!("{case}: a receipt was issued");
        };
        assert_eq!(error.status_code(), 500, "{case}: {error:?}");
        assert_eq!(error.diagnostic(), Some(ANSWERED), "{case}: {error:?}");
        assert!(
            logs.lines().iter().any(|line| line.starts_with("ERROR")
                && line.contains(LOGGED)
                && line.contains(why)),
            "{case}: {:#?}",
            logs.lines()
        );
        assert_eq!(s.acceptances().await, 0, "{case}: no receipt");
    }
    assert_eq!(s.acceptance_requests().len(), asked);
    s.fake.judging();
    let receipt = s.check(&query, "g-judged").await.unwrap();
    assert_eq!(receipt.quota.unwrap().rules[0].limit, "10");
}

/// D-548: Products' checks of a sale map to one acceptance refusal, the first that applies in
/// this order: not enabled, changed, any check other than a final limit off its grid at the
/// quantity, then that limit. A refusal that no quantity can fix wins over one that a new
/// quantity can fix. A sale with selected rules sends its quantity as a whole number from 1.
#[test]
fn products_checks_map_to_one_acceptance_refusal() {
    use bss_pricing::domain::commercial_terms::{quota_quantity, quota_refusal};
    use bss_products_sdk::quota::validation::{QuotaCheck, QuotaCheckCode as C};
    let check = |code, field: &str| QuotaCheck {
        code,
        rule_ids: vec![Uuid::from_u128(1)],
        field: field.into(),
        detail: String::new(),
    };
    let cases = [
        (vec![], None),
        (
            vec![check(C::ScopeUnsupported, "scope_selection")],
            Some(R::UnsupportedTerms),
        ),
        (
            vec![check(C::AmountInvalid, "amount")],
            Some(R::UnsupportedTerms),
        ),
        (
            vec![check(C::AmountInvalid, "quantity")],
            Some(R::InvalidQuantity),
        ),
        (
            vec![
                check(C::RuleInvalid, "definition_ref"),
                check(C::AmountInvalid, "quantity"),
            ],
            Some(R::UnsupportedTerms),
        ),
        (
            vec![
                check(C::AmountInvalid, "quantity"),
                check(C::DefinitionChanged, "definition_ref.digest"),
            ],
            Some(R::ResolutionChanged),
        ),
        (
            vec![
                check(C::DefinitionChanged, "definition_ref.digest"),
                check(C::QualificationUnavailable, "definition_ref"),
            ],
            Some(R::NotSellable),
        ),
    ];
    for (checks, expected) in cases {
        assert_eq!(quota_refusal(&checks), expected, "{checks:?}");
    }
    for (quantity, expected) in [
        ("1", Some("1")),
        ("2.00", Some("2")),
        ("200", Some("200")),
        ("1.5", None),
        ("0", None),
        ("0.0", None),
        ("-1", None),
    ] {
        assert_eq!(
            quota_quantity(quantity.parse().unwrap()).ok().as_deref(),
            expected,
            "{quantity}"
        );
    }
    assert_eq!(
        quota_quantity("1.5".parse().unwrap()).unwrap_err().reason,
        Some(R::InvalidQuantity)
    );
}

/// The regional shape: `region` with `eu` and `us`, each value's chain priced, and a default-chain
/// price, so every cell is sellable.
fn regional() -> Shape {
    Shape {
        regions: &["eu", "us"],
        ..Shape::default()
    }
}
