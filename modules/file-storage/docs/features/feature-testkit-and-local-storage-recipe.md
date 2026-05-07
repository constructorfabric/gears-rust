<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: Testkit and Local-Storage Recipe

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-testkit-and-local-storage-recipe`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-file-storage-feature-testkit-and-local-storage-recipe`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Operator Local-Disk Recipe](#operator-local-disk-recipe)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [LocalS3Fixture Bring-Up](#locals3fixture-bring-up)
  - [Self-Healing Regression Test](#self-healing-regression-test)
  - [Lifecycle End-to-End Test](#lifecycle-end-to-end-test)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Ship file-storage-testkit Crate](#ship-file-storage-testkit-crate)
  - [Pin s3s and s3s-fs Versions](#pin-s3s-and-s3s-fs-versions)
  - [Cover the 4 Mandatory Test Scenarios](#cover-the-4-mandatory-test-scenarios)
  - [Document the Local-Disk Operator Recipe](#document-the-local-disk-operator-recipe)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Stand up `file-storage-testkit` — the in-process `s3s-fs` fixture (DESIGN §4) plus the lifecycle and self-healing regression tests — and document the same `s3s-fs` setup as the P1 recipe for local-disk deployments. Operators who need local-disk storage run `s3s-fs` side-by-side with FileStorage and register it in the static TOML roster as a regular `s3-compatible` backend, instead of a native POSIX adapter that does not exist in P1.

### 1.2 Purpose

Two deliverables converge in one feature because they share the same component (`s3s-fs`) and the same configuration shape:

1. **Test infrastructure** — every other P1 feature reuses `LocalS3Fixture::start()` for integration tests. Pinning `s3s` and `s3s-fs` to exact versions makes the lifecycle and self-healing regression tests deterministic (per DESIGN §4).
2. **Operator recipe** — for deployments without managed object storage, `s3s-fs` is the supported way to land files on local disk while preserving the «one P1 backend kind» architecture. There is no native POSIX adapter in P1.

This feature is a P2 feature in `featstatus` rollup terms because the testkit becomes meaningful only after the P1 features it tests exist. It depends only on `module-foundation` for SDK type re-exports and is otherwise developable in parallel with everything else.

**Requirements**: (this feature covers no PRD requirement directly — it is verification infrastructure plus deployment recipe)

**Principles**: `cpt-cf-file-storage-principle-modular-backend-roster`

### 1.3 Actors

- `cpt-cf-file-storage-actor-cf-modules` — module developers running integration tests
- `cpt-cf-file-storage-actor-platform-user` — operators deploying FileStorage with a local-disk recipe

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) §4 Testing strategy (`cpt-cf-file-storage-design-testing`)
- **ADR**: [ADR-0003](../ADR/0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md), [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md)
- **Decomposition**: [DECOMPOSITION.md §2.9](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-file-storage-feature-module-foundation`

## 2. Actor Flows (CDSL)

### Operator Local-Disk Recipe

- [ ] `p2` - **ID**: `cpt-cf-file-storage-flow-testkit-and-local-storage-recipe-operator-recipe`

**Actor**: `cpt-cf-file-storage-actor-platform-user` (operator)

**Success Scenarios**:

- Operator runs `s3s-fs` as a side-process; FileStorage TOML registers it as `s3-compatible`; the deployment functions identically to AWS S3 / MinIO setups

**Error Scenarios**:

- Operator's `storage_path` for `s3s-fs` is not writable → `s3s-fs` fails to start (separate process); FileStorage surfaces the inability lazily on the first request

**Steps**:

1. [ ] - `p2` - Operator installs `s3s-fs` binary on the host (e.g. `cargo install s3s-fs --locked`) - `inst-recipe-1`
2. [ ] - `p2` - Operator launches `s3s-fs --host 127.0.0.1 --port 8014 --access-key … --secret-key … /var/lib/file-storage` as a service (systemd / supervisord / k8s sidecar) - `inst-recipe-2`
3. [ ] - `p2` - Operator adds an entry to FileStorage TOML: `[[backends]] id = "<UUID>", kind = "s3-compatible", endpoint = "http://127.0.0.1:8014", credentials = { access_key = "…", secret = "…" }, default_private = true` - `inst-recipe-3`
4. [ ] - `p2` - Operator starts FileStorage; module init succeeds (no boot-time backend probe per `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`) - `inst-recipe-4`
5. [ ] - `p2` - First end-client `create_presigned_url` lands a `pending_upload` row; presigned PUT URL targets `http://127.0.0.1:8014/<bucket>/<key>` - `inst-recipe-5`
6. [ ] - `p2` - End-client PUTs bytes; `s3s-fs` writes them under `/var/lib/file-storage/<bucket>/<key>` - `inst-recipe-6`
7. [ ] - `p2` - Application backend calls `reconcile(file_id)`; FileStorage HEADs `s3s-fs`, pulls the authoritative `(etag, version_id, mirrored metadata)`, and commits the row to `uploaded` - `inst-recipe-7`

## 3. Processes / Business Logic (CDSL)

### LocalS3Fixture Bring-Up

- [ ] `p2` - **ID**: `cpt-cf-file-storage-algo-testkit-and-local-storage-recipe-fixture-start`

**Input**: None (called by `#[tokio::test]` setup)

**Output**: `LocalS3Fixture { addr: SocketAddr, credentials: TestCredentials }` plus an internal RAII guard that shuts down the hyper server on `Drop`

**Steps**:

1. [ ] - `p2` - Create a fresh `tempfile::TempDir` for this test - `inst-fix-1`
2. [ ] - `p2` - Construct `s3s_fs::FileSystem::new(temp_dir.path())` - `inst-fix-2`
3. [ ] - `p2` - Mint random `TestCredentials { access_key, secret_key }` - `inst-fix-3`
4. [ ] - `p2` - Build `s3s::service::S3ServiceBuilder::new(filesystem)` and apply `SimpleAuth::from_single(creds)` - `inst-fix-4`
5. [ ] - `p2` - Bind a `tokio::net::TcpListener` to `127.0.0.1:0` (OS-assigned port) - `inst-fix-5`
6. [ ] - `p2` - Spawn a hyper-1 server task that serves the built `S3Service` until shutdown - `inst-fix-6`
7. [ ] - `p2` - Construct a `oneshot::channel`; the sender becomes the RAII shutdown handle - `inst-fix-7`
8. [ ] - `p2` - **RETURN** `LocalS3Fixture { addr: listener.local_addr(), credentials, _temp_dir: temp_dir, _shutdown: tx }` - `inst-fix-8`

### Self-Healing Regression Test

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-testkit-and-local-storage-recipe-self-heal-regression`

**Input**: A running `LocalS3Fixture` and a FileStorage SDK configured against it

**Output**: Test passes if both lazy (`read_file`) and eager (`reconcile`) self-heal triggers reconcile the row's etag against the backend after an out-of-band mutation; fails otherwise

**Steps**:

1. [ ] - `p1` - Land a normal upload via `create_presigned_url` → external PUT → `reconcile`; capture `info.etag`, `info.version_id`, and `s3_etag` - `inst-heal-reg-1`
2. [ ] - `p1` - Mutate the file in the fixture's `TempDir` directly (overwrite bytes outside FileStorage) - `inst-heal-reg-2`
3. [ ] - `p1` - Call `reconcile(ctx, file_id)`; assert `Ok(ReconcileResult)`; assert `info.etag` is the new raw S3 ETag from HEAD; assert `info.version_id` reflects S3 (or `None` on non-versioning fixtures) - `inst-heal-reg-3`
4. [ ] - `p1` - Mutate the fixture's `TempDir` again - `inst-heal-reg-4`
5. [ ] - `p1` - Call `read_file(ctx, file_id, None)`; assert `Ok(FileReadHandle)`; drain the stream and assert the bytes match the post-mutation content; assert `info.etag` reflects the new bytes (lazy in-process repair) - `inst-heal-reg-5`
6. [ ] - `p1` - Mutate the fixture's `TempDir` once more - `inst-heal-reg-6`
7. [ ] - `p1` - Call `read_file(ctx, file_id, Some(stale_etag))`; assert `Err(EtagMismatch{ current: derived })`; assert the row is repaired before the error is returned (next `get_file_info` shows the repaired etag) - `inst-heal-reg-7`

### Lifecycle End-to-End Test

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-testkit-and-local-storage-recipe-lifecycle-e2e`

**Input**: A running `LocalS3Fixture`, a FileStorage module configured to point at it, and an `aws-sdk-s3` "external client" simulating the end-client browser

**Output**: Test passes if every step lands the expected SDK response

**Steps**:

1. [ ] - `p1` - SDK call `create_presigned_url(ctx, Some(local_s3sfs_id), owner, "/test/file.bin", meta, params)` - `inst-e2e-1`
2. [ ] - `p1` - Assert the response carries `(file_id, upload_url, etag_pinned, expires_at)` and the row exists in DB with `status = 'pending_upload'` - `inst-e2e-2`
3. [ ] - `p1` - "External client" PUTs 1 KiB to `upload_url`; assert `200 OK` - `inst-e2e-3`
4. [ ] - `p1` - SDK call `reconcile(ctx, file_id)` - `inst-e2e-4`
5. [ ] - `p1` - Assert response is `Ok(ReconcileResult { info, s3_etag, s3_version_id })`; DB row's `status = 'uploaded'`, `etag` is the raw S3 ETag, `version_id` matches S3 (or `None`), `upload_expires_at` is `NULL` - `inst-e2e-5`
6. [ ] - `p1` - SDK call `presign_urls(ctx, [PresignDownloadItem { file_id, params, etag: Some(info.etag.clone()), version_id: None }])` - `inst-e2e-6`
7. [ ] - `p1` - Assert per-item outcome is `Ok(PresignedDownload { url, expires_at, is_public: false })` (with `response-content-type` and `response-content-disposition` query params present) - `inst-e2e-7`
8. [ ] - `p1` - HTTP GET the presigned URL; assert response body is the original 1 KiB - `inst-e2e-8`
9. [ ] - `p1` - SDK call `read_file(ctx, file_id, Some(&info.etag))`; assert handle.bytes drains exactly 1 KiB - `inst-e2e-9`
10. [ ] - `p1` - SDK call `put_file_info(ctx, file_id, FileMetaUpdate { name: Some("renamed.bin".into()), .. }, Some(&info.etag))`; assert `Ok(FileInfo)`; HEAD S3 to confirm the object's `Content-Disposition` reflects the new name - `inst-e2e-10`
11. [ ] - `p1` - SDK call `delete_file(ctx, file_id, Some(&info.etag))`; assert `Ok(())` and DB row gone after Phase 3; assert the backend object is also gone - `inst-e2e-11`

## 4. States (CDSL)

The fixture has no persistent states — it is per-test, with `TempDir` lifecycle bound to the test scope.

## 5. Definitions of Done

### Ship file-storage-testkit Crate

- [ ] `p2` - **ID**: `cpt-cf-file-storage-dod-testkit-and-local-storage-recipe-crate`

The system **MUST** ship a `file-storage-testkit` crate at `modules/file-storage/file-storage-testkit/` exposed only via `[dev-dependencies]`. The crate **MUST** export `LocalS3Fixture::start() -> anyhow::Result<LocalS3Fixture>`, `LocalS3Fixture::addr() -> SocketAddr`, `LocalS3Fixture::credentials() -> &TestCredentials`. The fixture **MUST** drop cleanly: `oneshot` shutdown signal triggers hyper graceful shutdown, then `TempDir` cleans the on-disk state.

**Implements**:

- `cpt-cf-file-storage-algo-testkit-and-local-storage-recipe-fixture-start`

**Constraints**: (no new constraint registrations)

**Touches**:

- Crate: `file-storage-testkit` (new crate, dev-only)

### Pin s3s and s3s-fs Versions

- [ ] `p2` - **ID**: `cpt-cf-file-storage-dod-testkit-and-local-storage-recipe-pin-versions`

The workspace `Cargo.toml` **MUST** pin `s3s = "=0.13.0"` and `s3s-fs = "=0.13.0"` exactly (not `^0.13` — exact match) under `[workspace.dev-dependencies]`. Upgrades **MUST** be gated by re-running the lifecycle and self-healing regression tests on the new version; a version bump that fails the regression **MUST NOT** be merged.

**Implements**:

- (cross-cutting — protects the testkit and every test that uses it)

**Constraints**: `cpt-cf-file-storage-constraint-static-config-p1`

**Touches**:

- Workspace: `Cargo.toml` `[workspace.dev-dependencies]` block

### Cover the 4 Mandatory Test Scenarios

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-testkit-and-local-storage-recipe-test-coverage`

The testkit crate **MUST** include automated tests covering: (a) lifecycle end-to-end per `cpt-cf-file-storage-algo-testkit-and-local-storage-recipe-lifecycle-e2e`; (b) self-healing regression per `cpt-cf-file-storage-algo-testkit-and-local-storage-recipe-self-heal-regression` (covers both eager `reconcile` and lazy `read_file` triggers); (c) 2-phase delete (Phase 1 → backend cleanup → Phase 3 row purge, plus the concurrent-write detection scenario where `reconcile` against a `Deleting` row returns `DeleteInProgress`); (d) cross-tenant isolation (file_id in tenant A → request from tenant B → `404 NotFound`, no enumeration leak); (e) variant-B re-upload (`create_presigned_overwrite_url` + external PUT + `reconcile` rotates `etag` while preserving `file_id` and metadata); (f) `PUT /meta` DB+S3 sync (the `CopyObject` self-copy rotates S3 user-metadata in place).

**Implements**:

- `cpt-cf-file-storage-algo-testkit-and-local-storage-recipe-self-heal-regression`
- `cpt-cf-file-storage-algo-testkit-and-local-storage-recipe-lifecycle-e2e`

**Constraints**: (no new constraint registrations)

**Touches**:

- Crate: `file-storage-testkit` (test files)

### Document the Local-Disk Operator Recipe

- [ ] `p2` - **ID**: `cpt-cf-file-storage-dod-testkit-and-local-storage-recipe-doc-recipe`

The system **MUST** document the operator recipe (per `cpt-cf-file-storage-flow-testkit-and-local-storage-recipe-operator-recipe`) in the testkit crate's README — including the `s3s-fs --help` flags it exposes and the matching FileStorage TOML stanza. P1 declares no optional capabilities for `s3s-fs` to advertise — only `PresignedUrls` is required.

**Implements**:

- `cpt-cf-file-storage-flow-testkit-and-local-storage-recipe-operator-recipe`

**Constraints**: (no new constraint registrations)

**Touches**:

- Documentation: `modules/file-storage/file-storage-testkit/README.md`

## 6. Acceptance Criteria

- [ ] `LocalS3Fixture::start()` brings up an in-process S3-compatible endpoint in under 100 ms and tears down cleanly on `Drop` (no port leaks, no `TempDir` orphans).
- [ ] The lifecycle end-to-end test exercises `create_presigned_url → external PUT → reconcile → presign_urls → external GET → read_file → put_file_info DB+S3 sync → delete_file` against the in-process fixture.
- [ ] The self-healing regression test mutates the `TempDir` between operations and asserts both the eager (`reconcile`) and lazy (`read_file`) triggers reconcile the row's etag against the backend.
- [ ] The 2-phase delete test confirms Phase 1 flips the row to `Deleting`, Phase 2 deletes the backend object, Phase 3 purges the row; concurrent `reconcile` against a `Deleting` row returns `DeleteInProgress`.
- [ ] The cross-tenant isolation test confirms that a `file_id` minted in tenant A is invisible to tenant B (`NotFound`, never `Forbidden`).
- [ ] Workspace `cargo build --tests` succeeds with `s3s` / `s3s-fs` pinned to exact versions in `[workspace.dev-dependencies]`; `cargo update` does not silently bump these.
- [ ] The testkit crate's README documents the operator recipe end-to-end.
