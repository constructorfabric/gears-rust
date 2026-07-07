//! Tenant-configurable FX **revaluation mode** (VHP-1986, design 06 §4.5 /
//! §13 F2): whether BSS runs the period-end **unrealized revaluation** for a
//! tenant, or defers to the tenant's ERP. Pure domain — the literal, the parse,
//! and the fail-safe default; the effective row is loaded by the repo (`infra`)
//! and this value type is its shape. No infra / DB imports (dylint DE0301).

use toolkit_macros::domain_model;

/// Revaluation-mode parse failure (a corrupt stored value, or a bad write).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RevaluationModeError {
    /// The stored / wire value was neither `MODE_A` nor `MODE_B`.
    #[error("unknown revaluation mode: {0}")]
    UnknownMode(String),
}

/// Who is the ledger of record for period-end FX remeasurement (design 06 §4.5 /
/// §13 F2 — ASC 830 / IAS 21 requires remeasurement in whatever ledger produces
/// the reporting balances).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RevaluationMode {
    /// The tenant's **ERP / GL** is the ledger of record and performs the
    /// period-end revaluation; BSS must **not** revalue (it would double-count).
    /// The default + fail-safe: an un-configured tenant is never revalued by BSS.
    #[default]
    ModeA,
    /// **BSS is the ledger of record** with open multi-currency monetary
    /// positions; BSS runs the period-end unrealized revaluation (`FX_UNREALIZED`).
    ModeB,
}

impl RevaluationMode {
    /// Stored / wire literal for [`Self::ModeA`].
    pub const MODE_A: &'static str = "MODE_A";
    /// Stored / wire literal for [`Self::ModeB`].
    pub const MODE_B: &'static str = "MODE_B";

    /// The stored / wire literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModeA => Self::MODE_A,
            Self::ModeB => Self::MODE_B,
        }
    }

    /// Parse the stored / wire literal.
    ///
    /// # Errors
    /// [`RevaluationModeError::UnknownMode`] for any other string.
    pub fn parse(s: &str) -> Result<Self, RevaluationModeError> {
        match s {
            Self::MODE_A => Ok(Self::ModeA),
            Self::MODE_B => Ok(Self::ModeB),
            other => Err(RevaluationModeError::UnknownMode(other.to_owned())),
        }
    }

    /// Whether BSS runs the period-end unrealized revaluation for a tenant in this
    /// mode. Only [`Self::ModeB`] (BSS = ledger of record) revalues; [`Self::ModeA`]
    /// defers to the tenant's ERP (design 06 §4.5 / §13 F2).
    #[must_use]
    pub const fn revalues(self) -> bool {
        matches!(self, Self::ModeB)
    }

    /// The fleet default applied to a tenant with NO explicit mode row (VHP-1986):
    /// the global `fx.revaluation_enabled` flag — `true` ⇒ [`Self::ModeB`] (the
    /// design-06 §13 F2 "Mode B default-on" fleet behaviour), `false` ⇒
    /// [`Self::ModeA`] (fail-safe off, the v1 prod default). An explicit per-tenant
    /// row OVERRIDES this (so a tenant can opt out even when the fleet is on).
    #[must_use]
    pub const fn fleet_default(global_revaluation_enabled: bool) -> Self {
        if global_revaluation_enabled {
            Self::ModeB
        } else {
            Self::ModeA
        }
    }
}

#[cfg(test)]
#[path = "revaluation_mode_tests.rs"]
mod tests;
