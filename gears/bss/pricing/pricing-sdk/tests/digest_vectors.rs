#![allow(clippy::expect_used, clippy::unwrap_used)]
// Compile the private encoder as a test module; no untyped production SDK door is exported.
#[allow(dead_code)]
#[path = "../src/digest.rs"]
pub mod digest;
use bss_pricing_sdk::{Digest, acceptance, read, terms};
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
        currency: "EUR".into(),
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
                json!({"currency":"EUR","model":{"kind":"per_unit","unit_amount":Decimal::from_str_exact(input).unwrap().normalize().to_string()},"minimum_fee":null})
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
    changed.currency = "USD".into();
    assert_ne!(baseline, bss_pricing_sdk::digest::money_digest(&changed));
    changed = original;
    changed.minimum_fee = Some(Decimal::ONE);
    assert_ne!(baseline, bss_pricing_sdk::digest::money_digest(&changed));
    changed = price("0.048");
    assert_ne!(baseline, bss_pricing_sdk::digest::money_digest(&changed));
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
            currency_scale: 2,
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
        ("terms", terms_digest(&q, &[sample_binding()])),
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
    let accepted = terms_digest(&q, &[a.clone(), b.clone()]);
    q.selections.reverse();
    assert_eq!(request, request_digest(&q));
    assert_eq!(accepted, terms_digest(&q, &[b.clone(), a.clone()]));
    b.price_book_entry_id = uuid::Uuid::from_u128(200);
    assert_ne!(accepted, terms_digest(&q, &[a.clone(), b.clone()]));
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
    let before = terms_digest(&q, &[b.clone()]);
    if let PriceModel::Volume { tiers } = &mut b.price.model {
        tiers.reverse();
    }
    assert_ne!(before, terms_digest(&q, &[b]));
}

#[test]
fn rules_only_policy_digest_covers_the_five_rating_rules() {
    use bss_pricing_sdk::digest::{policy_digest, selected_bindings_digest};
    const OLD_POLICY: &str = "c9dc411f7758532dcb616eeeb4389fc9b6fb10169fd52e60c9df1a599a707c7b";
    const NEW_POLICY: &str = "2bf1fcebb5742b520e2162b6ab65bcad5d04dc8c1100055d77757eaa87a60abc";
    const OLD_BINDINGS: &str = "b4be41c8abdd4ba9a808b3a1ab54ce85bec603982d81e19347ccc3cb6aba3e86";
    const NEW_BINDINGS: &str = "268fe0a1b05d7eaa4092bd5a4415d2669d028c865db702d73b761595e5d74457";
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
    };
    let new_bindings =
        hex(selected_bindings_digest(&resolved, std::slice::from_ref(&selection)).unwrap());
    assert_eq!(new_bindings, NEW_BINDINGS);
    assert_ne!(
        new_bindings, OLD_BINDINGS,
        "selected_bindings_digest old {OLD_BINDINGS} -> new {new_bindings} (policy part only)"
    );
    let mut other_meter = binding;
    other_meter.meter = None;
    let resolved_without = read::ResolvedBindings {
        plan_id: uuid::Uuid::from_u128(6),
        revision_id: uuid::Uuid::from_u128(7),
        cells: vec![read::ResolvedCell {
            selection: selection.clone(),
            binding: Some(other_meter),
        }],
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
