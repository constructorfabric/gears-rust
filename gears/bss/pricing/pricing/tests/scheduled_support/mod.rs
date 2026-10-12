//! The family `m20260929_000017` rebuilds on `SQLite` (D-446), seeded through the gear's own
//! repositories on the chain before it, so every value is bound as the application binds it: two
//! plans, their revisions in every state the old CHECK admits, and items with and without an entry.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_approval::{Store, Unit, UnitState};
use bss_pricing::infra::storage::{
    RepoError,
    entity::{plan as plan_e, plan_item, plan_revision, price_book, price_book_entry},
    repo::{
        approval_repo::PricingApprovalStore, book_repo, plan_item_repo, plan_repo,
        plan_revision_repo, price_book_entry_repo, price_repo,
    },
};
use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, Db, DbError};
use uuid::Uuid;

/// What [`seed`] wrote.
pub struct Family {
    pub alpha: Uuid,
    pub beta: Uuid,
    /// Alpha rev 1, published and then superseded by rev 2.
    pub superseded: Uuid,
    /// Alpha rev 2.
    pub published: Uuid,
    /// Alpha rev 3, pending in `pending_unit`.
    pub pending: Uuid,
    pub pending_unit: Uuid,
    /// Beta rev 1.
    pub draft: Uuid,
    pub items: usize,
}

/// `hour` o'clock UTC on 2026-09-27: whole seconds, which both dialects read back equal.
#[must_use]
pub fn at(hour: u8) -> time::OffsetDateTime {
    time::Date::from_calendar_date(2026, time::Month::September, 27)
        .unwrap()
        .with_hms(hour, 0, 0)
        .unwrap()
        .assume_utc()
}
fn day(d: u8) -> time::Date {
    time::Date::from_calendar_date(2026, time::Month::October, d).unwrap()
}

async fn unit(db: &DBProvider<DbError>, scope: &AccessScope, tenant: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let scope = scope.clone();
    price_repo::transaction(&db.db(), move |tx| {
        let scope = scope.clone();
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
                    kind: "plan_revision".into(),
                    ref_type: "plan_revision".into(),
                    ref_id: Uuid::new_v4(),
                    state: UnitState::Pending,
                    common_effective_date: None,
                    quorum_required: 1,
                    generation: 1,
                    submitted_by: Uuid::new_v4(),
                    submitted_at: at(9),
                    submit_note: None,
                    decided_at: None,
                    decided_note: None,
                    snapshot: serde_json::json!({}),
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
fn entry(
    book: &price_book::Model,
    charge_kind: &str,
    period: Option<&str>,
    model: &str,
) -> price_book_entry::Model {
    price_book_entry::Model {
        id: Uuid::new_v4(),
        tenant_id: book.tenant_id,
        book_id: book.id,
        sku_id: Uuid::new_v4(),
        charge_kind: charge_kind.into(),
        period: period.map(str::to_owned),
        model: model.into(),
        usage_policy_id: None,
        usage_policy_version: None,
        usage_policy_digest: None,
        usage_sku_version: None,
        dimension_key: None,
        invoice_line_override: Some("{name} per month".into()),
        reservation_id: Uuid::new_v4(),
        reference_state: "confirmed".into(),
        version: 1,
        created_at: at(9),
        updated_at: at(9),
    }
}
fn plan(tenant: Uuid, code: &str) -> plan_e::Model {
    plan_e::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        code: code.into(),
        name: format!("Plan {code}"),
        published_rev: None,
        version: 1,
        created_by: Uuid::new_v4(),
        created_at: at(9),
        updated_at: at(9),
        work_revision_id: None,
        work_state: None,
        scheduled_revision_id: None,
        scheduled_from: None,
        published_revision_id: None,
        current_book_id: None,
        current_currency: None,
        last_activity_at: at(9),
    }
}
fn revision(
    p: &plan_e::Model,
    book: &price_book::Model,
    rev_no: i32,
    available_from: Option<time::Date>,
) -> plan_revision::Model {
    plan_revision::Model {
        id: Uuid::new_v4(),
        tenant_id: p.tenant_id,
        plan_id: p.id,
        rev_no,
        book_id: book.id,
        state: "draft".into(),
        available_from,
        pending_unit_id: None,
        approved_by_unit_id: None,
        published_at: None,
        version: 1,
        created_by: Uuid::new_v4(),
        created_at: at(9),
        updated_at: at(9),
    }
}
/// An item of `r`: priced by `e` when given (the entry's SKU), else an included item with a
/// quantity and no entry.
fn item(
    r: &plan_revision::Model,
    e: Option<&price_book_entry::Model>,
    treatment: &str,
) -> plan_item::Model {
    let confirmed = e.is_some();
    plan_item::Model {
        id: Uuid::new_v4(),
        tenant_id: r.tenant_id,
        revision_id: r.id,
        sku_id: e.map_or_else(Uuid::new_v4, |e| e.sku_id),
        price_book_entry_id: e.map(|e| e.id),
        treatment: treatment.into(),
        included_qty: (treatment == "included").then(|| "10.5".to_owned()),
        qty_min: Some(i32::from(confirmed)),
        reservation_id: confirmed.then(Uuid::new_v4),
        reference_state: if confirmed { "confirmed" } else { "unreserved" }.into(),
        version: 1,
        created_by: Uuid::new_v4(),
        created_at: at(9),
        updated_at: at(9),
    }
}

/// The seeding tenant's repositories, and the count of items written.
struct Seeder {
    provider: DBProvider<DbError>,
    scope: AccessScope,
    tenant: Uuid,
    book: price_book::Model,
    items: usize,
}
impl Seeder {
    /// A draft revision of `p` with `items` (an entry, or none for an included item; the treatment).
    async fn draft(
        &mut self,
        p: &plan_e::Model,
        rev_no: i32,
        from: Option<time::Date>,
        items: &[(Option<&price_book_entry::Model>, &str)],
    ) -> plan_revision::Model {
        let conn = self.provider.conn().unwrap();
        let r =
            plan_revision_repo::insert(&conn, &self.scope, revision(p, &self.book, rev_no, from))
                .await
                .unwrap();
        for (e, treatment) in items {
            plan_item_repo::insert_as_given(&conn, &self.scope, item(&r, *e, treatment))
                .await
                .unwrap();
            self.items += 1;
        }
        r
    }
    /// A new unit locks the draft `id` (at version 1).
    async fn lock(&self, id: Uuid) -> Uuid {
        let u = unit(&self.provider, &self.scope, self.tenant).await;
        assert!(
            plan_revision_repo::try_lock(
                &self.provider.conn().unwrap(),
                &self.scope,
                self.tenant,
                id,
                u,
                1
            )
            .await
            .unwrap()
        );
        u
    }
    /// Publish the pending revision `id` of `p` held by `u` at `hour`, superseding `previous` (at
    /// its version) first, and advance the plan's projection. The plan moves once per publish, so
    /// its version before the publish of rev N is N.
    async fn publish(
        &self,
        p: &plan_e::Model,
        id: Uuid,
        rev_no: i32,
        u: Uuid,
        previous: Option<(Uuid, i64)>,
        hour: u8,
    ) {
        let conn = self.provider.conn().unwrap();
        if let Some((previous, version)) = previous {
            plan_revision_repo::supersede(
                &conn,
                &self.scope,
                self.tenant,
                previous,
                version,
                at(hour),
            )
            .await
            .unwrap();
        }
        plan_revision_repo::publish(&conn, &self.scope, self.tenant, id, u, at(hour))
            .await
            .unwrap();
        let plan_version = i64::from(rev_no);
        plan_repo::set_published(
            &conn,
            &self.scope,
            self.tenant,
            p.id,
            plan_version,
            rev_no,
            at(hour),
        )
        .await
        .unwrap();
    }
}
/// Seed the family for `tenant` on `db`, a database migrated without 000017.
pub async fn seed(db: Db, tenant: Uuid) -> Family {
    let provider = DBProvider::<DbError>::new(db);
    let scope = AccessScope::for_tenant(tenant);
    let conn = provider.conn().unwrap();
    let book = book_repo::insert(
        &conn,
        &scope,
        price_book::Model {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            code: "eur".into(),
            name: "Default EUR".into(),
            currency: "EUR".into(),
            valid_from: None,
            valid_until: None,
            description: Some("The house book".into()),
            version: 1,
            created_at: at(9),
            updated_at: at(9),
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap();
    let storage =
        price_book_entry_repo::insert(&conn, &scope, entry(&book, "usage", None, "per_unit"))
            .await
            .unwrap();
    let seat = price_book_entry_repo::insert(
        &conn,
        &scope,
        entry(&book, "recurring", Some("month"), "flat"),
    )
    .await
    .unwrap();
    let alpha = plan_repo::insert(&conn, &scope, plan(tenant, "alpha"))
        .await
        .unwrap();
    let beta = plan_repo::insert(&conn, &scope, plan(tenant, "beta"))
        .await
        .unwrap();
    let mut s = Seeder {
        provider,
        scope,
        tenant,
        book,
        items: 0,
    };
    // Alpha rev 1: published at 10:00, then superseded by rev 2 at 12:00 (rev 1 at version 3:
    // inserted, locked, published).
    let a1 = s
        .draft(
            &alpha,
            1,
            None,
            &[(Some(&storage), "paid"), (None, "included")],
        )
        .await;
    let u1 = s.lock(a1.id).await;
    s.publish(&alpha, a1.id, 1, u1, None, 10).await;
    let a2 = s
        .draft(
            &alpha,
            2,
            Some(day(1)),
            &[(Some(&storage), "paid"), (Some(&seat), "optional")],
        )
        .await;
    let u2 = s.lock(a2.id).await;
    s.publish(&alpha, a2.id, 2, u2, Some((a1.id, 3)), 12).await;
    // Alpha rev 3: pending in its unit.
    let a3 = s
        .draft(&alpha, 3, Some(day(15)), &[(Some(&seat), "paid")])
        .await;
    let u3 = s.lock(a3.id).await;
    // Beta rev 1: a draft.
    let b1 = s
        .draft(&beta, 1, None, &[(Some(&storage), "optional")])
        .await;
    Family {
        alpha: alpha.id,
        beta: beta.id,
        superseded: a1.id,
        published: a2.id,
        pending: a3.id,
        pending_unit: u3,
        draft: b1.id,
        items: s.items,
    }
}
