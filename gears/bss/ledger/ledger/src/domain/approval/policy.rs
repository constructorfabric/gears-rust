//! Pure currency-aware dual-control policy. Callers supply the existing valuation
//! basis; this module never converts currencies or consults the live registry.
use super::ApprovalKind;
use bss_ledger_sdk::{CurrencySpec, PostedMoney};
use chrono::{Datelike, NaiveDate, Weekday};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use time::OffsetDateTime;
use toolkit_macros::domain_model;

pub const D2_DEFAULT_RULE: &str = "per_currency_platform_default";
pub const DEFAULT_A6_BACKDATING_BIZ_DAYS: i32 = 5;
pub const DEFAULT_PENDING_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const A6_MIN_DAYS: i32 = 1;
pub const A6_MAX_DAYS: i32 = 30;

/// Validated per-currency D2 overrides: at most one threshold per currency
/// code, each within its scale-derived bounds, kept in currency-code order.
/// Built only by [`D2Thresholds::try_new`], so a duplicate currency cannot be
/// represented and a holder never re-validates.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct D2Thresholds(BTreeMap<String, PostedMoney>);

impl D2Thresholds {
    /// No per-currency override (the platform default).
    pub const EMPTY: Self = Self(BTreeMap::new());

    /// Validate the thresholds once (see [`validate_thresholds`]).
    ///
    /// # Errors
    /// [`PolicyConfigError::DuplicateCurrency`] / [`PolicyConfigError::MetadataConflict`] when a
    /// currency is configured twice (same or different scale);
    /// [`PolicyConfigError::D2OutOfRange`] when a threshold is outside its bounds.
    pub fn try_new(values: Vec<PostedMoney>) -> Result<Self, PolicyConfigError> {
        validate_thresholds(&values)?;
        Ok(Self(
            values
                .into_iter()
                .map(|value| (value.currency().code().to_owned(), value))
                .collect(),
        ))
    }

    /// The threshold configured for `currency`, if any.
    #[must_use]
    pub fn get(&self, currency: &str) -> Option<&PostedMoney> {
        self.0.get(currency)
    }

    /// The thresholds in currency-code order.
    pub fn iter(&self) -> impl Iterator<Item = &PostedMoney> {
        self.0.values()
    }

    /// Number of configured currencies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when no currency is overridden.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The thresholds in currency-code order.
    #[must_use]
    pub fn into_vec(self) -> Vec<PostedMoney> {
        self.0.into_values().collect()
    }
}

/// Currency overrides; missing currencies resolve the symbolic platform default.
///
/// The fields are private: a policy is built only by [`DualControlPolicy::try_new`]
/// (or is [`DualControlPolicy::DEFAULT`]), so a holder never re-validates it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DualControlPolicy {
    d2_thresholds: D2Thresholds,
    a6_backdating_biz_days: i32,
    pending_ttl_seconds: i64,
}
impl DualControlPolicy {
    pub const DEFAULT: Self = Self {
        d2_thresholds: D2Thresholds::EMPTY,
        a6_backdating_biz_days: DEFAULT_A6_BACKDATING_BIZ_DAYS,
        pending_ttl_seconds: DEFAULT_PENDING_TTL_SECONDS,
    };

    /// Build a policy from validated thresholds, checking A6 and the TTL once.
    ///
    /// # Errors
    /// [`PolicyConfigError::A6OutOfRange`] or [`PolicyConfigError::TtlNotPositive`] when A6 or
    /// the TTL is outside its allowed range.
    pub fn try_new(
        d2_thresholds: D2Thresholds,
        a6: i32,
        ttl: i64,
    ) -> Result<Self, PolicyConfigError> {
        validate_limits(a6, ttl)?;
        Ok(Self {
            d2_thresholds,
            a6_backdating_biz_days: a6,
            pending_ttl_seconds: ttl,
        })
    }

    /// The validated per-currency D2 overrides.
    #[must_use]
    pub const fn d2_thresholds(&self) -> &D2Thresholds {
        &self.d2_thresholds
    }

    /// The A6 material-backdating window, in business days.
    #[must_use]
    pub const fn a6_backdating_biz_days(&self) -> i32 {
        self.a6_backdating_biz_days
    }

    /// How long a pending approval stays actionable, in seconds.
    #[must_use]
    pub const fn pending_ttl_seconds(&self) -> i64 {
        self.pending_ttl_seconds
    }
    /// Resolve against comparand metadata, never against a later registry value.
    ///
    /// The thresholds were validated when the map was built, so the lookup does
    /// not re-validate them; an A6 or TTL defect is not a D2 lookup failure.
    ///
    /// # Errors
    /// [`PolicyConfigError::MetadataConflict`] when the configured threshold
    /// for `currency` carries a different scale;
    /// [`PolicyConfigError::D2DefaultUnrepresentable`] when the default threshold cannot be
    /// expressed at the currency's scale.
    pub fn d2_threshold(&self, currency: &CurrencySpec) -> Result<PostedMoney, PolicyConfigError> {
        if let Some(value) = self.d2_thresholds.get(currency.code()) {
            if value.currency() != currency {
                return Err(PolicyConfigError::MetadataConflict {
                    currency: currency.code().into(),
                    configured_scale: value.currency().scale(),
                    other_scale: currency.scale(),
                });
            }
            return Ok(value.clone());
        }
        PostedMoney::try_new(
            Decimal::new(100_000, u32::from(currency.scale())),
            currency.clone(),
        )
        .map_err(|_| PolicyConfigError::D2DefaultUnrepresentable(currency.clone()))
    }
}
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyVersion {
    pub effective_from: OffsetDateTime,
    pub version: i64,
    pub policy: DualControlPolicy,
}
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyConfigError {
    /// A configured D2 threshold lies outside `[min, max]` at its own currency's
    /// scale (the bounds are scale-derived, so they travel with the error).
    D2OutOfRange {
        threshold: PostedMoney,
        min: Decimal,
        max: Decimal,
    },
    /// The platform default D2 threshold cannot be expressed at this currency's
    /// scale (distinct from a configured threshold being out of range).
    D2DefaultUnrepresentable(CurrencySpec),
    DuplicateCurrency(String),
    /// One currency at two stored scales: two configured thresholds, or a
    /// configured threshold and the comparand's currency.
    MetadataConflict {
        currency: String,
        configured_scale: u8,
        other_scale: u8,
    },
    A6OutOfRange(i32),
    TtlNotPositive(i64),
}

/// Existing valuation behavior, including transaction basis for derived kinds.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValuationBasis {
    Transaction,
    Functional,
}
///
/// Material backdating is gated on business days, not on an amount, so it keeps
/// its captured transaction value (never converted to the functional currency).
#[must_use]
pub fn valuation_basis(kind: ApprovalKind) -> ValuationBasis {
    match kind {
        ApprovalKind::Reverse
        | ApprovalKind::RecognitionScheduleChange
        | ApprovalKind::MaterialBackdating => ValuationBasis::Transaction,
        ApprovalKind::CreditGrant
        | ApprovalKind::ChargebackLoss
        | ApprovalKind::PayerClosure
        | ApprovalKind::PeriodReopen
        | ApprovalKind::Refund
        | ApprovalKind::ManualAdjustment
        | ApprovalKind::CreditNote
        | ApprovalKind::DebitNote => ValuationBasis::Functional,
    }
}
/// Which rule decides whether an operation of a kind needs dual control. The
/// one classification both [`amount_gated`] and [`requires_dual_control`]
/// dispatch on, so the amount-gated set cannot drift between them.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateRule {
    /// The D2 amount threshold (the gate needs amount facts).
    Amount,
    /// The A6 material-backdating window.
    Backdating,
    /// An outstanding balance on the payer being closed.
    OutstandingBalance,
    /// Always dual-controlled.
    Always,
}

/// The [`GateRule`] for `kind`. Exhaustive, so a new kind must choose its rule
/// instead of falling into the threshold check (and failing open without an
/// amount).
#[must_use]
pub const fn gate_rule(kind: ApprovalKind) -> GateRule {
    match kind {
        ApprovalKind::MaterialBackdating => GateRule::Backdating,
        ApprovalKind::PayerClosure => GateRule::OutstandingBalance,
        ApprovalKind::PeriodReopen => GateRule::Always,
        ApprovalKind::Reverse
        | ApprovalKind::CreditGrant
        | ApprovalKind::ChargebackLoss
        | ApprovalKind::RecognitionScheduleChange
        | ApprovalKind::Refund
        | ApprovalKind::ManualAdjustment
        | ApprovalKind::CreditNote
        | ApprovalKind::DebitNote => GateRule::Amount,
    }
}

/// Whether `kind` is decided by the D2 amount threshold (and so needs amount
/// facts at the gate): [`GateRule::Amount`].
#[must_use]
pub const fn amount_gated(kind: ApprovalKind) -> bool {
    matches!(gate_rule(kind), GateRule::Amount)
}
/// Already-valued comparand. Resubmissions do not perform this gate.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationFacts {
    pub kind: ApprovalKind,
    pub amount: Option<PostedMoney>,
    pub effective_at: Option<NaiveDate>,
    pub has_outstanding_balance: bool,
}
#[must_use]
pub fn effective_version(versions: &[PolicyVersion], now: OffsetDateTime) -> Option<PolicyVersion> {
    versions
        .iter()
        .filter(|v| v.effective_from <= now)
        .max_by(|a, b| {
            a.effective_from
                .cmp(&b.effective_from)
                .then(a.version.cmp(&b.version))
        })
        .cloned()
}
#[must_use]
pub fn resolve_policy(versions: &[PolicyVersion], now: OffsetDateTime) -> DualControlPolicy {
    effective_version(versions, now).map_or(DualControlPolicy::DEFAULT, |v| v.policy)
}
/// Validate stored-spec bounds. New configuration must additionally validate each
/// currency against current configuration on the caller's transaction runner.
///
/// # Errors
/// [`PolicyConfigError::DuplicateCurrency`] / [`PolicyConfigError::MetadataConflict`] when a
/// currency is configured twice (same or different scale); [`PolicyConfigError::D2OutOfRange`],
/// [`PolicyConfigError::A6OutOfRange`] or [`PolicyConfigError::TtlNotPositive`] when D2, A6 or
/// the TTL is outside its allowed range.
pub fn validate_config(
    thresholds: &[PostedMoney],
    a6: i32,
    ttl: i64,
) -> Result<(), PolicyConfigError> {
    validate_thresholds(thresholds)?;
    validate_limits(a6, ttl)
}
/// Validate the A6 backdating window and the pending TTL.
///
/// # Errors
/// [`PolicyConfigError::A6OutOfRange`] or [`PolicyConfigError::TtlNotPositive`] when A6 or
/// the TTL is outside its allowed range.
pub fn validate_limits(a6: i32, ttl: i64) -> Result<(), PolicyConfigError> {
    if !(A6_MIN_DAYS..=A6_MAX_DAYS).contains(&a6) {
        return Err(PolicyConfigError::A6OutOfRange(a6));
    }
    if ttl <= 0 {
        return Err(PolicyConfigError::TtlNotPositive(ttl));
    }
    Ok(())
}
/// Validate the per-currency D2 thresholds alone: one threshold per currency
/// code, each within its scale-derived bounds.
///
/// # Errors
/// [`PolicyConfigError::DuplicateCurrency`] / [`PolicyConfigError::MetadataConflict`] when a
/// currency is configured twice (same or different scale);
/// [`PolicyConfigError::D2OutOfRange`] when a threshold is outside its bounds.
pub fn validate_thresholds(thresholds: &[PostedMoney]) -> Result<(), PolicyConfigError> {
    let mut currencies = std::collections::BTreeMap::new();
    for value in thresholds {
        let spec = value.currency();
        if let Some(previous) = currencies.insert(spec.code(), spec.scale()) {
            return Err(if previous == spec.scale() {
                PolicyConfigError::DuplicateCurrency(spec.code().into())
            } else {
                PolicyConfigError::MetadataConflict {
                    currency: spec.code().into(),
                    configured_scale: previous,
                    other_scale: spec.scale(),
                }
            });
        }
        let scale = u32::from(spec.scale());
        let (min, max) = (
            Decimal::new(10_000, scale),
            Decimal::new(100_000_000, scale),
        );
        if value.amount() < min || value.amount() > max {
            return Err(PolicyConfigError::D2OutOfRange {
                threshold: value.clone(),
                min,
                max,
            });
        }
    }
    Ok(())
}
/// Compare exact magnitude only after the existing valuation step.
///
/// # Errors
/// The [`PolicyConfigError`] of [`DualControlPolicy::d2_threshold`] when the policy's
/// thresholds are invalid or conflict with the amount's currency metadata.
pub fn requires_dual_control(
    op: &OperationFacts,
    policy: &DualControlPolicy,
    today: NaiveDate,
) -> Result<bool, PolicyConfigError> {
    Ok(match gate_rule(op.kind) {
        GateRule::Backdating => op
            .effective_at
            .is_some_and(|eff| business_days_between(eff, today) > policy.a6_backdating_biz_days),
        GateRule::OutstandingBalance => op.has_outstanding_balance,
        GateRule::Always => true,
        GateRule::Amount => match &op.amount {
            Some(amount) => {
                amount.amount().abs() >= policy.d2_threshold(amount.currency())?.amount()
            }
            None => false,
        },
    })
}

/// Count business days (Mon–Fri) after `from` up to and including `to`. `0` when
/// `from >= to`. MVP uses the Mon–Fri weekday rule; tenant holiday calendars
/// (foundation AC #20) are a follow-up.
#[must_use]
pub fn business_days_between(from: NaiveDate, to: NaiveDate) -> i32 {
    let mut count = 0;
    let mut day = from;
    while day < to {
        // `succ_opt` only returns `None` at the maximum representable date, far
        // outside any fiscal-period range — saturate there rather than panic.
        let Some(next) = day.succ_opt() else { break };
        day = next;
        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
