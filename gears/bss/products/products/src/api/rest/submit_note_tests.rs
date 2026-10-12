//! P-D-219: the submitter's note travels with the approval unit. `POST /skus/{id}/submit`,
//! `/changes` and `/retire` take an optional `note`; it is stored on the unit (`submit_note`),
//! every unit read carries it, and the submit's audit row keeps it for the history (P-D-213). It is
//! not content: neither the snapshot nor its fingerprint carries it.
//!
//! A child of the governance suite, so it drives the real doors through its `Fixture`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use axum::http::Method;

/// The longest note the three doors take, in characters (Unicode scalar values).
const LONGEST: usize = 2000;

/// `GET /approval-units/{id}`.
async fn unit_card(f: &Fixture, unit: &Value) -> Value {
    let id = unit["unit"]["id"].as_str().unwrap();
    let (status, b) = call(
        &f.app,
        &f.reviewer,
        Method::GET,
        &format!("/approval-units/{id}"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, 200, "{b}");
    b
}

/// The unit as `GET /approval-units?ref_id=` lists it, over every page (P-D-224).
async fn unit_listed(f: &Fixture, unit: &Value) -> Value {
    let listed = f.all_units(&format!("ref_id={}", f.id)).await;
    listed
        .iter()
        .find(|item| item["id"] == unit["unit"]["id"])
        .cloned()
        .unwrap_or_else(|| panic!("the unit is listed: {listed:?}"))
}

/// The note of the unit on its receipt, its card and its list entry, which must agree.
async fn note_on_every_read(f: &Fixture, unit: &Value) -> Value {
    let receipt = unit["unit"]["submit_note"].clone();
    assert!(
        unit["unit"]
            .as_object()
            .unwrap()
            .contains_key("submit_note"),
        "the field is always there: {unit}"
    );
    assert_eq!(unit_card(f, unit).await["submit_note"], receipt);
    assert_eq!(unit_listed(f, unit).await["submit_note"], receipt);
    receipt
}

/// Every `approval.submit` row of the SKU's history: its unit kind and its note.
async fn submit_rows(f: &Fixture) -> Vec<(Value, Value)> {
    let (status, page) = call(
        &f.app,
        &f.author,
        Method::GET,
        &format!("/skus/{}/history?limit=200", f.id),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, 200, "{page}");
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "approval.submit")
        .map(|e| (e["unit_kind"].clone(), e["note"].clone()))
        .collect()
}

/// A vote as the independent reviewer at the unit's generation.
async fn approve(f: &Fixture, unit: &Value) {
    let (status, b) = f.vote(unit, "approve", 1).await;
    assert_eq!(status, 200, "{b}");
}

/// Each of the three doors stores its note on the unit: the receipt, the card and the list carry
/// it, it stays after the unit is decided, and the submit's history row shows the same words.
#[tokio::test]
async fn each_submit_door_stores_its_note_on_the_unit_and_on_its_history_row() {
    let f = Fixture::new(1).await;
    let (status, publish) = f.post("/submit", json!({"note":"first release"})).await;
    assert_eq!(
        (status, &publish["applied"]),
        (200, &json!(false)),
        "{publish}"
    );
    assert_eq!(note_on_every_read(&f, &publish).await, "first release");
    approve(&f, &publish).await;
    assert_eq!(unit_card(&f, &publish).await["state"], "approved");
    assert_eq!(note_on_every_read(&f, &publish).await, "first release");

    let (status, change) = f
        .post("/changes", json!({"name":"Renamed","note":"raise for Q4"}))
        .await;
    assert_eq!(status, 200, "{change}");
    assert_eq!(note_on_every_read(&f, &change).await, "raise for Q4");
    approve(&f, &change).await;

    let (status, retire) = f.post("/retire", json!({"note":"  end of life\n"})).await;
    assert_eq!(status, 200, "{retire}");
    assert_eq!(
        note_on_every_read(&f, &retire).await,
        "  end of life\n",
        "stored as sent"
    );
    let (status, b) = call(
        &f.app,
        &f.author,
        Method::POST,
        &format!(
            "/approval-units/{}/withdraw",
            retire["unit"]["id"].as_str().unwrap()
        ),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(unit_card(&f, &retire).await["state"], "withdrawn");
    assert_eq!(note_on_every_read(&f, &retire).await, "  end of life\n");

    assert_eq!(
        submit_rows(&f).await,
        [
            (json!("sku_publish"), json!("first release")),
            (json!("sku_change"), json!("raise for Q4")),
            (json!("sku_retire"), json!("  end of life\n")),
        ]
    );
}

/// A submit without a note — no body, `{}`, or `note: null` — stores none: every read answers
/// `submit_note: null`, and the history row's note is null.
#[tokio::test]
async fn a_submit_without_a_note_reads_null() {
    let f = Fixture::new(0).await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/bss-products/v1/skus/{}/submit", f.id))
                .extension(f.author.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let publish = body_json(response).await;
    assert_eq!(note_on_every_read(&f, &publish).await, Value::Null);
    let (status, change) = f.post("/changes", json!({"name":"A","note":null})).await;
    assert_eq!(status, 200, "{change}");
    assert_eq!(note_on_every_read(&f, &change).await, Value::Null);
    let (status, retire) = f.post("/retire", json!({})).await;
    assert_eq!(status, 200, "{retire}");
    assert_eq!(note_on_every_read(&f, &retire).await, Value::Null);
    assert_eq!(
        submit_rows(&f).await,
        [
            (json!("sku_publish"), Value::Null),
            (json!("sku_change"), Value::Null),
            (json!("sku_retire"), Value::Null),
        ]
    );
}

/// The three doors take a note of at most 2000 characters (counted as Unicode scalar values, so
/// 2000 two-byte `é` pass); a longer one is 400 `NOTE_TOO_LONG` on `note` and writes nothing. The
/// submit and retire bodies still refuse any other field, and a note that is not a string.
#[tokio::test]
async fn a_note_longer_than_2000_characters_is_note_too_long_on_all_three_doors() {
    let f = Fixture::new(0).await;
    let too_long = "x".repeat(LONGEST + 1);
    let longest = "\u{e9}".repeat(LONGEST);
    for (suffix, extra) in [
        ("/submit", json!({})),
        ("/changes", json!({"name":"Renamed"})),
        ("/retire", json!({})),
    ] {
        let mut body = extra.clone();
        body["note"] = json!(too_long);
        let (status, b) = f.post(suffix, body).await;
        assert_eq!(status, 400, "{suffix}: {b}");
        assert_eq!(problem_code(&b), "NOTE_TOO_LONG", "{suffix}: {b}");
        assert!(violation_for(&b, "note").is_some(), "{suffix}: {b}");
        let mut body = extra;
        body["note"] = json!(longest);
        let (status, b) = f.post(suffix, body).await;
        assert_eq!(status, 200, "{suffix}: {b}");
        assert_eq!(b["unit"]["submit_note"], json!(longest), "{suffix}");
    }
    // Only the three accepted submits wrote a unit and a history row.
    let rows = submit_rows(&f).await;
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows.iter().all(|(_, note)| note == &json!(longest)));
    for (suffix, body) in [
        ("/submit", json!({"notes":"typo"})),
        ("/retire", json!({"reason":"typo"})),
        ("/retire", json!({"note":5})),
    ] {
        let (status, b) = f.post(suffix, body).await;
        assert_eq!(status, 400, "{suffix}: {b}");
    }
}

/// The unit's `snapshot_hash`, as stored.
async fn stored_hash(f: &Fixture, unit: &Value) -> String {
    let id = Uuid::parse_str(unit["unit"]["id"].as_str().unwrap()).unwrap();
    raw_string_opt(
        &f.dsn,
        &format!(
            "SELECT snapshot_hash AS v FROM products_approval_unit WHERE {}",
            id_matches("id", id)
        ),
    )
    .await
    .unwrap()
}

/// A note is not content: three change submits of the same patch that differ only by their note
/// (two notes and none; each withdrawn to free the SKU) record the same snapshot and the same
/// fingerprint, and the note is not in the snapshot.
#[tokio::test]
async fn two_submits_that_differ_only_by_their_note_fingerprint_alike() {
    let f = Fixture::new(0).await;
    f.publish().await;
    f.policy(1).await;
    let mut seen = Vec::new();
    for body in [
        json!({"name":"Same","note":"one reason"}),
        json!({"name":"Same","note":"another reason"}),
        json!({"name":"Same"}),
    ] {
        let (status, u) = f.post("/changes", body.clone()).await;
        assert_eq!(status, 200, "{u}");
        assert_eq!(u["unit"]["submit_note"], body["note"]);
        seen.push((stored_hash(&f, &u).await, u["unit"]["snapshot"].clone()));
        let (status, b) = call(
            &f.app,
            &f.author,
            Method::POST,
            &format!(
                "/approval-units/{}/withdraw",
                u["unit"]["id"].as_str().unwrap()
            ),
            json!({}),
            None,
        )
        .await;
        assert_eq!(status, 200, "{b}");
    }
    assert_eq!(seen[1], seen[0], "a note is not content");
    assert_eq!(seen[2], seen[0], "no note is the same content");
    assert!(
        !seen[0].1.to_string().contains("one reason"),
        "{}",
        seen[0].1
    );
}

/// A stale refresh rewrites the unit's items, snapshot and fingerprint and keeps its note.
#[tokio::test]
async fn a_stale_refresh_keeps_the_submitters_note() {
    let f = Fixture::new(0).await;
    f.publish().await;
    f.policy(1).await;
    let (status, u) = f
        .post("/changes", json!({"gl_code":"4012","note":"new ledger"}))
        .await;
    assert_eq!(status, 200, "{u}");
    let before = stored_hash(&f, &u).await;
    let (db, scope) = repo_connection(&f.dsn, f.tenant).await;
    let conn = db.conn().unwrap();
    let mut c = bss_products_sdk::models::SkuContent::from(
        &repo::find_sku(&conn, &scope, f.tenant, f.id)
            .await
            .unwrap()
            .unwrap(),
    );
    c.description = "drift".into();
    repo::write_sku_content(
        &conn,
        &scope,
        f.tenant,
        f.id,
        &c,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let (status, body) = f.vote(&u, "approve", 1).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(problem_code(&body), "UNIT_STALE");
    let card = unit_card(&f, &u).await;
    assert_eq!(card["generation"], 2, "{card}");
    assert_ne!(stored_hash(&f, &u).await, before, "refreshed");
    assert_eq!(card["submit_note"], "new ledger", "{card}");
}
