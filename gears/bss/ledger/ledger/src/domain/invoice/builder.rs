//! Direct-split invoice-entry builder (architecture §5.1, **Variant A** +
//! Slice 4 deferral split). Turns an invoice into a balanced [`PostEntry`]:
//!
//! - **DR AR** — one line for the gross receivable (`Σ items ex-tax + Σ tax`).
//! - **CR Revenue** — one line per `revenue_stream` (grouped sum of the
//!   *recognized-now* portion of the ex-tax item amounts in that stream —
//!   `amount − deferred`), carrying the resolved [`MappedLine`] class/status.
//! - **CR Contract-liability** — one line per `revenue_stream` whose items
//!   defer a non-zero amount (the grouped `Σ deferred` in that stream), same
//!   `revenue_stream` as the Revenue line (per-stream disaggregation, §3.5).
//! - **CR Tax** — one line per [`TaxBreakdown`], carrying its tax dims.
//!
//! **The deferral split (Slice 4, design §3.1 / Variant A) is driven entirely by
//! [`InvoiceItem::deferred_minor`]** — the per-item deferred amount the
//! recognition derivation ([`crate::domain::recognition`]) computes *before* the
//! builder and threads in on each item. `deferred_minor == 0` for **every** item
//! (the default, and the only case before Slice 4) emits NO Contract-liability
//! line and is **byte-identical** to the prior Variant-A output — the public
//! invoice-post contract is unchanged for non-deferred invoices.
//!
//! Money is pure `i64` summation: `Σ DR == Σ CR` exactly, no proportional split
//! and no residual rounding (the segment residual is the recognition builder's
//! concern; here `deferred` is already an exact per-item i64). Scale is NOT set
//! here — the foundation `CurrencyScaleResolver` fills each line's
//! `currency_scale` at post time, so every amount stays in the invoice's own
//! minor units. The emitted lines carry a placeholder nil `account_id`; the
//! posting glue binds the real chart row from
//! `(account_class, currency, revenue_stream)` before posting.

use std::collections::BTreeMap;

use bss_ledger_sdk::{AccountClass, MappingStatus, PostEntry, PostLine, Side, SourceDocType};
use chrono::NaiveDate;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::invoice::mapping::MappedLine;

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
    /// Ex-tax amount in the invoice's minor units. Must be `>= 0`.
    pub amount_minor_ex_tax: i64,
    /// The portion of [`Self::amount_minor_ex_tax`] deferred to
    /// `CONTRACT_LIABILITY` (Slice 4). The recognition derivation
    /// ([`crate::domain::recognition::builder::ScheduleBuilder`]) computes this
    /// *before* the builder and threads it onto the item; the builder credits
    /// `amount − deferred` to Revenue and `deferred` to Contract-liability on the
    /// SAME `revenue_stream`. `0` (the default, and absence-of-recognition) ⇒ the
    /// whole amount recognizes now and NO Contract-liability line is emitted
    /// (byte-identical to the pre-Slice-4 Variant-A output). Invariant:
    /// `0 <= deferred_minor <= amount_minor_ex_tax`.
    pub deferred_minor: i64,
    /// ISO currency of the item (every item + tax shares the invoice currency).
    pub currency: String,
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
    /// item is fully recognized now (`deferred_minor` stays `0`, today's
    /// Variant-A behaviour). When present, the orchestrator
    /// ([`crate::infra::invoice_post`]) derives [`Self::deferred_minor`] + the
    /// schedule plan from it via the recognition
    /// [`ScheduleBuilder`](crate::domain::recognition::builder::ScheduleBuilder)
    /// *before* the builder runs. Carried on the domain item (not consumed by the
    /// pure builder, which reads only the already-derived `deferred_minor`) so
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
    /// Tax amount in the invoice's minor units. Must be `>= 0`.
    pub amount_minor: i64,
    /// ISO currency (matches the invoice currency).
    pub currency: String,
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

impl PostedInvoice {
    /// Entry currency = the invoice currency. Taken from the first item, else
    /// the first tax breakdown; `None` for a degenerate empty invoice (rejected
    /// downstream by the foundation empty-entry invariant).
    #[must_use]
    pub fn currency(&self) -> Option<&str> {
        self.items
            .first()
            .map(|i| i.currency.as_str())
            .or_else(|| self.tax.first().map(|t| t.currency.as_str()))
    }

    /// Gross receivable in minor units: `Σ items ex-tax + Σ tax`. Pure `i64`
    /// summation (widened to `i128` while folding to avoid an intermediate
    /// overflow), the exact total the single DR AR line carries.
    #[must_use]
    pub fn gross_minor(&self) -> i64 {
        let items: i128 = self
            .items
            .iter()
            .map(|i| i128::from(i.amount_minor_ex_tax))
            .sum();
        let tax: i128 = self.tax.iter().map(|t| i128::from(t.amount_minor)).sum();
        // The foundation headroom guard keeps a single invoice within i64; a
        // pathological overflow saturates rather than panicking (the unbalanced
        // / amount guards then reject the entry).
        i64::try_from(items + tax).unwrap_or(i64::MAX)
    }
}

/// Build the balanced direct-split entry for `inv`, using `mapped[i]` as the
/// resolved GL target of `inv.items[i]` (positional; lengths must match).
///
/// Lines: one DR AR (gross), one CR Revenue per distinct
/// `(account_class, gl_code, mapping_status, revenue_stream)` group (summing the
/// *recognized-now* `amount − deferred` of each item), one CR Contract-liability
/// per `revenue_stream` whose items defer a non-zero amount (summing the per-item
/// `deferred_minor`), one CR Tax per [`TaxBreakdown`]. The AR carries
/// `invoice_id` + `due_date`; every Revenue / Contract-liability line carries its
/// `revenue_stream`; every Tax line carries its dims. `source_doc_type =
/// INVOICE_POST`, `source_business_id = invoice_id`, `reverses_* = None`.
///
/// **Deferral (Slice 4):** each item's [`InvoiceItem::deferred_minor`] (computed
/// upstream by the recognition derivation) splits its stream's credit into
/// Revenue (`amount − deferred`) + Contract-liability (`deferred`), same stream
/// on both. When every item defers `0` (the default) NO Contract-liability line
/// is emitted and the output is byte-identical to the pre-Slice-4 Variant-A
/// entry. `Σ DR == Σ CR` stays exact (`i64`): the split only re-labels part of an
/// already-balanced credit.
///
/// # Panics
/// Debug-asserts `mapped.len() == inv.items.len()`; in release a length
/// mismatch silently maps only the overlapping prefix (the glue always passes a
/// 1:1 vector).
#[must_use]
pub fn build_invoice_entry(inv: &PostedInvoice, mapped: &[MappedLine]) -> PostEntry {
    debug_assert_eq!(
        mapped.len(),
        inv.items.len(),
        "one MappedLine per invoice item"
    );
    let entry_id = Uuid::now_v7();
    let currency = inv.currency().unwrap_or_default().to_owned();

    // Worst case: 1 AR + one Revenue + one Contract-liability per item + one Tax
    // per breakdown.
    let mut lines: Vec<PostLine> = Vec::with_capacity(1 + 2 * inv.items.len() + inv.tax.len());

    // DR AR — the gross receivable (incl. tax). Single payer per entry.
    lines.push(PostLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: inv.payer_tenant_id,
        seller_tenant_id: Some(inv.seller_tenant_id),
        resource_tenant_id: inv.resource_tenant_id,
        account_id: Uuid::nil(),
        account_class: AccountClass::Ar,
        gl_code: None,
        side: Side::Debit,
        amount_minor: inv.gross_minor(),
        currency: currency.clone(),
        invoice_id: Some(inv.invoice_id.clone()),
        due_date: inv.due_date,
        revenue_stream: None,
        mapping_status: MappingStatus::Resolved,
        functional_amount_minor: None,
        functional_currency: None,
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
    // emitted and the entry is byte-identical to the pre-Slice-4 output.
    let mut deferred: BTreeMap<String, DeferredAgg> = BTreeMap::new();
    for (item, m) in inv.items.iter().zip(mapped.iter()) {
        // Clamp defensively: the recognition derivation guarantees
        // `0 <= deferred <= amount`, but a malformed input must never produce a
        // negative recognized-now credit (which would unbalance the entry) — the
        // orchestrator validates the invariant before calling, and the
        // foundation unbalanced guard is the backstop.
        //
        // `.max(0)` on the upper bound so a (rejected-at-the-boundary but
        // domain-constructible) negative `amount_minor_ex_tax` cannot make this
        // `clamp(0, negative)` panic with `min > max`; a negative amount then
        // clamps deferred to 0 and the unbalanced guard rejects the entry.
        let deferred_minor = item
            .deferred_minor
            .clamp(0, item.amount_minor_ex_tax.max(0));
        let recognized_now = item.amount_minor_ex_tax - deferred_minor;

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
            amount_minor: 0,
            account_class: m.account_class,
            gl_code: m.gl_code.clone(),
            mapping_status: m.mapping_status,
            // First item in the group seeds the line-level source refs.
            refs: ItemRefs::from(item),
        });
        agg.amount_minor += i128::from(recognized_now);

        // Fold the deferred remainder into its stream's Contract-liability line.
        if deferred_minor > 0 {
            let cl = deferred
                .entry(item.revenue_stream.clone())
                .or_insert_with(|| DeferredAgg {
                    amount_minor: 0,
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
            cl.amount_minor += i128::from(deferred_minor);
        }
    }
    for (key, agg) in revenue {
        // A stream whose entire recognized-now amount deferred (Slice 4) sums to
        // 0 — emit NO Revenue line: the engine rejects a zero-amount line, and the
        // deferred amount is carried by the CONTRACT_LIABILITY line below. (CL is
        // already only emitted for `deferred > 0`, so a fully-deferred item yields
        // a lone CONTRACT_LIABILITY credit, balanced against the AR/tax debit.)
        if agg.amount_minor == 0 {
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
            amount_minor: i64::try_from(agg.amount_minor).unwrap_or(i64::MAX),
            currency: currency.clone(),
            invoice_id: Some(inv.invoice_id.clone()),
            due_date: None,
            // Every Revenue line carries its stream (the DB CHECK requires it).
            revenue_stream: Some(key.revenue_stream),
            mapping_status: agg.mapping_status,
            functional_amount_minor: None,
            functional_currency: None,
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
            amount_minor: i64::try_from(agg.amount_minor).unwrap_or(i64::MAX),
            currency: currency.clone(),
            invoice_id: Some(inv.invoice_id.clone()),
            due_date: None,
            revenue_stream: Some(revenue_stream),
            mapping_status: MappingStatus::Resolved,
            functional_amount_minor: None,
            functional_currency: None,
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
        lines.push(PostLine {
            line_id: Uuid::now_v7(),
            payer_tenant_id: inv.payer_tenant_id,
            seller_tenant_id: Some(inv.seller_tenant_id),
            resource_tenant_id: inv.resource_tenant_id,
            account_id: Uuid::nil(),
            account_class: AccountClass::TaxPayable,
            gl_code: None,
            side: Side::Credit,
            amount_minor: t.amount_minor,
            currency: currency.clone(),
            invoice_id: Some(inv.invoice_id.clone()),
            due_date: None,
            revenue_stream: None,
            mapping_status: MappingStatus::Resolved,
            functional_amount_minor: None,
            functional_currency: None,
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

    PostEntry {
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
    }
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

/// Running fold of one revenue group: summed ex-tax amount (`i128` to avoid an
/// intermediate overflow), the typed dims to emit on the line, and the first
/// item's source refs.
#[domain_model]
struct RevenueAgg {
    amount_minor: i128,
    account_class: AccountClass,
    gl_code: Option<String>,
    mapping_status: MappingStatus,
    refs: ItemRefs,
}

/// Running fold of one stream's deferred (Contract-liability) credit: the summed
/// deferred amount (`i128` to avoid an intermediate overflow) and the first
/// deferring item's source refs. The class is fixed (`CONTRACT_LIABILITY`) and
/// the stream is the map key, so neither is stored here.
#[domain_model]
struct DeferredAgg {
    amount_minor: i128,
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
