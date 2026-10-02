use std::sync::Arc;

use async_trait::async_trait;
use github_mirror_sdk::{GithubMirrorClientV1, MirrorStatus, Repo, SyncSummary};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::service::Service;

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
        let service = Arc::clone(&self.service);
        let ctx = ctx.clone();
        let (owner, name) = (owner.to_owned(), name.to_owned());
        tokio::spawn(async move { service.sync_now(&ctx, &owner, &name).await })
            .await
            .map_err(|e| DomainError::internal(format!("the sync task did not finish: {e}")))?
            .map_err(CanonicalError::from)
    }
}
