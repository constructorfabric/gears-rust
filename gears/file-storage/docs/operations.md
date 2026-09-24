# FileStorage — Operational Configuration

This document covers every `FileStorageConfig` field (`gears/file-storage/file-storage/src/config.rs`), the sidecar's
own `FS_SIDECAR_*` environment variables (`gears/file-storage/file-storage/src/bin/sidecar.rs`), and the background
`CleanupEngine` sweep those config fields drive.

Every field below is verified against the current `default_*()` function or `main()`'s env-var parsing — each entry
names the function (or environment variable) that sets its default, rather than a line number, since line numbers
drift with every edit.

<!-- toc -->

- [Control-plane config: `FileStorageConfig`](#control-plane-config-filestorageconfig)
- [Sidecar config: `FS_SIDECAR_*` environment variables](#sidecar-config-fs_sidecar_-environment-variables)
- [The background cleanup sweep](#the-background-cleanup-sweep)
- [Idempotent-create semantics](#idempotent-create-semantics)
- [Upgrading from v0.2.x and rolling back](#upgrading-from-v02x-and-rolling-back)
- [Storage quota (not enforced)](#storage-quota-not-enforced)
- [The `SignatureProvider` / `SignatureVerifier` abstraction](#the-signatureprovider--signatureverifier-abstraction)

<!-- /toc -->

## Control-plane config: `FileStorageConfig`

All fields are `#[serde(default = "…")]`, so an operator's YAML only needs to override what it wants to change; a
gear started with no `file-storage` config section at all gets every default below — **except that it still cannot
actually boot**: `require_signing_key_seed` defaults to `true` with `signing_key_seed` unset, and
`FileStorageConfig::validate()` fails gear init on exactly that combination (see `require_signing_key_seed` below).
A genuinely zero-config deployment is dev/test-only (set `require_signing_key_seed: false` there).
`FileStorageConfig::validate()` (called at gear init, before anything is wired up) rejects **eighteen**
invalid configurations — six missing-secret/zero-value guards (`sweep_interval_secs == 0` with the sweep enabled;
`default_url_ttl_secs`, `multipart_session_ttl_secs` or `multipart_complete_lease_secs` equal to `0`, instead of
silently using one second; `signing_key_seed` absent while required; `finalize_internal_secret` absent while
required); seven absolute
ceilings (`finalize_token_grace_secs` above `MAX_FINALIZE_TOKEN_GRACE_SECS`, 7 days; `max_page_size` above
`MAX_PAGE_SIZE_CEILING`, 1000; `max_url_ttl_secs` above `MAX_URL_TTL_CEILING`, 30 days; `multipart_session_ttl_secs`
above `MAX_MULTIPART_SESSION_TTL_SECS`, 30 days; `multipart_complete_lease_secs` above
`MAX_MULTIPART_COMPLETE_LEASE_SECS`, 1 day; `orphan_grace_secs` above `MAX_ORPHAN_GRACE_SECS`, 30 days;
`idempotency_ttl_secs` above `MAX_IDEMPOTENCY_TTL_SECS`, 30 days — see those fields below); four cross-field ordering invariants (`default_url_ttl_secs`
vs. `max_url_ttl_secs`; `default_page_size` vs. `max_page_size`; `default_url_ttl_secs` vs. `orphan_grace_secs`;
`multipart_session_ttl_secs` vs. `default_url_ttl_secs`); and one per-entry format check (each
`previous_signing_public_keys` entry must be a validly-formed, 32-byte Ed25519 public key — see that field below) —
noted inline below. A separate check (`max_url_ttl_secs` vs. `orphan_grace_secs`) only logs a `warn` rather than
rejecting, since the recommended defaults (7 days vs. 1 hour) would otherwise fail on every default deployment — see
`orphan_grace_secs` below. Function names (rather than line numbers) are used as source pointers throughout this
table since line numbers drift with every edit.

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
| `default_page_size` | `50` | `default_page_size()` |
| `max_page_size` | `1000` | `default_max_page_size()` |
| `storage_root` | `"./.file-storage-data"` | `default_storage_root()` |
| `signing_key_seed` | `None` (no `#[serde(default = …)]`, just `Option::default()`) | struct field default |
| `require_signing_key_seed` | `true` | `default_require_signing_key_seed()` |
| `idempotency_ttl_secs` | `86400` (24h) | `default_idempotency_ttl_secs()` |
| `orphan_grace_secs` | `3600` (1h) | `default_orphan_grace_secs()` |
| `sweep_interval_secs` | `3600` (1h) | `default_sweep_interval_secs()` |
| `enable_background_sweep` | `true` | `default_enable_background_sweep()` |
| `enable_in_memory_backend` | `false` (bare `#[serde(default)]`) | struct field default |
| `s3_backends` | `[]` (empty, bare `#[serde(default)]`) | struct field default |
| `default_backend_id` | `None` (bare `#[serde(default)]`) | struct field default |
| `finalize_internal_secret` | `None` (bare `#[serde(default)]`) | struct field default |
| `require_finalize_internal_secret` | `false` (bare `#[serde(default)]`) | struct field default |
| `previous_signing_public_keys` | `[]` (empty, bare `#[serde(default)]`) | struct field default |

### `default_url_ttl_secs`
Default TTL (seconds) baked into every signed URL the control plane mints (`900` = 15 minutes), unless the caller's
presign request or the code path explicitly asks for something else. Bounds the "stale-permission window" — how long
a URL remains valid after the authorization decision was made at signing (no per-token revocation exists). **Production
recommendation**: keep short (minutes, not hours) for anything not explicitly meant to be long-lived or
shareable; raise only for known bulk/batch workflows. **Misconfiguration risk**: too long → a leaked/logged URL stays
exploitable for the full window; too short → legitimate slow uploads/downloads may need to be re-presigned mid-flight
(no such retry-on-expiry logic exists in the SDK/handlers, so a very small value can break large transfers — though `finalize_token_grace_secs` below keeps an upload that outran this TTL *mid-stream* from failing at the finalize step).
`FileStorageConfig::validate()` enforces two ordering invariants on this field at startup: it must not exceed
`max_url_ttl_secs` (otherwise the very first URL minted with no explicit override already violates the ceiling the
control plane is supposed to enforce), and it must not exceed `orphan_grace_secs` (see that field below).

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
the cleanup sweep aborts the session once it passes. Deliberately much longer than `default_url_ttl_secs` — it is a
budget for a whole multi-GB upload, not for one signed URL. `FileStorageConfig::validate()` **rejects** a value
*below* `default_url_ttl_secs`: the per-part URLs are minted at initiate time with the default TTL, so a shorter
session lifetime would let a part URL outlive the session it belongs to and have `complete`'s defense-in-depth
expiry check reject an upload whose URLs were still technically valid. **Production recommendation**: size it to the
slowest legitimate upload you intend to support. **Misconfiguration risk**: too short → long uploads are aborted
mid-flight by the sweep; too long → abandoned sessions (and their backend multipart handles) linger before the
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
Pagination defaults/ceiling for `GET /files` (and similar list endpoints) — `50` / `1000` respectively.
**Production recommendation**: the defaults are reasonable starting points; raise `max_page_size` only if clients
have a proven need for larger pages and the DB/latency budget supports it. **Misconfiguration risk**: a very large
`max_page_size` lets a caller force an expensive, unbounded-feeling listing query; a `default_page_size` larger than
`max_page_size` would be self-contradictory — `FileStorageConfig::validate()` rejects this combination at startup.
`max_page_size` also has its own absolute ceiling, `MAX_PAGE_SIZE_CEILING` (`1000`, the same value as the shipped
default): `validate()` rejects any configured `max_page_size` above it, independent of the `default_page_size`
check, since it is otherwise the only bound on a single listing request's row count and response size.

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

**Rotation is zero-outage** if BOTH of the platform's independent acceptance points are given the old key during the
rotation window, not just one of them: the sidecar verifies a token against its own small ordered set of public
keys (`FS_SIDECAR_PUBLIC_KEY` plus, optionally, `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`), and — separately — the control
plane verifies the sidecar's finalize/report-part callbacks against its OWN small ordered set (the current seed's
key plus, optionally, `previous_signing_public_keys` below). The sidecar's set does nothing for the control plane's
own callback verification; each side needs the old key configured on its own terms. Procedure:

1. Generate the new seed and get its public key **before** touching anything else. The reliable way to do this
   without a dedicated derivation utility: start (or restart, in staging) one control-plane replica with the new
   seed and read its own startup log — `gear.rs::init` always logs
   `sidecar_public_key = <base64url>` (`"file-storage URL-signing public key (configure FS_SIDECAR_PUBLIC_KEY with
   this)"`) derived from whichever seed it booted with, precisely so this key never has to be computed by hand.
2. Roll out the sidecar fleet with the NEW key added to its set — either as the new `FS_SIDECAR_PUBLIC_KEY` with the
   old key moved into `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`, or left as `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` alongside the
   still-current primary. Order only changes verification cost (the primary is tried first), never correctness —
   every sidecar now accepts tokens signed by either key. This must complete before step 3: the control plane must
   never sign with a key no sidecar accepts yet.
3. Restart the control plane on the new seed — **every replica at once** (see the multi-replica warning above: a
   mixed-seed control-plane fleet is exactly the failure mode that warns about) — with the OLD seed's public key set
   in `previous_signing_public_keys`. Tokens already issued under the old seed keep verifying at the sidecar,
   because step 2 already taught the sidecars the old key too; their finalize/report-part callbacks keep verifying
   at the control plane too, because this step's `previous_signing_public_keys` now teaches the control plane the
   old key. Skipping `previous_signing_public_keys` here reproduces exactly the bug this field exists to close:
   every upload started before the restart would fail its finalize/report-part callback the instant the control
   plane comes back up, even though the sidecar fleet still honours its signed URL.
4. Only once `max_url_ttl_secs + finalize_token_grace_secs` (default 7 days + 1h) has passed since step 3, remove
   the old key from every sidecar's `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`/`FS_SIDECAR_PUBLIC_KEY` **and** from the
   control plane's `previous_signing_public_keys`. This is longer than the sidecar-only bound an earlier version of
   this procedure used (`max_url_ttl_secs` alone): a token minted under the OLD seed right up until this restart can
   carry an `exp` up to `max_url_ttl_secs` after it, and the control plane's finalize/report-part routes then accept
   that same token up to `finalize_token_grace_secs` *past* that `exp` (`Verifier::verify_with_grace` — the same
   slack that lets an ordinary slow-but-live upload finalize after its token's nominal TTL). Removing the old key
   any earlier can reject the finalize/report-part callback of a legitimate, still-in-flight upload that happened to
   straddle both its own token's TTL and this rotation window.

**Compromise** follows the same four steps, except step 4 happens immediately, as a deliberate invalidation of every
URL the old key could still authorize (including its finalize/report-part callback) rather than something to wait
out: a holder of the private key can mint a token for *any* `file_id`/`op`/`backend_path`, so containing that risk
outweighs preserving in-flight URLs — and their callbacks — signed under it. Concretely:
- Outstanding **download** URLs simply get re-issued (a fresh `GET`/`HEAD` against the control plane mints a new one
  under the new key).
- An in-flight **single-part upload** resumes via a repeat `POST /files` with the same `idempotency_key` — the
  replay re-signs the returned upload URL under the current (new) key rather than replaying the old one (see F1 in
  [concurrency-and-failure-model.md](./concurrency-and-failure-model.md)).
- An in-flight **multipart session** survives the rotation through its existing resume path: `GET
  /files/{id}/multipart/{upload_id}` re-signs the remaining parts' URLs under the current key, still capped by the
  session's own `expires_at` — no special-casing needed for a key rotation specifically.

### `previous_signing_public_keys`
Public keys of previously-active `signing_key_seed`s that the control plane's OWN finalize/report-part callback
verification (`FileService::verifier`, `Verifier::verify_with_grace`) still accepts, on top of the current seed's
key. Empty (`[]`) by default. Each entry is a base64url-encoded (`URL_SAFE_NO_PAD`) raw Ed25519 **public** key — the
same wire format as one element of the sidecar's `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` list — never a seed or any other
private key material; the control plane only ever needs to hold retired *public* keys, never the retired seed
itself. This is a second, independent acceptance point from the sidecar's own `FS_SIDECAR_PUBLIC_KEY`/
`FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`: that set only widens what the *sidecar* accepts for PUT/GET/part-upload requests,
and does nothing for the control plane's own verification of the sidecar's finalize/report-part callbacks — see
`signing_key_seed`'s **Rotation** procedure above, which this field is a step of. **Production recommendation**:
leave empty outside an active rotation window; populate it with exactly the retiring seed's public key per step 3
of the **Rotation** procedure, and clear it again once step 4 completes. **Misconfiguration risk**: leaving it unset
during a rotation reproduces the bug this field exists to close — every upload started before the control-plane
restart fails its finalize/report-part callback the instant the restart happens, even though the sidecar fleet
still honours the client's in-flight signed URL. `validate()` fails gear init on a malformed entry (bad base64, or
the wrong decoded length); harmless duplicates (of the current key, or within this list) are silently deduped with
a startup warning once the current key is actually known — see `FileService::with_previous_signing_public_keys`
and `infra::signed_url::dedupe_public_keys`.

### `require_signing_key_seed`
When `true` (the default), `FileStorageConfig::validate()` makes gear init **fail fast** if `signing_key_seed` is
absent, instead of silently minting an ephemeral per-boot key. **Production recommendation**: leave at `true`
everywhere except local dev/test harnesses that construct a `FileStorageConfig` directly and intentionally want the
ephemeral-key behaviour — set `false` explicitly there. **Misconfiguration risk**: setting `false` in a real
multi-replica deployment removes the fail-fast guard and re-opens the "different key per replica" failure mode
described above, deferred from a loud startup error to a confusing runtime symptom.

### `idempotency_ttl_secs`
Window (seconds, default `86400` = 24h) an `idempotency_keys` row (from `POST /files`'s `idempotency_key`) remains
valid for replay-detection; after this, a retry with the same key is treated as a brand-new request. Expired rows
are reclaimed by the cleanup sweep's step 4 (see below). **Production recommendation**: size to the longest
realistic client retry window (default is generous for most HTTP retry policies). **Misconfiguration risk**: too
short → a legitimately delayed retry (e.g. after a long client-side backoff) creates a duplicate file instead of
being deduplicated; too long → more rows accumulate between sweep passes (bounded by `sweep_interval_secs`, not a
correctness issue, just storage/index bloat). Capped at `MAX_IDEMPOTENCY_TTL_SECS` (`2592000` s = 30 days);
`validate()` fails gear init above it, since the value is added to the current time for every idempotency record.

### `orphan_grace_secs`
Grace period (seconds, default `3600` = 1h) a `pending` version or an expired multipart session must age past
before the cleanup sweep reclaims it. **Production recommendation**: the default balances "reclaim abandoned uploads
promptly" against "don't race a slow-but-legitimate in-flight upload." **Misconfiguration risk**: too short → a
slow client upload can have its `pending` version reclaimed (and blob deleted) out from under it mid-upload,
surfacing as a finalize `404`/`400`; too long → abandoned pending rows and their blobs linger longer, using storage.
Because that first failure mode is a direct self-contradiction — a signed `PUT` URL still valid while the sweep
reclaims the version behind it — `FileStorageConfig::validate()` **rejects** a configuration where
`default_url_ttl_secs` exceeds `orphan_grace_secs`. There is no session row to guard a single-part upload the way
the live-multipart-session guard protects a multipart one, so this config check is the only thing enforcing it.
Capped at `MAX_ORPHAN_GRACE_SECS` (`2592000` s = 30 days); `validate()` fails gear init above it.

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
When `true` (**the default**), the cleanup sweep loop starts at gear init. **Production recommendation**: leave at
`true` in every real deployment; set `false` only in test/dev harnesses that construct a `FileStorageConfig` directly
(not via YAML) and need fully deterministic behavior (no background task racing test assertions). **Misconfiguration
risk**: `false` in production means **no** orphan reconciliation, **no** expired-multipart cleanup, **no**
retention-policy enforcement, and **no** idempotency-key garbage collection ever run — pending versions and
abandoned multipart sessions accumulate indefinitely, retention rules become inert (a compliance-relevant silent
failure, since a configured retention policy will appear to exist via `GET /retention-rules` but never actually
delete anything), and `idempotency_keys` grows without bound.

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
handles on a best-effort basis only, and two windows are not covered by any sweep: a control-plane crash between
`initiate_multipart` and the session-row insert leaves a handle with no persisted correlation at all, and a backend
abort that fails after the session has already flipped to `aborted` is never retried (later passes list only
`in_progress` and lease-expired `completing` sessions). No object bytes are at stake in either case, but S3 bills for
incomplete multipart uploads. The sweep remains the primary reclamation path; the lifecycle rule is only the
backstop for these two uncorrelated windows — see `concurrency-and-failure-model.md` §5.

**The endpoint must honour conditional writes (`If-None-Match: *`)** — this is a **requirement** for any S3-compatible
backend used here, not an optimisation. Publishing a version is create-exclusive: the single-part `PutObject` and
the multipart `CompleteMultipartUpload` both carry `If-None-Match: *`, and the endpoint must answer `412 Precondition
Failed` when the key already exists. That is what stops a replayed `PUT` on a still-valid signed URL from overwriting
bytes that are already published. FileStorage does not probe for this at runtime: an endpoint that silently ignores
the header degrades to last-write-wins with no error anywhere in the request path, so it **must not** be used with
this gear. AWS S3 and MinIO (the CI test double) honour it; check any other endpoint before configuring it, with a
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
Interim gear-local shared secret the s2s finalize/report-part callback routes additionally require, on top of the
signed upload token, via the `x-fs-internal-token` request header. `None` (the default) preserves the token-only
trust model. This is a stop-gap until the platform's `toolkit-security::internal_auth` profiles are deployable in
this gear (see [ADR-0003](./ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)'s trust-model section).
Enforcement begins the moment this secret is set, independent of `require_finalize_internal_secret`:
`FinalizeAuth::verify` is a no-op only while the secret is `None`; the flag doesn't gate the check at all, it only
turns a missing secret into a startup error. Once a secret is configured, the control plane rejects with `403`
every sidecar callback lacking a matching `x-fs-internal-token`, which the client sees as a `502` on every fresh
single-part `PUT` (a replay of the same `PUT` is answered `409`, per the sidecar's `!created` decision table) and
on every part of a multipart upload. **Rollout order**: (1) redeploy every sidecar talking
to this control plane with `FS_SIDECAR_INTERNAL_TOKEN` set first — a control plane with no secret configured
ignores the header either way; (2) only then set `finalize_internal_secret` on the control plane, together with
`require_finalize_internal_secret: true`.

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

### `require_finalize_internal_secret`
When `true`, gear init fails fast if `finalize_internal_secret` is absent instead of silently accepting the
token-only trust model for the finalize/report-part callbacks. Mirrors `require_signing_key_seed`. Defaults to
`false` so a control plane with no secret configured still starts. This flag does not disable the enforcement
check — see the rollout order under `finalize_internal_secret` above for the sequencing that actually matters.

## Sidecar config: `FS_SIDECAR_*` environment variables

The sidecar is a separate binary/process (`src/bin/sidecar.rs`) with its own env-var configuration — it does **not**
share `FileStorageConfig`. All of these are read once in `main()`.

| Variable | Default | Notes |
|---|---|---|
| `FS_SIDECAR_ADDR` | `0.0.0.0:8087` | Bind address. |
| `FS_SIDECAR_PUBLIC_KEY` | **required, no default** | Base64url Ed25519 **primary** public key; must match the control plane's `signing_key_seed`-derived keypair (see above). Startup fails (`anyhow::anyhow!`) if unset or malformed. |
| `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` | unset (no previous keys) | Optional comma-separated list of additional base64url Ed25519 public keys, checked **after** the primary (same "try each, first match wins" verifier — no `kid`). Exists purely to give a `signing_key_seed` rotation a window where tokens signed by either the old or the new key still verify — see `signing_key_seed`'s **Rotation** paragraph below for the procedure. **Cost**: one extra Ed25519 verification per token that fails against the primary, times the list length — keep it short (one entry covering the immediately-prior seed is the normal case) and drop a key once `max_url_ttl_secs` has passed since the seed that produced it stopped being primary, so no still-valid token could possibly have been signed with it. An entry that duplicates `FS_SIDECAR_PUBLIC_KEY` or repeats elsewhere within the list is dropped at startup with a `warn`-level log (`dropped_duplicates`) rather than rejected — a harmless no-op, not a startup error, since the list need not be scrubbed the instant a rotation finishes, but the warning is a signal that step 4 of the rotation procedure has not been completed yet. A malformed entry fails sidecar startup exactly like a malformed `FS_SIDECAR_PUBLIC_KEY`. |
| `FS_SIDECAR_BACKEND_ROOT` | `./.file-storage-data` | Local-fs backend root — same durability caveat as the control plane's `storage_root`; the two should point at the **same** underlying storage for a single-backend deployment, or the sidecar will read/write blobs the control plane's metadata doesn't expect to find there. |
| `FS_SIDECAR_CONTROL_URL` | `http://localhost:8080` | Base URL of the control plane, used for the finalize/report-part callbacks. Setting it to the **empty string** explicitly disables the callback (dev/test only) — uploaded versions then stay `pending` forever, since nothing ever calls finalize; production must always set this to a reachable control-plane URL. The scheme is **not** validated, and the callbacks carry `x-fs-token` plus, when configured, the `x-fs-internal-token` shared secret — so keep this hop inside a trusted network boundary or point it at an HTTPS/mTLS endpoint; a plain-HTTP URL puts that secret on the wire in the clear. |
| `FS_SIDECAR_MAX_BODY_BYTES` | `5368709120` (5 GiB) | Raises axum's blanket request-body floor (default 2 MiB). The limit is a `DefaultBodyLimit` layer on the **whole** sidecar router (`build_router`), so it applies to the request bodies of the single-part `PUT` and of multipart part uploads alike — not only to the single-part route. It does **not** bound download responses: the limit governs request-body extraction, and a download is a `GET`/`HEAD` whose response is streamed past it. This is a transport-layer ceiling only — the real per-request limit is the signed token's `max_size`/`exact_size` claim. **Misconfiguration risk**: setting it below the largest policy-permitted single-part upload causes legitimate uploads to be rejected at the transport layer before the token-level check even runs; because the planner may widen `part_size` up to `MAX_PART_SIZE` (5 GiB) for very large objects, lowering this variable can also reject every *part* of a multipart upload with `413`, which is easy to miss when tuning it with only single-part uploads in mind. |
| `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` | `60` | Maximum pause the sidecar tolerates between two consecutive chunks of a client's request body — and before the first one — on the single-part `PUT` upload and on `upload_multipart_part`; `0` disables the guard. This is a **per-chunk idle** bound, not a deadline on the whole stream: a slow-but-steady multi-GiB upload that never pauses longer than this between chunks still completes, no matter how long it takes overall. It closes a gap neither of the other two body-related controls covers: the signed token's `exp` is checked exactly once, before any body bytes are read, and `FS_SIDECAR_MAX_BODY_BYTES` bounds bytes, not time — without this timeout, a client that opens the connection and then stalls (or never sends at all) could hold the request open indefinitely (CWE-400). A client that goes idle past the deadline gets `408 Request Timeout`; the partial object is cleaned up exactly like any other broken upload stream (see F2 in [concurrency-and-failure-model.md](./concurrency-and-failure-model.md)). Independent of `FS_SIDECAR_FINALIZE_TIMEOUT_SECS`/`FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` below, which bound the sidecar→control-plane callback *after* the body stream has already finished. **Misconfiguration risk**: too low rejects legitimate uploads from clients on slow or lossy links (a real, live upload that merely pauses between chunks) with a `408` that looks like a client bug; too high re-opens the held-open-connection exposure this control exists to close. |
| `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` | `10` | Total wall-clock **budget** for the sidecar → control-plane finalize/report-part callback, covering the **entire retry loop** (all `CALLBACK_MAX_ATTEMPTS = 3` attempts and the delays between them), not a per-attempt allowance — `post_with_retry` wraps the whole loop in one deadline sized to this value, so a hung/unreachable control plane can never hold the client's request open for more than this long regardless of how many attempts it takes. The control plane re-reads and re-hashes the whole object inside this window on the single-part finalize path, so the budget has to cover a full read-back, not just the round trip. **Misconfiguration risk**: a single-part object whose read-back reliably exceeds the timeout never finalizes — the client sees `502` even though the bytes landed (F5 in [concurrency-and-failure-model.md](./concurrency-and-failure-model.md)). Raise the timeout for such workloads, or use multipart, whose `complete` performs no full read-back (ADR-0006). |
| `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` | `5` | Connect timeout for the same callbacks. Together with the timeout above, bounds how long a client's upload request can be held open by an unreachable or hung control plane — without these timeouts, a hung control plane could block the client indefinitely. **Misconfiguration risk**: too low in a high-latency network path causes spurious `502 Bad Gateway` responses to clients on otherwise-successful uploads; too high re-opens the "held open indefinitely" problem these timeouts exist to close. |
| `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS` | `2` | Caps how many `upload_multipart_part` requests take the `multipart_native` write path (`write_multipart_part_native`) concurrently. Each in-flight request on that path buffers up to `MAX_PART_SIZE` (5 GiB) in memory before writing it out, so with no cap N concurrent part uploads could drive memory to roughly `N * MAX_PART_SIZE`; at the default of `2` that is up to 10 GiB. A request that cannot immediately acquire a slot waits briefly (`PART_UPLOAD_ACQUIRE_TIMEOUT`, 200ms) for one to free up before it is rejected with `503`/`Retry-After: 1` — not queued indefinitely, but not rejected outright the instant the limit is hit either. At startup the sidecar checks this worst case against a process memory limit read from the cgroup filesystem (v2 `memory.max`, falling back to v1 `memory.limit_in_bytes`): if a limit is known and would clearly be exceeded, the sidecar refuses to start; if no limit can be read (not Linux, or unbounded), it only logs a warning with the computed worst case. |
| `FS_SIDECAR_INTERNAL_TOKEN` | unset (header omitted) | Interim shared secret sent as `x-fs-internal-token` on both the finalize and report-part control-plane callbacks — the sidecar's half of the control plane's `finalize_internal_secret`/`require_finalize_internal_secret` (see above). Unset/empty = the header is not sent, matching a control plane with the check disabled. Must match the control plane's configured secret from the moment `finalize_internal_secret` is set on the control plane, regardless of `require_finalize_internal_secret`. |
| `FS_SIDECAR_S3_BACKENDS` | unset (no S3 backends) | Optional JSON array of `S3BackendConfig` entries (mirrors the control plane's `s3_backends`), folded into the sidecar's own `BackendRegistry` alongside the always-present `local-fs` backend so a control-plane-registered `S3Backend` is reachable by real traffic dispatched per-request via `claims.backend_id`. Credentials embedded in this JSON blob are acceptable for the sidecar (the one component authorized to hold them, per ADR-0003) but should be sourced from a secrets manager / mounted file in production where supported. **Keep this list in lockstep with the control plane's `s3_backends`**: signed tokens carry `backend_id` and `backend_path`, and the sidecar resolves them against *its own* registry, with no reconciliation, handshake or version check between the two. A `backend_id` the sidecar does not know fails the request with `500` ("unknown backend") after the URL was already minted; worse, an id that resolves on both sides but points at a different endpoint/bucket fails silently in the other direction — the upload lands in the wrong bucket and the control plane's read-back finds nothing, surfacing as a `502` on finalize (or a `404` on a later download) rather than as a configuration error. |

Every `FS_SIDECAR_*` numeric env var (`FS_SIDECAR_MAX_BODY_BYTES`, `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS`,
`FS_SIDECAR_FINALIZE_TIMEOUT_SECS`, `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS`, and
`FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS`) fails sidecar startup with a
descriptive error if the value is *set but does not parse* (`parse_optional`/`parse_env_or_default`,
`src/bin/sidecar.rs`) — it does **not** silently fall back to the default on a typo like
`FS_SIDECAR_MAX_BODY_BYTES=5GB`. Only a genuinely unset variable uses the default.

A failed finalize/report-part callback (after the sidecar's retry budget, see `post_with_retry` /
`CALLBACK_MAX_ATTEMPTS` in `src/bin/sidecar.rs`) returns `502 Bad Gateway` to the client. The backend write itself
is **create-exclusive, not overwrite-safe**: `StorageBackend::publish_exclusive` writes only the *first* time a
given backend path is empty — a retried `PUT` to the same signed URL never overwrites bytes already landed there,
it reports `created: false` and leaves the stored blob untouched. The documented recovery is still to retry the
`PUT`: if the earlier publish landed but finalize never ran, this retry's measured bytes match what's already
stored and the handler answers `200` (a benign retry has converged) via a fresh finalize attempt. If the version
was already finalized, the outcome converges **regardless of the token's bind mode**: a replay whose size and
content hash match the stored version is answered `200` — for a token carrying the `bind_on_finalize` claim
(auto-bind `POST /files` path) the `X-FS-Bound`/`ETag` headers are recomputed against the file's *current*
`content_id`, so a concurrent bind in between can still flip `bound`↔`conflict` (F4 in the concurrency-and-failure
model, and `write.rs`'s already-available replay branch); a manual-bind token converges the same way, simply with no
bind-outcome headers to report, exactly as on the first call. A mismatched replay (different size or hash) is
answered `400` (hash/size check) — never a silent overwrite. `409` ("version already finalized") is now reserved for
a genuine **concurrent** double-finalize — two callbacks racing before either has observed the other's
already-`available` status — not for an ordinary sequential retry, which converges as above. For the finalize case
specifically, the version may already be correctly finalized server-side even though the client saw a transient
`502` on a preceding attempt (re-verify via `GET /files/{id}/versions` before assuming failure).

## The background cleanup sweep

`CleanupEngine::run_sweep` (`src/domain/cleanup.rs`) is the single entry point for the whole background lifecycle
job the gear schedules on a `sweep_interval_secs` timer when `enable_background_sweep` is `true` (see `gear.rs`).
Each step is **best-effort**: a failure in one step is logged at `warn` and does not abort the rest of the sweep, and
every operation is written to be safely idempotent under concurrent sweeps (no cross-instance leader election exists
today — every replica runs its own sweep independently; cross-instance coordination is expected in a future release).

The sweep runs **four** steps, in this order:

1. **Abandoned-pending sweep** (`cpt-cf-file-storage-fr-orphan-reconciliation`) — runs in two phases, still just one
   of the four sweep steps:
   - **(a)** Deletes `file_versions` rows still `pending` (pre-registered but never finalized) older than
     `orphan_grace_secs`, best-effort deletes their backend blobs, and additionally deletes the parent `files` row
     too if reclaiming its last pending version leaves it a permanent zero-version orphan (no versions left **and**
     `content_id IS NULL`, and no blocking in-progress/completing multipart session for that file).
   - **(b)** Separately sweeps `files` rows that never had a version in the first place. The list query's predicate
     is exactly three conditions — `content_id IS NULL`, no rows in `file_versions`, and `created_at` older than the
     same `grace_cutoff` (`orphan_grace_secs`) — and runs batched; each candidate is then re-verified and deleted
     through the same guarded `maybe_delete_orphaned_file` path that phase (a) uses for its own zero-version case,
     writing an `OrphanReconcile` audit row and a `file.deleted` event. The absence of a blocking
     `in_progress`/`completing` multipart session for the file is deliberately **not** part of that list query: it is
     checked per candidate, at delete time, inside `maybe_delete_orphaned_file`. This is the reaper for a `POST /files`
     multipart create whose control plane crashed between committing the bare file row and inserting the pending
     version, or whose synchronous `compensate_failed_multipart_initiate` compensation (see
     [concurrency-and-failure-model.md](./concurrency-and-failure-model.md) §2.2 M1) itself failed — before this
     phase existed, that versionless row was never picked up by any sweep pass; the zero-version cleanup in phase
     (a) only ever fired as a side effect of reclaiming a *pending* version, and step 2's expired-session sweep only
     ever fired as a side effect of aborting a session, so a file that got neither had no reaper at all. There is no
     race with a live `POST /files`: the gap between committing the file row and inserting its pending version is
     milliseconds, while `orphan_grace_secs` is measured in hours.
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
  a caller can never silently flip whether the replayed upload auto-binds.
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

See `docs/migration.sql`'s `idempotency_keys` table and `docs/api.md`'s `409` summary for the wire-level contract.

## Upgrading from v0.2.x and rolling back

This release adds one additive migration, `m20260924_000001_upload_flow_redesign`: new `multipart_uploads` columns
— `auto_bind`, `lease_until`, `lease_owner`, `complete_result`, `backend_id`, `backend_path` — and the `completing`
state in its CHECK (on SQLite it rebuilds `multipart_uploads`, carrying rows and parts over), plus indexes only
(`idempotency_keys_file_idx`, `multipart_uploads_sweep_idx`, `files_versionless_sweep_idx`,
`file_versions_file_created_idx`, `files_owner_listing_v2_idx` superseding `files_owner_listing_idx`). Existing rows
keep working: new columns are nullable or default to the old behaviour, and `backend_id`/`backend_path` are
backfilled from the session's version. No released migration changes.

**Mixed-version window.** Run the migration once, then roll the fleet. An older (v0.2.x) instance tolerates the new
schema, with two exceptions to keep short: it rejects a multipart session in the new `completing` state as
`invalid multipart upload state` (a `complete` in flight on a new instance), and it never auto-binds. Roll the
control plane and the sidecars together — the sidecar forwards the new finalize headers and the control plane
accepts the new token grace — and keep the window as short as a normal rolling restart.

**Rolling back the binary.** An older binary refuses to start against a database that has migrations it does not
know (sea-orm reports `Migration file of version '…' is missing`), so the order is: with the **new** binary, run
`m20260924_000001_upload_flow_redesign` down, then deploy the old binary. Its `down()` drops the new columns, so
sessions in `completing` and stored completion snapshots are lost; finish or abort in-flight multipart uploads
before rolling back. Files and versions themselves are untouched by either direction.

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

Signing and verification of the URL-signing token are behind an in-house trait pair
(`infra::signed_url::SignatureProvider` / `SignatureVerifier`), not called directly against a hard-wired crypto
library. The implementation is Ed25519 (`Issuer::from_seed`/`Issuer::generate`), codec-equivalent to
PASETO `v4.public` (see `docs/api.md`'s "Signed URLs" section). The abstraction exists specifically for **FIPS
posture**: a FIPS-validated deployment needs the sign/verify primitive to run inside a FIPS-validated module (the
platform's `rustls-corecrypto-provider`); the trait boundary lets that primitive be swapped (e.g. for ECDSA P-256)
without any change to the token's opaque wire format or any client-visible change. **The provider shipped today is
not FIPS-validated**: `Ed25519Provider` is a generic Ed25519 implementation, and while Ed25519 itself is approved
under FIPS 186-5, that approval requires the primitive to run inside a validated module. It therefore **MUST NOT**
be deployed in a FIPS-constrained environment until the provider behind it is swapped — the abstraction makes that
swap cheap, but it has not been made. See
[ADR-0004](./ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md) "FIPS posture" for the full rationale.
