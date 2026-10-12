#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::{BTreeMap, HashMap};

use bss_approvals_sdk::{InboxUnit, Order, SortKey};

use super::merge::{self, SourceAnswer};
use crate::test_support::{self, page_after};

fn ids_of(units: &[InboxUnit]) -> Vec<u128> {
    units.iter().map(|unit| unit.id.as_u128()).collect()
}

fn page(
    names: &[&str],
    catalog: &HashMap<&str, Vec<InboxUnit>>,
    order: Order,
    limit: u32,
    keys: &BTreeMap<String, Option<SortKey>>,
) -> merge::MergedPage {
    let pages: Vec<SourceAnswer> = names
        .iter()
        .map(|name| {
            let after = keys.get(*name).copied().flatten();
            let (units, has_more) = page_after(
                catalog.get(name).map_or(&[], Vec::as_slice),
                order,
                limit,
                after,
            );
            SourceAnswer {
                source: (*name).to_owned(),
                units,
                has_more,
            }
        })
        .collect();
    merge::merge(order, limit, keys, pages)
}

fn walk(
    names: &[&str],
    catalog: &HashMap<&str, Vec<InboxUnit>>,
    order: Order,
    limit: u32,
) -> Vec<u128> {
    let mut keys = BTreeMap::new();
    let mut out = Vec::new();
    for _ in 0..100 {
        let merged = page(names, catalog, order, limit, &keys);
        assert!(
            !merged.units.is_empty() || !merged.has_more,
            "a cursor that takes nothing must not continue"
        );
        if merged.units.is_empty() {
            break;
        }
        out.extend(ids_of(&merged.units));
        if !merged.has_more {
            break;
        }
        keys = merged.keys;
    }
    out
}

fn catalog<'a>(rows: &'a [(&'a str, i64, u128)]) -> HashMap<&'a str, Vec<InboxUnit>> {
    let mut out: HashMap<&str, Vec<InboxUnit>> = HashMap::new();
    for (name, secs, id) in rows {
        out.entry(*name)
            .or_default()
            .push(test_support::unit(name, *secs, *id));
    }
    out
}

#[test]
fn interleaved_both_orders_for_one_two_and_three_sources() {
    let rows = [
        ("a", 1, 1),
        ("a", 4, 4),
        ("b", 2, 2),
        ("b", 5, 5),
        ("c", 3, 3),
        ("c", 6, 6),
    ];
    let full = catalog(&rows);
    for names in [vec!["a"], vec!["a", "b"], vec!["a", "b", "c"]] {
        let subset: HashMap<&str, Vec<InboxUnit>> = names
            .iter()
            .filter_map(|name| full.get(name).map(|units| (*name, units.clone())))
            .collect();
        let mut expect: Vec<u128> = subset
            .values()
            .flat_map(|units| units.iter().map(|unit| unit.id.as_u128()))
            .collect();
        expect.sort_unstable();
        let asc = walk(&names, &subset, Order::Asc, 2);
        let mut desc_expect = expect.clone();
        desc_expect.reverse();
        assert_eq!(asc, expect, "{names:?}");
        assert_eq!(
            walk(&names, &subset, Order::Desc, 2),
            desc_expect,
            "{names:?}"
        );
    }
}

#[test]
fn equal_instant_breaks_the_tie_by_id_in_the_same_direction() {
    let rows = catalog(&[("a", 5, 2), ("b", 5, 1)]);
    assert_eq!(walk(&["a", "b"], &rows, Order::Asc, 10), vec![1, 2]);
    assert_eq!(walk(&["a", "b"], &rows, Order::Desc, 10), vec![2, 1]);
}

#[test]
fn an_empty_source_does_not_drop_or_repeat_the_others() {
    let rows = catalog(&[("b", 1, 1), ("b", 2, 2)]);
    assert_eq!(walk(&["a", "b"], &rows, Order::Asc, 1), vec![1, 2]);
}

#[test]
fn a_full_page_with_has_more_advances_the_key_including_one_source() {
    let rows = catalog(&[("a", 1, 1), ("a", 2, 2), ("a", 3, 3), ("b", 10, 10)]);
    for names in [vec!["a"], vec!["a", "b"]] {
        let subset: HashMap<&str, Vec<InboxUnit>> = names
            .iter()
            .filter_map(|name| rows.get(name).map(|units| (*name, units.clone())))
            .collect();
        let first = page(&names, &subset, Order::Asc, 2, &BTreeMap::new());
        assert!(first.has_more);
        assert_eq!(ids_of(&first.units), vec![1, 2]);
        let key = first.keys.get("a").copied().flatten().expect("a moved");
        assert_eq!(key.id.as_u128(), 2);
        let second = page(&names, &subset, Order::Asc, 2, &first.keys);
        assert!(!ids_of(&second.units).contains(&1));
        assert!(!ids_of(&second.units).contains(&2));
        assert!(ids_of(&second.units).contains(&3));
    }
}

#[test]
fn an_ascending_insert_into_a_short_sources_unread_range_appears_where_its_key_falls() {
    let mut rows = catalog(&[("a", 1, 1), ("b", 2, 2), ("b", 3, 3), ("b", 4, 4)]);
    let names = ["a", "b"];
    let first = page(&names, &rows, Order::Asc, 2, &BTreeMap::new());
    assert_eq!(ids_of(&first.units), vec![1, 2]);
    rows.get_mut("a").unwrap().push(test_support::unit_at(
        "a",
        test_support::at_micros(2, 500_000),
        25,
    ));
    let second = page(&names, &rows, Order::Asc, 2, &first.keys);
    assert_eq!(ids_of(&second.units), vec![25, 3]);
}

#[test]
fn a_descending_insert_now_does_not_appear_later_in_the_walk() {
    let mut rows = catalog(&[("a", 4, 4), ("a", 3, 3), ("a", 2, 2), ("a", 1, 1)]);
    let names = ["a"];
    let first = page(&names, &rows, Order::Desc, 2, &BTreeMap::new());
    assert_eq!(ids_of(&first.units), vec![4, 3]);
    rows.get_mut("a")
        .unwrap()
        .push(test_support::unit("a", 9, 9));
    let rest = walk_from(&names, &rows, Order::Desc, 2, first.keys);
    assert!(!rest.contains(&9), "{rest:?}");
    assert_eq!(rest, vec![2, 1]);
}

fn walk_from(
    names: &[&str],
    catalog: &HashMap<&str, Vec<InboxUnit>>,
    order: Order,
    limit: u32,
    mut keys: BTreeMap<String, Option<SortKey>>,
) -> Vec<u128> {
    let mut out = Vec::new();
    for _ in 0..100 {
        let merged = page(names, catalog, order, limit, &keys);
        if merged.units.is_empty() {
            assert!(!merged.has_more);
            break;
        }
        out.extend(ids_of(&merged.units));
        if !merged.has_more {
            break;
        }
        keys = merged.keys;
    }
    out
}

#[test]
fn a_unit_that_leaves_the_narrowing_is_not_skipped_and_not_repeated() {
    let mut rows = catalog(&[("a", 1, 1), ("a", 2, 2), ("a", 3, 3), ("a", 4, 4)]);
    let names = ["a"];
    let first = page(&names, &rows, Order::Asc, 2, &BTreeMap::new());
    assert_eq!(ids_of(&first.units), vec![1, 2]);
    rows.get_mut("a")
        .unwrap()
        .retain(|unit| unit.id.as_u128() != 3);
    let rest = walk_from(&names, &rows, Order::Asc, 2, first.keys);
    assert_eq!(rest, vec![4]);
}

#[test]
fn every_unit_appears_once_for_limit_1_2_and_50() {
    let rows = catalog(&[
        ("a", 1, 1),
        ("b", 2, 2),
        ("a", 3, 3),
        ("c", 4, 4),
        ("b", 5, 5),
        ("c", 6, 6),
        ("a", 7, 7),
    ]);
    let names = ["a", "b", "c"];
    for limit in [1, 2, 50] {
        for order in [Order::Asc, Order::Desc] {
            let seen = walk(&names, &rows, order, limit);
            let mut sorted = seen.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted, vec![1, 2, 3, 4, 5, 6, 7], "{order:?} {limit}");
            assert_eq!(seen.len(), 7, "{order:?} {limit}");
            let expected: Vec<u128> = match order {
                Order::Asc => vec![1, 2, 3, 4, 5, 6, 7],
                Order::Desc => vec![7, 6, 5, 4, 3, 2, 1],
            };
            assert_eq!(seen, expected, "{order:?} {limit}");
        }
    }
}
