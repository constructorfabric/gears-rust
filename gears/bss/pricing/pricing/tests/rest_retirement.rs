//! `POST /bss-pricing/v1/plans/{planId}/retire` over the wire
//! (`inst-rt-api`, `inst-rt-return`, `inst-re-warn`, D-109, D-128, D-182).
//!
//! `rest_cutovers`' shape. What this file adds that no other route test can is
//! the **default**: this is the one surface in the gear where omitting a body
//! field decides between reading and acting, and the two failure modes are not
//! symmetric — a caller who meant to confirm and got a preview reads it and calls
//! again, while a caller who meant to preview and got a confirm has opened an
//! approval unit over an irreversible act.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
mod rest_support;

use axum::http::StatusCode;
use bss_pricing::api::rest::retirement::PLAN_RETIRE;
use rest_support::{
    Harness, approval_rows, body_json, plan_state, problem_code, request, seed_publishable_plan,
    with_headers,
};
use uuid::Uuid;

const SUBMITTER: Uuid = Uuid::from_u128(0x_5e_11);
const APPROVER: Uuid = Uuid::from_u128(0x_5e_12);

fn path(plan_id: Uuid) -> String {
    PLAN_RETIRE.replace("{planId}", &plan_id.to_string())
}

fn confirm_body() -> serde_json::Value {
    serde_json::json!({ "dry_run": false })
}

fn preview_body() -> serde_json::Value {
    serde_json::json!({ "dry_run": true })
}

/// A published plan with one published price row and its coverage window.
async fn published(h: &Harness) -> Uuid {
    let plan_id = Uuid::now_v7();
    let seeded = seed_publishable_plan(h, plan_id).await;
    h.publish(plan_id, seeded.revision).await;
    h.publish_price(plan_id, seeded.price_id).await;
    plan_id
}

#[tokio::test]
async fn the_dry_run_answers_200_with_the_windows_labelled() {
    let h = Harness::new().await;
    let plan_id = published(&h).await;

    let response = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(preview_body())))
        .await;

    // **200, not 202.** The dry-run is a read: it opens no unit, requests no
    // version and writes nothing, which is what makes it legible to the approver
    // before the decision (D-61).
    assert_eq!(response.status(), StatusCode::OK);
    let view = body_json(response).await;
    assert_eq!(view["plan_id"], plan_id.to_string());
    assert_eq!(view["revision"], 0);

    // `inst-re-warn` + `inst-re-cancelflow`: every window carries what would
    // happen to it **and why**, not a bare count.
    let windows = view["windows"].as_array().expect("windows array");
    assert!(!windows.is_empty(), "the seed must carry a window: {view}");
    for window in windows {
        assert_eq!(window["disposition"], "kept");
        assert_eq!(window["kept_reason"], "presence_unresolved");
    }
    // D-182: the lane has no client, so the preview says the keeping is for want
    // of an answer rather than because a subscriber was found.
    assert_eq!(view["presence_unresolved"], true);
    assert!(
        view["blocking_references"]
            .as_array()
            .expect("array")
            .is_empty()
    );
}

#[tokio::test]
async fn a_body_that_omits_the_flag_is_read_as_a_dry_run() {
    // The safe default, and the asymmetry above is the argument. A regression
    // here does not fail loudly — it silently turns a reader into an actor.
    let h = Harness::new().await;
    let plan_id = published(&h).await;

    let response = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(serde_json::json!({}))))
        .await;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an omitted dry_run must not act"
    );
    let view = body_json(response).await;
    assert!(view.get("outcome").is_none(), "a preview has no outcome");
}

#[tokio::test]
async fn the_confirm_answers_202_and_opens_a_unit_rather_than_committing() {
    // D-109: retirement is a registered always-material trigger, so this arm can
    // only ever be `submitted_for_approval` however the tenant's thresholds are
    // configured.
    let h = Harness::new().await;
    let plan_id = published(&h).await;

    let handles_before = h.registry.requested().len();
    let response = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(confirm_body())))
        .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let view = body_json(response).await;
    assert_eq!(view["outcome"], "submitted_for_approval");
    assert!(view["approval"].is_object(), "{view}");
    // No version is requested for an act that did not commit (D-156) — **read off
    // the registry, not off the response**. The body-level check below was the
    // whole of this claim until 2026-08-20, and it cannot fail against the defect
    // D-156 names: a handler that took a handle *before* opening the unit strands
    // it pending forever and trips `pricing.catalogversion.commit_overdue` for a
    // publish that can never happen, while still rendering
    // `pending_version_ref: None` on this arm, because the view is built from the
    // `SubmittedForApproval` variant and that variant carries no ref to render.
    // The commit arm below is this delta's positive control: it moves.
    assert_eq!(
        h.registry.requested().len(),
        handles_before,
        "an act that did not commit strands no handle: {:?}",
        h.registry.requested()
    );
    assert!(view["pending_version_ref"].is_null(), "{view}");
    // The approver reads the same preview the submitter saw — on this arm it is
    // the response body rather than a second call.
    assert_eq!(view["preview"]["presence_unresolved"], true);
    assert_eq!(
        view["cancelled_window_ids"]
            .as_array()
            .expect("array")
            .len(),
        0
    );

    // **Both halves of this test's name, read off the store rather than off the
    // response.** Every assertion above reads the document the handler just wrote,
    // and until 2026-08-11 they were the whole test — so a handler that rendered an
    // `approval` object and opened nothing passed "opens a unit", and a handler that
    // retired the plan *and* rendered the same body passed "rather than committing".
    //
    // Retirement is an irreversible act over a published plan (D-128), which makes
    // this the highest-stakes place for that pattern. The tools were in the file's
    // reach the whole time: `rest_support/mod.rs:490` documents `read_approval` as
    // existing for exactly this — "the response can be made to say anything, and a
    // test that only asserted the response would pass against a handler that minted
    // an id and opened nothing. That is the exact defect D-225 records for the
    // overlay submit's 202."
    let approval_id = view["approval"]["approval_id"]
        .as_str()
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .unwrap_or_else(|| panic!("the 202 names the unit it opened: {view}"));
    let unit = h
        .read_approval(approval_id)
        .await
        .unwrap_or_else(|| panic!("the id the body names must be a stored unit: {view}"));
    assert_eq!(
        unit.state,
        bss_pricing::domain::approval::ApprovalState::Submitted,
        "the unit is open for a second principal to decide"
    );

    assert_eq!(
        plan_state(&h, plan_id, 0).await.as_deref(),
        Some("published"),
        "the plan is still published: a single principal's confirm stages the \
         retirement, it does not perform it"
    );
}

#[tokio::test]
async fn a_plan_that_is_not_published_is_refused() {
    let h = Harness::new().await;
    let plan_id = published(&h).await;
    h.retire(plan_id, 0).await;

    let response = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(preview_body())))
        .await;

    // The lifecycle refusal is architecturally a 422 and reaches the wire as a
    // 400 carrying its code (Foundation §3.3) — the code is the discriminator,
    // not the status. This case stated exactly that and then tested the half the
    // sentence says is not the discriminator; several distinct refusals share this
    // status, so the status alone passes with the wrong one.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(problem_code(response).await, "LIFECYCLE_FORBIDDEN");
}

#[tokio::test]
async fn an_unknown_plan_is_a_404_rather_than_an_empty_preview() {
    let h = Harness::new().await;

    let response = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(Uuid::now_v7()), Some(preview_body())))
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // The generic `NotFound` mints **no** wire code — `error_mapping.rs` puts the
    // id in `context.resource_name` and the subject in the detail — so the subject
    // is what discriminates this 404 from the four other things on this route that
    // can be absent. Asserted rather than the status alone, which every one of them
    // shares.
    let problem = body_json(response).await;
    assert!(
        problem["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("plan") && detail.contains("not found")),
        "the refusal names what was absent: {problem}"
    );
}

#[tokio::test]
async fn the_approved_retirement_commits_and_takes_exactly_one_version_handle() {
    // **The positive control for the handle delta above**, and the only tier that
    // drives retirement's commit arm end to end: `sqlite_retirement_unit`'s
    // `pending` helper panics on `RetirementOutcome::Retired`, so a delta asserted
    // against a registry nothing in the suite ever moves would pass just as well
    // against a `requested()` that was wired to nothing at all.
    //
    // Three calls, because that is what an operator makes: confirm (opens the
    // unit), a second principal approves it, confirm again (`authorizing_unit`
    // now answers, `retire_in`'s step 5 takes the handle, the flip runs).
    let h = Harness::new().await;
    let plan_id = published(&h).await;

    let opened = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(confirm_body())))
        .await;
    assert_eq!(opened.status(), StatusCode::ACCEPTED);
    let approval_id = approval_rows(&h)
        .await
        .first()
        .map(|row| row.approval_id)
        .expect("the confirm opened a unit");

    let decided = h
        .allowed_as(APPROVER)
        .send(with_headers(
            "POST",
            &format!("/bss-pricing/v1/approvals/{approval_id}/approve"),
            None,
            &[],
        ))
        .await;
    assert_eq!(
        decided.status(),
        StatusCode::OK,
        "the independent decision must land for the re-issue below to commit"
    );

    let handles_before = h.registry.requested().len();
    let committed = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(confirm_body())))
        .await;
    assert_eq!(committed.status(), StatusCode::ACCEPTED);
    let view = body_json(committed).await;
    assert_eq!(view["outcome"], "retired", "{view}");
    assert!(
        view["pending_version_ref"].is_string(),
        "the committing act names the handle it took: {view}"
    );
    assert_eq!(
        h.registry.requested().len(),
        handles_before + 1,
        "exactly one handle, and the delta above is therefore measuring something: {:?}",
        h.registry.requested()
    );
    assert_eq!(
        plan_state(&h, plan_id, 0).await.as_deref(),
        Some("retired"),
        "and the flip ran"
    );
}

/// **The committed arm's two presence members, pinned as a pair** (review
/// 2026-08-19, RUST-DATA-001).
///
/// `presence_unresolved` on that arm is a **hardcoded `true`**: `RetirementReceipt`
/// carries no presence flag, so the handler has no fact to render and states one.
/// It happens to be the correct one today, and the reason is the whole content of
/// this case — `RetirementService::compose_preview` can hand the composer only
/// `PresenceMap::fail_closed()` (D-131's clause, D-182 making it this system's only
/// case), so every key reads occupied, every window is kept for want of an answer,
/// and the condemned set is empty.
///
/// So the pair is what is asserted, not the flag: `presenceUnresolved == true`
/// **and** `cancelledWindowIds` empty is the only combination fail-closed can
/// produce, and the DTO's own doc rules out any other. The seed carries a window —
/// `the_dry_run_answers_200_with_the_windows_labelled` reads it back as
/// `kept`/`presence_unresolved` — so this is a real subject and not a vacuous
/// emptiness.
///
/// **This is the trap, and it is why the case exists.** A red probe for the defect
/// cannot be written through the route: it needs a resolved `PresenceMap`, and no
/// route-reachable path can supply one. The day the D-79 lane wires one, this plan's
/// unoccupied window is condemned, `cancelledWindowIds` stops being empty and this
/// case reddens with `presenceUnresolved` still claiming nobody could be asked. The
/// fix then is to carry the flag on the receipt and render it — **never** to relax
/// the assertion below.
#[tokio::test]
async fn the_committed_arm_reports_no_presence_answer_and_cancels_nothing() {
    let h = Harness::new().await;
    let plan_id = published(&h).await;

    let opened = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(confirm_body())))
        .await;
    assert_eq!(opened.status(), StatusCode::ACCEPTED);
    let approval_id = approval_rows(&h)
        .await
        .first()
        .map(|row| row.approval_id)
        .expect("the confirm opened a unit");

    let decided = h
        .allowed_as(APPROVER)
        .send(with_headers(
            "POST",
            &format!("/bss-pricing/v1/approvals/{approval_id}/approve"),
            None,
            &[],
        ))
        .await;
    assert_eq!(decided.status(), StatusCode::OK);

    let committed = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(confirm_body())))
        .await;
    assert_eq!(committed.status(), StatusCode::ACCEPTED);
    let view = body_json(committed).await;
    assert_eq!(view["outcome"], "retired", "{view}");
    assert_eq!(
        view["preview"]["presence_unresolved"], true,
        "the committed arm claims the presence lane could not answer: {view}"
    );
    assert!(
        view["cancelled_window_ids"]
            .as_array()
            .expect("cancelled_window_ids")
            .is_empty(),
        "and under fail-closed nothing may be condemned, which is what makes that claim \
         true rather than merely stated: {view}"
    );
}

/// **The SQL tenant predicate on this door.**
///
/// `rest_authz.rs`'s census cannot reach it: that seed leaves the plan a draft and
/// this route answers its **owner** a `404 current plan revision … not found`
/// there, so the row is listed in `BY_ID_WRITES_THIS_FIXTURE_CANNOT_STAGE`. Here
/// [`published`] commits the revision, so the owner's identical call is accepted
/// and a refusal of the foreign caller means tenancy rather than a missing
/// subject.
///
/// The **confirm** body and not the preview's: the dry run writes nothing, so a
/// refusal of it says nothing about a write path. `Harness::denied` drives a PDP
/// that refuses everything and `Harness::scope_mismatch` exercises `access_scope`'s
/// write-target membership assertion; neither hands a caller-supplied id of another
/// tenant's row to a repository, which is the only way the predicate is exercised
/// at all. A handler that resolved `{planId}` before narrowing would satisfy both
/// of those and put another tenant's plan in front of a reviewer for an
/// irreversible act.
#[tokio::test]
async fn a_foreign_tenant_cannot_retire_this_tenants_plan() {
    let h = Harness::new().await;
    let plan_id = published(&h).await;

    rest_support::foreign_is_indistinguishable(
        &h,
        request("POST", &path(plan_id), Some(confirm_body())),
        request("POST", &path(Uuid::now_v7()), Some(confirm_body())),
    )
    .await;

    // The control, and it is what makes the two refusals mean anything. Last,
    // because it opens the unit.
    let owner = h
        .allowed_as(SUBMITTER)
        .send(request("POST", &path(plan_id), Some(confirm_body())))
        .await;
    assert_eq!(
        owner.status(),
        StatusCode::ACCEPTED,
        "the owner's identical confirm must be accepted, or the refusals above are about the \
         request rather than about the tenant"
    );
}
