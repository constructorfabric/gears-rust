//! Settlement-entry builder (architecture §5.2, **Pattern A**). Turns a settled
//! payment into a balanced [`PostEntry`] that lands the cash in the unallocated
//! pool:
//!
//! - **DR `CASH_CLEARING`** — the net cash received (`gross - fee`).
//! - **DR `PSP_FEE_EXPENSE`** — the processor fee (`fee`); omitted when `fee == 0`
//!   (no zero line).
//! - **CR `UNALLOCATED`** — the gross (`gross`); the whole receipt parks in the
//!   pool, drained later by an allocation entry.
//!
//! `Σ DR (= net + fee = gross) == Σ CR (= gross)` exactly, using bounded exact major-unit arithmetic. The emitted lines carry a placeholder
//! nil `account_id` (the `crate::infra` orchestrator binds the real chart row from
//! `(account_class, currency)` before posting) and placeholder header fields it
//! likewise overwrites (`period_id`, `posted_by_actor_id`, `correlation_id`, and
//! — when `effective_at` is `None` — `effective_at`).

use bss_ledger_sdk::{AccountClass, MappingStatus, PostEntry, PostLine, Side, SourceDocType};

use crate::domain::exact_money::{matching_spec, subtract_posted};
use bss_ledger_sdk::PostedMoney;
use rust_decimal::Decimal;
use toolkit_macros::domain_model;
use uuid::Uuid;

use chrono::NaiveDate;

use crate::domain::error::DomainError;
use crate::domain::instant::to_naive_date;
use time::OffsetDateTime;

/// A settled payment to post (Pattern A input). `gross` is the amount the
/// payer was charged; `fee` is the processor's cut withheld from it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlementInput {
    /// The seller tenant whose ledger this posts into (`= entry.tenant_id`), and
    /// the `seller_tenant_id` stamped on each line.
    pub tenant_id: Uuid,
    /// The tenant that paid (the single payer of the entry).
    pub payer_tenant_id: Uuid,
    /// External payment identity — the `PAYMENT_SETTLE` idempotency business id
    /// (`source_business_id`).
    pub payment_id: String,
    /// Gross amount received in major units (what the payer was charged). Must be
    /// `> 0` and `> fee` (the net parked in the pool must be positive).
    pub gross: PostedMoney,
    /// Processor fee withheld in major units. Must be `>= 0` and `< gross`.
    pub fee: PostedMoney,
    /// Settlement instant. `None` ⇒ a placeholder effective date the orchestrator
    /// overwrites before posting (see module docs).
    pub effective_at: Option<OffsetDateTime>,
}

/// Build the balanced Pattern-A settlement entry for `input`.
///
/// Lines: DR `CASH_CLEARING` (`gross - fee`), DR `PSP_FEE_EXPENSE` (`fee`, omitted
/// when zero), CR `UNALLOCATED` (`gross`). `source_doc_type = PAYMENT_SETTLE`,
/// `source_business_id = payment_id`, `reverses_* = None`. Every line carries the
/// payer, the currency, and `seller_tenant_id = Some(tenant_id)`; `invoice_id` is
/// `None` (the receipt is not yet tied to a receivable).
///
/// # Errors
/// Money metadata conflicts and final posting-range failures retain their named domain errors.
/// [`DomainError::InvalidRequest`] when `gross <= 0`, `fee < 0`, or
/// `fee >= gross` (a meaningless / unrepresentable settlement, or a
/// 100%-fee settlement that would park a zero net in the pool).
pub fn build_settlement_entry(input: &SettlementInput) -> Result<PostEntry, DomainError> {
    matching_spec(&input.gross, &input.fee)?;
    // Reject a non-positive gross at the boundary with a precise `InvalidRequest`.
    // A zero gross would emit only zero-amount lines (`DR CASH_CLEARING 0` / `CR
    // UNALLOCATED 0`) which the engine rejects deep down as the misleading
    // `AMOUNT_OUT_OF_RANGE`; catch the meaningless settlement here (there is
    // nothing to park in the pool) where the wire code is clear.
    if input.gross.amount() <= Decimal::ZERO {
        return Err(DomainError::InvalidRequest(format!(
            "settlement gross must be > 0, got {}",
            input.gross
        )));
    }
    if input.fee.amount() < Decimal::ZERO {
        return Err(DomainError::InvalidRequest(format!(
            "settlement fee must be >= 0, got {}",
            input.fee
        )));
    }
    // Reject `fee >= gross` (net <= 0): a 100%-fee settlement parks nothing in the
    // pool — `net = 0` would emit a `DR CASH_CLEARING 0` line the engine rejects
    // deep down as the misleading `AMOUNT_OUT_OF_RANGE`. Catch it here with a clear
    // wire code (mirroring the `gross <= 0` guard): the net cash must be strictly
    // positive for there to be a receipt to settle. This also keeps `net > 0` for
    // every settled payment, so a later CASH_HOLD dispute always has a positive net
    // to size its hold against.
    if input.fee.amount() >= input.gross.amount() {
        return Err(DomainError::InvalidRequest(format!(
            "settlement fee ({}) must be < gross ({}) — net cash must be > 0",
            input.fee, input.gross
        )));
    }

    // Net cash = gross - fee. Both are non-negative and fee < gross (checked
    // above), so the final difference must be a strictly positive posting.
    let net = subtract_posted(&input.gross, &input.fee)?;

    // A nil account_id / Resolved status line carrying the entry-wide payer +
    // currency; only the class / side / amount differ per line.
    let line = |account_class: AccountClass, side: Side, amount: PostedMoney| PostLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: input.payer_tenant_id,
        seller_tenant_id: Some(input.tenant_id),
        resource_tenant_id: None,
        account_id: Uuid::nil(),
        account_class,
        gl_code: None,
        side,
        money: amount,
        invoice_id: None,
        due_date: None,
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
    };

    // DR CASH_CLEARING (net) + optional DR PSP_FEE_EXPENSE (fee) + CR UNALLOCATED
    // (gross). Σ DR = net + fee = gross = Σ CR.
    let mut lines: Vec<PostLine> = Vec::with_capacity(3);
    lines.push(line(AccountClass::CashClearing, Side::Debit, net));
    if input.fee.amount() > Decimal::ZERO {
        // Omit the fee line entirely when there is no fee — never a zero line.
        lines.push(line(
            AccountClass::PspFeeExpense,
            Side::Debit,
            input.fee.clone(),
        ));
    }
    lines.push(line(
        AccountClass::Unallocated,
        Side::Credit,
        input.gross.clone(),
    ));

    Ok(PostEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: input.tenant_id,
        // Placeholder header fields the infra orchestrator overwrites before
        // posting (it derives the period, stamps the actor/correlation, and fills
        // a real effective date for the `None` case) — mirrors the nil account_id.
        period_id: String::new(),
        entry_currency: input.gross.currency().code().to_owned(),
        source_doc_type: SourceDocType::PaymentSettle,
        source_business_id: input.payment_id.clone(),
        effective_at: input.effective_at.map_or(
            NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or(NaiveDate::MIN),
            to_naive_date,
        ),
        posted_by_actor_id: Uuid::nil(),
        correlation_id: Uuid::nil(),
        reverses_entry_id: None,
        reverses_period_id: None,
        lines,
    })
}

#[cfg(test)]
#[path = "settlement_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "r8a_tests.rs"]
mod decimal_tests;
