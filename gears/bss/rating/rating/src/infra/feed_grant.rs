//! The PDP check of Rating's grant on the usage-collector feed read (feature 04 §3.3
//! `inst-src-grant`; DESIGN [12 §4.6]).
//!
//! The collector skips the entries a caller may no longer read with no error, so before each read
//! and each page commit the reader asks the platform PDP the question the collector asks: Rating's
//! service identity, the v2 action `read_feed` on `gts.cf.core.uc.usage_record.v2~`, once per root
//! tenant of the source's configured scope. A root outside the compiled scope means the grant no
//! longer covers the scope, and the source pauses. A tenant carved out of a root's subtree is not
//! visible to this check; the scope digest asked of the collector closes it
//! (`…-upreq-usage-feed-scope-digest`).
//!
//! [12 §4.6]: ../../../docs/features/04-usage-intake.md#contract-12-4-6

use async_trait::async_trait;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use toolkit_security::pep_properties;

use crate::authz::service_context;
use crate::domain::error::{Cause, DomainError};
use crate::domain::usage::feed::METER_BASE_TYPE;
use crate::domain::usage::reader::FeedGrant;
use crate::domain::usage::source::SourceDefinition;

/// The collector's usage record resource the feed read is evaluated on: the base type every
/// meter derives from (usage-collector v2 `USAGE_RECORD_RESOURCE`).
pub const COLLECTOR_USAGE_RECORD: ResourceType =
    ResourceType::from_static(METER_BASE_TYPE, &[pep_properties::OWNER_TENANT_ID]);

/// The collector's feed-read action (usage-collector v2 DESIGN:2082-2091).
pub const READ_FEED: &str = "read_feed";

/// [`FeedGrant`] over the platform PDP.
#[derive(Clone)]
pub struct PdpFeedGrant {
    enforcer: PolicyEnforcer,
}

impl PdpFeedGrant {
    /// The check through `enforcer`.
    #[must_use]
    pub const fn new(enforcer: PolicyEnforcer) -> Self {
        Self { enforcer }
    }
}

#[async_trait]
impl FeedGrant for PdpFeedGrant {
    async fn covers(&self, source: &SourceDefinition) -> Result<bool, DomainError> {
        for root in source.tenant_scope().roots() {
            let ctx = service_context(*root)?;
            let request = AccessRequest::new()
                .require_constraints(true)
                .tenant_mode(TenantMode::RootOnly)
                .resource_property(pep_properties::OWNER_TENANT_ID, *root);
            let decision = self
                .enforcer
                .access_scope_with(&ctx, &COLLECTOR_USAGE_RECORD, READ_FEED, None, &request)
                .await;
            match decision {
                Ok(scope) if scope.contains_uuid(pep_properties::OWNER_TENANT_ID, *root) => {}
                Ok(_) | Err(EnforcerError::Denied { .. } | EnforcerError::CompileFailed(_)) => {
                    tracing::warn!(
                        target: "bss_rating.authz.deny",
                        tenant_id = %root,
                        "bss-rating: the feed-read grant does not cover a root of the source's scope"
                    );
                    return Ok(false);
                }
                Err(error @ EnforcerError::EvaluationFailed(_)) => {
                    return Err(DomainError::AuthzUnavailable(
                        "PDP evaluation failed".to_owned(),
                        Some(Cause::new(error)),
                    ));
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
#[path = "feed_grant_tests.rs"]
mod tests;
