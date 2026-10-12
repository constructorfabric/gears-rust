//! P-D-213: every act that touches a SKU's lifecycle stamps the move on its audit row
//! (`from_lifecycle`, `to_lifecycle`), and `GET /skus/{id}/history` reads the SKU's rows back.
//!
//! A child of the governance suite, so it drives the real doors through its `Fixture`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use axum::http::Method;

/// The SKU's audit rows as the history reads them — the SKU's own rows and its units' rows — raw,
/// one `action unit_kind from>to` line each (`-` for none), in the history's `audit_id` order.
async fn moves(f: &Fixture, sku: Uuid) -> Vec<String> {
    let hex = sku.simple().to_string().to_uppercase();
    let conn = sea_orm::Database::connect(&f.dsn).await.unwrap();
    let rows = {
        use sea_orm::ConnectionTrait;
        conn.query_all_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            format!(
                "SELECT a.action || ' ' || coalesce(u.kind, '-') || ' ' || \
                 coalesce(a.from_lifecycle, '-') || '>' || coalesce(a.to_lifecycle, '-') AS v \
                 FROM products_audit_log a LEFT JOIN products_approval_unit u \
                 ON a.subject_kind = 'approval_unit' AND u.id = a.subject_id \
                 WHERE (a.subject_kind = 'sku' AND hex(a.subject_id) = '{hex}') \
                 OR (a.subject_kind = 'approval_unit' AND hex(u.ref_id) = '{hex}') \
                 ORDER BY a.audit_id"
            ),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
    };
    conn.close().await.unwrap();
    rows
}

/// A unit's decision as `ctx`.
async fn decide(f: &Fixture, ctx: &SecurityContext, unit: &Value, action: &str) -> Value {
    let (status, b) = call(
        &f.app,
        ctx,
        Method::POST,
        &format!(
            "/approval-units/{}/{action}",
            unit["unit"]["id"].as_str().unwrap()
        ),
        if action == "withdraw" {
            json!({})
        } else {
            json!({"generation":unit["unit"]["generation"],"note":format!("{action} note")})
        },
        None,
    )
    .await;
    assert_eq!(status, 200, "{action}: {b}");
    b
}

/// An orphan fence the test leaves behind (no unit holds it), `age` old: the state a lost submit
/// leaves, and the only way to one through the doors' own tables.
async fn orphan_fence(f: &Fixture, kind: repo::Fence, age: time::Duration) {
    let (db, scope) = repo_connection(&f.dsn, f.tenant).await;
    let fenced = repo::fence_sku(
        &db.conn().unwrap(),
        &scope,
        f.tenant,
        f.id,
        kind,
        Uuid::new_v4(),
        time::OffsetDateTime::now_utc() - age,
    )
    .await
    .unwrap();
    assert!(matches!(fenced, repo::HeadWrite::Written(_)), "fenced");
}

/// The derivation table of P-D-213, driven through the doors: each act's row says the lifecycle it
/// found and the one it left, a unit's rows name its kind, the maintenance expiry writes its own
/// row as the system actor, and a row's `to` is the next row's `from` wherever no test fence came
/// between them.
#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one SKU's whole life, act by act")]
async fn every_lifecycle_move_is_stamped_by_the_act_that_made_it() {
    let f = Fixture::new(1).await;
    let tag = f.etag().await;
    let (status, _, b) = call_with(
        &f.app,
        &f.author,
        Method::PATCH,
        &format!("/skus/{}", f.id),
        json!({"description":"edited"}),
        &[("If-Match", tag)],
    )
    .await;
    assert_eq!(status, 200, "{b}");
    let (_, u) = f.post("/submit", json!({})).await;
    decide(&f, &f.reviewer, &u, "reject").await;
    f.policy(2).await;
    let (_, u) = f.post("/submit", json!({})).await;
    assert_eq!(
        decide(&f, &f.reviewer, &u, "approve").await["outcome"],
        "pending"
    );
    let second = authed_ctx(f.tenant);
    assert_eq!(
        decide(&f, &second, &u, "approve").await["outcome"],
        "applied"
    );
    f.policy(0).await;
    let (status, b) = f.post("/changes", json!({"lifecycle":"deprecated"})).await;
    assert_eq!((status, &b["applied"]), (200, &json!(true)), "{b}");
    f.policy(1).await;
    let (status, u) = f.post("/retire", json!({})).await;
    assert_eq!(status, 200, "{u}");
    decide(&f, &f.author, &u, "withdraw").await;
    orphan_fence(&f, repo::Fence::Retire, time::Duration::ZERO).await;
    assert_eq!(f.post("/unfence", json!({})).await.0, 200);
    orphan_fence(&f, repo::Fence::TypeChange, time::Duration::ZERO).await;
    assert_eq!(f.post("/unfence", json!({})).await.0, 200);
    // The card's maintenance expires an orphan fence, and so does the list's.
    orphan_fence(&f, repo::Fence::Retire, time::Duration::hours(2)).await;
    assert_eq!(f.card().await["lifecycle"], "deprecated");
    orphan_fence(&f, repo::Fence::Retire, time::Duration::hours(2)).await;
    let (status, b) = call(&f.app, &f.author, Method::GET, "/skus", json!({}), None).await;
    assert_eq!(status, 200, "{b}");
    f.policy(0).await;
    let (status, b) = f.post("/retire", json!({})).await;
    assert_eq!((status, &b["applied"]), (200, &json!(true)), "{b}");

    assert_eq!(
        moves(&f, f.id).await,
        [
            "sku.create - ->draft",
            "sku.draft_update - draft>draft",
            "approval.submit sku_publish draft>draft",
            "approval.rejected sku_publish draft>draft",
            "approval.submit sku_publish draft>draft",
            "approval.vote sku_publish draft>draft",
            "approval.approved sku_publish draft>published",
            "approval.submit sku_change published>published",
            "approval.applied sku_change published>deprecated",
            "approval.submit sku_retire deprecated>deprecated",
            "approval.withdrawn sku_retire deprecated>deprecated",
            // a test fence (retire) came between: no act of the doors made it
            "sku.unfence - deprecated>deprecated",
            // a test fence (type change) came between: it moves no lifecycle
            "sku.unfence - deprecated>deprecated",
            // a test fence (retire, two hours old) came between, expired by the card's read
            "sku.fence_expired - deprecated>deprecated",
            // and again, expired by the list's read
            "sku.fence_expired - deprecated>deprecated",
            "approval.submit sku_retire deprecated>deprecated",
            "approval.applied sku_retire deprecated>retired",
        ]
    );
    assert_eq!(
        raw_i64(
            &f.dsn,
            &format!(
                "SELECT COUNT(*) AS v FROM products_audit_log WHERE action = 'sku.fence_expired' \
                 AND {} AND reason = 'fence_ttl_minutes=30'",
                id_matches("actor_ref", repo::SYSTEM_ACTOR)
            ),
        )
        .await,
        2,
        "the expiry is the system's act, and says which TTL it applied"
    );
    // The acts on no SKU stamp nothing: the category, the policy writes.
    assert_eq!(
        raw_i64(
            &f.dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE subject_kind NOT IN ('sku', \
             'approval_unit') AND (from_lifecycle IS NOT NULL OR to_lifecycle IS NOT NULL)"
        )
        .await,
        0
    );
    assert!(
        raw_i64(
            &f.dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE subject_kind IN ('category', \
             'approval_policy')"
        )
        .await
            >= 5
    );
}

/// The derivation table's other rows: a draft delete leaves nothing, a refresh moves nothing, a
/// rejected retire returns to the lifecycle it came from, and a reference act stamps nothing.
#[tokio::test]
async fn a_delete_a_refresh_a_rejected_retire_and_a_reference_stamp_their_rows() {
    let f = Fixture::new(1).await;
    // A refresh, then the rejection of the refreshed generation.
    let (_, u) = f.post("/submit", json!({})).await;
    let (db, scope) = repo_connection(&f.dsn, f.tenant).await;
    let conn = db.conn().unwrap();
    let mut content = bss_products_sdk::models::SkuContent::from(
        &repo::find_sku(&conn, &scope, f.tenant, f.id)
            .await
            .unwrap()
            .unwrap(),
    );
    content.description = "drifted".into();
    repo::write_sku_content(
        &conn,
        &scope,
        f.tenant,
        f.id,
        &content,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let (status, body) = f.vote(&u, "reject", 1).await;
    assert_eq!((status, problem_code(&body).as_str()), (400, "UNIT_STALE"));
    assert_eq!(f.vote(&u, "reject", 2).await.0, 200);
    // A published SKU whose retire is rejected, then a reservation on it.
    f.publish().await;
    f.policy(1).await;
    let (_, u) = f.post("/retire", json!({})).await;
    decide(&f, &f.reviewer, &u, "reject").await;
    let (status, reference) = f.reserve(Uuid::new_v4()).await;
    assert_eq!(status, 201, "{reference}");
    assert_eq!(
        moves(&f, f.id).await,
        [
            "sku.create - ->draft",
            "approval.submit sku_publish draft>draft",
            "approval.refreshed sku_publish draft>draft",
            "approval.rejected sku_publish draft>draft",
            "approval.submit sku_publish draft>draft",
            "approval.applied sku_publish draft>published",
            "approval.submit sku_retire published>published",
            "approval.rejected sku_retire published>published",
        ]
    );
    assert_eq!(
        raw_i64(
            &f.dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE action = 'reference.reserve' \
             AND from_lifecycle IS NULL AND to_lifecycle IS NULL"
        )
        .await,
        1
    );
    // A never-published draft deleted: its create and its delete.
    let (status, s) = call(
        &f.app,
        &f.author,
        Method::POST,
        "/skus",
        json!({"code":"GONE","name":"Gone","type":"recurring"}),
        None,
    )
    .await;
    assert_eq!(status, 201, "{s}");
    let gone = Uuid::parse_str(s["id"].as_str().unwrap()).unwrap();
    let (status, _, b) = call_with(
        &f.app,
        &f.author,
        Method::DELETE,
        &format!("/skus/{gone}"),
        json!({}),
        &[("If-Match", "\"1\"".to_owned())],
    )
    .await;
    assert_eq!(status, 204, "{b}");
    assert_eq!(
        moves(&f, gone).await,
        ["sku.create - ->draft", "sku.delete - draft>-"]
    );
}

// ------------------------------------------------------------------ the read

/// `GET /skus/{sku}/history` with a raw query string, as `ctx`.
async fn history_as(f: &Fixture, ctx: &SecurityContext, sku: Uuid, query: &str) -> (u16, Value) {
    call(
        &f.app,
        ctx,
        Method::GET,
        &format!("/skus/{sku}/history{query}"),
        json!({}),
        None,
    )
    .await
}

/// Every entry of the history, walked `limit` at a time along `page_info.next_cursor`.
async fn walk(f: &Fixture, limit: usize) -> Vec<Value> {
    let mut entries = Vec::new();
    let mut query = format!("?limit={limit}");
    loop {
        let (status, page) = history_as(f, &f.author, f.id, &query).await;
        assert_eq!(status, 200, "{query}: {page}");
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= limit, "{page}");
        entries.extend(items.iter().cloned());
        let Some(next) = page["page_info"]["next_cursor"].as_str() else {
            break;
        };
        query = format!("?limit={limit}&cursor={next}");
        assert!(entries.len() < 50, "the walk ends");
    }
    entries
}

/// The history reads every act on the SKU and on its units, oldest first: who, what, the move,
/// the unit and its kind, the note. A quorum-0 submit and its apply share one instant, and a page
/// boundary between them skips and repeats nothing. A row written before the audit log carried
/// the lifecycle reads null for both. The history's own read expires an orphan fence, as the
/// system. Acts on no SKU (the category, the policy) and on another SKU are not in it.
#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one history, entry by entry")]
async fn the_history_reads_every_act_with_its_actor_unit_and_note_in_order() {
    let f = Fixture::new(1).await;
    // A row the deployed database wrote before 000008, a second before now: no lifecycle columns.
    let earlier = (time::OffsetDateTime::now_utc() - time::Duration::seconds(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let raw = sea_orm::Database::connect(&f.dsn).await.unwrap();
    {
        use sea_orm::ConnectionTrait;
        raw.execute_unprepared(&format!(
            "INSERT INTO products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,\
             subject_id,written_at,seal_state) VALUES (x'{}',x'{}',x'{}','sku.legacy','sku',x'{}',\
             '{earlier}','unsealed')",
            Uuid::nil().simple(),
            f.tenant.simple(),
            f.author.subject_id().simple(),
            f.id.simple(),
        ))
        .await
        .unwrap();
    }
    raw.close().await.unwrap();
    let (_, first) = f.post("/submit", json!({})).await;
    decide(&f, &f.reviewer, &first, "reject").await;
    f.policy(0).await;
    let (_, second) = f.post("/submit", json!({})).await;
    assert_eq!(second["applied"], true, "{second}");
    orphan_fence(&f, repo::Fence::Retire, time::Duration::hours(2)).await;
    // Another SKU's act is not in this one's history.
    let (status, other) = call(
        &f.app,
        &f.author,
        Method::POST,
        "/skus",
        json!({"code":"OTHER","name":"Other","type":"recurring"}),
        None,
    )
    .await;
    assert_eq!(status, 201, "{other}");

    let entries = walk(&f, 200).await;
    let author = f.author.subject_id().to_string();
    let reviewer = f.reviewer.subject_id().to_string();
    let system = repo::SYSTEM_ACTOR.to_string();
    let unit_of = |u: &Value| u["unit"]["id"].clone();
    let expected = [
        (
            "sku.legacy",
            &author,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ),
        (
            "sku.create",
            &author,
            Value::Null,
            json!("draft"),
            Value::Null,
            Value::Null,
            Value::Null,
        ),
        (
            "approval.submit",
            &author,
            json!("draft"),
            json!("draft"),
            unit_of(&first),
            json!("sku_publish"),
            Value::Null,
        ),
        (
            "approval.rejected",
            &reviewer,
            json!("draft"),
            json!("draft"),
            unit_of(&first),
            json!("sku_publish"),
            json!("reject note"),
        ),
        (
            "approval.submit",
            &author,
            json!("draft"),
            json!("draft"),
            unit_of(&second),
            json!("sku_publish"),
            Value::Null,
        ),
        (
            "approval.applied",
            &author,
            json!("draft"),
            json!("published"),
            unit_of(&second),
            json!("sku_publish"),
            Value::Null,
        ),
        (
            "sku.fence_expired",
            &system,
            json!("published"),
            json!("published"),
            Value::Null,
            Value::Null,
            json!("fence_ttl_minutes=30"),
        ),
    ];
    assert_eq!(entries.len(), expected.len(), "{entries:#?}");
    for (entry, (action, actor, from, to, unit, kind, note)) in entries.iter().zip(expected) {
        assert_eq!(
            (
                &entry["action"],
                &entry["actor"],
                &entry["from_lifecycle"],
                &entry["to_lifecycle"],
                &entry["unit_id"],
                &entry["unit_kind"],
                &entry["note"],
            ),
            (
                &json!(action),
                &json!(actor),
                &from,
                &to,
                &unit,
                &kind,
                &note
            ),
            "{entry}"
        );
    }
    assert_eq!(
        entries[4]["at"], entries[5]["at"],
        "at quorum 0 the submit and its apply share one instant"
    );
    for limit in [1, 2, 3] {
        assert_eq!(walk(&f, limit).await, entries, "limit {limit}");
    }
}

/// The history of a SKU the caller's tenant does not hold, of an unknown id and of a deleted draft
/// is 404; the read takes `limit` and `cursor` (and their `$` spellings) only, and a cursor from
/// another SKU's history is refused.
#[tokio::test]
async fn the_history_is_404_off_the_tenant_and_takes_only_its_page_keys() {
    let f = Fixture::new(1).await;
    let foreign = authed_ctx(Uuid::new_v4());
    assert_eq!(history_as(&f, &foreign, f.id, "").await.0, 404);
    assert_eq!(history_as(&f, &f.author, Uuid::new_v4(), "").await.0, 404);
    for query in [
        "?%24filter=action%20eq%20%27sku.create%27",
        "?%24orderby=written_at%20desc",
        "?%24select=action",
        "?%24count=true",
        "?bogus=1",
        "?limit=0",
        "?limit=x",
        "?cursor=garbage",
        "?limit=1&limit=2",
    ] {
        let (status, b) = history_as(&f, &f.author, f.id, query).await;
        assert_eq!(status, 400, "{query}: {b}");
    }
    let (status, page) = history_as(&f, &f.author, f.id, "?%24top=1").await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(
        page["page_info"]["next_cursor"].is_null(),
        "one row: {page}"
    );
    // A second SKU with two rows gives a cursor; it is not this SKU's.
    let (status, s) = call(
        &f.app,
        &f.author,
        Method::POST,
        "/skus",
        json!({"code":"GONE","name":"Gone","type":"recurring"}),
        None,
    )
    .await;
    assert_eq!(status, 201, "{s}");
    let gone = Uuid::parse_str(s["id"].as_str().unwrap()).unwrap();
    let (status, _, b) = call_with(
        &f.app,
        &f.author,
        Method::PATCH,
        &format!("/skus/{gone}"),
        json!({"description":"x"}),
        &[("If-Match", "\"1\"".to_owned())],
    )
    .await;
    assert_eq!(status, 200, "{b}");
    let (status, page) = history_as(&f, &f.author, gone, "?limit=1").await;
    assert_eq!(status, 200, "{page}");
    let cursor = page["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, b) = history_as(&f, &f.author, f.id, &format!("?cursor={cursor}")).await;
    assert_eq!(
        (status, problem_code(&b).as_str()),
        (400, "FILTER_MISMATCH"),
        "{b}"
    );
    assert_eq!(
        history_as(&f, &f.author, gone, &format!("?cursor={cursor}"))
            .await
            .0,
        200
    );
    // Deleted, its history is gone with it.
    let (status, _, b) = call_with(
        &f.app,
        &f.author,
        Method::DELETE,
        &format!("/skus/{gone}"),
        json!({}),
        &[("If-Match", "\"2\"".to_owned())],
    )
    .await;
    assert_eq!(status, 204, "{b}");
    assert_eq!(history_as(&f, &f.author, gone, "").await.0, 404);
}

/// B-1: a change's `note` is the reason its submitter gave, and the history shows it on the
/// change's `approval.submit` row. A change without one, a publish and the apply carry none.
#[tokio::test]
async fn a_changes_note_reaches_its_submit_row_in_the_history() {
    let f = Fixture::new(1).await;
    f.publish().await;
    let (status, b) = f
        .post("/changes", json!({"name":"Renamed","note":"raise for Q4"}))
        .await;
    assert_eq!((status, &b["applied"]), (200, &json!(true)), "{b}");
    let (status, b) = f.post("/changes", json!({"name":"Again"})).await;
    assert_eq!((status, &b["applied"]), (200, &json!(true)), "{b}");
    let entries = walk(&f, 200).await;
    let notes: Vec<(String, Value, Value)> = entries
        .iter()
        .filter(|e| e["unit_kind"].is_string())
        .map(|e| {
            (
                e["action"].as_str().unwrap().to_owned(),
                e["unit_kind"].clone(),
                e["note"].clone(),
            )
        })
        .collect();
    assert_eq!(
        notes,
        [
            (
                "approval.submit".to_owned(),
                json!("sku_publish"),
                Value::Null
            ),
            (
                "approval.applied".to_owned(),
                json!("sku_publish"),
                Value::Null
            ),
            (
                "approval.submit".to_owned(),
                json!("sku_change"),
                json!("raise for Q4")
            ),
            (
                "approval.applied".to_owned(),
                json!("sku_change"),
                Value::Null
            ),
            (
                "approval.submit".to_owned(),
                json!("sku_change"),
                Value::Null
            ),
            (
                "approval.applied".to_owned(),
                json!("sku_change"),
                Value::Null
            ),
        ],
        "{entries:#?}"
    );
}

/// A pending dated lifecycle is the outcome of the act that stored it. A later rename, a vote
/// that does not meet quorum, and a reject of that later unit record the lifecycle in force
/// (P-D-249). Quorum 0 applies the rename at submit (`sku_governance`).
#[tokio::test]
async fn a_later_act_does_not_record_a_pending_lifecycle_it_did_not_set() {
    let f = Fixture::new(0).await;
    f.publish().await;
    let date = time::OffsetDateTime::now_utc().date() + time::Duration::days(30);
    let (status, b) = f
        .post(
            "/changes",
            json!({"lifecycle":"deprecated","effective_from":date.to_string()}),
        )
        .await;
    assert_eq!((status, &b["applied"]), (200, &json!(true)), "{b}");
    let card = f.card().await;
    assert_eq!(card["lifecycle"], "published", "{card}");
    assert_eq!(card["lifecycle_next"]["lifecycle"], "deprecated", "{card}");

    let (status, b) = f
        .post(
            "/changes",
            json!({"name":"Renamed","effective_from":date.to_string()}),
        )
        .await;
    assert_eq!((status, &b["applied"]), (200, &json!(true)), "{b}");
    let card = f.card().await;
    assert_eq!(card["name"], "Renamed", "{card}");
    assert_eq!(card["lifecycle"], "published", "{card}");
    assert_eq!(card["lifecycle_next"]["lifecycle"], "deprecated", "{card}");

    f.policy(2).await;
    let (status, u) = f
        .post(
            "/changes",
            json!({"name":"Again","effective_from":date.to_string()}),
        )
        .await;
    assert_eq!(status, 200, "{u}");
    assert_eq!(u["applied"], false, "{u}");
    let (status, vote) = f.vote(&u, "approve", 1).await;
    assert_eq!(status, 200, "{vote}");
    assert_eq!(vote["outcome"], "pending", "{vote}");
    let generation = vote["unit"]["generation"].as_i64().unwrap();
    let (status, rejected) = call(
        &f.app,
        &f.author,
        Method::POST,
        &format!(
            "/approval-units/{}/reject",
            u["unit"]["id"].as_str().unwrap()
        ),
        json!({"generation":generation,"note":"no"}),
        None,
    )
    .await;
    assert_eq!(status, 200, "{rejected}");
    let card = f.card().await;
    assert_eq!(card["lifecycle"], "published", "{card}");
    assert_eq!(card["lifecycle_next"]["lifecycle"], "deprecated", "{card}");
    assert_eq!(card["name"], "Renamed", "{card}");

    let rows = moves(&f, f.id).await;
    assert!(
        rows.iter()
            .any(|row| row == "approval.vote sku_change published>published"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row == "approval.rejected sku_change published>published"),
        "{rows:#?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.as_str() == "approval.applied sku_change published>published")
            .count(),
        1,
        "{rows:#?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.ends_with(">deprecated"))
            .cloned()
            .collect::<Vec<_>>(),
        ["approval.applied sku_change published>deprecated"],
        "{rows:#?}"
    );
}
