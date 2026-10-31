//! Purging a retired declaration, driven as a client drives it.
//!
//! A purge is the one irreversible declaration operation: the row, every
//! value, restriction and staged secret filed under it, and the credential
//! store entries behind them all go, and the key becomes free again. So the
//! tests hold the gates in their order (authorization, then step-up, then the
//! kind and the status, then the tag), the release of secrets, and what the
//! audit trail keeps afterwards.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};
use toolkit_db::secure::SecureEntityExt;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::access::TenantAccess;
use crate::infra::storage::entity::{
    audit_record, pending_secret, setting_value, tenant_permission,
};
use crate::test_support::{BOOL, RestHarness, SECRET, TEXT};

const DECLARATIONS: &str = "/settings-service/v1/declarations";
const BATCH: &str = "/settings-service/v1/settings/batch";

/// What an admin-authored declaration is, as the tests hold it.
struct Declared {
    id: String,
    key: String,
    tag: String,
}

fn create_body(h: &RestHarness, name: &str, value_type: &str, default: &Value) -> Value {
    json!({
        "value_type_id": value_type,
        "vendor": "acme",
        "name": name,
        "category_id": h.inner.category_id(),
        "default_value": default,
        "scope_class": "cascading",
    })
}

/// Author a declaration through the surface, so it is admin-authored.
async fn create_typed(h: &RestHarness, name: &str, value_type: &str, default: &Value) -> Declared {
    let answer = h
        .send(
            "POST",
            DECLARATIONS,
            Some(create_body(h, name, value_type, default)),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    Declared {
        id: answer.body["id"].as_str().expect("an id").to_owned(),
        key: answer.body["key"].as_str().expect("a key").to_owned(),
        tag: answer.etag.expect("a state tag on the create"),
    }
}

async fn create(h: &RestHarness, name: &str) -> Declared {
    create_typed(h, name, BOOL, &json!(true)).await
}

async fn create_secret(h: &RestHarness, name: &str) -> Declared {
    create_typed(h, name, SECRET, &json!("")).await
}

/// The declaration's current state tag.
async fn tag_of(h: &RestHarness, id: &str) -> String {
    let answer = h
        .send(
            "GET",
            &format!("{DECLARATIONS}/{id}"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    answer.etag.expect("a state tag on the read")
}

/// Retire through the surface and answer with the tag the purge presents.
async fn retire(h: &RestHarness, declared: &Declared) -> String {
    let answer = h
        .send(
            "DELETE",
            &format!("{DECLARATIONS}/{}", declared.id),
            None,
            Some(&declared.tag),
            h.inner.tree.root,
        )
        .await;
    assert!(
        answer.status.is_success(),
        "{} {}",
        answer.status,
        answer.body
    );
    tag_of(h, &declared.id).await
}

async fn purge_as(h: &RestHarness, id: &str, tag: Option<&str>) -> crate::test_support::Answer {
    h.send(
        "POST",
        &format!("{DECLARATIONS}/{id}/purge"),
        None,
        tag,
        h.inner.tree.root,
    )
    .await
}

fn detail(answer: &crate::test_support::Answer) -> &str {
    answer.body["detail"].as_str().unwrap_or_default()
}

fn setting_uri(key: &str) -> String {
    format!("/settings-service/v1/settings/{}", key.replace('~', "%7E"))
}

/// Write one value through the batch, for a tenant.
async fn write(h: &RestHarness, key: &str, tenant: Uuid, value: Value) {
    let answer = h
        .send(
            "POST",
            BATCH,
            Some(json!({ "changes": [
                { "key": key, "tenant": tenant, "value": value, "if_match": "absent" },
            ]})),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(
        answer.body["results"][0]["outcome"],
        json!("committed"),
        "{}",
        answer.body
    );
}

/// Stage a secret for a tenant.
async fn stage(h: &RestHarness, key: &str, tenant: Uuid) {
    let answer = h
        .send(
            "POST",
            &format!("{}/secret-stage?tenant={tenant}", setting_uri(key)),
            Some(json!({ "value": "staged-plaintext" })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
}

/// The rows filed under a declaration: values, restrictions, staged secrets.
async fn filed_under(h: &RestHarness, id: &str) -> (usize, usize, usize) {
    let id = Uuid::parse_str(id).expect("a uuid");
    let conn = h.inner.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let values = setting_value::Entity::find()
        .filter(setting_value::Column::DeclarationId.eq(id))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("values")
        .len();
    let permissions = tenant_permission::Entity::find()
        .filter(tenant_permission::Column::DeclarationId.eq(id))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("permissions")
        .len();
    let pending = pending_secret::Entity::find()
        .filter(pending_secret::Column::DeclarationId.eq(id))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("pending")
        .len();
    (values, permissions, pending)
}

async fn audit_of(h: &RestHarness, key: &str) -> Vec<audit_record::Model> {
    let conn = h.inner.db.conn().expect("connection");
    audit_record::Entity::find()
        .filter(audit_record::Column::DeclarationKey.eq(key))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("audit records")
}

fn assert_stepup_challenge(answer: &crate::test_support::Answer) {
    assert_eq!(answer.status, 401, "{}", answer.body);
    let challenge = answer
        .headers
        .get("www-authenticate")
        .expect("the RFC 9470 challenge");
    assert!(
        challenge.contains("insufficient_user_authentication"),
        "{challenge}"
    );
}

async fn still_active(h: &RestHarness, id: &str) {
    let (status, body) = h
        .get(&format!("{DECLARATIONS}/{id}"), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], json!("active"), "{body}");
}

// ── The purge ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_retired_admin_declaration_is_purged_with_everything_filed_under_it() {
    let h = RestHarness::new().await;
    let d = create(&h, "proxy_enabled").await;
    write(&h, &d.key, h.inner.tree.a, json!(false)).await;
    h.restrict(
        Uuid::parse_str(&d.id).expect("uuid"),
        h.inner.tree.b,
        TenantAccess::ReadOnly,
    )
    .await;
    let before = filed_under(&h, &d.id).await;
    assert!(
        before.0 >= 1 && before.1 >= 1,
        "the fixture is in place: {before:?}"
    );
    let tag = retire(&h, &d).await;

    let answer = purge_as(&h, &d.id, Some(&tag)).await;

    assert_eq!(answer.status, 204, "{}", answer.body);
    assert_eq!(answer.body, Value::Null, "no body on a 204");
    let (status, _) = h
        .get(&format!("{DECLARATIONS}/{}", d.id), h.inner.tree.root)
        .await;
    assert_eq!(status, 404);
    assert_eq!(filed_under(&h, &d.id).await, (0, 0, 0));
}

#[tokio::test]
async fn an_active_declaration_is_not_purged() {
    let h = RestHarness::new().await;
    let d = create(&h, "proxy_enabled").await;
    let tag = tag_of(&h, &d.id).await;

    let answer = purge_as(&h, &d.id, Some(&tag)).await;

    assert_eq!(answer.status, 409, "{}", answer.body);
    assert!(
        detail(&answer).starts_with("declaration_not_retired"),
        "{}",
        answer.body
    );
    still_active(&h, &d.id).await;
}

#[tokio::test]
async fn a_contributed_declaration_is_never_purged_retired_or_not() {
    // A module owns what it contributes: the kind is judged before the status,
    // so an active one is refused for what it is, not for being active.
    let h = RestHarness::new().await;
    let retired = h.inner.declare("mtu", "cascading", json!(true)).await;
    h.inner.retire(retired).await;
    let active = h.inner.declare("proxy", "cascading", json!(true)).await;

    for id in [retired, active] {
        let tag = tag_of(&h, &id.to_string()).await;
        let answer = purge_as(&h, &id.to_string(), Some(&tag)).await;
        assert_eq!(answer.status, 409, "{}", answer.body);
        assert!(
            detail(&answer).starts_with("contributed_declaration_immutable"),
            "{}",
            answer.body
        );
        let (status, _) = h
            .get(&format!("{DECLARATIONS}/{id}"), h.inner.tree.root)
            .await;
        assert_eq!(status, 200, "and it is still there");
    }
}

// ── The gates ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_purge_without_a_fresh_step_up_is_challenged_and_removes_nothing() {
    // Creating is not step-up gated, but retiring and a value write are, so
    // the value is seeded and the declaration retired directly in the store:
    // the harness cannot be switched between a fresh and a stale assertion.
    let h = RestHarness::stale_step_up().await;
    let d = create(&h, "proxy_enabled").await;
    h.inner
        .set(
            Uuid::parse_str(&d.id).expect("uuid"),
            h.inner.tree.a,
            json!(false),
        )
        .await;
    h.inner.retire(Uuid::parse_str(&d.id).expect("uuid")).await;
    let tag = tag_of(&h, &d.id).await;

    let answer = purge_as(&h, &d.id, Some(&tag)).await;

    assert_stepup_challenge(&answer);
    let (status, _) = h
        .get(&format!("{DECLARATIONS}/{}", d.id), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "the declaration is still there");
    assert_eq!(filed_under(&h, &d.id).await.0, 1, "and so is its value");
}

#[tokio::test]
async fn a_purge_without_delete_authorization_is_refused_403_and_removes_nothing() {
    let h = RestHarness::denying().await;
    let d = create_denied_fixture(&h).await;

    let answer = purge_as(&h, &d.to_string(), Some("\"any\"")).await;

    assert_eq!(answer.status, 403, "{}", answer.body);
    let conn = h.inner.db.conn().expect("connection");
    let kept = crate::infra::storage::entity::declaration::Entity::find_by_id(d)
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&conn)
        .await
        .expect("read");
    assert!(kept.is_some(), "the declaration is still there");
    assert_eq!(filed_under(&h, &d.to_string()).await.0, 1);
}

/// A retired declaration with a value, seeded in the store because a denying
/// harness refuses the surface that would author one.
async fn create_denied_fixture(h: &RestHarness) -> Uuid {
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, h.inner.tree.a, json!(false)).await;
    h.inner.retire(id).await;
    id
}

#[tokio::test]
async fn the_gates_come_before_the_lookup_and_the_status() {
    let denying = RestHarness::denying().await;
    let answer = purge_as(&denying, &Uuid::new_v4().to_string(), Some("\"any\"")).await;
    assert_eq!(
        answer.status, 403,
        "authorization first, not 404: {}",
        answer.body
    );

    let stale = RestHarness::stale_step_up().await;
    let answer = purge_as(&stale, &Uuid::new_v4().to_string(), Some("\"any\"")).await;
    assert_stepup_challenge(&answer);

    let d = create(&stale, "proxy_enabled").await;
    let tag = tag_of(&stale, &d.id).await;
    let answer = purge_as(&stale, &d.id, Some(&tag)).await;
    assert_stepup_challenge(&answer);
}

#[tokio::test]
async fn a_purge_needs_the_tag_and_a_current_one() {
    let h = RestHarness::new().await;
    let d = create(&h, "proxy_enabled").await;
    let stale_tag = tag_of(&h, &d.id).await;
    let fresh_tag = retire(&h, &d).await;
    assert_ne!(stale_tag, fresh_tag, "the retirement moved the tag");

    let missing = purge_as(&h, &d.id, None).await;
    assert_eq!(missing.status, 428, "{}", missing.body);
    let stale = purge_as(&h, &d.id, Some(&stale_tag)).await;
    assert_eq!(stale.status, 412, "{}", stale.body);

    let (status, _) = h
        .get(&format!("{DECLARATIONS}/{}", d.id), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "the declaration is still there after both");
}

// ── Secrets ──────────────────────────────────────────────────────────────────

/// A secret declaration with values for two tenants and a staged secret for a
/// third, retired; answers the declaration, its tag and the held references.
async fn secret_fixture(h: &RestHarness) -> (Declared, String, Vec<String>) {
    let d = create_secret(h, "api_token").await;
    write(h, &d.key, h.inner.tree.a, json!("plaintext-a")).await;
    write(h, &d.key, h.inner.tree.b, json!("plaintext-b")).await;
    stage(h, &d.key, h.inner.tree.c).await;
    let held = h.secrets.held();
    assert_eq!(held.len(), 3, "two values and one staged entry");
    assert_eq!(filed_under(h, &d.id).await, (2, 0, 1));
    let tag = retire(h, &d).await;
    (d, tag, held)
}

#[tokio::test]
async fn a_purge_releases_every_credential_store_entry_of_the_declaration() {
    let h = RestHarness::new().await;
    let (d, tag, held) = secret_fixture(&h).await;

    let answer = purge_as(&h, &d.id, Some(&tag)).await;

    assert_eq!(answer.status, 204, "{}", answer.body);
    let mut released = h.secrets.deleted.lock().expect("lock").clone();
    released.sort();
    assert_eq!(released, held);
    assert_eq!(h.secrets.held(), Vec::<String>::new());
    assert_eq!(filed_under(&h, &d.id).await, (0, 0, 0));
}

#[tokio::test]
async fn the_release_runs_after_the_commit_so_the_declaration_is_already_gone() {
    let h = RestHarness::new().await;
    let (d, tag, held) = secret_fixture(&h).await;
    *h.secrets.row_probe.lock().expect("lock") = Some((
        std::sync::Arc::clone(&h.inner.db),
        d.id.parse().expect("an id"),
    ));

    let answer = purge_as(&h, &d.id, Some(&tag)).await;

    assert_eq!(answer.status, 204, "{}", answer.body);
    let seen = h
        .secrets
        .row_present_at_release
        .lock()
        .expect("lock")
        .clone();
    assert_eq!(seen.len(), held.len(), "every release looked: {seen:?}");
    assert!(
        seen.iter().all(|present| !present),
        "no release ran while the declaration row still existed: {seen:?}"
    );
}

#[tokio::test]
async fn a_refused_release_does_not_undo_the_purge() {
    let h = RestHarness::new().await;
    let (d, tag, held) = secret_fixture(&h).await;
    h.secrets.go_down();

    let answer = purge_as(&h, &d.id, Some(&tag)).await;

    assert_eq!(answer.status, 204, "{}", answer.body);
    let (status, _) = h
        .get(&format!("{DECLARATIONS}/{}", d.id), h.inner.tree.root)
        .await;
    assert_eq!(status, 404, "the declaration is gone");
    assert_eq!(
        h.secrets.held(),
        held,
        "the entries are left as orphans an operator reconciles; nothing retries them"
    );
    assert_eq!(filed_under(&h, &d.id).await, (0, 0, 0));
}

// ── The audit trail ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_purge_adds_one_remove_record_and_keeps_every_earlier_one() {
    let h = RestHarness::new().await;
    let (d, tag, held) = secret_fixture(&h).await;
    let before = audit_of(&h, &d.key).await;
    assert!(!before.is_empty(), "the declaration has a trail already");

    let answer = h
        .send_with_request_id(
            "POST",
            &format!("{DECLARATIONS}/{}/purge", d.id),
            Some(&tag),
            h.inner.tree.root,
            "req-purge-1",
        )
        .await;
    assert_eq!(answer.status, 204, "{}", answer.body);

    let after = audit_of(&h, &d.key).await;
    assert_eq!(after.len(), before.len() + 1, "exactly one record is added");
    for earlier in &before {
        assert!(
            after.iter().any(|r| r.id == earlier.id),
            "the earlier record {} is still there",
            earlier.id
        );
    }
    let added: Vec<_> = after
        .iter()
        .filter(|r| !before.iter().any(|b| b.id == r.id))
        .collect();
    let record = added[0];
    assert_eq!(
        record.actor,
        crate::test_support::context_for(h.inner.tree.root)
            .subject_id()
            .to_string(),
        "the record names the authenticated caller"
    );
    assert_eq!(record.request_id, "req-purge-1", "and the request's own id");
    assert_eq!(record.operation, "remove");
    assert_eq!(record.declaration_key, d.key);
    assert_eq!(record.tenant_id, None, "a definition sits at no scope");
    assert!(
        record.pre_value.is_some(),
        "the definition is the pre-image"
    );
    assert!(record.post_value.is_none(), "nothing is left after");
    let wire = format!("{:?} {:?}", record.pre_value, record.post_value);
    for secret in ["plaintext-a", "plaintext-b", "staged-plaintext"] {
        assert!(!wire.contains(secret), "{wire}");
    }
    for reference in &held {
        assert!(!wire.contains(reference), "{wire}");
    }
}

// ── What a purge frees ───────────────────────────────────────────────────────

#[tokio::test]
async fn the_last_declaration_purged_out_of_a_category_lets_the_category_go() {
    let h = RestHarness::new().await;
    let d = create(&h, "proxy_enabled").await;
    let tag = retire(&h, &d).await;
    let uri = format!("/settings-service/v1/categories/{}", h.inner.category_id());
    let held = h.send("DELETE", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(held.status, 428, "no tag, no delete: {}", held.body);
    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    let category_tag = read.etag.expect("the category's tag");
    let refused = h
        .send("DELETE", &uri, None, Some(&category_tag), h.inner.tree.root)
        .await;
    assert_eq!(
        refused.status, 409,
        "the retired declaration still blocks it"
    );

    let answer = purge_as(&h, &d.id, Some(&tag)).await;
    assert_eq!(answer.status, 204, "{}", answer.body);

    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    let category_tag = read.etag.expect("the category's tag");
    let answer = h
        .send("DELETE", &uri, None, Some(&category_tag), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 204, "{}", answer.body);
}

#[tokio::test]
async fn a_purged_key_can_be_declared_again_but_only_with_its_registered_type() {
    // The types registry keeps the type registered for a key for good, so the
    // freed key comes back with the same value type or not at all.
    let h = RestHarness::new().await;
    let d = create(&h, "proxy_enabled").await;
    let tag = retire(&h, &d).await;
    let answer = purge_as(&h, &d.id, Some(&tag)).await;
    assert_eq!(answer.status, 204, "{}", answer.body);

    let other = h
        .send(
            "POST",
            DECLARATIONS,
            Some(create_body(&h, "proxy_enabled", TEXT, &json!(""))),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(other.status, 409, "{}", other.body);

    let again = h
        .send(
            "POST",
            DECLARATIONS,
            Some(create_body(&h, "proxy_enabled", BOOL, &json!(true))),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(again.status, 201, "{}", again.body);
}

#[tokio::test]
async fn a_purged_key_is_no_longer_served_from_the_effective_cache() {
    let h = RestHarness::new().await;
    let d = create(&h, "proxy_enabled").await;
    write(&h, &d.key, h.inner.tree.a, json!(false)).await;
    let (status, body) = h.get(&setting_uri(&d.key), h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let tag = retire(&h, &d).await;

    let answer = purge_as(&h, &d.id, Some(&tag)).await;
    assert_eq!(answer.status, 204, "{}", answer.body);

    let (status, body) = h.get(&setting_uri(&d.key), h.inner.tree.root).await;
    assert_eq!(status, 404, "{body}");
}

#[tokio::test]
async fn a_purge_publishes_no_domain_event() {
    // Nothing resolves from a retired declaration, so no consumer has a value
    // to hear changed.
    let h = RestHarness::new().await;
    let d = create(&h, "proxy_enabled").await;
    write(&h, &d.key, h.inner.tree.a, json!(false)).await;
    let tag = retire(&h, &d).await;
    let before = h.published.events.lock().expect("lock").len();

    let answer = purge_as(&h, &d.id, Some(&tag)).await;
    assert_eq!(answer.status, 204, "{}", answer.body);

    assert_eq!(h.published.events.lock().expect("lock").len(), before);
}
