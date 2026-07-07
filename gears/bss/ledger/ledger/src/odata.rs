//! `OData` filter-field definitions for the ledger's row-collection list
//! endpoints (`GET …/accounts`, `GET …/journal-lines`, `GET …/balances`).
//!
//! Each enum declares the wire-named fields valid in a `$filter` / `$orderby`
//! clause on its endpoint. They feed both the `OpenAPI`
//! `with_odata_filter::<F>()` helper (which advertises the per-field operators)
//! and the `paginate_odata` call in the repo layer (via the column mappers in
//! [`crate::infra::storage::odata_mapping`]). This is the canonical platform
//! list pattern (RBAC/AM/RG): the `$filter` is **additive over** the SecureORM
//! tenant scope, never a replacement (BOLA preserved).
//!
//! Each enum carries a default keyset-order column as a recognised variant
//! (`account_id` / `line_id`) so the repo's default-order injection resolves
//! via [`FilterField::from_name`] — without that variant a bare list (no
//! `$orderby`, no cursor) would error "Unknown orderby field" the way the RBAC
//! C1 fix documents.

use toolkit_odata::filter::{FieldKind, FilterField};

/// Filter field enum for `GET /bss-ledger/v1/accounts`.
///
/// The chart of accounts is keyed by `account_id`; the listable dims are the
/// account coordinate (`account_class`, `currency`, `revenue_stream`) and the
/// `lifecycle_state`. `account_id` is the default keyset-order column
/// (`account_id ASC`, hits the `tenant_account` PK).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AccountInfoFilterField {
    /// Persistent account id — the default keyset-order column (`account_id
    /// ASC`). Must be a recognised field so the repo's default-order injection
    /// resolves via [`FilterField::from_name`].
    AccountId,
    AccountClass,
    Currency,
    RevenueStream,
    LifecycleState,
}

impl FilterField for AccountInfoFilterField {
    const FIELDS: &'static [Self] = &[
        Self::AccountId,
        Self::AccountClass,
        Self::Currency,
        Self::RevenueStream,
        Self::LifecycleState,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::AccountId => "account_id",
            Self::AccountClass => "account_class",
            Self::Currency => "currency",
            Self::RevenueStream => "revenue_stream",
            Self::LifecycleState => "lifecycle_state",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::AccountId => FieldKind::Uuid,
            // `account_class` / `lifecycle_state` are stored as their plain
            // string literals (not a SMALLINT-encoded enum), so the wire shape
            // matches storage and they stay ordinary `String` columns — no
            // `map_value` / `is_orderable` override needed.
            Self::AccountClass | Self::Currency | Self::RevenueStream | Self::LifecycleState => {
                FieldKind::String
            }
        }
    }
}

/// Filter field enum for `GET /bss-ledger/v1/journal-lines`.
///
/// The listable dims are the posted line dims a caller reconciles on:
/// `payer_tenant_id`, `account_class`, `period_id`, and `invoice_id` (the AR
/// line's business-document ref — a real `journal_line` column). `line_id` is
/// the default keyset-order column (`line_id ASC`, hits the `journal_line` PK),
/// matching the foundation's prior `ORDER BY line_id`.
///
/// Migration note: the legacy `LineFilter` also accepted `source_business_id`,
/// which is an **entry-header** dim (not on the line) and was resolved to
/// matching entry ids first. That arm is dropped here — the line carries its
/// own `invoice_id`, which is the per-line equivalent a `$filter` can target
/// directly.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum JournalLineFilterField {
    /// Line id — the default keyset-order column (`line_id ASC`). Must be a
    /// recognised field so the repo's default-order injection resolves.
    LineId,
    PayerTenantId,
    AccountClass,
    PeriodId,
    InvoiceId,
}

impl FilterField for JournalLineFilterField {
    const FIELDS: &'static [Self] = &[
        Self::LineId,
        Self::PayerTenantId,
        Self::AccountClass,
        Self::PeriodId,
        Self::InvoiceId,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::LineId => "line_id",
            Self::PayerTenantId => "payer_tenant_id",
            Self::AccountClass => "account_class",
            Self::PeriodId => "period_id",
            Self::InvoiceId => "invoice_id",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::LineId | Self::PayerTenantId => FieldKind::Uuid,
            Self::AccountClass | Self::PeriodId | Self::InvoiceId => FieldKind::String,
        }
    }
}

/// Filter field enum for `GET /bss-ledger/v1/journal-entries` (the entry-HEADER
/// list, read-surface R5). The listable dims are the header-only ones a caller
/// cross-cuts on — which is exactly why this is a separate collection over
/// `journal_entry` and NOT a new `journal_line` filter: `source_doc_type` /
/// `source_business_id` are columns on the entry HEADER, never on the line (the
/// line only carries `entry_id`), so "list all `MANUAL_ADJUSTMENT` entries" or
/// "all `REFUND` / `CREDIT_NOTE` entries" can only be served from the header
/// table. `entry_id` is the default keyset-order column (`entry_id ASC`, the
/// `journal_entry` PK's keyset leg), mirroring `recognition_run`'s `run_id` on a
/// composite-PK table.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum JournalEntryFilterField {
    /// Entry id — the default keyset-order column (`entry_id ASC`). Must be a
    /// recognised field so the repo's default-order injection resolves via
    /// [`FilterField::from_name`].
    EntryId,
    SourceDocType,
    SourceBusinessId,
    PeriodId,
}

impl FilterField for JournalEntryFilterField {
    const FIELDS: &'static [Self] = &[
        Self::EntryId,
        Self::SourceDocType,
        Self::SourceBusinessId,
        Self::PeriodId,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::EntryId => "entry_id",
            Self::SourceDocType => "source_doc_type",
            Self::SourceBusinessId => "source_business_id",
            Self::PeriodId => "period_id",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            // `entry_id` is the `Uuid` keyset leg; `source_doc_type` /
            // `source_business_id` / `period_id` are plain `varchar` dims.
            Self::EntryId => FieldKind::Uuid,
            Self::SourceDocType | Self::SourceBusinessId | Self::PeriodId => FieldKind::String,
        }
    }
}

/// Filter field enum for `GET /bss-ledger/v1/balances`.
///
/// The account-balance cache lists by `account_class` / `currency`. `account_id`
/// is the default keyset-order column (`account_id ASC`, hits the
/// `account_balance` PK), matching the foundation's prior `ORDER BY account_id`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum BalanceFilterField {
    /// Account id — the default keyset-order column (`account_id ASC`). Must be
    /// a recognised field so the repo's default-order injection resolves.
    AccountId,
    AccountClass,
    Currency,
}

impl FilterField for BalanceFilterField {
    const FIELDS: &'static [Self] = &[Self::AccountId, Self::AccountClass, Self::Currency];

    fn name(&self) -> &'static str {
        match self {
            Self::AccountId => "account_id",
            Self::AccountClass => "account_class",
            Self::Currency => "currency",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::AccountId => FieldKind::Uuid,
            Self::AccountClass | Self::Currency => FieldKind::String,
        }
    }
}

/// Filter field enum for `GET /bss-ledger/v1/refunds` (the refund-record list,
/// design §4.4 / read-surface). The `refund` table's surrogate PK is
/// `(tenant_id, refund_id)`; `refund_id` is the default keyset-order column
/// (`refund_id ASC`). NB: `refund` carries NO `payer_tenant_id` column, so the
/// filterable dims are the origin / lifecycle ones below (all `varchar`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RefundFilterField {
    /// Refund id — the default keyset-order column (`refund_id ASC`). Must be a
    /// recognised field so the repo's default-order injection resolves.
    RefundId,
    PaymentId,
    PspRefundId,
    Phase,
    Pattern,
    ClearingState,
    InvoiceId,
}

impl FilterField for RefundFilterField {
    const FIELDS: &'static [Self] = &[
        Self::RefundId,
        Self::PaymentId,
        Self::PspRefundId,
        Self::Phase,
        Self::Pattern,
        Self::ClearingState,
        Self::InvoiceId,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::RefundId => "refund_id",
            Self::PaymentId => "payment_id",
            Self::PspRefundId => "psp_refund_id",
            Self::Phase => "phase",
            Self::Pattern => "pattern",
            Self::ClearingState => "clearing_state",
            Self::InvoiceId => "invoice_id",
        }
    }

    fn kind(&self) -> FieldKind {
        // Every refund filter dim is a varchar column (refund_id / payment_id /
        // psp_refund_id / phase / pattern / clearing_state / invoice_id).
        FieldKind::String
    }
}

/// Filter field enum for `GET /bss-ledger/v1/credit-notes` (the credit-note record
/// list, read-surface §5). The `credit_note` table's PK is `(tenant_id,
/// credit_note_id)`; `credit_note_id` is the default keyset-order column
/// (`credit_note_id ASC`). The filterable dims are the origin / classification
/// ones below (all `varchar` — `credit_note_id` / `origin_invoice_id` /
/// `revenue_stream` / `reason_code`). NB: `credit_note` carries NO
/// `payer_tenant_id` / `entry_id` / `goodwill` column, so none of those are
/// filterable (the design §5 field list was aspirational — the entity is the
/// source of truth).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum CreditNoteFilterField {
    /// Credit-note id — the default keyset-order column (`credit_note_id ASC`).
    /// Must be a recognised field so the repo's default-order injection resolves.
    CreditNoteId,
    OriginInvoiceId,
    RevenueStream,
    ReasonCode,
}

impl FilterField for CreditNoteFilterField {
    const FIELDS: &'static [Self] = &[
        Self::CreditNoteId,
        Self::OriginInvoiceId,
        Self::RevenueStream,
        Self::ReasonCode,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::CreditNoteId => "credit_note_id",
            Self::OriginInvoiceId => "origin_invoice_id",
            Self::RevenueStream => "revenue_stream",
            Self::ReasonCode => "reason_code",
        }
    }

    fn kind(&self) -> FieldKind {
        // Every credit-note filter dim is a varchar column (credit_note_id /
        // origin_invoice_id / revenue_stream / reason_code).
        FieldKind::String
    }
}

/// Filter field enum for `GET /bss-ledger/v1/debit-notes` (the debit-note record
/// list, read-surface §5). The `debit_note` table's PK is `(tenant_id,
/// debit_note_id)`; `debit_note_id` is the default keyset-order column
/// (`debit_note_id ASC`). The `debit_note` table is leaner than `credit_note`
/// (NO `revenue_stream` / `reason_code` columns), so only the id + origin invoice
/// are filterable (both `varchar`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum DebitNoteFilterField {
    /// Debit-note id — the default keyset-order column (`debit_note_id ASC`).
    /// Must be a recognised field so the repo's default-order injection resolves.
    DebitNoteId,
    OriginInvoiceId,
}

impl FilterField for DebitNoteFilterField {
    const FIELDS: &'static [Self] = &[Self::DebitNoteId, Self::OriginInvoiceId];

    fn name(&self) -> &'static str {
        match self {
            Self::DebitNoteId => "debit_note_id",
            Self::OriginInvoiceId => "origin_invoice_id",
        }
    }

    fn kind(&self) -> FieldKind {
        // Both debit-note filter dims are varchar columns (debit_note_id /
        // origin_invoice_id).
        FieldKind::String
    }
}

/// Filter field enum for `GET /bss-ledger/v1/disputes` (the chargeback dispute
/// current-state list, read-surface R3). The `ledger_dispute` table's PK is
/// `(tenant_id, dispute_id)`; `dispute_id` is the default keyset-order column
/// (`dispute_id ASC`). The filterable dims are the dispute's origin / lifecycle
/// identity below (all `varchar` — `dispute_id` / `payment_id` / `last_phase` /
/// `variant`). NB: `ledger_dispute` carries NO `payer_tenant_id` / `created_at`
/// column, so neither is filterable; the numeric `cycle` / amount columns are not
/// listed as filter dims (a caller seeks by the string identity, mirroring the
/// refund/note surfaces).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum DisputeFilterField {
    /// Dispute id — the default keyset-order column (`dispute_id ASC`). Must be a
    /// recognised field so the repo's default-order injection resolves via
    /// [`FilterField::from_name`].
    DisputeId,
    PaymentId,
    LastPhase,
    Variant,
}

impl FilterField for DisputeFilterField {
    const FIELDS: &'static [Self] = &[
        Self::DisputeId,
        Self::PaymentId,
        Self::LastPhase,
        Self::Variant,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::DisputeId => "dispute_id",
            Self::PaymentId => "payment_id",
            Self::LastPhase => "last_phase",
            Self::Variant => "variant",
        }
    }

    fn kind(&self) -> FieldKind {
        // Every dispute filter dim is a varchar column (dispute_id / payment_id /
        // last_phase / variant).
        FieldKind::String
    }
}

/// Filter field enum for `GET /bss-ledger/v1/recognition-runs` (the ASC 606
/// recognition-run list, read-surface R4). The `recognition_run` table's PK is
/// `(tenant_id, period_id, run_id)`; `run_id` is the default keyset-order column
/// (`run_id ASC`). The filterable dims are the run's identity / lifecycle below:
/// `run_id` (the surrogate run id, a `Uuid`), `period_id` (the fiscal period the
/// run released, a `YYYYMM` `varchar`), and `status` (`RUNNING` / `DONE` /
/// `FAILED`, a `varchar`). The numeric `started_at_utc` is not a filter dim (a
/// caller seeks by the id / period / status identity, mirroring the dispute /
/// refund surfaces).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RecognitionRunFilterField {
    /// Run id — the default keyset-order column (`run_id ASC`). Must be a
    /// recognised field so the repo's default-order injection resolves via
    /// [`FilterField::from_name`].
    RunId,
    PeriodId,
    Status,
}

impl FilterField for RecognitionRunFilterField {
    const FIELDS: &'static [Self] = &[Self::RunId, Self::PeriodId, Self::Status];

    fn name(&self) -> &'static str {
        match self {
            Self::RunId => "run_id",
            Self::PeriodId => "period_id",
            Self::Status => "status",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            // `run_id` is the surrogate `Uuid` PK leg; `period_id` / `status` are
            // plain `varchar` dims.
            Self::RunId => FieldKind::Uuid,
            Self::PeriodId | Self::Status => FieldKind::String,
        }
    }
}

/// Filter field enum for `GET /bss-ledger/v1/exceptions` (the Revenue Assurance
/// exception-queue dashboard / list, Slice 7 Phase 2, design §4.6 / §5). The
/// `ledger_exception_queue` table's PK is `(tenant_id, exception_id)`;
/// `exception_id` is the default keyset-order column (`exception_id ASC`). The
/// filterable dims are the queue triage ones the dashboard cross-cuts on:
/// `exception_type` (the wire `type`, e.g. `RECON_MISMATCH`), `status`
/// (`OPEN` / `ACK` / `RESOLVED` / `APPROVED_EXCEPTION`), `business_ref` (the
/// offending business key), and `period_id` (the fiscal period). The `opened_at`
/// timestamp is not a filter dim (a caller seeks by the type / status / ref
/// identity, mirroring the dispute / refund surfaces). NB: `period_id` is
/// nullable (a non-period-scoped exception has none).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ExceptionFilterField {
    /// Exception id — the default keyset-order column (`exception_id ASC`). Must
    /// be a recognised field so the repo's default-order injection resolves via
    /// [`FilterField::from_name`].
    ExceptionId,
    /// The exception type, wire-named `type` (`type` is a Rust keyword, so the
    /// variant is `ExceptionType`, but its `$filter` field name is `type`).
    ExceptionType,
    Status,
    BusinessRef,
    PeriodId,
}

impl FilterField for ExceptionFilterField {
    const FIELDS: &'static [Self] = &[
        Self::ExceptionId,
        Self::ExceptionType,
        Self::Status,
        Self::BusinessRef,
        Self::PeriodId,
    ];

    fn name(&self) -> &'static str {
        match self {
            Self::ExceptionId => "exception_id",
            // The wire field is `type` (the existing query param), even though the
            // backing column is `exception_type`.
            Self::ExceptionType => "type",
            Self::Status => "status",
            Self::BusinessRef => "business_ref",
            Self::PeriodId => "period_id",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            // `exception_id` is the `Uuid` keyset leg; the rest are plain `varchar`
            // dims (`type` / `status` / `business_ref` / `period_id`).
            Self::ExceptionId => FieldKind::Uuid,
            Self::ExceptionType | Self::Status | Self::BusinessRef | Self::PeriodId => {
                FieldKind::String
            }
        }
    }
}

#[cfg(test)]
#[path = "odata_tests.rs"]
mod odata_tests;
