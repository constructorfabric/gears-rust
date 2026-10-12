//! The approvals inbox's pricing source against pricing's own doors (D-490): the same request
//! through the door and through the source answers the same status, code and body bytes, for the
//! list, the counts, the card and every vote refusal, `UNIT_STALE`'s `generation` included. The
//! door is the router `module.rs` serves (the authoring routes, the gear's enforcer, then the
//! platform's error layer); a source refusal is rendered at the door's own path through that same
//! error layer.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;

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
use bss_pricing::api::rest::authoring::{AuthoringState, inbox_source::PricingApprovalSource};
use bss_pricing::infra::storage::{entity::price, repo::approval_repo::PricingApprovalStore};
use entry_support::{Fixture, Script};
use sea_orm::EntityTrait;
use serde_json::{Value, json};
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, SecureUpdateExt};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

/// A PDP that refuses everything: the grant every door checks first.
struct Deny;
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Deny {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        _: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, CanonicalError> {
        Ok(authz_resolver_sdk::EvaluationResponse {
            decision: false,
            context: authz_resolver_sdk::EvaluationResponseContext::default(),
        })
    }
}
fn deny() -> authz_resolver_sdk::PolicyEnforcer {
    authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Deny))
}

/// The router `module.rs` serves over `state` under `enforcer`.
fn served(state: Arc<AuthoringState>, enforcer: authz_resolver_sdk::PolicyEnforcer) -> Router {
    entry_support::production(state)
        .layer(Extension(enforcer))
        .layer(axum::middleware::from_fn(
            toolkit::api::canonical_error_middleware,
        ))
}

struct Census {
    door: Router,
    denied_door: Router,
    source: PricingApprovalSource,
    denied_source: PricingApprovalSource,
    f: Fixture,
    book: String,
    entry: Uuid,
}

async fn census(quorum: u32) -> Census {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let tenant = f.ctx.subject_tenant_id();
    let (book, _) = f.book().await;
    let book = book["id"].as_str().unwrap().to_owned();
    let (status, entry, _) = f
        .call(
            "POST",
            &format!("/price-books/{book}/entries"),
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":entry_support::policy_support::input()}),
            None,
            Some("entry"),
        )
        .await;
    assert_eq!(status, 201, "{entry}");
    let entry = entry["id"].as_str().unwrap().parse().unwrap();
    let c = Census {
        door: served(f.state.clone(), entry_support::enforcer_for(tenant)),
        denied_door: served(f.state.clone(), deny()),
        source: PricingApprovalSource::new(f.state.clone(), entry_support::enforcer_for(tenant)),
        denied_source: PricingApprovalSource::new(f.state.clone(), deny()),
        f,
        book,
        entry,
    };
    c.policy(quorum).await;
    c
}

impl Census {
    async fn policy(&self, quorum: u32) {
        let (_, _, tag) = self
            .f
            .call("GET", "/approval-policy", json!({}), None, None)
            .await;
        let put = self
            .f
            .call(
                "PUT",
                "/approval-policy",
                json!({"quorum":quorum}),
                Some(&tag),
                None,
            )
            .await;
        assert_eq!(put.0, 200, "{put:?}");
    }
    /// A draft price by `who`, effective from `from`, submitted by `who`: its unit and the price.
    async fn unit_by(&self, who: &SecurityContext, from: &str) -> (Value, Uuid) {
        let (status, draft, _) = self
            .f
            .call_as(
                who,
                "POST",
                &format!("/price-book-entries/{}/prices", self.entry),
                json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":from}),
                None,
                Some(&format!("draft-{from}")),
            )
            .await;
        assert_eq!(status, 201, "{draft}");
        let price = draft["items"][0]["id"].as_str().unwrap().to_owned();
        let (status, receipt, _) = self
            .f
            .call_as(
                who,
                "POST",
                &format!("/prices/{price}/submit"),
                json!({}),
                None,
                Some(&format!("submit-{from}")),
            )
            .await;
        assert_eq!(status, 201, "{receipt}");
        (receipt["unit"].clone(), price.parse().unwrap())
    }
    async fn unit(&self, from: &str) -> Value {
        self.unit_by(&self.f.ctx, from).await.0
    }
    /// Units written straight through the gear's own store, `submitted_at` as given.
    async fn stored(&self, units: Vec<(&'static str, OffsetDateTime)>) -> Vec<Uuid> {
        let tenant = self.f.ctx.subject_tenant_id();
        let store = PricingApprovalStore {
            scope: AccessScope::for_tenant(tenant),
            tenant_id: tenant,
        };
        let units: Vec<Unit> = units
            .into_iter()
            .map(|(kind, at)| Unit {
                id: Uuid::new_v4(),
                tenant_id: tenant,
                kind: kind.to_owned(),
                ref_type: if kind == "prices" {
                    "price_book".into()
                } else {
                    "plan_revision".into()
                },
                ref_id: if kind == "prices" {
                    self.book.parse().unwrap()
                } else {
                    Uuid::new_v4()
                },
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
        self.f
            .db
            .db()
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
}

/// `(status, body bytes as text, content type)` of one request through `app`.
async fn send(
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

/// A source refusal as the door's own error layer answers it at the door's path.
async fn rendered(path: &str, error: CanonicalError) -> (u16, String) {
    let app = Router::new()
        .fallback(move || {
            let error = error.clone();
            async move { error.into_response() }
        })
        .layer(axum::middleware::from_fn(
            toolkit::api::canonical_error_middleware,
        ));
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
fn as_the_door(ours: &Value, door: &Value) -> Value {
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

/// The source's unit is the door's item byte for byte; what the inbox adds is the source name and
/// a null `subject_live` (D-490).
fn same_unit(door: &Value, unit: &InboxUnit) {
    let ours = serde_json::to_value(unit).unwrap();
    assert_eq!(ours["source"], "pricing");
    assert_eq!(ours["subject_live"], Value::Null);
    assert_eq!(
        serde_json::to_string(&as_the_door(&ours, door)).unwrap(),
        serde_json::to_string(door).unwrap(),
    );
}

fn narrowing(query: &[(&str, &str)]) -> SourceNarrowing {
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

fn query_string(query: &[(&str, &str)]) -> String {
    query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn key_of(item: &Value) -> SortKey {
    SortKey {
        submitted_at: OffsetDateTime::parse(
            item["submitted_at"].as_str().unwrap(),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap(),
        id: item["id"].as_str().unwrap().parse().unwrap(),
    }
}

const UNITS: &str = "/bss-pricing/v1/approval-units";

/// Every page of the list door under `query`, in `order`, two at a time, against the source's
/// page after the last key of the page before it: the same units, byte for byte, and the same
/// "more".
async fn walk(c: &Census, query: &[(&str, &str)], order: Order, impact: bool) -> usize {
    let mut pairs = query.to_vec();
    if order == Order::Desc {
        pairs.push(("$orderby", "submitted_at%20desc"));
    }
    if !impact {
        pairs.push(("impact", "false"));
    }
    let mut path = format!("{UNITS}?limit=2&{}", query_string(&pairs));
    // A continuation repeats the narrowing (its hash is in the cursor), the page size and
    // `impact`; the cursor carries the order.
    let mut again = query.to_vec();
    if !impact {
        again.push(("impact", "false"));
    }
    let mut after = None;
    let mut seen = 0;
    loop {
        let (status, body, _) = send(&c.door, &c.f.ctx, "GET", &path, "", None).await;
        assert_eq!(status, 200, "{path}: {body}");
        let door: Value = serde_json::from_str(&body).unwrap();
        let page = c
            .source
            .page(
                &c.f.ctx,
                &SourcePageQuery {
                    narrowing: narrowing(query),
                    order,
                    limit: 2,
                    after,
                    impact,
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
        path = format!("{UNITS}?limit=2&{}&cursor={next}", query_string(&again));
    }
}

#[tokio::test]
async fn the_page_is_the_list_doors_page_in_both_orders_after_any_key() {
    let c = census(1).await;
    for from in ["2031-03-01", "2031-04-01", "2031-05-01"] {
        c.unit(from).await;
    }
    let base = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() - time::Duration::days(1);
    c.stored(vec![
        ("plan_revision", base),
        ("prices", base + time::Duration::seconds(1)),
        ("plan_revision", base + time::Duration::seconds(2)),
    ])
    .await;
    let book = c.book.clone();
    for (query, expected) in [
        (vec![], 6),
        (vec![("state", "pending")], 6),
        (vec![("kind", "plan_revision")], 2),
        (vec![("kind", "prices"), ("book_id", book.as_str())], 4),
        (vec![("ref_id", book.as_str())], 4),
        (vec![("state", "approved")], 0),
    ] {
        for order in [Order::Asc, Order::Desc] {
            for impact in [true, false] {
                assert_eq!(walk(&c, &query, order, impact).await, expected, "{query:?}");
            }
        }
    }
}

#[tokio::test]
async fn the_list_and_counts_refusals_are_the_doors() {
    let c = census(1).await;
    let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    for (query, sees) in [
        (vec![("state", "bogus")], 400),
        (vec![("kind", "sku_publish"), ("state", "bogus")], 400),
        (vec![("ref_id", a.as_str()), ("book_id", b.as_str())], 400),
    ] {
        for door_path in [UNITS.to_owned(), format!("{UNITS}/counts")] {
            let path = format!("{door_path}?{}", query_string(&query));
            let door = send(&c.door, &c.f.ctx, "GET", &path, "", None).await;
            assert_eq!(door.0, sees, "{path}: {}", door.1);
            let error = if door_path == UNITS {
                c.source
                    .page(
                        &c.f.ctx,
                        &SourcePageQuery {
                            narrowing: narrowing(&query),
                            order: Order::Asc,
                            limit: 2,
                            after: None,
                            impact: true,
                        },
                    )
                    .await
                    .unwrap_err()
            } else {
                c.source
                    .counts(&c.f.ctx, &narrowing(&query))
                    .await
                    .unwrap_err()
            };
            assert_eq!(rendered(&path, error).await, (door.0, door.1), "{path}");
        }
    }
    // The grant: a caller the PDP refuses.
    for door_path in [UNITS.to_owned(), format!("{UNITS}/counts")] {
        let door = send(&c.denied_door, &c.f.ctx, "GET", &door_path, "", None).await;
        assert_eq!(door.0, 403, "{}", door.1);
        let error = if door_path == UNITS {
            c.denied_source
                .page(
                    &c.f.ctx,
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
                .counts(&c.f.ctx, &SourceNarrowing::default())
                .await
                .unwrap_err()
        };
        assert_eq!(rendered(&door_path, error).await, (door.0, door.1));
    }
}

/// AP-D-2: a kind pricing does not record is an empty page and zero counts, decided in the source
/// before any door; the door itself refuses it.
#[tokio::test]
async fn a_kind_pricing_does_not_record_is_empty_not_the_doors_400() {
    let c = census(1).await;
    c.unit("2031-03-01").await;
    for kind in ["sku_publish", "sku_retire", "bogus"] {
        let door = send(
            &c.door,
            &c.f.ctx,
            "GET",
            &format!("{UNITS}?kind={kind}"),
            "",
            None,
        )
        .await;
        assert_eq!(door.0, 400, "the door refuses it: {}", door.1);
        let n = narrowing(&[("kind", kind)]);
        let page = c
            .source
            .page(
                &c.f.ctx,
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
        assert!(page.units.is_empty() && !page.has_more, "{kind}");
        assert_eq!(
            c.source.counts(&c.f.ctx, &n).await.unwrap(),
            SourceCounts::default(),
            "{kind}"
        );
    }
}

#[tokio::test]
async fn the_counts_are_the_counts_doors() {
    let c = census(1).await;
    c.unit("2031-03-01").await;
    c.unit("2031-04-01").await;
    let base = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() - time::Duration::days(1);
    c.stored(vec![("plan_revision", base)]).await;
    let book = c.book.clone();
    for query in [
        vec![],
        vec![("state", "pending")],
        vec![("state", "approved")],
        vec![("kind", "plan_revision")],
        vec![("kind", "prices"), ("book_id", book.as_str())],
        vec![("ref_id", book.as_str())],
    ] {
        let path = format!("{UNITS}/counts?{}", query_string(&query));
        let (status, body, _) = send(&c.door, &c.f.ctx, "GET", &path, "", None).await;
        assert_eq!(status, 200, "{body}");
        let door: Value = serde_json::from_str(&body).unwrap();
        let ours =
            serde_json::to_value(c.source.counts(&c.f.ctx, &narrowing(&query)).await.unwrap())
                .unwrap();
        assert_eq!(
            serde_json::to_string(&as_the_door(&ours, &door)).unwrap(),
            serde_json::to_string(&door).unwrap(),
            "{path}"
        );
        for other in ["sku_publish", "sku_change", "sku_retire"] {
            assert_eq!(ours["by_kind"][other], 0, "{path}");
        }
    }
}

#[tokio::test]
async fn the_card_is_the_card_doors_and_a_miss_is_none() {
    let c = census(1).await;
    let unit = c.unit("2031-03-01").await;
    let id: Uuid = unit["id"].as_str().unwrap().parse().unwrap();
    let path = format!("{UNITS}/{id}");
    let (status, body, _) = send(&c.door, &c.f.ctx, "GET", &path, "", None).await;
    assert_eq!(status, 200, "{body}");
    let door: Value = serde_json::from_str(&body).unwrap();
    assert!(door["impact"].is_object(), "{door}");
    let ours = c.source.get(&c.f.ctx, id, true).await.unwrap().unwrap();
    same_unit(&door, &ours);
    let without = c.source.get(&c.f.ctx, id, false).await.unwrap().unwrap();
    assert_eq!(without.impact, None, "impact=false skips it");
    // A unit this tenant does not hold: the door's 404 is the source's miss.
    let missing = Uuid::new_v4();
    let door = send(
        &c.door,
        &c.f.ctx,
        "GET",
        &format!("{UNITS}/{missing}"),
        "",
        None,
    )
    .await;
    assert_eq!(door.0, 404, "{}", door.1);
    assert_eq!(c.source.get(&c.f.ctx, missing, true).await.unwrap(), None);
    let stranger = entry_support::user_of(Uuid::new_v4());
    assert_eq!(
        c.source.get(&stranger, id, true).await.unwrap(),
        None,
        "another tenant's unit is a miss"
    );
    // The grant.
    let door = send(&c.denied_door, &c.f.ctx, "GET", &path, "", None).await;
    assert_eq!(door.0, 403, "{}", door.1);
    let error = c.denied_source.get(&c.f.ctx, id, true).await.unwrap_err();
    assert_eq!(rendered(&path, error).await, (door.0, door.1));
}

/// One vote through the door and the same through the source, each under its own key: the same
/// status, content type and body bytes.
async fn both(
    c: &Census,
    who: &SecurityContext,
    id: Uuid,
    action: VoteAction,
    body: &str,
    keys: Option<(&str, &str)>,
    denied: bool,
) -> (u16, String) {
    let path = format!("{UNITS}/{id}/{}", action.as_str());
    let door_app = if denied { &c.denied_door } else { &c.door };
    let door = send(door_app, who, "POST", &path, body, keys.map(|k| k.0)).await;
    let source = if denied { &c.denied_source } else { &c.source };
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

/// One vote refusal: who votes, how, with which body and keys (door, source), whether the PDP
/// refuses, and the status and code the door answers.
type Case<'a> = (
    &'a SecurityContext,
    VoteAction,
    String,
    Option<(&'a str, &'a str)>,
    bool,
    u16,
    &'a str,
);

#[tokio::test]
async fn every_vote_refusal_is_the_vote_doors_byte_for_byte() {
    let c = census(2).await;
    let author = c.f.ctx.clone();
    let unit = c.unit("2031-03-01").await;
    let id: Uuid = unit["id"].as_str().unwrap().parse().unwrap();
    let (one, other) = (c.f.user(), c.f.user());
    let long = "x".repeat(2001);
    let cases: Vec<Case<'_>> = vec![
        (
            &one,
            VoteAction::Approve,
            "{}".into(),
            Some(("d1", "s1")),
            false,
            400,
            "generation",
        ),
        (
            &one,
            VoteAction::Approve,
            "not json".into(),
            Some(("d2", "s2")),
            false,
            400,
            "expected",
        ),
        (
            &one,
            VoteAction::Approve,
            r#"{"generation":1}"#.into(),
            None,
            false,
            400,
            "Idempotency-Key",
        ),
        (
            &one,
            VoteAction::Approve,
            r#"{"generation":7}"#.into(),
            Some(("d3", "s3")),
            false,
            400,
            "GENERATION_MISMATCH",
        ),
        (
            &one,
            VoteAction::Reject,
            r#"{"generation":1}"#.into(),
            Some(("d4", "s4")),
            false,
            400,
            "NOTE_REQUIRED",
        ),
        (
            &one,
            VoteAction::Approve,
            json!({"generation":1,"note":long}).to_string(),
            Some(("d5", "s5")),
            false,
            400,
            "NOTE_TOO_LONG",
        ),
        (
            &author,
            VoteAction::Withdraw,
            r#"{"x":1}"#.into(),
            Some(("d6", "s6")),
            false,
            400,
            "BODY_UNEXPECTED",
        ),
        (
            &author,
            VoteAction::Approve,
            r#"{"generation":1}"#.into(),
            Some(("d7", "s7")),
            false,
            403,
            "SOD_VIOLATION",
        ),
        (
            &other,
            VoteAction::Withdraw,
            "{}".into(),
            Some(("d8", "s8")),
            false,
            403,
            "NOT_SUBMITTER",
        ),
        (
            &one,
            VoteAction::Approve,
            r#"{"generation":1}"#.into(),
            Some(("d9", "s9")),
            true,
            403,
            "access denied",
        ),
    ];
    for (who, action, body, keys, denied, status, code) in cases {
        let (got, text) = both(&c, who, id, action, &body, keys, denied).await;
        assert_eq!(got, status, "{body}: {text}");
        assert!(text.contains(code), "{code}: {text}");
    }
    // A unit this tenant does not hold.
    let (got, text) = both(
        &c,
        &one,
        Uuid::new_v4(),
        VoteAction::Approve,
        r#"{"generation":1}"#,
        Some(("d10", "s10")),
        false,
    )
    .await;
    assert_eq!(got, 404, "{text}");
    // A second vote by one reviewer of this generation.
    let (got, text, _) = send(
        &c.door,
        &one,
        "POST",
        &format!("{UNITS}/{id}/approve"),
        r#"{"generation":1}"#,
        Some("first"),
    )
    .await;
    assert_eq!(got, 200, "{text}");
    let (got, text) = both(
        &c,
        &one,
        id,
        VoteAction::Approve,
        r#"{"generation":1}"#,
        Some(("d11", "s11")),
        false,
    )
    .await;
    assert_eq!(got, 409, "{text}");
    assert!(text.contains("DUPLICATE_VOTE"), "{text}");
    // The same key with another body.
    let (got, text) = both(
        &c,
        &one,
        id,
        VoteAction::Approve,
        r#"{"generation":1,"note":"another"}"#,
        Some(("first", "first")),
        false,
    )
    .await;
    assert_eq!(got, 409, "{text}");
    assert!(text.contains("IDEMPOTENCY_CONFLICT"), "{text}");
    // A decided unit.
    let (got, text, _) = send(
        &c.door,
        &other,
        "POST",
        &format!("{UNITS}/{id}/approve"),
        r#"{"generation":1}"#,
        Some("second"),
    )
    .await;
    assert_eq!(got, 200, "{text}");
    let (got, text) = both(
        &c,
        &c.f.user(),
        id,
        VoteAction::Approve,
        r#"{"generation":1}"#,
        Some(("d12", "s12")),
        false,
    )
    .await;
    assert_eq!(got, 409, "{text}");
    assert!(text.contains("UNIT_ALREADY_DECIDED"), "{text}");
}

/// The pending price's money changes under the unit: the next vote refreshes it.
async fn drift(c: &Census, price: Uuid) {
    let tenant = c.f.ctx.subject_tenant_id();
    price::Entity::update_many()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .col_expr(
            price::Column::PriceJson,
            sea_orm::sea_query::Expr::value(json!({"rate":"0.11"})),
        )
        .filter(sea_orm::Condition::all().add(sea_orm::ColumnTrait::eq(&price::Column::Id, price)))
        .exec(&c.f.db.conn().unwrap())
        .await
        .unwrap();
}

/// `UNIT_STALE` (D-470's refresh): the door's answer on one unit and the source's on a twin are the
/// same bytes but for the unit's own path, `generation` included; and the source's key replays the
/// door's stored answer.
#[tokio::test]
async fn unit_stale_reaches_the_inbox_with_its_generation() {
    let c = census(1).await;
    let author = c.f.ctx.clone();
    let ((a, pa), (b, pb)) = (
        c.unit_by(&author, "2031-03-01").await,
        c.unit_by(&author, "2031-04-01").await,
    );
    drift(&c, pa).await;
    drift(&c, pb).await;
    let reviewer = c.f.user();
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
    assert!(door.1.contains("UNIT_STALE"), "{}", door.1);
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
    // The door's key, through the source: its stored answer, byte for byte.
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

/// D-490, AP-D-4: the source votes under the door's own endpoint, so a vote through the inbox and
/// one through the door with the same key and body are ONE vote: the second replays the first.
#[tokio::test]
async fn a_source_vote_and_a_door_vote_with_one_key_replay_once() {
    let c = census(2).await;
    let reviewer = c.f.user();
    for (from, source_first) in [("2031-03-01", true), ("2031-04-01", false)] {
        let unit = c.unit(from).await;
        let id: Uuid = unit["id"].as_str().unwrap().parse().unwrap();
        let path = format!("{UNITS}/{id}/approve");
        let key = format!("one-vote-{from}");
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

/// The page reads the same statements for 10 units and for 100 (set-based, D-458), every one in
/// the list's transaction; the counts read ONE statement, outside any transaction (R32).
#[tokio::test]
async fn the_page_and_the_counts_read_fixed_statements() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let f = Fixture::on(db, tenant, dsn, Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let c = Census {
        door: served(f.state.clone(), entry_support::enforcer_for(tenant)),
        denied_door: served(f.state.clone(), deny()),
        source: PricingApprovalSource::new(f.state.clone(), entry_support::enforcer_for(tenant)),
        denied_source: PricingApprovalSource::new(f.state.clone(), deny()),
        f,
        book: book["id"].as_str().unwrap().to_owned(),
        entry: Uuid::nil(),
    };
    let base = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() - time::Duration::days(2);
    let mut statements = Vec::new();
    for n in [10_i64, 100] {
        let units = (0..n)
            .map(|i| {
                (
                    if i % 2 == 0 {
                        "prices"
                    } else {
                        "plan_revision"
                    },
                    base + time::Duration::seconds(i + n * 1000),
                )
            })
            .collect();
        c.stored(units).await;
        recorder.clear();
        let page = c
            .source
            .page(
                &c.f.ctx,
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
        assert_eq!(
            page.units.len(),
            usize::try_from(if n == 10 { 10 } else { 110 }).unwrap()
        );
        let events = recorder.events();
        let reads: Vec<_> = events
            .iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("pricing_"))
            })
            .collect();
        assert!(
            reads.iter().all(|q| q.in_tx),
            "the page reads in its transaction"
        );
        statements.push(reads.len());
        recorder.clear();
        c.source
            .counts(&c.f.ctx, &SourceNarrowing::default())
            .await
            .unwrap();
        let counted: Vec<_> = recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("pricing_"))
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

/// The page's statements on pricing's tables (D-458): the page, its units' items, their
/// decisions. These units carry no item, so their impact names no entry and no plan is read.
const PAGE_STATEMENTS: usize = 3;
