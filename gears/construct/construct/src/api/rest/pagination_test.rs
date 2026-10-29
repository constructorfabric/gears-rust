//! The shared pagination, over a subject's facts.

use serde_json::json;
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};

use super::dto::SUBJECT_FACTS_LISTING;
use super::pagination::{DEFAULT_LIMIT, MAX_LIMIT, PageError, page};
use crate::domain::profile::Category;
use crate::domain::profile_read::StoredFact;
use crate::test_support::stored_fact;

/// `count` skills stored one second apart, the first at second 1.
fn skills(count: i64) -> Vec<StoredFact> {
    (1..=count)
        .map(|second| stored_fact(Category::Skill, "skills", json!({ "n": second }), second))
        .collect()
}

fn filtered(raw: &str) -> ODataQuery {
    ODataQuery::new().with_filter(
        toolkit_odata::parse_filter_string(raw)
            .expect("a filter")
            .into_expr(),
    )
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
fn following_the_cursor_walks_every_fact_once_in_order() {
    let all = skills(7);
    let mut seen = Vec::new();
    let mut query = ODataQuery::new()
        .with_limit(3)
        .with_order(ordered("stored_at", SortDir::Asc));

    loop {
        let next = page(all.clone(), &query, &SUBJECT_FACTS_LISTING).unwrap();
        seen.extend(seconds(&next.items));
        let Some(cursor) = next.page_info.next_cursor else {
            break;
        };
        query = ODataQuery::new()
            .with_limit(3)
            .with_cursor(toolkit_odata::CursorV1::decode(&cursor).unwrap());
    }

    assert_eq!(seen, [1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn ties_are_broken_by_the_fact_id() {
    let mut same_time: Vec<StoredFact> = (0..4)
        .map(|_| stored_fact(Category::Skill, "skills", json!({}), 5))
        .collect();

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

    let roles = page(
        facts.clone(),
        &filtered("entity_kind eq 'role'"),
        &SUBJECT_FACTS_LISTING,
    )
    .unwrap();
    let late = page(
        facts.clone(),
        &filtered("stored_at gt 1970-01-01T00:00:02Z and origin eq 'connector'"),
        &SUBJECT_FACTS_LISTING,
    )
    .unwrap();
    let not_skills = page(
        facts,
        &filtered("not (entity_kind in ('skill', 'preference'))"),
        &SUBJECT_FACTS_LISTING,
    )
    .unwrap();

    assert_eq!(seconds(&roles.items), [4]);
    assert_eq!(seconds(&late.items), [4, 3]);
    assert_eq!(seconds(&not_skills.items), [4]);
}

#[test]
fn an_unknown_field_or_a_cursor_of_another_order_is_the_callers_mistake() {
    let unknown_filter = page(skills(2), &filtered("value eq 'x'"), &SUBJECT_FACTS_LISTING);
    let unknown_order = page(
        skills(2),
        &ODataQuery::new().with_order(ordered("value", SortDir::Asc)),
        &SUBJECT_FACTS_LISTING,
    );
    let first = page(
        skills(3),
        &ODataQuery::new().with_limit(1),
        &SUBJECT_FACTS_LISTING,
    )
    .unwrap();
    let cursor = toolkit_odata::CursorV1::decode(&first.page_info.next_cursor.unwrap()).unwrap();
    let other_order = page(
        skills(3),
        &ODataQuery::new()
            .with_cursor(cursor)
            .with_order(ordered("entity_kind", SortDir::Asc)),
        &SUBJECT_FACTS_LISTING,
    );

    assert!(
        matches!(unknown_filter, Err(PageError::Filter(_))),
        "{unknown_filter:?}"
    );
    assert!(
        matches!(unknown_order, Err(PageError::Order(_))),
        "{unknown_order:?}"
    );
    assert!(
        matches!(other_order, Err(PageError::Cursor(_))),
        "{other_order:?}"
    );
}
