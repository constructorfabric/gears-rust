# FileStorage — Operational Configuration

This document covers every `FileStorageConfig` field (`gears/file-storage/file-storage/src/config.rs`), the sidecar's
own `FS_SIDECAR_*` environment variables (`gears/file-storage/file-storage/src/bin/sidecar.rs`), and the background
`CleanupEngine` sweep those config fields drive. It exists to close a gap flagged in P2 remediation item 3.8: several
of these knobs (in particular the security-relevant ones) had no operator-facing documentation of their default,
production recommendation, or failure mode.

Every field below is verified against the current `default_*()` function or `main()` env-var parsing as of this doc
pass — see the file/line pointers in each entry.

<!-- toc -->

- [Control-plane config: `FileStorageConfig`](#control-plane-config-filestorageconfig)
- [Sidecar config: `FS_SIDECAR_*` environment variables](#sidecar-config-fs_sidecar_-environment-variables)
- [The background cleanup sweep](#the-background-cleanup-sweep)
- [Idempotent-create semantics](#idempotent-create-semantics)
- [Storage quota (not enforced)](#storage-quota-not-enforced)
- [The `SignatureProvider` / `SignatureVerifier` abstraction](#the-signatureprovider--signatureverifier-abstraction)

<!-- /toc -->

## Control-plane config: `FileStorageConfig`

All fields are `#[serde(default = "…")]`, so an operator's YAML only needs to override what it wants to change; a
gear started with no `file-storage` config section at all gets every default below. `FileStorageConfig::validate()`
(called at gear init, before anything is wired up) rejects two specific invalid combinations — noted inline.

| Field | Default | Source |
|---|---|---|
| `default_url_ttl_secs` | `900` (15 min) | `default_default_url_ttl_secs()`, `config.rs:184` |
| `max_url_ttl_secs` | `604800` (7 days) | `default_max_url_ttl_secs()`, `config.rs:190` |
| `sidecar_base_url` | `"http://localhost:8087"` | `default_sidecar_base_url()`, `config.rs:195` |
| `default_page_size` | `50` | `default_page_size()`, `config.rs:199` |
| `max_page_size` | `1000` | `default_max_page_size()`, `config.rs:203` |
| `storage_root` | `"./.file-storage-data"` | `default_storage_root()`, `config.rs:207` |
| `signing_key_seed` | `None` (no `#[serde(default = …)]`, just `Option::default()`) | `config.rs:53-54` |
| `require_signing_key_seed` | `true` | `default_require_signing_key_seed()`, `config.rs:227` |
| `idempotency_ttl_secs` | `86400` (24h) | `default_idempotency_ttl_secs()`, `config.rs:211` |
| `orphan_grace_secs` | `3600` (1h) | `default_orphan_grace_secs()`, `config.rs:215` |
| `sweep_interval_secs` | `3600` (1h) | `default_sweep_interval_secs()`, `config.rs:219` |
| `enable_background_sweep` | `true` | `default_enable_background_sweep()`, `config.rs:223` |
| `enable_in_memory_backend` | `false` (bare `#[serde(default)]`) | `config.rs:106-107` |

### `default_url_ttl_secs`
Default TTL (seconds) baked into every signed URL the control plane mints (`900` = 15 minutes), unless the caller's
presign request or the code path explicitly asks for something else. Bounds the "stale-permission window" — how long
a URL remains valid after the authorization decision was made at signing (no per-token revocation exists in P1).
**Production recommendation**: keep short (minutes, not hours) for anything not explicitly meant to be long-lived or
shareable; raise only for known bulk/batch workflows. **Misconfiguration risk**: too long → a leaked/logged URL stays
exploitable for the full window; too short → legitimate slow uploads/downloads may need to be re-presigned mid-flight
(no such retry-on-expiry logic exists in the SDK/handlers today, so a very small value can break large transfers).

### `max_url_ttl_secs`
Hard ceiling (seconds) the control plane will mint any signed URL to (`604800` = 7 days); enforced by `Issuer::issue`
which clamps `exp` down to `now + max_url_ttl_secs` regardless of what was requested. **Production recommendation**:
leave at the 7-day default or lower for stricter environments; do not raise without a specific long-lived/anonymous-
sharing use case (P3 FileShare is the intended mechanism for that, not a raised ceiling here). **Misconfiguration
risk**: raising it widens the window during which a leaked URL is exploitable, with no revocation mechanism to claw
it back.

### `sidecar_base_url`
The externally-reachable base URL of the data-plane sidecar that every signed URL points at (default assumes a
sidecar on `localhost:8087`, i.e. **local dev only**). **Production recommendation**: must be set to the sidecar's
real public/internal address for the deployment topology (e.g. behind a load balancer in front of multiple sidecar
replicas). **Misconfiguration risk**: every signed upload/download URL embeds this host — if it is wrong or
unreachable from the client, every content operation fails even though the control plane itself is healthy; this is
easy to overlook because control-plane health checks and metadata CRUD (`GET /files`, etc.) will look fine.

### `default_page_size` / `max_page_size`
Pagination defaults/ceiling for `GET /files` (and similar list endpoints) — `50` / `1000` respectively.
**Production recommendation**: the defaults are reasonable starting points; raise `max_page_size` only if clients
have a proven need for larger pages and the DB/latency budget supports it. **Misconfiguration risk**: a very large
`max_page_size` lets a caller force an expensive, unbounded-feeling listing query; a `default_page_size` larger than
`max_page_size` would be self-contradictory (not validated by `FileStorageConfig::validate()` — not asserted to be
rejected at startup).

### `storage_root`
Local filesystem root for the default `local-fs` backend (default `./.file-storage-data`, i.e. **relative to the
process's working directory** — not durable across container image rebuilds unless mounted). **Production
recommendation**: point at a durable, backed-up volume mount; never leave at the relative default in a containerized
deployment. **Misconfiguration risk**: content written under an ephemeral/container-local path is lost on
pod/container recreation — this is a **silent data-loss** risk since writes will appear to succeed.

### `signing_key_seed`
Base64url-encoded 32-byte Ed25519 seed for the URL-signing keypair. When set, the keypair (and the public key the
sidecar verifies against) is **stable across restarts**; when absent, `gear.rs::init` generates an ephemeral key
every boot (logged at `info` level with an explicit warning). **Production recommendation**: always set this in any
real deployment, and treat it as a secret (same handling tier as any other private key material) — never log or
serialize it (`FileStorageConfig`'s manual `Debug` impl redacts it deliberately). **Multi-replica warning**: every
replica must be configured with the **same** seed. If replicas each generate their own ephemeral key (i.e. the seed
is unset in a multi-replica deployment), signed URLs minted by one replica fail verification at the sidecar (which
is configured with only one public key via `FS_SIDECAR_PUBLIC_KEY`) — this looks like intermittent, replica-dependent
upload/download failures. `require_signing_key_seed` (below) exists specifically to fail fast on this misconfiguration
instead of degrading silently into that failure mode.

### `require_signing_key_seed`
When `true` (the default), `FileStorageConfig::validate()` makes gear init **fail fast** if `signing_key_seed` is
absent, instead of silently minting an ephemeral per-boot key. **Production recommendation**: leave at `true`
everywhere except local dev/test harnesses that construct a `FileStorageConfig` directly and intentionally want the
ephemeral-key behavior — set `false` explicitly there. **Misconfiguration risk**: setting `false` in a real
multi-replica deployment removes the fail-fast guard and re-opens the "different key per replica" failure mode
described above, deferred from a loud startup error to a confusing runtime symptom.

### `idempotency_ttl_secs`
Window (seconds, default `86400` = 24h) an `idempotency_keys` row (from `POST /files`'s `idempotency_key`) remains
valid for replay-detection; after this, a retry with the same key is treated as a brand-new request. Expired rows
are reclaimed by the cleanup sweep's step 4 (see below). **Production recommendation**: size to the longest
realistic client retry window (default is generous for most HTTP retry policies). **Misconfiguration risk**: too
short → a legitimately delayed retry (e.g. after a long client-side backoff) creates a duplicate file instead of
being deduplicated; too long → more rows accumulate between sweep passes (bounded by `sweep_interval_secs`, not a
correctness issue, just storage/index bloat).

### `orphan_grace_secs`
Grace period (seconds, default `3600` = 1h) a `pending` version or an expired multipart session must age past
before the cleanup sweep reclaims it. **Production recommendation**: the default balances "reclaim abandoned uploads
promptly" against "don't race a slow-but-legitimate in-flight upload." **Misconfiguration risk**: too short → a
slow client upload can have its `pending` version reclaimed (and blob deleted) out from under it mid-upload,
surfacing as a finalize `404`/`400`; too long → abandoned pending rows and their blobs linger longer, using storage.

### `sweep_interval_secs`
How often (seconds, default `3600` = 1h) the background cleanup sweep fires, when `enable_background_sweep` is
`true`. `FileStorageConfig::validate()` **rejects** `sweep_interval_secs == 0` combined with
`enable_background_sweep == true` at startup (a zero interval would otherwise spin the sweep loop tightly, pegging
the runtime and flooding logs). **Production recommendation**: the 1-hour default is reasonable for most deployments;
tighten it if orphan reconciliation / retention-driven deletion needs to be closer to real-time. **Misconfiguration
risk**: too long → orphaned pending versions, expired multipart sessions, retention-expired files, and expired
idempotency keys all accumulate for longer between passes (storage growth, and retention-policy compliance windows
run wider than the policy nominally states).

### `enable_background_sweep`
When `true` (**the default — flipped in Tier 1 item 1.4**; previously defaulted to `false`), the cleanup sweep loop
starts at gear init. **Production recommendation**: leave at `true` in every real deployment; set `false` only in
test/dev harnesses that construct a `FileStorageConfig` directly (not via YAML) and need fully deterministic
behavior (no background task racing test assertions). **Misconfiguration risk**: `false` in production means **no**
orphan reconciliation, **no** expired-multipart cleanup, **no** retention-policy enforcement, and **no**
idempotency-key garbage collection ever run — pending versions and abandoned multipart sessions accumulate
indefinitely, retention rules become inert (a compliance-relevant silent failure, since a configured retention
policy will appear to exist via `GET /retention-rules` but never actually delete anything), and `idempotency_keys`
grows without bound.

### `enable_in_memory_backend`
When `true` (default `false`), an additional non-durable `memory` backend (`memory-fs` or similar id) is registered
alongside the default `local-fs` backend. **Production recommendation**: leave at `false` in any deployment where
data loss is unacceptable — the in-memory backend loses all content on restart. Its only legitimate use is
dev/test scenarios that want a second backend id to exercise multi-backend code paths (e.g. `migrate_backend`)
without provisioning real durable storage. **Misconfiguration risk**: enabling it in production, combined with a
file or policy that routes content onto it, is **silent, guaranteed data loss** on the next restart — `migrate_backend`
additionally requires the caller's `ADMIN_POLICY` authorization scope (not just `WRITE`) specifically to make this
an explicit, elevated-privilege action rather than an accident.

## Sidecar config: `FS_SIDECAR_*` environment variables

The sidecar is a separate binary/process (`src/bin/sidecar.rs`) with its own env-var configuration — it does **not**
share `FileStorageConfig`. All of these are read once in `main()`.

| Variable | Default | Notes |
|---|---|---|
| `FS_SIDECAR_ADDR` | `0.0.0.0:8087` | Bind address. |
| `FS_SIDECAR_PUBLIC_KEY` | **required, no default** | Base64url Ed25519 public key; must match the control plane's `signing_key_seed`-derived keypair (see above). Startup fails (`anyhow::anyhow!`) if unset or malformed. |
| `FS_SIDECAR_BACKEND_ROOT` | `./.file-storage-data` | Local-fs backend root — same durability caveat as the control plane's `storage_root`; the two should point at the **same** underlying storage for a single-backend deployment, or the sidecar will read/write blobs the control plane's metadata doesn't expect to find there. |
| `FS_SIDECAR_CONTROL_URL` | `http://localhost:8080` | Base URL of the control plane, used for the finalize/report-part callbacks. Setting it to the **empty string** explicitly disables the callback (dev/test only) — uploaded versions then stay `pending` forever, since nothing ever calls finalize; production must always set this to a reachable control-plane URL. |
| `FS_SIDECAR_MAX_BODY_BYTES` | `5368709120` (5 GiB) | Raises axum's blanket request-body floor (default 2 MiB) for the `PUT` route. This is a transport-layer ceiling only — the real per-request limit is the signed token's `max_size`/`exact_size` claim. **Misconfiguration risk**: setting it below the largest policy-permitted single-part upload causes legitimate uploads to be rejected at the transport layer before the token-level check even runs. |
| `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` | `10` | Total request timeout for the sidecar → control-plane finalize/report-part callbacks. |
| `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` | `5` | Connect timeout for the same callbacks. Together with the timeout above, bounds how long a client's upload request can be held open by an unreachable or hung control plane (P2 remediation 1.5) — before this existed, a hung control plane could block the client indefinitely. **Misconfiguration risk**: too low in a high-latency network path causes spurious `502 Bad Gateway` responses to clients on otherwise-successful uploads; too high re-opens the "held open indefinitely" problem these timeouts exist to close. |

A failed finalize/report-part callback (after the sidecar's retry budget, see `post_with_retry` /
`CALLBACK_MAX_ATTEMPTS` in `src/bin/sidecar.rs`) returns `502 Bad Gateway` to the client. The upload itself is
**idempotent** (the backend `PUT` is overwrite-safe), so the documented recovery is: the client retries the upload
via a fresh `PUT` to the same signed URL, or — for the finalize case specifically — the version may already be
correctly finalized server-side even though the client saw a transient `502` on a preceding attempt (re-verify via
`GET /files/{id}/versions` before assuming failure).

## The background cleanup sweep

`CleanupEngine::run_sweep` (`src/domain/cleanup.rs`) is the single entry point for the whole background lifecycle
job the gear schedules on a `sweep_interval_secs` timer when `enable_background_sweep` is `true` (see `gear.rs`).
Each step is **best-effort**: a failure in one step is logged at `warn` and does not abort the rest of the sweep, and
every operation is written to be safely idempotent under concurrent sweeps (no cross-instance leader election exists
in P2 — every replica runs its own sweep independently; P3 is expected to add coordination).

The sweep runs **four** steps, in this order:

1. **Abandoned-pending sweep** (`cpt-cf-file-storage-fr-orphan-reconciliation`) — deletes `file_versions` rows still
   `pending` (pre-registered but never finalized) older than `orphan_grace_secs`, best-effort deletes their backend
   blobs, and — **P2 remediation 2.8** — additionally deletes the parent `files` row too if reclaiming its last
   pending version leaves it a permanent zero-version orphan (no versions left **and** `content_id IS NULL`, and no
   blocking in-progress multipart session for that file). This zero-version file cleanup is not a separate fifth
   sweep step; it is folded into step 1's per-version cleanup.
2. **Expired-multipart sweep** — aborts `multipart_uploads` sessions still `in_progress` whose `expires_at` has
   passed: wins the session's own `in_progress → aborted` CAS first (racing a concurrent `complete`/user-`abort`),
   and only on winning that race does it tell the backend to discard the in-progress upload and delete the
   associated pending version row.
3. **Retention-expiry sweep** (`cpt-cf-file-storage-fr-retention-policies`) — keyset-paginated (500 files per page,
   `RETENTION_SWEEP_BATCH`) scan of every file across every tenant, evaluated against all stored retention rules
   (tenant/user/file scope; age, inactivity, or custom-metadata-value criteria, OR-combined). A matching file is
   deleted through the same transactional-outbox path a user-initiated `DELETE` uses, so a `file.deleted` event is
   still emitted. Skipped entirely (no file scan at all) when zero retention rules are configured, for cheapness.
4. **Idempotency-key GC** (**P2 remediation 1.9**) — deletes `idempotency_keys` rows past their `expires_at`
   (governed by `idempotency_ttl_secs`). Deliberately does **not** touch `audit_outbox`/`events_outbox`: those rows'
   `published_at` stays `NULL` until the Tier 4 EventBroker relay exists, so an age-based purge there would silently
   drop undelivered rows.

## Idempotent-create semantics

`POST /files` accepts an optional `idempotency_key`. A retry with the same key, by the same `(tenant_id, owner_kind,
owner_id)`, within `idempotency_ttl_secs`, returns the original response instead of creating a second file — guarded
by two checks (both must pass, added across two separate remediations):

- **Subject binding** (P2 remediation 0.10): the stored row's `subject_id` (the authenticated caller who created the
  key) must match `ctx.subject_id()` on replay; a mismatch is `Forbidden`, not a silent fresh-create fallthrough.
  Pre-migration rows are backfilled with the nil UUID, which can never match a real subject.
- **Request-body binding** (P2 remediation 2.1): a SHA-256 `request_hash` over the identity-relevant fields (`name`,
  `gts_file_type`, `mime_type`, `custom_metadata`) is recomputed on replay and compared; a mismatch is `409 Conflict`
  ("idempotency key reused with a different request body"), rather than silently replaying the original ticket for a
  request the caller never actually made.

See `docs/migration.sql`'s `idempotency_keys` table and `docs/api.md`'s `409` summary for the wire-level contract.

## Storage quota (not enforced)

**Implementation status (P2, dated 2026-07-07).** `FileService` and `MultipartService` both accept an optional
`quota_client: Option<Arc<dyn QuotaClient>>` and call `check_quota` / `check_quota_bytes`
(`src/domain/service/create.rs`, `src/domain/multipart_service.rs`) before every storage-increasing operation
(`create_file`, `presign_version`, multipart initiate) — see the `QuotaClient` trait in
`src/infra/external_clients.rs`. That consumer-side port is designed to fail **closed**: if a wired client's
`check_storage_quota` call returns an error, the error propagates and the request is denied (see
`tests/enforce_test.rs`).

There is no config knob for this in the table above because there is nothing to configure yet — `gear.rs`
unconditionally constructs both services with `quota_client: None` (`TODO(P2)`, Tier 1 item 1.4). When `None`,
`check_quota`/`check_quota_bytes` short-circuit to `Ok(())`. **This means storage quota is not enforced in any
deployment today**: the effective behavior is permissive / fail-**open**, not the fail-closed behavior the port
was designed for. No `QuotaClient` implementation exists to wire in — the Quota Enforcement gear
(`gears/system/quota-enforcement/`) is docs-only (PRD/DESIGN/ADRs, no Rust crate, no SDK). **Operators must not
assume any storage limit is in effect** until this is wired.

## The `SignatureProvider` / `SignatureVerifier` abstraction

Signing and verification of the URL-signing token are behind an in-house trait pair
(`infra::signed_url::SignatureProvider` / `SignatureVerifier`), not called directly against a hard-wired crypto
library. The shipped P1 implementation is Ed25519 (`Issuer::from_seed`/`Issuer::generate`), codec-equivalent to
PASETO `v4.public` (see `docs/api.md`'s "Signed URLs" section). The abstraction exists specifically for **FIPS
posture**: a FIPS-validated deployment needs the sign/verify primitive to run inside a FIPS-validated module (the
platform's `rustls-corecrypto-provider`); the trait boundary lets that primitive be swapped (e.g. for ECDSA P-256)
without any change to the token's opaque wire format or any client-visible change. See
[ADR-0004](./ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md) "FIPS posture" for the full rationale.
