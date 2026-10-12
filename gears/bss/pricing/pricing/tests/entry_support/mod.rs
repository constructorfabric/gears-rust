//! Shared real REST and database fixture for reference execution.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "../policy_support/mod.rs"]
pub mod policy_support;
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;
#[path = "../storage_support/mod.rs"]
mod storage_support;
pub use storage_support::TestDsn;
/// A migrated file-backed database: provider, tenant scope, tenant and its [`TestDsn`], which the
/// caller holds for the test's life.
pub async fn test_db() -> (
    toolkit_db::DBProvider<toolkit_db::DbError>,
    toolkit_db::secure::AccessScope,
    Uuid,
    TestDsn,
) {
    storage_support::test_db().await
}
struct Resolver {
    tenant: Uuid,
    allow: bool,
    /// Grant every subject everything: a service principal holds its grants by policy, which the
    /// grant-in-subject-type encoding below cannot spell for a `*.system` subject.
    every_subject: bool,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Resolver {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        use authz_resolver_sdk::*;
        Ok(EvaluationResponse {
            decision: self.allow
                && (self.every_subject
                    || request
                        .subject
                        .subject_type
                        .as_deref()
                        .is_some_and(|grant| {
                            grant == "user"
                                || grant
                                    == format!(
                                        "{}:{}",
                                        request
                                            .resource
                                            .resource_type
                                            .trim_start_matches("gts.cf.bss.pricing.")
                                            .trim_end_matches(".v1~"),
                                        request.action.name
                                    )
                        })),
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        toolkit_security::pep_properties::OWNER_TENANT_ID,
                        vec![self.tenant],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}
/// Authoring state over any database, with the scripted registry in its hub.
pub async fn state_on(
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
    registry: Arc<dyn bss_products_sdk::ReferenceRegistryV1>,
) -> Arc<bss_pricing::api::rest::authoring::AuthoringState> {
    state_with_clock(
        db,
        registry,
        Arc::new(bss_pricing::infra::reference_work::WallClock),
    )
    .await
}
/// [`state_on`] whose approval doors read `clock`.
pub async fn state_with_clock(
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
    registry: Arc<dyn bss_products_sdk::ReferenceRegistryV1>,
    clock: Arc<dyn bss_pricing::infra::reference_work::Clock>,
) -> Arc<bss_pricing::api::rest::authoring::AuthoringState> {
    let hub = Arc::new(toolkit::ClientHub::default());
    hub.register::<dyn bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1>(Arc::new(
        policy_support::MeterProvider::default(),
    ));
    hub.register::<bss_products_sdk::PricingReferenceRegistry>(Arc::new(
        bss_products_sdk::PricingReferenceRegistry(registry),
    ));
    Arc::new(
        bss_pricing::api::rest::authoring::AuthoringState::new(db, hub)
            .await
            .unwrap()
            .with_clock(clock),
    )
}
/// The production router over a state, allowing every user of `tenant`.
pub fn app_for(
    state: Arc<bss_pricing::api::rest::authoring::AuthoringState>,
    tenant: Uuid,
) -> Router {
    production(state).layer(axum::Extension(authz_resolver_sdk::PolicyEnforcer::new(
        Arc::new(Resolver {
            tenant,
            allow: true,
            every_subject: false,
        }),
    )))
}
/// The production router over a state whose policy grants every subject of `tenant`
/// everything, a `*.system` service principal included.
pub fn app_granting_every_subject(
    state: Arc<bss_pricing::api::rest::authoring::AuthoringState>,
    tenant: Uuid,
) -> Router {
    production(state).layer(axum::Extension(authz_resolver_sdk::PolicyEnforcer::new(
        Arc::new(Resolver {
            tenant,
            allow: true,
            every_subject: true,
        }),
    )))
}
/// The gear's two routers as `module.rs` mounts them: authoring and the consumer reads.
pub fn production(state: Arc<bss_pricing::api::rest::authoring::AuthoringState>) -> Router {
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    bss_pricing::api::rest::authoring::router(state.clone(), &openapi).merge(
        bss_pricing::api::rest::read_contract::router(state, &openapi),
    )
}
/// A user principal of a tenant.
pub fn user_of(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap()
}
pub struct Fixture {
    /// The database's DSN; the fixture holds its temporary directory for the test's life.
    pub dsn: TestDsn,
    pub state: Arc<bss_pricing::api::rest::authoring::AuthoringState>,
    pub app: Router,
    pub denied: Router,
    pub ctx: SecurityContext,
    pub db: toolkit_db::DBProvider<toolkit_db::DbError>,
}
/// The policy the fixture's routers run under, for a caller outside the router (the SKU usage
/// port, D-428): every `user` of `tenant` holds every grant, another subject type holds the one
/// `label:action` it names.
pub fn enforcer_for(tenant: Uuid) -> authz_resolver_sdk::PolicyEnforcer {
    authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Resolver {
        tenant,
        allow: true,
        every_subject: false,
    }))
}
/// A migrated file-backed database whose every statement the returned recorder sees
/// (toolkit-db `QueryRecorder`): provider, recorder, tenant and DSN. The migrations' own
/// statements are cleared.
pub async fn recorded_db() -> (
    toolkit_db::DBProvider<toolkit_db::DbError>,
    toolkit_db::test_support::QueryRecorder,
    Uuid,
    TestDsn,
) {
    use toolkit::contracts::DatabaseCapability;
    let dsn = TestDsn::new("pricing-recorded-");
    let (db, recorder) = toolkit_db::test_support::connect_with_recorder(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
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
    recorder.clear();
    (
        toolkit_db::DBProvider::new(db),
        recorder,
        Uuid::new_v4(),
        dsn,
    )
}
impl Fixture {
    pub async fn new(registry: Arc<dyn bss_products_sdk::ReferenceRegistryV1>) -> Self {
        let (db, _, tenant, dsn) = storage_support::test_db().await;
        Self::on(db, tenant, dsn, registry).await
    }
    /// The fixture over a database the caller opened (a recorded one, [`recorded_db`]).
    pub async fn on(
        db: toolkit_db::DBProvider<toolkit_db::DbError>,
        tenant: Uuid,
        dsn: TestDsn,
        registry: Arc<dyn bss_products_sdk::ReferenceRegistryV1>,
    ) -> Self {
        Self::on_named(db, tenant, dsn, registry, None).await
    }
    /// [`Fixture::new`] whose reads name their actors through `directory` (D-519).
    pub async fn with_directory(
        registry: Arc<dyn bss_products_sdk::ReferenceRegistryV1>,
        directory: Arc<dyn bss_rest::actor_names::ActorDirectory>,
    ) -> Self {
        let (db, _, tenant, dsn) = storage_support::test_db().await;
        let names = bss_rest::actor_names::ActorNames::with_directory(
            directory,
            &bss_pricing::api::rest::authoring::SYSTEM_ACTORS,
        );
        Self::on_named(db, tenant, dsn, registry, Some(names)).await
    }
    /// [`Fixture::on`] with the actor names `names`, or the hub's when `None`.
    pub async fn on_named(
        db: toolkit_db::DBProvider<toolkit_db::DbError>,
        tenant: Uuid,
        dsn: TestDsn,
        registry: Arc<dyn bss_products_sdk::ReferenceRegistryV1>,
        names: Option<bss_rest::actor_names::ActorNames>,
    ) -> Self {
        let hub = Arc::new(toolkit::ClientHub::default());
        hub.register::<dyn bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1>(Arc::new(
            policy_support::MeterProvider::default(),
        ));
        hub.register::<bss_products_sdk::PricingReferenceRegistry>(Arc::new(
            bss_products_sdk::PricingReferenceRegistry(registry),
        ));
        let state = bss_pricing::api::rest::authoring::AuthoringState::new(db.clone(), hub)
            .await
            .unwrap();
        let state = Arc::new(match names {
            Some(names) => state.with_actor_names(names),
            None => state,
        });
        let make = |allow| {
            production(state.clone()).layer(axum::Extension(
                authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Resolver {
                    tenant,
                    allow,
                    every_subject: false,
                })),
            ))
        };
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .subject_type("user")
            .build()
            .unwrap();
        let (app, denied) = (make(true), make(false));
        Self {
            dsn,
            state,
            app,
            denied,
            ctx,
            db,
        }
    }
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        body: Value,
        tag: Option<&str>,
        key: Option<&str>,
    ) -> (u16, Value, String) {
        request(&self.app, &self.ctx, method, path, body, tag, key).await
    }
    /// Every approval unit `GET /approval-units?{query}` lists, as the fixture's user: the list
    /// pages (D-458), so a whole read follows `page_info.next_cursor` to its last page.
    pub async fn all_units(&self, query: &str) -> Vec<Value> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut path = format!("/approval-units?{query}");
            if let Some(cursor) = &cursor {
                // A cursor is base64url: it needs no escaping in a query.
                path.push_str(if query.is_empty() {
                    "cursor="
                } else {
                    "&cursor="
                });
                path.push_str(cursor);
            }
            let (s, b, _) = self.call("GET", &path, json!({}), None, None).await;
            assert_eq!(s, 200, "{path}: {b}");
            items.extend(b["items"].as_array().unwrap().iter().cloned());
            match b["page_info"]["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return items,
            }
        }
    }
    /// A second router over its own connection to the same database file.
    pub async fn second_app(&self) -> Router {
        let db = toolkit_db::connect_db(
            &self.dsn,
            toolkit_db::ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..toolkit_db::ConnectOpts::default()
            },
        )
        .await
        .unwrap();
        let state = Arc::new(
            bss_pricing::api::rest::authoring::AuthoringState::new(
                toolkit_db::DBProvider::new(db),
                self.state.hub.clone(),
            )
            .await
            .unwrap(),
        );
        production(state).layer(axum::Extension(authz_resolver_sdk::PolicyEnforcer::new(
            Arc::new(Resolver {
                tenant: self.ctx.subject_tenant_id(),
                allow: true,
                every_subject: false,
            }),
        )))
    }
    /// Call as another principal of the same tenant.
    pub async fn call_as(
        &self,
        ctx: &SecurityContext,
        method: &str,
        path: &str,
        body: Value,
        tag: Option<&str>,
        key: Option<&str>,
    ) -> (u16, Value, String) {
        request(&self.app, ctx, method, path, body, tag, key).await
    }
    /// Another user principal of the fixture tenant.
    pub fn user(&self) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(self.ctx.subject_tenant_id())
            .subject_type("user")
            .build()
            .unwrap()
    }
    pub async fn book(&self) -> (Value, String) {
        let (s, b, t) = self
            .call(
                "POST",
                "/price-books",
                json!({"code":"standard","name":"Standard","currency":"EUR"}),
                None,
                Some("create"),
            )
            .await;
        assert_eq!(s, 201, "{b}");
        (b, t)
    }
}
pub async fn request(
    app: &Router,
    ctx: &SecurityContext,
    method: &str,
    path: &str,
    body: Value,
    tag: Option<&str>,
    key: Option<&str>,
) -> (u16, Value, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/bss-pricing/v1{path}"))
        .extension(ctx.clone())
        .header("content-type", "application/json");
    if let Some(tag) = tag {
        req = req.header("if-match", tag);
    }
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let tag = response
        .headers()
        .get("etag")
        .map_or("", |v| v.to_str().unwrap())
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
        tag,
    )
}
/// [`request`] with a body sent as these bytes, the empty body included, which a JSON value
/// cannot spell: `(status, body)`.
pub async fn request_raw(
    app: &Router,
    ctx: &SecurityContext,
    method: &str,
    path: &str,
    body: &str,
    key: Option<&str>,
) -> (u16, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/bss-pricing/v1{path}"))
        .extension(ctx.clone())
        .header("content-type", "application/json");
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
    )
}
use bss_products_sdk::{
    ReferenceRegistryV1,
    models::{
        Lifecycle, ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuType, SkuVersion,
    },
};
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_canonical_errors::CanonicalError;
/// A dated SKU metering answer: `(effective_from, unit, usage_type_ref)`.
pub type Metering = (time::Date, Option<String>, Option<String>);
#[derive(Default)]
pub struct Script {
    pub reserve_calls: AtomicUsize,
    /// The reference kind of every reserve call, in call order.
    pub reserve_kinds: std::sync::Mutex<Vec<ReferenceKind>>,
    pub confirm_calls: AtomicUsize,
    pub releases: AtomicUsize,
    pub mode: AtomicUsize,
    pub parked: tokio::sync::Notify,
    pub resume: tokio::sync::Notify,
    pub actors: tokio::sync::Mutex<Vec<Uuid>>,
    pub refs: tokio::sync::Mutex<std::collections::BTreeMap<Uuid, (Uuid, ReferenceState)>>,
    /// Dated SKU metering `(effective_from, unit, usage_type_ref)`; empty answers `None`.
    pub versions: std::sync::Mutex<Vec<Metering>>,
    pub version_reads: AtomicUsize,
    /// When set, dated reads fail as an unavailable registry.
    pub versions_down: std::sync::atomic::AtomicBool,
    /// When set, Products refuses the dated read: the caller may not read SKUs (403).
    pub versions_refused: std::sync::atomic::AtomicBool,
    /// A tenant whose `states()` calls fail as an unavailable registry.
    pub states_down_for: std::sync::Mutex<Option<Uuid>>,
    /// Opt-in: when set, only these principals hold products `read`. Any other caller's SKU read
    /// (`sku_for_write`, `sku_version_as_of`) is Products' 403, as Products' registry authorizes
    /// the caller before it reads. Unset (the default) admits every caller.
    pub readers: std::sync::Mutex<Option<std::collections::BTreeSet<Uuid>>>,
    /// Opt-in: `sku_for_write` fails as an unavailable registry.
    pub skus_down: std::sync::atomic::AtomicBool,
    /// When set, the SKU's `usage_type_ref` replaces the fixture's `vm-hours`.
    pub usage_type_ref: std::sync::Mutex<Option<String>>,
}
impl Script {
    pub fn set(&self, mode: usize) {
        self.mode.store(mode, Ordering::SeqCst);
    }
    /// Only these principals may read SKUs from now on (products `read`).
    pub fn readers(&self, principals: impl IntoIterator<Item = Uuid>) {
        *self.readers.lock().unwrap() = Some(principals.into_iter().collect());
    }
    /// Products' 403 for a caller without products `read`, when the opt-in set is armed.
    fn read_denied(&self, ctx: &SecurityContext) -> Option<CanonicalError> {
        let readers = self.readers.lock().unwrap();
        let denied = readers
            .as_ref()
            .is_some_and(|set| !set.contains(&ctx.subject_id()));
        denied.then(|| {
            TestResource::permission_denied()
                .with_reason("SKU_READ_DENIED")
                .create()
        })
    }
    pub fn count(value: &AtomicUsize) -> usize {
        value.load(Ordering::SeqCst)
    }
    async fn park(&self) {
        self.parked.notify_one();
        self.resume.notified().await;
    }
}
#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.bss.pricing.price_book_entry.v1~"))]
struct TestResource;
/// Products' answer for a reservation id it does not hold.
pub fn unknown_reference() -> CanonicalError {
    TestResource::not_found("reference not found")
        .with_resource("reference")
        .create()
}
pub fn refusal(code: &str) -> CanonicalError {
    TestResource::aborted(code).with_reason(code).create()
}
#[async_trait::async_trait]
impl ReferenceRegistryV1 for Script {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        _: Uuid,
        _: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        self.actors.lock().await.push(ctx.subject_id());
        self.reserve_kinds.lock().unwrap().push(kind);
        self.reserve_calls.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 1 {
            self.park().await;
        }
        if mode == 4 {
            return Err(refusal("SKU_FENCED"));
        }
        if mode == 17 {
            // Products lost a race on the SKU and rolled back: retryable.
            return Err(refusal("UNIT_CONTENDED"));
        }
        if mode == 19 {
            return Err(TestResource::resource_exhausted("slow down")
                .with_quota_violation("reserve", "rate limited")
                .create());
        }
        if mode == 16 {
            // A refusal that says nothing about the SKU admitting references.
            return Err(TestResource::permission_denied()
                .with_reason("REFERENCE_OWNER_MISMATCH")
                .create());
        }
        let mut refs = self.refs.lock().await;
        let entry = refs
            .entry(ref_id)
            .or_insert_with(|| (Uuid::new_v4(), ReferenceState::Reserved));
        if entry.1 == ReferenceState::Released {
            *entry = (Uuid::new_v4(), ReferenceState::Reserved);
        }
        let id = entry.0;
        if mode == 5 {
            self.set(0);
            return Err(CanonicalError::service_unavailable().create());
        }
        Ok(ReservationReceipt {
            reservation_id: id,
            state: entry.1,
        })
    }
    async fn confirm(&self, _: &SecurityContext, _: Uuid, id: Uuid) -> Result<(), CanonicalError> {
        self.confirm_calls.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 2 {
            self.park().await;
        }
        if mode == 6 {
            return Err(CanonicalError::service_unavailable().create());
        }
        if mode == 20 {
            // Products was restored from a backup older than this reservation: it does not
            // know the id (`confirm_tx` answers 404).
            self.refs.lock().await.retain(|_, item| item.0 != id);
            return Err(unknown_reference());
        }
        if mode == 7 {
            // An operator released the reservation before this confirm.
            for item in self.refs.lock().await.values_mut() {
                if item.0 == id {
                    item.1 = ReferenceState::Released;
                }
            }
            return Err(refusal("REFERENCE_RELEASED"));
        }
        for item in self.refs.lock().await.values_mut() {
            if item.0 == id {
                item.1 = ReferenceState::Confirmed;
            }
        }
        Ok(())
    }
    async fn release(&self, _: &SecurityContext, _: Uuid, id: Uuid) -> Result<(), CanonicalError> {
        if self.mode.load(Ordering::SeqCst) == 3 {
            self.park().await;
        }
        if matches!(self.mode.load(Ordering::SeqCst), 12 | 14) {
            return Err(CanonicalError::service_unavailable().create());
        }
        if self.mode.load(Ordering::SeqCst) == 20
            && !self.refs.lock().await.values().any(|item| item.0 == id)
        {
            // A restored Products does not know the reservation (`release_tx` answers 404).
            return Err(unknown_reference());
        }
        self.releases.fetch_add(1, Ordering::SeqCst);
        for item in self.refs.lock().await.values_mut() {
            if item.0 == id {
                item.1 = ReferenceState::Released;
            }
        }
        Ok(())
    }
    async fn states(
        &self,
        _: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        if *self.states_down_for.lock().unwrap() == Some(tenant) {
            return Err(CanonicalError::service_unavailable().create());
        }
        let refs = self.refs.lock().await;
        // Products answers the batch 404 when it does not know one of the reservations.
        ids.iter()
            .map(|id| {
                refs.values()
                    .find(|v| v.0 == *id)
                    .map(|v| (*id, v.1))
                    .ok_or_else(unknown_reference)
            })
            .collect()
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        if let Some(denied) = self.read_denied(ctx) {
            return Err(denied);
        }
        if self.skus_down.load(Ordering::SeqCst) {
            return Err(CanonicalError::service_unavailable().create());
        }
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 18 && Self::count(&self.reserve_calls) > 0 {
            // The re-read after a successful reserve lost a race in Products.
            return Err(refusal("CONTENDED"));
        }
        Ok(Sku {
            id,
            tenant_id: tenant,
            code: "cpu".into(),
            name: "CPU".into(),
            r#type: if matches!(mode, 8 | 14) {
                SkuType::Bundle
            } else if mode == 11 {
                SkuType::Recurring
            } else if mode == 21 && Self::count(&self.reserve_calls) == 0 {
                // D-427: the door's read sees a one-time SKU; the re-read after the reserve sees
                // the type the reservation froze, usage, which does not allow `flat`.
                SkuType::OneTime
            } else {
                SkuType::Usage
            },
            category_id: Some(Uuid::new_v4()),
            description: String::new(),
            sellable: true,
            lifecycle: if mode == 9 {
                Lifecycle::Deprecated
            } else if mode == 10 {
                Lifecycle::Draft
            } else {
                Lifecycle::Published
            },
            revision: 1,
            published_version: 1,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: Some(
                self.usage_type_ref
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "vm-hours".into()),
            ),
            unit: Some("VM\u{b7}hour".into()),
            // Mode 4 is a fenced SKU: a pending type change refuses every new reference.
            type_change_pending: mode == 4,
            retire_pending: mode == 22,
            lifecycle_next: None,
            pending_unit_id: None,
            approved_by_unit_id: None,
            created_by: Uuid::new_v4(),
            created_at: time::OffsetDateTime::now_utc(),
            updated_at: time::OffsetDateTime::now_utc(),
            archived_at: None,
            archived_by: None,
        })
    }
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        self.version_reads.fetch_add(1, Ordering::SeqCst);
        if let Some(denied) = self.read_denied(ctx) {
            return Err(denied);
        }
        if self.versions_down.load(Ordering::SeqCst) {
            return Err(CanonicalError::service_unavailable().create());
        }
        if self.versions_refused.load(Ordering::SeqCst) {
            return Err(TestResource::permission_denied()
                .with_reason("SKU_READ_DENIED")
                .create());
        }
        let found = self
            .versions
            .lock()
            .unwrap()
            .iter()
            .filter(|(from, _, _)| *from <= date)
            .max_by_key(|(from, _, _)| *from)
            .cloned();
        let Some((from, unit, usage_type_ref)) = found else {
            return Ok(None);
        };
        let head = self.sku_for_write(ctx, tenant, sku).await?;
        let mut content = bss_products_sdk::models::SkuContent::from(&head);
        content.unit = unit;
        content.usage_type_ref = usage_type_ref;
        Ok(Some(SkuVersion {
            sku_id: sku,
            published_version: 1,
            effective_from: from,
            content,
            created_at: time::OffsetDateTime::now_utc(),
        }))
    }
}

use bss_pricing::infra::storage::entity::{price, price_book_entry};
use storage_support::at;
/// A draft of a `per_unit` entry: the money is in the entry's model (D-427).
pub fn price(p: &price_book_entry::Model) -> price::Model {
    price::Model {
        id: Uuid::new_v4(),
        tenant_id: p.tenant_id,
        price_book_entry_id: p.id,
        version_no: 1,
        dim_value: None,
        price_json: serde_json::json!({"rate":"0.1"}),
        min_fee: Some("12.34".into()),
        eligibility: "all".into(),
        effective_from: at(9).date(),
        effective_to: None,
        keep_for_bound: false,
        closed_explicitly: false,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        change_kind: "set".into(),
        target_price_id: None,
        cancelled_by_unit_id: None,
        state: "draft".into(),
        pending_unit_id: None,
        approved_by_unit_id: None,
        note: None,
        created_by: Uuid::new_v4(),
        approved_at: None,
        version: 1,
        created_at: at(9),
        updated_at: at(9),
    }
}

// ------------------------------------------------------------------ the two reference kinds

use bss_pricing::api::rest::authoring::{AuthoringState, dto, plan_items};
use bss_pricing::infra::storage::{
    entity::{plan, plan_item, plan_revision},
    repo::{plan_item_repo, plan_repo, plan_revision_repo, price_book_entry_repo},
};
use toolkit_db::secure::AccessScope;
/// The two kinds of reference the durable machine drives (D-407).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Entry,
    Item,
}
/// Every kind, for the suites parameterised over them.
pub const KINDS: [Kind; 2] = [Kind::Entry, Kind::Item];
/// Who calls: the router, the state beneath it and the principal.
pub struct Caller<'a> {
    pub app: &'a Router,
    pub state: &'a Arc<AuthoringState>,
    pub ctx: &'a SecurityContext,
}
impl Fixture {
    #[must_use]
    pub fn caller(&self) -> Caller<'_> {
        Caller {
            app: &self.app,
            state: &self.state,
            ctx: &self.ctx,
        }
    }
    /// A book and, for items, a plan with a draft revision 1 on it.
    pub async fn target(&self, kind: Kind) -> Target {
        Target::new(kind, &self.caller()).await
    }
}
/// Where one kind's references are made: an entry through the entries REST door of a book, a
/// plan item through the plan-item op-level API on a draft revision of a plan on that book (its
/// REST door is run 3.3's).
#[derive(Debug, Clone)]
pub struct Target {
    pub kind: Kind,
    pub book: Uuid,
    /// The plan and its draft revision; nil for entries.
    pub plan: Uuid,
    pub revision: Uuid,
}
/// Read a door's answer as `(status, body, etag)`, the shape [`request`] returns.
pub async fn answer(
    result: Result<axum::response::Response, CanonicalError>,
) -> (u16, Value, String) {
    use axum::response::IntoResponse;
    let response = result.unwrap_or_else(IntoResponse::into_response);
    let status = response.status().as_u16();
    let tag = response
        .headers()
        .get("etag")
        .map_or("", |v| v.to_str().unwrap())
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
        tag,
    )
}
/// A plan and its draft revision 1 on `book`, written through the repositories.
pub async fn plan_on(
    state: &AuthoringState,
    ctx: &SecurityContext,
    book: Uuid,
) -> (plan::Model, plan_revision::Model) {
    let tenant = ctx.subject_tenant_id();
    let scope = AccessScope::for_tenant(tenant);
    let conn = state.db.conn().unwrap();
    let now = time::OffsetDateTime::now_utc();
    let p = plan_repo::insert(
        &conn,
        &scope,
        plan::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            code: format!("plan-{}", Uuid::new_v4()),
            name: "Pro".into(),
            published_rev: None,
            version: 1,
            created_by: ctx.subject_id(),
            created_at: now,
            updated_at: now,
            work_revision_id: None,
            work_state: None,
            scheduled_revision_id: None,
            scheduled_from: None,
            published_revision_id: None,
            current_book_id: None,
            current_currency: None,
            last_activity_at: now,
        },
    )
    .await
    .unwrap();
    let r = plan_revision_repo::insert(
        &conn,
        &scope,
        plan_revision::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            plan_id: p.id,
            rev_no: 1,
            book_id: book,
            state: "draft".into(),
            available_from: None,
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    (p, r)
}
impl Target {
    pub async fn new(kind: Kind, c: &Caller<'_>) -> Self {
        let (s, book, _) = request(
            c.app,
            c.ctx,
            "POST",
            "/price-books",
            json!({"code":format!("book-{}", Uuid::new_v4()),"name":"Standard","currency":"EUR"}),
            None,
            Some("book"),
        )
        .await;
        assert_eq!(s, 201, "{book}");
        let book: Uuid = book["id"].as_str().unwrap().parse().unwrap();
        let (plan, revision) = if kind == Kind::Item {
            let (p, r) = plan_on(c.state, c.ctx, book).await;
            (p.id, r.id)
        } else {
            (Uuid::nil(), Uuid::nil())
        };
        Self {
            kind,
            book,
            plan,
            revision,
        }
    }
    /// The endpoint a create's Idempotency-Key belongs to, below `/bss-pricing/v1`.
    #[must_use]
    pub fn endpoint(&self) -> String {
        match self.kind {
            Kind::Entry => format!("/price-books/{}/entries", self.book),
            Kind::Item => format!("/plan-revisions/{}/items", self.revision),
        }
    }
    /// The op's `ref_kind`.
    #[must_use]
    pub const fn ref_kind(&self) -> &'static str {
        match self.kind {
            Kind::Entry => "price_book_entry",
            Kind::Item => "plan_item",
        }
    }
    /// The type of the kind's lost-reference event, and the field naming the reference in it.
    #[must_use]
    pub const fn lost_event(&self) -> (&'static str, &'static str) {
        match self.kind {
            Kind::Entry => (
                "gts.cf.core.events.event.v1~cf.bss.pricing.price_book_entry_reference_lost.v1~",
                "priceBookEntryId",
            ),
            Kind::Item => (
                "gts.cf.core.events.event.v1~cf.bss.pricing.plan_reference_lost.v1~",
                "itemId",
            ),
        }
    }
    /// A create body for a fresh SKU: an entry, or an item naming that SKU's entry of the book. A
    /// plan item is a SKU and its entry (D-467): the entry is written directly when the input is
    /// asked for, through `c`'s database, so it makes no reservation of its own. It stays
    /// `confirmation_pending`, which reconciliation (confirmed and lost references) never scans:
    /// it is not a reference a suite drives. Each call is a new SKU, so a target hands out as many
    /// as a suite asks for (the phase 9 review's R26, R61: no pool, no lock).
    pub async fn input(&self, c: &Caller<'_>) -> Value {
        match self.kind {
            // `per_unit` is a model every charge kind allows (D-386, D-427).
            Kind::Entry => {
                json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"})
            }
            Kind::Item => {
                let tenant = c.ctx.subject_tenant_id();
                let now = time::OffsetDateTime::now_utc();
                let sku = Uuid::new_v4();
                let entry = price_book_entry_repo::insert(
                    &c.state.db.conn().unwrap(),
                    &AccessScope::for_tenant(tenant),
                    price_book_entry::Model {
                        id: Uuid::now_v7(),
                        tenant_id: tenant,
                        book_id: self.book,
                        sku_id: sku,
                        charge_kind: "usage".into(),
                        period: None,
                        model: "per_unit".into(),
                        usage_policy_id: None,
                        usage_policy_version: None,
                        usage_policy_digest: None,
                        usage_sku_version: None,
                        dimension_key: None,
                        invoice_line_override: None,
                        reservation_id: Uuid::new_v4(),
                        reference_state: "confirmation_pending".into(),
                        version: 1,
                        created_at: now,
                        updated_at: now,
                    },
                )
                .await
                .unwrap();
                json!({"sku_id":sku,"price_book_entry_id":entry.id})
            }
        }
    }
    /// Create through the kind's front door and read its answer.
    pub async fn create(&self, c: &Caller<'_>, input: Value, key: &str) -> (u16, Value, String) {
        match self.kind {
            Kind::Entry => {
                request(
                    c.app,
                    c.ctx,
                    "POST",
                    &self.endpoint(),
                    input,
                    None,
                    Some(key),
                )
                .await
            }
            Kind::Item => {
                let digest = bss_pricing::api::rest::preconditions::request_digest(&input).unwrap();
                let body: dto::PricingPlanItemCreate = serde_json::from_value(input).unwrap();
                answer(
                    plan_items::create(
                        c.state.clone(),
                        AccessScope::for_tenant(c.ctx.subject_tenant_id()),
                        c.ctx.clone(),
                        self.revision,
                        Uuid::now_v7(),
                        key.to_owned(),
                        digest,
                        body,
                    )
                    .await,
                )
                .await
            }
        }
    }
    /// Delete one reference through the kind's front door.
    pub async fn delete(&self, c: &Caller<'_>, id: &Value) -> (u16, Value, String) {
        let id: Uuid = id.as_str().unwrap().parse().unwrap();
        match self.kind {
            Kind::Entry => {
                request(
                    c.app,
                    c.ctx,
                    "DELETE",
                    &format!("/price-book-entries/{id}"),
                    json!({}),
                    None,
                    None,
                )
                .await
            }
            Kind::Item => {
                answer(
                    plan_items::delete(
                        c.state.clone(),
                        AccessScope::for_tenant(c.ctx.subject_tenant_id()),
                        c.ctx.clone(),
                        Uuid::now_v7(),
                        id,
                    )
                    .await,
                )
                .await
            }
        }
    }
    /// The stored reference `(reference_state, reservation_id)`, `None` once the row is gone.
    pub async fn stored(&self, state: &AuthoringState, id: Uuid) -> Option<(String, Option<Uuid>)> {
        let conn = state.db.conn().unwrap();
        let scope = AccessScope::allow_all();
        match self.kind {
            Kind::Entry => {
                let tenant = entry_tenant(&conn, id).await?;
                price_book_entry_repo::find(&conn, &scope, tenant, id)
                    .await
                    .unwrap()
                    .map(|e| (e.reference_state, Some(e.reservation_id)))
            }
            Kind::Item => {
                let tenant = item_tenant(&conn, id).await?;
                plan_item_repo::find(&conn, &scope, tenant, id)
                    .await
                    .unwrap()
                    .map(|i| (i.reference_state, i.reservation_id))
            }
        }
    }
    /// The reference as its kind reads it: the entry door's body, or the item's DTO.
    pub async fn read(&self, c: &Caller<'_>, id: &Value) -> Value {
        let uuid: Uuid = id.as_str().unwrap().parse().unwrap();
        match self.kind {
            Kind::Entry => {
                let (status, body, _) = request(
                    c.app,
                    c.ctx,
                    "GET",
                    &format!("/price-book-entries/{uuid}"),
                    json!({}),
                    None,
                    None,
                )
                .await;
                assert_eq!(status, 200, "{body}");
                body
            }
            Kind::Item => {
                let item = plan_item_repo::find(
                    &c.state.db.conn().unwrap(),
                    &AccessScope::for_tenant(c.ctx.subject_tenant_id()),
                    c.ctx.subject_tenant_id(),
                    uuid,
                )
                .await
                .unwrap()
                .unwrap();
                serde_json::to_value(dto::PricingPlanItemDto::try_from(item).unwrap()).unwrap()
            }
        }
    }
}
async fn entry_tenant(conn: &toolkit_db::DbConn<'_>, id: Uuid) -> Option<Uuid> {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    price_book_entry::Entity::find_by_id(id)
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(conn)
        .await
        .unwrap()
        .map(|m| m.tenant_id)
}
async fn item_tenant(conn: &toolkit_db::DbConn<'_>, id: Uuid) -> Option<Uuid> {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    plan_item::Entity::find_by_id(id)
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(conn)
        .await
        .unwrap()
        .map(|m| m.tenant_id)
}
/// Every envelope of `type_id` in the outbox of the database at `dsn` (either dialect's raw
/// connection answers the same `payload` column).
pub async fn outbox_events(dsn: &str, type_id: &str) -> Vec<Value> {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let raw = Database::connect(dsn).await.unwrap();
    raw.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT CAST(payload AS TEXT) AS payload FROM bss_pricing_outbox_body",
    ))
    .await
    .unwrap()
    .iter()
    .map(|r| serde_json::from_str(&r.try_get::<String>("", "payload").unwrap()).unwrap())
    .filter(|e: &Value| e["type"] == type_id)
    .collect()
}
