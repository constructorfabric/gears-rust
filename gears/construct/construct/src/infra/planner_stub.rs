use crate::domain::error::DomainError;
use crate::domain::record_intake::{ReceivedRecord, RecordHandOff};

/// [`RecordHandOff`] until the Planner (Story 5.2) delivers the entry point:
/// it refuses every record. Intake then keeps no identity of the record and
/// answers 503, so no record is taken and lost; a connector sends it again
/// once processing exists.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-hand-off:p1
#[derive(Debug, Default)]
pub struct NoProcessingHandOff;

impl RecordHandOff for NoProcessingHandOff {
    fn hand_off(&self, _record: ReceivedRecord) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-record-intake-hand-off:p1:inst-hand-off-refuse
        Err(DomainError::Unavailable(
            "record processing is not available yet: no planner is wired in".to_owned(),
        ))
        // @cpt-end:cpt-cf-construct-algo-record-intake-hand-off:p1:inst-hand-off-refuse
    }
}
