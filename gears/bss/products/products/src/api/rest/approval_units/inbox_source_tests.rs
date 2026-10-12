//! The approvals inbox's products source against this gear's own doors (P-D-250): the same
//! request through the door and through the source answers the same status, code and body bytes,
//! for the list, the counts, the card and every vote refusal, `UNIT_STALE`'s `generation`
//! included. The door is the gear's router under its enforcer and the platform's error layer, as
//! the gateway serves it; a source refusal is rendered at the door's own path through that layer.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{ProductsApprovalSource, SOURCE};
use crate::infra::storage::repo;
use crate::test_support::*;
use axum::{
    Extension, Router,
    body::Body,
    http::{Request, header},
    response::IntoResponse,
};
use bss_approval::{Store, Unit, UnitState};
use bss_approvals_sdk::{
    ApprovalSourceV1, InboxUnit, Order, SortKey, SourceCounts, SourceNarrowing, SourcePageQuery,
    VoteAction, VoteRequest,
};
use serde_json::{Value, json};
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

/// The gear's doors, as `gear.rs` mounts them.
pub(super) fn routes(
    s: Arc<crate::api::rest::ApiState>,
    o: &dyn toolkit::api::OpenApiRegistry,
) -> Router {
    crate::api::rest::categories::router(s.clone(), o)
        .merge(crate::api::rest::skus::router(s.clone(), o))
        .merge(crate::api::rest::sku_governance::router(s.clone(), o))
        .merge(crate::api::rest::approval_units::router(s.clone(), o))
        .merge(crate::api::rest::approval_policy::router(s.clone(), o))
        .merge(crate::api::rest::references::router(s.clone(), o))
        .merge(crate::api::rest::browse::router(s.clone(), o))
        .merge(crate::api::rest::usage_types::router(s, o))
}

/// The platform's error layer, which the gateway puts around every door.
pub(super) fn gateway(app: Router) -> Router {
    app.layer(axum::middleware::from_fn(
        toolkit::api::canonical_error_middleware,
    ))
}

pub(super) struct Census {
    pub door: Router,
    pub denied_door: Router,
    pub source: ProductsApprovalSource,
    pub denied_source: ProductsApprovalSource,
    pub state: Arc<crate::api::rest::ApiState>,
    /// Held for the test's life: its temporary directory holds the database.
    pub dsn: TestDsn,
    pub tenant: Uuid,
    pub author: SecurityContext,
}

pub(super) async fn census_on(
    quorum: u32,
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
    dsn: TestDsn,
) -> Census {
    let tenant = Uuid::new_v4();
    let (app, state) = rest_app_on_db(tenant, routes, resolved_usage_types(), "test", db).await;
    let denied_door = gateway(
        routes(state.clone(), &toolkit::api::OpenApiRegistryImpl::new())
            .layer(Extension(denying_enforcer())),
    );
    let c = Census {
        door: gateway(app),
        denied_door,
        source: ProductsApprovalSource::new(state.clone(), flat_in_enforcer(tenant)),
        denied_source: ProductsApprovalSource::new(state.clone(), denying_enforcer()),
        state,
        dsn,
        tenant,
        author: authed_ctx(tenant),
    };
    c.policy(quorum).await;
    c
}

pub(super) async fn census(quorum: u32) -> Census {
    let (db, _, _, dsn) = test_db().await;
    census_on(quorum, db, dsn).await
}

impl Census {
    pub(super) async fn policy(&self, quorum: u32) {
        let response = request_as(
            &self.door,
            &self.author,
            axum::http::Method::GET,
            "/bss-products/v1/approval-policy",
            None,
            None,
        )
        .await;
        let tag = response.headers()["etag"].to_str().unwrap().to_owned();
        let response = request_as(
            &self.door,
            &self.author,
            axum::http::Method::PUT,
            "/bss-products/v1/approval-policy",
            Some(json!({"quorum":quorum})),
            Some(&tag),
        )
        .await;
        assert_eq!(response.status(), 200);
    }
    /// A recurring draft by the author, submitted for publication by the author: the SKU and the
    /// unit.
    pub(super) async fn unit(&self, code: &str) -> (Uuid, Value) {
        let (status, sku, _) = send(
            &self.door,
            &self.author,
            "POST",
            "/bss-products/v1/skus",
            &json!({"code":code,"name":code,"type":"recurring"}).to_string(),
            None,
        )
        .await;
        assert_eq!(status, 201, "{sku}");
        let sku: Value = serde_json::from_str(&sku).unwrap();
        let id: Uuid = sku["id"].as_str().unwrap().parse().unwrap();
        let (status, receipt, _) = send(
            &self.door,
            &self.author,
            "POST",
            &format!("/bss-products/v1/skus/{id}/submit"),
            "{}",
            None,
        )
        .await;
        assert_eq!(status, 200, "{receipt}");
        let receipt: Value = serde_json::from_str(&receipt).unwrap();
        (id, receipt["unit"].clone())
    }
    /// Units written straight through the gear's own store, `submitted_at` as given.
    pub(super) async fn stored(&self, units: Vec<(&'static str, Uuid, OffsetDateTime)>) {
        stored_in(&self.state.db, self.tenant, units).await;
    }
}

/// Units written straight through the products store, `submitted_at` as given: their ids.
pub(super) async fn stored_in(
    db: &toolkit_db::DBProvider<toolkit_db::DbError>,
    tenant: Uuid,
    units: Vec<(&'static str, Uuid, OffsetDateTime)>,
) -> Vec<Uuid> {
    let store = repo::ProductsApprovalStore {
        scope: AccessScope::for_tenant(tenant),
        tenant_id: tenant,
    };
    let units: Vec<Unit> = units
        .into_iter()
        .map(|(kind, sku, at)| Unit {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            kind: kind.to_owned(),
            ref_type: "sku".into(),
            ref_id: sku,
            state: UnitState::Pending,
            common_effective_date: None,
            quorum_required: 1,
            generation: 1,
            submitted_by: Uuid::new_v4(),
            submitted_at: at,
            submit_note: Some("stored".into()),
            decided_at: None,
            decided_note: None,
            snapshot: json!({"stored":true}),
            snapshot_hash: "h".into(),
            version: 1,
        })
        .collect();
    let ids = units.iter().map(|u| u.id).collect();
    db.db()
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                for unit in &units {
                    store
                        .insert_unit(tx, unit, &[])
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    ids
}

/// `(status, body bytes as text, content type)` of one request through `app`.
pub(super) async fn send(
    app: &Router,
    ctx: &SecurityContext,
    method: &str,
    path: &str,
    body: &str,
    key: Option<&str>,
) -> (u16, String, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .extension(ctx.clone())
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        req = req.header("Idempotency-Key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let kind = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map_or("", |v| v.to_str().unwrap())
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap(), kind)
}

/// A source refusal as the platform's error layer answers it at the door's path.
pub(super) async fn rendered(path: &str, error: CanonicalError) -> (u16, String) {
    let app = gateway(Router::new().fallback(move || {
        let error = error.clone();
        async move { error.into_response() }
    }));
    let response = app
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// `ours` laid out as the door's `door` is: the door's keys, in the door's order.
pub(super) fn as_the_door(ours: &Value, door: &Value) -> Value {
    match (ours, door) {
        (Value::Object(ours), Value::Object(door)) => Value::Object(
            door.iter()
                .map(|(k, d)| {
                    (
                        k.clone(),
                        as_the_door(ours.get(k).unwrap_or(&Value::Null), d),
                    )
                })
                .collect(),
        ),
        (Value::Array(ours), Value::Array(door)) if ours.len() == door.len() => Value::Array(
            ours.iter()
                .zip(door)
                .map(|(o, d)| as_the_door(o, d))
                .collect(),
        ),
        _ => ours.clone(),
    }
}

/// The source's unit is the door's item byte for byte: its `subject_live` is the door's
/// `impact_live`; what it adds is the source's name and the impact (P-D-250).
pub(super) fn same_unit(door: &Value, unit: &InboxUnit) {
    let mut ours = serde_json::to_value(unit).unwrap();
    assert_eq!(ours["source"], SOURCE);
    ours["impact_live"] = ours["subject_live"].clone();
    assert_eq!(
        serde_json::to_string(&as_the_door(&ours, door)).unwrap(),
        serde_json::to_string(door).unwrap(),
    );
}

pub(super) fn narrowing(query: &[(&str, &str)]) -> SourceNarrowing {
    let get = |key: &str| {
        query
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| (*v).to_owned())
    };
    SourceNarrowing {
        state: get("state"),
        kind: get("kind"),
        ref_id: get("ref_id").map(|v| v.parse().unwrap()),
        book_id: get("book_id").map(|v| v.parse().unwrap()),
    }
}

pub(super) fn query_string(query: &[(&str, &str)]) -> String {
    query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

pub(super) fn key_of(item: &Value) -> SortKey {
    SortKey {
        submitted_at: OffsetDateTime::parse(
            item["submitted_at"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap(),
        id: item["id"].as_str().unwrap().parse().unwrap(),
    }
}

const UNITS: &str = "/bss-products/v1/approval-units";

/// Every page of the list door under `query`, in `order`, two at a time, against the source's
/// page after the last key of the page before it: the same units, byte for byte, and the same
/// "more".
async fn walk(c: &Census, query: &[(&str, &str)], order: Order) -> usize {
    let mut pairs = query.to_vec();
    if order == Order::Desc {
        pairs.push(("$orderby", "submitted_at%20desc"));
    }
    let mut path = format!("{UNITS}?limit=2&{}", query_string(&pairs));
    let mut after = None;
    let mut seen = 0;
    loop {
        let (status, body, _) = send(&c.door, &c.author, "GET", &path, "", None).await;
        assert_eq!(status, 200, "{path}: {body}");
        let door: Value = serde_json::from_str(&body).unwrap();
        let page = c
            .source
            .page(
                &c.author,
                &SourcePageQuery {
                    narrowing: narrowing(query),
                    order,
                    limit: 2,
                    after,
                    impact: true,
                },
            )
            .await
            .unwrap();
        let items = door["items"].as_array().unwrap();
        assert_eq!(page.units.len(), items.len(), "{path}");
        for (item, unit) in items.iter().zip(&page.units) {
            same_unit(item, unit);
        }
        seen += items.len();
        let next = door["page_info"]["next_cursor"].as_str();
        assert_eq!(page.has_more, next.is_some(), "{path}");
        let Some(next) = next else {
            return seen;
        };
        after = Some(key_of(items.last().unwrap()));
        path = format!("{UNITS}?limit=2&{}&cursor={next}", query_string(query));
    }
}

#[tokio::test]
async fn the_page_is_the_list_doors_page_in_both_orders_after_any_key() {
    let c = census(1).await;
    let (sku, _) = c.unit("A").await;
    c.unit("B").await;
    c.unit("C").await;
    let base = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() - time::Duration::days(1);
    c.stored(vec![
        ("sku_change", sku, base),
        ("sku_retire", sku, base + time::Duration::seconds(1)),
        (
            "sku_change",
            Uuid::new_v4(),
            base + time::Duration::seconds(2),
        ),
    ])
    .await;
    let sku = sku.to_string();
    for (query, expected) in [
        (vec![], 6),
        (vec![("state", "pending")], 6),
        (vec![("kind", "sku_change")], 2),
        (vec![("kind", "sku_publish")], 3),
        (vec![("ref_id", sku.as_str())], 3),
        (vec![("state", "approved")], 0),
    ] {
        for order in [Order::Asc, Order::Desc] {
            assert_eq!(walk(&c, &query, order).await, expected, "{query:?}");
        }
    }
}

#[tokio::test]
async fn the_list_and_counts_refusals_are_the_doors() {
    let c = census(1).await;
    for door_path in [UNITS.to_owned(), format!("{UNITS}/counts")] {
        let path = format!("{door_path}?state=bogus");
        let door = send(&c.door, &c.author, "GET", &path, "", None).await;
        assert_eq!(door.0, 400, "{path}: {}", door.1);
        let n = narrowing(&[("state", "bogus")]);
        let error = if door_path == UNITS {
            c.source
                .page(
                    &c.author,
                    &SourcePageQuery {
                        narrowing: n,
                        order: Order::Asc,
                        limit: 2,
                        after: None,
                        impact: true,
                    },
                )
                .await
                .unwrap_err()
        } else {
            c.source.counts(&c.author, &n).await.unwrap_err()
        };
        assert_eq!(rendered(&path, error).await, (door.0, door.1), "{path}");
        // The grant.
        let door = send(&c.denied_door, &c.author, "GET", &door_path, "", None).await;
        assert_eq!(door.0, 403, "{}", door.1);
        let error = if door_path == UNITS {
            c.denied_source
                .page(
                    &c.author,
                    &SourcePageQuery {
                        narrowing: SourceNarrowing::default(),
                        order: Order::Asc,
                        limit: 2,
                        after: None,
                        impact: true,
                    },
                )
                .await
                .unwrap_err()
        } else {
            c.denied_source
                .counts(&c.author, &SourceNarrowing::default())
                .await
                .unwrap_err()
        };
        assert_eq!(rendered(&door_path, error).await, (door.0, door.1));
    }
}

/// AP-D-2: a kind products does not record, and any `book_id` (products holds no book), are an
/// empty page and zero counts, decided in the source before any door. The door itself refuses the
/// kind, and refuses `book_id` on the list and the counts (P-D-254). The source still answers
/// an empty page for `book_id` before any door.
#[tokio::test]
async fn a_foreign_kind_and_a_book_are_empty_not_the_doors_answer() {
    let c = census(1).await;
    c.unit("A").await;
    let book = Uuid::new_v4().to_string();
    for (query, list_door, counts_door) in [
        (vec![("kind", "prices")], 400, 400),
        (vec![("kind", "plan_revision")], 400, 400),
        (vec![("kind", "bogus")], 400, 400),
        (vec![("book_id", book.as_str())], 400, 400),
    ] {
        let q = query_string(&query);
        let list = send(&c.door, &c.author, "GET", &format!("{UNITS}?{q}"), "", None).await;
        assert_eq!(list.0, list_door, "{q}: {}", list.1);
        let counts = send(
            &c.door,
            &c.author,
            "GET",
            &format!("{UNITS}/counts?{q}"),
            "",
            None,
        )
        .await;
        assert_eq!(counts.0, counts_door, "{q}: {}", counts.1);
        let n = narrowing(&query);
        let page = c
            .source
            .page(
                &c.author,
                &SourcePageQuery {
                    narrowing: n.clone(),
                    order: Order::Desc,
                    limit: 50,
                    after: None,
                    impact: true,
                },
            )
            .await
            .unwrap();
        assert!(page.units.is_empty() && !page.has_more, "{q}");
        assert_eq!(
            c.source.counts(&c.author, &n).await.unwrap(),
            SourceCounts::default(),
            "{q}"
        );
    }
}

#[tokio::test]
async fn a_foreign_narrowing_still_refuses_an_unknown_state() {
    let c = census(1).await;
    let book = Uuid::new_v4().to_string();
    for query in [
        vec![("kind", "bogus"), ("state", "nope")],
        vec![("book_id", book.as_str()), ("state", "nope")],
    ] {
        let n = narrowing(&query);
        let page = c
            .source
            .page(
                &c.author,
                &SourcePageQuery {
                    narrowing: n.clone(),
                    order: Order::Desc,
                    limit: 50,
                    after: None,
                    impact: false,
                },
            )
            .await;
        assert!(page.is_err(), "{query:?}: {page:?}");
        let counts = c.source.counts(&c.author, &n).await;
        assert!(counts.is_err(), "{query:?}: {counts:?}");
    }
}

#[tokio::test]
async fn the_counts_are_the_counts_doors() {
    let c = census(1).await;
    let (sku, _) = c.unit("A").await;
    c.unit("B").await;
    let base = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() - time::Duration::days(1);
    c.stored(vec![("sku_retire", sku, base)]).await;
    let sku = sku.to_string();
    for query in [
        vec![],
        vec![("state", "pending")],
        vec![("state", "approved")],
        vec![("kind", "sku_retire")],
        vec![("ref_id", sku.as_str())],
    ] {
        let path = format!("{UNITS}/counts?{}", query_string(&query));
        let (status, body, _) = send(&c.door, &c.author, "GET", &path, "", None).await;
        assert_eq!(status, 200, "{body}");
        let door: Value = serde_json::from_str(&body).unwrap();
        let ours = serde_json::to_value(
            c.source
                .counts(&c.author, &narrowing(&query))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&as_the_door(&ours, &door)).unwrap(),
            serde_json::to_string(&door).unwrap(),
            "{path}"
        );
        for other in ["prices", "plan_revision"] {
            assert_eq!(ours["by_kind"][other], 0, "{path}");
        }
    }
}

#[tokio::test]
async fn the_card_is_the_card_doors_and_a_miss_is_none() {
    let c = census(1).await;
    let (_, unit) = c.unit("A").await;
    let id: Uuid = unit["id"].as_str().unwrap().parse().unwrap();
    let path = format!("{UNITS}/{id}");
    let (status, body, _) = send(&c.door, &c.author, "GET", &path, "", None).await;
    assert_eq!(status, 200, "{body}");
    let door: Value = serde_json::from_str(&body).unwrap();
    assert!(door["impact_live"].is_object(), "{door}");
    let ours = c.source.get(&c.author, id, true).await.unwrap().unwrap();
    same_unit(&door, &ours);
    assert_eq!(ours.impact, None, "a publish has no impact");
    let missing = Uuid::new_v4();
    let door = send(
        &c.door,
        &c.author,
        "GET",
        &format!("{UNITS}/{missing}"),
        "",
        None,
    )
    .await;
    assert_eq!(door.0, 404, "{}", door.1);
    assert_eq!(c.source.get(&c.author, missing, true).await.unwrap(), None);
    assert_eq!(
        c.source
            .get(&authed_ctx(Uuid::new_v4()), id, true)
            .await
            .unwrap(),
        None,
        "another tenant's unit is a miss"
    );
    let door = send(&c.denied_door, &c.author, "GET", &path, "", None).await;
    assert_eq!(door.0, 403, "{}", door.1);
    let error = c.denied_source.get(&c.author, id, true).await.unwrap_err();
    assert_eq!(rendered(&path, error).await, (door.0, door.1));
}

/// One vote refusal: who votes, how, with which body and key suffix, whether the PDP refuses, and
/// the status and code the door answers.
type Case<'a> = (
    &'a SecurityContext,
    VoteAction,
    String,
    &'a str,
    bool,
    u16,
    &'a str,
);

/// One vote through the door and the same through the source, each under its own key: the same
/// status, content type and body bytes.
pub(super) async fn both(
    door_app: &Router,
    source: &ProductsApprovalSource,
    who: &SecurityContext,
    id: Uuid,
    action: VoteAction,
    body: &str,
    keys: Option<(&str, &str)>,
) -> (u16, String) {
    let path = format!("{UNITS}/{id}/{}", action.as_str());
    let door = send(door_app, who, "POST", &path, body, keys.map(|k| k.0)).await;
    let ours = source
        .vote(
            who,
            id,
            action,
            VoteRequest {
                body: body.as_bytes().to_vec(),
                idempotency_key: keys.map(|k| k.1.to_owned()),
            },
        )
        .await
        .unwrap();
    let content_type = ours
        .headers
        .iter()
        .find(|(name, _)| name == "content-type")
        .map_or("", |(_, value)| value.as_str());
    assert_eq!(
        (
            ours.status,
            content_type,
            String::from_utf8(ours.body.clone()).unwrap()
        ),
        (door.0, door.2.as_str(), door.1.clone()),
        "{path} {body}"
    );
    (door.0, door.1)
}

#[tokio::test]
async fn every_vote_refusal_is_the_vote_doors_byte_for_byte() {
    let c = census(2).await;
    let (_, unit) = c.unit("A").await;
    let id: Uuid = unit["id"].as_str().unwrap().parse().unwrap();
    let (one, other) = (authed_ctx(c.tenant), authed_ctx(c.tenant));
    let long = "x".repeat(2001);
    let approve = VoteAction::Approve;
    let gen1 = r#"{"generation":1}"#;
    let cases: Vec<Case<'_>> = vec![
        (&one, approve, "{}".into(), "1", false, 400, ""),
        (&one, approve, "not json".into(), "2", false, 400, ""),
        (
            &one,
            approve,
            r#"{"generation":7}"#.into(),
            "3",
            false,
            400,
            "GENERATION_MISMATCH",
        ),
        (
            &one,
            VoteAction::Reject,
            gen1.into(),
            "4",
            false,
            400,
            "NOTE_REQUIRED",
        ),
        (
            &one,
            approve,
            json!({"generation":1,"note":long}).to_string(),
            "5",
            false,
            400,
            "NOTE_TOO_LONG",
        ),
        (
            &c.author,
            approve,
            gen1.into(),
            "6",
            false,
            403,
            "SOD_VIOLATION",
        ),
        (
            &other,
            VoteAction::Withdraw,
            String::new(),
            "7",
            false,
            403,
            "NOT_SUBMITTER",
        ),
        (&one, approve, gen1.into(), "8", true, 403, "access denied"),
    ];
    for (who, action, body, n, denied, status, code) in cases {
        let (door, source) = if denied {
            (&c.denied_door, &c.denied_source)
        } else {
            (&c.door, &c.source)
        };
        let keys = (format!("d{n}"), format!("s{n}"));
        let (got, text) = both(
            door,
            source,
            who,
            id,
            action,
            &body,
            Some((keys.0.as_str(), keys.1.as_str())),
        )
        .await;
        assert_eq!(got, status, "{body}: {text}");
        assert!(text.contains(code), "{code}: {text}");
    }
    let (got, text) = both(
        &c.door,
        &c.source,
        &one,
        Uuid::new_v4(),
        approve,
        gen1,
        Some(("d9", "s9")),
    )
    .await;
    assert_eq!(got, 404, "{text}");
    let (got, text, _) = send(
        &c.door,
        &one,
        "POST",
        &format!("{UNITS}/{id}/approve"),
        gen1,
        Some("first"),
    )
    .await;
    assert_eq!(got, 200, "{text}");
    let (got, text) = both(
        &c.door,
        &c.source,
        &one,
        id,
        approve,
        gen1,
        Some(("d10", "s10")),
    )
    .await;
    assert_eq!(got, 409, "{text}");
    assert!(text.contains("DUPLICATE_VOTE"), "{text}");
    let (got, text) = both(
        &c.door,
        &c.source,
        &one,
        id,
        approve,
        r#"{"generation":1,"note":"another"}"#,
        Some(("first", "first")),
    )
    .await;
    assert_eq!(got, 409, "{text}");
    assert!(text.contains("IDEMPOTENCY_CONFLICT"), "{text}");
    let (got, text, _) = send(
        &c.door,
        &other,
        "POST",
        &format!("{UNITS}/{id}/approve"),
        gen1,
        Some("second"),
    )
    .await;
    assert_eq!(got, 200, "{text}");
    let (got, text) = both(
        &c.door,
        &c.source,
        &authed_ctx(c.tenant),
        id,
        approve,
        gen1,
        Some(("d11", "s11")),
    )
    .await;
    assert_eq!(got, 409, "{text}");
    assert!(text.contains("UNIT_ALREADY_DECIDED"), "{text}");
}

/// The SKU's content drifts under its pending unit: the next vote refreshes the unit.
async fn drift(c: &Census, sku: Uuid) {
    let (db, scope) = repo_connection(&c.dsn, c.tenant).await;
    let conn = db.conn().unwrap();
    let mut content = bss_products_sdk::models::SkuContent::from(
        &repo::find_sku(&conn, &scope, c.tenant, sku)
            .await
            .unwrap()
            .unwrap(),
    );
    content.description = "drift".into();
    repo::write_sku_content(
        &conn,
        &scope,
        c.tenant,
        sku,
        &content,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
}

/// `UNIT_STALE`: the door's answer on one unit and the source's on a twin are the same bytes but
/// for the unit's own path, `generation` included; the source's key replays the door's stored
/// answer.
#[tokio::test]
async fn unit_stale_reaches_the_inbox_with_its_generation() {
    let c = census(1).await;
    let ((sa, a), (sb, b)) = (c.unit("A").await, c.unit("B").await);
    drift(&c, sa).await;
    drift(&c, sb).await;
    let reviewer = authed_ctx(c.tenant);
    let (ida, idb) = (a["id"].as_str().unwrap(), b["id"].as_str().unwrap());
    let door = send(
        &c.door,
        &reviewer,
        "POST",
        &format!("{UNITS}/{ida}/approve"),
        r#"{"generation":1}"#,
        Some("stale-door"),
    )
    .await;
    assert_eq!(door.0, 400, "{}", door.1);
    let problem: Value = serde_json::from_str(&door.1).unwrap();
    assert_eq!(problem_code(&problem), "UNIT_STALE");
    assert_eq!(problem["context"]["generation"], 2, "{problem}");
    let ours = c
        .source
        .vote(
            &reviewer,
            idb.parse().unwrap(),
            VoteAction::Approve,
            VoteRequest {
                body: br#"{"generation":1}"#.to_vec(),
                idempotency_key: Some("stale-source".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(ours.status, 400);
    assert_eq!(
        String::from_utf8(ours.body).unwrap().replace(idb, ida),
        door.1,
        "the same refusal, its generation included"
    );
    let replay = c
        .source
        .vote(
            &reviewer,
            ida.parse().unwrap(),
            VoteAction::Approve,
            VoteRequest {
                body: br#"{"generation":1}"#.to_vec(),
                idempotency_key: Some("stale-door".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (replay.status, String::from_utf8(replay.body).unwrap()),
        (door.0, door.1)
    );
}

/// P-D-250, AP-D-4: the source votes under the door's own endpoint, so a vote through the inbox
/// and one through the door with the same key and body are ONE vote.
#[tokio::test]
async fn a_source_vote_and_a_door_vote_with_one_key_replay_once() {
    let c = census(2).await;
    let reviewer = authed_ctx(c.tenant);
    for (code, source_first) in [("A", true), ("B", false)] {
        let (_, unit) = c.unit(code).await;
        let id: Uuid = unit["id"].as_str().unwrap().parse().unwrap();
        let path = format!("{UNITS}/{id}/approve");
        let key = format!("one-vote-{code}");
        let through_source = || async {
            let r = c
                .source
                .vote(
                    &reviewer,
                    id,
                    VoteAction::Approve,
                    VoteRequest {
                        body: br#"{"generation":1}"#.to_vec(),
                        idempotency_key: Some(key.clone()),
                    },
                )
                .await
                .unwrap();
            (r.status, String::from_utf8(r.body).unwrap())
        };
        let through_door = || async {
            let (status, body, _) = send(
                &c.door,
                &reviewer,
                "POST",
                &path,
                r#"{"generation":1}"#,
                Some(&key),
            )
            .await;
            (status, body)
        };
        let (first, second) = if source_first {
            (through_source().await, through_door().await)
        } else {
            (through_door().await, through_source().await)
        };
        assert_eq!(first.0, 200, "{}", first.1);
        assert_eq!(second, first, "the second is the first's replay");
        let card = c.source.get(&reviewer, id, false).await.unwrap().unwrap();
        assert_eq!(card.decisions.len(), 1, "one vote");
    }
}

/// The page reads the same statements for 10 units and for 100 (P-D-224), every one in the list's
/// transaction; the counts read ONE statement, outside any transaction (R32). With no usage port
/// registered the impact reads nothing.
#[tokio::test]
async fn the_page_and_the_counts_read_fixed_statements() {
    let (db, _, _, dsn, recorder) = recorded_test_db().await;
    let c = census_on(1, db, dsn).await;
    let base = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() - time::Duration::days(2);
    let mut statements = Vec::new();
    let mut total = 0;
    for n in [10_i64, 100] {
        let units = (0..n)
            .map(|i| {
                (
                    ["sku_publish", "sku_change", "sku_retire"][usize::try_from(i % 3).unwrap()],
                    Uuid::new_v4(),
                    base + time::Duration::seconds(i + n * 1000),
                )
            })
            .collect();
        c.stored(units).await;
        total += usize::try_from(n).unwrap();
        recorder.clear();
        let page = c
            .source
            .page(
                &c.author,
                &SourcePageQuery {
                    narrowing: SourceNarrowing::default(),
                    order: Order::Desc,
                    limit: 200,
                    after: None,
                    impact: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.units.len(), total);
        let reads: Vec<_> = recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("products_"))
            })
            .collect();
        assert!(
            reads.iter().all(|q| q.in_tx),
            "the page reads in its transaction"
        );
        statements.push(reads.len());
        recorder.clear();
        c.source
            .counts(&c.author, &SourceNarrowing::default())
            .await
            .unwrap();
        let counted: Vec<_> = recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("products_"))
            })
            .collect();
        assert_eq!(counted.len(), 1, "one grouped statement: {counted:?}");
        assert!(
            !counted[0].in_tx,
            "the counts stay off the list's transaction"
        );
    }
    assert_eq!(
        statements[0], statements[1],
        "10 and 100 units: {statements:?}"
    );
    assert_eq!(statements[0], PAGE_STATEMENTS, "pinned: {statements:?}");
}

/// The page's statements on products' tables (P-D-224): the page, its units' decisions, their
/// items' authors.
const PAGE_STATEMENTS: usize = 3;
