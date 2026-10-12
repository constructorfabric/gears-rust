//! A usage SKU pins a derived usage type at its first publish (P-D-232), through the real doors over a
//! migrated database.
//!
//! Every case runs twice: with the usage-type catalog CONFIGURED (it resolves every GTS ref) and
//! UNCONFIGURED (it answers every ref `Unavailable`, as `gear.rs` installs it when nothing is wired).
//! The catalog of the doors under test counts its calls: a derived ref is this gear's own data, so
//! the catalog is never asked for one, at draft save, at submit or at approve.
//!
//! The derived types are seeded through the repository: with the catalog unconfigured, the derived
//! type doors refuse a write 503 (Run 2), since they resolve each input through the catalog.
//!
//! A SKU on a GTS ref is created and published through a second router over the same database, whose
//! catalog resolves every ref: with no catalog, a GTS usage SKU never publishes (P-D-184).
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::api::rest::{ApiState, dto::ProductsDerivedDeclaration};
use crate::domain::derived::{self as rules, NewDerivedType, NewDerivedVersion};
use crate::domain::recognized::UsageTypeAnswer;
use crate::infra::storage::repo::{self, derived_usage_type_repo as store};
use crate::test_support::{
    StubUsageTypes, TestDsn, authed_ctx, body_json, flat_in_enforcer, probe_binding, problem_code,
    raw_i64, repo_connection, resolved_usage_types, rest_app_on_db, test_db, violation_for,
};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Method, Request},
};
use bss_products_sdk::derived::DerivedUsageDeclaration;
use bss_products_sdk::usage_types::{UsageTypeCatalog, UsageTypePage};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use time::OffsetDateTime;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

const CLOUDLET_UNIT: &str = "cloudlet\u{b7}hour";
const AT_1: &str = "products.derived/cloudlets@1";
const AT_2: &str = "products.derived/cloudlets@2";
/// A GTS ref; the configured catalog resolves every ref.
const GTS: &str = "usage:storage";
const RAM_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1";
const CPU_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.cpu_mhz.v1";

/// A catalog that counts every call and answers as `inner` does.
struct Counting {
    inner: Arc<dyn UsageTypeCatalog>,
    asked: AtomicUsize,
}
impl Counting {
    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}
#[async_trait]
impl UsageTypeCatalog for Counting {
    async fn resolve(&self, ctx: &SecurityContext, usage_type_ref: &str) -> UsageTypeAnswer {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.inner.resolve(ctx, usage_type_ref).await
    }
    async fn list(
        &self,
        ctx: &SecurityContext,
        q: Option<&str>,
        kind: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.inner.list(ctx, q, kind, limit, cursor).await
    }
}

/// The two catalogs every case runs with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leg {
    Configured,
    Unconfigured,
}
const LEGS: [Leg; 2] = [Leg::Configured, Leg::Unconfigured];
impl Leg {
    fn catalog(self) -> Arc<dyn UsageTypeCatalog> {
        match self {
            Self::Configured => Arc::new(StubUsageTypes::always(UsageTypeAnswer::Resolved(
                probe_binding(),
            ))),
            Self::Unconfigured => Arc::new(crate::infra::usage_types::UnconfiguredUsageTypes),
        }
    }
    fn source(self) -> &'static str {
        match self {
            Self::Configured => "test",
            Self::Unconfigured => crate::gear::USAGE_TYPE_SOURCE_UNCONFIGURED,
        }
    }
}

fn routes(s: Arc<ApiState>, o: &dyn toolkit::api::OpenApiRegistry) -> Router {
    crate::api::rest::skus::router(s.clone(), o)
        .merge(crate::api::rest::sku_governance::router(s.clone(), o))
        .merge(crate::api::rest::approval_units::router(s.clone(), o))
        .merge(crate::api::rest::approval_policy::router(s, o))
}

fn share(name: &str, divisor: &str) -> Value {
    json!({"op":"ceil","arg":{"op":"div_const","arg":{"op":"input","name":name},"divisor":divisor}})
}
/// The cloudlet of decision 1 on the wire, its CPU share's divisor given.
fn cloudlet(cpu_divisor: &str) -> Value {
    json!({
        "output_unit": CLOUDLET_UNIT,
        "granularity": "hour",
        "inputs": [
            {"name":"ram_mb","usage_type_ref":RAM_REF,"granule_fold":"peak","unit":"MB"},
            {"name":"cpu_mhz","usage_type_ref":CPU_REF,"granule_fold":"peak","unit":"MHz"}
        ],
        "formula": {"op":"max","args":[share("ram_mb","128"), share("cpu_mhz",cpu_divisor)]},
        "output_scale": 0,
        "output_round": "half_even"
    })
}

/// A derived type `code` of `tenant` with one version per declaration, through the repository.
async fn seed(dsn: &str, tenant: Uuid, code: &str, declarations: &[Value]) {
    let (db, _) = repo_connection(dsn, tenant).await;
    let conn = db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant);
    let now = OffsetDateTime::now_utc();
    let t = store::create_type(
        &conn,
        &scope,
        tenant,
        NewDerivedType {
            code: code.to_owned(),
            name: format!("{code} name"),
        },
        Uuid::from_u128(7),
        now,
    )
    .await
    .unwrap();
    for (n, wire) in (1..).zip(declarations) {
        let dto: ProductsDerivedDeclaration = serde_json::from_value(wire.clone()).unwrap();
        let declaration = DerivedUsageDeclaration::try_from(&dto).unwrap();
        store::insert_version(
            &conn,
            &scope,
            tenant,
            NewDerivedVersion {
                type_id: t.id,
                version: n,
                declaration_json: serde_json::to_value(ProductsDerivedDeclaration::from(
                    &declaration,
                ))
                .unwrap(),
                digest: rules::digest_hex(&declaration),
                created_by: Uuid::from_u128(7),
                created_at: now,
            },
        )
        .await
        .unwrap();
    }
}

struct F {
    leg: Leg,
    /// The doors under test, with the leg's counting catalog.
    app: Router,
    catalog: Arc<Counting>,
    /// The same doors over the same database with a catalog that resolves every ref: the GTS
    /// SKUs' setup. Held for the test's life: its outbox serves both routers.
    setup: Router,
    /// Held for the test's life: its temporary directory holds the database.
    dsn: TestDsn,
    /// The doors' event sink, so a direct `validate_change` can build a subject.
    sink: crate::infra::broker::EventSink,
    tenant: Uuid,
    author: SecurityContext,
    reviewer: SecurityContext,
}

impl F {
    /// The tenant holds `cloudlets` with versions 1 and 2, both selling `cloudlet·hour`; another
    /// tenant holds `foreign` with version 1.
    async fn new(leg: Leg) -> Self {
        let (db, _, _, dsn) = test_db().await;
        let tenant = Uuid::new_v4();
        let (setup, state) =
            rest_app_on_db(tenant, routes, resolved_usage_types(), "test", db).await;
        let (db, _) = repo_connection(&dsn, tenant).await;
        let catalog = Arc::new(Counting {
            inner: leg.catalog(),
            asked: AtomicUsize::new(0),
        });
        let leg_state = Arc::new(ApiState {
            db,
            sink: state.sink.clone(),
            usage_type_catalog: catalog.clone(),
            usage_type_catalog_source: leg.source(),
            idempotency_retention_hours: 24,
            fence_ttl_minutes: 30,
            reference_principals: state.reference_principals.clone(),
            hub: state.hub.clone(),
            actor_names: state.actor_names.clone(),
        });
        let app = routes(leg_state, &toolkit::api::OpenApiRegistryImpl::new())
            .layer(axum::Extension(flat_in_enforcer(tenant)));
        seed(
            &dsn,
            tenant,
            "cloudlets",
            &[cloudlet("400"), cloudlet("500")],
        )
        .await;
        seed(&dsn, Uuid::new_v4(), "foreign", &[cloudlet("400")]).await;
        Self {
            leg,
            app,
            catalog,
            setup,
            dsn,
            sink: state.sink.clone(),
            tenant,
            author: authed_ctx(tenant),
            reviewer: authed_ctx(tenant),
        }
    }

    async fn call(
        &self,
        app: &Router,
        ctx: &SecurityContext,
        method: Method,
        path: &str,
        body: Value,
        if_match: Option<&str>,
    ) -> (u16, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("/bss-products/v1{path}"))
            .extension(ctx.clone())
            .header("Content-Type", "application/json");
        if let Some(tag) = if_match {
            request = request.header("If-Match", tag);
        }
        let r = app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        (r.status().as_u16(), body_json(r).await)
    }

    /// P-D-205: the policy is written at the tag its read answered.
    async fn policy(&self, quorum: u32) {
        let r = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/bss-products/v1/approval-policy")
                    .extension(self.author.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let tag = r.headers()["etag"].to_str().unwrap().to_owned();
        let (status, b) = self
            .call(
                &self.app,
                &self.author,
                Method::PUT,
                "/approval-policy",
                json!({ "quorum": quorum }),
                Some(&tag),
            )
            .await;
        assert_eq!(status, 200, "{b}");
    }

    /// `POST /skus` through `app` as the author: a usage SKU on `reference` with `unit`.
    async fn create(
        &self,
        app: &Router,
        code: &str,
        reference: Option<&str>,
        unit: Option<&str>,
    ) -> (u16, Value) {
        let body = json!({
            "code": code, "name": code, "type": "usage",
            "usage_type_ref": reference, "unit": unit,
        });
        self.call(app, &self.author, Method::POST, "/skus", body, None)
            .await
    }

    /// A legacy raw usage SKU, written through the repository: the doors no longer accept a raw
    /// ref (P-D-259). `published` sets the lifecycle without a version snapshot.
    async fn legacy_raw(&self, code: &str, reference: &str, unit: &str, published: bool) -> Uuid {
        use crate::domain::sku::NewSku;
        use bss_products_sdk::models::{Lifecycle, SkuType};
        let (db, scope) = repo_connection(&self.dsn, self.tenant).await;
        let conn = db.conn().unwrap();
        let now = OffsetDateTime::now_utc();
        let sku = repo::insert_sku(
            &conn,
            &scope,
            self.tenant,
            NewSku {
                code: code.to_owned(),
                name: code.to_owned(),
                r#type: SkuType::Usage,
                category_id: None,
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: None,
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: Some(reference.to_owned()),
                unit: Some(unit.to_owned()),
            },
            self.author.subject_id(),
            now,
        )
        .await
        .unwrap();
        if published {
            repo::set_lifecycle(
                &conn,
                &scope,
                self.tenant,
                sku.id,
                &[Lifecycle::Draft],
                Lifecycle::Published,
                now,
            )
            .await
            .unwrap();
        }
        sku.id
    }

    /// [`F::create`] that must succeed; the SKU's id.
    async fn draft(&self, app: &Router, code: &str, reference: &str, unit: &str) -> Uuid {
        let (status, s) = self.create(app, code, Some(reference), Some(unit)).await;
        assert_eq!(status, 201, "{:?}: {s}", self.leg);
        Uuid::parse_str(s["id"].as_str().unwrap()).unwrap()
    }

    /// `PATCH /skus/{id}` through the doors under test, at the draft's current revision.
    async fn patch(&self, id: Uuid, body: Value) -> (u16, Value) {
        let revision = self.card(id).await["revision"].as_i64().unwrap();
        self.call(
            &self.app,
            &self.author,
            Method::PATCH,
            &format!("/skus/{id}"),
            body,
            Some(&format!("\"{revision}\"")),
        )
        .await
    }

    /// `POST /skus/{id}{suffix}` through `app` as the author.
    async fn post(&self, app: &Router, id: Uuid, suffix: &str, body: Value) -> (u16, Value) {
        self.call(
            app,
            &self.author,
            Method::POST,
            &format!("/skus/{id}{suffix}"),
            body,
            None,
        )
        .await
    }

    /// Submit the draft through `app` at quorum 0: the submit is the publish.
    async fn publish(&self, app: &Router, id: Uuid) {
        self.policy(0).await;
        let (status, b) = self.post(app, id, "/submit", json!({})).await;
        assert_eq!(status, 200, "{:?}: {b}", self.leg);
        assert_eq!(b["applied"], true, "{b}");
    }

    /// The reviewer's approve of `unit` at `generation`, through the doors under test.
    async fn approve(&self, unit: &Value, generation: i32) -> (u16, Value) {
        self.call(
            &self.app,
            &self.reviewer,
            Method::POST,
            &format!(
                "/approval-units/{}/approve",
                unit["unit"]["id"].as_str().unwrap()
            ),
            json!({ "generation": generation }),
            None,
        )
        .await
    }

    async fn card(&self, id: Uuid) -> Value {
        let (status, b) = self
            .call(
                &self.setup,
                &self.author,
                Method::GET,
                &format!("/skus/{id}"),
                json!({}),
                None,
            )
            .await;
        assert_eq!(status, 200, "{b}");
        b["sku"].clone()
    }

    async fn skus(&self) -> i64 {
        raw_i64(&self.dsn, "SELECT COUNT(*) AS v FROM products_sku").await
    }

    async fn units(&self) -> i64 {
        raw_i64(
            &self.dsn,
            "SELECT COUNT(*) AS v FROM products_approval_unit",
        )
        .await
    }

    /// A concurrent writer's content change on the head, outside every door.
    async fn drift(&self, id: Uuid, reference: &str, unit: &str) {
        let (db, scope) = repo_connection(&self.dsn, self.tenant).await;
        let conn = db.conn().unwrap();
        let mut c = bss_products_sdk::models::SkuContent::from(
            &repo::find_sku(&conn, &scope, self.tenant, id)
                .await
                .unwrap()
                .unwrap(),
        );
        c.usage_type_ref = Some(reference.to_owned());
        c.unit = Some(unit.to_owned());
        repo::write_sku_content(
            &conn,
            &scope,
            self.tenant,
            id,
            &c,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    }

    fn assert_never_asked(&self) {
        assert_eq!(
            self.catalog.asked(),
            0,
            "{:?}: the catalog is never asked for a derived ref",
            self.leg
        );
    }

    /// Apply's own judgement of `body`: `SkuChange::validate_change` on the head, in a transaction.
    ///
    /// A stored version cannot stop wrapping between submit and apply, because versions are
    /// append-only (P-D-231). This call is that judgement. The door's 400 never records a unit, so
    /// an HTTP apply of a refused change does not exist.
    async fn at_apply(&self, id: Uuid, body: Value) -> Result<(), bss_approval::ApprovalError> {
        use crate::domain::approvals::change::SkuChange;
        use crate::domain::approvals::{SkuProposal, publish::SkuPublish};
        use crate::domain::recognized::UsageRefAnswer;
        use crate::domain::sku::{SkuPatch, apply_patch};
        use crate::infra::events::TxOutbox;
        use bss_approval::ItemRef;
        use bss_products_sdk::models::SkuContent;

        let request: crate::api::rest::dto::SkuChangeRequest =
            serde_json::from_value(body).unwrap();
        let patch = SkuPatch::try_from(request.patch).unwrap();
        let (db, scope) = repo_connection(&self.dsn, self.tenant).await;
        let conn = db.conn().unwrap();
        let sku = repo::find_sku(&conn, &scope, self.tenant, id)
            .await
            .unwrap()
            .unwrap();
        let content = SkuContent::from(&sku);
        let proposed = apply_patch(&content, &patch);
        let usage_type = proposed
            .usage_type_ref
            .as_deref()
            .filter(|reference| rules::is_derived_ref(reference))
            .map(|meter| {
                UsageRefAnswer::Derived(rules::DerivedPin {
                    meter: meter.to_owned(),
                    output_unit: proposed.unit.clone().unwrap_or_default(),
                })
            });
        let change = SkuChange {
            base: SkuPublish {
                scope: AccessScope::for_tenant(self.tenant),
                tenant_id: self.tenant,
                outbox: TxOutbox::new(self.sink.clone()),
                actor: sku.created_by,
                now: OffsetDateTime::now_utc(),
                usage_type,
            },
            patch,
            effective_from: OffsetDateTime::now_utc().date(),
            fence_op_id: None,
        };
        let item = ItemRef {
            item_type: "sku".into(),
            item_id: id,
            created_by: sku.created_by,
            before: Some(
                serde_json::to_value(&SkuProposal {
                    content: content.clone(),
                    lifecycle: None,
                })
                .unwrap(),
            ),
            after: serde_json::to_value(&SkuProposal {
                content: proposed,
                lifecycle: None,
            })
            .unwrap(),
        };
        db.transaction(|tx| {
            let change = change.clone();
            let item = item.clone();
            Box::pin(async move { Ok(change.validate_change(tx, &[item]).await) })
        })
        .await
        .unwrap()
    }
}

/// The 400's code and the subject its violation names.
fn refused(status: u16, body: &Value, code: &str, subject: &str, leg: Leg) {
    assert_eq!(status, 400, "{leg:?}: {body}");
    assert_eq!(problem_code(body), code, "{leg:?}: {body}");
    assert!(
        violation_for(body, subject).is_some(),
        "{leg:?}: the violation names {subject}: {body}"
    );
}

/// A draft on `products.derived/cloudlets@1` selling its output unit is created, submitted and
/// approved: the SKU is published on that version. The catalog is asked nothing on the way.
#[tokio::test]
async fn a_usage_sku_on_a_derived_version_is_created_submitted_and_approved_without_the_catalog() {
    for leg in LEGS {
        let f = F::new(leg).await;
        f.policy(1).await;
        let id = f.draft(&f.app, "CL", AT_1, CLOUDLET_UNIT).await;
        let (status, unit) = f.post(&f.app, id, "/submit", json!({})).await;
        assert_eq!(status, 200, "{leg:?}: {unit}");
        assert_eq!(unit["applied"], false, "{unit}");
        let (status, b) = f.approve(&unit, 1).await;
        assert_eq!(status, 200, "{leg:?}: {b}");
        let s = f.card(id).await;
        assert_eq!(s["lifecycle"], "published", "{s}");
        assert_eq!(s["usage_type_ref"], AT_1);
        assert_eq!(s["unit"], CLOUDLET_UNIT);
        assert_eq!(s["published_version"], 1);
        f.assert_never_asked();
    }
}

/// A derived ref the tenant does not hold is 400 `DERIVED_USAGE_TYPE_UNKNOWN` at draft save, with
/// the catalog unconfigured too: an unknown version, an unknown code, another tenant's type, a
/// version that is not canonical and a ref with no version. A draft PATCH is judged the same way.
#[tokio::test]
async fn an_unknown_derived_version_is_refused_at_draft_save() {
    for leg in LEGS {
        let f = F::new(leg).await;
        for reference in [
            "products.derived/cloudlets@3",
            "products.derived/nothing@1",
            "products.derived/foreign@1",
            "products.derived/cloudlets@01",
            "products.derived/cloudlets@0",
            "products.derived/cloudlets",
            "products.derived/",
        ] {
            let (status, b) = f
                .create(&f.app, "U", Some(reference), Some(CLOUDLET_UNIT))
                .await;
            refused(
                status,
                &b,
                "DERIVED_USAGE_TYPE_UNKNOWN",
                "usage_type_ref",
                leg,
            );
        }
        assert_eq!(f.skus().await, 0, "{leg:?}: no draft was written");
        let (status, s) = f.create(&f.app, "P", None, None).await;
        assert_eq!(status, 201, "{leg:?}: {s}");
        let id = Uuid::parse_str(s["id"].as_str().unwrap()).unwrap();
        let (status, b) = f
            .patch(
                id,
                json!({"usage_type_ref":"products.derived/cloudlets@3","unit":CLOUDLET_UNIT}),
            )
            .await;
        refused(
            status,
            &b,
            "DERIVED_USAGE_TYPE_UNKNOWN",
            "usage_type_ref",
            leg,
        );
        assert!(f.card(id).await["usage_type_ref"].is_null());
        f.assert_never_asked();
    }
}

/// A unit other than the version's output unit is 400 `DERIVED_UNIT_MISMATCH`, at the create and
/// at a draft PATCH. A draft may omit its unit: the publish serves the version's output unit, and
/// the row stores none (P-D-259). A legacy raw draft cannot move onto that version with another unit.
#[tokio::test]
async fn a_unit_other_than_the_output_unit_is_refused() {
    for leg in LEGS {
        let f = F::new(leg).await;
        let (status, b) = f.create(&f.app, "M", Some(AT_1), Some("GB")).await;
        refused(status, &b, "DERIVED_UNIT_MISMATCH", "unit", leg);
        assert_eq!(f.skus().await, 0);
        let (status, s) = f.create(&f.app, "M", Some(AT_1), None).await;
        assert_eq!(status, 201, "{leg:?}: a draft omits its unit: {s}");
        let id = Uuid::parse_str(s["id"].as_str().unwrap()).unwrap();
        assert_eq!(s["unit"], CLOUDLET_UNIT, "{s}");
        let (status, b) = f.patch(id, json!({"unit":"GB"})).await;
        refused(status, &b, "DERIVED_UNIT_MISMATCH", "unit", leg);
        f.publish(&f.app, id).await;
        let card = f.card(id).await;
        assert_eq!(card["lifecycle"], "published");
        assert_eq!(card["unit"], CLOUDLET_UNIT);
        assert_eq!(
            raw_i64(
                &f.dsn,
                "SELECT COUNT(*) AS v FROM products_sku WHERE unit IS NULL"
            )
            .await,
            1,
            "a derived SKU stores no unit"
        );
        let gts = f.legacy_raw("G", GTS, "GB", false).await;
        let (status, b) = f.patch(gts, json!({ "usage_type_ref": AT_1 })).await;
        refused(status, &b, "DERIVED_UNIT_MISMATCH", "unit", leg);
        f.assert_never_asked();
    }
}

/// A draft that was never published may move its pin: `@1` → `@2` is a 200, and the SKU publishes
/// on `@2`.
#[tokio::test]
async fn a_draft_moves_its_derived_pin_until_its_first_publish() {
    for leg in LEGS {
        let f = F::new(leg).await;
        let id = f.draft(&f.app, "D", AT_1, CLOUDLET_UNIT).await;
        let (status, b) = f.patch(id, json!({ "usage_type_ref": AT_2 })).await;
        assert_eq!(status, 200, "{leg:?}: {b}");
        assert_eq!(b["usage_type_ref"], AT_2, "{b}");
        f.publish(&f.app, id).await;
        let s = f.card(id).await;
        assert_eq!(s["lifecycle"], "published");
        assert_eq!(s["usage_type_ref"], AT_2);
        f.assert_never_asked();
    }
}

/// After its first publish a usage SKU keeps its metering: a change to `@2`, to a GTS ref, a unit
/// change, or one that drops the ref (with a type change, or alone) is 400 `METERING_IMMUTABLE`
/// at submit; no unit is recorded and no fence is left. A change that leaves the ref and the unit
/// alone still applies.
#[tokio::test]
async fn after_its_first_publish_a_usage_sku_keeps_its_derived_pin() {
    for leg in LEGS {
        let f = F::new(leg).await;
        let id = f.draft(&f.app, "K", AT_1, CLOUDLET_UNIT).await;
        f.publish(&f.app, id).await;
        let units = f.units().await;
        for patch in [
            json!({ "usage_type_ref": AT_2 }),
            json!({ "usage_type_ref": GTS, "unit": "GB" }),
            json!({ "type": "recurring", "usage_type_ref": null, "unit": null }),
            json!({ "usage_type_ref": null }),
        ] {
            let (status, b) = f.post(&f.app, id, "/changes", patch.clone()).await;
            refused(status, &b, "METERING_IMMUTABLE", "usage_type_ref", leg);
            let s = f.card(id).await;
            assert_eq!(s["usage_type_ref"], AT_1, "{patch}");
            assert_eq!(s["type"], "usage", "{patch}");
            assert_eq!(s["type_change_pending"], false, "{patch}: no fence is left");
            assert!(s["pending_unit_id"].is_null(), "{patch}");
            assert_eq!(s["published_version"], 1, "{patch}");
        }
        let (status, b) = f.post(&f.app, id, "/changes", json!({"unit":"GB"})).await;
        refused(status, &b, "METERING_IMMUTABLE", "unit", leg);
        assert_eq!(f.card(id).await["unit"], CLOUDLET_UNIT);
        assert_eq!(f.units().await, units, "{leg:?}: no unit was recorded");
        let (status, b) = f
            .post(&f.app, id, "/changes", json!({"gl_code":"4012"}))
            .await;
        assert_eq!(status, 200, "{leg:?}: {b}");
        let s = f.card(id).await;
        assert_eq!(s["published_version"], 2);
        assert_eq!(s["usage_type_ref"], AT_1);
        f.assert_never_asked();
    }
}

/// A published GTS usage SKU cannot take a derived pin, and cannot move to another GTS ref:
/// both are 400 `METERING_IMMUTABLE` on `usage_type_ref` (P-D-258).
#[tokio::test]
async fn a_published_gts_usage_sku_cannot_take_a_derived_pin() {
    for leg in LEGS {
        let f = F::new(leg).await;
        let id = f.legacy_raw("G", GTS, "GB", true).await;
        let (status, b) = f
            .post(
                &f.app,
                id,
                "/changes",
                json!({ "usage_type_ref": AT_1, "unit": CLOUDLET_UNIT }),
            )
            .await;
        refused(status, &b, "METERING_IMMUTABLE", "usage_type_ref", leg);
        let s = f.card(id).await;
        assert_eq!(s["usage_type_ref"], GTS);
        assert_eq!(s["published_version"], 0);
        f.assert_never_asked();
        let (status, b) = f
            .post(
                &f.app,
                id,
                "/changes",
                json!({"usage_type_ref":"usage:other"}),
            )
            .await;
        refused(status, &b, "METERING_IMMUTABLE", "usage_type_ref", leg);
        assert_eq!(f.card(id).await["usage_type_ref"], GTS);
        f.assert_never_asked();
    }
}

/// A change that was legal when submitted is judged again at apply. The change is the identity
/// wrap, so its `after` names the wrapper. A concurrent writer then moves the head onto another
/// raw meter, keeping the unit. The rebuilt patch still sets the wrapper, so the proposal's
/// `after` is unchanged and the approve is not refreshed: it applies, and the apply refuses it,
/// 409 `METERING_IMMUTABLE`, the head as the writer left it and the unit still pending. The
/// catalog is asked nothing: the proposal's ref is derived.
#[tokio::test]
async fn a_stale_change_is_refused_at_apply_when_a_concurrent_write_pinned_a_derived_type() {
    const RAW: &str = "usage:storage";
    const GB: &str = "GB";
    const WRAP: &str = "products.derived/wrap@1";
    for leg in LEGS {
        let f = F::new(leg).await;
        seed(
            &f.dsn,
            f.tenant,
            "wrap",
            &[declaration(
                GB,
                &[named_input("disk", RAW, GB)],
                &identity_formula(),
            )],
        )
        .await;
        let id = f.legacy_raw("S", RAW, GB, true).await;
        f.policy(1).await;
        let (status, unit) = f
            .post(&f.app, id, "/changes", json!({"usage_type_ref": WRAP}))
            .await;
        assert_eq!(status, 200, "{leg:?}: {unit}");
        assert_eq!(unit["applied"], false, "{unit}");
        f.drift(id, "usage:other", GB).await;
        let left = f.card(id).await;
        let (status, b) = f.approve(&unit, 1).await;
        assert_eq!(status, 409, "{leg:?}: {b}");
        assert_eq!(problem_code(&b), "METERING_IMMUTABLE", "{leg:?}: {b}");
        let s = f.card(id).await;
        assert_eq!(s, left, "the head as the writer left it");
        assert_eq!(s["usage_type_ref"], "usage:other", "{s}");
        assert_eq!(s["unit"], GB, "{s}");
        assert_eq!(s["pending_unit_id"], unit["unit"]["id"], "still pending");
        f.assert_never_asked();
    }
}

fn named_input(name: &str, reference: &str, unit: &str) -> Value {
    json!({
        "name": name,
        "usage_type_ref": reference,
        "granule_fold": "sum",
        "unit": unit
    })
}
fn declaration(output: &str, inputs: &[Value], formula: &Value) -> Value {
    json!({
        "output_unit": output,
        "granularity": "hour",
        "inputs": inputs,
        "formula": formula,
        "output_scale": 0,
        "output_round": "half_even"
    })
}
fn identity_formula() -> Value {
    json!({"op": "input", "name": "disk"})
}

/// A published raw meter may move onto the one-input identity wrapper of that meter, in the same
/// unit (P-D-251). Every other move stays `METERING_IMMUTABLE` at the change door and in
/// `validate_change`.
///
/// A version cannot stop wrapping between submit and apply: versions are append-only (P-D-231).
/// Apply's own judgement is the direct `validate_change` call, not an HTTP apply of a change the
/// door already refused.
#[tokio::test]
async fn a_published_raw_usage_sku_moves_onto_the_identity_wrapper_of_its_meter() {
    const RAW: &str = "usage:storage";
    const GB: &str = "GB";
    const WRAP: &str = "products.derived/wrap@1";
    const CEIL_WRAP: &str = "products.derived/ceil@1";
    const MISMATCH: &str = "products.derived/mismatch@1";
    const TWO: &str = "products.derived/two@1";
    const MOVED: &str = "products.derived/moved@1";
    const OTHER: &str = "products.derived/other@1";
    let identity = identity_formula();
    let ceil = json!({"op":"ceil","arg":{"op":"input","name":"disk"}});
    for leg in LEGS {
        let f = F::new(leg).await;
        seed(
            &f.dsn,
            f.tenant,
            "wrap",
            &[declaration(GB, &[named_input("disk", RAW, GB)], &identity)],
        )
        .await;
        seed(
            &f.dsn,
            f.tenant,
            "ceil",
            &[declaration(GB, &[named_input("disk", RAW, GB)], &ceil)],
        )
        .await;
        seed(
            &f.dsn,
            f.tenant,
            "mismatch",
            &[declaration(
                GB,
                &[named_input("disk", RAW, "MB")],
                &identity,
            )],
        )
        .await;
        seed(
            &f.dsn,
            f.tenant,
            "two",
            &[declaration(
                GB,
                &[
                    named_input("disk", RAW, GB),
                    named_input("extra", "usage:other", GB),
                ],
                &identity,
            )],
        )
        .await;
        seed(
            &f.dsn,
            f.tenant,
            "moved",
            &[declaration(
                "MB",
                &[named_input("disk", RAW, "MB")],
                &identity,
            )],
        )
        .await;
        seed(
            &f.dsn,
            f.tenant,
            "other",
            &[declaration(
                GB,
                &[named_input("disk", "usage:other", GB)],
                &identity,
            )],
        )
        .await;

        let pinned = f.draft(&f.app, "P", AT_1, CLOUDLET_UNIT).await;
        f.publish(&f.app, pinned).await;
        for body in [
            json!({"usage_type_ref": RAW, "unit": GB}),
            json!({"usage_type_ref": AT_2}),
            json!({"usage_type_ref": null}),
            json!({"type": "recurring", "usage_type_ref": null, "unit": null}),
        ] {
            let (status, b) = f.post(&f.app, pinned, "/changes", body.clone()).await;
            refused(status, &b, "METERING_IMMUTABLE", "usage_type_ref", leg);
            match f.at_apply(pinned, body).await {
                Err(bss_approval::ApprovalError::InvalidSubmit { code, field, .. }) => {
                    assert_eq!(code, "METERING_IMMUTABLE", "{leg:?}");
                    assert_eq!(field, "usage_type_ref");
                }
                other => panic!("{leg:?}: apply judged {other:?}"),
            }
            assert_eq!(f.card(pinned).await["usage_type_ref"], AT_1);
        }

        let id = f.legacy_raw("R", RAW, GB, true).await;
        for body in [
            json!({"usage_type_ref": TWO}),
            json!({"usage_type_ref": CEIL_WRAP}),
            json!({"usage_type_ref": MISMATCH}),
            json!({"usage_type_ref": MOVED, "unit": "MB"}),
            json!({"usage_type_ref": OTHER}),
            json!({"usage_type_ref": "products.derived/missing@1"}),
        ] {
            let (status, b) = f.post(&f.app, id, "/changes", body.clone()).await;
            refused(status, &b, "METERING_IMMUTABLE", "usage_type_ref", leg);
            match f.at_apply(id, body).await {
                Err(bss_approval::ApprovalError::InvalidSubmit { code, field, .. }) => {
                    assert_eq!(code, "METERING_IMMUTABLE", "{leg:?}");
                    assert_eq!(field, "usage_type_ref");
                }
                other => panic!("{leg:?}: apply judged {other:?}"),
            }
            let card = f.card(id).await;
            assert_eq!(card["usage_type_ref"], RAW);
            assert_eq!(card["unit"], GB);
        }

        let wrap = json!({"usage_type_ref": WRAP});
        let judged = f.at_apply(id, wrap.clone()).await;
        assert!(
            judged.is_ok(),
            "{leg:?}: apply allows the identity wrapper: {judged:?}"
        );
        let (status, b) = f.post(&f.app, id, "/changes", wrap).await;
        assert_eq!(status, 200, "{leg:?}: the wrap is a change: {b}");
        assert_eq!(b["applied"], true, "{b}");
        let card = f.card(id).await;
        assert_eq!(card["usage_type_ref"], WRAP);
        assert_eq!(card["unit"], GB);
        assert_eq!(card["published_version"], 1);
        f.assert_never_asked();
    }
}

/// P-D-258: a published usage SKU, raw or derived, keeps its ref and its unit. A ref change is
/// 400 `METERING_IMMUTABLE` on `usage_type_ref`; a unit change is the same code on `unit`; a type
/// change away from usage is refused. The identity wrap still applies. A draft still edits both.
/// Apply's own `validate_change` refuses a raw ref change that carries the stored unit.
#[tokio::test]
async fn a_published_usage_sku_keeps_its_metering() {
    const RAW: &str = "usage:storage";
    const OTHER: &str = "usage:other";
    const GB: &str = "GB";
    const WRAP: &str = "products.derived/wrap@1";
    for leg in LEGS {
        let f = F::new(leg).await;
        seed(
            &f.dsn,
            f.tenant,
            "wrap",
            &[declaration(
                GB,
                &[named_input("disk", RAW, GB)],
                &identity_formula(),
            )],
        )
        .await;

        let raw = f.legacy_raw("RAW", RAW, GB, true).await;
        let (status, b) = f
            .post(&f.app, raw, "/changes", json!({"usage_type_ref": OTHER}))
            .await;
        refused(status, &b, "METERING_IMMUTABLE", "usage_type_ref", leg);
        assert_eq!(f.card(raw).await["usage_type_ref"], RAW);
        let (status, b) = f.post(&f.app, raw, "/changes", json!({"unit": "MB"})).await;
        refused(status, &b, "METERING_IMMUTABLE", "unit", leg);
        assert_eq!(f.card(raw).await["unit"], GB);
        let (status, b) = f
            .post(
                &f.app,
                raw,
                "/changes",
                json!({"type": "recurring", "usage_type_ref": null, "unit": null}),
            )
            .await;
        refused(status, &b, "METERING_IMMUTABLE", "usage_type_ref", leg);
        assert_eq!(f.card(raw).await["type"], "usage");
        match f
            .at_apply(raw, json!({"usage_type_ref": OTHER, "unit": GB}))
            .await
        {
            Err(bss_approval::ApprovalError::InvalidSubmit { code, field, .. }) => {
                assert_eq!(code, "METERING_IMMUTABLE", "{leg:?}");
                assert_eq!(field, "usage_type_ref");
            }
            other => panic!("{leg:?}: apply judged a raw ref change {other:?}"),
        }

        let derived = f.draft(&f.app, "DRV", AT_1, CLOUDLET_UNIT).await;
        f.publish(&f.app, derived).await;
        let (status, b) = f
            .post(&f.app, derived, "/changes", json!({"usage_type_ref": AT_2}))
            .await;
        refused(status, &b, "METERING_IMMUTABLE", "usage_type_ref", leg);
        let (status, b) = f
            .post(&f.app, derived, "/changes", json!({"unit": "GB"}))
            .await;
        refused(status, &b, "METERING_IMMUTABLE", "unit", leg);
        assert_eq!(f.card(derived).await["usage_type_ref"], AT_1);
        assert_eq!(f.card(derived).await["unit"], CLOUDLET_UNIT);

        assert_eq!(
            f.catalog.asked(),
            0,
            "{leg:?}: a published metering change asks no catalog"
        );
        let draft = f.legacy_raw("DFT", RAW, GB, false).await;
        let (status, b) = f
            .patch(draft, json!({"usage_type_ref": WRAP, "unit": GB}))
            .await;
        assert_eq!(
            status, 200,
            "{leg:?}: a raw draft moves onto a derived ref: {b}"
        );
        assert_eq!(b["usage_type_ref"], WRAP);
        assert_eq!(b["unit"], GB);

        let moving = f.legacy_raw("WRP", RAW, GB, true).await;
        let (status, b) = f
            .post(&f.app, moving, "/changes", json!({"usage_type_ref": WRAP}))
            .await;
        assert_eq!(status, 200, "{leg:?}: the wrap still applies: {b}");
        assert_eq!(b["applied"], true, "{b}");
        let card = f.card(moving).await;
        assert_eq!(card["usage_type_ref"], WRAP);
        assert_eq!(card["unit"], GB);
        f.assert_never_asked();
    }
}
