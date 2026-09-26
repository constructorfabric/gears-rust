//! ADR-0006 content-hash-modes acceptance criteria (§6).
//!
//! Proves the multipart offset-manifest composite mode end-to-end:
//!   - AC2: `complete_multipart` issues **no** `GetObject`/re-read of the
//!     assembled object (request-counting wrapper backend).
//!   - AC3: a client-side re-verification helper (split at manifest offsets,
//!     rehash, rebuild, compare to `root`) succeeds on real content and fails
//!     when any byte in any part is tampered with.
//!   - AC4: `migrate_backend` verifies a `multipart-composite-sha256` version
//!     from object bytes + the stored `version_hash_manifest` row ALONE, with
//!     the `multipart_upload_parts` rows deleted first.
//!
//! The manifest wire-format acceptance criterion (AC1) is proven by the
//! `hash_mode` unit tests (`src/infra/content/hash_mode_tests.rs`); the
//! `whole-sha256` finalize-time client-claim rejection (AC7) is proven by
//! `tests/finalize_test.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use sea_orm::{ConnectionTrait, Database, Statement};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::MultipartPlan;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{
    BackendCapabilities, BackendRegistry, InMemoryBackend, MultipartCompletionPart, StorageBackend,
};
use file_storage::infra::content::hash;
use file_storage::infra::content::hash_mode::{HashMode, Manifest};
use file_storage::infra::content::stream_verify::verify_stream;
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{ByteRange, NewFile, OwnerKind};

mod common;
use common::read_all;

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// Run `infra::content::stream_verify::verify_stream` over a fully
/// in-memory buffer, draining the wrapped stream and returning its verdict —
/// this test's stand-in for the removed `Store::verify_content_hash`
/// synchronous whole-buffer helper (client-side re-verification, AC3, does
/// not have a real backend stream to hand `verify_stream` the way
/// `migrate_backend` does, so this wraps `content` as a one-shot stream
/// instead).
async fn verify_content_hash(
    content: &[u8],
    hash_mode: HashMode,
    hash_value: &[u8],
    manifest: Option<&Manifest>,
) -> Result<(), DomainError> {
    let bytes = Bytes::copy_from_slice(content);
    let len = bytes.len() as u64;
    let inner: futures::stream::BoxStream<'static, std::io::Result<Bytes>> =
        Box::pin(futures::stream::once(async move { Ok(bytes) }));
    let (mut stream, slot) = verify_stream(
        inner,
        len,
        hash_mode,
        hash_value.to_vec(),
        manifest.cloned(),
    )?;
    while let Some(chunk) = stream.next().await {
        chunk.map_err(|e| DomainError::backend("test", e.to_string()))?;
    }
    slot.lock()
        .unwrap()
        .take()
        .expect("verify_stream must publish a verdict once fully drained")
}

/// A `StorageBackend` decorator that counts whole-object reads (`get_stream`)
/// so a test can assert the ADR-0006 "no re-read at complete" invariant.
/// Every other method delegates unchanged to the inner backend.
struct CountingBackend {
    inner: Arc<dyn StorageBackend>,
    reads: Arc<AtomicUsize>,
}

impl CountingBackend {
    fn new(inner: Arc<dyn StorageBackend>) -> (Arc<Self>, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(Self {
            inner,
            reads: Arc::clone(&reads),
        });
        (backend, reads)
    }
}

#[async_trait]
impl StorageBackend for CountingBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    async fn put_stream(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        self.inner.put_stream(path, stream, max_size).await
    }
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        self.inner.publish_exclusive(path, stream, max_size).await
    }
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get_stream(path, expected_len).await
    }
    // Not counted: a bounded prefix/range read is not a "whole-object read"
    // (see the struct doc comment above) -- unlike `get_stream`, it never
    // re-reads the entire assembled object, so it is not what AC2 guards
    // against. P2 remediation item 1.10 added exactly this kind of call (a
    // bounded MIME-sniff prefix read, now `read_prefix`) to
    // `complete_multipart_upload`, after this file's AC2 was written to prove
    // "no whole-object re-read".
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        self.inner.read_prefix(path, max_bytes).await
    }
    async fn get_range_stream(
        &self,
        path: &str,
        range: ByteRange,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_range_stream(path, range, expected_len).await
    }
    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        self.inner.size(path).await
    }
    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.inner.delete(path).await
    }
    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        self.inner.exists(path).await
    }
    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        self.inner.stat(path).await
    }
    async fn initiate_multipart(&self, path: &str) -> Result<String, DomainError> {
        self.inner.initiate_multipart(path).await
    }
    async fn upload_part_stream(
        &self,
        path: &str,
        upload_handle: &str,
        part_number: u32,
        part_offset: u64,
        stream: futures::stream::BoxStream<'static, std::io::Result<Bytes>>,
        len: u64,
    ) -> Result<(String, Vec<u8>), DomainError> {
        self.inner
            .upload_part_stream(path, upload_handle, part_number, part_offset, stream, len)
            .await
    }
    async fn complete_multipart(
        &self,
        path: &str,
        upload_handle: &str,
        parts: &[MultipartCompletionPart],
    ) -> Result<(Manifest, [u8; 32]), DomainError> {
        self.inner
            .complete_multipart(path, upload_handle, parts)
            .await
    }
    async fn abort_multipart(&self, path: &str, upload_handle: &str) -> Result<(), DomainError> {
        self.inner.abort_multipart(path, upload_handle).await
    }
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        self.inner.list_paths().await
    }
}

async fn build_db_with_dsn() -> (Arc<DBProvider<DbError>>, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-chm-test-{}.db", Uuid::now_v7().simple()));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    (Arc::new(DBProvider::new(db)), dsn)
}

fn cfg() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    }
}

fn services(
    db: &Arc<DBProvider<DbError>>,
    backends: BackendRegistry,
) -> (Arc<FileService>, Arc<MultipartService>, Store) {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg(),
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store.clone()) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    (svc, msvc, store)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file() -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "upload.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

/// Drive a full multipart upload (via the multipart service) using two 5 MiB
/// parts + a small tail, simulating the sidecar's `upload_part` +
/// `upsert_multipart_part` callbacks. Returns `(file_id, version_id,
/// upload_id, plan, full_bytes)`.
#[allow(clippy::type_complexity)]
async fn drive_multipart(
    svc: &FileService,
    msvc: &Arc<MultipartService>,
    store: &Store,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
) -> (Uuid, Uuid, Uuid, MultipartPlan, Vec<u8>) {
    let ticket = svc.create_file(ctx, new_file(), None, false).await.unwrap();
    let file_id = ticket.file_id;

    let part_size = 5 * 1024 * 1024usize;
    let part1 = vec![b'a'; part_size];
    let part2 = vec![b'b'; part_size];
    let part3 = vec![b'c'; 4096];
    let mut full = Vec::new();
    full.extend_from_slice(&part1);
    full.extend_from_slice(&part2);
    full.extend_from_slice(&part3);
    let declared_size = full.len() as u64;

    let plan = msvc
        .initiate_multipart_upload(
            ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();

    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);

    for part in &plan.parts {
        let data = match part.part_number {
            1 => Bytes::from(part1.clone()),
            2 => Bytes::from(part2.clone()),
            _ => Bytes::from(part3.clone()),
        };
        let len = data.len() as u64;
        let stream: futures::stream::BoxStream<'static, std::io::Result<Bytes>> =
            Box::pin(futures::stream::once(async move { Ok(data) }));
        let (etag, part_hash) = backend
            .upload_part_stream(
                &backend_path,
                &session.backend_upload_handle,
                part.part_number,
                part.offset,
                stream,
                len,
            )
            .await
            .unwrap();
        multipart_store
            .upsert_multipart_part(
                plan.upload_id,
                i32::try_from(part.part_number).unwrap(),
                &etag,
                part_hash,
                i64::try_from(part.size).unwrap(),
                time::OffsetDateTime::now_utc(),
            )
            .await
            .unwrap();
    }

    (file_id, plan.version_id, plan.upload_id, plan, full)
}

// ── AC2: no GetObject / re-read at complete time ──────────────────────────

#[tokio::test]
async fn complete_multipart_issues_no_object_reread() {
    let db = build_db_with_dsn().await.0;
    let inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let (counting, reads) = CountingBackend::new(inner);
    let backend: Arc<dyn StorageBackend> = counting;
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let (svc, msvc, store) = services(&db, backends);
    let ctx = ctx(Uuid::now_v7());

    let (file_id, version_id, upload_id, _plan, _full) =
        drive_multipart(&svc, &msvc, &store, &backend, &ctx).await;

    // Snapshot the whole-object-read counter, complete, and assert the
    // completion step re-read the assembled object ZERO times.
    let before = reads.load(Ordering::SeqCst);
    let _completed = msvc
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    let during_complete = reads.load(Ordering::SeqCst) - before;
    assert_eq!(
        during_complete, 0,
        "complete_multipart must not GetObject/re-read the assembled object (ADR-0006)"
    );

    // Sanity: the version really did land as multipart-composite.
    let version = store
        .get_version(file_id, version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.hash_mode, "multipart-composite-sha256");
    assert_eq!(version.part_count, Some(3));
    assert!(
        store
            .get_version_manifest(version_id)
            .await
            .unwrap()
            .is_some(),
        "a multipart-composite version must have a manifest row"
    );
}

// ── AC3: client-side re-verification succeeds; tamper fails ───────────────

#[tokio::test]
async fn client_reverification_succeeds_and_detects_tampering() {
    let db = build_db_with_dsn().await.0;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let (svc, msvc, store) = services(&db, backends);
    let ctx = ctx(Uuid::now_v7());

    let (file_id, version_id, upload_id, _plan, full) =
        drive_multipart(&svc, &msvc, &store, &backend, &ctx).await;
    let _completed = msvc
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();

    let version = store
        .get_version(file_id, version_id)
        .await
        .unwrap()
        .unwrap();
    let manifest = store
        .get_version_manifest(version_id)
        .await
        .unwrap()
        .unwrap();

    let parsed_manifest = Manifest::from_wire_string(&manifest).unwrap();

    // Independent client re-verification: split at manifest offsets, rehash,
    // rebuild, compare to root — succeeds against the real content.
    verify_content_hash(
        &full,
        HashMode::MultipartCompositeSha256,
        &version.hash_value,
        Some(&parsed_manifest),
    )
    .await
    .expect("re-verification must succeed on untampered content");

    // Flip a single byte in the FIRST part — verification must now fail.
    let mut tampered = full.clone();
    tampered[10] ^= 0xff;
    let err = verify_content_hash(
        &tampered,
        HashMode::MultipartCompositeSha256,
        &version.hash_value,
        Some(&parsed_manifest),
    )
    .await
    .expect_err("a tampered first part must fail re-verification");
    assert!(matches!(err, DomainError::HashMismatch { .. }));

    // Flip a byte in the LAST (tail) part too — also detected.
    let mut tampered_tail = full.clone();
    let last = tampered_tail.len() - 1;
    tampered_tail[last] ^= 0xff;
    assert!(
        verify_content_hash(
            &tampered_tail,
            HashMode::MultipartCompositeSha256,
            &version.hash_value,
            Some(&parsed_manifest),
        )
        .await
        .is_err(),
        "a tampered tail part must fail re-verification"
    );

    // Independent cross-check that root == sha256(manifest) using the parser.
    assert_eq!(
        parsed_manifest.root().as_slice(),
        version.hash_value.as_slice()
    );
    assert_eq!(hash::sha256(manifest.as_bytes()), version.hash_value);
}

// ── AC4: migrate_backend verifies from manifest row alone ─────────────────

#[tokio::test]
async fn migrate_backend_verifies_multipart_composite_without_parts_rows() {
    let (db, dsn) = build_db_with_dsn().await;
    let src: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let dst: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem2"));
    let backends =
        BackendRegistry::new(vec![Arc::clone(&src), Arc::clone(&dst)], "mem").expect("registry");
    let (svc, msvc, store) = services(&db, backends);
    let ctx = ctx(Uuid::now_v7());

    let (file_id, version_id, upload_id, _plan, _full) =
        drive_multipart(&svc, &msvc, &store, &src, &ctx).await;
    let _completed = msvc
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();

    // Delete the multipart-session part rows: migrate_backend's verification
    // must NOT depend on them (ADR-0006 §4 — the manifest is the durable,
    // self-contained record).
    let conn = Database::connect(&dsn).await.expect("raw connect");
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

    // `create_file` pre-registers an initial (never-finalized) pending version
    // alongside the multipart-composite one; migrate_backend only operates on
    // non-versioned files (exactly one version), so drop the leftover pending
    // row, leaving just the completed multipart-composite version.
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM file_versions WHERE status = 'pending'".to_owned(),
    ))
    .await
    .expect("delete leftover pending version");

    // Migrate mem -> mem2. Internally re-reads the object bytes, fetches the
    // version_hash_manifest row, and verifies via split-rehash-rebuild — with
    // no multipart_upload_parts rows in existence.
    svc.migrate_backend(&ctx, file_id, "mem2")
        .await
        .expect("migrate must verify from object bytes + manifest row alone");

    // The version now points at the destination backend and still verifies.
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
    let moved_len = dst.stat(&version.backend_path).await.unwrap().unwrap();
    let moved = read_all(&dst, &version.backend_path, moved_len).await;
    let parsed_manifest = Manifest::from_wire_string(&manifest).unwrap();
    verify_content_hash(
        &moved,
        HashMode::MultipartCompositeSha256,
        &version.hash_value,
        Some(&parsed_manifest),
    )
    .await
    .expect("destination copy must still verify against the manifest");
}

// ── t28: complete_result carries no duplicate manifest copy ───────────────

/// `StoredCompleteResult` (`domain/multipart.rs`) keeps no `manifest` field
/// of its own -- `version_hash_manifest` is already the canonical, durable
/// copy for a `multipart-composite-sha256` version, so persisting the same
/// (up to ~1 MiB) text a second time into `multipart_uploads.complete_result`
/// on every completion would only grow storage for no benefit.
///
/// Proves both sides of that: the persisted `complete_result` JSON contains
/// no `manifest` key, AND an idempotent re-complete (`replay_completed`)
/// still returns the correct manifest text, re-read from
/// `version_hash_manifest` rather than from the snapshot.
#[tokio::test]
async fn complete_result_snapshot_omits_manifest_but_replay_still_returns_it() {
    let (db, dsn) = build_db_with_dsn().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let (svc, msvc, store) = services(&db, backends);
    let ctx = ctx(Uuid::now_v7());

    let (file_id, version_id, upload_id, _plan, _full) =
        drive_multipart(&svc, &msvc, &store, &backend, &ctx).await;

    let first = msvc
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert!(
        first.manifest.is_some(),
        "a multipart-composite completion must return a manifest"
    );
    let canonical_manifest = store
        .get_version_manifest(version_id)
        .await
        .unwrap()
        .expect("version_hash_manifest row must exist for a composite version");
    assert_eq!(
        first.manifest.as_deref(),
        Some(canonical_manifest.as_str()),
        "the completion response's manifest must match the canonical version_hash_manifest row"
    );

    // Raw-SQL check of the persisted snapshot -- bypasses the domain layer,
    // which never deserializes an unknown `manifest` key back out, so this
    // is the only way to see it really is not written. `drive_multipart`
    // creates exactly one multipart session, so no upload_id filter is
    // needed.
    let conn = Database::connect(&dsn).await.expect("raw connect");
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

    // Idempotent re-complete: must still return the correct manifest, even
    // though the persisted snapshot it replays from carries none.
    let replay = msvc
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .expect("re-complete of a completed session must be idempotent")
        .unwrap_completed();
    assert_eq!(
        replay.manifest, first.manifest,
        "replay must return the same manifest as the original completion"
    );
}
