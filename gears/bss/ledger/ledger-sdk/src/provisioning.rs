//! Seller-provisioning request/response value types for the in-process
//! data-access API. Infrastructure-free: the REST DTOs (in the gear) own
//! serde/utoipa and map onto these. All amounts are `i64` minor units.

use uuid::Uuid;

use crate::enums::{AccountClass, Side};

/// Fiscal-calendar granularity. MVP supports monthly periods only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    Month,
}

impl Granularity {
    /// The stored literal (`Month => "MONTH"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Month => "MONTH",
        }
    }

    /// Parse a stored literal back to a granularity, or `None` if unknown.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "MONTH" => Some(Self::Month),
            _ => None,
        }
    }
}

/// The fiscal-calendar config seeded for a legal entity.
#[derive(Clone, Debug)]
pub struct FiscalCalendarSpec {
    pub timezone: String,
    pub granularity: Granularity,
    pub fy_start_month: u8,
    /// The legal entity's functional (books) currency, ISO-4217 (S5-F3). `None`
    /// seeds a single-currency tenant (no FX); set it to enable cross-currency FX.
    pub functional_currency: Option<String>,
}

/// One chart-of-accounts row to seed.
#[derive(Clone, Debug)]
pub struct ProvisionAccount {
    pub account_class: AccountClass,
    pub currency: String,
    pub revenue_stream: Option<String>,
    pub normal_side: Side,
    pub may_go_negative: bool,
}

/// One non-ISO currency-scale row to seed.
#[derive(Clone, Debug)]
pub struct ProvisionCurrencyScale {
    pub currency: String,
    /// Minor-unit scale (digits after the decimal point), e.g. `2` for USD,
    /// `8` for BTC. Always a small non-negative number — `u8` makes a negative
    /// or absurdly large scale unrepresentable.
    pub minor_units: u8,
    pub source: String,
    /// Per-currency plausible maximum in MAJOR units, governing the `i64`
    /// headroom guard at registration. `None` requests the default `10^12`
    /// (max scale 6); a higher-precision currency (e.g. BTC scale 8) passes
    /// a smaller cap (e.g. `21_000_000`) so its scale fits the headroom.
    pub plausible_max_major: Option<i64>,
}

/// A full seller-provisioning request: the chart of accounts, non-ISO
/// currency scales, and the fiscal-calendar config to seed in one txn. The
/// legal entity is NOT supplied — v1 is one legal entity per tenant, derived
/// server-side (= `tenant_id`); the DB column is retained for future multi-LE.
#[derive(Clone, Debug)]
pub struct ProvisionRequest {
    pub tenant_id: Uuid,
    pub accounts: Vec<ProvisionAccount>,
    pub currency_scales: Vec<ProvisionCurrencyScale>,
    pub fiscal_calendar: FiscalCalendarSpec,
}

/// A chart-of-accounts entry: its coordinate plus the persistent `account_id`
/// callers post to / read balances for. Returned both by provisioning (the
/// accounts a call created) and by `list_accounts` (the full chart).
#[derive(Clone, Debug)]
pub struct AccountInfo {
    pub account_id: Uuid,
    pub account_class: AccountClass,
    pub currency: String,
    pub revenue_stream: Option<String>,
    pub lifecycle_state: String,
}

/// The accounts a provisioning call CREATED + per-grain created-vs-existing
/// counts. The full chart of accounts (with ids) is read via `list_accounts`.
#[derive(Clone, Debug)]
pub struct ProvisionOutcome {
    /// The chart-of-accounts entries THIS call created (empty on a pure re-call).
    pub accounts: Vec<AccountInfo>,
    pub accounts_created: u32,
    pub accounts_existing: u32,
    pub scales_created: u32,
    pub scales_existing: u32,
    pub calendar_created: bool,
    pub period_id: String,
    pub period_created: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granularity_parses_known_literal() {
        assert_eq!(Granularity::parse("MONTH"), Some(Granularity::Month));
        assert!(Granularity::parse("WEEK").is_none());
        assert_eq!(Granularity::Month.as_str(), "MONTH");
    }
}
