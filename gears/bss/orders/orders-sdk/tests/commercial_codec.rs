//! Schema-2 `OrderPin` codec: the golden round trip, the exact refusal of every rule, and
//! property tests over arbitrary input and resealed receipt variations.
#![expect(
    clippy::unwrap_used,
    reason = "test code: the shared helpers unwrap outside a `#[test]` function, and a failed \
              setup or fixture must stop the test at once"
)]
use bss_orders_sdk::{
    authoring::AuthoredTerm,
    commercial::{
        AcceptedVersionRef, AssessmentRef, CodecError, MAX_BINDINGS, MAX_VERSION_BYTES,
        MalformedKind, OrderPin, SelectionEvidence, term::TermIntent,
    },
    models::OrderVersion,
};
use bss_pricing_sdk::{
    acceptance::{AcceptanceReceipt, Term},
    digest,
    read::{BindingSelection, PriceState, ResolvedBindings, ResolvedCell},
    terms::BillingCycle,
};
use proptest::prelude::*;
use serde_json::{Value, json};
use uuid::Uuid;
const GOLDEN: &str = include_str!("fixtures/commercial-pin-v2.json");
const DATA: CodecError = CodecError::Malformed(MalformedKind::Data);

fn golden() -> OrderPin {
    OrderPin::decode(GOLDEN.as_bytes()).unwrap()
}
fn decode_value(value: &Value) -> Result<OrderPin, CodecError> {
    OrderPin::decode(&serde_json::to_vec(value).unwrap())
}
/// Wrap a receipt with the golden's assessment and the committed row it names.
fn new_pin(receipt: AcceptanceReceipt) -> Result<OrderPin, CodecError> {
    let assessment = AssessmentRef {
        assessment_id: Uuid::from_u128(30),
        assessed_at: receipt.accepted_at,
        resolve_date: receipt.accepted_at.date(),
    };
    let selected = AcceptedVersionRef {
        order_id: receipt.query.order_id,
        order_version: OrderVersion::try_from(2).unwrap(),
        line_id: receipt.query.line_id,
    };
    OrderPin::new(assessment, selected, receipt)
}
/// Recompute every issuer digest the way Pricing signs a receipt (money, billing terms, selected
/// bindings, request, terms), so a mutation reaches the one codec rule it is meant to break
/// instead of failing an issuer digest first. Policy and template digests are content digests a
/// rule checks, so they are left as they are; a binding the selected-bindings digest refuses
/// keeps the old digest, and the codec refuses it through the same function.
fn reseal(receipt: &mut AcceptanceReceipt) {
    for b in &mut receipt.bindings {
        b.price.money_digest = digest::money_digest(&b.price);
    }
    receipt.query.billing_terms.digest = digest::billing_terms_digest(&receipt.query.billing_terms);
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
    if let Ok(selected) = digest::selected_bindings_digest(&resolved, &receipt.query.selections) {
        receipt.query.resolved_bindings_digest = selected;
    }
    receipt.request_digest = digest::request_digest(&receipt.query);
    receipt.terms_digest = digest::terms_digest(&receipt.query, &receipt.bindings);
}
fn evidence(receipt: &AcceptanceReceipt) -> SelectionEvidence<'_> {
    SelectionEvidence {
        acceptance_id: receipt.acceptance_id,
        request_digest: receipt.request_digest,
        terms_digest: receipt.terms_digest,
        evaluated_bindings: &receipt.bindings,
    }
}

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
    pin.verify_selection(&r.query, evidence(r)).unwrap();
    let mut other = r.query.clone();
    other.quantity += rust_decimal::Decimal::ONE;
    assert_eq!(
        pin.verify_selection(&other, evidence(r)),
        Err(CodecError::Mismatch)
    );
    // The two issuer digests are named, and each is compared on its own.
    let swapped = SelectionEvidence {
        request_digest: r.terms_digest,
        terms_digest: r.request_digest,
        ..evidence(r)
    };
    assert_eq!(
        pin.verify_selection(&r.query, swapped),
        Err(CodecError::Mismatch)
    );
}

#[test]
fn reseal_is_the_identity_on_a_valid_receipt() {
    let pin = golden();
    let mut receipt = pin.receipt().clone();
    reseal(&mut receipt);
    assert_eq!(&receipt, pin.receipt());
    assert_eq!(new_pin(receipt).unwrap().receipt(), pin.receipt());
}

/// Stored-document rules, each with its exact refusal. The decode-level rules (schema, links
/// outside the issuer digests, strict scalars, duplicate members) fail before any digest is
/// compared, so the exact variant pins the rule itself.
#[test]
fn unknown_schema_bad_links_and_scalars_refuse_with_their_exact_error() {
    let base: Value = serde_json::from_str(GOLDEN).unwrap();
    for (path, replacement, expected) in [
        ("/schema_version", json!(1), CodecError::Unsupported),
        ("/schema_version", json!(99), CodecError::Unsupported),
        (
            "/receipt/query/billing_terms/schema_version",
            json!(2),
            CodecError::Unsupported,
        ),
        // Pin-level links are outside every issuer digest: only the link rule can refuse.
        (
            "/accepted_version_ref/order_version",
            json!(1),
            CodecError::Mismatch,
        ),
        (
            "/activation_deadline",
            json!("2026-10-03T10:29:00.123456789Z"),
            CodecError::Mismatch,
        ),
        ("/accepted_version_ref/order_version", json!(0), DATA),
        ("/receipt/query/quantity", json!(1.25), DATA),
        (
            "/receipt/query/quantity",
            json!("0.12345678901234567890123456789"),
            DATA,
        ),
        ("/receipt/query/quantity", json!("1e2"), DATA),
        ("/receipt/query/quantity", json!("NaN"), DATA),
        (
            "/receipt/accepted_at",
            json!("2026-10-01T10:29:00.1234567891Z"),
            DATA,
        ),
        (
            "/receipt/accepted_at",
            json!("2026-10-01T12:29:00.123456789+02:00"),
            DATA,
        ),
        ("/receipt/request_digest", json!("AA".repeat(32)), DATA),
        ("/receipt/bindings/0/price/state", json!("future"), DATA),
        // A different well-formed value is caught by the issuer digests alone.
        (
            "/receipt/query/quantity",
            json!("1.2"),
            CodecError::Mismatch,
        ),
        (
            "/receipt/terms_digest",
            json!("00".repeat(32)),
            CodecError::Mismatch,
        ),
    ] {
        let mut changed = base.clone();
        *changed.pointer_mut(path).unwrap() = replacement.clone();
        assert_eq!(
            decode_value(&changed),
            Err(expected),
            "{path} = {replacement}"
        );
    }
    let duplicate = GOLDEN.replacen(
        "\"schema_version\": 2",
        "\"schema_version\": 2, \"schema_version\": 2",
        1,
    );
    assert_eq!(OrderPin::decode(duplicate.as_bytes()), Err(DATA));
    assert_eq!(
        OrderPin::decode(b"{\"schema_version\": 2"),
        Err(CodecError::Malformed(MalformedKind::Eof))
    );
    assert_eq!(
        OrderPin::decode(b"not json"),
        Err(CodecError::Malformed(MalformedKind::Syntax))
    );
}

/// Receipt rules an issuer could sign: each mutation is resealed, so the exact refusal comes
/// from the codec rule, not from an issuer digest.
#[test]
fn resealed_receipt_rules_refuse_with_their_exact_error() {
    type Mutation = fn(&mut AcceptanceReceipt);
    let cases: [(&str, Mutation, CodecError); 10] = [
        (
            "cancelled price",
            |r| r.bindings[0].price.state = PriceState::Cancelled,
            CodecError::Mismatch,
        ),
        (
            "empty gl code",
            |r| r.bindings[0].invoice.gl_code = String::new(),
            CodecError::Incomplete,
        ),
        (
            "blank tax category",
            |r| r.bindings[1].invoice.tax_category = " ".into(),
            CodecError::Incomplete,
        ),
        (
            "usage binding without meter",
            |r| r.bindings[2].meter = None,
            CodecError::Incomplete,
        ),
        (
            "usage binding without rating policy",
            |r| r.bindings[2].usage_rating_policy = None,
            CodecError::Incomplete,
        ),
        (
            "template replaced under its digest",
            |r| r.bindings[0].invoice.template = "live replacement".into(),
            CodecError::Mismatch,
        ),
        (
            "rating policy digest",
            |r| r.bindings[2].usage_rating_policy.as_mut().unwrap().digest = [0; 32],
            CodecError::Mismatch,
        ),
        (
            "hold not after acceptance",
            |r| r.hold_until = r.accepted_at,
            CodecError::Mismatch,
        ),
        (
            "zero quantity",
            |r| r.query.quantity = rust_decimal::Decimal::ZERO,
            CodecError::Incomplete,
        ),
        (
            "nil plan",
            |r| r.query.plan_id = Uuid::nil(),
            CodecError::Incomplete,
        ),
    ];
    for (name, mutate, expected) in cases {
        let mut receipt = golden().receipt().clone();
        mutate(&mut receipt);
        reseal(&mut receipt);
        assert_eq!(new_pin(receipt).map(drop), Err(expected), "{name}");
    }
}

/// An encoder refusal is `Unsupported`, not `Malformed`: nothing was decoded (D-220).
#[test]
fn a_value_the_encoder_refuses_is_unsupported() {
    let mut receipt = golden().receipt().clone();
    // `scalars::date` writes four-digit years only; the codec rules do not read `ends_on`.
    receipt.bindings[0].price.ends_on =
        Some(time::Date::from_calendar_date(-1, time::Month::January, 1).unwrap());
    reseal(&mut receipt);
    assert_eq!(new_pin(receipt).map(drop), Err(CodecError::Unsupported));
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
        assert_eq!(
            decode_value(&changed).map(drop),
            Err(DATA),
            "missing {parent}/{key} was defaulted"
        );
    }
    let mut extra = base;
    extra["receipt"]["query"]["invented_default"] = json!(true);
    assert_eq!(decode_value(&extra).map(drop), Err(DATA));
}
#[test]
fn issuer_hashes_do_not_replace_exact_binding_comparison() {
    let pin = golden();
    let r = pin.receipt();
    let mut bindings = r.bindings.clone();
    bindings[2].meter.as_mut().unwrap().version = "changed".into();
    // Current public Pricing digest excludes meter. Preserve it and compare full values anyway.
    assert_eq!(digest::terms_digest(&r.query, &bindings), r.terms_digest);
    assert_eq!(
        pin.verify_selection(
            &r.query,
            SelectionEvidence {
                evaluated_bindings: &bindings,
                ..evidence(r)
            },
        ),
        Err(CodecError::Mismatch)
    );
    // Construction uses an independently supplied committed row identity.
    let assessment = AssessmentRef {
        assessment_id: Uuid::from_u128(30),
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
fn one_currency_at_one_scale_across_the_receipt() {
    // Issuer digests are resealed, so only the scale rule (bss_money `ensure_same`) refuses.
    let mut receipt = golden().receipt().clone();
    receipt.bindings[1].price.currency = bss_money::CurrencySpec::try_new("EUR".into(), 3).unwrap();
    reseal(&mut receipt);
    assert_eq!(
        new_pin(receipt.clone()).map(drop),
        Err(CodecError::Mismatch)
    );
    // The same receipt at one scale is accepted.
    for b in &mut receipt.bindings {
        b.price.currency = bss_money::CurrencySpec::try_new("EUR".into(), 3).unwrap();
    }
    reseal(&mut receipt);
    assert!(new_pin(receipt).is_ok());
}
#[test]
fn calendar_terms_preserve_intent_and_never_approximate_days() {
    use TermIntent::{Calendar, Missing, Periods, Rolling};
    let calendar = |years, months, days, microseconds| Calendar {
        years,
        months,
        days,
        microseconds,
    };
    assert_eq!(
        calendar(1, 6, 0, 0).resolve(Some(BillingCycle::Month)),
        Ok(Term::FixedPeriods { count: 18 })
    );
    assert_eq!(
        calendar(1, 0, 0, 0).resolve(Some(BillingCycle::Year)),
        Ok(Term::FixedPeriods { count: 1 })
    );
    assert_eq!(Rolling.resolve(None), Ok(Term::Rolling));
    assert_eq!(
        Periods(12).resolve(Some(BillingCycle::Year)),
        Ok(Term::FixedPeriods { count: 12 })
    );
    for (intent, cycle, expected) in [
        // Absent input is incomplete: the intent, or the cycle periods need (D-220).
        (Missing, Some(BillingCycle::Month), CodecError::Incomplete),
        (Missing, None, CodecError::Incomplete),
        (Periods(12), None, CodecError::Incomplete),
        (calendar(1, 0, 0, 0), None, CodecError::Incomplete),
        // Present input the invoice periods cannot express exactly is unsupported.
        (
            Periods(0),
            Some(BillingCycle::Month),
            CodecError::Unsupported,
        ),
        (
            calendar(0, 0, 0, 0),
            Some(BillingCycle::Month),
            CodecError::Unsupported,
        ),
        (
            calendar(0, 0, 365, 0),
            Some(BillingCycle::Month),
            CodecError::Unsupported,
        ),
        (
            calendar(1, 0, 1, 0),
            Some(BillingCycle::Month),
            CodecError::Unsupported,
        ),
        (
            calendar(1, 0, 0, 1),
            Some(BillingCycle::Month),
            CodecError::Unsupported,
        ),
        (
            calendar(1, 6, 0, 0),
            Some(BillingCycle::Year),
            CodecError::Unsupported,
        ),
        (
            calendar(u64::MAX, 0, 0, 0),
            Some(BillingCycle::Month),
            CodecError::Unsupported,
        ),
        (
            calendar(0, u64::from(u32::MAX) + 1, 0, 0),
            Some(BillingCycle::Month),
            CodecError::Unsupported,
        ),
    ] {
        assert_eq!(intent.resolve(cycle), Err(expected), "{intent:?} {cycle:?}");
    }
}
#[test]
fn the_authored_term_converts_once_with_its_units_unchanged() {
    assert_eq!(TermIntent::from(None), TermIntent::Missing);
    assert_eq!(
        TermIntent::from(Some(AuthoredTerm::Rolling)),
        TermIntent::Rolling
    );
    assert_eq!(
        TermIntent::from(Some(AuthoredTerm::Periods { count: 7 })),
        TermIntent::Periods(7)
    );
    assert_eq!(
        TermIntent::from(Some(AuthoredTerm::Calendar {
            years: u32::MAX,
            months: 2,
            days: 3,
            microseconds: u64::MAX,
        })),
        TermIntent::Calendar {
            years: u64::from(u32::MAX),
            months: 2,
            days: 3,
            microseconds: u64::MAX,
        }
    );
}

#[test]
fn native_variants_remain_lossless_without_claiming_sale_eligibility() {
    use bss_pricing_sdk::{
        read::{PriceModel, Tier},
        terms::{AggregationScope, BillingAnchor, InputSource, RatingWindow, TermsSource},
    };
    use rust_decimal::Decimal;
    let original = golden();
    for (index, model) in [
        PriceModel::Flat {
            // The largest amount `bss_money` carries: a 28-digit coefficient.
            amount: Decimal::from_str_exact("9999999999999999999999999999").unwrap(),
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
        reseal(&mut receipt);
        let pin = new_pin(receipt.clone()).unwrap();
        let result = OrderPin::decode(&pin.encode().unwrap()).unwrap();
        assert_eq!(result.receipt(), &receipt, "native model {index}");
        // Canonical `bss_money` text (D-213): `10.000` is carried and read back as `10`.
        assert_eq!(
            result.receipt().bindings[2]
                .price
                .minimum_fee
                .unwrap()
                .scale(),
            0
        );
    }
}

#[test]
fn the_size_limit_refuses_a_valid_document_one_byte_over() {
    let trimmed = GOLDEN.trim_end();
    let padded = |len: usize| {
        let mut bytes = trimmed.as_bytes().to_vec();
        bytes.resize(len, b' ');
        bytes
    };
    // Trailing whitespace is valid JSON: at the limit the padded golden still decodes.
    assert_eq!(
        OrderPin::decode(&padded(MAX_VERSION_BYTES)).unwrap(),
        golden()
    );
    assert_eq!(
        OrderPin::decode(&padded(MAX_VERSION_BYTES + 1)),
        Err(CodecError::TooLarge)
    );
}

/// `MAX_BINDINGS` distinct bindings with matching selections are accepted; one more is refused
/// by the count cap itself, not by a count mismatch, a duplicate or the encoded-size check.
#[test]
fn the_binding_count_cap_refuses_one_distinct_binding_over() {
    let template = golden().receipt().clone();
    let receipt_of = |count: usize| {
        let mut receipt = template.clone();
        let mut one = receipt.bindings[0].clone();
        // A compact binding, so a receipt one binding over the cap still fits the version size.
        one.sku_code = "S".into();
        one.sku_name = "S".into();
        one.sku_version = 1;
        one.invoice.template = "t".into();
        one.invoice.template_digest = digest::template_digest(&one.invoice.template);
        receipt.bindings = (0..count)
            .map(|i| {
                let mut b = one.clone();
                b.item_id = Uuid::from_u128(1_000_000 + i as u128);
                b
            })
            .collect();
        receipt.query.selections = receipt
            .bindings
            .iter()
            .map(|b| BindingSelection {
                item_id: b.item_id,
                dimension_value: b.dimension_value.clone(),
            })
            .collect();
        reseal(&mut receipt);
        receipt
    };
    let at_cap = new_pin(receipt_of(MAX_BINDINGS)).unwrap();
    let encoded = at_cap.encode().unwrap().len();
    // Precondition: one more binding and selection still encode under the version limit, so
    // only the count cap can refuse the next receipt.
    assert!(
        encoded + encoded.div_ceil(MAX_BINDINGS) < MAX_VERSION_BYTES,
        "{encoded} bytes at the cap"
    );
    assert_eq!(
        new_pin(receipt_of(MAX_BINDINGS + 1)).map(drop),
        Err(CodecError::TooLarge)
    );
}

/// The duplicate-item rule on its own: two bindings of one item under different dimension
/// values are distinct selections an issuer can sign, so the resealed digests all agree and only
/// the item uniqueness rule refuses.
#[test]
fn a_duplicate_item_under_valid_issuer_digests_is_a_mismatch() {
    let mut receipt = golden().receipt().clone();
    receipt.bindings[1].item_id = receipt.bindings[0].item_id;
    receipt.query.selections[1].item_id = receipt.bindings[0].item_id;
    reseal(&mut receipt);
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
    // Precondition: the issuer digest accepts the duplicated item.
    assert_eq!(
        digest::selected_bindings_digest(&resolved, &receipt.query.selections).unwrap(),
        receipt.query.resolved_bindings_digest
    );
    assert_eq!(new_pin(receipt).map(drop), Err(CodecError::Mismatch));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// No input panics the decoder, and arbitrary bytes are never a pin.
    #[test]
    fn arbitrary_bytes_never_decode(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        prop_assert!(OrderPin::decode(&bytes).is_err());
    }

    /// Every proper prefix of a valid pin ends early.
    #[test]
    fn a_truncated_pin_is_malformed(cut in 0..GOLDEN.trim_end().len()) {
        prop_assert_eq!(
            OrderPin::decode(&GOLDEN.trim_end().as_bytes()[..cut]).map(drop),
            Err(CodecError::Malformed(MalformedKind::Eof))
        );
    }

    /// Resealed receipt variations round-trip exactly: decode(encode(pin)) == pin, and the
    /// re-encoded bytes are the same bytes.
    #[test]
    fn resealed_receipt_variations_round_trip(
        mantissa in 1_i64..i64::MAX,
        scale in 0_u32..=18,
        accepted_secs in 0_i64..100_000_000,
        accepted_nanos in 0_u32..1_000_000_000,
        hold_secs in 1_i64..100_000_000,
        dimensions in proptest::collection::vec(proptest::option::of("[a-z0-9-]{1,16}"), 2),
        term in proptest::option::of(1_u32..),
        sku_version in 1_i64..,
        hold_policy_version in 1_u64..,
        payer in 1_u128..,
    ) {
        let mut receipt = golden().receipt().clone();
        receipt.query.quantity = rust_decimal::Decimal::new(mantissa, scale).normalize();
        receipt.accepted_at = (receipt.accepted_at + time::Duration::seconds(accepted_secs))
            .replace_nanosecond(accepted_nanos)
            .unwrap();
        receipt.hold_until = receipt.accepted_at + time::Duration::seconds(hold_secs);
        for (i, dimension) in dimensions.into_iter().enumerate() {
            receipt.bindings[i + 1].dimension_value.clone_from(&dimension);
            receipt.query.selections[i + 1].dimension_value = dimension;
        }
        receipt.query.term = term.map_or(Term::Rolling, |count| Term::FixedPeriods { count });
        receipt.bindings[0].sku_version = sku_version;
        receipt.query.hold_policy_version = hold_policy_version;
        receipt.query.tenant_axes.payer_tenant_id = Uuid::from_u128(payer);
        reseal(&mut receipt);
        let pin = new_pin(receipt).unwrap();
        let encoded = pin.encode().unwrap();
        let decoded = OrderPin::decode(&encoded).unwrap();
        prop_assert_eq!(&decoded, &pin);
        prop_assert_eq!(decoded.encode().unwrap(), encoded);
    }
}
