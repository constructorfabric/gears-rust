//! BSS Billing Ledger SDK — infrastructure-free contract crate.
//!
//! Publishes the in-process data-access API trait (`LedgerClientV1`, resolved
//! from `ClientHub`) plus the value types, enums, and error codes a caller
//! needs to invoke the ledger. The money types come from the shared `bss-money`
//! crate and are re-exported here; exact arithmetic, rounding and allocation live
//! there too (feature `exact`), the currency-scale registry is gear-internal.

pub mod api;
pub mod bill_run_finished;
pub mod close;
pub mod enums;
pub mod error;
pub mod issued_invoice_manifest;
/// The shared money types, re-exported as part of the ledger contract.
pub mod money {
    pub use bss_money::{CurrencySpec, MoneyError, PostedMoney, canonical_decimal, parse_decimal};
}
pub mod odata;
pub mod posting;
pub mod provisioning;
pub mod psp_settlement_feed;
pub mod rate_provider;
pub mod rate_provider_plugin;

pub use api::LedgerClientV1;
pub use bill_run_finished::{BillRunFinishedV1, UnconfiguredBillRunFinishedV1};
pub use close::CloseOutcome;
pub use enums::{AccountClass, Flow, MappingStatus, Side, SourceDocType};
pub use error::LedgerError;
pub use issued_invoice_manifest::{
    ControlFeedError, GrossTotals, GrossTotalsError, IssuedInvoiceManifest,
    IssuedInvoiceManifestV1, UnconfiguredIssuedInvoiceManifestV1,
};
pub use money::{CurrencySpec, MoneyError, PostedMoney, canonical_decimal, parse_decimal};
pub use posting::{
    AllocateOutcome, AllocatePayment, AllocationApplied, AllocationQueued, AllocationSplit,
    AllocationView, ArInvoiceBalanceView, BalanceView, ChangeRecognitionSchedule, ChangeSegment,
    CreditApplication, CreditApplicationApplied, CreditApply, CreditDebitView, CreditGrant,
    DisputeOutcome, DisputeQueued, DisputeRecorded, EntryView, LineView, ODataQuery, Page,
    PostEntry, PostLine, PostingRef, RecognitionRunOutcome, RecognitionRunQueued,
    RecognitionRunRef, RecognitionScheduleList, RecognitionScheduleSegmentView,
    RecognitionScheduleSummaryView, RecognitionScheduleView, RecordDisputePhase, ReturnPayment,
    RevenueDisaggregation, RevenueDisaggregationEntry, RevenueDisaggregationQuery,
    ScheduleChangeRef, SettlePayment, SettledAmounts, TriggerRecognitionRun, UnallocatedView,
};
pub use provisioning::{
    AccountInfo, FiscalCalendarSpec, Granularity, ProvisionAccount, ProvisionCurrencyScale,
    ProvisionOutcome, ProvisionRequest,
};
pub use psp_settlement_feed::{
    PspSettlementFeedV1, PspSettlementReport, UnconfiguredPspSettlementFeedV1,
};
pub use rate_provider::{
    CurrencyPair, ProviderRate, RateProviderError, RateProviderV1, UnconfiguredRateProviderV1,
};
pub use rate_provider_plugin::RateProviderPluginSpecV1;
