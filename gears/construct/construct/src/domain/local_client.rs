use std::sync::Arc;

use async_trait::async_trait;
use construct_sdk::{ConstructClientV1, RecordOutcome};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::record_intake::{IntakeOutcome, RecordIdRepository, RecordIntakeService};
use crate::domain::subject_settings::SubjectSettingsRepository;

#[domain_model]
pub struct LocalClient<I, S>
where
    I: RecordIdRepository + 'static,
    S: SubjectSettingsRepository + 'static,
{
    intake: Arc<RecordIntakeService<I, S>>,
}

impl<I, S> LocalClient<I, S>
where
    I: RecordIdRepository + 'static,
    S: SubjectSettingsRepository + 'static,
{
    #[must_use]
    pub fn new(intake: Arc<RecordIntakeService<I, S>>) -> Self {
        Self { intake }
    }
}

/// @cpt-dod:cpt-cf-construct-dod-record-intake-client:p1
#[async_trait]
impl<I, S> ConstructClientV1 for LocalClient<I, S>
where
    I: RecordIdRepository + 'static,
    S: SubjectSettingsRepository + 'static,
{
    async fn submit_record(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        record: serde_json::Value,
    ) -> Result<RecordOutcome, CanonicalError> {
        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-client
        let outcome = self
            .intake
            .submit(ctx, tenant_id, record)
            .await
            .map_err(CanonicalError::from)?;
        Ok(match outcome {
            IntakeOutcome::Received => RecordOutcome::Received,
            IntakeOutcome::Repeat => RecordOutcome::Repeat,
        })
        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-client
    }
}
