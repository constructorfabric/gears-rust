use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use tokio::time::Instant;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, ScopeConstraint, SecurityContext, pep_properties};
use uuid::Uuid;

use super::agent::AgentMessage;
use super::processing::{Managed, NothingStored, Processing, ProcessingContexts};
use crate::domain::error::DomainError;
use crate::domain::profile_read::not_visible;
use crate::domain::subject_settings::SubjectKey;

pub(crate) const SUBJECT_FACTS_RESOURCE: ResourceType = ResourceType::from_static(
    toolkit_gts::gts_id!("cf.construct.subject.facts.v1~"),
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) mod actions {
    pub const MANAGE: &str = "manage";
}

/// The synchronous manage-facts operation: what a subject told an agent, decided and stored while the caller waits.
#[domain_model]
pub struct ManageFacts {
    /// The chain record processing runs; none when the deployment does not configure processing.
    processing: Option<Arc<Processing>>,
    contexts: Arc<dyn ProcessingContexts>,
    policy_enforcer: PolicyEnforcer,
    budget: Duration,
}

/// Whether one constraint of a policy scope reaches the subject at `key`. It must name the subject's tenant, so a
/// grant is always bound to it, and it may narrow the grant to named subjects. A filter that cannot be decided in
/// memory, or one on another property, reaches nothing.
///
/// Managing facts is a permission of its own, which the platform grants to the applications and agents that act for
/// the tenant's subjects: a constraint that names only the tenant reaches every subject in it.
fn reaches_by(constraint: &ScopeConstraint, key: SubjectKey) -> bool {
    let mut tenant_named = false;
    for filter in constraint.filters() {
        if !filter.is_representable_in_memory() {
            return false;
        }
        let values = filter.values();
        let holds = |id: Uuid| values.iter().any(|value| value.as_uuid() == Some(id));
        match filter.property() {
            pep_properties::OWNER_TENANT_ID if holds(key.tenant_id) => tenant_named = true,
            pep_properties::RESOURCE_ID if holds(key.subject_id) => {}
            _ => return false,
        }
    }
    tenant_named
}

/// Whether a policy scope reaches the subject at `key`: one of its constraints does. The request asks with
/// constraints required, so an unconstrained scope is the platform allowing everything.
fn reaches(scope: &AccessScope, key: SubjectKey) -> bool {
    scope.is_unconstrained()
        || scope
            .constraints()
            .iter()
            .any(|constraint| reaches_by(constraint, key))
}

impl ManageFacts {
    /// `contexts` gives the context the profile is read and the facts are written as, for every caller. `budget`
    /// bounds the steps before the write, so the answer fits the gateway's request timeout.
    #[must_use]
    pub fn new(
        processing: Arc<Processing>,
        contexts: Arc<dyn ProcessingContexts>,
        policy_enforcer: PolicyEnforcer,
        budget: Duration,
    ) -> Self {
        Self {
            processing: Some(processing),
            contexts,
            policy_enforcer,
            budget,
        }
    }

    /// Manage-facts for a deployment without record processing: a caller the platform lets act for the subject is
    /// answered not configured.
    #[must_use]
    pub fn not_configured(
        contexts: Arc<dyn ProcessingContexts>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            processing: None,
            contexts,
            policy_enforcer,
            budget: Duration::ZERO,
        }
    }

    /// Takes `message`, with the agent's recent `conversation`, for `subject_id` in `tenant`, or else in the caller's
    /// own tenant. The agent in the plan's origin is the caller the platform authenticated.
    ///
    /// The subject itself needs the permission to manage its own facts. Any other caller, an application or an agent
    /// acting for the subject, needs the permission to manage facts on the subject in that tenant. For either caller the
    /// profile is then read and the facts are written as Construct, through [`ProcessingContexts`], as record
    /// processing reads and writes them, so one identity writes every profile.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the subject may not manage its own facts, and [`DomainError::NotVisible`] when
    /// another caller may not act for the subject in the tenant, both answered like a subject that does not exist.
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
        let deadline = Instant::now() + self.budget;
        let key = SubjectKey {
            tenant_id: tenant.unwrap_or_else(|| ctx.subject_tenant_id()),
            subject_id,
        };
        let message = AgentMessage {
            tenant_id: key.tenant_id,
            subject_id,
            agent: ctx.subject_id(),
            message,
            conversation,
        };
        if ctx.subject_id() == subject_id && key.tenant_id == ctx.subject_tenant_id() {
            self.policy_enforcer
                .access_scope_with(
                    ctx,
                    &SUBJECT_FACTS_RESOURCE,
                    actions::MANAGE,
                    Some(subject_id),
                    &AccessRequest::new()
                        .resource_property(pep_properties::OWNER_TENANT_ID, key.tenant_id),
                )
                .await?;
        } else {
            self.authorize_for_subject(ctx, key).await?;
        }
        let processing = self.processing()?;
        if message.message.trim().is_empty() {
            return Ok(Managed::NothingStored(NothingStored::NoChange));
        }
        let (construct, scope) = self.contexts.for_tenant(key.tenant_id).await?;
        processing
            .manage(&construct, &scope, &message, deadline)
            .await
    }

    /// The processing chain, or not configured when the deployment has none.
    fn processing(&self) -> Result<&Processing, DomainError> {
        self.processing.as_deref().ok_or_else(|| {
            DomainError::NotConfigured("record processing is not configured".to_owned())
        })
    }

    /// Whether the platform lets `ctx` manage the facts of the subject at `key`, a subject other than itself.
    async fn authorize_for_subject(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<(), DomainError> {
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
    use toolkit_security::{ScopeFilter, ScopeValue};

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
}
