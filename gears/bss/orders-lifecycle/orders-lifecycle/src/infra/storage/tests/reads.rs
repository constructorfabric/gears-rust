//! Early S6-01/S6-04 on real PostgreSQL: the authorized draft reads through the shared read
//! service (REST and local SDK), the real rules PDP (S2-03) with a recording PDP for the
//! hidden/missing call pattern, outage and changed-facts cases, scoped keyset SQL pages, and
//! the `orders_read_access_log` served/refused decision table over the restricted runtime role.
//! Draft orders are authored through the S2-09 capture service and the S2-04 engine.
use super::capture::{
    BUYER, T, buyer, create, create_meta, http, line, new_order, result, view, write,
};
use super::*;
use crate::authz::ProofDenial;
use crate::authz::test_pdp::{
    RulesProvider, ScriptedPdp, allow, deny, eq, is_in, path, service, user, with_proof,
};
use crate::domain::read::{AccessOutcome, Collection, CursorBinding, Position};
use crate::infra::capture::{CaptureService, LocalOrdersClient};
use crate::infra::read::{ReadService, ReadSignals};
use arc_swap::ArcSwapOption;
use bss_orders_lifecycle_sdk::authoring::{BillingCycle, Category};
use bss_orders_lifecycle_sdk::catalog::{OrderState, Reason};
use bss_orders_lifecycle_sdk::reads::{
    Cursor, LineList, ListOrders, OrderFilters, OrderPage, PageSize, ReadMeta,
};
use bss_orders_lifecycle_sdk::{OrdersError, OrdersLifecycleV1};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

const ORDER_RT: &str = "gts.cf.bss.orders.order.v1~";
/// Same-tenant reader whose only path is its own resource tenant (provably confined).
const CONFINED: (u128, u128) = (110, 10);
/// Seller operator on seller tenant 20 (direct cross-tenant read, no proof).
const SELLER: (u128, u128) = (103, 20);
/// Payer reader on payer tenant 30.
const PAYER: (u128, u128) = (105, 30);
/// Delegated-only partner in tenant 60: reads resource tenant 10 with an accepted proof.
const PARTNER: (u128, u128) = (109, 60);
/// Workflow service principal (configured identity 106/99): finite order IDs only.
const WORKFLOW: (u128, u128) = (106, 99);
/// Tenant member with no grant at all.
const MEMBER: (u128, u128) = (108, 10);
/// Seller operator of another seller: an authorized scope that matches nothing here.
const OTHER_SELLER: (u128, u128) = (111, 21);
const PROOF: &str = "proof-ok";

fn s(id: Uuid) -> String {
    id.to_string()
}
fn rule(id: &str, subject: u128, actions: &[&str], property: &str, values: &[Uuid]) -> Value {
    json!({"id": id, "subject": {"id": s(u(subject))}, "resource_type": ORDER_RT,
        "actions": actions,
        "paths": [{"predicates": [{"property": property,
            "values": values.iter().map(|v| s(*v)).collect::<Vec<_>>()}]}]})
}
/// The read policy; `workflow_ids` are the explicit finite order IDs of the service principal.
fn policy(workflow_ids: &[Uuid]) -> Value {
    let mut buyer = rule(
        "buyer",
        BUYER.0,
        &["create", "write", "edit", "read"],
        "resource_tenant_id",
        &[u(10), u(11)],
    );
    buyer["payer_use"] = json!({"property": "payer_tenant_id", "values": [s(u(30)), s(u(31))]});
    let mut partner = rule(
        "partner-delegated",
        PARTNER.0,
        &["read"],
        "resource_tenant_id",
        &[u(10)],
    );
    partner["delegation"] = json!({"proof_property": "delegation_proof_ref", "accepted": [PROOF]});
    let mut rules = vec![
        buyer,
        rule(
            "confined",
            CONFINED.0,
            &["read"],
            "resource_tenant_id",
            &[u(10)],
        ),
        rule(
            "seller",
            SELLER.0,
            &["read", "hold", "resume"],
            "seller_tenant_id",
            &[u(20)],
        ),
        rule(
            "payer-reader",
            PAYER.0,
            &["read"],
            "payer_tenant_id",
            &[u(30)],
        ),
        rule(
            "other-seller",
            OTHER_SELLER.0,
            &["read"],
            "seller_tenant_id",
            &[u(21)],
        ),
        partner,
    ];
    // The service principal's rule exists only with a finite explicit order set (08 §4.3).
    if !workflow_ids.is_empty() {
        rules.push(json!({"id": "workflow", "subject": {"id": s(u(WORKFLOW.0))},
            "resource_type": ORDER_RT, "actions": ["read"],
            "paths": [{"predicates": [
                {"property": "id", "values": workflow_ids.iter().map(|v| s(*v)).collect::<Vec<_>>()},
                {"property": "seller_tenant_id", "values": [s(u(20))]}]}]}));
    }
    json!({"vendor": "constructorfabric", "priority": 10, "policy_revision": "s6-01-draft",
        "rules": rules})
}

struct R {
    t: T,
    capture: Arc<CaptureService>,
    reads: Arc<ReadService>,
    /// Rows already accounted for: the log is append-only and its retention guard refuses a
    /// young delete, so "clearing" advances this mark instead.
    mark: std::sync::atomic::AtomicUsize,
}
impl R {
    async fn new() -> Self {
        let t = T::new().await;
        let provider = RulesProvider::from_policy(policy(&[]));
        let capture = Arc::new(T::service_with(t.engine(provider.clone()), 200));
        let reads = Arc::new(t.reads_with(provider));
        Self {
            t,
            capture,
            reads,
            mark: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn reads_with(&self, api: Arc<dyn authz_resolver_sdk::AuthZResolverApi>) -> ReadService {
        self.t.reads_with(api)
    }
    /// The real rules PDP with recording signals, so the §3.8 instruments can be asserted.
    fn reads_recording(&self) -> (Arc<RecordingSignals>, ReadService) {
        let signals = Arc::new(RecordingSignals::default());
        let reads = self.t.reads_with_signals(
            RulesProvider::from_policy(policy(&[])),
            Arc::clone(&signals) as Arc<dyn ReadSignals>,
        );
        (signals, reads)
    }
    /// The database wall clock, which every evidence timestamp must come from (OL-2).
    async fn db_clock(&self) -> time::OffsetDateTime {
        instant(&self.t.json("SELECT to_jsonb(clock_timestamp()) AS j").await)
    }
    fn sdk(&self) -> LocalOrdersClient {
        LocalOrdersClient::new(Arc::clone(&self.capture), Arc::clone(&self.reads))
    }
    /// Every access-log row appended since the mark, oldest first, as JSON.
    async fn logs(&self) -> Vec<Value> {
        let rows = self
            .t
            .json(
                "SELECT coalesce(jsonb_agg(to_jsonb(l) ORDER BY accessed_at, access_id), '[]'::jsonb) AS j \
                 FROM bss_orders__read_access_log l",
            )
            .await;
        let all = rows.as_array().cloned().unwrap_or_default();
        all[self.mark.load(Ordering::SeqCst).min(all.len())..].to_vec()
    }
    async fn log_count(&self) -> i64 {
        i64::try_from(self.logs().await.len()).unwrap()
    }
    async fn clear_logs(&self) {
        let total = self
            .t
            .n("SELECT count(*) AS n FROM bss_orders__read_access_log")
            .await;
        self.mark
            .store(usize::try_from(total).unwrap(), Ordering::SeqCst);
    }
    /// A draft with `n` working lines authored by the buyer (and one removed line).
    async fn draft(&self, tag: &str, n: usize) -> (Uuid, Vec<Uuid>) {
        let id = create(&self.t, &self.capture, &format!("{tag}-c")).await;
        let mut lines = Vec::new();
        let mut revision = 0;
        for i in 0..n {
            let r = result(
                self.capture
                    .add_line(
                        &buyer(),
                        id,
                        line("EUR", Some(BillingCycle::Month)),
                        &write(&format!("{tag}-l{i}"), 1, Some(revision)),
                    )
                    .await
                    .unwrap(),
            );
            revision += 1;
            lines.push(r.line_id.unwrap());
        }
        // One removed identity stays reserved and must never be a current line.
        let removed = result(
            self.capture
                .add_line(
                    &buyer(),
                    id,
                    line("EUR", None),
                    &write(&format!("{tag}-lr"), 1, Some(revision)),
                )
                .await
                .unwrap(),
        )
        .line_id
        .unwrap();
        revision += 1;
        result(
            self.capture
                .remove_line(
                    &buyer(),
                    id,
                    removed,
                    &write(&format!("{tag}-rm"), 1, Some(revision)),
                )
                .await
                .unwrap(),
        );
        (id, lines)
    }
}
fn refusal(error: &OrdersError) -> Reason {
    match error {
        OrdersError::Refused(reason) => *reason,
        other => panic!("not a refusal: {other:?}"),
    }
}
/// A `timestamptz` as PostgreSQL renders it in JSON.
fn instant(value: &Value) -> time::OffsetDateTime {
    time::OffsetDateTime::parse(
        value.as_str().expect("a timestamp"),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap()
}

/// Records the read signals of 08 §3.8 (log-write failures by outcome, refusals by reason with
/// the operational proof classification) so the tests can assert that a failure or refusal
/// actually raised its instrument.
#[derive(Default)]
struct RecordingSignals {
    log_failures: std::sync::Mutex<Vec<AccessOutcome>>,
    refusals: std::sync::Mutex<Vec<(Reason, Option<ProofDenial>)>>,
}
impl RecordingSignals {
    fn log_failures(&self) -> Vec<AccessOutcome> {
        self.log_failures.lock().unwrap().clone()
    }
    fn refusals(&self) -> Vec<(Reason, Option<ProofDenial>)> {
        self.refusals.lock().unwrap().clone()
    }
}
impl ReadSignals for RecordingSignals {
    fn access_log_write_failed(&self, outcome: AccessOutcome) {
        self.log_failures.lock().unwrap().push(outcome);
    }
    fn refused(&self, reason: Reason, proof: Option<ProofDenial>) {
        self.refusals.lock().unwrap().push((reason, proof));
    }
}
fn list_all() -> ListOrders {
    ListOrders::default()
}
fn page(page_size: u64, cursor: Option<Cursor>) -> ListOrders {
    ListOrders {
        filters: OrderFilters::default(),
        page_size: Some(PageSize::try_from(page_size).unwrap()),
        cursor,
    }
}
fn meta(proof: Option<&str>) -> ReadMeta {
    ReadMeta {
        correlation_id: None,
        delegation_proof_ref: proof.map(|p| p.to_owned().try_into().unwrap()),
    }
}

/// 08 §3.6 *Read One Order* steps 4–9 and the first §4.4 row on the point read: composition
/// from the authorized snapshot in identity order with the removed line excluded, a coherent
/// version and draft revision, and an own-resource-tenant read without proof that appends
/// nothing — on the service and through the local SDK alike.
#[tokio::test]
async fn point_reads_compose_the_draft_from_the_snapshot_and_own_tenant_reads_log_nothing() {
    let r = R::new().await;
    let (id, lines) = r.draft("p", 3).await;
    let view = r.reads.get(&buyer(), id).await.unwrap();
    assert_eq!(view.order.order_id, id);
    assert_eq!(view.order.state, OrderState::Draft);
    assert_eq!(view.order.category, Category::NewSale);
    assert_eq!(i64::from(view.version.version), 1);
    assert_eq!(view.version.supersedes_version, None);
    assert_eq!(view.draft_revision.map(i64::from), Some(5));
    assert_eq!(
        view.lines.iter().map(|l| l.line_id).collect::<Vec<_>>(),
        lines,
        "lines follow (created_at, line_id) identity order; the removed line is absent"
    );
    assert_eq!(view.lines[0].currency.as_str(), "EUR");
    assert_eq!(view.lines[0].billing_cycle, Some(BillingCycle::Month));
    assert_eq!(
        view.lines[0].term_duration,
        Some(bss_orders_lifecycle_sdk::authoring::AuthoredTerm::Periods { count: 12 })
    );
    assert_eq!(
        r.log_count().await,
        0,
        "own-tenant read without proof logs nothing"
    );
    // The local SDK returns the same view under the same wrapper.
    let sdk = r.sdk();
    let via_sdk = sdk.get(buyer().ctx(), id, meta(None)).await.unwrap();
    assert_eq!(via_sdk, view);
    assert_eq!(r.log_count().await, 0);
}

/// 08 §2.2 on the line collection: identity order `(created_at, line_id)` through the current
/// parent, `page_size + 1` with the N/N+1 boundaries, and a cursor that survives removal of the
/// member that issued it (the token is a position, never a row lookup); the removed identity is
/// not a current line, and the page carries the revision of its own snapshot.
#[tokio::test]
async fn line_pages_follow_identity_order_and_a_cursor_survives_the_removed_member() {
    let r = R::new().await;
    let (id, lines) = r.draft("p", 3).await;
    let lines_of = |page: &bss_orders_lifecycle_sdk::reads::LinePage| {
        page.lines.iter().map(|l| l.line_id).collect::<Vec<_>>()
    };
    let request = |page_size: u64, cursor: Option<Cursor>| LineList {
        page_size: Some(PageSize::try_from(page_size).unwrap()),
        cursor,
    };
    // Page size 2 then the cursor: no duplicate, no skip, no cursor after the last member.
    let first = r
        .reads
        .list_lines(&buyer(), id, &request(2, None))
        .await
        .unwrap();
    assert_eq!(lines_of(&first), lines[..2]);
    assert_eq!(i64::from(first.current_version), 1);
    assert_eq!(first.draft_revision.map(i64::from), Some(5));
    let token = first.next_cursor.clone().expect("a third line follows");
    let second = r
        .reads
        .list_lines(&buyer(), id, &request(2, Some(token)))
        .await
        .unwrap();
    assert_eq!(lines_of(&second), lines[2..]);
    assert!(second.next_cursor.is_none());
    // A cursor survives the removal of the member that issued it: the continuation is strictly
    // after the encoded position, and the removed identity is not a current line.
    let one = r
        .reads
        .list_lines(&buyer(), id, &request(1, None))
        .await
        .unwrap();
    assert_eq!(one.lines[0].line_id, lines[0]);
    result(
        r.capture
            .remove_line(&buyer(), id, lines[0], &write("p-rm0", 1, Some(5)))
            .await
            .unwrap(),
    );
    let continued = r
        .reads
        .list_lines(&buyer(), id, &request(1, one.next_cursor))
        .await
        .unwrap();
    assert_eq!(continued.lines[0].line_id, lines[1]);
    assert_eq!(continued.draft_revision.map(i64::from), Some(6));
    let fresh = r
        .reads
        .list_lines(&buyer(), id, &LineList::default())
        .await
        .unwrap();
    assert_eq!(lines_of(&fresh), lines[1..]);
    // The exact page boundary (N rows at page size N) returns every remaining member and no
    // cursor; one row past it issued one above.
    let remaining = lines[1..].to_vec();
    let exact = r
        .reads
        .list_lines(&buyer(), id, &request(2, None))
        .await
        .unwrap();
    assert_eq!(
        lines_of(&exact),
        remaining,
        "the two remaining members fill the page exactly"
    );
    assert!(exact.next_cursor.is_none());
    assert_eq!(r.log_count().await, 0, "own-tenant line pages log nothing");
}

/// D-139 on the wire-independent service path: a line token binds the principal, the parent
/// and the collection, so replayed by another principal, against another parent or on the order
/// list it is `cursor-invalid` before the access decision, with no access-log row.
#[tokio::test]
async fn line_tokens_bind_principal_parent_and_collection() {
    let r = R::new().await;
    let (id, _) = r.draft("p", 3).await;
    let token = r
        .reads
        .list_lines(
            &buyer(),
            id,
            &LineList {
                page_size: Some(PageSize::try_from(2).unwrap()),
                cursor: None,
            },
        )
        .await
        .unwrap()
        .next_cursor
        .expect("a third line follows");
    let replay = |cursor: Cursor| LineList {
        page_size: None,
        cursor: Some(cursor),
    };
    let other = r
        .reads
        .list_lines(&user(CONFINED.0, CONFINED.1), id, &replay(token.clone()))
        .await
        .unwrap_err();
    assert_eq!(refusal(&other), Reason::CursorInvalid);
    let (other_order, _) = r.draft("q", 1).await;
    let cross = r
        .reads
        .list_lines(&buyer(), other_order, &replay(token.clone()))
        .await
        .unwrap_err();
    assert_eq!(refusal(&cross), Reason::CursorInvalid);
    let as_list = r
        .reads
        .list(&buyer(), &page(50, Some(token)))
        .await
        .unwrap_err();
    assert_eq!(refusal(&as_list), Reason::CursorInvalid);
    assert_eq!(r.log_count().await, 0, "input validation appends no row");
}

/// §4.4 rows 2–3 and the refused row on the point/child reads: a supplied proof on an own-tenant
/// read logs; direct seller and payer reads log with a NULL proof; the delegated partner logs
/// the proof PDP accepted; the partner without it is `order-not-found` with the classified
/// detail kept operational-only (D-141); the SDK records a supplied proof and refuses a
/// credential-bearing one before any decision; `accessed_at` is the database clock (OL-2).
#[tokio::test]
async fn served_rows_follow_the_decision_table_and_a_targeted_proof_denial_keeps_its_detail() {
    let r = R::new().await;
    let (id, _) = r.draft("p", 3).await;
    let before = r.db_clock().await;
    r.reads.get(&with_proof(&buyer(), PROOF), id).await.unwrap();
    r.reads.get(&user(SELLER.0, SELLER.1), id).await.unwrap();
    r.reads
        .list_lines(&user(PAYER.0, PAYER.1), id, &LineList::default())
        .await
        .unwrap();
    let partner = with_proof(&user(PARTNER.0, PARTNER.1), PROOF);
    r.reads.get(&partner, id).await.unwrap();
    let after = r.db_clock().await;
    let logs = r.logs().await;
    let expect = |row: &Value, actor: u128, op: &str, proof: Option<&str>| {
        assert_eq!(row["actor"], json!(s(u(actor))));
        assert_eq!(row["actor_class"], json!("user"));
        assert_eq!(row["operation"], json!(op));
        assert_eq!(row["outcome"], json!("served"));
        assert_eq!(row["order_id"], json!(s(id)));
        assert_eq!(row["requested_order_ref"], json!(s(id)));
        assert_eq!(row["refusal_reason"], Value::Null);
        assert_eq!(row["internal_refusal_detail"], Value::Null);
        assert_eq!(
            row["delegation_proof_ref"],
            proof.map_or(Value::Null, |p| json!(p))
        );
        let at = instant(&row["accessed_at"]);
        assert!(
            before <= at && at <= after,
            "accessed_at {at} is the database clock between {before} and {after}"
        );
    };
    assert_eq!(logs.len(), 4, "{logs:?}");
    expect(&logs[0], BUYER.0, "get", Some(PROOF));
    expect(&logs[1], SELLER.0, "get", None);
    expect(&logs[2], PAYER.0, "list_lines", None);
    expect(&logs[3], PARTNER.0, "get", Some(PROOF));
    // The partner without its proof has no path: order-not-found, logged refused with the
    // classified detail kept operational-only (D-141).
    r.clear_logs().await;
    let denied = r
        .reads
        .get(&user(PARTNER.0, PARTNER.1), id)
        .await
        .unwrap_err();
    assert_eq!(refusal(&denied), Reason::OrderNotFound);
    let logs = r.logs().await;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["outcome"], json!("refused"));
    assert_eq!(logs[0]["refusal_reason"], json!("order-not-found"));
    assert_eq!(logs[0]["order_id"], json!(s(id)));
    assert_eq!(logs[0]["requested_order_ref"], json!(s(id)));
    assert_eq!(
        logs[0]["internal_refusal_detail"],
        json!("delegation-proof-required")
    );
    // The SDK path records a supplied proof too, and refuses a credential-bearing reference
    // before any decision.
    r.clear_logs().await;
    let sdk = r.sdk();
    let sdk_view = sdk
        .get(buyer().ctx(), id, meta(Some("ref-7")))
        .await
        .unwrap();
    assert_eq!(sdk_view, r.reads.get(&buyer(), id).await.unwrap());
    let logs = r.logs().await;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["delegation_proof_ref"], json!("ref-7"));
    let bad = sdk
        .get(
            buyer().ctx(),
            id,
            meta(Some("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig")),
        )
        .await
        .unwrap_err();
    assert_eq!(refusal(&bad), Reason::RequestInvalid);
    assert_eq!(r.log_count().await, 1);
}

/// 08 §3.6 item 2 / D-68 / D-141: hidden and missing targets answer the same refusal, make the
/// same PDP calls, and both leave a refused row whose only difference is the FK column; PDP
/// outage returns the sanitized 503 on both arms with no row; invalid constraints fail closed
/// with no row; a targeted proof denial is `order-not-found` with the operational detail.
#[tokio::test]
async fn hidden_and_missing_targets_are_indistinguishable_and_both_arms_log() {
    let r = R::new().await;
    let (id, _) = r.draft("h", 1).await;
    let missing = u(0xdead);
    // The real provider: the member-only user has no relationship to either target.
    let member = user(MEMBER.0, MEMBER.1);
    let hidden = r.reads.get(&member, id).await.unwrap_err();
    let absent = r.reads.get(&member, missing).await.unwrap_err();
    assert_eq!(refusal(&hidden), Reason::OrderNotFound);
    assert_eq!(refusal(&absent), Reason::OrderNotFound);
    assert_eq!(
        crate::api::rest::problem(hidden).status,
        crate::api::rest::problem(absent).status
    );
    let hidden_lines = r
        .reads
        .list_lines(&member, id, &LineList::default())
        .await
        .unwrap_err();
    assert_eq!(refusal(&hidden_lines), Reason::OrderNotFound);
    let logs = r.logs().await;
    assert_eq!(logs.len(), 3, "{logs:?}");
    assert_eq!(logs[0]["order_id"], json!(s(id)));
    assert_eq!(logs[0]["requested_order_ref"], json!(s(id)));
    assert_eq!(
        logs[1]["order_id"],
        Value::Null,
        "missing arm keeps the FK NULL"
    );
    assert_eq!(logs[1]["requested_order_ref"], json!(s(missing)));
    assert_eq!(logs[2]["operation"], json!("list_lines"));
    for row in &logs {
        assert_eq!(row["outcome"], json!("refused"));
        assert_eq!(row["refusal_reason"], json!("order-not-found"));
        assert_eq!(row["internal_refusal_detail"], Value::Null);
        assert_eq!(row["actor"], json!(s(u(MEMBER.0))));
    }
    // The recording PDP: both arms make exactly one `read` decision on the target ID; the
    // hidden arm carries the prefetched axes, the missing arm an empty property set.
    let recorder = ScriptedPdp::new(|_| Some(deny(None)));
    let scripted = r.reads_with(recorder.clone());
    scripted.get(&member, id).await.unwrap_err();
    scripted.get(&member, missing).await.unwrap_err();
    let calls = recorder.calls();
    assert_eq!(
        calls,
        vec![
            ("read".to_owned(), Some(id)),
            ("read".to_owned(), Some(missing))
        ]
    );
    let carried: Vec<bool> = recorder
        .requests
        .lock()
        .iter()
        .map(|request| {
            request
                .resource
                .properties
                .contains_key("resource_tenant_id")
        })
        .collect();
    assert_eq!(carried, vec![true, false]);
    // A proof denial on a targeted read: public order-not-found, operational detail in the row.
    r.clear_logs().await;
    let proof_denied = r.reads_with(ScriptedPdp::new(|_| {
        Some(deny(Some(crate::authz::DENY_DELEGATION_PROOF_INVALID)))
    }));
    let caller = with_proof(&buyer(), "stale-proof");
    assert_eq!(
        refusal(&proof_denied.get(&caller, id).await.unwrap_err()),
        Reason::OrderNotFound
    );
    assert_eq!(
        refusal(&proof_denied.get(&caller, missing).await.unwrap_err()),
        Reason::OrderNotFound
    );
    let logs = r.logs().await;
    assert_eq!(logs.len(), 2);
    for row in &logs {
        assert_eq!(row["refusal_reason"], json!("order-not-found"));
        assert_eq!(
            row["internal_refusal_detail"],
            json!("delegation-proof-invalid")
        );
        assert_eq!(row["delegation_proof_ref"], json!("stale-proof"));
    }
    // An untargeted proof denial discloses the classified reason and keeps no hidden detail.
    r.clear_logs().await;
    let listed = proof_denied.list(&caller, &list_all()).await.unwrap_err();
    assert_eq!(refusal(&listed), Reason::DelegationProofInvalid);
    let logs = r.logs().await;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["refusal_reason"], json!("delegation-proof-invalid"));
    assert_eq!(logs[0]["internal_refusal_detail"], Value::Null);
    assert_eq!(logs[0]["order_id"], Value::Null);
    assert_eq!(logs[0]["requested_order_ref"], Value::Null);
    // PDP outage: sanitized unavailable on both arms, never order-not-found, and no row.
    r.clear_logs().await;
    let down = r.reads_with(ScriptedPdp::new(|_| None));
    for target in [id, missing] {
        assert!(matches!(
            down.get(&buyer(), target).await.unwrap_err(),
            OrdersError::Unavailable
        ));
    }
    assert!(matches!(
        down.list(&buyer(), &list_all()).await.unwrap_err(),
        OrdersError::Unavailable
    ));
    // Invalid constraints (an unsupported property) are an integration failure, not a refusal.
    let invalid = r.reads_with(ScriptedPdp::new(|_| {
        Some(allow(vec![path(vec![eq("owner_tenant_id", u(10))])]))
    }));
    assert!(matches!(
        invalid.get(&buyer(), id).await.unwrap_err(),
        OrdersError::Integration
    ));
    assert!(matches!(
        invalid.list(&buyer(), &list_all()).await.unwrap_err(),
        OrdersError::Integration
    ));
    // Constraints that exclude the target are a denial for it: not-found, not the row.
    let elsewhere = r.reads_with(ScriptedPdp::new(|_| {
        Some(allow(vec![path(vec![eq("resource_tenant_id", u(12))])]))
    }));
    assert_eq!(
        refusal(&elsewhere.get(&buyer(), id).await.unwrap_err()),
        Reason::OrderNotFound
    );
    assert_eq!(r.log_count().await, 1);
    // Reads are unavailable while the gear is not serving: no decision, no row.
    let unready = ReadService::new(
        Arc::new(ArcSwapOption::from(None)),
        Arc::new(crate::infra::read::OtelReadSignals::new()),
    );
    assert!(matches!(
        unready.get(&buyer(), id).await.unwrap_err(),
        OrdersError::Unavailable
    ));
    assert_eq!(r.log_count().await, 1);
}

/// 08 §3.6 *List Orders*: PDP scope, filters and the cursor boundary in SQL before
/// ORDER/LIMIT; immutable `(created_at, order_id)` order with same-instant ties; N/N+1 page
/// boundaries; the collection logging rule on confined, broad, empty and mixed scopes; the
/// finite service path; and cursor binding across principals and filters.
#[tokio::test]
async fn the_order_list_pages_the_scoped_set_in_sql_and_logs_by_effective_scope() {
    let r = R::new().await;
    // Three raw rows at one identical instant (ties resolved by binary order_id), then three
    // engine-created drafts on resource tenant 10 and one on 11, all seller 20 / payer 30.
    let scope = toolkit_security::AccessScope::for_resources(vec![u(3), u(1), u(2)]);
    r.t.env
        .db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                for id in [3u128, 1, 2] {
                    let row = repo::insert_order(tx, &scope, order(id)).await?;
                    let locked = repo::LockedOrder::acquire(tx, &scope, &row).await?;
                    locked.insert_order_version(version(id, 1, None)).await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let mut created = Vec::new();
    for tag in ["a", "b", "c"] {
        created.push(create(&r.t, &r.capture, tag).await);
    }
    let mut on_11 = new_order(Category::NewSale);
    on_11.resource_tenant_id = u(11);
    let foreign = view(
        r.capture
            .create(&buyer(), on_11, &create_meta("f"))
            .await
            .unwrap(),
    )
    .order
    .order_id;
    let expected: Vec<Uuid> = [u(1), u(2), u(3)]
        .into_iter()
        .chain(created.iter().copied())
        .chain([foreign])
        .collect();
    // The buyer sees all seven in order through page size 2 with no duplicate or skip.
    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page: OrderPage = r.reads.list(&buyer(), &page(2, cursor)).await.unwrap();
        pages += 1;
        seen.extend(page.orders.iter().map(|o| o.order.order_id));
        for summary in &page.orders {
            assert_eq!(i64::from(summary.current_version), 1);
            assert_eq!(summary.draft_revision.map(i64::from), Some(0));
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen, expected);
    assert_eq!(
        pages, 4,
        "7 rows at page size 2: 2+2+2+1, the last page without a cursor"
    );
    // The buyer's scope spans resource tenants 10 and 11: not provably confined, so every
    // page (the empty ones included) logs one served row with both target columns NULL.
    let logs = r.logs().await;
    assert_eq!(logs.len(), 4, "{logs:?}");
    for row in &logs {
        assert_eq!(row["outcome"], json!("served"));
        assert_eq!(row["operation"], json!("list"));
        assert_eq!(row["order_id"], Value::Null);
        assert_eq!(row["requested_order_ref"], Value::Null);
        assert_eq!(row["delegation_proof_ref"], Value::Null);
    }
    r.clear_logs().await;
    // A reader whose only path is its own resource tenant is confined: six rows, no log row,
    // including an empty page under a filter that matches nothing.
    let confined = user(CONFINED.0, CONFINED.1);
    let page_c = r.reads.list(&confined, &page(200, None)).await.unwrap();
    assert_eq!(page_c.orders.len(), 6);
    assert!(
        page_c
            .orders
            .iter()
            .all(|o| o.order.resource_tenant_id == u(10))
    );
    let none = r
        .reads
        .list(
            &confined,
            &ListOrders {
                filters: OrderFilters {
                    contract_id: Some(u(0xc0)),
                    ..OrderFilters::default()
                },
                page_size: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(none.orders.is_empty() && none.next_cursor.is_none());
    assert_eq!(r.log_count().await, 0, "a confined collection never logs");
    // A proof on the confined reader's allowed request logs anyway.
    r.reads
        .list(&with_proof(&confined, PROOF), &list_all())
        .await
        .unwrap();
    assert_eq!(r.log_count().await, 1);
    r.clear_logs().await;
    // The seller sees the seller-20 set (seven) and logs; another seller's authorized scope
    // matches nothing and still logs its empty page.
    let seller_page = r
        .reads
        .list(&user(SELLER.0, SELLER.1), &page(200, None))
        .await
        .unwrap();
    assert_eq!(seller_page.orders.len(), 7);
    let empty = r
        .reads
        .list(&user(OTHER_SELLER.0, OTHER_SELLER.1), &list_all())
        .await
        .unwrap();
    assert!(empty.orders.is_empty());
    assert_eq!(r.log_count().await, 2);
    // The payer reader sees payer-30 rows; the member-only user is refused untargeted.
    let payer_page = r
        .reads
        .list(&user(PAYER.0, PAYER.1), &page(200, None))
        .await
        .unwrap();
    assert_eq!(payer_page.orders.len(), 7);
    r.clear_logs().await;
    let denied = r
        .reads
        .list(&user(MEMBER.0, MEMBER.1), &list_all())
        .await
        .unwrap_err();
    assert_eq!(refusal(&denied), Reason::OperationNotPermittedForActor);
    let logs = r.logs().await;
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["outcome"], json!("refused"));
    assert_eq!(
        logs[0]["refusal_reason"],
        json!("operation-not-permitted-for-actor")
    );
    assert_eq!(logs[0]["order_id"], Value::Null);
    assert_eq!(logs[0]["requested_order_ref"], Value::Null);
    r.clear_logs().await;
}

/// 08 §3.6 *List Orders* steps 1-3 and 6 and §2.2: filters apply in SQL with the keyset
/// continuing inside the filtered set; tokens bind the principal and the normalized filters;
/// a cursor outlives the row that issued it; a forged position carries no authority; and the
/// Workflow service principal reads only its finite explicit order IDs (08 §4.3).
#[tokio::test]
async fn list_filters_cursors_and_service_scopes_are_applied_in_sql() {
    let r = R::new().await;
    let scope = toolkit_security::AccessScope::for_resources(vec![u(3), u(1), u(2)]);
    r.t.env
        .db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                for id in [3u128, 1, 2] {
                    let row = repo::insert_order(tx, &scope, order(id)).await?;
                    let locked = repo::LockedOrder::acquire(tx, &scope, &row).await?;
                    locked.insert_order_version(version(id, 1, None)).await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let mut created = Vec::new();
    for tag in ["a", "b", "c"] {
        created.push(create(&r.t, &r.capture, tag).await);
    }
    // Filters apply in SQL: state, created window (from inclusive, to exclusive), contract
    // and state_entered_before, with the keyset continuing inside the filtered set.
    let fixture_at = now();
    let filtered = r
        .reads
        .list(
            &buyer(),
            &ListOrders {
                filters: OrderFilters {
                    state: Some(OrderState::Draft),
                    created_from: Some(fixture_at),
                    created_to: Some(fixture_at + time::Duration::microseconds(1)),
                    state_entered_before: Some(fixture_at),
                    contract_id: None,
                },
                page_size: Some(PageSize::try_from(2).unwrap()),
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        filtered
            .orders
            .iter()
            .map(|o| o.order.order_id)
            .collect::<Vec<_>>(),
        vec![u(1), u(2)]
    );
    let rest = r
        .reads
        .list(
            &buyer(),
            &ListOrders {
                filters: OrderFilters {
                    state: Some(OrderState::Draft),
                    created_from: Some(fixture_at),
                    created_to: Some(fixture_at + time::Duration::microseconds(1)),
                    state_entered_before: Some(fixture_at),
                    contract_id: None,
                },
                page_size: Some(PageSize::try_from(2).unwrap()),
                cursor: filtered.next_cursor.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        rest.orders
            .iter()
            .map(|o| o.order.order_id)
            .collect::<Vec<_>>(),
        vec![u(3)]
    );
    assert!(rest.next_cursor.is_none());
    let exclusive = r
        .reads
        .list(
            &buyer(),
            &ListOrders {
                filters: OrderFilters {
                    created_to: Some(fixture_at),
                    ..OrderFilters::default()
                },
                page_size: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(exclusive.orders.is_empty(), "created_to is exclusive");
    let by_contract = r
        .reads
        .list(
            &buyer(),
            &ListOrders {
                filters: OrderFilters {
                    contract_id: Some(u(0xc0)),
                    ..OrderFilters::default()
                },
                page_size: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(by_contract.orders.is_empty());
    let other_state = r
        .reads
        .list(
            &buyer(),
            &ListOrders {
                filters: OrderFilters {
                    state: Some(OrderState::Submitted),
                    ..OrderFilters::default()
                },
                page_size: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(other_state.orders.is_empty());
    // Tokens are bound to the principal and to the normalized filters.
    let token = filtered.next_cursor.unwrap();
    let as_seller = r
        .reads
        .list(&user(SELLER.0, SELLER.1), &page(2, Some(token.clone())))
        .await
        .unwrap_err();
    assert_eq!(refusal(&as_seller), Reason::CursorInvalid);
    let other_filters = r
        .reads
        .list(&buyer(), &page(2, Some(token)))
        .await
        .unwrap_err();
    assert_eq!(refusal(&other_filters), Reason::CursorInvalid);
    // Order rows are immutable evidence and are never deleted by retention (only refused
    // audit, Preview diagnostics and the access log are), so the "cursor row deleted" case
    // applies to the line collection (a removed draft member) and is covered there.
    // A hand-built token at a position strictly before every row carries no authority: the
    // member-only user is still refused and a confined reader still sees only its own tenant.
    let hash = crate::domain::read::no_filters_hash();
    let forged = crate::domain::read::encode_cursor(
        CursorBinding {
            collection: Collection::Orders,
            parent: None,
            subject_id: u(MEMBER.0),
            subject_tenant_id: u(MEMBER.1),
            filters_hash: &hash,
        },
        Position {
            at_micros: 0,
            id: Uuid::nil(),
        },
    )
    .unwrap();
    let forged_denied = r
        .reads
        .list(&user(MEMBER.0, MEMBER.1), &page(200, Some(forged)))
        .await
        .unwrap_err();
    assert_eq!(
        refusal(&forged_denied),
        Reason::OperationNotPermittedForActor
    );
}

/// 08 §4.3: the Workflow service principal reads only its finite explicit order IDs; a service
/// decision without a finite ID path fails closed as integration; a service scope is never
/// provably confined, so it logs.
#[tokio::test]
async fn service_principals_read_only_their_finite_order_ids() {
    let r = R::new().await;
    let mut created = Vec::new();
    for tag in ["a", "b", "c"] {
        created.push(create(&r.t, &r.capture, tag).await);
    }
    // The Workflow service principal reads only its finite explicit order IDs (08 §4.3); a
    // service decision without a finite ID path fails closed as integration.
    let visible = created[0];
    let workflow = service(WORKFLOW.0, WORKFLOW.1);
    let rules = r.reads_with(RulesProvider::from_policy(policy(&[visible, created[1]])));
    r.clear_logs().await;
    let wf_page = rules.list(&workflow, &list_all()).await.unwrap();
    assert_eq!(
        wf_page
            .orders
            .iter()
            .map(|o| o.order.order_id)
            .collect::<Vec<_>>(),
        vec![visible, created[1]]
    );
    let logs = r.logs().await;
    assert_eq!(logs.len(), 1, "a service scope is never provably confined");
    assert_eq!(logs[0]["actor_class"], json!("service"));
    assert_eq!(
        refusal(&rules.get(&workflow, created[2]).await.unwrap_err()),
        Reason::OrderNotFound
    );
    let tenant_only = r.reads_with(ScriptedPdp::new(|_| {
        Some(allow(vec![path(vec![eq("seller_tenant_id", u(20))])]))
    }));
    assert!(matches!(
        tenant_only.list(&workflow, &list_all()).await.unwrap_err(),
        OrdersError::Integration
    ));
    let finite = r.reads_with(ScriptedPdp::new(move |_| {
        Some(allow(vec![path(vec![
            is_in("id", &[visible]),
            eq("seller_tenant_id", u(20)),
        ])]))
    }));
    let scoped = finite.list(&workflow, &list_all()).await.unwrap();
    assert_eq!(
        scoped
            .orders
            .iter()
            .map(|o| o.order.order_id)
            .collect::<Vec<_>>(),
        vec![visible]
    );
    let detail = finite.get(&workflow, visible).await.unwrap();
    assert_eq!(detail.order.order_id, visible);
}

/// §4.4 failure semantics on real privileges: a served log that cannot commit discloses
/// nothing (`read-store-unavailable`), an own-tenant read that needs no log still serves, and a
/// refused log that cannot commit preserves the refusal. Changed authorization facts between
/// the decision and the snapshot restart authorization and never disclose the prefetched row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn log_failures_and_changed_facts_follow_the_disclosure_rules() {
    let r = R::new().await;
    let (id, _) = r.draft("f", 1).await;
    let (signals, reads) = r.reads_recording();
    r.t.env
        .pg
        .sql("REVOKE INSERT ON bss_orders__read_access_log FROM bss_orders_runtime")
        .await
        .unwrap();
    let seller = user(SELLER.0, SELLER.1);
    let blocked = reads.get(&seller, id).await.unwrap_err();
    assert_eq!(refusal(&blocked), Reason::ReadStoreUnavailable);
    let blocked_lines = reads
        .list_lines(&seller, id, &LineList::default())
        .await
        .unwrap_err();
    assert_eq!(refusal(&blocked_lines), Reason::ReadStoreUnavailable);
    let blocked_list = reads.list(&seller, &list_all()).await.unwrap_err();
    assert_eq!(refusal(&blocked_list), Reason::ReadStoreUnavailable);
    assert!(reads.get(&buyer(), id).await.is_ok(), "no log needed");
    let still_refused = reads.get(&user(MEMBER.0, MEMBER.1), id).await.unwrap_err();
    assert_eq!(refusal(&still_refused), Reason::OrderNotFound);
    let still_refused_list = reads
        .list(&user(MEMBER.0, MEMBER.1), &list_all())
        .await
        .unwrap_err();
    assert_eq!(
        refusal(&still_refused_list),
        Reason::OperationNotPermittedForActor
    );
    assert_eq!(r.log_count().await, 0);
    // §3.8 / §4.4: every failed required write raised the write-failure signal with its
    // outcome, and each refusal raised the refusal signal; the own-tenant read raised nothing.
    assert_eq!(
        signals.log_failures(),
        vec![
            AccessOutcome::Served,
            AccessOutcome::Served,
            AccessOutcome::Served,
            AccessOutcome::Refused,
            AccessOutcome::Refused
        ]
    );
    assert_eq!(
        signals.refusals(),
        vec![
            (Reason::OrderNotFound, None),
            (Reason::OperationNotPermittedForActor, None)
        ]
    );
    r.t.env
        .pg
        .sql("GRANT INSERT ON bss_orders__read_access_log TO bss_orders_runtime")
        .await
        .unwrap();
    assert!(reads.get(&seller, id).await.is_ok());
    assert_eq!(r.log_count().await, 1);
    assert_eq!(
        signals.log_failures().len(),
        5,
        "a committed row raises nothing"
    );
    // Changed facts: the PDP's first decision runs while the payer axis is rewritten under it;
    // the scoped snapshot sees different axes, authorization restarts on the current facts,
    // and the view carries the committed payer, never the prefetched one.
    let raw = r.t.env.pg.raw.clone();
    let calls = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&calls);
    let racing = ScriptedPdp::new(move |request| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            let sql = format!(
                "UPDATE bss_orders__order SET payer_tenant_id='{}' WHERE order_id='{}'",
                u(31),
                request.resource.id.unwrap()
            );
            let raw = raw.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(raw.execute_unprepared(&sql))
                    .unwrap();
            });
        }
        Some(allow(vec![path(vec![eq("resource_tenant_id", u(10))])]))
    });
    let service = r.reads_with(racing);
    let view = service.get(&buyer(), id).await.unwrap();
    assert_eq!(view.order.payer_tenant_id, u(31));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one restart after the changed facts"
    );
}

/// The delivered authoring and read routes mounted behind the buyer's authenticated context.
fn mounted_router(r: &R) -> axum::Router {
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    crate::api::rest::capture::router(Arc::clone(&r.capture), super::capture::limiter(), &openapi)
        .merge(crate::api::rest::read::router(
            Arc::clone(&r.reads),
            &openapi,
        ))
        .layer(axum::Extension(buyer().ctx().clone()))
}

/// The mounted read routes on the wire (in-process router over real PostgreSQL, not the S2-12
/// live E2E): strong `ETag`, `snake_case` bodies, query validation precedence with problem+json
/// refusals and no access-log rows, the masked 404, the proof header carrier and page walking.
#[tokio::test]
async fn the_mounted_read_routes_carry_etag_pages_and_problems_on_the_wire() {
    let r = R::new().await;
    let router = mounted_router(&r);
    let problem = |h: &axum::http::HeaderMap| h["content-type"] == "application/problem+json";
    let (id, lines) = r.draft("w", 2).await;
    let order = format!("/bss-orders-lifecycle/v1/orders/{id}");
    let detail = http(&router, "GET", &order, &[], None).await;
    assert_eq!(detail.0, 200, "{:?}", detail.2);
    assert_eq!(detail.1["etag"], "\"1\"");
    assert_eq!(detail.2["order"]["order_id"], json!(s(id)));
    assert_eq!(detail.2["draft_revision"], json!(4));
    assert_eq!(detail.2["version"]["version"], json!(1));
    assert_eq!(detail.2["lines"].as_array().unwrap().len(), 2);
    assert_eq!(detail.2["lines"][0]["line_id"], json!(s(lines[0])));
    assert_eq!(
        detail.2["lines"][0]["term_duration"]["kind"],
        json!("periods")
    );
    assert!(detail.2.get("resolved_total").is_none());
    assert!(detail.2.get("draftRevision").is_none());
    // Line page: ETag, snake_case, page walk through next_cursor.
    let first = http(
        &router,
        "GET",
        &format!("{order}/lines?page_size=1"),
        &[],
        None,
    )
    .await;
    assert_eq!(first.0, 200, "{:?}", first.2);
    assert_eq!(first.1["etag"], "\"1\"");
    assert_eq!(first.2["current_version"], json!(1));
    assert_eq!(first.2["draft_revision"], json!(4));
    assert_eq!(first.2["lines"][0]["line_id"], json!(s(lines[0])));
    let token = first.2["next_cursor"].as_str().unwrap().to_owned();
    let second = http(
        &router,
        "GET",
        &format!("{order}/lines?page_size=1&cursor={token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(second.0, 200);
    assert_eq!(second.2["lines"][0]["line_id"], json!(s(lines[1])));
    assert!(second.2.get("next_cursor").is_none());
    // The order list with filters and a page walk.
    let list = http(
        &router,
        "GET",
        "/bss-orders-lifecycle/v1/orders?state=draft&page_size=1",
        &[],
        None,
    )
    .await;
    assert_eq!(list.0, 200, "{:?}", list.2);
    assert_eq!(list.2["orders"][0]["order"]["order_id"], json!(s(id)));
    assert_eq!(list.2["orders"][0]["current_version"], json!(1));
    assert!(list.1.get("etag").is_none(), "the list carries no ETag");
    assert!(list.2.get("next_cursor").is_none());
    r.clear_logs().await;
    // Input validation precedence, each a problem+json refusal with no access-log row.
    let expect = |path: &str, status: u16, code: &str| {
        let router = router.clone();
        let path = path.to_owned();
        let code = code.to_owned();
        async move {
            let out = http(&router, "GET", &path, &[], None).await;
            assert_eq!(out.0, status, "{path}: {:?}", out.2);
            assert!(problem(&out.1), "{path}");
            assert_eq!(out.2["error_code"], json!(code), "{path}");
            assert_eq!(out.2["error_domain"], json!("orders-lifecycle.v1"));
        }
    };
    for bad in ["0", "201", "abc", "-1", "+5", "1.5", ""] {
        expect(
            &format!("/bss-orders-lifecycle/v1/orders?page_size={bad}"),
            400,
            "PAGE_SIZE_EXCEEDED",
        )
        .await;
    }
    expect(
        "/bss-orders-lifecycle/v1/orders?page_size=1&page_size=2",
        400,
        "PAGE_SIZE_EXCEEDED",
    )
    .await;
    // Page size is checked before filters, filters before the cursor.
    expect(
        "/bss-orders-lifecycle/v1/orders?page_size=500&bogus=1&cursor=x",
        400,
        "PAGE_SIZE_EXCEEDED",
    )
    .await;
    for bad in [
        "bogus=1",
        "state=bogus",
        "state=draft&state=draft",
        "created_from=yesterday",
        "created_to=2026-13-01T00:00:00Z",
        "state_entered_before=2026-01-01",
        "contract_id=not-a-uuid",
        "sort=created_at",
    ] {
        expect(
            &format!("/bss-orders-lifecycle/v1/orders?{bad}&cursor=x"),
            400,
            "FILTER_INVALID",
        )
        .await;
    }
    for bad in ["x", "a%20b", "AAAA", &token] {
        expect(
            &format!("/bss-orders-lifecycle/v1/orders?cursor={bad}"),
            400,
            "CURSOR_INVALID",
        )
        .await;
    }
    expect(
        "/bss-orders-lifecycle/v1/orders?cursor=a&cursor=b",
        400,
        "CURSOR_INVALID",
    )
    .await;
    expect(&format!("{order}/lines?state=draft"), 400, "FILTER_INVALID").await;
    expect(
        &format!("{order}/lines?page_size=0"),
        400,
        "PAGE_SIZE_EXCEEDED",
    )
    .await;
    expect(&format!("{order}/lines?cursor=zzz"), 400, "CURSOR_INVALID").await;
    assert_eq!(r.log_count().await, 0, "input validation appends no row");
    // Missing and hidden targets: 404 ORDER_NOT_FOUND, logged refused.
    expect(
        &format!("/bss-orders-lifecycle/v1/orders/{}", u(0xbeef)),
        404,
        "ORDER_NOT_FOUND",
    )
    .await;
    expect(
        &format!("/bss-orders-lifecycle/v1/orders/{}/lines", u(0xbeef)),
        404,
        "ORDER_NOT_FOUND",
    )
    .await;
    assert_eq!(r.log_count().await, 2);
    // A malformed proof header is request-invalid before any decision; a valid one logs.
    let malformed = http(
        &router,
        "GET",
        &order,
        &[("x-delegation-proof-ref", "a b")],
        None,
    )
    .await;
    assert_eq!(malformed.0, 400);
    assert_eq!(malformed.2["error_code"], json!("REQUEST_INVALID"));
    let with_header = http(
        &router,
        "GET",
        &order,
        &[("x-delegation-proof-ref", "ref-9")],
        None,
    )
    .await;
    assert_eq!(with_header.0, 200);
    assert!(
        !with_header.2.to_string().contains("ref-9"),
        "the proof reference is never echoed"
    );
    assert_eq!(r.log_count().await, 3);
    assert_eq!(r.logs().await[2]["delegation_proof_ref"], json!("ref-9"));
    // The documentation mirrors accept the real wire exactly.
    let mirrored: crate::api::rest::dto::OrdersOrderView =
        serde_json::from_value(detail.2.clone()).unwrap();
    assert_eq!(serde_json::to_value(mirrored).unwrap(), detail.2);
    let mirrored: crate::api::rest::dto::OrdersLinePage =
        serde_json::from_value(first.2.clone()).unwrap();
    assert_eq!(serde_json::to_value(mirrored).unwrap(), first.2);
    let mirrored: crate::api::rest::dto::OrdersOrderPage =
        serde_json::from_value(list.2.clone()).unwrap();
    assert_eq!(serde_json::to_value(mirrored).unwrap(), list.2);
    // The ETag a read returns is what a write sends back as If-Match.
    let edit = http(
        &router,
        "PATCH",
        &order,
        &[
            ("content-type", "application/json"),
            ("if-match", detail.1["etag"].to_str().unwrap()),
            ("idempotency-key", "w-edit"),
        ],
        Some(json!({"fields": {"contract_id": s(u(0xc1))}, "expected_draft_revision": 4})),
    )
    .await;
    assert_eq!(edit.0, 200, "{:?}", edit.2);
    let again = http(&router, "GET", &order, &[], None).await;
    assert_eq!(again.2["draft_revision"], json!(5));
    assert_eq!(again.2["order"]["contract_id"], json!(s(u(0xc1))));
}

/// The SDK read entry points run the same wrapper as REST: the list and line pages agree with
/// the service, and a non-draft order fails closed as unavailable (later-package composition)
/// after authorization without disclosing committed content.
#[tokio::test]
async fn sdk_reads_match_the_service_and_undelivered_compositions_fail_closed() {
    let r = R::new().await;
    let (id, lines) = r.draft("s", 2).await;
    let sdk = r.sdk();
    let page = sdk
        .list(buyer().ctx(), list_all(), meta(None))
        .await
        .unwrap();
    assert_eq!(page.orders.len(), 1);
    assert_eq!(page.orders[0].order.order_id, id);
    let line_page = sdk
        .list_lines(buyer().ctx(), id, LineList::default(), meta(None))
        .await
        .unwrap();
    assert_eq!(
        line_page
            .lines
            .iter()
            .map(|l| l.line_id)
            .collect::<Vec<_>>(),
        lines
    );
    // Move the fixture out of draft directly (no business operation can yet): the point and
    // line reads refuse unavailable, the list still summarizes it without a draft revision.
    r.t.env
        .pg
        .sql(&format!(
            "UPDATE bss_orders__order SET state='submitted' WHERE order_id='{id}'"
        ))
        .await
        .unwrap();
    // The buyer's list scope spans two resource tenants, so each list logs; the undelivered
    // compositions disclose nothing and therefore append nothing.
    let before = r.log_count().await;
    assert!(matches!(
        sdk.get(buyer().ctx(), id, meta(None)).await.unwrap_err(),
        OrdersError::Unavailable
    ));
    assert_eq!(r.log_count().await, before);
    let summaries = sdk
        .list(buyer().ctx(), list_all(), meta(None))
        .await
        .unwrap();
    assert_eq!(summaries.orders[0].order.state, OrderState::Submitted);
    assert_eq!(summaries.orders[0].draft_revision, None);
    let before = r.log_count().await;
    assert!(matches!(
        sdk.list_lines(buyer().ctx(), id, LineList::default(), meta(None))
            .await
            .unwrap_err(),
        OrdersError::Unavailable
    ));
    assert_eq!(
        r.log_count().await,
        before,
        "nothing disclosed, nothing logged"
    );
}

/// 08 §3.6 *Prefetch and authorized snapshot* and *Read One Order* step 3: a row that leaves
/// the decided scope between the decision and the snapshot (here the resource tenant is
/// reassigned under the first decision) is neither disclosed from the prefetch nor refused
/// silently. Authorization restarts on the current facts, the fresh decision denies, and the
/// refusal appends its row exactly like any other hidden-target refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_row_that_leaves_the_decided_scope_restarts_and_logs_its_refusal() {
    let r = R::new().await;
    let (id, _) = r.draft("v", 1).await;
    let raw = r.t.env.pg.raw.clone();
    let calls = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&calls);
    let racing = ScriptedPdp::new(move |request| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            let sql = format!(
                "UPDATE bss_orders__order SET resource_tenant_id='{}' WHERE order_id='{}'",
                u(12),
                request.resource.id.unwrap()
            );
            let raw = raw.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(raw.execute_unprepared(&sql))
                    .unwrap();
            });
        }
        // The caller's only path is resource tenant 10: it admits the prefetched row but not
        // the row as it stands after the move, so the scoped re-read sees nothing.
        Some(allow(vec![path(vec![eq("resource_tenant_id", u(10))])]))
    });
    let (signals, _) = r.reads_recording();
    let service =
        r.t.reads_with_signals(racing, Arc::clone(&signals) as Arc<dyn ReadSignals>);
    r.clear_logs().await;
    let denied = service.get(&buyer(), id).await.unwrap_err();
    assert_eq!(refusal(&denied), Reason::OrderNotFound);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one restart: the second decision is made on the moved row"
    );
    let logs = r.logs().await;
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0]["outcome"], json!("refused"));
    assert_eq!(logs[0]["refusal_reason"], json!("order-not-found"));
    assert_eq!(logs[0]["operation"], json!("get"));
    assert_eq!(logs[0]["order_id"], json!(s(id)));
    assert_eq!(logs[0]["requested_order_ref"], json!(s(id)));
    assert_eq!(logs[0]["internal_refusal_detail"], Value::Null);
    assert_eq!(signals.refusals(), vec![(Reason::OrderNotFound, None)]);
    assert!(signals.log_failures().is_empty());
    // The real provider agrees on the moved row: the buyer has no path to tenant 12, and the
    // line page takes the same arm.
    assert_eq!(
        refusal(&r.reads.get(&buyer(), id).await.unwrap_err()),
        Reason::OrderNotFound
    );
    assert_eq!(
        refusal(
            &r.reads
                .list_lines(&buyer(), id, &LineList::default())
                .await
                .unwrap_err()
        ),
        Reason::OrderNotFound
    );
    assert_eq!(r.log_count().await, 3);
}

/// D-210 item 7 on the wire: an order outside draft carries committed-version content that
/// later packages compose, so the point and line reads answer the documented canonical 503
/// problem after authorization — no order or line member, no `ETag`, no access-log row — while
/// the list still summarizes the order without a draft revision.
#[tokio::test]
async fn undelivered_compositions_answer_the_documented_unavailable_problem_on_the_wire() {
    let r = R::new().await;
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let router = crate::api::rest::read::router(Arc::clone(&r.reads), &openapi)
        .layer(axum::Extension(buyer().ctx().clone()));
    let (id, _) = r.draft("u", 1).await;
    r.t.env
        .pg
        .sql(&format!(
            "UPDATE bss_orders__order SET state='submitted' WHERE order_id='{id}'"
        ))
        .await
        .unwrap();
    r.clear_logs().await;
    let order = format!("/bss-orders-lifecycle/v1/orders/{id}");
    for path in [order.clone(), format!("{order}/lines")] {
        let out = http(&router, "GET", &path, &[], None).await;
        assert_eq!(out.0, 503, "{path}: {:?}", out.2);
        assert_eq!(out.1["content-type"], "application/problem+json", "{path}");
        assert_eq!(out.2["status"], json!(503), "{path}");
        assert!(
            out.1.get("etag").is_none(),
            "{path}: no version is disclosed"
        );
        for member in [
            "order",
            "version",
            "lines",
            "draft_revision",
            "current_version",
        ] {
            assert!(out.2.get(member).is_none(), "{path}: {member} disclosed");
        }
    }
    assert_eq!(r.log_count().await, 0, "nothing disclosed, nothing logged");
    let list = http(&router, "GET", "/bss-orders-lifecycle/v1/orders", &[], None).await;
    assert_eq!(list.0, 200, "{:?}", list.2);
    assert_eq!(list.2["orders"][0]["order"]["order_id"], json!(s(id)));
    assert_eq!(list.2["orders"][0]["order"]["state"], json!("submitted"));
    assert_eq!(list.2["orders"][0]["current_version"], json!(1));
    assert!(list.2["orders"][0].get("draft_revision").is_none());
}
