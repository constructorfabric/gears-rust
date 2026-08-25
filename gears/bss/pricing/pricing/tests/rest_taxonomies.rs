//! `GET/PUT /config/taxonomies/{class}`, driven through the real router.
//!
//! # The positive control is the whole point of this file
//!
//! Every other suite in this crate can assume a taxonomy value exists, because
//! until now the only way to make one exist was direct SQL. What this surface
//! claims is that an **operator** can declare a value and that Slice 9's overlay
//! scope rule then accepts it — so the first case here drives exactly that, end
//! to end and through HTTP: `PUT` a brand, then author a brand-scoped overlay
//! against it.
//!
//! Without that case the file would be a pile of refusals, and a surface that
//! refuses everything passes every refusal test it has. It is also the specific
//! claim the slice exists to make good: `inst-plv-scope` and `inst-tx-region`
//! both validate against these four tables, and both shipped before any of them
//! had a writer.
//!
//! # Why the `ETag` cases are not ceremony here
//!
//! The `PUT` replaces the **whole** value set, so a lost update is not a
//! last-writer-wins on one field — it is the other author's addition being
//! **retired**, which reads afterwards exactly like a value somebody meant to
//! withdraw. That is why the precondition is asserted on the concurrent path and
//! not merely on a malformed header.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;
mod rest_support;

use axum::http::StatusCode;
use bss_pricing::api::rest::taxonomies::TAXONOMY;
use rest_support::{Harness, audit_rows, body_json, etag_of, problem_code, request, with_headers};
use serde_json::json;

/// The `CatalogAdmin` who configures the taxonomies.
const ADMIN: uuid::Uuid = uuid::Uuid::from_u128(0xca_d0);

fn path(class: &str) -> String {
    TAXONOMY.replace("{class}", class)
}

/// Read one taxonomy, answering the body and the tag together.
///
/// The two are taken from **one** response deliberately: a helper that read the
/// body and then re-read for a tag could hand a caller a tag describing a
/// different state than the body it was given, which is the exact failure the
/// precondition exists to catch.
async fn read(harness: &Harness, class: &str) -> (serde_json::Value, String) {
    let response = harness
        .allowed_as(ADMIN)
        .send(request("GET", &path(class), None))
        .await;
    assert_eq!(response.status(), StatusCode::OK, "the GET must answer 200");
    let tag = etag_of(&response).expect("a taxonomy read must carry its entity tag");
    (body_json(response).await, tag)
}

/// `PUT` a value set under a tag.
async fn put(
    harness: &Harness,
    class: &str,
    tag: &str,
    values: serde_json::Value,
) -> axum::http::Response<axum::body::Body> {
    harness
        .allowed_as(ADMIN)
        .send(with_headers(
            "PUT",
            &path(class),
            Some(json!({ "values": values })),
            &[("if-match", tag)],
        ))
        .await
}

/// One **published** overlay scoped to `(class, value)`, written through the
/// entity.
///
/// The authoring route cannot produce this state in one call — a submit opens an
/// always-material approval unit (D-50) — and what is under test here is the
/// taxonomy guard, not the overlay lifecycle.
async fn seed_published_overlay(harness: &Harness, class: &str, value: &str) {
    use sea_orm::ActiveValue::Set;
    use sea_orm::EntityTrait;
    use toolkit_db::secure::{AccessScope, SecureInsertExt};

    let conn = harness.db.conn().expect("conn");
    let row = bss_pricing::infra::storage::entity::price_overlay::ActiveModel {
        price_overlay_id: Set(uuid::Uuid::from_u128(0x0e_9a)),
        revision: Set(1),
        tenant_id: Set(harness.tenant),
        lifecycle_state: Set("published".to_owned()),
        scope_class: Set(class.to_owned()),
        scope_value: Set(value.to_owned()),
        precedence: Set(20),
        effective_from: Set(None),
        effective_to: Set(None),
        tax_basis: Set("delegated_tariffs".to_owned()),
        disclosure: Set("restricted".to_owned()),
        target_ref: Set(json!({"plans": []})),
        row_version: Set(0),
    };
    bss_pricing::infra::storage::entity::price_overlay::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::allow_all(), &row)
        .expect("scope")
        .exec(&conn)
        .await
        .expect("seed a published overlay");
}

/// One entry of a rendered taxonomy, by value.
///
/// By value and never by index: the harness declares the fixture region universe
/// (`inst-tx-region` is fail-closed, so a tenant that declares nothing publishes
/// nothing), and the list is ordered by value — so position 0 is whichever code
/// sorts first, not the one a case happens to care about.
fn entry_for<'a>(body: &'a serde_json::Value, value: &str) -> &'a serde_json::Value {
    body["values"]
        .as_array()
        .expect("values is an array")
        .iter()
        .find(|v| v["value"] == value)
        .unwrap_or_else(|| panic!("no `{value}` in the rendered taxonomy"))
}

fn codes(body: &serde_json::Value) -> Vec<String> {
    body["values"]
        .as_array()
        .expect("values is an array")
        .iter()
        .map(|v| {
            format!(
                "{}:{}",
                v["value"].as_str().expect("value"),
                v["state"].as_str().expect("state")
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The positive control.
// ---------------------------------------------------------------------------

/// A tenant with no taxonomy is answered `200` with an empty list **and a tag**.
///
/// That is a state, not an absent resource, and it is what makes the bootstrap
/// reachable: a first `PUT` reads its precondition off this response like every
/// other caller. It is also the sentence `threshold_policy` had to withdraw a
/// false version of, so it is pinned rather than assumed.
#[tokio::test]
async fn a_tenant_with_no_values_reads_200_with_an_empty_list_and_a_tag() {
    let harness = Harness::new().await;

    let (body, tag) = read(&harness, "brand").await;

    assert_eq!(body["class"], "brand");
    assert_eq!(codes(&body), Vec::<String>::new());
    assert!(
        !tag.is_empty(),
        "the bootstrap carries a tag like any other"
    );
}

/// The round trip: declare a value, read it back, and see it in the list.
#[tokio::test]
async fn a_put_declares_a_value_and_the_get_reads_it_back() {
    let harness = Harness::new().await;
    let (_, tag) = read(&harness, "brand").await;

    let response = put(
        &harness,
        "brand",
        &tag,
        json!([{ "value": "acme", "display_name": "Acme Corp" }]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (body, _) = read(&harness, "brand").await;
    assert_eq!(codes(&body), ["acme:active"]);
    assert_eq!(body["values"][0]["display_name"], "Acme Corp");
}

/// **The claim this slice exists to make good.** A value declared through this
/// surface is a value Slice 9's overlay scope rule accepts.
///
/// Driven through HTTP on both ends rather than through the repositories,
/// because what was missing was never a function — it was a *route*. An operator
/// holding only the API could not do this at all, and a test that reached past
/// the API to seed the brand would prove the rule works while leaving the actual
/// gap wide open.
#[tokio::test]
async fn a_brand_declared_here_is_one_a_brand_scoped_overlay_can_name() {
    let harness = Harness::new().await;
    let (_, tag) = read(&harness, "brand").await;

    let declared = put(
        &harness,
        "brand",
        &tag,
        json!([{ "value": "acme", "display_name": "Acme Corp" }]),
    )
    .await;
    assert_eq!(declared.status(), StatusCode::OK, "the brand is declared");

    let overlay = harness
        .allowed_as(ADMIN)
        .send(with_headers(
            "POST",
            "/bss-pricing/v1/price-overlays",
            Some(json!({
                "scope_class": "brand",
                "scope_value": "acme",
                "precedence": 10,
                "tax_basis": "delegated_tariffs",
                "target_plan_ids": [],
                "lines": [{
                    "adjustment_kind": "discount",
                    "magnitude_kind": "percent_bp",
                    "adjustment_value": 500,
                }]
            })),
            &[("idempotency-key", "brand-overlay-1")],
        ))
        .await;

    // **`CREATED`, not merely "some code other than SCOPE_VALUE_UNKNOWN".** The
    // weaker assertion is satisfied by a malformed request — which is exactly what
    // this case did on its first run, answering 400 for a missing field while
    // "proving" the scope rule accepted the brand. A positive control has to
    // assert the positive outcome.
    assert_eq!(
        overlay.status(),
        StatusCode::CREATED,
        "a brand declared through this surface must satisfy inst-plv-scope: if this fails, the \
         write surface and the read Slice 9 makes of it disagree about what `declared` means"
    );
}

// ---------------------------------------------------------------------------
// The whole-set semantics.
// ---------------------------------------------------------------------------

/// A value the body omits is retired, and stays readable.
#[tokio::test]
async fn a_value_the_body_omits_is_retired_and_still_listed() {
    let harness = Harness::new().await;
    let (_, tag) = read(&harness, "partner").await;
    put(
        &harness,
        "partner",
        &tag,
        json!([
            { "value": "reseller-a", "display_name": "A" },
            { "value": "reseller-b", "display_name": "B" }
        ]),
    )
    .await;

    let (_, tag) = read(&harness, "partner").await;
    let response = put(
        &harness,
        "partner",
        &tag,
        json!([{ "value": "reseller-a", "display_name": "A" }]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (body, _) = read(&harness, "partner").await;
    assert_eq!(codes(&body), ["reseller-a:active", "reseller-b:retired"]);
}

// ---------------------------------------------------------------------------
// The precondition.
// ---------------------------------------------------------------------------

/// A `PUT` with no `If-Match` is refused before anything is written.
#[tokio::test]
async fn a_put_without_if_match_is_refused() {
    let harness = Harness::new().await;

    let response = harness
        .allowed_as(ADMIN)
        .send(request(
            "PUT",
            &path("brand"),
            Some(json!({ "values": [{ "value": "acme", "display_name": "A" }] })),
        ))
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let (body, _) = read(&harness, "brand").await;
    assert_eq!(codes(&body), Vec::<String>::new(), "nothing was written");
}

/// **The lost update this precondition exists to stop.**
///
/// Two admins read the same brand list and each add one value. Without the
/// precondition the second `PUT` — a whole-set replacement authored against a
/// stale reading — would land and silently **retire** the first's addition. The
/// tag makes it a `409` instead.
#[tokio::test]
async fn a_concurrent_whole_set_put_is_refused_rather_than_retiring_the_other_authors_value() {
    let harness = Harness::new().await;
    let (_, shared_tag) = read(&harness, "brand").await;

    let first = put(
        &harness,
        "brand",
        &shared_tag,
        json!([{ "value": "acme", "display_name": "Acme" }]),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    // The second author is still holding the tag they read before the first
    // landed — which is exactly the sequential case an approval unit or a
    // "one open change" guard would not catch.
    let second = put(
        &harness,
        "brand",
        &shared_tag,
        json!([{ "value": "zenith", "display_name": "Zenith" }]),
    )
    .await;

    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(second).await, "STALE_VERSION");

    let (body, _) = read(&harness, "brand").await;
    assert_eq!(
        codes(&body),
        ["acme:active"],
        "the first author's value survives and the second's was not applied"
    );
}

/// The tag moves when a value is **re-labelled**, not only when membership
/// changes.
///
/// A validator that does not change when the representation changes is broken
/// rather than lenient — the argument the threshold policy's tag makes for
/// covering its pending unit as well as its version.
#[tokio::test]
async fn the_tag_moves_on_a_relabel() {
    let harness = Harness::new().await;
    let (_, tag) = read(&harness, "brand").await;
    put(
        &harness,
        "brand",
        &tag,
        json!([{ "value": "acme", "display_name": "Acme" }]),
    )
    .await;
    let (_, before) = read(&harness, "brand").await;

    put(
        &harness,
        "brand",
        &before,
        json!([{ "value": "acme", "display_name": "Acme Corporation" }]),
    )
    .await;
    let (_, after) = read(&harness, "brand").await;

    assert_ne!(before, after, "a re-label changes the representation");
}

/// The tag moves when **only** D-01's region markers change.
///
/// Found by review, and it is a lost update with no race in it. `view_of`
/// renders five fields and the digest covered three, so a `PUT` that changed a
/// region's `taxCategory` left the validator fixed: a second operator holding
/// the pre-change tag could revert it and be told the precondition held. The
/// reverted field is C4's `RegionTaxReadiness` input, so the blast radius is a
/// publish blocked, or allowed, for a readiness nobody authored.
///
/// It also made the tag a lying strong validator: a conditional `GET` answers
/// 304 for a body that changed.
#[tokio::test]
async fn the_tag_moves_when_only_the_region_tax_markers_change() {
    let harness = Harness::new().await;
    let (_, tag) = read(&harness, "region").await;
    put(
        &harness,
        "region",
        &tag,
        json!([{
            "value": "eu", "display_name": "Europe",
            "tax_category": "standard", "tax_rate_present": true
        }]),
    )
    .await;
    let (_, before) = read(&harness, "region").await;

    // Value, label and state all unchanged; only the two markers move.
    let response = put(
        &harness,
        "region",
        &before,
        json!([{
            "value": "eu", "display_name": "Europe",
            "tax_category": "reduced", "tax_rate_present": false
        }]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let (_, after) = read(&harness, "region").await;

    assert_ne!(
        before, after,
        "a taxCategory change is a change to the representation, so the validator must move"
    );
}

/// A tag read from one class does not satisfy a `PUT` on another — **and the two
/// classes are compared while their contents are identical**.
///
/// The first version of this case seeded the partner list before comparing, so
/// the two representations differed by *content* and the tags differed whether
/// or not the class was in the digest. A probe that removed the class from the
/// digest left it green: it asserted the conclusion without testing the reason.
///
/// Both lists are therefore empty here, which is the only state in which the
/// class is the **sole** difference between the two representations. Without it
/// a tag read off any tenant's empty brand list would satisfy a `PUT` on their
/// empty partner list — and since a `PUT` is a whole-set replacement, that is a
/// write to the wrong universe under a precondition that appeared to hold.
#[tokio::test]
async fn a_tag_from_another_class_does_not_satisfy_this_one() {
    let harness = Harness::new().await;

    let (brand_body, brand_tag) = read(&harness, "brand").await;
    let (partner_body, partner_tag) = read(&harness, "partner").await;
    assert_eq!(
        codes(&brand_body),
        codes(&partner_body),
        "the two lists must be indistinguishable by content for this case to mean anything"
    );
    assert_ne!(
        brand_tag, partner_tag,
        "two empty taxonomies still have different tags, because the class is in the digest"
    );

    let response = put(
        &harness,
        "partner",
        &brand_tag,
        json!([{ "value": "reseller-b", "display_name": "B" }]),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "four taxonomies are four resources, and one tag must not travel between them"
    );
}

// ---------------------------------------------------------------------------
// The body's own rules.
// ---------------------------------------------------------------------------

/// An unaddressable class is refused, and the refusal names the four that are.
#[tokio::test]
async fn an_unaddressable_class_is_refused_naming_the_four() {
    let harness = Harness::new().await;

    // `orgTier` joins the unaddressable list under D-241: it is the spelling §5
    // used to carry, and it is **refused rather than aliased**, because two
    // spellings that both route is the state in which neither is canonical.
    for class in ["global", "customer_group", "orgTier"] {
        let response = harness
            .allowed_as(ADMIN)
            .send(request("GET", &path(class), None))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "`{class}` is not an addressable universe"
        );

        // **"Naming the four" was the unasserted half of this test's own name**
        // until 2026-08-20. The enumeration is the only thing that tells an
        // operator which segment to type instead of the one they typed, and it
        // could have been dropped from `parse_class`'s message entirely with the
        // status assertion above still green. Read off the whole document rather
        // than one member, `rest_repricing_runs`'s reason: this route's refusal
        // renders no wire code, so the detail is the whole of it.
        let problem = body_json(response).await.to_string();
        for addressable in ["region", "brand", "partner", "org_tier"] {
            assert!(
                problem.contains(addressable),
                "the refusal must name `{addressable}` as addressable: {problem}"
            );
        }
        assert!(
            problem.contains(class),
            "and it must name the segment that was refused, so `{class}` is not \
             mistaken for a typo in a word spelled correctly: {problem}"
        );
    }
}

/// The org-tier class is addressed — and echoed — as `org_tier` (D-241).
///
/// **Both halves matter and the second is the one that used to be wrong**: the
/// path segment is what an operator types, and `class` in the response body is
/// what a client reads back. Before D-241 the segment was `orgTier` while the same
/// class arrived as `org_tier` in an overlay's `scopeClass`, so a client generated
/// from the `OpenAPI` document carried both spellings for one thing.
#[tokio::test]
async fn the_org_tier_class_is_addressed_and_echoed_as_one_token() {
    let harness = Harness::new().await;

    let (body, _) = read(&harness, "org_tier").await;

    assert_eq!(body["class"], "org_tier");
}

/// The two `tax_*` markers are refused on the three non-region classes rather
/// than silently dropped.
///
/// A field that vanishes reads, at the operator's end, exactly like one that
/// failed to save — they would read the `GET` back, see no category, and try
/// again.
#[tokio::test]
async fn the_tax_markers_are_refused_on_a_non_region_taxonomy() {
    let harness = Harness::new().await;
    let (_, tag) = read(&harness, "brand").await;

    let response = put(
        &harness,
        "brand",
        &tag,
        json!([{ "value": "acme", "display_name": "A", "tax_category": "standard" }]),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // Which fault, not merely a 400: this route has at least five independent
    // `invalid_argument` producers and none of them carries a wire code, so the
    // status is satisfied by any of them. `an_unaddressable_class_is_refused_naming_the_four`
    // is the shape -- read the document and require it to name the fault.
    let problem = body_json(response).await.to_string();
    assert!(
        problem.contains("taxCategory") && problem.contains("region taxonomy alone"),
        "the refusal must name the marker and the class that carries it: {problem}"
    );
}

/// They round-trip on the region taxonomy, which is the one that has the columns.
#[tokio::test]
async fn the_tax_markers_round_trip_on_the_region_taxonomy() {
    let harness = Harness::new().await;
    let (_, tag) = read(&harness, "region").await;

    let response = put(
        &harness,
        "region",
        &tag,
        json!([{
            "value": "eu",
            "display_name": "Europe",
            "tax_category": "standard",
            "tax_rate_present": true
        }]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (body, _) = read(&harness, "region").await;
    let eu = entry_for(&body, "eu");
    assert_eq!(eu["tax_category"], "standard");
    assert_eq!(eu["tax_rate_present"], true);
}

/// A blank value is refused at the edge, not by the store.
///
/// The `CHECK` is the backstop against a writer that is not this crate; reaching
/// it from a request would make a one-field caller mistake an internal fault —
/// `overlay_rules::check_authored_shape`'s argument, and the reason that entry
/// point exists.
#[tokio::test]
async fn a_blank_value_is_refused_at_the_edge() {
    let harness = Harness::new().await;
    // A class starts empty, so a readback against the default compares nothing
    // with nothing; the control is seeded so the refusal has something to lose.
    let (_, tag) = read(&harness, "brand").await;
    put(
        &harness,
        "brand",
        &tag,
        json!([{ "value": "acme", "display_name": "A" }]),
    )
    .await;
    let (before, tag) = read(&harness, "brand").await;
    assert_eq!(
        before["values"].as_array().map(Vec::len),
        Some(1),
        "the control this case is about losing: {before}"
    );

    let response = put(
        &harness,
        "brand",
        &tag,
        json!([{ "value": "   ", "display_name": "A" }]),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a whitespace value must not reach the store - the length CHECK would admit it"
    );
    let problem = body_json(response).await.to_string();
    assert!(
        problem.contains("must not be blank or whitespace"),
        "the refusal must name the blank value, not another of this route's 400s: {problem}"
    );
    // The `PUT` replaces the set wholesale, so a refusal that had already written
    // is the whole taxonomy gone -- the readback `a_put_without_if_match_is_refused`
    // carries, for the same reason.
    let (after, _) = read(&harness, "brand").await;
    assert_eq!(
        after["values"], before["values"],
        "a refused write leaves the taxonomy exactly where it was"
    );
}

/// One value twice in one body is refused rather than de-duplicated.
#[tokio::test]
async fn a_repeated_value_is_refused() {
    let harness = Harness::new().await;
    // The control, for `a_blank_value_is_refused_at_the_edge`'s reason.
    let (_, tag) = read(&harness, "brand").await;
    put(
        &harness,
        "brand",
        &tag,
        json!([{ "value": "widgets", "display_name": "W" }]),
    )
    .await;
    let (before, tag) = read(&harness, "brand").await;
    assert_eq!(
        before["values"].as_array().map(Vec::len),
        Some(1),
        "the control this case is about losing: {before}"
    );

    let response = put(
        &harness,
        "brand",
        &tag,
        json!([
            { "value": "acme", "display_name": "A", "state": "active" },
            { "value": "acme", "display_name": "A", "state": "retired" }
        ]),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // The sharpest of the three: this body is the only one in the file that sends
    // a `state` member, so the handler's state-token arm answers the same 400 and
    // the case stayed green with de-duplication removed entirely.
    let problem = body_json(response).await.to_string();
    assert!(
        problem.contains("appears twice in this body"),
        "the refusal must name the repetition, not the state token beside it: {problem}"
    );
    let (after, _) = read(&harness, "brand").await;
    assert_eq!(
        after["values"], before["values"],
        "a refused write leaves the taxonomy exactly where it was"
    );
}

/// The `409` this surface's whole guard exists to produce, driven through HTTP.
///
/// Added after review: the handler's violation arm and its error mapping had **no**
/// REST coverage — deleting the arm would have answered `200` with the unchanged
/// taxonomy and every case in this file would still have passed.
#[tokio::test]
async fn retiring_a_referenced_value_answers_409_with_the_declared_code() {
    let harness = Harness::new().await;
    let (_, tag) = read(&harness, "brand").await;
    put(
        &harness,
        "brand",
        &tag,
        json!([{ "value": "acme", "display_name": "Acme" }]),
    )
    .await;

    // Seeded published rather than authored through the route: `POST
    // /price-overlays` creates a **draft**, and a draft is deliberately not a
    // reference (`taxonomy_repo`'s module doc). Only a published overlay blocks a
    // retirement, so authoring one here would exercise the opposite case.
    seed_published_overlay(&harness, "brand", "acme").await;

    let (_, tag) = read(&harness, "brand").await;
    let refused = put(&harness, "brand", &tag, json!([])).await;

    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(refused).await, "TAXONOMY_VALUE_IN_USE");

    let (body, _) = read(&harness, "brand").await;
    assert_eq!(
        codes(&body),
        ["acme:active"],
        "a refused PUT writes nothing at all"
    );
}

// ---------------------------------------------------------------------------
// The audit half of `inst-tx-mutation`.
// ---------------------------------------------------------------------------

/// *"Taxonomy mutation is tenant-admin config, audited"* — one record per `PUT`.
#[tokio::test]
async fn a_put_is_audited_once_naming_the_taxonomy() {
    let harness = Harness::new().await;
    let before = audit_rows(&harness).await.len();
    let (_, tag) = read(&harness, "brand").await;

    put(
        &harness,
        "brand",
        &tag,
        json!([
            { "value": "acme", "display_name": "A" },
            { "value": "zenith", "display_name": "Z" }
        ]),
    )
    .await;

    let rows = audit_rows(&harness).await;
    assert_eq!(
        rows.len(),
        before + 1,
        "one PUT is one audited act, however many values it moved"
    );
    assert_eq!(rows.last().expect("a record").subject_ref, "taxonomy/brand");
}
