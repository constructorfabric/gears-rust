//! P-D-225: every text a request writes has an explicit length cap (twin of pricing D-457). A
//! longer text is 400 `FIELD_TOO_LONG` on its field and writes nothing; a note keeps
//! `NOTE_TOO_LONG`. The caps count characters (Unicode scalar values).
//!
//! A child of the governance suite, so it drives the real doors through its `Fixture`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use axum::http::Method;

/// The code of the violation on `subject`, as the problem lists it.
fn code_on(body: &Value, subject: &str) -> Option<String> {
    body["context"]["violations"]
        .as_array()?
        .iter()
        .find(|v| v["subject"] == json!(subject))
        .and_then(|v| v["type"].as_str())
        .map(ToOwned::to_owned)
}

/// A text one character over `max`.
fn over(max: usize) -> String {
    "x".repeat(max + 1)
}

/// The SKU fields a write carries, with their caps.
const SKU_FIELDS: &[(&str, usize)] = &[
    ("name", 200),
    ("description", 2000),
    ("gl_code", 64),
    ("tax_category", 64),
    ("invoice_line_template", 2000),
    ("usage_type_ref", 512),
    ("unit", 64),
];

/// RS-10, RS-11, RS-38: each text of the SKU, category and release doors is 400
/// `FIELD_TOO_LONG` on its field one character over its cap, and nothing is written.
#[tokio::test]
async fn every_text_a_request_writes_has_a_length_cap() {
    let f = Fixture::new(0).await;
    let before = f.card().await;
    // POST /skus: the code too.
    for (field, max) in SKU_FIELDS.iter().chain(&[("code", 64)]) {
        let mut body = json!({"code":"LONG","name":"Long","type":"usage"});
        body[*field] = json!(over(*max));
        let (status, b) = call(&f.app, &f.author, Method::POST, "/skus", body, None).await;
        assert_eq!(status, 400, "create {field}: {b}");
        assert_eq!(
            code_on(&b, field).as_deref(),
            Some("FIELD_TOO_LONG"),
            "create {field}: {b}"
        );
    }
    // PATCH /skus/{id} on the draft.
    for (field, max) in SKU_FIELDS {
        let tag = f.etag().await;
        let (status, _, b) = call_with(
            &f.app,
            &f.author,
            Method::PATCH,
            &format!("/skus/{}", f.id),
            json!({ *field: over(*max) }),
            &[("If-Match", tag)],
        )
        .await;
        assert_eq!(status, 400, "patch {field}: {b}");
        assert_eq!(
            code_on(&b, field).as_deref(),
            Some("FIELD_TOO_LONG"),
            "patch {field}: {b}"
        );
    }
    assert_eq!(f.card().await, before, "a refused write changes nothing");
    // POST /skus/{id}/changes on the published SKU.
    f.publish().await;
    for (field, max) in SKU_FIELDS {
        let (status, b) = f.post("/changes", json!({ *field: over(*max) })).await;
        assert_eq!(status, 400, "change {field}: {b}");
        assert_eq!(
            code_on(&b, field).as_deref(),
            Some("FIELD_TOO_LONG"),
            "change {field}: {b}"
        );
    }
    // POST /categories and PATCH /categories/{id}.
    for (field, max) in [("code", 64), ("name", 200)] {
        let mut body = json!({"code":"long","name":"Long"});
        body[field] = json!(over(max));
        let (status, b) = call(&f.app, &f.author, Method::POST, "/categories", body, None).await;
        assert_eq!(status, 400, "category {field}: {b}");
        assert_eq!(code_on(&b, field).as_deref(), Some("FIELD_TOO_LONG"), "{b}");
    }
    let (status, c) = call(
        &f.app,
        &f.author,
        Method::POST,
        "/categories",
        json!({"code":"short","name":"Short"}),
        None,
    )
    .await;
    assert_eq!(status, 201, "{c}");
    let (status, headers, _) = call_with(
        &f.app,
        &f.author,
        Method::GET,
        &format!("/categories/{}", c["id"].as_str().unwrap()),
        json!({}),
        &[],
    )
    .await;
    assert_eq!(status, 200);
    let (status, _, b) = call_with(
        &f.app,
        &f.author,
        Method::PATCH,
        &format!("/categories/{}", c["id"].as_str().unwrap()),
        json!({"name": over(200)}),
        &[("If-Match", headers["etag"].to_str().unwrap().to_owned())],
    )
    .await;
    assert_eq!(status, 400, "category rename: {b}");
    assert_eq!(
        code_on(&b, "name").as_deref(),
        Some("FIELD_TOO_LONG"),
        "{b}"
    );
    // DELETE /references/{id}: the operator's reason.
    let (status, r) = f.reserve(Uuid::new_v4()).await;
    assert_eq!(status, 201, "{r}");
    let (status, b) = call(
        &f.app,
        &f.author,
        Method::DELETE,
        &format!("/references/{}", r["reservation_id"].as_str().unwrap()),
        json!({"force":true,"reason": over(2000)}),
        None,
    )
    .await;
    assert_eq!(status, 400, "release reason: {b}");
    assert_eq!(
        code_on(&b, "reason").as_deref(),
        Some("FIELD_TOO_LONG"),
        "{b}"
    );
    let (status, refs) = call(
        &f.app,
        &f.author,
        Method::GET,
        &format!("/skus/{}/references", f.id),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, 200, "{refs}");
    assert!(refs.to_string().contains("reserved"), "still live: {refs}");
}

/// The caps count characters: a text of two-byte characters at the cap passes every door.
#[tokio::test]
async fn a_text_at_its_cap_in_two_byte_characters_passes() {
    let f = Fixture::new(0).await;
    let at = |max: usize| "\u{e9}".repeat(max);
    let (status, b) = call(
        &f.app,
        &f.author,
        Method::POST,
        "/skus",
        json!({
            "code": at(64),
            "name": at(200),
            "type": "recurring",
            "description": at(2000),
            "gl_code": at(64),
            "tax_category": at(64),
            "invoice_line_template": at(2000),
        }),
        None,
    )
    .await;
    assert_eq!(status, 201, "{b}");
    let (status, b) = call(
        &f.app,
        &f.author,
        Method::POST,
        "/categories",
        json!({"code": at(64), "name": at(200)}),
        None,
    )
    .await;
    assert_eq!(status, 201, "{b}");
}

/// RS-37: the vote note is capped by the approval engine (W1a, X-01) and answered 400
/// `NOTE_TOO_LONG` on `note` on approve and on reject, with nothing written.
#[tokio::test]
async fn a_vote_note_longer_than_2000_characters_is_note_too_long() {
    let f = Fixture::new(1).await;
    let (status, u) = f.post("/submit", json!({})).await;
    assert_eq!(status, 200, "{u}");
    for action in ["approve", "reject"] {
        let (status, b) = call(
            &f.app,
            &f.reviewer,
            Method::POST,
            &format!(
                "/approval-units/{}/{action}",
                u["unit"]["id"].as_str().unwrap()
            ),
            json!({"generation":1,"note": over(2000)}),
            None,
        )
        .await;
        assert_eq!(status, 400, "{action}: {b}");
        assert_eq!(code_on(&b, "note").as_deref(), Some("NOTE_TOO_LONG"), "{b}");
    }
    let (_, card) = call(
        &f.app,
        &f.reviewer,
        Method::GET,
        &format!("/approval-units/{}", u["unit"]["id"].as_str().unwrap()),
        json!({}),
        None,
    )
    .await;
    assert_eq!(card["state"], "pending");
    assert_eq!(card["decisions"], json!([]));
}
