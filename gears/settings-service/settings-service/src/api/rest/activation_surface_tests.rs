// @cpt-dod:cpt-cf-settings-service-dod-activation-read-tests:p1
//! The activation read and the reports the history attaches, driven as a client
//! drives them: a request through the router the gear registers.
//!
//! DESIGN-activation §7 Level 3 rows — *Activation read*, *Activation read —
//! out of subtree or hidden key*, *`AuthZ` deny — the action*, *History shows
//! reports under the change*, *History attaches descendant reports*, *History
//! at the written tenant*, *RFC 9457 errors* — and the rules of the feature
//! where no row exists. Log-capture assertions assume one process per test
//! (nextest, as the gear's `make` targets run).

use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_db::secure::secure_insert;
use toolkit_db::test_support::{QueryKind, QueryRecorder};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::audit::resource_id;
use crate::domain::access::TenantAccess;
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::recorder::Transition;
use crate::domain::activation::{ActivationRepository, ActivationStatus, tenant_text};
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::entity::audit_record::{self, Entity as AuditEntity};
use crate::test_support::{RestHarness, SECRET};

// ── Seeding ──────────────────────────────────────────────────────────────────

fn encoded(h: &RestHarness, name: &str) -> String {
    h.inner.key(name).to_string().replace('~', "%7E")
}

fn read_uri(h: &RestHarness, name: &str, query: &str) -> String {
    format!(
        "/settings-service/v1/settings/{}/activation{query}",
        encoded(h, name)
    )
}

fn history_uri(h: &RestHarness, name: &str, query: &str) -> String {
    format!(
        "/settings-service/v1/settings/{}/history{query}",
        encoded(h, name)
    )
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// How a seeded state was settled: the status, the applied value, the detail.
type Settled<'a> = (ActivationStatus, Option<&'a str>, Option<&'a str>);

/// A subscription of `subscriber` to the key at `tenant` with a state for
/// `change_ref`, settled when `settled` says so.
#[allow(clippy::too_many_arguments)]
async fn seed_state(
    h: &RestHarness,
    subscriber: &str,
    name: &str,
    tenant: Uuid,
    change_ref: Uuid,
    expected: &str,
    settled: Option<Settled<'_>>,
) {
    let conn = h.inner.db.conn().expect("connection");
    let key = h.inner.key(name).to_string();
    let tenant = tenant_text(tenant);
    let subscription = ActivationRepo
        .register(&conn, subscriber, &key, &tenant)
        .await
        .expect("registered");
    ActivationRepo
        .write_states(
            &conn,
            &StateWrite {
                key: key.clone(),
                change_ref,
                rows: vec![StateRow {
                    subscription_id: subscription.id,
                    tenant: tenant.clone(),
                    expected_value: expected.to_owned(),
                }],
            },
        )
        .await
        .expect("state written");
    if let Some((status, applied, detail)) = settled {
        ActivationRepo
            .report(
                &conn,
                &Transition {
                    subscriber: subscriber.to_owned(),
                    key,
                    tenant,
                    change_ref,
                    status,
                    applied_value: applied.map(str::to_owned),
                    detail: detail.map(str::to_owned),
                },
            )
            .await
            .expect("settled");
    }
}

/// One audit record as the table holds it, with the clock the test chooses.
struct Row<'a> {
    id: Uuid,
    name: &'a str,
    tenant: Option<Uuid>,
    operation: &'a str,
    actor: &'a str,
    classification: &'a str,
    outcome: &'a str,
    change_ref: Option<Uuid>,
    detail: Option<&'a str>,
    at: i64,
}

impl<'a> Row<'a> {
    /// A `change` record at `tenant`, `at` seconds into the test's epoch.
    fn change(id: Uuid, name: &'a str, tenant: Uuid, at: i64) -> Self {
        Self {
            id,
            name,
            tenant: Some(tenant),
            operation: "change",
            actor: "admin@acme",
            classification: "pii",
            outcome: "success",
            change_ref: None,
            detail: None,
            at,
        }
    }

    /// A subscriber's report on `change_ref` at `tenant`.
    fn report(id: Uuid, name: &'a str, tenant: Uuid, change_ref: Uuid, at: i64) -> Self {
        Self {
            id,
            name,
            tenant: Some(tenant),
            operation: "activation",
            actor: "gear-consumer",
            classification: "public",
            outcome: "success",
            change_ref: Some(change_ref),
            detail: None,
            at,
        }
    }

    fn failed(mut self, detail: &'a str) -> Self {
        self.outcome = "failure";
        self.detail = Some(detail);
        self
    }

    fn by(mut self, actor: &'a str) -> Self {
        self.actor = actor;
        self
    }
}

async fn seed_audit(h: &RestHarness, row: Row<'_>) {
    use sea_orm::ActiveValue::Set;
    let conn = h.inner.db.conn().expect("connection");
    let key = h.inner.key(row.name).to_string();
    let active = audit_record::ActiveModel {
        id: Set(row.id),
        resource: Set(resource_id::format_raw(&key, row.tenant)),
        declaration_key: Set(key),
        tenant_id: Set(row.tenant),
        operation: Set(row.operation.to_owned()),
        actor: Set(row.actor.to_owned()),
        actor_classification: Set(row.classification.to_owned()),
        pre_value: Set(None),
        post_value: Set(None),
        outcome: Set(row.outcome.to_owned()),
        request_id: Set("r".to_owned()),
        change_set_id: Set(None),
        change_ref: Set(row.change_ref),
        detail: Set(row.detail.map(str::to_owned)),
        occurred_at: Set(
            OffsetDateTime::from_unix_timestamp(1_800_000_000 + row.at).expect("a timestamp")
        ),
        retain_until: Set(None),
    };
    secure_insert::<AuditEntity>(active, &AccessScope::allow_all(), &conn)
        .await
        .expect("audit record");
}

/// What a problem document says, minus what differs per request.
fn problem(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.remove("trace_id");
        object.remove("instance");
    }
    body
}

fn subscribers(body: &Value) -> Vec<&str> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| item["subscriber"].as_str())
        .collect()
}

// ── The activation read ──────────────────────────────────────────────────────

#[tokio::test]
async fn the_read_shows_one_item_per_subscriber_in_order_and_no_value() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    // Seeded out of order: the answer is by subscriber.
    seed_state(
        &h,
        "gear-c",
        "proxy",
        a,
        id(3),
        "EXPECTED-C",
        Some((
            ActivationStatus::Failed,
            Some("APPLIED-C"),
            Some("pool full"),
        )),
    )
    .await;
    seed_state(&h, "gear-a", "proxy", a, id(1), "EXPECTED-A", None).await;
    seed_state(
        &h,
        "gear-b",
        "proxy",
        a,
        id(2),
        "EXPECTED-B",
        Some((ActivationStatus::Succeeded, Some("EXPECTED-B"), None)),
    )
    .await;

    let answer = h
        .send("GET", &read_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(subscribers(&answer.body), ["gear-a", "gear-b", "gear-c"]);
    let items = answer.body["items"].as_array().expect("items");
    assert_eq!(items[0]["status"], json!("awaiting"));
    assert_eq!(items[1]["status"], json!("succeeded"));
    assert_eq!(items[2]["status"], json!("failed"));
    for (item, change) in items.iter().zip([id(1), id(2), id(3)]) {
        assert_eq!(item["change_ref"], json!(change.to_string()));
    }
    assert!(items[0].get("reported_at").is_none(), "{}", items[0]);
    assert!(items[1]["reported_at"].is_string());
    assert!(items[2]["reported_at"].is_string());
    assert!(items[0].get("detail").is_none() && items[1].get("detail").is_none());
    assert_eq!(items[2]["detail"], json!("pool full"));
    assert!(answer.body.get("page_info").is_none(), "{}", answer.raw);
    assert!(answer.body.get("next_cursor").is_none());
    for value in ["EXPECTED", "APPLIED", "expected_value", "applied_value"] {
        assert!(
            !answer.raw.contains(value),
            "a value in the read: {}",
            answer.raw
        );
    }
}

#[tokio::test]
async fn without_tenant_the_read_answers_for_the_callers_own_and_returns_it_normalised() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    seed_state(&h, "gear-a", "proxy", a, id(1), "x", None).await;
    seed_state(&h, "gear-b", "proxy", b, id(2), "x", None).await;

    let own = h
        .send("GET", &read_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(own.status, 200, "{}", own.raw);
    assert_eq!(own.body["tenant"], json!(a.to_string()));
    assert_eq!(subscribers(&own.body), ["gear-a"]);

    // Upper case in, bare lowercase out; and the descendant's own states.
    let query = format!("?tenant={}", b.to_string().to_uppercase());
    let below = h
        .send("GET", &read_uri(&h, "proxy", &query), None, None, a)
        .await;
    assert_eq!(below.status, 200, "{}", below.raw);
    assert_eq!(below.body["tenant"], json!(b.to_string()));
    assert_eq!(subscribers(&below.body), ["gear-b"]);
}

#[tokio::test]
async fn a_key_nobody_subscribed_to_answers_ok_with_no_items() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let answer = h
        .send(
            "GET",
            &read_uri(&h, "proxy", ""),
            None,
            None,
            h.inner.tree.a,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(answer.body["items"], json!([]));
    assert!(answer.body.get("page_info").is_none());
}

#[tokio::test]
async fn a_retired_declarations_activation_is_readable() {
    let h = RestHarness::new().await;
    let declaration = h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    seed_state(&h, "gear-a", "proxy", a, id(1), "x", None).await;
    h.inner.retire(declaration).await;
    let answer = h
        .send("GET", &read_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(subscribers(&answer.body), ["gear-a"]);
}

#[tokio::test]
async fn a_secret_traits_states_read_with_no_hash_and_one_statement_on_activation_state() {
    let (h, recorder) = RestHarness::recorded().await;
    h.inner
        .declare_typed("token", "cascading", json!("x"), SECRET, "secret")
        .await;
    let a = h.inner.tree.a;
    let (expected, applied) = ("e".repeat(64), "f".repeat(64));
    seed_state(
        &h,
        "gear-a",
        "token",
        a,
        id(1),
        &expected,
        Some((ActivationStatus::Succeeded, Some(&expected), None)),
    )
    .await;
    seed_state(
        &h,
        "gear-b",
        "token",
        a,
        id(1),
        &expected,
        Some((ActivationStatus::Failed, Some(&applied), Some("wrong"))),
    )
    .await;
    recorder.clear();

    let answer = h
        .send("GET", &read_uri(&h, "token", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(answer.body["items"].as_array().map(Vec::len), Some(2));
    assert!(
        !answer.raw.contains(&expected) && !answer.raw.contains(&applied),
        "a hash in the read: {}",
        answer.raw
    );
    assert_eq!(selects_of(&recorder, "activation_state"), 1);
    assert_eq!(writes(&recorder), 0);
}

fn selects_of(recorder: &QueryRecorder, table: &str) -> usize {
    recorder
        .events()
        .iter()
        .filter(|e| e.kind == QueryKind::Select && e.table.as_deref() == Some(table))
        .count()
}

fn writes(recorder: &QueryRecorder) -> usize {
    recorder
        .events()
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                QueryKind::Insert | QueryKind::Update | QueryKind::Delete
            )
        })
        .count()
}

// ── Refusals ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn what_the_read_does_not_take_is_refused_before_any_decision() {
    // A caller the policy denies would be `403`; the `400` comes first.
    let h = RestHarness::denying().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let root = h.inner.tree.root;
    for query in [
        "?tenant=not-a-uuid",
        "?$filter=subscriber%20eq%20%27x%27",
        "?$orderby=subscriber",
        "?$select=subscriber",
        "?cursor=abc",
        "?limit=5",
        "?anything=else",
        &format!("?tenant={root}&tenant={root}"),
    ] {
        let answer = h
            .send("GET", &read_uri(&h, "proxy", query), None, None, root)
            .await;
        assert_eq!(answer.status, 400, "`{query}`: {}", answer.raw);
        assert!(answer.body["title"].is_string(), "{}", answer.raw);
    }
    let malformed = h
        .send(
            "GET",
            "/settings-service/v1/settings/not-a-key/activation",
            None,
            None,
            root,
        )
        .await;
    assert_eq!(malformed.status, 400, "{}", malformed.raw);
}

#[tokio::test]
async fn a_denied_caller_is_refused_the_same_for_every_key_and_tenant() {
    let h = RestHarness::denying().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let outside = format!("?tenant={}", h.inner.tree.c);
    let mut bodies = Vec::new();
    for uri in [
        read_uri(&h, "proxy", ""),
        read_uri(&h, "undeclared", ""),
        read_uri(&h, "proxy", &outside),
    ] {
        let answer = h.send("GET", &uri, None, None, a).await;
        assert_eq!(answer.status, 403, "{uri}: {}", answer.raw);
        assert!(answer.body.get("items").is_none());
        bodies.push(problem(answer.body));
    }
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "{bodies:?}"
    );
}

#[tokio::test]
async fn a_denied_action_other_than_read_does_not_refuse_the_read() {
    let h = RestHarness::denying_action("write").await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let answer = h
        .send(
            "GET",
            &read_uri(&h, "proxy", ""),
            None,
            None,
            h.inner.tree.a,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
}

/// A tenant below the standalone `s`, for the case where the standalone tenant
/// is not the one named.
fn below_standalone(h: &RestHarness) -> Uuid {
    let below = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(below, h.inner.tree.s);
    below
}

#[tokio::test]
async fn the_four_cases_an_authorized_caller_cannot_read_answer_one_not_found() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let walled = h.inner.declare("walled", "cascading", json!(true)).await;
    let hidden = h.inner.declare("hidden", "cascading", json!(true)).await;
    h.inner.bind_domain(walled, "commercial").await;
    h.restrict(hidden, h.inner.tree.a, TenantAccess::Hidden)
        .await;
    let a = h.inner.tree.a;
    let below = below_standalone(&h);
    // States exist everywhere, so absence is the read's decision, not the data's.
    for tenant in [h.inner.tree.c, h.inner.tree.s, below, a] {
        for key in ["proxy", "walled", "hidden"] {
            seed_state(&h, "gear-a", key, tenant, id(1), "x", None).await;
        }
    }

    let mut absent = Vec::new();
    for (name, query) in [
        ("proxy", format!("?tenant={}", h.inner.tree.c)),
        ("proxy", format!("?tenant={}", h.inner.tree.s)),
        ("proxy", format!("?tenant={below}")),
        ("proxy", format!("?tenant={}", Uuid::new_v4())),
        ("undeclared", String::new()),
        ("hidden", String::new()),
    ] {
        let answer = h
            .send("GET", &read_uri(&h, name, &query), None, None, a)
            .await;
        assert_eq!(answer.status, 404, "{name}{query}: {}", answer.raw);
        absent.push(problem(answer.body));
    }
    assert!(
        absent.windows(2).all(|pair| pair[0] == pair[1]),
        "{absent:?}"
    );
}

#[tokio::test]
async fn a_resolver_that_cannot_answer_fails_the_read_unavailable_never_not_found() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    // At the subtree test, a tenant outside the subtree: the failure is
    // decided before the `404` it would have been.
    h.inner.hierarchy.set_membership_unavailable(true);
    let at_subtree = h
        .send(
            "GET",
            &read_uri(&h, "proxy", &format!("?tenant={}", h.inner.tree.c)),
            None,
            None,
            a,
        )
        .await;
    assert_eq!(at_subtree.status, 503, "{}", at_subtree.raw);
    h.inner.hierarchy.set_membership_unavailable(false);
    h.inner.hierarchy.set_unavailable(true);
    // Inside the effective-access evaluation, for the caller's own tenant.
    let in_evaluation = h
        .send("GET", &read_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(in_evaluation.status, 503, "{}", in_evaluation.raw);
    assert!(in_evaluation.body["title"].is_string());
}

#[tokio::test]
async fn a_method_other_than_get_is_refused_without_reaching_the_read() {
    let (h, recorder) = RestHarness::recorded().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    recorder.clear();
    for method in ["PUT", "POST", "PATCH", "DELETE"] {
        let answer = h
            .send(
                method,
                &read_uri(&h, "proxy", ""),
                Some(json!({})),
                None,
                h.inner.tree.a,
            )
            .await;
        assert!(
            answer.status == 405 || answer.status == 404,
            "{method}: {}",
            answer.raw
        );
    }
    assert_eq!(selects_of(&recorder, "activation_state"), 0);
    assert_eq!(writes(&recorder), 0);
}

#[tokio::test]
async fn no_detail_or_value_reaches_a_log_line_of_either_read() {
    use crate::domain::activation::testing::{capture, logged};
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    seed_state(
        &h,
        "gear-a",
        "proxy",
        a,
        id(1),
        "EXPECTED-VALUE",
        Some((
            ActivationStatus::Failed,
            Some("APPLIED-VALUE"),
            Some("DETAIL-TEXT"),
        )),
    )
    .await;
    seed_audit(&h, Row::change(id(1), "proxy", a, 1)).await;
    seed_audit(
        &h,
        Row::report(id(2), "proxy", a, id(1), 2).failed("DETAIL-TEXT"),
    )
    .await;
    let (buf, _guard) = capture();
    let read = h
        .send("GET", &read_uri(&h, "proxy", ""), None, None, a)
        .await;
    let history = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!((read.status.as_u16(), history.status.as_u16()), (200, 200));
    let log = logged(&buf);
    for text in ["EXPECTED-VALUE", "APPLIED-VALUE", "DETAIL-TEXT"] {
        assert!(!log.contains(text), "`{text}` in the log: {log}");
    }
}

// ── The history ──────────────────────────────────────────────────────────────

/// A change at `a` (id 100), with a report at `a` (101) and one at its
/// descendant `b` (102), the report at `b` the later.
async fn change_with_two_reports(h: &RestHarness) {
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    seed_audit(h, Row::change(id(100), "proxy", a, 10)).await;
    seed_audit(h, Row::report(id(101), "proxy", a, id(100), 11)).await;
    seed_audit(
        h,
        Row::report(id(102), "proxy", b, id(100), 12).failed("pool full"),
    )
    .await;
}

fn listed_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|r| r["id"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn the_history_shows_the_reports_under_the_change_and_never_beside_it() {
    let h = RestHarness::new().await;
    change_with_two_reports(&h).await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);

    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(listed_ids(&answer.body), [id(100).to_string()]);
    assert_eq!(answer.body["page_info"]["total_count"], json!(1));
    let attached = answer.body["items"][0]["activations"]
        .as_array()
        .expect("activations");
    // Newest first.
    assert_eq!(attached.len(), 2, "{}", answer.raw);
    assert_eq!(attached[0]["tenant_id"], json!(b.to_string()));
    assert_eq!(attached[0]["outcome"], json!("failure"));
    assert_eq!(attached[0]["detail"], json!("pool full"));
    assert_eq!(attached[0]["actor"], json!("gear-consumer"));
    assert_eq!(attached[0]["change_ref"], json!(id(100).to_string()));
    assert!(attached[0]["occurred_at"].is_string());
    assert_eq!(attached[1]["tenant_id"], json!(a.to_string()));
    assert_eq!(attached[1]["outcome"], json!("success"));
    assert!(attached[1].get("detail").is_none(), "{}", attached[1]);
    for item in attached {
        for forbidden in ["pre_value", "post_value", "request_id"] {
            assert!(item.get(forbidden).is_none(), "{item}");
        }
    }
}

#[tokio::test]
async fn a_sibling_administrator_sees_none_of_a_descendants_reports() {
    let h = RestHarness::new().await;
    change_with_two_reports(&h).await;
    let c = h.inner.tree.c;
    // The sibling reads the history at the written tenant's... it may not; it
    // reads its own, which holds none of the change.
    let own = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, c)
        .await;
    assert_eq!(own.status, 200, "{}", own.raw);
    assert_eq!(own.body["items"], json!([]));
    assert!(!own.raw.contains("pool full"));
    let upward = h
        .send(
            "GET",
            &history_uri(&h, "proxy", &format!("?tenant={}", h.inner.tree.a)),
            None,
            None,
            c,
        )
        .await;
    assert_eq!(upward.status, 403, "{}", upward.raw);

    // `b`'s administrator reads `a`'s history? No: a descendant may not read
    // above itself. At `b`, its own history lists the report as an ordinary
    // record, and the change's reports are nowhere else for it.
    let at_b = h
        .send(
            "GET",
            &history_uri(&h, "proxy", &format!("?tenant={}", h.inner.tree.b)),
            None,
            None,
            h.inner.tree.b,
        )
        .await;
    assert_eq!(at_b.status, 200, "{}", at_b.raw);
    assert_eq!(listed_ids(&at_b.body), [id(102).to_string()]);
}

#[tokio::test]
async fn a_descendants_report_is_attached_above_and_listed_in_its_own_history() {
    let h = RestHarness::new().await;
    change_with_two_reports(&h).await;
    let b = h.inner.tree.b;

    let in_b = h
        .send(
            "GET",
            &history_uri(&h, "proxy", &format!("?tenant={b}")),
            None,
            None,
            h.inner.tree.a,
        )
        .await;
    assert_eq!(in_b.status, 200, "{}", in_b.raw);
    // `b`'s history lists the report as an ordinary record, change_ref and
    // detail carried; its change lives at `a`, so nothing is attached to it.
    assert_eq!(listed_ids(&in_b.body), [id(102).to_string()]);
    let record = &in_b.body["items"][0];
    assert_eq!(record["operation"], json!("activation"));
    assert_eq!(record["change_ref"], json!(id(100).to_string()));
    assert_eq!(record["detail"], json!("pool full"));
    assert_eq!(record["activations"], json!([]));
    assert!(record.get("pre_value").is_none() && record.get("post_value").is_none());

    // And attached under the change in the history of `a`.
    let in_a = h
        .send(
            "GET",
            &history_uri(&h, "proxy", ""),
            None,
            None,
            h.inner.tree.a,
        )
        .await;
    assert_eq!(
        in_a.body["items"][0]["activations"][0]["tenant_id"],
        json!(b.to_string())
    );
}

#[tokio::test]
async fn a_standalone_descendants_report_is_attached_in_no_history_above_it() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (a, s) = (h.inner.tree.a, h.inner.tree.s);
    let below = below_standalone(&h);
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    seed_audit(&h, Row::report(id(101), "proxy", s, id(100), 11)).await;
    seed_audit(&h, Row::report(id(102), "proxy", below, id(100), 12)).await;
    seed_audit(&h, Row::report(id(103), "proxy", a, id(100), 13)).await;

    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    let attached = answer.body["items"][0]["activations"]
        .as_array()
        .expect("array");
    assert_eq!(attached.len(), 1, "only the report at `a`: {}", answer.raw);
    assert_eq!(attached[0]["tenant_id"], json!(a.to_string()));
}

#[tokio::test]
async fn a_resolver_failing_for_a_reports_tenant_fails_the_history_unavailable() {
    let h = RestHarness::new().await;
    change_with_two_reports(&h).await;
    // The chain of the report's tenant is the one lookup the history makes
    // beyond its own gates, and the only one that fails: the gates' chains
    // answer, the report tenant's does not.
    h.inner.hierarchy.fail_chain_for(h.inner.tree.b);
    let answer = h
        .send(
            "GET",
            &history_uri(&h, "proxy", ""),
            None,
            None,
            h.inner.tree.a,
        )
        .await;
    assert_eq!(answer.status, 503, "{}", answer.raw);
    assert!(answer.body.get("items").is_none());
}

#[tokio::test]
async fn the_history_at_the_written_tenant_attaches_across_pages_and_counts_changes_only() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    // The change, its report at the same tenant, and a newer record of the key.
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    seed_audit(&h, Row::report(id(101), "proxy", a, id(100), 11)).await;
    seed_audit(&h, Row::change(id(200), "proxy", a, 20)).await;

    let first = h
        .send("GET", &history_uri(&h, "proxy", "?limit=1"), None, None, a)
        .await;
    assert_eq!(first.status, 200, "{}", first.raw);
    assert_eq!(listed_ids(&first.body), [id(200).to_string()]);
    assert_eq!(first.body["items"][0]["activations"], json!([]));
    assert_eq!(first.body["page_info"]["total_count"], json!(2));
    let cursor = first.body["page_info"]["next_cursor"]
        .as_str()
        .expect("a second page")
        .to_owned();

    let second = h
        .send(
            "GET",
            &history_uri(&h, "proxy", &format!("?limit=1&cursor={cursor}")),
            None,
            None,
            a,
        )
        .await;
    assert_eq!(second.status, 200, "{}", second.raw);
    assert_eq!(listed_ids(&second.body), [id(100).to_string()]);
    let attached = second.body["items"][0]["activations"]
        .as_array()
        .expect("array");
    assert_eq!(attached.len(), 1, "{}", second.raw);
    assert_eq!(attached[0]["change_ref"], json!(id(100).to_string()));
    assert_eq!(second.body["page_info"]["total_count"], json!(2));
    assert!(
        second.body["page_info"]["next_cursor"].is_null()
            || second.body["page_info"].get("next_cursor").is_none()
    );
}

#[tokio::test]
async fn a_report_whose_change_is_gone_is_listed_as_an_ordinary_record() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    // Retention removed the change record: nothing here is its change.
    seed_audit(
        &h,
        Row::report(id(101), "proxy", a, id(100), 11).failed("late"),
    )
    .await;
    seed_audit(&h, Row::change(id(200), "proxy", a, 20)).await;
    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(
        listed_ids(&answer.body),
        [id(200).to_string(), id(101).to_string()]
    );
    assert_eq!(answer.body["page_info"]["total_count"], json!(2));
    let report = &answer.body["items"][1];
    assert_eq!(report["change_ref"], json!(id(100).to_string()));
    assert_eq!(report["detail"], json!("late"));
}

#[tokio::test]
async fn two_reports_under_a_change_are_ordered_newest_first_then_by_id() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    seed_audit(
        &h,
        Row::report(id(103), "proxy", a, id(100), 12).by("gear-3"),
    )
    .await;
    seed_audit(
        &h,
        Row::report(id(101), "proxy", a, id(100), 12).by("gear-1"),
    )
    .await;
    seed_audit(
        &h,
        Row::report(id(102), "proxy", a, id(100), 15).by("gear-2"),
    )
    .await;
    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    let actors: Vec<&str> = answer.body["items"][0]["activations"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|r| r["actor"].as_str())
        .collect();
    // Newest first; the two of one instant in descending id, as the history
    // itself breaks a tie.
    assert_eq!(actors, ["gear-2", "gear-3", "gear-1"]);
}

#[tokio::test]
async fn a_subscriber_is_shown_unmasked_to_a_caller_without_the_pii_entitlement() {
    let h = RestHarness::without_pii_entitlement().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    seed_audit(&h, Row::report(id(101), "proxy", a, id(100), 11)).await;
    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(
        answer.body["items"][0]["actor_masked"],
        json!(true),
        "the change's actor"
    );
    assert_eq!(
        answer.body["items"][0]["activations"][0]["actor"],
        json!("gear-consumer")
    );
}

#[tokio::test]
async fn an_attached_pii_actor_alone_asks_for_the_entitlement_and_is_masked_without_it() {
    let h = RestHarness::without_pii_entitlement().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    // The change's own actor is public and the setting holds no personal
    // data, so only the attached report's actor is `pii`.
    let mut change = Row::change(id(100), "proxy", a, 10);
    change.classification = "public";
    seed_audit(&h, change).await;
    let mut report = Row::report(id(101), "proxy", a, id(100), 11).by("consumer@acme");
    report.classification = "pii";
    seed_audit(&h, report).await;
    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(answer.body["items"][0]["actor_masked"], json!(false));
    assert_eq!(
        answer.body["items"][0]["activations"][0]["actor"],
        json!(crate::domain::resolution::MASK_TOKEN)
    );
}

#[tokio::test]
async fn an_unknown_stored_outcome_fails_the_history_and_the_attached_path_as_an_integrity_error() {
    // Listed: a report whose change record is not in the history.
    let h = RestHarness::ignoring_checks().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let mut listed = Row::report(id(101), "proxy", a, id(100), 11);
    listed.outcome = "partial";
    seed_audit(&h, listed).await;
    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 500, "{}", answer.raw);
    assert!(answer.body.get("items").is_none());

    // Attached: a report under a change that is in the history.
    let h = RestHarness::ignoring_checks().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    let mut attached = Row::report(id(101), "proxy", a, id(100), 11);
    attached.outcome = "partial";
    seed_audit(&h, attached).await;
    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 500, "{}", answer.raw);
    assert!(answer.body.get("items").is_none());
}

#[tokio::test]
async fn the_history_keeps_its_own_refusals() {
    let h = RestHarness::new().await;
    let hidden = h.inner.declare("hidden", "cascading", json!(true)).await;
    h.restrict(hidden, h.inner.tree.a, TenantAccess::Hidden)
        .await;
    let a = h.inner.tree.a;
    let outside = format!("?tenant={}", h.inner.tree.c);
    let denied = h
        .send("GET", &history_uri(&h, "hidden", &outside), None, None, a)
        .await;
    assert_eq!(denied.status, 403, "{}", denied.raw);
    let absent = h
        .send("GET", &history_uri(&h, "hidden", ""), None, None, a)
        .await;
    assert_eq!(absent.status, 404, "{}", absent.raw);
}

#[tokio::test]
async fn a_history_page_makes_one_attachment_lookup_and_neither_read_writes() {
    let (h, recorder) = RestHarness::recorded().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    for n in 0..5_u128 {
        seed_audit(
            &h,
            Row::change(id(100 + n), "proxy", a, i64::try_from(n).unwrap_or(0)),
        )
        .await;
        seed_audit(
            &h,
            Row::report(
                id(200 + n),
                "proxy",
                a,
                id(100 + n),
                50 + i64::try_from(n).unwrap_or(0),
            ),
        )
        .await;
    }
    recorder.clear();
    let answer = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert!(
        answer.body["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|record| record["activations"]
                .as_array()
                .is_some_and(|r| r.len() == 1))
    );
    let attachment_lookups = recorder
        .events()
        .iter()
        .filter(|e| {
            e.kind == QueryKind::Select
                && e.table.as_deref() == Some("audit_records")
                && e.sql.contains("\"change_ref\" IN (")
                && e.sql.contains("\"operation\" = ?")
        })
        .count();
    assert_eq!(attachment_lookups, 1, "{}", recorder.dump());
    let read = h
        .send("GET", &read_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(read.status, 200);
    assert_eq!(writes(&recorder), 0, "{}", recorder.dump());
}
