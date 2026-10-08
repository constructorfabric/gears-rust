//! E1b end to end (P-D-233): a cloudlet derived usage type sells through pricing's real gates, and pricing hears its
//! meter semantics from Products alone.
//!
//! Both gears run in one process, as a deployment links them. Products boots through its own `Gear::init`, which
//! registers its reference registry and its meter-semantics dispatcher in the `ClientHub`; pricing's authoring state
//! and router are built on that same hub, as `sku_governance_tests`' cross-gear test builds them. No test meter
//! provider exists anywhere in this file: every meter answer pricing receives is Products' (decision 6).
//!
//! The walk: create the type through Products' door; publish a usage SKU on `products.derived/cloudlets@1` with its
//! invoice fields; create a pricing usage entry whose policy names that meter, the type's output unit and its
//! `derived-v1:<digest>` accrual; author, submit and apply a price; publish a plan revision; resolve it through
//! `PricingReadProvider` and run `SellabilityV1::check` on the `NewSaleQuery` built from the answer. Then the probes:
//! a wrong unit and a wrong accrual are refused `METER_POLICY_MISMATCH`, and a failing derived store is 503 at
//! pricing's entry create.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request};
use bss_pricing::api::pricing_read::PricingReadProvider;
use bss_pricing::api::rest::authoring::AuthoringState;
use bss_pricing::api::sellability::SellabilityProvider;
use bss_pricing::config::SellerHoldPolicy;
use bss_pricing::infra::clock::WallClock;
use bss_pricing::infra::commercial_terms::CommercialTermsService;
use bss_pricing_sdk::acceptance::{
    CommandMeta, Market, NewSaleQuery, SellabilityV1, TenantAxes, Term,
};
use bss_pricing_sdk::digest::{billing_terms_digest, selected_bindings_digest};
use bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1;
use bss_pricing_sdk::read::{CatalogRef, PricingReadV1, ResolveQuery};
use bss_pricing_sdk::terms::{BillingAnchor, BillingCycle, BillingTerms, TermsSource, Timezone};
use bss_products::gear::BssProductsGear;
use bss_products_sdk::usage_types::{
    UsageTypeAnswer, UsageTypeBinding, UsageTypeCatalog, UsageTypePage,
};
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

const CLOUDLET_UNIT: &str = "cloudlet\u{b7}hour";
const METER: &str = "products.derived/cloudlets@1";
const RAM_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1";
const CPU_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.cpu_mhz.v1";

/// A permitting PDP whose compiled scope is the one tenant: the flat-`In` shape a tenant's grant compiles to. Both
/// gears' doors and ports authorize through it.
struct FlatIn {
    tenant: Uuid,
}

/// `sku:read` compiles to one constraint, `owner_tenant_id = T AND resource_id IN {sku}`. `sku:author` carries that
/// same constraint and a tenant disjunct: a fresh SKU id is not in `{sku}`, and the insert checks the new id against
/// `resource_id`, so the AND constraint alone denies the create. The disjunct is what lets the pin run. Every other
/// products grant is the tenant alone.
struct SkuScoped {
    tenant: Uuid,
    sku: Uuid,
}
#[async_trait]
impl AuthZResolverApi for SkuScoped {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant = Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            vec![self.tenant],
        ));
        let sku_row = req
            .resource
            .resource_type
            .contains("cf.bss.products.sku.v1");
        let mut constraints = vec![Constraint {
            predicates: vec![tenant.clone()],
        }];
        if sku_row && (req.action.name == "read" || req.action.name == "author") {
            constraints[0]
                .predicates
                .push(Predicate::In(InPredicate::new(
                    pep_properties::RESOURCE_ID,
                    vec![self.sku],
                )));
        }
        if sku_row && req.action.name == "author" {
            constraints.push(Constraint {
                predicates: vec![tenant],
            });
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                deny_reason: None,
            },
        })
    }
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

/// The usage-type catalog a deployment registers: it knows the cloudlet's two raw inputs and nothing else.
struct RawInputs;
#[async_trait]
impl UsageTypeCatalog for RawInputs {
    async fn resolve(&self, _ctx: &SecurityContext, usage_type_ref: &str) -> UsageTypeAnswer {
        if [RAM_REF, CPU_REF].contains(&usage_type_ref) {
            UsageTypeAnswer::Resolved(UsageTypeBinding {
                gts_id: usage_type_ref.to_owned(),
                kind: "gauge".to_owned(),
                metadata_fields: Vec::new(),
            })
        } else {
            UsageTypeAnswer::Unresolved
        }
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

/// The `TypesRegistryClient` Products' init registers its authz labels with; nothing else is asked of it.
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

/// A file database in its own temporary directory, migrated with `migrations`; its DSN.
async fn database(
    dir: &tempfile::TempDir,
    migrations: Vec<Box<dyn sea_orm_migration::MigrationTrait>>,
) -> (toolkit_db::Db, String) {
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
    (db, dsn)
}

fn user(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type("gts.cf.core.security.subject_user.v1~")
        .token_scopes(vec!["*".to_owned()])
        .build()
        .unwrap()
}

/// Both gears over one `ClientHub`.
struct Deployment {
    tenant: Uuid,
    author: SecurityContext,
    reviewer: SecurityContext,
    products: Router,
    pricing: Router,
    state: Arc<AuthoringState>,
    enforcer: Arc<PolicyEnforcer>,
    hub: Arc<toolkit::ClientHub>,
    products_dsn: String,
    /// Holds Products' runtime (its outbox) for the test's life.
    _gear: BssProductsGear,
    /// The two databases' temporary directories, removed with the deployment.
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Deployment {
    async fn new() -> Self {
        let tenant = Uuid::new_v4();
        Self::boot(tenant, Arc::new(FlatIn { tenant })).await
    }

    /// Products authorizes through `products_authz`. Pricing's doors keep a tenant grant.
    async fn boot(tenant: Uuid, products_authz: Arc<dyn AuthZResolverApi>) -> Self {
        let gear = BssProductsGear::default();
        let products_dir = tempfile::Builder::new()
            .prefix("products-e2e-")
            .tempdir()
            .unwrap();
        let (products_db, products_dsn) = database(&products_dir, gear.migrations()).await;
        let hub = Arc::new(toolkit::ClientHub::new());
        hub.register::<dyn AuthZResolverApi>(products_authz);
        hub.register::<dyn TypesRegistryClient>(Arc::new(Accepting));
        hub.register::<dyn UsageTypeCatalog>(Arc::new(RawInputs));
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
            .prefix("pricing-e2e-")
            .tempdir()
            .unwrap();
        let (pricing_db, _) = database(
            &pricing_dir,
            bss_pricing::module::BssPricingGear::default().migrations(),
        )
        .await;
        let state = Arc::new(
            AuthoringState::new(toolkit_db::DBProvider::new(pricing_db), hub.clone())
                .await
                .unwrap(),
        );
        let enforcer = Arc::new(PolicyEnforcer::new(Arc::new(FlatIn { tenant })));
        let pricing =
            bss_pricing::api::rest::authoring::router(state.clone(), &OpenApiRegistryImpl::new())
                .layer(axum::Extension((*enforcer).clone()));
        Self {
            tenant,
            author: user(tenant),
            reviewer: user(tenant),
            products,
            pricing,
            state,
            enforcer,
            hub,
            products_dsn,
            _gear: gear,
            _dirs: (products_dir, pricing_dir),
        }
    }

    /// One request through `app` as `ctx`, with a fresh `Idempotency-Key`: its status and body.
    async fn send(
        app: &Router,
        ctx: &SecurityContext,
        method: Method,
        uri: &str,
        body: Option<Value>,
        if_match: Option<&str>,
    ) -> (u16, Value, Option<String>) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .extension(ctx.clone())
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

    async fn products(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, body, _) = Self::send(
            &self.products,
            &self.author,
            method,
            &format!("/bss-products/v1{path}"),
            body,
            None,
        )
        .await;
        (status, body)
    }

    async fn pricing_as(
        &self,
        ctx: &SecurityContext,
        method: Method,
        path: &str,
        body: Value,
    ) -> (u16, Value) {
        let (status, body, _) = Self::send(
            &self.pricing,
            ctx,
            method,
            &format!("/bss-pricing/v1{path}"),
            Some(body),
            None,
        )
        .await;
        (status, body)
    }

    async fn pricing(&self, method: Method, path: &str, body: Value) -> (u16, Value) {
        self.pricing_as(&self.author, method, path, body).await
    }

    /// Products' approval policy at quorum 0, written at the tag its read answered: a submit is the publish.
    async fn products_quorum_zero(&self) {
        let (status, _, tag) = Self::send(
            &self.products,
            &self.author,
            Method::GET,
            "/bss-products/v1/approval-policy",
            None,
            None,
        )
        .await;
        assert_eq!(status, 200);
        let (status, body, _) = Self::send(
            &self.products,
            &self.author,
            Method::PUT,
            "/bss-products/v1/approval-policy",
            Some(json!({"quorum": 0})),
            tag.as_deref(),
        )
        .await;
        assert_eq!(status, 200, "{body}");
    }

    /// The reviewer's approve of the unit a submit answered: the apply.
    async fn approve(&self, submitted: &Value) {
        let unit = submitted["unit"]["id"].as_str().unwrap();
        let (status, body) = self
            .pricing_as(
                &self.reviewer,
                Method::POST,
                &format!("/approval-units/{unit}/approve"),
                json!({"generation": 1}),
            )
            .await;
        assert_eq!(status, 200, "{body}");
    }

    /// A pricing usage entry of `book` for `sku` whose policy names `meter`, `unit` and `accrual`.
    async fn entry(
        &self,
        book: &str,
        sku: &str,
        meter: &Value,
        unit: &str,
        accrual: &str,
    ) -> (u16, Value) {
        self.pricing(
            Method::POST,
            &format!("/price-books/{book}/entries"),
            json!({
                "sku_id": sku,
                "model": "per_unit",
                "usage_rating_policy": {
                    "rating_window": {"kind": "billing_cycle"},
                    "aggregation_scope": "subscription_line",
                    "reset": "rating_window_start",
                    "quantity_semantics": {
                        "meter": meter,
                        "unit": unit,
                        "fold": "SUM",
                        "accrual_policy_version": accrual
                    },
                    "partial_window": "actual_quantity_full_thresholds"
                }
            }),
        )
        .await
    }

    async fn book(&self, code: &str) -> String {
        let (status, book) = self
            .pricing(
                Method::POST,
                "/price-books",
                json!({"code": code, "name": code, "currency": "EUR"}),
            )
            .await;
        assert_eq!(status, 201, "{book}");
        book["id"].as_str().unwrap().to_owned()
    }
}

/// The cloudlet of decision 1 on the wire: the larger of the RAM and the CPU share, per hour.
fn cloudlet() -> Value {
    let share = |name: &str, divisor: &str| json!({"op":"ceil","arg":{"op":"div_const","arg":{"op":"input","name":name},"divisor":divisor}});
    json!({
        "output_unit": CLOUDLET_UNIT,
        "granularity": "hour",
        "inputs": [
            {"name":"ram_mb","usage_type_ref":RAM_REF,"granule_fold":"peak","unit":"MB"},
            {"name":"cpu_mhz","usage_type_ref":CPU_REF,"granule_fold":"peak","unit":"MHz"}
        ],
        "formula": {"op":"max","args":[share("ram_mb","128"), share("cpu_mhz","400")]},
        "output_scale": 0,
        "output_round": "half_even"
    })
}

impl Deployment {
    /// A price of `entry` from `today`, authored, submitted and applied by the reviewer's approve.
    async fn price(&self, entry: &str, today: time::Date) {
        let (status, draft) = self
            .pricing(
                Method::POST,
                &format!("/price-book-entries/{entry}/prices"),
                json!({"price": {"rate": "1"}, "eligibility": "all", "effective_from": today.to_string()}),
            )
            .await;
        assert_eq!(status, 201, "{draft}");
        let price = draft["items"][0]["id"].as_str().unwrap().to_owned();
        let (status, submitted) = self
            .pricing(Method::POST, &format!("/prices/{price}/submit"), json!({}))
            .await;
        assert_eq!(status, 201, "{submitted}");
        self.approve(&submitted).await;
    }

    /// A plan of `book` whose revision 1 holds the SKU on `entry`, submitted and published by the reviewer's approve:
    /// the plan's id and the revision's.
    async fn publish_plan(&self, book: &str, sku: &str, entry: &str) -> (Uuid, Uuid) {
        let (status, plan) = self
            .pricing(
                Method::POST,
                "/plans",
                json!({"code": "CLOUD", "name": "Cloud", "book_id": book}),
            )
            .await;
        assert_eq!(status, 201, "{plan}");
        let plan_id: Uuid = plan["id"].as_str().unwrap().parse().unwrap();
        let revision: Uuid = plan["revisions"][0]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let (status, item) = self
            .pricing(
                Method::POST,
                &format!("/plan-revisions/{revision}/items"),
                json!({"sku_id": sku, "price_book_entry_id": entry}),
            )
            .await;
        assert_eq!(status, 201, "{item}");
        let (status, submitted) = self
            .pricing(
                Method::POST,
                &format!("/plan-revisions/{revision}/submit"),
                json!({}),
            )
            .await;
        assert_eq!(status, 201, "{submitted}");
        self.approve(&submitted).await;
        (plan_id, revision)
    }
}

/// A new sale of every resolved cell, starting `today` in the seller `tenant`, as an order line would ask it: its
/// selections and `resolved_bindings_digest` come from the resolve answer (`selected_bindings_digest`).
fn sale_query(
    tenant: Uuid,
    resolved: &bss_pricing_sdk::read::ResolvedBindings,
    today: time::Date,
) -> NewSaleQuery {
    let selections: Vec<_> = resolved.cells.iter().map(|c| c.selection.clone()).collect();
    let start_at = today.midnight().assume_utc();
    let mut billing_terms = BillingTerms {
        schema_version: 1,
        cycle: BillingCycle::Month,
        anchor: BillingAnchor::Calendar,
        // A calendar anchor is the first day of a month at 00:00 UTC (pricing's commercial terms
        // refuse any other as UNALIGNED_BILLING_ANCHOR), so the sale starting today anchors on
        // this month's first day. `today` itself passed only on the 1st.
        anchor_at: today
            .replace_day(1)
            .expect("the first day of a month")
            .midnight()
            .assume_utc(),
        timezone: Timezone::Utc,
        source: TermsSource::ExplicitOrder,
        digest: [0; 32],
    };
    billing_terms.digest = billing_terms_digest(&billing_terms);
    NewSaleQuery {
        tenant_axes: TenantAxes {
            seller_tenant_id: tenant,
            payer_tenant_id: Uuid::new_v4(),
            resource_tenant_id: Uuid::new_v4(),
        },
        order_id: Uuid::new_v4(),
        order_version: 1,
        line_id: Uuid::new_v4(),
        plan_id: resolved.plan_id,
        plan_revision_id: resolved.revision_id,
        resolved_bindings_digest: selected_bindings_digest(resolved, &selections).unwrap(),
        selections,
        quantity: rust_decimal::Decimal::ONE,
        market: Market {
            currency: "EUR".into(),
            region: None,
        },
        start_at,
        term: Term::Rolling,
        billing_terms,
        hold_policy_version: 1,
    }
}

#[tokio::test]
async fn a_cloudlet_sells_through_pricing_on_products_meter_semantics() {
    let s = Deployment::new().await;
    let today = time::OffsetDateTime::now_utc().date();

    // 1. The cloudlet derived type, through Products' door.
    let (status, version) = s
        .products(
            Method::POST,
            "/derived-usage-types",
            Some(json!({"code": "cloudlets", "name": "Cloudlets", "declaration": cloudlet()})),
        )
        .await;
    assert_eq!(status, 201, "{version}");
    let meter = version["meter_ref"].clone();
    assert_eq!(meter, json!({"usage_type_id": METER, "version": "1"}));
    assert_eq!(version["canonical_unit"], CLOUDLET_UNIT);
    let digest = version["digest"].as_str().unwrap().to_owned();
    let accrual = version["accrual_policy_version"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(accrual, format!("derived-v1:{digest}"));

    // 2. A usage SKU on it, with its invoice fields, published.
    s.products_quorum_zero().await;
    let (status, sku) = s
        .products(
            Method::POST,
            "/skus",
            Some(json!({
                "code": "CLOUDLET", "name": "Cloudlet", "type": "usage",
                "usage_type_ref": METER, "unit": CLOUDLET_UNIT,
                "gl_code": "usage", "tax_category": "standard",
                "invoice_line_template": "{sku}", "billing_timing": "arrears"
            })),
        )
        .await;
    assert_eq!(status, 201, "{sku}");
    let sku = sku["id"].as_str().unwrap().to_owned();
    let (status, published) = s
        .products(
            Method::POST,
            &format!("/skus/{sku}/submit"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, 200, "{published}");
    assert_eq!(published["applied"], true, "{published}");

    // 6a. The probes at entry create: a unit or an accrual other than the type's is a policy mismatch.
    let book = s.book("standard").await;
    let (status, wrong_unit) = s.entry(&book, &sku, &meter, "VM\u{b7}hour", &accrual).await;
    assert_eq!(status, 400, "{wrong_unit}");
    assert!(
        wrong_unit.to_string().contains("METER_POLICY_MISMATCH"),
        "{wrong_unit}"
    );
    let (status, wrong_accrual) = s
        .entry(
            &book,
            &sku,
            &meter,
            CLOUDLET_UNIT,
            &format!("derived-v1:{}", "0".repeat(64)),
        )
        .await;
    assert_eq!(status, 400, "{wrong_accrual}");
    assert!(
        wrong_accrual.to_string().contains("METER_POLICY_MISMATCH"),
        "{wrong_accrual}"
    );

    // 3. The usage entry on the derived meter.
    let (status, entry) = s.entry(&book, &sku, &meter, CLOUDLET_UNIT, &accrual).await;
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["reference_state"], "confirmed", "{entry}");
    let entry = entry["id"].as_str().unwrap().to_owned();

    // 4. A price authored, submitted and applied; a plan revision published.
    s.price(&entry, today).await;
    let (plan_id, revision) = s.publish_plan(&book, &sku, &entry).await;

    // 5. The sale: resolved through the read provider, checked through sellability.
    let read = PricingReadProvider::new(s.state.clone(), s.enforcer.clone());
    let resolved = read
        .resolve(
            &s.author,
            ResolveQuery {
                catalog: CatalogRef {
                    tenant_id: s.tenant,
                },
                revision_id: revision,
                date: today,
                item_id: None,
                pins: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(resolved.plan_id, plan_id);
    let query = sale_query(s.tenant, &resolved, today);
    let sellability = SellabilityProvider::new(Arc::new(CommercialTermsService::new(
        s.state.clone(),
        s.enforcer.clone(),
        Arc::new(WallClock),
        SellerHoldPolicy::default(),
    )));
    let receipt = sellability
        .check(
            &s.author,
            query.clone(),
            CommandMeta {
                idempotency_key: "accept-cloudlet".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt.query, query);
    // D-514: the policy holds the rating rules; the meter and the unit are the SKU revision's.
    let binding = &receipt.bindings[0];
    let policy = binding.usage_rating_policy.as_ref().unwrap();
    assert_eq!(policy.content.fold, bss_pricing_sdk::terms::Fold::Sum);
    assert_eq!(
        binding.meter.as_ref().map(|m| m.usage_type_id.as_str()),
        Some(METER)
    );
    assert_eq!(binding.unit.as_deref(), Some(CLOUDLET_UNIT));

    // 6b. The derived store failing at entry create is 503 at pricing.
    let probe_book = s.book("probe").await;
    let conn = sea_orm::Database::connect(&s.products_dsn).await.unwrap();
    sea_orm::ConnectionTrait::execute_unprepared(
        &conn,
        "DROP TABLE products_derived_usage_type_version;",
    )
    .await
    .unwrap();
    conn.close().await.unwrap();
    let (status, failed) = s
        .entry(&probe_book, &sku, &meter, CLOUDLET_UNIT, &accrual)
        .await;
    assert_eq!(status, 503, "{failed}");
    assert!(
        failed.to_string().contains("REGISTRY_UNAVAILABLE"),
        "the served unit is read with the SKU, so the dropped version table fails the registry: {failed}"
    );
    let (status, card) = s.products(Method::GET, &format!("/skus/{sku}"), None).await;
    assert_eq!(
        status, 500,
        "a dropped version table fails the SKU read (P-D-259): {card}"
    );

    // Every meter answer above was Products': the hub's one provider is its dispatcher.
    assert!(s.hub.get::<dyn UsageMeterSemanticsV1>().is_ok());
}

/// A caller whose `sku:read` and `sku:author` are `owner_tenant_id = T AND resource_id IN {S}` still pins, reads and
/// prices the tenant's derived meter. `S` is a SKU id, never the derived type's id.
#[tokio::test]
async fn a_sku_scoped_read_still_answers_the_derived_meter_it_pins() {
    let tenant = Uuid::new_v4();
    let sku_grant = Uuid::new_v4();
    let s = Deployment::boot(
        tenant,
        Arc::new(SkuScoped {
            tenant,
            sku: sku_grant,
        }),
    )
    .await;

    let (status, version) = s
        .products(
            Method::POST,
            "/derived-usage-types",
            Some(json!({"code": "cloudlets", "name": "Cloudlets", "declaration": cloudlet()})),
        )
        .await;
    assert_eq!(status, 201, "{version}");

    s.products_quorum_zero().await;
    let (status, sku) = s
        .products(
            Method::POST,
            "/skus",
            Some(json!({
                "code": "CLOUDLET", "name": "Cloudlet", "type": "usage",
                "usage_type_ref": METER, "unit": CLOUDLET_UNIT,
                "gl_code": "usage", "tax_category": "standard",
                "invoice_line_template": "{sku}", "billing_timing": "arrears"
            })),
        )
        .await;
    assert_eq!(status, 201, "{sku}");
    let sku_id = sku["id"].as_str().unwrap();
    assert_ne!(
        sku_id,
        sku_grant.to_string(),
        "the grant's SKU is not this row"
    );
    let (status, published) = s
        .products(
            Method::POST,
            &format!("/skus/{sku_id}/submit"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, 200, "{published}");
    assert_eq!(published["applied"], true, "{published}");

    let provider = s.hub.get::<dyn UsageMeterSemanticsV1>().unwrap();
    let answered = provider
        .resolve(
            &s.author,
            bss_pricing_sdk::terms::MeterRef {
                usage_type_id: METER.to_owned(),
                version: "1".to_owned(),
            },
        )
        .await
        .unwrap_or_else(|error| {
            let problem =
                serde_json::to_value(toolkit::api::canonical_prelude::Problem::from(error))
                    .unwrap();
            panic!("the meter exists and is not METER_VERSION_UNKNOWN: {problem}");
        });
    assert_eq!(answered.meter.usage_type_id, METER);
    assert_eq!(answered.canonical_unit, CLOUDLET_UNIT);

    let (status, body) = s
        .products(Method::GET, "/derived-usage-types/cloudlets", None)
        .await;
    assert_eq!(status, 200, "{body}");
}

/// A stored declaration that does not read stays 500 at entry create, with a fixed detail. A dropped version table
/// stays 503.
#[tokio::test]
async fn a_corrupt_derived_row_is_500_at_entry_create_and_a_dropped_table_is_503() {
    let s = Deployment::new().await;
    let (status, version) = s
        .products(
            Method::POST,
            "/derived-usage-types",
            Some(json!({"code": "cloudlets", "name": "Cloudlets", "declaration": cloudlet()})),
        )
        .await;
    assert_eq!(status, 201, "{version}");
    let meter = version["meter_ref"].clone();
    let accrual = version["accrual_policy_version"]
        .as_str()
        .unwrap()
        .to_owned();
    s.products_quorum_zero().await;
    let (status, sku) = s
        .products(
            Method::POST,
            "/skus",
            Some(json!({
                "code": "CLOUDLET", "name": "Cloudlet", "type": "usage",
                "usage_type_ref": METER, "unit": CLOUDLET_UNIT,
                "gl_code": "usage", "tax_category": "standard",
                "invoice_line_template": "{sku}", "billing_timing": "arrears"
            })),
        )
        .await;
    assert_eq!(status, 201, "{sku}");
    let sku = sku["id"].as_str().unwrap().to_owned();
    let (status, published) = s
        .products(
            Method::POST,
            &format!("/skus/{sku}/submit"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, 200, "{published}");
    let book = s.book("standard").await;

    let conn = sea_orm::Database::connect(&s.products_dsn).await.unwrap();
    sea_orm::ConnectionTrait::execute_unprepared(
        &conn,
        "DROP TRIGGER products_derived_usage_type_version_no_update;",
    )
    .await
    .unwrap();
    sea_orm::ConnectionTrait::execute_unprepared(
        &conn,
        "UPDATE products_derived_usage_type_version SET declaration_json = '{}';",
    )
    .await
    .unwrap();
    conn.close().await.unwrap();

    let (status, corrupt) = s.entry(&book, &sku, &meter, CLOUDLET_UNIT, &accrual).await;
    assert_eq!(status, 500, "{corrupt}");
    let text = corrupt.to_string();
    assert!(
        text.contains("a stored derived meter row does not read"),
        "{text}"
    );
    assert!(!text.contains("missing field"), "{text}");
    assert!(!text.contains("output_unit"), "{text}");
}
