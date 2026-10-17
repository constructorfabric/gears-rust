use crate::domain::record_intake::{DropEvent, DropReason, DropStage, IntakeEvents};

/// [`IntakeEvents`] written to the structured log, until the Audit gear ships.
/// The event carries no record content.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-drop-event:p1
#[derive(Debug, Default)]
pub struct LogIntakeEvents;

fn stage(stage: DropStage) -> &'static str {
    match stage {
        DropStage::Planner => "planner",
        DropStage::Checks => "checks",
        DropStage::Admission => "admission",
        DropStage::Writer => "writer",
    }
}

fn reason(reason: DropReason) -> &'static str {
    match reason {
        DropReason::NoPlanner => "no_planner",
        DropReason::ProcessingFailed => "processing_failed",
        DropReason::CheckFailed => "check_failed",
        DropReason::PersonalizationOff => "personalization_off",
        DropReason::ErasureInProgress => "erasure_in_progress",
        DropReason::UntrustedSource => "untrusted_source",
        DropReason::BelowConfidenceFloor => "below_confidence_floor",
        DropReason::EntityKindNotAllowed => "entity_kind_not_allowed",
        DropReason::WriteCapExceeded => "write_cap_exceeded",
    }
}

impl IntakeEvents for LogIntakeEvents {
    fn dropped(&self, event: DropEvent) {
        // @cpt-begin:cpt-cf-construct-algo-record-intake-drop-event:p1:inst-drop-event-log
        tracing::warn!(
            target: "construct::audit",
            event = "record_dropped",
            tenant_id = %event.tenant_id,
            subject_id = event.subject_id.map(tracing::field::display),
            connector = %event.connector,
            provenance = %event.provenance,
            version = %event.version,
            record_type = %event.type_id,
            stage = stage(event.stage),
            reason = reason(event.reason),
            "record dropped"
        );
        // @cpt-end:cpt-cf-construct-algo-record-intake-drop-event:p1:inst-drop-event-log
    }
}
