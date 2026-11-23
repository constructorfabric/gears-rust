#![allow(clippy::expect_used, clippy::unwrap_used)]
// Compile the private encoder as a test module; no untyped production SDK door is exported.
#[allow(dead_code)]
#[path = "../src/digest.rs"]
pub mod digest;
use bss_pricing_sdk::{Digest, acceptance, plan_quota, read, terms};
use digest::{CanonicalValue, canonical_json_bytes, hash_document};
use read::{ImmutablePrice, PriceModel};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn restricted(v: &Value) -> Result<CanonicalValue, &'static str> {
    Ok(match v {
        Value::Null => CanonicalValue::Null,
        Value::Bool(b) => CanonicalValue::Bool(*b),
        Value::String(s) => CanonicalValue::String(s.clone()),
        Value::Array(a) => {
            CanonicalValue::Array(a.iter().map(restricted).collect::<Result<_, _>>()?)
        }
        Value::Object(o) => CanonicalValue::Object(
            o.iter()
                .map(|(k, v)| Ok::<_, &'static str>((k.clone(), restricted(v)?)))
                .collect::<Result<_, _>>()?,
        ),
        Value::Number(_) => return Err("numbers are forbidden"),
    })
}
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/digests.json")).unwrap()
}
fn hex(digest: Digest) -> String {
    use std::fmt::Write;
    digest.iter().fold(String::new(), |mut out, b| {
        write!(out, "{b:02x}").unwrap();
        out
    })
}
fn price(rate: &str) -> ImmutablePrice {
    ImmutablePrice {
        price_id: uuid::Uuid::from_u128(1),
        price_book_entry_id: uuid::Uuid::from_u128(2),
        money_digest: [0; 32],
        currency: bss_money::CurrencySpec::try_new("EUR".into(), 2).unwrap(),
        model: PriceModel::PerUnit {
            unit_amount: Decimal::from_str_exact(rate).unwrap(),
        },
        minimum_fee: None,
        effective_from: time::Date::from_calendar_date(2026, time::Month::September, 1).unwrap(),
        ends_on: None,
        state: read::PriceState::Approved,
    }
}
#[test]
fn frozen_canonical_bytes_and_sha256_match_for_every_vector() {
    for vector in fixture()["vectors"].as_array().unwrap() {
        let domain = vector["domain"].as_str().unwrap();
        let payload = restricted(&vector["payload"]).unwrap();
        let document = CanonicalValue::Object(BTreeMap::from([
            ("domain".into(), CanonicalValue::String(domain.into())),
            ("payload".into(), payload.clone()),
        ]));
        assert_eq!(
            String::from_utf8(canonical_json_bytes(&document)).unwrap(),
            vector["canonical_text"]
        );
        assert_eq!(hex(hash_document(domain, payload)), vector["sha256"]);
    }
}
#[test]
fn semantic_inputs_are_normalized_before_hashing() {
    let fixture = fixture();
    for case in fixture["semantic_projections"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let vector =
            &fixture["vectors"][usize::try_from(case["vector"].as_u64().unwrap()).unwrap()];
        let payload = match case["kind"].as_str().unwrap() {
            "money" => {
                let p = price(input);
                assert_eq!(
                    hex(bss_pricing_sdk::digest::money_digest(&p)),
                    vector["sha256"]
                );
                json!({"currency":"EUR","currency_scale":"2","model":{"kind":"per_unit","unit_amount":Decimal::from_str_exact(input).unwrap().normalize().to_string()},"minimum_fee":null})
            }
            "u64" => json!({"version":input.parse::<u64>().unwrap().to_string()}),
            "instant" => {
                let at = time::OffsetDateTime::parse(
                    input,
                    &time::format_description::well_known::Rfc3339,
                )
                .unwrap()
                .to_offset(time::UtcOffset::UTC);
                json!({"at":format!("{}T{:02}:{:02}:{:02}.{:09}Z",at.date(),at.hour(),at.minute(),at.second(),at.nanosecond())})
            }
            other => panic!("unknown projection {other}"),
        };
        assert_eq!(payload, vector["payload"]);
    }
}
#[test]
fn numeric_and_malformed_unicode_inputs_are_rejected() {
    assert!(restricted(&json!({"amount":0.047})).is_err());
    let price = price("0.047");
    assert_eq!(hex(bss_pricing_sdk::digest::money_digest(&price)).len(), 64);
}
#[test]
fn money_ignores_identity_and_closure_but_covers_every_operand() {
    let original = price("0.047");
    let baseline = bss_pricing_sdk::digest::money_digest(&original);
    let mut changed = original.clone();
    changed.price_id = uuid::Uuid::from_u128(3);
    changed.price_book_entry_id = uuid::Uuid::from_u128(4);
    changed.ends_on = Some(original.effective_from);
    changed.money_digest = [9; 32];
    assert_eq!(baseline, bss_pricing_sdk::digest::money_digest(&changed));
    changed.currency = bss_money::CurrencySpec::try_new("USD".into(), 2).unwrap();
    assert_ne!(baseline, bss_pricing_sdk::digest::money_digest(&changed));
    changed = original;
    changed.minimum_fee = Some(Decimal::ONE);
    assert_ne!(baseline, bss_pricing_sdk::digest::money_digest(&changed));
    changed = price("0.048");
    assert_ne!(baseline, bss_pricing_sdk::digest::money_digest(&changed));
}
/// The scale is part of the money: the same amounts at another scale are other money, since the
/// minimum fee and the invoice line post at it. Only the scale changes here.
#[test]
fn money_digest_covers_the_currency_scale() {
    use bss_pricing_sdk::digest::money_digest;
    let original = price("0.047");
    let mut rescaled = original.clone();
    rescaled.currency = bss_money::CurrencySpec::try_new("EUR".into(), 3).unwrap();
    assert_eq!(rescaled.currency.code(), original.currency.code());
    assert_eq!(rescaled.model, original.model);
    assert_ne!(
        money_digest(&original),
        money_digest(&rescaled),
        "another scale is other money"
    );
}
/// A binding's price carries its scale on its own, not only through its money digest: with the
/// money digest held still, the scale alone still changes the accepted terms.
#[test]
fn the_binding_price_projection_carries_the_scale() {
    use bss_pricing_sdk::digest::terms_digest;
    let q = commercial_query();
    let original = sample_binding();
    let mut rescaled = original.clone();
    rescaled.price.currency = bss_money::CurrencySpec::try_new("EUR".into(), 3).unwrap();
    assert_eq!(rescaled.price.money_digest, original.price.money_digest);
    assert_ne!(
        terms_digest(&q, &[original], None),
        terms_digest(&q, &[rescaled], None),
        "the price projection names the scale"
    );
}

#[test]
fn read_contract_ir_is_valid_and_every_method_is_safe_read() {
    let ir = read::pricing_read_v1_ir();
    toolkit_contract::ir::validate_contract(&ir).unwrap();
    assert_eq!(ir.methods.len(), 3);
    assert!(
        ir.methods
            .iter()
            .all(|m| m.idempotency == toolkit_contract::ir::contract::Idempotency::SafeRead)
    );
}

fn policy_input() -> terms::UsageRatingPolicyInput {
    terms::UsageRatingPolicyInput {
        rating_window: terms::RatingWindow::CalendarHour {
            timezone: terms::Timezone::Utc,
        },
        aggregation_scope: terms::AggregationScope::SubscriptionLine,
        reset: terms::Reset::RatingWindowStart,
        partial_window: terms::PartialWindow::ActualQuantityFullThresholds,
        fold: terms::Fold::Sum,
    }
}
fn sample_binding() -> read::AcceptedBinding {
    use bss_pricing_sdk::digest::{money_digest, policy_digest, template_digest};
    let mut price = price("0.04700");
    price.money_digest = money_digest(&price);
    let content = policy_input();
    read::AcceptedBinding {
        item_id: uuid::Uuid::from_u128(3),
        price_book_entry_id: uuid::Uuid::from_u128(2),
        dimension_key: Some("region".into()),
        dimension_value: Some("eu".into()),
        sku_id: uuid::Uuid::from_u128(4),
        sku_version: 1,
        sku_code: "CLOUD".into(),
        sku_name: "Cloudlet".into(),
        unit: Some("cloudlet_hour".into()),
        meter: Some(terms::MeterRef {
            usage_type_id: "cloudlets".into(),
            version: "1".into(),
        }),
        price,
        kind: read::ChargeKind::Usage,
        recurring_period: None,
        via_default: true,
        usage_rating_policy: Some(terms::UsageRatingPolicy {
            policy_id: uuid::Uuid::from_u128(5),
            version: u64::MAX,
            digest: policy_digest(&content),
            content,
        }),
        invoice: terms::InvoiceInputs {
            template: "{sku}".into(),
            template_digest: template_digest("{sku}"),
            template_source: terms::InputSource::SkuVersion,
            gl_code: "usage".into(),
            tax_category: "standard".into(),
            timing: terms::BillingTiming::Arrears,
            rounding: terms::Rounding::HalfEven,
        },
    }
}
#[test]
fn public_binding_and_policy_projections_match_frozen_vectors() {
    use bss_pricing_sdk::digest::{policy_digest, selected_bindings_digest, template_digest};
    let fixture = fixture();
    let policy_vector = usize::try_from(fixture["policy_vector"].as_u64().unwrap()).unwrap();
    assert_eq!(
        hex(policy_digest(&policy_input())),
        fixture["vectors"][policy_vector]["sha256"]
    );
    for case in fixture["binding_vectors"].as_array().unwrap() {
        let mut binding = sample_binding();
        match case["change"].as_str().unwrap() {
            "original" => {}
            "unit" => binding.unit = Some("cloudlet_second".into()),
            "policy_version" => binding.usage_rating_policy.as_mut().unwrap().version = 1,
            "template" => {
                binding.invoice.template = "{sku} changed".into();
                binding.invoice.template_digest = template_digest(&binding.invoice.template);
            }
            other => panic!("unknown variant {other}"),
        }
        let selection = read::BindingSelection {
            item_id: binding.item_id,
            dimension_value: binding.dimension_value.clone(),
        };
        let resolved = read::ResolvedBindings {
            plan_id: uuid::Uuid::from_u128(6),
            revision_id: uuid::Uuid::from_u128(7),
            cells: vec![read::ResolvedCell {
                selection: selection.clone(),
                binding: Some(binding),
            }],
            quota_terms: None,
        };
        let expected = &fixture["vectors"]
            [usize::try_from(case["vector"].as_u64().unwrap()).unwrap()]["sha256"];
        assert_eq!(
            hex(selected_bindings_digest(&resolved, &[selection]).unwrap()),
            *expected
        );
    }
}
#[test]
fn every_model_operand_is_in_the_public_money_projection() {
    use bss_pricing_sdk::digest::money_digest;
    let tiers = vec![
        read::Tier {
            up_to: Some(Decimal::TEN),
            rate: Decimal::from_str_exact("0.04700").unwrap(),
        },
        read::Tier {
            up_to: None,
            rate: Decimal::from_str_exact("0.030").unwrap(),
        },
    ];
    let models = [
        PriceModel::Flat {
            amount: Decimal::from(12),
        },
        PriceModel::Package {
            package_size: Decimal::TEN,
            package_price: Decimal::from_str_exact("4.70").unwrap(),
        },
        PriceModel::Volume {
            tiers: tiers.clone(),
        },
        PriceModel::Graduated { tiers },
    ];
    let fixture = fixture();
    for (index, model) in models.into_iter().enumerate() {
        let mut price = price("0.047");
        price.model = model;
        price.minimum_fee = Some(Decimal::from(2));
        assert_eq!(
            hex(money_digest(&price)),
            fixture["vectors"][index + 10]["sha256"]
        );
    }
}

#[test]
fn binding_sets_sort_by_selection_and_reject_identity_mismatch() {
    use bss_pricing_sdk::digest::selected_bindings_digest;
    let binding = sample_binding();
    let mut other = binding.clone();
    other.item_id = uuid::Uuid::from_u128(8);
    let selection = read::BindingSelection {
        item_id: binding.item_id,
        dimension_value: binding.dimension_value.clone(),
    };
    let other_selection = read::BindingSelection {
        item_id: other.item_id,
        dimension_value: other.dimension_value.clone(),
    };
    let mut resolved = read::ResolvedBindings {
        plan_id: uuid::Uuid::from_u128(6),
        revision_id: uuid::Uuid::from_u128(7),
        cells: vec![
            read::ResolvedCell {
                selection: selection.clone(),
                binding: Some(binding),
            },
            read::ResolvedCell {
                selection: other_selection.clone(),
                binding: Some(other),
            },
        ],
        quota_terms: None,
    };
    let expected =
        selected_bindings_digest(&resolved, &[selection.clone(), other_selection.clone()]).unwrap();
    resolved.cells.reverse();
    assert_eq!(
        expected,
        selected_bindings_digest(&resolved, &[other_selection.clone(), selection]).unwrap()
    );
    resolved.cells[0]
        .binding
        .as_mut()
        .unwrap()
        .price_book_entry_id = uuid::Uuid::from_u128(9);
    assert!(selected_bindings_digest(&resolved, &[other_selection]).is_err());
}

fn commercial_query() -> acceptance::NewSaleQuery {
    use acceptance::{Market, NewSaleQuery, TenantAxes, Term};
    use bss_pricing_sdk::digest::{billing_terms_digest, selected_bindings_digest};
    use terms::{BillingAnchor, BillingCycle, BillingTerms, TermsSource, Timezone};
    let at = time::Date::from_calendar_date(2026, time::Month::October, 1)
        .unwrap()
        .midnight()
        .assume_utc();
    let mut billing_terms = BillingTerms {
        schema_version: 1,
        cycle: BillingCycle::Month,
        anchor: BillingAnchor::Calendar,
        anchor_at: at,
        timezone: Timezone::Utc,
        source: TermsSource::ExplicitOrder,
        digest: [0; 32],
    };
    billing_terms.digest = billing_terms_digest(&billing_terms);
    let b = sample_binding();
    let selection = read::BindingSelection {
        item_id: b.item_id,
        dimension_value: b.dimension_value.clone(),
    };
    let resolved = read::ResolvedBindings {
        plan_id: uuid::Uuid::from_u128(6),
        revision_id: uuid::Uuid::from_u128(7),
        cells: vec![read::ResolvedCell {
            selection: selection.clone(),
            binding: Some(b),
        }],
        quota_terms: None,
    };
    NewSaleQuery {
        tenant_axes: TenantAxes {
            seller_tenant_id: uuid::Uuid::from_u128(8),
            payer_tenant_id: uuid::Uuid::from_u128(9),
            resource_tenant_id: uuid::Uuid::from_u128(10),
        },
        order_id: uuid::Uuid::from_u128(11),
        order_version: u64::MAX,
        line_id: uuid::Uuid::from_u128(12),
        plan_id: resolved.plan_id,
        plan_revision_id: resolved.revision_id,
        resolved_bindings_digest: selected_bindings_digest(
            &resolved,
            std::slice::from_ref(&selection),
        )
        .unwrap(),
        selections: vec![selection],
        quantity: Decimal::ONE,
        market: Market {
            currency: "EUR".into(),
            region: Some("eu".into()),
        },
        start_at: at + time::Duration::minutes(630),
        term: Term::FixedPeriods { count: 12 },
        billing_terms,
        hold_policy_version: u64::MAX,
        expected_quota_digest: None,
    }
}
#[test]
fn commercial_projections_match_independent_canonical_vectors() {
    use bss_pricing_sdk::digest::{billing_terms_digest, request_digest, terms_digest};
    let q = commercial_query();
    let f = fixture();
    for (key, actual) in [
        ("billing_terms", billing_terms_digest(&q.billing_terms)),
        ("request", request_digest(&q)),
        ("terms", terms_digest(&q, &[sample_binding()], None)),
    ] {
        let i = usize::try_from(f["commercial_vectors"][key].as_u64().unwrap()).unwrap();
        assert_eq!(hex(actual), f["vectors"][i]["sha256"], "{key}");
    }
    let mut terms = q.billing_terms;
    terms.source = terms::TermsSource::SellerPolicy {
        id: uuid::Uuid::from_u128(20),
        version: u64::MAX,
    };
    let i = usize::try_from(f["commercial_vectors"]["seller_terms"].as_u64().unwrap()).unwrap();
    assert_eq!(hex(billing_terms_digest(&terms)), f["vectors"][i]["sha256"]);
}
#[test]
fn billing_terms_exclude_self_digest_preserve_source_and_normalize_instants() {
    use bss_pricing_sdk::digest::billing_terms_digest;
    let original = commercial_query().billing_terms;
    let baseline = billing_terms_digest(&original);
    let mut changed = original.clone();
    changed.digest = [9; 32];
    assert_eq!(baseline, billing_terms_digest(&changed));
    changed.anchor_at = changed
        .anchor_at
        .to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());
    assert_eq!(baseline, billing_terms_digest(&changed));
    changed.source = terms::TermsSource::SellerPolicy {
        id: uuid::Uuid::from_u128(20),
        version: 1,
    };
    assert_ne!(baseline, billing_terms_digest(&changed));
    changed = original.clone();
    changed.schema_version = 2;
    assert_ne!(baseline, billing_terms_digest(&changed));
    changed = original;
    changed.anchor = terms::BillingAnchor::SubscriptionStart;
    assert_ne!(baseline, billing_terms_digest(&changed));
}
#[test]
fn commercial_digests_cover_intent_and_sort_sets_without_reordering_tiers() {
    type Mutation = fn(&mut acceptance::NewSaleQuery);
    use bss_pricing_sdk::digest::{request_digest, terms_digest};
    let original = commercial_query();
    let baseline = request_digest(&original);
    let mut q = original.clone();
    q.quantity = "1.000".parse().unwrap();
    assert_eq!(baseline, request_digest(&q));
    let changes: &[Mutation] = &[
        |q| q.tenant_axes.payer_tenant_id = uuid::Uuid::from_u128(100),
        |q| q.tenant_axes.seller_tenant_id = uuid::Uuid::from_u128(100),
        |q| q.tenant_axes.resource_tenant_id = uuid::Uuid::from_u128(100),
        |q| q.order_id = uuid::Uuid::from_u128(100),
        |q| q.line_id = uuid::Uuid::from_u128(100),
        |q| q.plan_id = uuid::Uuid::from_u128(100),
        |q| q.plan_revision_id = uuid::Uuid::from_u128(100),
        |q| q.order_version = 1,
        |q| q.hold_policy_version = 1,
        |q| q.quantity = Decimal::TEN,
        |q| q.market.currency = "USD".into(),
        |q| q.market.region = None,
        |q| q.market.region = Some(String::new()),
        |q| q.start_at += time::Duration::seconds(1),
        |q| q.term = acceptance::Term::Rolling,
        |q| q.billing_terms.digest = [1; 32],
        |q| q.resolved_bindings_digest = [1; 32],
        |q| q.selections[0].dimension_value = None,
    ];
    for mutate in changes {
        let mut q = original.clone();
        mutate(&mut q);
        assert_ne!(baseline, request_digest(&q));
    }
    let a = sample_binding();
    let mut b = a.clone();
    b.item_id = uuid::Uuid::from_u128(100);
    q = original;
    q.selections.push(read::BindingSelection {
        item_id: b.item_id,
        dimension_value: b.dimension_value.clone(),
    });
    let request = request_digest(&q);
    let accepted = terms_digest(&q, &[a.clone(), b.clone()], None);
    q.selections.reverse();
    assert_eq!(request, request_digest(&q));
    assert_eq!(accepted, terms_digest(&q, &[b.clone(), a.clone()], None));
    b.price_book_entry_id = uuid::Uuid::from_u128(200);
    assert_ne!(accepted, terms_digest(&q, &[a.clone(), b.clone()], None));
    b = a;
    b.price.model = PriceModel::Volume {
        tiers: vec![
            read::Tier {
                up_to: Some(Decimal::TEN),
                rate: Decimal::ONE,
            },
            read::Tier {
                up_to: None,
                rate: Decimal::TEN,
            },
        ],
    };
    let before = terms_digest(&q, &[b.clone()], None);
    if let PriceModel::Volume { tiers } = &mut b.price.model {
        tiers.reverse();
    }
    assert_ne!(before, terms_digest(&q, &[b], None));
}

#[test]
fn rules_only_policy_digest_covers_the_five_rating_rules() {
    use bss_pricing_sdk::digest::{policy_digest, selected_bindings_digest};
    const OLD_POLICY: &str = "c9dc411f7758532dcb616eeeb4389fc9b6fb10169fd52e60c9df1a599a707c7b";
    const NEW_POLICY: &str = "2bf1fcebb5742b520e2162b6ab65bcad5d04dc8c1100055d77757eaa87a60abc";
    const NEW_BINDINGS: &str = "afa8076d53d6fbf900c49f20065c5bba1a8311bb154b82d4f516861c19cf9678";
    // A second policy that differed only in a former quantity_semantics field
    // (meter, unit, accrual) cannot be built: those fields are not on the type.
    let rules = terms::UsageRatingPolicyInput {
        rating_window: terms::RatingWindow::CalendarHour {
            timezone: terms::Timezone::Utc,
        },
        aggregation_scope: terms::AggregationScope::SubscriptionLine,
        reset: terms::Reset::RatingWindowStart,
        partial_window: terms::PartialWindow::ActualQuantityFullThresholds,
        fold: terms::Fold::Sum,
    };
    let canonical = "{\"domain\":\"pricing.policy.v1\",\"payload\":{\"aggregation_scope\":\"subscription_line\",\"fold\":\"SUM\",\"partial_window\":\"actual_quantity_full_thresholds\",\"rating_window\":{\"kind\":\"calendar_hour\",\"timezone\":\"UTC\"},\"reset\":\"rating_window_start\"}}";
    let parsed: Value = serde_json::from_str(canonical).unwrap();
    let payload = restricted(&parsed["payload"]).unwrap();
    assert_eq!(
        String::from_utf8(canonical_json_bytes(&CanonicalValue::Object(
            BTreeMap::from([
                (
                    "domain".into(),
                    CanonicalValue::String("pricing.policy.v1".into())
                ),
                ("payload".into(), payload),
            ])
        )))
        .unwrap(),
        canonical
    );
    assert!(!canonical.contains("quantity_semantics"));
    assert!(!canonical.contains("accrual_policy_version"));
    assert!(!canonical.contains("usage_type_id"));
    assert_eq!(hex(policy_digest(&rules)), NEW_POLICY);
    assert_ne!(NEW_POLICY, OLD_POLICY);

    let binding = sample_binding();
    let selection = read::BindingSelection {
        item_id: binding.item_id,
        dimension_value: binding.dimension_value.clone(),
    };
    let resolved = read::ResolvedBindings {
        plan_id: uuid::Uuid::from_u128(6),
        revision_id: uuid::Uuid::from_u128(7),
        cells: vec![read::ResolvedCell {
            selection: selection.clone(),
            binding: Some(binding.clone()),
        }],
        quota_terms: None,
    };
    let new_bindings =
        hex(selected_bindings_digest(&resolved, std::slice::from_ref(&selection)).unwrap());
    assert_eq!(new_bindings, NEW_BINDINGS);
    let mut other_meter = binding;
    other_meter.meter = None;
    let resolved_without = read::ResolvedBindings {
        plan_id: uuid::Uuid::from_u128(6),
        revision_id: uuid::Uuid::from_u128(7),
        cells: vec![read::ResolvedCell {
            selection: selection.clone(),
            binding: Some(other_meter),
        }],
        quota_terms: None,
    };
    assert_eq!(
        new_bindings,
        hex(selected_bindings_digest(&resolved_without, &[selection]).unwrap()),
        "the meter projection is outside the digest, so the change is the policy part only"
    );
}

#[test]
fn fulfilment_identity_covers_every_field_and_normalizes_instants() {
    use acceptance::{AcceptanceRef, FulfilmentQuery, Market, TenantAxes};
    use bss_pricing_sdk::digest::fulfilment_digest;
    let q = FulfilmentQuery {
        tenant_axes: TenantAxes {
            seller_tenant_id: uuid::Uuid::from_u128(1),
            payer_tenant_id: uuid::Uuid::from_u128(2),
            resource_tenant_id: uuid::Uuid::from_u128(3),
        },
        acceptance: AcceptanceRef {
            acceptance_id: uuid::Uuid::from_u128(4),
            terms_digest: [5; 32],
        },
        current_market: Market {
            currency: "EUR".into(),
            region: None,
        },
        activation_at: time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(),
    };
    let digest = fulfilment_digest(&q);
    let mut offset = q.clone();
    offset.activation_at = offset
        .activation_at
        .to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());
    assert_eq!(fulfilment_digest(&offset), digest);
    for field in 0..8 {
        let mut changed = q.clone();
        match field {
            0 => changed.tenant_axes.seller_tenant_id = uuid::Uuid::from_u128(100),
            1 => changed.tenant_axes.payer_tenant_id = uuid::Uuid::from_u128(100),
            2 => changed.tenant_axes.resource_tenant_id = uuid::Uuid::from_u128(100),
            3 => changed.acceptance.acceptance_id = uuid::Uuid::from_u128(100),
            4 => changed.acceptance.terms_digest[0] ^= 1,
            5 => changed.current_market.currency = "USD".into(),
            6 => changed.current_market.region = Some(String::new()),
            _ => changed.activation_at += time::Duration::nanoseconds(1),
        }
        assert_ne!(fulfilment_digest(&changed), digest);
    }
}

/// The rule `plan_quota_tests` builds first: one rule of every member, its scope of two keys.
fn quota_rule(n: u128, amount: &str) -> plan_quota::PlanQuotaRuleV1 {
    use plan_quota::{PlanQuotaBasis, PlanQuotaRuleV1, QuotaDefinitionRef, QuotaSubjectSelector};
    PlanQuotaRuleV1 {
        rule_id: uuid::Uuid::from_u128(n),
        definition_ref: QuotaDefinitionRef {
            id: uuid::Uuid::from_u128(0xd1),
            version: std::num::NonZeroU64::new(1).unwrap(),
            digest: [0xaa; 32],
        },
        item_id: uuid::Uuid::from_u128(0x11),
        dimension_value: None,
        logical_binding_key: "compute".into(),
        subject_selector: QuotaSubjectSelector::ResourceOwner,
        scope_selection: BTreeMap::from([
            ("region".into(), "region-1".into()),
            ("resource_pool".into(), "pool-1".into()),
        ]),
        amount: amount.into(),
        unit_ref: "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1".into(),
        basis: PlanQuotaBasis::PerPlanUnit,
    }
}
/// D-541: the frozen `pricing.quota-rules.v1` vector is the SDK function's digest, and its payload
/// is the SDK's projection of the rule.
#[test]
fn quota_rules_vector_matches_the_sdk_function() {
    let vector = quota_vector(&fixture(), "quota_rules_v1");
    assert_eq!(vector["domain"], "pricing.quota-rules.v1");
    let rule = quota_rule(1, "10");
    assert_eq!(
        restricted(&vector["payload"]).unwrap(),
        CanonicalValue::Object(BTreeMap::from([(
            "rules".to_owned(),
            CanonicalValue::Array(vec![digest::quota_rule(&rule)]),
        )]))
    );
    assert_eq!(
        hex(bss_pricing_sdk::digest::quota_rules_digest(&[rule])),
        vector["sha256"]
    );
}

/// The empty set's digest is a contract value: every revision without rules reads it (D-541,
/// D-544), so it is pinned here as a literal as well as in the fixture.
const EMPTY_QUOTA_RULES: &str = "599cdcc359ef3e8e6f8a32068b381eb4db89aa0d2fd0e702ed2935a793dd1d5a";
fn quota_vector(fixture: &Value, name: &str) -> Value {
    fixture["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("the fixture pins {name}"))
        .clone()
}
/// The empty set and a set of two rules given in reverse order: the fixture pins both, so the
/// order by `rule_id` is frozen in the data as well as in the code.
#[test]
fn quota_rules_vectors_pin_the_empty_set_and_the_rule_order() {
    let fixture = fixture();
    let empty = quota_vector(&fixture, "quota_rules_empty_v1");
    assert_eq!(empty["sha256"], EMPTY_QUOTA_RULES);
    assert_eq!(
        hex(bss_pricing_sdk::digest::quota_rules_digest(&[])),
        EMPTY_QUOTA_RULES
    );
    let two = quota_vector(&fixture, "quota_rules_two_v1");
    let (first, second) = (quota_rule(1, "10"), quota_rule(2, "20"));
    assert_eq!(
        restricted(&two["payload"]).unwrap(),
        CanonicalValue::Object(BTreeMap::from([(
            "rules".to_owned(),
            CanonicalValue::Array(vec![
                digest::quota_rule(&first),
                digest::quota_rule(&second)
            ]),
        )]))
    );
    assert_eq!(
        hex(bss_pricing_sdk::digest::quota_rules_digest(&[
            second, first
        ])),
        two["sha256"]
    );
}

/// The pinned evidence of the vCPU definition the quota vectors use (D-547): every member set.
fn quota_evidence() -> read::PinnedQuotaEvidence {
    use read::{PinnedQuotaEvidence, QuotaOwnerRef};
    let version = |n| std::num::NonZeroU64::new(n).unwrap();
    PinnedQuotaEvidence {
        sku_id: uuid::Uuid::from_u128(0x51),
        sku_version: version(3),
        metric_ref: "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.vcpu.v1".into(),
        metric_digest: [0xb1; 32],
        quota_kind: plan_quota::PlanQuotaKind::Allocation,
        canonical_unit_ref: "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1"
            .into(),
        unit_digest: [0xb2; 32],
        unit_numerator: "1".into(),
        unit_denominator: "1".into(),
        unit_min: "1".into(),
        unit_step: "1".into(),
        unit_max: Some("1024".into()),
        mapping: QuotaOwnerRef {
            id: uuid::Uuid::from_u128(0xa1),
            version: version(1),
            digest: [0xb3; 32],
        },
        profile: QuotaOwnerRef {
            id: uuid::Uuid::from_u128(0xa2),
            version: version(2),
            digest: [0xb4; 32],
        },
        accounting_policy_ref:
            "gts.cf.bss.products.quota_accounting_policy.v1~cf.compute.quota_adapter.allocated.v1"
                .into(),
        accounting_policy_digest: [0xb5; 32],
        scope_policy_ref:
            "gts.cf.bss.products.quota_scope_policy.v1~cf.compute.quota_adapter.regional_pool.v1"
                .into(),
        scope_policy_digest: [0xb6; 32],
        initial_set_supported: true,
        increase_supported: false,
    }
}
/// Rule `n` of [`quota_rule`] on `item` at `dimension_value`, with the vCPU evidence.
fn rule_on(n: u128, item: uuid::Uuid, dimension_value: Option<&str>) -> read::ResolvedQuotaRule {
    let mut rule = quota_rule(n, "10");
    rule.item_id = item;
    rule.dimension_value = dimension_value.map(Into::into);
    read::ResolvedQuotaRule {
        rule,
        evidence: quota_evidence(),
    }
}
/// A revision's terms: its rules and their set digest.
fn terms_of(rules: Vec<read::ResolvedQuotaRule>) -> read::QuotaTerms {
    let set: Vec<_> = rules.iter().map(|r| r.rule.clone()).collect();
    read::QuotaTerms {
        quota_rules_digest: bss_pricing_sdk::digest::quota_rules_digest(&set),
        rules,
    }
}
fn selection(item_id: uuid::Uuid, dimension_value: Option<&str>) -> read::BindingSelection {
    read::BindingSelection {
        item_id,
        dimension_value: dimension_value.map(Into::into),
    }
}
fn rule_ids(rules: &[&read::ResolvedQuotaRule]) -> Vec<uuid::Uuid> {
    rules.iter().map(|r| r.rule.rule_id).collect()
}
/// The sale of an existing vector: [`commercial_query`] and its one binding (item 3 at `eu`).
fn frozen_sale() -> (acceptance::NewSaleQuery, Vec<read::AcceptedBinding>) {
    (commercial_query(), vec![sample_binding()])
}
/// The terms the quota vectors use: the sale's item at `eu` (rule 1) and at `us` (rule 2).
fn sale_terms() -> read::QuotaTerms {
    let item = sample_binding().item_id;
    terms_of(vec![
        rule_on(2, item, Some("us")),
        rule_on(1, item, Some("eu")),
    ])
}
/// An accepted rule: `rule` with its final `limit` and availability revision `revision`.
fn accepted(
    rule: read::ResolvedQuotaRule,
    limit: &str,
    revision: u64,
) -> acceptance::AcceptedQuotaRule {
    acceptance::AcceptedQuotaRule {
        rule,
        limit: limit.into(),
        availability_revision: std::num::NonZeroU64::new(revision).unwrap(),
    }
}
/// The snapshot of the frozen sale: rule 1 at quantity 1, limit 10, availability revision 2.
fn sample_snapshot() -> acceptance::AcceptedQuotaSnapshot {
    let (query, _) = frozen_sale();
    let rules = vec![accepted(
        rule_on(1, sample_binding().item_id, Some("eu")),
        "10",
        2,
    )];
    snapshot_of(&query, rules)
}
/// A snapshot of `rules` for `query` with both digests computed the way acceptance computes them:
/// the selected digest over `rules` alone, the snapshot digest over it, the limits and the sale.
fn snapshot_of(
    query: &acceptance::NewSaleQuery,
    rules: Vec<acceptance::AcceptedQuotaRule>,
) -> acceptance::AcceptedQuotaSnapshot {
    use bss_pricing_sdk::digest::{quota_snapshot_digest, selected_quota_digest};
    let terms = terms_of(rules.iter().map(|r| r.rule.clone()).collect());
    let everything: Vec<_> = terms
        .rules
        .iter()
        .map(|r| selection(r.rule.item_id, r.rule.dimension_value.as_deref()))
        .collect();
    let selected = selected_quota_digest(&terms, &everything);
    acceptance::AcceptedQuotaSnapshot {
        selected_quota_digest: selected,
        quota_snapshot_digest: quota_snapshot_digest(&selected, &rules, query),
        original_quantity: query.quantity,
        rules,
    }
}

/// D-547: an item's rule for a value applies to that value and only to it, whatever the price's
/// default chain does; for an item without a default rule, a value no rule names selects nothing.
#[test]
fn selected_quota_digest_follows_the_requested_value_not_the_default_chain() {
    use bss_pricing_sdk::digest::{select_quota_rules, selected_quota_digest};
    let item = uuid::Uuid::from_u128(0xa);
    let terms = terms_of(vec![
        rule_on(1, item, Some("eu")),
        rule_on(2, item, Some("us")),
    ]);
    let eu = selected_quota_digest(&terms, &[selection(item, Some("eu"))]);
    let us = selected_quota_digest(&terms, &[selection(item, Some("us"))]);
    let apac = select_quota_rules(&terms, &[selection(item, Some("apac"))]);
    assert_ne!(eu, us);
    assert!(
        apac.is_empty(),
        "no rule applies to a value it does not name"
    );
    assert_eq!(
        rule_ids(&select_quota_rules(&terms, &[selection(item, Some("eu"))])),
        [uuid::Uuid::from_u128(1)]
    );
}

/// The RAM metric of the quota vectors' adapter.
const RAM_METRIC: &str = "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.ram.v1";
/// [`rule_on`] for the RAM quota: the same binding, the RAM metric.
fn ram_rule_on(
    n: u128,
    item: uuid::Uuid,
    dimension_value: Option<&str>,
) -> read::ResolvedQuotaRule {
    let mut rule = rule_on(n, item, dimension_value);
    rule.evidence.metric_ref = RAM_METRIC.into();
    rule
}
fn ids(numbers: &[u128]) -> Vec<uuid::Uuid> {
    numbers.iter().map(|n| uuid::Uuid::from_u128(*n)).collect()
}

/// D-547 (the owner's ruling of 2026-11-22): a requested value takes the item's rule for that
/// value, else the item's default rule (no dimension value); a selection with no dimension value
/// takes the default rule; an item without rules selects nothing.
#[test]
fn a_value_takes_its_own_rule_else_the_items_default_rule() {
    use bss_pricing_sdk::digest::select_quota_rules;
    let item = uuid::Uuid::from_u128(0xa);
    let terms = terms_of(vec![rule_on(1, item, None), rule_on(2, item, Some("eu"))]);
    let selected =
        |value: Option<&str>| rule_ids(&select_quota_rules(&terms, &[selection(item, value)]));
    assert_eq!(selected(Some("eu")), ids(&[2]), "eu takes its own rule");
    assert_eq!(selected(Some("apac")), ids(&[1]), "apac takes the default");
    assert_eq!(selected(None), ids(&[1]), "the null cell takes the default");
    assert!(
        select_quota_rules(&terms, &[selection(uuid::Uuid::from_u128(0xb), None)]).is_empty(),
        "an item without rules"
    );
}

/// [`rule_on`] on the logical binding `binding`.
fn rule_at(
    n: u128,
    item: uuid::Uuid,
    dimension_value: Option<&str>,
    binding: &str,
) -> read::ResolvedQuotaRule {
    let mut rule = rule_on(n, item, dimension_value);
    rule.rule.logical_binding_key = binding.into();
    rule
}

/// D-547: the default rule is taken per metric, whatever its binding. An item with an `eu` vCPU
/// rule on its own binding and a default for vCPU and for RAM sells `eu` with its own vCPU and the
/// default RAM, and the vCPU default stays out; a metric without a default selects nothing for a
/// value it has no rule for.
#[test]
fn the_default_rule_is_taken_per_metric() {
    use bss_pricing_sdk::digest::select_quota_rules;
    let item = uuid::Uuid::from_u128(0xa);
    let terms = terms_of(vec![
        rule_at(1, item, Some("eu"), "compute-eu"),
        rule_on(2, item, None),
        ram_rule_on(3, item, None),
    ]);
    let selected = |terms: &read::QuotaTerms, value: Option<&str>| {
        rule_ids(&select_quota_rules(terms, &[selection(item, value)]))
    };
    assert_eq!(
        selected(&terms, Some("eu")),
        ids(&[1, 3]),
        "eu: its own vCPU and the default RAM"
    );
    assert_eq!(selected(&terms, Some("apac")), ids(&[2, 3]));
    let no_vcpu_default = terms_of(vec![
        rule_on(1, item, Some("eu")),
        ram_rule_on(3, item, None),
    ]);
    assert_eq!(
        selected(&no_vcpu_default, Some("apac")),
        ids(&[3]),
        "no vCPU default: apac selects no vCPU rule"
    );
}

/// D-547: a quota is its metric. One binding per region (`compute-eu`, `compute-us`) for one
/// metric sells `eu` and `us` each with its own rule and nothing else, and leaves no metric
/// uncovered.
#[test]
fn per_region_bindings_sell_each_value_with_its_own_rule() {
    use bss_pricing_sdk::digest::{select_quota_rules, uncovered_quotas};
    let item = uuid::Uuid::from_u128(0xa);
    let terms = terms_of(vec![
        rule_at(1, item, Some("eu"), "compute-eu"),
        rule_at(2, item, Some("us"), "compute-us"),
    ]);
    for (value, rule) in [("eu", 1), ("us", 2)] {
        let sale = [selection(item, Some(value))];
        assert_eq!(rule_ids(&select_quota_rules(&terms, &sale)), ids(&[rule]));
        assert!(
            uncovered_quotas(&terms, &sale).is_empty(),
            "{value}: its own rule covers the metric"
        );
    }
}

/// D-547: an item's default rule on a binding of its own is not selected beside a rule for the
/// requested value of the same metric; it is selected for a value without one.
#[test]
fn a_default_on_its_own_binding_is_not_selected_beside_a_rule_of_its_metric() {
    use bss_pricing_sdk::digest::select_quota_rules;
    let item = uuid::Uuid::from_u128(0xa);
    let terms = terms_of(vec![
        rule_at(1, item, Some("eu"), "compute-eu"),
        rule_at(2, item, None, "compute"),
    ]);
    let selected =
        |value: Option<&str>| rule_ids(&select_quota_rules(&terms, &[selection(item, value)]));
    assert_eq!(selected(Some("eu")), ids(&[1]), "one vCPU rule for eu");
    assert_eq!(selected(Some("apac")), ids(&[2]), "apac takes the default");
}

/// D-547: every rule of the item for the requested value and a metric is taken, so two bindings
/// of one metric for one value (two adapter targets) are both selected; a value without one takes
/// the default and leaves the metric covered.
#[test]
fn two_bindings_of_one_metric_for_one_value_are_both_selected() {
    use bss_pricing_sdk::digest::{select_quota_rules, uncovered_quotas};
    let item = uuid::Uuid::from_u128(0xa);
    let terms = terms_of(vec![
        rule_at(1, item, Some("eu"), "target-a"),
        rule_at(2, item, Some("eu"), "target-b"),
        rule_at(3, item, None, "target-a"),
    ]);
    let eu = [selection(item, Some("eu"))];
    let apac = [selection(item, Some("apac"))];
    assert_eq!(rule_ids(&select_quota_rules(&terms, &eu)), ids(&[1, 2]));
    assert_eq!(rule_ids(&select_quota_rules(&terms, &apac)), ids(&[3]));
    assert!(uncovered_quotas(&terms, &eu).is_empty());
    assert!(
        uncovered_quotas(&terms, &apac).is_empty(),
        "the default covers the metric for apac"
    );
}

/// D-547: the metrics of a selected item that have neither a rule for the requested value nor the
/// item's default rule, each once, in order; none when a default covers the value, none for an
/// item without rules. The metric is named, not a binding.
#[test]
fn uncovered_quotas_names_each_metric_with_neither_a_rule_nor_a_default() {
    use bss_pricing_sdk::digest::uncovered_quotas;
    use read::UncoveredQuota;
    let (item, bare) = (uuid::Uuid::from_u128(0xa), uuid::Uuid::from_u128(0xb));
    let vcpu = quota_evidence().metric_ref;
    let missing = |value: Option<&str>, metric: &str| UncoveredQuota {
        item_id: item,
        dimension_value: value.map(Into::into),
        metric_ref: metric.into(),
    };
    let covered = terms_of(vec![rule_on(1, item, Some("eu")), rule_on(2, item, None)]);
    for value in [Some("eu"), Some("apac"), None] {
        assert!(
            uncovered_quotas(&covered, &[selection(item, value)]).is_empty(),
            "{value:?}: a default covers it"
        );
    }
    let terms = terms_of(vec![
        rule_at(1, item, Some("eu"), "compute-eu"),
        ram_rule_on(2, item, Some("eu")),
        rule_at(3, item, None, "compute"),
    ]);
    assert!(uncovered_quotas(&terms, &[selection(item, Some("eu"))]).is_empty());
    assert_eq!(
        uncovered_quotas(&terms, &[selection(item, Some("apac"))]),
        [missing(Some("apac"), RAM_METRIC)],
        "apac with no RAM default names RAM; the vCPU default covers vCPU"
    );
    let value_rules_only = terms_of(vec![
        rule_on(1, item, Some("eu")),
        ram_rule_on(2, item, Some("eu")),
    ]);
    let mut both = vec![missing(None, &vcpu), missing(None, RAM_METRIC)];
    both.sort();
    assert_eq!(
        uncovered_quotas(
            &value_rules_only,
            &[selection(item, None), selection(item, None)]
        ),
        both,
        "the null cell with value rules only, a selection given twice named once"
    );
    assert!(
        uncovered_quotas(&terms, &[selection(bare, Some("apac"))]).is_empty(),
        "an item without rules"
    );
}

/// D-547: the selection is ordered by `rule_id` whatever the order of the rules and the
/// selections; the digest covers each selected rule's evidence and nothing of a rule left out.
#[test]
fn the_selected_digest_orders_by_rule_id_and_covers_only_the_selected_rules() {
    use bss_pricing_sdk::digest::{select_quota_rules, selected_quota_digest};
    let (a, b) = (uuid::Uuid::from_u128(0xa), uuid::Uuid::from_u128(0xb));
    let rules = vec![
        rule_on(3, b, None),
        rule_on(1, a, Some("eu")),
        rule_on(2, a, Some("us")),
    ];
    let both = [selection(a, Some("eu")), selection(b, None)];
    let terms = terms_of(rules.clone());
    assert_eq!(
        rule_ids(&select_quota_rules(&terms, &both)),
        [uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(3)]
    );
    let digest = selected_quota_digest(&terms, &both);
    let mut reversed = rules;
    reversed.reverse();
    let reversed_terms = terms_of(reversed);
    let mut reversed_selections = both.clone();
    reversed_selections.reverse();
    assert_eq!(
        selected_quota_digest(&reversed_terms, &reversed_selections),
        digest
    );
    let mut unselected = terms.clone();
    unselected.rules[2].rule.amount = "99".into();
    unselected.rules[2].evidence.metric_digest = [0; 32];
    assert_eq!(
        selected_quota_digest(&unselected, &both),
        digest,
        "rule 2 (us) is not selected"
    );
    let mut evidence = terms.clone();
    evidence.rules[1].evidence.metric_digest = [0; 32];
    assert_ne!(selected_quota_digest(&evidence, &both), digest);
    let mut amount = terms;
    amount.rules[1].rule.amount = "20".into();
    assert_ne!(selected_quota_digest(&amount, &both), digest);
}

/// D-547: the selected digest covers the rule unit's grid and maximum and the quota kind, so a
/// consumer that recomputes a limit at another quantity checks it against the frozen grid. Each
/// change of `unit_min`, `unit_step` or `unit_max` (a value, and a maximum against none) moves
/// the selected digest and the snapshot digest that binds it.
#[test]
fn the_selected_digest_covers_the_unit_grid_and_the_kind() {
    type Mutation = fn(&mut read::PinnedQuotaEvidence);
    use bss_pricing_sdk::digest::{quota_snapshot_digest, selected_quota_digest};
    let (query, _) = frozen_sale();
    let snapshot_of_terms = |terms: &read::QuotaTerms| {
        let selected = selected_quota_digest(terms, &query.selections);
        let rules: Vec<_> = terms
            .rules
            .iter()
            .filter(|r| r.rule.dimension_value.as_deref() == Some("eu"))
            .map(|r| accepted(r.clone(), "10", 2))
            .collect();
        (selected, quota_snapshot_digest(&selected, &rules, &query))
    };
    let terms = sale_terms();
    let (selected, snapshot) = snapshot_of_terms(&terms);
    assert_eq!(
        terms.rules[1].evidence.quota_kind,
        plan_quota::PlanQuotaKind::Allocation
    );
    let changes: &[(&str, Mutation)] = &[
        ("min", |e| e.unit_min = "2".into()),
        ("step", |e| e.unit_step = "2".into()),
        ("step zero", |e| e.unit_step = "0".into()),
        ("max", |e| e.unit_max = Some("2048".into())),
        ("no max", |e| e.unit_max = None),
    ];
    for (name, mutate) in changes {
        let mut other = terms.clone();
        let eu = other
            .rules
            .iter_mut()
            .find(|r| r.rule.dimension_value.as_deref() == Some("eu"))
            .unwrap();
        mutate(&mut eu.evidence);
        let (other_selected, other_snapshot) = snapshot_of_terms(&other);
        assert_ne!(other_selected, selected, "{name}: the selected digest");
        assert_ne!(other_snapshot, snapshot, "{name}: the snapshot digest");
    }
    let payload = digest::selected_quota_rule(&terms.rules[1]);
    let CanonicalValue::Object(members) = &payload else {
        panic!("{payload:?}");
    };
    let CanonicalValue::Object(evidence) = &members["evidence"] else {
        panic!("{payload:?}");
    };
    for (member, value) in [
        ("quota_kind", "allocation"),
        ("unit_min", "1"),
        ("unit_step", "1"),
        ("unit_max", "1024"),
    ] {
        assert_eq!(
            evidence.get(member),
            Some(&CanonicalValue::String(value.into())),
            "{member}"
        );
    }
    let mut unbounded = terms.rules[1].clone();
    unbounded.evidence.unit_max = None;
    let payload = digest::selected_quota_rule(&unbounded);
    let CanonicalValue::Object(members) = &payload else {
        panic!("{payload:?}");
    };
    let CanonicalValue::Object(evidence) = &members["evidence"] else {
        panic!("{payload:?}");
    };
    assert_eq!(
        evidence.get("unit_max"),
        Some(&CanonicalValue::Null),
        "no maximum is null"
    );
}

/// D-548: a sale without a quota part hashes the same bytes as before: the frozen request and
/// terms vectors of a quota-free sale.
#[test]
fn quota_free_terms_and_request_digests_are_unchanged() {
    use bss_pricing_sdk::digest::{request_digest, terms_digest};
    let (query, bindings) = frozen_sale();
    assert_eq!(query.expected_quota_digest, None);
    let f = fixture();
    let vector = |key: &str| {
        let i = usize::try_from(f["commercial_vectors"][key].as_u64().unwrap()).unwrap();
        f["vectors"][i]["sha256"].as_str().unwrap().to_owned()
    };
    assert_eq!(hex(terms_digest(&query, &bindings, None)), vector("terms"));
    assert_eq!(hex(request_digest(&query)), vector("request"));
}

/// D-548: the snapshot digest binds the selected digest, the original quantity (exact, so `1.000`
/// is `1`), each tenant axis, the plan revision and the order line.
#[test]
fn the_snapshot_digest_binds_quantity_and_tenant_axes() {
    type Mutation = fn(&mut acceptance::NewSaleQuery);
    use bss_pricing_sdk::digest::quota_snapshot_digest;
    let (query, _) = frozen_sale();
    let rules = sample_snapshot().rules;
    let selected = [7u8; 32];
    let first = quota_snapshot_digest(&selected, &rules, &query);
    let mut same = query.clone();
    same.quantity = "1.000".parse().unwrap();
    assert_eq!(quota_snapshot_digest(&selected, &rules, &same), first);
    assert_ne!(quota_snapshot_digest(&[8u8; 32], &rules, &query), first);
    let changes: &[(&str, Mutation)] = &[
        ("quantity", |q| q.quantity = Decimal::from(2)),
        ("seller", |q| {
            q.tenant_axes.seller_tenant_id = uuid::Uuid::from_u128(100);
        }),
        ("payer", |q| {
            q.tenant_axes.payer_tenant_id = uuid::Uuid::from_u128(100);
        }),
        ("resource", |q| {
            q.tenant_axes.resource_tenant_id = uuid::Uuid::from_u128(100);
        }),
        ("plan revision", |q| {
            q.plan_revision_id = uuid::Uuid::from_u128(100);
        }),
        ("order id", |q| q.order_id = uuid::Uuid::from_u128(100)),
        ("order version", |q| q.order_version = 1),
        ("line id", |q| q.line_id = uuid::Uuid::from_u128(100)),
    ];
    for (name, mutate) in changes {
        let mut other = query.clone();
        mutate(&mut other);
        assert_ne!(
            quota_snapshot_digest(&selected, &rules, &other),
            first,
            "{name}"
        );
    }
}

/// D-548: the snapshot digest binds each accepted rule's final limit and availability revision,
/// ordered by `rule_id` whatever the input order; an added rule moves it too.
#[test]
fn the_snapshot_digest_binds_each_limit_and_availability_revision() {
    use bss_pricing_sdk::digest::quota_snapshot_digest;
    let (query, _) = frozen_sale();
    let item = sample_binding().item_id;
    let rules = vec![
        accepted(rule_on(1, item, Some("eu")), "10", 2),
        accepted(rule_on(2, item, Some("eu")), "20", 5),
    ];
    let selected = [7u8; 32];
    let first = quota_snapshot_digest(&selected, &rules, &query);
    let mut reversed = rules.clone();
    reversed.reverse();
    assert_eq!(quota_snapshot_digest(&selected, &reversed, &query), first);
    let mut limit = rules.clone();
    limit[1].limit = "21".into();
    assert_ne!(
        quota_snapshot_digest(&selected, &limit, &query),
        first,
        "limit"
    );
    let mut revision = rules.clone();
    revision[0].availability_revision = std::num::NonZeroU64::new(3).unwrap();
    assert_ne!(
        quota_snapshot_digest(&selected, &revision, &query),
        first,
        "availability revision"
    );
    assert_ne!(
        quota_snapshot_digest(&selected, &rules[..1], &query),
        first,
        "a rule fewer"
    );
}

/// D-548: the stored-snapshot check accepts a snapshot as acceptance issues it, and refuses one
/// that breaks a rule even when both digests are recomputed over the change, naming the first
/// check that fails.
#[test]
fn verify_quota_snapshot_accepts_the_issued_snapshot_and_refuses_each_broken_rule() {
    use bss_pricing_sdk::digest::{QuotaSnapshotMismatch as M, verify_quota_snapshot};
    let (query, _) = frozen_sale();
    let item = sample_binding().item_id;
    let issued = sample_snapshot();
    let mut sale = query.clone();
    sale.expected_quota_digest = Some(issued.selected_quota_digest);
    let mut other_expected = sale.clone();
    other_expected.expected_quota_digest = Some([9; 32]);
    let mut other_selected = issued.clone();
    other_selected.selected_quota_digest = [9; 32];
    let mut quantity = issued.clone();
    quantity.original_quantity = Decimal::from(2);
    let mut sealed = issued.clone();
    sealed.quota_snapshot_digest = [9; 32];
    let stored = [
        ("as issued", &sale, Some(&issued), Ok(())),
        ("a quota-free sale", &query, None, Ok(())),
        (
            "a snapshot without an expected digest",
            &query,
            Some(&issued),
            Err(M::Pairing),
        ),
        (
            "an expected digest without a snapshot",
            &sale,
            None,
            Err(M::Pairing),
        ),
        (
            "another expected digest",
            &other_expected,
            Some(&issued),
            Err(M::SelectedDigest),
        ),
        (
            "another stored selected digest",
            &sale,
            Some(&other_selected),
            Err(M::SelectedDigest),
        ),
        (
            "another original quantity",
            &sale,
            Some(&quantity),
            Err(M::Quantity),
        ),
        (
            "another stored snapshot digest",
            &sale,
            Some(&sealed),
            Err(M::SnapshotDigest),
        ),
    ];
    for (case, sale, snapshot, expected) in stored {
        assert_eq!(verify_quota_snapshot(sale, snapshot), expected, "{case}");
    }
    // Each case below recomputes both digests over its change, so only the rule it breaks
    // refuses it; the expected digest follows the recomputed selection.
    let eu = |n: u128, limit: &str| accepted(rule_on(n, item, Some("eu")), limit, 2);
    // The 128-bit bound: 39 digits one above `u128::MAX` are refused, `u128::MAX` is a limit.
    let above = "340282366920938463463374607431768211456";
    assert_eq!(above.len(), 39);
    let max = u128::MAX.to_string();
    let two = vec![eu(1, "10"), eu(2, "20")];
    let mut reversed = two.clone();
    reversed.reverse();
    // The same selection as `select_quota_rules` (D-547): the item's default rule is not selected
    // beside the value's own rule of the same quota, whatever its binding.
    let default = accepted(rule_on(1, item, None), "10", 2);
    let mut resealed: Vec<_> = ["010", "1.0", "-1", "+1", "", "1e3", &"1".repeat(40)]
        .into_iter()
        .map(|limit| {
            (
                format!("limit {limit:?}"),
                vec![eu(1, limit)],
                Err(M::Limit),
            )
        })
        .collect();
    resealed.extend([
        (
            "an explicit zero limit (D-541)".into(),
            vec![eu(1, "0")],
            Ok(()),
        ),
        (
            "a limit above u128::MAX".into(),
            vec![eu(1, above)],
            Err(M::Limit),
        ),
        ("u128::MAX is a limit".into(), vec![eu(1, &max)], Ok(())),
        (
            "a rule the sale's selections do not select".into(),
            vec![accepted(rule_on(1, item, Some("us")), "10", 2)],
            Err(M::Selection),
        ),
        ("two rules in order".into(), two.clone(), Ok(())),
        (
            "rules out of rule_id order".into(),
            reversed,
            Err(M::Selection),
        ),
        (
            "a rule stored twice".into(),
            vec![two[0].clone(), two[0].clone()],
            Err(M::Selection),
        ),
        (
            "the default beside eu's own rule of the same metric".into(),
            vec![default.clone(), eu(2, "20")],
            Err(M::Selection),
        ),
        (
            "a default on another binding beside eu's own rule of the same metric".into(),
            vec![
                accepted(rule_at(1, item, None, "compute"), "10", 2),
                accepted(rule_at(2, item, Some("eu"), "compute-eu"), "20", 2),
            ],
            Err(M::Selection),
        ),
        (
            "out of order with a limit that is not canonical: the selection is checked first"
                .into(),
            vec![eu(2, "20"), eu(1, "010")],
            Err(M::Selection),
        ),
    ]);
    for (case, rules, expected) in resealed {
        let snapshot = snapshot_of(&query, rules);
        let mut sale = query.clone();
        sale.expected_quota_digest = Some(snapshot.selected_quota_digest);
        assert_eq!(
            verify_quota_snapshot(&sale, Some(&snapshot)),
            expected,
            "{case}"
        );
    }
    // The item's default rule is selected for a value without its own rule.
    let mut apac_sale = query.clone();
    apac_sale.selections = vec![selection(item, Some("apac"))];
    let snapshot = snapshot_of(&apac_sale, vec![default]);
    apac_sale.expected_quota_digest = Some(snapshot.selected_quota_digest);
    assert_eq!(
        verify_quota_snapshot(&apac_sale, Some(&snapshot)),
        Ok(()),
        "apac with the default rule"
    );
}

/// D-548: an expected quota digest is part of the request, and a frozen snapshot is part of the
/// accepted terms.
#[test]
fn an_expected_digest_moves_the_request_digest_and_a_snapshot_moves_the_terms_digest() {
    use bss_pricing_sdk::digest::{request_digest, terms_digest};
    let (mut query, bindings) = frozen_sale();
    let before = request_digest(&query);
    query.expected_quota_digest = Some([1; 32]);
    assert_ne!(request_digest(&query), before);
    let snapshot = sample_snapshot();
    assert_ne!(
        terms_digest(&query, &bindings, Some(&snapshot)),
        terms_digest(&query, &bindings, None)
    );
    let mut other = snapshot.clone();
    other.quota_snapshot_digest = [2; 32];
    assert_ne!(
        terms_digest(&query, &bindings, Some(&other)),
        terms_digest(&query, &bindings, Some(&snapshot))
    );
}

/// The frozen `pricing.selected-quota.v1` vector is the SDK function's digest of the sale's
/// selection (rule 1 at `eu`, rule 2 at `us` left out), and its payload is the SDK's projection.
#[test]
fn selected_quota_vector_matches_the_sdk_function() {
    use bss_pricing_sdk::digest::{select_quota_rules, selected_quota_digest};
    let vector = quota_vector(&fixture(), "selected_quota_v1");
    assert_eq!(vector["domain"], "pricing.selected-quota.v1");
    let (query, _) = frozen_sale();
    let terms = sale_terms();
    let selected = select_quota_rules(&terms, &query.selections);
    assert_eq!(rule_ids(&selected), [uuid::Uuid::from_u128(1)]);
    assert_eq!(
        restricted(&vector["payload"]).unwrap(),
        CanonicalValue::Object(BTreeMap::from([(
            "rules".to_owned(),
            CanonicalValue::Array(vec![digest::selected_quota_rule(selected[0])]),
        )]))
    );
    assert_eq!(
        hex(selected_quota_digest(&terms, &query.selections)),
        vector["sha256"]
    );
}

/// The frozen `pricing.quota-snapshot.v1` vector binds the selected vector's digest to the frozen
/// sale; its payload is the SDK's projection.
#[test]
fn quota_snapshot_vector_matches_the_sdk_function() {
    use bss_pricing_sdk::digest::quota_snapshot_digest;
    let fixture = fixture();
    let vector = quota_vector(&fixture, "quota_snapshot_v1");
    assert_eq!(vector["domain"], "pricing.quota-snapshot.v1");
    let selected_hex = quota_vector(&fixture, "selected_quota_v1")["sha256"].clone();
    assert_eq!(vector["payload"]["selected_quota_digest"], selected_hex);
    let snapshot = sample_snapshot();
    assert_eq!(hex(snapshot.selected_quota_digest), selected_hex);
    assert_eq!(
        vector["payload"]["limits"],
        json!([{
            "rule_id": "00000000-0000-0000-0000-000000000001",
            "limit": "10",
            "availability_revision": "2",
        }])
    );
    let (query, _) = frozen_sale();
    assert_eq!(
        restricted(&vector["payload"]).unwrap(),
        digest::quota_snapshot(&snapshot.selected_quota_digest, &snapshot.rules, &query)
    );
    assert_eq!(
        hex(quota_snapshot_digest(
            &snapshot.selected_quota_digest,
            &snapshot.rules,
            &query
        )),
        vector["sha256"]
    );
    assert_eq!(hex(snapshot.quota_snapshot_digest), vector["sha256"]);
}

/// The frozen terms vector of a sale with a quota part is the quota-free terms vector plus two
/// members: the query's `expected_quota_digest` and `quota_snapshot_digest`. Nothing else moves.
#[test]
fn quota_terms_vector_adds_two_members_to_the_quota_free_terms() {
    use bss_pricing_sdk::digest::{request_digest, terms_digest};
    let fixture = fixture();
    let vector = quota_vector(&fixture, "accepted_terms_quota_v1");
    assert_eq!(vector["domain"], "pricing.terms.v1");
    let i = usize::try_from(fixture["commercial_vectors"]["terms"].as_u64().unwrap()).unwrap();
    let mut expected = fixture["vectors"][i]["payload"].clone();
    let selected = quota_vector(&fixture, "selected_quota_v1")["sha256"].clone();
    let snapshot_hex = quota_vector(&fixture, "quota_snapshot_v1")["sha256"].clone();
    expected["query"]["expected_quota_digest"] = selected;
    expected["quota_snapshot_digest"] = snapshot_hex;
    assert_eq!(vector["payload"], expected);
    let (mut query, bindings) = frozen_sale();
    let snapshot = sample_snapshot();
    query.expected_quota_digest = Some(snapshot.selected_quota_digest);
    assert_eq!(
        hex(terms_digest(&query, &bindings, Some(&snapshot))),
        vector["sha256"]
    );
    let mut request = vector["payload"]["query"].clone();
    request
        .as_object_mut()
        .unwrap()
        .remove("expected_quota_digest");
    let ir = usize::try_from(fixture["commercial_vectors"]["request"].as_u64().unwrap()).unwrap();
    assert_eq!(request, fixture["vectors"][ir]["payload"]);
    assert_ne!(
        hex(request_digest(&query)),
        fixture["vectors"][ir]["sha256"],
        "the expected digest is part of the request"
    );
}
