use serde::Deserialize;
use uuid::Uuid;

use crate::domain::record_intake::IntakeOutcome;

/// One connector record, as JSON. Intake checks it against its GTS type, so
/// the body is taken as it is and not shaped by this DTO.
#[derive(Debug)]
#[toolkit_macros::api_dto(request)]
#[schema(value_type = Object)]
pub struct RecordRequest(pub serde_json::Value);

/// The query of the intake route: the tenant the connector sends the record
/// for. The platform must authorize the connector for it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordQuery {
    pub tenant: Uuid,
}

/// The outcome of a record intake took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum OutcomeDto {
    /// Taken for processing.
    Received,
    /// The same identity was received before; nothing changes.
    Repeat,
}

/// What intake answers for a record it takes.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct RecordOutcomeDto {
    pub outcome: OutcomeDto,
}

impl From<IntakeOutcome> for RecordOutcomeDto {
    fn from(outcome: IntakeOutcome) -> Self {
        let outcome = match outcome {
            IntakeOutcome::Received => OutcomeDto::Received,
            IntakeOutcome::Repeat => OutcomeDto::Repeat,
        };
        Self { outcome }
    }
}
