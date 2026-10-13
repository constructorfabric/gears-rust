//! Pure persisted dispute phase and variant types.

use toolkit_macros::domain_model;

/// A dispute phase (the `phase` of one chargeback event). `opened` ships in
/// Group B; `won`/`lost` arrive in Group C; `partial` is behind a flag
/// (design §2). The literal is the third token of `source_business_id`
/// (`dispute_id:cycle:phase`) and is persisted via the journal, not stored
/// on the dispute row except as `last_phase`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputePhase {
    /// The dispute was raised — cash moved to a hold (`CASH_HOLD`) or the
    /// receivable reclassed to `DISPUTED` (`AR_RECLASS`).
    Opened,
    /// The dispute resolved in the seller's favour (Group C).
    Won,
    /// The dispute resolved against the seller — a clawback (Group C).
    Lost,
    /// A split outcome (behind a flag; Group C).
    Partial,
}

impl DisputePhase {
    /// Stable uppercase wire literal (the `last_phase` column value + the third
    /// `source_business_id` token).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "OPENED",
            Self::Won => "WON",
            Self::Lost => "LOST",
            Self::Partial => "PARTIAL",
        }
    }

    /// Parse a stored / wire phase literal (case-insensitive), or `None` for an
    /// unknown value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "OPENED" => Some(Self::Opened),
            "WON" => Some(Self::Won),
            "LOST" => Some(Self::Lost),
            "PARTIAL" => Some(Self::Partial),
            _ => None,
        }
    }
}

/// The dispute variant the LEDGER records at `opened` (design §2): it pins how
/// the `opened` move posts AND how `won`/`lost` branch. Chosen from
/// `funds_at_open`, NOT tenant/plugin policy.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeVariant {
    /// Card rails withheld the cash (`funds_at_open = withheld`): the settled
    /// cash is moved into a `DISPUTE_HOLD`.
    CashHold,
    /// Invoice / ACH did not move the cash (`funds_at_open = not_moved`): the
    /// receivable is reclassed `ACTIVE → DISPUTED` (AR-class-neutral).
    ArReclass,
}

impl DisputeVariant {
    /// Stable uppercase wire literal (the `variant` column value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CashHold => "CASH_HOLD",
            Self::ArReclass => "AR_RECLASS",
        }
    }

    /// Parse a stored variant literal (case-insensitive), or `None` for an
    /// unknown value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "CASH_HOLD" => Some(Self::CashHold),
            "AR_RECLASS" => Some(Self::ArReclass),
            _ => None,
        }
    }
}

/// The persisted dispute state a transition is checked against: the payment it
/// belongs to and its last phase and cycle.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedDispute<'a> {
    pub payment_id: &'a str,
    pub last_phase: DisputePhase,
    pub cycle: i32,
}

/// Why a requested dispute phase cannot follow the observed state.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DisputeTransitionError {
    /// A first opening must start cycle 1.
    #[error("dispute {dispute_id}: first opening must be cycle 1, not {cycle}")]
    FirstCycle { dispute_id: String, cycle: i32 },
    /// A reopening names another payment than the dispute's.
    #[error("dispute {dispute_id} belongs to payment {observed}, not {requested}")]
    ForeignPayment {
        dispute_id: String,
        observed: String,
        requested: String,
    },
    /// A reopening must follow a WON/LOST cycle, at the next cycle.
    #[error(
        "dispute {dispute_id}: reopen at cycle {cycle} requires WON/LOST at cycle \
         {cycle_before}; observed {} at cycle {observed_cycle}",
        observed_phase.as_str()
    )]
    Reopen {
        dispute_id: String,
        cycle: i32,
        cycle_before: i32,
        observed_phase: DisputePhase,
        observed_cycle: i32,
    },
    /// An outcome needs an opened cycle to resolve.
    #[error("dispute {dispute_id} has no opened cycle to resolve")]
    NoOpenedCycle { dispute_id: String },
    /// `OPENED` is not an outcome.
    #[error("dispute {dispute_id}: {} is not an outcome", phase.as_str())]
    NotAnOutcome {
        dispute_id: String,
        phase: DisputePhase,
    },
    /// An outcome resolves only the OPENED cycle it names.
    #[error(
        "dispute {dispute_id}: outcome {} requires OPENED at cycle {cycle}; observed {} at \
         cycle {observed_cycle}",
        phase.as_str(),
        observed_phase.as_str()
    )]
    OutcomeNotOpen {
        dispute_id: String,
        phase: DisputePhase,
        cycle: i32,
        observed_phase: DisputePhase,
        observed_cycle: i32,
    },
}

/// The dispute state machine (design §2): may `phase` at `cycle` for
/// `payment_id` follow `observed` (`None` = no dispute row yet)?
///
/// - `OPENED` starts cycle 1 on a new dispute, or reopens a WON/LOST dispute of
///   the same payment at the next cycle.
/// - `WON` / `LOST` / `PARTIAL` resolve the OPENED cycle they name.
///
/// # Errors
/// The [`DisputeTransitionError`] naming the broken rule.
pub fn check_transition(
    dispute_id: &str,
    observed: Option<ObservedDispute<'_>>,
    payment_id: &str,
    phase: DisputePhase,
    cycle: i32,
) -> Result<(), DisputeTransitionError> {
    match phase {
        DisputePhase::Opened => check_open(dispute_id, observed, payment_id, cycle),
        outcome => check_outcome(dispute_id, observed, outcome, cycle),
    }
}

/// The `OPENED` arm of [`check_transition`].
///
/// # Errors
/// [`DisputeTransitionError::FirstCycle`], [`DisputeTransitionError::ForeignPayment`]
/// or [`DisputeTransitionError::Reopen`].
pub fn check_open(
    dispute_id: &str,
    observed: Option<ObservedDispute<'_>>,
    payment_id: &str,
    cycle: i32,
) -> Result<(), DisputeTransitionError> {
    let Some(observed) = observed else {
        if cycle == 1 {
            return Ok(());
        }
        return Err(DisputeTransitionError::FirstCycle {
            dispute_id: dispute_id.to_owned(),
            cycle,
        });
    };
    if observed.payment_id != payment_id {
        return Err(DisputeTransitionError::ForeignPayment {
            dispute_id: dispute_id.to_owned(),
            observed: observed.payment_id.to_owned(),
            requested: payment_id.to_owned(),
        });
    }
    if matches!(observed.last_phase, DisputePhase::Won | DisputePhase::Lost)
        && observed.cycle.checked_add(1) == Some(cycle)
    {
        return Ok(());
    }
    Err(DisputeTransitionError::Reopen {
        dispute_id: dispute_id.to_owned(),
        cycle,
        cycle_before: cycle.saturating_sub(1),
        observed_phase: observed.last_phase,
        observed_cycle: observed.cycle,
    })
}

/// The outcome arm of [`check_transition`]; `OPENED` is refused here.
///
/// # Errors
/// [`DisputeTransitionError::NotAnOutcome`], [`DisputeTransitionError::NoOpenedCycle`]
/// or [`DisputeTransitionError::OutcomeNotOpen`].
pub fn check_outcome(
    dispute_id: &str,
    observed: Option<ObservedDispute<'_>>,
    phase: DisputePhase,
    cycle: i32,
) -> Result<(), DisputeTransitionError> {
    if phase == DisputePhase::Opened {
        return Err(DisputeTransitionError::NotAnOutcome {
            dispute_id: dispute_id.to_owned(),
            phase,
        });
    }
    let Some(observed) = observed else {
        return Err(DisputeTransitionError::NoOpenedCycle {
            dispute_id: dispute_id.to_owned(),
        });
    };
    if observed.last_phase == DisputePhase::Opened && observed.cycle == cycle {
        return Ok(());
    }
    Err(DisputeTransitionError::OutcomeNotOpen {
        dispute_id: dispute_id.to_owned(),
        phase,
        cycle,
        observed_phase: observed.last_phase,
        observed_cycle: observed.cycle,
    })
}

#[cfg(test)]
#[path = "dispute_state_tests.rs"]
mod tests;
