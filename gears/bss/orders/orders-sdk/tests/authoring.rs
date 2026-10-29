#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not \
              `expect`, because not every test in the file unwraps or expects"
)]
use bss_orders_sdk::authoring::{
    AddLine, AuthoredTerm, AuthoringInvalid, CalendarDate, Currency, LinePatch, SelectedItem,
};
use time::{Date, Month};
use uuid::Uuid;

/// `time` without `large-dates` stops at year 9999, so the out-of-range side is a negative year.
fn date(year: i32) -> CalendarDate {
    CalendarDate(Date::from_calendar_date(year, Month::January, 1).unwrap())
}

fn line() -> AddLine {
    AddLine {
        plan_id: Uuid::from_u128(1),
        plan_revision_id: Uuid::from_u128(2),
        selected_items: vec![SelectedItem {
            item_id: Uuid::from_u128(3),
            quantity: None,
            selected_dim_value: None,
        }],
        currency: Currency::try_from("USD".to_owned()).unwrap(),
        contract_effective_date: None,
        service_activation_date: None,
        acceptance_due_date: None,
        term_duration: None,
        billing_cycle: None,
    }
}

#[test]
fn add_line_refuses_dates_outside_the_wire_encoding() {
    // Both ends of the four-digit encoding are valid and serialize.
    for year in [0, 9999] {
        let mut ok = line();
        ok.contract_effective_date = Some(date(year));
        ok.validate().unwrap();
        serde_json::to_value(&ok).unwrap();
    }
    for year in [-1, -9999] {
        for (field, expected) in [
            (0, "contract_effective_date"),
            (1, "service_activation_date"),
            (2, "acceptance_due_date"),
        ] {
            let mut bad = line();
            let slot = match field {
                0 => &mut bad.contract_effective_date,
                1 => &mut bad.service_activation_date,
                _ => &mut bad.acceptance_due_date,
            };
            *slot = Some(date(year));
            // The validator names the field, before anything tries to serialize the value.
            assert_eq!(bad.validate(), Err(AuthoringInvalid(expected)));
            assert!(serde_json::to_value(&bad).is_err());
        }
    }
}

#[test]
fn line_patch_refuses_dates_outside_the_wire_encoding_and_still_clears_with_null() {
    let cleared = LinePatch {
        contract_effective_date: Some(None),
        ..LinePatch::default()
    };
    cleared.validate().unwrap();
    let edge = LinePatch {
        acceptance_due_date: Some(Some(date(9999))),
        ..LinePatch::default()
    };
    edge.validate().unwrap();
    for year in [-1, -9999] {
        let cases = [
            (
                LinePatch {
                    contract_effective_date: Some(Some(date(year))),
                    ..LinePatch::default()
                },
                "contract_effective_date",
            ),
            (
                LinePatch {
                    service_activation_date: Some(Some(date(year))),
                    ..LinePatch::default()
                },
                "service_activation_date",
            ),
            (
                LinePatch {
                    acceptance_due_date: Some(Some(date(year))),
                    ..LinePatch::default()
                },
                "acceptance_due_date",
            ),
        ];
        for (patch, expected) in cases {
            assert_eq!(patch.validate(), Err(AuthoringInvalid(expected)));
            assert!(serde_json::to_value(&patch).is_err());
        }
    }
}

/// The stored interval stays inside PostgreSQL's `interval` fields: whole months and days in
/// `i32`, microseconds in `i64`, and a period count whose month total fits `i32` under either
/// cycle. Each bound is pinned just inside and just over.
#[test]
fn authored_term_bounds_are_exact() {
    const MAX_MONTHS: u32 = i32::MAX.unsigned_abs();
    let invalid = Err(AuthoringInvalid("term_duration"));
    let calendar = |years, months, days, microseconds| AuthoredTerm::Calendar {
        years,
        months,
        days,
        microseconds,
    };
    // The largest count whose month total fits `i32`.
    let max_count = 178_956_970_u32;
    assert!(max_count * 12 <= MAX_MONTHS && (max_count + 1) * 12 > MAX_MONTHS);
    let max_micros = i64::MAX.unsigned_abs();
    for (term, expected) in [
        (AuthoredTerm::Rolling, Ok(())),
        (AuthoredTerm::Periods { count: 1 }, Ok(())),
        (AuthoredTerm::Periods { count: 0 }, invalid),
        (AuthoredTerm::Periods { count: max_count }, Ok(())),
        (
            AuthoredTerm::Periods {
                count: max_count + 1,
            },
            invalid,
        ),
        (AuthoredTerm::Periods { count: u32::MAX }, invalid),
        (calendar(0, 0, 0, 0), invalid),
        (calendar(0, 0, 0, 1), Ok(())),
        (calendar(0, MAX_MONTHS, 0, 0), Ok(())),
        (calendar(0, MAX_MONTHS + 1, 0, 0), invalid),
        // Years count twelve months towards the same bound.
        (calendar(max_count, MAX_MONTHS % 12, 0, 0), Ok(())),
        (calendar(max_count, MAX_MONTHS % 12 + 1, 0, 0), invalid),
        (calendar(u32::MAX, 0, 0, 0), invalid),
        (calendar(0, 0, MAX_MONTHS, 0), Ok(())),
        (calendar(0, 0, MAX_MONTHS + 1, 0), invalid),
        (calendar(0, 0, u32::MAX, 0), invalid),
        (calendar(0, 0, 0, max_micros), Ok(())),
        (calendar(0, 0, 0, max_micros + 1), invalid),
        (calendar(0, 0, 0, u64::MAX), invalid),
    ] {
        assert_eq!(term.validate(), expected, "{term:?}");
    }
}
