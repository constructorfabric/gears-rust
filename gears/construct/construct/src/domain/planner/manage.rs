use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use tokio::time::{Instant, timeout_at};
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::agent::AgentMessage;
use super::processing::{ManageDeadlines, Managed, NothingStored, Processing, ProcessingContexts};
use crate::domain::error::DomainError;
use crate::domain::profile_read::not_visible;
use crate::domain::subject_reach::reach;
use crate::domain::subject_settings::SubjectKey;

pub(crate) const SUBJECT_FACTS_RESOURCE: ResourceType = ResourceType::from_static(
    toolkit_gts::gts_id!("cf.construct.subject.fact.v1~"),
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) mod actions {
    pub const MANAGE: &str = "manage";
}

/// How long a manage-facts call may take from its start: the steps before the write, and the whole call, the write
/// included.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManageBudget {
    /// The profile read, the agent loop, the checks and Admission.
    pub before_write: Duration,
    /// The whole call, the write included.
    pub answer: Duration,
}

impl ManageBudget {
    /// The deadlines of a call that started at `start`.
    #[must_use]
    pub fn deadlines_from(self, start: Instant) -> ManageDeadlines {
        ManageDeadlines {
            before_write: start + self.before_write,
            answer: start + self.answer,
        }
    }
}

/// The synchronous manage-facts operation: what a subject told an agent, decided and stored while the caller waits.
#[domain_model]
pub struct ManageFacts {
    /// The chain record processing runs; none when the deployment does not configure processing.
    processing: Option<Arc<Processing>>,
    contexts: Arc<dyn ProcessingContexts>,
    policy_enforcer: PolicyEnforcer,
    /// How long a call may take.
    pub(crate) budget: ManageBudget,
}

/// Whether a policy scope reaches the subject at `key`: one of its constraints [`reach`]es it with no other filter.
///
/// Managing facts is a permission of its own, which the platform grants to the applications and agents that act for
/// the tenant's subjects: a constraint that names only the tenant reaches every subject in it.
fn reaches(scope: &AccessScope, key: SubjectKey) -> bool {
    scope
        .constraints()
        .iter()
        .any(|constraint| reach(constraint, key).is_some_and(|rest| rest.is_empty()))
}

impl ManageFacts {
    /// `contexts` gives the context the profile is read and the facts are written as, for every caller. `budget`
    /// bounds the call, so the answer fits the gateway's request timeout.
    #[must_use]
    pub fn new(
        processing: Arc<Processing>,
        contexts: Arc<dyn ProcessingContexts>,
        policy_enforcer: PolicyEnforcer,
        budget: ManageBudget,
    ) -> Self {
        Self {
            processing: Some(processing),
            contexts,
            policy_enforcer,
            budget,
        }
    }

    /// Manage-facts for a deployment without record processing: a caller the platform lets act for the subject is
    /// answered not configured. `budget` bounds the policy call as it bounds the steps before the write.
    #[must_use]
    pub fn not_configured(
        contexts: Arc<dyn ProcessingContexts>,
        policy_enforcer: PolicyEnforcer,
        budget: ManageBudget,
    ) -> Self {
        Self {
            processing: None,
            contexts,
            policy_enforcer,
            budget,
        }
    }

    /// The tenant a call is about: the one named, or else the caller's own.
    #[must_use]
    pub fn tenant_of(ctx: &SecurityContext, tenant: Option<Uuid>) -> Uuid {
        tenant.unwrap_or_else(|| ctx.subject_tenant_id())
    }

    /// Takes `message`, with the agent's recent `conversation`, for `subject_id` in the tenant of
    /// [`Self::tenant_of`]. The agent in the plan's origin is the caller the platform authenticated.
    ///
    /// Every caller, the subject itself or an application or agent acting for it, needs the permission to manage
    /// facts on the subject in that tenant. The profile is then read and the facts are written as Construct, through
    /// [`ProcessingContexts`], as record processing reads and writes them, so one identity writes every profile.
    ///
    /// # Errors
    ///
    /// [`DomainError::NotVisible`] when the caller may not act for the subject in the tenant, answered like a subject
    /// that does not exist. [`DomainError::Unavailable`] when the policy service does not answer within the budget.
    /// Not configured when the deployment does not configure record processing. The errors of
    /// [`ProcessingContexts::for_tenant`] and [`Processing::manage`].
    ///
    /// @cpt-dod:cpt-cf-construct-dod-planner-manage-route:p1
    #[tracing::instrument(skip_all, fields(caller = %ctx.subject_id(), subject_id = %subject_id))]
    pub async fn manage(
        &self,
        ctx: &SecurityContext,
        tenant: Option<Uuid>,
        subject_id: Uuid,
        message: String,
        conversation: String,
    ) -> Result<Managed, DomainError> {
        let deadlines = self.budget.deadlines_from(Instant::now());
        let key = SubjectKey {
            tenant_id: Self::tenant_of(ctx, tenant),
            subject_id,
        };
        let message = AgentMessage {
            tenant_id: key.tenant_id,
            subject_id,
            agent: ctx.subject_id(),
            message,
            conversation,
        };
        timeout_at(deadlines.before_write, self.authorize(ctx, key))
            .await
            .map_err(|_| {
                DomainError::Unavailable(
                    "the policy service did not answer within the budget".to_owned(),
                )
            })??;
        let processing = self.processing()?;
        if message.message.trim().is_empty() {
            return Ok(Managed::NothingStored(NothingStored::NoChange));
        }
        let (construct, scope) = self.contexts.for_tenant(key.tenant_id).await?;
        processing
            .manage(&construct, &scope, &message, deadlines)
            .await
    }

    /// The processing chain, or not configured when the deployment has none.
    fn processing(&self) -> Result<&Processing, DomainError> {
        self.processing.as_deref().ok_or_else(|| {
            DomainError::NotConfigured("record processing is not configured".to_owned())
        })
    }

    /// Whether the platform lets `ctx` manage the facts of the subject at `key`.
    async fn authorize(&self, ctx: &SecurityContext, key: SubjectKey) -> Result<(), DomainError> {
        // Constraints are required, so an answer without them is a refusal, never a grant on every subject.
        let request = AccessRequest::new()
            .require_constraints(true)
            .context_tenant_id(key.tenant_id)
            .tenant_mode(TenantMode::RootOnly)
            .resource_property(pep_properties::OWNER_TENANT_ID, key.tenant_id);
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SUBJECT_FACTS_RESOURCE,
                actions::MANAGE,
                Some(key.subject_id),
                &request,
            )
            .await
            .map_err(not_visible)?;
        if reaches(&scope, key) {
            Ok(())
        } else {
            Err(DomainError::NotVisible(
                "the policy scope does not reach the subject".to_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use toolkit_security::{ScopeConstraint, ScopeFilter, ScopeValue};

    use super::*;

    fn key() -> SubjectKey {
        SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn a_tenant_constraint_reaches_every_subject_in_the_tenant_and_no_other_tenant() {
        let key = key();
        let tenant = AccessScope::for_tenant(key.tenant_id);

        assert!(reaches(&tenant, key));
        assert!(reaches(
            &tenant,
            SubjectKey {
                subject_id: Uuid::new_v4(),
                ..key
            }
        ));
        assert!(!reaches(
            &tenant,
            SubjectKey {
                tenant_id: Uuid::new_v4(),
                ..key
            }
        ));
    }

    #[test]
    fn a_resource_id_filter_narrows_the_grant_to_the_subjects_it_names() {
        let key = key();
        let scope = AccessScope::single(ScopeConstraint::new(vec![
            ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, key.tenant_id),
            ScopeFilter::eq(pep_properties::RESOURCE_ID, key.subject_id),
        ]));

        assert!(reaches(&scope, key));
        assert!(!reaches(
            &scope,
            SubjectKey {
                subject_id: Uuid::new_v4(),
                ..key
            }
        ));
    }

    #[test]
    fn a_constraint_without_the_tenant_or_with_another_property_reaches_nothing() {
        let key = key();
        for constraint in [
            ScopeConstraint::new(vec![ScopeFilter::eq(
                pep_properties::RESOURCE_ID,
                key.subject_id,
            )]),
            ScopeConstraint::new(vec![
                ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, key.tenant_id),
                ScopeFilter::r#in("category", vec![ScopeValue::from("skill")]),
            ]),
        ] {
            assert!(
                !reaches(&AccessScope::single(constraint.clone()), key),
                "{constraint:?}"
            );
        }
    }

    #[test]
    fn an_unconstrained_scope_reaches_nothing() {
        assert!(!reaches(&AccessScope::allow_all(), key()));
    }
}
