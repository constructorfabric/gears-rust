// Created: 2026-07-27 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! Contract-drift tests for the file-storage DB-behavior audit (Step 4 of
//! the DB-behavior audit program -- see
//! `docs/toolkit_unified_system/14_db_behavior_testing.md`).
//!
//! `contract-drift` here means: a documented promise (in `gears/file-storage/
//! docs/concurrency-and-failure-model.md`, an ADR, or a code comment
//! describing intended behavior) does not match what the code actually
//! does. Two shapes are covered:
//!
//! - **FS-06 / F4**: a *behavioral* drift -- the documented "every retry is
//!   safe" property has a real gap, reproduced directly against the domain
//!   layer.
//! - **FS-12** (new finding, not in `tmp-review0.md`): a *documentation*
//!   drift -- `concurrency-and-failure-model.md`'s own Race Catalog item 2
//!   states an unqualified universal claim that this audit's PostgreSQL
//!   suite (`tests/pg_concurrency_test.rs::
//!   f2_stale_completer_strands_session_after_owner_unfenced_release`)
//!   mechanically falsifies for one specific (and reachable) interleaving.
//!   Pinned here via `include_str!` so the doc and the falsifying test stay
//!   linked -- if the doc is ever corrected (or the code fixed so the claim
//!   becomes true), this test's own assertion needs revisiting too.
//!
//! F5 (`POST /files` rejects `multipart` + `idempotency_key` before any DB
//! call -- `handlers.rs:178-188`) and F6 (single-part `bind:"manual"` emits
//! no `X-FS-Bound` response header -- `handlers.rs:895-922`) are verified
//! contract corrections but are deliberately **not** given tests here: both
//! are request-validation / HTTP-response-shape concerns with no DB
//! transaction, statement-count, or concurrency dimension at all -- squarely
//! the layer `14_db_behavior_testing.md`'s own "What Does NOT Belong Here"
//! section excludes ("JSON wire format, HTTP status codes" -- E2E/unit
//! tests' job, not this layer's). F7 (the multipart gate is the
//! `multipart_native` *capability*, not backend identity -- doc's "S3
//! required" is too strong) is already fully corroborated by this audit's
//! existing `db_behavior_audit_test.rs` pair
//! (`multipart_initiate_capability_reject_leaves_orphan_bare_file` +
//! `negative_control_multipart_native_backend_initiate_succeeds`, which
//! differ *only* in backend topology) -- no new test needed. F8 (no
//! session-list/discovery route) is a documented absence, not a DB-behavior
//! defect -- verified by direct code reading (`routes.rs`), not given a
//! test.

mod common;

use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::BindState;
use uuid::Uuid;

// =========================================================================
// FS-06 / F4: a completed multipart upload's exact retry is not always
// replayed -- a stale If-Match (valid at request time, no longer valid
// after the completion's own auto-bind moved the pointer) is rejected with
// PreconditionFailed *before* the session-state check that would otherwise
// recognize "this exact session is already Completed -- replay it" ever
// runs (`multipart_service.rs:796-806` runs before `:809-833`).
// =========================================================================

#[tokio::test]
async fn fs06_f4_completed_retry_with_stale_if_match_precondition_fails_instead_of_replaying() {
    let (db, _rec) = common::test_db_with_recorder().await;
    let s = common::make_services_full(&db);
    let dp = file_storage::domain::data_plane::DataPlaneService::new(std::sync::Arc::clone(&s.svc)
        as std::sync::Arc<dyn file_storage::domain::ports::DataPlanePort>);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // Bind an initial version first, so the file has a real (non-NULL)
    // content pointer with a real ETag before the multipart session below
    // ever starts -- this is the pointer the client legitimately observes
    // and can supply as If-Match on its complete call.
    let ticket = s
        .svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        bytes::Bytes::from_static(b"initial content"),
    )
    .await
    .expect("put_content");
    let bound = s
        .svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind initial content");
    let etag_of_initial_content =
        file_storage::domain::etag::etag_for(&bound).expect("bound file must have an etag");

    let plan = s
        .msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            10,
            None,
            None,
            true, // auto_bind
        )
        .await
        .expect("initiate_multipart_upload with auto_bind");
    common::simulate_all_parts(&s.multipart_store, &s.backend, &plan, ticket.file_id).await;

    // The client supplies the ETag of the content it knows about (the
    // initial version) as If-Match -- a defensible, correctly-behaving
    // precondition: "only complete if nothing else has changed the pointer
    // since I last looked." This matches on the first call (nothing else
    // has touched content_id yet), so complete succeeds and its own
    // embedded auto-bind CAS moves the pointer to the new version.
    let completed_first = s
        .msvc
        .complete_multipart_upload(
            &ctx,
            ticket.file_id,
            plan.upload_id,
            Some(&etag_of_initial_content),
        )
        .await
        .expect("first complete: If-Match matches the initial content pointer")
        .unwrap_completed();
    assert_eq!(completed_first.bind_state, BindState::Bound);

    // Simulate the realistic retry trigger: the request above succeeded on
    // the server, but its response never reached the client (timeout,
    // connection drop) -- the client, having never seen the new ETag the
    // successful response carried, retries the IDENTICAL request: same
    // upload_id, same (now-stale) If-Match value. Per concurrency-and-
    // failure-model.md's Ground Rule 2 ("Every retry is safe by
    // construction... `complete` replays its persisted result") and
    // Invariants ("Every retry is safe: ... `complete` (persisted-result
    // replay)"), this should replay the stored 200. It does not:
    let retry = s
        .msvc
        .complete_multipart_upload(
            &ctx,
            ticket.file_id,
            plan.upload_id,
            Some(&etag_of_initial_content),
        )
        .await;
    assert!(
        matches!(retry, Err(DomainError::PreconditionFailed { .. })),
        "known defect FS-06/F4: expected the documented-as-safe retry to be rejected with a \
         stale PreconditionFailed instead of replaying the persisted Completed result (the \
         If-Match check at the top of complete_multipart_upload runs against the file's \
         CURRENT etag, before the session-state replay branch ever gets a chance to recognize \
         this exact upload_id is already Completed) -- got: {retry:?}"
    );
}

/// Negative control: the SAME scenario, but the retry supplies no If-Match
/// at all (the common case -- most clients don't cache an etag from before
/// their own upload existed) -- correctly replays the persisted result,
/// proving the drift is specifically about a *stale, non-wildcard*
/// `If-Match`, not about retries in general.
#[tokio::test]
async fn negative_control_fs06_completed_retry_without_if_match_replays_correctly() {
    let (db, _rec) = common::test_db_with_recorder().await;
    let s = common::make_services_full(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let file_id = s
        .svc
        .create_file_bare(&ctx, common::new_file())
        .await
        .expect("create_file_bare");
    let plan = s
        .msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            10,
            None,
            None,
            true,
        )
        .await
        .expect("initiate_multipart_upload with auto_bind");
    common::simulate_all_parts(&s.multipart_store, &s.backend, &plan, file_id).await;

    let completed_first = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("first complete")
        .unwrap_completed();
    assert_eq!(completed_first.bind_state, BindState::Bound);

    let retry = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("retry with no If-Match must replay, not error")
        .unwrap_completed();
    assert_eq!(
        retry.version_id, completed_first.version_id,
        "the replayed result must match the original completion"
    );
}

// =========================================================================
// FS-12 (new finding): concurrency-and-failure-model.md's Race Catalog
// item 2 states an unqualified claim this audit's PostgreSQL suite
// falsifies for one specific, reachable interleaving.
// =========================================================================

#[test]
fn fs12_concurrency_doc_race_catalog_item_2_claim_is_falsified_by_pg_suite() {
    // This is a documentation-comparison pin, not a DB-behavior trace: it
    // asserts the CONTESTED sentence is still present verbatim in the doc
    // (so this test fails loudly, not silently, if the doc text drifts out
    // from under it), and exists specifically to point at the mechanical
    // falsification living in tests/pg_concurrency_test.rs.
    let doc = include_str!("../../docs/concurrency-and-failure-model.md");
    let contested_claim = "finish converges via `replay_completed`";
    assert!(
        doc.contains(contested_claim),
        "FS-12: expected concurrency-and-failure-model.md's Race Catalog item 2 to still \
         contain the contested claim ({contested_claim:?}) -- if this doc text changed, re-check \
         whether FS-12 (and this pin) needs updating instead of just fixing this assertion"
    );

    // The claim, in full (Race Catalog item 2): "A slow-but-alive original
    // owner that finishes assembly after losing its lease cannot corrupt
    // anything: its finish_complete CAS (WHERE state='completing') still
    // succeeds only if no one else finished first, and VersionRepo::
    // finalize's own status='pending' CAS makes the version flip
    // once-only; a lost finish converges via replay_completed
    // (finish_session's not-finished branch)."
    //
    // This is true for the TWO interleavings the doc's own narrative
    // considers: (a) the original owner's finish CAS wins outright (no one
    // else finished), or (b) someone else already reached `completed` (the
    // not-finished branch's `fresh.state == Completed` check replays it).
    // It is FALSE for a third, reachable interleaving this audit found by
    // building the general no-tx-write/CAS-inspection detector and reading
    // the takeover/release code paths together: a taken-over completer B
    // can lose its OWN (redundant) finalize attempt to the original owner
    // A, and B's resulting `release_multipart_complete_lease` (owner-scoped
    // to B, but the session is still `completing` at that exact moment --
    // A hasn't reached `finish_session` yet) succeeds, flipping the session
    // back to `in_progress` *before* A's own finish CAS runs. A's finish CAS
    // then fails (state is no longer `completing`), and the not-finished
    // branch's `fresh.state == Completed` check is false too (it's
    // `in_progress`) -- so A gets a hard error, not a converged replay,
    // despite having correctly finalized and bound the content. See
    // `tests/pg_concurrency_test.rs::
    // f2_stale_completer_strands_session_after_owner_unfenced_release` for
    // the live, real-PostgreSQL reproduction (asserts both callers observe
    // an error, and the session is left stranded at `in_progress` with the
    // version genuinely `available`/bound underneath it) and
    // `docs/analysis/DB_BEHAVIOR_AUDIT.md` §FS-02/FS-12 for the full
    // writeup. This is mechanical evidence that Race Catalog item 2's
    // "cannot corrupt anything" / "a lost finish converges" claim needs a
    // third bullet, not a rewrite -- the "cannot corrupt anything" half
    // (the DB stays consistent) still holds; the "always converges" half
    // does not.
}
