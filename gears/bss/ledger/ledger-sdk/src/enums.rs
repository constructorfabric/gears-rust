//! Central enums declared in their FINAL form (architecture §7.2): all
//! values present up front so no later phase needs an additive change.

use std::fmt;
use std::str::FromStr;

/// Raised when a stored literal does not match any known variant.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown {kind} literal: {value:?}")]
pub struct UnknownLiteral {
    pub kind: &'static str,
    pub value: String,
}

macro_rules! str_enum {
    ($name:ident, $kind:literal, { $($variant:ident => $lit:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$variant => $lit),+ }
            }
            /// Every literal, for building a SQL `CHECK (col IN (...))`.
            pub const ALL: &'static [&'static str] = &[ $($lit),+ ];
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl FromStr for $name {
            type Err = UnknownLiteral;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($lit => Ok(Self::$variant),)+
                    other => Err(UnknownLiteral { kind: $kind, value: other.to_owned() }),
                }
            }
        }
    };
}

str_enum!(Side, "side", { Debit => "DR", Credit => "CR" });
str_enum!(MappingStatus, "mapping_status", { Resolved => "RESOLVED", Pending => "PENDING" });

str_enum!(AccountClass, "account_class", {
    Ar => "AR",
    CashClearing => "CASH_CLEARING",
    Unallocated => "UNALLOCATED",
    ReusableCredit => "REUSABLE_CREDIT",
    ContractLiability => "CONTRACT_LIABILITY",
    Revenue => "REVENUE",
    TaxPayable => "TAX_PAYABLE",
    Suspense => "SUSPENSE",
    DisputeHold => "DISPUTE_HOLD",
    RefundClearing => "REFUND_CLEARING",
    ContraRevenue => "CONTRA_REVENUE",
    Goodwill => "GOODWILL",
    DisputeLossExpense => "DISPUTE_LOSS_EXPENSE",
    PspFeeExpense => "PSP_FEE_EXPENSE",
    FxGainLoss => "FX_GAIN_LOSS",
    FxUnrealized => "FX_UNREALIZED",
});

impl AccountClass {
    /// Account classes whose balance must never go negative — the single
    /// source of truth shared by the conditional no-negative CHECK on
    /// `account_balance`, the projector's in-posting guard, and the tie-out
    /// reconciliation backstop. Any class NOT listed here may legitimately
    /// go negative.
    pub const GUARDED: &'static [AccountClass] = &[
        AccountClass::Ar,
        AccountClass::CashClearing,
        AccountClass::Unallocated,
        AccountClass::ContractLiability,
        AccountClass::DisputeHold,
        AccountClass::RefundClearing,
    ];

    /// True when this class is in [`AccountClass::GUARDED`] (must stay `>= 0`).
    #[must_use]
    pub fn is_guarded(self) -> bool {
        Self::GUARDED.contains(&self)
    }

    /// Classes whose balance sub-divides by `revenue_stream` — the per-stream
    /// classes. The single source of truth shared by the chart-resolution key
    /// (these classes resolve on their stream; the rest on `stream = None`) and
    /// the `chk_journal_line_revenue_stream` CHECK (which requires a stream for
    /// exactly these classes). System parking / clearing classes (AR, TAX,
    /// SUSPENSE, CASH, …) carry no stream.
    pub const PER_STREAM: &'static [AccountClass] =
        &[AccountClass::Revenue, AccountClass::ContractLiability];

    /// True when this class keys on `revenue_stream` (is in
    /// [`AccountClass::PER_STREAM`]); false for the stream-less system classes.
    #[must_use]
    pub fn is_per_stream(self) -> bool {
        Self::PER_STREAM.contains(&self)
    }
}

str_enum!(SourceDocType, "source_doc_type", {
    InvoicePost => "INVOICE_POST",
    Reversal => "REVERSAL",
    MappingCorrection => "MAPPING_CORRECTION",
    PaymentSettle => "PAYMENT_SETTLE",
    PaymentAllocate => "PAYMENT_ALLOCATE",
    Chargeback => "CHARGEBACK",
    CreditApply => "CREDIT_APPLY",
    SettlementReturn => "SETTLEMENT_RETURN",
    ManualAdjustment => "MANUAL_ADJUSTMENT",
    ScheduleBuild => "SCHEDULE_BUILD",
    Recognition => "RECOGNITION",
    CreditNote => "CREDIT_NOTE",
    DebitNote => "DEBIT_NOTE",
    Refund => "REFUND",
    FxRevaluation => "FX_REVALUATION",
    FxRevalReversal => "FX_REVAL_REVERSAL",
});

// `Flow` shares the `source_doc_type` literal set per architecture §7.2.
pub type Flow = SourceDocType;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_class_round_trips_every_variant() {
        for lit in AccountClass::ALL {
            let parsed: AccountClass = lit.parse().unwrap();
            assert_eq!(&parsed.as_str(), lit);
        }
    }

    /// WIRE-TOKEN LOCK. The chain verifier
    /// (`bss-ledger::infra::jobs::verifier::reconstruct`) re-parses these stored
    /// literals to recompute each historical row hash. A renamed literal would
    /// still round-trip WITHIN the SDK (both `as_str` and `FromStr` change
    /// together) yet would fail to parse every already-persisted row → mass false
    /// tamper-freeze. Pinning the exact tokens (and the full set) makes any
    /// rename / add / remove break CI here instead of in production.
    #[test]
    fn wire_tokens_are_frozen() {
        assert_eq!(Side::ALL, ["DR", "CR"]);
        assert_eq!(MappingStatus::ALL, ["RESOLVED", "PENDING"]);
        assert_eq!(
            AccountClass::ALL,
            [
                "AR",
                "CASH_CLEARING",
                "UNALLOCATED",
                "REUSABLE_CREDIT",
                "CONTRACT_LIABILITY",
                "REVENUE",
                "TAX_PAYABLE",
                "SUSPENSE",
                "DISPUTE_HOLD",
                "REFUND_CLEARING",
                "CONTRA_REVENUE",
                "GOODWILL",
                "DISPUTE_LOSS_EXPENSE",
                "PSP_FEE_EXPENSE",
                "FX_GAIN_LOSS",
                "FX_UNREALIZED",
            ]
        );
        assert_eq!(
            SourceDocType::ALL,
            [
                "INVOICE_POST",
                "REVERSAL",
                "MAPPING_CORRECTION",
                "PAYMENT_SETTLE",
                "PAYMENT_ALLOCATE",
                "CHARGEBACK",
                "CREDIT_APPLY",
                "SETTLEMENT_RETURN",
                "MANUAL_ADJUSTMENT",
                "SCHEDULE_BUILD",
                "RECOGNITION",
                "CREDIT_NOTE",
                "DEBIT_NOTE",
                "REFUND",
                "FX_REVALUATION",
                "FX_REVAL_REVERSAL",
            ]
        );
    }

    #[test]
    fn unknown_literal_is_rejected() {
        assert!("NOPE".parse::<AccountClass>().is_err());
        assert_eq!("DR".parse::<Side>().unwrap(), Side::Debit);
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(Side::Debit.to_string(), "DR");
        assert_eq!(AccountClass::Ar.to_string(), "AR");
        assert_eq!(SourceDocType::Reversal.to_string(), "REVERSAL");
    }

    #[test]
    fn guarded_set_is_the_no_negative_classes() {
        for c in AccountClass::GUARDED {
            assert!(c.is_guarded(), "{c} must be guarded");
        }
        for c in [
            AccountClass::Revenue,
            AccountClass::TaxPayable,
            AccountClass::Suspense,
            AccountClass::ContraRevenue,
        ] {
            assert!(!c.is_guarded(), "{c} must not be guarded");
        }
        assert_eq!(AccountClass::GUARDED.len(), 6);
    }
}
