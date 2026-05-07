<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: Module Foundation

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-module-foundation`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-file-storage-feature-module-foundation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Module Init Flow](#module-init-flow)
  - [Default Backend Resolver](#default-backend-resolver)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Module Skeleton](#implement-module-skeleton)
  - [Implement TOML Backend Roster Loader](#implement-toml-backend-roster-loader)
  - [Implement SDK Crate Scaffolding](#implement-sdk-crate-scaffolding)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Stand up the FileStorage ModKit module shell — module manifest, static TOML configuration loader for the backend roster, `DatabaseCapability` declaration, `ClientHub` registration of the `FileStorageClient` trait, SDK crate scaffolding (trait stub, errors, models), and resolution of `default_private_storage_id` from the loaded roster.

### 1.2 Purpose

This is the foundation every other P1 feature attaches to. Without this feature there is no FileStorage module to register adapters under, no SDK trait for consumers to bind to, no DB connection for the Files Repo to use, and no resolver for the per-tenant default backend handles. Per `cpt-cf-file-storage-constraint-static-config-p1`, P1 loads the roster from a static TOML section at module init and never reloads it at runtime.

**Requirements**: `cpt-cf-file-storage-fr-rest-api`, `cpt-cf-file-storage-interface-sdk-trait`, `cpt-cf-file-storage-interface-rest-api`, `cpt-cf-file-storage-contract-cf-modules`

**Principles**: `cpt-cf-file-storage-principle-tenant-owner`

### 1.3 Actors

- `cpt-cf-file-storage-actor-cf-modules` — every module that consumes the FileStorage SDK depends on this feature standing up the `ClientHub` registration

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (§3.2 SDK Facade, §3.4 Internal Dependencies, §3.8 Topology)
- **Decomposition**: [DECOMPOSITION.md §2.1](../DECOMPOSITION.md)
- **Dependencies**: None (foundation feature)

## 2. Actor Flows (CDSL)

This feature has no user-facing actor flows — it is module-init plumbing. See §3 for the boot-time process.

## 3. Processes / Business Logic (CDSL)

### Module Init Flow

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-module-foundation-init`

**Input**: ModKit `ModuleCtx` plus the loaded TOML configuration

**Output**: A fully-wired FileStorage module with `dyn FileStorageClient` registered in `ClientHub`, or a fail-fast boot error

**Steps**:

1. [ ] - `p1` - Receive `ModuleCtx` from ModKit lifecycle - `inst-init-1`
2. [ ] - `p1` - Parse TOML `[[backends]]` section into typed `BackendConfig` records - `inst-init-2`
3. [ ] - `p1` - **FOR EACH** backend record in the TOML - `inst-init-3`
   1. [ ] - `p1` - Validate `id` is a valid UUID - `inst-init-3a`
   2. [ ] - `p1` - Validate `kind` is `s3-compatible` (the only P1 kind) - `inst-init-3b`
   3. [ ] - `p1` - Validate `default_private` is a boolean - `inst-init-3c`
4. [ ] - `p1` - **IF** more than one backend has `default_private = true` (within a single tenant view) - `inst-init-4`
   1. [ ] - `p1` - **RETURN** boot error: "duplicate default_private flag" - `inst-init-4a`
5. [ ] - `p1` - **IF** zero backends have `default_private = true` - `inst-init-5`
   1. [ ] - `p1` - **RETURN** boot error: "at least one backend MUST hold default_private" - `inst-init-5a`
6. [ ] - `p1` - Resolve `default_private_storage_id` from the flagged entry - `inst-init-6`
7. [ ] - `p1` - Acquire DB connection pool through ModKit `DatabaseCapability` - `inst-init-7`
8. [ ] - `p1` - Construct `FileStorageService` instance and wire in: parsed roster, default-backend handle, DB pool - `inst-init-8`
9. [ ] - `p1` - Register `Arc<FileStorageService>` as `dyn FileStorageClient` in `ClientHub` - `inst-init-9`
10. [ ] - `p1` - **RETURN** module ready for traffic - `inst-init-10`

### Default Backend Resolver

- [ ] `p2` - **ID**: `cpt-cf-file-storage-algo-module-foundation-default-resolver`

**Input**: A request that does not specify `backend_id`

**Output**: The resolved `&Backend` to route through (always the tenant's `default_private`)

**Steps**:

1. [ ] - `p1` - **RETURN** the backend referenced by `default_private_storage_id` - `inst-resolve-1`

(`default_public` is a separate role, available in P1 alongside `default_private`. When a future entry-point lets callers express "I want a public-read URL for this new file", that path will resolve through `default_public_storage_id` instead. P1's `create_presigned_url` always lands on `default_private` when `backend_id` is omitted.)

## 4. States (CDSL)

This feature does not introduce any persistent state machine. Module lifecycle (init → ready → shutdown) is owned by ModKit and is out of scope for FileStorage.

## 5. Definitions of Done

### Implement Module Skeleton

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-module-foundation-skeleton`

The system **MUST** ship a ModKit module with a manifest, lifecycle hooks (`Module`/`ModuleCtx`), and `DatabaseCapability` declaration. The module **MUST** fail-fast at boot on any invalid TOML configuration, missing `default_private` backend, or duplicate `default_private` flag.

**Implements**:

- `cpt-cf-file-storage-algo-module-foundation-init`

**Constraints**: `cpt-cf-file-storage-constraint-static-config-p1`, `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`

**Touches**:

- Crate: `file-storage` (module crate)
- Config: `[[backends]]` section in the static TOML

### Implement TOML Backend Roster Loader

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-module-foundation-toml-loader`

The system **MUST** parse the `[[backends]]` TOML section into `BackendConfig` records, validate the per-backend `id` UUID, reject non-P1 backend kinds, and resolve the `default_private_storage_id` handle. The loader **MUST NOT** open a connection to any backend during boot — connectivity is verified lazily on first use.

**Implements**:

- `cpt-cf-file-storage-algo-module-foundation-init`
- `cpt-cf-file-storage-algo-module-foundation-default-resolver`

**Constraints**: `cpt-cf-file-storage-constraint-static-config-p1`, `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`

**Touches**:

- Config types: `BackendConfig`, `RosterConfig`

### Implement SDK Crate Scaffolding

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-module-foundation-sdk-crate`

The system **MUST** publish `file-storage-sdk` with the `FileStorageClient` async trait stub (12 methods), `FileStorageError` enum (including the `DeleteInProgress` variant), and the full set of model types described in [rust-traits.md](../rust-traits.md): `FileId`, `BackendId`, `Etag`, `OwnerRef`, `Backend` (with `default_private`, `default_public`, `versioning` fields), `BackendKind` (`S3Compatible`), `BackendTransport` (`Redirect`), `BackendCapability` (`PresignedUrls`, `PublicReadUrls`), `FileMeta`, `FileMetaUpdate` (no `gts_file_type` field — structurally immutable), `FileInfo` (with `version_id: Option<String>`), `FileStatus` (`PendingUpload | Uploaded | Deleting`), `UrlParams`, `PresignedUploadHandle`, `PresignedDownload` (with `is_public`), `PresignDownloadItem` (with optional `version_id`), `PresignDownloadOutcome`, `ReconcileResult`, `FileReadHandle`, `ListFilesQuery`, `FileList`, `FileByteStream`. SDK methods are stubs in this feature; their bodies are filled in later features. The 12 SDK methods are: `list_backends`, `create_presigned_url`, `create_presigned_overwrite_url`, `reconcile`, `get_file_info`, `put_file_info`, `delete_file`, `list_files`, `read_file`, `put_file` (P1: `unimplemented!()` stub), `presign_urls`.

**Implements**:

- `cpt-cf-file-storage-algo-module-foundation-init`

**Constraints**: `cpt-cf-file-storage-constraint-no-ambient-authn`

**Touches**:

- Crate: `file-storage-sdk` (new crate)
- Trait: `FileStorageClient`

## 6. Acceptance Criteria

- [ ] Module boots with a valid TOML roster of one or more `s3-compatible` backends and exactly one `default_private` flag.
- [ ] Module fails-fast at boot when zero backends declare `default_private = true`.
- [ ] Module fails-fast at boot when more than one backend declares `default_private = true`.
- [ ] Module fails-fast at boot when a TOML entry uses any `kind` other than `s3-compatible`.
- [ ] Module does NOT attempt any backend connectivity check at boot (connectivity errors surface only on the first request, per `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`).
- [ ] After successful boot, `ClientHub::get::<dyn FileStorageClient>()` returns a working handle for in-process consumers.
- [ ] After successful boot, `default_private_storage_id` resolves to an entry in the parsed roster.
