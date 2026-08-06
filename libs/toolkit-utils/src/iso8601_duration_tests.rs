use std::time::Duration;

use super::{Iso8601Duration, Iso8601DurationError};

fn parse(s: &str) -> Result<Duration, Iso8601DurationError> {
    s.parse::<Iso8601Duration>()
        .map(Iso8601Duration::as_duration)
}

#[test]
fn parses_bare_seconds() {
    assert_eq!(parse("PT30S").unwrap(), Duration::from_secs(30));
}

#[test]
fn parses_minutes() {
    assert_eq!(parse("PT5M").unwrap(), Duration::from_mins(5));
}

#[test]
fn parses_hours() {
    assert_eq!(parse("PT1H").unwrap(), Duration::from_hours(1));
}

#[test]
fn parses_combined_hours_minutes_seconds() {
    assert_eq!(
        parse("PT1H30M5S").unwrap(),
        Duration::from_secs(3600 + 1800 + 5)
    );
}

#[test]
fn parses_days_as_exactly_24_hours() {
    assert_eq!(parse("P2D").unwrap(), Duration::from_hours(48));
}

#[test]
fn parses_days_combined_with_time_part() {
    assert_eq!(parse("P1DT2H").unwrap(), Duration::from_hours(26));
}

#[test]
fn parses_fractional_seconds() {
    assert_eq!(parse("PT0.5S").unwrap(), Duration::new(0, 500_000_000));
}

#[test]
fn parses_fractional_seconds_with_whole_part() {
    assert_eq!(parse("PT1.25S").unwrap(), Duration::new(1, 250_000_000));
}

#[test]
fn lowercase_designators_accepted() {
    assert_eq!(parse("pt1h").unwrap(), Duration::from_hours(1));
}

#[test]
fn rejects_empty_string() {
    assert_eq!(
        parse(""),
        Err(Iso8601DurationError::MissingPeriodDesignator)
    );
}

#[test]
fn rejects_missing_p_prefix() {
    assert_eq!(
        parse("30S"),
        Err(Iso8601DurationError::MissingPeriodDesignator)
    );
}

#[test]
fn rejects_bare_p() {
    assert_eq!(parse("P"), Err(Iso8601DurationError::Empty));
}

#[test]
fn rejects_bare_pt() {
    assert_eq!(parse("PT"), Err(Iso8601DurationError::Empty));
}

#[test]
fn rejects_garbage() {
    assert!(matches!(
        parse("garbage"),
        Err(Iso8601DurationError::MissingPeriodDesignator)
    ));
}

#[test]
fn rejects_years() {
    assert_eq!(
        parse("P1Y"),
        Err(Iso8601DurationError::UnsupportedDesignator('Y'))
    );
}

#[test]
fn rejects_months() {
    assert_eq!(
        parse("P1M"),
        Err(Iso8601DurationError::UnsupportedDesignator('M'))
    );
}

#[test]
fn rejects_weeks() {
    assert_eq!(
        parse("P1W"),
        Err(Iso8601DurationError::UnsupportedDesignator('W'))
    );
}

#[test]
fn rejects_negative_sign() {
    assert_eq!(
        parse("-PT30S"),
        Err(Iso8601DurationError::SignedDurationUnsupported)
    );
}

#[test]
fn rejects_positive_sign() {
    assert_eq!(
        parse("+PT30S"),
        Err(Iso8601DurationError::SignedDurationUnsupported)
    );
}

#[test]
fn rejects_out_of_order_components() {
    // seconds before minutes
    assert!(matches!(
        parse("PT5S3M"),
        Err(Iso8601DurationError::OutOfOrder { .. })
    ));
}

#[test]
fn rejects_repeated_components() {
    assert!(matches!(
        parse("PT1H2H"),
        Err(Iso8601DurationError::OutOfOrder { .. })
    ));
}

#[test]
fn rejects_fractional_hours() {
    assert!(matches!(
        parse("PT1.5H"),
        Err(Iso8601DurationError::OutOfOrder { .. })
    ));
}

#[test]
fn rejects_fractional_minutes() {
    assert!(matches!(
        parse("PT1.5M"),
        Err(Iso8601DurationError::OutOfOrder { .. })
    ));
}

#[test]
fn rejects_a_minutes_value_that_would_overflow_seconds_conversion() {
    // This is the exact overflow shape that made the old hand-rolled
    // parser silently produce a 1-second timeout instead of erroring.
    let huge = u64::MAX.to_string();
    assert_eq!(
        parse(&format!("PT{huge}M")),
        Err(Iso8601DurationError::Overflow)
    );
}

#[test]
fn rejects_a_days_value_that_would_overflow() {
    let huge = u64::MAX.to_string();
    assert_eq!(
        parse(&format!("P{huge}D")),
        Err(Iso8601DurationError::Overflow)
    );
}

#[test]
fn display_round_trips_seconds_only() {
    let d = Iso8601Duration::new(Duration::from_secs(30));
    assert_eq!(d.to_string(), "PT30S");
    assert_eq!(d.to_string().parse::<Iso8601Duration>().unwrap(), d);
}

#[test]
fn display_round_trips_combined_hours_minutes_seconds() {
    let d = Iso8601Duration::new(Duration::from_secs(3600 + 1800 + 5));
    assert_eq!(d.to_string(), "PT1H30M5S");
    assert_eq!(d.to_string().parse::<Iso8601Duration>().unwrap(), d);
}

#[test]
fn display_zero_duration() {
    assert_eq!(Iso8601Duration::new(Duration::ZERO).to_string(), "PT0S");
}

#[test]
fn display_normalizes_days_to_hours_not_a_d_component() {
    let d = Iso8601Duration::new(Duration::from_hours(24));
    assert_eq!(d.to_string(), "PT24H");
}

#[test]
fn display_includes_fractional_seconds_without_trailing_zeros() {
    let d = Iso8601Duration::new(Duration::new(1, 250_000_000));
    assert_eq!(d.to_string(), "PT1.25S");
}

#[test]
fn from_and_into_duration_round_trip() {
    let duration = Duration::from_secs(42);
    let iso: Iso8601Duration = duration.into();
    let back: Duration = iso.into();
    assert_eq!(back, duration);
}

#[test]
fn deref_gives_access_to_duration_methods() {
    let iso = Iso8601Duration::new(Duration::from_secs(90));
    assert_eq!(iso.as_secs(), 90);
}

#[cfg(feature = "serde")]
mod serde_tests {
    use std::time::Duration;

    use super::super::Iso8601Duration;

    #[test]
    fn serializes_as_the_canonical_string() {
        let d = Iso8601Duration::new(Duration::from_secs(90));
        assert_eq!(serde_json::to_string(&d).unwrap(), "\"PT1M30S\"");
    }

    #[test]
    fn deserializes_from_a_valid_string() {
        let d: Iso8601Duration = serde_json::from_str("\"PT1M30S\"").unwrap();
        assert_eq!(d.as_duration(), Duration::from_secs(90));
    }

    #[test]
    fn deserialize_rejects_a_malformed_string() {
        let result: Result<Iso8601Duration, _> = serde_json::from_str("\"garbage\"");
        assert!(result.is_err());
    }
}
