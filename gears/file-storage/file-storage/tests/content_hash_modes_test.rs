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
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{ByteRange, NewFile, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A `StorageBackend` decorator that counts whole-object reads (`get` /
/// `get_stream`) so a test can assert the ADR-0006 "no re-read at complete"
/// invariant. Every other method delegates unchanged to the inner backend.
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
    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        self.inner.put(path, bytes).await
    }
    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(path).await
    }
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<futures::stream::BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get_stream(path).await
    }
    // Not counted: a bounded range read is not a "whole-object read" (see the
    // struct doc comment above) -- unlike `get`/`get_stream`, it never
    // re-reads the entire assembled object, so it is not what AC2 guards
    // against. Without this override the trait's *default* `get_range`
    // (`full = self.get(path).await?`) would dispatch back through this
    // wrapper's counted `get` and inflate the counter for what is, on every
    // real backend (`LocalFsBackend`, `S3Backend`), a small native
    // range-limited fetch. P2 remediation item 1.10 added exactly this call
    // (a bounded MIME-sniff prefix read) to `complete_multipart_upload`,
    // after this file's AC2 was written to prove "no whole-object re-read".
    async fn get_range(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError> {
        self.inner.get_range(path, range).await
    }
    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.inner.delete(path).await
    }
    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        self.inner.exists(path).await
    }
    async fn initiate_multipart(&self, path: &str) -> Result<String, DomainError> {
        self.inner.initiate_multipart(path).await
    }
    async fn upload_part(
        &self,
        path: &str,
        upload_handle: &str,
        part_number: u32,
        part_offset: u64,
        data: Bytes,
    ) -> Result<(String, Vec<u8>), DomainError> {
        self.inner
            .upload_part(path, upload_handle, part_number, part_offset, data)
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
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
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
    msvc: &MultipartService,
    store: &Store,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
) -> (Uuid, Uuid, Uuid, MultipartPlan, Vec<u8>) {
    let ticket = svc.create_file(ctx, new_file(), None).await.unwrap();
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
            None,
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
        let (etag, part_hash) = backend
            .upload_part(
                &backend_path,
                &session.backend_upload_handle,
                part.part_number,
                part.offset,
                data,
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
    msvc.complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap();
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
    msvc.complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap();

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

    // Independent client re-verification: split at manifest offsets, rehash,
    // rebuild, compare to root — succeeds against the real content.
    Store::verify_content_hash(
        &full,
        HashMode::MultipartCompositeSha256,
        &version.hash_value,
        Some(&manifest),
    )
    .expect("re-verification must succeed on untampered content");

    // Flip a single byte in the FIRST part — verification must now fail.
    let mut tampered = full.clone();
    tampered[10] ^= 0xff;
    let err = Store::verify_content_hash(
        &tampered,
        HashMode::MultipartCompositeSha256,
        &version.hash_value,
        Some(&manifest),
    )
    .expect_err("a tampered first part must fail re-verification");
    assert!(matches!(err, DomainError::HashMismatch { .. }));

    // Flip a byte in the LAST (tail) part too — also detected.
    let mut tampered_tail = full.clone();
    let last = tampered_tail.len() - 1;
    tampered_tail[last] ^= 0xff;
    assert!(
        Store::verify_content_hash(
            &tampered_tail,
            HashMode::MultipartCompositeSha256,
            &version.hash_value,
            Some(&manifest),
        )
        .is_err(),
        "a tampered tail part must fail re-verification"
    );

    // Independent cross-check that root == sha256(manifest) using the parser.
    let parsed = Manifest::from_wire_string(&manifest).unwrap();
    assert_eq!(parsed.root().as_slice(), version.hash_value.as_slice());
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
    msvc.complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await
        .unwrap();

    // Delete the multipart-session part rows: migrate_backend's verification
    // must NOT depend on them (ADR-0006 §4 — the manifest is the durable,
    // self-contained record).
    let conn = Database::connect(&dsn).await.expect("raw connect");
    let deleted = conn
        .execute(Statement::from_string(
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
    conn.execute(Statement::from_string(
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
    let moved = dst.get(&version.backend_path).await.unwrap();
    Store::verify_content_hash(
        &moved,
        HashMode::MultipartCompositeSha256,
        &version.hash_value,
        Some(&manifest),
    )
    .expect("destination copy must still verify against the manifest");
}
