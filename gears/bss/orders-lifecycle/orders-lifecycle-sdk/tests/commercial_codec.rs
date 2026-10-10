use bss_orders_lifecycle_sdk::{
    commercial::{
        AcceptedVersionRef, AssessmentRef, CodecError, MAX_VERSION_BYTES, OrderPin,
        term::TermIntent,
    },
    models::OrderVersion,
};
use bss_pricing_sdk::{acceptance::Term, terms::BillingCycle};
use serde_json::{Value, json};
const GOLDEN: &str = include_str!("fixtures/commercial-pin-v2.json");
#[test]
fn independent_golden_round_trips_native_receipt_without_loss() {
    let pin = OrderPin::decode(GOLDEN.as_bytes()).unwrap();
    let encoded = pin.encode().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&encoded).unwrap(),
        serde_json::from_str::<Value>(GOLDEN).unwrap()
    );
    assert_eq!(OrderPin::decode(&encoded).unwrap(), pin);
    let r = pin.receipt();
    assert_eq!(
        r.query.quantity.to_string(),
        "123456789.1234567890123456789"
    );
    assert_eq!(r.query.hold_policy_version, u64::MAX);
    assert_eq!(r.bindings[0].sku_version, i64::MAX);
    assert_eq!(r.accepted_at.nanosecond(), 123_456_789);
    assert_eq!(pin.activation_deadline(), r.hold_until);
    assert_ne!(
        r.query.selections[0].dimension_value,
        r.query.selections[1].dimension_value
    );
    pin.verify_selection(
        &r.query,
        r.acceptance_id,
        r.request_digest,
        r.terms_digest,
        &r.bindings,
    )
    .unwrap();
    let mut other = r.query.clone();
    other.quantity += rust_decimal::Decimal::ONE;
    assert_eq!(
        pin.verify_selection(
            &other,
            r.acceptance_id,
            r.request_digest,
            r.terms_digest,
            &r.bindings
        ),
        Err(CodecError::Mismatch)
    );
}
#[test]
fn unknown_schema_bad_links_digests_and_scalars_refuse() {
    let base: Value = serde_json::from_str(GOLDEN).unwrap();
    for (path, replacement) in [
        ("/schema_version", json!(1)),
        ("/schema_version", json!(99)),
        ("/receipt/query/billing_terms/schema_version", json!(2)),
        ("/accepted_version_ref/order_version", json!(1)),
        ("/accepted_version_ref/order_version", json!(0)),
        (
            "/activation_deadline",
            json!("2026-10-03T10:29:00.123456789Z"),
        ),
        ("/receipt/query/quantity", json!(1.25)),
        (
            "/receipt/query/quantity",
            json!("0.12345678901234567890123456789"),
        ),
        ("/receipt/query/quantity", json!("1e2")),
        ("/receipt/query/quantity", json!("NaN")),
        ("/receipt/query/quantity", json!("1.2")),
        (
            "/receipt/accepted_at",
            json!("2026-10-01T10:29:00.1234567891Z"),
        ),
        (
            "/receipt/accepted_at",
            json!("2026-10-01T12:29:00.123456789+02:00"),
        ),
        ("/receipt/request_digest", json!("AA".repeat(32))),
        ("/receipt/terms_digest", json!("00".repeat(32))),
        ("/receipt/bindings/0/price/state", json!("cancelled")),
        ("/receipt/bindings/0/price/state", json!("future")),
        ("/receipt/bindings/0/invoice/gl_code", json!("")),
        ("/receipt/bindings/2/meter", Value::Null),
        (
            "/receipt/bindings/0/invoice/template",
            json!("live replacement"),
        ),
        (
            "/receipt/bindings/2/usage_rating_policy/digest",
            json!("00".repeat(32)),
        ),
    ] {
        let mut changed = base.clone();
        *changed.pointer_mut(path).unwrap() = replacement;
        assert!(
            OrderPin::decode(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "accepted mutation {path}"
        );
    }
    assert!(OrderPin::decode(&vec![b' '; MAX_VERSION_BYTES + 1]).is_err());
    let duplicate = GOLDEN.replacen(
        "\"schema_version\": 2",
        "\"schema_version\": 2, \"schema_version\": 2",
        1,
    );
    assert!(OrderPin::decode(duplicate.as_bytes()).is_err());
}
fn field_paths(value: &Value, parent: &str, paths: &mut Vec<(String, String)>) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                paths.push((parent.into(), key.clone()));
                field_paths(value, &format!("{parent}/{key}"), paths);
            }
        }
        Value::Array(values) => {
            for (i, value) in values.iter().enumerate() {
                field_paths(value, &format!("{parent}/{i}"), paths);
            }
        }
        _ => {}
    }
}
#[test]
fn every_required_field_including_nullable_fields_is_checked() {
    let base: Value = serde_json::from_str(GOLDEN).unwrap();
    let mut paths = vec![];
    field_paths(&base, "", &mut paths);
    for (parent, key) in paths {
        let mut changed = base.clone();
        changed
            .pointer_mut(&parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(&key);
        assert!(
            OrderPin::decode(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "missing {parent}/{key} was defaulted"
        );
    }
    let mut extra = base;
    extra["receipt"]["query"]["invented_default"] = json!(true);
    assert!(OrderPin::decode(&serde_json::to_vec(&extra).unwrap()).is_err());
}
#[test]
fn issuer_hashes_do_not_replace_exact_binding_comparison() {
    let pin = OrderPin::decode(GOLDEN.as_bytes()).unwrap();
    let r = pin.receipt();
    let mut bindings = r.bindings.clone();
    bindings[2].meter.as_mut().unwrap().version = "changed".into();
    // Current public Pricing digest excludes meter. Preserve it and compare full values anyway.
    assert_eq!(
        bss_pricing_sdk::digest::terms_digest(&r.query, &bindings),
        r.terms_digest
    );
    assert_eq!(
        pin.verify_selection(
            &r.query,
            r.acceptance_id,
            r.request_digest,
            r.terms_digest,
            &bindings
        ),
        Err(CodecError::Mismatch)
    );
    // Construction uses an independently supplied committed row identity.
    let assessment = AssessmentRef {
        assessment_id: uuid::Uuid::from_u128(30),
        assessed_at: r.accepted_at,
        resolve_date: r.accepted_at.date(),
    };
    let selected = AcceptedVersionRef {
        order_id: r.query.order_id,
        order_version: OrderVersion::try_from(3).unwrap(),
        line_id: r.query.line_id,
    };
    assert_eq!(
        OrderPin::new(assessment, selected, r.clone()),
        Err(CodecError::Mismatch)
    );
}
#[test]
fn calendar_terms_preserve_intent_and_never_approximate_days() {
    use TermIntent::{Calendar, Missing, Periods, Rolling};
    assert_eq!(
        Calendar {
            years: 1,
            months: 6,
            days: 0,
            nanoseconds: 0
        }
        .resolve(Some(BillingCycle::Month))
        .unwrap(),
        Term::FixedPeriods { count: 18 }
    );
    assert_eq!(
        Calendar {
            years: 1,
            months: 0,
            days: 0,
            nanoseconds: 0
        }
        .resolve(Some(BillingCycle::Year))
        .unwrap(),
        Term::FixedPeriods { count: 1 }
    );
    assert_eq!(Rolling.resolve(None).unwrap(), Term::Rolling);
    for intent in [
        Missing,
        Periods(0),
        Calendar {
            years: 0,
            months: 0,
            days: 365,
            nanoseconds: 0,
        },
        Calendar {
            years: 1,
            months: 0,
            days: 1,
            nanoseconds: 0,
        },
        Calendar {
            years: 1,
            months: 0,
            days: 0,
            nanoseconds: 1,
        },
        Calendar {
            years: u64::MAX,
            months: 0,
            days: 0,
            nanoseconds: 0,
        },
        Calendar {
            years: 0,
            months: u64::from(u32::MAX) + 1,
            days: 0,
            nanoseconds: 0,
        },
    ] {
        assert!(intent.resolve(Some(BillingCycle::Month)).is_err());
    }
    assert!(
        Calendar {
            years: 1,
            months: 6,
            days: 0,
            nanoseconds: 0
        }
        .resolve(Some(BillingCycle::Year))
        .is_err()
    );
    assert!(Periods(12).resolve(None).is_err());
}

#[test]
fn native_variants_remain_lossless_without_claiming_sale_eligibility() {
    use bss_pricing_sdk::{
        digest,
        read::{BindingSelection, PriceModel, ResolvedBindings, ResolvedCell, Tier},
        terms::{AggregationScope, BillingAnchor, InputSource, RatingWindow, TermsSource},
    };
    use rust_decimal::Decimal;
    let original = OrderPin::decode(GOLDEN.as_bytes()).unwrap();
    for (index, model) in [
        PriceModel::Flat {
            amount: Decimal::MAX,
        },
        PriceModel::PerUnit {
            unit_amount: Decimal::from_str_exact("0.0000000000000000000000000001").unwrap(),
        },
        PriceModel::Volume {
            tiers: vec![Tier {
                up_to: None,
                rate: Decimal::ZERO,
            }],
        },
        PriceModel::Graduated {
            tiers: vec![
                Tier {
                    up_to: Some(Decimal::ONE),
                    rate: Decimal::ONE,
                },
                Tier {
                    up_to: None,
                    rate: Decimal::ZERO,
                },
            ],
        },
        PriceModel::Package {
            package_size: Decimal::TEN,
            package_price: Decimal::ONE,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let mut receipt = original.receipt().clone();
        receipt.bindings[2].price.model = model;
        receipt.bindings[2].price.minimum_fee = Some(Decimal::from_str_exact("10.000").unwrap());
        receipt.bindings[2].price.ends_on = Some(receipt.query.start_at.date());
        receipt.bindings[0].invoice.template_source = InputSource::Entry;
        receipt.bindings[1].invoice.template_source = InputSource::SellerSettings;
        let policy = receipt.bindings[2].usage_rating_policy.as_mut().unwrap();
        policy.content.rating_window = RatingWindow::BillingCycle;
        policy.content.aggregation_scope = AggregationScope::Resource;
        policy.digest = digest::policy_digest(&policy.content);
        receipt.query.term = Term::Rolling;
        receipt.query.billing_terms.cycle = BillingCycle::Year;
        receipt.query.billing_terms.anchor = BillingAnchor::SubscriptionStart;
        receipt.query.billing_terms.source = TermsSource::SellerPolicy {
            id: uuid::Uuid::from_u128(99),
            version: u64::MAX,
        };
        // Synthetic serialization corpus. Pricing's live validator owns model compatibility.
        for b in &mut receipt.bindings {
            b.price.money_digest = digest::money_digest(&b.price);
        }
        receipt.query.billing_terms.digest =
            digest::billing_terms_digest(&receipt.query.billing_terms);
        let resolved = ResolvedBindings {
            plan_id: receipt.query.plan_id,
            revision_id: receipt.query.plan_revision_id,
            cells: receipt
                .bindings
                .iter()
                .map(|b| ResolvedCell {
                    selection: BindingSelection {
                        item_id: b.item_id,
                        dimension_value: b.dimension_value.clone(),
                    },
                    binding: Some(b.clone()),
                })
                .collect(),
        };
        receipt.query.resolved_bindings_digest =
            digest::selected_bindings_digest(&resolved, &receipt.query.selections).unwrap();
        receipt.request_digest = digest::request_digest(&receipt.query);
        receipt.terms_digest = digest::terms_digest(&receipt.query, &receipt.bindings);
        let pin = OrderPin::new(
            AssessmentRef {
                assessment_id: uuid::Uuid::from_u128(30),
                assessed_at: receipt.accepted_at,
                resolve_date: receipt.accepted_at.date(),
            },
            AcceptedVersionRef {
                order_id: receipt.query.order_id,
                order_version: OrderVersion::try_from(2).unwrap(),
                line_id: receipt.query.line_id,
            },
            receipt.clone(),
        )
        .unwrap();
        let result = OrderPin::decode(&pin.encode().unwrap()).unwrap();
        assert_eq!(result.receipt(), &receipt, "native model {index}");
        assert_eq!(
            result.receipt().bindings[2]
                .price
                .minimum_fee
                .unwrap()
                .scale(),
            3
        );
    }
}

#[test]
fn counts_and_encoder_size_are_checked_before_persistence() {
    use bss_orders_lifecycle_sdk::commercial::MAX_BINDINGS;
    let base: Value = serde_json::from_str(GOLDEN).unwrap();
    let mut too_many = base.clone();
    too_many["receipt"]["bindings"] = json!(vec![
        base["receipt"]["bindings"][0].clone();
        MAX_BINDINGS + 1
    ]);
    assert!(OrderPin::decode(&serde_json::to_vec(&too_many).unwrap()).is_err());
    let mut duplicate = base;
    duplicate["receipt"]["bindings"][1] = duplicate["receipt"]["bindings"][0].clone();
    assert!(OrderPin::decode(&serde_json::to_vec(&duplicate).unwrap()).is_err());
    let original = OrderPin::decode(GOLDEN.as_bytes()).unwrap();
    let mut receipt = original.receipt().clone();
    // Meter is not in the issuer digest, so this isolates the codec's encoder-size check.
    receipt.bindings[2].meter.as_mut().unwrap().version = "v".repeat(MAX_VERSION_BYTES);
    let assessment = AssessmentRef {
        assessment_id: uuid::Uuid::from_u128(30),
        assessed_at: receipt.accepted_at,
        resolve_date: receipt.accepted_at.date(),
    };
    let selected = AcceptedVersionRef {
        order_id: receipt.query.order_id,
        order_version: OrderVersion::try_from(2).unwrap(),
        line_id: receipt.query.line_id,
    };
    assert_eq!(
        OrderPin::new(assessment, selected, receipt),
        Err(CodecError::TooLarge)
    );
}
