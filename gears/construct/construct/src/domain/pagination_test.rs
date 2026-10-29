//! The shared pagination, over a subject's facts.

use serde_json::json;
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, SortDir};

use super::pagination::{DEFAULT_LIMIT, MAX_LIMIT, PageError, page};
use super::subject_control::SUBJECT_FACTS_LISTING;
use crate::domain::profile::Category;
use crate::domain::profile_read::StoredFact;
use crate::test_support::stored_fact;

/// More pages than any walk here needs; a walk past it is a cursor that
/// does not advance.
const MAX_PAGES: usize = 50;

/// Skills stored at the given seconds after the epoch.
fn skills_at(seconds: &[i64]) -> Vec<StoredFact> {
    seconds
        .iter()
        .map(|second| stored_fact(Category::Skill, "skills", json!({ "at": second }), *second))
        .collect()
}

/// `count` skills stored one second apart, the first at second 1.
fn skills(count: i64) -> Vec<StoredFact> {
    skills_at(&(1..=count).collect::<Vec<_>>())
}

/// The query the `OData` extractor builds for `raw`: the filter and its
/// hash, which a cursor carries.
fn filtered(raw: &str) -> ODataQuery {
    let expr = toolkit_odata::parse_filter_string(raw)
        .expect("a filter")
        .into_expr();
    let hash = toolkit_odata::pagination::short_filter_hash(Some(&expr));
    let query = ODataQuery::new().with_filter(expr);
    match hash {
        Some(hash) => query.with_filter_hash(hash),
        None => query,
    }
}

fn ordered(field: &str, dir: SortDir) -> ODataOrderBy {
    ODataOrderBy(vec![OrderKey {
        field: field.to_owned(),
        dir,
    }])
}

fn seconds(facts: &[StoredFact]) -> Vec<i64> {
    facts
        .iter()
        .map(|fact| fact.stored_at.unix_timestamp())
        .collect()
}

/// Every item a walk from `first` through the cursors serves, in order; the
/// next query keeps `first`'s filter, as a client sends it again.
fn walk(all: &[StoredFact], first: &ODataQuery) -> Vec<StoredFact> {
    let mut served = Vec::new();
    let mut query = first.clone();
    for _ in 0..MAX_PAGES {
        let next = page(all.to_vec(), &query, &SUBJECT_FACTS_LISTING).unwrap();
        served.extend(next.items);
        let Some(cursor) = next.page_info.next_cursor else {
            return served;
        };
        query = ODataQuery {
            order: ODataOrderBy::empty(),
            cursor: Some(CursorV1::decode(&cursor).unwrap()),
            ..first.clone()
        };
    }
    panic!("the cursor did not advance within {MAX_PAGES} pages");
}

#[test]
fn the_default_page_is_newest_first_and_a_limit_past_the_maximum_is_cut_to_it() {
    let defaulted = page(skills(60), &ODataQuery::new(), &SUBJECT_FACTS_LISTING).unwrap();
    let cut = page(
        skills(250),
        &ODataQuery::new().with_limit(1000),
        &SUBJECT_FACTS_LISTING,
    )
    .unwrap();
    let floored = page(
        skills(3),
        &ODataQuery::new().with_limit(0),
        &SUBJECT_FACTS_LISTING,
    )
    .unwrap();

    assert_eq!(
        defaulted.items.len(),
        usize::try_from(DEFAULT_LIMIT).unwrap()
    );
    assert_eq!(seconds(&defaulted.items)[..2], [60, 59]);
    assert!(defaulted.page_info.next_cursor.is_some());
    assert_eq!(cut.items.len(), usize::try_from(MAX_LIMIT).unwrap());
    assert_eq!(cut.page_info.limit, MAX_LIMIT);
    assert_eq!(floored.items.len(), 1, "a zero limit is one");
}

#[test]
fn following_the_cursor_walks_every_fact_once_in_the_order_asked() {
    let all = skills(7);

    let served = walk(
        &all,
        &ODataQuery::new()
            .with_limit(3)
            .with_order(ordered("stored_at", SortDir::Asc)),
    );

    assert_eq!(seconds(&served), [1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn the_default_order_walks_every_fact_once_with_ties_across_pages() {
    // Newest first, and the fact id ascending among facts stored together;
    // the two keys run in opposite directions, and pages of two split the
    // ties.
    let all = skills_at(&[5, 3, 5, 4, 5, 3, 4]);
    let mut expected = all.clone();
    expected.sort_by(|a, b| {
        b.stored_at
            .cmp(&a.stored_at)
            .then_with(|| a.node_key.0.cmp(&b.node_key.0))
    });

    let served = walk(&all, &ODataQuery::new().with_limit(2));

    assert_eq!(served, expected);
}

#[test]
fn ties_are_broken_by_the_fact_id() {
    let mut same_time = skills_at(&[5, 5, 5, 5]);

    let page = page(
        same_time.clone(),
        &ODataQuery::new(),
        &SUBJECT_FACTS_LISTING,
    )
    .unwrap();

    same_time.sort_by(|a, b| a.node_key.0.cmp(&b.node_key.0));
    assert_eq!(page.items, same_time);
}

#[test]
fn a_filter_keeps_the_facts_it_matches() {
    let mut facts = skills(3);
    facts.push(stored_fact(Category::Role, "roles", json!({}), 4));
    let keep = |raw: &str| {
        seconds(
            &page(facts.clone(), &filtered(raw), &SUBJECT_FACTS_LISTING)
                .unwrap()
                .items,
        )
    };

    assert_eq!(keep("entity_kind eq 'role'"), [4]);
    assert_eq!(
        keep("stored_at gt 1970-01-01T00:00:02Z and origin eq 'connector'"),
        [4, 3]
    );
    assert_eq!(keep("not (entity_kind in ('skill', 'preference'))"), [4]);
    assert_eq!(
        keep("stored_at lt 2300-01-01T00:00:00Z"),
        [4, 3, 2, 1],
        "a time past what nanoseconds in an i64 hold still compares"
    );
    assert_eq!(keep("stored_at gt 1960-01-01T00:00:00Z"), [4, 3, 2, 1]);
    assert_eq!(keep("stored_at lt 1960-01-01T00:00:00Z"), Vec::<i64>::new());
}

#[test]
fn an_unknown_field_or_a_value_of_another_kind_is_the_callers_mistake() {
    for raw in [
        "value eq 'x'",
        "stored_at eq 5",
        "entity_kind eq 5",
        "stored_at eq null",
    ] {
        let result = page(skills(2), &filtered(raw), &SUBJECT_FACTS_LISTING);
        assert!(
            matches!(result, Err(PageError::Filter(_))),
            "{raw}: {result:?}"
        );
    }
    let unknown_order = page(
        skills(2),
        &ODataQuery::new().with_order(ordered("value", SortDir::Asc)),
        &SUBJECT_FACTS_LISTING,
    );
    assert!(
        matches!(unknown_order, Err(PageError::Order(_))),
        "{unknown_order:?}"
    );
}

#[test]
fn a_cursor_continued_under_another_filter_is_refused() {
    let first = page(
        skills(4),
        &filtered("entity_kind eq 'skill'").with_limit(1),
        &SUBJECT_FACTS_LISTING,
    )
    .unwrap();
    let cursor = CursorV1::decode(&first.page_info.next_cursor.unwrap()).unwrap();

    for (what, next) in [
        ("another filter", filtered("origin eq 'connector'")),
        ("no filter", ODataQuery::new()),
    ] {
        let result = page(
            skills(4),
            &next.with_cursor(cursor.clone()),
            &SUBJECT_FACTS_LISTING,
        );
        assert!(
            matches!(result, Err(PageError::Cursor(_))),
            "{what}: {result:?}"
        );
    }
}
