//! Event-type GTS schemas and their types-registry registration helper.
//!
//! The JSON-Schema documents describe the [`payloads`](super::payloads) event
//! shapes and are embedded via `include_str!`. They are registered in
//! types-registry at `init()` (decision A — "make P5 real") so the
//! event-broker producers can validate every payload against a fetched schema
//! at `build_async` time (`ValidationTiming::Eager`).
//!
//! The schema `$id` carries the `gts://` URI prefix; the `*_TYPE_ID` consts
//! carry the bare GTS id (the prefix stripped) and match the corresponding
//! `TypedEvent::TYPE_ID` on the payload — a lockstep checked by the unit tests.

use anyhow::Context;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.entry.posted` event type.
pub const ENTRY_POSTED_TYPE_ID: &str = "gts.cf.core.events.event.v1~cf.bss.ledger.entry_posted.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.entry.reversed` event type.
pub const ENTRY_REVERSED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.entry_reversed.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.invariant.alarm` event type.
pub const INVARIANT_ALARM_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.invariant_alarm.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.dispute.recorded` event type.
pub const DISPUTE_RECORDED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.dispute_recorded.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.settlement.returned` event type.
pub const SETTLEMENT_RETURNED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.settlement_returned.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.revenue.recognized` event type.
pub const REVENUE_RECOGNIZED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.revenue_recognized.v1";

/// Bare GTS id (no `gts://` prefix) of the
/// `billing.ledger.revenue.recognition_reversed` event type.
pub const REVENUE_RECOGNITION_REVERSED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.revenue_recognition_reversed.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.schedule.changed` event type.
pub const SCHEDULE_CHANGED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.schedule_changed.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.credit_note.posted` event type.
pub const CREDIT_NOTE_POSTED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.credit_note_posted.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.debit_note.posted` event type.
pub const DEBIT_NOTE_POSTED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.debit_note_posted.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.refund.recorded` event type.
pub const REFUND_RECORDED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.refund_recorded.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.manual_adjustment.posted` event type.
pub const MANUAL_ADJUSTMENT_POSTED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.manual_adjustment_posted.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.fx.revaluation_completed` event type.
pub const FX_REVALUATION_COMPLETED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.fx_revaluation_completed.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.fx.revaluation_reversed` event type.
pub const FX_REVALUATION_REVERSED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.fx_revaluation_reversed.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.period.closed` event type.
pub const PERIOD_CLOSED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.period_closed.v1";

/// Bare GTS id (no `gts://` prefix) of the `billing.ledger.reconciliation.completed`
/// event type.
pub const RECONCILIATION_COMPLETED_TYPE_ID: &str =
    "gts.cf.core.events.event.v1~cf.bss.ledger.reconciliation_completed.v1";

/// Vendored JSON-Schema for the posted-entry event.
const ENTRY_POSTED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_entry_posted.v1.schema.json");

/// Vendored JSON-Schema for the entry-reversed event.
const ENTRY_REVERSED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_entry_reversed.v1.schema.json");

/// Vendored JSON-Schema for the invariant-alarm event.
const INVARIANT_ALARM_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_invariant_alarm.v1.schema.json");

/// Vendored JSON-Schema for the dispute-recorded event.
const DISPUTE_RECORDED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_dispute_recorded.v1.schema.json");

/// Vendored JSON-Schema for the settlement-returned event.
const SETTLEMENT_RETURNED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_settlement_returned.v1.schema.json");

/// Vendored JSON-Schema for the revenue-recognized event.
const REVENUE_RECOGNIZED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_revenue_recognized.v1.schema.json");

/// Vendored JSON-Schema for the revenue-recognition-reversed event.
const REVENUE_RECOGNITION_REVERSED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_revenue_recognition_reversed.v1.schema.json");

/// Vendored JSON-Schema for the schedule-changed event.
const SCHEDULE_CHANGED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_schedule_changed.v1.schema.json");

/// Vendored JSON-Schema for the credit-note-posted event.
const CREDIT_NOTE_POSTED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_credit_note_posted.v1.schema.json");

/// Vendored JSON-Schema for the debit-note-posted event.
const DEBIT_NOTE_POSTED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_debit_note_posted.v1.schema.json");

/// Vendored JSON-Schema for the refund-recorded event.
const REFUND_RECORDED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_refund_recorded.v1.schema.json");

/// Vendored JSON-Schema for the manual-adjustment-posted event.
const MANUAL_ADJUSTMENT_POSTED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_manual_adjustment_posted.v1.schema.json");

/// Vendored JSON-Schema for the fx-revaluation-completed event.
const FX_REVALUATION_COMPLETED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_fx_revaluation_completed.v1.schema.json");

/// Vendored JSON-Schema for the fx-revaluation-reversed event.
const FX_REVALUATION_REVERSED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_fx_revaluation_reversed.v1.schema.json");

/// Vendored JSON-Schema for the period-closed event.
const PERIOD_CLOSED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_period_closed.v1.schema.json");

/// Vendored JSON-Schema for the reconciliation-completed event.
const RECONCILIATION_COMPLETED_SCHEMA_JSON: &str =
    include_str!("../../../schemas/billing_ledger_reconciliation_completed.v1.schema.json");

/// Register the ledger event-type GTS schemas with the types-registry.
///
/// Mirrors the RBAC `register_schemas` pattern: parse each vendored schema,
/// register them in one batch, and abort on the first per-item failure. Must
/// run before the event-broker producers are built (Eager validation resolves
/// the just-registered schemas).
///
/// # Errors
///
/// Returns `Err` if a vendored schema fails to parse, if the batch
/// `register` call fails catastrophically (e.g. backend unavailable), or if
/// any individual schema is rejected by the registry
/// ([`RegisterResult::Err`]).
pub async fn register_event_schemas(registry: &dyn TypesRegistryClient) -> anyhow::Result<()> {
    let entry_posted_schema: serde_json::Value = serde_json::from_str(ENTRY_POSTED_SCHEMA_JSON)
        .context(
            "bss-ledger: failed to parse vendored billing_ledger_entry_posted.v1.schema.json",
        )?;
    let entry_reversed_schema: serde_json::Value = serde_json::from_str(ENTRY_REVERSED_SCHEMA_JSON)
        .context(
            "bss-ledger: failed to parse vendored billing_ledger_entry_reversed.v1.schema.json",
        )?;
    let invariant_alarm_schema: serde_json::Value =
        serde_json::from_str(INVARIANT_ALARM_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored billing_ledger_invariant_alarm.v1.schema.json",
        )?;
    let dispute_recorded_schema: serde_json::Value =
        serde_json::from_str(DISPUTE_RECORDED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored billing_ledger_dispute_recorded.v1.schema.json",
        )?;
    let settlement_returned_schema: serde_json::Value = serde_json::from_str(
        SETTLEMENT_RETURNED_SCHEMA_JSON,
    )
    .context(
        "bss-ledger: failed to parse vendored billing_ledger_settlement_returned.v1.schema.json",
    )?;
    let revenue_recognized_schema: serde_json::Value =
        serde_json::from_str(REVENUE_RECOGNIZED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored billing_ledger_revenue_recognized.v1.schema.json",
        )?;
    let revenue_recognition_reversed_schema: serde_json::Value =
        serde_json::from_str(REVENUE_RECOGNITION_REVERSED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored \
             billing_ledger_revenue_recognition_reversed.v1.schema.json",
        )?;
    let schedule_changed_schema: serde_json::Value =
        serde_json::from_str(SCHEDULE_CHANGED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored billing_ledger_schedule_changed.v1.schema.json",
        )?;
    let credit_note_posted_schema: serde_json::Value =
        serde_json::from_str(CREDIT_NOTE_POSTED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored billing_ledger_credit_note_posted.v1.schema.json",
        )?;
    let debit_note_posted_schema: serde_json::Value =
        serde_json::from_str(DEBIT_NOTE_POSTED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored billing_ledger_debit_note_posted.v1.schema.json",
        )?;
    let refund_recorded_schema: serde_json::Value =
        serde_json::from_str(REFUND_RECORDED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored billing_ledger_refund_recorded.v1.schema.json",
        )?;
    let manual_adjustment_posted_schema: serde_json::Value =
        serde_json::from_str(MANUAL_ADJUSTMENT_POSTED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored \
             billing_ledger_manual_adjustment_posted.v1.schema.json",
        )?;
    let fx_revaluation_completed_schema: serde_json::Value =
        serde_json::from_str(FX_REVALUATION_COMPLETED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored \
             billing_ledger_fx_revaluation_completed.v1.schema.json",
        )?;
    let fx_revaluation_reversed_schema: serde_json::Value =
        serde_json::from_str(FX_REVALUATION_REVERSED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored \
             billing_ledger_fx_revaluation_reversed.v1.schema.json",
        )?;
    let period_closed_schema: serde_json::Value = serde_json::from_str(PERIOD_CLOSED_SCHEMA_JSON)
        .context(
        "bss-ledger: failed to parse vendored billing_ledger_period_closed.v1.schema.json",
    )?;
    let reconciliation_completed_schema: serde_json::Value =
        serde_json::from_str(RECONCILIATION_COMPLETED_SCHEMA_JSON).context(
            "bss-ledger: failed to parse vendored \
             billing_ledger_reconciliation_completed.v1.schema.json",
        )?;

    let results = registry
        .register(vec![
            entry_posted_schema,
            entry_reversed_schema,
            invariant_alarm_schema,
            dispute_recorded_schema,
            settlement_returned_schema,
            revenue_recognized_schema,
            revenue_recognition_reversed_schema,
            schedule_changed_schema,
            credit_note_posted_schema,
            debit_note_posted_schema,
            refund_recorded_schema,
            manual_adjustment_posted_schema,
            fx_revaluation_completed_schema,
            fx_revaluation_reversed_schema,
            period_closed_schema,
            reconciliation_completed_schema,
        ])
        .await
        .context(
            "bss-ledger: TypesRegistryClient::register(...) failed for the \
             ledger event-type schemas",
        )?;
    for result in results {
        if let RegisterResult::Err { gts_id, error } = result {
            return Err(anyhow::anyhow!(
                "bss-ledger: failed to register event-type schema {} \
                 in types-registry: {error}",
                gts_id.as_deref().unwrap_or("<unknown gts_id>")
            ));
        }
    }
    tracing::info!(
        "bss-ledger: registered event-type GTS schemas {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, \
         {}, {}, {}, {} and {}",
        ENTRY_POSTED_TYPE_ID,
        ENTRY_REVERSED_TYPE_ID,
        INVARIANT_ALARM_TYPE_ID,
        DISPUTE_RECORDED_TYPE_ID,
        SETTLEMENT_RETURNED_TYPE_ID,
        REVENUE_RECOGNIZED_TYPE_ID,
        REVENUE_RECOGNITION_REVERSED_TYPE_ID,
        SCHEDULE_CHANGED_TYPE_ID,
        CREDIT_NOTE_POSTED_TYPE_ID,
        DEBIT_NOTE_POSTED_TYPE_ID,
        REFUND_RECORDED_TYPE_ID,
        MANUAL_ADJUSTMENT_POSTED_TYPE_ID,
        FX_REVALUATION_COMPLETED_TYPE_ID,
        FX_REVALUATION_REVERSED_TYPE_ID,
        PERIOD_CLOSED_TYPE_ID,
        RECONCILIATION_COMPLETED_TYPE_ID
    );
    Ok(())
}

// Tests parked with the event broker: the JSON-Schema lockstep / `TypedEvent`
// checks return with `event-broker-sdk` (see `crate::infra::events::publisher`).
