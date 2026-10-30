//! D-193 calendar intent conversion; no billing-policy or anchor selection.
use super::CodecError;
use crate::authoring::AuthoredTerm;
use bss_pricing_sdk::{acceptance::Term, terms::BillingCycle};

/// Capture retains calendar units and explicit rolling intent separately from missing input.
/// The time remainder is in microseconds, the unit of [`AuthoredTerm`] it is built from (D-220).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermIntent {
    Missing,
    Rolling,
    Periods(u32),
    Calendar {
        years: u64,
        months: u64,
        days: u64,
        microseconds: u64,
    },
}
/// The one conversion from the authored term: absence is [`TermIntent::Missing`], never rolling,
/// and every unit widens losslessly with its unit unchanged.
impl From<Option<AuthoredTerm>> for TermIntent {
    fn from(term: Option<AuthoredTerm>) -> Self {
        match term {
            None => Self::Missing,
            Some(AuthoredTerm::Rolling) => Self::Rolling,
            Some(AuthoredTerm::Periods { count }) => Self::Periods(count),
            Some(AuthoredTerm::Calendar {
                years,
                months,
                days,
                microseconds,
            }) => Self::Calendar {
                years: u64::from(years),
                months: u64::from(months),
                days: u64::from(days),
                microseconds,
            },
        }
    }
}
impl TermIntent {
    /// Convert only exact positive invoice periods. Missing does not mean rolling.
    /// # Errors
    /// `Incomplete` for a missing intent, or a missing billing cycle where periods need one;
    /// `Unsupported` for a zero count, day/time remainders, nonintegral years and overflow.
    #[expect(
        clippy::integer_division,
        reason = "every division is guarded by an exact-divisibility check, so nothing rounds"
    )]
    pub fn resolve(self, cycle: Option<BillingCycle>) -> Result<Term, CodecError> {
        match self {
            Self::Missing => Err(CodecError::Incomplete),
            Self::Rolling => Ok(Term::Rolling),
            Self::Periods(_) | Self::Calendar { .. } if cycle.is_none() => {
                Err(CodecError::Incomplete)
            }
            Self::Periods(count) if count > 0 => Ok(Term::FixedPeriods { count }),
            Self::Calendar {
                years,
                months,
                days: 0,
                microseconds: 0,
            } => {
                let months = years
                    .checked_mul(12)
                    .and_then(|n| n.checked_add(months))
                    .ok_or(CodecError::Unsupported)?;
                let count = match cycle {
                    Some(BillingCycle::Month) => months,
                    Some(BillingCycle::Year) if months % 12 == 0 => months / 12,
                    _ => return Err(CodecError::Unsupported),
                };
                let count = u32::try_from(count).map_err(|_| CodecError::Unsupported)?;
                if count == 0 {
                    return Err(CodecError::Unsupported);
                }
                Ok(Term::FixedPeriods { count })
            }
            _ => Err(CodecError::Unsupported),
        }
    }
}
