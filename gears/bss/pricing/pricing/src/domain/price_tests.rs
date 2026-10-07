#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::{
    money::PriceData,
    price_book_entry::{ChargeKind, Model},
    test_support::{date, dec},
};
use uuid::Uuid;
fn price(v: i32, from: &str, dim: Option<&str>, state: PriceState) -> Price {
    Price {
        id: Uuid::from_u128(u128::try_from(v).unwrap()),
        price_book_entry_id: Uuid::from_u128(100),
        version_no: v,
        dim_value: dim.map(str::to_owned),
        model: Model::PerUnit,
        price: Some(PriceData::PerUnit { rate: dec("1") }),
        min_fee: None,
        eligibility: Eligibility::All,
        effective_from: date(from),
        effective_to: None,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        closed_explicitly: false,
        state,
    }
}
fn prices() -> Vec<Price> {
    let mut r = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-06-01", None, PriceState::Approved),
        price(3, "2026-06-01", Some("us"), PriceState::Approved),
        price(4, "2026-12-01", Some("us"), PriceState::Pending),
    ];
    normalize_windows(&mut r);
    r
}
fn rules(
    r: &Price,
    siblings: &[Price],
    values: Option<&[String]>,
) -> Vec<crate::domain::RuleError> {
    validate(
        r,
        ChargeKind::Usage,
        values,
        siblings,
        date("2026-09-23"),
        2,
    )
}
#[test]
fn prototype_l45_l47_version_at_ignores_pending() {
    let mut r = prices();
    r[3].dim_value = None;
    assert_eq!(
        version_at(&r, r[0].price_book_entry_id, date("2026-03-01"), None)
            .unwrap()
            .version_no,
        1
    );
    assert_eq!(
        version_at(&r, r[0].price_book_entry_id, date("2026-06-01"), None)
            .unwrap()
            .version_no,
        2
    );
    assert_eq!(
        version_at(&r, r[0].price_book_entry_id, date("2026-12-15"), None)
            .unwrap()
            .version_no,
        2
    );
}
#[test]
fn prototype_l48_l50_status() {
    let r = prices();
    assert_eq!(status(&r[0], date("2026-09-23")), "superseded");
    assert_eq!(status(&r[1], date("2026-09-23")), "active");
    assert_eq!(status(&r[3], date("2026-09-23")), "pending");
    assert_eq!(
        status(
            &price(5, "2027-01-01", None, PriceState::Approved),
            date("2026-09-23")
        ),
        "scheduled"
    );
}
#[test]
fn prototype_l70_l72_validation_collects_kind_past_overlap() {
    let mut r = price(9, "2026-06-01", None, PriceState::Draft);
    r.model = Model::Flat;
    r.price = Some(PriceData::Flat { amount: dec("5") });
    let c = rules(&r, &prices(), None);
    for code in [
        "MODEL_KIND_CHARGEKIND_MISMATCH",
        "WINDOW_START_IN_PAST",
        "WINDOW_OVERLAP",
    ] {
        assert!(c.iter().any(|e| e.code == code), "{code}");
    }
}
#[test]
fn prototype_l73_price_missing() {
    let mut r = price(2, "2027-01-01", None, PriceState::Draft);
    r.model = Model::Flat;
    r.price = None;
    assert_eq!(
        validate(&r, ChargeKind::Recurring, None, &[], date("2026-09-23"), 2)[0].code,
        "PRICE_MISSING"
    );
}
#[test]
fn prototype_l78_windows_normalize() {
    let r = prices();
    assert_eq!(r[0].effective_to, Some(date("2026-06-01")));
    assert_eq!(r[1].effective_to, None);
}
#[test]
fn prototype_l190_l191_return_copy_is_detached() {
    let mut r = price(1, "2026-01-01", None, PriceState::Approved);
    r.model = Model::Flat;
    r.price = Some(PriceData::Flat { amount: dec("20") });
    let mut promo = r.clone();
    promo.id = Uuid::from_u128(2);
    promo.version_no = 2;
    promo.state = PriceState::Draft;
    promo.effective_from = date("2026-10-01");
    promo.price = Some(PriceData::Flat { amount: dec("40") });
    let pair = temporary(&[r.clone()], promo, date("2026-10-11"), Uuid::from_u128(3)).unwrap();
    assert_eq!(pair[1].version_no, 3);
    assert_eq!(pair[1].effective_from, date("2026-10-11"));
    assert_eq!(pair[1].price, r.price);
    assert_eq!(pair[1].return_of_price_id, Some(r.id));
    assert_eq!(pair[1].paired_price_id, Some(pair[0].id));
    assert_eq!(pair[0].paired_price_id, Some(pair[1].id));
    // The promo half keeps its own money; the return copied the restored price's (PT-07).
    assert_eq!(pair[0].price, Some(PriceData::Flat { amount: dec("40") }));
}
#[test]
fn prototype_l192_l194_temporary_end() {
    let start = date("2026-10-01");
    assert!(validate_temporary(start, "2026-10-11").is_ok());
    for end in ["2026-10-01", "", "bogus", "2026-02-30"] {
        assert_eq!(
            validate_temporary(start, end).unwrap_err().code,
            "WINDOW_END_INVALID"
        );
    }
}
#[test]
fn prototype_l195_empty_chain_has_no_return() {
    let pair = temporary(
        &[],
        price(2, "2026-10-01", Some("us"), PriceState::Draft),
        date("2026-10-11"),
        Uuid::from_u128(3),
    )
    .unwrap();
    assert_eq!(pair.len(), 1);
    assert!(pair[0].closed_explicitly);
    assert_eq!(pair[0].effective_to, Some(date("2026-10-11")));
    assert!(pair[0].paired_price_id.is_none());
}
#[test]
fn prototype_l200_l203_temporary_resolution_and_open_tail() {
    let mut base = price(1, "2026-01-01", None, PriceState::Approved);
    base.price = Some(PriceData::PerUnit { rate: dec("20") });
    let mut promo = price(2, "2026-10-01", None, PriceState::Approved);
    promo.price = Some(PriceData::PerUnit { rate: dec("40") });
    let mut r = temporary(
        &[base.clone()],
        promo,
        date("2026-10-11"),
        Uuid::from_u128(3),
    )
    .unwrap();
    r.push(base);
    normalize_windows(&mut r);
    for (day, v) in [("2026-09-30", 1), ("2026-10-05", 2), ("2026-10-11", 3)] {
        assert_eq!(
            version_at(&r, r[0].price_book_entry_id, date(day), None)
                .unwrap()
                .version_no,
            v
        );
    }
    assert_eq!(
        open_tail(&r, r[0].price_book_entry_id, None)
            .unwrap()
            .version_no,
        3
    );
}
#[test]
fn prototype_l238_l241_shift_preserves_duration() {
    let mut r = price(3, "2026-10-01", None, PriceState::Draft);
    r.temporary_until = Some(date("2026-10-11"));
    let shifted = shift(&r, date("2026-11-01")).unwrap();
    assert_eq!(shifted.effective_from, date("2026-11-01"));
    assert_eq!(shifted.temporary_until, Some(date("2026-11-11")));
    assert_eq!(shifted.price, r.price);
    assert_eq!(shift(&r, r.effective_from).unwrap(), r);
}
#[test]
fn prototype_l243_l244_proposed_rows() {
    let mut r = prices();
    let draft = price(5, "2026-11-01", None, PriceState::Draft);
    r.push(draft.clone());
    let book = Uuid::from_u128(10);
    let mapping = [(draft.price_book_entry_id, book)];
    assert_eq!(proposed_prices(book, &mapping, &r), vec![&r[4]]);
    assert!(proposed_prices(Uuid::nil(), &mapping, &r).is_empty());
}
#[test]
fn matrix_22_proposals_sort_by_start_entry_version() {
    let book = Uuid::nil();
    let mut r = vec![
        price(3, "2026-11-01", None, PriceState::Draft),
        price(2, "2026-11-01", None, PriceState::Draft),
        price(1, "2026-10-01", None, PriceState::Draft),
    ];
    r[2].price_book_entry_id = Uuid::from_u128(200);
    assert_eq!(
        proposed_prices(
            book,
            &[
                (r[0].price_book_entry_id, book),
                (r[2].price_book_entry_id, book)
            ],
            &r
        )
        .iter()
        .map(|x| x.version_no)
        .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}
#[test]
fn prototype_l252_l256_dimensional_windows_and_fallback() {
    let r = prices();
    assert_eq!(r[0].effective_to, Some(date("2026-06-01")));
    assert!(r[1].effective_to.is_none());
    assert!(r[2].effective_to.is_none());
    for (d, dim, v) in [
        ("2026-09-01", Some("us"), 3),
        ("2026-09-01", Some("eu"), 2),
        ("2026-09-01", None, 2),
        ("2026-03-01", Some("us"), 1),
    ] {
        assert_eq!(
            version_at(&r, r[0].price_book_entry_id, date(d), dim)
                .unwrap()
                .version_no,
            v
        );
    }
}
#[test]
fn prototype_l257_l259_coverage_per_value() {
    let r = prices();
    let vals = vec!["eu".into(), "us".into()];
    let c = coverage_on(&r, r[0].price_book_entry_id, date("2026-09-01"), &vals);
    assert!(c.missing.is_empty());
    assert_eq!(c.version.unwrap().version_no, 2);
    let c = coverage_on(&r[2..], r[0].price_book_entry_id, date("2026-09-01"), &vals);
    assert_eq!(c.missing, vec!["eu"]);
}
#[test]
fn prototype_l260_unknown_dimension_value() {
    assert!(
        rules(
            &price(9, "2026-12-01", Some("mars"), PriceState::Draft),
            &prices(),
            Some(&["eu".into(), "us".into()])
        )
        .iter()
        .any(|e| e.code == "DIM_VALUE_UNKNOWN")
    );
}
#[test]
fn prototype_l261_other_value_same_start_allowed() {
    assert!(
        !rules(
            &price(9, "2026-06-01", Some("ap"), PriceState::Draft),
            &prices(),
            Some(&["ap".into(), "us".into()])
        )
        .iter()
        .any(|e| e.code == "WINDOW_OVERLAP")
    );
}
#[test]
fn prototype_l262_same_value_same_start_overlap() {
    assert!(
        rules(
            &price(9, "2026-06-01", Some("us"), PriceState::Draft),
            &prices(),
            Some(&["eu".into(), "us".into()])
        )
        .iter()
        .any(|e| e.code == "WINDOW_OVERLAP")
    );
}
#[test]
fn prototype_l263_dimension_not_declared() {
    assert!(
        rules(
            &price(9, "2026-12-01", Some("us"), PriceState::Draft),
            &[],
            None
        )
        .iter()
        .any(|e| e.code == "DIM_NOT_DECLARED")
    );
}
#[test]
fn prototype_l273_return_uses_own_chain() {
    let mut r = prices();
    r[2].price = Some(PriceData::PerUnit { rate: dec("5") });
    let pair = temporary(
        &r,
        price(7, "2026-10-01", Some("us"), PriceState::Draft),
        date("2026-10-11"),
        Uuid::from_u128(8),
    )
    .unwrap();
    assert_eq!(pair[1].dim_value.as_deref(), Some("us"));
    assert_eq!(pair[1].price, r[2].price);
}
#[test]
fn matrix_12_19_closed_end_survives_twice_and_later_default() {
    let mut r = prices();
    let only = temporary(
        &r,
        price(7, "2026-10-01", Some("eu"), PriceState::Approved),
        date("2026-10-11"),
        Uuid::from_u128(8),
    )
    .unwrap();
    assert_eq!(only.len(), 1);
    r.extend(only);
    normalize_windows(&mut r);
    normalize_windows(&mut r);
    assert_eq!(r[4].effective_to, Some(date("2026-10-11")));
    assert_eq!(
        version_at(&r, r[0].price_book_entry_id, date("2026-10-11"), Some("eu"))
            .unwrap()
            .version_no,
        2
    );
    r.push(price(9, "2026-10-10", None, PriceState::Approved));
    normalize_windows(&mut r);
    assert_eq!(r[4].effective_to, Some(date("2026-10-11")));
    assert_eq!(
        version_at(&r, r[0].price_book_entry_id, date("2026-10-11"), Some("eu"))
            .unwrap()
            .version_no,
        9
    );
}
#[test]
fn matrix_18_min_fee_is_only_validated() {
    let mut r = price(9, "2026-12-01", None, PriceState::Draft);
    for fee in ["-1", "1.001", "1.000"] {
        r.min_fee = Some(dec(fee));
        assert!(
            rules(&r, &[], None)
                .iter()
                .any(|e| e.code == "MIN_FEE_INVALID")
        );
    }
    r.min_fee = Some(dec("30"));
    assert!(rules(&r, &[], None).is_empty());
    // `amount_for` takes no fee at all: that the fee is not applied holds by its signature
    // (PT-08), and resolve serves it as stored (tests/read_contract.rs).
}
#[test]
fn matrix_18_amount_package_and_start_invalid() {
    let mut r = price(9, "2026-12-01", None, PriceState::Draft);
    r.price = Some(PriceData::PerUnit { rate: dec("-1") });
    assert_eq!(rules(&r, &[], None)[0].code, "AMOUNT_INVALID");
    r.model = Model::Package;
    r.price = Some(PriceData::Package {
        package_size: dec("0"),
        package_price: dec("5"),
    });
    assert_eq!(rules(&r, &[], None)[0].code, "PACKAGE_FIELDS_INVALID");
    assert_eq!(
        parse_start("2026-02-30").unwrap_err().code,
        "WINDOW_START_INVALID"
    );
}
#[test]
fn matrix_24_dated_metering_guard() {
    let a = price(1, "2026-01-01", None, PriceState::Approved);
    let mut b = price(2, "2026-06-01", None, PriceState::Draft);
    let old = SkuMetering {
        unit: Some("GB".into()),
        usage_type_ref: Some("storage".into()),
    };
    let mut new = old.clone();
    assert!(chain_guard(ChargeKind::Usage, &a, &old, &b, &new).is_ok());
    new.usage_type_ref = Some("other".into());
    assert_eq!(
        chain_guard(ChargeKind::Usage, &a, &old, &b, &new)
            .unwrap_err()
            .code,
        "CHAIN_MODEL_CHANGED"
    );
    new = old.clone();
    new.unit = Some("TB".into());
    assert!(chain_guard(ChargeKind::Usage, &a, &old, &b, &new).is_err());
    b.model = Model::Volume;
    assert!(chain_guard(ChargeKind::Recurring, &a, &old, &b, &new).is_ok());
    // D-427: the model is the entry's, never compared by the guard; the same metering passes.
    assert!(chain_guard(ChargeKind::Usage, &a, &old, &b, &old).is_ok());
}
#[test]
fn a_closed_price_is_still_closed_by_a_successor_that_starts_inside_it() {
    // A temporary value price [10-01, 10-11) closed explicitly; a later value price
    // approved from 10-05 must cap it, or two prices are in force on 10-05..10-11.
    let mut r = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-10-01", Some("us"), PriceState::Approved),
        price(3, "2026-10-05", Some("us"), PriceState::Approved),
    ];
    r[1].effective_to = Some(date("2026-10-11"));
    r[1].closed_explicitly = true;
    normalize_windows(&mut r);
    assert_eq!(r[1].effective_to, Some(date("2026-10-05")));
    assert!(r[1].closed_explicitly, "the closure itself is kept");
    assert_eq!(r[2].effective_to, None);
    // A successor after the explicit end leaves the explicit end alone.
    r[2].effective_from = date("2026-12-01");
    r[1].effective_to = Some(date("2026-10-11"));
    normalize_windows(&mut r);
    assert_eq!(r[1].effective_to, Some(date("2026-10-11")));
}
#[test]
fn a_temporary_price_returns_only_to_a_price_in_force_on_its_end() {
    let promo = |from: &str| {
        let mut p = price(9, from, Some("us"), PriceState::Draft);
        p.price = Some(PriceData::PerUnit { rate: dec("4") });
        p
    };
    // The value's chain ended before the promo ends: no return price, one closed price.
    let mut ended = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-03-01", Some("us"), PriceState::Approved),
    ];
    ended[1].effective_to = Some(date("2026-05-01"));
    ended[1].closed_explicitly = true;
    let out = temporary(
        &ended,
        promo("2026-10-01"),
        date("2026-10-11"),
        Uuid::from_u128(77),
    )
    .unwrap();
    assert_eq!(out.len(), 1, "an ended value chain is not revived");
    assert!(out[0].closed_explicitly);
    assert_eq!(out[0].effective_to, Some(date("2026-10-11")));
    // The value's chain starts only after the promo ends: the value falls back
    // to the default between the two, so again no return price.
    let later = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2027-01-01", Some("us"), PriceState::Approved),
    ];
    let out = temporary(
        &later,
        promo("2026-10-01"),
        date("2026-10-11"),
        Uuid::from_u128(77),
    )
    .unwrap();
    assert_eq!(
        out.len(),
        1,
        "a chain that has not started yet is not copied backwards"
    );
}
/// Phase 4 second review M1: a return to a TEMPORARY price ends where that price ends. An inner
/// `all` pair nested in an outer `all` pair returns to the outer promo's money only until the
/// outer promo's own end (its `temporary_until`), as an explicit end the stored chain carries:
/// after it the outer return is in force, and a binding of the inner return says so (D-425).
#[test]
fn a_return_to_a_temporary_price_ends_where_that_price_ends() {
    let flat = |v: i32, from: &str, amount: &str| {
        let mut p = price(v, from, None, PriceState::Approved);
        p.model = Model::Flat;
        p.price = Some(PriceData::Flat {
            amount: dec(amount),
        });
        p
    };
    let approved = |pair: Vec<Price>| {
        pair.into_iter()
            .map(|mut p| {
                p.state = PriceState::Approved;
                p
            })
            .collect::<Vec<_>>()
    };
    let mut chain = vec![flat(1, "2026-01-01", "10.00")];
    let outer = temporary(
        &chain,
        flat(30, "2026-10-01", "8.00"),
        date("2026-12-01"),
        Uuid::from_u128(31),
    )
    .unwrap();
    chain.extend(approved(outer));
    normalize_windows(&mut chain);
    let inner = temporary(
        &chain,
        flat(40, "2026-10-15", "5.00"),
        date("2026-11-01"),
        Uuid::from_u128(41),
    )
    .unwrap();
    assert_eq!(
        inner.len(),
        2,
        "a pair: the chain has a price in force on 11-01"
    );
    let returned = &inner[1];
    assert_eq!(returned.effective_from, date("2026-11-01"));
    assert_eq!(
        returned.price,
        Some(PriceData::Flat {
            amount: dec("8.00")
        }),
        "the outer promo's money"
    );
    assert_eq!(returned.return_of_price_id, Some(Uuid::from_u128(30)));
    assert_eq!(returned.temporary_until, None);
    assert_eq!(
        returned.effective_to,
        Some(date("2026-12-01")),
        "the outer promo's own end"
    );
    assert!(
        returned.closed_explicitly,
        "an explicit end the stored chain carries"
    );
    // D-391 and D-406 hold: the pair restores the price in force on its end, and no price of
    // the pair crosses a temporary window.
    assert!(temporary_is_current(&chain, &inner[0], &inner));
    let mut around = chain.clone();
    around.extend(inner.iter().cloned());
    for p in &inner {
        assert!(window_crossing(p, &around).is_none(), "{}", p.version_no);
    }
    // Approved and normalised: the inner return keeps its end, where the outer return starts.
    chain.extend(approved(inner));
    normalize_windows(&mut chain);
    let stored = chain.iter().find(|p| p.id == Uuid::from_u128(41)).unwrap();
    assert_eq!(
        (stored.effective_to, stored.closed_explicitly),
        (Some(date("2026-12-01")), true)
    );
    for (day, v) in [("2026-10-20", 40), ("2026-11-15", 41), ("2026-12-01", 31)] {
        assert_eq!(
            version_at(&chain, Uuid::from_u128(100), date(day), None)
                .unwrap()
                .version_no,
            v,
            "{day}"
        );
    }
}
#[test]
fn decision_7_a_common_date_moves_singles_to_it_and_a_pair_by_one_delta() {
    let back = price(1, "2026-01-01", None, PriceState::Approved);
    let mut promo = price(2, "2026-10-01", None, PriceState::Draft);
    promo.price = Some(PriceData::PerUnit { rate: dec("4") });
    let pair = temporary(&[back], promo, date("2026-10-11"), Uuid::from_u128(3)).unwrap();
    let single = price(5, "2026-11-01", None, PriceState::Draft);
    let mut selection = pair;
    selection.push(single);
    let moved = shift_selection(&selection, Some(date("2026-10-05"))).unwrap();
    assert_eq!(moved[0].effective_from, date("2026-10-05"));
    assert_eq!(moved[0].temporary_until, Some(date("2026-10-15")));
    assert_eq!(moved[0].effective_to, Some(date("2026-10-15")));
    assert_eq!(
        moved[1].effective_from,
        date("2026-10-15"),
        "the return keeps the pair's length"
    );
    assert_eq!(moved[1].effective_to, None);
    assert_eq!(moved[2].effective_from, date("2026-10-05"));
    assert_eq!(
        shift_selection(&selection, None).unwrap(),
        selection,
        "no common date moves nothing"
    );
}
#[test]
fn the_predecessor_is_the_same_chain_price_in_force_the_day_before() {
    let mut chain = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-02-01", Some("us"), PriceState::Approved),
        price(3, "2026-06-01", Some("us"), PriceState::Approved),
    ];
    normalize_windows(&mut chain);
    assert_eq!(in_force_before(&chain, &chain[2]).unwrap().version_no, 2);
    assert!(
        in_force_before(&chain, &chain[1]).is_none(),
        "the default chain is not a value's predecessor"
    );
    chain[1].effective_to = Some(date("2026-04-01"));
    chain[1].closed_explicitly = true;
    assert!(
        in_force_before(&chain, &chain[2]).is_none(),
        "an ended value price does not precede a later start"
    );
}
#[test]
fn every_refusal_code_names_its_input_field() {
    // D-427: a price request carries no `model`, so no price refusal names it; the entry create
    // names `model` itself.
    for (code, field) in [
        ("MODEL_KIND_CHARGEKIND_MISMATCH", "price"),
        ("MODEL_INVALID", "price"),
        ("CHAIN_MODEL_CHANGED", "price"),
        ("WINDOW_OVERLAP", "effective_from"),
        ("WINDOW_END_INVALID", "temporary_until"),
        ("DIM_VALUE_UNKNOWN", "dim_value"),
        ("MIN_FEE_INVALID", "min_fee"),
        ("PAIR_SPLIT", "price_ids"),
        ("AMOUNT_INVALID", "price"),
    ] {
        assert_eq!(field_of(code), field, "{code}");
    }
}
#[test]
fn a_temporary_that_ends_on_the_next_approved_start_needs_no_return() {
    // Chains LOW-2: B already ends the promo, so a return starting on B's start is refused
    // WINDOW_OVERLAP and restores nothing B does not.
    let mut chain = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-03-01", None, PriceState::Approved),
    ];
    normalize_windows(&mut chain);
    let mut promo = price(9, "2026-02-01", None, PriceState::Draft);
    promo.price = Some(PriceData::PerUnit { rate: dec("4") });
    let out = temporary(&chain, promo, date("2026-03-01"), Uuid::from_u128(77)).unwrap();
    assert_eq!(out.len(), 1, "the promo alone");
    assert!(!out[0].closed_explicitly, "normalisation ends it at B");
    assert_eq!(out[0].temporary_until, Some(date("2026-03-01")));
    assert!(out[0].paired_price_id.is_none());
    assert!(temporary_is_current(&chain, &out[0], &out));
    let mut all = chain.clone();
    let mut approved = out[0].clone();
    approved.state = PriceState::Approved;
    all.push(approved);
    normalize_windows(&mut all);
    assert_eq!(all[2].effective_to, Some(date("2026-03-01")));
}
#[test]
fn a_pair_is_stale_when_a_price_of_its_own_unit_is_in_force_at_its_end() {
    // One unit: a new default price B from 03-15 and a pair T [03-01, 04-01) whose return R
    // copies A. After apply the chain reads A → T → B → R, so R would undo B from 04-01.
    let mut a = price(1, "2026-01-01", None, PriceState::Approved);
    a.price = Some(PriceData::PerUnit { rate: dec("10") });
    let approved = vec![a];
    let mut b = price(2, "2026-03-15", None, PriceState::Draft);
    b.price = Some(PriceData::PerUnit { rate: dec("12") });
    let mut promo = price(3, "2026-03-01", None, PriceState::Draft);
    promo.price = Some(PriceData::PerUnit { rate: dec("5") });
    let pair = temporary(&approved, promo, date("2026-04-01"), Uuid::from_u128(4)).unwrap();
    assert_eq!(pair.len(), 2);
    let unit = vec![b, pair[0].clone(), pair[1].clone()];
    assert!(
        !temporary_is_current(&approved, &pair[0], &unit),
        "a return that ignores its own unit's price restores the wrong money"
    );
    // Without B in the unit the same pair is current.
    let alone = vec![pair[0].clone(), pair[1].clone()];
    assert!(temporary_is_current(&approved, &pair[0], &alone));
}
#[test]
fn d406_no_price_starts_inside_a_temporary_window_and_no_temporary_spans_a_start() {
    let mut promo = price(2, "2026-02-01", None, PriceState::Approved);
    promo.temporary_until = Some(date("2026-03-01"));
    let chain = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        promo.clone(),
    ];
    for (from, code) in [
        ("2026-01-31", None),
        ("2026-02-01", Some("PRICE_INSIDE_TEMPORARY")),
        ("2026-02-28", Some("PRICE_INSIDE_TEMPORARY")),
        ("2026-03-01", None),
    ] {
        let p = price(9, from, None, PriceState::Draft);
        assert_eq!(window_crossing(&p, &chain).map(|e| e.code), code, "{from}");
    }
    let elsewhere = price(9, "2026-02-15", Some("us"), PriceState::Draft);
    assert!(
        window_crossing(&elsewhere, &chain).is_none(),
        "another chain"
    );
    let mut nested_return = price(9, "2026-02-15", None, PriceState::Draft);
    nested_return.return_of_price_id = Some(promo.id);
    assert!(
        window_crossing(&nested_return, &chain).is_none(),
        "a nested pair's return belongs to its pair"
    );
    let starts = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-03-01", None, PriceState::Approved),
    ];
    for (from, until, code) in [
        ("2026-02-01", "2026-03-01", None),
        ("2026-02-01", "2026-03-02", Some("TEMPORARY_SPANS_A_CHANGE")),
        ("2026-03-01", "2026-03-10", None),
        ("2025-12-01", "2026-02-01", Some("TEMPORARY_SPANS_A_CHANGE")),
    ] {
        let mut t = price(9, from, None, PriceState::Draft);
        t.temporary_until = Some(date(until));
        assert_eq!(
            window_crossing(&t, &starts).map(|e| e.code),
            code,
            "{from}..{until}"
        );
    }
    assert_eq!(field_of("PRICE_INSIDE_TEMPORARY"), "effective_from");
    assert_eq!(field_of("TEMPORARY_SPANS_A_CHANGE"), "temporary_until");
}
/// D-427: every price of a chain has its entry's model, so the pair guard compares package size
/// and dated metering only. Two prices whose models differ (a shape no entry can hold any more)
/// pass the guard when their metering matches; package size and metering still refuse.
#[test]
fn d427_the_pair_guard_no_longer_compares_a_model() {
    let a = price(1, "2026-01-01", None, PriceState::Approved);
    let mut b = price(2, "2026-06-01", None, PriceState::Draft);
    b.model = Model::Graduated;
    b.price = Some(PriceData::Tiers {
        tiers: vec![crate::domain::money::Tier {
            up_to: None,
            rate: dec("1"),
        }],
    });
    let metering = SkuMetering {
        unit: Some("GB".into()),
        usage_type_ref: Some("storage".into()),
    };
    assert!(
        chain_guard(ChargeKind::Usage, &a, &metering, &b, &metering).is_ok(),
        "the model is the entry's; the guard does not compare it"
    );
    let package = |v: i32, from: &str, size: &str| {
        let mut p = price(v, from, None, PriceState::Approved);
        p.model = Model::Package;
        p.price = Some(PriceData::Package {
            package_size: dec(size),
            package_price: dec("5"),
        });
        p
    };
    assert_eq!(
        chain_guard(
            ChargeKind::Usage,
            &package(1, "2026-01-01", "1000"),
            &metering,
            &package(2, "2026-06-01", "500"),
            &metering
        )
        .unwrap_err()
        .code,
        "CHAIN_MODEL_CHANGED",
        "the package size is still guarded"
    );
}
/// D-427: a return copies the restored price's money and min fee, never a model: its model is
/// its entry's, the one its promo half already carries.
#[test]
fn d427_a_return_copies_money_not_a_model() {
    let mut back = price(1, "2026-01-01", None, PriceState::Approved);
    back.model = Model::Volume;
    back.price = Some(PriceData::Tiers {
        tiers: vec![crate::domain::money::Tier {
            up_to: None,
            rate: dec("2"),
        }],
    });
    let mut promo = price(2, "2026-10-01", None, PriceState::Draft);
    promo.model = Model::Graduated;
    promo.price = Some(PriceData::Tiers {
        tiers: vec![crate::domain::money::Tier {
            up_to: None,
            rate: dec("1"),
        }],
    });
    let pair = temporary(
        &[back.clone()],
        promo,
        date("2026-10-11"),
        Uuid::from_u128(3),
    )
    .unwrap();
    assert_eq!(pair.len(), 2);
    assert_eq!(
        pair[1].price, back.price,
        "the money is the restored price's"
    );
    assert_eq!(pair[1].model, Model::Graduated, "the model is not copied");
    // Whatever the builder did, the currency check reads money and min fee only.
    let mut unit = pair;
    unit[1].model = Model::Graduated;
    assert!(
        temporary_is_current(&[back], &unit[0], &unit),
        "the model is not compared either"
    );
}
/// D-520: a cancelled price leaves its chain, so the predecessor re-opens onto the next
/// surviving start, or to open-ended when nothing survives.
#[test]
fn cancelling_a_scheduled_price_reopens_its_predecessor() {
    let mut chain = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-03-01", None, PriceState::Cancelled),
        price(3, "2026-05-01", None, PriceState::Approved),
    ];
    normalize_windows(&mut chain);
    assert_eq!(chain[0].effective_to, Some(date("2026-05-01")));
    assert_eq!(chain[2].effective_to, None);
    let mut open = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-03-01", None, PriceState::Cancelled),
    ];
    normalize_windows(&mut open);
    assert_eq!(open[0].effective_to, None);
}
/// D-520: a cancelled price is not an overlap and is never the price in force.
#[test]
fn a_cancelled_price_is_outside_overlap_and_in_force() {
    let cancelled = price(2, "2026-03-01", None, PriceState::Cancelled);
    let draft = price(9, "2026-03-01", None, PriceState::Draft);
    let errors = rules(&draft, std::slice::from_ref(&cancelled), None);
    assert!(
        !errors.iter().any(|e| e.code == "WINDOW_OVERLAP"),
        "{errors:?}"
    );
    let mut chain = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        cancelled,
    ];
    normalize_windows(&mut chain);
    let entry = chain[0].price_book_entry_id;
    for day in ["2026-01-01", "2026-03-01", "2026-12-01"] {
        let in_force = version_at(&chain, entry, date(day), None).unwrap();
        assert_eq!(in_force.version_no, 1, "{day}");
        assert_ne!(in_force.state, PriceState::Cancelled);
    }
    assert_eq!(
        status(
            &price(2, "2026-03-01", None, PriceState::Cancelled),
            date("2026-04-01")
        ),
        "cancelled"
    );
}
/// D-521: an end on or before the current end (the next approved start) is kept; an end
/// after that current end is `END_DATE_INVALID`.
#[test]
fn ending_before_the_next_start_closes_there_and_past_it_is_invalid() {
    let mut chain = vec![
        price(1, "2026-01-01", None, PriceState::Approved),
        price(2, "2026-03-01", None, PriceState::Approved),
    ];
    normalize_windows(&mut chain);
    let ended = end_price(&chain, chain[0].id, date("2026-02-15")).unwrap();
    assert_eq!(ended.effective_to, Some(date("2026-02-15")));
    assert!(ended.closed_explicitly);
    assert_eq!(
        end_price(&chain, chain[0].id, date("2026-04-15"))
            .unwrap_err()
            .code,
        "END_DATE_INVALID"
    );
}
/// D-521 (review RF-P item 9): an explicitly ended price with no successor keeps its explicit end
/// as its current end: a second end past it is `END_DATE_INVALID`, one before it is accepted, and
/// with a successor the sooner of the two bounds it.
#[test]
fn an_explicit_end_bounds_a_second_end() {
    let mut ended = price(1, "2026-01-01", None, PriceState::Approved);
    ended.effective_to = Some(date("2026-02-25"));
    ended.closed_explicitly = true;
    let alone = vec![ended.clone()];
    assert_eq!(
        end_price(&alone, ended.id, date("2027-01-01"))
            .unwrap_err()
            .code,
        "END_DATE_INVALID"
    );
    assert_eq!(
        end_price(&alone, ended.id, date("2026-02-26"))
            .unwrap_err()
            .code,
        "END_DATE_INVALID"
    );
    let sooner = end_price(&alone, ended.id, date("2026-02-20")).unwrap();
    assert_eq!(sooner.effective_to, Some(date("2026-02-20")));
    assert_eq!(
        end_price(&alone, ended.id, date("2026-02-25"))
            .unwrap()
            .effective_to,
        Some(date("2026-02-25")),
        "the explicit end itself"
    );
    let with_next = vec![
        ended.clone(),
        price(2, "2026-02-10", None, PriceState::Approved),
    ];
    assert_eq!(
        end_price(&with_next, ended.id, date("2026-02-20"))
            .unwrap_err()
            .code,
        "END_DATE_INVALID",
        "a successor inside the explicit end bounds it sooner"
    );
}
/// D-520, D-521: a cancel or end row has no window of its own. It shows its state, and once
/// applied it is `superseded`, whatever the dates it copied: never a price in force.
#[test]
fn a_change_row_shows_its_state_and_superseded_once_applied() {
    for (state, shown) in [
        (PriceState::Draft, DisplayStatus::Draft),
        (PriceState::Pending, DisplayStatus::Pending),
        (PriceState::Rejected, DisplayStatus::Rejected),
        (PriceState::Approved, DisplayStatus::Superseded),
    ] {
        assert_eq!(change_display(state), shown, "{state:?}");
    }
}
