//! Pure properties of the PostgreSQL interval text codec; the server-rendered bounds are in
//! `tests/repositories.rs` (Docker tier).
#![allow(
    clippy::unwrap_used,
    reason = "test code: a failed rendering must stop the test at once"
)]
use super::*;
use proptest::prelude::*;

fn interval() -> impl Strategy<Value = CalendarInterval> {
    (any::<i32>(), any::<i32>(), any::<i64>()).prop_map(|(months, days, microseconds)| {
        CalendarInterval {
            months,
            days,
            microseconds,
        }
    })
}

/// The time part as PostgreSQL's `IntervalStyle=postgres` renders it: `[-]HH:MM:SS[.ffffff]`.
#[expect(
    clippy::integer_division,
    reason = "splitting a non-negative microsecond count into whole hours, minutes and seconds \
              truncates by design, as the server's rendering does"
)]
fn clock(microseconds: i64) -> String {
    let sign = if microseconds < 0 { "-" } else { "" };
    let total = i128::from(microseconds).abs();
    let (seconds, fraction) = (total / 1_000_000, total % 1_000_000);
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    let fraction = if fraction == 0 {
        String::new()
    } else {
        format!(".{fraction:06}")
    };
    format!("{sign}{hours:02}:{minutes:02}:{seconds:02}{fraction}")
}

proptest! {
    /// `parse(postgres(x)) == x` over the whole representable range.
    #[test]
    fn parse_inverts_the_rendered_text(x in interval()) {
        prop_assert_eq!(CalendarInterval::parse(&x.postgres()).unwrap(), x);
    }

    /// The server's clock form decodes to the same microseconds as the unit form.
    #[test]
    fn parse_reads_the_server_clock_form(x in interval()) {
        let text = format!("{} mons {} days {}", x.months, x.days, clock(x.microseconds));
        prop_assert_eq!(CalendarInterval::parse(&text).unwrap(), x);
    }

    /// Arbitrary text is decoded or refused, never a panic.
    #[test]
    fn parse_never_panics_on_arbitrary_text(text in "\\PC{0,64}") {
        let _decoded_or_refused = CalendarInterval::parse(&text);
    }
}
