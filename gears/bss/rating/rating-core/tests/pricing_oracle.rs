//! Rating's arithmetic against Pricing's (feature 02 §3.6 step 5, §6; DESIGN §3.3 "Golden vectors").
//!
//! Every price model in Pricing's golden contract fixtures (`pricing/tests/contract/*.json`) and
//! seam fixtures (`pricing/tests/seam_fixtures/*.json`) is priced by each retained generation's model
//! function and by Pricing's `amount_for` oracle at a set of quantities, and the two must agree
//! exactly. `amount_for` computes with the same exact fractions (`bss_money::exact`), so every
//! case where it returns is an exact case. The seam fixtures' stated amounts are checked against
//! both. Rating never calls `amount_for` to price; it is an oracle only.
//!
//! A model a generation does not price yet fails `unsupported_primitive` and is listed as pending
//! below; the Atlas arithmetic fixtures are slots, each checked here or owned by a later feature.
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

/// The models generation 1 does not price yet; feature 02's pricing semantics (F02b) empties it.
const PENDING_MODELS: [&str; 3] = ["volume", "graduated", "package"];

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
    let (mut compared, mut pending) = (0, BTreeSet::new());
    for (model, price) in &models {
        let price: Value = serde_json::from_str(price).unwrap();
        let rating = rating_model(model, &price);
        for quantity in QUANTITIES {
            let Some(oracle) = oracle_amount(model, &price, quantity) else {
                continue;
            };
            match rating_amount(&rating, quantity) {
                Ok(amount) => {
                    assert_eq!(amount, oracle, "{model} {price} at {quantity}");
                    compared += 1;
                }
                Err("unsupported_primitive") if PENDING_MODELS.contains(&model.as_str()) => {
                    pending.insert(model.clone());
                }
                Err(code) => panic!("{model} {price} at {quantity}: {code}"),
            }
        }
    }
    assert!(compared > 0);
    let pending: Vec<String> = pending.into_iter().collect();
    println!(
        "compared {compared} amounts; pending models: {}",
        pending.join(", ")
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
            match rating_amount(&rating, quantity) {
                Ok(amount) => assert_eq!(amount, expected, "{} at {quantity}", path.display()),
                Err(code) => assert!(
                    code == "unsupported_primitive" && PENDING_MODELS.contains(&model),
                    "{}: {code}",
                    path.display()
                ),
            }
        }
    }
}

/// An Atlas arithmetic fixture: checked here, or owned by the feature that prices it.
enum Slot {
    Checked(fn()),
    Pending(&'static str),
}

/// F02: a VM-hour at 0.047 EUR per unit; 10 units are exactly 47/100 (pricing's `vm-hour`).
fn f02_vm_hour() {
    let rating = rating_model("per_unit", &json!({"rate": "0.047"}));
    assert_eq!(rating_amount(&rating, "10"), Ok(stated("0.47")));
}

/// F19: one third of a period stays the exact fraction 1/3.
fn f19_one_third_stays_a_third() {
    let third = ExactMoney::from_price(Decimal::ONE, &eur())
        .times_ratio(1, 3, gen_1::ArithmeticBudget::GEN_1, "F19")
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

fn atlas() -> Vec<(&'static str, Slot)> {
    vec![
        ("F02", Slot::Checked(f02_vm_hour)),
        (
            "F03",
            Slot::Pending("F02b: the recurring line math of namespace 09"),
        ),
        (
            "F10",
            Slot::Pending("F02b: evaluate_order excludes usage from order totals"),
        ),
        ("F19", Slot::Checked(f19_one_third_stays_a_third)),
        (
            "F23",
            Slot::Pending("F02b: hourly volume 0.16 + 0.18 = 0.34, never 0.30"),
        ),
        (
            "F24",
            Slot::Pending("F02b: volume 0.15 / graduated 0.20 and 0.23 bands"),
        ),
        (
            "F25",
            Slot::Pending("F02b: two resources in one hour pool on one line"),
        ),
        (
            "F26",
            Slot::Pending("F02b: a late record re-prices the whole volume window"),
        ),
        ("F27", Slot::Checked(f27_two_half_cents_are_one_cent)),
        (
            "F28",
            Slot::Pending("F02b: catch-up hours rate as their own windows"),
        ),
        (
            "F29",
            Slot::Pending("F02b: a partial hour keeps whole thresholds (Q = 4 is 0.08)"),
        ),
        (
            "F32",
            Slot::Pending("F02b: a correction re-rates the window exactly"),
        ),
    ]
}

#[test]
fn every_atlas_arithmetic_fixture_has_a_slot() {
    let slots = atlas();
    let ids: Vec<&str> = slots.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        ids,
        [
            "F02", "F03", "F10", "F19", "F23", "F24", "F25", "F26", "F27", "F28", "F29", "F32"
        ]
    );
    for (id, slot) in slots {
        match slot {
            Slot::Checked(check) => check(),
            Slot::Pending(owner) => println!("{id}: pending, {owner}"),
        }
    }
}
