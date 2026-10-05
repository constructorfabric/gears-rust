//! Ask 58 end to end (pricing D-522, products P-D-263): archiving a finished price book releases
//! its entries' SKU references in Products, so the SKU no longer counts as referenced, retires,
//! and is then archived.
//!
//! Both gears run in one process, as a deployment links them: Products boots through its own
//! `Gear::init`, which registers its reference registry in the `ClientHub`; pricing's authoring
//! state and router are built on that same hub (the harness of `derived_meter_e2e.rs`). Every
//! reference answer pricing receives is Products' own.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request};
use bss_pricing::api::rest::authoring::AuthoringState;
use bss_products::gear::BssProductsGear;
use bss_products_sdk::usage_types::{UsageTypeAnswer, UsageTypeCatalog, UsageTypePage};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::api::OpenApiRegistryImpl;
use toolkit::contracts::{DatabaseCapability, RestApiCapability};
use toolkit::{Gear, GearCtx};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use tower::ServiceExt;
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

/// A permitting PDP whose compiled scope is the one tenant. Both gears authorize through it.
struct FlatIn {
    tenant: Uuid,
}
#[async_trait]
impl AuthZResolverApi for FlatIn {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        vec![self.tenant],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// No usage type is known: the flow sells a recurring SKU.
struct NoUsageTypes;
#[async_trait]
impl UsageTypeCatalog for NoUsageTypes {
    async fn resolve(&self, _ctx: &SecurityContext, _usage_type_ref: &str) -> UsageTypeAnswer {
        UsageTypeAnswer::Unresolved
    }
    async fn list(
        &self,
        _ctx: &SecurityContext,
        _q: Option<&str>,
        _kind: Option<&str>,
        _limit: u32,
        _cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        Ok(UsageTypePage::default())
    }
}

/// The `TypesRegistryClient` Products' init registers its authz labels with.
struct Accepting;
#[async_trait]
impl TypesRegistryClient for Accepting {
    async fn register(&self, _entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        Ok(Vec::new())
    }
    async fn register_type_schemas(
        &self,
        _type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_type_schema(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_type_schema_by_uuid(
        &self,
        _type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_type_schemas(
        &self,
        _type_ids: Vec<String>,
    ) -> std::collections::HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        std::collections::HashMap::new()
    }
    async fn get_type_schemas_by_uuid(
        &self,
        _type_uuids: Vec<Uuid>,
    ) -> std::collections::HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        std::collections::HashMap::new()
    }
    async fn list_type_schemas(
        &self,
        _query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn register_instances(
        &self,
        _instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_instance(&self, _id: &str) -> Result<GtsInstance, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_instance_by_uuid(&self, _uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
    async fn get_instances(
        &self,
        _ids: Vec<String>,
    ) -> std::collections::HashMap<String, Result<GtsInstance, CanonicalError>> {
        std::collections::HashMap::new()
    }
    async fn get_instances_by_uuid(
        &self,
        _uuids: Vec<Uuid>,
    ) -> std::collections::HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        std::collections::HashMap::new()
    }
    async fn list_instances(
        &self,
        _query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        Err(CanonicalError::internal("unexpected registry call").create())
    }
}

struct NoConfig;
impl toolkit::config::ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
        None
    }
}

/// A file database in its own temporary directory, migrated with `migrations`.
async fn database(
    dir: &tempfile::TempDir,
    migrations: Vec<Box<dyn sea_orm_migration::MigrationTrait>>,
) -> toolkit_db::Db {
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("db.sqlite3").display()
    );
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&db, migrations)
        .await
        .unwrap();
    db
}

/// Both gears over one `ClientHub`.
struct Deployment {
    author: SecurityContext,
    products: Router,
    pricing: Router,
    /// Holds pricing's outbox for the test's life.
    _state: Arc<AuthoringState>,
    /// Holds Products' runtime (its outbox) for the test's life.
    _gear: BssProductsGear,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Deployment {
    async fn new() -> Self {
        let tenant = Uuid::new_v4();
        let gear = BssProductsGear::default();
        let products_dir = tempfile::Builder::new()
            .prefix("products-archive-e2e-")
            .tempdir()
            .unwrap();
        let products_db = database(&products_dir, gear.migrations()).await;
        let hub = Arc::new(toolkit::ClientHub::new());
        hub.register::<dyn AuthZResolverApi>(Arc::new(FlatIn { tenant }));
        hub.register::<dyn TypesRegistryClient>(Arc::new(Accepting));
        hub.register::<dyn UsageTypeCatalog>(Arc::new(NoUsageTypes));
        let ctx = GearCtx::new(
            "bss-products",
            Uuid::new_v4(),
            Arc::new(NoConfig),
            hub.clone(),
            tokio_util::sync::CancellationToken::new(),
        )
        .with_db(toolkit_db::DBProvider::new(products_db));
        gear.init(&ctx).await.unwrap();
        let products = gear
            .register_rest(&ctx, Router::new(), &OpenApiRegistryImpl::new())
            .unwrap();
        let pricing_dir = tempfile::Builder::new()
            .prefix("pricing-archive-e2e-")
            .tempdir()
            .unwrap();
        let pricing_db = database(
            &pricing_dir,
            bss_pricing::module::BssPricingGear::default().migrations(),
        )
        .await;
        let state = Arc::new(
            AuthoringState::new(toolkit_db::DBProvider::new(pricing_db), hub.clone())
                .await
                .unwrap(),
        );
        let enforcer = authz_resolver_sdk::PolicyEnforcer::new(Arc::new(FlatIn { tenant }));
        let pricing =
            bss_pricing::api::rest::authoring::router(state.clone(), &OpenApiRegistryImpl::new())
                .layer(axum::Extension(enforcer));
        let author = SecurityContext::builder()
            .subject_id(Uuid::now_v7())
            .subject_tenant_id(tenant)
            .subject_type("gts.cf.core.security.subject_user.v1~")
            .token_scopes(vec!["*".to_owned()])
            .build()
            .unwrap();
        Self {
            author,
            products,
            pricing,
            _state: state,
            _gear: gear,
            _dirs: (products_dir, pricing_dir),
        }
    }

    /// One request through `app`, with a fresh `Idempotency-Key`: its status, body and `ETag`.
    async fn send(
        &self,
        app: &Router,
        method: Method,
        uri: &str,
        body: Option<Value>,
        if_match: Option<&str>,
    ) -> (u16, Value, Option<String>) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .extension(self.author.clone())
            .header("content-type", "application/json")
            .header("idempotency-key", Uuid::new_v4().to_string());
        if let Some(tag) = if_match {
            request = request.header("if-match", tag);
        }
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let response = app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let tag = response
            .headers()
            .get("etag")
            .map(|t| t.to_str().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            tag,
        )
    }

    async fn products(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        if_match: Option<&str>,
    ) -> (u16, Value, Option<String>) {
        self.send(
            &self.products,
            method,
            &format!("/bss-products/v1{path}"),
            body,
            if_match,
        )
        .await
    }

    async fn pricing(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        if_match: Option<&str>,
    ) -> (u16, Value, Option<String>) {
        self.send(
            &self.pricing,
            method,
            &format!("/bss-pricing/v1{path}"),
            body,
            if_match,
        )
        .await
    }

    /// Quorum 0 in `gear`'s approval policy, written at the tag its read answered.
    async fn quorum_zero(&self, products: bool) {
        let app = if products {
            &self.products
        } else {
            &self.pricing
        };
        let path = if products {
            "/bss-products/v1/approval-policy"
        } else {
            "/bss-pricing/v1/approval-policy"
        };
        let (status, _, tag) = self.send(app, Method::GET, path, None, None).await;
        assert_eq!(status, 200);
        let (status, body, _) = self
            .send(
                app,
                Method::PUT,
                path,
                Some(json!({"quorum": 0})),
                tag.as_deref(),
            )
            .await;
        assert_eq!(status, 200, "{body}");
    }
}

#[tokio::test]
async fn an_archived_book_releases_its_sku_which_then_retires_and_archives() {
    let d = Deployment::new().await;
    d.quorum_zero(true).await;
    d.quorum_zero(false).await;
    let today = time::OffsetDateTime::now_utc().date();

    // Products: a recurring SKU, published at its submit.
    let (status, sku, _) = d
        .products(
            Method::POST,
            "/skus",
            Some(json!({"code": "SEAT", "name": "Seat", "type": "recurring"})),
            None,
        )
        .await;
    assert_eq!(status, 201, "{sku}");
    let sku = sku["id"].as_str().unwrap().to_owned();
    let (status, published, _) = d
        .products(
            Method::POST,
            &format!("/skus/{sku}/submit"),
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(status, 200, "{published}");
    assert_eq!(published["applied"], true, "{published}");

    // Pricing: a book, an entry of the SKU and an approved price.
    let (status, book, _) = d
        .pricing(
            Method::POST,
            "/price-books",
            Some(json!({"code": "seats", "name": "Seats", "currency": "EUR"})),
            None,
        )
        .await;
    assert_eq!(status, 201, "{book}");
    let book = book["id"].as_str().unwrap().to_owned();
    let (status, entry, _) = d
        .pricing(
            Method::POST,
            &format!("/price-books/{book}/entries"),
            Some(json!({"sku_id": sku, "period": "month", "model": "per_unit"})),
            None,
        )
        .await;
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["reference_state"], "confirmed", "{entry}");
    let entry = entry["id"].as_str().unwrap().to_owned();
    let (status, draft, _) = d
        .pricing(
            Method::POST,
            &format!("/price-book-entries/{entry}/prices"),
            Some(
                json!({"price": {"rate": "1"}, "eligibility": "all", "effective_from": today.to_string()}),
            ),
            None,
        )
        .await;
    assert_eq!(status, 201, "{draft}");
    let price = draft["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, submitted, _) = d
        .pricing(
            Method::POST,
            &format!("/prices/{price}/submit"),
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(status, 201, "{submitted}");
    assert_eq!(submitted["unit"]["state"], "approved", "{submitted}");

    // While the entry's reference lives, Products refuses the retire.
    let (status, refused, _) = d
        .products(
            Method::POST,
            &format!("/skus/{sku}/retire"),
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(status, 409, "{refused}");
    assert!(refused.to_string().contains("SKU_REFERENCED"), "{refused}");

    // Pricing archives the finished book: the entry's reference is released in Products.
    let (status, archived, _) = d
        .pricing(
            Method::POST,
            &format!("/price-books/{book}/archive"),
            Some(json!({})),
            Some("\"1\""),
        )
        .await;
    assert_eq!(status, 200, "{archived}");
    let (_, read, _) = d
        .pricing(
            Method::GET,
            &format!("/price-book-entries/{entry}"),
            None,
            None,
        )
        .await;
    assert_eq!(read["reference_state"], "released", "{read}");
    let (status, references, _) = d
        .products(Method::GET, &format!("/skus/{sku}/references"), None, None)
        .await;
    assert_eq!(status, 200, "{references}");
    assert_eq!(
        references["items"],
        json!([]),
        "no live reference: {references}"
    );

    // Products now retires the SKU, and archives it.
    let (status, retired, _) = d
        .products(
            Method::POST,
            &format!("/skus/{sku}/retire"),
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(status, 200, "{retired}");
    assert_eq!(retired["applied"], true, "{retired}");
    let (status, card, tag) = d
        .products(Method::GET, &format!("/skus/{sku}"), None, None)
        .await;
    assert_eq!(status, 200, "{card}");
    assert_eq!(card["sku"]["lifecycle"], "retired", "{card}");
    let (status, archived_sku, _) = d
        .products(
            Method::POST,
            &format!("/skus/{sku}/archive"),
            None,
            tag.as_deref(),
        )
        .await;
    assert_eq!(status, 200, "{archived_sku}");
    assert!(archived_sku["archived_at"].is_string(), "{archived_sku}");
    let (status, list, _) = d.products(Method::GET, "/skus", None, None).await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["items"], json!([]), "the archived SKU leaves the list");
}
