#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_approvals_sdk::InboxUnit;
use toolkit_canonical_errors::Problem;

use super::error;
use super::owner::{self, Resolved, SourceGet};
use crate::test_support;

#[derive(Clone, Copy)]
enum Cell {
    Hold,
    Miss,
    Deny,
    Down,
}

enum Expect {
    Unit(&'static str),
    Missing,
    Forbidden,
    Unavailable(&'static [&'static str]),
    Ambiguous(&'static [&'static str]),
}

fn held(name: &str) -> InboxUnit {
    let id = if name == "pricing" { 11 } else { 22 };
    test_support::unit(name, 1, id)
}

fn answer(name: &'static str, cell: Cell) -> (String, SourceGet) {
    let value = match cell {
        Cell::Hold => SourceGet::Found(Box::new(held(name))),
        Cell::Miss => SourceGet::Absent,
        Cell::Deny => SourceGet::Forbidden,
        Cell::Down => SourceGet::Unavailable,
    };
    (name.to_owned(), value)
}

fn matches(resolved: Resolved, expect: &Expect) -> bool {
    match (resolved, expect) {
        (Resolved::Found { source, unit }, Expect::Unit(name)) => {
            source == *name && unit.source == *name
        }
        (Resolved::Missing, Expect::Missing) | (Resolved::Forbidden, Expect::Forbidden) => true,
        (Resolved::Unavailable { sources }, Expect::Unavailable(names)) => {
            sources
                == names
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>()
        }
        (Resolved::Ambiguous { sources }, Expect::Ambiguous(names)) => {
            sources
                == names
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>()
        }
        _ => false,
    }
}

#[test]
fn every_owner_resolution_row() {
    use Cell::{Deny, Down, Hold, Miss};
    use Expect::{Ambiguous, Forbidden, Missing, Unavailable, Unit};
    let rows = [
        (Hold, Miss, Unit("pricing")),
        (Miss, Hold, Unit("products")),
        (Hold, Hold, Ambiguous(&["pricing", "products"])),
        (Hold, Deny, Unit("pricing")),
        (Hold, Down, Unit("pricing")),
        (Deny, Hold, Unit("products")),
        (Down, Hold, Unit("products")),
        (Miss, Miss, Missing),
        (Deny, Miss, Forbidden),
        (Miss, Deny, Forbidden),
        (Deny, Deny, Forbidden),
        (Down, Miss, Unavailable(&["pricing"])),
        (Miss, Down, Unavailable(&["products"])),
        (Down, Deny, Unavailable(&["pricing"])),
        (Deny, Down, Unavailable(&["products"])),
        (Down, Down, Unavailable(&["pricing", "products"])),
    ];
    for (index, (left, right, expect)) in rows.into_iter().enumerate() {
        let resolved = owner::resolve(vec![answer("pricing", left), answer("products", right)]);
        assert!(matches(resolved, &expect), "row {index}");
    }
}

#[test]
fn a_source_error_other_than_403_or_503_fails_the_card_unless_a_unit_was_found() {
    let failed = error::unauthenticated();
    let missed = owner::resolve(vec![
        ("pricing".to_owned(), SourceGet::Failed(Box::new(failed))),
        ("products".to_owned(), SourceGet::Absent),
    ]);
    let err =
        owner::require_one(missed).expect_err("a failed source with no unit is the source error");
    assert_eq!(err.status_code(), 401);

    let found = owner::resolve(vec![
        (
            "pricing".to_owned(),
            SourceGet::Found(Box::new(held("pricing"))),
        ),
        (
            "products".to_owned(),
            SourceGet::Failed(Box::new(error::unauthenticated())),
        ),
    ]);
    let (source, _) = owner::require_one(found).unwrap();
    assert_eq!(source, "pricing");
}

#[test]
fn the_refusals_name_what_the_rule_names() {
    let forbidden = owner::require_one(Resolved::Forbidden).unwrap_err();
    let body = serde_json::to_string(&Problem::from_error(&forbidden).unwrap()).unwrap();
    assert_eq!(forbidden.status_code(), 403);
    assert!(!body.contains("pricing"));
    assert!(!body.contains("products"));

    let ambiguous = owner::require_one(Resolved::Ambiguous {
        sources: vec!["pricing".to_owned(), "products".to_owned()],
    })
    .unwrap_err();
    let body = serde_json::to_string(&Problem::from_error(&ambiguous).unwrap()).unwrap();
    assert_eq!(ambiguous.status_code(), 500);
    assert!(body.contains("pricing"));
    assert!(body.contains("products"));

    let down = owner::require_one(Resolved::Unavailable {
        sources: vec!["products".to_owned()],
    })
    .unwrap_err();
    let body = serde_json::to_string(&Problem::from_error(&down).unwrap()).unwrap();
    assert_eq!(down.status_code(), 503);
    assert!(body.contains("SOURCE_UNAVAILABLE"));
    assert!(body.contains("products"));

    let missing = owner::require_one(Resolved::Missing).unwrap_err();
    assert_eq!(missing.status_code(), 404);
}
