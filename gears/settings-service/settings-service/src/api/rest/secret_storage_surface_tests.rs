//! A deployment with no Credential Store, driven as a client drives it.
//!
//! The store is optional. Without one every operation that needs secret
//! storage is refused with one error, a failed precondition and not an
//! outage, and refused before anything is written: no staged intent row, no
//! value row, nothing for the sweep to find. Everything that does not need the
//! store keeps working.

use sea_orm::EntityTrait;
use serde_json::{Value, json};
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::AccessScope;

use crate::domain::resolution::MASK_TOKEN;
use crate::infra::storage::entity::{pending_secret, setting_value};
use crate::test_support::{Answer, BOOL, RestHarness, SECRET};

const BATCH: &str = "/settings-service/v1/settings/batch";
const DECLARATIONS: &str = "/settings-service/v1/declarations";

fn key_path(h: &RestHarness, name: &str) -> String {
    h.inner.key(name).to_string().replace('~', "%7E")
}

fn value_of(h: &RestHarness, name: &str) -> String {
    format!("/settings-service/v1/settings/{}/value", key_path(h, name))
}

fn stage_of(h: &RestHarness, name: &str) -> String {
    format!(
        "/settings-service/v1/settings/{}/secret-stage",
        key_path(h, name)
    )
}

/// How many staged-secret intent rows and stored value rows exist.
async fn rows(h: &RestHarness) -> (u64, u64) {
    let conn = h.inner.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let pending = pending_secret::Entity::find()
        .secure()
        .scope_with(&scope)
        .count(&conn)
        .await
        .expect("pending rows");
    let values = setting_value::Entity::find()
        .secure()
        .scope_with(&scope)
        .count(&conn)
        .await
        .expect("value rows");
    (pending, values)
}

/// The refusal every secret operation answers without a store: a client
/// error, never the 503 a client would retry, saying what is missing.
fn assert_not_configured(answer: &Answer) {
    assert!(
        answer.status.is_client_error(),
        "a failed precondition, not an outage: {} {}",
        answer.status,
        answer.raw
    );
    assert!(
        answer.raw.contains("secret storage is not configured"),
        "{}",
        answer.raw
    );
}

async fn declare_secret(h: &RestHarness) {
    h.inner
        .declare_typed("api_token", "cascading", json!(""), SECRET, "secret")
        .await;
}

#[tokio::test]
async fn a_secret_set_is_refused_and_leaves_nothing_behind() {
    let h = RestHarness::new().await;
    declare_secret(&h).await;
    h.secrets.go_unconfigured();

    let answer = h
        .send(
            "PUT",
            &value_of(&h, "api_token"),
            Some(json!({ "value": "hunter2" })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;

    assert_not_configured(&answer);
    assert_eq!(rows(&h).await, (0, 0), "no intent row and no value row");
    assert!(!answer.raw.contains("hunter2"));
}

#[tokio::test]
async fn staging_a_secret_is_refused_and_leaves_no_intent_row() {
    let h = RestHarness::new().await;
    declare_secret(&h).await;
    h.secrets.go_unconfigured();

    let answer = h
        .send(
            "POST",
            &stage_of(&h, "api_token"),
            Some(json!({ "value": "hunter2" })),
            None,
            h.inner.tree.root,
        )
        .await;

    assert_not_configured(&answer);
    assert_eq!(rows(&h).await, (0, 0));
}

#[tokio::test]
async fn a_batch_adopting_a_staged_secret_is_refused_for_that_entry() {
    let h = RestHarness::new().await;
    declare_secret(&h).await;
    let staged = h
        .send(
            "POST",
            &stage_of(&h, "api_token"),
            Some(json!({ "value": "hunter2" })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(staged.status, 200, "{}", staged.raw);
    let pending = staged.body["pending_id"]
        .as_str()
        .expect("a token")
        .to_owned();
    assert_eq!(rows(&h).await, (1, 0));
    h.secrets.go_unconfigured();

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

    let item = &answer.body["results"][0];
    assert_eq!(item["outcome"], json!("rejected"), "{item}");
    assert_eq!(item["error"], json!("not_configured"), "{item}");
    assert_eq!(rows(&h).await, (1, 0), "the token and the absence stand");
}

#[tokio::test]
async fn removing_a_secret_value_is_refused_and_the_value_stays() {
    let h = RestHarness::new().await;
    declare_secret(&h).await;
    let set = h
        .send(
            "PUT",
            &value_of(&h, "api_token"),
            Some(json!({ "value": "hunter2" })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(set.status, 200, "{}", set.raw);
    let tag = set.etag.expect("a tag");
    h.secrets.go_unconfigured();

    let answer = h
        .send(
            "DELETE",
            &value_of(&h, "api_token"),
            None,
            Some(&tag),
            h.inner.tree.root,
        )
        .await;

    assert_not_configured(&answer);
    assert_eq!(rows(&h).await, (0, 1), "the row is still there");
}

#[tokio::test]
async fn a_secret_declaration_is_refused_but_an_ordinary_one_is_created() {
    let h = RestHarness::new().await;
    h.secrets.go_unconfigured();
    let body = |name: &str, value_type: &str, default: Value| {
        json!({
            "value_type_id": value_type,
            "vendor": "acme",
            "name": name,
            "category_id": h.inner.category_id(),
            "default_value": default,
            "scope_class": "cascading",
        })
    };

    let secret = h
        .send(
            "POST",
            DECLARATIONS,
            Some(body("api_token", SECRET, json!(""))),
            None,
            h.inner.tree.root,
        )
        .await;
    let plain = h
        .send(
            "POST",
            DECLARATIONS,
            Some(body("proxy", BOOL, json!(true))),
            None,
            h.inner.tree.root,
        )
        .await;

    assert_not_configured(&secret);
    assert_eq!(plain.status, 201, "{}", plain.raw);
}

#[tokio::test]
async fn purging_a_retired_secret_declaration_is_refused_and_it_stays() {
    let h = RestHarness::new().await;
    let created = h
        .send(
            "POST",
            DECLARATIONS,
            Some(json!({
                "value_type_id": SECRET,
                "vendor": "acme",
                "name": "api_token",
                "category_id": h.inner.category_id(),
                "default_value": "",
                "scope_class": "cascading",
            })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.raw);
    let id = created.body["id"].as_str().expect("an id").to_owned();
    let retired = h
        .send(
            "DELETE",
            &format!("{DECLARATIONS}/{id}"),
            None,
            created.etag.as_deref(),
            h.inner.tree.root,
        )
        .await;
    assert!(retired.status.is_success(), "{}", retired.raw);
    let tag = h
        .send(
            "GET",
            &format!("{DECLARATIONS}/{id}"),
            None,
            None,
            h.inner.tree.root,
        )
        .await
        .etag
        .expect("a tag");
    h.secrets.go_unconfigured();

    let purge = h
        .send(
            "POST",
            &format!("{DECLARATIONS}/{id}/purge"),
            None,
            Some(&tag),
            h.inner.tree.root,
        )
        .await;

    assert_not_configured(&purge);
    let (status, body) = h
        .get(&format!("{DECLARATIONS}/{id}"), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn ordinary_settings_and_masked_reads_work_without_secret_storage() {
    let h = RestHarness::new().await;
    declare_secret(&h).await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let set = h
        .send(
            "PUT",
            &value_of(&h, "api_token"),
            Some(json!({ "value": "hunter2" })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(set.status, 200, "{}", set.raw);
    h.secrets.go_unconfigured();

    let write = h
        .send(
            "PUT",
            &value_of(&h, "proxy"),
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    let read = h
        .get(
            &format!(
                "/settings-service/v1/settings/{}",
                key_path(&h, "api_token")
            ),
            h.inner.tree.root,
        )
        .await;

    assert_eq!(write.status, 200, "{}", write.raw);
    assert_eq!(read.0, 200, "{}", read.1);
    assert_eq!(
        read.1["value"],
        json!(MASK_TOKEN),
        "masked, store untouched"
    );
}
