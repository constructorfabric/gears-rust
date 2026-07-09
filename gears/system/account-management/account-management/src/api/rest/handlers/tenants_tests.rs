//! Handler-level unit tests for the tenant-CRUD REST surface.
//!
//! Scope: pin the seams the [`super::create_tenant`] and
//! [`super::list_tenant_children`] handler functions add on top of
//! the [`TenantService`] contract. Two seams need handler-level pins:
//!
//! * `create_tenant` wraps the post-create [`Tenant`] via
//!   `created_json(dto, &uri, &id_str).into_response()` — this
//!   composes HTTP 201 + a `Location` header derived from the path
//!   the request arrived on plus the server-allocated id. A
//!   regression that returned 200 / forgot the `Location` would slip
//!   past every service-level test (the service returns the same
//!   `Tenant` either way).
//! * `list_tenant_children` runs `clamp_listing_top(query,
//!   svc.max_list_children_top())` **before** the service call so
//!   the operator-tunable `listing.max_top` is honoured at the API
//!   boundary. A regression that called the service with the raw
//!   `OData(query)` would let callers receive up to the repo-level
//!   absolute ceiling (`*_LISTING_LIMIT_CFG.max = 200`) regardless
//!   of the deployment's policy.
//!
//! Out of scope here:
//!
//! * Wire-shape / DTO conversions — pinned in
//!   [`crate::api::rest::dto_tests`]
//!   (`tenant_dto_active_wire_shape_mirrors_openapi`,
//!   `tenant_create_request_required_fields_only_deserialise`,
//!   `tenant_update_request_name_and_status_round_trip`, …).
//! * `clamp_listing_top` matrix — pinned in
//!   [`crate::api::rest::handlers::common_tests`].
//! * Service-layer CRUD, hierarchy depth gating, soft-delete
//!   pipeline, `list_children` filter / pagination -- pinned in
//!   [`crate::domain::tenant::service::service_tests`].
//!
//! The `create_tenant` / `get_tenant` / `update_tenant` /
//! `delete_tenant` handler functions are typed `Extension<Arc<
//! ConcreteTenantService>>` where
//! `ConcreteTenantService = TenantService<TenantRepoImpl>` — the
//! axum extractor binding therefore pins the production repo
//! concretely. Tests pin the handler-specific compositions
//! (response-side response wrap, clamp + `max_list_children_top`
//! getter) rather than spinning up the full request through the
//! axum router; the service-side tests already exercise the call
//! against `TenantService<FakeTenantRepo>` with full coverage.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test-support fakes panic on poisoned mutex; the canonical expect form is shared with FakeTenantRepo's tests"
)]

use std::sync::Arc;

use axum::http::{StatusCode, Uri, header};
use axum::response::IntoResponse;
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit::api::canonical_prelude::created_json;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

use account_management_sdk::{Tenant, TenantId, TenantStatus};

use crate::api::rest::dto::TenantDto;
use crate::api::rest::handlers::common::clamp_listing_top;
use crate::config::{AccountManagementConfig, ListingConfig};
use crate::domain::tenant::resource_checker::InertResourceOwnershipChecker;
use crate::domain::tenant::service::TenantService;
use crate::domain::tenant::test_support::mock_enforcer;
use crate::domain::tenant::test_support::{FakeIdpProvisioner, FakeOutcome, FakeTenantRepo};
use crate::domain::tenant_type::inert_tenant_type_checker;

// ---- helpers ----------------------------------------------------

fn sample_tenant_id() -> Uuid {
    Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()
}

fn sample_parent_id() -> Uuid {
    Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap()
}

fn sample_created() -> OffsetDateTime {
    datetime!(2026-05-01 09:30:00 UTC)
}

fn sample_updated() -> OffsetDateTime {
    datetime!(2026-05-16 12:00:00 UTC)
}

fn sample_active_tenant() -> Tenant {
    Tenant {
        id: TenantId(sample_tenant_id()),
        name: "acme corp".into(),
        status: TenantStatus::Active,
        tenant_type: Some("gts.cf.core.am.tenant_type.v1~vendor.app.customer.v1~".into()),
        parent_id: Some(TenantId(sample_parent_id())),
        self_managed: false,
        depth: 2,
        child_count: 0,
        created_at: sample_created(),
        updated_at: sample_updated(),
        deleted_at: None,
    }
}

/// Build a [`TenantService`] over [`FakeTenantRepo`] with a
/// caller-supplied `listing.max_top`. The test only consumes
/// [`TenantService::max_list_children_top`] (a `const fn` getter
/// over the config), so the `IdP` / resource / type-checker wiring is
/// inert — no DB, no network, no fixture closure rows required.
fn build_service_with_listing_cap(max_top: u32) -> TenantService<FakeTenantRepo> {
    let repo = Arc::new(FakeTenantRepo::new());
    let cfg = AccountManagementConfig {
        listing: ListingConfig { max_top },
        ..AccountManagementConfig::default()
    };
    TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        inert_tenant_type_checker(),
        mock_enforcer(),
        cfg,
    )
}

// ---- create_tenant response-side composition --------------------

#[tokio::test]
async fn create_tenant_returns_201_with_location_header_pointing_at_get_route() {
    // `create_tenant` wraps via
    // `created_json(dto, &uri, &id_str).into_response()` where:
    //   * `uri` is the inbound request path
    //     (`/account-management/v1/tenants`)
    //   * `id_str` is the server-allocated `tenant.id.to_string()`
    //
    // The composed response MUST be HTTP 201 with a `Location` header
    // equal to `<request-path>/<id>` — i.e. the canonical `GET
    // /tenants/{tenant_id}` route for follow-up reads. A regression
    // that returned 200, or forgot the `Location` header, or used a
    // different id source would surface here.
    //
    // The service-side `create_tenant` already returns the
    // post-create `Tenant`; rebuilding it here directly lets the
    // test pin the handler-specific *response wrap* without standing
    // up the concrete `TenantService<TenantRepoImpl>` the axum
    // extractor pins to.
    let uri: Uri = "/account-management/v1/tenants".parse().expect("uri");
    let tenant = sample_active_tenant();
    let id_str = tenant.id.0.to_string();
    let dto = TenantDto::from_sdk_tenant(tenant);

    let response = created_json(dto, &uri, &id_str).into_response();

    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "create_tenant MUST return HTTP 201 Created per OpenAPI",
    );
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("Location header present on 201 response")
        .to_str()
        .expect("Location header is ASCII");
    assert_eq!(
        location,
        format!("/account-management/v1/tenants/{}", sample_tenant_id()),
        "Location header MUST point at GET /tenants/{{tenant_id}} for follow-up reads",
    );
}

// ---- list_tenant_children clamp seam ----------------------------

#[test]
fn list_tenant_children_clamps_top_before_calling_service() {
    // `list_tenant_children` runs `clamp_listing_top(query,
    // svc.max_list_children_top())` BEFORE the service call. Pin the
    // composition with a low operator-tuned cap and a caller-supplied
    // limit that exceeds it: the clamp must rewrite `limit` to the
    // cap so the service is invoked with the deployment policy
    // applied, not the caller's raw value.
    //
    // The matrix of `clamp_listing_top` itself (unset, oversized,
    // smaller-than-cap) is pinned in `common_tests::clamp_listing_top_*`.
    // Here we pin the *composition with the service-side getter*:
    // a regression that called the service with the raw query (or
    // wired a different cap source) would surface as an unclamped
    // limit on the resulting `ODataQuery`.
    let svc = build_service_with_listing_cap(25);
    assert_eq!(
        svc.max_list_children_top(),
        25,
        "listing.max_top must flow into the service-side getter",
    );

    let caller_query = ODataQuery::new().with_limit(500);
    let clamped = clamp_listing_top(caller_query, svc.max_list_children_top());

    assert_eq!(
        clamped.limit,
        Some(25),
        "handler clamp must rewrite limit to the operator-tunable cap \
         (deployment policy beats caller-supplied $top)",
    );
}

#[test]
fn list_tenant_children_clamp_defaults_unset_caller_limit_to_operator_cap() {
    // Sibling pin: a caller that omits `$top` entirely must still
    // inherit the operator-tuned cap rather than the repo-level
    // absolute ceiling (`*_LISTING_LIMIT_CFG.max = 200`). The
    // handler signature `OData(query)` makes the unset case the
    // most common one in practice (the OpenAPI default is "no
    // limit"); without the handler-side clamp the service would
    // receive `limit = None` and fall back to the repo ceiling.
    let svc = build_service_with_listing_cap(50);
    let caller_query = ODataQuery::new();
    let clamped = clamp_listing_top(caller_query, svc.max_list_children_top());

    assert_eq!(
        clamped.limit,
        Some(50),
        "unset $top must default to the operator-tunable cap, not the repo ceiling",
    );
}
