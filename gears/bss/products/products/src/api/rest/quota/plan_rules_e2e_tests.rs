//! Pricing's plan quota rules against Products' real quota catalog (P-D-275, P-D-276; pricing
//! D-542, D-545). Both gears run in one process, as a deployment links them: Products' quota
//! endpoints approve the definitions, Products' in-process quota catalog client and reference
//! registry are registered on one `ClientHub` as the gear's init registers them, and Pricing's
//! production router is built on that hub. No quota double exists in this file: every verdict
//! Pricing receives is Products' own judgement of the set.
//!
//! The walk: a SKU and its versions; vCPU and RAM definitions approved on one adapter target and
//! enabled; a priced draft revision of a plan with one item of the SKU; a `PUT` of 10 vCPU and
//! 20 GB per plan unit; green checks; a submit at quorum 0 that publishes the rules unchanged.
//! Then the refusals: an amount off the catalog's grid, a definition pending qualification that is
//! saved and checks red, and two definitions of two adapter targets on one binding. Last, a
//! purchase of two units through Pricing's read and sellability providers freezes 20 vCPU and
//! 40 GB (pricing D-547, D-548).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use axum::http::Method;
use bss_products_sdk::ProductsQuotaCatalogClientV1;
use bss_products_sdk::models::Lifecycle;
use bss_products_sdk::quota::catalog::QuotaVersion;
use serde_json::{Value, json};
use toolkit::contracts::DatabaseCapability;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::infra::quota::catalog_client::LocalQuotaCatalogClient;
use crate::infra::reference_registry::LocalReferenceRegistry;
use crate::quota_test_support::{
    GB_UNIT, QuotaApp, QuotaFakes, VCPU_UNIT, fresh_code, quota_app, ram_record_in,
    seed_sku_version, send, vcpu_record,
};
use crate::test_support::{TestDsn, body_json, repo_connection, tenant_user, toolkit_dsn};

/// A SKU of `tenant` with versions 1 to 3, published, so Pricing's entry, item and checks take it.
async fn published_sku(q: &QuotaApp, tenant: Uuid) -> Uuid {
    let sku = seed_sku_version(&q.dsn, tenant, &fresh_code(), 3).await;
    let (db, scope) = repo_connection(&q.dsn, tenant).await;
    crate::infra::storage::repo::set_lifecycle(
        &db.conn().unwrap(),
        &scope,
        tenant,
        sku,
        &[Lifecycle::Draft],
        Lifecycle::Published,
        crate::infra::storage::stored_now(),
    )
    .await
    .unwrap();
    sku
}

/// A draft revision of a new plan with one item of a SKU, its entry priced from today.
struct Draft {
    revision: Uuid,
    item: Uuid,
    etag: String,
}

/// Pricing on the hub Products registered its clients in: its state over a database of its own,
/// its production router under a PDP that grants the tenant's users, and the tenant's user.
struct PricingOverProducts {
    hub: Arc<toolkit::ClientHub>,
    router: axum::Router,
    user: SecurityContext,
    /// Pricing's runtime and policy enforcer, for its in-process providers.
    state: Arc<bss_pricing::api::rest::authoring::AuthoringState>,
    enforcer: Arc<PolicyEnforcer>,
    _dsn: TestDsn,
}

impl PricingOverProducts {
    async fn new(q: &QuotaApp, tenant: Uuid) -> Self {
        let enforcer = Arc::new(PolicyEnforcer::new(q.pdp.clone()));
        let hub = Arc::new(toolkit::ClientHub::new());
        // As `gear.rs` registers them: the quota catalog client and the reference registry
        // bound to the `pricing` owner, over Products' runtime.
        hub.register::<dyn ProductsQuotaCatalogClientV1>(Arc::new(LocalQuotaCatalogClient::new(
            q.state.clone(),
            enforcer.clone(),
        )));
        hub.register::<bss_products_sdk::PricingReferenceRegistry>(Arc::new(
            bss_products_sdk::PricingReferenceRegistry(Arc::new(
                LocalReferenceRegistry::for_owner("pricing")
                    .with_runtime(q.state.api.clone(), enforcer.clone()),
            )),
        ));
        let dsn = TestDsn::new("pricing-quota-e2e-");
        let db = toolkit_db::connect_db(
            &toolkit_dsn(&dsn),
            toolkit_db::ConnectOpts {
                max_conns: Some(4),
                min_conns: Some(1),
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
        let state = Arc::new(
            bss_pricing::api::rest::authoring::AuthoringState::new(
                toolkit_db::DBProvider::new(db),
                hub.clone(),
            )
            .await
            .unwrap(),
        );
        let router = bss_pricing::api::rest::authoring::router(
            state.clone(),
            &toolkit::api::OpenApiRegistryImpl::new(),
        )
        .layer(axum::Extension((*enforcer).clone()));
        Self {
            hub,
            router,
            user: tenant_user(tenant),
            state,
            enforcer,
            _dsn: dsn,
        }
    }

    /// One Pricing request as the tenant's user, with an `If-Match` and an `Idempotency-Key`
    /// when given: the status, the body and the `ETag`.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        if_match: Option<&str>,
        key: Option<&str>,
    ) -> (u16, Value, String) {
        let response = send(
            &self.router,
            &self.user,
            method,
            &format!("/bss-pricing/v1{path}"),
            (key, if_match),
            body.map(|b| b.to_string()),
        )
        .await;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        (status, body_json(response).await, etag)
    }

    /// A write answered 200 or 201: its body and `ETag`.
    async fn written(&self, method: Method, path: &str, body: Value) -> (Value, String) {
        let (status, answer, etag) = self
            .call(
                method,
                path,
                Some(body),
                None,
                Some(&Uuid::new_v4().to_string()),
            )
            .await;
        assert!(status == 200 || status == 201, "{path}: {status} {answer}");
        (answer, etag)
    }

    /// The tenant's approval policy for `kind` at `quorum`.
    async fn set_quorum(&self, kind: &str, quorum: u32) {
        let (_, _, tag) = self
            .call(Method::GET, "/approval-policy", None, None, None)
            .await;
        let (status, body, _) = self
            .call(
                Method::PUT,
                "/approval-policy",
                Some(json!({"kind": kind, "quorum": quorum})),
                Some(&tag),
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
    }

    /// A book, a monthly per-unit entry of `sku` priced from today (applied at once), a plan and
    /// one item of the SKU on that entry in its draft revision 1.
    async fn priced_draft_for(&self, sku: Uuid) -> Draft {
        self.priced_draft_with(sku, "per_unit", json!({"rate": "1"}))
            .await
    }

    /// [`Self::priced_draft_for`] with an entry of `model` priced at `price`.
    async fn priced_draft_with(&self, sku: Uuid, model: &str, price: Value) -> Draft {
        let code = format!("Q{}", &Uuid::new_v4().simple().to_string()[..8]).to_uppercase();
        let (book, _) = self
            .written(
                Method::POST,
                "/price-books",
                json!({"code": code, "name": code, "currency": "EUR"}),
            )
            .await;
        let book = book["id"].as_str().unwrap().to_owned();
        let (entry, _) = self
            .written(
                Method::POST,
                &format!("/price-books/{book}/entries"),
                json!({"sku_id": sku, "period": "month", "model": model}),
            )
            .await;
        let entry = entry["id"].as_str().unwrap().to_owned();
        self.set_quorum("prices", 0).await;
        let today = time::OffsetDateTime::now_utc().date().to_string();
        let (draft, _) = self
            .written(
                Method::POST,
                &format!("/price-book-entries/{entry}/prices"),
                json!({"price": price, "eligibility": "all", "effective_from": today}),
            )
            .await;
        let price = draft["items"][0]["id"].as_str().unwrap().to_owned();
        let (submitted, _) = self
            .written(Method::POST, &format!("/prices/{price}/submit"), json!({}))
            .await;
        assert_eq!(submitted["applied"], true, "{submitted}");
        let (plan, _) = self
            .written(
                Method::POST,
                "/plans",
                json!({"code": code, "name": code, "book_id": book}),
            )
            .await;
        let revision: Uuid = plan["revisions"][0]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let (item, _) = self
            .written(
                Method::POST,
                &format!("/plan-revisions/{revision}/items"),
                json!({"sku_id": sku, "price_book_entry_id": entry}),
            )
            .await;
        let item: Uuid = item["id"].as_str().unwrap().parse().unwrap();
        let (_, etag) = self.revision_with_etag(revision).await;
        Draft {
            revision,
            item,
            etag,
        }
    }

    /// The definition's digest, through Products' in-process client as the tenant's user.
    async fn definition_digest(&self, id: Uuid, version: QuotaVersion) -> String {
        self.hub
            .get::<dyn ProductsQuotaCatalogClientV1>()
            .unwrap()
            .get_quota_definition(&self.user, id, version, None)
            .await
            .unwrap()
            .definition_digest
    }

    /// Rule `n` on `item` of the definition `(id, version)` with `digest`: `amount` in `unit`,
    /// per plan unit, on the binding `compute` in the regional pool the fixtures' policy asks for.
    fn rule(
        n: u128,
        (id, version): (Uuid, QuotaVersion),
        digest: &str,
        item: Uuid,
        amount: &str,
        unit: &str,
    ) -> Value {
        json!({
            "rule_id": Uuid::from_u128(n),
            "definition_ref": {"id": id, "version": version.to_string(), "digest": digest},
            "item_id": item,
            "dimension_value": null,
            "logical_binding_key": "compute",
            "subject_selector": "resource_owner",
            "scope_selection": {"region": "region-1", "resource_pool": "pool-1"},
            "amount": amount,
            "unit_ref": unit,
            "basis": "per_plan_unit",
        })
    }

    /// `PUT` the set `rules` under the draft's `ETag` and `key`.
    async fn put_rules(&self, draft: &Draft, rules: Vec<Value>, key: &str) -> (u16, Value, String) {
        self.call(
            Method::PUT,
            &format!("/plan-revisions/{}/quota-rules", draft.revision),
            Some(json!({"quota_rules": rules})),
            Some(&draft.etag),
            Some(key),
        )
        .await
    }

    async fn checks(&self, draft: &Draft) -> Value {
        let (status, body, _) = self
            .call(
                Method::GET,
                &format!("/plan-revisions/{}/checks", draft.revision),
                None,
                None,
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }

    async fn submit(&self, draft: &Draft) -> (u16, Value) {
        let (status, body, _) = self
            .call(
                Method::POST,
                &format!("/plan-revisions/{}/submit", draft.revision),
                Some(json!({})),
                None,
                Some("submit"),
            )
            .await;
        (status, body)
    }

    async fn revision_with_etag(&self, revision: Uuid) -> (Value, String) {
        let (status, body, etag) = self
            .call(
                Method::GET,
                &format!("/plan-revisions/{revision}"),
                None,
                None,
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        (body, etag)
    }
}

/// The red check codes of a checks answer.
fn red(checks: &Value) -> Vec<String> {
    checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["ok"] == false)
        .map(|c| c["code"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plan_carries_10_vcpu_and_20_gb_per_unit_through_the_real_catalog() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = published_sku(&q, tenant).await;
    // One adapter target provisions the binding: both definitions on it.
    let target = q.fresh_target(tenant).await;
    let cpu = q.approved_on(tenant, target, vcpu_record(), sku, 3).await;
    let ram = q
        .approved_on(tenant, target, ram_record_in(GB_UNIT), sku, 3)
        .await;
    q.enable(tenant, cpu.0, cpu.1).await;
    q.enable(tenant, ram.0, ram.1).await;
    let pricing = PricingOverProducts::new(&q, tenant).await;
    let draft = pricing.priced_draft_for(sku).await;
    let rules = vec![
        PricingOverProducts::rule(
            1,
            cpu,
            &pricing.definition_digest(cpu.0, cpu.1).await,
            draft.item,
            "10",
            VCPU_UNIT,
        ),
        PricingOverProducts::rule(
            2,
            ram,
            &pricing.definition_digest(ram.0, ram.1).await,
            draft.item,
            "20",
            GB_UNIT,
        ),
    ];
    let (status, saved, _) = pricing.put_rules(&draft, rules, "k1").await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["quota_rules"][0]["availability"]["state"], "enabled");
    assert_eq!(saved["quota_rules"][1]["unit_ref"], GB_UNIT);
    let checks = pricing.checks(&draft).await;
    assert_eq!(checks["ready"], true, "{checks}");
    assert_eq!(checks["quota_rules_digest"], saved["quota_rules_digest"]);
    pricing.set_quorum("plan_revision", 0).await;
    let asked_before = q.pdp.asked().len();
    let (status, receipt) = pricing.submit(&draft).await;
    assert_eq!(
        (status, receipt["applied"].clone()),
        (201, json!(true)),
        "{receipt}"
    );
    // The submit validates the rules as its caller: Products asks the caller's `read` on its quota
    // definitions (pricing D-542).
    let read = (
        crate::authz::labels::QUOTA_DEFINITION.to_owned(),
        "read".to_owned(),
        Some(authz_resolver_sdk::models::TenantMode::RootOnly),
    );
    assert!(
        q.pdp.asked()[asked_before..].contains(&read),
        "{:?}",
        q.pdp.asked()
    );
    let after = &receipt["unit"]["snapshot"]["after"];
    assert_eq!(after["quota_rules_digest"], saved["quota_rules_digest"]);
    let (read, _) = pricing.revision_with_etag(draft.revision).await;
    assert_eq!(read["state"], "published");
    assert_eq!(read["quota_rules_digest"], saved["quota_rules_digest"]);
    assert_eq!(read["quota_rules"], saved["quota_rules"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_real_catalog_refuses_an_amount_off_its_grid_and_reports_a_pending_definition() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = published_sku(&q, tenant).await;
    // Approved, not enabled: its qualification is pending.
    let cpu = q.approved(tenant, vcpu_record(), sku, 3).await;
    let pricing = PricingOverProducts::new(&q, tenant).await;
    let draft = pricing.priced_draft_for(sku).await;
    let digest = pricing.definition_digest(cpu.0, cpu.1).await;
    let (status, problem, _) = pricing
        .put_rules(
            &draft,
            vec![PricingOverProducts::rule(
                1, cpu, &digest, draft.item, "5000", VCPU_UNIT,
            )],
            "k1",
        )
        .await;
    assert_eq!(status, 400, "{problem}");
    let text = problem.to_string();
    assert!(
        text.contains("QUOTA_AMOUNT_INVALID") && text.contains("quota_rules[0].amount"),
        "above the catalog's 1024: {text}"
    );
    let (status, saved, _) = pricing
        .put_rules(
            &draft,
            vec![PricingOverProducts::rule(
                1, cpu, &digest, draft.item, "10", VCPU_UNIT,
            )],
            "k2",
        )
        .await;
    assert_eq!(status, 200, "a pending definition is saved: {saved}");
    assert_eq!(saved["quota_rules"][0]["availability"]["state"], "disabled");
    let checks = pricing.checks(&draft).await;
    assert_eq!(checks["ready"], false);
    assert_eq!(
        red(&checks),
        ["QUOTA_QUALIFICATION_UNAVAILABLE"],
        "{checks}"
    );
}

/// Two definitions approved on two adapter targets cannot share one logical binding: Products
/// refuses the set on both rules' binding keys, and Pricing writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vcpu_and_ram_of_two_adapter_targets_cannot_share_a_binding() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = published_sku(&q, tenant).await;
    let (one, two) = (q.fresh_target(tenant).await, q.fresh_target(tenant).await);
    let cpu = q.approved_on(tenant, one, vcpu_record(), sku, 3).await;
    let ram = q
        .approved_on(tenant, two, ram_record_in(GB_UNIT), sku, 3)
        .await;
    q.enable(tenant, cpu.0, cpu.1).await;
    q.enable(tenant, ram.0, ram.1).await;
    let pricing = PricingOverProducts::new(&q, tenant).await;
    let draft = pricing.priced_draft_for(sku).await;
    let rules = vec![
        PricingOverProducts::rule(
            1,
            cpu,
            &pricing.definition_digest(cpu.0, cpu.1).await,
            draft.item,
            "10",
            VCPU_UNIT,
        ),
        PricingOverProducts::rule(
            2,
            ram,
            &pricing.definition_digest(ram.0, ram.1).await,
            draft.item,
            "20",
            GB_UNIT,
        ),
    ];
    let (status, problem, _) = pricing.put_rules(&draft, rules, "k1").await;
    assert_eq!(status, 400, "{problem}");
    let violations: Vec<(String, String)> = problem["context"]["field_violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["field"].as_str().unwrap().to_owned(),
                v["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    for field in [
        "quota_rules[0].logical_binding_key",
        "quota_rules[1].logical_binding_key",
    ] {
        assert!(
            violations.contains(&(field.to_owned(), "QUOTA_SET_INCOMPATIBLE".to_owned())),
            "{field}: {problem}"
        );
    }
    let (read, _) = pricing.revision_with_etag(draft.revision).await;
    assert_eq!(read["quota_rules"], json!([]), "nothing is written");
}

/// Pricing D-547, D-548: a purchase of two units of a plan with 10 vCPU and 20 GB per unit, through
/// the real catalog. The plan is published with its rules; the buyer resolves it through Pricing's
/// read provider and sends the selected bindings and selected quota digests of the item's cell at
/// quantity 2, as an order line does; `SellabilityProvider::check` freezes 20 vCPU and 40 GB, each
/// rule naming its definition by the catalog's own digest, and the receipt read answers the same
/// receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_purchase_of_two_units_freezes_20_vcpu_and_40_gb() {
    use bss_pricing::api::{
        pricing_acceptance::PricingAcceptanceProvider, pricing_read::PricingReadProvider,
        sellability::SellabilityProvider,
    };
    use bss_pricing_sdk::acceptance::{
        AcceptanceQuery, CommandMeta, Market, NewSaleQuery, PricingAcceptanceV1, SellabilityV1,
        TenantAxes, Term,
    };
    use bss_pricing_sdk::digest::{
        billing_terms_digest, selected_bindings_digest, selected_quota_digest,
    };
    use bss_pricing_sdk::read::{BindingSelection, CatalogRef, PricingReadV1, ResolveQuery};
    use bss_pricing_sdk::terms::{
        BillingAnchor, BillingCycle, BillingTerms, TermsSource, Timezone,
    };
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = published_sku(&q, tenant).await;
    let target = q.fresh_target(tenant).await;
    let cpu = q.approved_on(tenant, target, vcpu_record(), sku, 3).await;
    let ram = q
        .approved_on(tenant, target, ram_record_in(GB_UNIT), sku, 3)
        .await;
    q.enable(tenant, cpu.0, cpu.1).await;
    q.enable(tenant, ram.0, ram.1).await;
    let pricing = PricingOverProducts::new(&q, tenant).await;
    // The seeded SKU names no invoice inputs: the seller's defaults supply them.
    let (_, _, tag) = pricing
        .call(Method::GET, "/settings", None, None, None)
        .await;
    let (status, body, _) = pricing
        .call(
            Method::PUT,
            "/settings",
            Some(json!({"default_timing": "advance", "default_gl": "4000",
                "default_tax_category": "standard",
                "invoice_line_templates": {"recurring": "{sku}"}, "currencies": []})),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    // A flat monthly price: the seeded SKU has no unit, which a per-unit binding needs.
    let draft = pricing
        .priced_draft_with(sku, "flat", json!({"amount": "5"}))
        .await;
    let cpu_digest = pricing.definition_digest(cpu.0, cpu.1).await;
    let ram_digest = pricing.definition_digest(ram.0, ram.1).await;
    let rules = vec![
        PricingOverProducts::rule(1, cpu, &cpu_digest, draft.item, "10", VCPU_UNIT),
        PricingOverProducts::rule(2, ram, &ram_digest, draft.item, "20", GB_UNIT),
    ];
    let (status, saved, _) = pricing.put_rules(&draft, rules, "k1").await;
    assert_eq!(status, 200, "{saved}");
    pricing.set_quorum("plan_revision", 0).await;
    let (status, published) = pricing.submit(&draft).await;
    assert_eq!(status, 201, "{published}");
    assert_eq!(published["revision"]["state"], "published", "{published}");

    let today = time::OffsetDateTime::now_utc().date();
    let catalog = CatalogRef { tenant_id: tenant };
    let read = PricingReadProvider::new(pricing.state.clone(), pricing.enforcer.clone());
    let resolved = read
        .resolve(
            &pricing.user,
            ResolveQuery {
                catalog: catalog.clone(),
                revision_id: draft.revision,
                date: today,
                item_id: None,
                pins: vec![],
            },
        )
        .await
        .unwrap();
    let terms = resolved
        .quota_terms
        .as_ref()
        .expect("the preview's quota terms");
    assert_eq!(terms.rules.len(), 2);
    let selections = vec![BindingSelection {
        item_id: draft.item,
        dimension_value: None,
    }];
    let mut billing_terms = BillingTerms {
        schema_version: 1,
        cycle: BillingCycle::Month,
        anchor: BillingAnchor::Calendar,
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
    let query = NewSaleQuery {
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
        resolved_bindings_digest: selected_bindings_digest(&resolved, &selections).unwrap(),
        expected_quota_digest: Some(selected_quota_digest(terms, &selections)),
        selections,
        quantity: rust_decimal::Decimal::TWO,
        market: Market {
            currency: "EUR".into(),
            region: None,
        },
        start_at: today.midnight().assume_utc(),
        term: Term::Rolling,
        billing_terms,
        hold_policy_version: 1,
    };
    let service = Arc::new(
        bss_pricing::infra::commercial_terms::CommercialTermsService::new(
            pricing.state.clone(),
            pricing.enforcer.clone(),
            Arc::new(bss_pricing::infra::clock::WallClock),
            bss_pricing::config::SellerHoldPolicy::default(),
        ),
    );
    let receipt = SellabilityProvider::new(service.clone())
        .check(
            &pricing.user,
            query.clone(),
            CommandMeta {
                idempotency_key: "buy-two".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt.query, query);
    let quota = receipt.quota.as_ref().expect("the frozen quota terms");
    let frozen: Vec<(Uuid, &str, &str, String)> = quota
        .rules
        .iter()
        .map(|accepted| {
            let rule = &accepted.rule.rule;
            let digest =
                bss_pricing::infra::usage_policy_wire::digest_text(rule.definition_ref.digest);
            (
                rule.rule_id,
                accepted.limit.as_str(),
                rule.unit_ref.as_str(),
                digest,
            )
        })
        .collect();
    assert_eq!(
        frozen,
        [
            (Uuid::from_u128(1), "20", VCPU_UNIT, cpu_digest),
            (Uuid::from_u128(2), "40", GB_UNIT, ram_digest),
        ],
        "20 vCPU and 40 GB, each with the catalog's own definition digest"
    );
    assert_eq!(quota.original_quantity, rust_decimal::Decimal::TWO);
    assert_eq!(
        Some(quota.selected_quota_digest),
        query.expected_quota_digest
    );
    let read_back = PricingAcceptanceProvider::new(service)
        .acceptance(
            &pricing.user,
            AcceptanceQuery {
                catalog,
                acceptance_id: receipt.acceptance_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(read_back, receipt);
}
