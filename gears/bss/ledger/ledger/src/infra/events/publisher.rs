//! `LedgerEventPublisher` — the adapter that would front the platform event broker.
//!
//! Built once at `init()` (see [`crate::module`]) and threaded into the posting
//! engine and the background jobs.
//!
//! TODO(broker): the event broker (`event-broker-sdk`) is not yet available in
//! gears-rust, so this publisher is **parked**: every `publish_*` call logs and
//! returns `Ok(())` (the posting txn is unaffected), and `emit_invariant_alarm`
//! still mirrors the alarm into the Prometheus counter but does not publish.
//! When the broker lands, restore the transactional-outbox producers here: hold
//! one `event_broker_sdk::AsyncProducer` per event type (posted, entry-reversed,
//! dispute-recorded, settlement-returned, revenue-recognized,
//! revenue-recognition-reversed, schedule-changed, credit-note-posted,
//! debit-note-posted, refund-recorded, manual-adjustment-posted,
//! fx-revaluation-completed, fx-revaluation-reversed, period-closed,
//! reconciliation-completed, and the invariant alarm), and have each `publish_*`
//! call `producer.publish(ctx, txn, event).await` inside the caller's
//! transaction (transactional outbox: the outbox row commits atomically with the
//! entry, or not at all).

use std::sync::Arc;

use toolkit_security::SecurityContext;

use crate::domain::ports::metrics::LedgerMetricsPort;
use crate::infra::events::payloads::{
    CreditNotePosted, DebitNotePosted, LedgerDisputeRecorded, LedgerEntryPosted,
    LedgerEntryReversed, LedgerFxRevaluationCompleted, LedgerFxRevaluationReversed,
    LedgerInvariantAlarm, LedgerPeriodClosed, LedgerReconciliationCompleted,
    LedgerRevenueRecognitionReversed, LedgerRevenueRecognized, LedgerScheduleChanged,
    LedgerSettlementReturned, ManualAdjustmentPosted, RefundRecorded,
};

/// Publishes ledger events through the platform event broker.
///
/// Broker parked (see module docs): the only live field is the optional metrics
/// handle used to mirror the invariant-alarm counter. When the broker lands, the
/// per-event `Option<AsyncProducer>` fields + the alarm producer + the
/// `DBProvider` for the alarm's own transaction come back here.
pub struct LedgerEventPublisher {
    /// Metrics handle for the alarm-counter mirror (`ledger_alarm_total`).
    /// `Option` so the `noop` publisher (broker + metrics absent at `init()`)
    /// needs no metrics dependency; when present, the counter increments even
    /// with no broker.
    metrics: Option<Arc<dyn LedgerMetricsPort>>,
}

/// Failure publishing the posted event into the caller's transaction. The
/// caller maps this into the txn-rollback path so the event and the entry are
/// all-or-nothing.
///
/// Retained (even while the broker is parked) so the `publish_*` signatures and
/// the caller call sites are unchanged; every parked publish returns `Ok(())`.
#[derive(Debug, thiserror::Error)]
pub enum EventPublishError {
    /// Outbox enqueue / schema validation failed.
    #[error("event publish: {0}")]
    Publish(String),
}

/// Log a would-be event and return `Ok(())`. Broker parked: no outbox enqueue.
macro_rules! parked_publish {
    ($self:ident, $ctx:ident, $txn:ident, $event:ident, $name:literal) => {{
        // TODO(broker): re-emit `$name` via the transactional outbox once
        // `event-broker-sdk` lands — was `producer.publish($ctx, $txn, $event).await`.
        tracing::trace!(event = $name, "bss-ledger: event skipped (broker parked)");
        let _ = (&$self.metrics, $ctx, $txn, $event);
        Ok(())
    }};
}

impl LedgerEventPublisher {
    /// Build a publisher that mirrors the invariant-alarm counter into `metrics`
    /// but publishes no events (broker parked). Used at `init()` when events are
    /// enabled; the alarm counter is still observable in Prometheus.
    #[must_use]
    pub fn with_metrics(metrics: Arc<dyn LedgerMetricsPort>) -> Self {
        Self {
            metrics: Some(metrics),
        }
    }

    /// Build a no-op publisher: every event is skipped and no metric is mirrored.
    /// Used when the broker is absent at `init()` and by the unit/integration
    /// tests that exercise the broker-absent path.
    #[must_use]
    pub fn noop() -> Self {
        Self { metrics: None }
    }

    /// Publish the posted event into the caller's transaction. Broker parked:
    /// logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] when the broker is wired and the outbox
    /// enqueue or schema validation fails; currently never returns `Err`.
    pub async fn publish_entry_posted(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerEntryPosted,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.entry.posted")
    }

    /// Publish the entry-reversed event into the caller's transaction. Broker
    /// parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_entry_reversed(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerEntryReversed,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.entry.reversed")
    }

    /// Publish the dispute-recorded event into the caller's transaction. Broker
    /// parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_dispute_recorded(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerDisputeRecorded,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.dispute.recorded")
    }

    /// Publish the settlement-returned event into the caller's transaction.
    /// Broker parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_settlement_returned(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerSettlementReturned,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.settlement.returned")
    }

    /// Publish the revenue-recognized event into the caller's transaction.
    /// Broker parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_revenue_recognized(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerRevenueRecognized,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.revenue.recognized")
    }

    /// Publish the revenue-recognition-reversed event into the caller's
    /// transaction. Broker parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_revenue_recognition_reversed(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerRevenueRecognitionReversed,
    ) -> Result<(), EventPublishError> {
        parked_publish!(
            self,
            ctx,
            txn,
            event,
            "billing.ledger.revenue.recognition_reversed"
        )
    }

    /// Publish the schedule-changed event into the caller's transaction. Broker
    /// parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_schedule_changed(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerScheduleChanged,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.schedule.changed")
    }

    /// Publish the credit-note-posted event into the caller's transaction. Broker
    /// parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_credit_note_posted(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: CreditNotePosted,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.credit_note.posted")
    }

    /// Publish the debit-note-posted event into the caller's transaction. Broker
    /// parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_debit_note_posted(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: DebitNotePosted,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.debit_note.posted")
    }

    /// Publish the refund-recorded event into the caller's transaction. Broker
    /// parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_refund_recorded(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: RefundRecorded,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.refund.recorded")
    }

    /// Publish the manual-adjustment-posted event into the caller's transaction.
    /// Broker parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_manual_adjustment_posted(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: ManualAdjustmentPosted,
    ) -> Result<(), EventPublishError> {
        parked_publish!(
            self,
            ctx,
            txn,
            event,
            "billing.ledger.manual_adjustment.posted"
        )
    }

    /// Publish the fx-revaluation-completed event into the caller's transaction.
    /// Broker parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_fx_revaluation_completed(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerFxRevaluationCompleted,
    ) -> Result<(), EventPublishError> {
        parked_publish!(
            self,
            ctx,
            txn,
            event,
            "billing.ledger.fx.revaluation_completed"
        )
    }

    /// Publish the fx-revaluation-reversed event into the caller's transaction.
    /// Broker parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_fx_revaluation_reversed(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerFxRevaluationReversed,
    ) -> Result<(), EventPublishError> {
        parked_publish!(
            self,
            ctx,
            txn,
            event,
            "billing.ledger.fx.revaluation_reversed"
        )
    }

    /// Publish the period-closed event into the caller's transaction. Broker
    /// parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_period_closed(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerPeriodClosed,
    ) -> Result<(), EventPublishError> {
        parked_publish!(self, ctx, txn, event, "billing.ledger.period.closed")
    }

    /// Publish the reconciliation-completed event into the caller's transaction.
    /// Broker parked: logs and returns `Ok(())`.
    ///
    /// # Errors
    /// [`EventPublishError::Publish`] once the broker is wired; currently never.
    pub async fn publish_reconciliation_completed(
        &self,
        ctx: &SecurityContext,
        txn: &toolkit_db::secure::DbTx<'_>,
        event: LedgerReconciliationCompleted,
    ) -> Result<(), EventPublishError> {
        parked_publish!(
            self,
            ctx,
            txn,
            event,
            "billing.ledger.reconciliation.completed"
        )
    }

    /// Emit an invariant alarm. The Prometheus counter mirror
    /// (`ledger_alarm_total`) fires so the alarm is observable even with no
    /// broker. Broker parked: the durable transactional-outbox publish is
    /// skipped (logged).
    ///
    /// TODO(broker): when `event-broker-sdk` lands, open the alarm's OWN
    /// transaction (the alarm fires after the post rolled back, so there is no
    /// caller txn to ride) and enqueue the alarm into the transactional outbox so
    /// the relay delivers it at-least-once.
    pub async fn emit_invariant_alarm(&self, ctx: &SecurityContext, alarm: LedgerInvariantAlarm) {
        // Counter mirror first, independent of the durable outbox: the alarm is
        // observable in Prometheus/Alertmanager even when the broker is absent.
        if let Some(metrics) = self.metrics.as_ref() {
            metrics.invariant_alarm(alarm.category.as_str(), alarm.severity.as_str());
        }
        tracing::warn!(
            category = alarm.category.as_str(),
            "bss-ledger: invariant alarm not published (broker parked)"
        );
        let _ = ctx;
    }
}

#[cfg(test)]
#[path = "publisher_tests.rs"]
mod tests;
