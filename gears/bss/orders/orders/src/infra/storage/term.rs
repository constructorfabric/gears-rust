//! Storage mapping of authored term intent (D-193; `bss_orders__term_valid`): the
//! `(term_kind, authored_term, term_duration)` columns of a working line. The domain keeps
//! [`AuthoredTerm`]; only this module knows the column shapes.
use bss_orders_sdk::authoring::{AuthoredTerm, BillingCycle};
use serde::Deserialize;
use serde_json::Value;

use super::interval::CalendarInterval;
use crate::infra::diagnostics::serde_category;

/// The stored term columns of one working line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermColumns {
    /// `term_kind`: `missing`, `rolling` or `finite`.
    pub kind: &'static str,
    /// `authored_term`: the authored intent exactly as authored.
    pub authored: Value,
    /// `term_duration`: PostgreSQL interval text in exact calendar units; none for a missing or
    /// rolling term, or for a period count whose cycle is not authored yet.
    pub duration: Option<String>,
}

/// A term value outside the stored D-193 shape. It carries the serde category only, never the
/// serde message, which can quote stored content (D-44).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StoredShapeError {
    /// A stored `term_kind` outside the closed set (the database CHECK makes this unreachable).
    #[error("stored term_kind outside the closed set")]
    UnknownKind,
    /// A stored `authored_term` that does not decode under its `term_kind`.
    #[error("stored authored_term does not decode under term_kind {kind}")]
    Undecodable {
        /// The stored `term_kind`.
        kind: &'static str,
        /// The serde failure category ([`serde_category`]).
        category: &'static str,
    },
    /// An authored term with no interval representation (`AuthoredTerm::validate` refuses it
    /// first, so this is unreachable on a validated value).
    #[error("authored term has no stored interval representation")]
    Unrepresentable,
}

/// The stored columns of an authored term under the authored cycle. A period count without an
/// authored cycle keeps its intent and no interval; the interval follows the cycle once authored.
///
/// # Errors
/// [`StoredShapeError::Unrepresentable`] for a term `AuthoredTerm::validate` refuses.
pub fn term_columns(
    term: Option<&AuthoredTerm>,
    cycle: Option<BillingCycle>,
) -> Result<TermColumns, StoredShapeError> {
    let unrepresentable = |_| StoredShapeError::Unrepresentable;
    Ok(match term {
        None => TermColumns {
            kind: "missing",
            authored: serde_json::json!({"kind": "missing"}),
            duration: None,
        },
        Some(AuthoredTerm::Rolling) => TermColumns {
            kind: "rolling",
            authored: serde_json::json!({"kind": "rolling"}),
            duration: None,
        },
        Some(t @ AuthoredTerm::Periods { count }) => {
            let months = match cycle {
                Some(BillingCycle::Month) => Some(i64::from(*count)),
                Some(BillingCycle::Year) => Some(i64::from(*count) * 12),
                None => None,
            };
            let duration = months
                .map(|months| {
                    Ok::<_, StoredShapeError>(
                        CalendarInterval {
                            months: i32::try_from(months).map_err(unrepresentable)?,
                            days: 0,
                            microseconds: 0,
                        }
                        .postgres(),
                    )
                })
                .transpose()?;
            TermColumns {
                kind: "finite",
                authored: term_json(t)?,
                duration,
            }
        }
        Some(
            t @ AuthoredTerm::Calendar {
                years,
                months,
                days,
                microseconds,
            },
        ) => TermColumns {
            kind: "finite",
            authored: term_json(t)?,
            duration: Some(
                CalendarInterval {
                    months: i32::try_from(i64::from(*years) * 12 + i64::from(*months))
                        .map_err(unrepresentable)?,
                    days: i32::try_from(*days).map_err(unrepresentable)?,
                    microseconds: i64::try_from(*microseconds).map_err(unrepresentable)?,
                }
                .postgres(),
            ),
        },
    })
}

/// The authored term a stored row carries; `None` is the missing intent.
///
/// # Errors
/// A stored shape outside the closed D-193 set (the database CHECK makes this unreachable).
pub fn stored_term(kind: &str, authored: &Value) -> Result<Option<AuthoredTerm>, StoredShapeError> {
    let kind = match kind {
        "missing" => return Ok(None),
        "rolling" => "rolling",
        "finite" => "finite",
        _ => return Err(StoredShapeError::UnknownKind),
    };
    AuthoredTerm::deserialize(authored)
        .map(Some)
        .map_err(|error| StoredShapeError::Undecodable {
            kind,
            category: serde_category(&error),
        })
}

fn term_json(term: &AuthoredTerm) -> Result<Value, StoredShapeError> {
    serde_json::to_value(term).map_err(|_| StoredShapeError::Unrepresentable)
}

#[cfg(test)]
#[path = "term_tests.rs"]
mod tests;
