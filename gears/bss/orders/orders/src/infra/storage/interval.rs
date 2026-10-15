//! Lossless PostgreSQL calendar intervals. A month is never converted into days.
use toolkit_db::secure::ScopeError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalendarInterval {
    pub months: i32,
    pub days: i32,
    pub microseconds: i64,
}
impl CalendarInterval {
    pub(crate) fn postgres(self) -> String {
        format!(
            "{} mons {} days {} microseconds",
            self.months, self.days, self.microseconds
        )
    }
    /// Decode PostgreSQL's default `IntervalStyle=postgres` text output.
    /// Other styles fail explicitly instead of silently changing period geometry.
    pub(crate) fn parse(text: &str) -> Result<Self, ScopeError> {
        let invalid = || ScopeError::Invalid("invalid or overflowing calendar interval");
        let mut result = Self {
            months: 0,
            days: 0,
            microseconds: 0,
        };
        let mut words = text.split_whitespace();
        while let Some(token) = words.next() {
            if token.contains(':') {
                let sign = if token.starts_with('-') { -1i128 } else { 1 };
                let parts: Vec<_> = token.trim_start_matches(['+', '-']).split(':').collect();
                if parts.len() != 3 {
                    return Err(invalid());
                }
                let hours = parts[0].parse::<i128>().map_err(|_| invalid())?;
                let minutes = parts[1].parse::<i128>().map_err(|_| invalid())?;
                let (seconds, fraction) = parts[2].split_once('.').unwrap_or((parts[2], ""));
                let seconds = seconds.parse::<i128>().map_err(|_| invalid())?;
                if !(0..60).contains(&minutes)
                    || !(0..60).contains(&seconds)
                    || fraction.len() > 6
                    || !fraction.bytes().all(|c| c.is_ascii_digit())
                {
                    return Err(invalid());
                }
                let micros = if fraction.is_empty() {
                    0
                } else {
                    format!("{fraction:0<6}")
                        .parse::<i128>()
                        .map_err(|_| invalid())?
                };
                let value = hours
                    .checked_mul(3600)
                    .and_then(|v| v.checked_add(minutes.checked_mul(60)?))
                    .and_then(|v| v.checked_add(seconds))
                    .and_then(|v| v.checked_mul(1_000_000))
                    .and_then(|v| v.checked_add(micros))
                    .and_then(|v| v.checked_mul(sign))
                    .and_then(|v| i64::try_from(v).ok())
                    .ok_or_else(invalid)?;
                result.microseconds = result.microseconds.checked_add(value).ok_or_else(invalid)?;
            } else {
                let value = token.parse::<i64>().map_err(|_| invalid())?;
                match words.next().ok_or_else(invalid)? {
                    "year" | "years" => {
                        let n = value
                            .checked_mul(12)
                            .and_then(|n| i32::try_from(n).ok())
                            .ok_or_else(invalid)?;
                        result.months = result.months.checked_add(n).ok_or_else(invalid)?;
                    }
                    "mon" | "mons" => {
                        result.months = result
                            .months
                            .checked_add(i32::try_from(value).map_err(|_| invalid())?)
                            .ok_or_else(invalid)?;
                    }
                    "day" | "days" => {
                        result.days = result
                            .days
                            .checked_add(i32::try_from(value).map_err(|_| invalid())?)
                            .ok_or_else(invalid)?;
                    }
                    "microsecond" | "microseconds" => {
                        result.microseconds =
                            result.microseconds.checked_add(value).ok_or_else(invalid)?;
                    }
                    _ => return Err(invalid()),
                }
            }
        }
        if text.trim().is_empty() {
            return Err(invalid());
        }
        Ok(result)
    }
}
