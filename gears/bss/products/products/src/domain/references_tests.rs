#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
#[test]
fn reservations_require_an_unfenced_live_sku() {
    for lifecycle in [
        Lifecycle::Draft,
        Lifecycle::Published,
        Lifecycle::Deprecated,
        Lifecycle::Retired,
    ] {
        assert!(reservation_allowed(lifecycle, true).is_err());
        assert_eq!(
            reservation_allowed(lifecycle, false).is_ok(),
            matches!(lifecycle, Lifecycle::Published | Lifecycle::Deprecated)
        );
    }
    assert_eq!(
        reservation_allowed(Lifecycle::Published, true)
            .unwrap_err()
            .code(),
        "SKU_FENCED"
    );
    assert_eq!(
        reservation_allowed(Lifecycle::Draft, false)
            .unwrap_err()
            .code(),
        "SKU_FENCED"
    );
    assert_eq!(
        reservation_allowed(Lifecycle::Retired, false)
            .unwrap_err()
            .code(),
        "SKU_FENCED"
    );
}
#[test]
fn live_references_block_both_fence_kinds() {
    for code in ["SKU_REFERENCED", "SKU_TYPE_FROZEN"] {
        assert!(fence_allowed(0, code).is_ok());
        assert_eq!(fence_allowed(1, code).unwrap_err().code(), code);
    }
    assert_eq!(
        [
            RefKind::PriceBookEntry.as_str(),
            RefKind::PlanItem.as_str(),
            RefKind::SoldAs.as_str()
        ],
        ["price_book_entry", "plan_item", "sold_as"]
    );
    assert_eq!(
        [
            RefState::Reserved.as_str(),
            RefState::Confirmed.as_str(),
            RefState::Released.as_str()
        ],
        ["reserved", "confirmed", "released"]
    );
}
