//! Rating's arithmetic against Pricing's (feature 02 §3.6 step 5, §6; DESIGN §3.3 "Golden vectors").
//!
//! Every price model in Pricing's golden contract fixtures (`pricing/tests/contract/*.json`) and
//! seam fixtures (`pricing/tests/seam_fixtures/*.json`) is priced by each retained generation's model
//! function and by Pricing's `amount_for` oracle at a set of quantities, and the two must agree
//! exactly. `amount_for` computes with the same exact fractions (`bss_money::exact`), so every
//! case where it returns is an exact case. The seam fixtures' stated amounts are checked against
//! both. Rating never calls `amount_for` to price; it is an oracle only.
//!
//! Every model is priced; the Atlas arithmetic fixtures are slots, each checked here (through the
//! model function, or a golden vector of the generation) or owned by a later feature.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use bss_pricing::domain::money;
use bss_pricing::domain::price_book_entry::Model;
use bss_rating_core::engine::gen_1;
use gen_1::exact::{ExactMoney, ExactQuantity};
use gen_1::input::{CoreCurrency, CorePriceModel};
use rust_decimal::Decimal;
use serde_json::{Value, json};

/// Quantities every model is priced at: zero, whole, fractional, band edges and a quantity with
/// 28 fractional digits.
const QUANTITIES: [&str; 11] = [
    "0",
    "1",
    "2.5",
    "8",
    "10",
    "12",
    "20",
    "0.000001",
    "123456789.123456789",
    "3.0000000000000000000000000001",
    "0.0000000000000000000000000001",
];

fn pricing_tests() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pricing/pricing/tests")
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {}", dir.display());
    files
}

/// `(model, price body)` pairs: a price read (`{model, price}`) or a resolved item
/// (`{model, chains: [{binding: {price}}]}`).
fn priced_models(value: &Value, found: &mut BTreeSet<(String, String)>) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(model)) = object.get("model") {
                if let Some(price @ Value::Object(_)) = object.get("price") {
                    found.insert((model.clone(), price.to_string()));
                }
                for chain in object
                    .get("chains")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(price) = chain.pointer("/binding/price") {
                        found.insert((model.clone(), price.to_string()));
                    }
                }
            }
            object
                .values()
                .for_each(|nested| priced_models(nested, found));
        }
        Value::Array(items) => items.iter().for_each(|nested| priced_models(nested, found)),
        _ => {}
    }
}

fn canonical(text: &str) -> String {
    bss_money::canonical_decimal(bss_money::parse_decimal(text).unwrap())
}

/// The generation's frozen model for a Pricing price body (numbers as canonical text).
fn rating_model(model: &str, price: &Value) -> CorePriceModel {
    let text = |key: &str| canonical(price[key].as_str().unwrap());
    let body = match model {
        "flat" => json!({"model": "flat", "amount": text("amount")}),
        "per_unit" => json!({"model": "per_unit", "unit_amount": text("rate")}),
        "volume" | "graduated" => json!({
            "model": model,
            "tiers": price["tiers"].as_array().unwrap().iter().map(|tier| json!({
                "up_to": tier["up_to"].as_str().map(canonical),
                "rate": canonical(tier["rate"].as_str().unwrap()),
            })).collect::<Vec<_>>(),
        }),
        "package" => json!({
            "model": "package",
            "package_size": text("package_size"),
            "package_price": text("package_price"),
        }),
        other => panic!("unknown model {other}"),
    };
    serde_json::from_value(body).unwrap()
}

fn eur() -> CoreCurrency {
    CoreCurrency {
        code: "EUR".to_owned(),
        scale: 2,
    }
}

/// Generation 1's exact amount, as the money crate writes it, or its reason code.
fn rating_amount(model: &CorePriceModel, quantity: &str) -> Result<String, &'static str> {
    gen_1::model::price(
        model,
        &eur(),
        &ExactQuantity::parse(quantity).unwrap(),
        gen_1::ArithmeticBudget::GEN_1,
    )
    .map(|priced| priced.amount.amount().to_string())
    .map_err(|error| error.reason_code())
}

/// Pricing's `amount_for`, or `None` where it refuses the quantity.
fn oracle_amount(model: &str, price: &Value, quantity: &str) -> Option<String> {
    let model: Model = model.parse().unwrap();
    let data = money::decode(model, price.clone()).unwrap();
    let quantity = Decimal::from_str_exact(quantity).ok()?;
    money::amount_for(model, &data, quantity)
        .ok()
        .map(|amount| amount.to_string())
}

/// The exact value of a stated decimal amount, written as the money crate writes it.
fn stated(amount: &str) -> String {
    bss_money::exact::ExactAmount::from_decimal(bss_money::parse_decimal(amount).unwrap())
        .to_string()
}

#[test]
fn every_priced_fixture_model_agrees_with_amount_for() {
    let mut models = BTreeSet::new();
    for path in json_files(&pricing_tests().join("contract")) {
        priced_models(
            &serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap(),
            &mut models,
        );
    }
    assert!(models.len() >= 5, "{models:?}");
    let (mut compared, mut kinds) = (0, BTreeSet::new());
    for (model, price) in &models {
        let price: Value = serde_json::from_str(price).unwrap();
        let rating = rating_model(model, &price);
        for quantity in QUANTITIES {
            // Every quantity here is a `Decimal` the oracle takes, so a refusal is a fixture or
            // oracle change to look at, never a case to skip.
            let oracle = oracle_amount(model, &price, quantity).unwrap_or_else(|| {
                panic!(
                    "amount_for refuses {model} {price} at {quantity}; Rating gives {:?}",
                    rating_amount(&rating, quantity)
                )
            });
            let amount = rating_amount(&rating, quantity)
                .unwrap_or_else(|code| panic!("{model} {price} at {quantity}: {code}"));
            assert_eq!(amount, oracle, "{model} {price} at {quantity}");
            compared += 1;
            kinds.insert(model.clone());
        }
    }
    assert!(compared > 0);
    let kinds: Vec<String> = kinds.into_iter().collect();
    println!(
        "compared {compared} amounts of the models {}",
        kinds.join(", ")
    );
}

#[test]
fn the_seam_fixtures_state_what_rating_and_the_oracle_compute() {
    for path in json_files(&pricing_tests().join("seam_fixtures")) {
        let fixture: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let commercial = &fixture["given"]["commercial"];
        let model = commercial["model"].as_str().unwrap();
        let price = match model {
            "per_unit" => json!({"rate": commercial["rate"]}),
            _ => json!({"tiers": commercial["tiers"]}),
        };
        let rating = rating_model(model, &price);
        for case in fixture["expected"]["arithmetic"].as_array().unwrap() {
            let quantity = case["quantity"].as_str().unwrap();
            let expected = stated(case["amount"].as_str().unwrap());
            let oracle = oracle_amount(model, &price, quantity).unwrap();
            assert_eq!(oracle, expected, "{} oracle at {quantity}", path.display());
            assert_eq!(
                rating_amount(&rating, quantity),
                Ok(expected),
                "{} at {quantity}",
                path.display()
            );
        }
    }
}

/// F02: a VM-hour at 0.047 EUR per unit; 10 units are exactly 47/100 (pricing's `vm-hour`).
fn f02_vm_hour() {
    let rating = rating_model("per_unit", &json!({"rate": "0.047"}));
    assert_eq!(rating_amount(&rating, "10"), Ok(stated("0.47")));
}

/// F19: one third of a period stays the exact fraction 1/3.
fn f19_one_third_stays_a_third() {
    let third = ExactMoney::from_price(Decimal::ONE, &eur())
        .times_fraction(
            gen_1::exact::CoveredFraction {
                covered_seconds: 1,
                period_seconds: 3,
            },
            gen_1::ArithmeticBudget::GEN_1,
            "F19",
        )
        .unwrap();
    let wire = third.to_wire("F19").unwrap();
    assert_eq!(
        (wire.numerator.as_str(), wire.denominator.as_str()),
        ("1", "3")
    );
}

/// F27: two hourly contributions of 0.005 EUR sum to exactly 1/100, which one invoice line posts
/// as 0.01.
fn f27_two_half_cents_are_one_cent() {
    let half_cent = ExactMoney::from_price("0.005".parse().unwrap(), &eur());
    let sum = half_cent.checked_add(&half_cent, "F27").unwrap();
    let wire = sum.to_wire("F27").unwrap();
    assert_eq!(
        (wire.numerator.as_str(), wire.denominator.as_str()),
        ("1", "100")
    );
}

/// Bands `[0, 10)` at 0.02 and `[10, ∞)` at 0.015 (the Atlas cloudlet hours).
fn cloudlet_tiers(model: &str) -> CorePriceModel {
    rating_model(
        model,
        &json!({"tiers": [{"up_to": "10", "rate": "0.02"}, {"up_to": null, "rate": "0.015"}]}),
    )
}

/// One golden vector of generation 1, with the live digest in place of its token.
fn golden(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../rating-engine/gen-1/golden/{name}.input.json"));
    let case: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    case["input"]
        .to_string()
        .replace("<engine_digest>", gen_1::ENGINE_DIGEST)
}

/// The exact amount of the one line `evaluate` rates for a golden vector.
fn golden_line(name: &str) -> String {
    let input: gen_1::EvaluationInput = serde_json::from_str(&golden(name)).unwrap();
    let outcome = gen_1::evaluate(&input).unwrap();
    let line = &outcome.lines[0].exact_amount;
    format!("{}/{}", line.numerator, line.denominator)
}

/// F03: 2.5 VM-hours at 0.047 EUR are exactly 47/400.
fn f03_a_fraction_of_a_vm_hour() {
    let rating = rating_model("per_unit", &json!({"rate": "0.047"}));
    assert_eq!(rating_amount(&rating, "2.5"), Ok(stated("0.1175")));
}

/// F10: an order's usage is rates only, never part of its totals or TCV.
fn f10_usage_is_excluded_from_order_totals() {
    let input: gen_1::OrderEvaluationInput =
        serde_json::from_str(&golden("order-mixed-basis-and-usage")).unwrap();
    let evaluation = gen_1::evaluate_order(&input).unwrap();
    assert_eq!(evaluation.tcv.unwrap().amount.exact, "140");
    let usage = evaluation.lines[1]
        .items
        .iter()
        .find(|item| item.usage_rates.is_some())
        .unwrap();
    assert_eq!(usage.gross, None);
}

/// One cloudlet hour of the golden vector `f23-hourly-volume-q8` (volume, `[0, 10)` at 0.02 then
/// 0.015) moved to `hour` with quantity `q`, as `evaluate` rates it.
fn hourly_volume(hour: u8, q: &str) -> String {
    let mut input: Value = serde_json::from_str(&golden("f23-hourly-volume-q8")).unwrap();
    let at = |h: u8| format!("2026-09-15T{h:02}:00:00Z");
    let window = &mut input["child"]["window"];
    for (field, h) in [
        ("window_start", hour),
        ("window_end", hour + 1),
        ("served_from", hour),
        ("served_to", hour + 1),
    ] {
        window[field] = json!(at(h));
    }
    let slice = &mut input["quantities"]["slices"][0];
    slice["slice_start"] = json!(at(hour));
    slice["slice_end"] = json!(at(hour + 1));
    slice["q"] = json!(q);
    slice["granules"][0]["granule_start"] = json!(at(hour));
    slice["granules"][0]["inputs"][0]["q"] = json!(q);
    let input: gen_1::EvaluationInput = serde_json::from_value(input).unwrap();
    let outcome = gen_1::evaluate(&input).unwrap();
    assert_eq!(outcome.lines.len(), 1);
    let line = &outcome.lines[0].exact_amount;
    format!("{}/{}", line.numerator, line.denominator)
}

/// F23: each hour is its own window, so 8 then 12 cloudlet-hours rate 0.16 + 0.18 = 0.34, never the
/// combined-volume 0.30.
fn f23_hourly_volume_never_pools() {
    assert_eq!(golden_line("f23-hourly-volume-q8"), "4/25");
    assert_eq!(golden_line("f23-hourly-volume-q12"), "9/50");
    let volume = cloudlet_tiers("volume");
    assert_eq!(rating_amount(&volume, "20"), Ok(stated("0.30")));
}

/// F24: one hour at the band edge and above it, under volume and graduated.
fn f24_volume_and_graduated_bands() {
    let (volume, graduated) = (cloudlet_tiers("volume"), cloudlet_tiers("graduated"));
    assert_eq!(rating_amount(&volume, "10"), Ok(stated("0.15")));
    assert_eq!(rating_amount(&volume, "12"), Ok(stated("0.18")));
    assert_eq!(rating_amount(&graduated, "10"), Ok(stated("0.20")));
    assert_eq!(rating_amount(&graduated, "12"), Ok(stated("0.23")));
    assert_eq!(golden_line("f24-hourly-graduated-q12"), "23/100");
}

/// F25: two resources of 6 in one hour, subscription-line scope. Their records pool in the line's
/// counter (feature 05), so the core rates one hour of 12 on one line: 0.18. Rated per resource,
/// as `resource` scope would, they would be two hours of 6: 0.12 + 0.12 = 0.24.
fn f25_resources_pool_on_one_line() {
    assert_eq!(hourly_volume(10, "12"), "9/50");
    assert_eq!(hourly_volume(10, "6"), "3/25");
}

/// F26: a late record moves the volume window into the next band, and the next evaluation re-prices
/// all of it: 8 rated 0.16, then 12 rated 0.18 (never 0.16 plus 4 × 0.015 = 0.22).
fn f26_a_late_record_reprices_the_window() {
    assert_eq!(hourly_volume(10, "8"), "4/25");
    assert_eq!(hourly_volume(10, "12"), "9/50");
}

/// F28: catch-up hours rate as their own windows in any order: the 11:00 hour evaluated before the
/// 10:00 hour gives the same amounts.
fn f28_catch_up_hours_rate_alone_in_any_order() {
    let late_first = [hourly_volume(11, "12"), hourly_volume(10, "8")];
    let in_order = [hourly_volume(10, "8"), hourly_volume(11, "12")];
    assert_eq!(late_first, ["9/50".to_owned(), "4/25".to_owned()]);
    assert_eq!(
        [&in_order[1], &in_order[0]],
        [&late_first[0], &late_first[1]]
    );
}

/// F29: activation at 10:30 keeps the hour's whole thresholds: Q = 4 rates 0.08.
fn f29_a_partial_hour_keeps_whole_thresholds() {
    assert_eq!(golden_line("f29-partial-hour"), "2/25");
}

/// F32: corrections to different hours give exact revisions 0.34 → 0.33 → 0.351: each corrected
/// hour is evaluated again and the parent sums the hours' exact amounts.
fn f32_corrections_rerate_each_hour_exactly() {
    let revision = |a: &str, b: &str| {
        let sum = [hourly_volume(10, a), hourly_volume(11, b)]
            .iter()
            .map(|fraction| {
                let (numerator, denominator) = fraction.split_once('/').unwrap();
                bss_money::exact::ExactAmount::from_coefficient(numerator.parse().unwrap(), 0)
                    .checked_div(&bss_money::exact::ExactAmount::from_coefficient(
                        denominator.parse().unwrap(),
                        0,
                    ))
                    .unwrap()
            })
            .reduce(|left, right| left.checked_add(&right).unwrap())
            .unwrap();
        sum.to_string()
    };
    assert_eq!(revision("8", "12"), stated("0.34"));
    assert_eq!(revision("7.5", "12"), stated("0.33"));
    assert_eq!(revision("7.5", "13.4"), stated("0.351"));
}

/// An Atlas arithmetic fixture and its check.
type Fixture = (&'static str, fn());

/// Every Atlas arithmetic fixture of feature 02 §6 and its check.
fn atlas() -> Vec<Fixture> {
    vec![
        ("F02", f02_vm_hour),
        ("F03", f03_a_fraction_of_a_vm_hour),
        ("F10", f10_usage_is_excluded_from_order_totals),
        ("F19", f19_one_third_stays_a_third),
        ("F23", f23_hourly_volume_never_pools),
        ("F24", f24_volume_and_graduated_bands),
        ("F25", f25_resources_pool_on_one_line),
        ("F26", f26_a_late_record_reprices_the_window),
        ("F27", f27_two_half_cents_are_one_cent),
        ("F28", f28_catch_up_hours_rate_alone_in_any_order),
        ("F29", f29_a_partial_hour_keeps_whole_thresholds),
        ("F32", f32_corrections_rerate_each_hour_exactly),
    ]
}

#[test]
fn every_atlas_arithmetic_fixture_is_checked() {
    let slots = atlas();
    let ids: Vec<String> = slots.iter().map(|(id, _)| (*id).to_owned()).collect();
    assert_eq!(ids, feature_02_atlas_fixtures());
    for (_, check) in slots {
        check();
    }
}

/// The Atlas arithmetic fixtures feature 02 §6 lists ("F02, F03, F10, F19, F23–F29 and F32"), its
/// ranges expanded.
fn feature_02_atlas_fixtures() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/features/02-evaluation-core.md");
    let text = fs::read_to_string(path).unwrap();
    let criterion = text
        .lines()
        .find(|line| line.contains("The Atlas arithmetic fixtures"))
        .unwrap();
    let list = criterion
        .split("fixtures ")
        .nth(1)
        .and_then(|rest| rest.split(" produce").next())
        .unwrap()
        .replace(" and ", ", ");
    let number = |id: &str| id.trim().trim_start_matches('F').parse::<u32>().unwrap();
    list.split(", ")
        .flat_map(|item| match item.split_once('\u{2013}') {
            Some((from, to)) => (number(from)..=number(to)).collect::<Vec<_>>(),
            None => vec![number(item)],
        })
        .map(|n| format!("F{n:02}"))
        .collect()
}
