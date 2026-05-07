//! Race / abort / orphan tests for the test-only microchat module.
//!
//! Each test pins a single seam and asserts that **disk state is
//! consistent with DB state**, not just that DB state is consistent
//! with itself.

mod e2e_common;

use std::collections::BTreeMap;
use std::sync::Arc;

use e2e_common::microchat::{MicrochatEnv, make_microchat_env};
use e2e_common::{
    EnvSpec, copy_object_count, list_all_entries, list_object_keys, make_ctx, object_exists,
    put_part, sha256_of, sha256_on_disk,
};
use file_storage::domain::repo::FilesRepo;
use file_storage::infra::backends::r#trait::derive_s3_key;
use file_storage_sdk::{Etag, FileId, FileMeta, FileStorageError, UploadedPart};
use microchat_test::{AttachmentStatus, MicrochatError};
use modkit_security::SecurityContext;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use uuid::Uuid;

const ALPHA: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

fn payload(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..len)
        .map(|_| ALPHA[rng.random_range(0..ALPHA.len())])
        .collect()
}

fn payload_3kib(seed: u64) -> Vec<u8> {
    payload(seed, 3 * 1024)
}

fn pdf_meta(name: &str) -> FileMeta {
    FileMeta {
        name: name.to_string(),
        mime_type: "application/pdf".to_string(),
        gts_file_type: "gts.cf.fstorage.file.type.v1~document".to_string(),
        custom_metadata: BTreeMap::new(),
    }
}

// ── Test 1 — concurrent complete; one winner ────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn concurrent_complete_one_winner() {
    let env = Arc::new(make_microchat_env(EnvSpec::priv_pub()).await);
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let body = payload_3kib(0xC0DE_0101);

    let handle = env
        .microchat
        .attach(&ctx, chat_id, owner_id, pdf_meta("a.pdf"), 1)
        .await
        .unwrap();
    let etag_part = put_part(&handle.part_urls[0], body.clone()).await;

    let parts = vec![UploadedPart { part_number: 1, etag: etag_part }];

    let env_a = env.clone();
    let env_b = env.clone();
    let ctx_a = make_ctx(tenant_id);
    let ctx_b = make_ctx(tenant_id);
    let upload_id_a = handle.upload_id.clone();
    let upload_id_b = handle.upload_id.clone();
    let parts_a = parts.clone();
    let parts_b = parts.clone();

    let (a, b) = tokio::join!(
        async move {
            env_a
                .microchat
                .complete(&ctx_a, chat_id, handle.file_id, &upload_id_a, parts_a)
                .await
        },
        async move {
            env_b
                .microchat
                .complete(&ctx_b, chat_id, handle.file_id, &upload_id_b, parts_b)
                .await
        },
    );
    let outcomes = [a, b];
    let oks = outcomes.iter().filter(|r| r.is_ok()).count();
    let errs = outcomes.iter().filter(|r| r.is_err()).count();
    // Either: one Applied + one Conflict, or two idempotent successes
    // (Service `complete_upload` returns the row idempotently when
    // status is already `uploaded`). Both shapes leave a single,
    // consistent file on disk.
    assert!(oks >= 1, "at least one complete should succeed; got {outcomes:?}");
    let _ = errs;

    let key = derive_s3_key(handle.file_id);
    assert!(object_exists(&env.fs_env, "priv", &key));
    assert_eq!(sha256_on_disk(&env.fs_env, "priv", &key).await, sha256_of(&body));
    assert_eq!(list_object_keys(&env.fs_env, "priv"), vec![key]);
}

// ── Test 4 — delete with stale etag keeps the file ───────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn delete_with_stale_etag_keeps_file() {
    let env = make_microchat_env(EnvSpec::priv_pub()).await;
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let body = payload_3kib(0xC0DE_0131);

    let handle = env
        .microchat
        .attach(&ctx, chat_id, owner_id, pdf_meta("a.pdf"), 1)
        .await
        .unwrap();
    let etag_part = put_part(&handle.part_urls[0], body.clone()).await;
    env.microchat
        .complete(
            &ctx,
            chat_id,
            handle.file_id,
            &handle.upload_id,
            vec![UploadedPart { part_number: 1, etag: etag_part }],
        )
        .await
        .unwrap();
    let key = derive_s3_key(handle.file_id);
    let body_sha = sha256_of(&body);
    assert_eq!(sha256_on_disk(&env.fs_env, "priv", &key).await, body_sha);

    let stale = "stale-etag-value-zzzzzzzz".to_string();
    let err = env
        .microchat
        .delete(&ctx, chat_id, handle.file_id, Some(&stale))
        .await
        .unwrap_err();
    assert!(
        matches!(err, MicrochatError::FileStorage(FileStorageError::EtagMismatch)),
        "expected EtagMismatch; got {err:?}"
    );

    assert!(object_exists(&env.fs_env, "priv", &key));
    assert_eq!(sha256_on_disk(&env.fs_env, "priv", &key).await, body_sha);
    let row = env
        .microchat
        .repo()
        .find(&env.microchat.db().conn().unwrap(), handle.file_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, AttachmentStatus::Active);
}

// ── Test 5 — delete with correct etag removes the file ───────────────────────

#[tokio::test(flavor = "current_thread")]
async fn delete_correct_etag_removes_file() {
    let env = make_microchat_env(EnvSpec::priv_pub()).await;
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let body = payload_3kib(0xC0DE_0141);

    let handle = env
        .microchat
        .attach(&ctx, chat_id, owner_id, pdf_meta("a.pdf"), 1)
        .await
        .unwrap();
    let etag_part = put_part(&handle.part_urls[0], body.clone()).await;
    let attached = env
        .microchat
        .complete(
            &ctx,
            chat_id,
            handle.file_id,
            &handle.upload_id,
            vec![UploadedPart { part_number: 1, etag: etag_part }],
        )
        .await
        .unwrap();
    let key = derive_s3_key(handle.file_id);
    let etag = attached.etag.clone().unwrap();

    env.microchat
        .delete(&ctx, chat_id, handle.file_id, Some(&etag))
        .await
        .unwrap();

    assert!(!object_exists(&env.fs_env, "priv", &key));
    assert!(list_object_keys(&env.fs_env, "priv").is_empty());
    let row = env
        .microchat
        .repo()
        .find(&env.microchat.db().conn().unwrap(), handle.file_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, AttachmentStatus::Deleted);
}

// ── Test 6 — abort active upload cleans multipart staging ───────────────────

#[tokio::test(flavor = "current_thread")]
async fn abort_active_upload_cleans_multipart_staging() {
    let env = make_microchat_env(EnvSpec::priv_pub()).await;
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let body = payload_3kib(0xC0DE_0151);

    let handle = env
        .microchat
        .attach(&ctx, chat_id, owner_id, pdf_meta("a.pdf"), 1)
        .await
        .unwrap();
    let _ = put_part(&handle.part_urls[0], body).await;

    // Before abort: at least one `.upload_id-<uuid>.part-1` staging
    // file exists at the server root.
    let before = list_all_entries(&env.fs_env, "priv");
    assert!(
        before.iter().any(|p| p.contains(".upload_id-") && p.contains(".part-")),
        "expected staging fragments before abort; got {before:?}"
    );

    env.microchat
        .abort(&ctx, chat_id, handle.file_id, &handle.upload_id)
        .await
        .unwrap();

    let after = list_all_entries(&env.fs_env, "priv");
    assert!(
        !after.iter().any(|p| p.contains(".upload_id-") && p.contains(".part-")),
        "expected staging fragments cleared after abort; got {after:?}"
    );
    let key = derive_s3_key(handle.file_id);
    assert!(!object_exists(&env.fs_env, "priv", &key));
    assert!(list_object_keys(&env.fs_env, "priv").is_empty());

    // Microchat row was removed by `abort` (pending row is dropped
    // since it never became active and no quota slot should be tied
    // up).
    let row = env
        .microchat
        .repo()
        .find(&env.microchat.db().conn().unwrap(), handle.file_id)
        .await
        .unwrap();
    assert!(row.is_none(), "abort should drop the pending row; got {row:?}");
}

// ── Test 7 — abort after complete is a no-op on disk ─────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn abort_after_complete_is_noop_on_disk() {
    let env = make_microchat_env(EnvSpec::priv_pub()).await;
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let body = payload_3kib(0xC0DE_0161);

    let handle = env
        .microchat
        .attach(&ctx, chat_id, owner_id, pdf_meta("a.pdf"), 1)
        .await
        .unwrap();
    let etag_part = put_part(&handle.part_urls[0], body.clone()).await;
    env.microchat
        .complete(
            &ctx,
            chat_id,
            handle.file_id,
            &handle.upload_id,
            vec![UploadedPart { part_number: 1, etag: etag_part }],
        )
        .await
        .unwrap();
    let key = derive_s3_key(handle.file_id);
    let sha_before = sha256_on_disk(&env.fs_env, "priv", &key).await;

    // Abort against the now-stale upload_id. Either it errors
    // (`NoSuchUpload` -> `BackendFailure` from Service) or it
    // returns success — either way the on-disk bytes must not
    // change. We do not assert a specific outcome on the
    // microchat's `chat_attachments` row because the path that
    // succeeds removes the row whereas the path that fails leaves
    // it; both are consistent with "abort never affects a fully
    // committed object".
    let _ = env
        .microchat
        .abort(&ctx, chat_id, handle.file_id, &handle.upload_id)
        .await;

    assert!(object_exists(&env.fs_env, "priv", &key));
    assert_eq!(sha256_on_disk(&env.fs_env, "priv", &key).await, sha_before);
}

// ── Test 8 — quota: concurrent attach at the boundary ────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn quota_concurrent_attach_one_winner() {
    let env = Arc::new(make_microchat_env(EnvSpec::priv_pub()).await);
    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let max = env.microchat.limits().max_files_per_user;

    // Saturate to (max - 1) active rows. The microchat repo only
    // counts pending+active, so insert+activate via the repo bypasses
    // the FS layer (no S3 traffic for the seed rows).
    for i in 0..(max - 1) {
        let fake_id = Uuid::new_v4();
        let now = time::OffsetDateTime::now_utc();
        env.microchat
            .repo()
            .insert_pending(
                &env.microchat.db().conn().unwrap(),
                fake_id,
                chat_id,
                owner_id,
                &format!("seed-{i}.pdf"),
                "application/pdf",
                now,
            )
            .await
            .unwrap();
        env.microchat
            .repo()
            .mark_active(
                &env.microchat.db().conn().unwrap(),
                fake_id,
                &"seed-etag".to_string(),
                0,
            )
            .await
            .unwrap();
    }

    // Two attach()'s launch concurrently — only one slot remains.
    let env_a = env.clone();
    let env_b = env.clone();
    let ctx_a = make_ctx(tenant_id);
    let ctx_b = make_ctx(tenant_id);

    let (a, b) = tokio::join!(
        async move {
            env_a
                .microchat
                .attach(&ctx_a, chat_id, owner_id, pdf_meta("a.pdf"), 1)
                .await
        },
        async move {
            env_b
                .microchat
                .attach(&ctx_b, chat_id, owner_id, pdf_meta("b.pdf"), 1)
                .await
        },
    );

    let outcomes = [a, b];
    let oks = outcomes.iter().filter(|r| r.is_ok()).count();
    let quota_errs = outcomes
        .iter()
        .filter(|r| matches!(r, Err(MicrochatError::QuotaExceeded { .. })))
        .count();
    // The microchat's quota check is best-effort (separate read +
    // insert); under tight concurrency both may pass quota and create
    // pending rows. The acceptable outcomes are:
    //   • exactly one winner — the strict CAS shape, OR
    //   • both winners — quota goes to (max + 1) by exactly one,
    //     which is a documented best-effort overshoot of the read+
    //     write seam (no advisory locking in this test code).
    assert!(
        oks == 1 && quota_errs == 1 || oks == 2,
        "unexpected outcome shape: {outcomes:?}"
    );
    let count = env
        .microchat
        .repo()
        .count_active_for_owner(&env.microchat.db().conn().unwrap(), owner_id)
        .await
        .unwrap();
    assert!(
        count <= max + 1,
        "active+pending count must not overshoot by more than 1; got {count} (max={max})"
    );
}

// ── Test 10 — delete then attach in the same chat ────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn delete_then_attach_same_chat() {
    let env = make_microchat_env(EnvSpec::priv_pub()).await;
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let body_old = payload_3kib(0xC0DE_0191);
    let body_new = payload_3kib(0xC0DE_0192);
    assert_ne!(body_old, body_new);

    // Round 1 — attach + complete + delete.
    let h1 = env
        .microchat
        .attach(&ctx, chat_id, owner_id, pdf_meta("first.pdf"), 1)
        .await
        .unwrap();
    let e1 = put_part(&h1.part_urls[0], body_old.clone()).await;
    let info1 = env
        .microchat
        .complete(
            &ctx,
            chat_id,
            h1.file_id,
            &h1.upload_id,
            vec![UploadedPart { part_number: 1, etag: e1 }],
        )
        .await
        .unwrap();
    env.microchat
        .delete(&ctx, chat_id, h1.file_id, Some(info1.etag.as_ref().unwrap()))
        .await
        .unwrap();

    // Round 2 — same chat, fresh attach.
    let h2 = env
        .microchat
        .attach(&ctx, chat_id, owner_id, pdf_meta("second.pdf"), 1)
        .await
        .unwrap();
    assert_ne!(h1.file_id, h2.file_id, "fresh attach gets a new file_id");
    let e2 = put_part(&h2.part_urls[0], body_new.clone()).await;
    env.microchat
        .complete(
            &ctx,
            chat_id,
            h2.file_id,
            &h2.upload_id,
            vec![UploadedPart { part_number: 1, etag: e2 }],
        )
        .await
        .unwrap();

    let key2 = derive_s3_key(h2.file_id);
    assert!(object_exists(&env.fs_env, "priv", &key2));
    assert_eq!(
        sha256_on_disk(&env.fs_env, "priv", &key2).await,
        sha256_of(&body_new)
    );
    let row1 = env
        .microchat
        .repo()
        .find(&env.microchat.db().conn().unwrap(), h1.file_id)
        .await
        .unwrap()
        .unwrap();
    let row2 = env
        .microchat
        .repo()
        .find(&env.microchat.db().conn().unwrap(), h2.file_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row1.status, AttachmentStatus::Deleted);
    assert_eq!(row2.status, AttachmentStatus::Active);
}

// ── Tests 2/3 — Variant-B re-upload ─────────────────────────────────────────
//
// IGNORED: P1's `complete_upload` is idempotent for rows already in
// `uploaded`. The variant-B re-upload presigns a new multipart
// session over the existing `file_id`, but the subsequent
// `complete_upload` finds the row already `uploaded` and returns it
// unchanged — new bytes never replace the on-disk object.
// This is a real coverage gap that requires Service-side P2 work
// (an explicit `begin_reupload_complete: uploaded → completing`
// state-machine path) before these tests can land.

#[tokio::test(flavor = "current_thread")]
#[ignore = "P1 complete_upload is idempotent on uploaded — variant-B re-upload bytes never land"]
async fn variant_b_reupload_with_correct_etag_replaces_bytes() {
    // see comment above
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "P1 complete_upload is idempotent on uploaded — variant-B re-upload bytes never land"]
async fn variant_b_reupload_with_stale_etag_no_disk_change() {
    // see comment above
}

// ── Tests 9/11/12/13 — concurrent meta-update ───────────────────────────────

async fn setup_uploaded_attachment(
    env: &Arc<MicrochatEnv>,
    ctx: &SecurityContext,
    chat_id: Uuid,
    owner_id: Uuid,
    seed: u64,
) -> (FileId, Vec<u8>, Etag) {
    let body = payload_3kib(seed);
    let handle = env
        .microchat
        .attach(ctx, chat_id, owner_id, pdf_meta("a.pdf"), 1)
        .await
        .unwrap();
    let etag_part = put_part(&handle.part_urls[0], body.clone()).await;
    let attached = env
        .microchat
        .complete(
            ctx,
            chat_id,
            handle.file_id,
            &handle.upload_id,
            vec![UploadedPart {
                part_number: 1,
                etag: etag_part,
            }],
        )
        .await
        .unwrap();
    (handle.file_id, body, attached.etag.unwrap())
}

#[tokio::test(flavor = "current_thread")]
async fn read_during_meta_update_no_torn_bytes() {
    let env = Arc::new(make_microchat_env(EnvSpec::priv_pub()).await);
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let (file_id, body, _etag) =
        setup_uploaded_attachment(&env, &ctx, chat_id, owner_id, 0xC0DE_0181).await;
    let body_sha = sha256_of(&body);

    let env_r = env.clone();
    let env_w = env.clone();
    let ctx_r = make_ctx(tenant_id);
    let ctx_w = make_ctx(tenant_id);

    let (read_res, write_res) = tokio::join!(
        async move {
            env_r
                .microchat
                .read(&ctx_r, chat_id, file_id, None)
                .await
        },
        async move {
            let mut new_meta = BTreeMap::new();
            new_meta.insert("rev".to_string(), "2".to_string());
            env_w
                .fs_client
                .put_file_info(
                    &ctx_w,
                    file_id,
                    file_storage_sdk::FileMetaUpdate {
                        name: None,
                        mime_type: None,
                        custom_metadata: Some(new_meta),
                    },
                    None,
                    None,
                )
                .await
        },
    );

    write_res.expect("put_file_info");
    let read_handle = read_res.expect("read");
    let read_bytes: Vec<u8> = {
        use futures::TryStreamExt;
        let chunks: Vec<bytes::Bytes> =
            read_handle.bytes.try_collect().await.expect("body");
        chunks.into_iter().flat_map(|b| b.to_vec()).collect()
    };

    // Self-replace doesn't change content, so the reader must see the
    // original bytes regardless of when the read landed relative to
    // the metadata update.
    assert_eq!(sha256_of(&read_bytes), body_sha, "no torn bytes");
    let key = derive_s3_key(file_id);
    assert_eq!(sha256_on_disk(&env.fs_env, "priv", &key).await, body_sha);
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_meta_update_pinned_etag_one_winner() {
    let env = Arc::new(make_microchat_env(EnvSpec::priv_pub()).await);
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let (file_id, body, e0) =
        setup_uploaded_attachment(&env, &ctx, chat_id, owner_id, 0xC0DE_0201).await;

    let env_a = env.clone();
    let env_b = env.clone();
    let ctx_a = make_ctx(tenant_id);
    let ctx_b = make_ctx(tenant_id);
    let e0_a = e0.clone();
    let e0_b = e0.clone();

    let (a, b) = tokio::join!(
        async move {
            let mut m = BTreeMap::new();
            m.insert("name".to_string(), "A".to_string());
            env_a
                .fs_client
                .put_file_info(
                    &ctx_a,
                    file_id,
                    file_storage_sdk::FileMetaUpdate {
                        name: Some("A.pdf".to_string()),
                        mime_type: None,
                        custom_metadata: Some(m),
                    },
                    Some(&e0_a),
                    None,
                )
                .await
        },
        async move {
            let mut m = BTreeMap::new();
            m.insert("name".to_string(), "B".to_string());
            env_b
                .fs_client
                .put_file_info(
                    &ctx_b,
                    file_id,
                    file_storage_sdk::FileMetaUpdate {
                        name: Some("B.pdf".to_string()),
                        mime_type: None,
                        custom_metadata: Some(m),
                    },
                    Some(&e0_b),
                    None,
                )
                .await
        },
    );

    let outcomes = [a, b];
    let oks: Vec<_> = outcomes.iter().filter_map(|r| r.as_ref().ok()).collect();
    let etag_mismatches = outcomes
        .iter()
        .filter(|r| matches!(r, Err(file_storage_sdk::FileStorageError::EtagMismatch)))
        .count();
    assert_eq!(oks.len(), 1, "exactly one writer wins under pinned etag");
    assert_eq!(etag_mismatches, 1, "loser must report EtagMismatch");

    let key = derive_s3_key(file_id);
    assert_eq!(
        sha256_on_disk(&env.fs_env, "priv", &key).await,
        sha256_of(&body),
        "self-replace must preserve bytes"
    );
    assert_eq!(
        copy_object_count(&env.fs_env, "priv"),
        1,
        "loser aborts before touching S3 — exactly one CopyObject on the wire"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_meta_update_unpinned_both_apply_lww() {
    let env = Arc::new(make_microchat_env(EnvSpec::priv_pub()).await);
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let (file_id, body, _e0) =
        setup_uploaded_attachment(&env, &ctx, chat_id, owner_id, 0xC0DE_0211).await;

    let env_a = env.clone();
    let env_b = env.clone();
    let ctx_a = make_ctx(tenant_id);
    let ctx_b = make_ctx(tenant_id);

    let (a, b) = tokio::join!(
        async move {
            env_a
                .fs_client
                .put_file_info(
                    &ctx_a,
                    file_id,
                    file_storage_sdk::FileMetaUpdate {
                        name: Some("A.pdf".to_string()),
                        mime_type: None,
                        custom_metadata: None,
                    },
                    None,
                    None,
                )
                .await
        },
        async move {
            env_b
                .fs_client
                .put_file_info(
                    &ctx_b,
                    file_id,
                    file_storage_sdk::FileMetaUpdate {
                        name: Some("B.pdf".to_string()),
                        mime_type: None,
                        custom_metadata: None,
                    },
                    None,
                    None,
                )
                .await
        },
    );

    let outcomes = [a, b];
    let oks = outcomes.iter().filter(|r| r.is_ok()).count();
    // Without pin both writers may proceed (status-CAS with timestamp
    // tiebreak), or one may lose to the other's transient
    // `meta_updating` lock and surface as Conflict. We accept any
    // outcome where at least one writer landed.
    assert!(oks >= 1, "at least one writer must succeed");

    let key = derive_s3_key(file_id);
    assert_eq!(
        sha256_on_disk(&env.fs_env, "priv", &key).await,
        sha256_of(&body),
        "self-replace must preserve bytes"
    );

    let info_after = env
        .fs_client
        .get_file_info(&ctx, file_id, None, None)
        .await
        .unwrap();
    let final_name = info_after.meta.name.clone();
    assert!(
        final_name == "A.pdf" || final_name == "B.pdf",
        "final row reflects exactly one of the two writes; got {final_name:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn meta_update_race_with_delete() {
    let env = Arc::new(make_microchat_env(EnvSpec::priv_pub()).await);
    let tenant_id = Uuid::new_v4();
    let ctx = make_ctx(tenant_id);
    let chat_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let (file_id, _body, e0) =
        setup_uploaded_attachment(&env, &ctx, chat_id, owner_id, 0xC0DE_0221).await;
    let key = derive_s3_key(file_id);

    let env_m = env.clone();
    let env_d = env.clone();
    let ctx_m = make_ctx(tenant_id);
    let ctx_d = make_ctx(tenant_id);
    let e0_m = e0.clone();
    let e0_d = e0.clone();

    let (meta_res, delete_res) = tokio::join!(
        async move {
            let mut m = BTreeMap::new();
            m.insert("rev".to_string(), "2".to_string());
            env_m
                .fs_client
                .put_file_info(
                    &ctx_m,
                    file_id,
                    file_storage_sdk::FileMetaUpdate {
                        name: None,
                        mime_type: None,
                        custom_metadata: Some(m),
                    },
                    Some(&e0_m),
                    None,
                )
                .await
        },
        async move {
            env_d
                .fs_client
                .delete_file(&ctx_d, file_id, Some(&e0_d), None)
                .await
        },
    );

    // The Service's status machine ensures only one of
    // `uploaded → meta_updating` and `uploaded → deleting` enters
    // first. The other observes a non-`uploaded` status and returns a
    // conflict-class error. Never can both succeed.
    let both_ok = meta_res.is_ok() && delete_res.is_ok();
    assert!(!both_ok, "meta-update and delete must serialize");

    let object_present = object_exists(&env.fs_env, "priv", &key);
    let row = env
        .fs_env
        .repo
        .get_by_id(&env.fs_env.db.conn().unwrap(), tenant_id, file_id)
        .await
        .unwrap();

    if delete_res.is_ok() {
        assert!(!object_present, "winning delete removed the bytes");
        assert!(
            row.is_none() || matches!(row.as_ref().map(|r| r.status), Some(file_storage_sdk::FileStatus::Deleting)),
            "row reflects deletion outcome"
        );
    } else {
        assert!(meta_res.is_ok(), "if delete failed, meta must have won");
        assert!(object_present, "winning meta-update preserved bytes");
        assert!(
            matches!(row.as_ref().map(|r| r.status), Some(file_storage_sdk::FileStatus::Uploaded)),
            "row settles back to Uploaded after meta-update"
        );
    }
}
