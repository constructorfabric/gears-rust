//! The reference state machine end to end on native Postgres, once per reference kind (D-407):
//! the entries door, the plan-item op-level API (its REST door is run 3.3's) and the ticker over
//! pricing's real chain, against the scripted Products registry double of run 3 (see
//! `entry_support::Script`). Products' own Postgres half of the barrier (reserve against the
//! fences) is proven by its tier, `postgres_sku_chain.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
mod pg_support;
use bss_pricing::{
    api::rest::authoring::AuthoringState,
    infra::{
        reference_ticker::Ticker,
        reference_work::Clock,
        storage::repo::{price_book_entry_repo, price_repo, reference_op_repo as ops},
    },
};
use bss_products_sdk::models::ReferenceState;
use entry_support::policy_support;
use entry_support::{Caller, KINDS, Kind, Script, Target, app_for, request, state_on, user_of};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_db::{DBProvider, DbError, secure::AccessScope};
use toolkit_security::SecurityContext;
use uuid::Uuid;

struct FixedClock(time::OffsetDateTime);
impl Clock for FixedClock {
    fn now(&self) -> time::OffsetDateTime {
        self.0
    }
}
/// Past the in-flight grace, so every op a door left behind is due.
fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock(
        time::OffsetDateTime::now_utc() + time::Duration::days(2),
    ))
}

/// One pricing process on its own Postgres database with one book and, for items, a plan with
/// a draft revision on it.
struct Door {
    pg: pg_support::Pg,
    db: DBProvider<DbError>,
    state: Arc<AuthoringState>,
    app: axum::Router,
    script: Arc<Script>,
    ctx: SecurityContext,
    target: Target,
    /// The create body every `create` sends, so a replay matches it.
    input: Value,
}
async fn setup(kind: Kind) -> Door {
    let pg = pg_support::Pg::applied().await;
    let script = Arc::new(Script::default());
    let db = DBProvider::<DbError>::new(pg.db().await);
    let state = state_on(db.clone(), script.clone()).await;
    let tenant = Uuid::new_v4();
    let app = app_for(state.clone(), tenant);
    let ctx = user_of(tenant);
    let target = Target::new(
        kind,
        &Caller {
            app: &app,
            state: &state,
            ctx: &ctx,
        },
    )
    .await;
    let input = target
        .input(&Caller {
            app: &app,
            state: &state,
            ctx: &ctx,
        })
        .await;
    Door {
        pg,
        db,
        state,
        app,
        script,
        ctx,
        target,
        input,
    }
}
impl Door {
    fn tenant(&self) -> Uuid {
        self.ctx.subject_tenant_id()
    }
    fn scope(&self) -> AccessScope {
        AccessScope::for_tenant(self.tenant())
    }
    fn caller(&self) -> Caller<'_> {
        Caller {
            app: &self.app,
            state: &self.state,
            ctx: &self.ctx,
        }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Value,
        key: Option<&str>,
    ) -> (u16, Value, String) {
        request(&self.app, &self.ctx, method, path, body, None, key).await
    }
    async fn create(&self, key: &str) -> (u16, Value, String) {
        self.target
            .create(&self.caller(), self.input.clone(), key)
            .await
    }
    async fn delete(&self, id: &Value) -> (u16, Value, String) {
        self.target.delete(&self.caller(), id).await
    }
    async fn due(&self) -> Vec<bss_pricing::infra::storage::entity::reference_op::Model> {
        ops::due(&self.db.conn().unwrap(), &self.scope(), clock().now(), 10)
            .await
            .unwrap()
    }
    async fn tick(&self) {
        Ticker::new(self.state.clone(), clock(), 10, 1)
            .tick()
            .await
            .unwrap();
    }
    async fn reference_state(&self, id: &Value) -> String {
        self.target
            .stored(&self.state, id.as_str().unwrap().parse().unwrap())
            .await
            .map(|(state, _)| state)
            .unwrap_or_default()
    }
    /// Every envelope in the Postgres outbox, as JSON.
    async fn envelopes(&self) -> Vec<Value> {
        self.pg
            .raw()
            .await
            .query_all_raw(Statement::from_string(
                DbBackend::Postgres,
                "SELECT convert_from(payload, 'UTF8') AS p FROM public.bss_pricing_outbox_body \
                 ORDER BY id"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_str(&r.try_get::<String>("", "p").unwrap()).unwrap())
            .collect()
    }
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_create_reserves_writes_and_confirms_and_a_delete_releases() {
    for kind in KINDS {
        let p = setup(kind).await;
        let created = p.create("one").await;
        assert_eq!(created.0, 201, "{kind:?}: {created:?}");
        assert_eq!(created.1["reference_state"], "confirmed");
        let reservation = created.1["reservation_id"].clone();
        assert_eq!(
            p.script.refs.lock().await.values().next().copied(),
            Some((
                reservation.as_str().unwrap().parse().unwrap(),
                ReferenceState::Confirmed
            ))
        );
        assert!(p.due().await.is_empty(), "the door finished its op");
        assert_eq!(p.create("one").await, created, "the key replays its answer");
        assert_eq!(Script::count(&p.script.reserve_calls), 1);
        assert_eq!(p.delete(&created.1["id"]).await.0, 204, "{kind:?}");
        assert_eq!(Script::count(&p.script.releases), 1);
        assert!(p.due().await.is_empty(), "the delete op finished too");
        assert_eq!(p.reference_state(&created.1["id"]).await, "", "{kind:?}");
    }
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_every_crash_window_is_resumed_by_the_ticker() {
    for kind in KINDS {
        for (mode, state) in [(1, "reserving"), (2, "written")] {
            let p = setup(kind).await;
            p.script.set(mode);
            let mut door = Box::pin(p.create("crash"));
            tokio::select! {
                result = &mut door => panic!("door did not park: {result:?}"),
                () = p.script.parked.notified() => {}
            }
            drop(door);
            let due = p.due().await;
            assert_eq!(due.len(), 1);
            assert_eq!(due[0].state, state);
            assert_eq!(due[0].ref_kind, p.target.ref_kind());
            p.script.set(0);
            p.tick().await;
            assert!(
                p.due().await.is_empty(),
                "{kind:?} {state}: the ticker finished the op"
            );
            let replay = p.create("crash").await;
            assert_eq!(replay.0, 201, "{kind:?} {state}: {replay:?}");
            if mode == 1 {
                // Before Tx B the reserve outcome was unknown: the ticker cancelled the create,
                // released the key and the reservation, and the same key ran afresh.
                assert_ne!(replay.1["id"], due[0].ref_id.to_string());
                assert_eq!(Script::count(&p.script.releases), 1);
                let op = ops::find(&p.db.conn().unwrap(), &p.scope(), p.tenant(), due[0].op_id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(op.state, "done");
                assert_eq!(
                    bss_pricing::infra::reference_work::Work::read(&op)
                        .unwrap()
                        .outcome
                        .as_deref(),
                    Some("cancelled")
                );
                assert_eq!(p.reference_state(&json!(due[0].ref_id)).await, "");
            } else {
                assert_eq!(replay.1["id"], due[0].ref_id.to_string());
            }
            assert_eq!(p.reference_state(&replay.1["id"]).await, "confirmed");
        }
        let p = setup(kind).await;
        let created = p.create("one").await;
        assert_eq!(created.0, 201);
        p.script.set(3);
        let mut door = Box::pin(p.delete(&created.1["id"]));
        tokio::select! {
            result = &mut door => panic!("door did not park: {result:?}"),
            () = p.script.parked.notified() => {}
        }
        drop(door);
        assert_eq!(p.due().await[0].state, "releasing");
        p.script.set(0);
        p.tick().await;
        assert_eq!(Script::count(&p.script.releases), 1, "{kind:?}");
        assert!(p.due().await.is_empty());
    }
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_forced_release_reconciles_both_ways_and_announces_a_lost_reference() {
    for kind in KINDS {
        for fenced in [false, true] {
            let p = setup(kind).await;
            let created = p.create("one").await;
            assert_eq!(created.0, 201);
            for value in p.script.refs.lock().await.values_mut() {
                value.1 = ReferenceState::Released;
            }
            if fenced {
                p.script.set(4);
            }
            p.tick().await;
            let read = p.target.read(&p.caller(), &created.1["id"]).await;
            let (lost_type, lost_field) = p.target.lost_event();
            let lost: Vec<Value> = p
                .envelopes()
                .await
                .into_iter()
                .filter(|e| e["type"] == lost_type)
                .collect();
            if fenced {
                assert_eq!(read["reference_state"], "lost", "{kind:?}: {read}");
                assert_eq!(lost.len(), 1, "{lost:?}");
                assert_eq!(lost[0]["data"][lost_field], created.1["id"]);
                assert_eq!(lost[0]["tenant_id"], p.tenant().to_string());
                if kind == Kind::Entry {
                    let conn = p.db.conn().unwrap();
                    let entry = price_book_entry_repo::find(
                        &conn,
                        &p.scope(),
                        p.tenant(),
                        created.1["id"].as_str().unwrap().parse().unwrap(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    let refused =
                        price_repo::insert(&conn, &p.scope(), entry_support::price(&entry))
                            .await
                            .unwrap_err();
                    assert!(refused.to_string().contains("ENTRY_REFERENCE_LOST"));
                }
            } else {
                assert_eq!(read["reference_state"], "confirmed", "{kind:?}: {read}");
                assert_ne!(read["reservation_id"], created.1["reservation_id"]);
                assert!(lost.is_empty());
            }
            assert!(p.due().await.is_empty());
        }
    }
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_two_tickers_on_two_pools_finish_one_op_once() {
    for kind in KINDS {
        let p = setup(kind).await;
        p.script.set(6);
        let first = p.create("one").await;
        assert_eq!(first.0, 503, "{kind:?}: the confirm timed out: {first:?}");
        let due = p.due().await;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].state, "written");
        p.script.set(0);
        let other = state_on(
            DBProvider::<DbError>::new(p.pg.db().await),
            p.script.clone(),
        )
        .await;
        let (mut a, mut b) = (
            Ticker::new(p.state.clone(), clock(), 10, 100),
            Ticker::new(other, clock(), 10, 100),
        );
        let (left, right) = tokio::join!(a.tick(), b.tick());
        left.unwrap();
        right.unwrap();
        assert!(p.due().await.is_empty(), "exactly one of them finished it");
        let op = ops::find(&p.db.conn().unwrap(), &p.scope(), p.tenant(), due[0].op_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(op.state, "done");
        let replay = p.create("one").await;
        assert_eq!(replay.0, 201, "{kind:?}: {replay:?}");
        assert_eq!(replay.1["id"], due[0].ref_id.to_string());
        assert_eq!(replay.1["reference_state"], "confirmed");
        assert_eq!(
            Script::count(&p.script.releases),
            0,
            "a timeout never releases"
        );
    }
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_removing_a_dimension_key_an_entry_names_is_refused_409() {
    // Postgres raises 23503 on pricing_price_book_entry's key FK; the door must refuse before it.
    let p = setup(Kind::Entry).await;
    let (_, _, tag) = p.call("GET", "/dimension-keys", json!({}), None).await;
    let declared = request(
        &p.app,
        &p.ctx,
        "PUT",
        "/dimension-keys",
        json!({"items":[{"key":"region","values":["eu","us"]}]}),
        Some(&tag),
        None,
    )
    .await;
    assert_eq!(declared.0, 200, "{declared:?}");
    let created = p
        .call(
            "POST",
            &p.target.endpoint(),
            json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"dimension_key":"region","model":"per_unit"}),
            Some("one"),
        )
        .await;
    assert_eq!(created.0, 201, "{created:?}");
    let refused = request(
        &p.app,
        &p.ctx,
        "PUT",
        "/dimension-keys",
        json!({"items":[]}),
        Some(&declared.2),
        None,
    )
    .await;
    assert_eq!(refused.0, 409, "{refused:?}");
    assert!(
        refused.1.to_string().contains("DIMENSION_KEY_IN_USE"),
        "{refused:?}"
    );
}
