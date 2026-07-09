//! Tenant-configurable invoice-posting policies (VHP-1853, design 01a §4.2 /
//! §4.4): the missing-mapping mode and the AR-aging bucket thresholds. Pure
//! domain — the parsing, validation, and defaults; the effective row is loaded
//! by the repo (`infra`), and these value types are its shape. No infra / DB
//! imports (dylint DE0301).

use toolkit_macros::domain_model;

/// Policy parse / validation failure (a corrupt stored value, or a bad write).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// `missing_mapping_mode` was neither `SUSPENSE` nor `HARD_BLOCK`.
    #[error("unknown missing-mapping mode: {0}")]
    UnknownMissingMappingMode(String),
    /// `ar_aging_thresholds` was empty, non-numeric, non-positive, non-increasing,
    /// or exceeded the bucket cap.
    #[error("invalid AR-aging thresholds: {0}")]
    InvalidAgingThresholds(String),
}

/// What to do with an invoice item whose GL target cannot be resolved.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MissingMappingMode {
    /// Route the unmapped item to `SUSPENSE` / `PENDING` (an operator reclassifies
    /// before the period can close). The default + the prior hardcoded behaviour.
    #[default]
    Suspense,
    /// Reject the whole post with `ACCOUNT_MAPPING_MISSING` — the tenant requires
    /// every line to map up-front.
    HardBlock,
}

impl MissingMappingMode {
    /// Stored / wire literal for [`Self::Suspense`].
    pub const SUSPENSE: &'static str = "SUSPENSE";
    /// Stored / wire literal for [`Self::HardBlock`].
    pub const HARD_BLOCK: &'static str = "HARD_BLOCK";

    /// The stored / wire literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Suspense => Self::SUSPENSE,
            Self::HardBlock => Self::HARD_BLOCK,
        }
    }

    /// Parse the stored / wire literal.
    ///
    /// # Errors
    /// [`PolicyError::UnknownMissingMappingMode`] for any other string.
    pub fn parse(s: &str) -> Result<Self, PolicyError> {
        match s {
            Self::SUSPENSE => Ok(Self::Suspense),
            Self::HARD_BLOCK => Ok(Self::HardBlock),
            other => Err(PolicyError::UnknownMissingMappingMode(other.to_owned())),
        }
    }
}

/// Strict-increasing positive day-count upper bounds for the AR-aging buckets.
/// `[30, 60, 90]` (the default) yields `current / 1-30 / 31-60 / 61-90 / 90+`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgingThresholds(Vec<i64>);

impl Default for AgingThresholds {
    fn default() -> Self {
        Self(vec![30, 60, 90])
    }
}

impl AgingThresholds {
    /// The most bucket boundaries a tenant may configure — a sanity cap, well
    /// above any realistic aging schedule.
    pub const MAX_BOUNDS: usize = 12;

    /// Build from upper bounds, validating non-empty, all `> 0`, strictly
    /// increasing, and within [`Self::MAX_BOUNDS`].
    ///
    /// # Errors
    /// [`PolicyError::InvalidAgingThresholds`] when any rule is violated.
    pub fn new(bounds: Vec<i64>) -> Result<Self, PolicyError> {
        if bounds.is_empty() {
            return Err(PolicyError::InvalidAgingThresholds(
                "must have at least one boundary".to_owned(),
            ));
        }
        if bounds.len() > Self::MAX_BOUNDS {
            return Err(PolicyError::InvalidAgingThresholds(format!(
                "at most {} boundaries (got {})",
                Self::MAX_BOUNDS,
                bounds.len()
            )));
        }
        if bounds[0] <= 0 {
            return Err(PolicyError::InvalidAgingThresholds(
                "boundaries must be positive day counts".to_owned(),
            ));
        }
        if bounds.windows(2).any(|w| w[1] <= w[0]) {
            return Err(PolicyError::InvalidAgingThresholds(
                "boundaries must be strictly increasing".to_owned(),
            ));
        }
        Ok(Self(bounds))
    }

    /// Parse a CSV of bounds (e.g. `"30,60,90"`).
    ///
    /// # Errors
    /// [`PolicyError::InvalidAgingThresholds`] on a non-numeric token or a failed
    /// [`Self::new`] invariant.
    pub fn parse_csv(s: &str) -> Result<Self, PolicyError> {
        let bounds = s
            .split(',')
            .map(|t| {
                t.trim().parse::<i64>().map_err(|e| {
                    PolicyError::InvalidAgingThresholds(format!(
                        "non-numeric boundary '{}': {e}",
                        t.trim()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(bounds)
    }

    /// Render to the stored CSV form (e.g. `"30,60,90"`).
    #[must_use]
    pub fn to_csv(&self) -> String {
        self.0
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The upper bounds, strictly increasing.
    #[must_use]
    pub fn bounds(&self) -> &[i64] {
        &self.0
    }
}

/// The effective tenant invoice-posting policy. [`Default`] reproduces the prior
/// hardcoded behaviour (`Suspense` + `[30, 60, 90]`), applied when a tenant has
/// no policy row.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PostingPolicy {
    /// What to do with an unmapped invoice item.
    pub missing_mapping_mode: MissingMappingMode,
    /// AR-aging bucket boundaries.
    pub aging_thresholds: AgingThresholds,
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
