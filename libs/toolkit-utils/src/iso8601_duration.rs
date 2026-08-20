//! A minimal ISO 8601 duration type, fully interoperable with
//! `std::time::Duration` - hand-written, no external crate.
//!
//! Grammar (`P[nD][T[nH][nM][n[.f]S]]`, at least one component required):
//! mirrors `java.time.Duration::parse`'s established precedent for a
//! *fixed-length* duration string, not the full ISO 8601 duration grammar.
//! `Y` (years) and calendar `M` (months) are deliberately unsupported - they
//! aren't a fixed number of seconds (a year is 365 or 366 days; a month is
//! 28-31), so they can't round-trip through a `std::time::Duration` at all,
//! matching why `java.time.Duration::parse` excludes them too (only
//! `java.time.Period` - a calendar type, not a duration - accepts them).
//! `W` (weeks) is likewise excluded to match `java.time.Duration::parse`
//! exactly, even though a week is technically fixed-length (7 days):
//! ISO 8601 itself says `W` MUST NOT combine with other designators, which
//! would make it a second, mutually-exclusive grammar to support for a unit
//! this type's only real-world caller (`session_timeout`) never sends.
//! `D` (days) IS supported and is always exactly 24 hours, per the same
//! precedent.
//!
//! `std::time::Duration` is unsigned, so a leading `+`/`-` sign (which ISO
//! 8601 and `java.time.Duration::parse` both otherwise allow, negating the
//! whole duration) is rejected outright as a parse error rather than
//! silently dropped or made to panic.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 60 * SECS_PER_MINUTE;
const SECS_PER_DAY: u64 = 24 * SECS_PER_HOUR;

/// A `std::time::Duration` with ISO 8601 duration string parsing/formatting
/// bolted on. Fully interoperable with `Duration` via `From`/`Into`/`Deref` -
/// convert to/from this type only at the point a wire-format string needs to
/// become/come from a duration; use plain `Duration` everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Iso8601Duration(Duration);

impl Iso8601Duration {
    #[must_use]
    pub const fn new(duration: Duration) -> Self {
        Self(duration)
    }

    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }
}

impl From<Duration> for Iso8601Duration {
    fn from(duration: Duration) -> Self {
        Self(duration)
    }
}

impl From<Iso8601Duration> for Duration {
    fn from(value: Iso8601Duration) -> Self {
        value.0
    }
}

impl std::ops::Deref for Iso8601Duration {
    type Target = Duration;

    fn deref(&self) -> &Duration {
        &self.0
    }
}

/// Why an ISO 8601 duration string was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Iso8601DurationError {
    /// Input didn't start with (optional sign, then) `P`/`p`.
    MissingPeriodDesignator,
    /// A leading `+`/`-` sign was present - `std::time::Duration` is
    /// unsigned, so a negative duration has no valid representation, and a
    /// redundant `+` is rejected too rather than silently accepted.
    SignedDurationUnsupported,
    /// `P`/`PT` with no components at all.
    Empty,
    /// A component's numeric part wasn't a valid non-negative number, or a
    /// designator wasn't a recognized letter.
    InvalidComponent { text: String },
    /// A `Y`/`M`(onth)/`W` designator was used - not a fixed-length unit,
    /// unsupported by a `std::time::Duration`-compatible type (see the
    /// module doc comment for why).
    UnsupportedDesignator(char),
    /// A component appeared out of the required `D`, then `H`, `M`, `S`
    /// order, was repeated, or a non-`S` component had a fractional value.
    OutOfOrder { text: String },
    /// The computed duration doesn't fit in a `std::time::Duration`.
    Overflow,
}

impl fmt::Display for Iso8601DurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPeriodDesignator => write!(f, "duration must start with 'P'"),
            Self::SignedDurationUnsupported => {
                write!(
                    f,
                    "a signed duration is not supported (std::time::Duration is unsigned)"
                )
            }
            Self::Empty => write!(f, "duration has no components (bare 'P' or 'PT')"),
            Self::InvalidComponent { text } => {
                write!(f, "'{text}' is not a valid duration component")
            }
            Self::UnsupportedDesignator(d) => write!(
                f,
                "'{d}' is not a fixed-length unit (years/months/weeks aren't a \
                 constant number of seconds) and is not supported"
            ),
            Self::OutOfOrder { text } => write!(
                f,
                "'{text}' is out of order (expected D, then T, then H, M, S, \
                 each at most once, only S may be fractional)"
            ),
            Self::Overflow => write!(f, "duration is too large to represent"),
        }
    }
}

impl std::error::Error for Iso8601DurationError {}

/// One `<number><designator>` token, e.g. `("1", 'H')` from `"1H"`. `number`
/// may contain a single `.` for a fractional component.
fn tokenize(segment: &str) -> Result<Vec<(&str, char)>, Iso8601DurationError> {
    let mut tokens = Vec::new();
    let mut rest = segment;
    while !rest.is_empty() {
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        let (number, tail) = rest.split_at(digits_end);
        let mut chars = tail.chars();
        let Some(designator) = chars.next() else {
            return Err(Iso8601DurationError::InvalidComponent {
                text: rest.to_owned(),
            });
        };
        if number.is_empty() || !designator.is_ascii_alphabetic() {
            return Err(Iso8601DurationError::InvalidComponent {
                text: format!("{number}{designator}"),
            });
        }
        tokens.push((number, designator.to_ascii_uppercase()));
        rest = chars.as_str();
    }
    Ok(tokens)
}

fn parse_whole_secs(number: &str, token: &(&str, char)) -> Result<u64, Iso8601DurationError> {
    if number.contains('.') {
        return Err(Iso8601DurationError::OutOfOrder {
            text: format!("{}{}", token.0, token.1),
        });
    }
    number
        .parse()
        .map_err(|_| Iso8601DurationError::InvalidComponent {
            text: format!("{}{}", token.0, token.1),
        })
}

impl FromStr for Iso8601Duration {
    type Err = Iso8601DurationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with(['+', '-']) {
            return Err(Iso8601DurationError::SignedDurationUnsupported);
        }
        let rest = s
            .strip_prefix(['P', 'p'])
            .ok_or(Iso8601DurationError::MissingPeriodDesignator)?;
        let (date_part, time_part) = match rest.split_once(['T', 't']) {
            Some((d, t)) => (d, Some(t)),
            None => (rest, None),
        };

        let mut secs: u64 = 0;
        let mut nanos: u32 = 0;
        let mut saw_component = false;

        for token @ (number, designator) in tokenize(date_part)? {
            match designator {
                'D' => {
                    let days = parse_whole_secs(number, &token)?;
                    let component = days
                        .checked_mul(SECS_PER_DAY)
                        .ok_or(Iso8601DurationError::Overflow)?;
                    secs = secs
                        .checked_add(component)
                        .ok_or(Iso8601DurationError::Overflow)?;
                    saw_component = true;
                }
                'Y' | 'M' | 'W' => {
                    return Err(Iso8601DurationError::UnsupportedDesignator(designator));
                }
                _ => {
                    return Err(Iso8601DurationError::OutOfOrder {
                        text: format!("{number}{designator}"),
                    });
                }
            }
        }

        if let Some(time_part) = time_part {
            let tokens = tokenize(time_part)?;
            let mut seen_hour = false;
            let mut seen_minute = false;
            let mut seen_second = false;
            for token @ (number, designator) in tokens {
                match designator {
                    'H' if !seen_hour && !seen_minute && !seen_second => {
                        seen_hour = true;
                        let hours = parse_whole_secs(number, &token)?;
                        let component = hours
                            .checked_mul(SECS_PER_HOUR)
                            .ok_or(Iso8601DurationError::Overflow)?;
                        secs = secs
                            .checked_add(component)
                            .ok_or(Iso8601DurationError::Overflow)?;
                        saw_component = true;
                    }
                    'M' if !seen_minute && !seen_second => {
                        seen_minute = true;
                        let minutes = parse_whole_secs(number, &token)?;
                        let component = minutes
                            .checked_mul(SECS_PER_MINUTE)
                            .ok_or(Iso8601DurationError::Overflow)?;
                        secs = secs
                            .checked_add(component)
                            .ok_or(Iso8601DurationError::Overflow)?;
                        saw_component = true;
                    }
                    'S' if !seen_second => {
                        seen_second = true;
                        let (whole, frac) = number.split_once('.').unwrap_or((number, ""));
                        let whole_secs: u64 =
                            whole
                                .parse()
                                .map_err(|_| Iso8601DurationError::InvalidComponent {
                                    text: format!("{number}S"),
                                })?;
                        secs = secs
                            .checked_add(whole_secs)
                            .ok_or(Iso8601DurationError::Overflow)?;
                        if !frac.is_empty() {
                            let mut digits = frac.chars().chain(std::iter::repeat('0')).take(9);
                            let mut value = 0u32;
                            for _ in 0..9 {
                                let d = digits.next().unwrap_or('0');
                                value = value * 10
                                    + d.to_digit(10).ok_or_else(|| {
                                        Iso8601DurationError::InvalidComponent {
                                            text: format!("{number}S"),
                                        }
                                    })?;
                            }
                            nanos = value;
                        }
                        saw_component = true;
                    }
                    'Y' | 'W' => {
                        return Err(Iso8601DurationError::UnsupportedDesignator(designator));
                    }
                    _ => {
                        return Err(Iso8601DurationError::OutOfOrder {
                            text: format!("{number}{designator}"),
                        });
                    }
                }
            }
        }

        if !saw_component {
            return Err(Iso8601DurationError::Empty);
        }
        Ok(Self(Duration::new(secs, nanos)))
    }
}

impl fmt::Display for Iso8601Duration {
    /// Always renders in canonical `PT#H#M#S` form (never a `D` component,
    /// even if the value happens to be a whole number of days) - matching
    /// `java.time.Duration::toString`'s own normalization, and this type's
    /// only real-world caller never needing days in practice.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_secs = self.0.as_secs();
        let nanos = self.0.subsec_nanos();
        if total_secs == 0 && nanos == 0 {
            return write!(f, "PT0S");
        }
        // Integer division is exactly what's wanted here (whole
        // hours/minutes out of a whole-seconds count) - a float would be
        // wrong, not more precise.
        #[allow(clippy::integer_division)]
        let hours = total_secs / SECS_PER_HOUR;
        #[allow(clippy::integer_division)]
        let minutes = (total_secs % SECS_PER_HOUR) / SECS_PER_MINUTE;
        let secs = total_secs % SECS_PER_MINUTE;

        write!(f, "PT")?;
        if hours > 0 {
            write!(f, "{hours}H")?;
        }
        if minutes > 0 {
            write!(f, "{minutes}M")?;
        }
        if secs > 0 || nanos > 0 || (hours == 0 && minutes == 0) {
            if nanos > 0 {
                let mut frac = format!("{nanos:09}");
                while frac.ends_with('0') {
                    frac.pop();
                }
                write!(f, "{secs}.{frac}S")?;
            } else {
                write!(f, "{secs}S")?;
            }
        }
        Ok(())
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Iso8601Duration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Iso8601Duration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = <String as serde::Deserialize>::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
#[path = "iso8601_duration_tests.rs"]
mod iso8601_duration_tests;
