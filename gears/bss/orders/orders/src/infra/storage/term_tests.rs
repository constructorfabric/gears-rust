//! The term column mapping: intent kept as authored, intervals in exact calendar units.
#![allow(
    clippy::unwrap_used,
    reason = "test code: a failed mapping must stop the test at once"
)]
use super::*;

fn columns(kind: &'static str, authored: Value, duration: Option<&str>) -> TermColumns {
    TermColumns {
        kind,
        authored,
        duration: duration.map(str::to_owned),
    }
}

#[test]
fn term_columns_preserve_intent_and_calendar_units() {
    assert_eq!(
        term_columns(None, Some(BillingCycle::Month)).unwrap(),
        columns("missing", serde_json::json!({"kind": "missing"}), None)
    );
    assert_eq!(
        term_columns(Some(&AuthoredTerm::Rolling), None).unwrap(),
        columns("rolling", serde_json::json!({"kind": "rolling"}), None)
    );
    let periods = AuthoredTerm::Periods { count: 2 };
    assert_eq!(
        term_columns(Some(&periods), Some(BillingCycle::Year)).unwrap(),
        columns(
            "finite",
            serde_json::json!({"kind": "periods", "count": 2}),
            Some("24 mons 0 days 0 microseconds")
        )
    );
    // No cycle yet: the intent is kept with no interval.
    assert_eq!(term_columns(Some(&periods), None).unwrap().duration, None);
    let calendar = AuthoredTerm::Calendar {
        years: 1,
        months: 2,
        days: 3,
        microseconds: 4,
    };
    let stored = term_columns(Some(&calendar), None).unwrap();
    assert_eq!(
        stored,
        columns(
            "finite",
            serde_json::json!({"kind": "calendar", "years": 1, "months": 2, "days": 3, "microseconds": 4}),
            Some("14 mons 3 days 4 microseconds")
        )
    );
    assert_eq!(
        stored_term("finite", &stored.authored).unwrap(),
        Some(calendar)
    );
    assert_eq!(
        stored_term("missing", &serde_json::json!({"kind": "missing"})).unwrap(),
        None
    );
}

#[test]
fn stored_shape_failures_are_typed() {
    let json = serde_json::json!({"kind": "rolling"});
    assert_eq!(
        stored_term("other", &json),
        Err(StoredShapeError::UnknownKind)
    );
    assert_eq!(
        stored_term("finite", &serde_json::json!({"kind": "periods"})),
        Err(StoredShapeError::Undecodable {
            kind: "finite",
            category: "data"
        })
    );
    // A term `AuthoredTerm::validate` refuses has no interval; never a saturated one.
    let huge = AuthoredTerm::Calendar {
        years: u32::MAX,
        months: 0,
        days: 0,
        microseconds: 0,
    };
    assert_eq!(
        term_columns(Some(&huge), None),
        Err(StoredShapeError::Unrepresentable)
    );
}
