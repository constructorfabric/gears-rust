use std::sync::Arc;

use async_trait::async_trait;
use construct_sdk::{
    ConstructClientV1, FactCategory, FactOrigin, ProfileCategory, ProfileFact, RecordOutcome,
    SubjectProfile,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

use crate::domain::profile::Category;
use crate::domain::profile_read::{OriginKind, ProfileReadService, ProfileView};
use crate::domain::record_intake::{IntakeOutcome, RecordIdRepository, RecordIntakeService};
use crate::domain::subject_settings::SubjectSettingsRepository;

#[domain_model]
pub struct LocalClient<I, S>
where
    I: RecordIdRepository + 'static,
    S: SubjectSettingsRepository + 'static,
{
    intake: Arc<RecordIntakeService<I, S>>,
    profile: Arc<ProfileReadService>,
}

impl<I, S> LocalClient<I, S>
where
    I: RecordIdRepository + 'static,
    S: SubjectSettingsRepository + 'static,
{
    #[must_use]
    pub fn new(intake: Arc<RecordIntakeService<I, S>>, profile: Arc<ProfileReadService>) -> Self {
        Self { intake, profile }
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

    async fn read_profile(
        &self,
        ctx: &SecurityContext,
        tenant_id: Option<Uuid>,
        subject_id: Uuid,
    ) -> Result<SubjectProfile, CanonicalError> {
        // @cpt-begin:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-client
        // The error is mapped, and so logged, inside this span, so the log
        // names the caller, the subject and the tenant, as on REST.
        let span = tracing::info_span!(
            "profile_read",
            %subject_id,
            caller = %ctx.subject_id(),
            tenant_id = %tenant_id.unwrap_or_else(|| ctx.subject_tenant_id())
        );
        let view = async {
            self.profile
                .read(ctx, tenant_id, subject_id)
                .await
                .map_err(CanonicalError::from)
        }
        .instrument(span)
        .await?;
        Ok(subject_profile(view))
        // @cpt-end:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-client
    }
}

fn fact_category(category: Category) -> FactCategory {
    match category {
        Category::Identity => FactCategory::Identity,
        Category::Role => FactCategory::Role,
        Category::Skill => FactCategory::Skill,
        Category::Preference => FactCategory::Preference,
    }
}

fn fact_origin(origin: OriginKind) -> FactOrigin {
    match origin {
        OriginKind::Connector => FactOrigin::Connector,
        OriginKind::Agent => FactOrigin::Agent,
        OriginKind::Reviewer => FactOrigin::Reviewer,
        OriginKind::Administrator => FactOrigin::Administrator,
    }
}

/// The SDK form of a profile the read served.
fn subject_profile(view: ProfileView) -> SubjectProfile {
    SubjectProfile::new(
        view.subject_id,
        view.categories
            .into_iter()
            .map(|group| {
                ProfileCategory::new(
                    fact_category(group.category),
                    group
                        .facts
                        .into_iter()
                        .map(|fact| {
                            ProfileFact::new(
                                fact.node_key.0,
                                fact.property,
                                fact.value,
                                fact_origin(fact.origin),
                                fact.stored_at,
                                fact.produced_at,
                            )
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}
