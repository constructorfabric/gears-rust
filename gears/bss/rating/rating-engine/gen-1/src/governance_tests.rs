#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;
use uuid::Uuid;

use super::{ChargeKind, CoreCell, CoreResolve, check_bundle, check_injective};
use crate::input::{CellSelection, CorePriceModel, MeterRef};
use crate::test_input::{binding, cell, eur, id, item, period_line, price_id};

fn model() -> CorePriceModel {
    CorePriceModel::PerUnit {
        unit_amount: Decimal::ONE,
    }
}

fn usage_cell(item_id: Uuid, usage_type_id: &str, value: Option<&str>) -> CoreCell {
    let mut binding = binding(item_id, id(0x9_0000 + item_id.as_u128()), model(), eur());
    binding.kind = ChargeKind::Usage;
    binding.meter = Some(MeterRef {
        usage_type_id: usage_type_id.to_owned(),
        version: "1".to_owned(),
    });
    let mut cell = cell(binding);
    cell.selection.dimension_value = value.map(str::to_owned);
    cell
}

#[test]
fn two_items_on_one_meter_and_value_are_non_injective() {
    let rated = usage_cell(id(1), "products.derived/cloudlet", Some("eu"));
    let resolve = CoreResolve {
        resolve_date: time::macros::date!(2026 - 09 - 01),
        binding_digest: String::new(),
        cells: vec![
            rated.clone(),
            usage_cell(id(2), "products.derived/cloudlet", Some("eu")),
        ],
    };
    let error = check_injective(&resolve, &rated, rated.binding.as_ref().unwrap()).unwrap_err();
    assert_eq!(error.reason_code(), "non_injective_mapping");
}

#[test]
fn another_value_meter_or_kind_is_injective() {
    let rated = usage_cell(id(1), "products.derived/cloudlet", Some("eu"));
    let mut recurring = usage_cell(id(4), "products.derived/cloudlet", Some("eu"));
    recurring.binding.as_mut().unwrap().kind = ChargeKind::Recurring;
    let resolve = CoreResolve {
        resolve_date: time::macros::date!(2026 - 09 - 01),
        binding_digest: String::new(),
        cells: vec![
            rated.clone(),
            usage_cell(id(2), "products.derived/cloudlet", Some("us")),
            usage_cell(id(3), "products.derived/storage", Some("eu")),
            recurring,
            CoreCell {
                selection: CellSelection {
                    item_id: id(5),
                    dimension_value: Some("eu".into()),
                },
                binding: None,
            },
        ],
    };
    check_injective(&resolve, &rated, rated.binding.as_ref().unwrap()).unwrap();
}

#[test]
fn two_recurring_items_on_one_sku_are_non_injective() {
    let input = period_line(model(), "1");
    let rated = input.pricing.resolves[0].cells[0].clone();
    let mut twin = binding(id(7), price_id(), model(), eur());
    twin.sku_id = rated.binding.as_ref().unwrap().sku_id;
    let mut resolve = input.pricing.resolves[0].clone();
    resolve.cells.push(cell(twin));
    let error = check_injective(&resolve, &rated, rated.binding.as_ref().unwrap()).unwrap_err();
    assert_eq!(error.reason_code(), "non_injective_mapping");
    assert!(error.detail().contains(&item().to_string()));
}

#[test]
fn a_bundle_line_fails_closed() {
    let mut input = period_line(model(), "1");
    check_bundle(&input.fact).unwrap();
    input.fact.bundle = Some("starter-bundle".into());
    assert_eq!(
        check_bundle(&input.fact).unwrap_err().reason_code(),
        "unsupported_primitive"
    );
}
