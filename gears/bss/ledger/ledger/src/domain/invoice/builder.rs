//! Exact major-unit invoice builder. Billing supplies posted tax and deferral amounts.
//! Groups retain their existing dimensions and first-item source references.

use std::collections::BTreeMap;

use bss_ledger_sdk::{AccountClass, MappingStatus, PostEntry, PostLine, Side, SourceDocType};
use chrono::NaiveDate;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::exact_money::{ExactAmount, ExactError, sum_posted};
use crate::domain::invoice::mapping::MappedLine;
use bss_ledger_sdk::money::{CurrencySpec, PostedMoney};
use rust_decimal::Decimal;

/// One billable line of an invoice, ex-tax. Carries the revenue dimensions the
/// ledger posts on (`revenue_stream`, the optional Catalog/Contract mapping
/// inputs, and the source refs threaded onto the journal line for audit).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
// `invoice_item_ref` / `sku_or_plan_ref` etc. mirror the `journal_line` / `PostLine`
// column names verbatim; renaming to satisfy `struct_field_names` would diverge
// from the storage + SDK contract.
#[allow(clippy::struct_field_names)]
pub struct InvoiceItem {
    /// Ex-tax amount in the invoice's major units. Must be `>= 0`.
    pub amount_ex_tax: PostedMoney,
    /// The portion of [`Self::amount_ex_tax`] deferred to
    /// `CONTRACT_LIABILITY` (Slice 4). The recognition derivation
    /// ([`crate::domain::recognition::builder::ScheduleBuilder`]) computes this
    /// *before* the builder and threads it onto the item; the builder credits
    /// `amount − deferred` to Revenue and `deferred` to Contract-liability on the
    /// SAME `revenue_stream`. `0` (the default, and absence-of-recognition) ⇒ the
    /// whole amount recognizes now and NO Contract-liability line is emitted
    /// (same accounting split as the pre-Slice-4 Variant-A output). Invariant:
    /// `0 <= deferred <= amount_ex_tax`.
    pub deferred: PostedMoney,
    /// Revenue stream this item books to — the grouping key for the CR Revenue
    /// lines, and (with the class) the chart-resolution key.
    pub revenue_stream: String,
    /// Catalog-supplied GL class (the default mapping). `None` ⇒ no Catalog
    /// mapping for this item.
    pub catalog_class: Option<AccountClass>,
    /// Contract-supplied GL class override. Wins over [`Self::catalog_class`]
    /// when present.
    pub contract_class: Option<AccountClass>,
    /// Catalog GL code carried onto the posted line (audit / downstream GL).
    pub gl_code: Option<String>,
    /// The optional per-item ASC 606 recognition spec (Slice 4). `None` ⇒ the
    /// item is fully recognized now (`deferred` stays `0`, today's
    /// Variant-A behaviour). When present, the orchestrator
    /// ([`crate::infra::invoice_post`]) derives [`Self::deferred`] + the
    /// schedule plan from it via the recognition
    /// [`ScheduleBuilder`](crate::domain::recognition::builder::ScheduleBuilder)
    /// *before* the builder runs. Carried on the domain item (not consumed by the
    /// pure builder, which reads only the already-derived `deferred`) so
    /// the orchestrator has the per-item context the derivation needs.
    pub recognition: Option<crate::domain::recognition::input::RecognitionInput>,
    /// Source-document refs threaded onto the journal line for lineage.
    pub invoice_item_ref: Option<String>,
    pub sku_or_plan_ref: Option<String>,
    pub price_id: Option<String>,
    pub pricing_snapshot_ref: Option<String>,
}

/// One tax component of an invoice, already computed by the tax engine. Each
/// breakdown posts as its own CR Tax line carrying the filing dimensions.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxBreakdown {
    /// Tax amount in the invoice's major units. Must be `>= 0`.
    pub amount: PostedMoney,
    /// Filing jurisdiction (e.g. `"US-CA"`) — a `TAX_PAYABLE` sub-balance dim.
    pub tax_jurisdiction: String,
    /// Filing period (e.g. `"2026Q2"`) — the second `TAX_PAYABLE` sub-balance dim.
    pub tax_filing_period: String,
    /// Reference to the applied tax rate (audit), if any.
    pub tax_rate_ref: Option<String>,
}

/// A fully-recognized invoice to post (Variant A input). The whole amount is
/// recognized now — no deferral schedule.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostedInvoice {
    /// External invoice identity — the `INVOICE_POST` idempotency business id and
    /// the `invoice_id` dim on the AR line.
    pub invoice_id: String,
    /// The tenant that pays this invoice (the single payer of the entry).
    pub payer_tenant_id: Uuid,
    /// The tenant whose resources were consumed, if distinct from the payer
    /// (threaded onto each line for cost attribution); `None` ⇒ payer == resource.
    pub resource_tenant_id: Option<Uuid>,
    /// The seller tenant whose ledger this posts into (`= entry.tenant_id`).
    pub seller_tenant_id: Uuid,
    /// GL effective date of the entry.
    pub effective_at: NaiveDate,
    /// AR due date stamped on the AR line (drives AR-aging); `None` ⇒ due on
    /// posting.
    pub due_date: Option<NaiveDate>,
    /// The fiscal `period_id` (`YYYYMM`) the entry posts into.
    pub period_id: String,
    /// Ex-tax billable lines.
    pub items: Vec<InvoiceItem>,
    /// Tax components (may be empty ⇒ no CR Tax line).
    pub tax: Vec<TaxBreakdown>,
    /// Actor recorded as the poster (audit who).
    pub posted_by_actor_id: Uuid,
    /// Correlation id propagated onto the entry.
    pub correlation_id: Uuid,
}

/// An invoice cannot be built from invalid money or incomplete mapping.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvoiceError {
    /// There is no real currency specification for an empty invoice.
    #[error("empty invoice")]
    EmptyInvoice,
    /// Mapping is positional and must cover every item exactly once.
    #[error("one mapped line is required per invoice item")]
    MappingLengthMismatch,
    /// Ex-tax and tax amounts must be nonnegative.
    #[error("invoice amount must be nonnegative")]
    NegativeAmount,
    /// Deferral must be between zero and the ex-tax item amount.
    #[error("deferred amount must be between zero and item amount")]
    InvalidDeferral,
    /// Exact computation or final bounded money failed.
    #[error(transparent)]
    Exact(#[from] ExactError),
}

/// Validate code and stored scale before comparison or arithmetic, including zeros.
fn matching(value: &PostedMoney, spec: &CurrencySpec) -> Result<(), ExactError> {
    Ok(value.currency().ensure_same(spec)?)
}

impl PostedInvoice {
    /// Real entry currency, or none for an empty invoice.
    #[must_use]
    pub fn currency(&self) -> Option<&str> {
        self.items
            .first()
            .map(|i| i.amount_ex_tax.currency().code())
            .or_else(|| self.tax.first().map(|t| t.amount.currency().code()))
    }

    /// Validate all metadata and input signs before any comparison or fold.
    fn validated_spec(&self) -> Result<CurrencySpec, InvoiceError> {
        let spec = self
            .items
            .first()
            .map(|i| i.amount_ex_tax.currency())
            .or_else(|| self.tax.first().map(|t| t.amount.currency()))
            .ok_or(InvoiceError::EmptyInvoice)?
            .clone();
        for item in &self.items {
            matching(&item.amount_ex_tax, &spec)?;
            matching(&item.deferred, &spec)?;
        }
        for tax in &self.tax {
            matching(&tax.amount, &spec)?;
        }
        for item in &self.items {
            if item.amount_ex_tax.amount() < Decimal::ZERO {
                return Err(InvoiceError::NegativeAmount);
            }
            if item.deferred.amount() < Decimal::ZERO
                || item.deferred.amount() > item.amount_ex_tax.amount()
            {
                return Err(InvoiceError::InvalidDeferral);
            }
        }
        if self.tax.iter().any(|t| t.amount.amount() < Decimal::ZERO) {
            return Err(InvoiceError::NegativeAmount);
        }
        Ok(spec)
    }

    /// Tax-inclusive gross, folded exactly and narrowed only at the final total.
    /// # Errors
    /// Rejects empty invoices, metadata/sign/deferral violations, and final overflow.
    pub fn gross(&self) -> Result<PostedMoney, InvoiceError> {
        let spec = self.validated_spec()?;
        let values: Vec<_> = self
            .items
            .iter()
            .map(|i| i.amount_ex_tax.clone())
            .chain(self.tax.iter().map(|t| t.amount.clone()))
            .collect();
        Ok(sum_posted(&values, spec)?)
    }
}

/// Build the balanced invoice with deterministic revenue and deferred groups.
/// # Errors
/// Rejects missing mappings, invalid input money and overflowing final totals.
pub fn build_invoice_entry(
    inv: &PostedInvoice,
    mapped: &[MappedLine],
) -> Result<PostEntry, InvoiceError> {
    if mapped.len() != inv.items.len() {
        return Err(InvoiceError::MappingLengthMismatch);
    }
    let spec = inv.validated_spec()?;
    let gross = inv.gross()?;
    let entry_id = Uuid::now_v7();
    let currency = spec.code().to_owned();

    // Worst case: 1 AR + one Revenue + one Contract-liability per item + one Tax
    // per breakdown.
    let mut lines: Vec<PostLine> = Vec::with_capacity(1 + 2 * inv.items.len() + inv.tax.len());

    // DR AR — the gross receivable (incl. tax). Omit zero postings.
    if gross.amount() > Decimal::ZERO {
        lines.push(PostLine {
            line_id: Uuid::now_v7(),
            payer_tenant_id: inv.payer_tenant_id,
            seller_tenant_id: Some(inv.seller_tenant_id),
            resource_tenant_id: inv.resource_tenant_id,
            account_id: Uuid::nil(),
            account_class: AccountClass::Ar,
            gl_code: None,
            side: Side::Debit,
            money: gross,
            invoice_id: Some(inv.invoice_id.clone()),
            due_date: inv.due_date,
            revenue_stream: None,
            mapping_status: MappingStatus::Resolved,
            functional_money: None,
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
            invoice_item_ref: None,
            sku_or_plan_ref: None,
            price_id: None,
            pricing_snapshot_ref: None,
            po_allocation_group: None,
            credit_grant_event_type: None,
            ar_status: None,
        });
    }

    // CR Revenue — grouped by (class, gl_code, status, stream) so a SUSPENSE /
    // PENDING item never merges into a resolved revenue stream. Each item
    // contributes its *recognized-now* amount (`amount − deferred`); the deferred
    // remainder is folded into the per-stream Contract-liability map below. A
    // BTreeMap keys the groups deterministically (stable line order across
    // recomputes — the same financial intent always builds identically).
    let mut revenue: BTreeMap<RevenueKey, RevenueAgg> = BTreeMap::new();
    // CR Contract-liability — the deferred portion, grouped by `revenue_stream`
    // only (the class is fixed `CONTRACT_LIABILITY`; the chart resolves it per
    // stream). Empty when every item defers `0`, so NO Contract-liability line is
    // emitted and the accounting split matches the pre-Slice-4 output.
    let mut deferred: BTreeMap<String, DeferredAgg> = BTreeMap::new();
    for (item, m) in inv.items.iter().zip(mapped.iter()) {
        let deferred_amount = ExactAmount::from_decimal(item.deferred.amount());
        let recognized_now =
            ExactAmount::from_decimal(item.amount_ex_tax.amount()).checked_sub(&deferred_amount)?;

        // Key on the stored string forms (the SDK enums are not `Ord`, and a
        // BTreeMap key must be — the strings give a deterministic, stable line
        // order). The typed class/status are carried on the agg for emit.
        let key = RevenueKey {
            account_class: m.account_class.as_str().to_owned(),
            gl_code: m.gl_code.clone().unwrap_or_default(),
            mapping_status: m.mapping_status.as_str().to_owned(),
            revenue_stream: item.revenue_stream.clone(),
        };
        let agg = revenue.entry(key).or_insert_with(|| RevenueAgg {
            amount: ExactAmount::from_decimal(Decimal::ZERO),
            account_class: m.account_class,
            gl_code: m.gl_code.clone(),
            mapping_status: m.mapping_status,
            // First item in the group seeds the line-level source refs.
            refs: ItemRefs::from(item),
        });
        agg.amount = agg.amount.checked_add(&recognized_now)?;

        // Fold the deferred remainder into its stream's Contract-liability line.
        if item.deferred.amount() > Decimal::ZERO {
            let cl = deferred
                .entry(item.revenue_stream.clone())
                .or_insert_with(|| DeferredAgg {
                    amount: ExactAmount::from_decimal(Decimal::ZERO),
                    // FORWARD-DEPENDENCY: the per-stream merge
                    // seeds refs from the FIRST deferring item, but `derive_recognition`
                    // mints one schedule PER item. With ≥2 deferring items in one
                    // revenue_stream, the second item's schedule (its own
                    // `source_invoice_item_ref`) matches no journal line — only the
                    // first item's ref lands on this merged CONTRACT_LIABILITY line.
                    // AUDIT-ONLY today: nothing dereferences `source_invoice_item_ref`
                    // at runtime (the runner posts recognition with
                    // `invoice_item_ref: None`; the tie-out joins by `entry_id`), and
                    // the amounts reconcile (per-stream sum). This ARMS in Slice 7 if
                    // reconciliation starts joining schedules → CL lines by item-ref —
                    // fix then (per-item CL refs, or a schedule↔CL map). A
                    // multi-item-per-stream test is the pending coverage.
                    refs: ItemRefs::from(item),
                });
            cl.amount = cl.amount.checked_add(&deferred_amount)?;
        }
    }
    for (key, agg) in revenue {
        // A stream whose entire recognized-now amount deferred (Slice 4) sums to
        // 0 — emit NO Revenue line: the engine rejects a zero-amount line, and the
        // deferred amount is carried by the CONTRACT_LIABILITY line below. (CL is
        // already only emitted for `deferred > 0`, so a fully-deferred item yields
        // a lone CONTRACT_LIABILITY credit, balanced against the AR/tax debit.)
        if agg.amount == ExactAmount::from_decimal(Decimal::ZERO) {
            continue;
        }
        lines.push(PostLine {
            line_id: Uuid::now_v7(),
            payer_tenant_id: inv.payer_tenant_id,
            seller_tenant_id: Some(inv.seller_tenant_id),
            resource_tenant_id: inv.resource_tenant_id,
            account_id: Uuid::nil(),
            account_class: agg.account_class,
            gl_code: agg.gl_code,
            side: Side::Credit,
            money: agg.amount.into_posted_exact(spec.clone())?,
            invoice_id: Some(inv.invoice_id.clone()),
            due_date: None,
            // Every Revenue line carries its stream (the DB CHECK requires it).
            revenue_stream: Some(key.revenue_stream),
            mapping_status: agg.mapping_status,
            functional_money: None,
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
            invoice_item_ref: agg.refs.invoice_item_ref,
            sku_or_plan_ref: agg.refs.sku_or_plan_ref,
            price_id: agg.refs.price_id,
            pricing_snapshot_ref: agg.refs.pricing_snapshot_ref,
            po_allocation_group: None,
            credit_grant_event_type: None,
            ar_status: None,
        });
    }

    // CR Contract-liability — one line per stream with a deferred amount (Slice
    // 4). Emitted AFTER the Revenue lines (stable, deterministic order); the
    // schedule materialization is the orchestrator's sidecar, not the builder's.
    // The class is fixed `CONTRACT_LIABILITY` and resolved per stream by the
    // chart. `mapping_status` is `Resolved`: a deferral books to a real
    // Contract-liability obligation (the recognition derivation only defers a
    // genuine obligation line), independent of the revenue side's mapping —
    // unlike the Revenue/SUSPENSE split, a deferred liability is never PENDING.
    for (revenue_stream, agg) in deferred {
        lines.push(PostLine {
            line_id: Uuid::now_v7(),
            payer_tenant_id: inv.payer_tenant_id,
            seller_tenant_id: Some(inv.seller_tenant_id),
            resource_tenant_id: inv.resource_tenant_id,
            account_id: Uuid::nil(),
            account_class: AccountClass::ContractLiability,
            gl_code: None,
            side: Side::Credit,
            money: agg.amount.into_posted_exact(spec.clone())?,
            invoice_id: Some(inv.invoice_id.clone()),
            due_date: None,
            revenue_stream: Some(revenue_stream),
            mapping_status: MappingStatus::Resolved,
            functional_money: None,
            tax_jurisdiction: None,
            tax_filing_period: None,
            tax_rate_ref: None,
            invoice_item_ref: agg.refs.invoice_item_ref,
            sku_or_plan_ref: agg.refs.sku_or_plan_ref,
            price_id: agg.refs.price_id,
            pricing_snapshot_ref: agg.refs.pricing_snapshot_ref,
            po_allocation_group: None,
            credit_grant_event_type: None,
            ar_status: None,
        });
    }

    // CR Tax — one line per breakdown, carrying the filing dims.
    for t in &inv.tax {
        if t.amount.amount().is_zero() {
            continue;
        }
        lines.push(PostLine {
            line_id: Uuid::now_v7(),
            payer_tenant_id: inv.payer_tenant_id,
            seller_tenant_id: Some(inv.seller_tenant_id),
            resource_tenant_id: inv.resource_tenant_id,
            account_id: Uuid::nil(),
            account_class: AccountClass::TaxPayable,
            gl_code: None,
            side: Side::Credit,
            money: t.amount.clone(),
            invoice_id: Some(inv.invoice_id.clone()),
            due_date: None,
            revenue_stream: None,
            mapping_status: MappingStatus::Resolved,
            functional_money: None,
            tax_jurisdiction: Some(t.tax_jurisdiction.clone()),
            tax_filing_period: Some(t.tax_filing_period.clone()),
            tax_rate_ref: t.tax_rate_ref.clone(),
            invoice_item_ref: None,
            sku_or_plan_ref: None,
            price_id: None,
            pricing_snapshot_ref: None,
            po_allocation_group: None,
            credit_grant_event_type: None,
            ar_status: None,
        });
    }

    Ok(PostEntry {
        entry_id,
        tenant_id: inv.seller_tenant_id,
        period_id: inv.period_id.clone(),
        entry_currency: currency,
        source_doc_type: SourceDocType::InvoicePost,
        source_business_id: inv.invoice_id.clone(),
        effective_at: inv.effective_at,
        posted_by_actor_id: inv.posted_by_actor_id,
        correlation_id: inv.correlation_id,
        reverses_entry_id: None,
        reverses_period_id: None,
        lines,
    })
}

/// Grouping key for the CR Revenue lines — the stored *string* forms of the
/// dims (the SDK enums are not `Ord`, but a `BTreeMap` key must be). Ordering is
/// derived so the built line order is deterministic across recomputes.
#[domain_model]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RevenueKey {
    account_class: String,
    gl_code: String,
    mapping_status: String,
    revenue_stream: String,
}

/// Running fold of one revenue group: summed ex-tax amount (exact until final narrowing), the typed dims to emit on the line, and the first
/// item's source refs.
#[domain_model]
struct RevenueAgg {
    amount: ExactAmount,
    account_class: AccountClass,
    gl_code: Option<String>,
    mapping_status: MappingStatus,
    refs: ItemRefs,
}

/// Running fold of one stream's deferred (Contract-liability) credit: the summed
/// deferred amount (exact until final narrowing) and the first
/// deferring item's source refs. The class is fixed (`CONTRACT_LIABILITY`) and
/// the stream is the map key, so neither is stored here.
#[domain_model]
struct DeferredAgg {
    amount: ExactAmount,
    refs: ItemRefs,
}

/// The per-line source refs carried from the (first) item of a revenue group.
#[domain_model]
struct ItemRefs {
    invoice_item_ref: Option<String>,
    sku_or_plan_ref: Option<String>,
    price_id: Option<String>,
    pricing_snapshot_ref: Option<String>,
}

impl From<&InvoiceItem> for ItemRefs {
    fn from(item: &InvoiceItem) -> Self {
        Self {
            invoice_item_ref: item.invoice_item_ref.clone(),
            sku_or_plan_ref: item.sku_or_plan_ref.clone(),
            price_id: item.price_id.clone(),
            pricing_snapshot_ref: item.pricing_snapshot_ref.clone(),
        }
    }
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
