//! Reconciled corpus only: eight evaluations and eleven usage-chain guards.
//!
//! Every price and every expected charge is a major-unit decimal string (`"12.34"`, `"0.047"`)
//! in canonical spelling. An evaluation is compared as an exact value; it is never narrowed to a
//! `Decimal`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_money::exact::ExactAmount;
use bss_pricing::domain::{
    money::{PriceData, Tier, amount_for},
    price::{Eligibility, Price, PriceState, SkuMetering, chain_guard},
    price_book_entry::{ChargeKind, Model},
};
use bss_pricing_sdk::numeric;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use time::{Date, Month};
use uuid::Uuid;

#[derive(Deserialize)]
struct Case {
    family: String,
    id: String,
    kind: String,
    snapshot: Option<Snapshot>,
    predecessor: Option<Snapshot>,
    successor: Option<Snapshot>,
    #[serde(rename = "assert")]
    assertions: Vec<Assertion>,
    // Provenance is retained as data, not executed as an old-model authority.
    #[serde(flatten)]
    _ignored: BTreeMap<String, toml::Value>,
}
#[derive(Deserialize)]
struct Snapshot {
    model_kind: Model,
    charge_kind: ChargeKind,
    currency: String,
    amount: Option<String>,
    rate: Option<String>,
    package_size: Option<i64>,
    package_price: Option<String>,
    #[serde(default)]
    bands: Vec<Band>,
    included_allowance: Option<Included>,
    meter: Option<String>,
    billing_granularity: Option<String>,
    // tier_aggregation_window, quantity_source and other legacy descriptors are
    // read but ignored. Meter/granularity are ONLY translated to dated SKU
    // metering for publish cases. They do not affect amount_for.
    #[serde(flatten)]
    _ignored: BTreeMap<String, toml::Value>,
}
#[derive(Deserialize)]
struct Included {
    quantity: i64,
    #[serde(flatten)]
    _ignored: BTreeMap<String, toml::Value>,
}
#[derive(Deserialize)]
struct Band {
    from_qty: i64,
    to_qty: Top,
    rate: String,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Top {
    Number(i64),
    Open(String),
}
#[derive(Deserialize)]
struct Given {
    q: i64,
}
#[derive(Deserialize)]
struct Expected {
    charge: Option<String>,
    publish: Option<String>,
    error_code: Option<String>,
}
#[derive(Deserialize)]
struct Assertion {
    given: Option<Given>,
    expect: Expected,
    #[serde(default)]
    why: String,
}

/// A price decimal of the corpus: plain text within the rate limit, in its canonical spelling.
fn rate(text: &str) -> Decimal {
    let value = numeric::parse_rate(text).unwrap();
    assert_eq!(
        bss_money::canonical_decimal(value),
        text,
        "a corpus decimal is written in its canonical spelling"
    );
    value
}
/// An expected charge: major-unit text, read into an exact value.
fn charge(text: &str) -> ExactAmount {
    let value = bss_money::parse_decimal(text).unwrap();
    assert_eq!(
        bss_money::canonical_decimal(value),
        text,
        "a corpus charge is written in its canonical spelling"
    );
    ExactAmount::from_decimal(value)
}
fn data(s: &Snapshot) -> PriceData {
    assert!(matches!(s.currency.as_str(), "USD" | "EUR"));
    match s.model_kind {
        Model::Flat => PriceData::Flat {
            amount: rate(s.amount.as_deref().unwrap()),
        },
        Model::PerUnit => PriceData::PerUnit {
            rate: rate(s.rate.as_deref().unwrap()),
        },
        Model::Package => PriceData::Package {
            package_size: Decimal::from(s.package_size.unwrap()),
            package_price: rate(s.package_price.as_deref().unwrap()),
        },
        Model::Graduated | Model::Volume => {
            let mut previous = 0;
            let tiers = s
                .bands
                .iter()
                .map(|b| {
                    assert_eq!(b.from_qty, previous);
                    let up_to = match &b.to_qty {
                        Top::Number(n) => {
                            previous = *n;
                            Some(Decimal::from(*n))
                        }
                        Top::Open(s) => {
                            assert_eq!(s, "open");
                            None
                        }
                    };
                    Tier {
                        up_to,
                        rate: rate(&b.rate),
                    }
                })
                .collect();
            PriceData::Tiers { tiers }
        }
    }
}
fn price(s: &Snapshot, version: i32) -> Price {
    Price {
        id: Uuid::new_v4(),
        price_book_entry_id: Uuid::nil(),
        version_no: version,
        dim_value: None,
        model: s.model_kind,
        currency: bss_money::CurrencySpec::try_new("EUR".into(), 2).unwrap(),
        price: Some(data(s)),
        min_fee: None,
        eligibility: Eligibility::All,
        effective_from: Date::from_calendar_date(
            2026,
            Month::January,
            u8::try_from(version).unwrap(),
        )
        .unwrap(),
        effective_to: None,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        closed_explicitly: false,
        state: PriceState::Approved,
    }
}
fn run(path: &str) {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(format!("{path}.toml")),
    )
    .unwrap();
    let case: Case = toml::from_str(&text).unwrap();
    assert_eq!(format!("{}/{}", case.family, case.id), path);
    assert!(!case.assertions.is_empty());
    for a in &case.assertions {
        match case.kind.as_str() {
            "evaluation" => {
                let s = case.snapshot.as_ref().unwrap();
                // Test-only allowance translation, per reconciliation B. No production allowance API.
                let billable = (Decimal::from(a.given.as_ref().unwrap().q)
                    - Decimal::from(s.included_allowance.as_ref().map_or(0, |x| x.quantity)))
                .max(Decimal::ZERO);
                assert_eq!(
                    amount_for(s.model_kind, &data(s), billable).unwrap(),
                    charge(a.expect.charge.as_deref().unwrap()),
                    "{path}: {}",
                    a.why
                );
            }
            "publish" => {
                let pred = case.predecessor.as_ref().unwrap();
                let succ = case.successor.as_ref().unwrap();
                let metering = |s: &Snapshot| SkuMetering {
                    unit: s.billing_granularity.clone(),
                    usage_type_ref: s.meter.clone(),
                };
                let result = chain_guard(
                    pred.charge_kind,
                    &price(pred, 1),
                    &metering(pred),
                    &price(succ, 2),
                    &metering(succ),
                );
                match a.expect.publish.as_deref().unwrap() {
                    // D-427: the model is the entry's, fixed for its life and part of its key, so
                    // a successor in another model is a price of ANOTHER entry: a kind flip never
                    // reaches a chain, and the pair guard no longer compares a model. The corpus's
                    // rejection holds by the key, not by `chain_guard`.
                    "rejected" if pred.model_kind != succ.model_kind => {
                        assert_eq!(
                            a.expect.error_code.as_deref(),
                            Some("SUPERSESSION_UNIT_MISMATCH")
                        );
                        // The pair guard judges no model (D-427, PT-04): the flip passes it, and
                        // what keeps the two prices apart is the entry key, which takes the model
                        // (tests/entry_model_migration.rs pins that key on the stored table).
                        assert!(
                            result.is_ok(),
                            "{path}: the pair guard compares no model: {result:?}"
                        );
                    }
                    "accepted" => assert!(result.is_ok(), "{path}: {}", a.why),
                    "rejected" => {
                        assert_eq!(
                            a.expect.error_code.as_deref(),
                            Some("SUPERSESSION_UNIT_MISMATCH")
                        );
                        assert_eq!(result.unwrap_err().code, "CHAIN_MODEL_CHANGED", "{path}");
                    }
                    value => panic!("unknown publish expectation {value}"),
                }
            }
            kind => panic!("non-runnable golden {kind}"),
        }
    }
}
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for item in std::fs::read_dir(dir).unwrap() {
        let path = item.unwrap().path();
        if path.is_dir() {
            result.extend(files(&path));
        } else if path.extension().is_some_and(|s| s == "toml") {
            result.push(path);
        }
    }
    result
}
#[test]
fn exactly_nineteen_runnable_files() {
    let paths = files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden"));
    assert_eq!(paths.len(), 19);
    for path in paths {
        let text = std::fs::read_to_string(path).unwrap();
        // The other keys are read leniently, so a minor-unit key left in a case would be ignored.
        assert!(!text.contains("_minor"), "no case holds a minor-unit key");
        let c: Case = toml::from_str(&text).unwrap();
        assert!(matches!(c.kind.as_str(), "evaluation" | "publish"));
    }
}
macro_rules! case {
    ($name:ident,$path:literal) => {
        #[test]
        fn $name() {
            run($path);
        }
    };
}
case!(flat_quantity_independent, "flat/quantity-independent");
case!(per_unit_external_quantity, "per-unit/external-quantity");
case!(
    per_unit_untiered_included_allowance,
    "per-unit/untiered-included-allowance"
);
case!(per_unit_sub_minor_rate, "per-unit/sub-minor-rate");
case!(
    package_repeating_block_roundup,
    "package/repeating-block-roundup"
);
case!(
    tier_boundary_graduated_band_edge,
    "tier-boundary/graduated-band-edge"
);
case!(
    tier_boundary_graduated_included_allowance,
    "tier-boundary/graduated-included-allowance"
);
case!(
    tier_boundary_volume_variant_a_cliff,
    "tier-boundary/volume-variant-a-cliff"
);
case!(
    supersession_flat_price_change_accepted,
    "supersession-continuity/flat-price-change-accepted"
);
case!(
    supersession_per_unit_non_usage_price_change_accepted,
    "supersession-continuity/per-unit-non-usage-price-change-accepted"
);
case!(
    supersession_per_unit_usage_price_change_accepted,
    "supersession-continuity/per-unit-usage-price-change-accepted"
);
case!(
    supersession_price_change_accepted,
    "supersession-continuity/price-change-accepted"
);
case!(
    supersession_volume_band_change_accepted,
    "supersession-continuity/volume-band-change-accepted"
);
case!(
    supersession_package_price_change_accepted,
    "supersession-continuity/package-price-change-accepted"
);
case!(
    supersession_kind_flip_rejected,
    "supersession-continuity/kind-flip-rejected"
);
case!(
    supersession_package_size_change_rejected,
    "supersession-continuity/package-size-change-rejected"
);
case!(
    supersession_unit_change_rejected,
    "supersession-continuity/unit-change-rejected"
);
case!(
    supersession_per_unit_usage_granularity_change_rejected,
    "supersession-continuity/per-unit-usage-granularity-change-rejected"
);
case!(
    supersession_meter_change_rejected,
    "supersession-continuity/meter-change-rejected"
);
