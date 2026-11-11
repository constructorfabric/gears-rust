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
use crate::domain::activation::{ActivationRepository, ActivationStatus, InstanceKey, tenant_text};
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

/// The id of the consumer instance a test calls `n`: ordered as `n` is.
fn inst(n: u128) -> Uuid {
    Uuid::from_u128(0x1000 + n)
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
        .register(
            &conn,
            &InstanceKey {
                subscriber: subscriber.to_owned(),
                instance_id: Uuid::nil(),
                key: key.clone(),
                tenant: tenant.clone(),
            },
            None,
        )
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
                    instance_id: Uuid::nil(),
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
    /// The tenant the actor acted from; the record's own tenant by default.
    actor_tenant: Option<Uuid>,
    /// The consumer instance an `activation` record is about, with its label.
    instance: Option<(Uuid, &'a str)>,
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
            actor_tenant: Some(tenant),
            instance: None,
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
            actor_tenant: Some(tenant),
            instance: None,
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

    fn silent(mut self) -> Self {
        self.outcome = "silent";
        self.detail = None;
        self
    }

    fn instance(mut self, n: u128, label: &'a str) -> Self {
        self.instance = Some((inst(n), label));
        self
    }

    fn acting_from(mut self, tenant: Uuid) -> Self {
        self.actor_tenant = Some(tenant);
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
        actor_tenant_id: Set(row.actor_tenant),
        pre_value: Set(None),
        post_value: Set(None),
        outcome: Set(row.outcome.to_owned()),
        request_id: Set("r".to_owned()),
        change_set_id: Set(None),
        change_ref: Set(row.change_ref),
        detail: Set(row.detail.map(str::to_owned)),
        instance_id: Set(row.instance.map(|(id, _)| id)),
        instance_label: Set(row.instance.map(|(_, label)| label.to_owned())),
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
    let outcomes: Vec<_> = items.iter().map(|item| item["outcome"].clone()).collect();
    assert_eq!(
        outcomes,
        [json!("awaiting"), json!("succeeded"), json!("failed")]
    );
    for (item, change) in items.iter().zip([id(1), id(2), id(3)]) {
        assert_eq!(item["change_ref"], json!(change.to_string()));
        assert_eq!(item["expected"], json!(1));
        assert_eq!(item["silent"], json!(0));
        assert_eq!(item["silent_excluded"], json!(0));
        assert_eq!(item["instances"].as_array().map(Vec::len), Some(1));
        assert_eq!(item["instances"][0]["excluded"], json!(false));
    }
    let entry = |n: usize| &items[n]["instances"][0];
    assert_eq!(entry(0)["status"], json!("awaiting"));
    assert_eq!(entry(1)["status"], json!("succeeded"));
    assert_eq!(entry(2)["status"], json!("failed"));
    assert_eq!(items[1]["confirmed"], json!(1));
    assert_eq!(items[2]["failed"], json!(1));
    assert!(entry(0).get("reported_at").is_none(), "{}", entry(0));
    assert!(entry(1)["reported_at"].is_string());
    assert!(entry(2)["reported_at"].is_string());
    assert!(entry(0).get("detail").is_none() && entry(1).get("detail").is_none());
    assert_eq!(entry(2)["detail"], json!("pool full"));
    assert!(items.iter().all(|item| item.get("status").is_none()));
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
    // One read of the states, joined to their subscriptions, and one of the
    // silent subscriptions.
    assert_eq!(selects_of(&recorder, "activation_state"), 1);
    assert_eq!(
        recorder
            .events()
            .iter()
            .filter(|e| e.kind == QueryKind::Select
                && e.table.as_deref() == Some("subscriptions")
                && e.sql.contains("\"silent_since\" IS NOT NULL"))
            .count(),
        1,
        "{}",
        recorder.dump()
    );
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
async fn an_attached_actor_who_acted_above_the_reader_is_withheld_and_shown_to_the_root() {
    let h = RestHarness::ignoring_checks().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    // The consumer reported from the platform tenant, above the reader at `a`.
    seed_audit(
        &h,
        Row::report(id(101), "proxy", a, id(100), 11).acting_from(root),
    )
    .await;

    let below = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(below.status, 200, "{}", below.raw);
    let report = &below.body["items"][0]["activations"][0];
    assert_eq!(
        report["actor"],
        json!(crate::domain::resolution::MASK_TOKEN),
        "{report}"
    );
    assert_eq!(report["actor_withheld"], json!(true), "{report}");
    assert_eq!(
        below.body["items"][0]["actor"],
        json!("admin@acme"),
        "the change's own actor"
    );

    let above = h
        .send(
            "GET",
            &history_uri(&h, "proxy", &format!("?tenant={a}")),
            None,
            None,
            root,
        )
        .await;
    assert_eq!(above.status, 200, "{}", above.raw);
    let report = &above.body["items"][0]["activations"][0];
    assert_eq!(report["actor"], json!("gear-consumer"), "{report}");
    assert!(report.get("actor_withheld").is_none(), "{report}");
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

// ── The per-instance account ─────────────────────────────────────────────────

fn instance_key(
    h: &RestHarness,
    subscriber: &str,
    name: &str,
    tenant: Uuid,
    n: u128,
) -> InstanceKey {
    InstanceKey {
        subscriber: subscriber.to_owned(),
        instance_id: inst(n),
        key: h.inner.key(name).to_string(),
        tenant: tenant_text(tenant),
    }
}

/// Register instance `n` of `subscriber`, labelled `host-n`.
async fn subscribe(h: &RestHarness, subscriber: &str, name: &str, tenant: Uuid, n: u128) -> Uuid {
    let conn = h.inner.db.conn().expect("connection");
    ActivationRepo
        .register(
            &conn,
            &instance_key(h, subscriber, name, tenant, n),
            Some(&format!("host-{n}")),
        )
        .await
        .expect("registered")
        .id
}

/// A change `change_ref` writing a state for each of the subscriptions: the
/// ones whose instance is silent are left out, as the change itself leaves them.
async fn change(
    h: &RestHarness,
    name: &str,
    tenant: Uuid,
    change_ref: Uuid,
    subscriptions: &[Uuid],
) {
    let conn = h.inner.db.conn().expect("connection");
    ActivationRepo
        .write_states(
            &conn,
            &StateWrite {
                key: h.inner.key(name).to_string(),
                change_ref,
                rows: subscriptions
                    .iter()
                    .map(|subscription_id| StateRow {
                        subscription_id: *subscription_id,
                        tenant: tenant_text(tenant),
                        expected_value: "EXPECTED".to_owned(),
                    })
                    .collect(),
            },
        )
        .await
        .expect("states written");
}

#[allow(clippy::too_many_arguments)]
async fn settle(
    h: &RestHarness,
    subscriber: &str,
    name: &str,
    tenant: Uuid,
    n: u128,
    change_ref: Uuid,
    status: ActivationStatus,
    detail: Option<&str>,
) {
    let conn = h.inner.db.conn().expect("connection");
    let key = instance_key(h, subscriber, name, tenant, n);
    let settled = ActivationRepo
        .report(
            &conn,
            &Transition {
                subscriber: subscriber.to_owned(),
                instance_id: key.instance_id,
                key: key.key,
                tenant: key.tenant,
                change_ref,
                status,
                applied_value: Some("APPLIED".to_owned()),
                detail: detail.map(str::to_owned),
            },
        )
        .await
        .expect("settled");
    assert!(settled.is_some(), "the report applied");
}

/// The delivery loop's call of silence: the row `silent` and the subscription
/// marked, `seconds` into the test's epoch.
async fn call_silent(
    h: &RestHarness,
    subscriber: &str,
    name: &str,
    tenant: Uuid,
    n: u128,
    change_ref: Uuid,
    seconds: i64,
) {
    let conn = h.inner.db.conn().expect("connection");
    let key = instance_key(h, subscriber, name, tenant, n);
    let at = OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).expect("a timestamp");
    assert!(
        ActivationRepo
            .call_silent(&conn, &key, change_ref, at)
            .await
            .expect("called silent")
    );
    ActivationRepo
        .mark_silent(&conn, &key, at)
        .await
        .expect("marked");
}

async fn read_as(h: &RestHarness, name: &str, tenant: Uuid, caller: Uuid) -> Value {
    let answer = h
        .send(
            "GET",
            &read_uri(h, name, &format!("?tenant={tenant}")),
            None,
            None,
            caller,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    answer.body
}

/// The one item of a read, whoever reads.
fn only_item(body: &Value) -> &Value {
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "{body}");
    &items[0]
}

fn counts(item: &Value) -> [u64; 5] {
    [
        "expected",
        "confirmed",
        "failed",
        "silent",
        "silent_excluded",
    ]
    .map(|name| item[name].as_u64().expect(name))
}

fn statuses(item: &Value) -> Vec<(String, bool)> {
    item["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .map(|e| {
            (
                e["status"].as_str().expect("status").to_owned(),
                e["excluded"].as_bool().expect("excluded"),
            )
        })
        .collect()
}

#[tokio::test]
async fn an_item_lists_each_instance_with_the_counts_and_orders_them_by_status() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let (s1, s2, s3) = (
        subscribe(&h, "gear-a", "proxy", a, 1).await,
        subscribe(&h, "gear-a", "proxy", a, 2).await,
        subscribe(&h, "gear-a", "proxy", a, 3).await,
    );
    change(&h, "proxy", a, id(7), &[s1, s2, s3]).await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        2,
        id(7),
        ActivationStatus::Succeeded,
        None,
    )
    .await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        3,
        id(7),
        ActivationStatus::Failed,
        Some("pool full"),
    )
    .await;

    let body = read_as(&h, "proxy", a, a).await;
    let item = only_item(&body);
    assert_eq!(item["outcome"], json!("failed"));
    assert_eq!(counts(item), [3, 1, 1, 0, 0]);
    assert_eq!(
        statuses(item),
        [
            ("awaiting".to_owned(), false),
            ("succeeded".to_owned(), false),
            ("failed".to_owned(), false),
        ]
    );
}

#[tokio::test]
async fn the_outcome_follows_the_precedence_over_every_combination_of_rows() {
    // The four acceptance rows: failed over incomplete, incomplete over
    // awaiting, awaiting over succeeded, and all succeeded.
    let rows: [(&[ActivationStatus], &str); 4] = [
        (
            &[ActivationStatus::Failed, ActivationStatus::Silent],
            "failed",
        ),
        (
            &[ActivationStatus::Silent, ActivationStatus::Awaiting],
            "incomplete",
        ),
        (
            &[ActivationStatus::Awaiting, ActivationStatus::Succeeded],
            "awaiting",
        ),
        (
            &[ActivationStatus::Succeeded, ActivationStatus::Succeeded],
            "succeeded",
        ),
    ];
    for (statuses_of_rows, outcome) in rows {
        let h = RestHarness::new().await;
        h.inner.declare("proxy", "cascading", json!(true)).await;
        let a = h.inner.tree.a;
        let mut subscriptions = Vec::new();
        for n in 0..statuses_of_rows.len() {
            subscriptions.push(subscribe(&h, "gear-a", "proxy", a, n as u128 + 1).await);
        }
        change(&h, "proxy", a, id(7), &subscriptions).await;
        for (n, status) in statuses_of_rows.iter().enumerate() {
            let n = n as u128 + 1;
            match status {
                ActivationStatus::Awaiting => {}
                ActivationStatus::Silent => {
                    call_silent(&h, "gear-a", "proxy", a, n, id(7), 10).await;
                }
                other => settle(&h, "gear-a", "proxy", a, n, id(7), *other, Some("x")).await,
            }
        }
        let body = read_as(&h, "proxy", a, a).await;
        assert_eq!(
            only_item(&body)["outcome"],
            json!(outcome),
            "{statuses_of_rows:?}"
        );
    }
}

#[tokio::test]
async fn a_subscriber_whose_instances_are_all_silent_reads_incomplete_zero_of_m() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let subscriptions = [
        subscribe(&h, "gear-a", "proxy", a, 1).await,
        subscribe(&h, "gear-a", "proxy", a, 2).await,
    ];
    change(&h, "proxy", a, id(7), &subscriptions).await;
    for n in [1, 2] {
        call_silent(&h, "gear-a", "proxy", a, n, id(7), 10).await;
    }
    let body = read_as(&h, "proxy", a, a).await;
    let item = only_item(&body);
    assert_eq!(item["outcome"], json!("incomplete"));
    assert_eq!(counts(item), [2, 0, 0, 2, 0]);
}

#[tokio::test]
async fn a_late_report_and_a_re_arm_each_re_derive_the_next_read() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let mut subscriptions = Vec::new();
    for n in 1..=7 {
        subscriptions.push(subscribe(&h, "gear-a", "proxy", a, n).await);
    }
    change(&h, "proxy", a, id(7), &subscriptions).await;
    for n in 1..=6 {
        settle(
            &h,
            "gear-a",
            "proxy",
            a,
            n,
            id(7),
            ActivationStatus::Succeeded,
            None,
        )
        .await;
    }
    call_silent(&h, "gear-a", "proxy", a, 7, id(7), 10).await;

    let body = read_as(&h, "proxy", a, a).await;
    let item = only_item(&body);
    assert_eq!(item["outcome"], json!("incomplete"));
    assert_eq!(counts(item), [7, 6, 0, 1, 0]);
    let silent = item["instances"].as_array().expect("instances")[6].clone();
    assert_eq!(silent["status"], json!("silent"));
    assert!(silent["silent_since"].is_string() && silent.get("reported_at").is_none());

    // Re-arm: the row is `awaiting` again with no `silent_since`.
    let conn = h.inner.db.conn().expect("connection");
    assert!(
        ActivationRepo
            .rearm(&conn, &instance_key(&h, "gear-a", "proxy", a, 7), id(7))
            .await
            .expect("re-armed")
    );
    let body = read_as(&h, "proxy", a, a).await;
    let item = only_item(&body);
    assert_eq!(item["outcome"], json!("awaiting"));
    assert_eq!(counts(item), [7, 6, 0, 0, 0]);
    let rearmed = item["instances"].as_array().expect("instances")[0].clone();
    assert_eq!(rearmed["status"], json!("awaiting"));
    assert!(rearmed.get("silent_since").is_none(), "{rearmed}");

    // Late confirmation: `succeeded`, `silent_since` still beside `reported_at`.
    call_silent(&h, "gear-a", "proxy", a, 7, id(7), 20).await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        7,
        id(7),
        ActivationStatus::Succeeded,
        None,
    )
    .await;
    let body = read_as(&h, "proxy", a, a).await;
    let item = only_item(&body);
    assert_eq!(item["outcome"], json!("succeeded"));
    assert_eq!(counts(item), [7, 7, 0, 0, 0]);
    let late = item["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .find(|e| e["status"] == json!("succeeded") && e.get("silent_since").is_some())
        .expect("the late confirmation");
    assert!(late["reported_at"].is_string(), "{late}");
}

#[tokio::test]
async fn a_late_failure_reads_failed() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let subscriptions = [
        subscribe(&h, "gear-a", "proxy", a, 1).await,
        subscribe(&h, "gear-a", "proxy", a, 2).await,
    ];
    change(&h, "proxy", a, id(7), &subscriptions).await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        1,
        id(7),
        ActivationStatus::Succeeded,
        None,
    )
    .await;
    call_silent(&h, "gear-a", "proxy", a, 2, id(7), 10).await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        2,
        id(7),
        ActivationStatus::Failed,
        Some("pool full"),
    )
    .await;
    let body = read_as(&h, "proxy", a, a).await;
    let item = only_item(&body);
    assert_eq!(item["outcome"], json!("failed"));
    let failed = &item["instances"][1];
    assert!(failed["silent_since"].is_string() && failed["reported_at"].is_string());
    assert_eq!(failed["detail"], json!("pool full"));
}

#[tokio::test]
async fn a_silent_instance_the_change_left_out_is_an_excluded_entry_counted_apart() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let (first, second) = (
        subscribe(&h, "gear-a", "proxy", a, 1).await,
        subscribe(&h, "gear-a", "proxy", a, 2).await,
    );
    // Change 1 leaves instance 2 silent; change 2 is written without it.
    change(&h, "proxy", a, id(1), &[first, second]).await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        1,
        id(1),
        ActivationStatus::Succeeded,
        None,
    )
    .await;
    call_silent(&h, "gear-a", "proxy", a, 2, id(1), 10).await;
    change(&h, "proxy", a, id(2), &[first, second]).await;

    let body = read_as(&h, "proxy", a, a).await;
    let item = only_item(&body);
    assert_eq!(item["change_ref"], json!(id(2).to_string()));
    assert_eq!(counts(item), [1, 0, 0, 0, 1]);
    assert_eq!(item["outcome"], json!("awaiting"));
    assert_eq!(
        statuses(item),
        [("awaiting".to_owned(), false), ("silent".to_owned(), true)]
    );
    let excluded = &item["instances"][1];
    assert!(excluded["silent_since"].is_string(), "{excluded}");
    assert!(excluded.get("reported_at").is_none() && excluded.get("detail").is_none());
}

#[tokio::test]
async fn a_subscriber_with_a_registration_and_no_row_has_no_item() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    subscribe(&h, "gear-a", "proxy", a, 1).await;
    subscribe(&h, "gear-a", "proxy", a, 2).await;
    // One of them silent: still no item, and no excluded entry to carry.
    call_silent_without_row(&h, "gear-a", "proxy", a, 2).await;
    let body = read_as(&h, "proxy", a, a).await;
    assert_eq!(body["items"], json!([]), "{body}");

    // A subscriber with rows beside it keeps its own item alone.
    let other = subscribe(&h, "gear-b", "proxy", a, 3).await;
    change(&h, "proxy", a, id(5), &[other]).await;
    let body = read_as(&h, "proxy", a, a).await;
    assert_eq!(subscribers(&body), ["gear-b"]);
}

/// A subscription marked silent with no row at all.
async fn call_silent_without_row(
    h: &RestHarness,
    subscriber: &str,
    name: &str,
    tenant: Uuid,
    n: u128,
) {
    let conn = h.inner.db.conn().expect("connection");
    ActivationRepo
        .mark_silent(
            &conn,
            &instance_key(h, subscriber, name, tenant, n),
            OffsetDateTime::from_unix_timestamp(1_800_000_010).expect("a timestamp"),
        )
        .await
        .expect("marked");
}

#[tokio::test]
async fn forgetting_the_one_silent_instance_makes_the_next_read_succeeded_and_removes_no_record() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let subscriptions = [
        subscribe(&h, "gear-a", "proxy", a, 1).await,
        subscribe(&h, "gear-a", "proxy", a, 2).await,
    ];
    seed_audit(&h, Row::change(id(7), "proxy", a, 1)).await;
    change(&h, "proxy", a, id(7), &subscriptions).await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        1,
        id(7),
        ActivationStatus::Succeeded,
        None,
    )
    .await;
    call_silent(&h, "gear-a", "proxy", a, 2, id(7), 10).await;
    seed_audit(
        &h,
        Row::report(id(8), "proxy", a, id(7), 2)
            .silent()
            .instance(2, "host-2"),
    )
    .await;

    let body = read_as(&h, "proxy", a, a).await;
    assert_eq!(only_item(&body)["outcome"], json!("incomplete"));
    assert_eq!(counts(only_item(&body)), [2, 1, 0, 1, 0]);

    let conn = h.inner.db.conn().expect("connection");
    let forgotten = ActivationRepo
        .forget(
            &conn,
            &instance_key(&h, "gear-a", "proxy", a, 2),
            OffsetDateTime::from_unix_timestamp(1_800_000_000 + 86_400).expect("a timestamp"),
        )
        .await
        .expect("forgotten");
    assert!(forgotten);
    let body = read_as(&h, "proxy", a, a).await;
    assert_eq!(only_item(&body)["outcome"], json!("succeeded"));
    assert_eq!(counts(only_item(&body)), [1, 1, 0, 0, 0]);

    // The history keeps its record of the call of silence under the change.
    let history = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(history.status, 200, "{}", history.raw);
    let attached = history.body["items"][0]["activations"]
        .as_array()
        .expect("activations");
    assert_eq!(attached.len(), 1, "{}", history.raw);
    assert_eq!(attached[0]["outcome"], json!("silent"));
}

#[tokio::test]
async fn the_identity_is_shown_to_the_root_only_and_the_rest_of_the_read_is_the_same() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    // Ids are written out of the order they sort in; the order is the id's.
    let mut subscriptions = Vec::new();
    for n in [5, 3, 4, 1, 2] {
        subscriptions.push(subscribe(&h, "gear-a", "proxy", a, n).await);
    }
    let left_out = subscribe(&h, "gear-a", "proxy", a, 9).await;
    call_silent_without_row(&h, "gear-a", "proxy", a, 9).await;
    let _ = left_out;
    change(&h, "proxy", a, id(7), &subscriptions).await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        4,
        id(7),
        ActivationStatus::Succeeded,
        None,
    )
    .await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        3,
        id(7),
        ActivationStatus::Succeeded,
        None,
    )
    .await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        2,
        id(7),
        ActivationStatus::Failed,
        Some("boom"),
    )
    .await;
    call_silent(&h, "gear-a", "proxy", a, 5, id(7), 10).await;

    let platform = read_as(&h, "proxy", a, root).await;
    let tenant = read_as(&h, "proxy", a, a).await;

    // The root sees both fields on every entry, the excluded one included.
    let seen = only_item(&platform)["instances"]
        .as_array()
        .expect("instances")
        .clone();
    assert_eq!(seen.len(), 6);
    for entry in &seen {
        assert!(entry["instance_id"].is_string(), "{entry}");
        assert!(entry["instance_label"].is_string(), "{entry}");
    }
    assert_eq!(seen[5]["excluded"], json!(true));
    assert_eq!(seen[5]["instance_label"], json!("host-9"));
    // The tenant administrator sees none, though it is the one who wrote the
    // change; the fields are absent, not blank.
    assert!(!tenant.to_string().contains("instance_"), "{tenant}");
    assert!(!tenant.to_string().contains("host-"), "{tenant}");

    // Everything else, the order included, is the same document.
    let strip = |body: &Value| {
        let mut body = body.clone();
        for entry in body["items"][0]["instances"]
            .as_array_mut()
            .expect("instances")
        {
            let entry = entry.as_object_mut().expect("entry");
            entry.remove("instance_id");
            entry.remove("instance_label");
        }
        body
    };
    assert_eq!(strip(&platform), strip(&tenant));
    // Rows by status then id: awaiting 1, succeeded 3 and 4, failed 2, silent
    // 5; then the excluded one.
    let order: Vec<_> = seen
        .iter()
        .map(|e| e["instance_label"].as_str().expect("label").to_owned())
        .collect();
    assert_eq!(
        order,
        ["host-1", "host-3", "host-4", "host-2", "host-5", "host-9"]
    );
    // Two reads of an unchanged account list them alike.
    assert_eq!(read_as(&h, "proxy", a, root).await, platform);
}

#[tokio::test]
async fn an_item_lists_the_instances_the_registration_cap_allows_as_one_resource() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let mut subscriptions = Vec::new();
    for n in 1..=64 {
        subscriptions.push(subscribe(&h, "gear-a", "proxy", a, n).await);
    }
    change(&h, "proxy", a, id(7), &subscriptions).await;
    let answer = h
        .send("GET", &read_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.raw);
    let item = only_item(&answer.body);
    assert_eq!(item["instances"].as_array().map(Vec::len), Some(64));
    assert_eq!(counts(item), [64, 0, 0, 0, 0]);
    assert!(answer.body.get("page_info").is_none());
    assert!(answer.body.get("next_cursor").is_none());
}

// ── The history, per instance ────────────────────────────────────────────────

#[tokio::test]
async fn the_history_attaches_one_item_per_instance_and_shows_identity_to_the_root_only() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    seed_audit(
        &h,
        Row::report(id(101), "proxy", a, id(100), 11).instance(1, "host-1"),
    )
    .await;
    seed_audit(
        &h,
        Row::report(id(102), "proxy", a, id(100), 12).instance(2, "host-2"),
    )
    .await;

    let as_root = h
        .send(
            "GET",
            &history_uri(&h, "proxy", &format!("?tenant={a}")),
            None,
            None,
            root,
        )
        .await;
    assert_eq!(as_root.status, 200, "{}", as_root.raw);
    let attached = as_root.body["items"][0]["activations"]
        .as_array()
        .expect("activations");
    assert_eq!(
        attached.len(),
        2,
        "two instances, two items: {}",
        as_root.raw
    );
    assert_eq!(attached[0]["instance_id"], json!(inst(2).to_string()));
    assert_eq!(attached[0]["instance_label"], json!("host-2"));
    assert_eq!(attached[1]["instance_id"], json!(inst(1).to_string()));

    // The administrator who made the change is not shown them.
    let as_tenant = h
        .send("GET", &history_uri(&h, "proxy", ""), None, None, a)
        .await;
    assert_eq!(as_tenant.status, 200, "{}", as_tenant.raw);
    assert_eq!(
        as_tenant.body["items"][0]["activations"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert!(!as_tenant.raw.contains("instance_"), "{}", as_tenant.raw);
    assert!(!as_tenant.raw.contains("host-"), "{}", as_tenant.raw);
}

#[tokio::test]
async fn a_call_of_silence_stays_under_its_change_after_a_later_change_and_the_forgetting() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    let subscriptions = [
        subscribe(&h, "gear-a", "proxy", a, 1).await,
        subscribe(&h, "gear-a", "proxy", a, 2).await,
    ];
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    change(&h, "proxy", a, id(100), &subscriptions).await;
    settle(
        &h,
        "gear-a",
        "proxy",
        a,
        1,
        id(100),
        ActivationStatus::Succeeded,
        None,
    )
    .await;
    call_silent(&h, "gear-a", "proxy", a, 2, id(100), 20).await;
    seed_audit(
        &h,
        Row::report(id(101), "proxy", a, id(100), 11).instance(1, "host-1"),
    )
    .await;
    seed_audit(
        &h,
        Row::report(id(102), "proxy", a, id(100), 21)
            .silent()
            .instance(2, "host-2"),
    )
    .await;
    // A later change, then the forgetting of the silent instance.
    seed_audit(&h, Row::change(id(200), "proxy", a, 30)).await;
    change(&h, "proxy", a, id(200), &subscriptions).await;
    let conn = h.inner.db.conn().expect("connection");
    ActivationRepo
        .forget(
            &conn,
            &instance_key(&h, "gear-a", "proxy", a, 2),
            OffsetDateTime::from_unix_timestamp(1_800_000_000 + 86_400).expect("a timestamp"),
        )
        .await
        .expect("forgotten");

    let history = h
        .send(
            "GET",
            &history_uri(&h, "proxy", &format!("?tenant={a}")),
            None,
            None,
            root,
        )
        .await;
    assert_eq!(history.status, 200, "{}", history.raw);
    let items = history.body["items"].as_array().expect("items");
    let first = items
        .iter()
        .find(|r| r["id"] == json!(id(100).to_string()))
        .expect("the first change");
    let attached = first["activations"].as_array().expect("activations");
    assert_eq!(attached.len(), 2, "{first}");
    let silent = attached
        .iter()
        .find(|r| r["outcome"] == json!("silent"))
        .expect("the call of silence");
    assert!(silent.get("detail").is_none(), "{silent}");
    assert_eq!(silent["instance_label"], json!("host-2"));
    let later = items
        .iter()
        .find(|r| r["id"] == json!(id(200).to_string()))
        .expect("the later change");
    assert_eq!(later["activations"], json!([]));
}

#[tokio::test]
async fn a_listed_activation_record_carries_its_instance_for_the_root_only() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    // A report at `b` whose change was written at `a`: an ordinary record of
    // `b`'s history.
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    seed_audit(
        &h,
        Row::report(id(101), "proxy", b, id(100), 11).instance(1, "host-1"),
    )
    .await;

    let uri = history_uri(&h, "proxy", &format!("?tenant={b}"));
    let platform = h.send("GET", &uri, None, None, root).await;
    assert_eq!(platform.status, 200, "{}", platform.raw);
    assert_eq!(platform.body["items"][0]["instance_label"], json!("host-1"));
    assert_eq!(
        platform.body["items"][0]["instance_id"],
        json!(inst(1).to_string())
    );
    let tenant = h.send("GET", &uri, None, None, a).await;
    assert_eq!(tenant.status, 200, "{}", tenant.raw);
    assert_eq!(listed_ids(&tenant.body), [id(101).to_string()]);
    assert!(!tenant.raw.contains("instance_"), "{}", tenant.raw);
    assert!(!tenant.raw.contains("host-"), "{}", tenant.raw);
}

#[tokio::test]
async fn no_instance_identity_reaches_a_log_line_of_a_reader_who_may_not_see_it() {
    use crate::domain::activation::testing::{capture, logged};
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;
    let subscriptions = [subscribe(&h, "gear-a", "proxy", a, 1).await];
    change(&h, "proxy", a, id(100), &subscriptions).await;
    call_silent(&h, "gear-a", "proxy", a, 1, id(100), 10).await;
    seed_audit(&h, Row::change(id(100), "proxy", a, 10)).await;
    seed_audit(
        &h,
        Row::report(id(101), "proxy", a, id(100), 11)
            .silent()
            .instance(1, "host-1"),
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
    for identity in ["host-1", &inst(1).to_string()] {
        assert!(!log.contains(identity), "`{identity}` in the log: {log}");
    }
}
