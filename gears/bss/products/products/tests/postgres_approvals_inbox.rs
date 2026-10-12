#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The approvals inbox's walk over BOTH real gears on `PostgreSQL` (P-D-250, pricing D-490): each
//! gear on its own database, its source over its own state, the facade gear booted from its
//! config. Postgres orders `submitted_at` as an instant (`timestamptz`) and `id` as a `uuid`, so
//! the walk is checked for the EXACT `(submitted_at, id)` order in both directions, with
//! sub-second instants and ties within one gear and across the two. (`SQLite` keeps the instant as
//! text, which does not order as time inside one second, D-470.)
mod pg_support;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use bss_approval::{Store, Unit, UnitState};
use bss_approvals_sdk::ApprovalSourceV1;
use bss_pricing::api::rest::authoring::{AuthoringState, inbox_source::PricingApprovalSource};
use bss_products::api::rest::approval_units::inbox_source::ProductsApprovalSource;
use pg_support::Pg;
use serde_json::{Value, json};
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::client_hub::ClientScope;
use toolkit::contracts::DatabaseCapability;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_db::secure::AccessScope;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use tower::ServiceExt;
use uuid::Uuid;

/// Every subject of the tenant holds every grant: a flat `In` over the owner tenant.
struct Tenant(Uuid);
#[async_trait]
impl AuthZResolverApi for Tenant {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        _: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        vec![self.0],
                    ))],
                }],
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

struct Walk {
    _pg: (Pg, Pg),
    tenant: Uuid,
    facade: Router,
    pricing: toolkit_db::DBProvider<toolkit_db::DbError>,
    products: toolkit_db::DBProvider<toolkit_db::DbError>,
    _outbox: toolkit_db::outbox::OutboxHandle,
}

async fn walk_on_postgres() -> Walk {
    let tenant = Uuid::new_v4();
    let enforcer = PolicyEnforcer::new(Arc::new(Tenant(tenant)));
    // Products: its chain and its outbox's tables, a holding outbox, and the state its doors use.
    let products_pg = Pg::applied().await;
    let products_db = products_pg.db().await;
    toolkit_db::migration_runner::run_migrations_for_testing(
        &products_db,
        toolkit_db::outbox::outbox_migrations_with_prefix(
            bss_products::infra::events::OUTBOX_TABLE_PREFIX,
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let outbox = toolkit_db::outbox::Outbox::builder(products_db.clone())
        .table_prefix(bss_products::infra::events::OUTBOX_TABLE_PREFIX)
        .unwrap()
        .queue(
            bss_products::infra::events::QUEUE_NAME,
            toolkit_db::outbox::Partitions::of(bss_products::infra::events::PARTITIONS),
        )
        .leased(bss_products::infra::events::PendingBrokerProducer)
        .start()
        .await
        .unwrap();
    let products = toolkit_db::DBProvider::new(products_db);
    let products_state = Arc::new(bss_products::api::rest::ApiState {
        db: products.clone(),
        sink: bss_products::infra::broker::EventSink::Interim(Arc::clone(outbox.outbox())),
        usage_type_catalog: Arc::new(bss_products::infra::usage_types::UnconfiguredUsageTypes),
        usage_type_catalog_source: "unconfigured",
        idempotency_retention_hours: 24,
        fence_ttl_minutes: 30,
        reference_principals: std::collections::BTreeMap::new(),
        hub: Arc::new(toolkit::ClientHub::new()),
        actor_names: bss_products::api::rest::ApiState::names_from(&Arc::new(
            toolkit::ClientHub::new(),
        )),
    });
    // Pricing: its own database and chain, and its authoring state.
    let pricing_pg = Pg::empty().await;
    let pricing_db = pricing_pg.db().await;
    toolkit_db::migration_runner::run_migrations_for_testing(
        &pricing_db,
        bss_pricing::module::BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    let pricing = toolkit_db::DBProvider::new(pricing_db);
    let pricing_state = Arc::new(
        AuthoringState::new(pricing.clone(), Arc::new(toolkit::ClientHub::new()))
            .await
            .unwrap(),
    );
    // The facade, booted from its config over the two sources.
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register_scoped::<dyn ApprovalSourceV1>(
        ClientScope::new("pricing"),
        Arc::new(PricingApprovalSource::new(pricing_state, enforcer.clone())),
    );
    hub.register_scoped::<dyn ApprovalSourceV1>(
        ClientScope::new("products"),
        Arc::new(ProductsApprovalSource::new(products_state, enforcer)),
    );
    let ctx = GearCtx::new(
        "bss-approvals",
        Uuid::new_v4(),
        Arc::new(Config(
            json!({"config": {"sources": ["pricing", "products"]}}),
        )),
        hub,
        tokio_util::sync::CancellationToken::new(),
    );
    let gear = bss_approvals::BssApprovalsGear::default();
    gear.init(&ctx).await.unwrap();
    let facade = gear
        .register_rest(
            &ctx,
            Router::new(),
            &toolkit::api::OpenApiRegistryImpl::new(),
        )
        .unwrap();
    Walk {
        _pg: (pricing_pg, products_pg),
        tenant,
        facade,
        pricing,
        products,
        _outbox: outbox,
    }
}

/// A pending unit of `kind` at `at`, written through `gear`'s own store.
fn unit(tenant: Uuid, kind: &str, at: OffsetDateTime) -> Unit {
    Unit {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        kind: kind.to_owned(),
        ref_type: "subject".into(),
        ref_id: Uuid::new_v4(),
        state: UnitState::Pending,
        common_effective_date: None,
        quorum_required: 1,
        generation: 1,
        submitted_by: Uuid::new_v4(),
        submitted_at: at,
        submit_note: None,
        decided_at: None,
        decided_note: None,
        snapshot: json!({}),
        snapshot_hash: "h".into(),
        version: 1,
    }
}

impl Walk {
    async fn pricing_units(&self, units: Vec<Unit>) {
        let store = bss_pricing::infra::storage::repo::approval_repo::PricingApprovalStore {
            scope: AccessScope::for_tenant(self.tenant),
            tenant_id: self.tenant,
        };
        self.pricing
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    for u in &units {
                        store
                            .insert_unit(tx, u, &[])
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }
    async fn products_units(&self, units: Vec<Unit>) {
        let store = bss_products::infra::storage::repo::ProductsApprovalStore {
            scope: AccessScope::for_tenant(self.tenant),
            tenant_id: self.tenant,
        };
        self.products
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    for u in &units {
                        store
                            .insert_unit(tx, u, &[])
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }
    async fn get(&self, who: &SecurityContext, path: &str) -> Value {
        let response = self
            .facade
            .clone()
            .oneshot(
                Request::get(path)
                    .extension(who.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, 200, "{path}: {body}");
        body
    }
    /// Every unit the facade lists, `limit` at a time, following `next_cursor`.
    async fn ids(&self, who: &SecurityContext, first: &str, limit: u32) -> Vec<Uuid> {
        let mut ids = Vec::new();
        let mut path = format!("/bss-approvals/v1/approval-units?limit={limit}{first}");
        // More pages than this walk takes: a walk that repeats fails instead of looping.
        for _ in 0..100 {
            let page = self.get(who, &path).await;
            let items = page["items"].as_array().unwrap();
            assert!(items.len() <= usize::try_from(limit).unwrap(), "{path}");
            ids.extend(
                items
                    .iter()
                    .map(|u| u["id"].as_str().unwrap().parse::<Uuid>().unwrap()),
            );
            let Some(next) = page["next_cursor"].as_str() else {
                return ids;
            };
            path = format!("/bss-approvals/v1/approval-units?limit={limit}&cursor={next}");
        }
        panic!("the walk did not end within 100 pages: {ids:?}");
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_inbox_walks_both_gears_on_postgres_in_the_exact_order_both_ways() {
    let w = walk_on_postgres().await;
    let base = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() - time::Duration::days(1);
    let at = |micros: i64| base + time::Duration::microseconds(micros);
    // Sub-second instants, a tie inside each gear, and ties across the two gears.
    let pricing = vec![
        unit(w.tenant, "prices", at(10)),
        unit(w.tenant, "plan_revision", at(250_000)),
        unit(w.tenant, "prices", at(250_000)),
        unit(w.tenant, "plan_revision", at(999_999)),
        unit(w.tenant, "prices", at(1_000_000)),
        unit(w.tenant, "plan_revision", at(1_500_001)),
    ];
    let products = vec![
        unit(w.tenant, "sku_publish", at(9)),
        unit(w.tenant, "sku_change", at(250_000)),
        unit(w.tenant, "sku_retire", at(250_000)),
        unit(w.tenant, "sku_publish", at(1_000_000)),
        unit(w.tenant, "sku_change", at(1_000_001)),
        unit(w.tenant, "sku_retire", at(2_000_000)),
        unit(w.tenant, "sku_publish", at(1_500_001)),
    ];
    let mut expected: Vec<(OffsetDateTime, Uuid)> = pricing
        .iter()
        .chain(&products)
        .map(|u| (u.submitted_at, u.id))
        .collect();
    expected.sort();
    let ascending: Vec<Uuid> = expected.iter().map(|(_, id)| *id).collect();
    let descending: Vec<Uuid> = ascending.iter().rev().copied().collect();
    w.pricing_units(pricing).await;
    w.products_units(products).await;
    let who = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(w.tenant)
        .subject_type("user")
        .build()
        .unwrap();
    for limit in [1, 2, 3, 5, 50] {
        assert_eq!(
            w.ids(&who, "&$orderby=submitted_at%20asc", limit).await,
            ascending,
            "ascending, {limit} a page"
        );
        assert_eq!(
            w.ids(&who, "", limit).await,
            descending,
            "newest first, {limit} a page"
        );
    }
    let counts = w.get(&who, "/bss-approvals/v1/approval-units/counts").await;
    assert_eq!(counts["total"], 13, "{counts}");
    assert_eq!(
        counts["sources"],
        json!([{"name":"pricing","status":"ok"},{"name":"products","status":"ok"}])
    );
}
