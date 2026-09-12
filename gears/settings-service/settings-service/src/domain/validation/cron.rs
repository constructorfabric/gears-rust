// Created: 2026-09-08 by Constructor Tech
//! The standard five-field cron dialect.
//!
//! Written here rather than taken from a crate because what the trait asks for
//! is narrow and fixed: does this string parse as a schedule in the dialect the
//! type declares. A scheduler that later runs the expression owns firing times,
//! daylight saving and the rest; this only refuses a value that no scheduler
//! could read.
//!
//! The dialect is `minute hour day-of-month month day-of-week`, each field one
//! of `*`, a number, a range `a-b`, a step `*/n` or `a-b/n`, or a
//! comma-separated list of those. Months and weekdays also accept their usual
//! three-letter names. Seconds, years, `?`, `L`, `W` and `#` belong to other
//! dialects and are refused here rather than silently accepted.

/// One field's permitted range and names.
struct Field {
    name: &'static str,
    min: u32,
    max: u32,
    names: &'static [&'static str],
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

const FIELDS: [Field; 5] = [
    Field {
        name: "minute",
        min: 0,
        max: 59,
        names: &[],
    },
    Field {
        name: "hour",
        min: 0,
        max: 23,
        names: &[],
    },
    Field {
        name: "day of month",
        min: 1,
        max: 31,
        names: &[],
    },
    Field {
        name: "month",
        min: 1,
        max: 12,
        names: &MONTHS,
    },
    // Both 0 and 7 name Sunday, as every implementation of this dialect does.
    Field {
        name: "day of week",
        min: 0,
        max: 7,
        names: &DAYS,
    },
];

/// The dialect this module parses, as a value type declares it.
pub const STANDARD: &str = "standard";

/// Whether `dialect` is one this gear can check.
#[must_use]
pub fn is_known(dialect: &str) -> bool {
    dialect.eq_ignore_ascii_case(STANDARD)
}

/// Parse a cron expression in the standard five-field dialect.
///
/// # Errors
/// A sentence naming what refused it, suitable as a field-level message.
pub fn parse(expression: &str) -> Result<(), String> {
    let trimmed = expression.trim();
    if trimmed.is_empty() {
        return Err("a cron expression has five fields; this one is empty".to_owned());
    }
    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    if fields.len() != FIELDS.len() {
        return Err(format!(
            "the standard dialect has five fields (minute hour day-of-month month day-of-week); \
             this one has {}",
            fields.len()
        ));
    }
    for (field, spec) in fields.iter().zip(FIELDS.iter()) {
        parse_field(field, spec)?;
    }
    Ok(())
}

fn parse_field(field: &str, spec: &Field) -> Result<(), String> {
    if field.is_empty() {
        return Err(format!("the {} field is empty", spec.name));
    }
    for part in field.split(',') {
        parse_part(part, spec)?;
    }
    Ok(())
}

fn parse_part(part: &str, spec: &Field) -> Result<(), String> {
    let (range, step) = match part.split_once('/') {
        Some((range, step)) => {
            let parsed: u32 = step.parse().map_err(|_| {
                format!(
                    "the {} field has `{step}` as a step, which is not a number",
                    spec.name
                )
            })?;
            if parsed == 0 {
                return Err(format!("the {} field has a step of zero", spec.name));
            }
            (range, Some(parsed))
        }
        None => (part, None),
    };
    if range == "*" {
        return Ok(());
    }
    // A step over a single value rather than a range or `*` is not the
    // dialect: `5/10` says nothing a reader can act on.
    if let Some((from, to)) = range.split_once('-') {
        let from = value_of(from, spec)?;
        let to = value_of(to, spec)?;
        if from > to {
            return Err(format!(
                "the {} field has the range `{range}`, which runs backwards",
                spec.name
            ));
        }
        return Ok(());
    }
    value_of(range, spec)?;
    if step.is_some() {
        return Err(format!(
            "the {} field steps over `{range}`, which is a single value rather than a range \
             or `*`",
            spec.name
        ));
    }
    Ok(())
}

fn value_of(token: &str, spec: &Field) -> Result<u32, String> {
    if token.is_empty() {
        return Err(format!("the {} field has an empty value", spec.name));
    }
    let lower = token.to_ascii_lowercase();
    if let Some(index) = spec.names.iter().position(|n| *n == lower) {
        let numeric = u32::try_from(index).unwrap_or(0);
        // Month names start at one, weekday names at zero.
        return Ok(if spec.min == 1 { numeric + 1 } else { numeric });
    }
    let number: u32 = token.parse().map_err(|_| {
        if spec.names.is_empty() {
            format!(
                "the {} field has `{token}`, which is not a number in {}-{}",
                spec.name, spec.min, spec.max
            )
        } else {
            format!(
                "the {} field has `{token}`, which is neither a number in {}-{} nor a name",
                spec.name, spec.min, spec.max
            )
        }
    })?;
    if number < spec.min || number > spec.max {
        return Err(format!(
            "the {} field has `{number}`, outside {}-{}",
            spec.name, spec.min, spec.max
        ));
    }
    Ok(number)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "cron_tests.rs"]
mod cron_tests;
