//! `PricesPublished` and `ApprovalUnitDecided` through the toolkit outbox (D-400): written
//! in the transaction that ends a unit, in the broker's producer-outbox envelope, and absent
//! whenever that transaction does not commit.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
use bss_pricing::infra::{
    events::{
        APPROVAL_UNIT_SUBJECT_TYPE, ApprovalUnitDecided, PRICE_BOOK_SUBJECT_TYPE, PricesPublished,
        PublishedPrice, SOURCE, TOPIC,
    },
    reference_events::{PlanReferenceLost, PriceBookEntryReferenceLost},
    storage::{
        entity::price,
        repo::{price_book_entry_repo, price_repo},
    },
};
use entry_support::policy_support;
use entry_support::{Fixture, Script};
use event_broker_sdk::TypedEvent;
use sea_orm::{ConnectionTrait, Database, DbBackend, EntityTrait, Statement};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_db::secure::{AccessScope, SecureUpdateExt};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const PUBLISHED: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.prices_published.v1~";
const DECIDED: &str = "gts.cf.core.events.event.v1~cf.bss.pricing.approval_unit_decided.v1~";

struct Gov {
    f: Fixture,
    book: String,
    entry: Value,
}
async fn gov(quorum: u32) -> Gov {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let book = book["id"].as_str().unwrap().to_owned();
    let (status, entry, _) = f
        .call(
            "POST",
            &format!("/price-books/{book}/entries"),
            json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"}),
            None,
            Some("entry"),
        )
        .await;
    assert_eq!(status, 201, "{entry}");
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let put = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"quorum":quorum}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(put.0, 200, "{put:?}");
    Gov { f, book, entry }
}
fn body(from: &str, eligibility: &str) -> Value {
    json!({"price":{"rate":"0.10"},"eligibility":eligibility,"effective_from":from})
}
impl Gov {
    fn price_book_entry_id(&self) -> Uuid {
        self.entry["id"].as_str().unwrap().parse().unwrap()
    }
    async fn draft(&self, key: &str, body: Value) -> Value {
        let (status, b, _) = self
            .f
            .call(
                "POST",
                &format!("/price-book-entries/{}/prices", self.price_book_entry_id()),
                body,
                None,
                Some(key),
            )
            .await;
        assert_eq!(status, 201, "{b}");
        b["items"][0].clone()
    }
    async fn submit(&self, who: &SecurityContext, price: &Value, key: &str) -> Value {
        let (status, receipt, _) = self
            .f
            .call_as(
                who,
                "POST",
                &format!("/prices/{}/submit", price["id"].as_str().unwrap()),
                json!({}),
                None,
                Some(key),
            )
            .await;
        assert_eq!(status, 201, "{receipt}");
        receipt["unit"].clone()
    }
    async fn vote(
        &self,
        who: &SecurityContext,
        unit: &Value,
        action: &str,
        body: Value,
        key: &str,
    ) -> (u16, Value, String) {
        self.f
            .call_as(
                who,
                "POST",
                &format!("/approval-units/{}/{action}", unit["id"].as_str().unwrap()),
                body,
                None,
                Some(key),
            )
            .await
    }
    async fn price(&self, id: &Value) -> price::Model {
        let tenant = self.f.ctx.subject_tenant_id();
        price_repo::find(
            &self.f.db.conn().unwrap(),
            &AccessScope::for_tenant(tenant),
            tenant,
            id.as_str().unwrap().parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn raw(&self, sql: &str) -> Vec<sea_orm::QueryResult> {
        Database::connect(&self.f.dsn)
            .await
            .unwrap()
            .query_all_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .unwrap()
    }
    /// Every envelope in the queue, decoded with the broker SDK's own decoder.
    async fn envelopes(&self) -> Vec<Value> {
        self.raw("SELECT CAST(payload AS TEXT) AS payload FROM bss_pricing_outbox_body ORDER BY id")
            .await
            .iter()
            .map(|r| {
                let text = r.try_get::<String>("", "payload").unwrap();
                let envelope: event_broker_sdk::producer::ProducerOutboxEnvelope =
                    serde_json::from_str(&text).expect("the broker SDK decodes the envelope");
                serde_json::to_value(envelope).unwrap()
            })
            .collect()
    }
    async fn of(&self, type_id: &str) -> Vec<Value> {
        self.envelopes()
            .await
            .into_iter()
            .filter(|e| e["type"] == type_id)
            .collect()
    }
    async fn count(&self, sql: &str) -> i64 {
        self.raw(sql).await[0].try_get::<i64>("", "n").unwrap()
    }
}
fn id(v: &Value) -> Uuid {
    v.as_str().unwrap().parse().unwrap()
}

#[test]
fn three_event_contracts_have_stable_ids_subjects_and_camel_case_payloads() {
    let (tenant, book, unit, price, entry, actor) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let published = PricesPublished {
        tenant_id: tenant,
        book_id: book,
        unit_id: unit,
        prices: vec![PublishedPrice {
            price_id: price,
            price_book_entry_id: entry,
            dim_value: Some("eu".into()),
            effective_from: "2031-03-01".into(),
            effective_to: None,
            eligibility: "new".into(),
            state: Some("cancelled".into()),
        }],
        actor_ref: actor,
    };
    assert_eq!(PricesPublished::TYPE_ID, PUBLISHED);
    assert_eq!(PricesPublished::SUBJECT_TYPE, PRICE_BOOK_SUBJECT_TYPE);
    assert_eq!(
        PRICE_BOOK_SUBJECT_TYPE,
        "gts.cf.core.events.subject.v1~cf.bss.pricing.price_book.v1~"
    );
    assert_eq!(PricesPublished::SOURCE, "bss-pricing");
    assert_eq!(published.subject(), book.to_string());
    assert_eq!(published.tenant_id(), Some(tenant));
    assert_eq!(
        serde_json::to_value(&published).unwrap(),
        json!({"tenantId":tenant,"bookId":book,"unitId":unit,"prices":[{"priceId":price,"priceBookEntryId":entry,
            "dimValue":"eu","effectiveFrom":"2031-03-01","effectiveTo":null,"eligibility":"new",
            "state":"cancelled"}],"actorRef":actor})
    );
    // D-520 amended: `state` is additive. An envelope written before it still reads, with none.
    let older: PricesPublished = serde_json::from_value(
        json!({"tenantId":tenant,"bookId":book,"unitId":unit,"prices":[{"priceId":price,
            "priceBookEntryId":entry,"dimValue":null,"effectiveFrom":"2031-03-01",
            "effectiveTo":null,"eligibility":"all"}],"actorRef":actor}),
    )
    .unwrap();
    assert_eq!(older.prices[0].state, None);
    assert!(
        !serde_json::to_value(&older).unwrap()["prices"][0]
            .as_object()
            .unwrap()
            .contains_key("state"),
        "an absent state is not written as null"
    );
    let decided = ApprovalUnitDecided {
        tenant_id: tenant,
        unit_id: unit,
        kind: "prices".into(),
        state: "withdrawn".into(),
        generation: 3,
        actors: vec![actor],
    };
    assert_eq!(ApprovalUnitDecided::TYPE_ID, DECIDED);
    assert_eq!(
        ApprovalUnitDecided::SUBJECT_TYPE,
        APPROVAL_UNIT_SUBJECT_TYPE
    );
    assert_eq!(
        APPROVAL_UNIT_SUBJECT_TYPE,
        "gts.cf.core.events.subject.v1~cf.bss.pricing.approval_unit.v1~"
    );
    assert_eq!(ApprovalUnitDecided::SOURCE, SOURCE);
    assert_eq!(decided.subject(), unit.to_string());
    assert_eq!(decided.tenant_id(), Some(tenant));
    assert_eq!(
        serde_json::to_value(&decided).unwrap(),
        json!({"tenantId":tenant,"unitId":unit,"kind":"prices","state":"withdrawn",
            "generation":3,"actors":[actor]})
    );
    assert_eq!(
        PriceBookEntryReferenceLost::TYPE_ID,
        "gts.cf.core.events.event.v1~cf.bss.pricing.price_book_entry_reference_lost.v1~"
    );
    assert_eq!(PriceBookEntryReferenceLost::SOURCE, SOURCE);
    assert_eq!(
        TOPIC,
        "gts.cf.core.events.topic.v1~cf.bss.pricing.catalog.v1"
    );
}

#[tokio::test]
async fn quorum_zero_publish_announces_the_normalised_prices_and_the_decision_in_its_transaction() {
    let g = gov(0).await;
    let first = g.draft("a", body("2031-03-01", "all")).await;
    let second = g.draft("b", body("2031-06-01", "new")).await;
    let (status, receipt, _) =
        g.f.call(
            "POST",
            &format!("/price-books/{}/publish-changes", g.book),
            json!({}),
            None,
            Some("publish"),
        )
        .await;
    assert_eq!(status, 201, "{receipt}");
    assert_eq!(receipt["applied"], true);
    let unit = id(&receipt["unit"]["id"]);
    let tenant = g.f.ctx.subject_tenant_id();

    let published = g.of(PUBLISHED).await;
    assert_eq!(published.len(), 1, "{published:?}");
    let envelope = &published[0];
    assert_eq!(envelope["version"], 1);
    assert_eq!(envelope["topic"], TOPIC);
    assert_eq!(envelope["source"], "bss-pricing");
    assert_eq!(envelope["subject"], g.book);
    assert_eq!(envelope["subject_type"], PRICE_BOOK_SUBJECT_TYPE);
    assert_eq!(envelope["tenant_id"], tenant.to_string());
    assert_eq!(envelope["producer_mode"], "stateless");
    let mut prices = vec![
        json!({"priceId":first["id"],"priceBookEntryId":g.entry["id"],"dimValue":null,
            "effectiveFrom":"2031-03-01","effectiveTo":"2031-06-01","eligibility":"all",
            "state":"approved"}),
        json!({"priceId":second["id"],"priceBookEntryId":g.entry["id"],"dimValue":null,
            "effectiveFrom":"2031-06-01","effectiveTo":null,"eligibility":"new",
            "state":"approved"}),
    ];
    prices.sort_by_key(|r| r["priceId"].as_str().unwrap().to_owned());
    assert_eq!(
        envelope["data"],
        json!({"tenantId":tenant,"bookId":g.book,"unitId":unit,"prices":prices,
            "actorRef":g.f.ctx.subject_id()}),
        "the event carries the window the chain was approved with"
    );
    let decided = g.of(DECIDED).await;
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0]["subject"], unit.to_string());
    assert_eq!(decided[0]["subject_type"], APPROVAL_UNIT_SUBJECT_TYPE);
    assert_eq!(
        decided[0]["data"],
        json!({"tenantId":tenant,"unitId":unit,"kind":"prices","state":"approved",
            "generation":1,"actors":[g.f.ctx.subject_id()]})
    );
    let replay =
        g.f.call(
            "POST",
            &format!("/price-books/{}/publish-changes", g.book),
            json!({}),
            None,
            Some("publish"),
        )
        .await;
    assert_eq!(replay.0, 201);
    assert_eq!(
        g.envelopes().await.len(),
        2,
        "a keyed replay answers the stored receipt and announces nothing again"
    );
}

#[tokio::test]
async fn every_terminal_decision_is_announced_and_only_an_apply_publishes_prices() {
    let g = gov(1).await;
    let submitter = g.f.user();
    let (one, two) = (g.f.user(), g.f.user());

    let approved = g
        .submit(
            &submitter,
            &g.draft("a", body("2031-03-01", "all")).await,
            "sa",
        )
        .await;
    assert!(
        g.envelopes().await.is_empty(),
        "a pending unit announces nothing"
    );
    let (status, b, _) = g
        .vote(&one, &approved, "approve", json!({"generation":1}), "va")
        .await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(b["outcome"], "applied");
    assert_eq!(g.of(PUBLISHED).await.len(), 1);
    let decided = g.of(DECIDED).await;
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0]["data"]["state"], "approved");
    assert_eq!(decided[0]["data"]["actors"], json!([one.subject_id()]));

    let rejected = g
        .submit(
            &submitter,
            &g.draft("b", body("2031-04-01", "all")).await,
            "sb",
        )
        .await;
    let (status, b, _) = g
        .vote(
            &two,
            &rejected,
            "reject",
            json!({"generation":1,"note":"too cheap"}),
            "vb",
        )
        .await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(b["outcome"], "rejected");

    let withdrawn = g
        .submit(
            &submitter,
            &g.draft("c", body("2031-05-01", "all")).await,
            "sc",
        )
        .await;
    let (status, b, _) = g
        .vote(&submitter, &withdrawn, "withdraw", json!({}), "vc")
        .await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(b["outcome"], "withdrawn");

    assert_eq!(
        g.of(PUBLISHED).await.len(),
        1,
        "reject and withdraw publish no prices"
    );
    let decided = g.of(DECIDED).await;
    let summary: Vec<(Value, Value, Value)> = decided
        .iter()
        .map(|e| {
            (
                e["data"]["unitId"].clone(),
                e["data"]["state"].clone(),
                e["data"]["actors"].clone(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                approved["id"].clone(),
                json!("approved"),
                json!([one.subject_id()])
            ),
            (
                rejected["id"].clone(),
                json!("rejected"),
                json!([two.subject_id()])
            ),
            (
                withdrawn["id"].clone(),
                json!("withdrawn"),
                json!([submitter.subject_id()])
            ),
        ]
    );
}

#[tokio::test]
async fn nothing_is_announced_by_a_vote_short_of_quorum_a_stale_refresh_or_a_refused_apply() {
    let g = gov(2).await;
    let (one, two) = (g.f.user(), g.f.user());
    let price = g.draft("a", body("2031-03-01", "all")).await;
    let unit = g.submit(&g.f.ctx, &price, "s").await;
    let (status, b, _) = g
        .vote(&one, &unit, "approve", json!({"generation":1}), "1")
        .await;
    assert_eq!((status, b["outcome"].clone()), (200, json!("pending")));
    let tenant = g.f.ctx.subject_tenant_id();
    price::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .col_expr(
            price::Column::PriceJson,
            sea_orm::sea_query::Expr::value(json!({"rate":"0.11"})),
        )
        .filter(sea_orm::Condition::all().add(sea_orm::ColumnTrait::eq(
            &price::Column::Id,
            id(&price["id"]),
        )))
        .exec(&g.f.db.conn().unwrap())
        .await
        .unwrap();
    let (status, b, _) = g
        .vote(&two, &unit, "approve", json!({"generation":1}), "2")
        .await;
    assert_eq!(status, 400, "{b}");
    assert!(b.to_string().contains("UNIT_STALE"), "{b}");
    assert!(
        g.envelopes().await.is_empty(),
        "votes and refreshes end nothing"
    );

    // The chain gains an approved price on the unit's start underneath it: the apply is refused
    // and its whole transaction, event writes included, rolls back.
    let conn = g.f.db.conn().unwrap();
    let scope = AccessScope::for_tenant(tenant);
    let p = price_book_entry_repo::find(&conn, &scope, tenant, g.price_book_entry_id())
        .await
        .unwrap()
        .unwrap();
    let mut taken = entry_support::price(&p);
    taken.version_no = 9;
    taken.state = "approved".into();
    taken.effective_from = time::Date::parse(
        "2031-03-01",
        &time::format_description::well_known::Iso8601::DATE,
    )
    .unwrap();
    price_repo::insert(&conn, &scope, taken).await.unwrap();
    let (status, b, _) = g
        .vote(&one, &unit, "approve", json!({"generation":2}), "1b")
        .await;
    assert_eq!(
        (status, b["outcome"].clone()),
        (200, json!("pending")),
        "{b}"
    );
    let (status, b, _) = g
        .vote(&two, &unit, "approve", json!({"generation":2}), "2b")
        .await;
    assert_eq!(status, 409, "{b}");
    assert!(b.to_string().contains("APPLY_REFUSED"), "{b}");
    assert!(
        g.envelopes().await.is_empty(),
        "a refused apply leaves no event"
    );
    assert_eq!(g.price(&price["id"]).await.state, "pending");
}

#[tokio::test]
async fn a_failed_outbox_write_rolls_back_the_decision_and_its_audit() {
    let g = gov(1).await;
    let reviewer = g.f.user();
    let price = g.draft("a", body("2031-03-01", "all")).await;
    let unit = g.submit(&g.f.ctx, &price, "s").await;
    let audits = "SELECT COUNT(*) AS n FROM pricing_audit";
    let before = g.count(audits).await;
    g.raw(
        "CREATE TRIGGER outbox_down BEFORE INSERT ON bss_pricing_outbox_body \
         BEGIN SELECT RAISE(ABORT, 'outbox down'); END",
    )
    .await;
    let (status, b, _) = g
        .vote(&reviewer, &unit, "approve", json!({"generation":1}), "v")
        .await;
    assert_eq!(status, 500, "{b}");
    assert_eq!(g.count(audits).await, before, "no audit without its event");
    assert_eq!(
        g.price(&price["id"]).await.state,
        "pending",
        "no apply without its event"
    );
    let (_, card, _) =
        g.f.call(
            "GET",
            &format!("/approval-units/{}", unit["id"].as_str().unwrap()),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(card["state"], "pending");
    assert_eq!(card["decisions"], json!([]), "no vote without its event");
    g.raw("DROP TRIGGER outbox_down").await;
    let (status, b, _) = g
        .vote(&reviewer, &unit, "approve", json!({"generation":1}), "v")
        .await;
    assert_eq!(status, 200, "the key was never answered: {b}");
    assert_eq!(b["outcome"], "applied");
    assert_eq!(g.of(PUBLISHED).await.len(), 1);
    assert_eq!(g.of(DECIDED).await.len(), 1);
    assert_eq!(g.price(&price["id"]).await.state, "approved");
}

#[tokio::test]
async fn an_apply_rolled_back_after_its_events_were_written_leaves_none() {
    let g = gov(1).await;
    let reviewer = g.f.user();
    let price = g.draft("a", body("2031-03-01", "all")).await;
    let unit = g.submit(&g.f.ctx, &price, "s").await;
    // The terminal audit is written after both events: failing it rolls back an apply whose
    // events are already in the outbox.
    g.raw(
        "CREATE TRIGGER audit_down BEFORE INSERT ON pricing_audit \
         WHEN NEW.action = 'approval.approved' BEGIN SELECT RAISE(ABORT, 'audit down'); END",
    )
    .await;
    let (status, b, _) = g
        .vote(&reviewer, &unit, "approve", json!({"generation":1}), "v")
        .await;
    assert_eq!(status, 500, "{b}");
    assert!(
        g.envelopes().await.is_empty(),
        "the rollback erased both events"
    );
    assert_eq!(g.price(&price["id"]).await.state, "pending");
    g.raw("DROP TRIGGER audit_down").await;
    let (status, b, _) = g
        .vote(&reviewer, &unit, "approve", json!({"generation":1}), "v")
        .await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(g.of(PUBLISHED).await.len(), 1);
    assert_eq!(g.of(DECIDED).await.len(), 1);
}

/// A lost entry reference (D-401) is announced about the entry, naming its SKU and the receipt
/// it held.
#[test]
fn price_book_entry_reference_lost_is_a_typed_event_about_the_entry() {
    let (tenant, entry, sku, reservation, actor) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let lost = PriceBookEntryReferenceLost {
        tenant_id: tenant,
        price_book_entry_id: entry,
        sku_id: sku,
        reservation_id: reservation,
        actor_ref: actor,
    };
    assert_eq!(
        PriceBookEntryReferenceLost::TYPE_ID,
        "gts.cf.core.events.event.v1~cf.bss.pricing.price_book_entry_reference_lost.v1~"
    );
    assert_eq!(
        PriceBookEntryReferenceLost::SUBJECT_TYPE,
        "gts.cf.core.events.subject.v1~cf.bss.pricing.price_book_entry.v1~"
    );
    assert_eq!(PriceBookEntryReferenceLost::SOURCE, SOURCE);
    assert_eq!(lost.subject(), entry.to_string());
    assert_eq!(lost.tenant_id(), Some(tenant));
    assert_eq!(
        serde_json::to_value(&lost).unwrap(),
        json!({"tenantId":tenant,"priceBookEntryId":entry,"skuId":sku,
            "reservationId":reservation,"actorRef":actor})
    );
}

/// A lost plan-item reference (D-407) is announced about the item, naming its plan and revision.
#[test]
fn plan_reference_lost_is_a_typed_event_about_the_item() {
    let (tenant, plan, revision, item, sku, actor) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let lost = PlanReferenceLost {
        tenant_id: tenant,
        plan_id: plan,
        revision_id: revision,
        item_id: item,
        sku_id: sku,
        reservation_id: None,
        actor_ref: actor,
    };
    assert_eq!(
        PlanReferenceLost::TYPE_ID,
        "gts.cf.core.events.event.v1~cf.bss.pricing.plan_reference_lost.v1~"
    );
    assert_eq!(
        PlanReferenceLost::SUBJECT_TYPE,
        "gts.cf.core.events.subject.v1~cf.bss.pricing.plan_item.v1~"
    );
    assert_eq!(PlanReferenceLost::SOURCE, SOURCE);
    assert_eq!(lost.subject(), item.to_string());
    assert_eq!(lost.tenant_id(), Some(tenant));
    assert_eq!(
        serde_json::to_value(&lost).unwrap(),
        json!({"tenantId":tenant,"planId":plan,"revisionId":revision,"itemId":item,
            "skuId":sku,"reservationId":null,"actorRef":actor})
    );
}
