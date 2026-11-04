# FileStorage — Operational Configuration

This document covers every `FileStorageConfig` field (`gears/file-storage/file-storage/src/config.rs`), the sidecar's
own `FS_SIDECAR_*` environment variables (`gears/file-storage/file-storage/src/bin/sidecar.rs`), and the state of the
`CleanupEngine` (not scheduled by the gear).

Every field below is verified against the current `default_*()` function or `main()`'s env-var parsing — each entry
names the function (or environment variable) that sets its default, rather than a line number, since line numbers
drift with every edit.

<!-- toc -->

- [Control-plane config: `FileStorageConfig`](#control-plane-config-filestorageconfig)
- [Sidecar config: `FS_SIDECAR_*` environment variables](#sidecar-config-fs_sidecar_-environment-variables)
- [Cleanup (not running yet)](#cleanup-not-running-yet)
- [Idempotent-create semantics](#idempotent-create-semantics)
- [Upgrading from v0.2.x and rolling back](#upgrading-from-v02x-and-rolling-back)
- [Storage quota (not enforced)](#storage-quota-not-enforced)
- [The `SignatureProvider` / `SignatureVerifier` abstraction](#the-signatureprovider--signatureverifier-abstraction)

<!-- /toc -->

## Control-plane config: `FileStorageConfig`

All fields are `#[serde(default = "…")]`, so an operator's YAML only needs to override what it wants to change; a
gear started with no `file-storage` config section at all gets every default below — **except that it still cannot
actually boot**: `require_signing_key` defaults to `true` with `signing_key_pkcs8` unset, and
`FileStorageConfig::validate()` fails gear init on exactly that combination (see `require_signing_key` below).
A genuinely zero-config deployment is dev/test-only (set `require_signing_key: false` there).
`FileStorageConfig::validate()` (called at gear init, before anything is wired up) rejects invalid configurations —
missing-secret/zero-value guards (`default_url_ttl_secs`, `multipart_session_ttl_secs`,
`multipart_complete_lease_secs` equal to `0`, instead of
silently using one second; `default_page_size` or `max_page_size` equal to `0`; `signing_key_pkcs8` absent while
required; `finalize_internal_secret` absent or empty);
absolute ceilings (`finalize_token_grace_secs` above `MAX_FINALIZE_TOKEN_GRACE_SECS`, 7 days; `max_page_size` above
`MAX_PAGE_SIZE_CEILING`, 200; `max_url_ttl_secs` above `MAX_URL_TTL_CEILING`, 30 days; `multipart_session_ttl_secs`
above `MAX_MULTIPART_SESSION_TTL_SECS`, 30 days; `multipart_complete_lease_secs` above
`MAX_MULTIPART_COMPLETE_LEASE_SECS`, 1 day;
`idempotency_ttl_secs` above `MAX_IDEMPOTENCY_TTL_SECS`, 30 days); cross-field ordering
invariants (`default_url_ttl_secs` vs. `max_url_ttl_secs`; `default_page_size` vs. `max_page_size`;
`multipart_session_ttl_secs` vs. `default_url_ttl_secs`), as noted inline below. Function names (rather than line numbers) are used as source pointers throughout this
table since line numbers drift with every edit.

**Removed keys.** `enable_background_sweep`, `sweep_interval_secs`, `sweep_time_budget_secs`, `orphan_grace_secs` and
`require_finalize_internal_secret` no longer exist: there is no background sweep, and the internal secret is always
required. Because the config struct denies unknown fields, a config that still sets any of them fails to load.

Config for this gear (like every gear on this platform) is loaded from its own platform YAML configuration section —
there is no standalone TOML/JSON file of its own.

| Field | Default | Source |
|---|---|---|
| `default_url_ttl_secs` | `900` (15 min) | `default_default_url_ttl_secs()` |
| `max_url_ttl_secs` | `604800` (7 days) | `default_max_url_ttl_secs()` |
| `finalize_token_grace_secs` | `3600` (1h) | `default_finalize_token_grace_secs()` |
| `multipart_session_ttl_secs` | `86400` (24h) | `default_multipart_session_ttl_secs()` |
| `multipart_complete_lease_secs` | `120` (2 min) | `default_multipart_complete_lease_secs()` |
| `sidecar_base_url` | `"http://localhost:8087"` | `default_sidecar_base_url()` |
| `default_page_size` | `25` | `default_page_size()` |
| `max_page_size` | `200` | `default_max_page_size()` |
| `storage_root` | `"./.file-storage-data"` | `default_storage_root()` |
| `signing_key_pkcs8` | `None` (no `#[serde(default = …)]`, just `Option::default()`) | struct field default |
| `require_signing_key` | `true` | `default_require_signing_key()` |
| `idempotency_ttl_secs` | `86400` (24h) | `default_idempotency_ttl_secs()` |
| `enable_in_memory_backend` | `false` (bare `#[serde(default)]`) | struct field default |
| `s3_backends` | `[]` (empty, bare `#[serde(default)]`) | struct field default |
| `default_backend_id` | `None` (bare `#[serde(default)]`) | struct field default |
| `finalize_internal_secret` | **required** (`None` fails `validate()`) | struct field default |

### `default_url_ttl_secs`
Default TTL (seconds) baked into every signed URL the control plane mints (`900` = 15 minutes), unless the caller's
presign request or the code path explicitly asks for something else. Bounds the "stale-permission window" — how long
a URL remains valid after the authorization decision was made at signing (no per-token revocation exists). **Production
recommendation**: keep short (minutes, not hours) for anything not explicitly meant to be long-lived or
shareable; raise only for known bulk/batch workflows. **Misconfiguration risk**: too long → a leaked/logged URL stays
exploitable for the full window; too short → legitimate slow uploads/downloads may need to be re-presigned mid-flight
(no such retry-on-expiry logic exists in the SDK/handlers, so a very small value can break large transfers — though `finalize_token_grace_secs` below keeps an upload that outran this TTL *mid-stream* from failing at the finalize step).
`FileStorageConfig::validate()` enforces an ordering invariant on this field at startup: it must not exceed
`max_url_ttl_secs` (otherwise the very first URL minted with no explicit override already violates the ceiling the
control plane is supposed to enforce).

### `max_url_ttl_secs`
Hard ceiling (seconds) the control plane will mint any signed URL to (`604800` = 7 days); enforced by `Issuer::issue`
which clamps `exp` down to `now + max_url_ttl_secs` regardless of what was requested. **Production recommendation**:
leave at the 7-day default or lower for stricter environments; do not raise without a specific long-lived/anonymous-
sharing use case (a separate FileShare gear, not yet built, is the intended mechanism for that — not a raised ceiling
here). **Misconfiguration risk**: raising it widens the window during which a leaked URL is exploitable, with no
revocation mechanism to claw it back. Lowering it below `default_url_ttl_secs` is rejected by
`FileStorageConfig::validate()` at startup rather than silently clamping every default-TTL mint, and so is raising it
above `MAX_URL_TTL_CEILING` (`2592000` s = 30 days).

### `finalize_token_grace_secs`
How far past its `exp` (seconds, default `3600` = 1 hour) the signed upload token is still accepted **on the
server-to-server finalize and report-part callbacks only** (`Verifier::verify_with_grace`); `0` restores strict
`exp` enforcement everywhere. Capped at `MAX_FINALIZE_TOKEN_GRACE_SECS` (`604800` s = 7 days):
`FileStorageConfig::validate()` fails gear init outright if configured above that ceiling, rather than letting an
oversized value degrade the finalize/report-part callbacks' `exp` check into a de-facto no-op (`gear.rs` converts
this field to `i64` via a saturating `unwrap_or(i64::MAX)`, so an unchecked oversized value would otherwise become
effectively infinite grace). It exists because the sidecar verifies the token once, at the start of the `PUT`, and
deliberately never re-checks it mid-stream — `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` bounds the gap between chunks, not
the transfer's total duration — and then forwards that same token to the control plane once the bytes have landed.
Without the grace, an upload slower than `default_url_ttl_secs` would have every byte durably published and its
finalize rejected as expired, leaving a `pending` version the client cannot commit. The grace never applies to the
check that starts a `PUT`/part-`PUT`/`GET`, and it does not weaken what else is verified: signature, `op` and the
token's binding to `(file_id, version_id)` are enforced exactly as before. **Production recommendation**: size it to
the slowest legitimate single transfer you intend to support on the worst link you serve; the default hour covers a
multi-GiB upload on a slow connection. **Misconfiguration risk**: too small → slow uploads fail at finalize with the
bytes already written (see F5's neighbours in
[concurrency-and-failure-model.md](./concurrency-and-failure-model.md) §2.1); too large → a leaked token stays usable
against the finalize/report-part callbacks for that much longer after its nominal expiry, which is a narrower
exposure than a leaked *upload* URL (those callbacks only commit or report a version whose bytes are already in the
backend) but is still real. Set `0` if your deployment fronts uploads with something that guarantees a bounded
transfer time.

### `multipart_session_ttl_secs`
Lifetime (seconds, default `86400` = 24h) of a multipart session row: `expires_at` is stamped at initiate time, and
a cleanup job (none runs yet) is meant to abort the session once it passes. Deliberately much longer than `default_url_ttl_secs` — it is a
budget for a whole multi-GB upload, not for one signed URL. `FileStorageConfig::validate()` **rejects** a value
*below* `default_url_ttl_secs`: the per-part URLs are minted at initiate time with the default TTL, so a shorter
session lifetime would let a part URL outlive the session it belongs to and have `complete`'s defense-in-depth
expiry check reject an upload whose URLs were still technically valid. **Production recommendation**: size it to the
slowest legitimate upload you intend to support. **Misconfiguration risk**: too short → long uploads are aborted
mid-flight by that future cleanup; too long → abandoned sessions (and their backend multipart handles) linger before the
reaper touches them. Capped at `MAX_MULTIPART_SESSION_TTL_SECS` (`2592000` s = 30 days): `validate()` fails gear
init above it, because the value is added to the current time when a session is created and an oversized one would
otherwise overflow that timestamp.

### `multipart_complete_lease_secs`
How long (seconds, default `120`) one caller may hold the `completing` lease on a multipart session before another
caller is allowed to take it over. `complete` moves the session `in_progress → completing(lease_owner, lease_until)
→ completed(complete_result)` with single conditional `UPDATE`s, holding **no** DB transaction across the backend
assembly I/O; a second caller arriving while the lease is live is answered `202 completing` and polls. **Production
recommendation**: size it to the backend's assembly time for your largest objects. **Misconfiguration risk**: too
short → a slow-but-healthy assembly has its lease stolen and the work is redone by a second caller; too long → a
session whose completer really did crash stays unavailable for takeover for the whole lease window. Capped at
`MAX_MULTIPART_COMPLETE_LEASE_SECS` (`86400` s = 1 day); `validate()` fails gear init above it.

### `sidecar_base_url`
The externally-reachable base URL of the data-plane sidecar that every signed URL points at (default assumes a
sidecar on `localhost:8087`, i.e. **local dev only**). **Production recommendation**: must be set to the sidecar's
real public/internal address for the deployment topology (e.g. behind a load balancer in front of multiple sidecar
replicas). **Misconfiguration risk**: every signed upload/download URL embeds this host — if it is wrong or
unreachable from the client, every content operation fails even though the control plane itself is healthy; this is
easy to overlook because control-plane health checks and metadata CRUD (`GET /files`, etc.) will look fine.

### `default_page_size` / `max_page_size`
Pagination defaults/ceiling for `GET /files`, `GET /files/{id}/versions` and `GET /retention-rules` — `25` / `200`
respectively, matching the platform-wide cursor-pagination convention (`guidelines/DNA/REST/QUERYING.md`).
**Production recommendation**: the defaults are reasonable starting points; raise `max_page_size` only if clients
have a proven need for larger pages and the DB/latency budget supports it. **Misconfiguration risk**: a very large
`max_page_size` lets a caller force an expensive, unbounded-feeling listing query; a `default_page_size` larger than
`max_page_size` would be self-contradictory — `FileStorageConfig::validate()` rejects this combination at startup.
A `0` in either field is rejected at startup too: it would make every listing return an empty page with no cursor.
`max_page_size` also has its own absolute ceiling, `MAX_PAGE_SIZE_CEILING` (`200`, the same value as the shipped
default): `validate()` rejects any configured `max_page_size` above it, independent of the `default_page_size`
check, since it is otherwise the only bound on a single listing request's row count and response size. A caller-supplied
`limit` (or its synonym `$top`) above `max_page_size` is not rejected — it is silently clamped down to `max_page_size`
(`limit.min(max)`); only `limit == 0` / `$top=0` is a `400`.

### `storage_root`
Local filesystem root for the default `local-fs` backend (default `./.file-storage-data`, i.e. **relative to the
process's working directory** — not durable across container image rebuilds unless mounted). **Production
recommendation**: point at a durable, backed-up volume mount; never leave at the relative default in a containerized
deployment. **Misconfiguration risk**: content written under an ephemeral/container-local path is lost on
pod/container recreation — this is a **silent data-loss** risk since writes will appear to succeed.

`storage_root` must live on a filesystem that supports hard links (`link(2)`): the exclusive publish of a single-part
upload hard-links the written temp file into place, and there is deliberately no `rename` fallback (it would
overwrite). On a filesystem without them (FAT/exFAT, some FUSE and SMB mounts) every single-part upload fails. The
sidecar's `/readyz` checks this with a probe file in the root (a success is cached for the process, a failure is retried on the next probe) and reports the backend as not ready.

### `signing_key_pkcs8`
Base64url-encoded PKCS#8 DER ECDSA P-256 private key for the URL-signing keypair. When set, the keypair (and the public key the
sidecar verifies against) is **stable across restarts**; when absent, `gear.rs::init` generates an ephemeral key
every boot (logged at `info` level with an explicit warning). **Production recommendation**: always set this in any
real deployment, and treat it as a secret (same handling tier as any other private key material) — never log or
serialize it (`FileStorageConfig`'s manual `Debug` impl redacts it deliberately). **Multi-replica warning**: every
replica must be configured with the **same** key. If replicas each generate their own ephemeral key (i.e. the key
is unset in a multi-replica deployment), signed URLs minted by one replica fail verification at the sidecar (which
is configured with only one public key via `FS_SIDECAR_PUBLIC_KEY`) — this looks like intermittent, replica-dependent
upload/download failures. `require_signing_key` (below) exists specifically to fail fast on this misconfiguration
instead of degrading silently into that failure mode.

**Rotation:** only one active key is implemented. Key replacement requires draining
outstanding URLs/uploads and coordinating control-plane and sidecar configuration.
Follow [the migration procedure](#ed25519-to-p-256-migration), also for later P-256
key rotations. On compromise, deliberately invalidate outstanding URLs and issue new
ones after replacing both components' keys.

Previously documented overlap settings (`previous_signing_public_keys` and
`FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`) are **not implemented**. Do not configure them:
the control plane rejects the unknown field and the sidecar ignores the environment
variable. There is no zero-outage multi-key rotation in this implementation.

### `require_signing_key`
When `true` (the default), `FileStorageConfig::validate()` makes gear init **fail fast** if `signing_key_pkcs8` is
absent, instead of silently minting an ephemeral per-boot key. **Production recommendation**: leave at `true`
everywhere except local dev/test harnesses that construct a `FileStorageConfig` directly and intentionally want the
ephemeral-key behaviour — set `false` explicitly there. **Misconfiguration risk**: setting `false` in a real
multi-replica deployment removes the fail-fast guard and re-opens the "different key per replica" failure mode
described above, deferred from a loud startup error to a confusing runtime symptom.

### `idempotency_ttl_secs`
Window (seconds, default `86400` = 24h) an `idempotency_keys` row (from `POST /files`'s `idempotency_key`) remains
valid for replay-detection; after this, a retry with the same key is treated as a brand-new request. Expired rows
are not reclaimed yet: no cleanup job runs (see below). **Production recommendation**: size to the longest
realistic client retry window (default is generous for most HTTP retry policies). **Misconfiguration risk**: too
short → a legitimately delayed retry (e.g. after a long client-side backoff) creates a duplicate file instead of
being deduplicated; too long → expired rows accumulate (not a
correctness issue, just storage/index bloat) until a cleanup job exists. Capped at `MAX_IDEMPOTENCY_TTL_SECS` (`2592000` s = 30 days);
`validate()` fails gear init above it, since the value is added to the current time for every idempotency record.

### `enable_in_memory_backend`
When `true` (default `false`), an additional non-durable backend registered under the id `memory` (`MEMORY_ID`,
`gear.rs`) is registered alongside the default `local-fs` backend. **Production recommendation**: leave at `false`
in any deployment where data loss is unacceptable — the in-memory backend loses all content on restart. Its only
legitimate use is dev/test scenarios that want a second backend id to exercise multi-backend code paths (e.g.
`migrate_backend`) without provisioning real durable storage; it is also the simplest way to get a `multipart_native`
backend for exercising multipart upload without configuring S3 (see `default_backend_id` below). **Misconfiguration
risk**: enabling it in production, combined with a file or policy that routes content onto it, is **silent, guaranteed
data loss** on the next restart — `migrate_backend` additionally requires the caller's `ADMIN_POLICY` authorization
scope (not just `WRITE`) specifically to make this an explicit, elevated-privilege action rather than an accident.

### `s3_backends`
Zero or more S3-compatible backends (`S3BackendConfig` entries: `id`, `endpoint`, `region`, `bucket`,
`access_key_id`, `secret_access_key`, `path_style`) registered alongside `local-fs` (and `memory` if enabled). Empty
by default — a deployment opts in explicitly. Each entry becomes one `S3Backend` in the registry, keyed by its own
`id`; ids must be unique across the whole registry (`BackendRegistry::new` rejects a collision at startup). S3
support is gated by [ADR-0005](./ADR/0005-cpt-cf-file-storage-adr-s3-client-selection.md), whose status is
`proposed` pending a team security review of its external HTTP-signing/XML-parsing dependencies; the code builds and
is tested regardless, but merging it to `main` is conditioned on that review. **Production recommendation**: omit
`access_key_id`/`secret_access_key` and resolve credentials from the process environment
(`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`) instead of embedding them in gear YAML. **Misconfiguration risk**: a bad
endpoint or missing credentials with no env fallback fails gear init (fail-fast, not a runtime surprise).

**Set an `AbortIncompleteMultipartUpload` lifecycle rule on every bucket used here** — this is **required**, not a
recommendation. The rule's `DaysAfterInitiation` parameter only accepts whole days, minimum 1, so set
`DaysAfterInitiation >= ceil(multipart_session_ttl_secs / 86400) + 1` (2 days at the default 24h TTL). The `+ 1` is
required, not cosmetic: `DaysAfterInitiation` counts whole days, and whenever the TTL is an exact multiple of a day,
`ceil(ttl / 86400)` in seconds equals the TTL exactly, leaving zero margin against S3's day-granularity enforcement
of the rule — the `+ 1` guarantees at least one full day of headroom beyond the session's actual lifetime regardless
of how the TTL lines up with day boundaries. Example: a 48h TTL (172800 s) needs a minimum of 3 days, not 2. This is
safe because no session outlives its `expires_at` (a resume re-caps `exp` at the same `expires_at`), so a handle still
open once that many days have passed is by construction already abandoned. FileStorage aborts backend multipart
handles on a best-effort basis only, and two windows are not covered by any cleanup: a control-plane crash between
`initiate_multipart` and the session-row insert leaves a handle with no persisted correlation at all, and a backend
abort that fails after the session has already flipped to `aborted` is never retried (later passes list only
`in_progress` and lease-expired `completing` sessions). No object bytes are at stake in either case, but S3 bills for
incomplete multipart uploads. Until a cleanup job exists the lifecycle rule is the only reclamation path, and later it remains the
backstop for these two uncorrelated windows — see `concurrency-and-failure-model.md` §5.

**The endpoint must honour conditional writes (`If-None-Match: *`)** — this is a **requirement** for any S3-compatible
backend used here, not an optimisation. Publishing a version is create-exclusive: the single-part `PutObject` and
the multipart `CompleteMultipartUpload` both carry `If-None-Match: *`, and the endpoint must answer `412 Precondition
Failed` when the key already exists. That is what stops a replayed `PUT` on a still-valid signed URL from overwriting
bytes that are already published. FileStorage does not probe for this at runtime: an endpoint that silently ignores
the header degrades to last-write-wins with no error anywhere in the request path, so it **must not** be used with
this gear. AWS S3 documents support for it (since 2024-08-20); `s3s-fs` — the test double this crate's own tests and
CI use — is verified against it by the crate's `s3_tests.rs`, sequentially only: it checks the key with a plain
existence test, so two truly concurrent writers can both succeed there. That is acceptable for a test double and is
why the commands below check a real endpoint; it is not an endpoint to deploy. Check any other endpoint before
configuring it, with a
recent AWS CLI v2 (one that accepts `--if-none-match`) and credentials for the target bucket:

```bash
EP=https://s3.example.com; B=my-bucket; K=fs-conditional-write-check-$(date +%s)
printf x > /tmp/fs-check
# 1. First conditional write — must succeed.
aws --endpoint-url "$EP" s3api put-object --bucket "$B" --key "$K" --body /tmp/fs-check --if-none-match '*'
# 2. Same key again — must fail with PreconditionFailed (412). Success here means the endpoint ignores the header.
aws --endpoint-url "$EP" s3api put-object --bucket "$B" --key "$K" --body /tmp/fs-check --if-none-match '*'
# 3. Multipart completion onto the existing key — must also fail with PreconditionFailed (412).
U=$(aws --endpoint-url "$EP" s3api create-multipart-upload --bucket "$B" --key "$K" --query UploadId --output text)
E=$(aws --endpoint-url "$EP" s3api upload-part --bucket "$B" --key "$K" --upload-id "$U" --part-number 1 \
      --body /tmp/fs-check --query ETag --output text)
aws --endpoint-url "$EP" s3api complete-multipart-upload --bucket "$B" --key "$K" --upload-id "$U" \
    --multipart-upload "Parts=[{ETag=$E,PartNumber=1}]" --if-none-match '*'
# Clean up.
aws --endpoint-url "$EP" s3api abort-multipart-upload --bucket "$B" --key "$K" --upload-id "$U" 2>/dev/null
aws --endpoint-url "$EP" s3api delete-object --bucket "$B" --key "$K"
```

The backend is usable only if step 1 succeeds and steps 2 and 3 both fail with `412`.

### `default_backend_id`
Backend id `build_backend_registry` designates as the registry's default — the backend new `create`/
`initiate_multipart` calls write to. `None` (the default) keeps `local-fs` as the default. Set this to one of
`s3_backends`' configured ids to make that S3 backend the default instead. **Misconfiguration risk**: an id that
does not name a registered backend surfaces as a fail-fast gear-init error, never a panic.

`POST /files/{id}/multipart` (initiate) always targets this default backend, and requires it to advertise the
`multipart_native` capability. `local-fs` does **not** advertise `multipart_native`, so multipart uploads are
unavailable against the plain default configuration; switch the default to a configured S3 backend, or to the
dev/test `memory` backend (`enable_in_memory_backend` above), to enable multipart uploads. See `docs/api.md`'s
"P2 — Multipart upload" section.

### `finalize_internal_secret`
Interim gear-local shared secret the s2s finalize/report-part callback routes require, on top of the signed upload
token, via the `x-fs-internal-token` request header. **Required**: `validate()` fails gear init when it is missing or
empty, and the sidecar must be given the same value as `FS_SIDECAR_INTERNAL_TOKEN` (it refuses to start without it).
The control plane trusts the size and SHA-256 the sidecar reports on these authenticated callbacks and does not
re-read the object at finalize (it checks the stored length via backend metadata and reads only the MIME-sniff
prefix). This is a stop-gap until the platform's `toolkit-security::internal_auth` profiles are deployable in this
gear (see [ADR-0003](./ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)'s trust-model section). The control
plane rejects with `403` every sidecar callback lacking a matching `x-fs-internal-token`, which the client sees as a
`502` on every fresh single-part `PUT` (a replay of the same `PUT` is also answered `502`: only a `400` from finalize
turns a replay into `409`, see the finalize outcomes below) and on every part of a multipart upload.

**Rotating an already-configured secret is a brief upload outage, not a zero-downtime operation.**
`FinalizeAuth` holds exactly one secret, built once at gear init and kept in a `OnceLock`, so changing it takes a
control-plane restart; each sidecar likewise reads `FS_SIDECAR_INTERNAL_TOKEN` once at startup. There is no
dual-accept window on either side, so any sidecar whose token differs from the control plane's current secret
fails exactly as above (`403` server-side, `502` at the client) for as long as the two disagree. The sequence that
keeps that window shortest: stage the replacement sidecar fleet with the new token **outside** the load balancer,
restart the control plane on the new secret, cut traffic over to the new fleet, then drain the old one. Uploads in
flight on the old fleet at the cutover still fail and have to be retried. A true dual-secret window would need a
code change, and is expected to arrive with the `internal_auth` migration that retires this stop-gap.

Never logged (`FileStorageConfig`'s manual `Debug` impl redacts it).

## Sidecar config: `FS_SIDECAR_*` environment variables

The sidecar is a separate binary/process (`src/bin/sidecar.rs`) with its own env-var configuration — it does **not**
share `FileStorageConfig`. All of these are read once in `main()`.

| Variable | Default | Notes |
|---|---|---|
| `FS_SIDECAR_ADDR` | `0.0.0.0:8087` | Bind address. |
| `FS_SIDECAR_PUBLIC_KEY` | **required, no default** | Base64url ECDSA P-256/SHA-256 **primary** public key; must match the control plane's `signing_key_pkcs8`-derived keypair (see above). Startup fails (`anyhow::anyhow!`) if unset or malformed. |
| `FS_SIDECAR_BACKEND_ROOT` | `./.file-storage-data` | Local-fs backend root — same durability caveat as the control plane's `storage_root`; the two should point at the **same** underlying storage for a single-backend deployment, or the sidecar will read/write blobs the control plane's metadata doesn't expect to find there. |
| `FS_SIDECAR_CONTROL_URL` | `http://localhost:8080` | Base URL of the control plane, used for the finalize/report-part callbacks. Setting it to the **empty string** explicitly disables the callback (dev/test only) — uploaded versions then stay `pending` forever, since nothing ever calls finalize; production must always set this to a reachable control-plane URL. The scheme is **not** validated, and the callbacks carry `x-fs-token` plus the `x-fs-internal-token` shared secret — so keep this hop inside a trusted network boundary or point it at an HTTPS/mTLS endpoint; a plain-HTTP URL puts that secret on the wire in the clear. |
| `FS_SIDECAR_MAX_BODY_BYTES` | `5368709120` (5 GiB) | Raises axum's blanket request-body floor (default 2 MiB). The limit is a `DefaultBodyLimit` layer on the **whole** sidecar router (`build_router`), so it applies to the request bodies of the single-part `PUT` and of multipart part uploads alike — not only to the single-part route. It does **not** bound download responses: the limit governs request-body extraction, and a download is a `GET`/`HEAD` whose response is streamed past it. This is a transport-layer ceiling only — the real per-request limit is the signed token's `max_size`/`exact_size` claim. **Misconfiguration risk**: setting it below the largest policy-permitted single-part upload causes legitimate uploads to be rejected at the transport layer before the token-level check even runs; because the planner may widen `part_size` up to `MAX_PART_SIZE` (5 GiB) for very large objects, lowering this variable can also reject every *part* of a multipart upload with `413`, which is easy to miss when tuning it with only single-part uploads in mind. |
| `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` | `60` | Maximum pause the sidecar tolerates between two consecutive chunks of a client's request body — and before the first one — on the single-part `PUT` upload and on `upload_multipart_part`; `0` disables the guard. This is a **per-chunk idle** bound, not a deadline on the whole stream: a slow-but-steady multi-GiB upload that never pauses longer than this between chunks still completes, no matter how long it takes overall. It closes a gap neither of the other two body-related controls covers: the signed token's `exp` is checked exactly once, before any body bytes are read, and `FS_SIDECAR_MAX_BODY_BYTES` bounds bytes, not time — without this timeout, a client that opens the connection and then stalls (or never sends at all) could hold the request open indefinitely (CWE-400). A client that goes idle past the deadline gets `408 Request Timeout`; the partial object is cleaned up exactly like any other broken upload stream (see F2 in [concurrency-and-failure-model.md](./concurrency-and-failure-model.md)). Independent of `FS_SIDECAR_FINALIZE_TIMEOUT_SECS`/`FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` below, which bound the sidecar→control-plane callback *after* the body stream has already finished. **Misconfiguration risk**: too low rejects legitimate uploads from clients on slow or lossy links (a real, live upload that merely pauses between chunks) with a `408` that looks like a client bug; too high re-opens the held-open-connection exposure this control exists to close. |
| `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` | `10` | Total wall-clock **budget** for the sidecar → control-plane finalize/report-part callback, covering the **entire retry loop** (all `CALLBACK_MAX_ATTEMPTS = 3` attempts and the delays between them), not a per-attempt allowance — `post_with_retry` wraps the whole loop in one deadline sized to this value, so a hung/unreachable control plane can never hold the client's request open for more than this long regardless of how many attempts it takes. The control plane does not re-read the object at finalize (a metadata size check plus a ranged MIME-prefix read), so the budget only has to cover the round trip and those two backend calls. **Misconfiguration risk**: too low → the callback times out and the client sees `502` even though the bytes landed (F5 in [concurrency-and-failure-model.md](./concurrency-and-failure-model.md)). |
| `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` | `5` | Connect timeout for the same callbacks. Together with the timeout above, bounds how long a client's upload request can be held open by an unreachable or hung control plane — without these timeouts, a hung control plane could block the client indefinitely. **Misconfiguration risk**: too low in a high-latency network path causes spurious `502 Bad Gateway` responses to clients on otherwise-successful uploads; too high re-opens the "held open indefinitely" problem these timeouts exist to close. |
| `FS_SIDECAR_INTERNAL_TOKEN` | **required, no default** | Interim shared secret sent as `x-fs-internal-token` on both the finalize and report-part control-plane callbacks — the sidecar's half of the control plane's `finalize_internal_secret` (see above). Startup fails if it is unset or empty. Must equal the control plane's `finalize_internal_secret`. |
| `FS_SIDECAR_S3_BACKENDS` | unset (no S3 backends) | Optional JSON array of `S3BackendConfig` entries (mirrors the control plane's `s3_backends`), folded into the sidecar's own `BackendRegistry` alongside the always-present `local-fs` backend so a control-plane-registered `S3Backend` is reachable by real traffic dispatched per-request via `claims.backend_id`. Credentials embedded in this JSON blob are acceptable for the sidecar (the one component authorized to hold them, per ADR-0003) but should be sourced from a secrets manager / mounted file in production where supported. **Keep this list in lockstep with the control plane's `s3_backends`**: signed tokens carry `backend_id` and `backend_path`, and the sidecar resolves them against *its own* registry, with no reconciliation, handshake or version check between the two. A `backend_id` the sidecar does not know fails the request with `500` ("unknown backend") after the URL was already minted; worse, an id that resolves on both sides but points at a different endpoint/bucket fails silently in the other direction — the upload lands in the wrong bucket and the control plane's finalize size check finds no object, surfacing as a `400` on finalize (or a `404` on a later download) rather than as a configuration error. |

A `multipart_native` backend's part write (`upload_multipart_part` against, e.g., an `S3Backend`) streams the
part body straight through to the backend (`write_multipart_part_native`, `StorageBackend::upload_part_stream`) —
it is never buffered whole in the sidecar process, and its SHA-256 is computed incrementally on the same pass
that streams it out. There is accordingly no concurrency limiter or cgroup memory-budget check on this path:
neither the native nor the non-native (offset-object) write path buffers a whole part, so neither has a
worst-case buffered-memory footprint to bound.

Every `FS_SIDECAR_*` numeric env var (`FS_SIDECAR_MAX_BODY_BYTES`, `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS`,
`FS_SIDECAR_FINALIZE_TIMEOUT_SECS`, and `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS`) fails sidecar startup with a
descriptive error if the value is *set but does not parse* (`parse_optional`/`parse_env_or_default`,
`src/bin/sidecar.rs`) — it does **not** silently fall back to the default on a typo like
`FS_SIDECAR_MAX_BODY_BYTES=5GB`. Only a genuinely unset variable uses the default.

A failed finalize/report-part callback (after the sidecar's retry budget, see `post_with_retry` /
`CALLBACK_MAX_ATTEMPTS` in `src/bin/sidecar.rs`) returns `502 Bad Gateway` to the client, with these exceptions for
the single-part finalize, decided by the control plane's status. A `400` (the control plane rejected the content: size
against the policy, MIME, the stored-object check) on the `PUT` that created the object is relayed to the client as
`400` with the control plane's message, and the sidecar deletes the object best-effort, so a corrected retry goes
through as a fresh upload. A `404` (the version was deleted concurrently) on the creating `PUT` is answered `404` and
the object is deleted the same way. A `400` on a replay (the object already existed) is `409 content already
published`: the stored bytes were published and the control plane still rejects them. Every other `4xx` (`401`,
`403`, `408`, `429`, ...) and `5xx` is `502` and the object is **kept** — another sidecar may already have finalized
it, e.g. during a secret rotation — and a retry of the same `PUT` is allowed and is never turned into `409`. The backend write itself
is **create-exclusive, not overwrite-safe**: `StorageBackend::publish_exclusive` writes only the *first* time a
given backend path is empty — a retried `PUT` to the same signed URL never overwrites bytes already landed there,
it reports `created: false` and leaves the stored blob untouched. The documented recovery is still to retry the `PUT`
with the same bytes: the sidecar reads the stored object back and compares its hash with the retry's. If the earlier
publish landed but finalize never ran, a matching retry finalizes and the handler answers `200` (a benign retry has
converged); a retry whose bytes differ from the stored object is answered `409` and finalizes nothing. A replayed
`PUT` therefore re-reads the whole stored object. If the version was already finalized, the outcome converges
**regardless of the token's bind mode**: a replay whose size and content hash match the stored version is answered
`200` — for a token carrying the `bind_on_finalize` claim
(auto-bind `POST /files` path) a bind won at finalize replays as `bound` from the persisted `bound_on_finalize`
flag (only a won bind is sticky), while a lost CAS follows the file's *current* `content_id` on every replay: `bound`
while the file points at this version, `conflict` with the current ETag otherwise (F4 in the concurrency-and-failure
model); a manual-bind token converges the same way, simply with no bind-outcome headers to report, exactly as on
the first call. A mismatched replay (different size or hash) is answered
`409` — never a silent overwrite. Two finalize callbacks racing for the same version converge the same way: the one
that loses the `pending → available` CAS re-reads the version and applies the same size/hash comparison. For the
finalize case specifically, the version may already be correctly finalized server-side even though the client saw a
transient `502` on a preceding attempt (re-verify via `GET /files/{id}/versions` before assuming failure).

## Cleanup (not running yet)

There is **no background worker**: the gear schedules no periodic sweep. Retention rules, abandoned `pending`
versions, expired multipart sessions and expired idempotency keys are therefore **not cleaned up or enforced** until a
separate cleanup job exists. `CleanupEngine::run_sweep` (`src/domain/cleanup.rs`) is kept for that job to call; the
description below is how it behaves when invoked. Each step is **best-effort**: a failure in one step is logged at
`warn` and does not abort the rest of the sweep, and every operation is written to be safely idempotent under
concurrent sweeps (no cross-instance leader election exists today).

Each invocation has an overall time budget. Rather
than taking exactly one batch per step and stopping, one tick repeats the four steps below as further passes, in
the same order, each pass taking one more batch per step that has not yet run out of candidates — until either
every step is exhausted (its last batch came back shorter than its page size, or its query/delete errored) or the
budget runs out. The very first pass always completes in full regardless of the budget, so even
a very low time budget still processes one batch per step every tick. `run_sweep`'s returned
`SweepResult` carries `budget_exhausted` (`true` when the tick stopped on the time budget rather than because
every step was exhausted) and `elapsed_ms` (the tick's wall-clock duration), both logged alongside the existing
tallies.

Two of the four steps (abandoned-pending, expired-multipart) keep a keyset cursor for this
purpose that is **local to one tick**: a candidate a batch could not actually reclaim this pass (a still-active
multipart session, a transient per-row error) does not block the rest of that step's backlog for the remainder of
the tick, and every new tick starts each of these two steps over from its oldest candidate again — so a
candidate that was blocked in one tick is retried once whatever was blocking it has cleared, rather than being
skipped forever. The retention-expiry step is the exception: its `file_id` cursor is **persisted on the
`CleanupEngine`** and survives across ticks, because a full table scan can legitimately take longer than one
tick's budget on a large deployment, and restarting it from the beginning every tick would starve files later in
`file_id` order. The idempotency-key step needs no cursor at all — each batch deletes the rows it selects, so the
next call (later in the same tick, or the next tick) naturally sees the next-oldest backlog.

The sweep runs **four** steps, in this order:

1. **Abandoned-pending sweep** (`cpt-cf-file-storage-fr-orphan-reconciliation`) — deletes `file_versions` rows still
   `pending` (pre-registered but never finalized) older than the orphan grace period, best-effort deletes their
   backend blobs, and additionally deletes the parent `files` row too if reclaiming its last pending version leaves it
   a permanent zero-version orphan (no versions left **and** `content_id IS NULL`, and no blocking
   in-progress/completing multipart session for that file). A separate phase for `files` rows that never had a version
   is **not implemented**: it is planned together with the cleanup job (see
   [retention-cleanup.md](./features/retention-cleanup.md), Sweep Versionless Files). A composite `POST /files` and a
   multipart abort do not leave such rows: the file and its pending version are committed in one transaction, and a
   failed initiate or an abort removes the version and then the file (see
   [concurrency-and-failure-model.md](./concurrency-and-failure-model.md) §2.2 M1).
2. **Expired-multipart sweep** — aborts `multipart_uploads` sessions whose `expires_at` has passed: those still
   `in_progress`, **and** those left `completing` by a completer that died, once their lease has expired too (a
   live lease is never reaped mid-assembly). It wins the session's own `→ aborted` CAS first (racing a concurrent
   `complete`/user-`abort`), and only on winning that race does it tell the backend to discard the in-progress
   upload and delete the associated pending version row. The backend-side abort is best-effort and is **not**
   retried by a later pass — see the `AbortIncompleteMultipartUpload` lifecycle rule required under
   [`s3_backends`](#s3_backends) above.
3. **Retention-expiry sweep** (`cpt-cf-file-storage-fr-retention-policies`) — keyset-paginated (500 files per page,
   `RETENTION_SWEEP_BATCH`) scan of every file across every tenant, evaluated against all stored retention rules
   (tenant/user/file scope; age, inactivity, or custom-metadata-value criteria, OR-combined). A matching file is
   deleted through the same transactional-outbox path a user-initiated `DELETE` uses, so a `file.deleted` event is
   still emitted. Skipped entirely (no file scan at all) when zero retention rules are configured, for cheapness.
4. **Idempotency-key GC** — deletes `idempotency_keys` rows past their `expires_at` (governed by
   `idempotency_ttl_secs`). Deliberately does **not** touch `audit_outbox`/`events_outbox`: rows in those tables are
   inserted with `published_at = NULL` and nothing in this gear ever sets it — no relay currently drains either
   outbox table, so an age-based purge here would silently drop rows that were never delivered. Both tables
   therefore grow without bound until an `EventBroker` relay exists to drain them.

   **Operational signal**: since nothing currently drains either table, watch the trend rather than a fixed
   threshold — the row count in `audit_outbox`/`events_outbox` (or, more precisely, the count of rows with
   `published_at IS NULL`, which today is effectively all of them) and the age of the oldest unpublished row
   (`MIN(occurred_at) WHERE published_at IS NULL`, the same predicate the `audit_outbox_unpublished_idx` partial
   index — and its `events_outbox` counterpart — already covers). Steady growth by itself is **expected** and not
   an incident: it is the direct consequence of the relay not existing yet, not a bug in the sweep or in
   idempotency-key GC. What warrants operator attention is growth that looks abnormal for the deployment's actual
   write volume, or an oldest-unpublished-row age that keeps climbing with no plateau — either can eventually
   become a storage or query-performance problem for the table. Until the `EventBroker` relay ships, the only
   available mitigation is manual: archive/export the older rows out of `audit_outbox`/`events_outbox` (e.g. to
   cold storage) and delete them from the live table, accepting that the corresponding events are then permanently
   undelivered — a deliberate, occasional maintenance action, not something this gear automates. Escalate instead
   of deleting when the growth rate looks wrong for the deployment, since that usually points at a different
   problem (e.g. an unexpectedly high write rate) rather than at the relay simply not existing yet.

## Idempotent-create semantics

`POST /files` accepts an optional `idempotency_key`. A retry with the same key, by the same `(tenant_id, owner_kind,
owner_id)`, within `idempotency_ttl_secs`, returns the original response instead of creating a second file — guarded
by checks that must all pass:

- **Subject binding**: the stored row's `subject_id` (the authenticated caller who created the key) must match
  `ctx.subject_id()` on replay; a mismatch is `Forbidden`, not a silent fresh-create fallthrough. Pre-migration rows
  are backfilled with the nil UUID, which can never match a real subject.
- **Request-body binding**: a SHA-256 `request_hash` over the identity-relevant fields (`owner_kind`, `owner_id`,
  `name`, `gts_file_type`, `mime_type`, `custom_metadata`) is recomputed on replay and compared; a mismatch is
  `409 Conflict` ("idempotency key reused with a different request body"), rather than silently replaying the
  original ticket for a request the caller never actually made.
- **Bind-mode binding**: `bind` is deliberately excluded from `request_hash` (so a key minted before this flag
  existed still matches), but a replay that changes it is rejected with the same `409` as an outright body mismatch —
  a caller can never silently flip whether the replayed upload auto-binds. A key stored before `bind` existed
  matches under any `bind` value and is replayed in manual mode, exactly as its original create issued it.
- **Owner binding**: a replay re-reads the file's *live* owner and requires it to still match the request's
  `(owner_kind, owner_id)` — closing the gap where `POST /files/{id}/transfer` changes the file's owner without
  touching an already-stored ticket. A stale-owner replay is `409`, not a still-valid ticket.
- **Version-state binding**: a replay is rejected with `409` if the ticket's target version is no longer `pending`
  — i.e. an earlier, successfully delivered `PUT`+finalize already completed it and only the `201` response back to
  the client was lost. Re-minting a fresh upload token against content that already exists is not what a replay is
  for.

Deletion is deliberately **not** one of these replay cases: `idempotency_keys.file_id` carries `ON DELETE CASCADE`
from `files`, so deleting a file also deletes its idempotency key. A subsequent retry with the same key then finds
no stored ticket at all and creates a brand-new file, exactly as if the key had never been used — not a `409`.

See the `idempotency_keys` table (`m20260701_000001_p2_initial`, extended by `m20260706_000001_idempotency_subject_id` and `m20260706_000002_idempotency_request_hash`) and `docs/api.md`'s `409` summary for the wire-level contract.

## Upgrading from v0.2.x and rolling back

This release adds two additive migrations, applied in this order: `m20261008_000001_listing_indexes` (the
`file_versions_file_created_idx` and `files_owner_listing_v2_idx` listing indexes, the latter superseding
`files_owner_listing_idx`) and `m20261014_000001_upload_flow`. The second adds new `multipart_uploads` columns
— `auto_bind`, `lease_until`, `lease_owner`, `complete_result`, `backend_id`, `backend_path` — and the `completing`
state in its CHECK (on SQLite it rebuilds `multipart_uploads`, carrying rows and parts over), plus the
`idempotency_keys_file_idx` index and the `file_versions.bound_on_finalize` column. For existing multipart sessions
`backend_id`/`backend_path` are copied from the session's version row; a session an old instance starts after the
migration has them NULL and is read through its version row and the storage layout. Existing rows keep working: new
columns are nullable or default to the old behaviour. The sweep indexes for the cleanup job (`multipart_uploads_sweep_idx`,
`files_versionless_sweep_idx`) come with a later migration, not this release. No released migration changes.

**Mixed-version window.** Run the migrations once, then roll the fleet. An older (v0.2.x) instance tolerates the new
schema, with two exceptions to keep short: it rejects a multipart session in the new `completing` state as
`invalid multipart upload state` (a `complete` in flight on a new instance), and it never auto-binds. An auto-bind
upload whose finalize or `complete` lands on an old instance therefore ends unbound; a retry on a new instance answers
`bind_state: "manual"`, and the client calls `POST /files/{id}/bind` itself. Roll the control plane and the sidecars
together — the sidecar forwards the new finalize headers and the control plane accepts the new token grace — and keep
the window as short as a normal rolling restart.

**Rolling back the binary.** An older binary refuses to start against a database that has migrations it does not
know (sea-orm reports `Migration file of version '…' is missing`), so the order is: with the **new** binary, run
`m20261014_000001_upload_flow` down, then `m20261008_000001_listing_indexes` down (reverse of the apply order), then
deploy the old binary. The down step of `m20261014_000001_upload_flow` folds any session in `completing` into
`aborted` (the narrow pre-upgrade CHECK cannot hold it), narrows the CHECK again, and drops the new columns and the
index, so stored completion snapshots, leases and the `bound_on_finalize` flag are lost; finish or abort in-flight
multipart uploads before rolling back. Files and versions themselves are untouched by either direction.

Three consequences of rolling back deserve to be planned for. First, once `m20261014_000001_upload_flow` is down the
new binary cannot work against `file_versions` and `multipart_uploads` (its entities read the dropped columns), so
stop the new fleet before running `down`; between `down` and the start of the old binary the service is unavailable.
Second, the old binary ignores the auto-bind claim: single-part uploads from `POST /files` with `bind: auto` that are
finalized after the rollback stay unbound (the client binds manually), and so does a replay of an idempotency key.
Third, `down` moves `completing` to `aborted` in the database only — the multipart upload on the backend and the
pending version stay and nobody cleans them up, which makes finishing or aborting in-flight multipart uploads a
requirement of the rollback, not a recommendation.

## Storage quota (not enforced)

`FileService` and `MultipartService` both accept an optional `quota_client: Option<Arc<dyn QuotaClient>>` and call
`check_quota` / `check_quota_bytes` (`src/domain/service/create.rs`, `src/domain/multipart_service.rs`) before every
storage-increasing operation (`create_file`, `presign_version`, multipart initiate) — see the `QuotaClient` trait in
`src/infra/external_clients.rs`. That consumer-side port is designed to fail **closed**: if a wired client's
`check_storage_quota` call returns an error, the error propagates and the request is denied (see
`tests/enforce_test.rs`).

There is no config knob for this in the table above because there is nothing to configure yet — `gear.rs`
unconditionally constructs both services with `quota_client: None`. When `None`, `check_quota`/`check_quota_bytes`
short-circuit to `Ok(())`. **Storage quota is not enforced in any deployment**: the effective behavior is permissive
/ fail-**open**, not the fail-closed behavior the port was designed for. No `QuotaClient` implementation exists to
wire in — the Quota Enforcement gear (`gears/system/quota-enforcement/`) is docs-only (PRD/DESIGN/ADRs, no Rust
crate, no SDK). **Operators must not assume any storage limit is in effect** until this is wired.

Similarly, there is no usage reporter wired: `FileService`, `MultipartService`, and `CleanupEngine` all accept an
optional usage-reporting sink and `gear.rs` constructs every one of them with `None`, so no usage deltas are ever
reported anywhere today.

Content-hash modes (whole-object SHA-256 for single-part uploads; a multipart offset-manifest composite SHA-256 for
multipart uploads) are implemented — see [ADR-0006](./ADR/0006-cpt-cf-file-storage-adr-content-hash-modes.md) and
`docs/features/content-hash-modes.md`. The `hash_mode`/`part_count` columns and the `version_hash_manifest` table
were added by migration `m20260707_000001_content_hash_modes`.

## The `SignatureProvider` / `SignatureVerifier` abstraction

Signed URLs use ECDSA P-256/SHA-256 through `toolkit_security::crypto`. Private keys
use PKCS#8 DER, public keys use 65-byte uncompressed SEC1 points, and signatures use
ASN.1 DER. The private two-part token codec is not JWT or PASETO. Signing failures
propagate as errors. Sidecar startup checks the public-key length, uncompressed
encoding and backend availability before creating HTTP clients. The backend checks
the curve point during signature verification. The sidecar explicitly installs
the facade-selected TLS provider.

### Ed25519 to P-256 migration

This change requires new keys and coordinated control-plane/sidecar deployment.
Old Ed25519 seeds, public keys and outstanding tokens are not accepted. File contents,
content hashes and metadata do not change.

1. Generate a new P-256 private key with an embedded public point, and export PKCS#8 DER:

   ```sh
   umask 077
   openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out signing-key.pem
   openssl pkcs8 -topk8 -nocrypt -in signing-key.pem -outform DER -out signing-key.pk8
   ```

   Provision the base64url encoding without padding as secret `signing_key_pkcs8`.
   Use an approved provisioning process where the deployment's FIPS policy requires one.
   Do not reuse the public fixture shipped in local/E2E configs.
2. Stop issuing old signed URLs and drain outstanding uploads/downloads. Wait for the
   maximum lifetime of old URLs and multipart sessions, including callback grace, or
   schedule an outage that explicitly invalidates outstanding URLs and sessions.
3. Configure all control-plane replicas with the same new key. Obtain its
   `sidecar_public_key` startup log value and configure `FS_SIDECAR_PUBLIC_KEY` on all
   sidecars. Roll both components together; mixed Ed25519/P-256 fleets do not interoperate.
4. Resume traffic and obtain fresh URLs. For rollback, restore both old binaries and
   their matching old key configuration; newly issued P-256 tokens will be invalid.

The old `signing_key_seed` / `require_signing_key_seed` settings have been replaced by
`signing_key_pkcs8` / `require_signing_key`. A seed is not a PKCS#8 key and must not be
copied into the new setting. The new required-key flag remains true by default.

### Building the sidecar with FIPS policy

Use the existing toolkit feature; an ordinary build does not enable FIPS policy:

```sh
cargo build --locked -p cf-gears-file-storage --bin sidecar --features toolkit/fips
```

This enables toolkit-security's native backend policy and toolkit-http's FIPS
configuration. On Windows the production process still requires system FIPS policy;
on all targets the module's supported operating environment remains a deployment requirement.
