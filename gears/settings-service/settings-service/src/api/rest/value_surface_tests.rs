// Created: 2026-09-17 by Virtuozzo International GmbH
//! The write surface driven as a client drives it.
//!
//! Validate then set: a value takes effect when the caller sets it, there is
//! no pending state and no separate activation. What a request has to
//! demonstrate is the order of the two gates, the refusals that keep one
//! administrator from overwriting another, the per-item independence of a
//! batch, and that a secret leaves the settings row on the way in and never
//! comes back on the way out.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::resolution::MASK_TOKEN;
use crate::test_support::{RestHarness, SECRET};

/// The value surface of one setting.
fn value_of(h: &RestHarness, name: &str) -> String {
    let key = h.inner.key(name).to_string().replace('~', "%7E");
    format!("/settings-service/v1/settings/{key}/value")
}

/// The setting's own path, for the read that follows a write.
fn setting(h: &RestHarness, name: &str) -> String {
    let key = h.inner.key(name).to_string().replace('~', "%7E");
    format!("/settings-service/v1/settings/{key}")
}

const BATCH: &str = "/settings-service/v1/settings/batch";

// ── Set ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_set_takes_effect_at_once_and_reports_both_images() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let uri = value_of(&h, "proxy");

    let answer = h
        .send(
            "PUT",
            &uri,
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["operation"], json!("create"));
    assert!(
        answer.body.get("old_value").is_none(),
        "a first write has no image before: {}",
        answer.body
    );
    assert_eq!(answer.body["new_value"], json!(false));
    assert!(!answer.body["masked"].as_bool().unwrap_or(true));

    // And it is what the read answers, with no apply step in between.
    let (status, body) = h.get(&setting(&h, "proxy"), h.inner.tree.root).await;
    assert_eq!(status, 200);
    assert_eq!(body["value"], json!(false));
    assert_eq!(body["source"], json!("own_override"));
}

#[tokio::test]
async fn a_second_set_carries_the_image_before_it() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let uri = value_of(&h, "proxy");
    let first = h
        .send(
            "PUT",
            &uri,
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    let tag = first.body["etag"].as_str().expect("a tag").to_owned();

    let answer = h
        .send(
            "PUT",
            &uri,
            Some(json!({ "value": true })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["operation"], json!("change"));
    assert_eq!(answer.body["old_value"], json!(false));
    assert_eq!(answer.body["new_value"], json!(true));
}

#[tokio::test]
async fn a_value_that_does_not_validate_is_refused_before_anything_is_stored() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let answer = h
        .send(
            "PUT",
            &value_of(&h, "proxy"),
            Some(json!({ "value": "not a boolean" })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);

    let (_, body) = h.get(&setting(&h, "proxy"), h.inner.tree.root).await;
    assert_eq!(
        body["source"],
        json!("schema_default"),
        "a setting that fails leaves no value behind"
    );
}

#[tokio::test]
async fn a_set_is_conditional_on_the_state_the_caller_read() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let uri = value_of(&h, "proxy");
    let body = Some(json!({ "value": false }));

    let answer = h
        .send("PUT", &uri, body.clone(), None, h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 428, "{}", answer.body);

    let answer = h
        .send(
            "PUT",
            &uri,
            body.clone(),
            Some("a tag from another time"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 412, "{}", answer.body);

    // The write lands, and the absent-state tag is stale from then on.
    h.send("PUT", &uri, body.clone(), Some("absent"), h.inner.tree.root)
        .await;
    let answer = h
        .send("PUT", &uri, body, Some("absent"), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 412, "{}", answer.body);
}

#[tokio::test]
async fn a_tenant_scoped_write_to_a_global_setting_is_refused_409() {
    // Scope class is stronger than any grant: a `global` setting has no
    // tenant-scoped value for anyone to write.
    let h = RestHarness::new().await;
    h.inner
        .declare("platform_wide", "global", json!(true))
        .await;

    let answer = h
        .send(
            "PUT",
            &format!(
                "{}?tenant={}",
                value_of(&h, "platform_wide"),
                h.inner.tree.a
            ),
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
}

#[tokio::test]
async fn a_write_to_a_setting_that_is_not_declared_is_reported_absent() {
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "PUT",
            &value_of(&h, "never_declared"),
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 404, "{}", answer.body);
}

// ── The second gate ──────────────────────────────────────────────────────────

#[tokio::test]
async fn an_interactive_write_to_a_gated_setting_needs_a_recent_re_authentication() {
    let h = RestHarness::stale_step_up().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let answer = h
        .send(
            "PUT",
            &value_of(&h, "proxy"),
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 401, "{}", answer.body);
    let challenge = answer
        .headers
        .get("www-authenticate")
        .expect("the RFC 9470 challenge");
    assert!(
        challenge.contains("insufficient_user_authentication"),
        "{challenge}"
    );
    assert!(challenge.contains("max_age="), "{challenge}");
}

// ── Revert and remove ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_revert_clears_the_scopes_own_row_and_reports_what_it_falls_back_to() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, h.inner.tree.root, json!(false)).await;
    h.inner.set(id, h.inner.tree.a, json!(true)).await;

    let read = h
        .send(
            "GET",
            &format!("{}?tenant={}", setting(&h, "proxy"), h.inner.tree.a),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    let tag = read.body["etag"].as_str().expect("a tag").to_owned();

    let answer = h
        .send(
            "POST",
            &format!("{}/revert?tenant={}", value_of(&h, "proxy"), h.inner.tree.a),
            None,
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["change"]["operation"], json!("revert"));
    assert_eq!(
        answer.body["effective"]["value"],
        json!(false),
        "the nearest ancestor's override is what it falls back to"
    );
    assert_eq!(answer.body["effective"]["source"], json!("inherited"));
}

#[tokio::test]
async fn a_revert_where_the_scope_holds_no_override_is_reported_absent() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let answer = h
        .send(
            "POST",
            &format!("{}/revert", value_of(&h, "proxy")),
            None,
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 404, "{}", answer.body);
}

#[tokio::test]
async fn a_clone_copies_the_source_scopes_value() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, h.inner.tree.a, json!(false)).await;

    let answer = h
        .send(
            "POST",
            &format!("{}/clone?tenant={}", value_of(&h, "proxy"), h.inner.tree.b),
            Some(json!({ "from": h.inner.tree.a })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["new_value"], json!(false));
    assert_eq!(answer.body["tenant_id"], json!(h.inner.tree.b.to_string()));
}

#[tokio::test]
async fn a_remove_deletes_the_scopes_row_and_reports_the_fallback() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, h.inner.tree.root, json!(false)).await;
    let read = h.get(&setting(&h, "proxy"), h.inner.tree.root).await;
    let tag = read.1["etag"].as_str().expect("a tag").to_owned();

    let answer = h
        .send(
            "DELETE",
            &value_of(&h, "proxy"),
            None,
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["change"]["operation"], json!("remove"));
    assert_eq!(
        answer.body["effective"]["source"],
        json!("schema_default"),
        "with no ancestor override left, the declaration's own default stands"
    );
    assert_eq!(answer.body["effective"]["value"], json!(true));
}

#[tokio::test]
async fn a_committed_write_carries_its_tag_in_the_etag_header() {
    // The header is what a conditional client reads; the body field exists for
    // a batch, where there is no one header to carry them all.
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let answer = h
        .send(
            "PUT",
            &value_of(&h, "proxy"),
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    let header = answer.etag.as_deref().expect("an ETag header");
    assert_eq!(Some(header), answer.body["etag"].as_str());

    // And it is the tag the next write presents.
    let next = h
        .send(
            "PUT",
            &value_of(&h, "proxy"),
            Some(json!({ "value": true })),
            Some(header),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(next.status, 200, "{}", next.body);
}

// ── Preview, which stores nothing ────────────────────────────────────────────

#[tokio::test]
async fn validate_reports_the_verdict_and_changes_nothing() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let uri = format!(
        "/settings-service/v1/settings/{}/validate",
        h.inner.key("proxy").to_string().replace('~', "%7E")
    );

    let ok = h
        .send(
            "POST",
            &uri,
            Some(json!({ "value": false })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(ok.body["valid"], json!(true));
    assert_eq!(ok.body["violations"], json!([]));

    let bad = h
        .send(
            "POST",
            &uri,
            Some(json!({ "value": "not a boolean" })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(
        bad.status, 200,
        "an invalid candidate is a verdict, not a refusal"
    );
    assert_eq!(bad.body["valid"], json!(false));
    assert!(
        !bad.body["violations"]
            .as_array()
            .expect("violations")
            .is_empty(),
        "and it says where: {}",
        bad.body
    );

    // Neither call stored anything.
    let (_, body) = h.get(&setting(&h, "proxy"), h.inner.tree.root).await;
    assert_eq!(body["source"], json!("schema_default"));
}

#[tokio::test]
async fn impact_names_the_descendants_a_candidate_would_change() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, h.inner.tree.root, json!(true)).await;
    let uri = format!(
        "/settings-service/v1/settings/{}/impact",
        h.inner.key("proxy").to_string().replace('~', "%7E")
    );

    let answer = h
        .send(
            "POST",
            &uri,
            Some(json!({ "value": false })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer.body["scanned"].as_u64().unwrap_or(0) > 0,
        "the walk reports what it examined: {}",
        answer.body
    );
}

// ── Batch ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_batch_commits_each_change_on_its_own_and_reports_each_outcome() {
    let h = RestHarness::new().await;
    h.inner.declare("first", "cascading", json!(true)).await;
    h.inner.declare("second", "cascading", json!(true)).await;

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": h.inner.key("first").to_string(), "value": false, "if_match": "absent" },
                { "key": h.inner.key("second").to_string(), "value": false, "if_match": "absent" },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let results = answer.body["results"].as_array().expect("per-item results");
    assert_eq!(results.len(), 2);
    for item in results {
        assert_eq!(item["outcome"], json!("committed"), "{item}");
    }
    assert!(
        answer.body["change_set_id"].is_string(),
        "one change set covers the press: {}",
        answer.body
    );
}

#[tokio::test]
async fn one_rejected_change_leaves_the_rest_of_the_batch_committed() {
    // The rule the Apply screen rests on: a batch is not atomic across its
    // items, and a failure is an outcome rather than a request error.
    let h = RestHarness::new().await;
    h.inner.declare("good", "cascading", json!(true)).await;
    h.inner.declare("bad", "cascading", json!(true)).await;

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": h.inner.key("good").to_string(), "value": false, "if_match": "absent" },
                { "key": h.inner.key("bad").to_string(), "value": "not a boolean", "if_match": "absent" },
                { "key": h.inner.key("good").to_string(), "value": true },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let results = answer.body["results"].as_array().expect("results");
    assert_eq!(results[0]["outcome"], json!("committed"));
    assert_eq!(results[1]["outcome"], json!("rejected"));
    assert_eq!(
        results[1]["error"],
        json!("invalid"),
        "from the closed vocabulary: {}",
        results[1]
    );
    assert_eq!(results[2]["outcome"], json!("rejected"));
    assert_eq!(
        results[2]["error"],
        json!("if_match_required"),
        "a change with no tag refuses on its own: {}",
        results[2]
    );
}

#[tokio::test]
async fn a_batch_mixing_a_set_and_a_revert_commits_both() {
    let h = RestHarness::new().await;
    let first = h.inner.declare("first", "cascading", json!(true)).await;
    h.inner.declare("second", "cascading", json!(true)).await;
    h.inner.set(first, h.inner.tree.root, json!(false)).await;
    let read = h.get(&setting(&h, "first"), h.inner.tree.root).await;
    let tag = read.1["etag"].as_str().expect("a tag").to_owned();

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": h.inner.key("first").to_string(), "op": "revert", "if_match": tag },
                { "key": h.inner.key("second").to_string(), "op": "set", "value": false, "if_match": "absent" },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let results = answer.body["results"].as_array().expect("results");
    assert_eq!(results[0]["outcome"], json!("committed"), "{}", results[0]);
    assert_eq!(results[0]["change"]["operation"], json!("revert"));
    assert_eq!(results[1]["outcome"], json!("committed"), "{}", results[1]);
    assert_eq!(results[1]["change"]["operation"], json!("create"));
}

#[tokio::test]
async fn a_revert_carrying_a_value_and_a_set_without_one_are_each_invalid() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": h.inner.key("proxy").to_string(), "op": "revert", "value": false, "if_match": "absent" },
                { "key": h.inner.key("proxy").to_string(), "op": "set", "if_match": "absent" },
                { "key": h.inner.key("proxy").to_string(), "op": "sideways", "value": false, "if_match": "absent" },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    for item in answer.body["results"].as_array().expect("results") {
        assert_eq!(item["outcome"], json!("rejected"), "{item}");
        assert_eq!(item["error"], json!("invalid"), "{item}");
    }
}

#[tokio::test]
async fn a_batch_verifies_step_up_once_for_the_whole_request() {
    // Not per item: a missing or stale proof refuses the request with the
    // challenge, and nothing is committed.
    let h = RestHarness::stale_step_up().await;
    h.inner.declare("first", "cascading", json!(true)).await;
    h.inner.declare("second", "cascading", json!(true)).await;

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": h.inner.key("first").to_string(), "value": false, "if_match": "absent" },
                { "key": h.inner.key("second").to_string(), "value": false, "if_match": "absent" },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 401, "{}", answer.body);
    assert!(
        answer.body.get("results").is_none(),
        "the whole request is refused, never a per-item code: {}",
        answer.body
    );

    let (_, body) = h.get(&setting(&h, "first"), h.inner.tree.root).await;
    assert_eq!(body["source"], json!("schema_default"), "nothing committed");
}

#[tokio::test]
async fn a_batch_past_the_limit_is_refused_before_anything_is_written() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let changes: Vec<Value> = (0..=crate::infra::value_writes::BATCH_LIMIT)
        .map(|_| json!({ "key": h.inner.key("proxy").to_string(), "value": false, "if_match": "absent" }))
        .collect();

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": changes })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}

// ── Secrets, across the write path ───────────────────────────────────────────

#[tokio::test]
async fn a_secret_leaves_the_settings_row_and_never_comes_back() {
    let h = RestHarness::new().await;
    h.inner
        .declare_typed("api_token", "cascading", json!(""), SECRET, "secret")
        .await;

    let answer = h
        .send(
            "PUT",
            &value_of(&h, "api_token"),
            Some(json!({ "value": "hunter2" })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(
        answer.body["new_value"],
        json!(MASK_TOKEN),
        "the response never echoes what was sent"
    );
    assert!(answer.body["masked"].as_bool().unwrap_or(false));
    assert_eq!(h.secrets.held().len(), 1, "the plaintext went to the store");

    let wire = answer.body.to_string();
    assert!(!wire.contains("hunter2"), "{wire}");
    for held in h.secrets.held() {
        assert!(
            !wire.contains(&held),
            "and the store reference is not returned either: {wire}"
        );
    }
}

#[tokio::test]
async fn a_secret_can_be_staged_ahead_of_the_step_up_redirect_and_adopted_by_its_token() {
    let h = RestHarness::new().await;
    h.inner
        .declare_typed("api_token", "cascading", json!(""), SECRET, "secret")
        .await;
    let staged = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/secret-stage",
                h.inner.key("api_token").to_string().replace('~', "%7E")
            ),
            Some(json!({ "value": "hunter2" })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(staged.status, 200, "{}", staged.body);
    let pending = staged.body["pending_id"]
        .as_str()
        .expect("a single-use token")
        .to_owned();
    assert!(staged.body["expires_at"].is_string());
    let wire = staged.body.to_string();
    assert!(!wire.contains("hunter2"), "{wire}");

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": h.inner.key("api_token").to_string(),
                  "value": { "pending_id": pending },
                  "if_match": "absent" },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let item = &answer.body["results"][0];
    assert_eq!(item["outcome"], json!("committed"), "{item}");
    assert_eq!(
        h.secrets.stores.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the staged entry is adopted, not stored a second time"
    );
}

#[tokio::test]
async fn a_pending_token_is_single_use() {
    let h = RestHarness::new().await;
    h.inner
        .declare_typed("api_token", "cascading", json!(""), SECRET, "secret")
        .await;
    let staged = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/secret-stage",
                h.inner.key("api_token").to_string().replace('~', "%7E")
            ),
            Some(json!({ "value": "hunter2" })),
            None,
            h.inner.tree.root,
        )
        .await;
    let pending = staged.body["pending_id"]
        .as_str()
        .expect("a token")
        .to_owned();
    let adopt = |tag: &str| {
        json!({ "changes": [
            { "key": h.inner.key("api_token").to_string(),
              "value": { "pending_id": pending.clone() },
              "if_match": tag },
        ]})
    };

    let first = h
        .send(
            "POST",
            BATCH,
            Some(adopt("absent")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(first.body["results"][0]["outcome"], json!("committed"));

    let again = h
        .send(
            "POST",
            BATCH,
            Some(adopt("absent")),
            None,
            h.inner.tree.root,
        )
        .await;
    let item = &again.body["results"][0];
    assert_eq!(item["outcome"], json!("rejected"), "{item}");
    assert_eq!(item["error"], json!("invalid"), "{item}");
}

#[tokio::test]
async fn staging_is_refused_on_a_declaration_that_is_not_a_secret() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let answer = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/secret-stage",
                h.inner.key("proxy").to_string().replace('~', "%7E")
            ),
            Some(json!({ "value": "hunter2" })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}

// ── What a commit publishes ──────────────────────────────────────────────────

#[tokio::test]
async fn a_commit_publishes_the_change_it_made() {
    use crate::domain::ports::ValueEvent;
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    h.send(
        "PUT",
        &value_of(&h, "proxy"),
        Some(json!({ "value": false })),
        Some("absent"),
        h.inner.tree.root,
    )
    .await;

    let events = h
        .published
        .events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ValueEvent::Changed { key, .. } if key.contains("proxy"))),
        "a durable commit is followed by its event: {events:?}"
    );
}

// ── Authorization ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_denied_caller_reaches_no_write() {
    let h = RestHarness::denying().await;
    let key = h.inner.key("proxy").to_string();
    let value = value_of(&h, "proxy");

    for (method, uri, body) in [
        ("PUT", value.clone(), Some(json!({ "value": false }))),
        ("POST", format!("{value}/revert"), None),
        ("DELETE", value.clone(), None),
        (
            "POST",
            format!("{value}/clone"),
            Some(json!({ "from": Uuid::new_v4() })),
        ),
        (
            "POST",
            BATCH.to_owned(),
            Some(json!({ "changes": [
                { "key": key, "value": false, "if_match": "absent" },
            ]})),
        ),
    ] {
        let answer = h
            .send(method, &uri, body, Some("absent"), h.inner.tree.root)
            .await;
        assert_eq!(answer.status, 403, "{method} {uri}: {}", answer.body);
    }
}

#[tokio::test]
async fn a_service_principal_is_refused_a_setting_that_requires_a_person() {
    // Step-up asks a human to re-authenticate. A machine has nobody to ask, so
    // the refusal is a flat 403 rather than a challenge it could never answer.
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let answer = h
        .send_as(
            "PUT",
            &value_of(&h, "proxy"),
            Some(json!({ "value": false })),
            Some("absent"),
            crate::test_support::service_context_for(h.inner.tree.root),
        )
        .await;
    assert_eq!(answer.status, 403, "{}", answer.body);
    assert!(
        !answer.headers.contains_key("www-authenticate"),
        "and no challenge: there is no re-authentication to offer"
    );
}

#[tokio::test]
async fn validate_of_a_cascading_setting_carries_the_impact_the_change_would_have() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, h.inner.tree.root, json!(true)).await;

    let answer = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/validate",
                h.inner.key("proxy").to_string().replace('~', "%7E")
            ),
            Some(json!({ "value": false })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let impact = &answer.body["impact"];
    assert!(
        !impact.is_null(),
        "a cascading setting reaches further: {}",
        answer.body
    );
    assert!(impact["scanned"].as_u64().unwrap_or(0) > 0, "{impact}");
    assert_eq!(answer.body["effective"]["value"], json!(true));
}

#[tokio::test]
async fn a_batch_entry_naming_an_undeclared_setting_is_rejected_alone() {
    let h = RestHarness::new().await;
    h.inner.declare("declared", "cascading", json!(true)).await;

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": h.inner.key("never_declared").to_string(), "value": false, "if_match": "absent" },
                { "key": h.inner.key("declared").to_string(), "value": false, "if_match": "absent" },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let results = answer.body["results"].as_array().expect("results");
    assert_eq!(results[0]["outcome"], json!("rejected"));
    assert_eq!(results[0]["error"], json!("not_found"), "{}", results[0]);
    assert_eq!(results[1]["outcome"], json!("committed"), "{}", results[1]);
}

#[tokio::test]
async fn a_batch_entry_whose_tag_is_stale_is_rejected_alone() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("first", "cascading", json!(true)).await;
    h.inner.declare("second", "cascading", json!(true)).await;
    h.inner.set(id, h.inner.tree.root, json!(false)).await;

    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": h.inner.key("first").to_string(), "value": true, "if_match": "absent" },
                { "key": h.inner.key("second").to_string(), "value": false, "if_match": "absent" },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    let results = answer.body["results"].as_array().expect("results");
    assert_eq!(results[0]["outcome"], json!("rejected"));
    assert_eq!(
        results[0]["error"],
        json!("stale"),
        "a row exists, so the absent-state tag is not the state that was read: {}",
        results[0]
    );
    assert_eq!(results[1]["outcome"], json!("committed"), "{}", results[1]);
}

// ── The text guard ───────────────────────────────────────────────────────────

/// A literal a double cannot hold is refused as written, before the value is
/// parsed, gated or validated against its type — so a `bool` setting reports
/// `value_not_canonical`, not a type mismatch.
#[tokio::test]
async fn a_number_finer_than_a_double_is_refused_rather_than_rounded() {
    let h = RestHarness::new().await;
    h.inner.declare("ratio", "cascading", json!(true)).await;

    for (method, uri) in [
        ("PUT", value_of(&h, "ratio")),
        ("POST", format!("{}/validate", setting(&h, "ratio"))),
        ("POST", format!("{}/impact", setting(&h, "ratio"))),
    ] {
        let answer = h
            .send_text(
                method,
                &uri,
                r#"{"value": 0.10000000000000000555}"#,
                Some("absent"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(answer.status, 400, "{method} {uri}: {}", answer.body);
        assert!(
            answer.body.to_string().contains("value_not_canonical"),
            "{method} {uri}: {}",
            answer.body
        );
    }

    let (_, body) = h.get(&setting(&h, "ratio"), h.inner.tree.root).await;
    assert_eq!(
        body["source"],
        json!("schema_default"),
        "nothing was stored"
    );
}

#[tokio::test]
async fn a_batch_entry_the_text_guard_refuses_is_rejected_alone() {
    let h = RestHarness::new().await;
    h.inner.declare("first", "cascading", json!(true)).await;
    h.inner.declare("second", "cascading", json!(true)).await;
    let body = format!(
        r#"{{"changes": [
            {{"key": "{first}", "value": 9007199254740993.0, "if_match": "absent"}},
            {{"key": "{second}", "value": false, "if_match": "absent"}}
        ]}}"#,
        first = h.inner.key("first"),
        second = h.inner.key("second"),
    );

    let answer = h
        .send_text("POST", BATCH, &body, None, h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let results = answer.body["results"].as_array().expect("results");
    assert_eq!(results[0]["outcome"], json!("rejected"), "{}", results[0]);
    assert_eq!(results[0]["error"], json!("invalid"), "{}", results[0]);
    assert!(
        results[0]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("round trip")),
        "{}",
        results[0]
    );
    assert_eq!(results[1]["outcome"], json!("committed"), "{}", results[1]);
}

// ── The tenant boundary ──────────────────────────────────────────────────────

/// The boundary a write may not cross is enforced by the gate, not by a PDP
/// constraint on the value resource: `c` is the caller's sibling, `s` a
/// standalone descendant, and every write form is refused for both.
#[tokio::test]
async fn a_target_outside_the_callers_subtree_is_refused_on_every_write() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let caller = h.inner.tree.a;

    for outside in [h.inner.tree.c, h.inner.tree.s] {
        let value = format!("{}?tenant={outside}", value_of(&h, "proxy"));
        let requests: [(&str, String, Option<Value>); 6] = [
            ("PUT", value.clone(), Some(json!({ "value": false }))),
            ("DELETE", value.clone(), None),
            (
                "POST",
                format!(
                    "{value_of}/revert?tenant={outside}",
                    value_of = value_of(&h, "proxy")
                ),
                None,
            ),
            (
                "POST",
                format!("{}/clone?tenant={outside}", value_of(&h, "proxy")),
                Some(json!({ "from": h.inner.tree.root })),
            ),
            (
                "POST",
                format!("{}/validate?tenant={outside}", setting(&h, "proxy")),
                Some(json!({ "value": false })),
            ),
            (
                "POST",
                format!("{}/impact?tenant={outside}", setting(&h, "proxy")),
                Some(json!({ "value": false })),
            ),
        ];
        for (method, uri, body) in requests {
            let answer = h.send(method, &uri, body, Some("absent"), caller).await;
            assert_eq!(answer.status, 403, "{method} {uri}: {}", answer.body);
        }
    }

    let (_, body) = h.get(&setting(&h, "proxy"), h.inner.tree.root).await;
    assert_eq!(
        body["source"],
        json!("schema_default"),
        "nothing was stored"
    );
}
