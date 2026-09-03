// Created: 2026-07-27 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! Contract-drift tests for the file-storage DB-behavior audit (see
//! `docs/toolkit_unified_system/14_db_behavior_testing.md`).
//!
//! `contract-drift` here means: a documented promise (in `gears/file-storage/
//! docs/concurrency-and-failure-model.md`, an ADR, or a code comment
//! describing intended behavior) does not match what the code actually
//! does. Two shapes are covered:
//!
//! - A *behavioral* drift: the documented "every retry is safe" property
//!   had a real gap (a stale If-Match could fail-precondition an otherwise-
//!   honest retry of an already-Completed session instead of replaying it),
//!   reproduced directly against the domain layer, then fixed by reordering
//!   the session-state replay check ahead of the If-Match precondition
//!   check.
//! - A *doc-claim* drift: `concurrency-and-failure-model.md`'s own Race
//!   Catalog item 2 stated a claim that this audit found reachably false
//!   for one specific interleaving, before the owner-fencing fix covered in
//!   `pg_concurrency_test.rs`'s `f2_*` scenarios. That fix makes the same
//!   interleaving converge correctly, so the claim is true again -- pinned
//!   here (via `include_str!`, cross-referenced to
//!   `tests/pg_concurrency_test.rs::
//!   f2_stale_completer_converges_instead_of_stranding_after_owner_fencing_fix`)
//!   so a future regression of that fix would also show up as a doc-vs-code
//!   drift here, not just a silent behavior change.
//!
//! A few other contract corrections from the same review are deliberately
//! **not** given tests here: request-validation / HTTP-response-shape
//! concerns (`POST /files`'s multipart+idempotency_key rejection in
//! `handlers.rs:178-188`; the missing `X-FS-Bound` header on a manual
//! single-part bind in `handlers.rs:895-922`) have no DB transaction,
//! statement-count, or concurrency dimension at all -- squarely the layer
//! `14_db_behavior_testing.md`'s own "What Does NOT Belong Here" section
//! excludes ("JSON wire format, HTTP status codes" -- E2E/unit tests' job,
//! not this layer's). The multipart gate being the `multipart_native`
//! *capability*, not backend identity (the doc's "S3 required" is too
//! strong), is already fully corroborated by this audit's existing
//! `db_behavior_audit_test.rs` pair
//! (`multipart_initiate_capability_reject_leaves_orphan_bare_file` +
//! `negative_control_multipart_native_backend_initiate_succeeds`, which
//! differ *only* in backend topology) -- no new test needed. The absence of
//! a session-list/discovery route is a documented absence, not a
//! DB-behavior defect -- verified by direct code reading (`routes.rs`), not
//! given a test.

mod common;

use file_storage::domain::multipart::BindState;
use uuid::Uuid;

// =========================================================================
// A completed multipart upload's exact retry used to not always be
// replayed -- a stale If-Match (valid at request time, no longer valid
// after the completion's own auto-bind moved the pointer) was rejected
// with PreconditionFailed *before* the session-state check that would
// otherwise recognize "this exact session is already Completed -- replay
// it" ever ran. Fixed by moving the session-state replay check before the
// If-Match precondition check.
// =========================================================================

#[tokio::test]
async fn fs06_f4_completed_retry_with_stale_if_match_now_replays_instead_of_failing_precondition() {
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
    // construction... `complete` replays its persisted result"), this must
    // replay the stored 200 -- the session-state replay check now runs
    // before the If-Match precondition check, so this no longer fails.
    let retry = s
        .msvc
        .complete_multipart_upload(
            &ctx,
            ticket.file_id,
            plan.upload_id,
            Some(&etag_of_initial_content),
        )
        .await
        .expect(
            "FS-06/F4 fix: a retry against an already-Completed session must replay, not \
                 fail a stale precondition check",
        )
        .unwrap_completed();
    assert_eq!(
        retry.version_id, completed_first.version_id,
        "the replayed result must match the original completion"
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
// concurrency-and-failure-model.md's Race Catalog item 2 states a claim
// that was reachably false before the owner-fencing fix (see
// pg_concurrency_test.rs's f2_* scenarios), and is true again now -- pinned
// here so a future regression of that fix would also be caught as a
// doc-vs-code drift, not just a silent behavior change.
// =========================================================================

#[test]
fn fs12_concurrency_doc_race_catalog_item_2_claim_holds_after_fs02_fix() {
    // This is a documentation-comparison pin, not a DB-behavior trace: it
    // asserts the claim text is still present verbatim in the doc (so this
    // test fails loudly, not silently, if the doc text drifts out from
    // under it), and exists specifically to point at the live,
    // real-PostgreSQL confirmation in tests/pg_concurrency_test.rs.
    let doc = include_str!("../../docs/concurrency-and-failure-model.md");
    let claim = "finish converges via `replay_completed`";
    assert!(
        doc.contains(claim),
        "FS-12: expected concurrency-and-failure-model.md's Race Catalog item 2 to still \
         contain the claim ({claim:?}) -- if this doc text changed, re-check whether this pin \
         needs updating instead of just fixing this assertion"
    );

    // The claim, in full (Race Catalog item 2): "A slow-but-alive original
    // owner that finishes assembly after losing its lease cannot corrupt
    // anything: its finish_complete CAS (WHERE state='completing') still
    // succeeds only if no one else finished first, and VersionRepo::
    // finalize's own status='pending' CAS makes the version flip
    // once-only; a lost finish converges via replay_completed
    // (finish_session's not-finished branch)."
    //
    // This was briefly false for a third interleaving the doc's own
    // two-case narrative didn't consider: a taken-over completer B could
    // lose its own (redundant) finalize attempt to the original,
    // lease-expired owner A, and B's `release_multipart_complete_lease`
    // (owner-scoped to B, but the session was still `completing` at that
    // moment) would flip the session back to `in_progress` before A's own
    // finish CAS ran -- stranding it despite the content being correctly
    // finalized and bound. Fixed in
    // `multipart_service.rs::converge_or_error_after_lost_finalize_cas` (a
    // lost finalize CAS now converges instead of erroring -- the same fix
    // covered in full by `pg_concurrency_test.rs`'s `f2_*` scenarios), so
    // the claim holds again for every reachable interleaving. Confirmed
    // live against real PostgreSQL by
    // `tests/pg_concurrency_test.rs::
    // f2_stale_completer_converges_instead_of_stranding_after_owner_fencing_fix`.
}
