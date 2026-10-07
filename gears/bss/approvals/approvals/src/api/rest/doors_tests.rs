#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bss_approvals_sdk::{
    ApprovalSourceV1, InboxUnit, SourceCounts, SourceNarrowing, SourcePage, SourcePageQuery,
    UnitState, VoteAction, VoteRequest, VoteResponse,
};
use toolkit::ClientHub;
use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
use toolkit::client_hub::ClientScope;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::Error as ODataError;
use tower::ServiceExt;
use uuid::Uuid;

use super::routes::router;
use crate::api::ApiState;
use crate::domain::error;
use crate::test_support::{self, page_after};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Serve,
    Forbidden,
    Unavailable,
    Reject,
}

struct Fake {
    units: Mutex<Vec<InboxUnit>>,
    mode: Mutex<Mode>,
    vote: Mutex<VoteResponse>,
    seen: Mutex<Option<(VoteAction, VoteRequest)>>,
    last_page: Mutex<Option<SourcePageQuery>>,
    /// The actors this source declares are not people.
    system: Vec<Uuid>,
}

impl Fake {
    fn serving(units: Vec<InboxUnit>) -> Self {
        Self {
            units: Mutex::new(units),
            mode: Mutex::new(Mode::Serve),
            vote: Mutex::new(VoteResponse {
                status: 200,
                headers: Vec::new(),
                body: Vec::new(),
            }),
            seen: Mutex::new(None),
            last_page: Mutex::new(None),
            system: Vec::new(),
        }
    }

    fn mode_error(&self) -> Result<(), CanonicalError> {
        match *self.mode.lock().unwrap() {
            Mode::Serve => Ok(()),
            Mode::Forbidden => Err(error::forbidden()),
            Mode::Unavailable => Err(CanonicalError::service_unavailable().create()),
            Mode::Reject => Err(ODataError::InvalidFilter("state".into()).into()),
        }
    }
}

#[async_trait]
impl ApprovalSourceV1 for Fake {
    fn system_actors(&self) -> &[Uuid] {
        &self.system
    }

    async fn page(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        q: &SourcePageQuery,
    ) -> Result<SourcePage, CanonicalError> {
        let result = (|| {
            self.mode_error()?;
            *self.last_page.lock().unwrap() = Some(q.clone());
            let kept: Vec<InboxUnit> = self
                .units
                .lock()
                .unwrap()
                .iter()
                .filter(|unit| keeps(unit, &q.narrowing))
                .cloned()
                .collect();
            let (units, has_more) = page_after(&kept, q.order, q.limit, q.after);
            Ok(SourcePage { units, has_more })
        })();
        std::future::ready(result).await
    }

    async fn counts(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        n: &SourceNarrowing,
    ) -> Result<SourceCounts, CanonicalError> {
        let result = (|| {
            self.mode_error()?;
            let mut counts = SourceCounts::default();
            for unit in self.units.lock().unwrap().iter() {
                if !keeps(unit, n) {
                    continue;
                }
                match unit.state {
                    UnitState::Pending => counts.by_state.pending += 1,
                    UnitState::Approved => counts.by_state.approved += 1,
                    UnitState::Rejected => counts.by_state.rejected += 1,
                    UnitState::Withdrawn => counts.by_state.withdrawn += 1,
                }
                match unit.kind {
                    bss_approvals_sdk::InboxKind::Prices => counts.by_kind.prices += 1,
                    bss_approvals_sdk::InboxKind::PlanRevision => {
                        counts.by_kind.plan_revision += 1;
                    }
                    bss_approvals_sdk::InboxKind::SkuPublish => counts.by_kind.sku_publish += 1,
                    bss_approvals_sdk::InboxKind::SkuChange => counts.by_kind.sku_change += 1,
                    bss_approvals_sdk::InboxKind::SkuRetire => counts.by_kind.sku_retire += 1,
                }
                counts.total += 1;
            }
            Ok(counts)
        })();
        std::future::ready(result).await
    }

    async fn get(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        id: Uuid,
        _impact: bool,
    ) -> Result<Option<InboxUnit>, CanonicalError> {
        let result = (|| {
            self.mode_error()?;
            Ok(self
                .units
                .lock()
                .unwrap()
                .iter()
                .find(|unit| unit.id == id)
                .cloned())
        })();
        std::future::ready(result).await
    }

    async fn vote(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _id: Uuid,
        action: VoteAction,
        request: VoteRequest,
    ) -> Result<VoteResponse, CanonicalError> {
        let result = (|| {
            self.mode_error()?;
            *self.seen.lock().unwrap() = Some((action, request));
            Ok(self.vote.lock().unwrap().clone())
        })();
        std::future::ready(result).await
    }
}

fn keeps(unit: &InboxUnit, narrowing: &SourceNarrowing) -> bool {
    if let Some(state) = &narrowing.state
        && unit.state.as_str() != state
    {
        return false;
    }
    if let Some(kind) = &narrowing.kind
        && unit.kind.as_str() != kind
    {
        return false;
    }
    if let Some(id) = narrowing.ref_id
        && unit.ref_id != id
    {
        return false;
    }
    true
}

fn register(name: &str, fake: Arc<Fake>, hub: &ClientHub) {
    hub.register_scoped::<dyn ApprovalSourceV1>(ClientScope::new(name), fake);
}

fn inbox(names: &[&str], fakes: &[(&str, Arc<Fake>)]) -> Router {
    let hub = Arc::new(ClientHub::new());
    for (name, fake) in fakes {
        register(name, fake.clone(), &hub);
    }
    let state = Arc::new(ApiState::new(
        names.iter().map(|name| (*name).to_owned()).collect(),
        hub,
    ));
    router(state, &OpenApiRegistryImpl::new())
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: &[u8],
    auth: bool,
) -> axum::response::Response {
    call_with(app, method, uri, body, auth, None).await
}

async fn call_with(
    app: &Router,
    method: &str,
    uri: &str,
    body: &[u8],
    auth: bool,
    idempotency: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::from(body.to_vec()))
        .unwrap();
    if auth {
        request.extensions_mut().insert(test_support::caller());
    }
    if let Some(key) = idempotency {
        request
            .headers_mut()
            .insert("Idempotency-Key", key.parse().unwrap());
    }
    app.clone().oneshot(request).await.unwrap()
}

async fn bytes(response: axum::response::Response) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, body.to_vec())
}

#[tokio::test]
async fn the_list_merges_pages_and_the_counts_sum_the_readable_sources() {
    let pricing = Arc::new(Fake::serving(vec![
        test_support::unit("pricing", 1, 1),
        test_support::unit("pricing", 3, 3),
    ]));
    let products = Arc::new(Fake::serving(vec![test_support::unit("products", 2, 2)]));
    let app = inbox(
        &["pricing", "products"],
        &[("pricing", pricing.clone()), ("products", products.clone())],
    );
    let mut seen = Vec::new();
    let mut cursor = String::new();
    for _ in 0..5 {
        let uri = if cursor.is_empty() {
            "/bss-approvals/v1/approval-units?limit=2".to_owned()
        } else {
            format!("/bss-approvals/v1/approval-units?limit=2&cursor={cursor}")
        };
        let (status, _, body) = bytes(call(&app, "GET", &uri, b"", true).await).await;
        assert_eq!(status, StatusCode::OK);
        let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let sources = page["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["status"], "ok");
        for item in page["items"].as_array().unwrap() {
            seen.push(item["id"].as_str().unwrap().to_owned());
        }
        match page.get("next_cursor").and_then(|value| value.as_str()) {
            Some(next) => cursor = next.to_owned(),
            None => break,
        }
    }
    assert_eq!(seen.len(), 3);
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            "/bss-approvals/v1/approval-units/counts",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let counts: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(counts["total"], 3);
    assert_eq!(counts["by_state"]["pending"], 3);
    assert_eq!(counts["by_kind"]["prices"], 3);
    assert_eq!(counts["sources"][0]["name"], "pricing");
}

#[tokio::test]
async fn a_forbidden_source_is_omitted_and_named_and_a_down_source_is_unavailable() {
    let pricing = Arc::new(Fake::serving(vec![test_support::unit("pricing", 1, 1)]));
    let products = Arc::new(Fake::serving(vec![test_support::unit("products", 2, 2)]));
    *products.mode.lock().unwrap() = Mode::Forbidden;
    let app = inbox(
        &["pricing", "products"],
        &[("pricing", pricing), ("products", products)],
    );
    let (status, _, body) =
        bytes(call(&app, "GET", "/bss-approvals/v1/approval-units", b"", true).await).await;
    assert_eq!(status, StatusCode::OK);
    let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["sources"][1]["status"], "forbidden");
    assert_eq!(page["sources"][1]["name"], "products");

    let down = Arc::new(Fake::serving(Vec::new()));
    *down.mode.lock().unwrap() = Mode::Unavailable;
    let down_app = inbox(
        &["pricing", "products"],
        &[
            (
                "pricing",
                Arc::new(Fake::serving(vec![test_support::unit("pricing", 1, 1)])),
            ),
            ("products", down),
        ],
    );
    let (status, _, body) = bytes(
        call(
            &down_app,
            "GET",
            "/bss-approvals/v1/approval-units",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["sources"][1]["status"], "unavailable");

    let both = inbox(
        &["pricing", "products"],
        &[
            ("pricing", {
                let fake = Arc::new(Fake::serving(Vec::new()));
                *fake.mode.lock().unwrap() = Mode::Unavailable;
                fake
            }),
            ("products", {
                let fake = Arc::new(Fake::serving(Vec::new()));
                *fake.mode.lock().unwrap() = Mode::Unavailable;
                fake
            }),
        ],
    );
    let (status, _, body) =
        bytes(call(&both, "GET", "/bss-approvals/v1/approval-units", b"", true).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("SOURCE_UNAVAILABLE"));
    assert!(text.contains("products"));
    assert!(text.contains("pricing"));

    let missing_app = inbox(
        &["pricing", "products"],
        &[("pricing", Arc::new(Fake::serving(Vec::new())))],
    );
    let (status, _, body) = bytes(
        call(
            &missing_app,
            "GET",
            "/bss-approvals/v1/approval-units/counts",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let counts: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(counts["total"], 0);
    assert_eq!(counts["sources"][1]["name"], "products");
    assert_eq!(counts["sources"][1]["status"], "unavailable");
}

#[tokio::test]
async fn a_down_source_stays_on_the_page_as_unavailable() {
    let pricing = Arc::new(Fake::serving(vec![
        test_support::unit("pricing", 1, 1),
        test_support::unit("pricing", 3, 3),
    ]));
    let products = Arc::new(Fake::serving(vec![test_support::unit("products", 2, 2)]));
    *products.mode.lock().unwrap() = Mode::Unavailable;
    let app = inbox(
        &["pricing", "products"],
        &[("pricing", pricing), ("products", products.clone())],
    );
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            "/bss-approvals/v1/approval-units?limit=1",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["sources"][1]["name"], "products");
    assert_eq!(page["sources"][1]["status"], "unavailable");
    let cursor = page["next_cursor"].as_str().expect("the walk continues");
    *products.mode.lock().unwrap() = Mode::Serve;
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            &format!("/bss-approvals/v1/approval-units?limit=1&cursor={cursor}"),
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let next: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(next["sources"][1]["status"], "unavailable");
    assert!(products.last_page.lock().unwrap().is_none());
}

#[tokio::test]
async fn every_source_forbidden_is_403_and_a_bad_narrowing_is_that_400() {
    let pricing = Arc::new(Fake::serving(Vec::new()));
    *pricing.mode.lock().unwrap() = Mode::Forbidden;
    let products = Arc::new(Fake::serving(Vec::new()));
    *products.mode.lock().unwrap() = Mode::Forbidden;
    let app = inbox(
        &["pricing", "products"],
        &[("pricing", pricing), ("products", products)],
    );
    let (status, _, body) =
        bytes(call(&app, "GET", "/bss-approvals/v1/approval-units", b"", true).await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains("pricing"));
    assert!(!text.contains("products"));
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            "/bss-approvals/v1/approval-units/counts",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains("pricing"), "{text}");
    assert!(!text.contains("products"), "{text}");

    let pricing = Arc::new(Fake::serving(Vec::new()));
    *pricing.mode.lock().unwrap() = Mode::Reject;
    let reject_app = inbox(&["pricing"], &[("pricing", pricing)]);
    let (status, _, _) = bytes(
        call(
            &reject_app,
            "GET",
            "/bss-approvals/v1/approval-units?state=nope",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn impact_false_and_book_id_are_forwarded_and_the_cursor_rules_hold() {
    let pricing = Arc::new(Fake::serving(vec![test_support::unit("pricing", 1, 1)]));
    let app = inbox(&["pricing"], &[("pricing", pricing.clone())]);
    let (status, _, _) = bytes(
        call(
            &app,
            "GET",
            "/bss-approvals/v1/approval-units?impact=false&book_id=00000000-0000-0000-0000-000000000009",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let query = pricing.last_page.lock().unwrap().clone().unwrap();
    assert!(!query.impact);
    assert_eq!(query.narrowing.book_id, Some(Uuid::from_u128(9)));

    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            "/bss-approvals/v1/approval-units?cursor=aaaa&$orderby=submitted_at%20asc",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        String::from_utf8(body)
            .unwrap()
            .contains("ORDER_WITH_CURSOR")
    );
}

#[tokio::test]
async fn the_card_outcomes_and_votes_pass_through_byte_for_byte() {
    let unit = test_support::unit("pricing", 1, 7);
    let pricing = Arc::new(Fake::serving(vec![unit.clone()]));
    let products = Arc::new(Fake::serving(Vec::new()));
    let app = inbox(
        &["pricing", "products"],
        &[("pricing", pricing.clone()), ("products", products.clone())],
    );
    let id = unit.id;
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            &format!("/bss-approvals/v1/approval-units/{id}"),
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let card: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(card["source"], "pricing");
    assert!(card.get("version").is_none());

    *products.mode.lock().unwrap() = Mode::Unavailable;
    let (status, _, _) = bytes(
        call(
            &app,
            "GET",
            &format!("/bss-approvals/v1/approval-units/{id}"),
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let products_unit = test_support::unit("products", 1, 7);
    *products.mode.lock().unwrap() = Mode::Serve;
    products.units.lock().unwrap().push(products_unit);
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            &format!("/bss-approvals/v1/approval-units/{id}"),
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("pricing"));
    assert!(text.contains("products"));

    products.units.lock().unwrap().clear();
    *pricing.mode.lock().unwrap() = Mode::Forbidden;
    *products.mode.lock().unwrap() = Mode::Forbidden;
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            &format!("/bss-approvals/v1/approval-units/{id}"),
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let text = String::from_utf8(body).unwrap();
    assert!(!text.contains("pricing"));
    assert!(!text.contains("products"));

    *pricing.mode.lock().unwrap() = Mode::Serve;
    *products.mode.lock().unwrap() = Mode::Serve;
    let missing = Uuid::from_u128(99);
    let (status, _, _) = bytes(
        call(
            &app,
            "GET",
            &format!("/bss-approvals/v1/approval-units/{missing}"),
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let cases: &[(u16, &str, &[u8])] = &[
        (
            200,
            "application/json",
            br#"{"have":1,"need":1,"outcome":"approved"}"#,
        ),
        (
            400,
            "application/problem+json",
            br#"{"code":"GENERATION_REQUIRED"}"#,
        ),
        (
            400,
            "application/problem+json",
            br#"{"code":"GENERATION_MISMATCH","generation":2}"#,
        ),
        (
            400,
            "application/problem+json",
            br#"{"code":"UNIT_STALE","generation":4}"#,
        ),
        (
            400,
            "application/problem+json",
            br#"{"code":"NOTE_REQUIRED"}"#,
        ),
        (
            400,
            "application/problem+json",
            br#"{"code":"NOTE_TOO_LONG"}"#,
        ),
        (
            400,
            "application/problem+json",
            br#"{"code":"BODY_UNEXPECTED"}"#,
        ),
        (
            403,
            "application/problem+json",
            br#"{"code":"SOD_VIOLATION"}"#,
        ),
        (
            404,
            "application/problem+json",
            br#"{"code":"UNIT_NOT_FOUND"}"#,
        ),
        (
            409,
            "application/problem+json",
            br#"{"code":"DUPLICATE_VOTE"}"#,
        ),
        (
            409,
            "application/problem+json",
            br#"{"code":"UNIT_ALREADY_DECIDED"}"#,
        ),
        (
            409,
            "application/problem+json",
            br#"{"code":"IDEMPOTENCY_CONFLICT"}"#,
        ),
        (
            503,
            "application/problem+json",
            br#"{"code":"UNAVAILABLE"}"#,
        ),
    ];
    let payload = br#"{"generation":1,"note":"keep"}"#;
    for action in ["approve", "reject", "withdraw"] {
        let expected = match action {
            "approve" => VoteAction::Approve,
            "reject" => VoteAction::Reject,
            "withdraw" => VoteAction::Withdraw,
            _ => unreachable!("the loop names the three vote doors"),
        };
        for (status, content_type, body) in cases {
            *pricing.seen.lock().unwrap() = None;
            *pricing.vote.lock().unwrap() = VoteResponse {
                status: *status,
                headers: vec![
                    ("content-type".to_owned(), (*content_type).to_owned()),
                    ("x-gear".to_owned(), "pricing".to_owned()),
                ],
                body: body.to_vec(),
            };
            let (got, headers, got_body) = bytes(
                call_with(
                    &app,
                    "POST",
                    &format!("/bss-approvals/v1/approval-units/{id}/{action}"),
                    payload,
                    true,
                    Some("key-1"),
                )
                .await,
            )
            .await;
            assert_eq!(got.as_u16(), *status, "{action}");
            assert_eq!(got_body, *body, "{action}");
            assert_eq!(
                headers.get("x-gear").and_then(|value| value.to_str().ok()),
                Some("pricing")
            );
            assert_eq!(
                headers
                    .get("content-type")
                    .and_then(|value| value.to_str().ok()),
                Some(*content_type)
            );
            let seen = pricing
                .seen
                .lock()
                .unwrap()
                .clone()
                .expect("the source saw this vote");
            assert_eq!(seen.0, expected, "{action}");
            assert_eq!(seen.1.body, payload);
            assert_eq!(seen.1.idempotency_key.as_deref(), Some("key-1"));
        }
    }
}

#[tokio::test]
async fn a_vote_without_an_idempotency_key_is_refused() {
    let unit = test_support::unit("pricing", 1, 7);
    let pricing = Arc::new(Fake::serving(vec![unit.clone()]));
    let app = inbox(&["pricing"], &[("pricing", pricing.clone())]);
    let (status, _, body) = bytes(
        call(
            &app,
            "POST",
            &format!("/bss-approvals/v1/approval-units/{}/approve", unit.id),
            br#"{"generation":1}"#,
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("Idempotency-Key"), "{text}");
    assert!(pricing.seen.lock().unwrap().is_none());
}

#[tokio::test]
async fn the_vote_spec_does_not_declare_412() {
    let registry = OpenApiRegistryImpl::new();
    let _router = router(
        Arc::new(ApiState::new(Vec::new(), Arc::new(ClientHub::new()))),
        &registry,
    );
    let spec = registry
        .build_openapi(&OpenApiInfo::default())
        .expect("openapi");
    let json = serde_json::to_value(&spec).unwrap();
    let list = &json["paths"]["/bss-approvals/v1/approval-units"]["get"];
    assert!(
        list["x-odata-orderby"]["allowedFields"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == "submitted_at asc")),
        "{list}"
    );
    for action in ["approve", "reject", "withdraw"] {
        let responses = &json["paths"]
            [&format!("/bss-approvals/v1/approval-units/{{id}}/{action}")]["post"]["responses"];
        assert!(responses.get("412").is_none(), "{responses}");
        assert!(responses.get("409").is_some(), "{responses}");
        assert!(responses.get("400").is_some(), "{responses}");
        let parameters = json["paths"]
            [&format!("/bss-approvals/v1/approval-units/{{id}}/{action}")]["post"]["parameters"]
            .as_array()
            .unwrap();
        let key = parameters
            .iter()
            .find(|param| param["name"] == "Idempotency-Key")
            .expect("the key");
        assert_eq!(key["required"], true, "{key}");
    }
}

#[tokio::test]
async fn a_source_vote_that_does_not_read_is_500_and_a_bad_query_names_query() {
    let unit = test_support::unit("pricing", 1, 7);
    let pricing = Arc::new(Fake::serving(vec![unit.clone()]));
    let app = inbox(&["pricing"], &[("pricing", pricing.clone())]);
    let id = unit.id;
    for (status, headers) in [
        (
            1000_u16,
            vec![("content-type".to_owned(), "text/plain".to_owned())],
        ),
        (200, vec![("not a header".to_owned(), "x".to_owned())]),
    ] {
        *pricing.vote.lock().unwrap() = VoteResponse {
            status,
            headers,
            body: b"{}".to_vec(),
        };
        let (got, _, _) = bytes(
            call_with(
                &app,
                "POST",
                &format!("/bss-approvals/v1/approval-units/{id}/approve"),
                br#"{"generation":1}"#,
                true,
                Some("key-1"),
            )
            .await,
        )
        .await;
        assert_eq!(got, StatusCode::INTERNAL_SERVER_ERROR);
    }
    *pricing.vote.lock().unwrap() = VoteResponse {
        status: 200,
        headers: vec![
            ("x-gear".to_owned(), "a".to_owned()),
            ("x-gear".to_owned(), "b".to_owned()),
        ],
        body: b"{}".to_vec(),
    };
    let (got, headers, _) = bytes(
        call_with(
            &app,
            "POST",
            &format!("/bss-approvals/v1/approval-units/{id}/reject"),
            br#"{"generation":1}"#,
            true,
            Some("key-2"),
        )
        .await,
    )
    .await;
    assert_eq!(got, StatusCode::OK);
    let values: Vec<_> = headers
        .get_all("x-gear")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    assert_eq!(values, ["a", "b"]);
    assert!(headers.get("content-type").is_none());

    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            "/bss-approvals/v1/approval-units?limit=nope",
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("\"query\""), "{text}");
    assert!(!text.contains("INVALID_FILTER"), "{text}");
}

#[tokio::test]
async fn a_card_whose_only_source_is_not_registered_is_unavailable() {
    let app = inbox(&["pricing"], &[]);
    let id = test_support::unit("pricing", 1, 11).id;
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            &format!("/bss-approvals/v1/approval-units/{id}"),
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("SOURCE_UNAVAILABLE"), "{text}");
    assert!(text.contains("pricing"), "{text}");

    let products = Arc::new(Fake::serving(vec![test_support::unit("products", 2, 22)]));
    let app = inbox(&["pricing", "products"], &[("products", products)]);
    let (status, _, body) = bytes(
        call(
            &app,
            "GET",
            &format!(
                "/bss-approvals/v1/approval-units/{}",
                test_support::unit("products", 2, 22).id
            ),
            b"",
            true,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("products"), "{text}");
}

#[tokio::test]
async fn an_anonymous_caller_is_401() {
    let app = inbox(
        &["pricing"],
        &[("pricing", Arc::new(Fake::serving(Vec::new())))],
    );
    let (status, _, _) =
        bytes(call(&app, "GET", "/bss-approvals/v1/approval-units", b"", false).await).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// One GET as the test caller, with an optional `If-None-Match`.
async fn get_tagged(
    app: &Router,
    uri: &str,
    tag: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(test_support::caller());
    if let Some(tag) = tag {
        request
            .headers_mut()
            .insert(axum::http::header::IF_NONE_MATCH, tag.parse().unwrap());
    }
    bytes(app.clone().oneshot(request).await.unwrap()).await
}

fn header_text(headers: &axum::http::HeaderMap, name: axum::http::HeaderName) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// AP-D-10: the merged page and the summed counts answer a weak `ETag` of their JSON and
/// `Cache-Control: private, no-cache`; the same GET with that tag is 304 with an empty body. A
/// source that goes from ok to unavailable changes the body, so it changes the tag.
#[tokio::test]
async fn the_list_and_the_counts_answer_304_until_a_source_status_changes() {
    use axum::http::header::{CACHE_CONTROL, ETAG};
    let pricing = Arc::new(Fake::serving(vec![
        test_support::unit("pricing", 1, 1),
        test_support::unit("pricing", 3, 3),
    ]));
    let products = Arc::new(Fake::serving(vec![test_support::unit("products", 2, 2)]));
    let app = inbox(
        &["pricing", "products"],
        &[("pricing", pricing), ("products", products.clone())],
    );
    let paths = [
        "/bss-approvals/v1/approval-units",
        "/bss-approvals/v1/approval-units/counts",
    ];
    let mut tags = Vec::new();
    for path in paths {
        let (status, headers, body) = get_tagged(&app, path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(!body.is_empty(), "{path}");
        let tag = header_text(&headers, ETAG);
        assert!(tag.starts_with("W/\""), "{path}: {tag}");
        assert_eq!(header_text(&headers, CACHE_CONTROL), "private, no-cache");
        for sent in [tag.clone(), format!("\"other\", {tag}"), "*".to_owned()] {
            let (status, again, body) = get_tagged(&app, path, Some(&sent)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{path}: {sent}");
            assert!(body.is_empty(), "{path}: {sent}");
            assert_eq!(header_text(&again, ETAG), tag, "{path}: {sent}");
            assert_eq!(
                header_text(&again, CACHE_CONTROL),
                "private, no-cache",
                "{path}: {sent}"
            );
        }
        tags.push(tag);
    }
    *products.mode.lock().unwrap() = Mode::Unavailable;
    for (path, old) in paths.into_iter().zip(tags) {
        let (status, headers, body) = get_tagged(&app, path, Some(&old)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(answer["sources"][1]["status"], "unavailable", "{path}");
        let fresh = header_text(&headers, ETAG);
        assert!(fresh.starts_with("W/\""), "{path}: {fresh}");
        assert_ne!(fresh, old, "{path}");
        assert_eq!(header_text(&headers, CACHE_CONTROL), "private, no-cache");
    }
}

/// AP-D-10: a refusal is never turned into a 304 and carries no tag.
#[tokio::test]
async fn a_refused_list_is_not_conditional() {
    let pricing = Arc::new(Fake::serving(Vec::new()));
    *pricing.mode.lock().unwrap() = Mode::Unavailable;
    let app = inbox(&["pricing"], &[("pricing", pricing)]);
    for path in [
        "/bss-approvals/v1/approval-units",
        "/bss-approvals/v1/approval-units/counts",
    ] {
        let (status, headers, _) = get_tagged(&app, path, Some("*")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert!(headers.get(axum::http::header::ETAG).is_none(), "{path}");
    }
}

/// AP-D-10: the served spec declares `If-None-Match`, the `ETag` and `Cache-Control` of the 200,
/// and the 304 with both headers, on the list and the counts.
#[tokio::test]
async fn the_list_and_the_counts_declare_the_conditional_get() {
    let registry = OpenApiRegistryImpl::new();
    let _router = router(
        Arc::new(ApiState::new(Vec::new(), Arc::new(ClientHub::new()))),
        &registry,
    );
    let spec = registry
        .build_openapi(&OpenApiInfo::default())
        .expect("openapi");
    let json = serde_json::to_value(&spec).unwrap();
    for path in [
        "/bss-approvals/v1/approval-units",
        "/bss-approvals/v1/approval-units/counts",
    ] {
        let op = &json["paths"][path]["get"];
        let parameter = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|param| param["name"] == "If-None-Match")
            .unwrap_or_else(|| panic!("{path}: If-None-Match"));
        assert_eq!(parameter["in"], "header", "{path}");
        assert_eq!(parameter["required"], false, "{path}");
        for status in ["200", "304"] {
            let headers = &op["responses"][status]["headers"];
            assert!(headers.get("ETag").is_some(), "{path} {status}: {headers}");
            assert!(
                headers.get("Cache-Control").is_some(),
                "{path} {status}: {headers}"
            );
        }
    }
    let card = &json["paths"]["/bss-approvals/v1/approval-units/{id}"]["get"]["responses"];
    assert!(card.get("304").is_none(), "the card is not conditional");
}

// ------------------------------------------------------------------ AP-D-11 actor names

/// The people the directory knows, every lookup it answered, and whether it is down.
#[derive(Default)]
struct People {
    names: Mutex<std::collections::BTreeMap<Uuid, String>>,
    calls: Mutex<Vec<Vec<Uuid>>>,
    down: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl bss_rest::actor_names::ActorDirectory for People {
    async fn list_users(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        query: bss_rest::actor_names::ListUsersQuery,
    ) -> Result<toolkit_odata::Page<bss_rest::actor_names::IdpUser>, CanonicalError> {
        let ids = bss_rest::actor_names::queried_ids(&query);
        self.calls.lock().unwrap().push(ids.clone());
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(CanonicalError::service_unavailable().create());
        }
        let names = self.names.lock().unwrap();
        let users = ids
            .iter()
            .filter_map(|id| {
                names.get(id).map(|name| {
                    bss_rest::actor_names::IdpUser::new(*id, "login").with_display_name(name)
                })
            })
            .collect();
        Ok(toolkit_odata::Page::new(
            users,
            toolkit_odata::PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 200,
            },
        ))
    }
}

impl People {
    fn know(&self, id: Uuid, name: &str) {
        self.names.lock().unwrap().insert(id, name.to_owned());
    }
    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

/// A pricing unit submitted by 3 with a vote by 4, and a products unit submitted by 5 whose live
/// SKU was created by 6 and archived by 9; the inbox names them through `people`.
fn named_inbox(people: Arc<People>) -> Router {
    let mut priced = test_support::unit("pricing", 1, 1);
    priced.decisions = vec![bss_approvals_sdk::InboxDecision {
        actor: Uuid::from_u128(4),
        generation: 1,
        decision: bss_approvals_sdk::DecisionKind::Approve,
        note: None,
        at: test_support::at(2),
        stale: false,
    }];
    let mut sku = test_support::unit("products", 3, 2);
    sku.submitted_by = Uuid::from_u128(5);
    sku.subject_live = Some(serde_json::json!({
        "id": Uuid::from_u128(8),
        "created_by": Uuid::from_u128(6),
        "created_by_name": null,
        "archived_by": Uuid::from_u128(9),
        "archived_by_name": null,
    }));
    let hub = Arc::new(ClientHub::new());
    register("pricing", Arc::new(Fake::serving(vec![priced])), &hub);
    register("products", Arc::new(Fake::serving(vec![sku])), &hub);
    let state = ApiState::new(vec!["pricing".into(), "products".into()], hub).with_actor_names(
        bss_rest::actor_names::ActorNames::with_directory(people, &crate::api::SYSTEM_ACTORS),
    );
    router(Arc::new(state), &OpenApiRegistryImpl::new())
}

fn known_people() -> Arc<People> {
    let people = Arc::new(People::default());
    for (id, name) in [(3, "Sam"), (4, "Vic"), (5, "Pat"), (6, "Cid"), (9, "Ari")] {
        people.know(Uuid::from_u128(id), name);
    }
    people
}

/// One read as the test caller: 200, its body and its `ETag`, and one directory lookup.
async fn named_read(
    app: &Router,
    people: &People,
    uri: &str,
    tag: Option<&str>,
) -> (StatusCode, serde_json::Value, String) {
    let before = people.calls();
    let mut request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(test_support::caller());
    if let Some(tag) = tag {
        request
            .headers_mut()
            .insert("If-None-Match", tag.parse().unwrap());
    }
    let (status, headers, body) = bytes(app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(people.calls() - before, 1, "{uri}: one lookup per read");
    let etag = headers
        .get("etag")
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let json = if body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, json, etag)
}

/// The `*_name` sibling of `field` on `value`: present, and the given text or null.
#[track_caller]
fn named(value: &serde_json::Value, field: &str, expected: Option<&str>) {
    let key = format!("{field}_name");
    let got = value
        .get(&key)
        .unwrap_or_else(|| panic!("{key} is missing: {value}"));
    assert_eq!(
        got,
        &expected.map_or(serde_json::Value::Null, |name| serde_json::json!(name)),
        "{key}: {value}"
    );
}

/// The inbox's list and card with the names each is expected to carry, all or none.
async fn inbox_reads(app: &Router, people: &People, known: bool) {
    let name = |text: &'static str| known.then_some(text);
    let (status, page, _) = named_read(app, people, "/bss-approvals/v1/approval-units", None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{page}");
    let products = items.iter().find(|u| u["source"] == "products").unwrap();
    let pricing = items.iter().find(|u| u["source"] == "pricing").unwrap();
    named(pricing, "submitted_by", name("Sam"));
    named(&pricing["decisions"][0], "actor", name("Vic"));
    named(products, "submitted_by", name("Pat"));
    named(&products["subject_live"], "created_by", name("Cid"));
    named(&products["subject_live"], "archived_by", name("Ari"));
    let card_uri = format!("/bss-approvals/v1/approval-units/{}", Uuid::from_u128(2));
    let (status, card, _) = named_read(app, people, &card_uri, None).await;
    assert_eq!(status, StatusCode::OK, "{card}");
    named(&card, "submitted_by", name("Pat"));
    named(&card["subject_live"], "created_by", name("Cid"));
    named(&card["subject_live"], "archived_by", name("Ari"));
}

#[tokio::test]
async fn the_inbox_names_its_submitters_and_voters_in_one_lookup() {
    let people = known_people();
    let app = named_inbox(people.clone());
    inbox_reads(&app, &people, true).await;
    let mut asked = people.calls.lock().unwrap()[0].clone();
    asked.sort_unstable();
    assert_eq!(
        asked,
        [3, 4, 5, 6, 9].map(Uuid::from_u128),
        "the merged page's actors, once each"
    );
}

#[tokio::test]
async fn an_unavailable_directory_leaves_the_names_null_on_a_200() {
    let people = known_people();
    people.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let app = named_inbox(people.clone());
    inbox_reads(&app, &people, false).await;
}

#[tokio::test]
async fn a_renamed_submitter_changes_the_list_tag() {
    let people = known_people();
    let app = named_inbox(people.clone());
    let uri = "/bss-approvals/v1/approval-units";
    let (_, _, first) = named_read(&app, &people, uri, None).await;
    let (status, _, again) = named_read(&app, &people, uri, Some(&first)).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert_eq!(again, first);
    people.know(Uuid::from_u128(3), "Samantha");
    let (status, page, renamed) = named_read(&app, &people, uri, Some(&first)).await;
    assert_eq!(status, StatusCode::OK, "a rename is a new body");
    assert_ne!(renamed, first);
    let pricing = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["source"] == "pricing")
        .unwrap();
    named(pricing, "submitted_by", Some("Samantha"));
}

/// AP-D-11: an actor a source declares a system actor reads "System" on the list and the card,
/// whichever unit shows it, and is never asked of the directory.
#[tokio::test]
async fn a_system_actor_a_source_declares_reads_system_and_is_never_asked() {
    let declared = Uuid::from_u128(0xf01);
    let mut priced = test_support::unit("pricing", 1, 1);
    priced.decisions = vec![bss_approvals_sdk::InboxDecision {
        actor: declared,
        generation: 1,
        decision: bss_approvals_sdk::DecisionKind::Reject,
        note: Some("expired".into()),
        at: test_support::at(2),
        stale: false,
    }];
    let mut products = Fake::serving(Vec::new());
    products.system = vec![declared];
    let hub = Arc::new(ClientHub::new());
    register("pricing", Arc::new(Fake::serving(vec![priced])), &hub);
    register("products", Arc::new(products), &hub);
    let people = known_people();
    let state = ApiState::new(vec!["pricing".into(), "products".into()], hub).with_actor_names(
        bss_rest::actor_names::ActorNames::with_directory(
            people.clone(),
            &crate::api::SYSTEM_ACTORS,
        ),
    );
    let app = router(Arc::new(state), &OpenApiRegistryImpl::new());
    let (status, page, _) =
        named_read(&app, &people, "/bss-approvals/v1/approval-units", None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    named(&page["items"][0]["decisions"][0], "actor", Some("System"));
    named(&page["items"][0], "submitted_by", Some("Sam"));
    let card_uri = format!("/bss-approvals/v1/approval-units/{}", Uuid::from_u128(1));
    let (status, card, _) = named_read(&app, &people, &card_uri, None).await;
    assert_eq!(status, StatusCode::OK, "{card}");
    named(&card["decisions"][0], "actor", Some("System"));
    let asked = people.calls.lock().unwrap().clone();
    assert!(
        asked.iter().all(|ids| !ids.contains(&declared)),
        "a declared system actor is never asked: {asked:?}"
    );
}

/// An inbox over one pricing source serving `unit`, naming actors through `people`.
fn one_unit_inbox(unit: InboxUnit, people: Arc<People>) -> Router {
    let hub = Arc::new(ClientHub::new());
    register("pricing", Arc::new(Fake::serving(vec![unit])), &hub);
    let state = ApiState::new(vec!["pricing".into()], hub).with_actor_names(
        bss_rest::actor_names::ActorNames::with_directory(people, &crate::api::SYSTEM_ACTORS),
    );
    router(Arc::new(state), &OpenApiRegistryImpl::new())
}

/// AP-D-11: a live subject's actor is named only where its gear put the `*_name` key beside it.
/// A subject with `created_by` and `archived_by` but neither name key is not looked up for them,
/// and gains no key, on the list and the card.
#[tokio::test]
async fn a_live_subject_without_its_name_keys_is_not_named() {
    let (creator, archiver) = (Uuid::from_u128(6), Uuid::from_u128(9));
    let mut unit = test_support::unit("pricing", 1, 1);
    unit.subject_live = Some(serde_json::json!({
        "id": Uuid::from_u128(8),
        "created_by": creator,
        "archived_by": archiver,
    }));
    let people = known_people();
    let app = one_unit_inbox(unit, people.clone());
    let card_uri = format!("/bss-approvals/v1/approval-units/{}", Uuid::from_u128(1));
    for uri in ["/bss-approvals/v1/approval-units", card_uri.as_str()] {
        let (status, body, _) = named_read(&app, &people, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let unit = body.get("items").map_or(&body, |items| &items[0]);
        named(unit, "submitted_by", Some("Sam"));
        let live = unit["subject_live"].as_object().unwrap();
        assert!(!live.contains_key("created_by_name"), "{uri}: {body}");
        assert!(!live.contains_key("archived_by_name"), "{uri}: {body}");
    }
    let asked = people.calls.lock().unwrap().clone();
    assert!(
        asked
            .iter()
            .all(|ids| !ids.contains(&creator) && !ids.contains(&archiver)),
        "{asked:?}"
    );
}

/// AP-D-11: the nil id, the platform's system context, reads "System" as a voter and as a live
/// subject's creator, and is never asked of the directory.
#[tokio::test]
async fn the_nil_actor_reads_system_and_is_never_asked() {
    let mut unit = test_support::unit("pricing", 1, 1);
    unit.decisions = vec![bss_approvals_sdk::InboxDecision {
        actor: Uuid::nil(),
        generation: 1,
        decision: bss_approvals_sdk::DecisionKind::Reject,
        note: Some("expired".into()),
        at: test_support::at(2),
        stale: false,
    }];
    unit.subject_live = Some(serde_json::json!({
        "id": Uuid::from_u128(8),
        "created_by": Uuid::nil(),
        "created_by_name": null,
    }));
    let people = known_people();
    let app = one_unit_inbox(unit, people.clone());
    let card_uri = format!("/bss-approvals/v1/approval-units/{}", Uuid::from_u128(1));
    for uri in ["/bss-approvals/v1/approval-units", card_uri.as_str()] {
        let (status, body, _) = named_read(&app, &people, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let unit = body.get("items").map_or(&body, |items| &items[0]);
        named(&unit["decisions"][0], "actor", Some("System"));
        named(&unit["subject_live"], "created_by", Some("System"));
        named(unit, "submitted_by", Some("Sam"));
    }
    let asked = people.calls.lock().unwrap().clone();
    assert!(
        asked.iter().all(|ids| !ids.contains(&Uuid::nil())),
        "{asked:?}"
    );
}
