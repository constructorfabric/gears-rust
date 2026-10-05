//! Authz census stays equal to the runtime and source route sets.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "common/census.rs"]
pub mod census;
pub mod rest_support;

fn census() -> census::Routes {
    [
        ("POST", "/bss-pricing/v1/price-books"),
        ("POST", "/bss-pricing/v1/price-books/{id}/entries"),
        ("GET", "/bss-pricing/v1/price-book-entries/{id}"),
        ("PATCH", "/bss-pricing/v1/price-book-entries/{id}"),
        ("DELETE", "/bss-pricing/v1/price-book-entries/{id}"),
        ("GET", "/bss-pricing/v1/reference-ops"),
        ("GET", "/bss-pricing/v1/price-books"),
        ("GET", "/bss-pricing/v1/price-books/{id}"),
        ("PATCH", "/bss-pricing/v1/price-books/{id}"),
        ("DELETE", "/bss-pricing/v1/price-books/{id}"),
        ("POST", "/bss-pricing/v1/price-books/{id}/archive"),
        ("POST", "/bss-pricing/v1/price-books/{id}/unarchive"),
        ("GET", "/bss-pricing/v1/price-books/{id}/entries"),
        ("GET", "/bss-pricing/v1/price-books/{id}/export"),
        ("GET", "/bss-pricing/v1/settings"),
        ("PUT", "/bss-pricing/v1/settings"),
        ("GET", "/bss-pricing/v1/dimension-keys"),
        ("PUT", "/bss-pricing/v1/dimension-keys"),
        ("POST", "/bss-pricing/v1/price-book-entries/{id}/prices"),
        ("PATCH", "/bss-pricing/v1/prices/{id}"),
        ("DELETE", "/bss-pricing/v1/prices/{id}"),
        ("POST", "/bss-pricing/v1/prices/{id}/cancel"),
        ("POST", "/bss-pricing/v1/prices/{id}/end"),
        ("POST", "/bss-pricing/v1/prices/{id}/submit"),
        ("GET", "/bss-pricing/v1/price-books/{id}/publish-changes"),
        ("POST", "/bss-pricing/v1/price-books/{id}/publish-changes"),
        ("GET", "/bss-pricing/v1/approval-units"),
        ("GET", "/bss-pricing/v1/approval-units/counts"),
        ("GET", "/bss-pricing/v1/approval-units/{id}"),
        ("POST", "/bss-pricing/v1/approval-units/{id}/approve"),
        ("POST", "/bss-pricing/v1/approval-units/{id}/reject"),
        ("POST", "/bss-pricing/v1/approval-units/{id}/withdraw"),
        ("GET", "/bss-pricing/v1/approval-policy"),
        ("PUT", "/bss-pricing/v1/approval-policy"),
        ("POST", "/bss-pricing/v1/plans"),
        ("GET", "/bss-pricing/v1/plans"),
        ("GET", "/bss-pricing/v1/plans/counts"),
        ("GET", "/bss-pricing/v1/plans/{id}"),
        ("PATCH", "/bss-pricing/v1/plans/{id}"),
        ("POST", "/bss-pricing/v1/plans/{id}/revisions"),
        ("GET", "/bss-pricing/v1/plan-revisions/{id}"),
        ("PATCH", "/bss-pricing/v1/plan-revisions/{id}"),
        ("DELETE", "/bss-pricing/v1/plan-revisions/{id}"),
        ("POST", "/bss-pricing/v1/plan-revisions/{id}/items"),
        ("PATCH", "/bss-pricing/v1/plan-items/{id}"),
        ("DELETE", "/bss-pricing/v1/plan-items/{id}"),
        ("GET", "/bss-pricing/v1/plan-revisions/checks"),
        ("GET", "/bss-pricing/v1/plan-revisions/{id}/checks"),
        ("POST", "/bss-pricing/v1/plan-revisions/{id}/submit"),
        ("POST", "/bss-pricing/v1/plans/{id}/clone"),
        ("GET", "/bss-pricing/v1/resolve"),
        ("GET", "/bss-pricing/v1/prices/{id}"),
        ("GET", "/bss-pricing/v1/price-book-entries"),
        ("GET", "/bss-pricing/v1/plan-items/{id}"),
        ("DELETE", "/bss-pricing/v1/approval-policy/{kind}"),
        ("PATCH", "/bss-pricing/v1/dimension-keys"),
        ("GET", "/bss-pricing/v1/price-book-entries/{id}/prices"),
        ("POST", "/bss-pricing/v1/plan-revisions/{id}/unschedule"),
        ("GET", "/bss-pricing/v1/plan-revisions/{id}/reservations"),
        ("GET", "/bss-pricing/v1/approval-policy/{kind}/effective"),
    ]
    .into_iter()
    .map(|(m, p)| (m.to_owned(), p.to_owned()))
    .collect()
}

#[tokio::test]
async fn the_census_covers_every_route_the_routers_register() {
    let harness = rest_support::Harness::new().await.unwrap();
    let (_, openapi) = harness.router(axum::Router::new()).unwrap();
    let registered: census::Routes = openapi
        .operation_specs
        .iter()
        .map(|e| {
            let (method, path) = e.key().split_once(':').unwrap();
            (method.to_owned(), path.to_owned())
        })
        .collect();
    assert_eq!(registered, census());
    assert_eq!(census::source_routes(), registered);
    assert_eq!(census::readers("require_authenticated("), registered);
    assert_eq!(census::readers("authz::access_scope("), registered);
    assert_eq!(registered.len(), 60);
    assert_eq!(bss_pricing::authz::labels::ALL.len(), 7);
    let permissions: Vec<_> = toolkit_gts::inventory::iter::<toolkit_gts::InventoryInstance>
        .into_iter()
        .filter(|i| i.instance_id.contains("~cf.bss.pricing."))
        .collect();
    assert!(permissions.is_empty());
}

/// Fix run W1c M1 (D-424, products P-D-222): pricing's system actor acts in-process only. A REST
/// caller whose context carries it in either half (the subject type `bss-pricing.system` or the
/// id `PRICING_SYSTEM_ACTOR`; a token's claims can carry both) is refused at every door the real
/// `register_rest` serves, 403 `SYSTEM_ACTOR_RESERVED`, before the PDP is asked. Another system
/// subject (Rating's, which calls resolve) passes the edge and meets the PDP, here one that cannot
/// answer (503).
#[tokio::test]
async fn no_rest_door_serves_pricings_system_actor() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let harness = rest_support::Harness::new().await.unwrap();
    let (app, openapi) = harness.router(axum::Router::new()).unwrap();
    let tenant = uuid::Uuid::new_v4();
    let subject = |id: uuid::Uuid, kind: &str| {
        toolkit_security::SecurityContext::builder()
            .subject_id(id)
            .subject_tenant_id(tenant)
            .subject_type(kind)
            .build()
            .unwrap()
    };
    let pricing = bss_products_sdk::PRICING_SYSTEM_ACTOR;
    let asserted = [
        subject(pricing, "bss-pricing.system"),
        subject(uuid::Uuid::new_v4(), "bss-pricing.system"),
        subject(pricing, "user"),
    ];
    let rating = subject(uuid::Uuid::new_v4(), "bss-rating.system");
    let doors: Vec<(String, String)> = openapi
        .operation_specs
        .iter()
        .map(|e| {
            let (method, path) = e.key().split_once(':').unwrap();
            (method.to_owned(), path.to_owned())
        })
        .collect();
    assert_eq!(doors.len(), 60, "every served door");
    for (method, template) in doors {
        let path = template
            .replace("{id}", &uuid::Uuid::new_v4().to_string())
            .replace("{kind}", "prices");
        let callers = asserted.iter().map(|c| (c, true));
        for (ctx, refused) in callers.chain([(&rating, false)]) {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.as_str())
                        .uri(&path)
                        .extension(ctx.clone())
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status().as_u16();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let who = (ctx.subject_id(), ctx.subject_type());
            if refused {
                assert_eq!(status, 403, "{method} {path} as {who:?}: {body}");
                assert_eq!(
                    body["context"]["reason"], "SYSTEM_ACTOR_RESERVED",
                    "{method} {path} as {who:?}: {body}"
                );
            } else {
                assert_eq!(status, 503, "{method} {path} as {who:?}: {body}");
            }
        }
    }
}

#[test]
fn the_authentication_and_authz_parsers_have_positive_controls() {
    // One per route (55), and more: `require_authenticated(` is also its own definition;
    // `authz::access_scope(` is also the SKU usage port, which authorizes the Products caller it
    // serves (D-428), and the money's second judgement, price_book read, in the one helper the
    // SKU's entry list, the two entry reads and an entry's prices call (D-434, D-440).
    // D-501 adds the shared authorization gate of the three PricingReadV1 methods.
    // D-506 adds the shared commercial service gate (four SDK methods).
    // Both needles are also the approvals inbox's source, whose page and counts judge the caller
    // as the list and the counts doors do, once, in one helper (D-490); its card and votes call
    // the doors.
    for (needle, more) in [("require_authenticated(", 2), ("authz::access_scope(", 5)] {
        assert_eq!(census::count_in_functions(census::CONTROL, needle), 2);
        assert_eq!(census::production_count(needle), 60 + more, "{needle}");
    }
    let routes = census::registrations(census::CONTROL);
    assert_eq!(
        routes
            .iter()
            .filter(|r| r.declaration.contains(".authenticated()"))
            .count(),
        2
    );
    assert_eq!(census::source_routes().len(), 60);
}

#[test]
fn every_mounted_router_is_merged_into_both_censuses() {
    // Both census files and rest_support register through the runtime capability:
    // their inventories cannot omit a router by forgetting a manual merge.
    assert_eq!(
        census::functions(census::CONTROL)
            .keys()
            .filter(|name| name.ends_with("router"))
            .count(),
        1
    );
    let routers: Vec<_> = census::sources()
        .iter()
        .flat_map(|path| {
            census::functions(&std::fs::read_to_string(path).unwrap())
                .into_keys()
                .filter(|name| name.ends_with("router"))
                .collect::<Vec<_>>()
        })
        .collect();
    // `module.rs` mounts two: the authoring router and the consumer read contract's.
    assert_eq!(
        routers,
        vec!["router".to_owned(), "router".to_owned()],
        "every router is mounted"
    );
    assert_eq!(census::source_routes(), census());
}

// Run-3 route contract: method | path | resource:action | If-Match | Idempotency-Key
// POST /price-books price_book:author false true
// GET /price-books price_book:read false false
// GET /price-books/{id} price_book:read false false
// PATCH /price-books/{id} price_book:author true false
// GET /price-books/{id}/entries price_book_entry:read false false
// GET /price-books/{id}/export price_book:read false false
// GET /settings config:read false false
// PUT /settings config:settings true false
// GET /dimension-keys config:read false false
// PUT /dimension-keys config:settings true false

// POST /price-books/{id}/entries price_book_entry:author false true
// GET /price-book-entries/{id} price_book_entry:read false false
// PATCH /price-book-entries/{id} price_book_entry:author true false
// DELETE /price-book-entries/{id} price_book_entry:author false false

// GET /reference-ops config:settings false false

// Run-4 prices: method | path | resource:action | If-Match | Idempotency-Key
// POST /price-book-entries/{id}/prices price:author false true
// PATCH /prices/{id} price:author true false
// DELETE /prices/{id} price:author true false
// POST /prices/{id}/cancel price:author false true
// POST /prices/{id}/end price:author false true

// Run-4 approvals: method | path | resource:action | If-Match | Idempotency-Key
// POST /prices/{id}/submit price:submit false true
// GET /price-books/{id}/publish-changes price_book:read false false
// POST /price-books/{id}/publish-changes price_book:submit false true
// GET /approval-units approval_unit:read false false
// GET /approval-units/counts approval_unit:read false false (D-470)
// GET /approval-units/{id} approval_unit:read false false
// POST /approval-units/{id}/approve approval_unit:approve false true
// POST /approval-units/{id}/reject approval_unit:approve false true
// POST /approval-units/{id}/withdraw approval_unit:submit false true
// GET /approval-policy config:read false false
// PUT /approval-policy config:settings true false

// Run 3.3 plans: method | path | resource:action | If-Match | Idempotency-Key
// POST /plans plan:author (then price_book:read, D-456) false true
// GET /plans plan:read false false
// GET /plans/counts plan:read false false (D-485)
// GET /plans/{id} plan:read false false
// PATCH /plans/{id} plan:author true false
// POST /plans/{id}/revisions plan:author false true
// GET /plan-revisions/{id} plan:read (then price_book:read for the sale-date price, D-480) false false
// PATCH /plan-revisions/{id} plan:author (then price_book:read when it names a book, D-456) true false
// DELETE /plan-revisions/{id} plan:author false false

// Run 3.3 items and checks: method | path | resource:action | If-Match | Idempotency-Key
// POST /plan-revisions/{id}/items plan:author false true
// PATCH /plan-items/{id} plan:author true false
// DELETE /plan-items/{id} plan:author false false
// GET /plan-revisions/checks plan:read false false
// GET /plan-revisions/{id}/checks plan:read false false

// Run 3.4 plan approvals: method | path | resource:action | If-Match | Idempotency-Key
// POST /plan-revisions/{id}/submit plan:submit false true
// POST /plans/{id}/clone plan:author (then price_book:read, D-456) false true

// Run 4.3 read contract: method | path | resource:action | If-Match | Idempotency-Key
// GET /resolve plan:read false false
// GET /prices/{id} price:read false false

// Run 6.4 (D-434 to D-436): method | path | resource:action | If-Match | Idempotency-Key
// GET /price-book-entries price_book_entry:read false false
// GET /plan-items/{id} plan:read false false
// DELETE /approval-policy/{kind} config:settings true false
// PATCH /dimension-keys config:settings true false

// Run 7.1 (D-440): method | path | resource:action | If-Match | Idempotency-Key
// GET /price-book-entries/{id}/prices price_book_entry:read (then price_book:read) false false

// Run 8.2 (D-452): method | path | resource:action | If-Match | Idempotency-Key
// POST /plan-revisions/{id}/unschedule plan:submit false true

// Run 9.6 (D-480, D-481): method | path | resource:action | If-Match | Idempotency-Key
// GET /plan-revisions/{id}/reservations plan:read false false
// GET /approval-policy/{kind}/effective price_book_entry:read for prices, plan:read for plan_revision false false
