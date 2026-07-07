//! Payment domain (architecture §5.2 — Pattern A settle + oldest-first
//! allocation). Pure, backend-agnostic builders for the two payment postings and
//! the precedence policy that splits a lump across open invoices:
//!
//! - [`settlement`] — `build_settlement_entry`: the cash-landing entry
//!   (`PAYMENT_SETTLE`). DR `CASH_CLEARING` + optional DR `PSP_FEE_EXPENSE`,
//!   CR `UNALLOCATED` for the gross. Money lands in the unallocated pool.
//! - [`settlement_return`] — `build_settlement_return_entry`: the clawback entry
//!   (`SETTLEMENT_RETURN`). DR `UNALLOCATED` / CR `CASH_CLEARING`, removing a
//!   returned receipt from the pool.
//! - [`chargeback`] — `build_chargeback_entry`: the dispute entry (`CHARGEBACK`).
//!   Group B builds the `opened` phase in both variants — cash-hold
//!   (`DR DISPUTE_HOLD / CR CASH_CLEARING`) and AR-reclass (two AR legs that
//!   reclass `ACTIVE → DISPUTED`, AR-class-neutral). `won`/`lost` are Group C.
//! - [`precedence`] — `oldest_first`: the `oldest-first.v1` policy that
//!   sequentially fills open invoices from a lump (oldest first, optional hint to
//!   the front), pure `i64`, no proportional split.
//! - [`allocation`] — `build_allocation_entry`: the apply entry
//!   (`PAYMENT_ALLOCATE`). DR `UNALLOCATED` (Σ splits) + CR `AR` per invoice,
//!   draining the pool into the receivables it pays.
//! - [`credit`] — reusable-credit (wallet) money-shapes: `build_grant_entry`
//!   (`CREDIT_APPLY`; DR `UNALLOCATED` / CR `REUSABLE_CREDIT`, parking pool cash
//!   into the wallet), `build_apply_entry` (`CREDIT_APPLY`; N×DR `REUSABLE_CREDIT`
//!   / M×CR `AR`, spending the wallet against receivables), the
//!   `plan_wallet_debit` sub-grain fill planner, and the `validate_credit_targets`
//!   AR-target validator.
//!
//! Every module here is pure (no infra / DB imports — dylint DE0301): it computes
//! over SDK value types and produces SDK `PostEntry`/`PostLine` with placeholder
//! header + `account_id` fields the `crate::infra` orchestrator binds before
//! posting (exactly like the invoice builder).

pub mod allocation;
pub mod chargeback;
pub mod credit;
pub mod precedence;
pub mod settlement;
pub mod settlement_return;
