use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::record_intake::{
    DropEvent, DropReason, DropStage, IntakeEvents, ReceivedRecord, RecordHandOff,
};

/// [`RecordHandOff`] until the Planner (Story 5.2) delivers the entry point:
/// it takes every record and drops it, with a content-free drop event. Intake
/// answers received and keeps the record's identity, so a resend after the
/// Planner ships is a repeat: records received while this stub is wired in are
/// not processed.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-hand-off:p1
pub struct DroppingHandOff {
    events: Arc<dyn IntakeEvents>,
}

impl DroppingHandOff {
    #[must_use]
    pub fn new(events: Arc<dyn IntakeEvents>) -> Self {
        Self { events }
    }
}

impl RecordHandOff for DroppingHandOff {
    fn hand_off(&self, record: ReceivedRecord) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-record-intake-hand-off:p1:inst-hand-off-drop
        self.events.dropped(DropEvent::for_record(
            &record,
            DropStage::Planner,
            DropReason::NoPlanner,
        ));
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-record-intake-hand-off:p1:inst-hand-off-drop
    }
}
