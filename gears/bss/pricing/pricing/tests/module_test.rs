//! Runtime registration, declared routes and header readers must remain equal.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "common/census.rs"]
pub mod census;
pub mod rest_support;
use census::Routes;

fn declared_paths() -> Routes {
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
fn if_match_routes() -> Routes {
    [
        ("PATCH", "/bss-pricing/v1/price-book-entries/{id}"),
        ("PATCH", "/bss-pricing/v1/price-books/{id}"),
        ("DELETE", "/bss-pricing/v1/price-books/{id}"),
        ("POST", "/bss-pricing/v1/price-books/{id}/archive"),
        ("POST", "/bss-pricing/v1/price-books/{id}/unarchive"),
        ("PUT", "/bss-pricing/v1/settings"),
        ("PUT", "/bss-pricing/v1/dimension-keys"),
        ("PATCH", "/bss-pricing/v1/prices/{id}"),
        ("DELETE", "/bss-pricing/v1/prices/{id}"),
        ("PUT", "/bss-pricing/v1/approval-policy"),
        ("PATCH", "/bss-pricing/v1/plans/{id}"),
        ("PATCH", "/bss-pricing/v1/plan-revisions/{id}"),
        ("PATCH", "/bss-pricing/v1/plan-items/{id}"),
        ("DELETE", "/bss-pricing/v1/approval-policy/{kind}"),
        ("PATCH", "/bss-pricing/v1/dimension-keys"),
    ]
    .into_iter()
    .map(|(m, p)| (m.to_owned(), p.to_owned()))
    .collect()
}
fn idempotency_key_routes() -> Routes {
    [
        ("POST", "/bss-pricing/v1/price-books"),
        ("POST", "/bss-pricing/v1/price-books/{id}/entries"),
        ("POST", "/bss-pricing/v1/price-book-entries/{id}/prices"),
        ("POST", "/bss-pricing/v1/prices/{id}/cancel"),
        ("POST", "/bss-pricing/v1/prices/{id}/end"),
        ("POST", "/bss-pricing/v1/prices/{id}/submit"),
        ("POST", "/bss-pricing/v1/price-books/{id}/publish-changes"),
        ("POST", "/bss-pricing/v1/approval-units/{id}/approve"),
        ("POST", "/bss-pricing/v1/approval-units/{id}/reject"),
        ("POST", "/bss-pricing/v1/approval-units/{id}/withdraw"),
        ("POST", "/bss-pricing/v1/plans"),
        ("POST", "/bss-pricing/v1/plans/{id}/revisions"),
        ("POST", "/bss-pricing/v1/plan-revisions/{id}/items"),
        ("POST", "/bss-pricing/v1/plan-revisions/{id}/submit"),
        ("POST", "/bss-pricing/v1/plans/{id}/clone"),
        ("POST", "/bss-pricing/v1/plan-revisions/{id}/unschedule"),
    ]
    .into_iter()
    .map(|(m, p)| (m.to_owned(), p.to_owned()))
    .collect()
}

#[tokio::test]
async fn the_registered_route_set_is_exactly_the_declared_paths() {
    let harness = rest_support::Harness::new().await.unwrap();
    let (router, openapi) = harness.router(axum::Router::new()).unwrap();
    let registered: Routes = openapi
        .operation_specs
        .iter()
        .map(|e| {
            let (method, path) = e.key().split_once(':').unwrap();
            (method.to_owned(), path.to_owned())
        })
        .collect();
    assert_eq!(registered, declared_paths());
    assert_eq!(census::source_routes(), registered);
    assert_eq!(registered.len(), 60);
    assert!(router.has_routes());
}

/// D-428, P-D-197: init registers pricing's SKU usage port under the key Products resolves at
/// each SKU read, and the port asks the policy before it reads anything: with the harness's
/// authorization down it answers 503, never a guess.
#[tokio::test]
async fn init_registers_the_sku_usage_port_products_resolves() {
    use bss_products_sdk::sku_usage::SkuUsageV1;
    let harness = rest_support::Harness::new().await.unwrap();
    let port = harness
        .ctx
        .client_hub()
        .get::<dyn SkuUsageV1>()
        .expect("pricing registers dyn SkuUsageV1 at init");
    let tenant = uuid::Uuid::new_v4();
    let reader = toolkit_security::SecurityContext::builder()
        .subject_id(uuid::Uuid::new_v4())
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap();
    let refused = port
        .usage(&reader, tenant, &[uuid::Uuid::new_v4()])
        .await
        .unwrap_err();
    let response = axum::response::IntoResponse::into_response(refused);
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
}

#[test]
fn the_registration_parser_has_a_positive_control() {
    let routes = census::registrations(census::CONTROL);
    assert_eq!(routes.len(), 2);
    assert_eq!(
        (&*routes[0].method, &*routes[0].path, &*routes[0].handler),
        ("POST", "/bss-pricing/v1/control", "create")
    );
    assert_eq!(
        (&*routes[1].method, &*routes[1].path, &*routes[1].handler),
        ("GET", "/bss-pricing/v1/control", "read")
    );
}

#[test]
fn every_precondition_reading_route_is_in_the_precondition_census() {
    assert_eq!(
        census::readers("preconditions::if_match("),
        if_match_routes()
    );
    assert_eq!(
        census::readers("preconditions::idempotency_key("),
        idempotency_key_routes()
    );
    for (needle, control, production) in [
        // + 2: the book's archive and unarchive (D-522).
        ("preconditions::if_match(", 1, 15),
        ("preconditions::idempotency_key(", 1, 16),
        ("Query<", 1, 0),
        // + 1: plan_items::delete answers 204 below its door; + 16: the plan and revision doors
        // (eight registrations and the statuses their handlers and operations answer); + 6: the
        // item and checks doors (four registrations, the item PATCH and the checks answer); + 2:
        // the revision submit door (its registration and its 201 answer); + 2: the clone door
        // (its registration and its 201 answer); + 2: the resolve door (its registration and its
        // 200 answer); + 2: the pinned price read (its registration and its 200 answer); + 9: run
        // 6.4's four doors (D-434 to D-436) — the SKU's entries, the plan item read, the policy
        // reset and the dimension PATCH, each registration and each 200 answer (the PATCH has two:
        // an empty patch answers without a write); + 2: run 7.1's entry prices (D-440), its
        // registration and its 200 answer; + 1: run 7.2's price PATCH answers a temporary
        // draft's new dates from their own function (D-443); + 2: run 7.2's book delete (D-444),
        // its registration and its 204 answer; + 2: run 8.2's unschedule door (D-452), its
        // registration and its 200 answer; - 1: the vote's `GENERATION_MISMATCH` renders through
        // the problem's own response, which carries its status (whole-branch review PS-07); - 1:
        // a claimed key's stored status is read back by one function (`support::stored_status`,
        // PS-43), where the claim and the book create each read it; + 2: run 9.3's counts door
        // (D-470), its registration and its 200 answer; + 4: run 9.6's reservations read and
        // effective-policy read (D-480, D-481), each registration and its 200 answer; + 2: run
        // 9.7's batch checks read (D-482), its registration and its 200 answer; + 2: run 9.8b's
        // plans counts (D-485), its registration and its 200 answer. D-518 kept the sum for the
        // three list reads (each registration's 304, each handler's 200 now answered by
        // `respond`); + 3: the settings read's 304 registration and `revalidate_version`'s 200
        // check and 304 answer; + 3: the cancel and end draft doors (D-520, D-521); - 11: D-519's
        // twelve reads that name their actors answer their 200 through the one `names::named`
        // (`StatusCode::OK` once), not each from its own function; + 3: the book's archive and
        // unarchive (D-522), two registrations and two 200 answers, while the book read answers
        // through `names::named` now.
        ("StatusCode::", 2, 110),
    ] {
        assert_eq!(census::count_in_functions(census::CONTROL, needle), control);
        assert_eq!(census::production_count(needle), production, "{needle}");
    }
}

#[tokio::test]
async fn every_precondition_reading_route_declares_the_header_it_reads() {
    use toolkit::api::operation_builder::ParamLocation;
    let harness = rest_support::Harness::new().await.unwrap();
    let (_, openapi) = harness.router(axum::Router::new()).unwrap();
    for (header, expected) in [
        ("if-match", if_match_routes()),
        ("idempotency-key", idempotency_key_routes()),
    ] {
        let declared: Routes = openapi
            .operation_specs
            .iter()
            .filter(|e| {
                e.value().params.iter().any(|p| {
                    p.location == ParamLocation::Header && p.name.eq_ignore_ascii_case(header)
                })
            })
            .map(|e| {
                let (method, path) = e.key().split_once(':').unwrap();
                (method.to_owned(), path.to_owned())
            })
            .collect();
        assert_eq!(declared, expected, "{header}");
    }
}

/// The reads that answer an `ETag` (the version or content tag a following write sends back as
/// If-Match); `read_contract::exactly_the_reads_that_declare_an_etag_answer_one` measures it.
fn etag_routes() -> Routes {
    [
        ("GET", "/bss-pricing/v1/price-books/{id}"),
        ("GET", "/bss-pricing/v1/settings"),
        ("GET", "/bss-pricing/v1/dimension-keys"),
        ("GET", "/bss-pricing/v1/price-book-entries/{id}"),
        ("GET", "/bss-pricing/v1/approval-policy"),
        ("GET", "/bss-pricing/v1/plans/{id}"),
        ("GET", "/bss-pricing/v1/plan-revisions/{id}"),
        ("GET", "/bss-pricing/v1/plan-items/{id}"),
        // D-518: the list reads. The tag is a weak hash of the JSON body, not an If-Match version.
        ("GET", "/bss-pricing/v1/plans"),
        ("GET", "/bss-pricing/v1/plans/counts"),
        ("GET", "/bss-pricing/v1/price-books"),
        // D-469: the write answers that set one (run 9.2's census).
        ("POST", "/bss-pricing/v1/price-books"),
        ("PATCH", "/bss-pricing/v1/price-books/{id}"),
        // D-522: the archive mark moves the book's version.
        ("POST", "/bss-pricing/v1/price-books/{id}/archive"),
        ("POST", "/bss-pricing/v1/price-books/{id}/unarchive"),
        ("PUT", "/bss-pricing/v1/settings"),
        ("PUT", "/bss-pricing/v1/dimension-keys"),
        ("PATCH", "/bss-pricing/v1/dimension-keys"),
        ("POST", "/bss-pricing/v1/price-books/{id}/entries"),
        ("PATCH", "/bss-pricing/v1/price-book-entries/{id}"),
        ("PUT", "/bss-pricing/v1/approval-policy"),
        ("DELETE", "/bss-pricing/v1/approval-policy/{kind}"),
        ("POST", "/bss-pricing/v1/price-book-entries/{id}/prices"),
        ("POST", "/bss-pricing/v1/prices/{id}/cancel"),
        ("POST", "/bss-pricing/v1/prices/{id}/end"),
        ("PATCH", "/bss-pricing/v1/prices/{id}"),
        ("POST", "/bss-pricing/v1/plans"),
        ("PATCH", "/bss-pricing/v1/plans/{id}"),
        ("POST", "/bss-pricing/v1/plans/{id}/revisions"),
        ("POST", "/bss-pricing/v1/plans/{id}/clone"),
        ("PATCH", "/bss-pricing/v1/plan-revisions/{id}"),
        ("POST", "/bss-pricing/v1/plan-revisions/{id}/unschedule"),
        ("POST", "/bss-pricing/v1/plan-revisions/{id}/items"),
        ("PATCH", "/bss-pricing/v1/plan-items/{id}"),
    ]
    .into_iter()
    .map(|(m, p)| (m.to_owned(), p.to_owned()))
    .collect()
}

/// Every operation describes itself (plan review M3): a human summary that is not its operation
/// id's suffix, and a description of what it does and its main refusals.
#[tokio::test]
async fn every_operation_has_a_human_summary_and_a_description() {
    let harness = rest_support::Harness::new().await.unwrap();
    let (_, openapi) = harness.router(axum::Router::new()).unwrap();
    let mut described = 0;
    for entry in &openapi.operation_specs {
        let op = entry.value();
        let id = op.operation_id.as_deref().unwrap_or_default();
        let suffix = id.strip_prefix("bss_pricing.").unwrap_or(id);
        let summary = op.summary.as_deref().unwrap_or_default().trim();
        assert!(
            summary.chars().next().is_some_and(char::is_uppercase),
            "{id}: a summary is a human phrase: {summary:?}"
        );
        assert_ne!(summary, suffix, "{id}: the summary is its operation id");
        assert_ne!(
            summary.to_lowercase().replace(' ', "_"),
            suffix,
            "{id}: the summary spells its operation id"
        );
        let description = op.description.as_deref().unwrap_or_default().trim();
        assert!(
            description.split_whitespace().count() >= 8,
            "{id}: a description says what the operation does: {description:?}"
        );
        assert_ne!(description, summary, "{id}");
        described += 1;
    }
    assert_eq!(described, 60);
}

/// Every answer that sets an `ETag` declares the header on its success response, and nothing else
/// declares one: the eight reads and the nineteen write answers of D-469, each the version a
/// following If-Match takes, plus the three list reads of D-518 (a weak tag of the JSON body,
/// also declared on the 304) and the settings read's 304 (its strong version tag, D-518).
#[tokio::test]
async fn every_answer_that_sets_an_etag_declares_it() {
    let harness = rest_support::Harness::new().await.unwrap();
    let (_, openapi) = harness.router(axum::Router::new()).unwrap();
    let declared: Routes = openapi
        .operation_specs
        .iter()
        .filter(|e| {
            e.value().responses.iter().any(|r| {
                r.headers
                    .iter()
                    .any(|h| h.name.eq_ignore_ascii_case("etag"))
                    && (200..300).contains(&r.status)
            })
        })
        .map(|e| {
            let (method, path) = e.key().split_once(':').unwrap();
            (method.to_owned(), path.to_owned())
        })
        .collect();
    assert_eq!(declared, etag_routes());
    let anywhere = openapi
        .operation_specs
        .iter()
        .flat_map(|e| e.value().responses.clone())
        .filter(|r| {
            r.headers
                .iter()
                .any(|h| h.name.eq_ignore_ascii_case("etag"))
        })
        .count();
    assert_eq!(
        anywhere, 38,
        "only the success answer of those ops declares it, plus the 304 of the three list reads \
         and of the settings read, the 201 of the cancel and end drafts, and the 200 of the book's \
         archive and unarchive"
    );
}

#[test]
fn no_handler_takes_axums_json_extractor() {
    assert_eq!(
        census::count_in_functions("async fn create(Json(body): Json<Input>) {}", "Json<"),
        1
    );
    assert_eq!(census::production_count("Json<"), 0);
}

#[tokio::test]
async fn no_operation_declares_a_422() {
    let harness = rest_support::Harness::new().await.unwrap();
    let (_, openapi) = harness.router(axum::Router::new()).unwrap();
    for entry in &openapi.operation_specs {
        for response in &entry.value().responses {
            assert_ne!(response.status, 422);
        }
    }
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
#[tokio::test]
async fn init_registers_pricing_read_beside_sku_usage_and_checks_pdp() {
    use bss_pricing_sdk::read::{CatalogRef, PriceQuery, PricingReadV1};
    let harness = rest_support::Harness::new().await.unwrap();
    let port = harness.ctx.client_hub().get::<dyn PricingReadV1>().unwrap();
    assert!(
        harness
            .ctx
            .client_hub()
            .get::<dyn bss_products_sdk::sku_usage::SkuUsageV1>()
            .is_ok()
    );
    let tenant = uuid::Uuid::new_v4();
    let reader = toolkit_security::SecurityContext::builder()
        .subject_id(uuid::Uuid::new_v4())
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap();
    let error = port
        .price(
            &reader,
            PriceQuery {
                catalog: CatalogRef { tenant_id: tenant },
                price_id: uuid::Uuid::new_v4(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.status_code(), 503, "the configured PDP is down");
}

#[tokio::test]
async fn init_registers_both_commercial_ports_separately_from_read() {
    use bss_pricing_sdk::acceptance::{AcceptanceQuery, PricingAcceptanceV1, SellabilityV1};
    let h = rest_support::Harness::new().await.unwrap();
    let hub = h.ctx.client_hub();
    assert!(
        hub.get::<dyn bss_pricing_sdk::read::PricingReadV1>()
            .is_ok()
    );
    assert!(hub.get::<dyn SellabilityV1>().is_ok());
    let acceptance = hub.get::<dyn PricingAcceptanceV1>().unwrap();
    let tenant = uuid::Uuid::new_v4();
    let ctx = toolkit_security::SecurityContext::builder()
        .subject_id(uuid::Uuid::new_v4())
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap();
    assert_eq!(
        acceptance
            .acceptance(
                &ctx,
                AcceptanceQuery {
                    catalog: bss_pricing_sdk::read::CatalogRef { tenant_id: tenant },
                    acceptance_id: uuid::Uuid::new_v4()
                }
            )
            .await
            .unwrap_err()
            .status_code(),
        503
    );
}

#[tokio::test]
async fn startup_validates_versioned_hold_policy_before_registering_providers() {
    for policy in [
        serde_json::json!({"version":0,"duration_seconds":86400}),
        serde_json::json!({"version":1,"duration_seconds":0}),
        serde_json::json!({"version":1,"duration_seconds":-1}),
        serde_json::json!({"version":1}),
        serde_json::json!({"version":1,"duration_seconds":86400,"typo":1}),
    ] {
        let result =
            rest_support::Harness::with_config(serde_json::json!({"seller_hold_policy":policy}))
                .await;
        let Err(error) = result else {
            panic!("a bad hold policy must not register providers");
        };
        let text = format!("{error:#}");
        assert!(
            text.contains("seller_hold_policy")
                || text.contains("nonzero")
                || text.contains("invalid config"),
            "the refusal names the policy and registers nothing: {text}"
        );
    }
    rest_support::Harness::with_config(
        serde_json::json!({"seller_hold_policy":{"version":2,"duration_seconds":3600}}),
    )
    .await
    .unwrap();
    let default = bss_pricing::config::BssPricingConfig::default().seller_hold_policy;
    assert_eq!(
        (default.version.get(), default.duration_seconds.get()),
        (1, 86_400)
    );
}

#[test]
fn resource_and_action_census_is_exact() {
    assert_eq!(
        bss_pricing::authz::labels::ALL,
        [
            "gts.cf.bss.pricing.price_book.v1~",
            "gts.cf.bss.pricing.price_book_entry.v1~",
            "gts.cf.bss.pricing.price.v1~",
            "gts.cf.bss.pricing.approval_unit.v1~",
            "gts.cf.bss.pricing.config.v1~",
            "gts.cf.bss.pricing.plan.v1~",
            "gts.cf.bss.pricing.acceptance.v1~",
        ]
    );
    assert_eq!(
        [
            bss_pricing::authz::actions::CREATE,
            bss_pricing::authz::actions::READ,
            bss_pricing::authz::actions::HOLD
        ],
        ["create", "read", "hold"]
    );
}

#[tokio::test]
async fn absent_pdp_is_a_named_unconfigured_dependency() {
    let error = rest_support::Harness::with_dependencies(serde_json::json!({}), false)
        .await
        .err()
        .unwrap();
    let canonical = error
        .downcast_ref::<toolkit_canonical_errors::CanonicalError>()
        .unwrap();
    assert_eq!(canonical.status_code(), 400);
    let problem =
        serde_json::to_value(toolkit_canonical_errors::Problem::from(canonical.clone())).unwrap();
    assert_eq!(
        problem["context"]["violations"][0]["type"], "UNCONFIGURED_DEPENDENCY",
        "{problem}"
    );
    assert_eq!(
        problem["context"]["violations"][0]["description"],
        "unconfigured dependency: AuthZResolverApi"
    );
    assert_eq!(
        problem["context"]["violations"][0]["subject"],
        "AuthZResolverApi"
    );
}
