//! E2E harness for FileStorage: in-process s3s-fs S3 servers + a fully
//! wired `Service<R>` against SQLite `:memory:`.
//!
//! These tests are not full HTTP-level E2E in the strict sense of
//! `docs/modkit_unified_system/13_e2e_testing.md` — the FileStorage
//! module is not yet mounted on `cf-server`. They are the next-best
//! integration substrate available today: full Rust stack
//! (Service → SeaOrmRepo → SQLite) talking to a real S3 wire protocol
//! (s3s-fs over hyper on a loopback TCP socket), with real SigV4 in
//! both directions and real multipart upload.
//!
//! Caveat: `s3s-fs` does not implement S3 versioning. Versioned
//! capabilities (`*.versioned.v1`) cannot be exercised end-to-end and
//! are out of scope here — those checks remain in repo-level race
//! tests where we can fabricate `version_id` directly in the row.

#![allow(dead_code)] // shared helpers — not every test uses every helper

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, PolicyEnforcer,
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    Client as S3Client,
    config::{BehaviorVersion, Config as S3Config, SharedCredentialsProvider},
};
use file_storage::config::FileStorageConfig;
use file_storage::domain::service::{OrphanQueue, Service};
use file_storage::infra::backends::r#trait::{BackendDescriptor, SharedBackend};
use file_storage::infra::backends::registry::BackendRegistry;
use file_storage::infra::backends::s3::{S3Backend, S3BackendConfig};
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::sea_orm_repo::SeaOrmFilesRepository;
use file_storage_sdk::{Backend, BackendId, CapabilityTag};
use futures::future::BoxFuture;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use modkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::secure::Db;
use modkit_security::SecurityContext;
use s3s::access::{S3Access, S3AccessContext};
use s3s::auth::SimpleAuth;
use s3s::service::S3ServiceBuilder;
use s3s_fs::FileSystem;
use sea_orm_migration::MigratorTrait;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

const ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const REGION: &str = "us-east-1";

// ── AllowAll AuthZ mock (Service authz_check is currently a no-op, but
//    PolicyEnforcer construction still needs a backing client). ─────────────

struct AllowAllAuthZ;

#[async_trait]
impl AuthZResolverClient for AllowAllAuthZ {
    async fn evaluate(
        &self,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

// ── Anonymous-mode access policy ───────────────────────────────────────────

#[derive(Clone)]
struct AllowAllAccess;

#[async_trait]
impl S3Access for AllowAllAccess {
    async fn check(&self, _cx: &mut S3AccessContext<'_>) -> s3s::S3Result<()> {
        Ok(())
    }
}

// ── Auth mode + counted hyper service wrapper ───────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthMode {
    pub require_sigv4: bool,
}

impl AuthMode {
    pub const REQUIRE_SIGV4: Self = Self { require_sigv4: true };
    pub const ANONYMOUS: Self = Self { require_sigv4: false };
}

/// Wraps `s3s::service::S3Service` and ticks an `AtomicU64` whenever a
/// `CopyObject` request lands on the server. `CopyObject` is a `PUT`
/// with the `x-amz-copy-source` header — that is the wire-level
/// signature the AWS SDK emits for both `copy_object` and the
/// `CopyObject`-as-self-replace path used by `put_file_info`.
#[derive(Clone)]
struct CountedS3Service {
    inner: s3s::service::S3Service,
    copy_object_counter: Arc<AtomicU64>,
}

impl hyper::service::Service<hyper::Request<hyper::body::Incoming>> for CountedS3Service {
    type Response = s3s::HttpResponse;
    type Error = s3s::HttpError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn call(&self, req: hyper::Request<hyper::body::Incoming>) -> Self::Future {
        if req.method() == hyper::Method::PUT
            && req.headers().contains_key("x-amz-copy-source")
        {
            self.copy_object_counter.fetch_add(1, Ordering::Relaxed);
        }
        hyper::service::Service::call(&self.inner, req)
    }
}

// ── In-process s3s-fs server ────────────────────────────────────────────────

/// Live in-process S3 server backed by `s3s-fs` on a temp dir. Drops
/// the shutdown channel on `drop`, so the spawned accept loop exits
/// and the `TempDir` is removed.
pub struct TestS3Server {
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    pub auth_mode: AuthMode,
    /// Per-server count of received `CopyObject` requests. Tests that
    /// pin ETag-CAS races read this to assert that the losing writer
    /// aborted before touching S3.
    pub copy_object_counter: Arc<AtomicU64>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    root: TempDir,
}

impl TestS3Server {
    pub fn root_path(&self) -> &Path {
        self.root.path()
    }
}

impl Drop for TestS3Server {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Bind s3s-fs to `127.0.0.1:0` with SigV4 auth required and spawn the
/// accept loop. Equivalent to `start_s3_server_with(AuthMode::REQUIRE_SIGV4)`.
pub async fn start_s3_server() -> TestS3Server {
    start_s3_server_with(AuthMode::REQUIRE_SIGV4).await
}

/// Bind s3s-fs to `127.0.0.1:0` and spawn the accept loop. The `auth`
/// parameter chooses whether the server requires SigV4 — that is the
/// test-side equivalent of a real S3 bucket policy granting (or
/// denying) anonymous access.
///
/// Both modes register `SimpleAuth`, so the AWS SDK (which signs
/// every request) always succeeds. The mode only differs in the
/// access policy: `REQUIRE_SIGV4` uses s3s's default policy that
/// denies anonymous/unsigned requests, while `ANONYMOUS` installs an
/// `AllowAll` access policy that passes both signed and unsigned
/// requests through. The latter is what makes a bare-URL GET on a
/// `download.s3.public.v1` URL succeed without a signature, mirroring
/// a real public-bucket policy.
pub async fn start_s3_server_with(auth: AuthMode) -> TestS3Server {
    let root = tempfile::tempdir().expect("tempdir");
    let fs = FileSystem::new(root.path()).expect("FileSystem::new");
    let svc = {
        let mut b = S3ServiceBuilder::new(fs);
        b.set_auth(SimpleAuth::from_single(ACCESS_KEY, SECRET_KEY));
        if !auth.require_sigv4 {
            b.set_access(AllowAllAccess);
        }
        b.build()
    };

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");

    let counter = Arc::new(AtomicU64::new(0));
    let counted = CountedS3Service {
        inner: svc,
        copy_object_counter: counter.clone(),
    };

    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let http = ConnBuilder::new(TokioExecutor::new());
        let mut shutdown = pin!(rx);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                accepted = listener.accept() => {
                    let Ok((sock, _)) = accepted else { continue };
                    let svc = counted.clone();
                    let conn = http.serve_connection(TokioIo::new(sock), svc).into_owned();
                    tokio::spawn(async move {
                        let _ = conn.await;
                    });
                }
            }
        }
    });

    // SimpleAuth is registered for both modes (so the AWS SDK can
    // always sign successfully); the difference is only in the access
    // policy. Hand the same canonical credentials back to callers.
    TestS3Server {
        endpoint: format!("http://{addr}"),
        access_key: ACCESS_KEY.to_string(),
        secret_key: SECRET_KEY.to_string(),
        auth_mode: auth,
        copy_object_counter: counter,
        shutdown_tx: Some(tx),
        root,
    }
}

/// Direct AWS SDK client against a test server — handy for
/// `create_bucket` and tearing down state from inside a test.
pub fn aws_client(s3: &TestS3Server) -> S3Client {
    let creds = Credentials::new(
        s3.access_key.clone(),
        s3.secret_key.clone(),
        None,
        None,
        "test",
    );
    let cfg = S3Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(REGION))
        .endpoint_url(s3.endpoint.clone())
        .credentials_provider(SharedCredentialsProvider::new(creds))
        .force_path_style(true)
        .build();
    S3Client::from_conf(cfg)
}

pub async fn create_bucket(s3: &TestS3Server, bucket: &str) {
    aws_client(s3)
        .create_bucket()
        .bucket(bucket)
        .send()
        .await
        .expect("create_bucket");
}

// ── DB + AuthZ + Service wiring ─────────────────────────────────────────────

/// SQLite `:memory:` DB with `cf-file-storage` migrations applied.
pub async fn test_db() -> Db {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite::memory:");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run file_storage migrations");
    db
}

pub fn make_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(AllowAllAuthZ))
}

pub fn make_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("ctx")
}

// ── Bucket spec + env spec ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BucketSpec {
    /// Logical key used by tests to address this bucket
    /// (`env.buckets[key]`). Must be unique within an `EnvSpec`.
    pub key: &'static str,
    pub default_private: bool,
    pub default_public: bool,
    pub capabilities: Vec<&'static str>,
    /// Auth mode of the s3s server hosting this bucket. The microchat
    /// public-bucket flow requires `ANONYMOUS`; SigV4 buckets require
    /// `REQUIRE_SIGV4`.
    pub auth_mode: AuthMode,
}

#[derive(Debug, Clone)]
pub struct EnvSpec {
    pub buckets: Vec<BucketSpec>,
}

impl EnvSpec {
    /// Single private bucket — the legacy harness shape used by the
    /// smoke test.
    pub fn private_only(caps: Vec<&'static str>) -> Self {
        Self {
            buckets: vec![BucketSpec {
                key: "priv",
                default_private: true,
                default_public: false,
                capabilities: caps,
                auth_mode: AuthMode::REQUIRE_SIGV4,
            }],
        }
    }

    /// Two-quadrant standard env for microchat tests: one SigV4-auth
    /// `priv` bucket and one anonymous `pub` bucket. Each bucket lives
    /// on its own s3s server with its own `TempDir`.
    pub fn priv_pub() -> Self {
        Self {
            buckets: vec![
                BucketSpec {
                    key: "priv",
                    default_private: true,
                    default_public: false,
                    capabilities: vec![
                        "upload.s3.multipart.sigv4.v1",
                        "download.s3.sigv4.v1",
                    ],
                    auth_mode: AuthMode::REQUIRE_SIGV4,
                },
                BucketSpec {
                    key: "pub",
                    default_private: false,
                    default_public: true,
                    capabilities: vec![
                        "upload.s3.multipart.sigv4.v1",
                        "download.s3.public.v1",
                    ],
                    auth_mode: AuthMode::ANONYMOUS,
                },
            ],
        }
    }
}

// ── BucketHandle + TestEnv ─────────────────────────────────────────────────

#[derive(Clone)]
pub struct BucketHandle {
    pub bucket: String,
    pub backend_id: BackendId,
    /// Path on the test runner's filesystem — `<server_root>/<bucket>`.
    pub root_path: PathBuf,
    pub s3_endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    pub auth_mode: AuthMode,
    pub copy_object_counter: Arc<AtomicU64>,
    /// Path of the s3s server's `TempDir`. Used by `list_all_entries`
    /// to walk the entire root (including hidden `.upload_id-*`
    /// staging files) when an abort test asserts that no fragments
    /// were left behind.
    pub server_root: PathBuf,
}

/// All wires connected: live S3 + DB + Service. Each test owns its own
/// `TestEnv`; tests do not share state.
pub struct TestEnv {
    pub buckets: HashMap<&'static str, BucketHandle>,
    pub default_private_id: BackendId,
    pub default_public_id: Option<BackendId>,
    pub service: Arc<Service<SeaOrmFilesRepository>>,
    pub repo: Arc<SeaOrmFilesRepository>,
    pub db: Db,
    /// Owned servers — kept alive so the accept-loop runs for the
    /// duration of the test. Drops in reverse order on test exit.
    _servers: Vec<TestS3Server>,
}

pub async fn make_env(spec: EnvSpec) -> TestEnv {
    assert!(!spec.buckets.is_empty(), "EnvSpec must declare at least one bucket");

    let db = test_db().await;
    let provider: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db.clone()));
    let repo = Arc::new(SeaOrmFilesRepository::new());

    let mut backends_map: HashMap<BackendId, SharedBackend> = HashMap::new();
    let mut buckets: HashMap<&'static str, BucketHandle> = HashMap::new();
    let mut servers: Vec<TestS3Server> = Vec::with_capacity(spec.buckets.len());

    let mut default_private_id: Option<BackendId> = None;
    let mut default_public_id: Option<BackendId> = None;

    for bspec in &spec.buckets {
        let server = start_s3_server_with(bspec.auth_mode).await;
        let bucket_name = format!("bkt-{}-{}", bspec.key, Uuid::new_v4().simple());
        create_bucket(&server, &bucket_name).await;

        let backend_id = Uuid::new_v4();
        let backend = make_backend(
            &server,
            &bucket_name,
            backend_id,
            bspec.default_private,
            bspec.default_public,
            &bspec.capabilities,
        );
        backends_map.insert(backend_id, backend);

        if bspec.default_private {
            assert!(
                default_private_id.replace(backend_id).is_none(),
                "EnvSpec must not declare more than one default-private bucket"
            );
        }
        if bspec.default_public {
            assert!(
                default_public_id.replace(backend_id).is_none(),
                "EnvSpec must not declare more than one default-public bucket"
            );
        }

        let server_root = server.root_path().to_path_buf();
        let handle = BucketHandle {
            bucket: bucket_name.clone(),
            backend_id,
            root_path: server_root.join(&bucket_name),
            s3_endpoint: server.endpoint.clone(),
            access_key: server.access_key.clone(),
            secret_key: server.secret_key.clone(),
            auth_mode: server.auth_mode,
            copy_object_counter: server.copy_object_counter.clone(),
            server_root,
        };
        buckets.insert(bspec.key, handle);
        servers.push(server);
    }

    let default_private_id =
        default_private_id.expect("EnvSpec must declare exactly one default-private bucket");

    let registry = Arc::new(BackendRegistry::new(backends_map));
    let cfg = Arc::new(FileStorageConfig {
        default_public_storage_id: default_public_id,
        default_private_storage_id: Some(default_private_id),
        orphan_delete_grace_seconds: 86_400,
        signed_url_clock_skew_margin_seconds: 60,
        backends: vec![],
    });
    let orphan_queue: OrphanQueue = Arc::new(tokio::sync::Mutex::new(Default::default()));
    let service = Arc::new(Service::new(
        provider,
        repo.clone(),
        make_enforcer(),
        cfg,
        registry,
        orphan_queue,
    ));

    TestEnv {
        buckets,
        default_private_id,
        default_public_id,
        service,
        repo,
        db,
        _servers: servers,
    }
}

fn make_backend(
    s3: &TestS3Server,
    bucket: &str,
    id: BackendId,
    default_private: bool,
    default_public: bool,
    caps: &[&'static str],
) -> SharedBackend {
    let descriptor = BackendDescriptor {
        sdk: Backend {
            id,
            default_private,
            default_public,
            capabilities: caps.iter().map(|s| (*s).to_string()).collect::<Vec<CapabilityTag>>(),
            max_file_size_bytes: None,
            max_metadata_bytes: None,
            max_presign_ttl_seconds: Some(3_600),
        },
        max_signed_url_ttl_seconds_value: 3_600,
        tenant_access: vec![],
    };
    Arc::new(S3Backend::new(S3BackendConfig {
        descriptor,
        endpoint: s3.endpoint.clone(),
        region: REGION.to_string(),
        bucket: bucket.to_string(),
        access_key: s3.access_key.clone(),
        secret_key: s3.secret_key.clone(),
    }))
}

// ── Multipart helpers ───────────────────────────────────────────────────────

/// PUT bytes to a single presigned URL and return the ETag the server
/// reports. The S3 ETag header is double-quoted (`"abcd..."`); the
/// quotes are stripped so callers can pass the value straight into
/// `UploadedPart`.
pub async fn put_part(url: &str, body: Vec<u8>) -> String {
    let client = reqwest::Client::new();
    let resp = client
        .put(url)
        .body(body)
        .send()
        .await
        .expect("PUT presigned URL");
    assert!(
        resp.status().is_success(),
        "PUT failed: {} body={:?}",
        resp.status(),
        resp.text().await.unwrap_or_default()
    );
    resp.headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_string())
        .expect("server returned ETag")
}

/// GET a presigned download URL, return body bytes (decoded).
pub async fn get_url(url: &str) -> Vec<u8> {
    reqwest::get(url)
        .await
        .expect("GET presigned URL")
        .error_for_status()
        .expect("GET status")
        .bytes()
        .await
        .expect("body bytes")
        .to_vec()
}

// ── Filesystem-level helpers (s3s-fs TempDir) ───────────────────────────────

/// Path on disk for a single-object key. s3s-fs lays out completed
/// objects as `<root>/<bucket>/<key>`, so this is just
/// `bucket_handle.root_path.join(object_key)`.
pub fn object_path(env: &TestEnv, bucket_key: &str, object_key: &str) -> PathBuf {
    let h = env
        .buckets
        .get(bucket_key)
        .unwrap_or_else(|| panic!("unknown bucket key `{bucket_key}`"));
    h.root_path.join(object_key)
}

/// `true` if the object's data file exists on disk.
pub fn object_exists(env: &TestEnv, bucket_key: &str, object_key: &str) -> bool {
    object_path(env, bucket_key, object_key).is_file()
}

/// Read the on-disk bytes for an object. Panics if the file does not
/// exist — call `object_exists` first when the absence is part of the
/// assertion.
pub async fn read_object_from_disk(
    env: &TestEnv,
    bucket_key: &str,
    object_key: &str,
) -> Vec<u8> {
    let p = object_path(env, bucket_key, object_key);
    tokio::fs::read(&p)
        .await
        .unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// SHA-256 of the on-disk bytes for an object.
pub async fn sha256_on_disk(
    env: &TestEnv,
    bucket_key: &str,
    object_key: &str,
) -> [u8; 32] {
    sha256_of(&read_object_from_disk(env, bucket_key, object_key).await)
}

/// SHA-256 of an in-memory byte slice, for comparing against the disk
/// read.
pub fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut a = [0u8; 32];
    a.copy_from_slice(&out);
    a
}

/// All object keys persisted under `<root>/<bucket>/`. Returns paths
/// relative to the bucket root (e.g. `f/<simple>`), recursing into
/// subdirectories so multi-level keys round-trip cleanly.
pub fn list_object_keys(env: &TestEnv, bucket_key: &str) -> Vec<String> {
    let h = &env.buckets[bucket_key];
    let mut out = Vec::new();
    walk_files(&h.root_path, &h.root_path, &mut out);
    out.sort();
    out
}

fn walk_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_files(root, &p, out);
        } else if p.is_file() {
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"));
            }
        }
    }
}

/// All entries — including hidden multipart staging — under
/// `<server_root>/`. Used by abort tests to assert that nothing is
/// left behind. Returns paths relative to the server root, sorted.
pub fn list_all_entries(env: &TestEnv, bucket_key: &str) -> Vec<String> {
    let h = &env.buckets[bucket_key];
    let mut out = Vec::new();
    walk_files(&h.server_root, &h.server_root, &mut out);
    out.sort();
    out
}

fn s3_client_for_bucket(env: &TestEnv, bucket_key: &str) -> S3Client {
    let h = &env.buckets[bucket_key];
    let creds = Credentials::new(
        h.access_key.clone(),
        h.secret_key.clone(),
        None,
        None,
        "test",
    );
    let cfg = S3Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(REGION))
        .endpoint_url(h.s3_endpoint.clone())
        .credentials_provider(SharedCredentialsProvider::new(creds))
        .force_path_style(true)
        .build();
    S3Client::from_conf(cfg)
}

/// User metadata mirrored on the S3 object (`x-amz-meta-*`), fetched
/// via a real HEAD through the AWS SDK. Empty map on a hit with no
/// metadata; panics on backend error.
pub async fn head_user_metadata(
    env: &TestEnv,
    bucket_key: &str,
    object_key: &str,
) -> BTreeMap<String, String> {
    let h = &env.buckets[bucket_key];
    let resp = s3_client_for_bucket(env, bucket_key)
        .head_object()
        .bucket(&h.bucket)
        .key(object_key)
        .send()
        .await
        .expect("head_object");
    resp.metadata()
        .cloned()
        .map(|m| m.into_iter().collect::<BTreeMap<_, _>>())
        .unwrap_or_default()
}

/// `(content_type, content_length, etag)` from a real HEAD. The ETag
/// is returned with surrounding quotes stripped, matching the
/// stable-ETag convention used by `complete_upload`.
pub async fn head_basics(
    env: &TestEnv,
    bucket_key: &str,
    object_key: &str,
) -> (String, u64, String) {
    let h = &env.buckets[bucket_key];
    let resp = s3_client_for_bucket(env, bucket_key)
        .head_object()
        .bucket(&h.bucket)
        .key(object_key)
        .send()
        .await
        .expect("head_object");
    let ct = resp.content_type().unwrap_or("").to_string();
    let cl = u64::try_from(resp.content_length().unwrap_or(0)).unwrap_or(0);
    let etag = resp
        .e_tag()
        .unwrap_or("")
        .trim_matches('"')
        .to_string();
    (ct, cl, etag)
}

/// Number of `CopyObject` requests the server hosting `bucket_key`
/// has accepted since `make_env` was called. Used by Race 11/12 to
/// assert wire-level CAS behaviour.
pub fn copy_object_count(env: &TestEnv, bucket_key: &str) -> u64 {
    env.buckets[bucket_key]
        .copy_object_counter
        .load(Ordering::Relaxed)
}

pub mod microchat;
