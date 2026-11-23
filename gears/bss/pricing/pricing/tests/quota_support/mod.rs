//! Shared fixture for the quota rule endpoints (D-540 to D-545): a Products quota catalog double
//! the hub resolves, enabled vCPU and RAM definitions, a draft with two items whose SKUs have a
//! version on the sale date, and the rule bodies a PUT sends.
//!
//! The double judges only what Pricing needs to see from it: an unknown definition version is
//! `QUOTA_RULE_INVALID`, another digest `QUOTA_DEFINITION_CHANGED` and a version that is not
//! enabled `QUOTA_QUALIFICATION_UNAVAILABLE`. Products' own judgement is proven in Products.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::plan_support::{self, Catalog, Fixture};
use bss_products_sdk::ProductsQuotaCatalogClientV1;
use bss_products_sdk::models::SkuType;
use bss_products_sdk::quota::catalog::{
    QuotaDefinition, QuotaDefinitionListRequest, QuotaDefinitionPage, QuotaVersion,
};
use bss_products_sdk::quota::validation::{
    QuotaCheck, QuotaCheckCode, QuotaRuleVerdict, QuotaSetRequest, QuotaSetValidation,
};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

pub const VCPU_METRIC: &str =
    "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.vcpu.v1";
pub const VCPU_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1";
pub const RAM_METRIC: &str = "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.ram.v1";
pub const GIB_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.gib.v1";
const ACCOUNTING: &str =
    "gts.cf.bss.products.quota_accounting_policy.v1~cf.compute.quota_adapter.allocated.v1";
const SCOPE_POLICY: &str =
    "gts.cf.bss.products.quota_scope_policy.v1~cf.compute.quota_adapter.regional_pool.v1";

/// Work the double runs once, before it answers its next call.
type Hook = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Products' quota catalog as Pricing sees it.
#[derive(Default)]
pub struct FakeQuotaCatalog {
    definitions: Mutex<Vec<QuotaDefinition>>,
    scripted: Mutex<Option<QuotaSetValidation>>,
    refusal: Mutex<Option<CanonicalError>>,
    down: AtomicBool,
    requests: Mutex<Vec<QuotaSetRequest>>,
    /// Who each call was made as: its subject id and subject type, in call order.
    callers: Mutex<Vec<(Uuid, Option<String>)>>,
    /// When set, each call records whether a connection to this database could be opened: none
    /// can on a task inside a transaction.
    watched: Mutex<Option<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    outside: Mutex<Vec<bool>>,
    hook: Mutex<Option<Hook>>,
}

impl FakeQuotaCatalog {
    /// Store a definition and answer it.
    pub fn add(&self, definition: QuotaDefinition) -> QuotaDefinition {
        self.definitions.lock().unwrap().push(definition.clone());
        definition
    }
    /// Answer `validation` to every later call.
    pub fn answering(&self, validation: QuotaSetValidation) {
        *self.scripted.lock().unwrap() = Some(validation);
    }
    /// Judge every later call again, as before [`Self::answering`].
    pub fn judging(&self) {
        *self.scripted.lock().unwrap() = None;
    }
    /// Withdraw one stored definition version (`qualification_withdrawn`).
    pub fn withdraw(&self, id: Uuid, version: u64) {
        for definition in self.definitions.lock().unwrap().iter_mut() {
            if definition.definition_id == id && definition.version.get() == version {
                withdrawn(definition);
            }
        }
    }
    /// Withdraw every stored definition version.
    pub fn withdraw_all(&self) {
        for definition in self.definitions.lock().unwrap().iter_mut() {
            withdrawn(definition);
        }
    }
    /// Every later call is a 503.
    pub fn go_down(&self) {
        self.down.store(true, Ordering::SeqCst);
    }
    /// Every later call answers again.
    pub fn come_back(&self) {
        self.down.store(false, Ordering::SeqCst);
    }
    /// Every later call is refused with `error`, as Products would refuse it.
    pub fn refusing(&self, error: CanonicalError) {
        *self.refusal.lock().unwrap() = Some(error);
    }
    /// Every request answered or refused, in call order.
    pub fn requests(&self) -> Vec<QuotaSetRequest> {
        self.requests.lock().unwrap().clone()
    }
    /// Who each call answered or refused was made as, in call order: its subject id and type.
    pub fn callers(&self) -> Vec<(Uuid, Option<String>)> {
        self.callers.lock().unwrap().clone()
    }
    /// Record, on each later call, whether a connection to `db` can be opened.
    pub fn watch(&self, db: toolkit_db::DBProvider<toolkit_db::DbError>) {
        *self.watched.lock().unwrap() = Some(db);
    }
    /// Whether each watched call could open a connection: `false` on a transaction's task.
    pub fn outside(&self) -> Vec<bool> {
        self.outside.lock().unwrap().clone()
    }
    /// Run `work` once, before the next call answers.
    pub fn before_next_answer(&self, work: impl Future<Output = ()> + Send + 'static) {
        *self.hook.lock().unwrap() = Some(Box::pin(work));
    }
    fn judge(&self, request: &QuotaSetRequest) -> QuotaSetValidation {
        let definitions = self.definitions.lock().unwrap();
        let mut rules = Vec::new();
        let mut checks = Vec::new();
        for rule in &request.rules {
            let found = definitions.iter().find(|d| {
                d.definition_id == rule.definition.id && d.version == rule.definition.version
            });
            rules.push(QuotaRuleVerdict {
                rule_id: rule.rule_id,
                definition: found.cloned(),
                limit: None,
            });
            let Some(found) = found else {
                checks.push(check(
                    QuotaCheckCode::RuleInvalid,
                    rule.rule_id,
                    "definition_ref",
                    "no such definition version",
                ));
                continue;
            };
            if found.definition_digest != rule.definition.digest {
                checks.push(check(
                    QuotaCheckCode::DefinitionChanged,
                    rule.rule_id,
                    "definition_ref.digest",
                    "the stored digest differs",
                ));
            }
            let state = serde_json::to_value(found.availability.state).unwrap();
            if state != "enabled" {
                checks.push(check(
                    QuotaCheckCode::QualificationUnavailable,
                    rule.rule_id,
                    "definition_ref",
                    &format!(
                        "{}: {}",
                        state.as_str().unwrap(),
                        found.availability.reason_code.clone().unwrap_or_default()
                    ),
                ));
            }
        }
        QuotaSetValidation { rules, checks }
    }
}

fn check(code: QuotaCheckCode, rule: Uuid, field: &str, detail: &str) -> QuotaCheck {
    QuotaCheck {
        code,
        rule_ids: vec![rule],
        field: field.into(),
        detail: detail.into(),
    }
}

fn withdrawn(definition: &mut QuotaDefinition) {
    let next = definition.availability.revision.get() + 1;
    definition.availability.state =
        serde_json::from_value(json!("withdrawn")).expect("a Products state");
    definition.availability.revision = QuotaVersion::new(next).unwrap();
    definition.availability.reason_code = Some("qualification_withdrawn".into());
}

#[async_trait::async_trait]
impl ProductsQuotaCatalogClientV1 for FakeQuotaCatalog {
    async fn list_quota_definitions(
        &self,
        _: &SecurityContext,
        _: QuotaDefinitionListRequest,
    ) -> Result<QuotaDefinitionPage, CanonicalError> {
        Err(CanonicalError::internal("Pricing never lists definitions").create())
    }
    async fn get_quota_definition(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: QuotaVersion,
        _: Option<&str>,
    ) -> Result<QuotaDefinition, CanonicalError> {
        Err(CanonicalError::internal("Pricing never reads one definition").create())
    }
    async fn validate_quota_set(
        &self,
        ctx: &SecurityContext,
        request: QuotaSetRequest,
    ) -> Result<QuotaSetValidation, CanonicalError> {
        self.callers
            .lock()
            .unwrap()
            .push((ctx.subject_id(), ctx.subject_type().map(str::to_owned)));
        let watched = self.watched.lock().unwrap().clone();
        if let Some(db) = watched {
            self.outside.lock().unwrap().push(db.conn().is_ok());
        }
        self.requests.lock().unwrap().push(request.clone());
        let hook = self.hook.lock().unwrap().take();
        if let Some(hook) = hook {
            hook.await;
        }
        if self.down.load(Ordering::SeqCst) {
            return Err(CanonicalError::service_unavailable().create());
        }
        if let Some(refusal) = self.refusal.lock().unwrap().clone() {
            return Err(refusal);
        }
        if let Some(scripted) = self.scripted.lock().unwrap().clone() {
            return Ok(scripted);
        }
        Ok(self.judge(&request))
    }
}

/// An enabled definition of `metric` for `sku` at SKU version 3, its only unit `unit`, with a
/// fresh id and digest (the Products `set_tests` shape).
pub fn definition(sku: Uuid, metric: &str, unit: &str, max: &str) -> QuotaDefinition {
    let digest = format!("{:064x}", Uuid::new_v4().as_u128());
    serde_json::from_value(json!({
        "definition_id": Uuid::new_v4(), "version": "1", "definition_digest": digest,
        "sku_id": sku, "sku_version": "3", "metric_ref": metric,
        "metric_digest": "b".repeat(64), "quota_kind": "allocation", "display_name": "quota",
        "description": "A quota", "locale": "en", "canonical_unit_ref": unit,
        "allowed_units": [{"unit_ref": unit, "digest": "c".repeat(64), "symbol": "u",
            "numerator": "1", "denominator": "1", "min": "0", "step": "1", "max": max}],
        "subject_type_ref": "gts.cf.core.compute.tenant.v1~",
        "resource_type_refs": ["gts.cf.core.compute.vm.v1~"],
        "mapping_ref": {"id": Uuid::from_u128(0xa1), "version": "1", "digest": "d".repeat(64)},
        "profile_ref": {"id": Uuid::from_u128(0xb1), "version": "2", "digest": "e".repeat(64)},
        "qualification_ref": {"id": Uuid::from_u128(0xc1), "version": "1",
            "digest": "f".repeat(64)},
        "accounting_policy_ref": ACCOUNTING,
        "accounting_policy_digest": "2".repeat(64),
        "accounting_policy": {"id": ACCOUNTING,
            "quantity_basis": "allocated_capacity", "aggregation": "sum",
            "description": {"en": "Counts allocated capacity."}},
        "scope_policy_ref": SCOPE_POLICY,
        "scope_policy_digest": "3".repeat(64),
        "scope_policy": {"id": SCOPE_POLICY,
            "subject_selector": "resource_owner", "aggregation": "single_verified_scope",
            "required_dimensions": ["region", "resource_pool"]},
        "change_support": {"initial_set": true, "increase": false},
        "profile_conditions": "Counts stopped VMs.", "supported_bases": ["Fixed", "PerPlanUnit"],
        "availability": {"state": "enabled", "revision": "2",
            "observed_at": "2026-11-21T08:30:00Z", "reason_code": null}
    }))
    .unwrap()
}
/// An enabled vCPU definition of `sku`.
pub fn vcpu(sku: Uuid) -> QuotaDefinition {
    definition(sku, VCPU_METRIC, VCPU_UNIT, "1024")
}
/// An enabled RAM definition of `sku`, in GiB.
pub fn ram(sku: Uuid) -> QuotaDefinition {
    definition(sku, RAM_METRIC, GIB_UNIT, "4096")
}
/// `definition`, disabled while its qualification is pending (products P-D-271).
pub fn disabled(mut definition: QuotaDefinition) -> QuotaDefinition {
    definition.availability.state =
        serde_json::from_value(json!("disabled")).expect("a Products state");
    definition.availability.revision = QuotaVersion::new(1).unwrap();
    definition.availability.reason_code = Some("qualification_pending".into());
    definition
}

/// A double registered in the fixture's hub, where the endpoints resolve the catalog per call.
pub fn install(f: &Fixture) -> Arc<FakeQuotaCatalog> {
    let fake = Arc::new(FakeQuotaCatalog::default());
    f.state
        .hub
        .register::<dyn ProductsQuotaCatalogClientV1>(fake.clone());
    fake
}

/// A draft revision with two items, each a SKU of its own at version 3 from 2026-01-01.
#[derive(Debug, Clone)]
pub struct Draft {
    pub plan: Uuid,
    pub revision: Uuid,
    /// The revision's `ETag` as the revision read answers it.
    pub etag: String,
    pub items: Vec<Uuid>,
    /// The first item's SKU.
    pub sku: Uuid,
    /// The second item's SKU.
    pub other_sku: Uuid,
}

/// A book, a plan and two items added through the endpoints, each SKU with its entry and a
/// version in force on the sale date (the catalog's dated reads).
pub async fn draft_with_items(f: &Fixture, catalog: &Catalog) -> Draft {
    let code = format!("q{}", &Uuid::new_v4().simple().to_string()[..8]);
    let book = plan_support::book(f, &code).await;
    let (plan_body, revision) = plan_support::plan(f, &code, book).await;
    let mut items = Vec::new();
    let mut skus = Vec::new();
    for n in 0..2 {
        let sku = catalog.sku(SkuType::Recurring);
        catalog.version(sku, 3, "2026-01-01", catalog.content(sku));
        let entry = plan_support::entry(f, book, sku, "recurring", Some("month")).await;
        let (s, b, _) = f
            .call(
                "POST",
                &format!("/plan-revisions/{revision}/items"),
                json!({"sku_id": sku, "price_book_entry_id": entry}),
                None,
                Some(&format!("{code}-item-{n}")),
            )
            .await;
        assert_eq!(s, 201, "{b}");
        items.push(plan_support::id_of(&b["id"]));
        skus.push(sku);
    }
    let (s, read, etag) = f
        .call(
            "GET",
            &format!("/plan-revisions/{revision}"),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{read}");
    Draft {
        plan: plan_support::id_of(&plan_body["id"]),
        revision,
        etag,
        items,
        sku: skus[0],
        other_sku: skus[1],
    }
}

/// An approved price of `entry` from `from`, written through the repository: the entry's item is
/// covered from that day.
pub async fn approved_price(f: &Fixture, entry: Uuid, from: &str) {
    use bss_pricing::infra::storage::repo::{price_book_entry_repo, price_repo};
    let conn = f.db.conn().unwrap();
    let tenant = f.ctx.subject_tenant_id();
    let stored = price_book_entry_repo::find(&conn, &plan_support::scope(f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut price = plan_support::entry_support::price(&stored);
    price.state = "approved".into();
    price.effective_from =
        time::Date::parse(from, &time::format_description::well_known::Iso8601::DATE).unwrap();
    price_repo::insert(&conn, &plan_support::scope(f), price)
        .await
        .unwrap();
}

/// [`draft_with_items`] with an approved price from 2020-01-01 on each item's entry: every check
/// but the quota rows is green, so the quota rows alone decide whether it is ready.
pub async fn priced_draft_with_items(f: &Fixture, catalog: &Catalog) -> Draft {
    let draft = draft_with_items(f, catalog).await;
    for item in plan_support::items(f, draft.revision).await {
        approved_price(f, item.price_book_entry_id.unwrap(), "2020-01-01").await;
    }
    draft
}

/// Rule `n` (its id `Uuid::from_u128(n)`) on `item`, in the definition's unit, per plan unit,
/// with the scope the definition's policy asks for.
pub fn rule_json(n: u128, definition: &QuotaDefinition, item: Uuid, amount: &str) -> Value {
    json!({
        "rule_id": Uuid::from_u128(n),
        "definition_ref": {
            "id": definition.definition_id,
            "version": definition.version.to_string(),
            "digest": definition.definition_digest,
        },
        "item_id": item,
        "dimension_value": null,
        "logical_binding_key": "compute",
        "subject_selector": "resource_owner",
        "scope_selection": {"region": "region-1", "resource_pool": "pool-1"},
        "amount": amount,
        "unit_ref": definition.canonical_unit_ref,
        "basis": "per_plan_unit",
    })
}

/// The quota rules path of `draft`'s revision.
pub fn path(draft: &Draft) -> String {
    format!("/plan-revisions/{}/quota-rules", draft.revision)
}

/// `PUT` the whole set `rules` under `draft`'s `ETag` and the key `key`.
pub async fn put(f: &Fixture, draft: &Draft, rules: &[Value], key: &str) -> (u16, Value, String) {
    f.call(
        "PUT",
        &path(draft),
        json!({ "quota_rules": rules }),
        Some(&draft.etag),
        Some(key),
    )
    .await
}

/// The log lines of the current thread while the guard [`LogCapture::install`] answers lives:
/// each event as `LEVEL message name=value …`. A `#[tokio::test]` runs on one thread, so it sees
/// every line its requests write, the detached catalog calls included.
#[derive(Clone, Default)]
pub struct LogCapture {
    lines: Arc<Mutex<Vec<String>>>,
}

impl LogCapture {
    /// Capture this thread's log lines until the guard drops.
    #[must_use]
    pub fn install() -> (Self, tracing::subscriber::DefaultGuard) {
        let capture = Self::default();
        let guard = tracing::subscriber::set_default(Capturing(capture.clone()));
        (capture, guard)
    }
    /// Every line captured so far.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

struct Capturing(LogCapture);

/// An event's message first, then its other fields as `name=value`.
#[derive(Default)]
struct Fields(Vec<String>);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.insert(0, format!("{value:?}"));
        } else {
            self.0.push(format!("{}={value:?}", field.name()));
        }
    }
}

impl tracing::Subscriber for Capturing {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let line = format!("{} {}", event.metadata().level(), fields.0.join(" "));
        self.0.lines.lock().unwrap().push(line);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
