//! Erasure: the subject's request to erase everything Construct holds about
//! it. The request marks the erasure; the leader's deletion job runs it in
//! the background, and runs it again until it is done.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::deletion::{DeletionEvent, DeletionEvents, DeletionStore};
use super::error::DomainError;
use super::profile_read::settings_scope;
use super::subject_access::own;
use super::subject_settings::{SettingsWrite, SubjectSettings, out_of_scope};

pub(crate) mod actions {
    /// The subject erases everything Construct holds about it.
    pub const ERASE: &str = "erase";
}

/// The subject's erasure request, owner only.
#[domain_model]
pub struct ErasureService {
    store: Arc<dyn DeletionStore>,
    events: Arc<dyn DeletionEvents>,
    policy_enforcer: PolicyEnforcer,
}

impl ErasureService {
    #[must_use]
    pub fn new(
        store: Arc<dyn DeletionStore>,
        events: Arc<dyn DeletionEvents>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            store,
            events,
            policy_enforcer,
        }
    }

    /// Mark the caller's erasure under way and set its personalization off;
    /// the leader's deletion job runs it. A repeat request keeps the mark.
    ///
    /// cancel-safe: the mark is one write.
    ///
    /// # Errors
    ///
    /// Forbidden when the caller is not the subject; out of scope when the
    /// scope does not reach its settings; otherwise the store's errors.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-erasure-request:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn request(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
    ) -> Result<SubjectSettings, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-erasure-request:p1:inst-request-own
        let (key, scope) = own(&self.policy_enforcer, ctx, subject_id, actions::ERASE).await?;
        // @cpt-end:cpt-cf-construct-flow-erasure-request:p1:inst-request-own
        // @cpt-begin:cpt-cf-construct-flow-erasure-request:p1:inst-request-mark
        match self
            .store
            .mark(&settings_scope(&scope), key, OffsetDateTime::now_utc())
            .await?
        {
            SettingsWrite::Written => {}
            SettingsWrite::OutOfScope(refusal) => {
                return Err(out_of_scope(ctx, key, refusal));
            }
        }
        // @cpt-end:cpt-cf-construct-flow-erasure-request:p1:inst-request-mark
        // @cpt-begin:cpt-cf-construct-flow-erasure-request:p1:inst-request-hand-off
        self.events.record(DeletionEvent::ErasureRequested {
            tenant_id: key.tenant_id,
            subject_id,
        });
        Ok(SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: true,
        })
        // @cpt-end:cpt-cf-construct-flow-erasure-request:p1:inst-request-hand-off
    }
}
