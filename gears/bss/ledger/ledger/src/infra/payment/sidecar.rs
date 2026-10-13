//! The payment post sidecars — in-transaction [`PostSidecar`] hooks the
//! settle / allocate orchestrators thread into the posting engine.
//!
//! [`SettlementSidecar`] seeds the `payment_settlement` counter row for a fresh
//! settlement. [`AllocationSidecar`] applies one allocation: it bumps
//! `allocated_minor` (the per-payment cap CHECK is the `SERIALIZABLE`
//! concurrency backstop — an over-cap surfaces as
//! [`DomainError::MoneyOutCapExceeded`]), inserts the N `payment_allocation`
//! rows, then bumps the per-`(payment, invoice)` allocation-refund counter.
//!
//! Both run inside the SAME serializable transaction as the journal post (see
//! the [`PostSidecar`] contract): their writes commit atomically with the entry
//! or roll back with it. This is INFRA code, so the wall clock (`OffsetDateTime::now_utc()`)
//! for the `allocated_at_utc` audit stamp is allowed here.

use std::sync::Arc;

use bss_ledger_sdk::PostedMoney;
use rust_decimal::Decimal;

use toolkit_db::secure::{AccessScope, DbTx};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::exact_money::map_money_error;
use crate::domain::model::RepoError;
use crate::domain::payment::chargeback::DisputeVariant;
use crate::domain::payment::dispute_state::check_outcome;
use crate::infra::adjustment::map_adjustment_repo_err;
use crate::infra::events::payloads::{LedgerDisputeRecorded, LedgerSettlementReturned};
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::posting::service::{PostSidecar, PostedFacts};
use crate::infra::storage::repo::dispute_repo::DisputeState;
use crate::infra::storage::repo::payment_repo::{NewAllocationRow, SettlementCounter};
use crate::infra::storage::repo::{DisputeRepo, PaymentRepo};
use time::OffsetDateTime;

/// In-transaction sidecar for a payment settlement: seeds the
/// `payment_settlement` counter row so a later allocation can net against it.
pub struct SettlementSidecar {
    pub tenant: Uuid,
    pub payment_id: String,
    pub gross: PostedMoney,
    pub fee: PostedMoney,
    pub payment: PaymentRepo,
}

/// In-transaction sidecar for a settlement return (Model N, D1): decrements BOTH
/// the original payment's `settled_minor` (by the returned gross) AND its
/// `fee_minor` (by the reversed proportional `fee_share_minor`), so the
/// clawed-back receipt no longer counts as settled and `net = settled − fee`
/// stays consistent. It also publishes `billing.ledger.settlement.returned` IN
/// the same post txn (transactional outbox), so the event commits atomically with
/// the entry + the counter decrements, or rolls back with them (mirrors
/// [`ChargebackSidecar`]). The per-payment cap CHECKs are the `SERIALIZABLE`
/// backstop — an over-claw (return / fee-reverse more than is still returnable)
/// surfaces as [`DomainError::SettlementReturnOverAllocated`].
pub struct SettlementReturnSidecar {
    pub tenant: Uuid,
    pub payment_id: String,
    /// External return identity — the event's `psp_return_id`.
    pub psp_return_id: String,
    /// The returned gross (decremented from `settled`; the event amount).
    pub amount: PostedMoney,
    /// The proportional fee slice reversed (decremented from `fee`). Zero
    /// when the original settle had no fee.
    pub fee_share: PostedMoney,
    /// The payment counter store the decrements are written through.
    pub payment: PaymentRepo,
    /// The event publisher: `billing.ledger.settlement.returned` is published IN
    /// this post txn (the transactional outbox) so it commits atomically with the
    /// return entry, or rolls back with it. Mirrors [`ChargebackSidecar`].
    pub publisher: Arc<LedgerEventPublisher>,
    /// The security context for the in-txn outbox publish (the same `ctx` the
    /// engine threads through; cloned by the service into the sidecar).
    pub ctx: SecurityContext,
}

/// In-transaction sidecar for a chargeback phase, lock rank 0 (taken before the
/// rank-1 settlement write). The `op` selects the dispute-row write:
///
/// - [`ChargebackDisputeOp::Open`] seeds the `ledger_dispute` current-state row
///   (the chosen `variant`, `cycle`, `disputed_amount_minor`, `last_phase =
///   OPENED`) so the later `won`/`lost` outcomes can branch on the recorded
///   variant.
/// - [`ChargebackDisputeOp::Advance`] advances the existing row's `last_phase`
///   (to `WON`/`LOST`) and — when `clawed_back_minor > 0` (a `lost` cash-out) —
///   bumps the payment's `clawed_back_minor` under the total money-out cap CHECK.
pub struct ChargebackSidecar {
    pub tenant: Uuid,
    pub dispute_id: String,
    pub payment_id: String,
    pub variant: DisputeVariant,
    pub cycle: i32,
    pub disputed_amount: PostedMoney,
    /// The cash held in `DISPUTE_HOLD` at `opened` (`min(disputed, net)`, Model
    /// N) — persisted on the dispute row by the [`ChargebackDisputeOp::Open`]
    /// write so the later `won`/`lost` outcome sizes its release / forfeit off
    /// THIS amount, not a re-read `settled − fee` (a settlement-return between
    /// `opened` and the outcome would otherwise strand the hold). `0` for
    /// `AR_RECLASS` (no cash leg) and irrelevant on the `Advance` op (it never
    /// rewrites the stored hold).
    pub cash_hold: PostedMoney,
    /// The dispute-row write this phase performs (open vs advance-to-outcome).
    pub op: ChargebackDisputeOp,
    /// The dispute current-state store (rank 0).
    pub dispute: DisputeRepo,
    /// The payment counter store (rank 1).
    pub payment: PaymentRepo,
    /// The event publisher: the `billing.ledger.dispute.recorded` event is
    /// published IN this post txn (the transactional outbox) so it commits
    /// atomically with the dispute entry, or rolls back with it. Wired for every
    /// phase (opened too) — design §6 / C3.
    pub publisher: Arc<LedgerEventPublisher>,
    /// The security context for the in-txn outbox publish (the same `ctx` the
    /// engine threads through; cloned by the service into the sidecar).
    pub ctx: SecurityContext,
}

/// The dispute current-state write a [`ChargebackSidecar`] performs, selected by
/// the phase the service is posting.
pub enum ChargebackDisputeOp {
    /// `opened`: seed the `ledger_dispute` row (`last_phase = OPENED`).
    Open,
    /// `won` / `lost`: advance the existing row to `last_phase`, and (on a
    /// `lost` cash-out) bump `clawed_back_minor` by `clawed_back_minor` under the
    /// per-payment total money-out cap CHECK. `clawed_back_minor == 0` ⇒ no cash
    /// left (a `won`, or a negative-cash documented-loss `lost`), so the counter
    /// is untouched.
    Advance {
        last_phase: crate::domain::payment::chargeback::DisputePhase,
        clawed_back: PostedMoney,
    },
}

/// In-transaction sidecar for a payment allocation: bumps the settled-payment's
/// `allocated_minor`, inserts the per-invoice `payment_allocation` rows, and
/// bumps each `(payment, invoice)` allocation-refund counter.
pub struct AllocationSidecar {
    pub tenant: Uuid,
    pub payer: Uuid,
    pub payment_id: String,
    pub allocation_id: Uuid,
    pub splits: Vec<crate::domain::payment::precedence::Allocated>,
    pub total: PostedMoney,
    pub policy_ref: String,
    pub payment: PaymentRepo,
}

#[async_trait::async_trait]
impl PostSidecar for SettlementSidecar {
    async fn run(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        _posted: &PostedFacts,
    ) -> Result<(), DomainError> {
        self.payment
            .seed_settlement(
                txn,
                scope,
                self.tenant,
                &self.payment_id,
                &self.gross,
                &self.fee,
            )
            .await
            .map_err(map_repo_err)?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl PostSidecar for AllocationSidecar {
    async fn run(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        _posted: &PostedFacts,
    ) -> Result<(), DomainError> {
        // 1. Bump the settled-payment's running allocated total. The
        //    `allocated_minor <= settled_minor` cap CHECK is the SERIALIZABLE
        //    backstop — an over-cap surfaces as `MoneyOutCapExceeded`.
        self.payment
            .add_allocated(txn, scope, self.tenant, &self.payment_id, &self.total)
            .await
            .map_err(map_repo_err)?;

        // 2. Insert one `payment_allocation` row per split.
        let now = OffsetDateTime::now_utc();
        let rows: Vec<NewAllocationRow> = self
            .splits
            .iter()
            .map(|split| NewAllocationRow {
                tenant_id: self.tenant,
                allocation_id: self.allocation_id,
                payer_tenant_id: self.payer,
                payment_id: self.payment_id.clone(),
                invoice_id: split.invoice_id.clone(),
                amount: split.amount.clone(),
                precedence_policy_ref: self.policy_ref.clone(),
                allocated_at_utc: now,
            })
            .collect();
        self.payment
            .insert_allocation_rows(txn, scope, &rows)
            .await
            .map_err(map_repo_err)?;

        // 3. Bump the per-`(payment, invoice)` allocation-refund counter by the
        //    amount this allocation applied (feeds the refund cap downstream).
        //    One locked read for all the split invoices, then a CAS or insert each.
        let splits: Vec<(&str, &PostedMoney)> = self
            .splits
            .iter()
            .map(|split| (split.invoice_id.as_str(), &split.amount))
            .collect();
        self.payment
            .bump_allocation_refunds(txn, scope, self.tenant, &self.payment_id, &splits)
            .await
            .map_err(map_repo_err)?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl PostSidecar for SettlementReturnSidecar {
    async fn run(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        _posted: &PostedFacts,
    ) -> Result<(), DomainError> {
        // Claw the receipt back out (Model N, symmetric reverse): decrement BOTH
        // `settled_minor` (by the returned gross) AND `fee_minor` (by the
        // reversed proportional fee slice), so `net = settled − fee` stays
        // consistent. The per-payment cap CHECKs reject a return that exceeds
        // what is still returnable (over the allocated / refunded / clawed-back
        // total, or below zero) — surfaced as `SettlementReturnOverAllocated`.
        // Decrement `fee_minor` FIRST, then `settled_minor`. The
        // `fee_minor <= settled_minor` CHECK is re-evaluated after EVERY UPDATE,
        // so dropping `settled_minor` to its new (lower) total while `fee_minor`
        // still holds the old (higher) value would trip the CHECK mid-sidecar
        // (a full return: settled 100->0 with fee still 3 ⇒ `3 <= 0` fails).
        // Reversing the fee slice first keeps `fee <= settled` true at every step.
        // Skip the fee write when there is no slice to reverse (avoids a no-op
        // UPDATE + version bump). The `fee_minor >= 0` / `<= settled_minor` CHECKs
        // back this decrement (mapped to `SettlementReturnOverAllocated`).
        //
        // Both decrements land in ONE locked read and ONE version CAS, and the caps
        // are validated on the combined post-state, so the fee-before-settled order
        // no longer matters for the `fee <= settled` cap.
        let fee = if self.fee_share.amount().is_zero() {
            None
        } else {
            Some(negated(&self.fee_share)?)
        };
        let settled = negated(&self.amount)?;
        let mut deltas = Vec::with_capacity(2);
        if let Some(fee) = &fee {
            deltas.push((SettlementCounter::Fee, fee));
        }
        deltas.push((SettlementCounter::Settled, &settled));
        self.payment
            .add_settlement_deltas(txn, scope, self.tenant, &self.payment_id, &deltas)
            .await
            .map_err(map_return_repo_err)?;

        // Publish `billing.ledger.settlement.returned` into the SAME post txn
        // (transactional outbox): the event row commits atomically with the
        // return entry + the counter decrements, or a publish failure rolls the
        // whole post back. Ids + amount only (no PII).
        let amount_minor = crate::infra::v1_payload::v1_minor_units_for(
            &self.amount,
            "amount_minor",
            &format_args!(
                "settlement.returned payment {} return {} tenant {}",
                self.payment_id, self.psp_return_id, self.tenant
            ),
        );
        self.publisher
            .publish_settlement_returned(
                &self.ctx,
                txn,
                LedgerSettlementReturned {
                    payment_id: self.payment_id.clone(),
                    psp_return_id: self.psp_return_id.clone(),
                    tenant_id: self.tenant,
                    amount_minor,
                    currency: self.amount.currency().code().to_owned(),
                },
            )
            .await
            .map_err(|e| DomainError::Internal(format!("publish settlement_returned: {e}")))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl PostSidecar for ChargebackSidecar {
    /// A `won` / `lost` reads the dispute again in the post transaction. It
    /// refuses with `InvalidDisputeTransition` unless the dispute is still
    /// `OPENED` at this cycle.
    ///
    /// The other outcome can commit after this post's out-of-txn guard. It then
    /// has already emptied the hold that this outcome releases. Without this
    /// check, the projector refuses first, with `NegativeBalance`. The losing
    /// outcome must be an invalid transition instead.
    ///
    /// An `opened` has no check here: its own write checks the state machine.
    async fn check_before_projection(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
    ) -> Result<(), DomainError> {
        let ChargebackDisputeOp::Advance { last_phase, .. } = &self.op else {
            return Ok(());
        };
        let observed = self
            .dispute
            .read_dispute_in(txn, scope, self.tenant, &self.dispute_id)
            .await
            .map_err(map_repo_err)?;
        check_outcome(
            &self.dispute_id,
            observed.as_ref().map(DisputeState::observed),
            *last_phase,
            self.cycle,
        )
        .map_err(|e| DomainError::InvalidDisputeTransition(e.to_string()))
    }

    async fn run(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        _posted: &PostedFacts,
    ) -> Result<(), DomainError> {
        // Lock rank 0 — the dispute-state write precedes the rank-1 settlement
        // write. A replay returns before the sidecar, so this is only reached on
        // the first post for `(tenant, CHARGEBACK, dispute_id:cycle:phase)`.
        match &self.op {
            // `opened`: seed the dispute current-state row.
            ChargebackDisputeOp::Open => {
                self.dispute
                    .dispute_upsert(
                        txn,
                        scope,
                        self.tenant,
                        &self.dispute_id,
                        &self.payment_id,
                        self.variant,
                        self.cycle,
                        &self.disputed_amount,
                        &self.cash_hold,
                    )
                    .await
                    .map_err(map_repo_err)?;
            }
            // `won` / `lost`: advance the existing row to the outcome, and (on a
            // `lost` cash-out) bump the payment's `clawed_back_minor` under the
            // total money-out cap CHECK (refunded + clawed <= settled).
            ChargebackDisputeOp::Advance {
                last_phase,
                clawed_back,
            } => {
                self.dispute
                    .dispute_advance(
                        txn,
                        scope,
                        self.tenant,
                        &self.dispute_id,
                        *last_phase,
                        self.cycle,
                        &self.disputed_amount,
                    )
                    .await
                    .map_err(map_repo_err)?;
                if clawed_back.amount() > Decimal::ZERO {
                    self.payment
                        .add_clawed_back(txn, scope, self.tenant, &self.payment_id, clawed_back)
                        .await
                        .map_err(map_clawback_repo_err)?;
                }
            }
        }

        // Publish `billing.ledger.dispute.recorded` into the SAME post txn
        // (transactional outbox): the event row commits atomically with the
        // dispute entry + the dispute-state write, or a publish failure rolls the
        // whole post back. Wired for EVERY phase (opened/won/lost). Ids + enum
        // codes only (no PII / amounts).
        let phase = match &self.op {
            ChargebackDisputeOp::Open => crate::domain::payment::chargeback::DisputePhase::Opened,
            ChargebackDisputeOp::Advance { last_phase, .. } => *last_phase,
        };
        self.publisher
            .publish_dispute_recorded(
                &self.ctx,
                txn,
                LedgerDisputeRecorded {
                    dispute_id: self.dispute_id.clone(),
                    payment_id: self.payment_id.clone(),
                    tenant_id: self.tenant,
                    cycle: self.cycle,
                    phase: phase.as_str().to_owned(),
                    variant: self.variant.as_str().to_owned(),
                },
            )
            .await
            .map_err(|e| DomainError::Internal(format!("publish dispute_recorded: {e}")))?;
        Ok(())
    }
}

/// The signed counter delta for a return decrement (a negated posted amount is
/// still on its posting grid).
fn negated(value: &PostedMoney) -> Result<PostedMoney, DomainError> {
    PostedMoney::try_new(-value.amount(), value.currency().clone()).map_err(map_money_error)
}

/// Map a settlement-return counter [`RepoError`] into [`DomainError`]: a cap
/// CHECK violation (the return would push `settled` below the still-owed
/// allocated / refunded / clawed-back total, or below zero) becomes
/// [`DomainError::SettlementReturnOverAllocated`] (the
/// `SETTLEMENT_RETURN_OVER_ALLOCATED` wire code); every other repo failure goes
/// through the shared table.
fn map_return_repo_err(e: RepoError) -> DomainError {
    match e {
        RepoError::MoneyOutCapExceeded(m) => DomainError::SettlementReturnOverAllocated(m),
        other => map_adjustment_repo_err("settlement-return sidecar", other),
    }
}

/// Map a payment-counter [`RepoError`] into the sidecar's [`DomainError`]: a
/// per-payment cap CHECK violation becomes [`DomainError::MoneyOutCapExceeded`]
/// (the `ALLOCATION_EXCEEDS_SETTLED` wire code); every other repo failure goes
/// through the shared table (a counter CAS race stays retryable, a dispute that
/// is no longer open is an invalid transition, storage faults are internal).
fn map_repo_err(e: RepoError) -> DomainError {
    match e {
        RepoError::MoneyOutCapExceeded(m) => DomainError::MoneyOutCapExceeded(m),
        other => map_adjustment_repo_err("payment sidecar", other),
    }
}

/// Map a chargeback clawback-counter [`RepoError`] into [`DomainError`]: the
/// total money-out cap CHECK violation (`refunded_minor + clawed_back_minor >
/// settled_minor` — the same settlement that was already paid out via a refund
/// can't also be clawed back) becomes [`DomainError::ChargebackExceedsSettled`]
/// (the `CHARGEBACK_EXCEEDS_SETTLED` wire code); every other repo failure goes
/// through the shared table. Distinct from [`map_repo_err`] only in the
/// cap-violation variant it raises.
fn map_clawback_repo_err(e: RepoError) -> DomainError {
    match e {
        RepoError::MoneyOutCapExceeded(m) => DomainError::ChargebackExceedsSettled(m),
        other => map_adjustment_repo_err("chargeback sidecar", other),
    }
}

#[cfg(test)]
#[path = "sidecar_mapping_tests.rs"]
mod mapping_tests;
