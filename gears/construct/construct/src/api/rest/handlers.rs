use std::sync::Arc;

use axum::extract::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::{Json, Query};
use toolkit_security::SecurityContext;
use tracing::Instrument as _;

use crate::api::rest::error::for_rest;
use crate::api::rest::types::ConcreteIntake;
use crate::domain::record_intake::IntakeOutcome;

use super::dto::{RecordOutcomeDto, RecordQuery, RecordRequest};

/// The answer's status: 202 for a received record, which is processed later,
/// and 200 for a repeat, which changes nothing.
fn status_of(outcome: IntakeOutcome) -> StatusCode {
    match outcome {
        IntakeOutcome::Received => StatusCode::ACCEPTED,
        IntakeOutcome::Repeat => StatusCode::OK,
    }
}

/// @cpt-dod:cpt-cf-construct-dod-record-intake-route:p1
pub async fn submit_record(
    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteIntake>>,
    Query(query): Query<RecordQuery>,
    Json(RecordRequest(record)): Json<RecordRequest>,
    // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-parse
) -> ApiResult<impl IntoResponse> {
    // The error is mapped, and so logged, inside this span, so the log names
    // the connector and the tenant it sent the record for.
    let span = tracing::info_span!(
        "record_intake",
        tenant_id = %query.tenant,
        connector = %ctx.subject_id()
    );
    let outcome = async {
        svc.submit(&ctx, query.tenant, record)
            .await
            .map_err(for_rest)
    }
    .instrument(span)
    .await?;
    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-answer
    Ok((status_of(outcome), Json(RecordOutcomeDto::from(outcome))))
    // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-answer
}
