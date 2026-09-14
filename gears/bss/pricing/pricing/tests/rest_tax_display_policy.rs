//! `GET/PUT /bss-pricing/v1/config/tax-display-policy` — C4's enforcement mode,
//! driven through the real router (`design/04-currency-tax.md` §5, §6,
//! `inst-td-policy`; D-154).
//!
//! # Why this file exists (Z12-5)
//!
//! The surface had **no behavioural test at any tier**. The route was declared in
//! `module_test`'s path census and catalogued in `rest_authz`'s, and the only
//! request ever made to it was `rest_authz`'s own — a `PUT` under a deliberately
//! stale `If-Match`, so the handler was refused before it wrote and the assertion
//! was only that the status is not 401/403. Nothing read the `GET`, nothing
//! completed a `PUT`, and `grep -rn "TaxDisplayPolicy" tests/` returned nothing at
//! all.
//!
//! The consequence was one branch further in. `infra::publish` resolves the
//! tenant's mode and hands it to the rule set, so a tenant on `warn` publishes
//! under a rule arm that **no integration test at any tier had ever executed**:
//! every publish in the crate ran under `FailClosed`, and the `warn` branch of
//! `inst-td-policy` would have hatched with green tests the first time an operator
//! selected it.
//!
//! # The two arms are asserted together, because the switch is about the difference
//!
//! C4 reads as one switch and D-154 splits it: `warn` relaxes the **rate** arm — a
//! `taxInclusive` row in a region declaring no tax rate — and does not touch the
//! **category** arm, which blocks publish whatever the policy says because
//! `taxCategory` is a pinned D-48 v1 descriptor element. A suite proving only the
//! first would read as proof of a switch that relaxes everything, which is the one
//! way the module's own doc says this surface gets misread. So the category case
//! runs under **both** modes and is refused under both.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
mod rest_support;

use axum::http::StatusCode;
use bss_pricing::api::rest::tax_display_policy::TAX_DISPLAY_POLICY;
use bss_pricing::domain::price_record::PriceContent;
use bss_pricing::domain::scope_key::PlanId;
use bss_pricing::infra::storage::entity::region_taxonomy;
use bss_pricing::infra::storage::repo::NewPriceDraft;
use rest_support::{
    Harness, body_json, etag_of, problem_code, publishable_row, publishable_scope_key, refused_by,
    seed_publishable_shape, with_headers,
};
use sea_orm::ActiveValue::Set;
use sea_orm::EntityTrait;
use toolkit_db::secure::{AccessScope, SecureInsertExt};
use uuid::Uuid;

/// The submitting principal.
const SUBMITTER: Uuid = Uuid::from_u128(0x5_7d);

/// A region no `common::declare_fixture_regions` value collides with, declared
/// **without** a tax rate.
const NO_RATE_REGION: &str = "td-norate";

/// The same, declared without a rate **and** without a default category.
const NO_BASIS_REGION: &str = "td-nobasis";

// ---------------------------------------------------------------------------
// The surface itself.
// ---------------------------------------------------------------------------

async fn read_policy(harness: &Harness) -> (StatusCode, Option<String>, serde_json::Value) {
    let response = harness
        .allowed()
        .send(with_headers("GET", TAX_DISPLAY_POLICY, None, &[]))
        .await;
    let status = response.status();
    let tag = etag_of(&response);
    (status, tag, body_json(response).await)
}

async fn write_policy(
    harness: &Harness,
    mode: &str,
    tag: &str,
) -> axum::http::Response<axum::body::Body> {
    harness
        .allowed()
        .send(with_headers(
            "PUT",
            TAX_DISPLAY_POLICY,
            Some(serde_json::json!({ "mode": mode })),
            &[("if-match", tag)],
        ))
        .await
}

/// A tenant that has configured nothing is answered **200 with the ratified
/// default**, and it carries a tag.
///
/// Not a 404: the module's own argument is that unset and fail-closed are one
/// state, which is what lets a first `PUT` assert an `If-Match` like any other
/// caller. A 404 here would make the bootstrap `PUT` unaskable.
#[tokio::test]
async fn a_tenant_that_configured_nothing_reads_the_fail_closed_default_with_a_tag() {
    let harness = Harness::new().await;

    let (status, tag, body) = read_policy(&harness).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unset and fail-closed are one state"
    );
    assert_eq!(body["mode"], "fail_closed");
    let tag = tag.expect("the resource always has a representation, so it always has a tag");
    assert!(
        tag.starts_with('"') && tag.len() > 2,
        "the tag is an opaque quoted entity tag, not an empty header: {tag}"
    );
}

/// The `PUT` under the `GET`'s tag flips the mode, and the flip is **read back**.
///
/// Both halves matter: the response body is what the caller is told, and the
/// re-read is what the next publish will resolve. A case asserting only the
/// response would pass against a handler that answered the mode it was sent
/// without writing it.
#[tokio::test]
async fn a_put_under_the_read_tag_flips_the_mode_and_the_get_agrees() {
    let harness = Harness::new().await;
    let (_, tag, _) = read_policy(&harness).await;
    let tag = tag.expect("the bootstrap tag");

    let response = write_policy(&harness, "warn", &tag).await;

    assert_eq!(response.status(), StatusCode::OK);
    let written = body_json(response).await;
    assert_eq!(written["mode"], "warn");

    let (status, fresh_tag, body) = read_policy(&harness).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["mode"], "warn",
        "the write landed, not merely answered"
    );
    assert_ne!(
        fresh_tag.expect("the tag of what now stands"),
        tag,
        "the representation moved, so its tag must have moved with it"
    );
}

/// A tag that no longer describes the policy is `409 STALE_VERSION`, and
/// **nothing is written**.
///
/// The stale tag is the bootstrap one **re-used after a flip**, which is the only
/// way to reach this refusal rather than the malformed-tag one below: the handler
/// resolves the asserted tag to a mode and the store puts that mode in the
/// `WHERE`, so a premise has to be *well-formed and out of date*, not merely
/// unrecognizable. A fabricated tag is answered `400` by the parse and never
/// reaches the compare-and-swap — which is what `rest_authz`'s own drive of this
/// route does, and why that drive could not see this path.
///
/// The re-read is the assertion that makes this about the precondition rather than
/// about the status: comparing the tag here and updating unconditionally there is
/// the T-7 defect this module's own comment records being rebuilt once already, and
/// it would answer 409 on the *next* call having applied this one.
#[tokio::test]
async fn a_tag_that_no_longer_describes_the_policy_is_refused_and_writes_nothing() {
    let harness = Harness::new().await;
    let (_, bootstrap, _) = read_policy(&harness).await;
    let bootstrap = bootstrap.expect("the bootstrap tag");
    assert_eq!(
        write_policy(&harness, "warn", &bootstrap).await.status(),
        StatusCode::OK,
        "the flip has to land, or the tag under test is not stale"
    );

    let response = write_policy(&harness, "fail_closed", &bootstrap).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(response).await, "STALE_VERSION");
    let (_, _, body) = read_policy(&harness).await;
    assert_eq!(
        body["mode"], "warn",
        "a refused precondition leaves the policy where the flip left it"
    );
}

/// A tag the parser cannot read at all is a `400`, not the 409 above.
///
/// The pair is the point: this module refuses a request it cannot *understand*,
/// and the store refuses one whose premise has *moved*. One status for both would
/// tell a client to re-read the `GET` when what they actually sent was garbage.
#[tokio::test]
async fn a_malformed_tag_is_a_client_fault_and_not_a_stale_premise() {
    let harness = Harness::new().await;

    let response = write_policy(&harness, "warn", "not-a-tag").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // The precondition and not the vocabulary: both render one `invalid_argument`
    // on one status, and the name's whole claim is that this is the client's tag
    // rather than a premise that went stale under them.
    refused_by(
        &body_json(response).await,
        "invalid_argument",
        "a strong entity tag is wrapped in double quotes",
    );
    let (_, _, body) = read_policy(&harness).await;

    assert_eq!(body["mode"], "fail_closed", "and nothing was written");
}

/// A mode outside the two-valued vocabulary is a `400`, named.
#[tokio::test]
async fn a_mode_outside_the_vocabulary_is_refused() {
    let harness = Harness::new().await;
    let (_, tag, _) = read_policy(&harness).await;

    let response = write_policy(&harness, "warn_only", &tag.expect("the bootstrap tag")).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // **Which** 400: this route renders several `invalid_argument` refusals —
    // the malformed tag its sibling drives among them — and only the sentence
    // separates them. No code is minted, so the sentence is the discriminator.
    refused_by(
        &body_json(response).await,
        "invalid_argument",
        "tax-display policy mode `warn_only` is not one of",
    );
    // And nothing was written, as this case's two siblings prove for their own
    // refusals: a `PUT` replaces the policy wholesale, so a refusal that had
    // already written would have replaced the mode with the one it rejected.
    let (_, _, after) = read_policy(&harness).await;
    assert_eq!(after["mode"], "fail_closed", "and nothing was written");
}

// ---------------------------------------------------------------------------
// What the mode governs: `inst-td-policy`'s two arms, through the publish route.
// ---------------------------------------------------------------------------

/// Declare a region carrying the D-01 markers this case needs, past the taxonomy
/// route.
///
/// Direct because the taxonomy surface is not what these cases are about —
/// `common::declare_fixture_regions`' own reason — and the values are the point:
/// every fixture region in this crate declares `tax_rate_present = true` and a
/// default category, which is exactly why no publish anywhere had ever met either
/// arm of this rule.
async fn declare_region(harness: &Harness, value: &str, category: Option<&str>, rate: bool) {
    let conn = harness.db.conn().expect("conn");
    let row = region_taxonomy::ActiveModel {
        tenant_id: Set(harness.tenant),
        value: Set(value.to_owned()),
        display_name: Set(format!("tax-display fixture {value}")),
        state: Set("active".to_owned()),
        tax_category: Set(category.map(ToOwned::to_owned)),
        tax_rate_present: Set(rate),
    };
    region_taxonomy::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::allow_all(), &row)
        .expect("scope the region")
        .exec(&conn)
        .await
        .expect("declare the region");
}

/// A plan whose one row is publishable in **every** respect but the tax basis:
/// tax-inclusive, on `region`, carrying `category` if any.
///
/// Built on `seed_publishable_shape` plus one row rather than on
/// `seed_publishable_plan`, because that seed authors its row on `eu` — a fixture
/// region that declares a rate — and a second row on a second market would put
/// `inst-td-basis-uniform` between this case and the rule it is about.
async fn seed_tax_inclusive_plan(
    harness: &Harness,
    plan_id: Uuid,
    region: &str,
    category: Option<&str>,
) -> String {
    let plan = PlanId::new(plan_id);
    let scope = harness.scope();
    let shape = seed_publishable_shape(harness, plan_id).await;

    let content = PriceContent {
        tax_inclusive: true,
        tax_category_ref: category.map(ToOwned::to_owned),
        ..publishable_row()
    };
    let price_id = Uuid::now_v7();
    harness
        .state
        .prices
        .create_draft(
            &scope,
            harness.tenant,
            NewPriceDraft {
                price_id,
                scope_key: publishable_scope_key(plan, shape.phase, region),
                content,
                created_by: rest_support::SEED_ACTOR,
                created_at_utc: rest_support::at(10),
                correlation_id: Uuid::from_u128(0x_c0_11_a7_10),
            },
        )
        .await
        .expect("author the tax-inclusive row");

    // `inst-wc-required`: no row publishes without a window on its canonical key.
    let conn = harness.state.db.conn().expect("conn");
    common::schedule_coverage_window(
        &conn,
        &scope,
        harness.tenant,
        price_id,
        rest_support::seed_stamp(),
    )
    .await;

    shape.etag()
}

async fn publish(
    harness: &Harness,
    plan_id: Uuid,
    tag: &str,
) -> axum::http::Response<axum::body::Body> {
    harness
        .allowed_as(SUBMITTER)
        .send(with_headers(
            "POST",
            &format!("/bss-pricing/v1/plans/{plan_id}/publish"),
            None,
            &[("if-match", tag)],
        ))
        .await
}

/// Set the tenant's mode through the route, so the world these cases publish in is
/// one an operator could have put it in.
async fn select(harness: &Harness, mode: &str) {
    let (_, tag, _) = read_policy(harness).await;
    let response = write_policy(harness, mode, &tag.expect("the bootstrap tag")).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the mode under test has to be the mode the tenant holds, or the publish below \
         proves nothing"
    );
}

/// **The rate arm, under `fail_closed`: refused.** The default half of the pair.
#[tokio::test]
async fn a_tax_inclusive_row_in_a_rateless_region_is_refused_under_fail_closed() {
    let harness = Harness::new().await;
    declare_region(&harness, NO_RATE_REGION, Some("standard"), false).await;
    let plan_id = Uuid::now_v7();
    let tag = seed_tax_inclusive_plan(&harness, plan_id, NO_RATE_REGION, None).await;

    let response = publish(&harness, plan_id, &tag).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(problem_code(response).await, "TAX_BASIS_INCOMPLETE");
}

/// **The rate arm, under `warn`: it publishes** — and this is the branch Z12-5
/// found unexecuted outside a domain unit test.
///
/// The same world as the case above with **one** thing moved, which is the whole
/// of what makes the pair a proof about the policy: same region, same row, same
/// seed, same tag. `report.warn(...)` instead of `report.violate(...)` in
/// `inst-td-policy`'s rate arm is the only difference the route can see, and until
/// this case existed nothing at any integration tier reached it.
#[tokio::test]
async fn the_same_row_publishes_under_warn() {
    let harness = Harness::new().await;
    declare_region(&harness, NO_RATE_REGION, Some("standard"), false).await;
    select(&harness, "warn").await;
    let plan_id = Uuid::now_v7();
    let tag = seed_tax_inclusive_plan(&harness, plan_id, NO_RATE_REGION, None).await;

    let response = publish(&harness, plan_id, &tag).await;

    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "`warn` relaxes the rate arm: the missing fact is a rate nobody in this catalog owns \
         and the Tax Engine is pre-GA, so a tenant may accept it"
    );
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "submitted_for_approval");
}

/// **The category arm blocks under both modes** (D-154), and the negative control
/// the case above rests on.
///
/// Without it `the_same_row_publishes_under_warn` would read as proof that `warn`
/// relaxes the whole rule, which is the module doc's own named misreading:
/// `taxCategory` is a pinned D-48 v1 descriptor element and a per-tenant display
/// policy may not publish past a pinned contract element. Both modes are driven in
/// one case so the two answers are known to be about the same world.
#[tokio::test]
async fn a_row_resolving_no_tax_category_is_refused_under_either_mode() {
    for mode in ["fail_closed", "warn"] {
        let harness = Harness::new().await;
        // Neither marker declared: the row states no category and the region
        // defaults none, so `coalesce(row, readiness)` is empty.
        declare_region(&harness, NO_BASIS_REGION, None, false).await;
        if mode == "warn" {
            select(&harness, mode).await;
        }
        let plan_id = Uuid::now_v7();
        let tag = seed_tax_inclusive_plan(&harness, plan_id, NO_BASIS_REGION, None).await;

        let response = publish(&harness, plan_id, &tag).await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "the category arm is unconditional, and `{mode}` does not reach it"
        );
        let body = body_json(response).await;
        let code = rest_support::code_in(&body);
        assert_eq!(code, "TAX_BASIS_INCOMPLETE", "under `{mode}`: {body}");
        // And it is the **category** half that answered, not the rate half wearing
        // the same code: the two arms share `TAX_BASIS_INCOMPLETE`, so a case
        // reading the code alone could not tell which one refused - and under
        // `warn` the rate half does not refuse at all, which is exactly what makes
        // this assertion load-bearing rather than decorative.
        let rendered = body.to_string();
        assert!(
            rendered.contains("resolves no tax category"),
            "under `{mode}` the refusal must carry the category arm's violation: {rendered}"
        );
        // **Which arm answered is asserted, not inferred from the code.** The two
        // arms share `TAX_BASIS_INCOMPLETE`, so a case reading the code alone could
        // not tell them apart - and the row here trips both under `fail_closed`
        // (rateless region, no category anywhere) and exactly one under `warn`.
        // That difference is the mode reaching the rule: under `warn` the rate
        // violation is **downgraded to an advisory** and leaves the refusal, while
        // the category violation stays. A refusal carrying both under `warn` would
        // mean the policy was never resolved.
        let rate_arm_refused = rendered.contains("cannot be decomposed without a rate");
        assert_eq!(
            rate_arm_refused,
            mode == "fail_closed",
            "under `{mode}` the rate arm must {} the refusal: {rendered}",
            if mode == "fail_closed" {
                "be in"
            } else {
                "have left"
            }
        );
    }
}
