// Created: 2026-07-27 by Constructor Tech
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::too_many_lines
)]
//! DB-behavior audit for file-storage (Step 4 of the DB-behavior audit
//! program -- see `docs/toolkit_unified_system/14_db_behavior_testing.md`,
//! methodology validated against `resource-group`'s Step 1-3 audit).
//!
//! Ground truth for this module comes from an independent review
//! (`tmp-review0.md` at the repo root, not committed -- read once, findings
//! transcribed here as FS-IDs) verified directly against this branch's code
//! during this audit (see `docs/analysis/DB_BEHAVIOR_AUDIT.md` for the full
//! inventory and validation matrix). F1/F2/F3/F4/F9/F10 map to FS-01/02/03/06/
//! 04/05; F5-F8 (contract/doc corrections) map to FS-07..FS-10; FS-11/FS-12
//! are new findings from this audit's own read.
//!
//! Two mechanisms, mirroring resource-group's:
//!
//! 1. **Dynamic trace analysis** (this file's majority): every write
//!    operation runs against a real SQLite connection with
//!    [`common::query_recorder::QueryRecorder`] attached, then:
//!    - `writes_outside_tx()` must be empty -- every INSERT/UPDATE/DELETE
//!      must run inside a `Store`-owned `transaction_ref_mapped` closure
//!      (`no-tx-write` class). Unlike resource-group, file-storage never
//!      uses `TxConfig::serializable()`/`transaction_with_retry` anywhere --
//!      confirmed by `grep -rn "transaction_with_retry\|TxConfig"
//!      gears/file-storage/file-storage/src/` returning zero matches -- so
//!      the `no-retry-serializable` class (as literally defined: opened
//!      directly via `transaction_ref_mapped_with_config` instead of
//!      `transaction_with_retry`) does not apply to this gear at all: there
//!      is no positive example of the retry-aware helper anywhere in this
//!      codebase to compare against. This gear's concurrency-safety strategy
//!      is single-statement CAS `UPDATE ... WHERE` predicates under
//!      whatever the connection's default isolation is, not
//!      SERIALIZABLE+retry -- a different, and not inherently wrong,
//!      architecture. See `DB_BEHAVIOR_AUDIT.md` "Method Boundaries" for the
//!      full reasoning.
//!    - **Scale-invariance**: statement counts for a given (kind, table)
//!      shape must not grow with N (a patch's entry count, a page size) --
//!      growth is the `n-plus-one` class.
//!    - `redundant_reads_after_write()` flags the `redundant-io` class.
//! 2. **Structural + static rules** (no DB, some not even a source-scan):
//!    - `external-call-in-tx`: file-storage's `infra::storage` module tree
//!      (the only place `transaction_ref_mapped` is ever opened) has zero
//!      dependency on `infra::backend` -- confirmed both by import-boundary
//!      grep (below) and by reading every `Store::*` transaction closure.
//!      This makes the RG-09-shaped defect **structurally impossible**, not
//!      merely "not found this time" -- a stronger class of evidence than
//!      RG's own source-text heuristic, since it's a real crate-internal
//!      module boundary the compiler enforces, not a regex over text.
//!    - `check-then-act-without-constraint`: no mechanized rule exists yet
//!      for this class (matching `14_db_behavior_testing.md`'s own stated
//!      boundary) -- FS-02/FS-04/FS-05 are CAS predicates missing a
//!      column, which a regex can't reliably detect without a real SQL
//!      parser + schema cross-reference. Validated instead by inspecting
//!      the recorder's *raw* captured SQL text for the specific CAS
//!      statements (this file, Section 3) and by live PostgreSQL barrier
//!      races with a post-state invariant checker
//!      (`tests/pg_concurrency_test.rs`).
//!
//! Known defects are asserted for real and marked `#[ignore = "known defect
//! FS-XX: ..."]` so the suite stays green while the defect stays
//! executable-and-documented. Operations that are *not* known-defective
//! assert the healthy invariant directly (non-ignored) -- these double as
//! negative controls proving the rules don't fire indiscriminately.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use common::query_recorder::QueryKind;
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::ports::DataPlanePort;
use file_storage::infra::backend::{LocalFsBackend, StorageBackend};
use file_storage_sdk::CustomMetadataPatch;
use uuid::Uuid;

// =========================================================================
// Section 1 -- dynamic trace snapshots + writes-in-tx assertions
// =========================================================================

#[tokio::test]
async fn trace_create_file() {
    let (db, rec) = common::test_db_with_recorder().await;
    let (svc, _msvc) = common::make_services(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    rec.clear();
    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file should succeed");
    assert!(!ticket.upload_url.is_empty());

    assert!(
        rec.writes_outside_tx().is_empty(),
        "create_file must run its writes inside a transaction:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_full_upload_finalize_and_bind() {
    let (db, rec) = common::test_db_with_recorder().await;
    let (svc, _msvc) = common::make_services(&db);
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file");

    rec.clear();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"hello world"),
    )
    .await
    .expect("put_content (upload + finalize)");
    assert!(
        rec.writes_outside_tx().is_empty(),
        "finalize_upload must run its writes inside a transaction:\n{}",
        rec.dump()
    );

    rec.clear();
    let bound = svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind (first bind, no If-Match)");
    assert_eq!(bound.content_id, Some(ticket.version_id));
    assert!(
        rec.writes_outside_tx().is_empty(),
        "bind must run its writes inside a transaction:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_update_metadata() {
    let (db, rec) = common::test_db_with_recorder().await;
    let (svc, _msvc) = common::make_services(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file");

    rec.clear();
    let patch = CustomMetadataPatch {
        entries: vec![("k1".to_owned(), Some("v1".to_owned()))],
    };
    let updated = svc
        .update_metadata(&ctx, ticket.file_id, patch, None)
        .await
        .expect("update_metadata should succeed");
    assert!(updated.meta_version >= 1);

    assert!(
        rec.writes_outside_tx().is_empty(),
        "update_metadata must run its writes inside a transaction:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_delete_file() {
    let (db, rec) = common::test_db_with_recorder().await;
    let (svc, _msvc) = common::make_services(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file");

    rec.clear();
    svc.delete_file(&ctx, ticket.file_id, Some("*"))
        .await
        .expect("delete_file should succeed");

    assert!(
        rec.writes_outside_tx().is_empty(),
        "delete_file must run its writes inside a transaction:\n{}",
        rec.dump()
    );
}

#[tokio::test]
async fn trace_multipart_complete() {
    let (db, rec) = common::test_db_with_recorder().await;
    let s = common::make_services_full(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let file_id = s
        .svc
        .create_file_bare(&ctx, common::new_file())
        .await
        .expect("create_file_bare");
    // 10 MiB declared at a 5 MiB (the minimum allowed) part size -> 2 parts.
    let plan = s
        .msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            10 * 1024 * 1024,
            Some(5 * 1024 * 1024),
            None,
            false,
        )
        .await
        .expect("initiate_multipart_upload (in-memory backend supports multipart_native)");
    assert_eq!(plan.parts.len(), 2, "10 MiB at part_size=5 MiB -> 2 parts");

    common::simulate_all_parts(&s.multipart_store, &s.backend, &plan, file_id).await;

    rec.clear();
    let completed = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("complete_multipart_upload")
        .unwrap_completed();
    assert_eq!(completed.size, 10 * 1024 * 1024);

    // FS-13 (new finding, LOW-MEDIUM -- see docs/analysis/DB_BEHAVIOR_AUDIT.md):
    // exactly one write runs outside a transaction --
    // `acquire_complete_lease`'s own CAS UPDATE (`state`/`lease_until`/
    // `lease_owner`, fresh-acquire-or-takeover). A single UPDATE is atomic
    // regardless of an enclosing transaction, so this specific statement's
    // *own* correctness doesn't depend on being wrapped -- but it does mean
    // the lease is acquired as an independent step from everything that
    // follows (the backend assembly, then the finalize transaction, then
    // `finish_complete`), with no overarching transaction tying the whole
    // workflow together. This is mechanical evidence *for* FS-02 (F2): the
    // gap isn't "no transaction anywhere", it's "several independently-
    // atomic steps with no cross-step ownership fence", exactly what FS-02
    // describes at the domain level. Pinned directly (not `#[ignore]`d,
    // since it's the intentional/expected shape, not a regression target)
    // so a future change that adds a *second* untransacted write here would
    // still be caught.
    let outside = rec.writes_outside_tx();
    assert_eq!(
        outside.len(),
        1,
        "expected exactly one write outside a transaction \
         (acquire_complete_lease's CAS) -- got {}:\n{}",
        outside.len(),
        rec.dump()
    );
    assert_eq!(
        outside[0].table.as_deref(),
        Some("multipart_uploads"),
        "the one untransacted write must be acquire_complete_lease's UPDATE \
         on multipart_uploads, got: {:?}",
        outside[0]
    );
    assert!(
        outside[0].sql.contains("\"lease_until\"")
            && !outside[0].sql.contains("\"complete_result\""),
        "expected acquire_complete_lease's shape (sets lease_until, not \
         complete_result -- that's finish_complete's column), got: {}",
        outside[0].sql
    );
}

// =========================================================================
// Section 2 -- FS-01 (F1): orphan file when multipart initiation fails
// =========================================================================

/// Build a `FileService` + `MultipartService` pair backed by a
/// `LocalFsBackend` (`multipart_native == false`, per `local_fs.rs`'s
/// `BackendCapabilities::default()` -- confirmed by reading the impl: it
/// only ever sets `range_native`/`durable`, never `multipart_native`).
/// Reproduces F1's capability-reject half deterministically, with no test
/// double needed -- just backend topology choice.
async fn make_services_local_fs_only() -> common::Services {
    let db = common::test_db().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new("fs", tmp.keep()));
    common::make_services_with_backends(&db, vec![backend], "fs")
}

#[tokio::test]
async fn multipart_initiate_capability_reject_leaves_orphan_bare_file() {
    // FS-01 / F1 -- FIXED at the orchestration layer (see
    // docs/analysis/DB_BEHAVIOR_AUDIT.md §5): `FileService::
    // compensate_failed_multipart_initiate` now reclaims this orphan, but
    // only the merged `POST /files` handler (`api/rest/handlers.rs::
    // create_file`'s multipart branch) actually calls it -- correctly:
    // compensation must know "this file was JUST created together with
    // this specific initiate attempt", which only that orchestrating
    // caller knows, not either domain service in isolation. This test
    // calls `create_file_bare` + `initiate_multipart_upload` directly, the
    // same bare two-call sequence the handler itself makes *before*
    // reaching its own error-handling branch -- it still (correctly, by
    // design) shows the bare file surviving, because nothing here has
    // invoked the compensation yet. See
    // `multipart_initiate_capability_reject_with_compensation_reclaims_orphan`
    // immediately below for the fixed end-to-end flow.
    let s = make_services_local_fs_only().await;
    let (svc, msvc) = (s.svc, s.msvc);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let file_id = svc
        .create_file_bare(&ctx, common::new_file())
        .await
        .expect("create_file_bare commits the bare file row");

    let err = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            20,
            Some(10),
            None,
            false,
        )
        .await
        .expect_err("local-fs backend does not advertise multipart_native");
    assert!(
        matches!(err, DomainError::MultipartNotSupported { .. }),
        "expected a clean MultipartNotSupported, got: {err}"
    );

    // Without the handler's compensation call, the bare file survives --
    // this is the raw two-call sequence's behavior, unchanged by the FS-01
    // fix (which lives one layer up, in the orchestrating caller).
    let still_there = svc.get_file(&ctx, file_id).await;
    assert!(
        still_there.is_ok(),
        "the orphaned bare file must still exist after JUST the two raw domain calls, with no \
         compensation invoked -- got: {still_there:?}"
    );
}

#[tokio::test]
async fn multipart_initiate_capability_reject_with_compensation_reclaims_orphan() {
    // FS-01 / F1 fix, end-to-end: mirrors exactly what
    // `api/rest/handlers.rs::create_file`'s multipart branch now does on an
    // initiate failure -- call `compensate_failed_multipart_initiate` with
    // the same `file_id`, then confirm the orphan is gone.
    let s = make_services_local_fs_only().await;
    let (svc, msvc) = (s.svc, s.msvc);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let file_id = svc
        .create_file_bare(&ctx, common::new_file())
        .await
        .expect("create_file_bare commits the bare file row");
    msvc.initiate_multipart_upload(
        &ctx,
        file_id,
        "application/octet-stream",
        20,
        Some(10),
        None,
        false,
    )
    .await
    .expect_err("local-fs backend does not advertise multipart_native");

    svc.compensate_failed_multipart_initiate(&ctx, file_id)
        .await;

    let gone = svc.get_file(&ctx, file_id).await;
    assert!(
        matches!(gone, Err(DomainError::FileNotFound { .. })),
        "FS-01/F1 fix: the compensating delete must reclaim the orphan file, got: {gone:?}"
    );
}

// =========================================================================
// Section 3 -- FS-02/FS-04 (F2/F9): CAS predicates inspected via raw SQL
// =========================================================================
//
// `check-then-act-without-constraint` has no general mechanized rule (see
// this file's module doc and `docs/toolkit_unified_system/
// 14_db_behavior_testing.md`'s own stated boundary) -- but the recorder
// already captures the *exact* SQL text of every CAS `UPDATE`, so the
// specific, already-known-from-code-reading gap (a WHERE clause missing a
// column) can be asserted directly against the captured statement text.
// This is not a general rule that would find a *new*, unknown instance of
// the class the way `writes_outside_tx()` would for `no-tx-write` -- it is
// a targeted regression pin for the two gaps this audit's code reading
// already found (FS-02, FS-04), analogous to `redundant_reads_after_write`
// being "general" but the fault-injection table still needing a *specific*
// site to inject into.

#[tokio::test]
async fn multipart_finish_complete_cas_omits_lease_owner() {
    // FS-02 / F2 (known defect, MEDIUM, worst case: session stranded until
    // expiry -- see docs/analysis/DB_BEHAVIOR_AUDIT.md). `finish_complete`'s
    // UPDATE filters on `state = 'completing'` only; unlike
    // `release_complete_lease`/`abort_expired_completing`, it has no
    // `lease_owner` predicate. This pins the *current* (defective) SQL
    // shape directly -- if `lease_owner` is added to the WHERE clause, this
    // test starts failing, signaling FS-02 (this half of it) is fixed.
    let (db, rec) = common::test_db_with_recorder().await;
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
            false,
        )
        .await
        .expect("initiate_multipart_upload");
    common::simulate_all_parts(&s.multipart_store, &s.backend, &plan, file_id).await;

    rec.clear();
    let _ = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("complete_multipart_upload");

    // Values are bound parameters (`?`), never literal text in the captured
    // SQL, so "state = 'completed'" can't be grepped for directly --
    // `complete_result` is set only by `finish_complete`, uniquely
    // distinguishing its UPDATE from `acquire_complete_lease`'s (which also
    // touches `state`/`lease_until`/`lease_owner`, but never `complete_result`).
    let finish_complete_updates: Vec<_> = rec
        .events()
        .into_iter()
        .filter(|e| {
            e.kind == QueryKind::Update
                && e.table.as_deref() == Some("multipart_uploads")
                && e.sql.contains("\"complete_result\"")
        })
        .collect();
    assert_eq!(
        finish_complete_updates.len(),
        1,
        "expected exactly one UPDATE multipart_uploads (finish_complete, \
         identified by its complete_result column) statement:\n{}",
        rec.dump()
    );
    // `lease_owner` legitimately appears in the SET list (finish_complete
    // clears the lease on success) -- the defect is its *absence* from the
    // WHERE predicate specifically, so check only the clause after WHERE.
    let sql = &finish_complete_updates[0].sql;
    let where_clause = sql
        .split_once(" WHERE ")
        .map_or("", |(_, rhs)| rhs)
        .to_ascii_lowercase();
    assert!(
        !where_clause.contains("lease_owner"),
        "known defect FS-02 regression: finish_complete's UPDATE now filters \
         its WHERE clause on lease_owner -- FS-02 may be fixed, update the \
         report. Full SQL: {sql}"
    );
}

#[tokio::test]
async fn multipart_complete_auto_bind_no_if_match_cas_now_requires_content_id_is_null() {
    // FS-04 / F9 fix (see docs/analysis/DB_BEHAVIOR_AUDIT.md §5): the
    // embedded auto-bind CAS inside finalize_version used to bind against
    // `expected_content_id = file.content_id` observed at completion time,
    // unconditionally -- a stale snapshot from the top of
    // `complete_multipart_upload`, not a caller-confirmed precondition.
    // With no `If-Match` supplied, the CAS now targets `content_id IS NULL`
    // instead (mirroring the single-part finalize path's own always-NULL
    // target for a brand-new file), so it correctly LOSES here instead of
    // silently clobbering. This test creates a file whose content is
    // already bound via a first single-part upload+bind, then completes an
    // auto-bind multipart upload for it *without* an If-Match, and confirms
    // both the outcome (`Conflict`, not `Bound`) and the captured
    // `bind_content_cas` SQL shape directly (`IS NULL`, not the observed
    // non-NULL content_id).
    let (db, rec) = common::test_db_with_recorder().await;
    let s = common::make_services_full(&db);
    let (svc, msvc) = (s.svc.clone(), s.msvc.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);

    // First, bind some content via the ordinary single-part path so
    // file.content_id is non-NULL going into the multipart complete below.
    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"first content"),
    )
    .await
    .expect("put_content");
    let bound_first = svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind first content");

    // Now run a multipart upload with auto_bind = true for the SAME file --
    // its complete has no If-Match, so the auto-bind CAS must require
    // content_id IS NULL, which no longer matches (content_id is Some(..)).
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            10,
            None,
            None,
            true,
        )
        .await
        .expect("initiate_multipart_upload with auto_bind");
    common::simulate_all_parts(&s.multipart_store, &s.backend, &plan, ticket.file_id).await;

    rec.clear();
    let completed = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .expect("complete_multipart_upload (no If-Match) must still succeed -- only the bind is conditional")
        .unwrap_completed();
    // FS-04/F9 fix: the auto-bind CAS correctly loses -- the previously
    // bound content survives, with no client-supplied CAS token needed to
    // protect it (the protection is now the CAS's own NULL requirement).
    assert_eq!(
        completed.bind_state,
        file_storage::domain::multipart::BindState::Conflict,
        "FS-04/F9 fix: expected the auto-bind CAS to lose (content_id IS NULL no longer \
         matches) rather than unconditionally clobbering the previously bound content"
    );
    let file_after = svc.get_file(&ctx, ticket.file_id).await.expect("get_file");
    assert_eq!(
        file_after.content_id,
        Some(ticket.version_id),
        "the previously bound content must survive -- the multipart version stays available \
         and manually rebindable, exactly like any other lost bind CAS"
    );
    let _ = bound_first; // (kept for narration; superseded by file_after above)

    let bind_updates: Vec<_> = rec
        .events()
        .into_iter()
        .filter(|e| e.kind == QueryKind::Update && e.table.as_deref() == Some("files"))
        .collect();
    assert_eq!(
        bind_updates.len(),
        1,
        "expected exactly one UPDATE files (bind_content_cas):\n{}",
        rec.dump()
    );
    let sql = &bind_updates[0].sql;
    assert!(
        sql.to_ascii_lowercase().contains("is null"),
        "FS-04/F9 fix regression: multipart complete's auto-bind CAS (no If-Match supplied) \
         must target content_id IS NULL, got: {sql}"
    );
}

/// Negative control for the FS-04/F9 fix: supplying a correct `If-Match`
/// (matching the file's current content) is the caller explicitly
/// confirming the pointer it observed -- the auto-bind CAS then correctly
/// targets that observed (non-NULL) pointer and wins, exactly as before the
/// fix. Same call sequence as
/// `multipart_complete_auto_bind_no_if_match_cas_now_requires_content_id_is_null`,
/// only the `If-Match` argument differs -- proving the fix narrows the CAS
/// target specifically for the no-If-Match case, not for every auto-bind
/// completion.
#[tokio::test]
async fn negative_control_multipart_complete_auto_bind_with_if_match_still_binds() {
    let (db, rec) = common::test_db_with_recorder().await;
    let s = common::make_services_full(&db);
    let (svc, msvc) = (s.svc.clone(), s.msvc.clone());
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);

    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"first content"),
    )
    .await
    .expect("put_content");
    let bound_first = svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind first content");
    let etag_first =
        file_storage::domain::etag::etag_for(&bound_first).expect("bound file must have an etag");

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            10,
            None,
            None,
            true,
        )
        .await
        .expect("initiate_multipart_upload with auto_bind");
    common::simulate_all_parts(&s.multipart_store, &s.backend, &plan, ticket.file_id).await;

    rec.clear();
    let completed = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, Some(&etag_first))
        .await
        .expect("complete_multipart_upload with a correct If-Match")
        .unwrap_completed();
    assert_eq!(
        completed.bind_state,
        file_storage::domain::multipart::BindState::Bound,
        "supplying the correct If-Match must still let the auto-bind win"
    );

    let bind_updates: Vec<_> = rec
        .events()
        .into_iter()
        .filter(|e| e.kind == QueryKind::Update && e.table.as_deref() == Some("files"))
        .collect();
    assert_eq!(bind_updates.len(), 1);
    let sql = &bind_updates[0].sql;
    assert!(
        !sql.to_ascii_lowercase().contains("is null"),
        "with a confirmed If-Match, the CAS must target the observed (non-NULL) content_id, \
         not IS NULL, got: {sql}"
    );
}

// =========================================================================
// Section 4 -- scale-invariance (FS-11, new n-plus-one finding)
// =========================================================================

async fn metadata_upsert_statements_for_patch_size(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let (svc, _msvc) = common::make_services(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file");

    rec.clear();
    let patch = CustomMetadataPatch {
        entries: (0..n)
            .map(|i| (format!("k{i}"), Some(format!("v{i}"))))
            .collect(),
    };
    svc.update_metadata(&ctx, ticket.file_id, patch, None)
        .await
        .expect("update_metadata should succeed");

    rec.stats()
        .into_iter()
        .filter(|((kind, table), _)| {
            *kind == common::query_recorder::QueryKind::Insert && table == "custom_metadata"
        })
        .map(|(_, count)| count)
        .sum()
}

// FS-11 fix: patch_metadata_atomic now batches its DELETE and INSERT into
// one statement each (Store::metadata's patch_metadata_atomic, via the new
// MetadataRepo::delete_keys/insert_many, backed by toolkit-db's
// secure_insert_many -- cherry-picked from fix/rg-db-remediation for this
// fix). No longer `#[ignore]`d: this is now the healthy invariant, asserted
// directly.
#[tokio::test]
async fn scale_metadata_patch_inserts_do_not_grow_with_entry_count() {
    let small = metadata_upsert_statements_for_patch_size(2).await;
    let large = metadata_upsert_statements_for_patch_size(15).await;
    assert_eq!(
        small, large,
        "custom_metadata INSERT count must not scale with patch entry count \
         (small={small} at N=2, large={large} at N=15)"
    );
}

// =========================================================================
// Section 5 -- external-call-in-tx: structural boundary (no static rule
// needed -- a real crate-internal module boundary)
// =========================================================================

#[test]
fn structural_rule_infra_storage_never_imports_infra_backend() {
    // file-storage's RG-09 analog: every transaction_ref_mapped closure is
    // defined inside gears/file-storage/file-storage/src/infra/storage/ --
    // if that whole module tree has zero dependency on infra::backend (the
    // only place a StorageBackend call could come from), no closure defined
    // there can possibly reach a backend call, structurally -- stronger
    // than a source-text scan for a closure *referencing* a client value
    // (RG's version of this rule), since this is a real module boundary the
    // compiler enforces via `use` visibility, not a regex.
    let storage_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/infra/storage");
    let mut offenders = Vec::new();
    visit_rs_files(&storage_dir, &mut |path, src| {
        if src.contains("infra::backend") || src.contains("StorageBackend") {
            offenders.push(path.display().to_string());
        }
    });
    assert!(
        offenders.is_empty(),
        "expected zero infra::backend/StorageBackend references anywhere under \
         src/infra/storage/ (this would mean a DB transaction closure could \
         reach a backend call) -- found in: {offenders:?}"
    );
}

fn visit_rs_files(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_rs_files(&path, f);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(src) = std::fs::read_to_string(&path)
        {
            f(&path, &src);
        }
    }
}

// =========================================================================
// Section 6 -- negative controls
// =========================================================================

#[tokio::test]
async fn negative_control_read_paths_produce_no_write_statements() {
    // Read paths don't produce write-statements (not "don't produce noise" --
    // FS-11 is itself a legitimate n-plus-one finding on a write path driven
    // by a small request-supplied N, and this audit doesn't claim reads are
    // exempt from the scale-invariance rule either, only that the
    // write-oriented no-tx-write/redundant-io rules can't misfire on a
    // read-only call).
    let (db, rec) = common::test_db_with_recorder().await;
    let (svc, msvc) = common::make_services(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file");

    rec.clear();
    svc.get_file(&ctx, ticket.file_id)
        .await
        .expect("get_file should succeed");
    svc.list_versions(&ctx, ticket.file_id, None, 0)
        .await
        .expect("list_versions should succeed");
    let _ = msvc; // silence unused in case future reads move here

    let stats = rec.stats();
    for (kind, _table) in stats.keys() {
        assert!(
            !matches!(
                kind,
                QueryKind::Insert | QueryKind::Update | QueryKind::Delete
            ),
            "read-only calls must not produce write statements:\n{}",
            rec.dump()
        );
    }
}

#[tokio::test]
async fn negative_control_multipart_native_backend_initiate_succeeds() {
    // Same scenario as FS-01's repro, but with a multipart_native backend
    // (InMemoryBackend) instead of LocalFsBackend -- proves the capability
    // gate itself works correctly (rejects only when it should), and gives
    // the FS-01 repro a same-shape control: identical call sequence, only
    // the backend topology differs.
    let (db, _rec) = common::test_db_with_recorder().await;
    let (svc, msvc) = common::make_services(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    let file_id = svc
        .create_file_bare(&ctx, common::new_file())
        .await
        .expect("create_file_bare");
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            10 * 1024 * 1024,
            Some(5 * 1024 * 1024),
            None,
            false,
        )
        .await
        .expect("in-memory backend advertises multipart_native, initiate must succeed");
    assert_eq!(plan.parts.len(), 2);
}

// =========================================================================
// Section 7 -- scale-invariance (FS-14 fix, the same n-plus-one shape as
// FS-11 at create_file's own sibling call site)
// =========================================================================

async fn create_file_metadata_insert_statements_for_entry_count(n: usize) -> usize {
    let (db, rec) = common::test_db_with_recorder().await;
    let (svc, _msvc) = common::make_services(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);
    let mut new = common::new_file();
    new.custom_metadata = (0..n)
        .map(|i| file_storage_sdk::CustomMetadataEntry {
            key: format!("k{i}"),
            value: format!("v{i}"),
        })
        .collect();

    rec.clear();
    svc.create_file(&ctx, new, None, false)
        .await
        .expect("create_file with initial custom_metadata should succeed");

    rec.stats()
        .into_iter()
        .filter(|((kind, table), _)| {
            *kind == common::query_recorder::QueryKind::Insert && table == "custom_metadata"
        })
        .map(|(_, count)| count)
        .sum()
}

/// FS-14 fix: `create_file_with_pending_version{,_with_event,_with_idempotency}`'s
/// own initial-`custom_metadata` loop had the exact same n-plus-one shape as
/// FS-11's `patch_metadata_atomic`, found while fixing that one. Fixed the
/// same way (batched via `MetadataRepo::insert_many`); asserted directly
/// (not `#[ignore]`d) since this is the healthy invariant now.
#[tokio::test]
async fn scale_create_file_metadata_inserts_do_not_grow_with_entry_count() {
    let small = create_file_metadata_insert_statements_for_entry_count(2).await;
    let large = create_file_metadata_insert_statements_for_entry_count(15).await;
    assert_eq!(
        small, large,
        "custom_metadata INSERT count at create_file time must not scale with the initial \
         entry count (small={small} at N=2, large={large} at N=15)"
    );
}
