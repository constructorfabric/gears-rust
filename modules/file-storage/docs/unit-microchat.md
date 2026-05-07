# Microchat Test Module + Full-Stack E2E Plan

<!-- Created: 2026-05-07 -->

This document is the implementation plan for a **test-only microchat
module** that exercises `cf-file-storage` end-to-end through a real
S3 wire protocol (`s3s-fs` over a loopback ephemeral TCP port) and
validates correctness all the way down to the bytes that land on the
mocked S3 server's filesystem.

The plan is for **integration testing of `cf-file-storage` only**. The
microchat module is throw-away test infrastructure — it is **not** the
production `mini-chat` module, it has no REST surface, and it never
ships in any binary.

---

## 1 — Goals

1. Provide a realistic SDK consumer of `cf-file-storage`. Real client
   code calls `create_presigned_upload` → PUT bytes → `complete_upload`
   → `read_file` → `delete_file`, with all data flowing through the
   full Rust stack and a real S3 wire protocol underneath.
2. Validate the **full chain end-to-end**: domain validators →
   `FileStorageClient` (in-process via `LocalClient`) → `Service<R>` →
   `SeaOrmFilesRepository` against SQLite `:memory:` → `S3Backend` (real
   `aws-sdk-s3`) → SigV4-signed HTTP → `s3s-fs` → `tempfile::TempDir` on
   the test runner's filesystem.
3. Cover corner cases that DB-only race tests cannot — anything that
   only manifests on the wire (real composite ETag from
   `CompleteMultipartUpload`, real range responses, real `CopyObject`
   self-replace ETag rotation, real abort cleanup).
4. Verify that **bytes on disk match expectations** after every
   mutating operation. Every persisted lifecycle assertion is backed by
   reading the actual file from the s3s-fs `TempDir` and comparing
   `sha256`.
5. Run **fully in parallel under `cargo nextest`**, no shared state, no
   `static`/`once_cell`, no fixed ports.

## 2 — Non-Goals

- No `cf-server` binary, no HTTP routing in `cf-file-storage` itself.
  The microchat consumes `Arc<dyn FileStorageClient>` directly.
- No `pytest`/Python E2E layer — that is the canonical E2E surface
  described in `docs/modkit_unified_system/13_e2e_testing.md`, but it
  requires a registered REST module (P2) and is out of scope here.
- No production `mini-chat` changes. The real mini-chat module is
  untouched.
- **No versioning capabilities.** `s3s-fs` does not implement S3
  versioning (no `PutBucketVersioning`, no `version_id` in responses).
  `download.s3.sigv4.versioned.v1` and `download.s3.public.versioned.v1`
  are deferred — current versioning behaviour stays covered by the
  existing repo-level race tests
  (`tests/repo_race_test.rs`) where `version_id` is fabricated in SQL.
- No `tower::Service` ↔ `aws-sdk-s3` in-memory bridge (`DuplexStream`).
  An ephemeral `127.0.0.1:0` TCP port is used instead — random,
  collision-free across parallel workers, invisible to the test author.

## 3 — Architectural Overview

### 3.1 — Crate layout

```
modules/file-storage/
├── microchat-test/                     # NEW — test-only crate
│   ├── Cargo.toml                      # publish = false, crate-type = lib
│   └── src/
│       ├── lib.rs                      # re-exports
│       ├── service.rs                  # Microchat struct + public API
│       ├── repo.rs                     # SeaORM repository for chat_attachments
│       ├── entity.rs                   # SeaORM entity for chat_attachments
│       ├── migration.rs                # MigrationTrait — single CREATE TABLE
│       ├── validators.rs               # mime / filename / quota
│       └── error.rs                    # MicrochatError enum
└── file-storage/
    ├── Cargo.toml                      # ADD microchat-test as dev-dependency
    └── tests/
        ├── e2e_common/mod.rs           # extended: 4 buckets, fs helpers
        ├── microchat_validators_test.rs
        ├── microchat_lifecycle_test.rs
        └── microchat_race_test.rs
```

### 3.2 — Wiring picture

```
                ┌──────────────────────────────────────────────────────┐
                │  test fn (#[tokio::test], current_thread, parallel)  │
                │                                                      │
                │  let env = make_microchat_env(EnvSpec::Both).await;  │
                │  env.microchat.attach(...).await                     │
                │      .complete(...).await                            │
                └─────────────────────┬────────────────────────────────┘
                                      │
                                      ▼
                ┌──────────────────────────────────────────────────────┐
                │  Microchat (microchat-test crate)                    │
                │  - validates mime / filename / quota                 │
                │  - delegates to fs: Arc<dyn FileStorageClient>       │
                │  - persists chat ↔ file_id link in its own SQLite    │
                └─────────────────────┬────────────────────────────────┘
                                      │
                                      ▼
                ┌──────────────────────────────────────────────────────┐
                │  LocalClient (cf-file-storage::domain::local_client) │
                │     wraps  Service<SeaOrmFilesRepository>            │
                └─────────────────────┬────────────────────────────────┘
                                      │
                                      ▼
                ┌──────────────────────────────────────────────────────┐
                │  Service<R>                                          │
                │  - 3-phase commit, validators, registry              │
                │  - calls S3Backend  +  SeaOrmFilesRepository         │
                └────────────┬─────────────────────────────┬───────────┘
                             │                             │
                             ▼                             ▼
              ┌──────────────────────────┐  ┌──────────────────────────┐
              │ S3Backend (aws-sdk-s3)   │  │ SeaOrmFilesRepository    │
              │  signs SigV4, multipart  │  │  on SQLite :memory:      │
              │  presign, copy, delete   │  │  (cf-file-storage)       │
              └────────────┬─────────────┘  └──────────────────────────┘
                           │ HTTP (loopback :ephemeral)
                           ▼
              ┌──────────────────────────────────────────────┐
              │  hyper accept-loop (spawned per test)        │
              │  s3s::S3Service { auth: SimpleAuth }         │
              │  s3s_fs::FileSystem { root: TempDir }        │
              └──────────────────────────────────────────────┘
                           │
                           ▼
              ┌──────────────────────────────────────────────┐
              │  TempDir (per test, dropped on test exit)    │
              │  └─ bkt-priv-nov/  bkt-pub-nov/              │
              │      └─ f/<simple_uuid>     ← bytes here     │
              └──────────────────────────────────────────────┘
```

### 3.3 — Why a test-only crate, not just a module under `tests/`?

A `tests/` module is an extra binary per file; sharing rich code
(SeaORM entity, migration runner, repository) becomes painful. A
crate gives us:
- Reusable `pub` API across all three test files
- Its own dependencies (SeaORM, migration trait) without polluting
  `cf-file-storage`'s production graph
- A clean unit boundary — the microchat is a small, real consumer of
  `cf-file-storage-sdk`, mirroring how a real downstream module would
  shape its code

## 4 — Microchat Module Detail

### 4.1 — `Microchat` public API (`microchat-test/src/service.rs`)

```rust
pub struct Microchat {
    fs: Arc<dyn FileStorageClient>,
    db: Db,                         // modkit_db::secure::Db
    repo: Arc<MicrochatRepo>,
    limits: MicrochatLimits,
}

#[derive(Clone, Debug)]
pub struct MicrochatLimits {
    pub max_files_per_user: u32,    // default 5
    pub allowed_mimes: Vec<&'static str>, // default = MIME_ALLOWLIST
    pub max_filename_len: usize,    // default 255
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachHandle {
    pub file_id: FileId,
    pub upload_id: String,
    pub part_urls: Vec<String>,
    pub expires_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub chat_id: Uuid,
    pub file_id: FileId,
    pub owner_id: Uuid,
    pub name: String,
    pub mime: String,
    pub status: AttachmentStatus,
    pub etag: Option<Etag>,
    pub size_bytes: Option<u64>,
    pub created_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentStatus { Pending, Active, Deleted }

impl Microchat {
    pub fn new(
        fs: Arc<dyn FileStorageClient>,
        db: Db,
        limits: MicrochatLimits,
    ) -> Self;

    pub async fn attach(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        owner_id: Uuid,
        meta: FileMeta,
        part_count: u32,
    ) -> Result<AttachHandle, MicrochatError>;

    pub async fn complete(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        file_id: FileId,
        upload_id: &str,
        parts: Vec<UploadedPart>,
    ) -> Result<Attachment, MicrochatError>;

    pub async fn abort(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        file_id: FileId,
        upload_id: &str,
    ) -> Result<(), MicrochatError>;

    pub async fn list(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
    ) -> Result<Vec<Attachment>, MicrochatError>;

    pub async fn read(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        file_id: FileId,
        range: Option<ByteRange>,
    ) -> Result<FileReadHandle, MicrochatError>;

    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<(), MicrochatError>;

    pub async fn presign_download(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        file_id: FileId,
        capability: &CapabilityTag,
    ) -> Result<PresignedDownload, MicrochatError>;
}
```

### 4.2 — Validators (`validators.rs`)

```rust
/// Allowlist enforced by the microchat. Real production would be
/// configurable; for tests it is a fixed set covering common cases.
pub const MIME_ALLOWLIST: &[&str] = &[
    "application/pdf",
    "image/png",
    "image/jpeg",
    "text/plain",
    "text/csv",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
];

pub fn validate_mime(mime: &str, limits: &MicrochatLimits) -> Result<(), MicrochatError>;
pub fn validate_filename(name: &str, limits: &MicrochatLimits) -> Result<(), MicrochatError>;
```

**`validate_mime` rejects** anything not in `limits.allowed_mimes`
(case-insensitive compare on the bare type, params stripped).

**`validate_filename` rejects:**
- empty string
- length > `limits.max_filename_len`
- contains `'/'` or `'\\'`  (path traversal)
- contains `".."` segment
- starts or ends with whitespace
- contains any control character (`c.is_control()`)

**Quota** (`enforce_quota`) is implemented in the repository, not in
this module, since it requires a `COUNT(*)` query:
```rust
SELECT COUNT(*) FROM chat_attachments
WHERE owner_id = ? AND status IN ('pending', 'active')
```

### 4.3 — Errors (`error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum MicrochatError {
    #[error("mime type not allowed: {0}")]
    MimeNotAllowed(String),

    #[error("invalid filename: {0}")]
    InvalidFilename(&'static str),

    #[error("quota exceeded: max {max} files per user")]
    QuotaExceeded { max: u32 },

    #[error("attachment not found")]
    NotFound,

    #[error("file storage: {0}")]
    FileStorage(#[from] FileStorageError),

    #[error("database: {0}")]
    Database(String),
}
```

### 4.4 — `chat_attachments` schema

```sql
CREATE TABLE chat_attachments (
    file_id     BLOB NOT NULL PRIMARY KEY,   -- UUID, file_id from FS
    chat_id     BLOB NOT NULL,
    owner_id    BLOB NOT NULL,
    name        TEXT NOT NULL,
    mime        TEXT NOT NULL,
    status      TEXT NOT NULL CHECK(status IN ('pending','active','deleted')),
    etag        TEXT,                        -- NULL until complete
    size_bytes  INTEGER,                     -- NULL until complete
    created_at  TEXT NOT NULL                -- RFC3339 UTC
);
CREATE INDEX idx_chat_attachments_owner_status
    ON chat_attachments (owner_id, status);
CREATE INDEX idx_chat_attachments_chat
    ON chat_attachments (chat_id);
```

Migration is a single `Migrator` impl; runs against the same `Db`
handle that holds `cf-file-storage`'s tables. Two namespaces, one
SQLite — keeps the harness simple. **No FK to `cf-file-storage`'s
`files` table** — the microchat keeps its own state and is allowed
to drift if the FS row is gone (mirrors real-world distributed
ownership of file metadata).

### 4.5 — Repository (`repo.rs`)

```rust
pub struct MicrochatRepo;

impl MicrochatRepo {
    pub async fn insert_pending(&self, conn: &DbConn<'_>, ...) -> Result<(), DbError>;
    pub async fn mark_active(&self, conn, file_id, etag, size) -> Result<(), DbError>;
    pub async fn mark_deleted(&self, conn, file_id) -> Result<(), DbError>;
    pub async fn delete_row(&self, conn, file_id) -> Result<(), DbError>;
    pub async fn count_active_for_owner(&self, conn, owner_id) -> Result<u32, DbError>;
    pub async fn find(&self, conn, file_id) -> Result<Option<Attachment>, DbError>;
    pub async fn list_by_chat(&self, conn, chat_id) -> Result<Vec<Attachment>, DbError>;
}
```

All queries scoped via `Db.conn()` (no `secure_*` extensions —
microchat doesn't need tenant scoping for tests). Status transitions
are unconditional UPDATEs: the test owns its data and there is no
concurrent writer outside the test itself.

### 4.6 — `attach` flow (illustrative)

```rust
async fn attach(...) -> Result<AttachHandle, MicrochatError> {
    validate_mime(&meta.mime_type, &self.limits)?;
    validate_filename(&meta.name, &self.limits)?;

    let conn = self.db.conn().map_err(|e| MicrochatError::Database(e.to_string()))?;
    let active = self.repo.count_active_for_owner(&conn, owner_id).await?;
    if active >= self.limits.max_files_per_user {
        return Err(MicrochatError::QuotaExceeded { max: self.limits.max_files_per_user });
    }

    let cap: CapabilityTag = "upload.s3.multipart.sigv4.v1".into();
    let handle = self.fs.create_presigned_upload(
        ctx, None, None,
        OwnerRef { tenant_id: ctx.subject_tenant_id(), owner_id },
        meta.clone(), &cap, part_count, UrlParams::default(),
    ).await?;

    self.repo.insert_pending(&conn, handle.file_id, chat_id, owner_id,
        &meta.name, &meta.mime_type, time::OffsetDateTime::now_utc()).await?;

    Ok(AttachHandle { file_id: handle.file_id, upload_id: handle.upload_id,
                      part_urls: handle.part_urls, expires_at: handle.expires_at })
}
```

## 5 — Harness Detail

### 5.1 — Existing pieces (already implemented, tests passing)

- `start_s3_server() → TestS3Server` — binds `127.0.0.1:0`, spawns
  hyper accept-loop with `s3s::S3Service` over `s3s_fs::FileSystem`
  rooted at a `tempfile::TempDir`. `Drop` sends shutdown signal.
- `aws_client(s3) → S3Client` — convenience client for harness-side
  `create_bucket` / inspection.
- `test_db() → Db` — SQLite `:memory:` with `cf-file-storage`
  migrations applied.
- `make_enforcer()` — `PolicyEnforcer::new(Arc<AllowAllAuthZ>)`.
- `make_ctx(tenant_id) → SecurityContext`.
- `make_env(EnvSpec) → TestEnv` — builds `Service<R>` with one or
  two backends pointing at the live s3s server.
- `put_part(url, body) → ETag` — real reqwest PUT to a presigned URL.
- `get_url(url) → Vec<u8>` — real reqwest GET.

### 5.2 — Extensions for this plan

#### 5.2.1 — Multi-bucket env

```rust
pub struct EnvSpec {
    pub buckets: Vec<BucketSpec>,
}

pub struct BucketSpec {
    pub key: &'static str,                   // logical name in TestEnv
    pub default_private: bool,
    pub default_public: bool,
    pub capabilities: Vec<&'static str>,
}

impl EnvSpec {
    /// Two-quadrant standard env for microchat tests.
    pub fn priv_pub() -> Self {
        EnvSpec { buckets: vec![
            BucketSpec {
                key: "priv",
                default_private: true, default_public: false,
                capabilities: vec![
                    "upload.s3.multipart.sigv4.v1",
                    "download.s3.sigv4.v1",
                ],
            },
            BucketSpec {
                key: "pub",
                default_private: false, default_public: true,
                capabilities: vec![
                    "upload.s3.multipart.sigv4.v1",
                    "download.s3.public.v1",
                ],
            },
        ]}
    }
}

pub struct TestEnv {
    pub s3: TestS3Server,
    pub buckets: HashMap<&'static str, BucketHandle>,
    pub default_private_id: BackendId,
    pub default_public_id: Option<BackendId>,
    pub service: Arc<Service<SeaOrmFilesRepository>>,
    pub repo: Arc<SeaOrmFilesRepository>,
    pub db: Db,
}

pub struct BucketHandle {
    pub bucket: String,                      // actual S3 bucket name
    pub backend_id: BackendId,
    pub root_path: PathBuf,                  // <s3s root>/<bucket>
}
```

Each `BucketHandle.bucket` is a fresh `format!("bkt-{}-{}", spec.key,
Uuid::new_v4().simple())` so even within one process collisions are
impossible.

#### 5.2.2 — Microchat env

```rust
pub struct MicrochatEnv {
    pub fs_env: TestEnv,                     // same TestEnv as above
    pub microchat: Microchat,
    pub fs_client: Arc<dyn FileStorageClient>,
}

pub async fn make_microchat_env(spec: EnvSpec) -> MicrochatEnv {
    let fs_env = make_env(spec).await;
    // LocalClient wraps the same Service<R> the test owns.
    let local: Arc<dyn FileStorageClient> =
        Arc::new(LocalClient::new(fs_env.service.clone()));
    // Run microchat migration on the same Db handle.
    run_microchat_migration(&fs_env.db).await;
    let microchat = Microchat::new(local.clone(), fs_env.db.clone(), MicrochatLimits::default());
    MicrochatEnv { fs_env, microchat, fs_client: local }
}
```

#### 5.2.3 — Filesystem-level helpers

```rust
/// Path on disk for a single-object key (post-complete).
/// s3s-fs lays out objects as `<root>/<bucket>/<key>`.
pub fn object_path(env: &TestEnv, bucket_key: &str, object_key: &str) -> PathBuf;

/// True if the object's data file exists.
pub fn object_exists(env: &TestEnv, bucket_key: &str, object_key: &str) -> bool;

/// Read the on-disk bytes for an object (panics if missing).
pub async fn read_object_from_disk(
    env: &TestEnv, bucket_key: &str, object_key: &str,
) -> Vec<u8>;

/// SHA-256 of bytes on disk for an object.
pub async fn sha256_on_disk(
    env: &TestEnv, bucket_key: &str, object_key: &str,
) -> [u8; 32];

/// SHA-256 of a byte slice — for comparing against the disk read.
pub fn sha256_of(bytes: &[u8]) -> [u8; 32];

/// All object keys actually persisted under <root>/<bucket>/.
/// Returns paths relative to the bucket root (so `f/<simple>`).
pub fn list_object_keys(env: &TestEnv, bucket_key: &str) -> Vec<String>;

/// All entries — including hidden multipart staging — under <root>/<bucket>/.
/// Used by abort tests to assert that nothing is left behind.
pub fn list_all_entries(env: &TestEnv, bucket_key: &str) -> Vec<String>;

/// Returns the user-metadata mirrored on the S3 object (`x-amz-meta-*`),
/// fetched via a real HEAD request through the AWS SDK. Used by tests
/// that assert the metadata update path actually rotated the metadata
/// on the wire, not just in DB.
pub async fn head_user_metadata(
    env: &TestEnv, bucket_key: &str, object_key: &str,
) -> std::collections::BTreeMap<String, String>;

/// Returns (content_type, content_length, etag) from a real HEAD.
pub async fn head_basics(
    env: &TestEnv, bucket_key: &str, object_key: &str,
) -> (String, u64, String);

/// Counts how many `CopyObject` requests have hit a particular bucket
/// since the env was created. Used by Race-test 11 to prove that the
/// losing writer aborted before touching S3.
///
/// Implemented as a `tower::Layer` wrapped around the s3s service that
/// inspects request paths — no s3s-fs internals are touched.
pub fn copy_object_count(env: &TestEnv, bucket_key: &str) -> u64;
```

> **s3s-fs internal layout note**: s3s-fs writes completed objects
> directly at `<root>/<bucket>/<key>`. Multipart parts during an
> in-progress upload are staged under a hidden namespace (the exact
> path is treated as an implementation detail and asserted only via
> `list_all_entries`, which collects everything). The first task in
> harness work is to confirm the layout by writing a tiny exploratory
> test and printing the directory tree.

### 5.3 — Parallelism guarantees

| Resource | Per-test | Shared |
|---|---|---|
| `tempfile::TempDir` | yes | no |
| `TcpListener` (`127.0.0.1:0`) | yes | no |
| `s3s::S3Service` | yes | no |
| SQLite `:memory:` | yes (own connection) | no |
| `Service<R>` | yes | no |
| Microchat | yes | no |

No `static`, no `OnceLock`, no `lazy_static` in any test or harness
helper. `cargo nextest` runs each `#[tokio::test(flavor = "current_thread")]`
in its own thread; harness uses `tokio::spawn` for the accept-loop
which inherits the test's runtime — when the test ends and `TempDir`
+ `_shutdown_tx` drop, the spawned accept-loop exits cleanly.

`flavor = "current_thread"` is chosen deliberately: it forces a single
work-stealing scheduler per test, which avoids any cross-test runtime
sharing and keeps drop semantics predictable.

## 6 — Test Suites

### 6.1 — `microchat_validators_test.rs` (synchronous, no S3)

These tests do not need S3 or even the `Service<R>` — they exercise
microchat's pure validators and the SQLite-backed quota check.

| # | Name | Asserts |
|---|---|---|
| 1 | `mime_allowlist_accepts_each_known_mime` | each MIME in `MIME_ALLOWLIST` returns `Ok` |
| 2 | `mime_rejects_unknown_application_octet_stream` | `application/octet-stream` → `MimeNotAllowed` |
| 3 | `mime_rejects_html` | `text/html` → `MimeNotAllowed` |
| 4 | `mime_strips_params_before_compare` | `text/plain; charset=utf-8` → `Ok` |
| 5 | `filename_rejects_empty` | `""` → `InvalidFilename` |
| 6 | `filename_rejects_too_long` | 256 chars → `InvalidFilename` |
| 7 | `filename_rejects_path_traversal` | `"../etc/passwd"` → `InvalidFilename` |
| 8 | `filename_rejects_forward_slash` | `"a/b.pdf"` → `InvalidFilename` |
| 9 | `filename_rejects_backslash` | `"a\\b.pdf"` → `InvalidFilename` |
| 10 | `filename_rejects_control_char` | `"foo\x07.pdf"` → `InvalidFilename` |
| 11 | `filename_rejects_leading_whitespace` | `" doc.pdf"` → `InvalidFilename` |
| 12 | `filename_accepts_unicode_letters` | `"отчёт.pdf"` → `Ok` |
| 13 | `quota_under_limit_passes` | 4 active rows, attach #5 → `Ok` |
| 14 | `quota_at_limit_rejects` | 5 active rows, attach #6 → `QuotaExceeded` |
| 15 | `quota_counts_pending_against_limit` | 5 pending → 6th → `QuotaExceeded` |
| 16 | `quota_does_not_count_deleted` | 5 deleted + 1 active → 7th attach (pending=1, active=1) actually ok up to the limit (verifies that deleted is excluded) |

Each test uses `make_microchat_env(EnvSpec::priv_pub()).await` for
tests #13-16; tests #1-12 can use a stand-alone constructor that
skips s3s startup (faster) — `Microchat::new_for_validators_only()`
returns a Microchat with a stub `FileStorageClient` that panics if
called. This keeps validator tests under 5 ms each.

### 6.2 — `microchat_lifecycle_test.rs` (real wire, real disk)

Each test owns one `MicrochatEnv` (private + public bucket). Test
data uses **deterministic** bytes from a seeded PRNG so SHA-256 is
reproducible across runs.

#### 6.2.1 — Payload contract: every file is exactly 3 KiB, unique

| Property | Value |
|---|---|
| Size | exactly `3 * 1024` bytes (3 KiB) |
| Content | random ASCII text from `[a-zA-Z0-9]` (printable, easy to inspect) |
| Uniqueness | every test that creates a file uses a distinct PRNG seed; the same test never reuses a seed across two attachments |

```rust
/// Generates exactly `3 * 1024` bytes of printable ASCII text from a
/// reproducible PRNG seed. Every file in the test suite is generated
/// this way — same size, distinct contents.
pub fn payload_3kib(seed: u64) -> Vec<u8> {
    use rand::{Rng, SeedableRng, rngs::StdRng};
    const ALPHA: &[u8] =
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = StdRng::seed_from_u64(seed);
    (0..3 * 1024).map(|_| ALPHA[rng.gen_range(0..ALPHA.len())]).collect()
}
```

**Rationale for 3 KiB**: well under any backend size limit; small
enough that whole-file reads in tests are instant; large enough to
fit a non-trivial multi-part upload (3 parts of 1 KiB each) and to
cover a non-trivial range read (`bytes=1000-1999`, 1 KiB inside a
3 KiB body).

**Seed registry** (every distinct file in the suite gets a unique
seed; reusing a seed is a test bug):

| Test | Variable | Seed |
|---|---|---|
| Lifecycle 1 (`private_full_lifecycle`) | `body` | `0xC0DE_0001` |
| Lifecycle 2 (`public_lifecycle`) | `body` | `0xC0DE_0002` |
| Lifecycle 3 (`multi_part_upload`) — part 1 | `p1` | `0xC0DE_0011` |
| Lifecycle 3 — part 2 | `p2` | `0xC0DE_0012` |
| Lifecycle 3 — part 3 | `p3` | `0xC0DE_0013` |
| Lifecycle 4 (`presign_urls_batch`) — file A | `body_a` | `0xC0DE_0021` |
| Lifecycle 4 — file B | `body_b` | `0xC0DE_0022` |
| Race 1 (`concurrent_complete`) | `body` | `0xC0DE_0101` |
| Race 2 (`variant_b_correct_etag`) — original / new | `orig` / `new` | `0xC0DE_0111` / `0xC0DE_0112` |
| Race 3 (`variant_b_stale_etag`) — original / attempted | `orig` / `attempt` | `0xC0DE_0121` / `0xC0DE_0122` |
| Race 4 (`delete_stale_etag`) | `body` | `0xC0DE_0131` |
| Race 5 (`delete_correct_etag`) | `body` | `0xC0DE_0141` |
| Race 6 (`abort_active_upload`) | `body` | `0xC0DE_0151` |
| Race 7 (`abort_after_complete`) | `body` | `0xC0DE_0161` |
| Race 8 (`quota_concurrent_attach`) — files 1-5 | `f1`..`f5` | `0xC0DE_0171`..`0xC0DE_0175` |
| Race 9 (`read_during_meta_update`) | `body` | `0xC0DE_0181` |
| Race 10 (`delete_then_attach`) — old / new | `old` / `new` | `0xC0DE_0191` / `0xC0DE_0192` |
| Race 11 (`concurrent_meta_update`) | `body` | `0xC0DE_0201` |
| Race 12 (`concurrent_meta_update_unpinned`) | `body` | `0xC0DE_0211` |
| Race 13 (`meta_update_during_delete`) | `body` | `0xC0DE_0221` |

**Test 1 — `private_full_lifecycle_disk_byte_exact`** (the long one,
covers most happy-path API methods plus full-stack metadata
verification)

```text
Setup:
  env  = make_microchat_env(EnvSpec::priv_pub())
  ctx  = make_ctx(tenant=T)
  chat = uuid()
  owner= uuid()
  body = payload_3kib(0xC0DE_0001)        # exactly 3 KiB
  body_sha = sha256_of(&body)
  meta = FileMeta {
    name="report.pdf",
    mime="application/pdf",
    gts="gts.cf.fstorage.file.type.v1~document",
    custom_metadata = {
      "origin": "lifecycle",
      "tenant_label": "alpha",
    },
  }

Steps & asserts:
1. attach → AttachHandle
   assert: handle.part_urls.len() == 1
   assert: object_exists(env, "priv", &derive_s3_key(handle.file_id)) == false
   assert: chat_attachments row { status="pending", name="report.pdf",
           mime="application/pdf", etag=NULL, size_bytes=NULL }

2. PUT body to handle.part_urls[0]  (real reqwest)
   → returns part_etag

3. complete(handle.file_id, handle.upload_id, [{1, part_etag}])
   → Attachment { status=Active, etag=Some(_), size_bytes=Some(3072) }

   # ── disk-level (filesystem of s3s-fs TempDir) ───────────────
   assert: object_exists(env, "priv", key) == true                      ←★
   assert: read_object_from_disk(env, "priv", key) == body              ←★
   assert: sha256_on_disk(env, "priv", key) == body_sha                 ←★
   assert: list_object_keys(env, "priv") == vec![key]                   ←★

   # ── microchat DB row (chat_attachments) ─────────────────────
   assert: row.status == "active"
   assert: row.name   == "report.pdf"
   assert: row.mime   == "application/pdf"
   assert: row.etag   == Attachment.etag
   assert: row.size_bytes == 3072

   # ── cf-file-storage DB row (files), via repo.find_by_id ─────
   assert: fs_row.status == FileStatus::Uploaded
   assert: fs_row.size_bytes == 3072
   assert: fs_row.etag == Attachment.etag
   assert: fs_row.meta.name == "report.pdf"
   assert: fs_row.meta.mime_type == "application/pdf"
   assert: fs_row.meta.gts_file_type == "gts.cf.fstorage.file.type.v1~document"
   assert: fs_row.meta.custom_metadata["origin"] == "lifecycle"
   assert: fs_row.meta.custom_metadata["tenant_label"] == "alpha"

   # ── service-level: get_file_info round-trip ─────────────────
   info = fs_client.get_file_info(ctx, file_id, None, None)
   assert: info.etag == Attachment.etag
   assert: info.size_bytes == 3072
   assert: info.meta == fs_row.meta                              # full equality

   # ── backend-level: HEAD against the real S3 wire ────────────
   head = aws_client(&env.fs_env.s3).head_object()
            .bucket(&env.fs_env.buckets["priv"].bucket)
            .key(key).send().await
   assert: head.content_type() == Some("application/pdf")        # mirrored
   assert: head.content_length() == Some(3072)
   assert: head.e_tag().trim_matches('"') == Attachment.etag
   assert: head.metadata()["origin"] == "lifecycle"              # x-amz-meta-*
   assert: head.metadata()["tenant_label"] == "alpha"
   assert: head.metadata()["name"] == "report.pdf"               # display-name hint
   assert: !head.metadata().contains_key("gts_file_type")        # DB-only, NOT mirrored

4. read_file (full)
   assert: collected_stream == body
   assert: sha256_of(&collected_stream) == body_sha

5. read_file (range Inclusive {1000, 1999})
   assert: collected.len() == 1000
   assert: collected == body[1000..2000]
   assert: handle.range == Some(ResolvedByteRange{start:1000, end_inclusive:1999, total:3072})

6. put_file_info (rename to "report-v2.pdf", set custom_metadata
                  to {"origin":"lifecycle","tenant_label":"alpha","rev":"2"})
   → Attachment.etag changes (new ETag from CopyObject self-replace)

   # ── disk: same bytes, fresh metadata ────────────────────────
   assert: object_exists(env, "priv", key) == true                      ←★
   assert: sha256_on_disk(env, "priv", key) == body_sha                 ←★ same bytes

   # ── microchat DB row: name + mime updated ───────────────────
   assert: row.name == "report-v2.pdf"
   assert: row.etag == new Attachment.etag                              # rotated

   # ── cf-file-storage DB row + S3 HEAD: metadata changed ──────
   info_v2 = fs_client.get_file_info(ctx, file_id, None, None)
   assert: info_v2.etag != Attachment.etag (from step 3)
   assert: info_v2.meta.name == "report-v2.pdf"
   assert: info_v2.meta.custom_metadata["rev"] == "2"
   assert: info_v2.meta.custom_metadata["origin"] == "lifecycle"        # preserved
   assert: info_v2.meta.gts_file_type == "gts.cf.fstorage.file.type.v1~document"  # immutable

   head_v2 = aws_client(...).head_object()...send()
   assert: head_v2.metadata()["rev"] == "2"
   assert: head_v2.metadata()["name"] == "report-v2.pdf"
   assert: head_v2.e_tag() != head.e_tag()                              # ETag rotated

7. presign_download (capability="download.s3.sigv4.v1")
   → PresignedDownload { url, is_public=false, expires_at }
   GET url with reqwest (no creds — relies on URL signature)
   assert: response.status == 200
   assert: response.body == body

8. delete (etag=Some(info_v2.etag), to exercise pinned delete)
   assert: object_exists(env, "priv", key) == false                     ←★
   assert: list_object_keys(env, "priv") == vec![]                      ←★
   assert: chat_attachments row { status="deleted" }
   assert: HEAD against backend → 404 (NoSuchKey)

9. read_file again → MicrochatError::NotFound
```

10 starred filesystem assertions; every mutating step verifies state
on **all four levels** in scope (disk, S3 HEAD, cf-file-storage DB
row, microchat DB row).

**Test 2 — `public_lifecycle_with_bare_url_download`**

Similar but uses the `pub` bucket and capability
`download.s3.public.v1`. Key check:

```text
4. presign_download (capability="download.s3.public.v1")
   → PresignedDownload { is_public=true }
   GET url with reqwest, NO auth at all
   assert: response.status == 200
   assert: response.body == body

   NOTE: s3s harness is started without auth in public-bucket tests
         (see §7) so a bare URL works — this matches how a real
         public bucket policy would behave.
```

**Test 3 — `multi_part_upload_round_trip`**

```text
Use 3-part upload (handle.part_urls.len() == 3).
Body: 3 KiB total — three distinct 1 KiB parts:
  p1 = payload_3kib(0xC0DE_0011)[..1024]   # first 1 KiB
  p2 = payload_3kib(0xC0DE_0012)[..1024]
  p3 = payload_3kib(0xC0DE_0013)[..1024]
  body = concat(p1, p2, p3)                # exactly 3 KiB

(s3s-fs does not enforce the AWS 5 MiB minimum for non-final parts,
 so 1 KiB parts are acceptable in this harness.)

PUT each part to its presigned URL, collect ETags, complete.
assert: object_exists("priv", key) == true                              ←★
assert: sha256_on_disk("priv", key) == sha256_of(&body)                 ←★
assert: read_object_from_disk("priv", key) == body                      ←★
assert: get_file_info(...).size_bytes == 3072
assert: HEAD content_length() == 3072
```

**Test 4 — `presign_urls_batch`**

```text
Two files, 3 KiB each, distinct contents:
  body_a = payload_3kib(0xC0DE_0021)
  body_b = payload_3kib(0xC0DE_0022)
  assert: body_a != body_b                                              # uniqueness

Attach + complete each separately, then call:
  fs_client.presign_urls(ctx, vec![item_a, item_b])

assert: outcomes.len() == 2
assert: outcomes[0].result.is_ok() && outcomes[1].result.is_ok()
GET both URLs in parallel.
assert: bytes_a == body_a && sha256(bytes_a) == sha256(body_a)
assert: bytes_b == body_b && sha256(bytes_b) == sha256(body_b)
assert: list_object_keys("priv").len() == 2                             ←★
```

### 6.3 — `microchat_race_test.rs` (real wire, races, abort, orphan)

Every race test pins a single seam and asserts that **disk state is
consistent with DB state**, not just that DB state is consistent
with itself.

| # | Name | Seam | Filesystem assertion |
|---|---|---|---|
| 1 | `concurrent_complete_one_winner` | `tokio::join!(complete, complete)` for same upload_id; one Applied, second Conflict | `list_object_keys` == 1 entry; sha256 matches the winning bytes |
| 2 | `variant_b_reupload_with_correct_etag_replaces_bytes` | row already in `uploaded`, attach with `file_id_input=Some(prev)`, PUT new body, complete | sha256 on disk == new body hash; old etag invalidated in DB |
| 3 | `variant_b_reupload_with_stale_etag_no_disk_change` | same as above but pass stale etag → `EtagMismatch` | sha256 on disk == old body hash (unchanged) |
| 4 | `delete_with_stale_etag_keeps_file` | uploaded row, `delete(etag=stale)` | sha256 on disk unchanged; DB still in `uploaded` |
| 5 | `delete_correct_etag_removes_file` | uploaded row, `delete(etag=correct)` | object_exists == false; DB row gone |
| 6 | `abort_active_upload_cleans_multipart_staging` | attach → 1 part PUT → abort BEFORE complete | `list_all_entries` returns no fragments under multipart staging; `list_object_keys` == empty |
| 7 | `abort_after_complete_is_noop` | attach → PUT → complete → abort with same upload_id | sha256 on disk unchanged; DB row still `uploaded` |
| 8 | `quota_concurrent_attach_one_winner` | owner has 4 active, two attaches in parallel | only one becomes pending, second → `QuotaExceeded`; `list_object_keys` count consistent (≤ 5) |
| 9 | `read_during_meta_update_no_torn_bytes` | uploaded row, `tokio::join!(read_full, put_file_info)`; reader either gets pre-update bytes or post-update bytes, never torn | `sha256_of(read_bytes) ∈ {body_sha}` (CopyObject self-replace doesn't change body) — proves we don't get a half-written file |
| 10 | `delete_then_attach_same_chat` | delete → attach → complete | sha256 on disk == new body hash; DB rows: old=deleted, new=active |
| 11 | `concurrent_meta_update_pinned_etag_one_winner` | uploaded row, `tokio::join!(put_file_info(etag=e0, name="A"), put_file_info(etag=e0, name="B"))` — both pin the same starting etag | exactly one returns `Ok` with rotated etag; loser → `EtagMismatch`; sha256 unchanged; HEAD x-amz-meta-name == winner's name |
| 12 | `concurrent_meta_update_unpinned_both_apply_lww` | uploaded row, two parallel `put_file_info(etag=None, name=…)` — neither pins | both succeed, both rotate etag; final HEAD matches **one** of the two writes (last-writer-wins via DB CAS on internal `(etag, updated_at)`); sha256 unchanged; chat_attachments row coherent |
| 13 | `meta_update_race_with_delete` | uploaded row, `tokio::join!(put_file_info(...), delete_file(etag=correct))` | one of: (a) put_file_info ok + delete fails with `EtagMismatch`/`Conflict`, file remains; or (b) delete ok + put_file_info fails with `Conflict`/`DeleteInProgress`/`NotFound`, file removed; **never both succeed**; disk state matches the chosen outcome |

> **Test 1 — concurrent complete** notes: the second complete may
> fail at the **DB** (begin_complete_upload returns NoMatch since the
> row is no longer in `pending_upload`) or at the **S3** (the parts
> from the first complete were consumed; the second
> `CompleteMultipartUpload` against the same upload_id returns
> `NoSuchUpload`). Either path must result in `Conflict` from the
> Service. The test asserts the resulting outcome is consistent and
> the on-disk state is the result of exactly one upload.

> **Test 9 — torn-bytes** is the most interesting one. It validates
> that `CopyObject` self-replace (the metadata update path) does not
> intermediate-state-leak the body. s3s-fs implements
> `CopyObject` atomically (atomic rename on the underlying FS), so
> this test should pass — but pinning it as an assertion guards
> future backend changes.

> **Test 11 — pinned concurrent meta** is the textbook ETag-CAS race.
> Both writers stage with `begin_meta_update(etag=e0)`. The first
> winning DB UPDATE rotates the row's etag to a transient
> `meta_updating` state; the second writer's CAS fails with NoMatch.
> The Service surfaces that as `EtagMismatch` (loser) and continues
> the winner's flow on S3 + finalize. The crucial property is
> **exactly one** S3 `CopyObject` is observed on the wire (proving
> the second writer aborted before touching S3). To verify, the test
> wraps the s3s-fs harness with a request counter that increments on
> every `CopyObject` against the test bucket — assert the counter is
> exactly 1 after both joins return.

> **Test 12 — unpinned LWW** documents the contrast. Both writers
> proceed and both touch S3 (counter == 2). The Service uses a
> non-pinned `begin_meta_update(etag=None)` which still uses CAS but
> against the row's `(status, updated_at)` tuple, not against
> `etag`. Whichever DB UPDATE lands second wins. Final state is
> deterministically that of the second writer; assertion is on the
> *consistency* between disk-side ETag (HEAD), DB row, and microchat
> row, not on which specific writer wins (which is a scheduler
> coin-flip).

> **Test 13 — meta vs delete** is the most safety-critical race. A
> partial outcome (DB says deleted, disk has old bytes; or DB says
> updated, disk has nothing) would be data corruption. The test
> proves the Service serializes the two via the file's status
> machine: only one of `meta_updating` and `deleting` can be entered
> from `uploaded` at a time. The asserter must check **all three
> outcomes simultaneously** (Service result, S3 HEAD, microchat row,
> cf-file-storage row) and reject any combination where they
> disagree.

### 6.4 — Hash-assertion discipline

Every test that expects bytes on disk asserts via:
```rust
let want = sha256_of(&expected);
let got  = sha256_on_disk(&env.fs_env, "priv", &derive_s3_key(file_id)).await;
assert_eq!(got, want, "bytes on disk diverge from expected");
```

Negative assertions (file should NOT exist) use:
```rust
assert!(!object_exists(&env.fs_env, "priv", &derive_s3_key(file_id)));
```

`list_object_keys` is used to assert the **complete state** of a
bucket — not just that the file we care about is there/gone, but
that no other unexpected entries appeared.

## 7 — Authentication strategy in s3s harness

The s3s `S3ServiceBuilder::set_auth(SimpleAuth::from_single(ak, sk))`
is **not** unconditionally enabled. It is enabled per-bucket-flavour:

| Bucket key | s3s auth | Rationale |
|---|---|---|
| `priv`     | enabled  | SigV4 must be valid for our S3Backend to issue presigned URLs that work. Wrong creds → 403. |
| `pub`      | disabled | `download.s3.public.v1` returns a bare URL with no signature. With auth on, s3s would refuse the bare GET. Disabling auth is the test-side equivalent of a real public bucket policy granting `s3:GetObject` to anonymous. |

The harness exposes:
```rust
pub struct AuthMode { pub require_sigv4: bool }
pub async fn start_s3_server_with(auth: AuthMode) -> TestS3Server;
```

For `priv_pub()` env we start **two separate s3s servers** — one
authenticated for the private bucket, one anonymous for the public
bucket. They live on different ephemeral ports. Each `S3Backend` is
configured with its own endpoint. Same TempDir? **No** — each server
gets its own `tempfile::tempdir()` (simpler, fully isolated; cost is
~1 ms per extra TempDir).

## 8 — Cargo dependency additions

### 8.1 — `microchat-test/Cargo.toml`

```toml
[package]
name = "cf-microchat-test"
version = "0.0.0"
publish = false
edition.workspace = true
license.workspace = true

[lib]
name = "microchat_test"

[dependencies]
file-storage-sdk = { package = "cf-file-storage-sdk", path = "../file-storage-sdk" }
modkit-db        = { workspace = true, features = ["sqlite"] }
modkit-security  = { workspace = true }
sea-orm          = { workspace = true }
sea-orm-migration = { workspace = true }
async-trait      = { workspace = true }
thiserror        = { workspace = true }
time             = { workspace = true }
tokio            = { workspace = true }
tracing          = { workspace = true }
uuid             = { workspace = true }
```

### 8.2 — `cf-file-storage/Cargo.toml` (additions only)

```toml
[dev-dependencies]
# ... existing entries ...
cf-microchat-test = { path = "../microchat-test" }
sha2              = { workspace = true }       # already in [dependencies] but
                                                # we use it from tests too
rand              = { workspace = true }
```

(`sha2` is already a non-dev dep on the host crate, but listing it in
dev-deps is a cargo non-issue since unification happens at the
features level.)

### 8.3 — Workspace `Cargo.toml`

Add `modules/file-storage/microchat-test` to `members`.

## 9 — Makefile target

```makefile
.PHONY: test-file-storage-e2e

## Run cf-file-storage e2e tests through the test-only microchat module
## (real s3s-fs over loopback ephemeral port, parallel-safe).
test-file-storage-e2e: install-tools
	cargo nextest run -p cf-file-storage \
	    --test microchat_validators_test \
	    --test microchat_lifecycle_test \
	    --test microchat_race_test
```

Lives next to `test-users-info-pg` in the `# -------- Tests --------`
section. **Not** added to `make ci` initially — first prove
zero-flake on local for ≥ 20 consecutive runs.

## 10 — Phasing (execution order)

| Phase | Deliverable | Estimate | Verifies |
|---|---|---|---|
| P1 | `microchat-test` crate skeleton (Cargo.toml, lib.rs, error, validators, migration, empty service stubs) | 1 h | `cargo build -p cf-microchat-test` |
| P2 | `validators.rs` + `repo.rs` + 16 validator tests | 2 h | `cargo nextest run -p cf-file-storage --test microchat_validators_test` 16 passes |
| P3 | Harness extensions: multi-bucket env, two s3s servers (auth/anon), `object_path`, `read_object_from_disk`, `sha256_on_disk`, `list_object_keys`, `list_all_entries`, `head_user_metadata`, `head_basics`, `copy_object_count` (tower::Layer counter); exploratory test that prints s3s-fs layout | 4 h | exploratory test prints expected layout; counter increments on a manual CopyObject smoke |
| P4 | `microchat-test/src/service.rs` full impl (attach/complete/abort/list/read/delete/presign_download) | 2 h | `cargo build -p cf-microchat-test` |
| P5 | `microchat_lifecycle_test.rs` — 4 tests, with full four-level state assertions (disk, HEAD, fs row, microchat row) and 3-KiB unique-payload contract | 4 h | all 4 pass with disk-hash + HEAD + DB asserts |
| P6 | `microchat_race_test.rs` — 13 tests (10 base + 3 concurrent-meta tests #11–13) | 5 h | all 13 pass; run `nextest run --test microchat_race_test --test-threads 8 --jobs 8 --fail-fast` 5 times in a row to confirm zero flakes; verify `copy_object_count` assertions work as expected |
| P7 | Makefile target + DOC update + register codebase paths in `.cypilot/config/artifacts.toml` if needed | 30 min | `make test-file-storage-e2e` green |

**Total estimate: ≈ 18 hours of focused work** (~2 working days).

## 11 — Open questions & risks

1. **s3s-fs multipart staging path** — confirmed empirically in P3.
   If staging objects live somewhere our `list_all_entries` can't
   reach (e.g. `/tmp` outside the TempDir), test #6 (`abort_active_
   upload_cleans_multipart_staging`) needs adjustment. Mitigation:
   first task of P3 is to check this.

2. **AWS SDK retries during shutdown** — when `TestS3Server` drops
   mid-test (e.g. test panics), in-flight requests may retry against
   a closed listener. We rely on `aws-sdk-s3` default retry policy
   surfacing as a clean error rather than hanging. If hangs appear,
   wrap presigned-PUT calls in `tokio::time::timeout(2s, ...)` (no
   `sleep`, just deadline) per `12_unit_testing.md` rules.

3. **Hash assertion clock for `put_file_info`** — `CopyObject`
   self-replace produces a new ETag; if s3s-fs implements it as
   a copy-and-rename rather than a metadata flip, there is a brief
   window where two files exist on disk. Test #6 of lifecycle
   (`put_file_info`) only asserts post-completion state, so this
   is fine — but documenting it here so we don't get surprised.

4. **Public-bucket auth-off mode** — disabling auth on s3s opens the
   server to *any* request from the test host. Since we bind to
   `127.0.0.1:0` and the port is OS-ephemeral, the practical attack
   surface is zero. Documented for reviewers.

5. **`s3s-fs` semantics drift** — s3s-fs is described by upstream as
   "experimental". Capturing the exact version in
   `Cargo.toml` (`s3s-fs = "0.13"`) and pinning the patch (no `*`)
   protects against silent behavior changes.

## 12 — Acceptance criteria

- [ ] `cargo build -p cf-microchat-test` clean
- [ ] `cargo build -p cf-file-storage --tests` clean
- [ ] `make test-file-storage-e2e` — 33 tests (16 validators +
      4 lifecycle + 13 race), all pass
- [ ] Suite runtime < 15 s on a developer laptop (per `13_e2e_testing.md`
      spirit, even though strictly speaking these are full-stack Rust
      integration tests, not HTTP E2E)
- [ ] 20 consecutive runs, zero flakes
- [ ] Every mutating test contains at least one **filesystem-level**
      hash or existence assertion against the s3s-fs TempDir
- [ ] Every file generated by a test is exactly 3 KiB; every file in
      the suite is byte-distinct (unique PRNG seed per attachment —
      enforced by the Seed Registry table in §6.2.1)
- [ ] Every mutating lifecycle test verifies state on all four
      levels: (a) on-disk bytes/sha256, (b) S3 HEAD response
      (content_type, content_length, x-amz-meta-*), (c)
      cf-file-storage `files` row via `repo.find_by_id`, (d)
      microchat `chat_attachments` row
- [ ] Concurrent meta-update tests (Race 11/12) assert wire-level
      `CopyObject` invocation count via the harness counter
      (exactly 1 for pinned, exactly 2 for unpinned)
- [ ] No `static`, no `OnceLock`, no shared mutable state
- [ ] No `tokio::time::sleep`, no polling loops, no `timeout` longer
      than 2 s
- [ ] No changes outside `modules/file-storage/` and `Makefile`
- [ ] No changes to production `cf-file-storage` source files (this
      is a test-side initiative; if a production change is needed,
      it goes in a separate PR)

---

**Status: PLANNED. Awaiting approval to start P1.**
