//! The approvals inbox end to end over BOTH real gears in one process (P-D-250, pricing D-490):
//! the facade gear `bss-approvals` booted from its config, with pricing's and products' sources in
//! its hub, each over its own gear's state and database. The cross-gear precedent is
//! `sku_governance_tests`' real pricing entry. The exact order of a walk is proved on Postgres
//! (`tests/postgres_approvals_inbox.rs`); here every instant is a whole second, which `SQLite`'s
//! text column orders as time.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::ProductsApprovalSource;
use super::tests::{Census, census_on, send, stored_in};
use crate::test_support::*;
use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::{Extension, Router};
use bss_approval::{Store, Unit, UnitState};
use bss_approvals_sdk::{ApprovalSourceV1, Order, SourceNarrowing, SourcePageQuery};
use bss_pricing::api::rest::authoring::{AuthoringState, inbox_source::PricingApprovalSource};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::client_hub::ClientScope;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_db::secure::AccessScope;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

/// One PDP for both gears: every subject of the tenant holds every grant (a flat `In` over the
/// owner tenant), except a subject listed without products, without pricing, or without
/// pricing's `price_book_entry` read.
#[derive(Default)]
struct Grants {
    tenant: Mutex<Uuid>,
    no_products: Mutex<BTreeSet<Uuid>>,
    no_pricing: Mutex<BTreeSet<Uuid>>,
    no_entry_read: Mutex<BTreeSet<Uuid>>,
}
#[async_trait]
impl AuthZResolverApi for Grants {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let who = request.subject.id;
        let kind = request.resource.resource_type.as_str();
        let refused = (kind.contains("bss.products.")
            && self.no_products.lock().unwrap().contains(&who))
            || (kind.contains("bss.pricing.") && self.no_pricing.lock().unwrap().contains(&who))
            || (kind.contains("bss.pricing.price_book_entry.")
                && self.no_entry_read.lock().unwrap().contains(&who));
        Ok(EvaluationResponse {
            decision: !refused,
            context: EvaluationResponseContext {
                constraints: if refused {
                    Vec::new()
                } else {
                    vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            vec![*self.tenant.lock().unwrap()],
                        ))],
                    }]
                },
                deny_reason: None,
            },
        })
    }
}

struct Config(Value);
impl toolkit::config::ConfigProvider for Config {
    fn get_gear_config(&self, name: &str) -> Option<&Value> {
        (name == "bss-approvals").then_some(&self.0)
    }
}

/// Both gears, their served doors and the facade.
struct Inbox {
    products: Census,
    pricing_state: Arc<AuthoringState>,
    /// Held for the test's life: pricing's database in its own temporary directory.
    _pricing_dsn: TestDsn,
    pricing: Router,
    grants: Arc<Grants>,
    facade: Router,
    pricing_source: Arc<PricingApprovalSource>,
    products_source: Arc<ProductsApprovalSource>,
    /// A published usage SKU, and pricing's book and entry over it.
    sku: Uuid,
    book: Uuid,
    entry: Uuid,
}

const FACADE: &str = "/bss-approvals/v1/approval-units";
/// More pages than any walk here takes: a walk that repeats fails instead of looping.
const WALK_BOUND: usize = 100;

async fn inbox() -> Inbox {
    let (db, _, _, dsn) = test_db().await;
    inbox_on(db, dsn, None).await
}

/// A fresh file database carrying pricing's whole chain, and the DSN that holds its directory.
async fn fresh_pricing_db() -> (toolkit_db::DBProvider<toolkit_db::DbError>, TestDsn) {
    use toolkit::contracts::DatabaseCapability;
    let dsn = TestDsn::new("pricing-inbox-");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        bss_pricing::module::BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    (toolkit_db::DBProvider::new(db), dsn)
}

/// A legacy raw usage SKU, published through the repository: the doors no longer accept a raw ref,
/// and the pricing fixture's meter provider answers `storage` (P-D-259). The SKU's id.
async fn published_sku(products: &Census) -> Uuid {
    use crate::domain::sku::NewSku;
    use bss_products_sdk::models::{Lifecycle, SkuType};
    let (db, scope) = repo_connection(&products.dsn, products.tenant).await;
    let conn = db.conn().unwrap();
    let now = time::OffsetDateTime::now_utc();
    let sku = crate::infra::storage::repo::insert_sku(
        &conn,
        &scope,
        products.tenant,
        NewSku {
            code: "SKU".into(),
            name: "SKU".into(),
            r#type: SkuType::Usage,
            category_id: None,
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: Some("storage".into()),
            unit: Some("GB".into()),
        },
        products.author.subject_id(),
        now,
    )
    .await
    .unwrap();
    crate::infra::storage::repo::set_lifecycle(
        &conn,
        &scope,
        products.tenant,
        sku.id,
        &[Lifecycle::Draft],
        Lifecycle::Published,
        now,
    )
    .await
    .unwrap();
    // A real publish writes version 1; pricing records it on the entry (D-514: `usage_sku_version` >= 1).
    let raw = sea_orm::Database::connect(&products.dsn).await.unwrap();
    sea_orm::ConnectionTrait::execute_unprepared(
        &raw,
        &format!(
            "UPDATE products_sku SET published_version = 1 WHERE id = x'{}'",
            sku.id.simple()
        ),
    )
    .await
    .unwrap();
    products.policy(1).await;
    sku.id
}

/// One request to pricing's served door as `who`: the status and the JSON body.
async fn pricing_call(
    pricing: &Router,
    who: &SecurityContext,
    method: &str,
    path: &str,
    body: &Value,
    key: Option<&str>,
) -> (u16, Value) {
    let (status, body, _) = send(
        pricing,
        who,
        method,
        &format!("/bss-pricing/v1{path}"),
        &body.to_string(),
        key,
    )
    .await;
    (
        status,
        serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
    )
}

/// A book with one entry over `sku`, and a pricing quorum of one: the book and the entry.
async fn priced(pricing: &Router, who: &SecurityContext, sku: Uuid) -> (Uuid, Uuid) {
    let (status, book) = pricing_call(
        pricing,
        who,
        "POST",
        "/price-books",
        &json!({"code":"standard","name":"Standard","currency":"EUR"}),
        Some("book"),
    )
    .await;
    assert_eq!(status, 201, "{book}");
    let book: Uuid = book["id"].as_str().unwrap().parse().unwrap();
    let (status, entry) = pricing_call(
        pricing,
        who,
        "POST",
        &format!("/price-books/{book}/entries"),
        &json!({"sku_id":sku,"model":"per_unit","usage_rating_policy":pricing_policy_support::storage_input()}),
        Some("entry"),
    )
    .await;
    assert_eq!(status, 201, "{entry}");
    let entry: Uuid = entry["id"].as_str().unwrap().parse().unwrap();
    let response = request_as(
        pricing,
        who,
        axum::http::Method::GET,
        "/bss-pricing/v1/approval-policy",
        None,
        None,
    )
    .await;
    let tag = response.headers()["etag"].to_str().unwrap().to_owned();
    let response = request_as(
        pricing,
        who,
        axum::http::Method::PUT,
        "/bss-pricing/v1/approval-policy",
        Some(json!({"quorum":1})),
        Some(&tag),
    )
    .await;
    assert_eq!(response.status(), 200);
    (book, entry)
}

/// The facade gear booted from its config over a hub holding the two sources: its router.
async fn facade_over(
    pricing_source: Arc<PricingApprovalSource>,
    products_source: Arc<ProductsApprovalSource>,
) -> Router {
    facade_in(
        pricing_source,
        products_source,
        &toolkit::api::OpenApiRegistryImpl::new(),
    )
    .await
}

/// [`facade_over`], with its doors registered in `openapi`.
async fn facade_in(
    pricing_source: Arc<PricingApprovalSource>,
    products_source: Arc<ProductsApprovalSource>,
    openapi: &dyn toolkit::api::OpenApiRegistry,
) -> Router {
    let facade_hub = Arc::new(toolkit::ClientHub::new());
    facade_hub.register_scoped::<dyn ApprovalSourceV1>(ClientScope::new("pricing"), pricing_source);
    facade_hub
        .register_scoped::<dyn ApprovalSourceV1>(ClientScope::new("products"), products_source);
    let ctx = GearCtx::new(
        "bss-approvals",
        Uuid::new_v4(),
        Arc::new(Config(
            json!({"config": {"sources": ["pricing", "products"]}}),
        )),
        facade_hub,
        tokio_util::sync::CancellationToken::new(),
    );
    let gear = bss_approvals::BssApprovalsGear::default();
    gear.init(&ctx).await.unwrap();
    gear.register_rest(&ctx, Router::new(), openapi).unwrap()
}

/// Pricing on its own database (`pricing_db`, or a fresh file), products on `db`, both under
/// one PDP, and the facade over the two.
async fn inbox_on(
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
    dsn: TestDsn,
    pricing_db: Option<(toolkit_db::DBProvider<toolkit_db::DbError>, TestDsn)>,
) -> Inbox {
    let products = census_on(0, db, dsn).await;
    let grants = Arc::new(Grants::default());
    *grants.tenant.lock().unwrap() = products.tenant;
    let enforcer = PolicyEnforcer::new(grants.clone());
    let sku = published_sku(&products).await;
    // Pricing, as the cross-gear precedent boots it.
    let (pricing_db, pricing_dsn) = if let Some(pair) = pricing_db {
        pair
    } else {
        fresh_pricing_db().await
    };
    let hub = Arc::new(toolkit::ClientHub::default());
    hub.register::<dyn bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1>(Arc::new(
        pricing_policy_support::MeterProvider::default(),
    ));
    hub.register::<bss_products_sdk::PricingReferenceRegistry>(Arc::new(
        bss_products_sdk::PricingReferenceRegistry(Arc::new(
            crate::infra::reference_registry::LocalReferenceRegistry::for_owner("pricing")
                .with_runtime(
                    products.state.clone(),
                    Arc::new(flat_in_enforcer(products.tenant)),
                ),
        )),
    ));
    let pricing_state = Arc::new(AuthoringState::new(pricing_db, hub).await.unwrap());
    let pricing = bss_pricing::api::rest::authoring::router(
        pricing_state.clone(),
        &toolkit::api::OpenApiRegistryImpl::new(),
    )
    .layer(Extension(enforcer.clone()))
    .layer(axum::middleware::from_fn(
        toolkit::api::canonical_error_middleware,
    ));
    // Products' SKU reads carry pricing's usage through pricing's port (P-D-197).
    products
        .state
        .hub
        .register::<dyn bss_products_sdk::sku_usage::SkuUsageV1>(Arc::new(
            bss_pricing::api::sku_usage::PricingSkuUsage::new(
                pricing_state.clone(),
                enforcer.clone(),
            ),
        ));
    let (book, entry) = priced(&pricing, &products.author, sku).await;
    let pricing_source = Arc::new(PricingApprovalSource::new(
        pricing_state.clone(),
        enforcer.clone(),
    ));
    let products_source = Arc::new(ProductsApprovalSource::new(
        products.state.clone(),
        enforcer.clone(),
    ));
    let facade = facade_over(pricing_source.clone(), products_source.clone()).await;
    // The products door votes under the same PDP.
    let products = Census {
        door: super::tests::gateway(
            super::tests::routes(
                products.state.clone(),
                &toolkit::api::OpenApiRegistryImpl::new(),
            )
            .layer(Extension(enforcer)),
        ),
        ..products
    };
    Inbox {
        products,
        pricing_state,
        _pricing_dsn: pricing_dsn,
        pricing,
        grants,
        facade,
        pricing_source,
        products_source,
        sku,
        book,
        entry,
    }
}

impl Inbox {
    fn tenant(&self) -> Uuid {
        self.products.tenant
    }
    /// A pending prices unit: a draft by `who`, submitted by `who`.
    async fn prices_unit(&self, who: &SecurityContext, from: &str) -> Uuid {
        let (status, draft, _) = send(
            &self.pricing,
            who,
            "POST",
            &format!("/bss-pricing/v1/price-book-entries/{}/prices", self.entry),
            &json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":from}).to_string(),
            Some(&format!("draft-{from}")),
        )
        .await;
        assert_eq!(status, 201, "{draft}");
        let draft: Value = serde_json::from_str(&draft).unwrap();
        let price = draft["items"][0]["id"].as_str().unwrap().to_owned();
        let (status, receipt, _) = send(
            &self.pricing,
            who,
            "POST",
            &format!("/bss-pricing/v1/prices/{price}/submit"),
            "{}",
            Some(&format!("submit-{from}")),
        )
        .await;
        assert_eq!(status, 201, "{receipt}");
        let receipt: Value = serde_json::from_str(&receipt).unwrap();
        receipt["unit"]["id"].as_str().unwrap().parse().unwrap()
    }
    /// Pricing units written straight through pricing's own store.
    async fn pricing_stored(&self, units: Vec<(&'static str, OffsetDateTime)>) -> Vec<Uuid> {
        let tenant = self.tenant();
        let store = bss_pricing::infra::storage::repo::approval_repo::PricingApprovalStore {
            scope: AccessScope::for_tenant(tenant),
            tenant_id: tenant,
        };
        let book = self.book;
        let units: Vec<Unit> = units
            .into_iter()
            .map(|(kind, at)| Unit {
                id: Uuid::new_v4(),
                tenant_id: tenant,
                kind: kind.to_owned(),
                ref_type: if kind == "prices" {
                    "price_book".into()
                } else {
                    "plan_revision".into()
                },
                ref_id: if kind == "prices" {
                    book
                } else {
                    Uuid::new_v4()
                },
                state: UnitState::Pending,
                common_effective_date: None,
                quorum_required: 1,
                generation: 1,
                submitted_by: Uuid::new_v4(),
                submitted_at: at,
                submit_note: None,
                decided_at: None,
                decided_note: None,
                snapshot: json!({"stored":true}),
                snapshot_hash: "h".into(),
                version: 1,
            })
            .collect();
        let ids = units.iter().map(|u| u.id).collect();
        self.pricing_state
            .db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    for unit in &units {
                        store
                            .insert_unit(tx, unit, &[])
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        ids
    }
    async fn get(&self, who: &SecurityContext, path: &str) -> (u16, Value) {
        let (status, body, _) = send(&self.facade, who, "GET", path, "", None).await;
        (status, serde_json::from_str(&body).unwrap_or(Value::Null))
    }
    /// Every unit id the facade lists from `query` (which may name an order), `limit` at a time,
    /// following `next_cursor`, which carries the order and the narrowing.
    async fn walk(&self, who: &SecurityContext, query: &str, again: &str) -> Vec<Uuid> {
        let mut ids = Vec::new();
        let mut path = format!("{FACADE}?{query}");
        for _ in 0..WALK_BOUND {
            let (status, page) = self.get(who, &path).await;
            assert_eq!(status, 200, "{path}: {page}");
            ids.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|u| u["id"].as_str().unwrap().parse::<Uuid>().unwrap()),
            );
            let Some(next) = page["next_cursor"].as_str() else {
                return ids;
            };
            path = format!("{FACADE}?{again}&cursor={next}");
        }
        panic!("the walk did not end within {WALK_BOUND} pages: {ids:?}");
    }
}

fn whole(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_000 + seconds).unwrap()
}

/// A server registers every linked gear's doors in ONE `OpenAPI` registry, which refuses (a panic at
/// boot) a schema name two gears define differently. The facade's doors register beside products'
/// and pricing's: its schemas carry their own names.
#[tokio::test]
async fn the_facade_registers_beside_both_gears_in_one_openapi_registry() {
    let i = inbox().await;
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let _products = super::tests::routes(i.products.state.clone(), &openapi);
    let _pricing = bss_pricing::api::rest::authoring::router(i.pricing_state.clone(), &openapi);
    let _read = bss_pricing::api::rest::read_contract::router(i.pricing_state.clone(), &openapi);
    let _facade = facade_in(
        i.pricing_source.clone(),
        i.products_source.clone(),
        &openapi,
    )
    .await;
    let spec = openapi
        .build_openapi(&toolkit::api::OpenApiInfo::default())
        .expect("openapi");
    let json = serde_json::to_value(&spec).unwrap();
    let paths = json["paths"].as_object().expect("paths");
    assert!(
        paths.keys().any(|path| path.starts_with("/bss-products/")),
        "{paths:?}"
    );
    assert!(
        paths.keys().any(|path| path.starts_with("/bss-pricing/")),
        "{paths:?}"
    );
    assert!(
        paths.keys().any(|path| path.starts_with("/bss-approvals/")),
        "{paths:?}"
    );
}

/// A walk over prices, plan revisions and the three SKU kinds of both gears, in both orders and at
/// every page size: every unit once, in `(submitted_at, id)` order, ties across and within a gear
/// broken by the id.
#[tokio::test]
async fn the_inbox_walks_both_gears_in_both_orders_each_unit_once() {
    let i = inbox().await;
    let mut all: Vec<(OffsetDateTime, Uuid)> = Vec::new();
    for (seconds, kind) in [
        (1, "prices"),
        (3, "plan_revision"),
        (3, "prices"),
        (6, "plan_revision"),
        (9, "prices"),
    ] {
        let id = i.pricing_stored(vec![(kind, whole(seconds))]).await[0];
        all.push((whole(seconds), id));
    }
    for (seconds, kind) in [
        (2, "sku_publish"),
        (3, "sku_change"),
        (5, "sku_retire"),
        (6, "sku_publish"),
        (7, "sku_change"),
        (7, "sku_retire"),
    ] {
        let id = stored_in(
            &i.products.state.db,
            i.tenant(),
            vec![(kind, i.sku, whole(seconds))],
        )
        .await[0];
        all.push((whole(seconds), id));
    }
    all.sort();
    let ascending: Vec<Uuid> = all.iter().map(|(_, id)| *id).collect();
    let descending: Vec<Uuid> = ascending.iter().rev().copied().collect();
    let who = i.products.author.clone();
    // Pending only: the fixture's SKU was published at once, by an approved unit of its own.
    for limit in [1, 2, 3, 50] {
        assert_eq!(
            i.walk(
                &who,
                &format!("state=pending&limit={limit}&$orderby=submitted_at%20asc"),
                &format!("state=pending&limit={limit}")
            )
            .await,
            ascending,
            "ascending, {limit} a page"
        );
        assert_eq!(
            i.walk(
                &who,
                &format!("state=pending&limit={limit}"),
                &format!("state=pending&limit={limit}")
            )
            .await,
            descending,
            "newest first by default, {limit} a page"
        );
    }
    let (status, counts) = i.get(&who, &format!("{FACADE}/counts?state=pending")).await;
    assert_eq!(status, 200, "{counts}");
    assert_eq!(counts["total"], 11);
    assert_eq!(
        counts["by_kind"],
        json!({"prices":3,"plan_revision":2,"sku_publish":2,"sku_change":2,"sku_retire":2})
    );
}

/// AP-D-3: a caller without products' approval-unit read sees pricing's units, and `sources` names
/// products forbidden; without either gear's read, 403.
#[tokio::test]
async fn a_pricing_only_approver_sees_products_forbidden() {
    let i = inbox().await;
    i.pricing_stored(vec![("prices", whole(1))]).await;
    stored_in(
        &i.products.state.db,
        i.tenant(),
        vec![("sku_publish", i.sku, whole(2))],
    )
    .await;
    let who = authed_ctx(i.tenant());
    i.grants
        .no_products
        .lock()
        .unwrap()
        .insert(who.subject_id());
    for path in [FACADE.to_owned(), format!("{FACADE}/counts")] {
        let (status, body) = i.get(&who, &path).await;
        assert_eq!(status, 200, "{path}: {body}");
        assert_eq!(
            body["sources"],
            json!([{"name":"pricing","status":"ok"},{"name":"products","status":"forbidden"}]),
            "{path}"
        );
        if path == FACADE {
            let sources: Vec<&str> = body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|u| u["source"].as_str().unwrap())
                .collect();
            assert_eq!(sources, vec!["pricing"]);
        } else {
            assert_eq!(body["total"], 1);
        }
    }
    i.grants.no_pricing.lock().unwrap().insert(who.subject_id());
    assert_eq!(i.get(&who, FACADE).await.0, 403);
}

/// AP-D-4: the facade votes under the owning door's own endpoint, so a facade vote and a direct
/// vote with the same `Idempotency-Key` and body are ONE vote, whichever comes first; the same key
/// with another body is the door's `IDEMPOTENCY_CONFLICT`, byte for byte.
#[tokio::test]
async fn a_facade_vote_and_a_direct_vote_with_one_key_replay_once() {
    let i = inbox().await;
    let pricing_unit = i.prices_unit(&i.products.author, "2031-03-01").await;
    let (_, products_unit) = i.products.unit("B").await;
    let products_unit: Uuid = products_unit["id"].as_str().unwrap().parse().unwrap();
    let reviewer = authed_ctx(i.tenant());
    for (door, door_path, id, facade_first) in [
        (
            &i.pricing,
            "/bss-pricing/v1/approval-units",
            pricing_unit,
            true,
        ),
        (
            &i.products.door,
            "/bss-products/v1/approval-units",
            products_unit,
            false,
        ),
    ] {
        let body = r#"{"generation":1,"note":"looks right"}"#;
        let key = format!("one-{id}");
        let facade = format!("{FACADE}/{id}/approve");
        let direct = format!("{door_path}/{id}/approve");
        let (first, second) = if facade_first {
            (
                send(&i.facade, &reviewer, "POST", &facade, body, Some(&key)).await,
                send(door, &reviewer, "POST", &direct, body, Some(&key)).await,
            )
        } else {
            (
                send(door, &reviewer, "POST", &direct, body, Some(&key)).await,
                send(&i.facade, &reviewer, "POST", &facade, body, Some(&key)).await,
            )
        };
        assert_eq!(first.0, 200, "{}", first.1);
        assert_eq!(second, first, "the second is the first's replay: {direct}");
        let (status, card) = i.get(&reviewer, &format!("{FACADE}/{id}")).await;
        assert_eq!(status, 200, "{card}");
        assert_eq!(card["decisions"].as_array().unwrap().len(), 1, "{card}");
        let other = r#"{"generation":1,"note":"another"}"#;
        let through_facade = send(&i.facade, &reviewer, "POST", &facade, other, Some(&key)).await;
        let through_door = send(door, &reviewer, "POST", &direct, other, Some(&key)).await;
        assert_eq!(through_facade.0, 409, "{}", through_facade.1);
        assert!(
            through_facade.1.contains("IDEMPOTENCY_CONFLICT"),
            "{}",
            through_facade.1
        );
        assert_eq!(through_facade, through_door);
    }
}

/// The owning gear judges separation of duties: the submitter's approve through the facade is the
/// door's own 403 `SOD_VIOLATION`, byte for byte, in both gears.
#[tokio::test]
async fn separation_of_duties_is_the_doors_refusal_through_the_facade() {
    let i = inbox().await;
    let author = i.products.author.clone();
    let pricing_unit = i.prices_unit(&author, "2031-03-01").await;
    let (_, products_unit) = i.products.unit("B").await;
    let products_unit: Uuid = products_unit["id"].as_str().unwrap().parse().unwrap();
    for (door, door_path, id) in [
        (&i.pricing, "/bss-pricing/v1/approval-units", pricing_unit),
        (
            &i.products.door,
            "/bss-products/v1/approval-units",
            products_unit,
        ),
    ] {
        let body = r#"{"generation":1}"#;
        let through_facade = send(
            &i.facade,
            &author,
            "POST",
            &format!("{FACADE}/{id}/approve"),
            body,
            Some("sod-facade"),
        )
        .await;
        let through_door = send(
            door,
            &author,
            "POST",
            &format!("{door_path}/{id}/approve"),
            body,
            Some("sod-door"),
        )
        .await;
        assert_eq!(through_facade.0, 403, "{}", through_facade.1);
        assert!(
            through_facade.1.contains("SOD_VIOLATION"),
            "{}",
            through_facade.1
        );
        assert_eq!(through_facade, through_door);
    }
}

/// P-D-250, P-D-197: a SKU change or retire unit carries pricing's usage of its SKU; a caller
/// without pricing's entry read sees `impact: null` and the page still answers; a publish has
/// none.
#[tokio::test]
async fn products_impact_is_null_for_a_caller_without_entry_read() {
    let i = inbox().await;
    stored_in(
        &i.products.state.db,
        i.tenant(),
        vec![
            ("sku_change", i.sku, whole(1)),
            ("sku_retire", i.sku, whole(2)),
            ("sku_publish", i.sku, whole(3)),
        ],
    )
    .await;
    let reader = authed_ctx(i.tenant());
    let blind = authed_ctx(i.tenant());
    i.grants
        .no_entry_read
        .lock()
        .unwrap()
        .insert(blind.subject_id());
    for (who, sees) in [(&reader, true), (&blind, false)] {
        let (status, page) = i.get(who, &format!("{FACADE}?kind=sku_change")).await;
        assert_eq!(status, 200, "{page}");
        let (status, retire) = i.get(who, &format!("{FACADE}?kind=sku_retire")).await;
        assert_eq!(status, 200, "{retire}");
        for unit in [&page["items"][0], &retire["items"][0]] {
            if sees {
                assert_eq!(unit["impact"]["entries"], 1, "{unit}");
            } else {
                assert_eq!(unit["impact"], Value::Null, "{unit}");
            }
            let id = unit["id"].as_str().unwrap();
            let (status, card) = i.get(who, &format!("{FACADE}/{id}")).await;
            assert_eq!(status, 200, "{card}");
            assert_eq!(card["impact"], unit["impact"], "the card's impact");
        }
        let (status, publish) = i.get(who, &format!("{FACADE}?kind=sku_publish")).await;
        assert_eq!(status, 200, "{publish}");
        assert!(
            publish["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|u| u["impact"].is_null()),
            "{publish}"
        );
        let (status, skipped) = i
            .get(who, &format!("{FACADE}?kind=sku_change&impact=false"))
            .await;
        assert_eq!(status, 200, "{skipped}");
        assert_eq!(
            skipped["items"][0]["id"], page["items"][0]["id"],
            "{skipped}"
        );
        assert_eq!(skipped["items"][0]["impact"], Value::Null, "impact=false");
    }
}

/// AP-D-2: a kind one gear does not record, and products' `book_id`, are that gear's empty answer:
/// the facade answers 200 with the other gear's units and both gears named ok.
#[tokio::test]
async fn a_foreign_kind_and_a_products_book_id_are_empty_not_400() {
    let i = inbox().await;
    i.pricing_stored(vec![("prices", whole(1)), ("plan_revision", whole(2))])
        .await;
    stored_in(
        &i.products.state.db,
        i.tenant(),
        vec![
            ("sku_publish", i.sku, whole(3)),
            ("sku_change", i.sku, whole(4)),
        ],
    )
    .await;
    let who = i.products.author.clone();
    let book = i.book.to_string();
    // Pending only: the fixture's SKU was published at once, by an approved unit of its own.
    for (query, sources, total) in [
        (
            "state=pending&kind=sku_publish".to_owned(),
            vec!["products"],
            1,
        ),
        (
            "state=pending&kind=sku_change".to_owned(),
            vec!["products"],
            1,
        ),
        ("state=pending&kind=prices".to_owned(), vec!["pricing"], 1),
        (
            "state=pending&kind=plan_revision".to_owned(),
            vec!["pricing"],
            1,
        ),
        ("kind=bogus".to_owned(), vec![], 0),
        (format!("book_id={book}"), vec!["pricing"], 1),
        (format!("kind=prices&book_id={book}"), vec!["pricing"], 1),
    ] {
        let (status, page) = i.get(&who, &format!("{FACADE}?{query}")).await;
        assert_eq!(status, 200, "{query}: {page}");
        let seen: Vec<&str> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["source"].as_str().unwrap())
            .collect();
        assert_eq!(seen, sources, "{query}");
        assert_eq!(
            page["sources"],
            json!([{"name":"pricing","status":"ok"},{"name":"products","status":"ok"}]),
            "{query}"
        );
        let (status, counts) = i.get(&who, &format!("{FACADE}/counts?{query}")).await;
        assert_eq!(status, 200, "{query}: {counts}");
        assert_eq!(counts["total"], total, "{query}");
    }
}

/// The facade adds no statement of its own: a list, the counts and a card read, on each gear's
/// database, exactly the statements that gear's source reads for the same question; the counts
/// read off any transaction.
#[tokio::test]
async fn the_facade_adds_no_statement_beyond_its_sources() {
    let (db, _, _, dsn, products_recorder) = recorded_test_db().await;
    let pricing_dsn = TestDsn::new("pricing-inbox-recorded-");
    let (pricing_db, pricing_recorder) = toolkit_db::test_support::connect_with_recorder(
        &pricing_dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    {
        use toolkit::contracts::DatabaseCapability;
        toolkit_db::migration_runner::run_migrations_for_testing(
            &pricing_db,
            bss_pricing::module::BssPricingGear::default().migrations(),
        )
        .await
        .unwrap();
    }
    let i = inbox_on(
        db,
        dsn,
        Some((toolkit_db::DBProvider::new(pricing_db), pricing_dsn)),
    )
    .await;
    let pricing_ids = i
        .pricing_stored(vec![("prices", whole(1)), ("plan_revision", whole(3))])
        .await;
    stored_in(
        &i.products.state.db,
        i.tenant(),
        vec![
            ("sku_change", i.sku, whole(2)),
            ("sku_publish", i.sku, whole(4)),
        ],
    )
    .await;
    let who = i.products.author.clone();
    let statements = |recorder: &toolkit_db::test_support::QueryRecorder, prefix: &str| {
        recorder
            .events()
            .into_iter()
            .filter(|q| q.table.as_deref().is_some_and(|t| t.starts_with(prefix)))
            .map(|q| (q.sql, q.in_tx))
            .collect::<Vec<_>>()
    };
    let clear = || {
        products_recorder.clear();
        pricing_recorder.clear();
    };
    // The list.
    clear();
    let (status, page) = i.get(&who, &format!("{FACADE}?limit=10")).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(
        page["items"].as_array().unwrap().len(),
        4,
        "the four stored units; the legacy raw SKU has no publish unit"
    );
    let facade = (
        statements(&pricing_recorder, "pricing_"),
        statements(&products_recorder, "products_"),
    );
    clear();
    let q = SourcePageQuery {
        narrowing: SourceNarrowing::default(),
        order: Order::Desc,
        limit: 10,
        after: None,
        impact: true,
    };
    i.pricing_source.page(&who, &q).await.unwrap();
    i.products_source.page(&who, &q).await.unwrap();
    let direct = (
        statements(&pricing_recorder, "pricing_"),
        statements(&products_recorder, "products_"),
    );
    assert!(!facade.0.is_empty() && !facade.1.is_empty());
    assert_eq!(facade, direct, "the list");
    // The counts: one statement in each gear, off any transaction.
    clear();
    assert_eq!(i.get(&who, &format!("{FACADE}/counts")).await.0, 200);
    let facade = (
        statements(&pricing_recorder, "pricing_"),
        statements(&products_recorder, "products_"),
    );
    clear();
    i.pricing_source
        .counts(&who, &SourceNarrowing::default())
        .await
        .unwrap();
    i.products_source
        .counts(&who, &SourceNarrowing::default())
        .await
        .unwrap();
    let direct = (
        statements(&pricing_recorder, "pricing_"),
        statements(&products_recorder, "products_"),
    );
    assert_eq!(facade, direct, "the counts");
    assert_eq!((facade.0.len(), facade.1.len()), (1, 1), "{facade:?}");
    assert!(
        facade.0.iter().chain(&facade.1).all(|(_, in_tx)| !in_tx),
        "the counts stay off the list's transaction: {facade:?}"
    );
    // The card: every source is asked (AP-D-3).
    clear();
    let id = pricing_ids[0];
    assert_eq!(i.get(&who, &format!("{FACADE}/{id}")).await.0, 200);
    let facade = (
        statements(&pricing_recorder, "pricing_"),
        statements(&products_recorder, "products_"),
    );
    clear();
    i.pricing_source.get(&who, id, true).await.unwrap();
    i.products_source.get(&who, id, true).await.unwrap();
    let direct = (
        statements(&pricing_recorder, "pricing_"),
        statements(&products_recorder, "products_"),
    );
    assert_eq!(facade, direct, "the card");
}
