#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, PolicyEnforcer,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use github_mirror::domain::service::{Service, ServiceConfig};
use github_mirror::infra::storage::migrations::Migrator;
use github_mirror::infra::storage::sea_orm_repo::{SeaOrmIssueRepository, SeaOrmRepoRepository};
use toolkit::{ClientHub, ConfigProvider, GearCtx};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

pub type ConcreteService = Service<SeaOrmRepoRepository, SeaOrmIssueRepository>;

/// PDP fake: allows everything, constrained to the caller's tenant.
pub struct MockAuthZResolver;

#[async_trait]
impl AuthZResolverClient for MockAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let root_id = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tc| tc.root_id)
            .or_else(|| {
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .ok_or_else(|| AuthZResolverError::Internal("tenant context is required".to_owned()))?;

        let predicates = vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            [root_id],
        ))];

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                ..Default::default()
            },
        })
    }
}

pub async fn inmem_db() -> Db {
    use sea_orm_migration::MigratorTrait;

    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .unwrap_or_else(|e| panic!("in-memory database must connect: {e}"));

    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .unwrap_or_else(|e| panic!("migrations must apply: {e}"));

    db
}

pub fn enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(MockAuthZResolver);
    PolicyEnforcer::new(authz)
}

pub fn service_over(db: Db, api_base_url: &str) -> Arc<ConcreteService> {
    Arc::new(Service::new(
        Arc::new(DBProvider::new(db)),
        Arc::new(SeaOrmRepoRepository::new()),
        Arc::new(SeaOrmIssueRepository::new()),
        enforcer(),
        ServiceConfig {
            api_base_url: api_base_url.to_owned(),
        },
    ))
}

pub async fn service(api_base_url: &str) -> Arc<ConcreteService> {
    service_over(inmem_db().await, api_base_url)
}

pub struct StaticConfig {
    pub section: Option<serde_json::Value>,
}

impl ConfigProvider for StaticConfig {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        if gear_name == "github-mirror" {
            self.section.as_ref()
        } else {
            None
        }
    }
}

/// A `GearCtx` good enough for `Gear::init`: config + hub with a fake PDP + an
/// in-memory database with migrations applied.
pub async fn gear_ctx(hub: Arc<ClientHub>, section: Option<serde_json::Value>) -> GearCtx {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(MockAuthZResolver);
    hub.register::<dyn AuthZResolverClient>(authz);

    GearCtx::new(
        "github-mirror",
        Uuid::new_v4(),
        Arc::new(StaticConfig { section }),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(DBProvider::new(inmem_db().await))
}

pub fn caller_in(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap_or_else(|e| panic!("test caller context must build: {e}"))
}

pub fn caller() -> SecurityContext {
    caller_in(Uuid::new_v4())
}
