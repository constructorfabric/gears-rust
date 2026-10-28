//! ADR-0006 acceptance tests for the multipart offset-manifest composite hash mode.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use bytes::Bytes;
use sea_orm::{ConnectionTrait, Database, Statement};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::infra::backend::{InMemoryBackend, StorageBackend};
use file_storage::infra::content::hash;
use file_storage::infra::content::hash_mode::{HashMode, Manifest};
use file_storage::infra::storage::Store;
use file_storage_sdk::NewFile;

mod common;
use common::{CompleteOutcomeExt, CountingBackend, Stack, StackBuilder, ctx, put_part};

/// A stack over `backends` (default id `default_id`) on a fresh DB.
async fn stack_over(backends: Vec<Arc<dyn StorageBackend>>, default_id: &str) -> Stack {
    StackBuilder::new("chm-test")
        .backends(backends, default_id)
        .build()
        .await
}

fn new_file() -> NewFile {
    common::new_file("upload.bin")
}

/// Drive a full multipart upload via the service (two 5 MiB parts plus a small tail), simulating the
/// sidecar callbacks. Returns `(file_id, version_id, upload_id, full content)`.
async fn drive_multipart(
    s: &Stack,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
) -> (Uuid, Uuid, Uuid, Vec<u8>) {
    let ticket = s
        .svc
        .create_file(ctx, new_file(), None, false)
        .await
        .unwrap();
    let file_id = ticket.file_id;

    let part_size = 5 * 1024 * 1024usize;
    let parts = [
        Bytes::from(vec![b'a'; part_size]),
        Bytes::from(vec![b'b'; part_size]),
        Bytes::from(vec![b'c'; 4096]),
    ];
    let full: Vec<u8> = parts.iter().flat_map(|p| p.iter().copied()).collect();

    let plan = s
        .msvc
        .initiate_multipart_upload(
            ctx,
            file_id,
            "application/octet-stream",
            full.len() as u64,
            None,
            false,
        )
        .await
        .unwrap();

    for part in &plan.parts {
        let data = parts[usize::try_from(part.part_number).unwrap() - 1].clone();
        put_part(
            &s.multipart_store,
            backend,
            &plan,
            file_id,
            part.part_number,
            data,
        )
        .await;
    }

    (file_id, plan.version_id, plan.upload_id, full)
}

/// One 10 MiB multipart upload, end to end: completing issues no object re-read (ADR-0006); the
/// version is `multipart-composite-sha256` with a manifest a client can re-verify the content
/// against (and tampering anywhere is detected); the persisted complete-result snapshot omits the
/// manifest while an idempotent re-complete still returns it.
#[tokio::test]
async fn complete_multipart_composite_end_to_end() {
    let counting = CountingBackend::new(Arc::new(InMemoryBackend::new("mem")));
    let backend: Arc<dyn StorageBackend> = Arc::clone(&counting) as Arc<dyn StorageBackend>;
    let s = stack_over(vec![Arc::clone(&backend)], "mem").await;
    let ctx = ctx(Uuid::now_v7());

    let (file_id, version_id, upload_id, full) = drive_multipart(&s, &backend, &ctx).await;

    let before = counting.reads();
    let first = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(
        counting.reads() - before,
        0,
        "complete_multipart must not GetObject/re-read the assembled object (ADR-0006)"
    );

    let version = s
        .store
        .get_version(file_id, version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.hash_mode, "multipart-composite-sha256");
    assert_eq!(version.part_count, Some(3));
    let manifest = s
        .store
        .get_version_manifest(version_id)
        .await
        .unwrap()
        .expect("a multipart-composite version must have a manifest row");
    assert_eq!(
        first.manifest.as_deref(),
        Some(manifest.as_str()),
        "the completion response's manifest must match the canonical version_hash_manifest row"
    );

    // Client re-verification: the content verifies; a tampered first part or tail does not.
    let verify = |content: &[u8]| {
        Store::verify_content_hash(
            content,
            HashMode::MultipartCompositeSha256,
            &version.hash_value,
            Some(&manifest),
        )
    };
    verify(&full).expect("re-verification must succeed on untampered content");
    let mut tampered = full.clone();
    tampered[10] ^= 0xff;
    assert!(
        matches!(verify(&tampered), Err(DomainError::HashMismatch { .. })),
        "a tampered first part must fail re-verification"
    );
    let mut tampered_tail = full.clone();
    let last = tampered_tail.len() - 1;
    tampered_tail[last] ^= 0xff;
    assert!(
        verify(&tampered_tail).is_err(),
        "a tampered tail part must fail re-verification"
    );

    let parsed = Manifest::from_wire_string(&manifest).unwrap();
    assert_eq!(parsed.root().as_slice(), version.hash_value.as_slice());
    assert_eq!(hash::sha256(manifest.as_bytes()), version.hash_value);

    // Raw-SQL check of the persisted snapshot -- bypasses the domain layer, which never
    // deserializes an unknown `manifest` key back out, so this is the only way to see it really is
    // not written.
    let conn = Database::connect(&s.dsn).await.expect("raw connect");
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT complete_result FROM multipart_uploads".to_owned(),
        ))
        .await
        .expect("query complete_result")
        .expect("exactly one multipart_uploads row");
    let complete_result_json: String = row
        .try_get("", "complete_result")
        .expect("complete_result column must be non-NULL after a successful complete");
    assert!(
        !complete_result_json.contains("manifest"),
        "persisted complete_result JSON must not contain a manifest field: {complete_result_json}"
    );

    // Idempotent re-complete: must still return the correct manifest, even though the persisted
    // snapshot it replays from carries none.
    let replay = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .expect("re-complete of a completed session must be idempotent")
        .unwrap_completed();
    assert_eq!(
        replay.manifest, first.manifest,
        "replay must return the same manifest as the original completion"
    );
}

#[tokio::test]
async fn migrate_backend_verifies_multipart_composite_without_parts_rows() {
    let src: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let dst: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem2"));
    let s = stack_over(vec![Arc::clone(&src), Arc::clone(&dst)], "mem").await;
    let (svc, store) = (&s.svc, &s.store);
    let ctx = ctx(Uuid::now_v7());

    let (file_id, version_id, upload_id, _full) = drive_multipart(&s, &src, &ctx).await;
    s.msvc
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap();

    // `migrate_backend` verification must not depend on the part rows.
    let conn = Database::connect(&s.dsn).await.expect("raw connect");
    let deleted = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DELETE FROM multipart_upload_parts".to_owned(),
        ))
        .await
        .expect("delete parts");
    assert!(
        deleted.rows_affected() >= 1,
        "the test must actually delete the part rows it is proving are unnecessary"
    );

    // Drop the leftover pending version from `create_file`: `migrate_backend` needs exactly one.
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM file_versions WHERE status = 'pending'".to_owned(),
    ))
    .await
    .expect("delete leftover pending version");

    svc.migrate_backend(&ctx, file_id, "mem2")
        .await
        .expect("migrate must verify from object bytes + manifest row alone");

    let version = store
        .get_version(file_id, version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.backend_id, "mem2");
    let manifest = store
        .get_version_manifest(version_id)
        .await
        .unwrap()
        .unwrap();
    let moved = dst.get(&version.backend_path).await.unwrap();
    Store::verify_content_hash(
        &moved,
        HashMode::MultipartCompositeSha256,
        &version.hash_value,
        Some(&manifest),
    )
    .expect("destination copy must still verify against the manifest");
}
