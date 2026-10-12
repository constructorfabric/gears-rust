#![allow(clippy::expect_used, clippy::unwrap_used)]
//! D-419…D-421 as plain data: one test per rule and per case of the phase 4 plan (Task 4.2.2).
use super::*;
use crate::domain::{
    money::PriceData,
    price::{self, Eligibility, Price, PriceState},
    price_book_entry::{ChargeKind, Model},
    test_support::{date, dec},
};
use bss_products_sdk::models::{BillingTiming, SkuContent, SkuType, SkuVersion};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const ENTRY: u128 = 100;
const OTHER_ENTRY: u128 = 200;
const ITEM: u128 = 1_000;
const OTHER_ITEM: u128 = 2_000;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
/// An approved flat price of `entry`: money is exact decimal text.
fn flat(
    entry: u128,
    n: u128,
    amount: &str,
    from: &str,
    dim: Option<&str>,
    e: Eligibility,
) -> Price {
    Price {
        id: id(n),
        price_book_entry_id: id(entry),
        version_no: i32::try_from(n % 1_000).unwrap(),
        dim_value: dim.map(str::to_owned),
        model: Model::Flat,
        price: Some(PriceData::Flat {
            amount: dec(amount),
        }),
        min_fee: None,
        eligibility: e,
        effective_from: date(from),
        effective_to: None,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        closed_explicitly: false,
        state: PriceState::Approved,
    }
}
fn all(n: u128, amount: &str, from: &str, dim: Option<&str>) -> Price {
    flat(ENTRY, n, amount, from, dim, Eligibility::All)
}
fn new(n: u128, amount: &str, from: &str, dim: Option<&str>) -> Price {
    flat(ENTRY, n, amount, from, dim, Eligibility::New)
}
/// A temporary price `promo` until `until` on the chain as it stands, built by the domain's own
/// pair builder, then approved: the promo and its return (or the promo alone).
fn with_temporary(mut chain: Vec<Price>, promo: Price, until: &str, return_id: u128) -> Vec<Price> {
    let pair = price::temporary(&chain, promo, date(until), id(return_id)).unwrap();
    chain.extend(pair);
    chain
}
fn entry_of(n: u128, mut prices: Vec<Price>, values: &[&str]) -> Entry {
    price::normalize_windows(&mut prices);
    Entry {
        id: id(n),
        charge_kind: ChargeKind::Recurring,
        period: Some("month".to_owned()),
        model: Model::Flat,
        invoice_line_override: None,
        values: values.iter().map(|v| (*v).to_owned()).collect(),
        prices,
    }
}
fn entry(prices: Vec<Price>, values: &[&str]) -> Entry {
    entry_of(ENTRY, prices, values)
}
fn item_of(n: u128, e: Option<Entry>) -> Item {
    Item {
        id: id(n),
        sku_id: id(n + 1),
        entry: e,
    }
}
fn ctx(items: Vec<Item>, keep: &[u128]) -> ResolveContext {
    ResolveContext {
        items,
        keep_for_bound: keep.iter().copied().map(id).collect::<BTreeSet<_>>(),
    }
}
fn one(e: Entry) -> ResolveContext {
    ctx(vec![item_of(ITEM, Some(e))], &[])
}
fn pin(n: u128) -> Pin {
    Pin {
        price_id: id(n),
        dim_value: None,
    }
}
fn pin_for(n: u128, value: &str) -> Pin {
    Pin {
        price_id: id(n),
        dim_value: Some(value.to_owned()),
    }
}
fn resolve(c: &ResolveContext, on: &str, pins: &[Pin]) -> Vec<ItemResolution> {
    matrix(c, date(on), pins).unwrap()
}
fn refusal(c: &ResolveContext, on: &str, pins: &[Pin]) -> &'static str {
    matrix(c, date(on), pins).unwrap_err().code
}
fn chain_of<'a>(r: &'a [ItemResolution], item: u128, dim: Option<&str>) -> &'a Chain {
    r.iter()
        .find(|i| i.item_id == id(item))
        .unwrap()
        .chains
        .iter()
        .find(|c| c.dim_value.as_deref() == dim)
        .unwrap()
}
fn binding<'a>(r: &'a [ItemResolution], dim: Option<&str>) -> &'a Binding {
    chain_of(r, ITEM, dim).binding.as_ref().unwrap()
}
/// The bound price's id, as the number the fixture gave it.
fn bound(r: &[ItemResolution], dim: Option<&str>) -> u128 {
    binding(r, dim).price.id.as_u128()
}
fn amount(r: &[ItemResolution], dim: Option<&str>) -> String {
    match &binding(r, dim).price.price {
        Some(PriceData::Flat { amount }) => amount.to_string(),
        other => panic!("not a flat price: {other:?}"),
    }
}

// ---------- D-420 rule 1: signup ----------

#[test]
fn rule_1_a_signup_binds_the_price_in_force_on_the_date() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            all(2, "12.00", "2026-10-01", None),
        ],
        &[],
    ));
    let sept = resolve(&c, "2026-09-15", &[]);
    assert_eq!(bound(&sept, None), 1);
    let oct = resolve(&c, "2026-10-05", &[]);
    assert_eq!(bound(&oct, None), 2);
    assert_eq!(binding(&oct, None).pinned_from, None);
    assert_eq!(binding(&oct, None).dim_used(), None);
}

#[test]
fn rule_1_a_value_binds_its_own_chain_else_the_default_and_dim_used_says_which() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            all(2, "9.00", "2026-09-01", Some("eu")),
        ],
        &["eu", "us"],
    ));
    let r = resolve(&c, "2026-10-05", &[]);
    assert_eq!((bound(&r, None), binding(&r, None).dim_used()), (1, None));
    assert_eq!(
        (bound(&r, Some("eu")), binding(&r, Some("eu")).dim_used()),
        (2, Some("eu"))
    );
    assert_eq!(
        (bound(&r, Some("us")), binding(&r, Some("us")).dim_used()),
        (1, None)
    );
}

#[test]
fn rule_1_nothing_in_force_is_uncovered_never_an_invented_price() {
    // A value chain with no default: the default row and the other value are uncovered; a date
    // before every price leaves every row uncovered. Uncovered is an answer, never a refusal.
    let c = one(entry(
        vec![all(2, "9.00", "2026-09-01", Some("eu"))],
        &["eu", "us"],
    ));
    let r = resolve(&c, "2026-10-05", &[]);
    assert!(chain_of(&r, ITEM, None).uncovered());
    assert!(chain_of(&r, ITEM, None).binding.is_none());
    assert!(!chain_of(&r, ITEM, Some("eu")).uncovered());
    assert!(chain_of(&r, ITEM, Some("us")).uncovered());
    let early = resolve(&c, "2026-08-31", &[]);
    assert!(early[0].chains.iter().all(Chain::uncovered));
}

#[test]
fn the_matrix_is_the_default_chain_then_every_registered_value_in_registry_order() {
    let c = one(entry(
        vec![all(1, "10.00", "2026-09-01", None)],
        &["us", "eu", "apac"],
    ));
    let r = resolve(&c, "2026-10-05", &[]);
    let dims: Vec<Option<&str>> = r[0].chains.iter().map(|c| c.dim_value.as_deref()).collect();
    assert_eq!(dims, [None, Some("us"), Some("eu"), Some("apac")]);
    assert_eq!(r[0].price_book_entry_id, Some(id(ENTRY)));
    assert_eq!(r[0].charge_kind, Some(ChargeKind::Recurring));
    assert_eq!(r[0].period.as_deref(), Some("month"));
    assert_eq!(
        r[0].model,
        Some(Model::Flat),
        "D-427: the entry's model, on the item"
    );
}

// ---------- D-420 rule 2: the pinned walk; spec §7.1 ----------

/// Spec §7.1, verbatim: pinned €10 → an `all` price €12 → a `new` price €15: the renewal binds
/// €12, a signup binds €15.
#[test]
fn spec_7_1_pinned_10_then_all_12_then_new_15_renews_at_12_and_signs_up_at_15() {
    let c = one(entry(
        vec![
            all(10, "10.00", "2026-09-01", None),
            all(12, "12.00", "2026-10-01", None),
            new(15, "15.00", "2026-11-01", None),
        ],
        &[],
    ));
    let renewal = resolve(&c, "2026-11-05", &[pin(10)]);
    assert_eq!(amount(&renewal, None), "12.00");
    assert_eq!(binding(&renewal, None).pinned_from, Some(id(10)));
    let signup = resolve(&c, "2026-11-05", &[]);
    assert_eq!(amount(&signup, None), "15.00");
    assert_eq!(binding(&signup, None).pinned_from, None);
}

#[test]
fn rule_2_the_walk_takes_every_all_successor_in_force_by_the_date() {
    let c = one(entry(
        vec![
            all(10, "10.00", "2026-09-01", None),
            all(11, "11.00", "2026-10-01", None),
            all(12, "12.00", "2026-11-01", None),
        ],
        &[],
    ));
    assert_eq!(bound(&resolve(&c, "2026-11-05", &[pin(10)]), None), 12);
    assert_eq!(bound(&resolve(&c, "2026-11-05", &[pin(11)]), None), 12);
}

#[test]
fn rule_2_the_walk_is_bounded_by_the_date() {
    let c = one(entry(
        vec![
            all(10, "10.00", "2026-09-01", None),
            all(12, "12.00", "2026-12-01", None),
        ],
        &[],
    ));
    let r = resolve(&c, "2026-11-05", &[pin(10)]);
    assert_eq!(bound(&r, None), 10);
    assert_eq!(bound(&resolve(&c, "2026-12-01", &[pin(10)]), None), 12);
    // Phase 4 review G-1: where the bound decides the binding. A value pinned to the default, its
    // own `new` price in force (rule 4 does not move it), a default successor after the date:
    // walked past the date, the pin would reach a price not yet in force, and rule 3 would bind
    // the value's own `new` price — a renewal bound to a price for new subscribers.
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            new(20, "9.00", "2026-10-01", Some("us")),
            all(2, "12.00", "2027-01-01", None),
        ],
        &["us"],
    ));
    let r = resolve(&c, "2026-10-05", &[pin_for(1, "us")]);
    assert_eq!(bound(&r, Some("us")), 1);
    assert_eq!(binding(&r, Some("us")).dim_used(), None);
    assert_eq!(binding(&r, Some("us")).pinned_from, Some(id(1)));
    // From its start, the default successor is walked to.
    let later = resolve(&c, "2027-01-05", &[pin_for(1, "us")]);
    assert_eq!(bound(&later, Some("us")), 2);
}

#[test]
fn rule_2_the_walk_stops_before_the_first_new_successor_even_with_an_all_after_it() {
    let c = one(entry(
        vec![
            all(10, "10.00", "2026-09-01", None),
            new(15, "15.00", "2026-10-01", None),
            all(18, "18.00", "2026-11-01", None),
        ],
        &[],
    ));
    assert_eq!(bound(&resolve(&c, "2026-11-05", &[pin(10)]), None), 10);
    // A pin already past the `new` price walks on through `all` successors.
    assert_eq!(bound(&resolve(&c, "2026-11-05", &[pin(15)]), None), 18);
}

#[test]
fn rule_2_a_value_chain_pin_walks_its_own_chain_and_names_the_pin() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            all(20, "20.00", "2026-09-01", Some("us")),
            all(21, "21.00", "2026-10-01", Some("us")),
        ],
        &["us"],
    ));
    let r = resolve(&c, "2026-10-05", &[pin(20)]);
    assert_eq!(bound(&r, Some("us")), 21);
    assert_eq!(binding(&r, Some("us")).dim_used(), Some("us"));
    assert_eq!(binding(&r, Some("us")).pinned_from, Some(id(20)));
    // The pin is the value's: the default row is still a signup.
    assert_eq!(binding(&r, None).pinned_from, None);
}

// ---------- D-420 rule 3: a binding is always in force ----------

#[test]
fn rule_3_a_pin_on_an_ended_temporary_price_binds_the_price_in_force() {
    // A `new` promo pair: the return inherits the promo's eligibility, so the walk from the promo
    // stops before it; the promo's window has ended, so the binding is the price in force.
    let prices = with_temporary(
        vec![all(10, "10.00", "2026-09-01", None)],
        new(30, "7.00", "2026-10-10", None),
        "2026-11-10",
        31,
    );
    let c = one(entry(prices, &[]));
    let during = resolve(&c, "2026-10-20", &[pin(30)]);
    assert_eq!(bound(&during, None), 30);
    let after = resolve(&c, "2026-11-15", &[pin(30)]);
    assert_eq!(bound(&after, None), 31);
    assert_eq!(amount(&after, None), "10.00");
    assert_eq!(binding(&after, None).pinned_from, Some(id(30)));
}

#[test]
fn rule_3_a_pin_on_a_closed_value_chain_falls_back_to_the_default() {
    // A temporary price on a value with no chain of its own is one closed price, no return.
    let prices = with_temporary(
        vec![all(1, "10.00", "2026-09-01", None)],
        all(40, "8.00", "2026-10-01", Some("us")),
        "2026-11-01",
        41,
    );
    assert_eq!(prices.len(), 2, "one closed price, no return");
    let c = one(entry(prices, &["us"]));
    let inside = resolve(&c, "2026-10-15", &[pin(40)]);
    assert_eq!(bound(&inside, Some("us")), 40);
    let after = resolve(&c, "2026-11-05", &[pin(40)]);
    assert_eq!(bound(&after, Some("us")), 1);
    assert_eq!(binding(&after, Some("us")).dim_used(), None);
    assert_eq!(binding(&after, Some("us")).pinned_from, Some(id(40)));
}

#[test]
fn rule_3_an_ended_chain_with_nothing_else_in_force_is_uncovered() {
    let prices = with_temporary(
        vec![],
        all(40, "8.00", "2026-10-01", Some("us")),
        "2026-11-01",
        41,
    );
    let c = one(entry(prices, &["us"]));
    let r = resolve(&c, "2026-11-05", &[pin(40)]);
    assert!(chain_of(&r, ITEM, Some("us")).uncovered());
}

#[test]
fn rule_3_a_new_promo_pair_is_walked_not_skipped_so_it_stops_the_walk() {
    // The owner, 2026-09-26: no promotion-specific rule. A pin before a `new` pair stops at the
    // pair, and a later `all` price is not reached; the pinned price stays bound (keep_for_bound).
    let mut prices = with_temporary(
        vec![all(10, "10.00", "2026-09-01", None)],
        new(30, "7.00", "2026-10-10", None),
        "2026-11-10",
        31,
    );
    prices.push(all(50, "11.00", "2026-12-01", None));
    let c = ctx(vec![item_of(ITEM, Some(entry(prices, &[])))], &[10]);
    for on in ["2026-10-20", "2026-11-15", "2026-12-05"] {
        let r = resolve(&c, on, &[pin(10)]);
        assert_eq!(bound(&r, None), 10, "on {on}");
        assert!(binding(&r, None).keep_for_bound);
    }
}

#[test]
fn rule_3_an_all_promo_pair_is_walked_through_to_its_return() {
    let prices = with_temporary(
        vec![all(10, "10.00", "2026-09-01", None)],
        all(30, "7.00", "2026-10-10", None),
        "2026-11-10",
        31,
    );
    let c = one(entry(prices, &[]));
    assert_eq!(bound(&resolve(&c, "2026-10-20", &[pin(10)]), None), 30);
    assert_eq!(bound(&resolve(&c, "2026-11-15", &[pin(10)]), None), 31);
    assert_eq!(bound(&resolve(&c, "2026-11-15", &[pin(30)]), None), 31);
}

#[test]
fn rule_3_a_pin_not_yet_in_force_binds_the_price_in_force() {
    let c = one(entry(
        vec![
            all(10, "10.00", "2026-09-01", None),
            all(12, "12.00", "2026-12-01", None),
        ],
        &[],
    ));
    let r = resolve(&c, "2026-11-05", &[pin(12)]);
    assert_eq!(bound(&r, None), 10);
    assert_eq!(binding(&r, None).pinned_from, Some(id(12)));
}

/// Phase 4 review C-1: a pinned price ends at its OWN end, never at the stored end that
/// normalisation gives it. An inner `new` pair inside an outer `all` pair (both built by the
/// domain's own pair builder) cuts the outer promo's stored window at the inner start; that start
/// is a successor the walk did not take, so the pin keeps the outer promo until its
/// `temporary_until`, then binds the outer return.
#[test]
fn rule_3_a_pinned_outer_promo_binds_to_its_own_end_through_a_nested_new_pair() {
    let mut chain = with_temporary(
        vec![all(10, "10.00", "2026-01-01", None)],
        all(30, "8.00", "2026-10-01", None),
        "2026-12-01",
        31,
    );
    price::normalize_windows(&mut chain);
    let chain = with_temporary(chain, new(40, "5.00", "2026-10-15", None), "2026-11-01", 41);
    // Apply marks the outer promo keep_for_bound: it is the price in force before the `new` one.
    let c = ctx(vec![item_of(ITEM, Some(entry(chain, &[])))], &[30]);
    let outer = c.items[0]
        .entry
        .as_ref()
        .unwrap()
        .prices
        .iter()
        .find(|p| p.id == id(30))
        .unwrap();
    assert_eq!(
        (outer.effective_to, outer.temporary_until),
        (Some(date("2026-10-15")), Some(date("2026-12-01"))),
        "the stored window ends at the inner start; the promo's own end is later"
    );
    for on in ["2026-10-20", "2026-11-05"] {
        let r = resolve(&c, on, &[pin(30)]);
        assert_eq!(bound(&r, None), 30, "on {on}");
        assert_eq!(amount(&r, None), "8.00", "on {on}");
        assert_eq!(binding(&r, None).pinned_from, Some(id(30)), "on {on}");
        assert!(binding(&r, None).keep_for_bound, "on {on}");
    }
    let after = resolve(&c, "2026-12-05", &[pin(30)]);
    assert_eq!(bound(&after, None), 31, "the outer return");
    assert_eq!(amount(&after, None), "10.00");
    assert_eq!(binding(&after, None).pinned_from, Some(id(30)));
    // A signup inside the inner window takes the new-customers promo.
    assert_eq!(bound(&resolve(&c, "2026-10-20", &[]), None), 40);
}

/// The review's chain for M1: an outer `all` pair (8.00 from 10-01 until 12-01, its return 10.00)
/// and, nested in it, an inner pair built by `inner` (`all` or `new`: 5.00 from 10-15 until
/// 11-01, its return the outer promo's 8.00) — both built by the domain's own pair builder.
fn nested_pairs(inner: fn(u128, &str, &str, Option<&str>) -> Price) -> ResolveContext {
    let mut chain = with_temporary(
        vec![all(10, "10.00", "2026-01-01", None)],
        all(30, "8.00", "2026-10-01", None),
        "2026-12-01",
        31,
    );
    price::normalize_windows(&mut chain);
    let chain = with_temporary(
        chain,
        inner(40, "5.00", "2026-10-15", None),
        "2026-11-01",
        41,
    );
    one(entry(chain, &[]))
}

/// Phase 4 second review M1: a nested pair's return restores the outer promo's money only until
/// the outer promo's own end. A pin on the outer promo renewed on 11-05 walks the inner `all`
/// pair to its return, which ends on 12-01 (`ends_on`), so the consumer slices there; after it,
/// the pin binds the outer return — and so does the inner return's pin resolved again on its end.
#[test]
fn a_pin_on_an_outer_promo_binds_the_nested_return_until_the_outer_end() {
    let c = nested_pairs(all);
    let renewed = resolve(&c, "2026-11-05", &[pin(30)]);
    assert_eq!(bound(&renewed, None), 41, "the walk takes the inner pair");
    assert_eq!(amount(&renewed, None), "8.00");
    assert_eq!(binding(&renewed, None).pinned_from, Some(id(30)));
    assert_eq!(
        binding(&renewed, None).ends_on(),
        Some(date("2026-12-01")),
        "the outer promo's end, never an open end"
    );
    let after = resolve(&c, "2026-12-05", &[pin(30)]);
    assert_eq!(bound(&after, None), 31, "the outer return");
    assert_eq!(amount(&after, None), "10.00");
    assert_eq!(binding(&after, None).ends_on(), None);
    let again = resolve(&c, "2026-12-01", &[pin(41)]);
    assert_eq!(
        (bound(&again, None), amount(&again, None)),
        (31, "10.00".to_owned()),
        "resolved again with its pin on its ends_on"
    );
}

/// Phase 4 second review M1, the same chain with an inner `new` pair: a signup on 11-05 binds the
/// inner return (a `new` return, as its promo) until the outer promo's end, and so does the inner
/// promo's holder resolved again at its own end (rule 3 falls to rule 1); after it, the outer
/// return.
#[test]
fn a_signup_on_a_nested_new_pairs_return_ends_where_the_outer_promo_ends() {
    let c = nested_pairs(new);
    let signup = resolve(&c, "2026-11-05", &[]);
    assert_eq!(bound(&signup, None), 41);
    assert_eq!(amount(&signup, None), "8.00");
    assert_eq!(binding(&signup, None).ends_on(), Some(date("2026-12-01")));
    let holder = resolve(&c, "2026-11-01", &[pin(40)]);
    assert_eq!(
        (bound(&holder, None), binding(&holder, None).ends_on()),
        (41, Some(date("2026-12-01")))
    );
    let later = resolve(&c, "2026-12-05", &[]);
    assert_eq!(
        (bound(&later, None), amount(&later, None)),
        (31, "10.00".to_owned())
    );
}

/// D-425: the binding says where it ends for its holder — a temporary price's `temporary_until`,
/// an explicitly closed price's end — and names no end for a price whose stored window only a
/// successor's start closes.
#[test]
fn a_binding_ends_on_its_own_end_never_on_a_successors_start() {
    // 12 is kept for its pins: its stored window closes at the `new` 15's start, not its own end.
    let c = ctx(
        vec![item_of(
            ITEM,
            Some(entry(
                vec![
                    all(10, "10.00", "2026-09-01", None),
                    all(12, "12.00", "2026-10-01", None),
                    new(15, "15.00", "2026-11-01", None),
                ],
                &[],
            )),
        )],
        &[12],
    );
    let kept = resolve(&c, "2026-11-05", &[pin(10)]);
    assert_eq!(bound(&kept, None), 12);
    assert_eq!(
        binding(&kept, None).price.effective_to,
        Some(date("2026-11-01"))
    );
    assert_eq!(binding(&kept, None).ends_on(), None);
    // An `all` promo pair: the promo ends at its `temporary_until`; its return has no end.
    let pair = one(entry(
        with_temporary(
            vec![all(10, "10.00", "2026-09-01", None)],
            all(30, "7.00", "2026-10-10", None),
            "2026-11-10",
            31,
        ),
        &[],
    ));
    let promo = resolve(&pair, "2026-10-20", &[pin(10)]);
    assert_eq!(bound(&promo, None), 30);
    assert_eq!(binding(&promo, None).ends_on(), Some(date("2026-11-10")));
    let back = resolve(&pair, "2026-11-15", &[pin(10)]);
    assert_eq!(
        (bound(&back, None), binding(&back, None).ends_on()),
        (31, None)
    );
    // A value price closed with no return, then a promo inside it: the promo's return copies the
    // explicit close (no `temporary_until`), and ends there.
    let mut closed_chain = with_temporary(
        vec![all(1, "10.00", "2026-09-01", None)],
        all(40, "8.00", "2026-10-01", Some("us")),
        "2026-12-01",
        41,
    );
    price::normalize_windows(&mut closed_chain);
    let nested = with_temporary(
        closed_chain,
        all(50, "6.00", "2026-10-10", Some("us")),
        "2026-11-01",
        51,
    );
    let returned = nested.iter().find(|p| p.id == id(51)).unwrap();
    assert!(returned.closed_explicitly && returned.temporary_until.is_none());
    let c = one(entry(nested, &["us"]));
    let r = resolve(&c, "2026-11-15", &[pin(50)]);
    assert_eq!(bound(&r, Some("us")), 51);
    assert_eq!(binding(&r, Some("us")).ends_on(), Some(date("2026-12-01")));
    // The default chain's open price has no end.
    assert_eq!(binding(&r, None).ends_on(), None);
}

// ---------- D-420 rule 4: a default-chain pin moves to the value's own later `all` price ----------

#[test]
fn rule_4_a_default_pin_moves_to_the_values_later_all_price() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            all(20, "9.00", "2026-10-01", Some("us")),
        ],
        &["us"],
    ));
    let r = resolve(&c, "2026-11-05", &[pin_for(1, "us")]);
    assert_eq!(bound(&r, Some("us")), 20);
    assert_eq!(binding(&r, Some("us")).dim_used(), Some("us"));
    assert_eq!(binding(&r, Some("us")).pinned_from, Some(id(1)));
}

#[test]
fn rule_4_a_new_own_price_does_not_move_a_default_pin() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            new(20, "9.00", "2026-10-01", Some("us")),
        ],
        &["us"],
    ));
    let r = resolve(&c, "2026-11-05", &[pin_for(1, "us")]);
    assert_eq!(bound(&r, Some("us")), 1);
    assert_eq!(binding(&r, Some("us")).dim_used(), None);
}

#[test]
fn rule_4_an_own_price_that_started_before_the_pin_does_not_move_it() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-08-01", None),
            all(2, "11.00", "2026-10-01", None),
            all(20, "9.00", "2026-09-01", Some("us")),
        ],
        &["us"],
    ));
    let r = resolve(&c, "2026-11-05", &[pin_for(2, "us")]);
    assert_eq!(bound(&r, Some("us")), 2);
}

#[test]
fn a_default_pin_walks_the_default_chain_for_its_value() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            all(2, "12.00", "2026-10-01", None),
        ],
        &["us"],
    ));
    let r = resolve(&c, "2026-10-05", &[pin_for(1, "us")]);
    assert_eq!(bound(&r, Some("us")), 2);
    assert_eq!(binding(&r, Some("us")).pinned_from, Some(id(1)));
    assert_eq!(binding(&r, None).pinned_from, None);
}

// ---------- D-419: the pins ----------

#[test]
fn pin_foreign_a_pin_on_an_entry_no_item_of_the_revision_names() {
    let mine = entry(vec![all(1, "10.00", "2026-09-01", None)], &[]);
    let theirs = entry_of(
        OTHER_ENTRY,
        vec![flat(
            OTHER_ENTRY,
            201,
            "5.00",
            "2026-09-01",
            None,
            Eligibility::All,
        )],
        &[],
    );
    let c = one(mine);
    assert_eq!(refusal(&c, "2026-10-05", &[pin(201)]), "PIN_FOREIGN");
    assert_eq!(
        refusal(&c, "2026-10-05", &[pin(1), pin(999)]),
        "PIN_FOREIGN"
    );
    // Named by another item of the revision, the same price is a valid pin of that item.
    let both = ctx(
        vec![
            item_of(
                ITEM,
                Some(entry(vec![all(1, "10.00", "2026-09-01", None)], &[])),
            ),
            item_of(OTHER_ITEM, Some(theirs)),
        ],
        &[],
    );
    let r = resolve(&both, "2026-10-05", &[pin(201)]);
    assert_eq!(
        chain_of(&r, OTHER_ITEM, None)
            .binding
            .as_ref()
            .unwrap()
            .pinned_from,
        Some(id(201))
    );
    assert_eq!(binding(&r, None).pinned_from, None);
}

#[test]
fn pin_foreign_a_pin_on_a_draft_pending_or_rejected_price() {
    for state in [PriceState::Draft, PriceState::Pending, PriceState::Rejected] {
        let mut draft = all(12, "12.00", "2026-12-01", None);
        draft.state = state;
        let c = one(entry(
            vec![all(10, "10.00", "2026-09-01", None), draft],
            &[],
        ));
        assert_eq!(
            refusal(&c, "2026-10-05", &[pin(12)]),
            "PIN_FOREIGN",
            "{state:?}"
        );
    }
}

#[test]
fn pin_foreign_a_value_pin_on_a_price_that_is_not_a_default_chain_price() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            all(20, "9.00", "2026-09-01", Some("us")),
        ],
        &["us", "eu"],
    ));
    assert_eq!(
        refusal(&c, "2026-10-05", &[pin_for(20, "us")]),
        "PIN_FOREIGN"
    );
    assert_eq!(
        refusal(&c, "2026-10-05", &[pin_for(20, "eu")]),
        "PIN_FOREIGN"
    );
}

#[test]
fn pin_duplicate_two_pins_for_one_item_and_value() {
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            all(20, "9.00", "2026-10-01", Some("us")),
        ],
        &["us"],
    ));
    assert_eq!(
        refusal(&c, "2026-10-05", &[pin(1), pin(1)]),
        "PIN_DUPLICATE"
    );
    assert_eq!(
        refusal(&c, "2026-10-05", &[pin_for(1, "us"), pin(20)]),
        "PIN_DUPLICATE"
    );
    // One pin for the default row and one for the value are two different (item, value) pairs.
    resolve(&c, "2026-10-05", &[pin(1), pin_for(1, "us")]);
}

#[test]
fn pins_too_many_is_more_than_a_thousand_and_judged_first() {
    let c = one(entry(vec![all(1, "10.00", "2026-09-01", None)], &[]));
    let foreign: Vec<Pin> = (0..=u128::try_from(MAX_PINS).unwrap())
        .map(|n| pin(5_000 + n))
        .collect();
    assert_eq!(foreign.len(), 1_001);
    assert_eq!(refusal(&c, "2026-10-05", &foreign), "PINS_TOO_MANY");
    assert_eq!(
        refusal(&c, "2026-10-05", &foreign[..MAX_PINS]),
        "PIN_FOREIGN"
    );
}

#[test]
fn a_removed_value_still_resolves_for_its_pin() {
    // "us" was removed from the registry (it had no prices of its own); a subscription bound to
    // the default for "us" still resolves, and the value is not checked against the registry.
    let c = one(entry(
        vec![
            all(1, "10.00", "2026-09-01", None),
            all(2, "12.00", "2026-10-01", None),
        ],
        &["eu"],
    ));
    let r = resolve(&c, "2026-10-05", &[pin_for(1, "us")]);
    let dims: Vec<Option<&str>> = r[0].chains.iter().map(|c| c.dim_value.as_deref()).collect();
    assert_eq!(dims, [None, Some("eu"), Some("us")]);
    assert_eq!(bound(&r, Some("us")), 2);
    assert_eq!(binding(&r, Some("us")).pinned_from, Some(id(1)));
    // Without the pin the removed value is not in the matrix.
    let signup = resolve(&c, "2026-10-05", &[]);
    assert_eq!(signup[0].chains.len(), 2);
}

#[test]
fn a_keep_for_bound_binding_reports_it() {
    let prices = vec![
        all(10, "10.00", "2026-09-01", None),
        all(12, "12.00", "2026-10-01", None),
        new(15, "15.00", "2026-11-01", None),
    ];
    let c = ctx(vec![item_of(ITEM, Some(entry(prices, &[])))], &[12]);
    let renewal = resolve(&c, "2026-11-05", &[pin(10)]);
    assert_eq!(bound(&renewal, None), 12);
    assert!(binding(&renewal, None).keep_for_bound);
    // Its window closed at the `new` price's start; it binds all the same.
    assert_eq!(
        binding(&renewal, None).price.effective_to,
        Some(date("2026-11-01"))
    );
    let signup = resolve(&c, "2026-11-05", &[]);
    assert!(!binding(&signup, None).keep_for_bound);
}

#[test]
fn an_item_without_an_entry_has_no_chains() {
    let c = ctx(
        vec![
            item_of(OTHER_ITEM, None),
            item_of(
                ITEM,
                Some(entry(vec![all(1, "10.00", "2026-09-01", None)], &[])),
            ),
        ],
        &[],
    );
    let r = resolve(&c, "2026-10-05", &[]);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].item_id, id(OTHER_ITEM));
    assert!(r[0].chains.is_empty());
    assert_eq!(r[0].price_book_entry_id, None);
    assert_eq!(r[0].charge_kind, None);
    assert_eq!(r[0].model, None, "D-427: no entry, no model");
    assert_eq!(r[1].item_id, id(ITEM));
}

/// D-467: an item is its SKU and its entry, and resolves as such.
#[test]
fn the_item_carries_its_sku_and_its_entry() {
    let it = item_of(
        ITEM,
        Some(entry(vec![all(1, "10.00", "2026-09-01", None)], &[])),
    );
    let r = resolve(&ctx(vec![it], &[]), "2026-10-05", &[]);
    assert_eq!(r[0].sku_id, id(ITEM + 1));
    assert_eq!(r[0].price_book_entry_id, Some(id(ENTRY)));
    assert_eq!(r[0].charge_kind, Some(ChargeKind::Recurring));
    assert_eq!(r[0].period.as_deref(), Some("month"));
    assert_eq!(r[0].model, Some(Model::Flat));
}

// ---------- D-421: resolved invoice inputs with their source ----------

fn sku_version(
    line: Option<&str>,
    gl: Option<&str>,
    tax: Option<&str>,
    timing: Option<BillingTiming>,
) -> SkuVersion {
    SkuVersion {
        sku_id: id(ITEM + 1),
        published_version: 3,
        effective_from: date("2026-09-01"),
        content: SkuContent {
            code: "WP-PRO".to_owned(),
            name: "WordPress Pro".to_owned(),
            r#type: SkuType::Recurring,
            category_id: Some(id(7)),
            description: String::new(),
            sellable: true,
            gl_code: gl.map(str::to_owned),
            tax_category: tax.map(str::to_owned),
            invoice_line_template: line.map(str::to_owned),
            billing_timing: timing,
            usage_type_ref: None,
            unit: None,
        },
        created_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}
fn tenant(timing: &str) -> TenantDefaults {
    TenantDefaults {
        default_timing: timing.to_owned(),
        default_gl: Some("4000".to_owned()),
        default_tax_category: Some("std".to_owned()),
        invoice_line_templates: BTreeMap::from([
            ("recurring".to_owned(), "{sku} - {period}".to_owned()),
            ("usage".to_owned(), "{sku}, {unit}".to_owned()),
        ]),
    }
}
fn resolved(value: Option<&str>, source: Option<Source>) -> Resolved {
    Resolved {
        value: value.map(str::to_owned),
        source,
    }
}

#[test]
fn invoice_line_the_entry_override_wins_over_the_sku_and_the_tenant() {
    let v = sku_version(Some("{sku} hosting"), None, None, None);
    let got = invoice_inputs(
        Some("Pro {period}"),
        Some(&v),
        &tenant("advance"),
        Some(ChargeKind::Recurring),
    );
    assert_eq!(
        got.invoice_line_template,
        resolved(Some("Pro {period}"), Some(Source::Entry))
    );
}

#[test]
fn invoice_line_the_sku_version_template_wins_over_the_tenant() {
    let v = sku_version(Some("{sku} hosting"), None, None, None);
    let got = invoice_inputs(
        None,
        Some(&v),
        &tenant("advance"),
        Some(ChargeKind::Recurring),
    );
    assert_eq!(
        got.invoice_line_template,
        resolved(Some("{sku} hosting"), Some(Source::Sku))
    );
    // A blank override is no override.
    let blank = invoice_inputs(
        Some("  "),
        Some(&v),
        &tenant("advance"),
        Some(ChargeKind::Recurring),
    );
    assert_eq!(
        blank.invoice_line_template,
        resolved(Some("{sku} hosting"), Some(Source::Sku))
    );
}

#[test]
fn invoice_line_the_tenant_template_for_the_charge_kind_is_the_last_fallback() {
    let v = sku_version(None, None, None, None);
    let rec = invoice_inputs(
        None,
        Some(&v),
        &tenant("advance"),
        Some(ChargeKind::Recurring),
    );
    assert_eq!(
        rec.invoice_line_template,
        resolved(Some("{sku} - {period}"), Some(Source::Tenant))
    );
    let usage = invoice_inputs(None, Some(&v), &tenant("advance"), Some(ChargeKind::Usage));
    assert_eq!(
        usage.invoice_line_template,
        resolved(Some("{sku}, {unit}"), Some(Source::Tenant))
    );
    // No template for one-time charges anywhere: null, and no source is invented.
    let one_time = invoice_inputs(
        None,
        Some(&v),
        &tenant("advance"),
        Some(ChargeKind::OneTime),
    );
    assert_eq!(one_time.invoice_line_template, resolved(None, None));
}

#[test]
fn invoice_line_an_item_without_an_entry_takes_the_template_of_its_sku_type() {
    let v = sku_version(None, None, None, None);
    let got = invoice_inputs(None, Some(&v), &tenant("advance"), None);
    assert_eq!(
        got.invoice_line_template,
        resolved(Some("{sku} - {period}"), Some(Source::Tenant))
    );
    let nothing = invoice_inputs(None, None, &tenant("advance"), None);
    assert_eq!(nothing.invoice_line_template, resolved(None, None));
}

#[test]
fn gl_code_the_sku_then_the_tenant_then_null() {
    let v = sku_version(None, Some("4100-HOST"), None, None);
    let t = tenant("advance");
    assert_eq!(
        invoice_inputs(None, Some(&v), &t, Some(ChargeKind::Recurring)).gl_code,
        resolved(Some("4100-HOST"), Some(Source::Sku))
    );
    let bare = sku_version(None, None, None, None);
    assert_eq!(
        invoice_inputs(None, Some(&bare), &t, Some(ChargeKind::Recurring)).gl_code,
        resolved(Some("4000"), Some(Source::Tenant))
    );
    let none = TenantDefaults {
        default_gl: None,
        ..t
    };
    assert_eq!(
        invoice_inputs(None, Some(&bare), &none, Some(ChargeKind::Recurring)).gl_code,
        resolved(None, None)
    );
}

#[test]
fn tax_category_the_sku_then_the_tenant_then_null() {
    let v = sku_version(None, None, Some("reduced"), None);
    let t = tenant("advance");
    assert_eq!(
        invoice_inputs(None, Some(&v), &t, Some(ChargeKind::Recurring)).tax_category,
        resolved(Some("reduced"), Some(Source::Sku))
    );
    let bare = sku_version(None, None, None, None);
    assert_eq!(
        invoice_inputs(None, Some(&bare), &t, Some(ChargeKind::Recurring)).tax_category,
        resolved(Some("std"), Some(Source::Tenant))
    );
    let none = TenantDefaults {
        default_tax_category: None,
        ..t
    };
    assert_eq!(
        invoice_inputs(None, Some(&bare), &none, Some(ChargeKind::Recurring)).tax_category,
        resolved(None, None)
    );
}

/// PRD AC #13: given arrears as the tenant default and advance on the SKU, advance wins.
#[test]
fn ac_13_advance_on_the_sku_beats_arrears_as_the_tenant_default() {
    let t = tenant("arrears");
    let v = sku_version(None, None, None, Some(BillingTiming::Advance));
    assert_eq!(
        invoice_inputs(None, Some(&v), &t, Some(ChargeKind::Recurring)).billing_timing,
        resolved(Some("advance"), Some(Source::Sku))
    );
    let bare = sku_version(None, None, None, None);
    assert_eq!(
        invoice_inputs(None, Some(&bare), &t, Some(ChargeKind::Recurring)).billing_timing,
        resolved(Some("arrears"), Some(Source::Tenant))
    );
    let arrears = sku_version(None, None, None, Some(BillingTiming::Arrears));
    assert_eq!(
        invoice_inputs(
            None,
            Some(&arrears),
            &tenant("advance"),
            Some(ChargeKind::Recurring)
        )
        .billing_timing,
        resolved(Some("arrears"), Some(Source::Sku))
    );
}

#[test]
fn without_a_sku_version_every_input_falls_to_the_entry_or_the_tenant() {
    let got = invoice_inputs(
        Some("Pro {period}"),
        None,
        &tenant("arrears"),
        Some(ChargeKind::Recurring),
    );
    assert_eq!(
        got.invoice_line_template,
        resolved(Some("Pro {period}"), Some(Source::Entry))
    );
    assert_eq!(got.gl_code, resolved(Some("4000"), Some(Source::Tenant)));
    assert_eq!(
        got.tax_category,
        resolved(Some("std"), Some(Source::Tenant))
    );
    assert_eq!(
        got.billing_timing,
        resolved(Some("arrears"), Some(Source::Tenant))
    );
}

#[test]
fn a_blank_sku_descriptor_falls_through_to_the_tenant() {
    let v = sku_version(Some(" "), Some(""), Some("  "), None);
    let got = invoice_inputs(
        None,
        Some(&v),
        &tenant("advance"),
        Some(ChargeKind::Recurring),
    );
    assert_eq!(got.invoice_line_template.source, Some(Source::Tenant));
    assert_eq!(got.gl_code, resolved(Some("4000"), Some(Source::Tenant)));
    assert_eq!(
        got.tax_category,
        resolved(Some("std"), Some(Source::Tenant))
    );
}
