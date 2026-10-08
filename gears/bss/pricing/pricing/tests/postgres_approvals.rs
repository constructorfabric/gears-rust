//! `prices` approvals on native Postgres: concurrent units on one chain and the
//! approved-start index behind the apply write.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
mod pg_support;
use bss_approval::{Store, Unit, UnitState};
use bss_pricing::infra::storage::{
    RepoError,
    entity::price,
    repo::{approval_repo::PricingApprovalStore, price_book_entry_repo, price_repo},
};
use entry_support::policy_support;
use entry_support::{Script, app_for, request, state_on, user_of};
use serde_json::{Value, json};
use std::sync::Arc;
use time::Date;
use toolkit_db::{DBProvider, DbError, secure::AccessScope};
use uuid::Uuid;

fn day(s: &str) -> Date {
    Date::parse(s, &time::format_description::well_known::Iso8601::DATE).unwrap()
}
fn body(from: &str) -> Value {
    json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":from})
}

/// Two authoring processes on their own pools of one database, one book and one usage entry
/// whose default chain already has an approved price.
struct Two {
    pg: pg_support::Pg,
    a: axum::Router,
    b: axum::Router,
    db: DBProvider<DbError>,
    tenant: Uuid,
    entry: Uuid,
}
async fn two() -> Two {
    let pg = pg_support::Pg::applied().await;
    let tenant = Uuid::new_v4();
    let script: Arc<dyn bss_products_sdk::ReferenceRegistryV1> = Arc::new(Script::default());
    let db = DBProvider::<DbError>::new(pg.db().await);
    let a = app_for(state_on(db.clone(), script.clone()).await, tenant);
    let b = app_for(
        state_on(DBProvider::<DbError>::new(pg.db().await), script).await,
        tenant,
    );
    let author = user_of(tenant);
    let (s, book, _) = request(
        &a,
        &author,
        "POST",
        "/price-books",
        json!({"code":"standard","name":"Standard","currency":"EUR"}),
        None,
        Some("book"),
    )
    .await;
    assert_eq!(s, 201, "{book}");
    let (s, entry, _) = request(
        &a,
        &author,
        "POST",
        &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
        json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"}),
        None,
        Some("entry"),
    )
    .await;
    assert_eq!(s, 201, "{entry}");
    let (_, _, tag) = request(
        &a,
        &author,
        "GET",
        "/approval-policy",
        json!({}),
        None,
        None,
    )
    .await;
    let (s, _, _) = request(
        &a,
        &author,
        "PUT",
        "/approval-policy",
        json!({"quorum":1}),
        Some(&tag),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let entry: Uuid = entry["id"].as_str().unwrap().parse().unwrap();
    let scope = AccessScope::for_tenant(tenant);
    let conn = db.conn().unwrap();
    let p = price_book_entry_repo::find(&conn, &scope, tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut base = entry_support::price(&p);
    base.state = "approved".into();
    base.effective_from = day("2031-01-01");
    price_repo::insert(&conn, &scope, base).await.unwrap();
    Two {
        pg,
        a,
        b,
        db,
        tenant,
        entry,
    }
}
impl Two {
    /// A draft submitted as its own pending unit; returns the unit id.
    async fn unit(&self, key: &str, from: &str) -> String {
        let author = user_of(self.tenant);
        let (s, created, _) = request(
            &self.a,
            &author,
            "POST",
            &format!("/price-book-entries/{}/prices", self.entry),
            body(from),
            None,
            Some(key),
        )
        .await;
        assert_eq!(s, 201, "{created}");
        let (s, receipt, _) = request(
            &self.a,
            &author,
            "POST",
            &format!(
                "/prices/{}/submit",
                created["items"][0]["id"].as_str().unwrap()
            ),
            json!({}),
            None,
            Some(key),
        )
        .await;
        assert_eq!(s, 201, "{receipt}");
        receipt["unit"]["id"].as_str().unwrap().to_owned()
    }
    async fn approved_chain(&self) -> Vec<price::Model> {
        let mut prices: Vec<_> = price_repo::for_entry(
            &self.db.conn().unwrap(),
            &AccessScope::for_tenant(self.tenant),
            self.tenant,
            self.entry,
        )
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.state == "approved" && r.dim_value.is_none())
        .collect();
        prices.sort_by_key(|r| r.effective_from);
        prices
    }
    async fn race(
        &self,
        first: &str,
        second: &str,
    ) -> ((u16, Value, String), (u16, Value, String)) {
        let (one, two) = (user_of(self.tenant), user_of(self.tenant));
        let (left, right) = (
            format!("/approval-units/{first}/approve"),
            format!("/approval-units/{second}/approve"),
        );
        tokio::join!(
            request(
                &self.a,
                &one,
                "POST",
                &left,
                json!({"generation":1}),
                None,
                Some("one"),
            ),
            request(
                &self.b,
                &two,
                "POST",
                &right,
                json!({"generation":1}),
                None,
                Some("two"),
            )
        )
    }
}
/// No two approved prices of the chain are in force on the same date.
fn assert_no_overlap(chain: &[price::Model]) {
    for pair in chain.windows(2) {
        assert!(pair[0].effective_from < pair[1].effective_from, "{chain:?}");
        assert!(
            pair[0]
                .effective_to
                .is_some_and(|end| end <= pair[1].effective_from),
            "{chain:?}"
        );
    }
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_two_concurrent_approvals_on_one_chain_never_overlap() {
    // Different starts: both may apply, each re-reading the other's committed price.
    let db = two().await;
    let (march, june) = (
        db.unit("m", "2031-03-01").await,
        db.unit("j", "2031-06-01").await,
    );
    let (first, second) = db.race(&march, &june).await;
    for answer in [&first, &second] {
        assert!(
            answer.0 == 200 || answer.0 == 409,
            "a lost race is a refusal, never a failure: {answer:?}"
        );
    }
    let chain = db.approved_chain().await;
    assert_eq!(
        chain.len(),
        1 + usize::from(first.0 == 200) + usize::from(second.0 == 200),
        "{chain:?}"
    );
    assert_no_overlap(&chain);
    if first.0 == 200 && second.0 == 200 {
        assert_eq!(chain[0].effective_to, Some(day("2031-03-01")));
        assert_eq!(chain[1].effective_to, Some(day("2031-06-01")));
        assert_eq!(chain[2].effective_to, None);
    }
    // The same start: exactly one unit applies, the other is refused whole.
    let db = two().await;
    let (april, again) = (
        db.unit("x", "2031-04-01").await,
        db.unit("y", "2031-04-01").await,
    );
    let (first, second) = db.race(&april, &again).await;
    let wins = [&first, &second].iter().filter(|r| r.0 == 200).count();
    assert_eq!(wins, 1, "{first:?} {second:?}");
    let loser = if first.0 == 200 { &second } else { &first };
    assert_eq!(loser.0, 409, "{loser:?}");
    assert!(loser.1.to_string().contains("APPLY_REFUSED"), "{loser:?}");
    let chain = db.approved_chain().await;
    assert_eq!(chain.len(), 2);
    assert_no_overlap(&chain);
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_the_approved_start_index_refuses_an_apply_onto_a_taken_start() {
    let t = two().await;
    let conn = t.db.conn().unwrap();
    let scope = AccessScope::for_tenant(t.tenant);
    let p = price_book_entry_repo::find(&conn, &scope, t.tenant, t.entry)
        .await
        .unwrap()
        .unwrap();
    let unit = Unit {
        id: Uuid::new_v4(),
        tenant_id: t.tenant,
        kind: "prices".into(),
        ref_type: "price_book".into(),
        ref_id: p.book_id,
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
        snapshot_hash: "h".into(),
        version: 1,
    };
    let mut draft = entry_support::price(&p);
    draft.version_no = 2;
    draft.effective_from = day("2031-05-01");
    let draft = price_repo::insert(&conn, &scope, draft).await.unwrap();
    let store = PricingApprovalStore {
        scope: scope.clone(),
        tenant_id: t.tenant,
    };
    let tx_scope = scope.clone();
    let result = price_repo::transaction(&t.db.db(), move |tx| {
        let (store, unit, scope) = (store.clone(), unit.clone(), tx_scope.clone());
        Box::pin(async move {
            store
                .insert_unit(tx, &unit, &[])
                .await
                .map_err(|e| RepoError::Db(e.to_string()))?;
            assert!(price_repo::try_lock(tx, &scope, unit.tenant_id, draft.id, unit.id, 1).await?);
            // The pure re-check is bypassed on purpose: only the index stands here.
            price_repo::approve(
                tx,
                &scope,
                unit.tenant_id,
                draft.id,
                unit.id,
                price_repo::Approval {
                    effective_from: day("2031-01-01"),
                    effective_to: None,
                    temporary_until: None,
                    keep_for_bound: false,
                },
                time::OffsetDateTime::now_utc(),
            )
            .await
        })
    })
    .await;
    assert!(
        matches!(
            result,
            Err(RepoError::Conflict {
                code: "WINDOW_OVERLAP"
            })
        ),
        "{result:?}"
    );
    let still = price_repo::find(&conn, &scope, t.tenant, draft.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.state, "draft", "the refused transaction rolled back");
}

/// Every envelope type in the Postgres outbox, in enqueue order.
async fn event_types(pg: &pg_support::Pg) -> Vec<String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    pg.raw()
        .await
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT convert_from(payload, 'UTF8')::jsonb ->> 'type' AS t \
             FROM public.bss_pricing_outbox_body ORDER BY id"
                .to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| r.try_get::<String>("", "t").unwrap())
        .collect()
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_an_apply_commits_its_prices_and_both_events_and_a_refused_one_leaves_none() {
    let t = two().await;
    let published = "gts.cf.core.events.event.v1~cf.bss.pricing.prices_published.v1~";
    let decided = "gts.cf.core.events.event.v1~cf.bss.pricing.approval_unit_decided.v1~";
    let march = t.unit("m", "2031-03-01").await;
    let (s, b, _) = request(
        &t.b,
        &user_of(t.tenant),
        "POST",
        &format!("/approval-units/{march}/approve"),
        json!({"generation":1}),
        None,
        Some("m"),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(event_types(&t.pg).await, vec![published, decided]);
    assert_eq!(t.approved_chain().await.len(), 2);

    // A price approved on the unit's start underneath it: the apply is refused and rolls back.
    let june = t.unit("j", "2031-06-01").await;
    let conn = t.db.conn().unwrap();
    let scope = AccessScope::for_tenant(t.tenant);
    let p = price_book_entry_repo::find(&conn, &scope, t.tenant, t.entry)
        .await
        .unwrap()
        .unwrap();
    let mut taken = entry_support::price(&p);
    taken.version_no = 99;
    taken.state = "approved".into();
    taken.effective_from = day("2031-06-01");
    price_repo::insert(&conn, &scope, taken).await.unwrap();
    let (s, b, _) = request(
        &t.b,
        &user_of(t.tenant),
        "POST",
        &format!("/approval-units/{june}/approve"),
        json!({"generation":1}),
        None,
        Some("j"),
    )
    .await;
    assert_eq!(s, 409, "{b}");
    assert!(b.to_string().contains("APPLY_REFUSED"), "{b}");
    assert_eq!(
        event_types(&t.pg).await,
        vec![published, decided],
        "the refused apply left no event"
    );
}
