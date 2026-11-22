//! Text of a spreadsheet cell, the way Excel displays its value.

use time::{Date, Duration, Month};

use super::numfmt::Kind;

/// Largest serial Excel can show as a date: 9999-12-31.
const MAX_DATE_SERIAL: f64 = 2_958_465.0;
/// Duration magnitudes beyond this many days are printed as numbers.
const MAX_DURATION_DAYS: f64 = 1.0e11;

pub(super) fn bool_text(b: bool) -> &'static str {
    if b { "TRUE" } else { "FALSE" }
}

/// `2024-01-02T03:04:05` as `2024-01-02 03:04:05`, matching serial dates.
pub(super) fn iso_datetime(s: &str) -> String {
    s.replacen('T', " ", 1)
}

/// A number with at most the 15 significant digits Excel keeps: `=0.1+0.2`
/// shows as `0.3`, `1999.0` as `1999`. Very large or very small magnitudes use
/// Excel's scientific form (`1E+300`, `1.5E-12`).
pub(super) fn format_float(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let rounded = format!("{value:.14e}").parse::<f64>().unwrap_or(value);
    if rounded == 0.0 {
        // Covers -0.0 too.
        return "0".to_owned();
    }
    let magnitude = rounded.abs();
    if (1e-9..1e15).contains(&magnitude) {
        return rounded.to_string();
    }
    // `{:e}` gives `1e300` / `1.5e-12`; Excel writes `1E+300` / `1.5E-12`.
    let sci = format!("{rounded:e}");
    match sci.split_once('e') {
        Some((mantissa, exp)) if exp.starts_with('-') => format!("{mantissa}E{exp}"),
        Some((mantissa, exp)) => format!("{mantissa}E+{exp}"),
        None => sci,
    }
}

/// Display text of a date-formatted serial number (days since the workbook's
/// epoch). Out-of-range serials fall back to the plain number, as Excel shows
/// `#####` for them.
pub(super) fn serial_text(serial: f64, kind: Kind, date1904: bool) -> String {
    match kind {
        Kind::Number => format_float(serial),
        Kind::Duration => format_duration(serial),
        Kind::Time => time_of_day(serial).unwrap_or_else(|| format_float(serial)),
        Kind::Date | Kind::DateTime => {
            let Some((date, seconds)) = split_serial(serial, date1904) else {
                return format_float(serial);
            };
            if kind == Kind::Date || seconds == 0 {
                date
            } else {
                format!("{date} {}", hms(seconds))
            }
        }
    }
}

/// `serial` as an ISO date plus the seconds into that day, or `None` outside
/// Excel's date range.
fn split_serial(serial: f64, date1904: bool) -> Option<(String, u32)> {
    if !(0.0..MAX_DATE_SERIAL + 1.0).contains(&serial) {
        return None;
    }
    let mut days = serial.trunc();
    let mut seconds = ((serial - days) * 86_400.0).round();
    if seconds >= 86_400.0 {
        days += 1.0;
        seconds = 0.0;
    }
    let days = whole(days)?;
    let seconds = u32::try_from(whole(seconds)?).ok()?;

    let date = if date1904 {
        epoch(1904, Month::January, 1)?.checked_add(Duration::days(days))?
    } else {
        // Excel's 1900 system counts a 29 February 1900 that never existed.
        match days {
            0 => return Some(("1900-01-00".to_owned(), seconds)),
            60 => return Some(("1900-02-29".to_owned(), seconds)),
            1..60 => epoch(1899, Month::December, 31)?.checked_add(Duration::days(days))?,
            _ => epoch(1899, Month::December, 30)?.checked_add(Duration::days(days))?,
        }
    };
    Some((
        format!(
            "{:04}-{:02}-{:02}",
            date.year(),
            u8::from(date.month()),
            date.day()
        ),
        seconds,
    ))
}

/// A whole, finite `f64` well inside `i64` as `i64`.
fn whole(value: f64) -> Option<i64> {
    if value.fract() != 0.0 || !(-9.0e15..=9.0e15).contains(&value) {
        return None;
    }
    format!("{value:.0}").parse().ok()
}

fn epoch(year: i32, month: Month, day: u8) -> Option<Date> {
    Date::from_calendar_date(year, month, day).ok()
}

/// The time-of-day part of `serial` as `HH:MM:SS`.
fn time_of_day(serial: f64) -> Option<String> {
    if !serial.is_finite() || serial < 0.0 {
        return None;
    }
    let seconds = whole(((serial - serial.trunc()) * 86_400.0).round())?;
    let seconds = u32::try_from(seconds).ok()?.min(86_399);
    Some(hms(seconds))
}

fn hms(seconds: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds.div_euclid(3600),
        seconds.div_euclid(60).rem_euclid(60),
        seconds.rem_euclid(60)
    )
}

/// `days` as `[-]H:MM:SS`, hours unbounded like Excel's `[h]:mm:ss`.
fn format_duration(days: f64) -> String {
    if !days.is_finite() || days.abs() > MAX_DURATION_DAYS {
        return format_float(days);
    }
    let Some(seconds) = whole((days * 86_400.0).round()) else {
        return format_float(days);
    };
    let sign = if seconds < 0 { "-" } else { "" };
    let total = seconds.unsigned_abs();
    format!(
        "{sign}{}:{:02}:{:02}",
        total.div_euclid(3600),
        total.div_euclid(60).rem_euclid(60),
        total.rem_euclid(60)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_formatting() {
        assert_eq!(format_float(1999.0), "1999");
        assert_eq!(format_float(9.99), "9.99");
        assert_eq!(format_float(0.1 + 0.2), "0.3");
        assert_eq!(format_float(100.0 * 1.19), "119");
        assert_eq!(format_float(-0.0), "0");
        assert_eq!(format_float(-2.5), "-2.5");
        assert_eq!(format_float(1e300), "1E+300");
        assert_eq!(format_float(-1.5e-12), "-1.5E-12");
        assert_eq!(format_float(123_456_789_012_345.0), "123456789012345");
        assert_eq!(format_float(f64::NAN), "NaN");
    }

    #[test]
    fn serial_dates_in_both_epochs() {
        assert_eq!(serial_text(45_352.0, Kind::Date, false), "2024-03-01");
        assert_eq!(
            serial_text(45_352.5, Kind::DateTime, false),
            "2024-03-01 12:00:00"
        );
        assert_eq!(serial_text(45_352.5, Kind::Date, false), "2024-03-01");
        assert_eq!(serial_text(1.0, Kind::Date, false), "1900-01-01");
        assert_eq!(serial_text(59.0, Kind::Date, false), "1900-02-28");
        assert_eq!(serial_text(60.0, Kind::Date, false), "1900-02-29");
        assert_eq!(serial_text(61.0, Kind::Date, false), "1900-03-01");
        assert_eq!(serial_text(0.0, Kind::Date, true), "1904-01-01");
        assert_eq!(serial_text(1.0, Kind::Date, true), "1904-01-02");
        // Rounding up to midnight moves to the next day.
        assert_eq!(
            serial_text(45_352.999_999_9, Kind::DateTime, false),
            "2024-03-02"
        );
    }

    #[test]
    fn out_of_range_serials_print_as_numbers() {
        assert_eq!(serial_text(-1.0, Kind::Date, false), "-1");
        assert_eq!(serial_text(-1e300, Kind::DateTime, false), "-1E+300");
        assert_eq!(serial_text(3_000_000.0, Kind::Date, false), "3000000");
        assert_eq!(serial_text(f64::INFINITY, Kind::Time, false), "inf");
        assert_eq!(serial_text(1e300, Kind::Duration, false), "1E+300");
    }

    #[test]
    fn times_and_durations() {
        assert_eq!(serial_text(0.25, Kind::Time, false), "06:00:00");
        assert_eq!(serial_text(45_352.75, Kind::Time, false), "18:00:00");
        assert_eq!(serial_text(1.5, Kind::Duration, false), "36:00:00");
        assert_eq!(serial_text(-0.5, Kind::Duration, false), "-12:00:00");
    }
}
