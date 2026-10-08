//! Books, configuration and export contracts against the real router and database.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;
mod storage_support;
struct Resolver {
    tenant: Uuid,
    allow: bool,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Resolver {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        use authz_resolver_sdk::*;
        Ok(EvaluationResponse {
            decision: self.allow
                && request
                    .subject
                    .subject_type
                    .as_deref()
                    .is_some_and(|grant| {
                        grant == "user"
                            || grant
                                == format!(
                                    "{}:{}",
                                    request
                                        .resource
                                        .resource_type
                                        .trim_start_matches("gts.cf.bss.pricing.")
                                        .trim_end_matches(".v1~"),
                                    request.action.name
                                )
                    }),
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        toolkit_security::pep_properties::OWNER_TENANT_ID,
                        vec![self.tenant],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}
struct Fixture {
    app: Router,
    denied: Router,
    ctx: SecurityContext,
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
    /// Every `(method, path)` the production router registers.
    registered: std::collections::BTreeSet<(String, String)>,
    /// Holds the database's temporary directory for the fixture's life.
    _dsn: storage_support::TestDsn,
}
impl Fixture {
    async fn new() -> Self {
        let (db, _, tenant, dsn) = storage_support::test_db().await;
        let state = Arc::new(
            bss_pricing::api::rest::authoring::AuthoringState::new(
                db.clone(),
                Arc::new(toolkit::ClientHub::default()),
            )
            .await
            .unwrap(),
        );
        // The gear mounts two routers (`module.rs`): authoring and the consumer reads.
        let production = |registry: &toolkit::api::OpenApiRegistryImpl| {
            bss_pricing::api::rest::authoring::router(state.clone(), registry).merge(
                bss_pricing::api::rest::read_contract::router(state.clone(), registry),
            )
        };
        let make = |allow| {
            production(&toolkit::api::OpenApiRegistryImpl::new()).layer(axum::Extension(
                authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Resolver { tenant, allow })),
            ))
        };
        let ctx = SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .subject_type("user")
            .build()
            .unwrap();
        let registry = toolkit::api::OpenApiRegistryImpl::new();
        let _counted = production(&registry);
        let registered = registry
            .operation_specs
            .iter()
            .map(|e| {
                let (method, path) = e.key().split_once(':').unwrap();
                (method.to_owned(), path.to_owned())
            })
            .collect();
        Self {
            app: make(true),
            denied: make(false),
            ctx,
            db,
            registered,
            _dsn: dsn,
        }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Value,
        tag: Option<&str>,
        key: Option<&str>,
    ) -> (u16, Value, String) {
        request(&self.app, &self.ctx, method, path, body, tag, key).await
    }
    async fn book(&self) -> (Value, String) {
        let (s, b, t) = self
            .call(
                "POST",
                "/price-books",
                json!({"code":"standard","name":"Standard","currency":"EUR"}),
                None,
                Some("create"),
            )
            .await;
        assert_eq!(s, 201, "{b}");
        (b, t)
    }
}
async fn request(
    app: &Router,
    ctx: &SecurityContext,
    method: &str,
    path: &str,
    body: Value,
    tag: Option<&str>,
    key: Option<&str>,
) -> (u16, Value, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/bss-pricing/v1{path}"))
        .extension(ctx.clone())
        .header("content-type", "application/json");
    if let Some(tag) = tag {
        req = req.header("if-match", tag);
    }
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let tag = response
        .headers()
        .get("etag")
        .map_or("", |v| v.to_str().unwrap())
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
        tag,
    )
}
#[tokio::test]
async fn books_positive_preconditions_validation_and_post_replay() {
    let f = Fixture::new().await;
    let payload = json!({"code":"standard","name":"Standard","currency":"EUR"});
    assert_eq!(
        f.call("POST", "/price-books", payload.clone(), None, None)
            .await
            .0,
        400
    );
    let (b, tag) = f.book().await;
    assert_eq!(tag, "\"1\"");
    let replay = f
        .call("POST", "/price-books", payload, None, Some("create"))
        .await;
    assert_eq!(replay, (201, b.clone(), tag.clone()));
    let duplicate = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"standard","name":"Other","currency":"EUR"}),
            None,
            Some("dup"),
        )
        .await;
    assert_eq!(duplicate.0, 409);
    assert!(duplicate.1.to_string().contains("BOOK_CODE_TAKEN"));
    assert_eq!(
        f.call(
            "POST",
            "/price-books",
            json!({"code":"new","name":"","currency":"eur"}),
            None,
            Some("invalid")
        )
        .await
        .0,
        400
    );
    let path = format!("/price-books/{}", b["id"].as_str().unwrap());
    // The list pages (D-442): the book is found by its code, and the page is the whole list.
    let listed = f
        .call("GET", "/price-books?q=standard", json!({}), None, None)
        .await
        .1;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1, "{listed}");
    assert!(listed["page_info"]["next_cursor"].is_null(), "{listed}");
    // The read is the created book with its stats (D-441); the write answers carry none.
    let mut read = f.call("GET", &path, json!({}), None, None).await.1;
    let stats = read.as_object_mut().unwrap().remove("stats").unwrap();
    assert_eq!(read, b);
    assert_eq!(stats["entries"], 0, "{stats}");
    assert_eq!(stats["last_change_at"], b["updated_at"], "{stats}");
    assert_eq!(
        f.call("PATCH", &path, json!({"name":"Changed"}), None, None)
            .await
            .0,
        400
    );
    let edited = f
        .call(
            "PATCH",
            &path,
            json!({"name":"Changed","valid_from":"2026-10-01","valid_until":"2026-11-01"}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(edited.0, 200, "{edited:?}");
    assert_eq!(edited.2, "\"2\"");
    assert_eq!(
        f.call("PATCH", &path, json!({"name":"stale"}), Some(&tag), None)
            .await
            .0,
        409
    );
    assert_eq!(
        f.call(
            "PATCH",
            &path,
            json!({"currency":"USD"}),
            Some(&edited.2),
            None
        )
        .await
        .0,
        400
    );
    let clear = f
        .call(
            "PATCH",
            &path,
            json!({"valid_until":null}),
            Some(&edited.2),
            None,
        )
        .await;
    assert_eq!(clear.0, 200);
    assert!(clear.1["valid_until"].is_null());
}
#[tokio::test]
async fn settings_positive_preconditions_and_matrix_8_templates() {
    let f = Fixture::new().await;
    let (s, mut body, tag) = f.call("GET", "/settings", json!({}), None, None).await;
    assert_eq!(s, 200);
    assert_eq!(tag, "\"0\"");
    // What the read adds to what the PUT takes (D-438): the version and who changed it when.
    for field in ["version", "updated_at", "updated_by"] {
        body.as_object_mut().unwrap().remove(field);
    }
    body["invoice_line_templates"] = json!({"usage":"{sku}"});
    assert_eq!(
        f.call("PUT", "/settings", body.clone(), None, None).await.0,
        400
    );
    let saved = f
        .call("PUT", "/settings", body.clone(), Some(&tag), None)
        .await;
    assert_eq!(saved.0, 200, "{saved:?}");
    assert_eq!(
        f.call("PUT", "/settings", body.clone(), Some(&tag), None)
            .await
            .0,
        409
    );
    for template in ["", "{phase}", "{unknown}", "{sku_name"] {
        body["invoice_line_templates"] = json!({"usage":template});
        assert_eq!(
            f.call("PUT", "/settings", body.clone(), Some(&saved.2), None)
                .await
                .0,
            400
        );
    }
    assert_eq!(
        f.call("GET", "/settings", json!({}), None, None).await.1,
        saved.1
    );
}
#[tokio::test]
async fn dimension_registry_positive_preconditions_and_matrix_11() {
    let f = Fixture::new().await;
    let (s, b, tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        b,
        json!({"items":[{"key":"region","values":[]}]}),
        "seeded with region, declared and not yet valued"
    );
    let input = json!({"items":[{"key":" region ","values":[" eu ","us",""]}]});
    assert_eq!(
        f.call("PUT", "/dimension-keys", input.clone(), None, None)
            .await
            .0,
        400
    );
    let saved = f
        .call("PUT", "/dimension-keys", input.clone(), Some(&tag), None)
        .await;
    assert_eq!(saved.0, 200, "{saved:?}");
    assert_eq!(saved.1["items"][0]["key"], "region");
    // D-436: the answer carries each value with its use.
    assert_eq!(
        saved.1["items"][0]["values"],
        json!([
            {"value": "eu", "usage": {"prices": 0}},
            {"value": "us", "usage": {"prices": 0}},
        ])
    );
    assert_eq!(
        f.call("PUT", "/dimension-keys", input, Some(&tag), None)
            .await
            .0,
        409
    );
    for item in [
        json!({"key":"Region","values":["eu","us"]}),
        json!({"key":"region","values":["eu"]}),
        json!({"key":"region","values":["eu","eu"]}),
        json!({"key":"region","values":["eu","bad value"]}),
    ] {
        assert_eq!(
            f.call(
                "PUT",
                "/dimension-keys",
                json!({"items":[item]}),
                Some(&saved.2),
                None
            )
            .await
            .0,
            400
        );
    }
}
#[tokio::test]
async fn every_route_denies_authorization_before_preconditions_or_disclosure() {
    let f = Fixture::new().await;
    let (b, _) = f.book().await;
    let id = b["id"].as_str().unwrap();
    for (method, path) in [
        ("POST", "/price-books".into()),
        ("POST", format!("/price-books/{id}/entries")),
        ("GET", format!("/price-book-entries/{id}")),
        ("PATCH", format!("/price-book-entries/{id}")),
        ("DELETE", format!("/price-book-entries/{id}")),
        ("GET", "/price-books".into()),
        ("GET", "/reference-ops".into()),
        ("GET", format!("/price-books/{id}")),
        ("PATCH", format!("/price-books/{id}")),
        ("DELETE", format!("/price-books/{id}")),
        ("POST", format!("/price-books/{id}/archive")),
        ("POST", format!("/price-books/{id}/unarchive")),
        ("GET", format!("/price-books/{id}/entries")),
        ("GET", format!("/price-books/{id}/export")),
        ("GET", "/settings".into()),
        ("PUT", "/settings".into()),
        ("GET", "/dimension-keys".into()),
        ("PUT", "/dimension-keys".into()),
        ("POST", format!("/price-book-entries/{id}/prices")),
        ("PATCH", format!("/prices/{id}")),
        ("DELETE", format!("/prices/{id}")),
        ("POST", format!("/prices/{id}/cancel")),
        ("POST", format!("/prices/{id}/end")),
        ("POST", format!("/prices/{id}/submit")),
        ("GET", format!("/price-books/{id}/publish-changes")),
        ("POST", format!("/price-books/{id}/publish-changes")),
        ("GET", "/approval-units".into()),
        ("GET", "/approval-units/counts".into()),
        ("GET", format!("/approval-units/{id}")),
        ("POST", format!("/approval-units/{id}/approve")),
        ("POST", format!("/approval-units/{id}/reject")),
        ("POST", format!("/approval-units/{id}/withdraw")),
        ("GET", "/approval-policy".into()),
        ("PUT", "/approval-policy".into()),
        ("POST", "/plans".into()),
        ("GET", "/plans".into()),
        ("GET", "/plans/counts".into()),
        ("GET", format!("/plans/{id}")),
        ("PATCH", format!("/plans/{id}")),
        ("POST", format!("/plans/{id}/revisions")),
        ("GET", format!("/plan-revisions/{id}")),
        ("PATCH", format!("/plan-revisions/{id}")),
        ("DELETE", format!("/plan-revisions/{id}")),
        ("POST", format!("/plan-revisions/{id}/items")),
        ("PATCH", format!("/plan-items/{id}")),
        ("DELETE", format!("/plan-items/{id}")),
        ("GET", "/plan-revisions/checks".into()),
        ("GET", format!("/plan-revisions/{id}/checks")),
        ("POST", format!("/plan-revisions/{id}/submit")),
        ("POST", format!("/plans/{id}/clone")),
        ("GET", "/resolve".into()),
        ("GET", format!("/prices/{id}")),
        ("GET", "/price-book-entries".into()),
        ("GET", format!("/plan-items/{id}")),
        ("DELETE", "/approval-policy/prices".into()),
        ("PATCH", "/dimension-keys".into()),
        ("GET", format!("/price-book-entries/{id}/prices")),
        ("POST", format!("/plan-revisions/{id}/unschedule")),
        ("GET", format!("/plan-revisions/{id}/reservations")),
        ("GET", "/approval-policy/prices/effective".into()),
    ] {
        assert_eq!(
            request(&f.denied, &f.ctx, method, &path, json!({}), None, None)
                .await
                .0,
            403,
            "{method} {path}"
        );
    }
}

use bss_pricing::infra::storage::{
    entity::{price, price_book, price_book_entry},
    repo::{book_repo, price_book_entry_repo, price_repo},
};
use storage_support::at;
fn book(tenant: Uuid) -> price_book::Model {
    price_book::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        code: "standard".into(),
        name: "Standard".into(),
        currency: "EUR".into(),
        valid_from: None,
        valid_until: None,
        description: None,
        version: 1,
        created_at: at(9),
        updated_at: at(9),
        archived_at: None,
        archived_by: None,
    }
}
fn entry(b: &price_book::Model) -> price_book_entry::Model {
    price_book_entry::Model {
        id: Uuid::new_v4(),
        tenant_id: b.tenant_id,
        book_id: b.id,
        sku_id: Uuid::new_v4(),
        charge_kind: "usage".into(),
        period: None,
        model: "per_unit".into(),
        usage_policy_id: None,
        usage_policy_version: None,
        usage_policy_digest: None,
        usage_sku_version: None,
        dimension_key: None,
        invoice_line_override: None,
        reservation_id: Uuid::new_v4(),
        reference_state: "confirmed".into(),
        version: 1,
        created_at: at(9),
        updated_at: at(9),
    }
}
fn price(p: &price_book_entry::Model) -> price::Model {
    price::Model {
        id: Uuid::new_v4(),
        tenant_id: p.tenant_id,
        price_book_entry_id: p.id,
        version_no: 1,
        dim_value: None,
        price_json: serde_json::json!({"rate":"0.1"}),
        min_fee: Some("12.34".into()),
        eligibility: "all".into(),
        effective_from: at(9).date(),
        effective_to: None,
        keep_for_bound: false,
        closed_explicitly: false,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        change_kind: "set".into(),
        target_price_id: None,
        cancelled_by_unit_id: None,
        state: "draft".into(),
        pending_unit_id: None,
        approved_by_unit_id: None,
        note: None,
        created_by: Uuid::new_v4(),
        approved_at: None,
        version: 1,
        created_at: at(9),
        updated_at: at(9),
    }
}

#[tokio::test]
async fn export_contains_all_states_in_order_and_used_dimension_cannot_be_removed() {
    let f = Fixture::new().await;
    let tenant = f.ctx.subject_tenant_id();
    let scope = toolkit_db::secure::AccessScope::for_tenant(tenant);
    let (_, _, tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    let saved = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":"region","values":["eu","us","ap"]}]}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(saved.0, 200);
    let conn = f.db.conn().unwrap();
    let b = book_repo::insert(&conn, &scope, book(tenant))
        .await
        .unwrap();
    for sku in [2, 1] {
        let mut p = entry(&b);
        p.sku_id = Uuid::from_u128(sku);
        p.dimension_key = Some("region".into());
        let p = price_book_entry_repo::insert(&conn, &scope, p)
            .await
            .unwrap();
        for (n, value, state) in [
            (3, "us", "approved"),
            (1, "eu", "draft"),
            (2, "ap", "rejected"),
            (4, "eu", "pending"),
        ] {
            let mut r = price(&p);
            r.version_no = n;
            r.dim_value = Some(value.into());
            r.state = state.into();
            price_repo::insert(&conn, &scope, r).await.unwrap();
        }
    }
    let path = format!("/price-books/{}/export", b.id);
    let first = f.call("GET", &path, json!({}), None, None).await;
    assert_eq!(first.0, 200, "{first:?}");
    assert_eq!(first, f.call("GET", &path, json!({}), None, None).await);
    assert_eq!(first.1["entries"].as_array().unwrap().len(), 2);
    assert_eq!(
        first.1["entries"][0]["entry"]["sku_id"],
        Uuid::from_u128(1).to_string()
    );
    assert_eq!(
        first.1["entries"][0]["prices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["dim_value"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["ap", "eu", "eu", "us"]
    );
    assert_eq!(
        f.call(
            "GET",
            &format!("/price-books/{}/entries", b.id),
            json!({}),
            None,
            None
        )
        .await
        .1["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for items in [json!([]), json!([{"key":"region","values":["eu","us"]}])] {
        let refused = f
            .call(
                "PUT",
                "/dimension-keys",
                json!({"items":items}),
                Some(&saved.2),
                None,
            )
            .await;
        assert_eq!(refused.0, 409);
        assert!(refused.1.to_string().contains("DIM_VALUE_IN_USE"));
    }
    // Nothing was removed: the registry's content tag is the one the PUT answered (its `usage`
    // now counts the prices written since, D-436).
    let (_, now, tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    assert_eq!(tag, saved.2);
    assert_eq!(
        now["items"][0]["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["value"].clone())
            .collect::<Vec<_>>(),
        saved.1["items"][0]["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["value"].clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the label table is a route census: one row per route, in one place"
)]
async fn authorization_labels_actions_and_cross_tenant_reads_are_pinned() {
    let f = Fixture::new().await;
    let (b, _) = f.book().await;
    let id = b["id"].as_str().unwrap();
    let table = [
        ("POST", "/price-books".into(), "price_book", "author"),
        (
            "POST",
            format!("/price-books/{id}/entries"),
            "price_book_entry",
            "author",
        ),
        (
            "GET",
            format!("/price-book-entries/{id}"),
            "price_book_entry",
            "read",
        ),
        (
            "PATCH",
            format!("/price-book-entries/{id}"),
            "price_book_entry",
            "author",
        ),
        (
            "DELETE",
            format!("/price-book-entries/{id}"),
            "price_book_entry",
            "author",
        ),
        ("GET", "/price-books".into(), "price_book", "read"),
        ("GET", "/reference-ops".into(), "config", "settings"),
        ("GET", format!("/price-books/{id}"), "price_book", "read"),
        (
            "PATCH",
            format!("/price-books/{id}"),
            "price_book",
            "author",
        ),
        (
            "DELETE",
            format!("/price-books/{id}"),
            "price_book",
            "author",
        ),
        // D-522: the archive mark is a book write.
        (
            "POST",
            format!("/price-books/{id}/archive"),
            "price_book",
            "author",
        ),
        (
            "POST",
            format!("/price-books/{id}/unarchive"),
            "price_book",
            "author",
        ),
        (
            "GET",
            format!("/price-books/{id}/entries"),
            "price_book_entry",
            "read",
        ),
        (
            "GET",
            format!("/price-books/{id}/export"),
            "price_book",
            "read",
        ),
        ("GET", "/settings".into(), "config", "read"),
        ("PUT", "/settings".into(), "config", "settings"),
        ("GET", "/dimension-keys".into(), "config", "read"),
        ("PUT", "/dimension-keys".into(), "config", "settings"),
        (
            "POST",
            format!("/price-book-entries/{id}/prices"),
            "price",
            "author",
        ),
        ("PATCH", format!("/prices/{id}"), "price", "author"),
        ("DELETE", format!("/prices/{id}"), "price", "author"),
        ("POST", format!("/prices/{id}/cancel"), "price", "author"),
        ("POST", format!("/prices/{id}/end"), "price", "author"),
        ("POST", format!("/prices/{id}/submit"), "price", "submit"),
        (
            "GET",
            format!("/price-books/{id}/publish-changes"),
            "price_book",
            "read",
        ),
        (
            "POST",
            format!("/price-books/{id}/publish-changes"),
            "price_book",
            "submit",
        ),
        ("GET", "/approval-units".into(), "approval_unit", "read"),
        // D-470: the counts under the list's own grant.
        (
            "GET",
            "/approval-units/counts".into(),
            "approval_unit",
            "read",
        ),
        (
            "GET",
            format!("/approval-units/{id}"),
            "approval_unit",
            "read",
        ),
        (
            "POST",
            format!("/approval-units/{id}/approve"),
            "approval_unit",
            "approve",
        ),
        (
            "POST",
            format!("/approval-units/{id}/reject"),
            "approval_unit",
            "approve",
        ),
        (
            "POST",
            format!("/approval-units/{id}/withdraw"),
            "approval_unit",
            "submit",
        ),
        ("GET", "/approval-policy".into(), "config", "read"),
        ("PUT", "/approval-policy".into(), "config", "settings"),
        ("POST", "/plans".into(), "plan", "author"),
        ("GET", "/plans".into(), "plan", "read"),
        ("GET", "/plans/counts".into(), "plan", "read"),
        ("GET", format!("/plans/{id}"), "plan", "read"),
        ("PATCH", format!("/plans/{id}"), "plan", "author"),
        ("POST", format!("/plans/{id}/revisions"), "plan", "author"),
        ("GET", format!("/plan-revisions/{id}"), "plan", "read"),
        ("PATCH", format!("/plan-revisions/{id}"), "plan", "author"),
        ("DELETE", format!("/plan-revisions/{id}"), "plan", "author"),
        (
            "POST",
            format!("/plan-revisions/{id}/items"),
            "plan",
            "author",
        ),
        ("PATCH", format!("/plan-items/{id}"), "plan", "author"),
        ("DELETE", format!("/plan-items/{id}"), "plan", "author"),
        ("GET", "/plan-revisions/checks".into(), "plan", "read"),
        (
            "GET",
            format!("/plan-revisions/{id}/checks"),
            "plan",
            "read",
        ),
        (
            "POST",
            format!("/plan-revisions/{id}/submit"),
            "plan",
            "submit",
        ),
        ("POST", format!("/plans/{id}/clone"), "plan", "author"),
        ("GET", "/resolve".into(), "plan", "read"),
        ("GET", format!("/prices/{id}"), "price", "read"),
        (
            "GET",
            "/price-book-entries".into(),
            "price_book_entry",
            "read",
        ),
        ("GET", format!("/plan-items/{id}"), "plan", "read"),
        (
            "DELETE",
            "/approval-policy/prices".into(),
            "config",
            "settings",
        ),
        ("PATCH", "/dimension-keys".into(), "config", "settings"),
        // D-440: entry read reaches the entry; its money is judged a second time (price_book
        // read on the entry's book), after the entry is found — the book's id here is no entry.
        (
            "GET",
            format!("/price-book-entries/{id}/prices"),
            "price_book_entry",
            "read",
        ),
        // D-452: withdrawing an approved change is plan submit's (D-418).
        (
            "POST",
            format!("/plan-revisions/{id}/unschedule"),
            "plan",
            "submit",
        ),
        (
            "GET",
            format!("/plan-revisions/{id}/reservations"),
            "plan",
            "read",
        ),
        // D-481: prices is read under price_book_entry read. plan_revision is the other grant
        // of the same door, pinned in tests/revision_reads.rs.
        (
            "GET",
            "/approval-policy/prices/effective".into(),
            "price_book_entry",
            "read",
        ),
    ];
    // The label table is a route census: exactly the routes the router registers, one row each.
    let rows: std::collections::BTreeSet<(String, String)> = table
        .iter()
        .map(|(method, path, _, _): &(&str, String, &str, &str)| {
            (
                (*method).to_owned(),
                format!(
                    "/bss-pricing/v1{}",
                    path.replace(id, "{id}")
                        .replace("/approval-policy/prices", "/approval-policy/{kind}")
                ),
            )
        })
        .collect();
    // 60: run 9.7's batch checks read (D-482), run 9.8b's plans counts (D-485), the cancel and
    // end doors (D-520, D-521), and the book's archive and unarchive (D-522).
    assert_eq!(table.len(), 60);
    assert_eq!(rows.len(), table.len(), "one row per route");
    assert_eq!(
        rows, f.registered,
        "the label table covers every registered route"
    );
    for (method, path, label, action) in table {
        let context = |grant: &str| {
            SecurityContext::builder()
                .subject_id(f.ctx.subject_id())
                .subject_tenant_id(f.ctx.subject_tenant_id())
                .subject_type(grant)
                .build()
                .unwrap()
        };
        let allowed = context(&format!("{label}:{action}"));
        assert_ne!(
            request(&f.app, &allowed, method, &path, json!({}), None, None)
                .await
                .0,
            403,
            "{method} {path}"
        );
        assert_eq!(
            request(
                &f.app,
                &context("wrong:wrong"),
                method,
                &path,
                json!({}),
                None,
                None
            )
            .await
            .0,
            403,
            "{method} {path}"
        );
    }
    let stranger = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .subject_type("user")
        .build()
        .unwrap();
    for path in [
        format!("/price-books/{id}"),
        format!("/price-books/{id}/export"),
    ] {
        assert_eq!(
            request(&f.app, &stranger, "GET", &path, json!({}), None, None)
                .await
                .0,
            404,
            "{path}: a foreign book is not disclosed"
        );
    }
}

// Surface F5: a NUL in any free text is 400 VALIDATION on every dialect, and nothing is written.
#[tokio::test]
async fn a_nul_character_in_free_text_is_refused_before_any_write() {
    let f = Fixture::new().await;
    let (status, b, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"standard","name":"a\u{0}b","currency":"EUR"}),
            None,
            Some("nul"),
        )
        .await;
    assert_eq!(status, 400, "{b}");
    assert!(b.to_string().contains("VALIDATION"), "{b}");
    let (status, list, _) = f.call("GET", "/price-books", json!({}), None, None).await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["items"], json!([]), "no book was written");
    assert!(
        list["page_info"]["next_cursor"].is_null(),
        "one page is the whole list: {list}"
    );
    let (_, _, tag) = f.call("GET", "/settings", json!({}), None, None).await;
    let (status, b, _) = f
        .call(
            "PUT",
            "/settings",
            json!({"default_timing":"advance","default_rounding":"half\u{0}up","invoice_line_templates":{}}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 400, "{b}");
    assert!(b.to_string().contains("VALIDATION"), "{b}");
}
