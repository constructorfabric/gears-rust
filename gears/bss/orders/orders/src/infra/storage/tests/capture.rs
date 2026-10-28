//! S2-09 on real PostgreSQL: draft authoring through the shared capture service, the S2-04 engine,
//! the PEP over the policy fake (S2-03, D-227), the registry (S2-05), the sealed v3 audit writer (S2-06) and the
//! real Event Broker managed producer (S2-08), over the restricted runtime role.
//!
//! No catalog, Pricing, Products or Contracts client exists in this harness: draft authoring must
//! succeed without them (02 §3.5), while a PDP outage still fails closed.
use super::events::Env;
use super::*;
use crate::api::local_client::sdk_result;
use crate::authz::test_pdp::{FakePdp, Grant, ScriptedPdp, pep_with, user, with_proof};
use crate::authz::{Action, Caller};
use crate::domain::audit::{ActorIdentities, AdminTextKey, Principal, ServiceRole};
use crate::domain::idempotency::{LeaseDuration, StoredResponse};
use crate::infra::broker::BoundProducer;
use crate::infra::capture::{CaptureService, CaptureSettings, Settled, WriteMeta};
use crate::infra::engine::{Engine, EngineParts, Registries};
use arc_swap::ArcSwapOption;
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::{
    AddLine, AuthoredTerm, BillingCycle, CalendarDate, Category, CreateMeta, CreateOrder, Currency,
    HeaderPatch, LinePatch, OrderView, SelectedItem,
};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::models::{
    CallMeta, DraftRevision, IdempotencyKey, OrderVersion, TransitionResult,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) const BUYER: (u128, u128) = (102, 10);
/// A seller-role-only principal: no authoring rule names it.
pub(super) const SELLER_OPERATOR: (u128, u128) = (103, 20);

pub(super) fn s(id: Uuid) -> String {
    id.to_string()
}
/// The buyer's authoring actions.
const AUTHORING: &[Action] = &[
    Action::OrderCreate,
    Action::OrderWrite,
    Action::OrderEdit,
    Action::OrderRead,
];
/// The policy fake (D-227). The buyer authors on resource tenants 10/11 (never 12). Which payer
/// an order names is a business rule checked at submit (D-215), so the policy grants no payer
/// condition.
pub(super) fn policy() -> Arc<FakePdp> {
    FakePdp::new(
        vec![
            Grant::new(u(BUYER.0), AUTHORING).path([("resource_tenant_id", vec![u(10), u(11)])]),
            // Seller operations on the seller's orders: read, hold and resume only (02 §1.3).
            Grant::new(
                u(SELLER_OPERATOR.0),
                &[Action::OrderRead, Action::OrderHold, Action::OrderResume],
            )
            .path([("seller_tenant_id", vec![u(20)])]),
        ],
        &[],
    )
}
pub(super) fn identities() -> ActorIdentities {
    ActorIdentities::new(
        Some(Principal {
            subject_id: u(900),
            subject_tenant_id: u(901),
        }),
        [(
            ServiceRole::Workflow,
            Principal {
                subject_id: u(106),
                subject_tenant_id: u(99),
            },
        )],
    )
    .unwrap()
}
pub(super) fn buyer() -> Caller {
    user(BUYER.0, BUYER.1)
}
pub(super) fn key(text: &str) -> IdempotencyKey {
    IdempotencyKey::try_from(text.to_owned()).unwrap()
}
pub(super) fn create_meta(text: &str) -> CreateMeta {
    CreateMeta {
        idempotency_key: key(text),
        correlation_id: None,
        delegation_proof_ref: None,
    }
}
pub(super) fn write(text: &str, version: i64, revision: Option<i64>) -> WriteMeta {
    WriteMeta {
        call: CallMeta {
            expected_version: OrderVersion::try_from(version).unwrap(),
            idempotency_key: key(text),
            correlation_id: None,
            delegation_proof_ref: None,
        },
        expected_draft_revision: revision.map(|r| DraftRevision::try_from(r).unwrap()),
    }
}
pub(super) fn new_order(category: Category) -> CreateOrder {
    CreateOrder {
        resource_tenant_id: u(10),
        payer_tenant_id: u(30),
        seller_tenant_id: u(20),
        category,
        contract_id: None,
    }
}
pub(super) fn line(currency: &str, cycle: Option<BillingCycle>) -> AddLine {
    AddLine {
        plan_id: u(200),
        plan_revision_id: u(201),
        selected_items: vec![SelectedItem {
            item_id: u(300),
            quantity: Some(serde_json::from_value(json!("3")).unwrap()),
            selected_dim_value: None,
        }],
        currency: Currency::try_from(currency.to_owned()).unwrap(),
        contract_effective_date: Some(CalendarDate(
            time::Date::from_calendar_date(2026, time::Month::December, 1).unwrap(),
        )),
        service_activation_date: None,
        acceptance_due_date: None,
        term_duration: Some(AuthoredTerm::Periods { count: 12 }),
        billing_cycle: cycle,
    }
}

pub(super) struct T {
    pub(super) env: Env,
    _bound: BoundProducer,
    pub(super) sink: crate::infra::events::EventSink,
}
impl T {
    pub(super) async fn new() -> Self {
        let env = Env::new().await.unwrap();
        let bound = env.bind().await;
        let sink = bound.sink().clone();
        Self {
            env,
            _bound: bound,
            sink,
        }
    }
    pub(super) fn engine(&self, api: Arc<dyn authz_resolver_sdk::AuthZResolverApi>) -> Engine {
        Engine::new(
            EngineParts {
                db: self.env.db.clone(),
                pep: pep_with(api, identities()),
                sink: self.sink.clone(),
                identities: identities(),
                admin_key: Arc::new(AdminTextKey::new("k1", &[7u8; 32]).unwrap()),
                lease: LeaseDuration::from_seconds(30).unwrap(),
            },
            Registries::compile().unwrap(),
        )
    }
    pub(super) fn service_with(engine: Engine, line_cap: usize) -> CaptureService {
        CaptureService::new(
            Arc::new(ArcSwapOption::from(Some(Arc::new(engine)))),
            CaptureSettings { line_cap },
        )
    }
    pub(super) fn service(&self) -> CaptureService {
        Self::service_with(self.engine(policy()), 200)
    }
    /// The read service over the same database, PDP and identities (early S6-01/S6-04).
    pub(super) fn reads(&self) -> crate::infra::read::ReadService {
        self.reads_with(policy())
    }
    pub(super) fn reads_with(
        &self,
        api: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
    ) -> crate::infra::read::ReadService {
        self.reads_with_signals(api, Arc::new(crate::infra::read::OtelReadSignals::new()))
    }
    /// The read service with the given PDP and signal sink (the tests record the signals).
    pub(super) fn reads_with_signals(
        &self,
        api: Arc<dyn authz_resolver_sdk::AuthZResolverApi>,
        signals: Arc<dyn crate::infra::read::ReadSignals>,
    ) -> crate::infra::read::ReadService {
        crate::infra::read::ReadService::new(
            Arc::new(ArcSwapOption::from(Some(Arc::new(
                crate::infra::read::ReadParts {
                    db: self.env.db.clone(),
                    pep: pep_with(api, identities()),
                    identities: identities(),
                },
            )))),
            signals,
        )
    }
    pub(super) async fn n(&self, sql: &str) -> i64 {
        self.env.pg.scalar(sql).await.unwrap()
    }
    pub(super) async fn json(&self, sql: &str) -> Value {
        self.env
            .pg
            .raw
            .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
            .await
            .unwrap()
            .map_or(Value::Null, |r| r.try_get::<Value>("", "j").unwrap())
    }
    pub(super) async fn order_row(&self, id: Uuid) -> Value {
        self.json(&format!(
            "SELECT to_jsonb(o) AS j FROM bss_orders__order o WHERE order_id='{id}'"
        ))
        .await
    }
    pub(super) async fn outbox(&self) -> i64 {
        self.n("SELECT count(*) AS n FROM toolkit_outbox_body")
            .await
    }
    pub(super) async fn audits(&self, order: Uuid) -> i64 {
        self.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__transition_audit WHERE order_id='{order}' OR requested_order_ref='{order}'"
        ))
        .await
    }
    pub(super) async fn registry(&self) -> i64 {
        self.n("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await
    }
}

/// The edge limiter at its design baseline (20 per minute per caller and order): no wire test
/// here sends that many writes to one order, so it never trips outside the throttling suite.
pub(super) fn limiter() -> Arc<crate::api::rest::throttle::PerOrderLimiter> {
    crate::api::rest::throttle::PerOrderLimiter::new(
        crate::api::rest::throttle::ThrottleSettings::default(),
    )
}

pub(super) fn view(r: Settled) -> OrderView {
    assert_eq!(r.response.status(), 201, "{:?}", r.response.body());
    sdk_result(r).unwrap()
}
pub(super) fn result(r: Settled) -> TransitionResult {
    assert_eq!(r.response.status(), 200, "{:?}", r.response.body());
    sdk_result(r).unwrap()
}
pub(super) fn refused(r: &Settled) -> Reason {
    let code = r.response.body()["error_code"].as_str().unwrap();
    *Reason::ALL
        .iter()
        .find(|x| x.mapping().code == code)
        .unwrap()
}
pub(super) fn revision(r: &TransitionResult) -> i64 {
    i64::from(r.draft_revision.unwrap())
}
pub(super) async fn create(t: &T, svc: &CaptureService, text: &str) -> Uuid {
    let _ = t;
    view(
        svc.create(&buyer(), new_order(Category::NewSale), &create_meta(text))
            .await
            .unwrap(),
    )
    .order
    .order_id
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn create_commits_an_empty_draft_with_trusted_evidence_and_replays_it() {
    let t = T::new().await;
    let svc = t.service();
    let outbox = t.outbox().await;
    let first = svc
        .create(&buyer(), new_order(Category::NewSale), &create_meta("c-1"))
        .await
        .unwrap();
    assert_eq!(first.response.headers()["etag"], "\"1\"");
    let v = view(first.clone());
    let id = v.order.order_id;
    // `Location` depends on the public path, so the REST adapter adds it; it is never stored.
    assert!(!first.response.headers().contains_key("location"));
    assert_eq!(i64::from(v.version.version), 1);
    assert_eq!(v.draft_revision.map(i64::from), Some(0));
    assert!(v.lines.is_empty());
    assert_eq!(
        v.order.sales_path,
        bss_orders_sdk::authoring::SalesPath::SelfService
    );
    assert_eq!(first.response.body()["draft_revision"], json!(0));
    // Persisted aggregate: trusted actor, three axes, version 1 / revision 0, no lines/pin/total.
    let row = t.order_row(id).await;
    assert_eq!(row["state"], "draft");
    assert_eq!(row["current_version"], 1);
    assert_eq!(row["draft_revision"], 0);
    assert_eq!(row["initiating_actor"], json!(s(u(BUYER.0))));
    assert_eq!(row["seller_tenant_id"], json!(s(u(20))));
    for table in [
        "draft_content",
        "order_line",
        "resolved_total",
        "order_line_identity",
    ] {
        assert_eq!(
            t.n(&format!(
                "SELECT count(*) AS n FROM bss_orders__{table} WHERE order_id='{id}'"
            ))
            .await,
            0,
            "{table}"
        );
    }
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{id}' AND version=1 AND market_currency IS NULL"
        ))
        .await,
        1
    );
    // Same key: the original identity and number, unchanged; no second order or audit.
    let replay = svc
        .create(&buyer(), new_order(Category::NewSale), &create_meta("c-1"))
        .await
        .unwrap();
    assert_eq!(replay.response, first.response);
    assert!(
        replay.replayed && !first.replayed,
        "only the replay is marked"
    );
    assert_eq!(t.n("SELECT count(*) AS n FROM bss_orders__order").await, 1);
    assert_eq!(t.audits(id).await, 1);
    // A different key creates a second draft with a distinct seller-unique number; a supplied
    // delegation proof records the partner path.
    let partner = view(
        svc.create(
            &with_proof(&buyer(), "proof-1"),
            new_order(Category::NewSale),
            &create_meta("c-2"),
        )
        .await
        .unwrap(),
    );
    assert_ne!(partner.order.order_number, v.order.order_number);
    assert_eq!(
        partner.order.sales_path,
        bss_orders_sdk::authoring::SalesPath::PartnerPlaced
    );
    // Create and draft authoring are eventless.
    assert_eq!(t.outbox().await, outbox);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn create_refusals_and_pdp_outage_create_nothing() {
    let t = T::new().await;
    let svc = t.service();
    // `change` is registered but not admitted: a settled, audited refusal with no order.
    let refusal = svc
        .create(&buyer(), new_order(Category::Change), &create_meta("c-x"))
        .await
        .unwrap();
    assert_eq!(refused(&refusal), Reason::CategoryNotAdmitted);
    assert_eq!(t.n("SELECT count(*) AS n FROM bss_orders__order").await, 0);
    // A seller-role-only principal has no authoring path: denied, nothing created.
    let seller = user(SELLER_OPERATOR.0, SELLER_OPERATOR.1);
    let denied = svc
        .create(&seller, new_order(Category::NewSale), &create_meta("c-s"))
        .await
        .unwrap_err();
    assert!(matches!(denied, OrdersError::Refused(_)), "{denied:?}");
    assert_eq!(t.n("SELECT count(*) AS n FROM bss_orders__order").await, 0);
    // The seller role grants no authoring on the seller's own orders either: header edits and
    // line insertions are denied before any registry row, audit settlement or write (AC 12).
    let id = create(&t, &svc, "c-b").await;
    let row = t.order_row(id).await;
    let registry = t.registry().await;
    let edit = svc
        .patch_order(
            &seller,
            id,
            HeaderPatch {
                contract_id: Some(Some(u(77))),
                ..HeaderPatch::default()
            },
            &write("s-e", 1, Some(0)),
        )
        .await
        .unwrap_err();
    assert!(matches!(edit, OrdersError::Refused(_)), "{edit:?}");
    let add = svc
        .add_line(&seller, id, line("EUR", None), &write("s-a", 1, Some(0)))
        .await
        .unwrap_err();
    assert!(matches!(add, OrdersError::Refused(_)), "{add:?}");
    assert_eq!(t.order_row(id).await, row);
    assert_eq!(t.registry().await, registry);
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__draft_content WHERE order_id='{id}'"
        ))
        .await,
        0
    );
    let orders = t.n("SELECT count(*) AS n FROM bss_orders__order").await;
    // PDP outage fails closed: unavailable, no order, no registry settlement.
    let outage = T::service_with(t.engine(ScriptedPdp::new(|_| None)), 200);
    let registry = t.registry().await;
    let err = outage
        .create(&buyer(), new_order(Category::NewSale), &create_meta("c-o"))
        .await
        .unwrap_err();
    assert!(matches!(err, OrdersError::Unavailable), "{err:?}");
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__order").await,
        orders
    );
    assert_eq!(t.registry().await, registry);
    // Before the engine is bound (gear not ready) nothing is written either.
    let unready = CaptureService::new(
        Arc::new(ArcSwapOption::from(None)),
        CaptureSettings { line_cap: 200 },
    );
    assert!(matches!(
        unready
            .create(&buyer(), new_order(Category::NewSale), &create_meta("c-u"))
            .await,
        Err(OrdersError::Unavailable)
    ));
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn authoring_keeps_stable_line_identities_and_increments_only_the_draft_revision() {
    let t = T::new().await;
    let svc = t.service();
    let outbox = t.outbox().await;
    let id = create(&t, &svc, "c-1").await;
    // Insert: server-reserved line identity, revision 0 -> 1, version stays 1.
    let first = svc
        .add_line(
            &buyer(),
            id,
            line("EUR", Some(BillingCycle::Month)),
            &write("a-1", 1, Some(0)),
        )
        .await
        .unwrap();
    assert_eq!(first.response.headers()["etag"], "\"1\"");
    let added = result(first.clone());
    let l1 = added.line_id.unwrap();
    assert_eq!((revision(&added), i64::from(added.version)), (1, 1));
    // A same-key retry returns the same identity and adds nothing.
    let replay = svc
        .add_line(
            &buyer(),
            id,
            line("EUR", Some(BillingCycle::Month)),
            &write("a-1", 1, Some(0)),
        )
        .await
        .unwrap();
    assert_eq!(replay.response, first.response);
    assert!(
        replay.replayed && !first.replayed,
        "only the replay is marked"
    );
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__draft_content WHERE order_id='{id}'"
        ))
        .await,
        1
    );
    // Authored values are retained exactly: no cascade, interval in calendar units.
    let stored = t
        .json(&format!(
            "SELECT jsonb_build_object('items', selected_items, 'ced', contract_effective_date, 'sad', service_activation_date, 'term', term_duration::text, 'kind', term_kind, 'authored', authored_term, 'cycle', billing_cycle) AS j FROM bss_orders__draft_content WHERE line_id='{l1}'"
        ))
        .await;
    assert_eq!(
        stored,
        json!({"items": [{"item_id": s(u(300)), "quantity": "3", "selected_dim_value": null}],
               "ced": "2026-12-01",
               "sad": null, "term": "1 year", "kind": "finite",
               "authored": {"kind": "periods", "count": 12}, "cycle": "month"})
    );
    // A second line with a different cycle is valid (02 §2.2).
    let l2 = result(
        svc.add_line(
            &buyer(),
            id,
            line("EUR", Some(BillingCycle::Year)),
            &write("a-2", 1, Some(1)),
        )
        .await
        .unwrap(),
    )
    .line_id
    .unwrap();
    assert_ne!(l1, l2);
    // Commercial edit of one line preserves every unnamed authored value.
    let edit = LinePatch {
        selected_items: Some(vec![SelectedItem {
            item_id: u(301),
            quantity: Some(serde_json::from_value(json!("0.5")).unwrap()),
            selected_dim_value: Some("eu".into()),
        }]),
        service_activation_date: Some(Some(CalendarDate(
            time::Date::from_calendar_date(2027, time::Month::January, 15).unwrap(),
        ))),
        ..LinePatch::default()
    };
    let edited = result(
        svc.patch_line(&buyer(), id, l1, edit, &write("e-1", 1, Some(2)))
            .await
            .unwrap(),
    );
    assert_eq!(revision(&edited), 3);
    assert_eq!(edited.line_id, None);
    let stored = t
        .json(&format!(
            "SELECT jsonb_build_object('plan', plan_id, 'sad', service_activation_date, 'ced', contract_effective_date, 'term', term_duration::text, 'cycle', billing_cycle) AS j FROM bss_orders__draft_content WHERE line_id='{l1}'"
        ))
        .await;
    assert_eq!(
        stored,
        json!({"plan": s(u(200)), "sad": "2027-01-15", "ced": "2026-12-01", "term": "1 year", "cycle": "month"})
    );
    // Remove: membership goes, the identity stays reserved.
    let removed = result(
        svc.remove_line(&buyer(), id, l1, &write("r-1", 1, Some(3)))
            .await
            .unwrap(),
    );
    assert_eq!(revision(&removed), 4);
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__draft_content WHERE line_id='{l1}'"
        ))
        .await,
        0
    );
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_line_identity WHERE line_id='{l1}'"
        ))
        .await,
        1
    );
    // A removed identity answers line-not-found exactly like one that never existed, for an edit
    // and for a second removal; the refusal is settled and changes nothing.
    for (k, target) in [("e-2", l1), ("e-3", u(999))] {
        let r = svc
            .patch_line(
                &buyer(),
                id,
                target,
                LinePatch {
                    currency: Some(Currency::try_from("EUR".to_owned()).unwrap()),
                    ..LinePatch::default()
                },
                &write(k, 1, Some(4)),
            )
            .await
            .unwrap();
        assert_eq!(
            (r.response.status(), refused(&r)),
            (404, Reason::LineNotFound)
        );
    }
    let r = svc
        .remove_line(&buyer(), id, l1, &write("r-2", 1, Some(4)))
        .await
        .unwrap();
    assert_eq!(refused(&r), Reason::LineNotFound);
    // A fresh insertion never re-admits the removed identity.
    let l3 = result(
        svc.add_line(
            &buyer(),
            id,
            line("EUR", Some(BillingCycle::Month)),
            &write("a-3", 1, Some(4)),
        )
        .await
        .unwrap(),
    )
    .line_id
    .unwrap();
    assert!(l3 != l1 && l3 != l2);
    let row = t.order_row(id).await;
    assert_eq!(
        (
            row["current_version"].clone(),
            row["draft_revision"].clone()
        ),
        (json!(1), json!(5))
    );
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{id}'"
        ))
        .await,
        1
    );
    // Every committed draft write is one v3 audit entry; refusals add theirs; no event.
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__transition_audit WHERE order_id='{id}' AND hash_version<>3"
        ))
        .await,
        0
    );
    assert_eq!(t.outbox().await, outbox);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn structural_and_field_refusals_follow_the_declared_precedence() {
    let t = T::new().await;
    let svc = T::service_with(t.engine(policy()), 2);
    let id = create(&t, &svc, "c-1").await;
    result(
        svc.add_line(&buyer(), id, line("EUR", None), &write("a-1", 1, Some(0)))
            .await
            .unwrap(),
    );
    let before = t.order_row(id).await;
    let audits = t.audits(id).await;
    let mut cases: Vec<(&str, Reason, Settled)> = Vec::new();
    // Mixed currency on insert.
    cases.push((
        "currency",
        Reason::CurrencyMixed,
        svc.add_line(&buyer(), id, line("USD", None), &write("x-1", 1, Some(1)))
            .await
            .unwrap(),
    ));
    // Mixed field classes in one header request (commercial + administrative).
    cases.push((
        "mixed",
        Reason::MixedFieldClasses,
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                contract_id: Some(Some(u(77))),
                display_label: Some(Some("label".into())),
                ..HeaderPatch::default()
            },
            &write("x-2", 1, Some(1)),
        )
        .await
        .unwrap(),
    ));
    // The seller is fixed even when the request repeats the current value.
    cases.push((
        "seller",
        Reason::TenantAxisImmutable,
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                seller_tenant_id: Some(u(20)),
                ..HeaderPatch::default()
            },
            &write("x-3", 1, Some(1)),
        )
        .await
        .unwrap(),
    ));
    // `change` is refused on commercial draft edits too.
    cases.push((
        "category",
        Reason::CategoryNotAdmitted,
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                category: Some(Category::Change),
                ..HeaderPatch::default()
            },
            &write("x-4", 1, Some(1)),
        )
        .await
        .unwrap(),
    ));
    // Omitted draft revision inside draft: version-conflict naming the current revision.
    let conflict = svc
        .add_line(&buyer(), id, line("EUR", None), &write("x-5", 1, None))
        .await
        .unwrap();
    assert_eq!(
        conflict.response.body()["context"]["data"],
        json!({"current_version": 1, "draft_revision": 1})
    );
    cases.push(("omitted", Reason::VersionConflict, conflict));
    // Mixed + seller + change named together: the first registered guard (mixed) wins.
    cases.push((
        "precedence",
        Reason::MixedFieldClasses,
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                seller_tenant_id: Some(u(20)),
                category: Some(Category::Change),
                internal_notes: Some(Some("n".into())),
                ..HeaderPatch::default()
            },
            &write("x-6", 1, Some(1)),
        )
        .await
        .unwrap(),
    ));
    for (name, reason, response) in &cases {
        assert_eq!(refused(response), *reason, "{name}");
    }
    // Line cap 2: the second insertion fits, a third refuses; an edit at the cap does not.
    let l2 = result(
        svc.add_line(&buyer(), id, line("EUR", None), &write("a-2", 1, Some(1)))
            .await
            .unwrap(),
    )
    .line_id
    .unwrap();
    let over = svc
        .add_line(&buyer(), id, line("EUR", None), &write("x-7", 1, Some(2)))
        .await
        .unwrap();
    assert_eq!(refused(&over), Reason::LineCapExceeded);
    result(
        svc.patch_line(
            &buyer(),
            id,
            l2,
            LinePatch {
                billing_cycle: Some(Some(BillingCycle::Year)),
                ..LinePatch::default()
            },
            &write("e-1", 1, Some(2)),
        )
        .await
        .unwrap(),
    );
    // A periods term with the cycle authored later gains its exact interval.
    assert_eq!(
        t.json(&format!(
            "SELECT to_jsonb(term_duration::text) AS j FROM bss_orders__draft_content WHERE line_id='{l2}'"
        ))
        .await,
        json!("12 years")
    );
    // Refusals were settled and audited and changed nothing; refused keys replay verbatim.
    assert_eq!(
        t.audits(id).await,
        audits + i64::try_from(cases.len()).unwrap() + 3
    );
    let after = t.order_row(id).await;
    assert_eq!(after["draft_revision"], 3);
    assert_eq!(after["contract_id"], before["contract_id"]);
    assert_eq!(after["category"], before["category"]);
    let replay = svc
        .add_line(&buyer(), id, line("USD", None), &write("x-1", 1, Some(1)))
        .await
        .unwrap();
    assert_eq!(replay.response, cases[0].2.response);
    assert!(
        replay.replayed,
        "a settled refusal replays marked (ADR-0005, D-212)"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn outside_draft_inadmissibility_wins_and_administrative_edits_stay_unavailable() {
    let t = T::new().await;
    let svc = t.service();
    let id = create(&t, &svc, "c-1").await;
    t.env
        .pg
        .sql(&format!(
            "INSERT INTO bss_orders__order_version (order_id, version, supersedes_version, market_currency, market_region, payer_tenant_id, category, contract_id, actor, actor_tenant_id, reason, amendment_reason, created_at) \
             VALUES ('{id}', 2, 1, 'EUR', 'DE', '{}', '{}', NULL, '{}', '{}', 'submit', NULL, now()); \
             UPDATE bss_orders__order SET version_allocation_high_water=2, current_version=2, state='submitted' WHERE order_id='{id}'",
            u(30),
            Category::NEW_SALE,
            u(BUYER.0),
            u(BUYER.1)
        ))
        .await
        .unwrap();
    // A commercial (even mixed) edit without a draft revision refuses not-admissible first.
    let r = svc
        .patch_order(
            &buyer(),
            id,
            HeaderPatch {
                contract_id: Some(None),
                display_label: Some(Some("x".into())),
                ..HeaderPatch::default()
            },
            &write("p-1", 2, None),
        )
        .await
        .unwrap();
    assert_eq!(refused(&r), Reason::NotAdmissible);
    assert_eq!(
        r.response.body()["context"]["data"],
        json!({"state": "submitted", "trigger": "draft-mutate"})
    );
    let r = svc
        .add_line(&buyer(), id, line("EUR", None), &write("p-2", 2, None))
        .await
        .unwrap();
    assert_eq!(refused(&r), Reason::NotAdmissible);
    // Administrative-only edits select administrative-edit (S4-05, undelivered): unavailable
    // with no authorization, audit, registry row or write, in draft and outside it.
    let draft = create(&t, &svc, "c-2").await;
    for target in [id, draft] {
        let audits = t.audits(target).await;
        let registry = t.registry().await;
        let row = t.order_row(target).await;
        let header = svc
            .patch_order(
                &buyer(),
                target,
                HeaderPatch {
                    external_reference: Some(Some("PO-1".into())),
                    ..HeaderPatch::default()
                },
                &write("adm-1", 1, None),
            )
            .await;
        assert!(
            matches!(header, Err(OrdersError::Unavailable)),
            "{header:?}"
        );
        let line_edit = svc
            .patch_line(
                &buyer(),
                target,
                u(5),
                LinePatch {
                    display_label: Some(Some("x".into())),
                    ..LinePatch::default()
                },
                &write("adm-2", 1, None),
            )
            .await;
        assert!(matches!(line_edit, Err(OrdersError::Unavailable)));
        assert_eq!(t.audits(target).await, audits);
        assert_eq!(t.registry().await, registry);
        assert_eq!(t.order_row(target).await, row);
        assert_eq!(
            t.n(&format!(
                "SELECT count(*) AS n FROM bss_orders__order_admin WHERE order_id='{target}'"
            ))
            .await,
            0
        );
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn concurrent_writes_at_one_revision_commit_exactly_once() {
    let t = T::new().await;
    let svc = t.service();
    let id = create(&t, &svc, "c-1").await;
    let caller = buyer();
    let (ka, kb) = (write("k-a", 1, Some(0)), write("k-b", 1, Some(0)));
    let (a, b) = tokio::join!(
        svc.add_line(&caller, id, line("EUR", None), &ka),
        svc.add_line(&caller, id, line("EUR", None), &kb),
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    let committed = outcomes
        .iter()
        .filter(|r| r.response.status() == 200)
        .count();
    let conflicts = outcomes
        .iter()
        .filter(|r| r.response.status() != 200 && refused(r) == Reason::VersionConflict)
        .count();
    assert_eq!((committed, conflicts), (1, 1), "{outcomes:?}");
    let row = t.order_row(id).await;
    assert_eq!(row["draft_revision"], 1);
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__draft_content WHERE order_id='{id}'"
        ))
        .await,
        1
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn header_and_axis_edits_are_written_by_the_engine_under_complete_authorization() {
    let t = T::new().await;
    let svc = t.service();
    let id = create(&t, &svc, "c-1").await;
    // Contract reference recorded unresolved, then cleared.
    let r = result(
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                contract_id: Some(Some(u(77))),
                ..HeaderPatch::default()
            },
            &write("h-1", 1, Some(0)),
        )
        .await
        .unwrap(),
    );
    assert_eq!(revision(&r), 1);
    assert_eq!(t.order_row(id).await["contract_id"], json!(s(u(77))));
    result(
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                contract_id: Some(None),
                ..HeaderPatch::default()
            },
            &write("h-2", 1, Some(1)),
        )
        .await
        .unwrap(),
    );
    assert_eq!(t.order_row(id).await["contract_id"], Value::Null);
    // Authorized payer and resource changes commit through the proposed-arrangement decision.
    let axes = || HeaderPatch {
        payer_tenant_id: Some(u(31)),
        resource_tenant_id: Some(u(11)),
        ..HeaderPatch::default()
    };
    let committed = svc
        .patch_order(&buyer(), id, axes(), &write("h-3", 1, Some(2)))
        .await
        .unwrap();
    result(committed.clone());
    let audits = t.audits(id).await;
    // A same-key retry after its own axis change commits replays the stored outcome: the
    // fingerprint binds the arrangement the request establishes, not the one it replaced.
    let replay = svc
        .patch_order(&buyer(), id, axes(), &write("h-3", 1, Some(2)))
        .await
        .unwrap();
    assert_eq!(replay.response, committed.response);
    assert!(replay.replayed && !committed.replayed);
    assert_eq!(t.audits(id).await, audits);
    let row = t.order_row(id).await;
    assert_eq!(
        (
            row["payer_tenant_id"].clone(),
            row["resource_tenant_id"].clone(),
            row["draft_revision"].clone()
        ),
        (json!(s(u(31))), json!(s(u(11))), json!(3))
    );
    // The immutable audit namespace and seller stay as created.
    assert_eq!(row["audit_tenant_id"], json!(s(u(10))));
    assert_eq!(row["seller_tenant_id"], json!(s(u(20))));
    // A resource tenant the caller may not author on is refused before any write, never settled.
    let registry = t.registry().await;
    let denied = svc
        .patch_order(
            &buyer(),
            id,
            HeaderPatch {
                resource_tenant_id: Some(u(12)),
                ..HeaderPatch::default()
            },
            &write("h-4", 1, Some(3)),
        )
        .await
        .unwrap_err();
    assert!(matches!(denied, OrdersError::Refused(_)), "{denied:?}");
    assert_eq!(t.registry().await, registry);
    assert_eq!(t.order_row(id).await, row);
    // Any payer is accepted on a draft: the payer relationship is a business rule the submit
    // gate checks (D-215), not a permission the policy grants.
    let r = result(
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                payer_tenant_id: Some(u(32)),
                ..HeaderPatch::default()
            },
            &write("h-5", 1, Some(3)),
        )
        .await
        .unwrap(),
    );
    assert_eq!(revision(&r), 4);
    assert_eq!(t.order_row(id).await["payer_tenant_id"], json!(s(u(32))));
}

/// Resource tenants 10 and 11 granted by separate grants: each decision is constrained to one
/// tenant, so no single scope sees the order both before and after the move.
fn tenant_grant(tenant: u128) -> Grant {
    Grant::new(u(BUYER.0), AUTHORING).path([("resource_tenant_id", vec![u(tenant)])])
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_moves_between_tenants_granted_by_separate_rules() {
    let t = T::new().await;
    let svc = T::service_with(
        t.engine(FakePdp::new(vec![tenant_grant(10), tenant_grant(11)], &[])),
        200,
    );
    let id = create(&t, &svc, "c-1").await;
    let audits = t.audits(id).await;
    // The engine writes the moved row under the tenant-B decision; the audit append that
    // follows in the same transaction must use that scope too, not the tenant-A one.
    let r = result(
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                resource_tenant_id: Some(u(11)),
                ..HeaderPatch::default()
            },
            &write("move-1", 1, Some(0)),
        )
        .await
        .unwrap(),
    );
    assert_eq!(revision(&r), 1);
    let row = t.order_row(id).await;
    assert_eq!(row["resource_tenant_id"], json!(s(u(11))));
    assert_eq!(row["audit_tenant_id"], json!(s(u(10))));
    assert_eq!(t.audits(id).await, audits + 1);
    // The order is now edited under the tenant-B grant alone.
    result(
        svc.patch_order(
            &buyer(),
            id,
            HeaderPatch {
                contract_id: Some(Some(u(77))),
                ..HeaderPatch::default()
            },
            &write("move-2", 1, Some(1)),
        )
        .await
        .unwrap(),
    );
    // Without the tenant-B grant the move is refused and nothing is written.
    let a_only = T::service_with(t.engine(FakePdp::new(vec![tenant_grant(10)], &[])), 200);
    let other = create(&t, &a_only, "c-2").await;
    let before = t.order_row(other).await;
    let denied = a_only
        .patch_order(
            &buyer(),
            other,
            HeaderPatch {
                resource_tenant_id: Some(u(11)),
                ..HeaderPatch::default()
            },
            &write("move-3", 1, Some(0)),
        )
        .await
        .unwrap_err();
    assert!(matches!(denied, OrdersError::Refused(_)), "{denied:?}");
    assert_eq!(t.order_row(other).await, before);
}

/// Under a gateway `prefix_path` the routes are nested: `Location` keeps the prefix on the first
/// response and on a replay, and it resolves to the created order.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn location_keeps_the_gateway_prefix_on_create_and_replay() {
    let t = T::new().await;
    let svc = Arc::new(t.service());
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let gear = crate::api::rest::capture::router(Arc::clone(&svc), limiter(), &openapi).merge(
        crate::api::rest::read::router(Arc::new(t.reads()), &openapi),
    );
    // The gateway nests every gear router under its `prefix_path`.
    let gateway = axum::Router::new()
        .nest("/cf", gear)
        .layer(axum::Extension(buyer().ctx().clone()));
    let json_ct = ("content-type", "application/json");
    let create = json!({"resource_tenant_id": u(10), "payer_tenant_id": u(30),
                        "seller_tenant_id": u(20), "category": Category::NEW_SALE});
    let headers = [json_ct, ("idempotency-key", "prefixed")];
    let path = "/cf/bss-orders/v1/orders";
    let first = http(&gateway, "POST", path, &headers, Some(create.clone())).await;
    assert_eq!(first.0, 201, "{:?}", first.2);
    let id = first.2["order"]["order_id"].as_str().unwrap().to_owned();
    let location = format!("{path}/{id}");
    assert_eq!(first.1["location"], location.as_str());
    let replay = http(&gateway, "POST", path, &headers, Some(create)).await;
    assert_eq!((replay.0, &replay.2), (201, &first.2));
    assert_eq!(replay.1["location"], location.as_str());
    let read = http(&gateway, "GET", &location, &[], None).await;
    assert_eq!(read.0, 200, "{:?}", read.2);
    assert_eq!(read.2["order"]["order_id"], json!(id));
}

/// One in-process HTTP exchange through the mounted router (no TCP listener or host gateway).
pub(super) async fn http(
    router: &axum::Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, axum::http::HeaderMap, Value) {
    let mut request = axum::http::Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = body.map_or_else(axum::body::Body::empty, |b| {
        axum::body::Body::from(b.to_string())
    });
    let response = tower::ServiceExt::oneshot(router.clone(), request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, headers, body)
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_mounted_routes_carry_engine_outcomes_on_the_wire() {
    let t = T::new().await;
    let svc = Arc::new(t.service());
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = crate::api::rest::capture::router(Arc::clone(&svc), limiter(), &openapi)
        .layer(axum::Extension(buyer().ctx().clone()));
    let json_ct = ("content-type", "application/json");
    let problem = |h: &axum::http::HeaderMap| h["content-type"] == "application/problem+json";
    // Create: 201 with the OrderView, strong ETag and Location; the same key replays it exactly.
    let create = json!({"resource_tenant_id": u(10), "payer_tenant_id": u(30),
                        "seller_tenant_id": u(20), "category": Category::NEW_SALE});
    let first = http(
        &router,
        "POST",
        "/bss-orders/v1/orders",
        &[json_ct, ("idempotency-key", "w-c")],
        Some(create.clone()),
    )
    .await;
    assert_eq!(first.0, 201, "{:?}", first.2);
    let id = first.2["order"]["order_id"].as_str().unwrap().to_owned();
    assert_eq!(first.1["etag"], "\"1\"");
    assert_eq!(
        first.1["location"],
        format!("/bss-orders/v1/orders/{id}").as_str()
    );
    assert_eq!(first.2["draft_revision"], json!(0));
    let replay = http(
        &router,
        "POST",
        "/bss-orders/v1/orders",
        &[json_ct, ("idempotency-key", "w-c")],
        Some(create.clone()),
    )
    .await;
    assert_eq!((replay.0, &replay.2), (201, &first.2));
    assert_eq!(replay.1["location"], first.1["location"]);
    // Only the replay is marked (DNA REST §8); the stored response itself is unchanged.
    assert!(!first.1.contains_key("idempotency-replayed"));
    assert_eq!(replay.1["idempotency-replayed"], "true");
    // An actor override in the body is a boundary refusal that touches no registry row.
    let registry = t.registry().await;
    let mut forged = create.clone();
    forged["initiating_actor"] = json!(u(1));
    let bad = http(
        &router,
        "POST",
        "/bss-orders/v1/orders",
        &[json_ct, ("idempotency-key", "w-f")],
        Some(forged),
    )
    .await;
    assert_eq!(bad.0, 400);
    assert!(problem(&bad.1));
    assert_eq!(t.registry().await, registry);
    let order = format!("/bss-orders/v1/orders/{id}");
    let lines = format!("{order}/lines");
    let mut body = serde_json::to_value(line("EUR", Some(BillingCycle::Month))).unwrap();
    // In draft an omitted draft revision is a settled 409 naming the current counters.
    let conflict = http(
        &router,
        "POST",
        &lines,
        &[json_ct, ("idempotency-key", "w-0"), ("if-match", "\"1\"")],
        Some(body.clone()),
    )
    .await;
    assert_eq!(conflict.0, 409);
    assert!(problem(&conflict.1));
    assert_eq!(conflict.2["error_code"], "VERSION_CONFLICT");
    assert_eq!(
        conflict.2["context"]["data"],
        json!({"current_version": 1, "draft_revision": 0})
    );
    // Insert: 200 TransitionResult in snake_case with the reserved line_id.
    body["expected_draft_revision"] = json!(0);
    let added = http(
        &router,
        "POST",
        &lines,
        &[json_ct, ("idempotency-key", "w-1"), ("if-match", "\"1\"")],
        Some(body.clone()),
    )
    .await;
    assert_eq!(added.0, 200, "{:?}", added.2);
    assert_eq!(added.1["etag"], "\"1\"");
    assert_eq!(added.2["draft_revision"], json!(1));
    assert!(added.2.get("draftRevision").is_none());
    let line_id = added.2["line_id"].as_str().unwrap().to_owned();
    let again = http(
        &router,
        "POST",
        &lines,
        &[json_ct, ("idempotency-key", "w-1"), ("if-match", "\"1\"")],
        Some(body),
    )
    .await;
    assert_eq!((again.0, &again.2), (200, &added.2));
    // Line PATCH: `{fields, expected_draft_revision}`.
    let line_path = format!("{lines}/{line_id}");
    let edited = http(
        &router,
        "PATCH",
        &line_path,
        &[json_ct, ("idempotency-key", "w-2"), ("if-match", "\"1\"")],
        Some(json!({"fields": {"billing_cycle": "year"}, "expected_draft_revision": 1})),
    )
    .await;
    assert_eq!((edited.0, &edited.2["draft_revision"]), (200, &json!(2)));
    // A stale If-Match is the engine's settled precondition failure (412), not a boundary
    // refusal; the body names the current version and draft revision.
    let stale = http(
        &router,
        "PATCH",
        &order,
        &[json_ct, ("idempotency-key", "w-3"), ("if-match", "\"2\"")],
        Some(json!({"fields": {"contract_id": null}, "expected_draft_revision": 2})),
    )
    .await;
    assert_eq!(
        (stale.0, &stale.2["error_code"], &stale.2["status"]),
        (412, &json!("PRECONDITION_FAILED"), &json!(412))
    );
    assert_eq!(
        stale.2["context"]["data"],
        json!({"current_version": 1, "draft_revision": 2})
    );
    // Administrative-only PATCH: 503 Problem, nothing recorded.
    let registry = t.registry().await;
    let admin = http(
        &router,
        "PATCH",
        &order,
        &[json_ct, ("idempotency-key", "w-4"), ("if-match", "\"1\"")],
        Some(json!({"fields": {"external_reference": "PO-1"}})),
    )
    .await;
    assert_eq!(admin.0, 503);
    assert!(problem(&admin.1));
    assert_eq!(t.registry().await, registry);
    // DELETE without a body omits the revision (409); with the body it removes the member.
    let omitted = http(
        &router,
        "DELETE",
        &line_path,
        &[("idempotency-key", "w-5"), ("if-match", "\"1\"")],
        None,
    )
    .await;
    assert_eq!(omitted.0, 409);
    let removed = http(
        &router,
        "DELETE",
        &line_path,
        &[json_ct, ("idempotency-key", "w-6"), ("if-match", "\"1\"")],
        Some(json!({"expected_draft_revision": 2})),
    )
    .await;
    assert_eq!((removed.0, &removed.2["draft_revision"]), (200, &json!(3)));
    let gone = http(
        &router,
        "DELETE",
        &line_path,
        &[json_ct, ("idempotency-key", "w-7"), ("if-match", "\"1\"")],
        Some(json!({"expected_draft_revision": 3})),
    )
    .await;
    assert_eq!(
        (gone.0, &gone.2["error_code"]),
        (404, &json!("LINE_NOT_FOUND"))
    );
    assert!(problem(&gone.1));
    // Committed state behind the wire: revision 3, version 1, no member, reserved identity.
    let row = t.order_row(Uuid::parse_str(&id).unwrap()).await;
    assert_eq!(
        (
            row["draft_revision"].clone(),
            row["current_version"].clone()
        ),
        (json!(3), json!(1))
    );
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_line_identity WHERE line_id='{line_id}'"
        ))
        .await,
        1
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_local_sdk_returns_settled_refusals_with_their_diagnostics() {
    use crate::api::local_client::LocalOrdersClient;
    use bss_orders_sdk::OrdersV1;
    let t = T::new().await;
    let client = LocalOrdersClient::new(Arc::new(t.service()), Arc::new(t.reads()));
    let ctx = buyer().ctx().clone();
    let call = |k: &str| CallMeta {
        expected_version: OrderVersion::try_from(1).unwrap(),
        idempotency_key: key(k),
        correlation_id: None,
        delegation_proof_ref: None,
    };
    let created = client
        .create(&ctx, new_order(Category::NewSale), create_meta("s-c"))
        .await
        .unwrap();
    let id = created.order.order_id;
    assert_eq!(created.draft_revision.map(i64::from), Some(0));
    let added = client
        .add_line(
            &ctx,
            id,
            line("EUR", None),
            Some(DraftRevision::try_from(0).unwrap()),
            call("s-1"),
        )
        .await
        .unwrap();
    assert_eq!(added.draft_revision.map(i64::from), Some(1));
    assert!(added.line_id.is_some());
    // An omitted revision in draft: the settled version conflict names the current counters,
    // as the REST Problem does, and a same-key retry replays the identical refusal.
    for _ in 0..2 {
        let error = client
            .add_line(&ctx, id, line("EUR", None), None, call("s-2"))
            .await
            .unwrap_err();
        assert_eq!(error.reason(), Some(Reason::VersionConflict));
        let diagnostics = error.diagnostics().unwrap_or_else(|| panic!("{error:?}"));
        assert_eq!(diagnostics.current_version.map(i64::from), Some(1));
        assert_eq!(diagnostics.draft_revision.map(i64::from), Some(1));
        let problem = crate::api::rest::problem(error.clone());
        assert_eq!(
            problem.context["data"],
            json!({"current_version": 1, "draft_revision": 1})
        );
        assert_eq!(problem.status, Some(409));
    }
    // A guard refusal carries its registered reason through the same mapping.
    let error = client
        .add_line(
            &ctx,
            id,
            line("USD", None),
            Some(DraftRevision::try_from(1).unwrap()),
            call("s-3"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.reason(), Some(Reason::CurrencyMixed));
    assert_eq!(
        crate::api::rest::problem(error).error_code.as_deref(),
        Some("CURRENCY_MIXED")
    );
}

/// D-202 on the SDK write path: a credential-bearing delegation proof reference is
/// `request-invalid` before any PDP, registry or store work, exactly as the REST header and the
/// SDK reads refuse it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn sdk_writes_refuse_a_credential_bearing_proof_reference_before_the_pdp() {
    use crate::api::local_client::LocalOrdersClient;
    use bss_orders_sdk::OrdersV1;
    use bss_orders_sdk::models::DelegationProofRef;
    let t = T::new().await;
    let id = create(&t, &t.service(), "p-c").await;
    // A PDP that records every request and would answer an outage if it were reached.
    let pdp = ScriptedPdp::new(|_| None);
    let client = LocalOrdersClient::new(
        Arc::new(T::service_with(t.engine(pdp.clone()), 200)),
        Arc::new(t.reads()),
    );
    let ctx = buyer().ctx().clone();
    let jwt = || {
        Some(
            DelegationProofRef::try_from("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig".to_owned())
                .unwrap(),
        )
    };
    let registry = t.registry().await;
    let error = client
        .create(
            &ctx,
            new_order(Category::NewSale),
            CreateMeta {
                delegation_proof_ref: jwt(),
                ..create_meta("p-1")
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, OrdersError::Refused(Reason::RequestInvalid)),
        "{error:?}"
    );
    let error = client
        .add_line(
            &ctx,
            id,
            line("EUR", None),
            Some(DraftRevision::try_from(0).unwrap()),
            CallMeta {
                expected_version: OrderVersion::try_from(1).unwrap(),
                idempotency_key: key("p-2"),
                correlation_id: None,
                delegation_proof_ref: jwt(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, OrdersError::Refused(Reason::RequestInvalid)),
        "{error:?}"
    );
    // Refused at the boundary: no PDP request, no registry row.
    assert!(pdp.requests.lock().is_empty(), "{:?}", pdp.calls());
    assert_eq!(t.registry().await, registry);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn sdk_dates_outside_the_wire_encoding_are_request_invalid_not_a_panic() {
    use crate::api::local_client::LocalOrdersClient;
    use bss_orders_sdk::OrdersV1;
    let t = T::new().await;
    let client = LocalOrdersClient::new(Arc::new(t.service()), Arc::new(t.reads()));
    let ctx = buyer().ctx().clone();
    let id = client
        .create(&ctx, new_order(Category::NewSale), create_meta("d-c"))
        .await
        .unwrap()
        .order
        .order_id;
    let call = |k: &str| CallMeta {
        expected_version: OrderVersion::try_from(1).unwrap(),
        idempotency_key: key(k),
        correlation_id: None,
        delegation_proof_ref: None,
    };
    // REST cannot decode a negative year; an in-process caller can construct one.
    let negative = CalendarDate(time::Date::from_calendar_date(-1, time::Month::March, 1).unwrap());
    let (registry, audits) = (t.registry().await, t.audits(id).await);
    let mut bad = line("EUR", None);
    bad.service_activation_date = Some(negative);
    let error = client
        .add_line(
            &ctx,
            id,
            bad,
            Some(DraftRevision::try_from(0).unwrap()),
            call("d-1"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, OrdersError::Refused(Reason::RequestInvalid)),
        "{error:?}"
    );
    let patch = LinePatch {
        acceptance_due_date: Some(Some(negative)),
        ..LinePatch::default()
    };
    let error = client
        .patch_line(
            &ctx,
            id,
            u(999),
            patch,
            Some(DraftRevision::try_from(0).unwrap()),
            call("d-2"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, OrdersError::Refused(Reason::RequestInvalid)),
        "{error:?}"
    );
    // Boundary-refused: no registry row, audit or write.
    assert_eq!(t.registry().await, registry);
    assert_eq!(t.audits(id).await, audits);
}

#[test]
#[ignore = "requires Docker (testcontainers)"]
fn an_undecodable_committed_body_is_an_internal_failure_and_a_bad_refusal_stays_integration() {
    use crate::infra::capture::SettledOutcome;
    let settled = |status, body, outcome| Settled {
        response: StoredResponse::new(status, body, std::collections::BTreeMap::new(), None)
            .unwrap(),
        outcome,
        order_id: Some(u(1)),
        replayed: true,
    };
    // A committed success whose body is not the SDK type: the write settled, so this is an
    // unexpected persistence failure, not an authorization integration failure.
    let committed = settled(
        200,
        json!({"order_id": s(u(1)), "state": "invented"}),
        SettledOutcome::Committed,
    );
    assert!(matches!(
        sdk_result::<TransitionResult>(committed),
        Err(OrdersError::Internal)
    ));
    // The recorded outcome decides, not the stored status (D-219).
    let problem = |reason| {
        serde_json::to_value(crate::api::rest::error::settled_refusal_problem(
            reason,
            json!({"current_version": 4}),
        ))
        .unwrap()
    };
    let error = sdk_result::<TransitionResult>(settled(
        200,
        problem(Reason::VersionConflict),
        SettledOutcome::Refused(Some(Reason::VersionConflict)),
    ))
    .unwrap_err();
    assert_eq!(error.reason(), Some(Reason::VersionConflict));
    assert_eq!(
        error.diagnostics().unwrap().current_version.map(i64::from),
        Some(4)
    );
    // An unregistered code, a code that disagrees with the recorded reason, or an unknown
    // recorded token: integration, never a success or an unknown outcome.
    for (body, recorded) in [
        (
            json!({"error_domain": "orders.v1", "error_code": "NOT_A_REGISTERED_CODE"}),
            Some(Reason::NotAdmissible),
        ),
        (
            problem(Reason::NotAdmissible),
            Some(Reason::VersionConflict),
        ),
        (problem(Reason::NotAdmissible), None),
    ] {
        assert!(matches!(
            sdk_result::<TransitionResult>(settled(409, body, SettledOutcome::Refused(recorded))),
            Err(OrdersError::Integration)
        ));
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn only_draft_mutation_and_amendment_may_propose_an_axis_change() {
    use crate::authz::ArrangementDelta;
    use crate::domain::contributions::AggregateContribution;
    use crate::infra::engine::{
        EngineError, Preparation, PreparationView, PrepareError, Prepared, TransitionRequest,
    };
    use bss_orders_sdk::catalog::Trigger;
    struct Nothing;
    #[async_trait::async_trait]
    impl Preparation for Nothing {
        async fn prepare(&self, _: &PreparationView) -> Result<Prepared, PrepareError> {
            Ok(Prepared::new(AggregateContribution::None))
        }
    }
    let t = T::new().await;
    let svc = t.service();
    let id = create(&t, &svc, "c-1").await;
    let engine = t.engine(policy());
    let (row, registry, audits) = (
        t.order_row(id).await,
        t.registry().await,
        t.audits(id).await,
    );
    for (trigger, delta) in [
        (
            Trigger::Hold,
            ArrangementDelta {
                resource_tenant_id: None,
                payer_tenant_id: Some(u(31)),
            },
        ),
        (
            Trigger::Amendment,
            ArrangementDelta {
                resource_tenant_id: Some(u(11)),
                payer_tenant_id: Some(u(31)),
            },
        ),
        (Trigger::DraftMutate, ArrangementDelta::default()),
    ] {
        let request = TransitionRequest {
            order_id: id,
            trigger,
            operation: "patch_order",
            expected_version: OrderVersion::try_from(1).unwrap(),
            expected_draft_revision: Some(DraftRevision::try_from(0).unwrap()),
            document: json!({}),
            proposed: Some(delta),
            caller_reason: None,
            idempotency_key: key("misuse"),
            correlation_id: None,
            execution: None,
        };
        let error = engine
            .transition(&buyer(), request, &Nothing)
            .await
            .unwrap_err();
        assert!(
            matches!(error, EngineError::Integration),
            "{trigger:?}: {error:?}"
        );
    }
    // Refused before authorization, a read or any write: nothing recorded.
    assert_eq!(t.order_row(id).await, row);
    assert_eq!(t.registry().await, registry);
    assert_eq!(t.audits(id).await, audits);
}
