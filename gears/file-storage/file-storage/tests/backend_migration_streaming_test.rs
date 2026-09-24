//! Streaming `migrate_backend` regression tests.
//!
//! `migrate_backend` used to read the whole source object into a `Bytes`
//! buffer, verify its hash against the buffer, then write the whole buffer to
//! the destination. These tests exercise the streaming replacement: the
//! object must round-trip byte-identical while arriving in many chunks (both
//! content-hash modes, including a real S3-compatible multipart source via
//! `s3s-fs`), and the two failure modes a streaming read/verify/write
//! pipeline must still catch correctly — a corrupted source read and a
//! source stream that breaks mid-transfer — must leave the version on its
//! original backend with nothing written at the destination.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use file_storage_sdk::{NewFile, OwnerKind};
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use sea_orm::ConnectionTrait;
use sea_orm_migration::MigratorTrait;
use tempfile::TempDir;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{
    BackendCapabilities, BackendRegistry, InMemoryBackend, LocalFsBackend, MultipartCompletionPart,
    S3Backend, StorageBackend,
};
use file_storage::infra::content::hash_mode::Manifest;
use file_storage::infra::content::{hash, mime};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;

/// Direct byte-path test double for what `DataPlaneService` used to provide
/// (removed: production never constructed it — the sidecar is the only real
/// byte path). Same steps `DataPlaneService::put_content` used to perform,
/// but through `put_stream` rather than the whole-object `put` that no
/// longer exists on `StorageBackend`.
struct TestDataPlane {
    svc: Arc<FileService>,
    store: Store,
    backends: BackendRegistry,
}

impl TestDataPlane {
    fn new(svc: Arc<FileService>, store: Store, backends: BackendRegistry) -> Self {
        Self {
            svc,
            store,
            backends,
        }
    }

    async fn put_content(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        declared_mime: &str,
        bytes: Bytes,
    ) -> Result<(), DomainError> {
        mime::validate(declared_mime, &bytes)?;
        self.svc.authorize_write(ctx, file_id).await?;
        let version = self
            .store
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;
        let backend = self.backends.get(&version.backend_id)?;
        let len = bytes.len() as u64;
        let digest = hash::sha256(&bytes);
        let stream: BoxStream<'static, std::io::Result<Bytes>> =
            Box::pin(stream::once(async move { Ok(bytes) }));
        backend
            .put_stream(&version.backend_path, stream, Some(len))
            .await?;
        self.svc
            .finalize_upload(
                ctx,
                file_id,
                version_id,
                i64::try_from(len).unwrap_or(i64::MAX),
                digest,
            )
            .await
    }
}

/// Read the whole blob at `path` back via `get_stream` (collecting every
/// chunk) — the test-only stand-in for the whole-object `get` the trait no
/// longer has.
async fn read_all(backend: &Arc<dyn StorageBackend>, path: &str, expected_len: u64) -> Bytes {
    let mut stream = backend
        .get_stream(path, expected_len)
        .await
        .expect("get_stream");
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.expect("chunk"));
    }
    Bytes::from(buf)
}

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.migrate_stream_test.file.type.v1~");

// ── shared harness (mirrors tests/cleanup_test.rs's own, self-contained) ───

async fn build_db() -> Arc<DBProvider<DbError>> {
    build_db_with_dsn().await.0
}

/// Like [`build_db`], but also returns the sqlite DSN -- needed by the
/// multipart-composite happy-path test below, which opens a second,
/// independent raw connection to delete `create_file`'s leftover pending
/// version row (mirrors `tests/content_hash_modes_test.rs`'s own use of this
/// pattern).
async fn build_db_with_dsn() -> (Arc<DBProvider<DbError>>, String) {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-migrate-stream-test-{}.db",
        Uuid::now_v7().simple()
    ));
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
        name: "migrate.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

struct Services {
    svc: Arc<FileService>,
    msvc: Arc<MultipartService>,
    dp: TestDataPlane,
    store: Store,
}

/// Build a full service set (`FileService` + `MultipartService` +
/// `DataPlaneService`) against `db` and `backends`. Two calls sharing the
/// same `db` but different registries build two independent `FileService`s
/// over the identical, shared file/version rows — used by the fault-injection
/// tests below to set up a file through a plain backend, then exercise
/// `migrate_backend` through a second service whose registry substitutes a
/// fault-injecting wrapper for that same backend id.
fn build_services(db: &Arc<DBProvider<DbError>>, backends: BackendRegistry) -> Services {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends.clone());
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store.clone()) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    Services {
        svc,
        msvc,
        dp,
        store,
    }
}

fn expected_dest_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}

// ── ChunkCountingBackend: counts `get_stream` chunks, passes everything else
//    straight through to `inner` (including multipart, so it can also be the
//    backend a real multipart upload was driven against before migrating) ──

struct ChunkCountingBackend {
    inner: Arc<dyn StorageBackend>,
    get_stream_chunks: Arc<AtomicUsize>,
}

impl ChunkCountingBackend {
    fn new(inner: Arc<dyn StorageBackend>) -> (Arc<Self>, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                inner,
                get_stream_chunks: Arc::clone(&counter),
            }),
            counter,
        )
    }
}

#[async_trait]
impl StorageBackend for ChunkCountingBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    async fn put_stream(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        self.inner.put_stream(path, stream, max_size).await
    }
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        self.inner.publish_exclusive(path, stream, max_size).await
    }
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        let inner_stream = self.inner.get_stream(path, expected_len).await?;
        let counter = Arc::clone(&self.get_stream_chunks);
        let wrapped = inner_stream.inspect(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        Ok(Box::pin(wrapped))
    }
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        self.inner.read_prefix(path, max_bytes).await
    }
    async fn get_range_stream(
        &self,
        path: &str,
        range: file_storage_sdk::ByteRange,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
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
    async fn initiate_multipart(&self, path: &str) -> Result<String, DomainError> {
        self.inner.initiate_multipart(path).await
    }
    async fn upload_part_stream(
        &self,
        path: &str,
        upload_handle: &str,
        part_number: u32,
        part_offset: u64,
        stream: BoxStream<'static, std::io::Result<Bytes>>,
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
}

// ── FaultyBackend: injects a corrupted or truncated `get_stream`, otherwise
//    passes straight through to `inner` ─────────────────────────────────────

enum Fault {
    /// Flip one bit at this byte offset within the object.
    FlipByte(usize),
    /// Yield only the first `n` bytes, then a transport error.
    AbortAfter(usize),
}

struct FaultyBackend {
    inner: Arc<dyn StorageBackend>,
    fault: Fault,
}

/// Chunk size [`FaultyBackend::get_stream`] rebuilds its stream with.
const FAULTY_CHUNK_SIZE: usize = 4096;

#[async_trait]
impl StorageBackend for FaultyBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    async fn put_stream(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        self.inner.put_stream(path, stream, max_size).await
    }
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        self.inner.publish_exclusive(path, stream, max_size).await
    }
    /// Fetches the real object once (test-harness-only; the actual object
    /// under test is small), then rebuilds a chunked stream from it with the
    /// configured fault applied — a corrupted byte (same total length, so
    /// only the hash check catches it) or a stream that errors out partway
    /// (a shorter total length, simulating a dropped connection).
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        let mut real_stream = self.inner.get_stream(path, expected_len).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = real_stream.next().await {
            bytes.extend_from_slice(
                &chunk.map_err(|e| DomainError::backend("faulty", e.to_string()))?,
            );
        }
        assert_eq!(
            bytes.len() as u64,
            expected_len,
            "test setup: the faulty backend's declared length must match the real object"
        );
        match self.fault {
            Fault::FlipByte(pos) => {
                bytes[pos] ^= 0xff;
                let chunks: Vec<IoResultBytes> = bytes
                    .chunks(FAULTY_CHUNK_SIZE)
                    .map(|c| Ok(Bytes::copy_from_slice(c)))
                    .collect();
                Ok(Box::pin(stream::iter(chunks)))
            }
            Fault::AbortAfter(n) => {
                let mut chunks: Vec<IoResultBytes> = bytes[..n]
                    .chunks(FAULTY_CHUNK_SIZE)
                    .map(|c| Ok(Bytes::copy_from_slice(c)))
                    .collect();
                chunks.push(Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "simulated source transport failure",
                )));
                Ok(Box::pin(stream::iter(chunks)))
            }
        }
    }
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        self.inner.read_prefix(path, max_bytes).await
    }
    async fn get_range_stream(
        &self,
        path: &str,
        range: file_storage_sdk::ByteRange,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
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
}

/// Named alias purely so the `Vec<...>` type annotations above read cleanly.
type IoResultBytes = std::io::Result<Bytes>;

// ── s3s-fs harness (mirrors src/infra/backend/s3_tests.rs's private one —
//    that module is internal to the crate and not reachable from here) ────

const TEST_ACCESS_KEY: &str = "test-access-key";
const TEST_SECRET_KEY: &str = "test-secret-key";

async fn start_s3s_fs() -> (SocketAddr, TempDir) {
    let dir = tempfile::tempdir().expect("create temp dir for s3s-fs backing store");
    let fs = s3s_fs::FileSystem::new(dir.path()).expect("init s3s-fs FileSystem");

    let mut builder = s3s::service::S3ServiceBuilder::new(fs);
    builder.set_auth(s3s::auth::SimpleAuth::from_single(
        TEST_ACCESS_KEY,
        TEST_SECRET_KEY,
    ));
    let service = builder.build();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind ephemeral port for s3s-fs test server");
    let local_addr = listener.local_addr().expect("resolve bound local addr");

    tokio::spawn(async move {
        let http_server =
            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                continue;
            };
            let io = hyper_util::rt::TokioIo::new(socket);
            let conn = http_server
                .serve_connection(io, service.clone())
                .into_owned();
            tokio::spawn(async move {
                drop(conn.await);
            });
        }
    });

    (local_addr, dir)
}

async fn make_s3_backend(addr: SocketAddr, dir: &TempDir, bucket: &str) -> S3Backend {
    tokio::fs::create_dir_all(dir.path().join(bucket))
        .await
        .expect("pre-create s3s-fs bucket directory");
    let endpoint: url::Url = format!("http://{addr}")
        .parse()
        .expect("valid endpoint url");
    S3Backend::new(
        "s3-source",
        endpoint,
        "us-east-1",
        bucket,
        TEST_ACCESS_KEY,
        TEST_SECRET_KEY,
    )
    .expect("construct S3Backend")
}

/// Drive a real multipart upload (initiate -> native `upload_part_stream` x N
/// -> `complete_multipart_upload`) against `backend`, producing a bound
/// `multipart-composite-sha256` version. Mirrors
/// `tests/content_hash_modes_test.rs`'s `drive_multipart` helper. Returns
/// `(file_id, version_id, full_bytes)`.
async fn drive_multipart(
    svc: &FileService,
    msvc: &Arc<MultipartService>,
    store: &Store,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
) -> (Uuid, Uuid, Vec<u8>) {
    let ticket = svc.create_file(ctx, new_file(), None, false).await.unwrap();
    let file_id = ticket.file_id;

    let part_size = 5 * 1024 * 1024usize; // DEFAULT_MIN_PART_SIZE
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
        let stream: BoxStream<'static, std::io::Result<Bytes>> =
            Box::pin(stream::once(async move { Ok(data) }));
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

    let completed = msvc
        .complete_multipart_upload(ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();

    (file_id, completed.version_id, full)
}

// ── happy path: whole-sha256, many chunks, byte-identical, CAS switched ────

#[tokio::test]
async fn migrate_whole_sha256_streams_many_chunks_byte_identical() {
    let tmp = tempfile::tempdir().expect("tempdir for local-fs backend");
    let local_inner: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new("local", tmp.path()));
    let (local, source_chunks) = ChunkCountingBackend::new(local_inner);
    let local: Arc<dyn StorageBackend> = local;
    let dest_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));

    let backends =
        BackendRegistry::new(vec![Arc::clone(&local), Arc::clone(&dest_backend)], "local")
            .expect("registry");
    let db = build_db().await;
    let Services { svc, dp, store, .. } = build_services(&db, backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    // A few MiB, deliberately not a round/trivial pattern.
    let content: Vec<u8> = (0u32..(3 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "application/octet-stream",
        Bytes::from(content.clone()),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.hash_mode, "whole-sha256");
    assert_eq!(before.backend_id, "local");

    svc.migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .expect("migrate must succeed");

    let chunks_seen = source_chunks.load(Ordering::SeqCst);
    assert!(
        chunks_seen > 4,
        "expected the source object to stream in many chunks, saw only {chunks_seen}"
    );

    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.backend_id, "alt",
        "CAS must switch the version to alt"
    );

    let dest_path = expected_dest_path(ticket.file_id, ticket.version_id);
    let moved = read_all(&dest_backend, &dest_path, content.len() as u64).await;
    assert_eq!(
        moved.as_ref(),
        content.as_slice(),
        "destination must be byte-identical to source"
    );
    assert!(
        !local.exists(&dest_path).await.unwrap(),
        "source blob must be best-effort deleted after a successful migration"
    );
}

// ── happy path: multipart-composite-sha256 via a real S3 (s3s-fs) source,
//    many chunks over HTTP, byte-identical, CAS switched ───────────────────

#[tokio::test]
async fn migrate_multipart_composite_streams_many_chunks_byte_identical() {
    let (addr, dir) = start_s3s_fs().await;
    let bucket = format!("test-{}", Uuid::now_v7());
    let s3_inner: Arc<dyn StorageBackend> = Arc::new(make_s3_backend(addr, &dir, &bucket).await);
    let (s3_source, source_chunks) = ChunkCountingBackend::new(s3_inner);
    let s3_source: Arc<dyn StorageBackend> = s3_source;
    let dest_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));

    let backends = BackendRegistry::new(
        vec![Arc::clone(&s3_source), Arc::clone(&dest_backend)],
        "s3-source",
    )
    .expect("registry");
    let (db, dsn) = build_db_with_dsn().await;
    let Services {
        svc, msvc, store, ..
    } = build_services(&db, backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let (file_id, version_id, full) = drive_multipart(&svc, &msvc, &store, &s3_source, &ctx).await;

    // `create_file` pre-registers an initial (never-finalized) pending
    // version alongside the multipart-composite one; `migrate_backend` only
    // operates on non-versioned files (exactly one version), so drop the
    // leftover pending row, leaving just the completed multipart-composite
    // version (mirrors `content_hash_modes_test.rs`'s AC4 test).
    let conn = sea_orm::Database::connect(&dsn).await.expect("raw connect");
    conn.execute_raw(sea_orm::Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM file_versions WHERE status = 'pending'".to_owned(),
    ))
    .await
    .expect("delete leftover pending version");

    let before = store
        .get_version(file_id, version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.hash_mode, "multipart-composite-sha256");
    assert_eq!(before.backend_id, "s3-source");

    svc.migrate_backend(&ctx, file_id, "alt")
        .await
        .expect("migrate must succeed");

    let chunks_seen = source_chunks.load(Ordering::SeqCst);
    assert!(
        chunks_seen > 4,
        "expected the S3 source object to stream over HTTP in many chunks, saw only {chunks_seen}"
    );

    let after = store
        .get_version(file_id, version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.backend_id, "alt",
        "CAS must switch the version to alt"
    );

    let dest_path = expected_dest_path(file_id, version_id);
    let moved = read_all(&dest_backend, &dest_path, full.len() as u64).await;
    assert_eq!(
        moved.as_ref(),
        full.as_slice(),
        "destination must be byte-identical to the assembled multipart source"
    );
}

// ── failure: corrupted source read ──────────────────────────────────────

/// Both fault-injection tests below share this shape: set up a file/version
/// through a *plain* `InMemoryBackend("mem")` (so setup itself -- including
/// `finalize_upload`'s own read-back verification -- sees only real,
/// untampered bytes), then build a *second* `FileService` against the exact
/// same `db`/store, but whose registry substitutes a fault-injecting
/// [`FaultyBackend`] for that same `"mem"` id, and exercise `migrate_backend`
/// only through that second service. Returns
/// `(faulty_svc, faulty_store, dest_backend, file_id)`.
async fn setup_then_swap_in_faulty_source(
    fault: Fault,
    content: &[u8],
) -> (Arc<FileService>, Store, Arc<dyn StorageBackend>, Uuid, Uuid) {
    let db = build_db().await;
    let mem_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let dest_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));

    let setup_backends = BackendRegistry::new(
        vec![Arc::clone(&mem_inner), Arc::clone(&dest_backend)],
        "mem",
    )
    .expect("registry");
    let setup = build_services(&db, setup_backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let ticket = setup
        .svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    setup
        .dp
        .put_content(
            &ctx,
            ticket.file_id,
            ticket.version_id,
            "application/octet-stream",
            Bytes::copy_from_slice(content),
        )
        .await
        .unwrap();
    setup
        .svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let faulty: Arc<dyn StorageBackend> = Arc::new(FaultyBackend {
        inner: Arc::clone(&mem_inner),
        fault,
    });
    let faulty_backends =
        BackendRegistry::new(vec![Arc::clone(&faulty), Arc::clone(&dest_backend)], "mem")
            .expect("registry");
    let faulty_svc = build_services(&db, faulty_backends);

    (
        faulty_svc.svc,
        faulty_svc.store,
        dest_backend,
        ticket.file_id,
        tenant,
    )
}

#[tokio::test]
async fn migrate_corrupted_source_read_fails_and_leaves_source_untouched() {
    let content: Vec<u8> = (0u32..(64 * 1024)).map(|i| (i % 200) as u8).collect();
    let (svc, store, dest_backend, file_id, tenant) =
        setup_then_swap_in_faulty_source(Fault::FlipByte(1234), &content).await;
    let ctx = ctx(tenant);

    let err = svc.migrate_backend(&ctx, file_id, "alt").await.unwrap_err();
    assert!(
        matches!(err, DomainError::HashMismatch { .. }),
        "a corrupted source read must fail with HashMismatch, got {err:?}"
    );

    let version_id = store.list_versions(file_id).await.unwrap()[0].version_id;
    let after = store
        .get_version(file_id, version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.backend_id, "mem",
        "the version must stay on the source backend after a failed migration"
    );
    let dest_path = expected_dest_path(file_id, version_id);
    assert!(
        !dest_backend.exists(&dest_path).await.unwrap(),
        "the destination must have nothing written after a failed migration"
    );
}

// ── failure: source stream breaks mid-transfer ──────────────────────────

#[tokio::test]
async fn migrate_source_stream_aborts_mid_read_fails_and_leaves_source_untouched() {
    let content: Vec<u8> = (0u32..(64 * 1024)).map(|i| (i % 200) as u8).collect();
    let (svc, store, dest_backend, file_id, tenant) =
        setup_then_swap_in_faulty_source(Fault::AbortAfter(10_000), &content).await;
    let ctx = ctx(tenant);

    // Whatever the exact error shape, it must be an error, not a success --
    // asserted precisely below via the version/destination state.
    svc.migrate_backend(&ctx, file_id, "alt").await.unwrap_err();

    let version_id = store.list_versions(file_id).await.unwrap()[0].version_id;
    let after = store
        .get_version(file_id, version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.backend_id, "mem",
        "the version must stay on the source backend after a broken source stream"
    );
    let dest_path = expected_dest_path(file_id, version_id);
    assert!(
        !dest_backend.exists(&dest_path).await.unwrap(),
        "the destination must have nothing written after a broken source stream"
    );
}
