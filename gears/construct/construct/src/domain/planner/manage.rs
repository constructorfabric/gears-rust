use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use tokio::time::Instant;
use toolkit_macros::domain_model;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use super::agent::AgentMessage;
use super::processing::{Managed, NothingStored, Processing};
use crate::domain::error::DomainError;

pub(crate) const SUBJECT_FACTS_RESOURCE: ResourceType = ResourceType::from_static(
    "construct.subject_facts",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) mod actions {
    pub const MANAGE: &str = "manage";
}

/// The synchronous manage-facts operation: what a subject told an agent, decided and stored while the caller waits.
#[domain_model]
pub struct ManageFacts {
    processing: Processing,
    policy_enforcer: PolicyEnforcer,
    budget: Duration,
}

impl ManageFacts {
    /// `budget` bounds the steps before the write, so the answer fits the gateway's request timeout.
    #[must_use]
    pub fn new(processing: Processing, policy_enforcer: PolicyEnforcer, budget: Duration) -> Self {
        Self {
            processing,
            policy_enforcer,
            budget,
        }
    }

    /// Takes `message`, with the agent's recent `conversation`, for the subject the caller acts for. The agent in the
    /// plan's origin is the caller the platform authenticated.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the caller may not act for the subject, and the errors of
    /// [`Processing::manage`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-planner-manage-route:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), subject_id = %subject_id))]
    pub async fn manage(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        message: String,
        conversation: String,
    ) -> Result<Managed, DomainError> {
        let deadline = Instant::now() + self.budget;
        if ctx.subject_id() != subject_id {
            return Err(DomainError::forbidden(format!(
                "subject {} may not manage the facts of subject {subject_id} (tenant {})",
                ctx.subject_id(),
                ctx.subject_tenant_id()
            )));
        }
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SUBJECT_FACTS_RESOURCE,
                actions::MANAGE,
                Some(subject_id),
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;
        if message.trim().is_empty() {
            return Ok(Managed::NothingStored(NothingStored::NoChange));
        }
        let message = AgentMessage {
            tenant_id,
            subject_id,
            agent: ctx.subject_id(),
            message,
            conversation,
        };
        self.processing
            .manage(ctx, &scope, &message, deadline)
            .await
    }
}
