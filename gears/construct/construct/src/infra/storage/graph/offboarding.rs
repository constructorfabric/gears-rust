use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::models::OffboardRequest;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{DEFAULT_CALL_TIMEOUT, graph_error, unanswered};
use crate::domain::error::DomainError;
use crate::domain::tenant_exit::{GraphExit, GraphOffboarding};

/// [`GraphOffboarding`] over graph storage's tenant offboarding. Starting it
/// again answers the offboarding graph storage already has, so one call both
/// starts it and asks how far it has gone.
///
/// @cpt-dod:cpt-cf-construct-dod-retention-tenant-exit:p2
pub struct GraphTenantOffboarding {
    client: Arc<dyn GraphStorageClientV1>,
    timeout: Duration,
}

impl GraphTenantOffboarding {
    #[must_use]
    pub fn new(client: Arc<dyn GraphStorageClientV1>) -> Self {
        Self {
            client,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }
}

#[async_trait]
impl GraphOffboarding for GraphTenantOffboarding {
    async fn offboard(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<GraphExit, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-graph
        const WHAT: &str = "tenant offboarding";
        let record = tokio::time::timeout(
            self.timeout,
            self.client
                .offboard_tenant(ctx, tenant_id, OffboardRequest::default()),
        )
        .await
        .map_err(|_| unanswered(WHAT, self.timeout))?
        .map_err(|err| offboarding_error(&err))?;
        Ok(if record.is_complete() {
            GraphExit::Done
        } else {
            GraphExit::UnderWay
        })
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-graph
    }
}

/// A failed offboarding call as a domain error. Graph storage answers a
/// refused grant as not found, so that is the deployment's missing grant for
/// Construct, said as such; the rest as every graph storage call.
fn offboarding_error(err: &CanonicalError) -> DomainError {
    match err {
        CanonicalError::NotFound { .. } => DomainError::internal(format!(
            "graph storage refused Construct's offboarding of the tenant; the deployment must \
             grant Construct's identity `tenant_offboard` on `gts.cf.core.graph.tenant.v1~` \
             naming the tenant: {err}"
        )),
        _ => graph_error("tenant offboarding", err),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use graph_storage::config::GraphStorageConfig;
    use graph_storage::domain::embedding::{EmbeddingCoordinator, SpaceState};
    use graph_storage::domain::local_client::GraphStorageLocalClient;
    use graph_storage::domain::offboarding::{
        TenantDirectory, TenantState, WorkerSettings, WorkerState, run_pending,
    };
    use graph_storage::domain::service::GraphServices;
    use graph_storage::infra::embedding::fake::FakeEmbeddingProvider;
    use graph_storage::infra::fake_store::{FakeGraphEngine, FakeGraphStore};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::infra::processing_contexts::ServiceIdentity;
    use crate::test_support::AllowInOwnTenant;

    /// The platform as graph storage asks it: every tenant is `state`.
    struct Platform(TenantState);

    #[async_trait]
    impl TenantDirectory for Platform {
        async fn state(
            &self,
            _ctx: &SecurityContext,
            _tenant: Uuid,
            _within: Duration,
        ) -> Result<TenantState, graph_storage::domain::error::DomainError> {
            Ok(self.0)
        }
    }

    /// Graph storage's real service over its in-memory store, where the
    /// platform says every tenant is `state`; and the store, for its worker.
    fn graph_storage(state: TenantState) -> (Arc<dyn GraphStorageClientV1>, Arc<FakeGraphStore>) {
        graph_storage_under(state, Arc::new(AllowInOwnTenant))
    }

    /// The same, with the policy service `policy`.
    fn graph_storage_under(
        state: TenantState,
        policy: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
    ) -> (Arc<dyn GraphStorageClientV1>, Arc<FakeGraphStore>) {
        let store = Arc::new(FakeGraphStore::new());
        let services = GraphServices::new(
            GraphStorageConfig::default().validated().unwrap(),
            Arc::clone(&store) as _,
            Arc::new(FakeGraphEngine::new(Arc::clone(&store))),
            authz_resolver_sdk::PolicyEnforcer::new(policy),
            EmbeddingCoordinator::new(
                Arc::new(FakeEmbeddingProvider::new(384)),
                SpaceState::Active { epoch: 1 },
                8 * 1024,
            ),
        )
        .with_tenant_directory(Arc::new(Platform(state)));
        (
            Arc::new(GraphStorageLocalClient::new(Arc::new(services))),
            store,
        )
    }

    fn construct_in(tenant_id: Uuid) -> SecurityContext {
        ServiceIdentity::new(Uuid::new_v4())
            .context(tenant_id)
            .unwrap()
    }

    #[tokio::test]
    async fn construct_starts_the_offboarding_and_sees_it_done_once_graph_storage_removed_the_graph()
     {
        let (client, store) = graph_storage(TenantState::Deleted);
        let graph = GraphTenantOffboarding::new(client);
        let tenant = Uuid::new_v4();

        assert_eq!(
            graph.offboard(&construct_in(tenant), tenant).await.unwrap(),
            GraphExit::UnderWay,
            "fenced; graph storage's worker has not run"
        );
        assert_eq!(
            graph.offboard(&construct_in(tenant), tenant).await.unwrap(),
            GraphExit::UnderWay,
            "asking again answers the same offboarding"
        );

        let settings = WorkerSettings::new(
            NonZeroU64::new(100).unwrap(),
            Duration::from_secs(1),
            Duration::from_secs(10),
        )
        .unwrap();
        let finished = run_pending(
            store.as_ref(),
            settings,
            &mut WorkerState::default(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(finished, 1);

        assert_eq!(
            graph.offboard(&construct_in(tenant), tenant).await.unwrap(),
            GraphExit::Done
        );
    }

    #[tokio::test]
    async fn a_tenant_the_platform_has_not_deleted_is_refused_by_graph_storage() {
        let (client, _) = graph_storage(TenantState::Live);
        let tenant = Uuid::new_v4();

        let refused = GraphTenantOffboarding::new(client)
            .offboard(&construct_in(tenant), tenant)
            .await;

        assert!(
            matches!(&refused, Err(DomainError::Internal(message)) if message.contains("failed_precondition")),
            "a refusal a rerun does not cure: {refused:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_grant_names_the_permission_the_deployment_must_give() {
        let (client, _) = graph_storage_under(
            TenantState::Deleted,
            Arc::new(crate::test_support::DenyResolver),
        );
        let tenant = Uuid::new_v4();

        let refused = GraphTenantOffboarding::new(client)
            .offboard(&construct_in(tenant), tenant)
            .await;

        assert!(
            matches!(&refused, Err(DomainError::Internal(message)) if message.contains("tenant_offboard")),
            "a refusal is the deployment's missing grant, not a retry: {refused:?}"
        );
    }
}
