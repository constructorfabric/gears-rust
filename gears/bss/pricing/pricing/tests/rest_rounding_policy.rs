//! `GET/PUT /bss-pricing/v1/config/rounding-policy` — the tenant's default
//! rounding policy (PRD §17.4, D-320).
//!
//! # What is under test, and why the last case is the point of the surface
//!
//! Four of these pin the resource's own contract — unset reads as a state and
//! not an absence, a `PUT` under the read tag lands, a stale tag writes nothing,
//! blank is refused so that unset keeps one spelling. The fifth pins the reason
//! the route was built: with a default set, a plan whose rows carry **no**
//! `roundingPolicyRef` publishes, where before this surface existed the same
//! plan could not — `default_rounding_policy_ref` had no writer, so
//! `foundation.rounding_policy_resolved` always took its fail-closed arm and
//! every row of every plan had to carry its own.
//!
//! Without that last case the suite would prove a policy row can be written and
//! nothing about what writing it *does*, which is the shape of a green test over
//! an inert feature.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
mod rest_support;

use axum::http::StatusCode;
use bss_pricing::api::rest::rounding_policy::ROUNDING_POLICY;
use bss_pricing::domain::price_record::PriceContent;
use bss_pricing::domain::scope_key::PlanId;
use bss_pricing::infra::storage::entity::rounding_policy_taxonomy;
use bss_pricing::infra::storage::repo::NewPriceDraft;
use rest_support::{
    Harness, body_json, etag_of, problem_code, publishable_row, publishable_scope_key,
    seed_publishable_shape, with_headers,
};
use sea_orm::ActiveValue::Set;
use sea_orm::EntityTrait;
use toolkit_db::secure::{AccessScope, SecureInsertExt};
use uuid::Uuid;

/// The submitting principal.
const SUBMITTER: Uuid = Uuid::from_u128(0x5_7d);

// ---------------------------------------------------------------------------
// The surface itself.
// ---------------------------------------------------------------------------

async fn read_policy(harness: &Harness) -> (StatusCode, Option<String>, serde_json::Value) {
    let response = harness
        .allowed()
        .send(with_headers("GET", ROUNDING_POLICY, None, &[]))
        .await;
    let status = response.status();
    let tag = etag_of(&response);
    (status, tag, body_json(response).await)
}

async fn write_policy(
    harness: &Harness,
    value: serde_json::Value,
    tag: &str,
) -> axum::http::Response<axum::body::Body> {
    harness
        .allowed()
        .send(with_headers(
            "PUT",
            ROUNDING_POLICY,
            Some(serde_json::json!({ "default_rounding_policy_ref": value })),
            &[("if-match", tag)],
        ))
        .await
}

/// A tenant that has set nothing is answered **200 with `null`**, and it carries
/// a tag.
///
/// Not a 404, for the tax-display surface's reason: unset is a state — the one
/// every tenant is in — and answering an absence would make the bootstrap `PUT`
/// unaskable, there being no tag to assert.
#[tokio::test]
async fn a_tenant_that_set_nothing_reads_null_with_a_tag() {
    let harness = Harness::new().await;

    let (status, tag, body) = read_policy(&harness).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["default_rounding_policy_ref"], serde_json::Value::Null);
    assert!(tag.is_some(), "unset still has a representation to assert");
}

/// A `PUT` under the tag the `GET` handed back lands, and the `GET` agrees.
#[tokio::test]
async fn a_put_under_the_read_tag_sets_the_default_and_the_get_agrees() {
    let harness = Harness::new().await;
    let (_, tag, _) = read_policy(&harness).await;

    let response = write_policy(
        &harness,
        serde_json::json!("half_even"),
        &tag.expect("a tag"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["default_rounding_policy_ref"],
        serde_json::json!("half_even")
    );

    let (_, _, body) = read_policy(&harness).await;
    assert_eq!(
        body["default_rounding_policy_ref"],
        serde_json::json!("half_even"),
        "the write is what the next reader sees, not just what the response said"
    );
}

/// The default can be cleared, and clearing is spelled `null`.
///
/// The state matters: a tenant who clears goes back to needing a reference on
/// every published row, so this is a real setting and not a one-way door.
#[tokio::test]
async fn the_default_can_be_cleared_back_to_null() {
    let harness = Harness::new().await;
    let (_, tag, _) = read_policy(&harness).await;
    // **The transition, not the end state.** Both assertions below hold in a run
    // where the default was never set at all - an unset tenant already reads
    // `null` - so the set has to be proved before it can be proved cleared.
    let set = write_policy(
        &harness,
        serde_json::json!("half_even"),
        &tag.expect("a tag"),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);

    let (_, set_tag, stored) = read_policy(&harness).await;
    assert_eq!(
        stored["default_rounding_policy_ref"],
        serde_json::json!("half_even"),
        "the premise: the default is set before this case clears it"
    );

    let response = write_policy(&harness, serde_json::Value::Null, &set_tag.expect("a tag")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let (_, _, body) = read_policy(&harness).await;
    assert_eq!(body["default_rounding_policy_ref"], serde_json::Value::Null);
}

/// A tag that no longer describes the stored default is refused, and nothing is
/// written.
///
/// The second half is the load-bearing one: a refusal that had already written
/// would be the lost update the precondition exists to prevent.
#[tokio::test]
async fn a_stale_tag_is_refused_and_writes_nothing() {
    let harness = Harness::new().await;
    let (_, first_tag, _) = read_policy(&harness).await;
    let first_tag = first_tag.expect("a tag");
    write_policy(&harness, serde_json::json!("half_even"), &first_tag).await;

    // The same tag again: it described the unset state, which has moved.
    let response = write_policy(&harness, serde_json::json!("bankers"), &first_tag).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(response).await, "STALE_VERSION");
    let (_, _, body) = read_policy(&harness).await;
    assert_eq!(
        body["default_rounding_policy_ref"],
        serde_json::json!("half_even"),
        "a refused precondition leaves the stored default exactly where it was"
    );
}

/// A blank reference is refused rather than stored beside `null`.
///
/// D-318's rule on `planName`, applied to the field one surface over: two
/// spellings of unset is a state every reader has to special-case, and the first
/// that forgets shows a default that is there and means nothing.
#[tokio::test]
async fn a_blank_reference_is_refused() {
    let harness = Harness::new().await;
    let (_, tag, _) = read_policy(&harness).await;

    let response = write_policy(&harness, serde_json::json!("   "), &tag.expect("a tag")).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let (_, _, body) = read_policy(&harness).await;
    assert_eq!(
        body["default_rounding_policy_ref"],
        serde_json::Value::Null,
        "the refusal wrote nothing"
    );
}

/// **The reason the surface exists**: with a default set, a plan whose rows
/// carry no `roundingPolicyRef` publishes.
///
/// Before this route `default_rounding_policy_ref` had no writer at all, so
/// `foundation.rounding_policy_resolved` always took its fail-closed arm and
/// this same plan answered `ROUNDING_POLICY_UNRESOLVED`. The assertion is
/// therefore about the rule's **other** arm, which nothing in this crate could
/// reach — and the refusal is asserted first, so the case cannot pass by the
/// plan having been publishable all along.
#[tokio::test]
async fn with_a_default_set_a_plan_whose_rows_have_no_ref_publishes() {
    let harness = Harness::new().await;
    let plan_id = Uuid::now_v7();
    let shape = seed_publishable_shape(&harness, plan_id).await;
    let plan = PlanId::new(plan_id);
    let scope = harness.scope();

    // One row, publishable in every respect but the rounding reference.
    let price_id = Uuid::now_v7();
    harness
        .state
        .prices
        .create_draft(
            &scope,
            harness.tenant,
            NewPriceDraft {
                price_id,
                scope_key: publishable_scope_key(plan, shape.phase, "eu"),
                content: PriceContent {
                    rounding_policy_ref: None,
                    ..publishable_row()
                },
                created_by: rest_support::SEED_ACTOR,
                created_at_utc: rest_support::at(10),
                correlation_id: Uuid::from_u128(0x_c0_11_a7_11),
            },
        )
        .await
        .expect("author the row");

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

    let refused = publish(&harness, plan_id, &shape.etag()).await;
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "with no tenant default and no per-row ref, publish must refuse"
    );
    let detail = body_json(refused).await.to_string();
    assert!(
        detail.contains("ROUNDING_POLICY_UNRESOLVED"),
        "the refusal names the rounding rule; got {detail}"
    );

    let (_, tag, _) = read_policy(&harness).await;
    let set = write_policy(
        &harness,
        serde_json::json!("half_even"),
        &tag.expect("a tag"),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);

    let after = publish(&harness, plan_id, &shape.etag()).await;
    let status = after.status();
    let body = body_json(after).await;
    // **202 and an opened unit**, not merely "the code is absent". A publish that
    // failed for some other reason would satisfy an absence assertion just as
    // well, and the claim is that the plan now clears the whole rule set.
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "the tenant default resolves every row that carries none; got {body}"
    );
    assert_eq!(body["outcome"], serde_json::json!("submitted_for_approval"));
}

/// **The vocabulary, end to end**: an undeclared reference is refused at publish,
/// and declaring it lets the same plan through.
///
/// The pair matters more than either half. A test that only declared a value and
/// published would pass against a rule whose operand nobody loaded — the empty
/// set means "unconstrained", so an unwired `rule_params` looks exactly like a
/// satisfied vocabulary. Refusing first is what proves the set reached the rule.
#[tokio::test]
async fn an_undeclared_rounding_reference_is_refused_and_declaring_it_lets_the_plan_publish() {
    let harness = Harness::new().await;
    let plan_id = Uuid::now_v7();
    let shape = seed_publishable_shape(&harness, plan_id).await;
    let plan = PlanId::new(plan_id);
    let scope = harness.scope();

    let price_id = Uuid::now_v7();
    harness
        .state
        .prices
        .create_draft(
            &scope,
            harness.tenant,
            NewPriceDraft {
                price_id,
                scope_key: publishable_scope_key(plan, shape.phase, "eu"),
                content: PriceContent {
                    rounding_policy_ref: Some("half_even".to_owned()),
                    ..publishable_row()
                },
                created_by: rest_support::SEED_ACTOR,
                created_at_utc: rest_support::at(10),
                correlation_id: Uuid::from_u128(0x_c0_11_a7_12),
            },
        )
        .await
        .expect("author the row");
    let conn = harness.state.db.conn().expect("conn");
    common::schedule_coverage_window(
        &conn,
        &scope,
        harness.tenant,
        price_id,
        rest_support::seed_stamp(),
    )
    .await;

    // A vocabulary that does not contain the row's reference.
    declare_rounding_value(&harness, "bankers").await;

    let refused = publish(&harness, plan_id, &shape.etag()).await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let detail = body_json(refused).await.to_string();
    assert!(
        detail.contains("ROUNDING_POLICY_UNKNOWN"),
        "the refusal names the vocabulary rule; got {detail}"
    );

    declare_rounding_value(&harness, "half_even").await;

    let after = publish(&harness, plan_id, &shape.etag()).await;
    let status = after.status();
    let body = body_json(after).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "the declared reference clears the rule; got {body}"
    );
}

/// An empty vocabulary constrains nothing — the opt-in reading (D-334).
///
/// The negative control for the case above: without it, a rule that refused
/// *every* reference would satisfy the refusal half and look correct.
#[tokio::test]
async fn a_tenant_with_no_declared_vocabulary_publishes_any_reference() {
    let harness = Harness::new().await;
    let plan_id = Uuid::now_v7();
    let shape = seed_publishable_shape(&harness, plan_id).await;
    let plan = PlanId::new(plan_id);
    let scope = harness.scope();

    let price_id = Uuid::now_v7();
    harness
        .state
        .prices
        .create_draft(
            &scope,
            harness.tenant,
            NewPriceDraft {
                price_id,
                scope_key: publishable_scope_key(plan, shape.phase, "eu"),
                content: PriceContent {
                    rounding_policy_ref: Some("anything_at_all".to_owned()),
                    ..publishable_row()
                },
                created_by: rest_support::SEED_ACTOR,
                created_at_utc: rest_support::at(10),
                correlation_id: Uuid::from_u128(0x_c0_11_a7_13),
            },
        )
        .await
        .expect("author the row");
    let conn = harness.state.db.conn().expect("conn");
    common::schedule_coverage_window(
        &conn,
        &scope,
        harness.tenant,
        price_id,
        rest_support::seed_stamp(),
    )
    .await;

    let after = publish(&harness, plan_id, &shape.etag()).await;
    assert_eq!(
        after.status(),
        StatusCode::ACCEPTED,
        "declaring nothing is not opting in; got {}",
        body_json(after).await
    );
}

/// A default naming a value the tenant's own vocabulary does not declare is
/// refused **at this door** (review F1, 2026-08-19, D-348).
///
/// The default is a reference like any other, and until this refusal it was the
/// one reference nothing judged: `RoundingPolicyDeclared` carries `tenant_default`
/// but runs only on the publish path, while `infra::supersession`,
/// `infra::cutover` and mass repricing freeze the default onto a row with no rule
/// over it at all. So a default outside the vocabulary was refused at publish and
/// frozen through the other three doors — the asymmetry
/// `RoundingPolicyDeclared::violation_for`'s own doc describes and leaves for a
/// decision.
#[tokio::test]
async fn a_default_outside_the_declared_vocabulary_is_refused() {
    let harness = Harness::new().await;
    declare_rounding_value(&harness, "half_even/2").await;
    let (_, tag, _) = read_policy(&harness).await;

    let response = write_policy(
        &harness,
        serde_json::json!("banker/7"),
        &tag.expect("a tag"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        problem_code(response).await,
        "ROUNDING_POLICY_UNKNOWN",
        "the code is the one the publish rule reports for the same fault, not a fresh one"
    );
    let (_, _, body) = read_policy(&harness).await;
    assert_eq!(
        body["default_rounding_policy_ref"],
        serde_json::Value::Null,
        "and the refusal wrote nothing"
    );
}

/// The **positive control**: the same write lands when the value is declared.
///
/// Without it the refusal above would pass against a door that refused every
/// default.
#[tokio::test]
async fn a_default_inside_the_declared_vocabulary_is_stored() {
    let harness = Harness::new().await;
    declare_rounding_value(&harness, "half_even/2").await;
    let (_, tag, _) = read_policy(&harness).await;

    let response = write_policy(
        &harness,
        serde_json::json!("half_even/2"),
        &tag.expect("a tag"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (_, _, body) = read_policy(&harness).await;
    assert_eq!(body["default_rounding_policy_ref"], "half_even/2");
}

/// The **second positive control**, and the one that keeps this from becoming a
/// migration every existing tenant has to run: a tenant who has declared no
/// vocabulary constrains nothing.
///
/// `violation_for`'s own first clause (`self.declared.is_empty()`), which is how
/// `RegionsDeclared` behaves at its write door too. Without this case the refusal
/// above would be indistinguishable from one that requires a vocabulary before a
/// default can be set at all.
#[tokio::test]
async fn a_tenant_with_no_declared_vocabulary_may_set_any_default() {
    let harness = Harness::new().await;
    let (_, tag, _) = read_policy(&harness).await;

    let response = write_policy(
        &harness,
        serde_json::json!("anything/9"),
        &tag.expect("a tag"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let (_, _, body) = read_policy(&harness).await;
    assert_eq!(body["default_rounding_policy_ref"], "anything/9");
}

/// Declare one rounding value straight at the table.
///
/// Direct because the taxonomy surface is not what these cases are about — the
/// tax-display suite's `declare_region` carries the same reasoning — and because
/// what is under test is the publish rule's operand.
async fn declare_rounding_value(harness: &Harness, value: &str) {
    let conn = harness.db.conn().expect("conn");
    let row = rounding_policy_taxonomy::ActiveModel {
        tenant_id: Set(harness.tenant),
        value: Set(value.to_owned()),
        display_name: Set(format!("fixture {value}")),
        state: Set("active".to_owned()),
    };
    rounding_policy_taxonomy::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::allow_all(), &row)
        .expect("scope the value")
        .exec(&conn)
        .await
        .expect("declare the value");
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

// ---------------------------------------------------------------------------
// The vocabulary's own surface (D-334)
// ---------------------------------------------------------------------------

async fn read_vocabulary(harness: &Harness) -> (StatusCode, Option<String>, serde_json::Value) {
    let response = harness
        .allowed()
        .send(with_headers(
            "GET",
            "/bss-pricing/v1/config/rounding-policies",
            None,
            &[],
        ))
        .await;
    let status = response.status();
    let tag = etag_of(&response);
    (status, tag, body_json(response).await)
}

async fn write_vocabulary(
    harness: &Harness,
    values: serde_json::Value,
    tag: &str,
) -> axum::http::Response<axum::body::Body> {
    harness
        .allowed()
        .send(with_headers(
            "PUT",
            "/bss-pricing/v1/config/rounding-policies",
            Some(serde_json::json!({ "values": values })),
            &[("if-match", tag)],
        ))
        .await
}

/// A tenant that has declared nothing reads an empty set **with a tag**.
#[tokio::test]
async fn an_undeclared_vocabulary_reads_empty_with_a_tag() {
    let harness = Harness::new().await;

    let (status, tag, body) = read_vocabulary(&harness).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["values"], serde_json::json!([]));
    assert!(tag.is_some(), "the empty set is a state and carries a tag");
}

/// A `PUT` declares the set and the `GET` agrees; state defaults to `active`.
#[tokio::test]
async fn a_declared_set_round_trips_and_defaults_to_active() {
    let harness = Harness::new().await;
    let (_, tag, _) = read_vocabulary(&harness).await;

    let response = write_vocabulary(
        &harness,
        serde_json::json!([{ "value": "half_even", "display_name": "Half to even" }]),
        &tag.expect("a tag"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (_, _, body) = read_vocabulary(&harness).await;
    assert_eq!(body["values"][0]["value"], serde_json::json!("half_even"));
    assert_eq!(body["values"][0]["state"], serde_json::json!("active"));
}

/// A value the tenant default names cannot be retired, and nothing is written.
///
/// The guard's whole point: retiring under a live reference would leave the
/// default pointing at a value no vocabulary declares, which is the dangling
/// state the taxonomy exists to prevent.
#[tokio::test]
async fn a_value_the_default_names_cannot_be_retired() {
    let harness = Harness::new().await;

    let (_, vocab_tag, _) = read_vocabulary(&harness).await;
    write_vocabulary(
        &harness,
        serde_json::json!([{ "value": "half_even", "display_name": "Half to even" }]),
        &vocab_tag.expect("a tag"),
    )
    .await;

    let (_, policy_tag, _) = read_policy(&harness).await;
    let set = write_policy(
        &harness,
        serde_json::json!("half_even"),
        &policy_tag.expect("a tag"),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);

    // Now drop it from the set, which is a retirement.
    let (_, vocab_tag, _) = read_vocabulary(&harness).await;
    let refused =
        write_vocabulary(&harness, serde_json::json!([]), &vocab_tag.expect("a tag")).await;

    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(refused).await, "TAXONOMY_VALUE_IN_USE");
    let (_, _, body) = read_vocabulary(&harness).await;
    assert_eq!(
        body["values"][0]["state"],
        serde_json::json!("active"),
        "a refused retirement writes nothing at all"
    );
}

/// A stale tag on the vocabulary is refused and writes nothing.
#[tokio::test]
async fn a_stale_vocabulary_tag_is_refused() {
    let harness = Harness::new().await;
    let (_, first, _) = read_vocabulary(&harness).await;
    let first = first.expect("a tag");
    write_vocabulary(
        &harness,
        serde_json::json!([{ "value": "bankers", "display_name": "Bankers" }]),
        &first,
    )
    .await;

    let response = write_vocabulary(
        &harness,
        serde_json::json!([{ "value": "half_even", "display_name": "Half even" }]),
        &first,
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    // Which 409, not merely some 409: this route answers `TAXONOMY_VALUE_IN_USE`
    // on the same status, and the sibling `a_stale_tag_is_refused_and_writes_nothing`
    // already names its code for this reason. A precondition check that stopped
    // firing while the other arm answered instead left this green.
    assert_eq!(problem_code(response).await, "STALE_VERSION");
    let (_, _, body) = read_vocabulary(&harness).await;
    assert_eq!(body["values"][0]["value"], serde_json::json!("bankers"));
}

/// A blank value is refused by the surface rather than by a constraint.
#[tokio::test]
async fn a_blank_vocabulary_value_is_refused() {
    let harness = Harness::new().await;
    // **The refusal needs something to lose.** A tenant's vocabulary starts empty,
    // so a readback taken against the default compares nothing with nothing and is
    // satisfied by a refusal that wiped the set on its way out — the fixture-
    // degenerate positive. One value is seeded first and asserted present.
    let (_, tag, _) = read_vocabulary(&harness).await;
    write_vocabulary(
        &harness,
        serde_json::json!([{ "value": "half_even", "display_name": "Half to even" }]),
        &tag.expect("a tag"),
    )
    .await;
    let (_, tag, before) = read_vocabulary(&harness).await;
    assert_eq!(
        before["values"].as_array().map(Vec::len),
        Some(1),
        "the control this case is about losing: {before}"
    );

    let response = write_vocabulary(
        &harness,
        serde_json::json!([{ "value": "", "display_name": "nothing" }]),
        &tag.expect("a tag"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // The readback its policy sibling `a_blank_reference_is_refused` carries: a
    // `PUT` replaces the set wholesale, so a refusal that had already written
    // would have replaced the vocabulary with the one blank entry it rejected.
    let (_, _, after) = read_vocabulary(&harness).await;
    assert_eq!(
        after["values"], before["values"],
        "a refused write leaves the vocabulary exactly where it was"
    );
}

/// A value listed twice is refused by the surface rather than by the key.
///
/// Not left to the store's `(tenant_id, value)` key, which never sees the
/// repetition: `apply_replace_rounding_policy` collects the submitted set into a
/// `BTreeMap` keyed by value, so the alternative to this refusal is a 200 for a
/// set the author did not send. `rest_taxonomies`' `a_repeated_value_is_refused`
/// is the same case one surface over, and the same argument applies to it.

#[tokio::test]
async fn a_value_listed_twice_in_one_body_is_refused() {
    let harness = Harness::new().await;
    let (_, tag, _) = read_vocabulary(&harness).await;
    write_vocabulary(
        &harness,
        serde_json::json!([{ "value": "half_even", "display_name": "Half to even" }]),
        &tag.expect("a tag"),
    )
    .await;
    let (_, tag, before) = read_vocabulary(&harness).await;
    assert_eq!(
        before["values"].as_array().map(Vec::len),
        Some(1),
        "the control this case is about losing: {before}"
    );

    let response = write_vocabulary(
        &harness,
        serde_json::json!([
            { "value": "bankers", "display_name": "B", "state": "active" },
            { "value": "bankers", "display_name": "B", "state": "retired" }
        ]),
        &tag.expect("a tag"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // **Which** 400: this route renders several — a blank value, an unparsable
    // state token, a body that will not deserialize — and a bare status cannot
    // say which one answered.

    let problem = body_json(response).await.to_string();
    assert!(
        problem.contains("appears twice in this body"),
        "the refusal must name the repetition, not the state token beside it: {problem}"
    );
    let (_, _, after) = read_vocabulary(&harness).await;
    assert_eq!(
        after["values"], before["values"],
        "a refused write leaves the vocabulary exactly where it was"
    );
}
