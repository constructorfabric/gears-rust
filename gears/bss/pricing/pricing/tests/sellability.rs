//! Pure supported-profile contract; no database or live provider is consulted.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
mod seam_support;
use bss_pricing::domain::commercial_terms::validate_commercial_terms;
use bss_pricing_sdk::{
    digest::{money_digest, policy_digest},
    terms::{RatingWindow, Timezone},
};
use seam_support::{sale_query, vm_binding};

#[test]
fn hourly_window_does_not_make_the_invoice_hourly() {
    let query = sale_query();
    let mut binding = vm_binding();
    let policy = binding.usage_rating_policy.as_mut().unwrap();
    policy.content.rating_window = RatingWindow::CalendarHour {
        timezone: Timezone::Utc,
    };
    policy.digest = policy_digest(&policy.content);
    assert!(validate_commercial_terms(&query, &[binding.clone()]).is_ok());
    binding.price.minimum_fee = Some("1".parse().unwrap());
    binding.price.money_digest = money_digest(&binding.price);
    assert_eq!(
        validate_commercial_terms(&query, &[binding])
            .unwrap_err()
            .code,
        "UNSUPPORTED_TERMS"
    );
}

use bss_pricing::domain::{
    commercial_terms::{SaleObservation, validate_new_sale_observation},
    money,
};
use bss_pricing_sdk::{
    acceptance::{CommercialReason as R, NewSaleQuery, Term},
    digest::billing_terms_digest,
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
#[test]
fn floors_belong_to_the_subscription_period() {
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
                if hour || resource {
                    reason(&sale_query(), &[b], R::UnsupportedTerms);
                } else {
                    assert!(validate_commercial_terms(&sale_query(), &[b]).is_ok());
                }
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
        (|b| b.price.currency = "USD".into(), R::CurrencyMismatch),
        (|b| b.invoice.currency_scale = 3, R::UnsupportedTerms),
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
            value.parse::<Decimal>().unwrap()
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
    };
    assert!(validate_new_sale_observation(&good).is_ok());
    for name in [
        "superseded",
        "unavailable",
        "retired",
        "deprecated",
        "off_sale",
        "uncovered",
    ] {
        let mut o = good.clone();
        match name {
            "superseded" => o.revision_is_current = false,
            "unavailable" => o.revision_available = false,
            "retired" | "deprecated" => o.sku_active = false,
            "off_sale" => o.sku_sellable = false,
            "uncovered" => o.covered = false,
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
