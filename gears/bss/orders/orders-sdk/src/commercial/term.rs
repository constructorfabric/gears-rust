//! D-193 calendar intent conversion; no billing-policy or anchor selection.
use super::CodecError;
use bss_pricing_sdk::{acceptance::Term, terms::BillingCycle};

/// Capture retains calendar units and explicit rolling intent separately from missing input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermIntent {
    Missing,
    Rolling,
    Periods(u32),
    Calendar {
        years: u64,
        months: u64,
        days: u64,
        nanoseconds: u64,
    },
}
impl TermIntent {
    /// Convert only exact positive invoice periods. Missing does not mean rolling.
    /// # Errors
    /// Rejects missing intent/cycle, day/time remainders, nonintegral years and overflow.
    #[allow(clippy::integer_division)] // Guarded by exact divisibility; no rounding.
    pub fn resolve(self, cycle: Option<BillingCycle>) -> Result<Term, CodecError> {
        match self {
            Self::Missing => Err(CodecError::Incomplete),
            Self::Rolling => Ok(Term::Rolling),
            Self::Periods(count) if count > 0 && cycle.is_some() => {
                Ok(Term::FixedPeriods { count })
            }
            Self::Calendar {
                years,
                months,
                days: 0,
                nanoseconds: 0,
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
