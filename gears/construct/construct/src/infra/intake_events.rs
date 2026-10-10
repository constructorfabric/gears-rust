use crate::domain::record_intake::{DropCause, DropEvent, IntakeEvents};

/// [`IntakeEvents`] written to the structured log, until the Audit gear ships.
/// The event carries no record content. The Planner emits it for a received
/// record it drops.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-drop-event:p1
#[derive(Debug, Default)]
pub struct LogIntakeEvents;

fn cause(cause: DropCause) -> &'static str {
    match cause {
        DropCause::ProcessingFailed => "processing_failed",
        DropCause::RoundCap => "round_cap",
        DropCause::TokenCap => "token_cap",
        DropCause::ModelFailed => "model_failed",
    }
}

impl IntakeEvents for LogIntakeEvents {
    fn dropped(&self, event: DropEvent) {
        // @cpt-begin:cpt-cf-construct-algo-record-intake-drop-event:p1:inst-drop-event-log
        tracing::warn!(
            target: "construct::audit",
            event = "record_dropped",
            tenant_id = %event.tenant_id,
            connector = %event.connector,
            record_type = %event.type_id,
            subject_id = event.subject_id.map(tracing::field::display),
            cause = cause(event.cause),
            "record dropped"
        );
        // @cpt-end:cpt-cf-construct-algo-record-intake-drop-event:p1:inst-drop-event-log
    }
}
