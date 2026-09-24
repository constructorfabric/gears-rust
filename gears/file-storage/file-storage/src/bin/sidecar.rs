//! `FileStorage` data-plane sidecar (`cpt-cf-file-storage-component-sidecar-gateway`,
//! `cpt-cf-file-storage-component-stream-proxy`).
//!
//! The sidecar is the only component that moves user bytes. It verifies the
//! control-minted Ed25519 signed-URL token, enforces the token's upload
//! constraints (size / hash), and streams content to/from a storage backend.
//! Clients never address a backend directly — the signed URL always points here.
//!
//! Configuration (env):
//!   - `FS_SIDECAR_ADDR`         — bind address (default `0.0.0.0:8087`)
//!   - `FS_SIDECAR_PUBLIC_KEY`   — base64url Ed25519 **primary** public key (from control)
//!   - `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` — optional comma-separated list of
//!     additional base64url Ed25519 public keys, tried in order *after* the
//!     primary. Unset/empty = none (single-key behaviour, unchanged from
//!     before this existed). This is what lets a control-plane
//!     `signing_key_seed` rotation happen without an outage or invalidating
//!     already-issued signed URLs: the sidecar fleet is rolled out first with
//!     the new key added to its set (as primary or previous — order only
//!     affects verification cost, not correctness), so tokens signed by
//!     either the old or the new key verify throughout the rollout. There is
//!     deliberately no `kid` claim selecting among these — the set stays
//!     small, so trying each key in turn is cheap. See
//!     `docs/operations.md`'s `signing_key_seed` → **Rotation** section for
//!     the full procedure.
//!   - `FS_SIDECAR_BACKEND_ROOT` — local-fs backend root (default `./.file-storage-data`)
//!   - `FS_SIDECAR_CONTROL_URL`  — base URL of the control plane (for finalize callback,
//!     default `http://localhost:8080`). When set to an empty string the callback is
//!     disabled (dev/test mode only).
//!   - `FS_SIDECAR_MAX_BODY_BYTES` — raises axum's blanket 2 MiB request-body floor
//!     (default `5_368_709_120`, i.e. 5 GiB). This is only a transport-layer ceiling;
//!     the real per-request limit is still enforced by the signed token's
//!     `claims.upload.max_size`/`exact_size`.
//!   - `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` — total wall-clock **budget** (seconds) for
//!     the sidecar→control-plane finalize/report-part callback, covering the entire
//!     retry loop (all attempts and delays between them), not a per-attempt timeout
//!     (default `10`).
//!   - `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` — connect timeout (seconds) for the
//!     same callbacks (default `5`). Together these bound how long a client's upload
//!     request can be held open by an unreachable or hung control plane.
//!   - `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` — maximum time (seconds) the sidecar
//!     will wait between two consecutive chunks of a client's request body (and
//!     for the first chunk) on the `upload` and `upload_multipart_part` routes
//!     (default `60`; `0` disables the guard entirely). This bounds only the
//!     *pause* between chunks, never the stream's total duration — a slow but
//!     steady multi-GiB upload always completes. It exists because the token's
//!     `exp` is checked exactly once, before any body bytes are read, and
//!     `FS_SIDECAR_MAX_BODY_BYTES` only bounds bytes, not time: without this, a
//!     client that opens the connection and then never sends (or stalls
//!     forever mid-stream) would hold the request open indefinitely (CWE-400).
//!     A client that goes idle past this timeout gets `408 Request Timeout`
//!     and any partial object is cleaned up the same way an ordinary broken
//!     stream is (see `idle_timeout_stream`'s doc comment).
//!   - `FS_SIDECAR_INTERNAL_TOKEN` — optional gear-local shared secret sent as the
//!     `x-fs-internal-token` header on BOTH the finalize and report-part
//!     control-plane callbacks. Unset/empty = the header is not sent, which is
//!     exactly what a control plane with `FileStorageConfig::finalize_internal_secret`
//!     unset expects. Must match the control plane's configured secret once it
//!     flips `require_finalize_internal_secret` on (see the migration-path note
//!     in `docs/ADR/0003-…-sidecar-data-plane.md`).
//!   - `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS` -- caps how many
//!     `upload_multipart_part` requests against a `multipart_native` backend
//!     (e.g. `S3Backend`) this sidecar processes at once (default `2`; must
//!     be at least `1` -- `0` fails sidecar startup rather than silently
//!     rejecting every part upload). Each such request buffers up to one
//!     whole part (`write_multipart_part_native`, bounded by `MAX_PART_SIZE`,
//!     currently 5 GiB, the same as `DEFAULT_MAX_BODY_BYTES`) in memory
//!     before writing it, since S3's `UploadPart` needs the whole part up
//!     front to sign and send. Without a cap on concurrent in-flight part
//!     uploads, N simultaneous large parts from ordinary authorized traffic
//!     (not an attack) can OOM the sidecar process; this bounds worst-case
//!     buffered memory from this path to exactly `N * MAX_PART_SIZE` --
//!     `2 * 5 GiB = 10 GiB` at the default -- so raising it is a direct,
//!     linear tradeoff against the sidecar's available memory, not a knob to
//!     turn without doing that arithmetic first. The non-native
//!     (offset-object, e.g. `LocalFsBackend`) write path streams each part
//!     straight to the backend without buffering it whole and is therefore
//!     NOT gated by this limiter at all -- see `write_multipart_part`'s doc
//!     comment. A request that cannot acquire a slot within
//!     `PART_UPLOAD_ACQUIRE_TIMEOUT` (200ms) gets `503` with `Retry-After`
//!     rather than queuing indefinitely -- see `upload_multipart_part`'s doc
//!     comment. At startup, `main` checks this worst case
//!     (`max_concurrent_part_uploads * MAX_PART_SIZE`) against a process
//!     memory limit read from the cgroup filesystem (v2 `memory.max`, falling
//!     back to v1 `memory.limit_in_bytes`): if a limit is known and the
//!     configured worst case exceeds it, the sidecar refuses to start rather
//!     than risk exactly the OOM this paragraph describes; if no limit can be
//!     read (not Linux, or unbounded), it only logs a warning with the
//!     computed worst case. See `check_part_buffer_budget`/
//!     `read_cgroup_memory_limit_bytes`.
//!   - `FS_SIDECAR_S3_BACKENDS` — an optional JSON array of
//!     `file_storage::config::S3BackendConfig` entries, e.g. a single entry
//!     `{"id":"s3-primary","endpoint":"http://127.0.0.1:9000","region":"us-east-1",
//!     "bucket":"my-bucket","access_key_id":"...","secret_access_key":"...","path_style":true}`
//!     wrapped in a JSON array. Unset or empty = no S3 backends. Credentials
//!     embedded in this env var are acceptable for the sidecar (it is the one
//!     component authorized to hold them, per ADR-0003's sidecar/control-plane
//!     split) but in production this JSON blob should be sourced from a
//!     secrets manager / mounted file where the deployment platform supports
//!     it. Each entry is validated at startup (a bad endpoint or missing
//!     credentials fails the sidecar fast) and, alongside the always-present
//!     `local-fs` backend, is folded into a `BackendRegistry`: every request
//!     resolves its backend from the verified token's `claims.backend_id`.
//!
//! ## Upload lifecycle
//!
//! After a successful single-part `PUT`, the sidecar:
//! 1. Publishes the blob to the backend, **create-exclusive**
//!    (`StorageBackend::publish_exclusive`): a fresh `backend_path` (a new
//!    version's canonical, never-before-used path) always lands; a second
//!    `PUT` to a path that already holds a published blob never overwrites
//!    it — closing a `PUT`-token-replay integrity gap (a signed upload
//!    token's signature never covers the body bytes and remains valid until
//!    `exp`, so without this guard a replay within the TTL could silently
//!    swap out already-served content).
//! 2. Posts a finalize callback to the control plane:
//!    `POST {control_url}/api/file-storage/v1/files/{file_id}/versions/{version_id}/finalize`
//!    carrying the signed upload token + the measured size+hash.
//! 3. Returns `200 OK` to the client only when the callback succeeds. A
//!    failed callback returns `502 Bad Gateway` and the client should retry
//!    — safe because step 1 is idempotent, never overwriting an
//!    already-published blob — see `upload`'s own doc comment for the exact
//!    retry/replay decision table.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::StreamExt;
use serde::Deserialize;
use time::OffsetDateTime;
use toolkit_utils::SecretString;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::MAX_PART_SIZE;
use file_storage::domain::ports::FileStorageMetricsPort;
use file_storage::infra::backend::{BackendRegistry, LocalFsBackend, S3Backend, StorageBackend};
use file_storage::infra::content::{hash, range};
use file_storage::infra::metrics::FileStorageMetricsMeter;
use file_storage::infra::signed_url::{
    Claims, Op, Verifier, dedupe_public_keys, parse_public_key_list,
};

/// Id of the local-fs backend, and the sidecar's `BackendRegistry` default id.
/// The default is never actually consulted by request dispatch (every request
/// names its backend explicitly via `claims.backend_id`), but
/// `BackendRegistry::new` requires a valid default id to construct at all.
const LOCAL_FS_ID: &str = "local-fs";

#[derive(Clone)]
struct SidecarState {
    verifier: Arc<Verifier>,
    /// Backends this sidecar can dispatch to, keyed by id. The backend used
    /// for a given request is resolved *per request* from the verified
    /// token's `claims.backend_id` — never a single hardcoded backend.
    backends: BackendRegistry,
    /// Base URL of the control plane, e.g. `http://localhost:8080`.
    /// Empty string = finalize callback disabled (dev/no-control-plane mode).
    control_base_url: String,
    /// Gear-local shared secret (`FS_SIDECAR_INTERNAL_TOKEN`) sent as
    /// `x-fs-internal-token` on the finalize/report-part callbacks. `None` =
    /// header not sent (matches a control plane with the check disabled).
    internal_token: Option<String>,
    http: reqwest::Client,
    /// Ingress/egress bytes and route/method/status/latency for the
    /// sidecar's own HTTP routes. The control plane's routes are already
    /// covered by the platform's api-gateway `http.server.request.duration`
    /// middleware; this process is never proxied by it, so it owns its own
    /// `OTel` `Meter` instance.
    metrics: Arc<dyn FileStorageMetricsPort>,
    /// Concurrency limiter for `upload_multipart_part` requests that take the
    /// `multipart_native` write path, sized by
    /// `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS`. Lives on `SidecarState`
    /// rather than as a process-wide `static` so that (a) `main()`'s
    /// fail-fast-parsed configured value is never silently shadowed by a
    /// lazily-initialized default racing ahead of it, and (b) two
    /// independently configured `SidecarState`s (e.g. two routers under test,
    /// or a future multi-listener deployment) can each carry their own limit
    /// instead of sharing one process-global choke point. See
    /// [`acquire_part_upload_slot`] for how a request acquires a permit from
    /// this field.
    part_upload_semaphore: Arc<tokio::sync::Semaphore>,
    /// Maximum pause allowed between two consecutive body chunks (and before
    /// the first one) on `upload`/`upload_multipart_part`, from
    /// `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` (default 60s; `None` when set to
    /// `0`, disabling the guard). Deliberately not an absolute deadline on
    /// the whole stream — see the module doc comment and
    /// [`idle_timeout_stream`].
    body_idle_timeout: Option<Duration>,
    /// Overall wall-clock budget for a sidecar→control-plane finalize/
    /// report-part callback, **including every retry attempt and the delays
    /// between them** — from `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` (default
    /// 10s). `post_with_retry` wraps its whole retry loop in a single
    /// `tokio::time::timeout` sized to this value, so a hung or unreachable
    /// control plane can never hold a client's upload request open for more
    /// than this long, regardless of `CALLBACK_MAX_ATTEMPTS`.
    callback_retry_budget: Duration,
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    #[serde(rename = "fs-token")]
    fs_token: Option<SecretString>,
}

/// Default value for `FS_SIDECAR_MAX_BODY_BYTES` (5 GiB) — comfortably above any
/// policy-permitted single-part upload. The real ceiling is still enforced
/// per-request by the signed token's `claims.upload.max_size`/`exact_size`;
/// this constant only bounds axum's blanket request-body floor (2 MiB default).
const DEFAULT_MAX_BODY_BYTES: usize = 5_368_709_120;

/// Default value for `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS` -- see the
/// module doc comment for the worst-case-memory arithmetic this default is
/// chosen against.
const DEFAULT_MAX_CONCURRENT_PART_UPLOADS: usize = 2;

/// Parse an optional environment variable's raw value (already fetched by
/// the caller, so this half is a pure function and unit-testable without
/// touching real process env) as `T`, falling back to `default` when unset
/// (`raw.is_none()`) — but failing fast when a value WAS supplied and
/// doesn't parse, mirroring `FS_SIDECAR_PUBLIC_KEY`'s "set but invalid ->
/// hard error at startup" treatment below. A silently swallowed parse
/// failure would turn a typo like `FS_SIDECAR_MAX_BODY_BYTES=5GB` into a
/// quiet fallback to the default instead of a loud misconfiguration.
fn parse_optional<T>(name: &str, raw: Option<String>, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match raw {
        Some(raw) => raw
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("invalid {name}={raw:?}: {e}")),
        None => Ok(default),
    }
}

/// Cgroup v1's "no limit configured" sentinel for `memory.limit_in_bytes` is
/// a page-aligned value near `i64::MAX` (commonly
/// `9223372036854771712`) rather than a real ceiling -- several orders of
/// magnitude above any real container memory limit (which tops out in the
/// TiB range at most). A v1 reading at or above this threshold is treated
/// the same as cgroup v2's explicit `"max"`: no usable limit.
const CGROUP_V1_UNLIMITED_THRESHOLD: u64 = 1 << 62;

/// Best-effort read of this process's memory limit from the cgroup
/// filesystem: cgroup v2's `memory.max` first, falling back to cgroup v1's
/// `memory.limit_in_bytes`. Returns `None` when no usable limit could be
/// determined -- not Linux, neither file exists, or the file reports "no
/// limit" (`"max"` on v2, or a reading at/above
/// [`CGROUP_V1_UNLIMITED_THRESHOLD`] on v1) -- so the caller falls back to a
/// warning instead of a hard startup failure; see
/// [`check_part_buffer_budget`].
fn read_cgroup_memory_limit_bytes() -> Option<u64> {
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let raw = raw.trim();
        return if raw == "max" {
            None
        } else {
            raw.parse::<u64>().ok()
        };
    }
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        && let Ok(limit) = raw.trim().parse::<u64>()
        && limit < CGROUP_V1_UNLIMITED_THRESHOLD
    {
        return Some(limit);
    }
    None
}

/// Checks the worst-case buffered memory footprint of native multipart part
/// uploads (`max_concurrent * part_size` -- see the module doc comment on
/// `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS`) against a process memory limit,
/// when one is known.
///
/// `limit: None` (no limit could be read, or the cgroup reports unlimited)
/// never fails the check -- the caller logs a warning instead, since this is
/// a best-effort guard, not an authoritative one (a limit enforced some
/// other way, e.g. by an orchestrator watching RSS out-of-process, is
/// invisible here). A pure function so the arithmetic is unit-testable
/// without a real cgroup filesystem; [`read_cgroup_memory_limit_bytes`]
/// handles the (untestable-in-CI) file reads.
fn check_part_buffer_budget(
    max_concurrent: usize,
    part_size: u64,
    limit: Option<u64>,
) -> Result<(), String> {
    let worst_case = (max_concurrent as u64).saturating_mul(part_size);
    match limit {
        Some(limit) if worst_case > limit => Err(format!(
            "worst-case native multipart part-buffer memory ({worst_case} bytes = \
             {max_concurrent} concurrent part(s) * {part_size} bytes) exceeds the detected \
             process memory limit ({limit} bytes); lower FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS \
             (currently {max_concurrent}) so that {max_concurrent} * {part_size} <= {limit}, or \
             raise the container's memory limit"
        )),
        _ => Ok(()),
    }
}

/// Parsed, validated sidecar configuration — the deterministic subset of
/// `main()`'s startup assembly: env-var parsing/defaults, key-rotation
/// dedup + `Verifier` construction, and the `0`-value validations, with no
/// I/O beyond the `lookup` closure itself (no socket bind, no `reqwest`
/// client, no cgroup read, no `S3` backend construction — those stay in
/// `main()`). Built by [`build_config`]; see that function's doc comment.
///
/// `Debug` is implemented manually (rather than derived) solely because
/// `Verifier` itself has no `Debug` impl (it holds `Arc<dyn
/// SignatureVerifier>` trait objects) -- every other field is printed as-is,
/// there is no secret here to redact (unlike, say,
/// `FileStorageConfig::signing_key_seed`).
struct SidecarConfig {
    addr: SocketAddr,
    root: String,
    /// Accepts the primary key plus any still-valid previous key — see
    /// [`dedupe_public_keys`] and [`Verifier::from_public_keys`].
    verifier: Arc<Verifier>,
    /// Number of keys `verifier` actually accepts (primary + deduped
    /// previous), for `main()`'s startup log line.
    accepted_key_count: usize,
    /// Number of `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` entries dropped as
    /// duplicates of an already-accepted key, for `main()`'s startup log line.
    dropped_duplicate_keys: usize,
    control_base_url: String,
    max_body_bytes: usize,
    finalize_timeout_secs: u64,
    finalize_connect_timeout_secs: u64,
    body_idle_timeout: Option<Duration>,
    max_concurrent_part_uploads: usize,
    internal_token: Option<String>,
}

impl std::fmt::Debug for SidecarConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarConfig")
            .field("addr", &self.addr)
            .field("root", &self.root)
            .field("verifier", &"Verifier { .. }")
            .field("accepted_key_count", &self.accepted_key_count)
            .field("dropped_duplicate_keys", &self.dropped_duplicate_keys)
            .field("control_base_url", &self.control_base_url)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("finalize_timeout_secs", &self.finalize_timeout_secs)
            .field(
                "finalize_connect_timeout_secs",
                &self.finalize_connect_timeout_secs,
            )
            .field("body_idle_timeout", &self.body_idle_timeout)
            .field(
                "max_concurrent_part_uploads",
                &self.max_concurrent_part_uploads,
            )
            .field("internal_token", &self.internal_token)
            .finish()
    }
}

/// Assemble [`SidecarConfig`] from environment-variable lookups, reached
/// exclusively through `lookup` rather than `std::env::var` directly — this
/// is what makes the whole function unit-testable without touching the real
/// process environment. `main()` calls it with `|name|
/// std::env::var(name).ok()`; tests pass an in-memory closure instead
/// (typically backed by a `HashMap`).
///
/// Fails fast — mirroring each constituent parse's own contract — on a
/// missing `FS_SIDECAR_PUBLIC_KEY`, a malformed key of either kind (bad
/// base64, or the wrong decoded length), a
/// `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS` of `0`, or any other value that
/// fails to parse.
fn build_config(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<SidecarConfig> {
    let addr: SocketAddr = lookup("FS_SIDECAR_ADDR")
        .unwrap_or_else(|| "0.0.0.0:8087".to_owned())
        .parse()?;
    let root =
        lookup("FS_SIDECAR_BACKEND_ROOT").unwrap_or_else(|| "./.file-storage-data".to_owned());
    let public_key_b64 = lookup("FS_SIDECAR_PUBLIC_KEY")
        .ok_or_else(|| anyhow::anyhow!("FS_SIDECAR_PUBLIC_KEY is required"))?;
    let public_key = URL_SAFE_NO_PAD
        .decode(public_key_b64.trim())
        .map_err(|e| anyhow::anyhow!("invalid FS_SIDECAR_PUBLIC_KEY: {e}"))?;

    // Keys retained from an earlier `signing_key_seed`, tried after the
    // primary during a rotation window (see `docs/operations.md`'s
    // `signing_key_seed` → Rotation section, and this module's doc comment).
    // Unset/empty = no previous keys, matching pre-rotation behaviour.
    let previous_public_keys: Vec<Vec<u8>> = match lookup("FS_SIDECAR_PREVIOUS_PUBLIC_KEYS") {
        Some(raw) if !raw.trim().is_empty() => parse_public_key_list(&raw)
            .map_err(|e| anyhow::anyhow!("invalid FS_SIDECAR_PREVIOUS_PUBLIC_KEYS: {e}"))?,
        _ => Vec::new(),
    };
    // Primary always leads the set (`Verifier::verify` tries keys in
    // order); a duplicate -- of the primary, or within `previous_public_keys`
    // itself -- is silently dropped rather than rejected: it's a harmless
    // no-op (see `dedupe_public_keys`'s doc) that would otherwise force
    // operators to scrub the list precisely at the moment a rotation
    // completes.
    let (verifier_keys, dropped_duplicate_keys) =
        dedupe_public_keys(public_key, previous_public_keys);
    let accepted_key_count = verifier_keys.len();
    let verifier = Arc::new(Verifier::from_public_keys(verifier_keys).map_err(|e| {
        anyhow::anyhow!("invalid FS_SIDECAR_PUBLIC_KEY/FS_SIDECAR_PREVIOUS_PUBLIC_KEYS: {e}")
    })?);

    // `FS_SIDECAR_CONTROL_URL` — base URL of the control-plane finalize endpoint.
    // An empty string disables the callback (useful for local dev or standalone tests).
    let control_base_url =
        lookup("FS_SIDECAR_CONTROL_URL").unwrap_or_else(|| "http://localhost:8080".to_owned());

    // Raises axum's blanket 2 MiB request-body floor. The real per-request
    // ceiling is still enforced by the signed token's
    // `claims.upload.max_size`/`exact_size` inside the handlers; this value
    // only needs to be large enough that no policy-permitted upload hits it.
    let max_body_bytes: usize = parse_optional(
        "FS_SIDECAR_MAX_BODY_BYTES",
        lookup("FS_SIDECAR_MAX_BODY_BYTES"),
        DEFAULT_MAX_BODY_BYTES,
    )?;

    // Bound how long the sidecar will wait on the control-plane
    // finalize/report-part callbacks — without these, a hung or unreachable
    // control plane could block the client's upload request indefinitely.
    // `finalize_timeout_secs` is the **overall** budget for the whole
    // retry loop (all attempts plus the delays between them), not a
    // per-attempt allowance -- see `SidecarState::callback_retry_budget` and
    // `post_with_retry`.
    let finalize_timeout_secs: u64 = parse_optional(
        "FS_SIDECAR_FINALIZE_TIMEOUT_SECS",
        lookup("FS_SIDECAR_FINALIZE_TIMEOUT_SECS"),
        10,
    )?;
    let finalize_connect_timeout_secs: u64 = parse_optional(
        "FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS",
        lookup("FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS"),
        5,
    )?;

    // Bound the pause between body chunks (and before the first one) on the
    // upload paths — see the module doc comment and `SidecarState::body_idle_timeout`.
    // `0` disables the guard entirely (`None`); this is a per-chunk idle bound,
    // never an absolute deadline on the whole stream.
    let body_idle_timeout_secs: u64 = parse_optional(
        "FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS",
        lookup("FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS"),
        60,
    )?;
    let body_idle_timeout = if body_idle_timeout_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(body_idle_timeout_secs))
    };

    // See the module doc comment and `DEFAULT_MAX_CONCURRENT_PART_UPLOADS` for
    // the memory rationale and worst-case arithmetic. `0` is rejected
    // explicitly below: `Semaphore::new(0)` would not panic, but it would
    // silently turn every `multipart_native` part-upload request into an
    // unconditional `503` -- a configuration mistake, not a legitimate
    // "disable part uploads" knob, so it must fail sidecar startup instead of
    // failing quietly at request time.
    let max_concurrent_part_uploads: usize = parse_optional(
        "FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS",
        lookup("FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS"),
        DEFAULT_MAX_CONCURRENT_PART_UPLOADS,
    )?;
    if max_concurrent_part_uploads == 0 {
        return Err(anyhow::anyhow!(
            "FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS=0 would reject every multipart part \
             upload with 503; unset it to use the default of \
             {DEFAULT_MAX_CONCURRENT_PART_UPLOADS} or set it to a value >= 1"
        ));
    }

    // Attached as `x-fs-internal-token` on both callbacks below. Unset/empty
    // = not sent.
    let internal_token = lookup("FS_SIDECAR_INTERNAL_TOKEN").filter(|s| !s.is_empty());

    Ok(SidecarConfig {
        addr,
        root,
        verifier,
        accepted_key_count,
        dropped_duplicate_keys,
        control_base_url,
        max_body_bytes,
        finalize_timeout_secs,
        finalize_connect_timeout_secs,
        body_idle_timeout,
        max_concurrent_part_uploads,
        internal_token,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = build_config(|name| std::env::var(name).ok())?;

    if config.dropped_duplicate_keys > 0 {
        // Key material itself is never logged -- only the count.
        tracing::warn!(
            dropped_duplicates = config.dropped_duplicate_keys,
            "FS_SIDECAR_PREVIOUS_PUBLIC_KEYS contains keys already in the accepted set; \
             dropped \u{2014} a completed rotation usually means the list should be cleared"
        );
    }
    tracing::info!(
        accepted_key_count = config.accepted_key_count,
        "sidecar signed-URL verifier configured"
    );

    if config.control_base_url.is_empty() {
        tracing::warn!(
            "FS_SIDECAR_CONTROL_URL is empty \u{2014} finalize callback disabled. \
             Uploaded versions will remain in 'pending' status."
        );
    } else {
        tracing::info!(
            control_base_url = %config.control_base_url,
            "sidecar finalize callback enabled"
        );
    }

    // Worst-case buffered memory for the native multipart part-upload path is
    // `max_concurrent_part_uploads * MAX_PART_SIZE` (module doc comment).
    // When a process memory limit can be read from the cgroup filesystem,
    // reject a configuration that would clearly exceed it outright rather
    // than let ordinary, non-adversarial concurrent large-part traffic OOM
    // the process later. When no limit could be read (not Linux, no cgroup
    // limit configured, or an unbounded one), this is only a warning: the
    // limit may still be enforced some other way (e.g. by an orchestrator
    // watching RSS out-of-process) that isn't visible here.
    let part_buffer_memory_limit = read_cgroup_memory_limit_bytes();
    check_part_buffer_budget(
        config.max_concurrent_part_uploads,
        MAX_PART_SIZE,
        part_buffer_memory_limit,
    )
    .map_err(|msg| anyhow::anyhow!(msg))?;
    if let Some(limit) = part_buffer_memory_limit {
        tracing::info!(
            max_concurrent_part_uploads = config.max_concurrent_part_uploads,
            max_part_size = MAX_PART_SIZE,
            memory_limit_bytes = limit,
            "native multipart part-buffer worst-case memory fits within the detected process \
             memory limit"
        );
    } else {
        tracing::warn!(
            max_concurrent_part_uploads = config.max_concurrent_part_uploads,
            max_part_size = MAX_PART_SIZE,
            worst_case_bytes =
                (config.max_concurrent_part_uploads as u64).saturating_mul(MAX_PART_SIZE),
            "could not detect a process memory limit (not Linux cgroups, or unlimited) -- \
             unable to verify FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS's worst-case native \
             multipart part-buffer memory footprint stays within it"
        );
    }

    // This per-request `.timeout()` is a backstop, not the primary bound any
    // more: `post_with_retry` wraps its whole retry loop (all attempts, not
    // just the first) in its own `tokio::time::timeout` sized to the same
    // `finalize_timeout_secs`, which always fires at or before this one once
    // any prior attempt has consumed part of the budget. Left in place so a
    // single request made directly against this client (bypassing
    // `post_with_retry`) still can't hang indefinitely.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.finalize_timeout_secs))
        .connect_timeout(Duration::from_secs(config.finalize_connect_timeout_secs))
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest client: {e}"))?;

    if config.internal_token.is_some() {
        tracing::info!(
            "sidecar configured with FS_SIDECAR_INTERNAL_TOKEN \u{2014} finalize/report-part \
             callbacks will carry x-fs-internal-token"
        );
    }

    // A JSON array of `S3BackendConfig` entries. Parsed and eagerly
    // constructed here (so a misconfigured entry, e.g. a bad endpoint URL or
    // missing credentials with no env fallback, fails sidecar startup fast).
    // Folded into the `BackendRegistry` below alongside `local-fs`, so
    // entries here are reachable by traffic via `claims.backend_id` dispatch.
    let s3_backends: Vec<Arc<dyn StorageBackend>> = match std::env::var("FS_SIDECAR_S3_BACKENDS") {
        Ok(json) if !json.trim().is_empty() => {
            let entries: Vec<file_storage::config::S3BackendConfig> =
                serde_json::from_str(&json)
                    .map_err(|e| anyhow::anyhow!("invalid FS_SIDECAR_S3_BACKENDS: {e}"))?;
            entries
                .iter()
                .map(|entry| {
                    S3Backend::from_config(entry)
                        .map(|backend| Arc::new(backend) as Arc<dyn StorageBackend>)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow::anyhow!("FS_SIDECAR_S3_BACKENDS: {e}"))?
        }
        _ => Vec::new(),
    };
    if !s3_backends.is_empty() {
        tracing::info!(
            count = s3_backends.len(),
            "sidecar parsed FS_SIDECAR_S3_BACKENDS \u{2014} registered for claims.backend_id dispatch"
        );
    }

    let mut backend_list: Vec<Arc<dyn StorageBackend>> =
        vec![Arc::new(LocalFsBackend::new(LOCAL_FS_ID, config.root))];
    backend_list.extend(s3_backends);
    let backends = BackendRegistry::new(backend_list, LOCAL_FS_ID)
        .map_err(|e| anyhow::anyhow!("failed to build sidecar backend registry: {e}"))?;

    // The sidecar is its own OS process, so it owns its own OTel `Meter` —
    // mirrors the control plane's `meter_with_scope` call in `gear.rs`,
    // scoped under the sidecar's own instrumentation name.
    let metrics_scope =
        opentelemetry::InstrumentationScope::builder("file-storage-sidecar".to_owned()).build();
    let metrics: Arc<dyn FileStorageMetricsPort> = Arc::new(FileStorageMetricsMeter::new(
        &opentelemetry::global::meter_with_scope(metrics_scope),
        "file_storage",
    ));

    let state = SidecarState {
        verifier: config.verifier,
        backends,
        control_base_url: config.control_base_url,
        internal_token: config.internal_token,
        http,
        metrics,
        // See `SidecarState::part_upload_semaphore`'s doc comment for why it
        // lives here rather than as a process-wide static.
        part_upload_semaphore: Arc::new(tokio::sync::Semaphore::new(
            config.max_concurrent_part_uploads,
        )),
        body_idle_timeout: config.body_idle_timeout,
        callback_retry_budget: Duration::from_secs(config.finalize_timeout_secs),
    };

    let app = build_router(state, config.max_body_bytes);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    tracing::info!(addr = %config.addr, "file-storage sidecar listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the sidecar's `Router` from a `SidecarState`, without binding a
/// socket. Factored out of `main()` so the `#[cfg(test)]` module can exercise
/// routes in-process via `Router::oneshot` (see `13_e2e_testing.md`'s
/// route-smoke pattern).
///
/// `max_body_bytes` raises axum's blanket 2 MiB request-body floor via
/// `DefaultBodyLimit` — the real per-request ceiling is still enforced inside
/// the handlers by the signed token's `claims.upload.max_size`/`exact_size`.
fn build_router(state: SidecarState, max_body_bytes: usize) -> Router {
    Router::new()
        .route(
            "/api/file-storage-data/v1/upload/{file_id}/{version_id}",
            put(upload),
        )
        // `.head(download_head)` overrides axum's default GET-derived HEAD
        // handling: without it, a HEAD request would run the full
        // `download` handler — including streaming the entire object off the
        // backend — only to discard the body afterwards. See
        // `download_head`'s doc comment.
        .route(
            "/api/file-storage-data/v1/download/{file_id}/{version_id}",
            get(download).head(download_head),
        )
        // Server-authoritative multipart part upload. The control plane
        // mints a `multipart_part` token for each part; the sidecar verifies
        // and enforces the exact `size` claim before writing.
        .route(
            "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/{part_number}",
            put(upload_multipart_part),
        )
        // Liveness probe: always 200 once the process is up and the router is
        // wired. No backend/dependency check — see `readyz` below for that.
        .route("/healthz", get(healthz))
        // Readiness probe: reflects real backend availability (e.g. an
        // unmounted local-fs root or an unreachable S3 endpoint) — see
        // `readyz`'s doc comment.
        .route("/readyz", get(readyz))
        // Route-level latency + status. Bound to its own state clone (not
        // the shared router state) via `from_fn_with_state` so it wraps every
        // route above regardless of extractor ordering.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            record_request_metrics,
        ))
        .with_state(state)
        .layer(DefaultBodyLimit::max(max_body_bytes))
}

/// Records one `file_storage_sidecar_request_duration_ms` observation per
/// request: route (from [`MatchedPath`], falling back to `"unmatched"` so
/// cardinality stays bounded), method, status, and latency. The control
/// plane's routes already get an equivalent metric for free from the
/// platform's api-gateway; this process is never proxied by it.
async fn record_request_metrics(
    State(state): State<SidecarState>,
    matched_path: Option<MatchedPath>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().as_str().to_owned();
    let route = matched_path
        .as_ref()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let start = std::time::Instant::now();
    let response = next.run(req).await;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    state
        .metrics
        .record_request(&route, &method, response.status().as_u16(), elapsed_ms);
    response
}

/// Liveness probe handler. Always returns `200 OK` with a trivial body once
/// the sidecar process is serving requests. Intentionally does not check
/// backend health — that is `readyz`'s job.
async fn healthz() -> &'static str {
    "ok"
}

/// Time budget for a single backend's readiness probe. Bounds how long an
/// unreachable/hung backend (e.g. a stalled S3 endpoint) can delay the whole
/// `/readyz` response — well under a typical k8s readiness-probe period
/// (~10s default).
const READYZ_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Readiness probe handler. Polls every configured backend's
/// [`StorageBackend::is_ready`] concurrently, each bounded by
/// `READYZ_PROBE_TIMEOUT`. Returns `200 "ready"` only when every backend
/// answers `Ok` within the timeout; otherwise `503`, naming only the failing
/// backend ids in the body (e.g. `"not ready: s3-primary"`) — never the
/// underlying error text, so a probe response can never leak backend
/// internals (transport details, credentials-adjacent error strings, etc.).
async fn readyz(State(state): State<SidecarState>) -> Response {
    let checks = state.backends.iter().map(|(id, backend)| {
        let id = id.to_owned();
        let backend = Arc::clone(backend);
        async move {
            match tokio::time::timeout(READYZ_PROBE_TIMEOUT, backend.is_ready()).await {
                Ok(Ok(())) => None,
                Ok(Err(_)) | Err(_) => Some(id),
            }
        }
    });

    let failing: Vec<String> = futures::future::join_all(checks)
        .await
        .into_iter()
        .flatten()
        .collect();

    if failing.is_empty() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("not ready: {}", failing.join(", ")),
        )
            .into_response()
    }
}

/// Extract the token from the `fs-token` query param and/or the `X-FS-Token`
/// header.
///
/// Either transport alone is accepted as before. When both are present and
/// agree, that shared value is used. When both are present and *disagree*,
/// this is a malformed request, not a query-wins/header-wins ambiguity to
/// silently resolve one way: a caller or intermediary that attached two
/// different credentials to the same request has a bug worth surfacing
/// rather than masking, so this is rejected with `400` before either token
/// is ever handed to the verifier. `Err` carries the ready-made response to
/// return: `400` for the conflicting-tokens case, `401` for the
/// neither-present case (unchanged from before this existed).
#[allow(clippy::result_large_err)]
fn extract_token(q: &TokenQuery, headers: &HeaderMap) -> Result<String, Response> {
    let query_token = q.fs_token.as_ref().map(|s| s.expose().to_owned());
    let header_token = headers
        .get("x-fs-token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    match (query_token, header_token) {
        (Some(q), Some(h)) if q == h => Ok(q),
        (Some(_), Some(_)) => Err((
            StatusCode::BAD_REQUEST,
            "conflicting fs-token in query and header",
        )
            .into_response()),
        (Some(t), None) | (None, Some(t)) => Ok(t),
        (None, None) => Err((StatusCode::UNAUTHORIZED, "missing fs-token").into_response()),
    }
}

/// Response text used for both the single-part `upload` and
/// `upload_multipart_part` routes when [`idle_timeout_stream`] fires.
const BODY_IDLE_TIMEOUT_MESSAGE: &str = "request body idle timeout";

/// Wrap a body byte-stream so that a pause of more than `idle` between two
/// consecutive chunks — or before the very first one — ends the stream with
/// `Err(io::Error::new(ErrorKind::TimedOut, ..))` and sets `timed_out` to
/// `true`, instead of leaving the read hanging forever (CWE-400: the token's
/// `exp` is only checked once, up front, and `FS_SIDECAR_MAX_BODY_BYTES`
/// bounds bytes, not time).
///
/// This is a *per-chunk idle* bound, never an absolute deadline on the
/// stream's total duration: a slow-but-steady multi-GiB upload that never
/// pauses longer than `idle` between chunks is expected to run to
/// completion no matter how long that takes in total. `idle == None`
/// disables the guard: the input stream is returned unwrapped (boxed) and
/// `timed_out` is never touched.
///
/// Once the timeout fires, the wrapped stream yields exactly that one `Err`
/// and then ends (`None`) — callers already treat any stream error as
/// terminal, so this just gives them a way (via `timed_out`) to tell "the
/// client/backend stream broke" apart from "the client went idle past the
/// deadline" and answer `408 Request Timeout` instead of the generic
/// error status. Every storage backend's `publish_exclusive`/`put_stream`
/// already cleans up a partial object on *any* stream error (see their own
/// doc comments — `LocalFsBackend` removes the temp file, `S3Backend` aborts
/// the multipart session it may have started, `InMemoryBackend` simply never
/// inserted anything), so an idle-timeout abort is cleaned up exactly like
/// any other broken upload — no special-casing needed there.
fn idle_timeout_stream<S>(
    stream: S,
    idle: Option<Duration>,
    timed_out: Arc<AtomicBool>,
) -> futures::stream::BoxStream<'static, std::io::Result<bytes::Bytes>>
where
    S: futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send + 'static,
{
    let Some(idle) = idle else {
        return Box::pin(stream);
    };
    Box::pin(futures::stream::unfold(
        (Box::pin(stream), false),
        move |(mut stream, done)| {
            let timed_out = Arc::clone(&timed_out);
            async move {
                if done {
                    return None;
                }
                match tokio::time::timeout(idle, stream.next()).await {
                    Ok(Some(item)) => Some((item, (stream, false))),
                    Ok(None) => None,
                    Err(_elapsed) => {
                        timed_out.store(true, Ordering::SeqCst);
                        Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                BODY_IDLE_TIMEOUT_MESSAGE,
                            )),
                            (stream, true),
                        ))
                    }
                }
            }
        },
    ))
}

/// If `timed_out` was set by [`idle_timeout_stream`] (the read stalled
/// longer than `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS`), logs one
/// `tracing::warn!` naming `context` (and `bytes_received` when the caller
/// has it cheaply on hand — see each call site) and returns the `408
/// Request Timeout` response callers should answer with, instead of their
/// normal backend/stream-error handling. Returns `None` when this wasn't an
/// idle timeout, so the caller falls through to its own error handling
/// unchanged.
///
/// Split out of the three call sites ([`upload`], `write_multipart_part_native`,
/// `write_multipart_part_offset_object`) purely to keep their cognitive
/// complexity under the crate's lint ceiling — the branch itself is
/// otherwise identical in all three.
fn idle_timeout_response(
    timed_out: &AtomicBool,
    context: &str,
    bytes_received: Option<u64>,
) -> Option<Response> {
    if !timed_out.load(Ordering::SeqCst) {
        return None;
    }
    if let Some(bytes_received) = bytes_received {
        tracing::warn!(
            context,
            bytes_received,
            "request body idle timeout \u{2014} answering 408"
        );
    } else {
        tracing::warn!(context, "request body idle timeout \u{2014} answering 408");
    }
    Some((StatusCode::REQUEST_TIMEOUT, BODY_IDLE_TIMEOUT_MESSAGE).into_response())
}

/// `PUT` upload: verify token (op=PUT), stream bytes straight to the backend.
///
/// The body is never buffered whole in this handler — it is converted to a
/// byte stream and handed to `StorageBackend::publish_exclusive`, which
/// writes + hashes chunks as they arrive and aborts mid-stream the moment
/// `claims.upload.max_size` is exceeded. `exact_size`/`expected_hash` can
/// only be checked once the stream is fully drained (the incremental
/// length/hash are only final at that point), so those checks run *after*
/// `publish_exclusive` returns.
///
/// `publish_exclusive` reports `created: false` instead of overwriting when
/// `claims.backend_path` already holds a blob. This handler's response for
/// that case is:
/// * finalize succeeds (the earlier publish landed but finalize never ran,
///   and this attempt's measured bytes match what's already stored) → `200`,
///   a benign retry has converged;
/// * anything else (finalize rejects because the version is already
///   `available` — a genuine replay — a finalize transport failure, or no
///   control plane configured at all) → `409 Conflict`. The one fact that is
///   always true in the `!created` branch is that *this* `PUT` did not take
///   effect, so `409` is reported even when the underlying finalize failure
///   was transport-level rather than a logical conflict — the alternative
///   (a `502`) would wrongly suggest the bytes might have been stored.
async fn upload(
    State(state): State<SidecarState>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match extract_token(&q, &headers) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let claims = match state.verifier.verify(&token, OffsetDateTime::now_utc()) {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    if claims.op != Op::Put || claims.file_id != file_id || claims.version_id != version_id {
        return (
            StatusCode::FORBIDDEN,
            "token does not authorize this operation",
        )
            .into_response();
    }

    let backend = match state.backends.get(&claims.backend_id) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unknown backend '{}': {e}", claims.backend_id),
            )
                .into_response();
        }
    };

    let timed_out = Arc::new(AtomicBool::new(false));
    let byte_stream = idle_timeout_stream(
        body.into_data_stream()
            .map(|r| r.map_err(std::io::Error::other)),
        state.body_idle_timeout,
        Arc::clone(&timed_out),
    );
    let outcome = match backend
        .publish_exclusive(&claims.backend_path, byte_stream, claims.upload.max_size)
        .await
    {
        Ok(v) => v,
        // `publish_exclusive`'s only `Validation` error is the mid-stream
        // `max_size` guard — every other failure is a genuine backend error.
        Err(DomainError::Validation { .. }) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "exceeds max_size").into_response();
        }
        Err(e) => {
            if let Some(resp) = idle_timeout_response(&timed_out, &claims.backend_path, None) {
                return resp;
            }
            tracing::error!(error = %e, "backend publish_exclusive failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    };
    let (bytes_written, digest, created) = (outcome.bytes_written, outcome.digest, outcome.created);

    // Enforce the remaining upload constraints now that the streamed
    // length/hash are final.
    if claims
        .upload
        .exact_size
        .is_some_and(|exact| bytes_written != exact)
    {
        return reject_upload_bad_content(
            backend.as_ref(),
            &claims,
            created,
            "size does not match exact_size",
        )
        .await;
    }
    if let Some(expected) = &claims.upload.expected_hash {
        let got = format!("{}:{}", hash::ALGORITHM, hex::encode(digest));
        if !expected.eq_ignore_ascii_case(&got) {
            return reject_upload_bad_content(
                backend.as_ref(),
                &claims,
                created,
                "content hash mismatch",
            )
            .await;
        }
    }

    let size = i64::try_from(bytes_written).unwrap_or(i64::MAX);
    let hash_hex = hex::encode(digest);

    // The sidecar is the only component that ever sees content bytes, so
    // this is the sole place to record ingress.
    #[allow(clippy::cast_precision_loss)]
    state.metrics.record_ingress_bytes(bytes_written as f64);

    // Finalize callback: notify the control plane that bytes have landed so it
    // can mark the version `available`. The same signed token proves this was
    // a pre-authorized upload (DESIGN §bind-service). `claims.request_id` is
    // echoed back as `x-request-id` so both planes' logs for this upload can
    // be correlated.
    let finalize_result = finalize_with_control_plane(
        &state,
        &token,
        &claims.request_id,
        file_id,
        version_id,
        size,
        &hash_hex,
    )
    .await;

    if !created {
        // Immutability guard: `publish_exclusive` refused to write because
        // `claims.backend_path` already held a blob — either an earlier
        // successful PUT for this same upload (finalize may or may not have
        // run yet), or a PUT-token replay after the version was already
        // finalized/bound. The live bytes on the backend were NOT touched
        // either way. The finalize call above was still attempted with
        // *this* attempt's measured size/hash: if the earlier publish
        // landed but finalize never ran, this is a benign retry and
        // finalize's own read-back-and-compare converges it to success
        // without ever re-touching the backend object; any other outcome
        // (finalize already ran, a transport failure, or no control plane
        // configured) is reported as `409` rather than `502` — see
        // `upload`'s doc comment for the full decision table.
        //
        // Why reporting *this* attempt's digest here cannot poison metadata:
        // finalize never trusts the size/hash the sidecar reports, in this
        // `!created` case or any other. `finalize_upload_by_token`
        // (`domain/service/write.rs`) independently re-reads the actual
        // stored blob at `version.backend_path` and recomputes both size and
        // hash from those bytes, then rejects the request if that recomputed
        // pair doesn't match what was reported. So there are exactly two
        // possible outcomes here, and both are safe: this retry's bytes are
        // identical to what's already published (the common case) and
        // finalize succeeds against the one real object on disk; or this
        // retry's bytes differ (an adversarial or corrupted replay) and
        // finalize's re-verification rejects it, leaving the version exactly
        // as it already was. In neither case does the sidecar's
        // self-reported digest get persisted unverified — `created: false`
        // only ever changes what gets *asserted* to finalize, never what
        // finalize actually *trusts*.
        return match finalize_result {
            Err(_) => (
                StatusCode::CONFLICT,
                "content already published for this version",
            )
                .into_response(),
            Ok(_) if state.control_base_url.is_empty() => (
                StatusCode::CONFLICT,
                "content already published for this version",
            )
                .into_response(),
            Ok(echo) => uploaded_response(&echo),
        };
    }

    match finalize_result {
        Err(resp) => resp,
        Ok(echo) => uploaded_response(&echo),
    }
}

/// Reject an upload whose streamed bytes failed the post-publish
/// `exact_size`/`expected_hash` check with `400 Bad Request`, first cleaning
/// up the object `publish_exclusive` just wrote **iff this request is the
/// one that created it** (`created == true`).
///
/// # Why the `created` gate matters
/// `publish_exclusive` runs *before* these constraints can be checked (the
/// streamed length/hash are only final once the whole body has been read —
/// see `upload`'s own doc comment), so a validation failure here can mean one
/// of two very different things:
/// * `created == true`: this call's own bytes just landed at
///   `claims.backend_path` and immediately failed validation. Left in place,
///   that object would permanently poison the version's immutable path —
///   `publish_exclusive` never overwrites an existing object (the whole
///   point of the replay-`PUT` fix), so a corrected retry with the *right*
///   bytes would itself get `created: false` and be rejected as a conflict,
///   with no way to ever land the correct content short of the orphan-sweep
///   reclaiming the path (an hour, by default). Deleting it here —
///   best-effort, `tracing::warn!` on failure rather than failing the
///   response — lets an immediate corrected retry succeed instead of waiting
///   out that reclaim window.
/// * `created == false`: some *other* request (an earlier successful PUT, or
///   a concurrent one that landed first) already owns whatever object
///   currently lives at that path. This request's own bytes were never
///   written anywhere — `publish_exclusive` measured them in memory/on a temp
///   file and then discarded them without touching the destination — so there
///   is nothing of *this* request's to clean up, and deleting the live object
///   would destroy content this request has no claim to (and no evidence is
///   even wrong: the mismatch is between *this* replay's bytes and the
///   claims, not necessarily between the stored object and the claims).
async fn reject_upload_bad_content(
    backend: &dyn StorageBackend,
    claims: &Claims,
    created: bool,
    reason: &'static str,
) -> Response {
    if created && let Err(e) = backend.delete(&claims.backend_path).await {
        tracing::warn!(
            error = %e,
            backend_path = %claims.backend_path,
            "failed to clean up freshly-published object after post-validation failure; \
             path will stay poisoned until orphan reconciliation reclaims it"
        );
    }
    (StatusCode::BAD_REQUEST, reason).into_response()
}

/// Build the sidecar's `200 uploaded` response, echoing the control plane's
/// auto-bind outcome as `X-FS-Bound` / `ETag` headers when the finalize
/// response carried them.
fn uploaded_response(echo: &FinalizeEcho) -> Response {
    let mut resp = (StatusCode::OK, "uploaded").into_response();
    if let Some(bound) = &echo.bound
        && let Ok(v) = HeaderValue::from_str(bound)
    {
        resp.headers_mut().insert("x-fs-bound", v);
    }
    if let Some(etag) = &echo.etag
        && let Ok(v) = HeaderValue::from_str(etag)
    {
        resp.headers_mut().insert(header::ETAG, v);
    }
    if let Some(cur) = &echo.current_etag
        && let Ok(v) = HeaderValue::from_str(cur)
    {
        resp.headers_mut().insert("x-fs-current-etag", v);
    }
    resp
}

/// Build the finalize request body bytes (JSON `{size, hash_hex}`).
///
/// Returns an internal-error `Response` (boxed) if JSON serialization fails,
/// which is only possible if `serde_json` itself has a bug (our value is trivial).
#[allow(clippy::result_large_err)]
fn finalize_body(size: i64, hash_hex: &str) -> Result<Vec<u8>, Response> {
    let body = serde_json::json!({ "size": size, "hash_hex": hash_hex });
    serde_json::to_vec(&body).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize finalize request body");
        (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
    })
}

/// Auto-bind outcome echoed by the control plane's finalize response: the
/// `x-fs-bound` / `etag` response headers set by `handlers::finalize_version`
/// when the upload token carried `bind_on_finalize`. The sidecar copies them
/// verbatim onto its own `200` `PUT` response (as `X-FS-Bound` / `ETag`) so
/// the uploading client learns the bind outcome without any extra request.
/// Both `None` for tokens that did not request a bind (manual mode).
#[derive(Debug, Default, Clone)]
struct FinalizeEcho {
    bound: Option<String>,
    etag: Option<String>,
    current_etag: Option<String>,
}

/// Interpret the HTTP response from the control-plane finalize call.
async fn interpret_finalize_response(
    resp: reqwest::Response,
    file_id: Uuid,
    version_id: Uuid,
) -> Result<FinalizeEcho, Response> {
    if resp.status().is_success() {
        tracing::debug!(%file_id, %version_id, "finalize callback succeeded");
        let hdr = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        return Ok(FinalizeEcho {
            bound: hdr("x-fs-bound"),
            etag: hdr("etag"),
            current_etag: hdr("x-fs-current-etag"),
        });
    }
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    tracing::error!(
        %file_id, %version_id,
        http_status = %status,
        body = %body_text,
        "control-plane finalize callback returned error"
    );
    // The detailed status/body stay in the server-side log above — forwarding
    // them to the client would leak the control plane's raw error body
    // (which can carry internal details) to an uploading client.
    Err((StatusCode::BAD_GATEWAY, "finalize failed").into_response())
}

/// Maximum number of attempts (including the first) for a sidecar→control-plane
/// callback POST (finalize or report-part). Only transport-level failures
/// (`reqwest::Error::is_connect()` / `is_timeout()`) are retried; a
/// successful-but-error HTTP status is a real 4xx/5xx from the control plane
/// and is returned immediately by the caller's response interpretation.
const CALLBACK_MAX_ATTEMPTS: u32 = 3;

/// Fixed delay between callback retry attempts. Short enough that even the
/// maximum number of attempts adds well under a second to the test suite's
/// wall-clock budget.
const CALLBACK_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Error returned by [`post_with_retry`]: either a transport failure from an
/// attempt that actually ran, or the overall retry budget (`retry_budget`,
/// [`SidecarState::callback_retry_budget`]) running out before any attempt
/// returned a response. Callers treat both identically (log and answer
/// `502`) — this only exists so the budget case gets an honest message
/// instead of being stretched over an unrelated [`reqwest::Error`] variant.
#[derive(Debug)]
enum CallbackError {
    Transport(reqwest::Error),
    BudgetExceeded(Duration),
}

impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "{e}"),
            Self::BudgetExceeded(budget) => write!(
                f,
                "control-plane callback retry budget ({:.3}s) exceeded before any attempt \
                 completed",
                budget.as_secs_f64()
            ),
        }
    }
}

impl std::error::Error for CallbackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(e) => Some(e),
            Self::BudgetExceeded(_) => None,
        }
    }
}

/// POST `body_bytes` to `url` under the sidecar's callback retry policy: up to
/// `CALLBACK_MAX_ATTEMPTS` attempts total, retrying only on a transport
/// connect/timeout failure, with `CALLBACK_RETRY_DELAY` between attempts.
/// Shared by `finalize_with_control_plane` and `report_part_with_control_plane`
/// so both callbacks get the same bounded-retry behavior.
///
/// The entire retry loop -- every attempt plus every delay between them -- is
/// wrapped in a single `tokio::time::timeout(retry_budget, ..)`, so the total
/// wall-clock time this function can take is bounded by `retry_budget`
/// regardless of `CALLBACK_MAX_ATTEMPTS`: an attempt that is already in
/// flight when the budget runs out is cancelled immediately rather than
/// allowed to run to its own (per-attempt) client timeout. `retry_budget` is
/// normally `SidecarState::callback_retry_budget`, i.e.
/// `FS_SIDECAR_FINALIZE_TIMEOUT_SECS`.
///
/// `internal_token` (`SidecarState::internal_token`) is attached as
/// `x-fs-internal-token` when present; `None` omits the header entirely
/// (works against a control plane with the check disabled).
async fn post_with_retry(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    request_id: &str,
    internal_token: Option<&str>,
    body_bytes: &[u8],
    retry_budget: Duration,
) -> Result<reqwest::Response, CallbackError> {
    use tokio_retry::RetryIf;
    use tokio_retry::strategy::FixedInterval;

    let mut attempt: u32 = 0;
    let action = || {
        attempt += 1;
        let this_attempt = attempt;
        let mut req = http
            .post(url)
            .header("content-type", "application/json")
            .header("x-fs-token", token);
        // Propagate the signed URL's correlation id so the control plane's
        // finalize/report-part log lines can be joined with this sidecar's
        // own logs for the same upload.
        if !request_id.is_empty() {
            req = req.header("x-request-id", request_id);
        }
        if let Some(internal_token) = internal_token {
            req = req.header("x-fs-internal-token", internal_token);
        }
        let fut = req.body(body_bytes.to_vec()).send();
        async move {
            let result = fut.await;
            if let Err(ref e) = result
                && this_attempt < CALLBACK_MAX_ATTEMPTS
                && (e.is_connect() || e.is_timeout())
            {
                tracing::warn!(
                    attempt = this_attempt,
                    error = %e,
                    "control-plane callback transport error, retrying"
                );
            }
            result
        }
    };
    // Retry only transport connect/timeout failures; a real HTTP status is
    // returned to the caller unchanged. `CALLBACK_MAX_ATTEMPTS` includes the
    // initial attempt, so the schedule carries one fewer delay.
    let retryable = |e: &reqwest::Error| e.is_connect() || e.is_timeout();
    let strategy =
        FixedInterval::new(CALLBACK_RETRY_DELAY).take((CALLBACK_MAX_ATTEMPTS - 1) as usize);
    // The whole loop -- not just its first iteration -- is inside this single
    // timeout, so an attempt that's already in flight when `retry_budget`
    // elapses is cancelled on the spot: dropping the in-progress `send()`
    // future here is safe (no partial write to clean up -- the request body
    // is a small, already-materialized JSON buffer, and `reqwest` tears down
    // the underlying connection on drop).
    match tokio::time::timeout(retry_budget, RetryIf::start(strategy, action, retryable)).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => Err(CallbackError::Transport(e)),
        Err(_elapsed) => Err(CallbackError::BudgetExceeded(retry_budget)),
    }
}

/// Call the control-plane finalize endpoint after a successful PUT.
///
/// Returns `Ok(())` when the control plane accepted the finalize, or
/// `Err(Response)` with a `502 Bad Gateway` response when the callback
/// fails (so the upload handler can surface the failure to the client).
///
/// When `control_base_url` is empty, the callback is skipped (dev mode).
async fn finalize_with_control_plane(
    state: &SidecarState,
    token: &str,
    request_id: &str,
    file_id: Uuid,
    version_id: Uuid,
    size: i64,
    hash_hex: &str,
) -> Result<FinalizeEcho, Response> {
    if state.control_base_url.is_empty() {
        return Ok(FinalizeEcho::default());
    }

    let url = format!(
        "{}/api/file-storage/v1/files/{}/versions/{}/finalize",
        state.control_base_url.trim_end_matches('/'),
        file_id,
        version_id,
    );

    let body_bytes = finalize_body(size, hash_hex)?;

    match post_with_retry(
        &state.http,
        &url,
        token,
        request_id,
        state.internal_token.as_deref(),
        &body_bytes,
        state.callback_retry_budget,
    )
    .await
    {
        Ok(resp) => interpret_finalize_response(resp, file_id, version_id).await,
        Err(e) => {
            tracing::error!(
                %file_id, %version_id, error = %e,
                "control-plane finalize callback failed"
            );
            // `e` (a `reqwest::Error`) embeds the request URL, i.e. the
            // internal `FS_SIDECAR_CONTROL_URL` host:port — never forward it
            // to the client. The detail is already in the log above.
            Err((StatusCode::BAD_GATEWAY, "finalize failed").into_response())
        }
    }
}

/// Build the report-part request body bytes (JSON `{backend_etag, hash_hex, size}`).
///
/// Returns an internal-error `Response` (boxed) if JSON serialization fails,
/// which is only possible if `serde_json` itself has a bug (our value is trivial).
#[allow(clippy::result_large_err)]
fn report_part_body(backend_etag: &str, hash_hex: &str, size: i64) -> Result<Vec<u8>, Response> {
    let body = serde_json::json!({
        "backend_etag": backend_etag,
        "hash_hex": hash_hex,
        "size": size,
    });
    serde_json::to_vec(&body).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize report-part request body");
        (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
    })
}

/// Interpret the HTTP response from the control-plane report-part call.
async fn interpret_report_part_response(
    resp: reqwest::Response,
    upload_id: Uuid,
    part_number: u32,
) -> Result<(), Response> {
    if resp.status().is_success() {
        tracing::debug!(%upload_id, part_number, "report-part callback succeeded");
        return Ok(());
    }
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    tracing::error!(
        %upload_id, part_number,
        http_status = %status,
        body = %body_text,
        "control-plane report-part callback returned error"
    );
    // Same no-leak principle as `interpret_finalize_response` — the detailed
    // status/body stay server-side only.
    Err((StatusCode::BAD_GATEWAY, "report failed").into_response())
}

/// Call the control-plane report-part endpoint after a successful part write.
///
/// This is the sidecar half of the "report part" callback: without it,
/// nothing ever populates `multipart_upload_parts`, so
/// `complete_multipart_upload`'s `list_multipart_parts` is structurally empty
/// in a real deployment. Mirrors `finalize_with_control_plane`'s contract:
/// returns `Ok(())` when the control plane accepted the report, or
/// `Err(Response)` with a `502 Bad Gateway` when the callback fails (the
/// client should retry — the part write and this report are both idempotent
/// per `(upload_id, part_number)`).
///
/// When `control_base_url` is empty, the callback is skipped (dev mode).
#[allow(clippy::too_many_arguments)]
async fn report_part_with_control_plane(
    state: &SidecarState,
    token: &str,
    request_id: &str,
    file_id: Uuid,
    version_id: Uuid,
    upload_id: Uuid,
    part_number: u32,
    backend_etag: &str,
    hash_hex: &str,
    size: i64,
) -> Result<(), Response> {
    if state.control_base_url.is_empty() {
        return Ok(());
    }

    let url = format!(
        "{}/api/file-storage/v1/files/{}/versions/{}/multipart/{}/parts/{}/report",
        state.control_base_url.trim_end_matches('/'),
        file_id,
        version_id,
        upload_id,
        part_number,
    );

    let body_bytes = report_part_body(backend_etag, hash_hex, size)?;

    match post_with_retry(
        &state.http,
        &url,
        token,
        request_id,
        state.internal_token.as_deref(),
        &body_bytes,
        state.callback_retry_budget,
    )
    .await
    {
        Ok(resp) => interpret_report_part_response(resp, upload_id, part_number).await,
        Err(e) => {
            tracing::error!(
                %file_id, %version_id, %upload_id, part_number, error = %e,
                "control-plane report-part callback failed"
            );
            // Same no-leak principle as `finalize_with_control_plane` — `e`
            // embeds the internal control-plane URL.
            Err((StatusCode::BAD_GATEWAY, "report failed").into_response())
        }
    }
}

/// Writes one multipart part to `backend`, returning `(body_len, backend_etag,
/// hash_hex)` on success or an early terminal `Response` on any client/backend
/// error.
///
/// Two write models, chosen by the backend's own capabilities:
/// * `multipart_native` (e.g. `S3Backend`): call the backend's own
///   `upload_part` against its native multipart session
///   (`claims.multipart.backend_handle`, minted by `initiate_multipart_upload`
///   at plan time). `upload_part`'s trait signature takes the whole part as
///   one `Bytes` — S3's `UploadPart` needs the full body up front to sign and
///   send in a single request — so the part is buffered here, bounded by the
///   token's exact `size` claim (the same bound the non-native path enforces
///   via `put_stream`'s `max_size`), so this never buffers more than one
///   part's worth of bytes.
/// * otherwise (e.g. `LocalFsBackend`, which has no native multipart): each
///   part is written as its own backend object at `{backend_path}.part.{n}`
///   via `put_stream`, and `complete_multipart_upload`'s local-fs fallback
///   assembles them.
///
/// `semaphore` (`SidecarState::part_upload_semaphore`) is only ever acquired
/// around the `multipart_native` branch, since only that branch buffers a
/// whole part in memory; the offset-object branch streams straight to the
/// backend via `put_stream` and would gain nothing from the same limiter.
async fn write_multipart_part(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
    semaphore: &Arc<tokio::sync::Semaphore>,
    idle: Option<Duration>,
) -> Result<(u64, String, String), Response> {
    if backend.capabilities().multipart_native {
        // The permit is held only for the duration of the buffering write
        // below, released right after it completes and before the
        // report-part callback -- see `upload_multipart_part`'s doc comment.
        let permit = acquire_part_upload_slot(semaphore).await?;
        let result = write_multipart_part_native(backend, claims, part_number, body, idle).await;
        drop(permit);
        result
    } else {
        write_multipart_part_offset_object(backend, claims, part_number, body, idle).await
    }
}

/// `multipart_native` backend write path — see `write_multipart_part`'s doc
/// comment.
async fn write_multipart_part_native(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
    idle: Option<Duration>,
) -> Result<(u64, String, String), Response> {
    let max_size = claims.multipart.size;
    let timed_out = Arc::new(AtomicBool::new(false));
    let mut stream = idle_timeout_stream(
        body.into_data_stream()
            .map(|r| r.map_err(std::io::Error::other)),
        idle,
        Arc::clone(&timed_out),
    );
    let mut buf = bytes::BytesMut::new();
    loop {
        match stream.next().await {
            Some(Ok(chunk)) => {
                if (buf.len() as u64).saturating_add(chunk.len() as u64) > max_size {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!("part body length exceeds token size claim {max_size}"),
                    )
                        .into_response());
                }
                buf.extend_from_slice(&chunk);
            }
            Some(Err(e)) => {
                // Cheap here (unlike the other two call sites): the part is
                // buffered locally, so `buf.len()` is exactly how much of
                // this part had arrived when the client went idle.
                if let Some(resp) = idle_timeout_response(
                    &timed_out,
                    &format!("{} (part {part_number})", claims.backend_path),
                    Some(buf.len() as u64),
                ) {
                    return Err(resp);
                }
                tracing::error!(error = %e, part_number, "part body stream read failed");
                return Err((StatusCode::BAD_REQUEST, "body read error").into_response());
            }
            None => break,
        }
    }
    let body_len = buf.len() as u64;
    // FEATURE §4, point 2: reject if body length ≠ size claim. Checked here
    // (before the backend call) rather than after, since the whole part is
    // already buffered — no partial native upload to clean up.
    //
    // The mid-stream guard above already rejects any chunk that would push
    // `buf` past `max_size`, so the only way to reach this check with a
    // mismatch is an *undersized* part (client sent fewer bytes than
    // claimed) — a client error, not a body exceeding a size limit, hence
    // `400 Bad Request` rather than `413 Payload Too Large`.
    if body_len != max_size {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("part body length {body_len} does not match token size claim {max_size}"),
        )
            .into_response());
    }
    match backend
        .upload_part(
            &claims.backend_path,
            &claims.multipart.backend_handle,
            part_number,
            // ADR-0006: the part's byte offset within the assembled object,
            // authoritatively minted into the token at initiate time.
            claims.multipart.offset,
            buf.freeze(),
        )
        .await
    {
        Ok((etag, hash)) => Ok((body_len, etag, hex::encode(hash))),
        Err(e) => {
            tracing::error!(error = %e, part_number, "backend native upload_part failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response())
        }
    }
}

/// Non-native (offset-object) backend write path — see
/// `write_multipart_part`'s doc comment.
async fn write_multipart_part_offset_object(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
    idle: Option<Duration>,
) -> Result<(u64, String, String), Response> {
    let part_path = format!("{}.part.{}", claims.backend_path, part_number);
    let timed_out = Arc::new(AtomicBool::new(false));
    let byte_stream = idle_timeout_stream(
        body.into_data_stream()
            .map(|r| r.map_err(std::io::Error::other)),
        idle,
        Arc::clone(&timed_out),
    );
    let (body_len, part_hash) = match backend
        .put_stream(&part_path, byte_stream, Some(claims.multipart.size))
        .await
    {
        Ok(v) => v,
        Err(DomainError::Validation { .. }) => {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "part body length exceeds token size claim {}",
                    claims.multipart.size
                ),
            )
                .into_response());
        }
        Err(e) => {
            if let Some(resp) = idle_timeout_response(&timed_out, &part_path, None) {
                return Err(resp);
            }
            tracing::error!(error = %e, part_number, "backend part write failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response());
        }
    };

    // FEATURE §4, point 2: reject if body length ≠ size claim. The
    // `max_size` guard above only rejects an *oversized* part mid-stream (via
    // `Err(DomainError::Validation { .. })` above, mapped to `413`); an
    // undersized part still streams to completion, so the exact-length check
    // happens here, now that `body_len` is final. Reaching this point means
    // the part was *not* oversized, so a mismatch here can only be
    // undersized — a client error (`400 Bad Request`), not a body exceeding
    // a size limit. The mismatched part is removed so a rejected part never
    // lingers as an orphaned backend object.
    if body_len != claims.multipart.size {
        drop(backend.delete(&part_path).await);
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "part body length {} does not match token size claim {}",
                body_len, claims.multipart.size
            ),
        )
            .into_response());
    }

    let part_etag = hex::encode(part_hash);
    Ok((body_len, part_etag.clone(), part_etag))
}

/// How long `upload_multipart_part` will wait for a concurrency-limit permit
/// once the semaphore is observed exhausted, before giving up and
/// answering `503`/`Retry-After` instead. Short by design: this is meant to
/// smooth over a slot freeing up moments later (a part write finishing), not
/// to let requests queue behind a sustained overload — a sidecar at its
/// concurrency ceiling should shed load quickly so clients back off and
/// retry, rather than accumulating held-open connections.
const PART_UPLOAD_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(200);

/// Build the `503 Service Unavailable` response `upload_multipart_part`
/// returns when it cannot acquire a concurrency-limit permit within
/// `PART_UPLOAD_ACQUIRE_TIMEOUT`. `Retry-After: 1` is a deliberately
/// short, fixed hint — a part write is typically fast, so a slot is likely to
/// free up well within a second — not a promise, just a cheap nudge for a
/// well-behaved retrying client.
fn part_upload_busy_response() -> Response {
    let mut resp = (
        StatusCode::SERVICE_UNAVAILABLE,
        "sidecar is at its concurrent-part-upload limit, retry shortly",
    )
        .into_response();
    resp.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    resp
}

/// Acquire one part-upload slot from `semaphore`
/// ([`SidecarState::part_upload_semaphore`]), or hand back the response to
/// return.
///
/// Split out of [`upload_multipart_part`] so that handler stays under the
/// crate's cognitive-complexity ceiling; the policy itself is described at
/// the call site. `Err` carries a ready-made response -- `503` +
/// `Retry-After` when the sidecar is simply busy, `500` for the
/// never-closed-in-practice closed-semaphore case.
async fn acquire_part_upload_slot(
    semaphore: &Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, Response> {
    match Arc::clone(semaphore).try_acquire_owned() {
        Ok(permit) => return Ok(permit),
        Err(tokio::sync::TryAcquireError::NoPermits) => {}
        Err(tokio::sync::TryAcquireError::Closed) => {
            tracing::error!("part-upload semaphore unexpectedly closed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response());
        }
    }

    match tokio::time::timeout(
        PART_UPLOAD_ACQUIRE_TIMEOUT,
        Arc::clone(semaphore).acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => Ok(permit),
        // The semaphore is never `close()`d anywhere in this process, so this
        // is unreachable in practice; treated as a hard failure rather than
        // silently proceeding unbounded.
        Ok(Err(_)) => {
            tracing::error!("part-upload semaphore unexpectedly closed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response())
        }
        Err(_) => Err(part_upload_busy_response()),
    }
}

/// `PUT` multipart part: verify `op=multipart_part` token, stream the part
/// straight to the backend, enforce the exact `size` claim, compute and
/// return the part hash.
///
/// Concurrency limit: [`write_multipart_part`] acquires a permit from
/// `state.part_upload_semaphore` (sized by
/// `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS`) around its `multipart_native`
/// branch only, held until that write completes. See
/// [`acquire_part_upload_slot`]'s own inline comment for the
/// try-then-bounded-wait contract, and [`part_upload_busy_response`] for the
/// `503` a caller gets when no slot is available in time.
///
/// On the offset-object path the part body is never buffered whole here —
/// like `upload`, it streams through `StorageBackend::put_stream`, which
/// enforces the token's declared `size` as an upper bound (`max_size`) while
/// bytes arrive, aborting mid-stream on an oversized part instead of
/// buffering it first. An *undersized* part can only be detected once the
/// stream is fully drained, so the exact-length check (FEATURE §4, point 2)
/// runs after the write completes, comparing against the streamed
/// `bytes_written`. The *native* multipart path is the exception: S3's
/// `UploadPart` needs the part's full length up front, so
/// `write_multipart_part_native` does buffer one whole part in memory (see
/// its own doc comment) — that is what the permit bounds, since the per-part
/// size ceiling alone does not cap how many such buffers can exist at once.
///
/// This is the sidecar half of the server-authoritative multipart model. The
/// control plane mints the token (sole minter, ADR-0004); the sidecar only
/// verifies and enforces — it can never mint a token.
///
/// Idempotent per `(upload_id, part_number)`: a re-PUT with the same token
/// overwrites the earlier part (safe for resume — ADR-0004 §4).
async fn upload_multipart_part(
    State(state): State<SidecarState>,
    Path((file_id, version_id, part_number)): Path<(Uuid, Uuid, u32)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match extract_token(&q, &headers) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    // Sidecar: verify the signed token (asymmetric Ed25519; sidecar cannot
    // mint tokens -- ADR-0004). `inst-part-token-reject` below covers the
    // reject-on-invalid-token branch (FEATURE §2 "Upload a Part" step 3).
    let claims = match state
        .verifier
        .verify(&token, time::OffsetDateTime::now_utc())
    {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };

    // Verify op and path bindings.
    if claims.op != Op::MultipartPart
        || claims.file_id != file_id
        || claims.version_id != version_id
    {
        return (
            StatusCode::FORBIDDEN,
            "token does not authorize this operation",
        )
            .into_response();
    }

    // Verify part-number binding (prevents replaying another part's token here).
    if claims.multipart.part_number != part_number {
        return (
            StatusCode::FORBIDDEN,
            "token part_number does not match path",
        )
            .into_response();
    }

    let backend = match state.backends.get(&claims.backend_id) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unknown backend '{}': {e}", claims.backend_id),
            )
                .into_response();
        }
    };

    // Write the part -- see `write_multipart_part`'s doc comment for the two
    // models this dispatches between and the concurrency-limit permit it
    // acquires around only the `multipart_native` branch: `try_acquire_owned`
    // is checked first so a request never even starts waiting once the
    // semaphore is provably exhausted; `PART_UPLOAD_ACQUIRE_TIMEOUT` then
    // bounds how long a request that arrives just as capacity frees up will
    // wait for a slot, rather than letting client connections pile up
    // indefinitely behind a busy sidecar. Either way, a request that can't
    // get a slot promptly gets `503`/`Retry-After` -- cheap for the client to
    // retry -- instead of buffering that part's body. The permit is released
    // as soon as the write completes, before the report-part callback below
    // (pure network I/O with no bearing on the memory this semaphore
    // guards).
    let (body_len, backend_etag, hash_hex) = match write_multipart_part(
        backend.as_ref(),
        &claims,
        part_number,
        body,
        &state.part_upload_semaphore,
        state.body_idle_timeout,
    )
    .await
    {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    #[allow(clippy::cast_precision_loss)]
    state.metrics.record_ingress_bytes(body_len as f64);

    // Report-part callback: notify the control plane that this part's bytes
    // have landed so it can record the part row `complete_multipart_upload`
    // assembles from. `claims.request_id` is echoed back as `x-request-id`
    // so both planes' logs for this upload can be correlated.
    if let Err(resp) = report_part_with_control_plane(
        &state,
        &token,
        &claims.request_id,
        file_id,
        version_id,
        claims.multipart.upload_id,
        part_number,
        &backend_etag,
        &hash_hex,
        i64::try_from(body_len).unwrap_or(i64::MAX),
    )
    .await
    {
        return resp;
    }

    // Return the part hash and ETag so callers can track per-part integrity.
    let body = serde_json::json!({
        "part_number": part_number,
        "etag": backend_etag,
        "hash_algorithm": "SHA-256",
        "hash": hash_hex,
    });
    (StatusCode::OK, axum::Json(body)).into_response()
}

/// Fallback `Content-Type` for a sidecar download response.
///
/// The control plane stamps the version's real stored MIME into the GET
/// token's `content_type` claim at download-URL-issuance time (the sidecar
/// itself remains a stateless byte-mover with no DB access — it only echoes
/// what the token carries). This fallback applies to a token minted before
/// this field existed (`claims.content_type` empty) or if the claim's value
/// fails to parse as a header value — a generic octet-stream type is always
/// a safe (if non-specific) answer.
const FALLBACK_CONTENT_TYPE: &str = "application/octet-stream";

/// Resolve the `Content-Type` header for a download response from the
/// token's claims — see [`FALLBACK_CONTENT_TYPE`] for when it falls back
/// instead of echoing `claims.content_type`.
fn content_type_header(claims: &Claims) -> HeaderValue {
    if claims.content_type.is_empty() {
        return HeaderValue::from_static(FALLBACK_CONTENT_TYPE);
    }
    HeaderValue::from_str(&claims.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static(FALLBACK_CONTENT_TYPE))
}

/// Resolve the `ETag` header for a download response from the token's
/// claims. `claims.etag` already carries the quoted, opaque content `ETag`
/// (`domain::etag::content_etag`) minted by the control plane — one source of
/// truth, no re-quoting here. `None` when the claim is empty (a token minted
/// before this field existed) or fails to parse as a header value, in which
/// case the response simply omits `ETag`.
fn etag_header(claims: &Claims) -> Option<HeaderValue> {
    if claims.etag.is_empty() {
        return None;
    }
    HeaderValue::from_str(&claims.etag).ok()
}

/// Build a `Content-Range` header value, e.g. `bytes 0-99/1000` or
/// `bytes */1000` (the unsatisfiable-range form, RFC 9110 §14.4).
fn header_value(s: &str) -> HeaderValue {
    // Every caller builds this from ASCII digits/literals, so this can only
    // fail if a future edit introduces non-ASCII content; fall back to a
    // clearly-invalid-but-safe placeholder rather than panicking.
    HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static("invalid"))
}

/// `GET` download: verify token (op=GET), stream bytes, honour `Range`.
///
/// Every backend error is mapped distinctly: blob-not-found, unsatisfiable-
/// range, and genuine I/O failures never fold into a blanket `416`.
/// `Content-Range` is emitted on every `206` (and on `416`, per RFC 9110
/// §14.4). `Content-Type` and `ETag` are sourced from the token's
/// `content_type`/`etag` claims (real stored MIME + content `ETag`, see
/// [`content_type_header`]/[`etag_header`]), falling back to
/// [`FALLBACK_CONTENT_TYPE`] and no `ETag` at all for a token minted before
/// those claims existed.
///
/// *Not implemented, documented rather than silently skipped*:
/// `If-None-Match` → `304` on a match. Every download token is already
/// single-use-scoped to one `(file_id, version_id)`, so the bandwidth win of
/// a conditional download is small; add it here, mirroring
/// `api/rest/handlers.rs::get_file`'s pattern, if a caller class needs it.
async fn download(
    State(state): State<SidecarState>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    let token = match extract_token(&q, &headers) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let claims = match state.verifier.verify(&token, OffsetDateTime::now_utc()) {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    if claims.op != Op::Get || claims.file_id != file_id || claims.version_id != version_id {
        return (
            StatusCode::FORBIDDEN,
            "token does not authorize this operation",
        )
            .into_response();
    }

    let backend = match state.backends.get(&claims.backend_id) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unknown backend '{}': {e}", claims.backend_id),
            )
                .into_response();
        }
    };

    let path = &claims.backend_path;

    // Resolve existence and size together via `stat`, distinctly from
    // any later I/O failure -- a missing blob must be `404`, never folded
    // into `416` (bad range) or `500` (genuine backend fault). `stat`
    // already distinguishes a real "not found" from other I/O errors per
    // backend (see `StorageBackend::stat`'s contract), so anything failing
    // after this point is a genuine backend error, not a missing blob.
    // Resolved once here and threaded through to both `download_range` and
    // `download_whole`.
    let total = match backend.stat(path).await {
        Ok(Some(n)) => n,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "backend stat failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    };

    // Range support (random read access) — a single signed URL serves many ranges.
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(range::parse);

    match range {
        Some(r) => download_range(&state, &backend, path, total, r, &claims).await,
        None => download_whole(&state, &backend, path, total, &claims).await,
    }
}

/// `HEAD` download: same token verification and `404` contract as
/// `download`, but never reads any content -- only `StorageBackend::stat` is
/// called (one metadata-only round-trip on every backend: a single `stat(2)`
/// on `local-fs`, a single `HeadObject` on `S3Backend`), and the body is
/// always empty. Registered explicitly via `.head(download_head)` in
/// `build_router` -- see that route's comment for why.
///
/// Returns the same `Accept-Ranges`/`Content-Type`/`ETag` headers as
/// `download`'s `200`, plus `Content-Length` (which a real `200`/`206`
/// response gets for free from its body's known size — `HEAD` has no body to
/// derive it from, so it is set explicitly here from `backend.stat`).
async fn download_head(
    State(state): State<SidecarState>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    let token = match extract_token(&q, &headers) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let claims = match state.verifier.verify(&token, OffsetDateTime::now_utc()) {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    if claims.op != Op::Get || claims.file_id != file_id || claims.version_id != version_id {
        return (
            StatusCode::FORBIDDEN,
            "token does not authorize this operation",
        )
            .into_response();
    }

    let backend = match state.backends.get(&claims.backend_id) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unknown backend '{}': {e}", claims.backend_id),
            )
                .into_response();
        }
    };

    let path = &claims.backend_path;

    // Same existence-and-size contract as `download`, via one `stat` round-trip.
    let total = match backend.stat(path).await {
        Ok(Some(n)) => n,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "backend stat failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    };

    let mut resp = (StatusCode::OK, ()).into_response();
    let headers_mut = resp.headers_mut();
    headers_mut.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers_mut.insert(header::CONTENT_TYPE, content_type_header(&claims));
    if let Some(v) = etag_header(&claims) {
        headers_mut.insert(header::ETAG, v);
    }
    headers_mut.insert(header::CONTENT_LENGTH, header_value(&total.to_string()));
    resp
}

/// Map a [`StorageBackend::get_stream`]/[`StorageBackend::get_range_stream`]
/// failure to a response, once existence and (for a range request)
/// satisfiability have already been confirmed by the caller's `stat`.
///
/// [`DomainError::Conflict`] means the backend's own re-check (its
/// `expected_len` comparison against a fresh `stat`/`metadata()` call, taken
/// *after* the caller's `stat`) found the object had changed size in that
/// window — a real but transient race, not a fault. That gets `503` with a
/// short `Retry-After`: the object settling to a new state and a retry
/// picking up the right length/content is the expected recovery, so this is
/// logged at `warn!`, not `error!`. Every other error is a genuine I/O fault
/// (e.g. a disk error or backend outage) and keeps the previous `500` +
/// `error!`.
fn backend_read_error_response(e: &DomainError, context: &str) -> Response {
    if let DomainError::Conflict { .. } = e {
        tracing::warn!(error = %e, context, "object changed during read, retrying should succeed");
        let mut resp = (
            StatusCode::SERVICE_UNAVAILABLE,
            "object changed during read, retry",
        )
            .into_response();
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return resp;
    }
    tracing::error!(error = %e, context, "backend read failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response()
}

/// Serve a `Range`-qualified `GET` once the blob's existence and size have
/// already been resolved by the caller (`download`'s single `stat` call,
/// -- `total` is that call's result, not a fresh backend round-trip).
/// Split out of `download` to keep its cognitive complexity down.
///
/// The response body is streamed from `StorageBackend::get_range_stream`
/// (`axum::body::Body::from_stream`) rather than materialized into a `Bytes`
/// buffer first. This matters even for a "partial" request: `Range:
/// bytes=0-` (`ByteRange::OpenEnded { start: 0 }`) resolves to a range
/// spanning the *entire* object — it is the very first request many media
/// players issue — so the unbounded case is not a rare edge.
async fn download_range(
    state: &SidecarState,
    backend: &Arc<dyn StorageBackend>,
    path: &str,
    total: u64,
    r: file_storage_sdk::ByteRange,
    claims: &Claims,
) -> Response {
    let Some((start, end)) = r.resolve(total) else {
        // Genuine range-unsatisfiable (RFC 9110 §14.4): the client asked for
        // bytes past the end of a blob that does exist.
        let mut resp = (StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable").into_response();
        let headers_mut = resp.headers_mut();
        headers_mut.insert(
            header::CONTENT_RANGE,
            header_value(&format!("bytes */{total}")),
        );
        // api.md: "every download response includes Accept-Ranges" — the 416
        // path must not be an exception.
        headers_mut.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        return resp;
    };
    // The exact length already committed to below (Content-Length) —
    // threaded into the backend call so it can refuse a body that would
    // disagree with the header this handler is about to send.
    let range_len = end - start + 1;
    match backend.get_range_stream(path, r, range_len).await {
        Ok(stream) => {
            // Counted per chunk, not as one lump up front: the stream is not
            // fully materialized before any of it reaches the client, so
            // egress must be attributed as each chunk actually leaves the
            // process — otherwise a connection that drops mid-transfer would
            // over-report bytes that were never actually sent.
            let metrics = Arc::clone(&state.metrics);
            let body_stream = stream.map(move |chunk| {
                if let Ok(bytes) = &chunk {
                    #[allow(clippy::cast_precision_loss)]
                    metrics.record_egress_bytes(bytes.len() as f64);
                }
                chunk
            });

            let mut resp =
                (StatusCode::PARTIAL_CONTENT, Body::from_stream(body_stream)).into_response();
            let headers_mut = resp.headers_mut();
            headers_mut.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            headers_mut.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes {start}-{end}/{total}")),
            );
            // A streamed body gets no `Content-Length` for free from axum,
            // since nothing here knows the stream's length up front — set it
            // explicitly from the already-resolved range bounds instead. Same
            // value already passed into `get_range_stream` above as
            // `range_len`, so the header and the backend's own length check
            // can never drift apart.
            headers_mut.insert(header::CONTENT_LENGTH, header_value(&range_len.to_string()));
            headers_mut.insert(header::CONTENT_TYPE, content_type_header(claims));
            if let Some(v) = etag_header(claims) {
                headers_mut.insert(header::ETAG, v);
            }
            resp
        }
        // Existence and range satisfiability were already confirmed above;
        // see `backend_read_error_response` for how a `Conflict` (object
        // changed size between `stat` and this call) differs from a genuine
        // I/O fault.
        Err(e) => backend_read_error_response(&e, "get_range_stream"),
    }
}

/// Serve a whole-blob `GET` (no `Range` header) -- see `download_range`'s
/// doc comment for why `total` is a parameter rather than a fresh backend
/// round-trip, and why this is split out of `download`.
///
/// The response body is streamed from `StorageBackend::get_stream`
/// (`axum::body::Body::from_stream`) rather than materialized into a `Bytes`
/// buffer first — the sidecar's own `FS_SIDECAR_MAX_BODY_BYTES` default alone
/// permits objects up to 5 GiB, and a whole in-memory copy per concurrent
/// whole-object download at that size is not acceptable.
async fn download_whole(
    state: &SidecarState,
    backend: &Arc<dyn StorageBackend>,
    path: &str,
    total: u64,
    claims: &Claims,
) -> Response {
    // `total` is exactly the value already committed to in the
    // `Content-Length` header set below — threaded through so the backend
    // can refuse a body that would disagree with it.
    match backend.get_stream(path, total).await {
        Ok(stream) => {
            // Counted per chunk as it is handed to the client — see
            // `download_range`'s identical comment for why.
            let metrics = Arc::clone(&state.metrics);
            let body_stream = stream.map(move |chunk| {
                if let Ok(bytes) = &chunk {
                    #[allow(clippy::cast_precision_loss)]
                    metrics.record_egress_bytes(bytes.len() as f64);
                }
                chunk
            });

            let mut resp = (StatusCode::OK, Body::from_stream(body_stream)).into_response();
            let headers_mut = resp.headers_mut();
            headers_mut.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            headers_mut.insert(header::CONTENT_TYPE, content_type_header(claims));
            if let Some(v) = etag_header(claims) {
                headers_mut.insert(header::ETAG, v);
            }
            headers_mut.insert(header::CONTENT_LENGTH, header_value(&total.to_string()));
            resp
        }
        // Existence was already confirmed above; see
        // `backend_read_error_response` for how a `Conflict` (object changed
        // size between `stat` and this call) differs from a genuine backend
        // fault.
        Err(e) => backend_read_error_response(&e, "get_stream"),
    }
}

#[cfg(test)]
#[path = "sidecar_tests.rs"]
mod tests;
