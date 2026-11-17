use std::sync::Mutex;

use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo,
};

use super::*;

/// A resolver that answers `get_tenant` with what it was given, after
/// `takes`, and keeps the tenant of each context that asked.
struct Resolver {
    answer: Mutex<Option<Result<TenantInfo, TenantResolverError>>>,
    takes: Duration,
    asked_as: Mutex<Vec<Uuid>>,
}

impl Resolver {
    fn answering(answer: Result<TenantInfo, TenantResolverError>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Some(answer)),
            takes: Duration::ZERO,
            asked_as: Mutex::default(),
        })
    }
}

#[async_trait]
impl TenantResolverClient for Resolver {
    async fn get_tenant(
        &self,
        ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        self.asked_as.lock().unwrap().push(ctx.subject_tenant_id());
        tokio::time::sleep(self.takes).await;
        self.answer.lock().unwrap().take().expect("asked once")
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        unreachable!("tenant exit asks for one tenant")
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        unreachable!("tenant exit asks for one tenant")
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        unreachable!("tenant exit asks for one tenant")
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        unreachable!("tenant exit asks for one tenant")
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        unreachable!("tenant exit asks for one tenant")
    }
}

/// What the resolver answers to `get_tenant`.
type Answer = Result<TenantInfo, TenantResolverError>;

fn info(tenant_id: Uuid, status: tenant_resolver_sdk::TenantStatus) -> TenantInfo {
    TenantInfo {
        id: TenantId(tenant_id),
        name: "a tenant".to_owned(),
        status,
        tenant_type: None,
        parent_id: None,
        self_managed: false,
    }
}

fn construct_in(tenant_id: Uuid) -> SecurityContext {
    crate::infra::processing_contexts::ServiceIdentity::new(Uuid::new_v4())
        .context(tenant_id)
        .unwrap()
}

#[tokio::test]
async fn each_answer_of_the_resolver_is_a_status_or_an_error() {
    use tenant_resolver_sdk::TenantStatus as Platform;
    let tenant = Uuid::new_v4();
    let cases: Vec<(Answer, Result<TenantStatus, &str>)> = vec![
        (
            Ok(info(tenant, Platform::Deleted)),
            Ok(TenantStatus::Deleted),
        ),
        (
            Ok(info(tenant, Platform::Active)),
            Ok(TenantStatus::Present),
        ),
        (
            Ok(info(tenant, Platform::Suspended)),
            Ok(TenantStatus::Present),
        ),
        (
            Err(TenantResolverError::TenantNotFound {
                tenant_id: TenantId(tenant),
            }),
            Ok(TenantStatus::Unknown),
        ),
        (
            Err(TenantResolverError::NoPluginAvailable),
            Err("unavailable"),
        ),
        (
            Err(TenantResolverError::ServiceUnavailable("down".to_owned())),
            Err("unavailable"),
        ),
        (Err(TenantResolverError::Unauthorized), Err("internal")),
        (
            Err(TenantResolverError::Internal("broken".to_owned())),
            Err("internal"),
        ),
    ];
    for (answer, expected) in cases {
        let shown = format!("{answer:?}");
        let resolver = Resolver::answering(answer);
        let statuses = HubTenantStatuses::over(Arc::clone(&resolver) as _, RESOLVER_TIMEOUT);

        let status = statuses.status(&construct_in(tenant), tenant).await;

        match (status, expected) {
            (Ok(status), Ok(expected)) => assert_eq!(status, expected, "{shown}"),
            (Err(DomainError::Unavailable(_)), Err("unavailable"))
            | (Err(DomainError::Internal(_)), Err("internal")) => {}
            (status, expected) => panic!("{shown}: got {status:?}, expected {expected:?}"),
        }
        let asked_as = resolver.asked_as.lock().unwrap().clone();
        assert_eq!(asked_as, [tenant], "asked as Construct in the tenant");
    }
}

#[tracing_test::traced_test]
#[tokio::test]
async fn without_a_resolver_in_the_hub_no_tenant_is_checked_and_that_is_said_once() {
    let statuses = HubTenantStatuses::new(Arc::new(ClientHub::new()));

    for _ in 0..3 {
        let tenant = Uuid::new_v4();
        assert!(matches!(
            statuses.status(&construct_in(tenant), tenant).await,
            Err(DomainError::NotConfigured(_))
        ));
    }

    logs_assert(|lines: &[&str]| {
        match lines
            .iter()
            .filter(|line| line.contains("no tenant resolver is registered"))
            .count()
        {
            1 => Ok(()),
            n => Err(format!("said {n} times")),
        }
    });
}

#[tokio::test(start_paused = true)]
async fn a_resolver_that_does_not_answer_in_time_is_unavailable() {
    let tenant = Uuid::new_v4();
    let resolver = Arc::new(Resolver {
        answer: Mutex::new(Some(Ok(info(
            tenant,
            tenant_resolver_sdk::TenantStatus::Deleted,
        )))),
        takes: Duration::from_secs(60),
        asked_as: Mutex::default(),
    });
    let statuses = HubTenantStatuses::over(resolver, Duration::from_secs(1));

    assert!(matches!(
        statuses.status(&construct_in(tenant), tenant).await,
        Err(DomainError::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_resolver_registered_after_the_first_question_is_found() {
    let hub = Arc::new(ClientHub::new());
    let statuses = HubTenantStatuses::new(Arc::clone(&hub));
    let tenant = Uuid::new_v4();
    assert!(matches!(
        statuses.status(&construct_in(tenant), tenant).await,
        Err(DomainError::NotConfigured(_))
    ));

    hub.register::<dyn TenantResolverClient>(Resolver::answering(Ok(info(
        tenant,
        tenant_resolver_sdk::TenantStatus::Deleted,
    ))));

    assert_eq!(
        statuses
            .status(&construct_in(tenant), tenant)
            .await
            .unwrap(),
        TenantStatus::Deleted
    );
}
