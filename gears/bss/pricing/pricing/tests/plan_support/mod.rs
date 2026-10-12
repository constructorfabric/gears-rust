//! Shared fixture for the plan, revision and item doors (run 3.3): a Products catalog double
//! whose SKUs a test types and ages, and helpers that set up books, entries, plans and published
//! revisions through the repositories where the door under test is not the subject.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "../entry_support/mod.rs"]
pub mod entry_support;
use bss_approval::{Store, Unit, UnitState};
use bss_pricing::infra::storage::{
    RepoError,
    entity::{plan_item, price_book_entry},
    repo::{
        approval_repo::PricingApprovalStore, plan_item_repo, plan_repo, plan_revision_repo,
        price_book_entry_repo, price_repo, reference_op_repo,
    },
};
use bss_products_sdk::{
    ReferenceRegistryV1,
    models::{
        Lifecycle, ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuContent, SkuType,
        SkuVersion,
    },
};
#[allow(
    unused_imports,
    reason = "not every suite that shares this fixture calls a door raw"
)]
pub use entry_support::{Fixture, request};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.bss.products.sku.v1~"))]
struct SkuResource;

/// Who a registry call was made as: subject id, subject type, and the tenant it asked for.
pub type Caller = (Uuid, Option<String>, Uuid);
/// One SKU as the catalog answers it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub r#type: SkuType,
    pub lifecycle: Lifecycle,
    pub meter: Option<String>,
    pub unit: Option<String>,
    pub name: String,
    /// The SKU's current GL code, a descriptor (D-408): information, never content.
    pub gl_code: Option<String>,
}
/// Products as pricing sees it: SKUs a test declares and ages, reservations that always
/// succeed, and a switch that takes the whole registry down.
#[derive(Default)]
pub struct Catalog {
    pub skus: Mutex<BTreeMap<Uuid, Entry>>,
    pub down: AtomicBool,
    pub contended: AtomicBool,
    /// Every `sku_for_write` call, answered or not.
    pub reads: AtomicUsize,
    pub reserve_kinds: Mutex<Vec<ReferenceKind>>,
    pub releases: AtomicUsize,
    pub refs: Mutex<BTreeMap<Uuid, (Uuid, ReferenceState)>>,
    /// Opt-in: when set, only these principals hold products `read`; any other caller's SKU read
    /// is Products' 403, as Products' registry authorizes the caller before it reads. Unset (the
    /// default) admits every caller.
    pub readers: Mutex<Option<std::collections::BTreeSet<Uuid>>>,
    /// Opt-in: when set, only these principals hold products `reference`; any other caller's
    /// reserve, confirm or release is Products' 403. Unset (the default) admits every caller.
    pub referencers: Mutex<Option<std::collections::BTreeSet<Uuid>>>,
    /// Every registry call, answered or not.
    pub calls: AtomicUsize,
    /// Opt-in: the dated SKU versions Products holds. When set, `sku_version_as_of` answers the
    /// version in force on the date (the last one whose `effective_from` is on or before it) for
    /// a SKU the catalog declares, `None` when none is in force yet, and Products' 404 for a SKU
    /// it does not declare. Unset (the default) answers `None` for every SKU.
    pub versions: Mutex<Option<BTreeMap<Uuid, Vec<SkuVersion>>>>,
    /// Who each `sku_version_as_of` call was made as: subject id, subject type and the tenant
    /// asked for, in call order.
    pub version_readers: Mutex<Vec<Caller>>,
    /// Products bound its registry to another owner (a wiring error): every system subject,
    /// pricing's own included, is `REFERENCE_OWNER_MISMATCH`.
    pub foreign_owner: AtomicBool,
    /// Opt-in: a `release` waits this many milliseconds before it answers (a slow Products).
    /// Zero (the default) answers at once.
    pub stall_releases_ms: AtomicU64,
}
impl Catalog {
    /// Arm the dated reads (see `versions`) and add one published version of a declared SKU.
    pub fn version(
        &self,
        sku: Uuid,
        published_version: i64,
        effective_from: &str,
        content: SkuContent,
    ) {
        let effective_from = time::Date::parse(
            effective_from,
            &time::format_description::well_known::Iso8601::DATE,
        )
        .unwrap();
        let mut versions = self.versions.lock().unwrap();
        let chain = versions
            .get_or_insert_with(BTreeMap::new)
            .entry(sku)
            .or_default();
        chain.push(SkuVersion {
            sku_id: sku,
            published_version,
            effective_from,
            content,
            created_at: time::OffsetDateTime::now_utc(),
        });
        chain.sort_by_key(|v| v.effective_from);
    }
    /// Arm the dated reads with no version yet: every undeclared SKU is then Products' 404.
    pub fn dated(&self) {
        self.versions
            .lock()
            .unwrap()
            .get_or_insert_with(BTreeMap::new);
    }
    /// The content of a declared SKU as a published version carries it, descriptors unset.
    pub fn content(&self, sku: Uuid) -> SkuContent {
        let entry = self.skus.lock().unwrap()[&sku].clone();
        SkuContent {
            code: entry.name.clone(),
            name: entry.name,
            r#type: entry.r#type,
            category_id: Some(Uuid::nil()),
            description: String::new(),
            sellable: true,
            gl_code: entry.gl_code,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: entry.meter,
            unit: entry.unit,
        }
    }
    /// Only these principals may read SKUs from now on (products `read`).
    pub fn readers(&self, principals: impl IntoIterator<Item = Uuid>) {
        *self.readers.lock().unwrap() = Some(principals.into_iter().collect());
    }
    /// Only these principals may reserve, confirm or release from now on (products `reference`).
    pub fn referencers(&self, principals: impl IntoIterator<Item = Uuid>) {
        *self.referencers.lock().unwrap() = Some(principals.into_iter().collect());
    }
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    /// Products' own rule for a system subject, which it applies before its PDP
    /// (`reference_registry::scope`): pricing's system actor is admitted without a grant; any
    /// other `*.system` subject, and pricing's own under a foreign owner, is 403
    /// `REFERENCE_OWNER_MISMATCH`. `None` for any other subject: its grant decides.
    fn system_subject(&self, ctx: &SecurityContext) -> Option<Result<(), CanonicalError>> {
        if !ctx.subject_type().is_some_and(|s| s.ends_with(".system")) {
            return None;
        }
        let pricing = ctx.subject_type() == Some("bss-pricing.system")
            && ctx.subject_id() == bss_products_sdk::PRICING_SYSTEM_ACTOR
            && !self.foreign_owner.load(Ordering::SeqCst);
        Some(if pricing {
            Ok(())
        } else {
            Err(SkuResource::permission_denied()
                .with_reason("REFERENCE_OWNER_MISMATCH")
                .create())
        })
    }
    /// Products' 403 for a caller without products `read`, when the opt-in set is armed.
    fn read_denied(&self, ctx: &SecurityContext) -> Result<(), CanonicalError> {
        if let Some(verdict) = self.system_subject(ctx) {
            return verdict;
        }
        let readers = self.readers.lock().unwrap();
        if readers
            .as_ref()
            .is_some_and(|set| !set.contains(&ctx.subject_id()))
        {
            return Err(SkuResource::permission_denied()
                .with_reason("SKU_READ_DENIED")
                .create());
        }
        Ok(())
    }
    /// Products' 403 for a caller without products `reference`, when the opt-in set is armed.
    fn reference_denied(&self, ctx: &SecurityContext) -> Result<(), CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(verdict) = self.system_subject(ctx) {
            return verdict;
        }
        let referencers = self.referencers.lock().unwrap();
        if referencers
            .as_ref()
            .is_some_and(|set| !set.contains(&ctx.subject_id()))
        {
            return Err(SkuResource::permission_denied()
                .with_reason("SKU_REFERENCE_DENIED")
                .create());
        }
        Ok(())
    }
    /// Declare (or re-declare) a SKU.
    pub fn put(&self, id: Uuid, r#type: SkuType, lifecycle: Lifecycle, meter: Option<&str>) {
        self.skus.lock().unwrap().insert(
            id,
            Entry {
                r#type,
                lifecycle,
                meter: meter.map(str::to_owned),
                unit: (r#type == SkuType::Usage).then(|| "VM\u{b7}hour".into()),
                name: format!("sku-{}", &id.to_string()[..8]),
                gl_code: None,
            },
        );
    }
    /// Change a declared SKU's GL code, as a published `sku_change` would.
    pub fn describe(&self, id: Uuid, gl_code: &str) {
        self.skus.lock().unwrap().get_mut(&id).unwrap().gl_code = Some(gl_code.to_owned());
    }
    /// A new published SKU of a type.
    pub fn sku(&self, r#type: SkuType) -> Uuid {
        let id = Uuid::new_v4();
        self.put(
            id,
            r#type,
            Lifecycle::Published,
            (r#type == SkuType::Usage).then_some("vm-hours"),
        );
        id
    }
    /// Age a declared SKU.
    pub fn age(&self, id: Uuid, lifecycle: Lifecycle) {
        self.skus.lock().unwrap().get_mut(&id).unwrap().lifecycle = lifecycle;
    }
    pub fn name(&self, id: Uuid) -> String {
        self.skus.lock().unwrap()[&id].name.clone()
    }
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
    pub fn reserves(&self) -> usize {
        self.reserve_kinds.lock().unwrap().len()
    }
    pub fn releases(&self) -> usize {
        self.releases.load(Ordering::SeqCst)
    }
    fn unavailable() -> CanonicalError {
        CanonicalError::service_unavailable().create()
    }
}
#[async_trait::async_trait]
impl ReferenceRegistryV1 for Catalog {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        _: Uuid,
        _: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        self.reference_denied(ctx)?;
        if self.down.load(Ordering::SeqCst) {
            return Err(Self::unavailable());
        }
        self.reserve_kinds.lock().unwrap().push(kind);
        let mut refs = self.refs.lock().unwrap();
        let entry = refs
            .entry(ref_id)
            .or_insert_with(|| (Uuid::new_v4(), ReferenceState::Reserved));
        if entry.1 == ReferenceState::Released {
            *entry = (Uuid::new_v4(), ReferenceState::Reserved);
        }
        Ok(ReservationReceipt {
            reservation_id: entry.0,
            state: entry.1,
        })
    }
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        _: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.reference_denied(ctx)?;
        if self.down.load(Ordering::SeqCst) {
            return Err(Self::unavailable());
        }
        for item in self.refs.lock().unwrap().values_mut() {
            if item.0 == id {
                item.1 = ReferenceState::Confirmed;
            }
        }
        Ok(())
    }
    async fn release(
        &self,
        ctx: &SecurityContext,
        _: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.reference_denied(ctx)?;
        if self.down.load(Ordering::SeqCst) {
            return Err(Self::unavailable());
        }
        let stall = self.stall_releases_ms.load(Ordering::SeqCst);
        if stall > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(stall)).await;
        }
        self.releases.fetch_add(1, Ordering::SeqCst);
        for item in self.refs.lock().unwrap().values_mut() {
            if item.0 == id {
                item.1 = ReferenceState::Released;
            }
        }
        Ok(())
    }
    async fn states(
        &self,
        _: &SecurityContext,
        _: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let refs = self.refs.lock().unwrap();
        Ok(ids
            .iter()
            .map(|id| {
                (
                    *id,
                    refs.values()
                        .find(|v| v.0 == *id)
                        .map_or(ReferenceState::Confirmed, |v| v.1),
                )
            })
            .collect())
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        if self.contended.load(Ordering::SeqCst) {
            return Err(SkuResource::aborted("contended")
                .with_reason("CONTENDED")
                .create());
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.read_denied(ctx)?;
        if self.down.load(Ordering::SeqCst) {
            return Err(Self::unavailable());
        }
        let Some(entry) = self.skus.lock().unwrap().get(&id).cloned() else {
            return Err(SkuResource::not_found("SKU not found")
                .with_resource("sku")
                .create());
        };
        Ok(Sku {
            id,
            tenant_id: tenant,
            code: entry.name.clone(),
            name: entry.name,
            r#type: entry.r#type,
            category_id: Some(Uuid::nil()),
            description: String::new(),
            sellable: true,
            lifecycle: entry.lifecycle,
            revision: 1,
            published_version: 1,
            gl_code: entry.gl_code,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: entry.meter,
            unit: entry.unit,
            type_change_pending: false,
            retire_pending: false,
            lifecycle_next: None,
            pending_unit_id: None,
            approved_by_unit_id: None,
            created_by: Uuid::nil(),
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
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.version_readers.lock().unwrap().push((
            ctx.subject_id(),
            ctx.subject_type().map(str::to_owned),
            tenant,
        ));
        self.read_denied(ctx)?;
        if self.down.load(Ordering::SeqCst) {
            return Err(Self::unavailable());
        }
        let versions = self.versions.lock().unwrap();
        let Some(versions) = versions.as_ref() else {
            return Ok(None);
        };
        if !self.skus.lock().unwrap().contains_key(&id) {
            return Err(SkuResource::not_found("SKU not found")
                .with_resource("sku")
                .create());
        }
        Ok(versions.get(&id).and_then(|chain| {
            chain
                .iter()
                .rev()
                .find(|v| v.effective_from <= date)
                .cloned()
        }))
    }
}

/// A fixture over a fresh catalog.
pub async fn setup() -> (Fixture, Arc<Catalog>) {
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::new(catalog.clone()).await;
    (f, catalog)
}
pub fn id_of(value: &Value) -> Uuid {
    value.as_str().unwrap().parse().unwrap()
}
pub fn scope(f: &Fixture) -> AccessScope {
    AccessScope::for_tenant(f.ctx.subject_tenant_id())
}
/// A principal of the fixture tenant holding exactly one `label:action` grant.
pub fn holding(f: &Fixture, grant: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(f.ctx.subject_tenant_id())
        .subject_type(grant)
        .build()
        .unwrap()
}
/// A user of another tenant.
pub fn stranger() -> SecurityContext {
    entry_support::user_of(Uuid::new_v4())
}
/// A book of the fixture tenant, through its door.
pub async fn book(f: &Fixture, code: &str) -> Uuid {
    let (s, b, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":code,"name":code,"currency":"EUR"}),
            None,
            Some(&format!("book-{code}")),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    id_of(&b["id"])
}
/// An entry of `book` for `sku`, written directly: it costs the catalog no reservation. Its model
/// is `per_unit` (D-427), one every charge kind allows and the money `entry_support::price` writes.
pub async fn entry(
    f: &Fixture,
    book: Uuid,
    sku: Uuid,
    charge_kind: &str,
    period: Option<&str>,
) -> Uuid {
    entry_in(f, book, sku, charge_kind, period, "per_unit").await
}
/// An entry as `entry` writes it, in `model` (D-427).
pub async fn entry_in(
    f: &Fixture,
    book: Uuid,
    sku: Uuid,
    charge_kind: &str,
    period: Option<&str>,
    model: &str,
) -> Uuid {
    entry_with_policy(f, book, sku, charge_kind, period, model, None).await
}
pub async fn entry_with_policy(
    f: &Fixture,
    book: Uuid,
    sku: Uuid,
    charge_kind: &str,
    period: Option<&str>,
    model: &str,
    policy: Option<bss_pricing::infra::usage_policy_wire::UsageRatingPolicy>,
) -> Uuid {
    let now = time::OffsetDateTime::now_utc();
    price_book_entry_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price_book_entry::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            book_id: book,
            sku_id: sku,
            charge_kind: charge_kind.into(),
            period: period.map(str::to_owned),
            model: model.into(),
            usage_policy_id: policy.as_ref().map(|p| p.policy_id),
            usage_policy_version: policy.as_ref().map(|_| 1),
            usage_policy_digest: policy.as_ref().map(|p| p.digest.clone()),
            usage_sku_version: None,
            dimension_key: None,
            invoice_line_override: None,
            reservation_id: Uuid::new_v4(),
            reference_state: "confirmed".into(),
            version: 1,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id
}
/// A plan through its door: `(plan body, revision 1 id)`. Its code is `code` upper-cased, a code
/// the rule of D-468 admits (`pro` gives `PRO`).
pub async fn plan(f: &Fixture, code: &str, book: Uuid) -> (Value, Uuid) {
    let (s, b, _) = f
        .call(
            "POST",
            "/plans",
            json!({"code":code.to_uppercase(),"name":format!("Plan {code}"),"book_id":book}),
            None,
            Some(&format!("plan-{code}")),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    let revision = id_of(&b["revisions"][0]["id"]);
    (b, revision)
}
/// An item written straight into a draft revision, confirmed, with a receipt the catalog does
/// not hold (a release of it is still counted).
pub async fn item(
    f: &Fixture,
    revision: Uuid,
    sku: Uuid,
    entry: Option<Uuid>,
    treatment: &str,
) -> plan_item::Model {
    let now = time::OffsetDateTime::now_utc();
    plan_item_repo::insert_as_given(
        &f.db.conn().unwrap(),
        &scope(f),
        plan_item::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            revision_id: revision,
            sku_id: sku,
            price_book_entry_id: entry,
            treatment: treatment.into(),
            included_qty: None,
            qty_min: None,
            reservation_id: Some(Uuid::new_v4()),
            reference_state: "confirmed".into(),
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
}
/// An included usage item with its quantity, written straight into a draft revision.
pub async fn item_with_qty(f: &Fixture, revision: Uuid, sku: Uuid, qty: &str) -> plan_item::Model {
    let mut m = item(f, revision, sku, None, "included").await;
    let now = time::OffsetDateTime::now_utc();
    m.included_qty = Some(qty.to_owned());
    m.updated_at = now;
    plan_item_repo::update_draft_as_given(&f.db.conn().unwrap(), &scope(f), m.clone())
        .await
        .unwrap();
    m.version += 1;
    m
}
pub async fn items(f: &Fixture, revision: Uuid) -> Vec<plan_item::Model> {
    plan_item_repo::for_revision(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        revision,
    )
    .await
    .unwrap()
}
/// A pending `plan_revision` approval unit, so a lock can name it.
pub async fn unit(f: &Fixture) -> Uuid {
    unit_of_kind(f, "plan_revision").await
}
/// A pending approval unit of any stored kind, with no items, written through the store.
pub async fn unit_of_kind(f: &Fixture, kind: &str) -> Uuid {
    let (id, tenant, scope) = (Uuid::new_v4(), f.ctx.subject_tenant_id(), scope(f));
    let kind = kind.to_owned();
    price_repo::transaction(&f.db.db(), move |tx| {
        let (scope, kind) = (scope.clone(), kind.clone());
        Box::pin(async move {
            PricingApprovalStore {
                scope,
                tenant_id: tenant,
            }
            .insert_unit(
                tx,
                &Unit {
                    id,
                    tenant_id: tenant,
                    ref_type: kind.clone(),
                    kind,
                    ref_id: Uuid::new_v4(),
                    state: UnitState::Pending,
                    common_effective_date: None,
                    quorum_required: 1,
                    generation: 1,
                    submitted_by: Uuid::new_v4(),
                    submitted_at: time::OffsetDateTime::now_utc(),
                    submit_note: None,
                    decided_at: None,
                    decided_note: None,
                    snapshot: json!({}),
                    snapshot_hash: "hash".into(),
                    version: 1,
                },
                &[],
            )
            .await
            .map_err(|e| RepoError::Db(e.to_string()))
        })
    })
    .await
    .unwrap();
    id
}
/// Lock the revision under a pending unit, as the submit door (run 3.4) will.
pub async fn lock(f: &Fixture, revision: Uuid) -> Uuid {
    let u = unit(f).await;
    let tenant = f.ctx.subject_tenant_id();
    let conn = f.db.conn().unwrap();
    let version = plan_revision_repo::find(&conn, &scope(f), tenant, revision)
        .await
        .unwrap()
        .unwrap()
        .version;
    assert!(
        plan_revision_repo::try_lock(&conn, &scope(f), tenant, revision, u, version)
            .await
            .unwrap()
    );
    u
}
/// Lock, publish and project the revision, as an applied unit (run 3.4) will.
pub async fn publish(f: &Fixture, plan: Uuid, revision: Uuid) {
    let u = lock(f, revision).await;
    let tenant = f.ctx.subject_tenant_id();
    let conn = f.db.conn().unwrap();
    let now = time::OffsetDateTime::now_utc();
    let r = plan_revision_repo::find(&conn, &scope(f), tenant, revision)
        .await
        .unwrap()
        .unwrap();
    if let Some(previous) = plan_revision_repo::for_plan(&conn, &scope(f), tenant, plan)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.state == "published")
    {
        plan_revision_repo::supersede(&conn, &scope(f), tenant, previous.id, previous.version, now)
            .await
            .unwrap();
    }
    plan_revision_repo::publish(&conn, &scope(f), tenant, revision, u, now)
        .await
        .unwrap();
    let p = plan_repo::find(&conn, &scope(f), tenant, plan)
        .await
        .unwrap()
        .unwrap();
    plan_repo::set_published(&conn, &scope(f), tenant, plan, p.version, r.rev_no, now)
        .await
        .unwrap();
}
/// The tenant's durable reference ops for one reference.
pub async fn ops_for(
    f: &Fixture,
    ref_id: Uuid,
) -> Vec<bss_pricing::infra::storage::entity::reference_op::Model> {
    reference_op_repo::page(
        &f.db.conn().unwrap(),
        &scope(f),
        f.ctx.subject_tenant_id(),
        None,
        None,
        1000,
    )
    .await
    .unwrap()
    .into_iter()
    .filter(|op| op.ref_id == ref_id)
    .collect()
}
/// Run raw SQL on the fixture's database file (a probe's trigger).
pub async fn raw(f: &Fixture, sql: &str) {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    Database::connect(&f.dsn)
        .await
        .unwrap()
        .execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .unwrap();
}
/// The problem body's text, where a code is looked for.
pub fn text(b: &Value) -> String {
    b.to_string()
}

/// A modern fixture entry, with policy evidence required by new publication.
pub async fn policy_entry(
    f: &Fixture,
    book: Uuid,
    sku: Uuid,
    kind: &str,
    period: Option<&str>,
) -> Uuid {
    let policy = if kind == "usage" {
        let content = serde_json::from_value(entry_support::policy_support::input()).unwrap();
        Some(
            bss_pricing::infra::storage::repo::usage_policy_repo::intern(
                &f.db.conn().unwrap(),
                &scope(f),
                f.ctx.subject_tenant_id(),
                f.ctx.subject_id(),
                &content,
                time::OffsetDateTime::now_utc(),
            )
            .await
            .unwrap(),
        )
    } else {
        None
    };
    entry_with_policy(f, book, sku, kind, period, "per_unit", policy).await
}
