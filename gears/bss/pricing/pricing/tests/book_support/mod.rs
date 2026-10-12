//! Shared helpers of the book reads' suites (D-440 to D-442) on both dialects: books, entries,
//! prices and units written through the repositories with the timestamps a test chooses, and
//! the reads' small accessors.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::plan_support::{Fixture, entry_support, id_of, scope};
use bss_approval::{Store, Unit, UnitState};
use bss_pricing::infra::storage::{
    RepoError,
    entity::{price, price_book, price_book_entry},
    repo::{approval_repo::PricingApprovalStore, book_repo, price_book_entry_repo, price_repo},
};
use serde_json::{Value, json};
use uuid::Uuid;

pub fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}
pub fn days(n: i64) -> time::Duration {
    time::Duration::days(n)
}
pub fn instant(text: &str) -> time::OffsetDateTime {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).unwrap()
}
pub async fn get(f: &Fixture, path: &str) -> (u16, Value, String) {
    f.call("GET", path, json!({}), None, None).await
}
pub async fn ok(f: &Fixture, path: &str) -> Value {
    let (s, b, _) = get(f, path).await;
    assert_eq!(s, 200, "{path}: {b}");
    b
}
pub fn ids(b: &Value) -> Vec<String> {
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}
pub fn codes(b: &Value) -> Vec<String> {
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap().to_owned())
        .collect()
}
pub fn code_of(b: &Value) -> String {
    b.to_string()
}

/// A book of the fixture tenant written through the repository, updated at `updated`.
pub async fn stored_book(f: &Fixture, code: &str, updated: time::OffsetDateTime) -> Uuid {
    book_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price_book::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            code: code.into(),
            name: code.into(),
            currency: "EUR".into(),
            valid_from: None,
            valid_until: None,
            description: None,
            version: 1,
            created_at: updated,
            updated_at: updated,
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap()
    .id
}
/// An entry of `book` for `sku` in `model`, keyed by the region dimension, updated at `updated`.
pub async fn stored_entry(
    f: &Fixture,
    book: Uuid,
    sku: Uuid,
    model: &str,
    updated: time::OffsetDateTime,
) -> Uuid {
    price_book_entry_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price_book_entry::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            book_id: book,
            sku_id: sku,
            charge_kind: "usage".into(),
            period: None,
            model: model.into(),
            usage_policy_id: None,
            usage_policy_version: None,
            usage_policy_digest: None,
            usage_sku_version: None,
            dimension_key: Some("region".into()),
            invoice_line_override: None,
            reservation_id: Uuid::new_v4(),
            reference_state: "confirmed".into(),
            version: 1,
            created_at: updated,
            updated_at: updated,
        },
    )
    .await
    .unwrap()
    .id
}
/// One stored price of a chain.
pub struct Row {
    pub version_no: i32,
    pub state: &'static str,
    pub dim: Option<&'static str>,
    pub from: time::Date,
    pub to: Option<time::Date>,
    pub updated: time::OffsetDateTime,
}
impl Row {
    pub fn new(version_no: i32, state: &'static str, from: time::Date) -> Self {
        Self {
            version_no,
            state,
            dim: None,
            from,
            to: None,
            updated: time::OffsetDateTime::now_utc(),
        }
    }
    pub fn to(self, to: time::Date) -> Self {
        Self {
            to: Some(to),
            ..self
        }
    }
    pub fn on(self, dim: &'static str) -> Self {
        Self {
            dim: Some(dim),
            ..self
        }
    }
    pub fn updated(self, updated: time::OffsetDateTime) -> Self {
        Self { updated, ..self }
    }
}
/// A price written straight through the repository (the doors cannot write an approved, pending
/// or rejected price without a unit).
pub async fn stored_price(f: &Fixture, entry: Uuid, row: Row) -> price::Model {
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), f.ctx.subject_tenant_id(), entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = entry_support::price(&e);
    p.id = Uuid::now_v7();
    p.version_no = row.version_no;
    p.state = row.state.into();
    p.dim_value = row.dim.map(str::to_owned);
    p.effective_from = row.from;
    p.effective_to = row.to;
    p.created_at = row.updated;
    p.updated_at = row.updated;
    price_repo::insert(&conn, &scope(f), p).await.unwrap()
}
/// A `prices` unit on `book` (its `ref_id`), or another kind's unit naming the same id.
pub async fn unit_on(
    f: &Fixture,
    kind: &str,
    book: Uuid,
    state: UnitState,
    submitted: time::OffsetDateTime,
    decided: Option<time::OffsetDateTime>,
) -> Uuid {
    let (id, tenant, scope) = (Uuid::now_v7(), f.ctx.subject_tenant_id(), scope(f));
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
                    ref_type: if kind == "prices" {
                        "price_book".into()
                    } else {
                        kind.clone()
                    },
                    kind,
                    ref_id: book,
                    state,
                    common_effective_date: None,
                    quorum_required: 1,
                    generation: 1,
                    submitted_by: Uuid::new_v4(),
                    submitted_at: submitted,
                    submit_note: None,
                    decided_at: decided,
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

/// A draft revision `rev_no` of `plan` on `book`, with no items, written through the repository.
pub async fn bare_revision(f: &Fixture, plan: Uuid, rev_no: i32, book: Uuid) -> Uuid {
    use bss_pricing::infra::storage::{entity::plan_revision, repo::plan_revision_repo};
    let now = time::OffsetDateTime::now_utc();
    plan_revision_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        plan_revision::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            plan_id: plan,
            rev_no,
            book_id: book,
            state: "draft".into(),
            available_from: None,
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id
}

/// A book through its door.
pub async fn door_book(
    f: &Fixture,
    code: &str,
    name: &str,
    currency: &str,
    from: Option<&str>,
    until: Option<&str>,
) -> Uuid {
    let (s, b, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":code,"name":name,"currency":currency,"valid_from":from,"valid_until":until}),
            None,
            // A header is ASCII: the key is not the code, which may not be.
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    id_of(&b["id"])
}

/// A query string with its `%` and its spaces percent-encoded, as a client sends them.
pub fn encode(q: &str) -> String {
    q.replace('%', "%25").replace(' ', "%20")
}
