#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_rating_core::engine::current::input::{
    BillingTiming, CellSelection, CoreCell, RatingWindowKind,
};
use rust_decimal::Decimal;
use uuid::Uuid;

use super::{cell_to_core, cell_to_pricing, digest_bytes, digest_hex, to_core, to_pricing};
use crate::test_providers::subscriptions::{recurring_binding, usage_binding};

#[test]
fn a_digest_reads_back_from_its_hex() {
    let bytes: [u8; 32] = std::array::from_fn(|i| u8::try_from(i * 7 % 256).unwrap());
    let hex = digest_hex(&bytes);
    assert_eq!(hex.len(), 64);
    assert_eq!(digest_bytes("d", &hex).unwrap(), bytes);
    assert!(digest_bytes("d", "XYZ").is_err());
}

#[test]
fn a_binding_goes_through_both_conversions_unchanged() {
    for binding in [
        usage_binding(
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            RatingWindowKind::CalendarHourUtc,
            "products.derived/cloudlet@1",
            time::Date::from_ordinal_date(2026, 300).ok(),
        ),
        recurring_binding(
            Uuid::from_u128(3),
            Uuid::from_u128(4),
            Decimal::new(1999, 2),
            BillingTiming::Arrears,
        ),
    ] {
        assert_eq!(to_core(&to_pricing(&binding).unwrap()), binding);
    }
}

#[test]
fn an_uncovered_cell_stays_uncovered() {
    let cell = CoreCell {
        selection: CellSelection {
            item_id: Uuid::from_u128(1),
            dimension_value: Some("eu".to_owned()),
        },
        binding: None,
    };
    assert_eq!(cell_to_core(&cell_to_pricing(&cell).unwrap()), cell);
}
