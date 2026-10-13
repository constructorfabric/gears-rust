#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::price_book_entry::Model;

fn decoded(id: u128) -> Price {
    Price {
        id: Uuid::from_u128(id),
        price_book_entry_id: Uuid::from_u128(100),
        version_no: 1,
        dim_value: None,
        model: Model::PerUnit,
        currency: bss_money::CurrencySpec::try_new("EUR".into(), 2).unwrap(),
        price: Some(money::PriceData::PerUnit { rate: Decimal::ONE }),
        min_fee: None,
        eligibility: Eligibility::All,
        effective_from: time::macros::date!(2026 - 01 - 01),
        effective_to: None,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        closed_explicitly: false,
        state: PriceState::Draft,
    }
}

/// A price DTO is built from a row and the price decoded from that row. Another row's decoded
/// price would serve one price's money under another's currency spec, so it is a corrupt row.
#[test]
fn a_row_served_with_another_rows_price_is_a_corrupt_row() {
    let own = decoded(1);
    let row = stored(
        Uuid::from_u128(9),
        &own,
        None,
        Uuid::from_u128(8),
        OffsetDateTime::UNIX_EPOCH,
    )
    .unwrap();
    let today = time::macros::date!(2026 - 06 - 01);
    let served = PricingPriceDto::at(row.clone(), &own, today).unwrap();
    assert_eq!(served.id, own.id, "the row's own price serves it");
    assert!(
        matches!(
            PricingPriceDto::at(row, &decoded(2), today),
            Err(RepoError::CorruptRow(_))
        ),
        "another row's price is refused"
    );
}
