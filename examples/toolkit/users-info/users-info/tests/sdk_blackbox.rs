#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use authz_resolver_sdk::{
    AuthZResolverApi,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, DatabaseCapability, Gear, GearCtx};
use toolkit_db::migration_runner::run_migrations_for_gear;
use toolkit_db::{ConnectOpts, DBProvider, Db, DbError, connect_db};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use users_info::UsersInfo;
use users_info_sdk::{NewUser, UsersInfoClientV1};

/// Mock `AuthZ` resolver for tests (`allow_all` mode).
///
/// Tenant resolution: `context.tenant_context.root_id` if present, otherwise
/// `subject.properties.tenant_id` (like a real PDP).
struct MockAuthZResolver;

#[async_trait::async_trait]
impl AuthZResolverApi for MockAuthZResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        // Resolve tenant: explicit context > subject property (like a real PDP)
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
            });

        let constraints = if request.context.require_constraints {
            match root_id {
                Some(id) => vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [id],
                    ))],
                }],
                None => vec![],
            }
        } else {
            vec![]
        };

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                ..Default::default()
            },
        })
    }
}

struct MockConfigProvider {
    gears: HashMap<String, serde_json::Value>,
}

impl MockConfigProvider {
    fn new_users_info_default() -> Self {
        let mut gears = HashMap::new();
        // GearCtx::raw_config expects: gears.<name> = { database: ..., config: ... }
        // For this test we supply config only; DB handle is injected directly.
        gears.insert(
            "users_info".to_owned(),
            json!({
                "config": {
                    "default_page_size": 50,
                    "max_page_size": 1000,
                    "audit_base_url": "http://audit.local",
                    "notifications_base_url": "http://notifications.local",
                }
            }),
        );
        Self { gears }
    }
}

impl ConfigProvider for MockConfigProvider {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        self.gears.get(gear_name)
    }
}

#[tokio::test]
async fn users_info_registers_sdk_client_and_handles_basic_crud() {
    // Arrange: build a real Db for sqlite in-memory, run gear migrations, then init gear.
    let db: Db = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("db connect");
    let dbp: DBProvider<DbError> = DBProvider::new(db.clone());

    let hub = Arc::new(ClientHub::new());

    // Register mock AuthZ resolver before initializing the gear
    hub.register::<dyn AuthZResolverApi>(Arc::new(MockAuthZResolver));

    let ctx = GearCtx::new(
        "users_info",
        Uuid::new_v4(),
        Arc::new(MockConfigProvider::new_users_info_default()),
        hub.clone(),
        CancellationToken::new(),
    )
    .with_db(dbp);

    let gear = UsersInfo::default();
    run_migrations_for_gear(&db, "users_info", gear.migrations())
        .await
        .expect("migrate");
    gear.init(&ctx).await.expect("init");

    // Act: resolve SDK client from hub and do basic CRUD.
    let client = ctx
        .client_hub()
        .get::<dyn UsersInfoClientV1>()
        .expect("UsersInfoClientV1 must be registered");

    // Create a security context with tenant access
    let tenant_id = Uuid::new_v4();
    let sec = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap();

    let created = client
        .create_user(
            sec.clone(),
            NewUser {
                id: None,
                tenant_id,
                email: "test@example.com".to_owned(),
                display_name: "Test".to_owned(),
            },
        )
        .await
        .unwrap();

    let fetched = client.get_user(sec.clone(), created.id).await.unwrap();
    assert_eq!(fetched.email, "test@example.com");

    client.delete_user(sec, created.id).await.unwrap();
}
