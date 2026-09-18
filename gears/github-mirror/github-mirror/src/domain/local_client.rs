use std::sync::Arc;

use async_trait::async_trait;
use github_mirror_sdk::{GithubMirrorClientV1, MirrorStatus, Repo, SyncSummary};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext};

use crate::domain::ports::github::FetchOptions;
use crate::domain::service::{Service, SyncProgress};

#[domain_model]
pub struct LocalClient {
    service: Arc<Service>,
}

impl LocalClient {
    #[must_use]
    pub fn new(service: Arc<Service>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl GithubMirrorClientV1 for LocalClient {
    async fn status(&self, _ctx: &SecurityContext) -> Result<MirrorStatus, CanonicalError> {
        Ok(self.service.status())
    }

    async fn list_repos(
        &self,
        ctx: &SecurityContext,
        query: ODataQuery,
    ) -> Result<Page<Repo>, CanonicalError> {
        self.service
            .list_repos(ctx, &query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn sync_repository(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
    ) -> Result<SyncSummary, CanonicalError> {
        let cancel = self.service.shutdown_token();
        self.service
            .sync_repository(
                ctx,
                owner,
                name,
                &FetchOptions {
                    tenant_id: ctx.subject_tenant_id(),
                    access_scope: AccessScope::default(),
                    scope: self.service.default_scope(),
                    force: false,
                    since: None,
                    cancel: cancel.clone(),
                },
                &SyncProgress::new(),
                &cancel,
            )
            .await
            .map_err(CanonicalError::from)
    }
}
