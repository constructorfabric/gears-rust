<!-- TEMPORARY WORKING DOCUMENT — not a committed design artifact. -->
<!-- Delete once the work is broken into tracked tickets/PRs. -->

# File-Storage P2 — Step-by-Step Implementation & Verification Plan (TEMPORARY)

> ⚠️ **This is a temporary working plan, not an authoritative design doc.** It is the
> single source of ordered, individually-verifiable remediation steps for
> `feat/file-storage-p2`. **Goal: a stable, production-usable file-storage gear whose
> behavior fully matches its documentation** (`DESIGN.md`, the ADRs, `api.md`,
> `features/`). Each step lists exactly what to change, which files, and **how to verify
> it before moving on**. Once this work is split into tracked tickets/PRs, delete this file.
>
> Every file/line reference here was verified directly against the code on
> `feat/file-storage-p2`. Line numbers drift as edits land — re-run each step's pre-state
> `rg` to re-anchor before editing.

Paths are relative to `gears/file-storage/file-storage/` unless noted. Verification
commands assume the repo root as CWD. All `git commit` calls are DCO-signed (`git commit -s`)
on this feature branch — never on `main`.

### Severity map (what "must ship" means)

- **Tier 0** — correctness/security showstoppers. A cross-user data exposure, silent
  data loss, or a structurally non-functional feature. All must land before any release.
- **Tier 1** — production-readiness blockers (durability, resource bounds, multi-replica,
  observability).
- **Tier 2** — correctness hardening (races, contract drift, validation).
- **Tier 3** — documentation reconciliation (code is authoritative; docs must match it).
- **Tier 4** — deferred / explicit P3 follow-ups.

---

## How to use this plan

- **Order matters.** Follow the tier order and, within Tier 0, the critical-path sequence
  (`0.7 → 0.9 → 0.10 → 0.11 → 0.1 → 0.4 → 0.8 → 0.3 → 0.2 → 0.6 → 0.5`). Later steps assume
  earlier ones landed. `0.9`/`0.10`/`0.11` share the authorization/validation seams touched
  by `0.7`, so they land adjacent to it.
- **Verify after every step.** Do not proceed to the next step until its verification block
  passes. Each step's "Verify" is designed to fail loudly if the change is wrong.
- **One logical change per commit**, DCO-signed (`git commit -s`), on this feature branch —
  never on `main`.
- Steps that require a **team design decision before coding** are marked 🛑: **0.1** (finalize
  trust model), **0.7** (tenant-admin scope model — its `actions::ADMIN_POLICY` decision is
  reused by 0.5, 0.9 and 0.11). Resolve these first.

### Global preconditions (run once, before Step 1)

```bash
# 1. Confirm you are on the feature branch, not main
git -C /Users/roman/work/cyberfabric-core branch --show-current   # expect: feat/file-storage-p2

# 2. Fast-forward local main so future diffs are clean (audit found local main is stale)
git -C /Users/roman/work/cyberfabric-core fetch origin
git -C /Users/roman/work/cyberfabric-core log --oneline origin/main -1

# 3. Baseline: the gear builds and the existing suite is green BEFORE you change anything
cargo test -p file-storage -p file-storage-sdk 2>&1 | tail -20     # expect: all pass
cargo build -p file-storage --bin sidecar                          # expect: ok
```

> **Note on the `rg`/`sed` snippets in this doc.** Pre-state checks use plain `rg -n 'pat'`
> (a single alternation with real `|`, not `\|`) and, where a line range is shown,
> `sed -n '10,20p'` (comma, not hyphen). Older revisions of this plan contained
> `rg -rln`/`\|`/`sed -n '10-20p'` — all three are wrong and were corrected.

If the baseline is not green, stop and fix the environment first — you cannot attribute a
later failure to your change otherwise.

### Global verification (run after each tier)

```bash
cargo fmt --all -- --check
cargo clippy -p file-storage -p file-storage-sdk --all-targets -- -D warnings
cargo test -p file-storage -p file-storage-sdk
# e2e lifecycle (real server + sidecar) — gated, opt-in:
#   build sidecar, export FS_E2E_BINARY / FS_SIDECAR_BINARY per tools/scripts/ci.py, then:
#   pytest testing/e2e/gears/file_storage -q
```

---

## Tier 0 — Correctness / security showstoppers (step-by-step)

Order follows the critical path: **0.7 → 0.9 → 0.10 → 0.11 → 0.1 → 0.4 → 0.8 →
0.3 → 0.2 → 0.6 → 0.5**. Items 0.7/0.9/0.10/0.11 are the authorization &
validation cluster (all rooted in resource-less `authorize(…, "", None)` calls and
missing input validation) and should be reviewed together.

---

### 0.7 — Cross-user policy/retention authorization gap
- **Goal / definition of done**: `PUT /policy`, `GET /policy`,
  `POST /retention-rules`, `DELETE /retention-rules/{id}` all reject a caller
  who is not the resource owner (`scope_owner_id`/`scope_target_id` for a
  `user`-scope object, or not WRITE-authorized on the target file for a
  `file`-scope retention rule) unless the caller holds a distinct
  admin/tenant-wide action. Tenant-scope operations still require the caller
  to be authorized for tenant-wide policy administration (not just any
  WRITE).
- **Design decision required 🛑**: whether "tenant-admin" is an existing PDP
  action/scope the platform already models, or a new action this gear must
  define and register (`libs/authz-resolver-sdk`, PDP bundle). **This guide
  assumes the latter**: a new action constant `actions::ADMIN_POLICY` is
  introduced in `src/domain/authz.rs`, authorized via the existing
  `Authorizer::authorize(ctx, actions::ADMIN_POLICY, "", None)` call (same
  resource type `file_storage.file`, distinct action string) so the change is
  contained to this gear and does not require a new PDP resource type. If the
  team confirms a platform-wide admin scope already exists, swap the action
  string only — the ownership-comparison logic below is unchanged either way.
  Flag this to the team before merging; do not silently ship the assumption.
- **Pre-state check**:
  ```
  rg -n "scope_owner_id == ctx.subject_id\(\)|subject_id\(\) == " src/domain/policy_service.rs
  ```
  Expected current output: **no matches** — confirms no ownership comparison
  exists today. Also confirm the coarse check is in place:
  ```
  rg -n 'authorize\(ctx, actions::(WRITE|READ|DELETE), "", None\)' src/domain/policy_service.rs
  ```
  Expected: 5 matches (`get_own_policy` L62-64, `set_policy` L87-89,
  `get_effective_policy` L121-123, `list_retention_rules` L153-155,
  `create_retention_rule` L172-174, `delete_retention_rule` L207-209 — actual
  file has 6 call sites, all with the same resource-less pattern; the plan's
  line numbers are close but not exact, e.g. `set_policy` body starts at L79,
  the `authorize` call itself is L86-89).

- **Implementation steps**:
  1. **File**: `src/domain/authz.rs`. Add `pub const ADMIN_POLICY: &str = "admin_policy";`
     to the `actions` module (alongside `READ`/`WRITE`/`DELETE`). WHY: gives
     the service layer a distinct, PDP-checkable action for cross-owner /
     tenant-wide policy administration, separate from ordinary file
     read/write/delete.
  2. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` — compiles
     clean (new `pub const` only).
  3. **File**: `src/domain/policy_service.rs`, `PolicyService::set_policy`
     (currently L79-106). Replace the single coarse `authorize` call with:
     first attempt `self.authorizer.authorize(ctx, actions::ADMIN_POLICY, "", None)`;
     if that succeeds, proceed unconditionally (admin path) using its
     returned scope. If it errors (`Forbidden`), fall back to the existing
     `actions::WRITE` authorize call **plus** a new guard:
     `if let Some(owner) = scope_owner_id && owner != ctx.subject_id() { return Err(DomainError::Forbidden); }`
     placed immediately after the WRITE authorize succeeds and before the
     `upsert_policy` store call. Tenant-scope requests (`scope_owner_id ==
     None`) still require the WRITE authorize to succeed but no ownership
     comparison applies (there is no "owner" at tenant scope — tighten this
     further only if the team decides tenant-scope writes should also
     require `ADMIN_POLICY`; the plan does not mandate that, so leave tenant
     writes gated on `WRITE` for now and note it as a follow-up in the PR
     description). WHY: closes the exact failure scenario in the plan —
     `PUT /policy?scope=user&scope_owner_id=<victim>` from a non-owner,
     non-admin caller.
  4. **File**: `src/domain/policy_service.rs`, `get_own_policy` (L55-73).
     Same pattern: try `ADMIN_POLICY` first; on `Forbidden`, fall back to
     `READ` + the same `scope_owner_id != ctx.subject_id()` guard when
     `scope_owner_id.is_some()`.
  5. **File**: `src/domain/ports.rs`, `PolicyStore` trait (currently
     L216-263). Add `async fn require_file(&self, scope: &AccessScope, file_id: Uuid) -> Result<File, DomainError>;`
     to the trait (mirrors the identical method already required by
     `MultipartStore` at L106). WHY: `PolicyService` currently has no way to
     resolve a `file`-scope retention rule's `scope_target_id` to a `File`
     (needed for step 6's per-file authorize) — `Store` already implements
     `require_file` (used by `MultipartStore`/`FileService`), so this is
     exposing an existing method through a second narrow port, not new
     domain logic.
  6. **File**: `src/infra/storage/store/traits.rs` — both `impl MultipartStore
     for Store` (L92) and `impl crate::domain::ports::PolicyStore for Store`
     (L240) live here (the latter is written fully-qualified, so a bare
     `rg -n "impl PolicyStore for"` misses it — use
     `rg -n "PolicyStore for Store" src/`). Add the matching `PolicyStore::require_file`
     method that just calls the same private/shared `Store::require_file` the
     `MultipartStore` impl calls. WHY: satisfies the new trait method with zero
     new SQL — same underlying `FileRepo` lookup already exists.
  7. **Verify after 5-6**: `cargo build -p file-storage 2>&1 | tail -30` —
     compiles; any other `PolicyStore` implementors (test fakes, if any —
     `rg -n "PolicyStore for" tests/ src/`) must add the same delegating method
     or the build fails there first (fix those too).
  8. **File**: `src/domain/policy_service.rs`,
     `create_retention_rule` (L164-196). After the existing
     `retention_scope`/`scope_target_id` are known but before calling
     `self.store.insert_retention_rule(...)`, branch on `retention_scope`:
     - `RetentionScope::Tenant`: keep today's `WRITE`-only gate (no owner to
       compare).
     - `RetentionScope::User`: require
       `scope_target_id == Some(ctx.subject_id())` unless `ADMIN_POLICY`
       authorized (same try-admin-first pattern as step 3).
     - `RetentionScope::File`: resolve the target via
       `self.store.require_file(&AccessScope::allow_all(), target_id).await?`
       (using the new port method from step 5), then call
       `self.authorizer.authorize(ctx, actions::WRITE, &file.gts_file_type, Some(target_id)).await?`
       — the same per-file check `read_ops.rs`/`write.rs` already use. A
       missing/foreign file surfaces as `DomainError::file_not_found` from
       `require_file`, which also closes verifier finding **B4** (a rule
       pre-staged against a file the caller cannot write, or that doesn't
       exist yet) since the file must exist and be WRITE-authorized *now*.
     WHY: this is the file-scope re-authorization the plan calls for —
     without it, a `scope=file` rule against `<victim-file>` sails through
     the same resource-less `WRITE, "", None` check that gates every other
     retention-rule create.
  9. **File**: `src/domain/policy_service.rs`, `delete_retention_rule`
     (L201-211). Change to fetch-then-reauthorize: call
     `self.store.get_retention_rule(&AccessScope::allow_all(), rule_id).await?`
     (this method already exists on `Store` at
     `src/infra/storage/store/policy.rs::get_retention_rule` — it just needs
     exposing through the `PolicyStore` trait; add
     `async fn get_retention_rule(&self, scope: &AccessScope, rule_id: Uuid) -> Result<Option<StoredRetentionRule>, DomainError>;`
     to the trait in `ports.rs` next to `insert_retention_rule`, plus the
     one-line delegating impl on `Store`, same as step 6). If `None`, return
     `DomainError::file_not_found(rule_id)` (matches the existing 404
     mapping already used by the handler). If `Some(rule)`, re-run the same
     scope-based check as step 8 (`Tenant` → `WRITE`; `User` → owner match or
     `ADMIN_POLICY`; `File` → resolve + per-file `WRITE`) **before** calling
     `self.store.delete_retention_rule(...)`. WHY: today `delete_retention_rule`
     authorizes with the same resource-less `DELETE, "", None` check and then
     deletes by bare `rule_id` with no ownership comparison at all — any
     tenant member can delete any other member's retention rule.
  10. **Verify after 8-9**: `cargo build -p file-storage 2>&1 | tail -30` —
      compiles; `cargo clippy -p file-storage -- -D warnings` clean.

- **New/changed tests** — new `tests/policy_authz_test.rs` (mirror the
  `build_db`/`ctx(tenant)` harness from `tests/multipart_test.rs`; use
  `TenantOnlyAuthorizer`-style test doubles are **not** sufficient here since
  `TenantOnlyAuthorizer` ignores `action`/`file_id` entirely — write a small
  local `ScopedTestAuthorizer` that grants `READ`/`WRITE`/`DELETE` always but
  denies `ADMIN_POLICY` unless a `is_admin: bool` flag is set, so tests can
  exercise both the admin and non-admin paths):
  - `set_policy_foreign_owner_without_admin_scope_is_denied` — user A
    (`ctx(tenant)`), `scope_owner_id = Some(user_b)`; assert
    `matches!(err, DomainError::Forbidden)`; assert zero rows in `policies`
    for `scope_owner_id = user_b` via direct `PolicyEntity::find()`.
  - `set_policy_self_owner_is_allowed` — `scope_owner_id = Some(ctx.subject_id())`
    (positive control); assert `Ok`, and the stored row's `scope_owner_id`
    matches via direct DB read.
  - `set_policy_tenant_admin_scope_allows_foreign_owner` — same as the first
    case but with `is_admin = true` on the authorizer; assert `Ok` and the DB
    row exists for `user_b`.
  - `create_retention_rule_file_scope_target_not_writable_is_denied` —
    `scope_target_id = victim_file_id` (a real file created in a different
    "owner" context in the test, authorizer configured to deny `WRITE` for
    that specific `file_id`); assert `Forbidden`/`NotFound` and
    `RetentionRuleEntity::find().count() == 0`.
  - `create_retention_rule_file_scope_target_writable_is_allowed` — positive
    control; also stands in for verifier finding **B4** (nonexistent
    `scope_target_id` → `DomainError::FileNotFound`, zero rows written).
  - `delete_retention_rule_foreign_owner_is_denied` — create a `User`-scope
    rule as user A, attempt delete as user B; assert denial and the rule row
    still exists (direct DB read).
  Run: `cargo test -p file-storage --test policy_authz_test` — all pass.
  - **E2E** (justified negative — real cross-**subject**, same-tenant AuthZ
    decision is a genuine seam unit tests can't prove): add
    `test_cross_user_policy_write_denied_by_real_authz` to
    `testing/e2e/gears/file_storage/test_file_storage_seams.py`. Introduce
    `E2E_AUTH_TOKEN_USER_B` (new optional env var, same shape as
    `E2E_AUTH_TOKEN_TENANT_B` in `testing/e2e/gears/resource_group/test_integration_seams.py:207`)
    mapped to a second subject in the *same* tenant; `pytest.skip(...)` when
    unset. Do not block landing 0.7 on this env var existing — the unit
    tests above are sufficient sole coverage until it does.

- **Done-check**:
  ```
  cargo test -p file-storage --test policy_authz_test && \
  rg -n "scope_owner_id != ctx.subject_id\(\)|scope_owner_id == Some\(ctx.subject_id" src/domain/policy_service.rs
  ```
  Green tests + at least one ownership-comparison match = complete.

---

### 0.1 — Finalize trusts client-supplied size/hash; never verifies the blob
- **Goal / definition of done**: `finalize_upload_by_token` (and
  `finalize_upload`) never persist a `size`/`hash_value` that was not
  independently derived from the bytes actually present at the version's
  backend path. A finalize call for a version with no prior successful `PUT`
  is rejected; a finalize call with a size/hash claim that doesn't match the
  real blob is rejected.
- **Design decision required 🛑**: the plan offers (1) sidecar-only finalize
  behind internal-auth (retiring client-driven finalize) or (2) server-side
  re-verification (read the blob back, recompute hash/length, reject on
  mismatch). **This guide assumes option (2)** as the immediate fix: this
  repo's `libs/toolkit-security::internal_auth` two-plane primitives exist
  (`InternalCredential`, `PlatformSecurityContext`) but have **zero call
  sites in `file-storage`** today (`rg -ln 'internal_auth|InternalCredential' src/`
  returns nothing) — wiring option (1) means adding axum middleware to the
  finalize route, teaching the sidecar to attach a workload credential, and
  is a genuinely separate, larger effort (plan rates it L vs M). Flag to the
  team that option (2) closes the *data-integrity* half of the bug (no more
  "upload nothing, finalize with a forged size/hash") but does **not** close
  the fact that the upload token embedded in the returned `upload_url` is
  still plaintext-visible to the client — retiring client-driven finalize
  (option 1) remains the correct long-term fix and should be tracked as a
  fast-follow once internal-auth is wired into this gear.
- **Pre-state check**:
  ```
  rg -n "finalize_upload_by_token" tests/
  ```
  Expected: **no matches** — confirms the token-authenticated finalize path
  has zero existing test coverage (the only finalize coverage today,
  `tests/audit_test.rs::finalize_upload_leaves_audit_row`, exercises
  `DataPlaneService::put_content` → `finalize_upload`, the user-context
  variant, not the sidecar/token variant). Also confirm the missing
  read-back:
  ```
  rg -n "backend.get\(" src/domain/service/write.rs
  ```
  Expected: **no matches** inside `finalize_upload`/`finalize_upload_by_token`
  (the only `backend.get` call in the service layer is in
  `src/domain/service/backend.rs::migrate_backend`, unrelated).

- **Implementation steps**:
  1. **File**: `src/domain/service/write.rs`, `finalize_upload_by_token`
     (currently L421-489). Immediately after computing `backend` (the
     existing `let backend = if backend_id.is_empty() { ... } else { self.backends.get(&backend_id)? };`
     block, L449-453), change the earlier `let version = self.store.get_version(file_id, version_id).await?;`
     (L441) to reject a missing version outright:
     `let version = self.store.get_version(file_id, version_id).await?.ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;`
     — replacing the current `.as_ref().map_or_else(...)` placeholder-default
     pattern used only for `version_mime`/`backend_id`. WHY: the read-back
     step needs `version.backend_path`, which the current code never
     extracts; a version that doesn't exist should never reach a blob
     read-back attempt anyway (tightens an adjacent latent gap for free).
  2. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` — will fail at
     the two downstream `.map_or_else` reads of `version_mime`/`backend_id`;
     update those two lines to read `version.mime_type.clone()` /
     `version.backend_id.clone()` directly (no `Option` handling left).
     Build clean.
  3. **File**: `src/domain/service/write.rs`, same function. After the
     existing policy-size-limit check (ends ~L467) and before building the
     `audit` entry, add the read-back + verification block:
     ```rust
     let blob = backend
         .get(&version.backend_path)
         .await
         .map_err(|_| DomainError::validation(
             "content",
             "no uploaded content found at the backend path; PUT was not completed",
         ))?;
     let actual_size = i64::try_from(blob.len()).unwrap_or(i64::MAX);
     if actual_size != size {
         return Err(DomainError::validation(
             "size",
             "claimed size does not match the uploaded content",
         ));
     }
     crate::infra::storage::Store::verify_content_hash(&blob, &hash_value)?;
     ```
     WHY: `Store::verify_content_hash` (`src/infra/storage/store/mod.rs:130-143`)
     already exists and is used by `migrate_backend` for the identical
     "recompute SHA-256, compare, error on mismatch" need — reuse it rather
     than duplicating hashing logic. `backend.get` failing (no object at that
     path) is the "finalize without prior PUT" attack — mapped to a clean
     400 (`validation`), not a raw backend/500 error.
     NOTE: `StorageBackend::get` returns `bytes::Bytes` (`src/infra/backend/mod.rs:61`),
     a whole-blob read (not a stream); `Bytes::len()` and `&blob` deref-coerce to
     `&[u8]` for `verify_content_hash`/`sha256`, so the snippet compiles as-is.
     COST: this loads the entire blob into memory at every finalize (and in the
     `finalize_upload` sibling, re-reads bytes `put_content` just wrote). For P2's
     current small-object profile that is acceptable; when 1.2 (streaming) and 1.7
     (S3) land, replace the buffered read-back with a streaming hash so large-object
     finalize does not buffer the whole blob — track as a fast-follow, do not block
     0.1 on it.
  4. **File**: same function. Change the final
     `self.store.finalize_version(file_id, version_id, size, hash_value, audit)`
     call to persist the **read-back-derived** values, not the caller's
     claim, even though by this point they have been proven equal: pass
     `actual_size` and `blob`-derived hash (reuse `hash::sha256(&blob)` from
     `crate::infra::content::hash`, matching the exact byte computation
     `Store::verify_content_hash` already performed) instead of the raw
     `size`/`hash_value` parameters. WHY: this is what makes the positive
     test (`finalize_matching_size_and_hash_succeeds`) actually prove the
     read-back happened rather than just re-asserting the caller's own
     input — persisting `size`/`hash_value` verbatim after "verifying" them
     would silently regress into the old bug if the verification step were
     ever accidentally removed/short-circuited later.
  5. **File**: `src/domain/service/write.rs`, `finalize_upload` (the
     user-context sibling, currently L44-107). Apply the identical read-back
     block (steps 1, 3, 4) for symmetry — `DataPlaneService::put_content`
     already writes the blob to the backend before calling this, so the
     read-back will always succeed on the happy path; it exists here purely
     as the same defense-in-depth the plan calls for on both finalize
     entry points, and keeps the two code paths from silently diverging.
  6. **Verify after 3-5**: `cargo build -p file-storage 2>&1 | tail -30` —
     compiles. `cargo clippy -p file-storage -- -D warnings` clean.

- **New/changed tests** — new `tests/finalize_test.rs` (mirror the
  `build_db`/`build_service`/`ctx`/`new_file` helper pattern from
  `tests/multipart_test.rs`; use `InMemoryBackend` so `backend.get`/`put`
  are directly controllable):
  - `finalize_without_prior_put_is_rejected` — create a file (pending
    version, nothing ever `put` to the backend), call
    `svc.finalize_upload(&ctx, file_id, version_id, 100, vec![0u8; 32])`
    directly (bypassing `dp.put_content`); assert `Err(DomainError::Validation { .. })`;
    assert via direct `file_version::Entity::find()` the row is still
    `status = "pending"` with `size = 0`/empty `hash_value` (whatever the
    pending-row placeholder is — confirm via `pending_version()` helper in
    `src/infra/storage/store/mod.rs`).
  - `finalize_size_mismatch_is_rejected` — `backend.put(path, b"hello".into())`
    directly, then call `finalize_upload(..., size = 999, hash = sha256(b"hello"))`;
    assert rejection and DB row unchanged (still pending).
  - `finalize_hash_mismatch_is_rejected` — same but `size = 5` (correct),
    `hash_value = vec![0u8; 32]` (wrong); assert rejection, DB unchanged.
  - `finalize_matching_size_and_hash_succeeds` — `backend.put` real bytes,
    call finalize with the true size/hash; assert `Ok(())`; direct DB read
    asserts `status = "available"` **and** `size`/`hash_value` equal the
    independently-recomputed `hash::sha256(&known_bytes)` — not merely equal
    to what was passed in (this is the assertion that proves read-back, not
    pass-through).
  - `finalize_by_token_without_prior_put_is_rejected` — same as the first
    case but drive it through `finalize_upload_by_token(&claims, size, hash)`
    with hand-built `Claims` (mirrors how `handlers::finalize_version`
    constructs/verifies them) to cover the token path specifically, since it
    has zero existing coverage.
  Run: `cargo test -p file-storage --test finalize_test` — all pass.
  - **E2E** (justified negative — the vulnerability is the trust boundary at
    the real `.public()` route + real token verification, which a unit test
    calling the Rust function directly cannot prove is wired correctly):
    add `test_finalize_forged_size_hash_is_rejected` to
    `testing/e2e/gears/file_storage/lifecycle/test_file_storage_lifecycle.py`
    — presign, skip the real PUT (or PUT different bytes), POST finalize
    with a forged `size`/`hash_hex` carrying the real token; assert the real
    server rejects it (400), and `GET /files/{id}/versions` still shows the
    version `pending`. The happy path stays covered by the existing
    `test_localfs_single_part_full_lifecycle` — no second positive call
    needed per the "one call per API method" E2E rule.

- **Done-check**:
  ```
  cargo test -p file-storage --test finalize_test && \
  rg -n "verify_content_hash" src/domain/service/write.rs
  ```
  Green tests + the verification call present in `write.rs` = complete.

---

### 0.4 — `VersionRepo::finalize` has no status guard; double-finalize corrupts an `Available` version
- **Goal / definition of done**: a second `finalize` call against an
  already-`Available` version is a no-op that leaves `size`/`hash_value`
  untouched and surfaces a clear conflict, not a silent overwrite.
- **Pre-state check**:
  ```
  rg -n 'Status\.eq' src/infra/storage/repo/version_repo.rs
  ```
  Expected current output: matches only in the read-side filters
  `list_pending_older_than` (L227) and `list_non_current_older_than` (L256) —
  **not** in the `finalize` filter (currently
  `src/infra/storage/repo/version_repo.rs:115-144`, filter at L133-137 uses only
  `Column::FileId`/`Column::VersionId`). NOTE: do **not** grep for bare
  `Column::Status` — it also appears at L130 as the `SET status = Available`
  expression *inside* `finalize`, and would falsely suggest the guard already
  exists. The gap is a missing status predicate in the WHERE clause, not the SET.
  Confirm the gap with a quick manual double-call check (optional):
  ```
  cargo test -p file-storage --test multipart_test complete -- --nocapture 2>&1 | tail -5
  ```
  (existing tests pass today because none of them call `finalize`/`complete`
  twice on the same version — this is the coverage gap, not a failing test.)

- **Implementation steps**:
  1. **File**: `src/infra/storage/repo/version_repo.rs`, `finalize`
     (L115-144). Add a status predicate to the existing `Condition::all()`
     filter:
     ```rust
     .add(Column::FileId.eq(file_id))
     .add(Column::VersionId.eq(version_id))
     .add(Column::Status.eq(VersionStatus::Pending.as_str()))
     ```
     (the third `.add(...)` is new). WHY: turns the `UPDATE` into a CAS that
     only matches rows still `pending`, so a second call against an `available`
     row updates zero rows. NOTE: there is **no** existing status-CAS in
     `version_repo.rs` to copy — `mark_available` (L88-111) and `rebind_backend`
     (L273-296) filter only on `(file_id, version_id)` with no status predicate
     (this fix introduces the pattern). `mark_available` in fact has the same
     missing-guard shape; add the identical `Column::Status.eq(Pending)` predicate
     to it in the same commit (a double `mark_available` on an already-available
     row is the same class of silent overwrite).
  2. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` — compiles
     (no signature change; `VersionStatus` is already imported at the top of
     the file).
  3. **File**: `src/infra/storage/repo/version_repo.rs`. No caller changes
     are required for correctness — `finalize` already returns
     `Ok(res.rows_affected == 1)` and all three call sites
     (`write.rs::finalize_upload` L99-105, `write.rs::finalize_upload_by_token`
     L481-487 — line numbers shift slightly after 0.1's edits, re-`rg` to
     confirm — and `multipart_service.rs::complete_multipart_upload`
     L509-527) already treat `false` as an error. For a clearer error than
     the generic `version_not_found` on the "already finalized" case
     specifically, add an optional refinement: in `write.rs::finalize_upload_by_token`
     (and the user-context sibling), when `finalize_version` returns `false`,
     re-fetch the version via the already-available `version` binding (from
     0.1's step 1 restructuring — it's in scope) and branch:
     `if version.status == VersionStatus::Available { DomainError::conflict("version already finalized") } else { DomainError::version_not_found(file_id, version_id) }`.
     WHY: distinguishes "someone deleted the row" (404) from "double-finalize
     race/replay" (409) for callers/telemetry, at zero extra DB cost (the
     `version` value was already read earlier in the same function for the
     read-back check in 0.1).
  4. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` and
     `cargo clippy -p file-storage -- -D warnings` clean.

- **New/changed tests**:
  - New test in `tests/finalize_test.rs` (repo-level, no service layer):
    `version_repo_finalize_twice_second_call_returns_false` — build a
    `VersionRepo` + SQLite `:memory:`-style temp DB (same `build_db()`
    pattern), insert a pending version, call
    `repo.finalize(conn, scope, file_id, version_id, 100, hash_a).await` →
    assert `Ok(true)`; call again with `(200, hash_b)` → assert `Ok(false)`;
    direct `file_version::Entity::find()` read asserts `size == 100` and
    `hash_value == hash_a` (the **first** call's values, never overwritten).
  - `finalize_upload_after_already_available_returns_conflict` (or
    `_not_found` if step 3's refinement is skipped) — service-level: `dp.put_content(...)`
    once (succeeds, version → `Available`), call
    `svc.finalize_upload(&ctx, file_id, version_id, 999, vec![1u8; 32])`
    again directly; assert the distinguishing error variant and that the DB
    row's `size`/`hash_value` still match the first call's values.
  - Extend `tests/multipart_test.rs`: `multipart_complete_after_already_finalized_is_rejected`
    — run a full initiate→report-parts→complete cycle (see 0.2's tests for
    the report-part path once implemented; until then, use the existing
    `upsert_multipart_part` direct-call pattern already in this file) to
    completion, then call `complete_multipart_upload` again for the same
    `upload_id`; assert rejection (the session-state guard already returns
    `multipart_upload_not_in_progress` here, since `complete_multipart_upload`
    flips the session to `completed` on the first call — confirms 0.4's
    version-level guard is defense-in-depth behind the session-level guard,
    not the only line of defense).
  Run: `cargo test -p file-storage --test finalize_test --test multipart_test`
  — all pass.
  - **E2E**: none — deterministic status-guard tightening, fully
    reproducible on SQLite, no PG/AuthZ dependency involved. "Unit only, not
    a seam."

- **Done-check**:
  ```
  cargo test -p file-storage --test finalize_test version_repo_finalize_twice_second_call_returns_false
  ```
  Green = complete.

---

### 0.8 — `DELETE .../versions/{version_id}` silently deletes the whole file on a non-matching id
- **Goal / definition of done**: deleting a version id that does not match
  the file's single remaining version returns `404 version_not_found` and
  leaves the file and its version untouched, instead of deleting the whole
  file.
- **Pre-state check**:
  ```
  rg -n "all.len\(\) <= 1" src/domain/service/read_ops.rs
  ```
  Expected: one match, in `delete_version` (currently
  `src/domain/service/read_ops.rs:261-304`, the branch at L275-280 — actual
  code:
  ```rust
  let all = self.store.list_versions(file_id).await?;
  if all.len() <= 1 {
      return self.delete_file_inner(ctx, file_id).await;
  }
  ```
  — confirms the `find(|v| v.version_id == version_id)` guard (L281) is
  reached only in the `else` branch, exactly as the plan states — line
  numbers match the plan closely).

- **Implementation steps**:
  1. **File**: `src/domain/service/read_ops.rs`, `delete_version`
     (L261-304). Change the `all.len() <= 1` branch to:
     ```rust
     if all.len() <= 1 {
         if !all.iter().any(|v| v.version_id == version_id) {
             return Err(DomainError::version_not_found(file_id, version_id));
         }
         return self.delete_file_inner(ctx, file_id).await;
     }
     ```
     WHY: this is the minimal fix — it also correctly handles the
     (currently latent, same-shaped) `all.len() == 0` edge case, where the
     old code would call `delete_file_inner` on a file with zero versions
     for *any* `version_id` at all; now it 404s instead.
  2. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` — compiles
     (no new imports needed; `DomainError::version_not_found` already used
     elsewhere in this file).

- **New/changed tests** — extend `tests/service_test.rs` (or
  `tests/audit_test.rs` near `delete_version_leaves_audit_row`, whichever
  already has a `dp.put_content` + `svc.delete_version` helper chain — check
  with `rg -n "fn delete_version_leaves_audit_row" tests/audit_test.rs` and
  colocate there for shared setup):
  - `delete_version_single_version_file_wrong_id_returns_not_found` — create
    a file with exactly one `Available` version `v1` (via `dp.put_content`),
    call `svc.delete_version(&ctx, file_id, Uuid::now_v7())` (a random,
    non-existent id); assert `matches!(err, DomainError::VersionNotFound { .. })`;
    direct DB reads assert both the `files` row and `v1`'s `file_versions`
    row still exist.
  - `delete_version_single_version_file_matching_id_deletes_whole_file` —
    positive control: call with `v1`'s real id; assert `Ok(())`; direct DB
    read confirms the `files` row is gone (preserves today's intended
    behavior — this is the existing regression-lock case, add it only if
    not already covered by an existing test in the file).
  Run: `cargo test -p file-storage --test service_test delete_version` (or
  wherever colocated) — all pass.
  - **E2E**: none — pure conditional-branch-ordering bug, deterministic,
    fully reproducible on SQLite. "Unit only, not a seam."

- **Done-check**:
  ```
  cargo test -p file-storage delete_version_single_version_file_wrong_id_returns_not_found
  ```
  Green = complete.

---

### 0.3 — Background sweep can delete a live, bound version mid-completion
- **Goal / definition of done**: the sweep never deletes a multipart
  session's pending version row unless it first wins the session's own
  `in_progress → aborted` CAS; a session that a concurrent `complete` already
  finished is left completely untouched by the sweep.
- **Pre-state check**:
  ```
  sed -n '223,256p' src/domain/cleanup.rs
  ```
  Expected: `abort_expired_multipart_session` calls
  `self.cleanup_expired_session_version(&session).await` (which unconditionally
  deletes the pending version row, no status/CAS check) **before** calling
  `self.store.abort_multipart_upload(session.upload_id, abort_audit).await`
  (the session-level CAS) — confirms the plan's root cause: version cleanup
  happens before, not after, the session CAS. Also confirm
  `complete_multipart_upload` never checks expiry:
  ```
  rg -n "expires_at" src/domain/multipart_service.rs
  ```
  Expected: matches only inside `initiate_multipart_upload` (setting it) —
  none inside `complete_multipart_upload` (L400-552).

- **Implementation steps**:
  1. **File**: `src/domain/cleanup.rs`, `abort_expired_multipart_session`
     (currently L223-256). Reorder so the session CAS runs first and gates
     the version cleanup:
     ```rust
     async fn abort_expired_multipart_session(&self, session: MultipartUploadSession) -> usize {
         let abort_audit = AuditEntry { /* ...unchanged... */ };
         match self.store.abort_multipart_upload(session.upload_id, abort_audit).await {
             Ok(true) => {
                 // We won the CAS: no concurrent complete can have bound this
                 // version afterward. Safe to clean up the backend handle and
                 // delete the pending version row.
                 self.cleanup_expired_session_version(&session).await;
                 1
             }
             Ok(false) => {
                 // A concurrent complete/abort already transitioned the
                 // session out of in_progress. If it was `complete`, the
                 // version is now Available and bound — do NOT touch it.
                 tracing::info!(
                     upload_id = %session.upload_id,
                     "cleanup: skipping version cleanup, session no longer in_progress \
                      (concurrent complete/abort won the race)"
                 );
                 0
             }
             Err(e) => {
                 tracing::warn!(error = ?e, upload_id = %session.upload_id,
                     "cleanup: failed to mark expired multipart upload as aborted");
                 0
             }
         }
     }
     ```
     WHY: this is exactly the CAS-first pattern `abort_multipart_upload`
     (the user-driven path, `multipart_service.rs:614-627`) already uses —
     the sweep was doing the two steps in the wrong order.
  2. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` — compiles;
     `sweep_expired_multipart`'s caller contract (`usize` per session) is
     unchanged.
  3. **File**: `src/domain/multipart_service.rs`, `complete_multipart_upload`
     (L400-434). Add a defense-in-depth expiry check immediately after the
     existing `session.state != MultipartUploadState::InProgress` check
     (around L428-433):
     ```rust
     if session.expires_at <= OffsetDateTime::now_utc() {
         return Err(DomainError::multipart_upload_not_in_progress(
             upload_id,
             "expired",
         ));
     }
     ```
     WHY: closes the narrow residual window where `complete` reads a
     not-yet-swept `in_progress` session whose `expires_at` has already
     passed, races ahead of the next sweep tick, and finalizes content that
     should have been aborted. Reuses the existing error variant (adds an
     `"expired"` state string) rather than introducing a new one.
  4. **Verify**: `cargo build -p file-storage 2>&1 | tail -20`.
  5. **Recommended additional hardening (goes beyond the plan's literal
     text, included because analysis below shows step 1 alone leaves a
     narrower residual race)**: even with step 1's reordering, a session can
     still be caught mid-flight — `complete_multipart_upload` calls
     `finalize_version` (flips the version `pending → available` and binds
     it) several lines *before* its own final
     `self.store.complete_multipart_upload(upload_id, audit)` CAS
     (L537-549). If the sweep's session CAS (step 1) runs in that exact
     window — session row is still `in_progress` in the DB because
     `complete` hasn't reached its own CAS yet — the sweep's CAS succeeds
     (`Ok(true)`), and it proceeds to delete a version that `finalize_version`
     already flipped to `Available` moments earlier. Close this by making
     `cleanup_expired_session_version`'s pending-version delete itself
     status-guarded: add `VersionRepo::delete_if_status<C: DBRunner>(conn, scope, file_id, version_id, expected: VersionStatus) -> Result<bool, DomainError>`
     in `src/infra/storage/repo/version_repo.rs` (same `Condition::all()` +
     `Column::Status.eq(expected.as_str())` pattern as 0.4's `finalize` fix),
     expose it through `CleanupStore` as
     `delete_pending_version(file_id, version_id, audit) -> Result<bool, DomainError>`
     in `src/domain/ports.rs` + the `Store` impl, and have
     `cleanup_expired_session_version` (`src/domain/cleanup.rs:260-298`) call
     that instead of the unguarded `self.store.delete_version(...)`. WHY:
     turns the version delete itself into a CAS (`pending`-only), so even a
     mid-flight interleaving can no longer delete an `Available` row — the
     `UPDATE`/`DELETE ... WHERE status = 'pending'` simply matches zero rows
     in that case, exactly like 0.4's fix for `finalize`.
  6. **Verify after 5**: `cargo build -p file-storage 2>&1 | tail -30`;
     `cargo clippy -p file-storage -- -D warnings` clean.

- **New/changed tests** — extend `tests/cleanup_test.rs` (unit only; races
  are tested as deterministic call-orderings per the unit-testing doctrine,
  never via `sleep`/real concurrency):
  - `sweep_after_complete_wins_does_not_delete_bound_version` — initiate a
    multipart session, report parts (or, until 0.2 lands, use the existing
    `upsert_multipart_part` direct-call pattern already in this file), call
    `complete_multipart_upload` to full success (binds `file.content_id`),
    then set the session's `expires_at` into the past **after** the successful
    complete, via a test helper / raw update. (Do NOT build the session with a
    past `expires_at` from the start — step 3 makes `complete` reject any
    still-`in_progress` expired session, so the complete must happen first,
    then backdate.) Then call
    `engine.run_sweep()`. Assert: (a) the version row still exists and is
    `Available` via direct `file_version::Entity::find()`, (b)
    `files.content_id` unchanged, (c) `SweepResult.expired_multipart_aborted == 0`
    for this session (the CAS returned `false`).
  - `sweep_before_complete_wins_cleans_up_expired_session` — reverse order:
    initiate a session with a past `expires_at`, call `engine.run_sweep()`
    first (CAS succeeds, version deleted, session `aborted`), then attempt
    `complete_multipart_upload` for the same `upload_id`; assert it now
    fails (`multipart_upload_not_in_progress`), and direct DB reads confirm
    the version row is gone.
  - `complete_after_session_expired_is_rejected` — session `expires_at` in
    the past, state still `in_progress` (no sweep involved at all); call
    `complete_multipart_upload` directly; assert rejection (covers step 3's
    defense-in-depth independent of the sweep).
  - If step 5's hardening is implemented: `sweep_mid_flight_after_finalize_but_before_session_cas_does_not_delete_available_version`
    — simulate the narrow window directly at the repo/store level: manually
    flip the version to `Available` (as `finalize_version` would, without
    going through the full `complete_multipart_upload` orchestration) while
    the session row is still `in_progress`, then call
    `cleanup_expired_session_version` directly; assert the version row is
    untouched (status-guarded delete matched zero rows).
  Run: `cargo test -p file-storage --test cleanup_test` — all pass, suite
  stays sleep-free and completes in well under 5s.
  - **E2E**: none — "unit only, not a seam." The orderings above **are** the
    race, expressed deterministically; e2e's no-sleep rule makes a
    real-concurrency version of this test structurally forbidden there.

- **Done-check**:
  ```
  cargo test -p file-storage --test cleanup_test sweep_after_complete_wins_does_not_delete_bound_version sweep_before_complete_wins_cleans_up_expired_session
  ```
  Both green = complete.

---

### 0.2 — Multipart upload is structurally non-functional
- **Goal / definition of done**: (a) it is explicit and tested which backend
  topology multipart works against today (locking in the current 422 against
  the real `[local-fs, memory]` default topology so a silent regression is
  caught), and (b) the "report part" callback path exists and is exercised
  end-to-end against at least one `multipart_native` backend, so
  `complete_multipart_upload`'s `list_multipart_parts` is never structurally
  empty. Full production-grade multipart against `local-fs` (or S3) is **not**
  required to close this item — see the two structural fixes below, and the
  explicit S3-first sequencing note.
- **Pre-state check**:
  ```
  rg -n "multipart_native" src/infra/backend/local_fs.rs
  ```
  Expected: **no match** (confirms `LocalFsBackend::capabilities()`,
  L61-66, never sets it — only `range_native: true`). Confirm the
  report-part gap:
  ```
  rg -n "upsert_multipart_part" src/bin/sidecar.rs
  ```
  Expected: **no match** — the sidecar's `upload_multipart_part` handler
  (`src/bin/sidecar.rs:297-380`) writes to `{backend_path}.part.{N}` via
  plain `backend.put` (L363-364) and returns a JSON body, but never calls
  back into the control plane. Confirm the resulting empty-parts bug:
  ```
  rg -n "list_multipart_parts" src/domain/multipart_service.rs
  ```
  Expected: one call, `complete_multipart_upload` L435 — with nothing ever
  populating `multipart_upload_parts` in a real deployment, this always
  returns `[]`.

- **Structural fix group A — backend capability decision** (independently
  verifiable from group B):
  1. **Decision**: per the plan's own fix-approach note ("OR gate this
     behind the S3 backend from Tier 1 item 1.7 and make multipart S3-only
     for now"), **this guide recommends sequencing full backend-native
     multipart after 1.7 (S3)**, not attempting a true offset-write
     `LocalFsBackend` implementation now. Reason found during verification:
     `StorageBackend::upload_part(&self, path, upload_handle, part_number, data)`
     (`src/infra/backend/mod.rs:97-105`) has no `offset` parameter — the
     server-authoritative plan (`compute_plan` in `src/domain/multipart.rs`)
     already computes exact per-part offsets and the signed `MultipartClaims`
     already carries `offset` (`src/infra/signed_url/mod.rs:71-76`), but nothing
     downstream of the token currently threads that offset into a backend
     call. Making `local-fs` `multipart_native` correctly would require
     widening the trait signature to carry `offset`/`part_size` — exactly
     the same signature surface item **1.7.4** (S3 streaming) already plans
     to touch. Doing it twice is wasted churn; do it once, with 1.7.
  2. **File**: `docs/features/multipart-coordinator.md`. Add an explicit
     caveat (independent, S-effort, do immediately regardless of the rest of
     this item): state that `LocalFsBackend.multipart_native == false`, so
     every `POST /files/{id}/multipart` 422s (`multipart_not_supported`)
     against the default topology until a `multipart_native` backend (today:
     `InMemoryBackend`, non-durable — see 0.5; going forward: S3, item 1.7)
     is configured as the default backend. WHY: closes the doc/code gap
     called out in Tier 3 item 3.3 immediately, at zero engineering risk.
  3. **Verify**: `rg -n "multipart_native == false\|not functional" docs/features/multipart-coordinator.md`
     shows the new caveat.

- **Structural fix group B — the "report part" callback** (the must-do,
  backend-agnostic fix; unblocks *any* future `multipart_native` backend
  including S3 the moment 1.7 lands, without further control-plane changes):
  4. **File**: `src/api/rest/routes.rs`. Register a new token-authenticated,
     `.public()` route (same pattern as `finalize_version`, L41-67):
     `POST {BASE}/files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report`,
     handler `handlers::report_multipart_part`. WHY: matches the plan's
     option (a) — a dedicated report endpoint, matching
     `docs/features/multipart-coordinator.md`'s original intent, and reuses
     the existing per-part `multipart_part` signed token (already minted at
     initiate time, already scoped to exactly `(file_id, version_id,
     upload_id, part_number)` via `MultipartClaims`) rather than inventing a
     new auth mechanism.
  5. **File**: `src/api/rest/handlers.rs`. Add
     `pub async fn report_multipart_part(...)` mirroring `finalize_version`
     (L524-557): extract `x-fs-token`, verify via `Verifier`, assert
     `claims.op == Op::MultipartPart`, `claims.file_id`/`claims.version_id`/
     `claims.multipart.upload_id`/`claims.multipart.part_number` all match
     the path params; decode a small JSON body `{backend_etag: String,
     hash_hex: String, size: i64}`; call a new
     `MultipartService::report_part(&claims, backend_etag, hash_value, size)`.
  6. **File**: `src/domain/multipart_service.rs`. Add
     `pub async fn report_part(&self, claims: &Claims, backend_etag: String, hash_value: Vec<u8>, size: i64) -> Result<(), DomainError>`
     that: loads the session via `self.store.get_multipart_upload(claims.multipart.upload_id)`,
     confirms `session.file_id == claims.file_id && session.version_id ==
     claims.version_id && session.state == InProgress` (else
     `multipart_upload_not_found`/`multipart_upload_not_in_progress`), then
     calls `self.store.upsert_multipart_part(upload_id, i32::try_from(claims.multipart.part_number)?, &backend_etag, hash_value, size, now)`
     — the existing `MultipartStore::upsert_multipart_part`
     (`src/domain/ports.rs:160-168`) already exists and is already
     implemented on `Store`; it simply has zero callers today. WHY: this is
     the entire missing link — no new persistence code needed, only a new
     call site.
  7. **File**: `src/bin/sidecar.rs`, `upload_multipart_part`
     (L297-380). After the existing `state.backend.put(&part_path, body).await`
     succeeds (L364), add a call to the new report endpoint mirroring
     `finalize_with_control_plane`'s structure (build URL, POST JSON
     `{backend_etag: part_etag, hash_hex: part_etag, size: body_len}`,
     `x-fs-token: token` header, treat a failed callback as a `502` response
     to the uploading client — same "the client should retry" contract
     finalize already uses). WHY: this is the sidecar half of group B — today
     it silently drops this responsibility entirely.
  8. **Verify after 4-7**: `cargo build -p file-storage --bin sidecar 2>&1 | tail -30`
     and `cargo build -p file-storage 2>&1 | tail -30` both compile.

- **New/changed tests** — rewrite the misleading topology in
  `tests/multipart_test.rs` (per Tier 3 item 3.3's verifier finding: existing
  green tests build their own `BackendRegistry::new(vec![InMemoryBackend], "mem")`
  and call `store.upsert_multipart_part(...)` directly — a topology/call path
  that never occurs via `gear.rs`'s real wiring):
  - `multipart_initiate_against_real_default_topology_is_rejected_until_backend_supports_it`
    — build the registry exactly as `gear.rs::init` does
    (`BackendRegistry::new(vec![LocalFsBackend("local-fs"), InMemoryBackend("memory")], "local-fs")`),
    call `initiate_multipart_upload`; assert
    `matches!(err, DomainError::MultipartNotSupported { .. })`. Locks in
    today's real behavior so a silent "it works now" regression (or silent
    breakage) is caught; flip the assertion once a real default-topology
    backend sets `multipart_native: true`.
  - `multipart_complete_uses_reported_parts_not_empty_list` — using the
    `InMemoryBackend`-only registry (multipart-capable), initiate → for each
    part call the sidecar's new report path *in-process* (either call
    `MultipartService::report_part` directly with hand-built `Claims`, or —
    preferably — go through `handlers::report_multipart_part` via
    `Router::oneshot` for a route-registration smoke check too) → call
    `complete_multipart_upload`; assert `multipart_upload_parts` has exactly
    N rows matching the reported etags/hashes/sizes via direct entity
    `find()` (not `list_multipart_parts` — the very method under test), and
    the completed version's `size` equals the sum of those DB rows.
  - `multipart_initiate_rejected_when_backend_not_multipart_native` —
    table-driven: `[("local-fs"-only registry, expect reject), ("memory"-only
    registry, expect accept)]`.
  Run: `cargo test -p file-storage --test multipart_test` — all pass.
  - **E2E**: none for the structural bug itself (deterministic
    registration/wiring defect, fully caught by the unit test above — "unit
    only, not a seam"). Once a real backend supports multipart (S3, item
    1.7), add one happy-path
    `test_multipart_full_lifecycle_against_real_sidecar` to
    `lifecycle/test_file_storage_lifecycle.py` — genuinely needs the real
    sidecar HTTP part-upload + report-part callback wiring end to end, which
    SQLite+fake-backend unit tests cannot exercise.

- **Done-check**:
  ```
  cargo test -p file-storage --test multipart_test multipart_initiate_against_real_default_topology_is_rejected_until_backend_supports_it multipart_complete_uses_reported_parts_not_empty_list
  ```
  Both green = the two structural fixes are independently verified.

---

### 0.6 — Retention sweep swallows a `list_versions` error and still deletes the file
- **Goal / definition of done**: a transient `list_versions` failure during
  the retention sweep aborts that file's expiry (no delete) instead of
  proceeding to delete the file row while silently treating the error as
  "zero versions" (which would orphan the real, un-enumerated blobs
  permanently).
- **Pre-state check**:
  ```
  rg -n "unwrap_or_default" src/domain/cleanup.rs
  ```
  Expected: one match, in `expire_file` (currently
  `src/domain/cleanup.rs:434-499`, the `list_versions` call at L437-441:
  `self.store.list_versions(file.file_id).await.unwrap_or_default();`).
  Compare against the already-correct sibling pattern:
  ```
  rg -n "list_metadata" src/domain/cleanup.rs
  ```
  shows `maybe_expire_file` (L408-419) already propagates a `list_metadata`
  error via `match ... Err(e) => { tracing::warn!(...); return 0; }` —
  `expire_file` is the one outlier that swallows instead of propagating.

- **Implementation steps**:
  1. **File**: `src/domain/cleanup.rs`, `expire_file` (L434-499). Replace:
     ```rust
     let versions = self.store.list_versions(file.file_id).await.unwrap_or_default();
     ```
     with:
     ```rust
     let versions = match self.store.list_versions(file.file_id).await {
         Ok(v) => v,
         Err(e) => {
             tracing::warn!(
                 error = ?e,
                 file_id = %file.file_id,
                 "cleanup: failed to list versions for retention-expired file; skipping expiry"
             );
             return 0;
         }
     };
     ```
     placed **before** the `audit`/`event` construction and the
     `delete_file_with_event` call later in the function. WHY: matches the
     exact pattern `maybe_expire_file` already uses for `list_metadata`; a
     confirmed version list is now a precondition for ever calling
     `delete_file_with_event`.
  2. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` — compiles
     (no signature change, same `usize` return type via the early `return 0`).

- **New/changed tests** — extend `tests/cleanup_test.rs` with a
  fault-injectable fake store (same shape as `enforce_test.rs`'s
  `ErroringQuota`/`CappedQuota` pattern — here, a small wrapper `Arc<dyn
  CleanupStore>` whose `list_versions` always returns `Err(..)` while every
  other method delegates to a real `Store`; since `CleanupStore` is a narrow
  trait this is a small hand-written newtype, not a mocking-framework fake):
  - `expire_file_list_versions_error_does_not_delete_file` — seed a
    tenant-scope retention rule with `max_age_days = 0` (guaranteed to
    match, same pattern the existing retention sweep tests in this file
    already use) and a file old enough to match it; wrap the store so
    `list_versions` errors for that specific `file_id`; call
    `engine.run_sweep()`; assert (a) the file row still exists via direct
    `file::Entity::find()`, (b) `SweepResult.retention_expired_deleted == 0`
    for this file, (c) a **second**, unrelated matching file in the same
    sweep (real `list_versions`, no fault injected) **is** deleted —
    proving one file's fault doesn't abort the whole sweep.
  Run: `cargo test -p file-storage --test cleanup_test expire_file_list_versions_error_does_not_delete_file`
  — passes.
  - **E2E**: none — fault injection via a fake store trait is inherently a
    unit concern; there is no stable, non-flaky way to force this specific
    transient-DB-error branch through real PostgreSQL. "Unit only, not a
    seam."

- **Done-check**:
  ```
  cargo test -p file-storage --test cleanup_test expire_file_list_versions_error_does_not_delete_file
  ```
  Green = complete.

---

### 0.5 — In-memory backend registered in every deployment, reachable via `migrate_backend`
- **Goal / definition of done**: `InMemoryBackend` is absent from the
  registry by default (only present when an explicit dev/test flag is set),
  and even when present, `migrate_backend` refuses to move content onto a
  non-durable backend unless the caller holds an elevated scope.
- **Pre-state check**:
  ```
  rg -n "InMemoryBackend::new" src/gear.rs
  ```
  Expected: one unconditional match (`src/gear.rs:77`,
  `let memory: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new(MEMORY_ID));`)
  — confirms it is always constructed, with no config gate. Confirm no
  durability flag exists yet:
  ```
  rg -n "durable" src/infra/backend/mod.rs
  ```
  Expected: **no match** (`BackendCapabilities`, L33-44, has no `durable`
  field today).

- **Implementation steps**:
  1. **File**: `src/config.rs`. Add a new field to `FileStorageConfig`:
     `#[serde(default)] pub enable_in_memory_backend: bool,` (default
     `false`, matching the existing `serde(default)` style used for
     `signing_key_seed`) plus the corresponding `Default` impl entry and
     `Debug` impl field (append to `fmt::Debug::fmt`'s `debug_struct` chain
     like the other plain fields — it holds no secret, so no redaction
     needed). WHY: makes in-memory-backend registration an explicit,
     off-by-default opt-in rather than an unconditional constant, matching
     how `enable_background_sweep` is already gated.
  2. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` — compiles;
     `cargo test -p file-storage config_tests::rejects_unknown_fields` still
     passes (new field has a default, so omitting it from JSON config stays
     valid).
  3. **File**: `src/gear.rs`, `init` (L62-79). Change the backend-vec
     construction:
     ```rust
     let local: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new(LOCAL_FS_ID, &cfg.storage_root));
     let mut backend_list: Vec<Arc<dyn StorageBackend>> = vec![local];
     if cfg.enable_in_memory_backend {
         backend_list.push(Arc::new(InMemoryBackend::new(MEMORY_ID)));
     }
     let backends = BackendRegistry::new(backend_list, LOCAL_FS_ID)
         .map_err(|e| anyhow::anyhow!("backend registry: {e}"))?;
     ```
     WHY: `local-fs` remains the unconditional default (unaffected — it's
     durable); `memory` only joins the registry when explicitly opted in.
     Existing test/dev configs relying on `"memory"` being present
     (`rg -n "enable_in_memory_backend\|\"memory\"" config/*.yaml tests/`)
     must set the new flag explicitly — check `config/e2e-local.yaml`,
     `config/quickstart.yaml`, and any test harness config and add
     `enable_in_memory_backend: true` there if they rely on the `"memory"`
     backend id being registered.
  4. **Verify**: `cargo build -p file-storage 2>&1 | tail -30`; run the full
     existing suite once to catch any test/config that silently depended on
     `"memory"` being present unconditionally:
     `cargo test -p file-storage 2>&1 | tail -60` — any new failures here
     point at a config that needs the new flag added (see step 3's note).
  5. **File**: `src/infra/backend/mod.rs`, `BackendCapabilities`
     (L31-44). Add `pub durable: bool,` to the struct. Since the struct
     derives `Default` and `durable: bool` defaults to `false`, **every**
     existing backend impl's `capabilities()` that doesn't explicitly set it
     will now report `durable: false` — this is backwards (local-fs and S3
     are durable). Explicitly set `durable: true` in
     `LocalFsBackend::capabilities()` (`src/infra/backend/local_fs.rs:61-66`)
     and leave `InMemoryBackend::capabilities()`
     (`src/infra/backend/in_memory.rs:58-64`) relying on the `false`
     default (add a comment noting it's intentional). WHY: this is the
     capability flag the plan's `migrate_backend` guard (next step) keys off.
  6. **Verify**: `cargo build -p file-storage 2>&1 | tail -20` — compiles
     (struct field addition + two explicit-set edits).
  7. **File**: `src/domain/service/backend.rs`, `migrate_backend`
     (L35-117). After resolving `dest = self.backends.get(target_backend_id)?`
     (L69) and before reading the source blob, add:
     ```rust
     if !dest.capabilities().durable {
         self.authorizer
             .authorize(ctx, actions::ADMIN_POLICY, &file.gts_file_type, Some(file_id))
             .await?;
     }
     ```
     reusing the same `actions::ADMIN_POLICY` action introduced in 0.7 (if
     0.7 has not landed yet, introduce the constant here instead — either
     order is fine, they're independent per the plan). WHY: closes the hole
     even for a legitimately-configured non-durable backend (e.g. a
     dev-tenant `memory` backend) — an ordinary WRITE-authorized caller can
     no longer silently migrate durable content onto it.
  8. **Verify**: `cargo build -p file-storage 2>&1 | tail -20`;
     `cargo clippy -p file-storage -- -D warnings` clean.

- **New/changed tests**:
  - Extend `src/gear_tests.rs`:
    `gear_default_config_excludes_in_memory_backend` — construct
    `FileStorageConfig::default()`, verify (at the unit level available
    here — `gear_tests.rs` cannot call `init()` without a live `GearCtx`, so
    assert `cfg.enable_in_memory_backend == false` and, if feasible, extract
    the backend-construction logic from step 3 into a small
    `fn build_backend_registry(cfg: &FileStorageConfig) -> Result<BackendRegistry, DomainError>`
    free function so it's callable directly from a unit test without a full
    gear `init()` — call it with `FileStorageConfig::default()` and assert
    `registry.list().iter().all(|(id, _)| id != "memory")`.
    `gear_dev_flag_enables_in_memory_backend` — same helper with
    `enable_in_memory_backend: true`, assert `"memory"` present.
  - Extend `tests/cleanup_test.rs` (already covers `migrate_backend` per its
    module doc, "Backend migration (`migrate_backend`) — happy path and
    rejection of versioned files"):
    `migrate_backend_rejects_non_durable_target_for_non_admin` — build a
    dual-backend registry (`build_all_dual_backend`-style helper already in
    this file, but make the `"alt"` backend an `InMemoryBackend` — non-durable
    by the new default) with a non-admin test authorizer; call
    `svc.migrate_backend(&ctx, file_id, "alt")`; assert
    `Forbidden`/`PermissionDenied` and, via direct `file_version::Entity::find()`,
    that `backend_id`/`backend_path` are unchanged.
    `migrate_backend_allows_non_durable_target_for_admin_scope` — same with
    an admin-scoped authorizer; assert `Ok(())` and the DB row now points at
    `"alt"`.
  Run: `cargo test -p file-storage gear_default_config_excludes_in_memory_backend gear_dev_flag_enables_in_memory_backend migrate_backend_rejects_non_durable_target_for_non_admin migrate_backend_allows_non_durable_target_for_admin_scope`
  — all pass.
  - **E2E**: none — "unit only, not a seam." Registration is a pure
    config-to-registry construction function; the authorization gate is a
    deterministic scope check. Neither needs HTTP/PG to prove.

- **Done-check**:
  ```
  cargo test -p file-storage gear_default_config_excludes_in_memory_backend migrate_backend_rejects_non_durable_target_for_non_admin
  ```
  Both green = complete.

---

### 0.9 — Cross-user file enumeration via `list_files` (resource-less authorize + attacker-controlled owner filter)
- **Goal / definition of done**: a non-privileged caller cannot list another
  subject's files. `GET /files?owner_kind=user&owner_id=<other>` from a caller
  who is neither `<other>` nor holds a tenant-wide admin grant is rejected
  (`Forbidden`) or transparently scoped to the caller's own files — never
  returns the victim's file metadata.
- **Why this is Tier 0 (not folded into 0.7)**: same root cause as 0.7
  (resource-less `authorize(ctx, actions::READ, "", None)`), but the exposed
  surface is **actual file listings** (name, `gts_file_type`, `content_id`,
  timestamps), a higher-impact intra-tenant data leak than policy config. It is
  a distinct endpoint and needs its own fix + test.
- **Pre-state check**:
  ```
  rg -n 'authorize\(ctx, actions::READ, "", None\)' src/domain/service/read_ops.rs
  rg -n 'owner_kind|owner_id' src/api/rest/handlers.rs
  ```
  Expected: `list_files` (`src/domain/service/read_ops.rs:46-64`) authorizes
  resource-less, then queries `store.list_files(&Self::tenant_scope(ctx), owner, …)`
  where `owner` is built from the request query `ListQuery` (`handlers.rs:41-47`,
  `165-184`) — unbound to `ctx.subject_id()`. `PolicyEnforcerAuthorizer::authorize`
  (`src/infra/authz.rs:53-68`) forwards only `owner_tenant_id` + `gts_file_type`,
  so with `gts_file_type=""`/`file_id=None` the PDP can only render a coarse
  "may this subject read files at all" verdict — no per-owner check.
- **Implementation steps**:
  1. **File**: `src/domain/service/read_ops.rs`, `list_files` (L46-64). After
     the existing coarse `READ` authorize, add an ownership gate mirroring 0.7's
     try-admin-first pattern: if the requested `owner_kind == user` and
     `owner_id != ctx.subject_id()`, require `actions::ADMIN_POLICY` (reuse the
     0.7 constant); on `Forbidden`, return `DomainError::Forbidden` instead of
     listing. A caller listing their **own** files (`owner_id ==
     ctx.subject_id()`) or a tenant-admin proceeds. For `owner_kind` values
     other than `user` (e.g. tenant/service owners), require `ADMIN_POLICY`
     unless the caller is that owner. WHY: closes
     `GET /files?owner_kind=user&owner_id=<victim>` for non-admins.
  2. **Consider defense-in-depth**: if the product intent is "a user only ever
     lists their own files," drop the client-supplied `owner_id` entirely for
     non-admin callers and force `owner_id = ctx.subject_id()` server-side —
     simpler and eliminates the parameter as an attack surface. Bring this
     product decision to the team (🛑, same discussion as 0.7's admin scope);
     the ownership-gate above is the minimal fix either way.
  3. **Verify**: `cargo build -p file-storage 2>&1 | tail -20`; clippy clean.
- **New/changed tests** — extend `tests/service_test.rs` (or a new
  `tests/list_authz_test.rs`) using the deny-capable `ScopedTestAuthorizer`
  from 0.7 (not `TenantOnlyAuthorizer`, which grants everything):
  - `list_files_foreign_owner_without_admin_is_denied` — user A creates files;
    user B calls `list_files` with `owner_id = user_a`; assert `Forbidden` and
    that no victim rows are returned.
  - `list_files_self_owner_is_allowed` — positive control.
  - `list_files_foreign_owner_with_admin_scope_is_allowed` — `is_admin = true`;
    assert the victim's files are returned.
  - **E2E**: same optional cross-subject seam as 0.7 (reuse
    `E2E_AUTH_TOKEN_USER_B`); `test_cross_user_file_listing_denied_by_real_authz`.
- **Done-check**: `cargo test -p file-storage list_files_foreign_owner_without_admin_is_denied` green.

---

### 0.10 — Idempotency replay bypasses authorization, policy and quota; key is scoped by request-body owner, not the caller
- **Goal / definition of done**: the idempotency replay path never returns a
  stored `UploadTicket` (which embeds a live signed PUT URL) to a caller who is
  not authorized for the write *now*, and one caller can never surface another
  caller's ticket by reusing/guessing their `(owner_kind, owner_id, key)` tuple.
- **Why this is Tier 0 and distinct from 2.1**: 2.1 fixes "replay doesn't
  validate the retried *body*". This is an **authorization-ordering + identity**
  bug: the replay lookup and early `return Ok(ticket)` happen *before*
  `validate_gts_type` and `authorize(…, WRITE, …)`, and the key is keyed on the
  request-body `owner_id`, not `ctx.subject_id()`. A caller who guesses a
  non-random key (`"upload-1"`) with the victim's `owner_id` gets a working
  upload URL to the victim's file with zero authorization; a caller whose WRITE
  was revoked mid-window can still replay. Both independent audits flagged this.
- **Pre-state check**:
  ```
  sed -n '100,130p' src/domain/service/create.rs
  ```
  Expected: `create.rs:105-121` looks up `get_idempotency_key` and returns the
  deserialized ticket unconditionally, **before** `validate_gts_type` (L124) and
  `authorizer.authorize(...)` (L125-128). `IdempotencyRepo::get`
  (`idempotency_repo.rs:29-53`) keys on `(tenant_id, owner_kind, owner_id, key)`
  where `owner_kind`/`owner_id` come from the request body (`create.rs:102-103`,
  `handlers.rs:70-88`).
- **Implementation steps**:
  1. **File**: `src/domain/service/create.rs`, `create_file`. **Move** the
     idempotency lookup + replay `return` to **after** `validate_gts_type` and
     the `authorize(ctx, actions::WRITE, &new.gts_file_type, None)` call. So
     every replay is authorized with the caller's current grants before any
     stored ticket is handed back. WHY: a revoked/never-authorized caller now
     hits `Forbidden` instead of a live upload URL.
  2. **File**: same function + `src/infra/storage/repo/idempotency_repo.rs`.
     Bind the idempotency key to the authenticated subject: include
     `ctx.subject_id()` in the key (add a `subject_id` column via the same
     additive-migration discipline as 2.1, or verify on replay that the stored
     record's subject matches `ctx.subject_id()` and treat a mismatch as a fresh
     request / `Forbidden`). WHY: one caller's key can never surface another's
     ticket. Coordinate the column addition with 2.1's `request_hash` migration
     — do both in one additive migration to avoid two schema churns.
  3. **Verify**: `cargo build -p file-storage 2>&1 | tail -20`; clippy clean.
- **New/changed tests** — extend `tests/multipart_test.rs`' idempotency block
  (L584-677) with the deny-capable authorizer:
  - `idempotency_replay_requires_authorization` — seed a ticket as an
    authorized caller; replay as a caller the authorizer denies `WRITE`; assert
    `Forbidden` and that **no** ticket/upload URL is returned.
  - `idempotency_key_scoped_to_subject` — caller A stores key `k` for
    `owner_id = X`; caller B replays `(owner_id = X, k)`; assert B does **not**
    receive A's ticket (fresh request or denial), and A's replay still works.
  - **E2E**: fold into the 0.9/0.7 cross-subject seam if `E2E_AUTH_TOKEN_USER_B`
    exists; otherwise unit coverage is sufficient to land.
- **Done-check**: `cargo test -p file-storage idempotency_replay_requires_authorization idempotency_key_scoped_to_subject` green, and
  `rg -n "authorize" src/domain/service/create.rs` shows the authorize call
  precedes the idempotency lookup.

---

### 0.11 — Retention rules / policies accepted with no semantic validation; a `max_age_days: 0` rule wipes the tenant
- **Goal / definition of done**: `POST /retention-rules` and `PUT /policy`
  reject semantically dangerous or dead input at create time. In particular a
  retention rule that would match **every** file in the tenant (e.g.
  `max_age_days: 0`) is rejected, not silently accepted and executed by the next
  sweep (which permanently deletes every matching file's rows **and** blobs via
  `expire_file`, with no dry-run and no undo).
- **Pre-state check**:
  ```
  rg -n "max_age_days|Duration::days" src/domain/cleanup.rs
  rg -n "fn create_retention_rule" src/domain/policy_service.rs
  ```
  Expected: `create_retention_rule` (`policy_service.rs:164-196`) inserts the
  body as-is; no validation in `dto.rs`/`retention_rule_repo.rs`. The sweep
  matcher (`cleanup.rs:561-575`) evaluates `now - created_at > Duration::days(N)`
  — with `N = 0` this is true for every file. Other silent-dead cases:
  all-criteria-`None` rule (never matches), `scope=user`/`file` with
  `scope_target_id = null` (dead rule), `set_policy(scope=user,
  scope_owner_id=None)` (row effective-policy resolution never reads — it always
  queries `Some(owner_id)`), and a `*/*` mime pattern (treated as deny-all by
  `mime_allowed`/`intersect_mime` in `policy.rs`).
- **Implementation steps**:
  1. **File**: `src/domain/policy_service.rs`, `create_retention_rule` — add a
     `validate_retention_rule(&body)` guard before insert that rejects:
     - all age/inactivity/version-count criteria `None` (a rule that matches
       nothing is almost certainly a mistake → `DomainError::validation`);
     - any `*_days` value `< 1` (0 = "match everything immediately"; if an
       "expire all now" operation is ever a real need it must be an explicit,
       separately-authorized admin action, never a normal retention rule);
     - `scope` ∈ {`user`,`file`} with `scope_target_id = None`.
     WHY: closes the tenant-wipe footgun and the silent-dead-rule cases.
  2. **File**: `src/domain/policy_service.rs`, `set_policy` — reject
     `scope=user` with `scope_owner_id=None` (dead row) and, in the mime policy,
     reject or explicitly define `*/*` rather than letting it act as silent
     deny-all. Confirm the effective-policy reader's expectations
     (`create.rs:40-43` queries `Some(owner_id)`), so a `None`-owner user-scope
     row can never be read — reject it at write time.
  3. **Optional hardening (bring to team)**: make retention deletes emit a
     count-based safety valve — if a single rule would match more than N% of the
     tenant's files in one sweep tick, log loudly / require a confirmation flag.
     Track as a fast-follow; not required to land the validation above.
  4. **Verify**: `cargo build -p file-storage 2>&1 | tail -20`; clippy clean.
- **New/changed tests** — extend `tests/enforce_test.rs` or `src/domain/policy_tests.rs`:
  - `create_retention_rule_zero_max_age_is_rejected` — assert
    `DomainError::Validation` and zero rows written; **and** a companion
    `sweep_does_not_run_zero_age_rule` proving no such rule can reach the sweep.
  - `create_retention_rule_all_criteria_none_is_rejected`.
  - `create_retention_rule_user_scope_without_target_is_rejected`.
  - `set_policy_user_scope_without_owner_is_rejected`.
  - `set_policy_star_slash_star_mime_is_rejected_or_defined` (assert whichever
    semantic the team picks — reject, or documented allow-all).
  - **E2E**: none — deterministic input-validation logic, "unit only, not a seam."
- **Done-check**: `cargo test -p file-storage create_retention_rule_zero_max_age_is_rejected` green.

---

## Tier 1 — Production-readiness blockers (step-by-step)

All paths below are relative to `gears/file-storage/file-storage/` unless stated otherwise. Every file/line reference was re-read directly against `feat/file-storage-p2` while writing this guide (not copied from the plan without verification); a few plan claims turned out to need a correction — flagged inline where relevant (notably in 1.3 and 1.4).

---

### 1.1 — Local-fs backend has no fsync and no atomic write

- **Goal / definition of done**: `LocalFsBackend::put` never leaves a torn/partial file observable by a concurrent reader, and a successful `Ok(())` return means the bytes and the rename that exposed them are fsync'd to durable storage (parent-directory fsync best-effort, never a hard failure on filesystems that don't support it).

- **Pre-state check**:
  ```
  rg -n "sync_all|sync_data|\.tmp\.|rename\(" src/infra/backend/local_fs.rs
  ```
  Expected: no matches. Current `put` (confirmed at `src/infra/backend/local_fs.rs:68-78`) is exactly:
  ```rust
  async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
      let target = self.resolve(path)?;
      if let Some(parent) = target.parent() {
          tokio::fs::create_dir_all(parent).await.map_err(|e| self.io_err(e))?;
      }
      tokio::fs::write(&target, &bytes).await.map_err(|e| self.io_err(e))
  }
  ```
  — a direct write to the final path, no temp file, no fsync.

- **Implementation steps**:
  1. In `src/infra/backend/local_fs.rs`, add a `fsync_parent_dir: bool` field to `LocalFsBackend` (default `true`) and a builder method `with_fsync_parent_dir(mut self, enabled: bool) -> Self`. Keep `LocalFsBackend::new(id, root)` signature unchanged (defaults the field to `true`) so every existing call site (`src/gear.rs:76`, `src/bin/sidecar.rs:95`, `src/infra/backend/backend_tests.rs`, `tests/cleanup_test.rs`) keeps compiling untouched.
  2. Rewrite `put` to: (a) build a sibling temp path `format!("{}.tmp.{}", target.display(), Uuid::now_v7())` in the same directory as `target` (same filesystem is required for step (c)'s rename to be atomic); (b) `tokio::fs::File::create(&tmp)`, write all bytes via `AsyncWriteExt::write_all`, then call the file's `sync_all().await` (durable data + metadata flush) before dropping the handle; (c) `tokio::fs::rename(&tmp, &target).await` — atomic replace on POSIX same-filesystem rename; (d) on success, best-effort fsync the **parent directory** (open it with `tokio::fs::File::open(parent)`, call `.sync_all().await`) — required on Linux/ext4/xfs to make the rename entry itself durable. `uuid` is already a workspace dependency used elsewhere in this crate (no new `Cargo.toml` edit needed).
  3. Make the parent-dir fsync step obey `self.fsync_parent_dir` and never propagate its own error as a `put` failure: on `Err`, `tracing::warn!(error = ?e, "parent-dir fsync failed or unsupported by this filesystem — continuing")` and return `Ok(())` anyway (the blob itself is already durably renamed into place; only the directory-entry durability guarantee is best-effort per the plan's "log a warning, don't fail hard" instruction).
  4. On any failure *before* the rename (temp-file create/write/sync error), attempt `tokio::fs::remove_file(&tmp)` best-effort (ignore its own error) before returning the original `Err` — prevents orphaned `*.tmp.*` files accumulating under the storage root.
  5. Update the module doc comment at the top of `local_fs.rs` (currently silent on durability) to state the temp-file → fsync → rename → parent-dir-fsync sequence and that parent-dir fsync is best-effort.

- **Verify after this step**:
  1. `cargo build -p cf-gears-file-storage --lib` — compiles.
  2. `rg -n "tokio::fs::write\(&target" src/infra/backend/local_fs.rs` — no matches (confirms the direct-write path is gone).
  3. `rg -n "sync_all" src/infra/backend/local_fs.rs` — at least two matches (file sync + parent-dir sync).

- **New/changed tests** — extend `src/infra/backend/backend_tests.rs` (mirrors the existing `local_fs_put_get_round_trip` at line 63):
  - `local_fs_put_is_atomic_under_concurrent_writers` (unit) — spawn N (e.g. 8) concurrent `put()` calls to the *same* `backend_path` with N distinct full-size payloads (e.g. 64KiB each, all different byte patterns so a torn mix is detectable); `join_all`; then `get()` once and assert the result byte-for-byte equals exactly one of the N inputs in full — never a prefix/suffix mix of two. This is the deterministic, non-timing-based way to observe atomicity per `12_unit_testing.md`'s "no sleep/timing" rule — concurrency via `tokio::spawn`/`join_all` is fine, the assertion itself does not depend on ordering.
  - `local_fs_put_leaves_no_tmp_file_after_success` (unit) — `put()` once, then `tokio::fs::read_dir` the target's parent directory and assert no entry matches `*.tmp.*`.
  - `local_fs_put_cleans_up_tmp_file_on_write_failure` (unit) — force a write failure (e.g. resolve a path whose parent directory is deliberately made read-only, or point `root` at a location where `create_dir_all` succeeds but the file create fails due to a pre-existing directory at the target path) and assert no `*.tmp.*` sibling remains afterward.
  Run: `cargo test -p cf-gears-file-storage --lib local_fs_put_is_atomic_under_concurrent_writers local_fs_put_leaves_no_tmp_file_after_success local_fs_put_cleans_up_tmp_file_on_write_failure` — all pass.
  - **Out of scope (documented, not automated)**: crash-consistency (kill -9 mid-write, page-cache flush timing before an OS crash) cannot be produced deterministically in a `#[tokio::test]` without flakiness and both testing docs forbid sleep/timing-based tests — record this as a manual/chaos-testing runbook step (ties to Tier 4 item 4.11's ops runbook), not a pytest/cargo test.

- **Done-check**: `cargo test -p cf-gears-file-storage --lib -- local_fs_put` green, and `rg -n "tokio::fs::write\(&target" src/infra/backend/local_fs.rs` returns nothing.

---

### 1.2 — Sidecar has no body-size limit and does not stream; unbounded memory buffering

Two independent fixes, verifiable separately.

#### 1.2(a) — Immediate: `DefaultBodyLimit` layer

- **Goal / definition of done**: uploads larger than axum's 2 MiB `Bytes`-extractor default are no longer rejected by a bare 413 before the handler's own token/size checks ever run.

- **Pre-state check**:
  ```
  rg -n "DefaultBodyLimit" src/bin/sidecar.rs
  ```
  Expected: no matches — confirmed; the `Router` construction at `src/bin/sidecar.rs:100-116` has no `.layer(...)` call at all. Also confirm the ceiling live, with a running sidecar (see step 3 below).

- **Implementation steps**:
  1. In `src/bin/sidecar.rs`, add `use axum::extract::DefaultBodyLimit;` and a new env var `FS_SIDECAR_MAX_BODY_BYTES` parsed in `main()` next to the existing `FS_SIDECAR_*` vars (lines 66-80), default e.g. `5_368_709_120` (5 GiB — comfortably above any policy-permitted single-part upload; the real ceiling is still enforced per-request by `claims.upload.max_size`/`exact_size`, this layer only removes axum's blanket 2 MiB floor).
  2. Chain `.layer(DefaultBodyLimit::max(max_body_bytes))` onto the `Router` built at `src/bin/sidecar.rs:100-116` (after `.with_state(state)` or before — `DefaultBodyLimit` is a `tower::Layer`, order relative to `.with_state` does not matter, but add it as the outermost `.layer(...)` call for clarity).
  3. Update the sidecar's module doc comment (`src/bin/sidecar.rs:9-15`) to document the new `FS_SIDECAR_MAX_BODY_BYTES` env var.

- **Verify after this step**:
  1. `cargo build --bin sidecar -p cf-gears-file-storage` — compiles.
  2. Manual curl against a running sidecar (`FS_SIDECAR_PUBLIC_KEY=<any-valid-b64-key> cargo run --bin sidecar -p cf-gears-file-storage &`), **before** the fix:
     ```
     head -c 3145728 /dev/urandom > /tmp/big.bin   # 3 MiB, over the 2 MiB axum default
     curl -s -o /dev/null -w '%{http_code}\n' -X PUT \
       --data-binary @/tmp/big.bin \
       'http://localhost:8087/api/file-storage-data/v1/upload/00000000-0000-0000-0000-000000000000/00000000-0000-0000-0000-000000000000?fs-token=garbage'
     ```
     Expected **before** the fix: `413` (axum's body-limit extractor rejects it before the handler body — and therefore token verification — ever runs).
  3. Same curl **after** the fix (with `FS_SIDECAR_MAX_BODY_BYTES` at its default or unset): expected `403` (or `401`) — the request now reaches `extract_token`/`verifier.verify`, which rejects the garbage token; the status flipping from `413` to `403` is the observable proof the body-size floor was lifted, without needing a real signed token for this specific check.

- **New/changed tests**:
  - E2E only (this is a real HTTP-layer/axum-wiring fact that no unit test calling Rust functions directly can observe — `bin/sidecar.rs`'s actual compiled `Router`/`main()` is the seam). Add to `testing/e2e/gears/file_storage/lifecycle/test_file_storage_lifecycle.py`:
    - `test_upload_over_2mib_succeeds_once_policy_allows` (positive, justified new case since no existing lifecycle test uploads a body >2MiB) — run the same create → upload → bind → download flow as `test_localfs_single_part_full_lifecycle` but with a >2MiB `PAYLOAD`; assert the `PUT` returns `200` (not `413`) and the downloaded bytes match.
  Run: `FS_E2E_BINARY=<path> pytest testing/e2e/gears/file_storage/lifecycle/ -k test_upload_over_2mib_succeeds_once_policy_allows -v` — passes once the layer lands (currently would fail with `413`).

- **Done-check**: `rg -n "DefaultBodyLimit::max" src/bin/sidecar.rs` returns one match, and the curl in step 3 above returns `403`/`401`, not `413`, for a 3 MiB body.

#### 1.2(b) — Real streaming (memory-DoS fix)

- **Goal / definition of done**: no component (sidecar HTTP layer, `LocalFsBackend`) ever materializes a whole uploaded blob in memory; oversized uploads are aborted mid-stream (no full buffering first) with the partial temp file cleaned up.

- **Pre-state check**:
  ```
  rg -n "body: Bytes" src/bin/sidecar.rs
  rg -n "async fn put\(&self, path: &str, bytes: Bytes\)" src/infra/backend/mod.rs
  ```
  Expected: `body: Bytes` matches at the `upload` (`src/bin/sidecar.rs:140`) and `upload_multipart_part` (`src/bin/sidecar.rs:302`) handler signatures; `StorageBackend::put`'s `Bytes`-typed signature at `src/infra/backend/mod.rs:58`. Both confirm the whole-body-in-memory pattern the plan describes.

- **Implementation steps** (coordinate with 1.7.4 — the trait signature change lands once, both `LocalFsBackend` and the new `S3Backend` implement it in the same pass):
  1. Add a new `StorageBackend` trait method in `src/infra/backend/mod.rs` (alongside `put` at line 58):
     ```rust
     /// Stream a blob into `path`, hashing incrementally and enforcing
     /// `max_size` as bytes arrive (never buffering the whole body). Returns
     /// `(bytes_written, sha256_digest)`.
     async fn put_stream(
         &self,
         path: &str,
         stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
         max_size: Option<u64>,
     ) -> Result<(u64, [u8; 32]), DomainError> {
         // Default: fall back to buffering (safe for backends not yet upgraded).
         let mut buf = Vec::new();
         let mut s = stream;
         while let Some(chunk) = futures::StreamExt::next(&mut s).await {
             buf.extend_from_slice(&chunk.map_err(|e| DomainError::backend(self.id(), e.to_string()))?);
             if max_size.is_some_and(|m| buf.len() as u64 > m) {
                 return Err(DomainError::validation("size", "exceeds max_size"));
             }
         }
         let digest = crate::infra::content::hash::sha256(&buf);
         self.put(path, Bytes::from(buf)).await?;
         Ok((digest_len_u64, digest))
     }
     ```
     (adjust to the crate's actual `hash::sha256` return type — confirmed `Vec<u8>`/32 bytes in `src/infra/content/hash.rs`, used already at `src/bin/sidecar.rs:173`). The default impl keeps every backend that doesn't override `put_stream` correct (just not memory-bounded) — this is the incremental-adoption seam.
  2. Override `put_stream` in `LocalFsBackend` (`src/infra/backend/local_fs.rs`) to reuse 1.1's temp-file primitive: open the `.tmp.{uuid}` file, loop reading chunks off the stream, `write_all` each chunk, feed each chunk into a running `sha2::Sha256` hasher, check the running byte count against `max_size` after each chunk (abort and `tokio::fs::remove_file` the temp file the moment it's exceeded — never wait for the full stream), `sync_all()` + `rename()` + parent-dir fsync exactly as 1.1 does for the whole-buffer `put`.
  3. Add `futures` as a workspace-pinned dependency in `Cargo.toml` if not already present (check first: `rg "^futures" Cargo.toml ../../../Cargo.toml`) — needed for `BoxStream`/`StreamExt`.
  4. In `src/bin/sidecar.rs`, change the `upload` handler's `body: Bytes` parameter to `body: axum::body::Body`, convert it to a byte stream via `body.into_data_stream()` (axum 0.8's `Body` exposes this), map errors to `std::io::Error`, and call `state.backend.put_stream(&claims.backend_path, boxed_stream, claims.upload.max_size).await` instead of the current buffer-then-`put` at lines 158-181. The `exact_size`/`expected_hash` checks (lines 159-170) move to *after* streaming completes, comparing against the `(bytes_written, digest)` `put_stream` returned, since the incremental hash is only final once the stream is drained — do the same for `upload_multipart_part` (`src/bin/sidecar.rs:297-380`, its `body: Bytes` parameter and the exact-size check at lines 338-348).
  5. Update `InMemoryBackend` (`src/infra/backend/in_memory.rs`) with a `put_stream` override too (collect into `Bytes` is acceptable there — it's explicitly a non-durable, in-memory test/dev backend — but still implement it so `assert_backend_contract`-style shared tests, see 1.7's test strategy, can run identically across backends).

- **Verify after this step**:
  1. `cargo build -p cf-gears-file-storage --lib --bin sidecar` — compiles with the new trait method + both overrides + the sidecar handler rewrite.
  2. `rg -n "fn put_stream" src/infra/backend/*.rs` — three matches (trait default + `LocalFsBackend` + `InMemoryBackend`).
  3. `rg -n "body: Bytes" src/bin/sidecar.rs` — no matches remain on the `upload`/`upload_multipart_part` signatures.

- **New/changed tests** — extend `src/infra/backend/backend_tests.rs`:
  - `local_fs_put_stream_enforces_max_size_mid_stream` (unit) — build a stream (e.g. `futures::stream::iter` of several `Bytes` chunks) whose cumulative size exceeds a small `max_size`; assert the call returns `Err` and that no file (partial or final) exists at the target path afterward (the plan's "no destination file left behind" assertion).
  - `local_fs_put_stream_computes_hash_incrementally_matches_full_buffer_hash` (unit) — stream N chunks of known bytes through `put_stream`, separately compute `hash::sha256` over the concatenated bytes directly; assert the two digests are equal and that `bytes_written` equals the total chunk length.
  Run: `cargo test -p cf-gears-file-storage --lib -- put_stream` — both pass.
  - E2E — genuine HTTP seam (only a real compiled sidecar binary proves the handler doesn't buffer): extend `lifecycle/test_file_storage_lifecycle.py`:
    - `test_upload_exceeding_policy_max_size_rejected_mid_stream` (justified negative) — issue an upload URL whose token carries a small `max_size` (needs a control-plane API surface that lets the test request a size-constrained upload URL, or directly mint a constrained token via the lifecycle conftest's key material if `create` doesn't expose a `max_size` param yet — check `POST /files` request DTO for an existing `max_size`/policy field before adding one), PUT a body larger than that `max_size`; assert the sidecar returns an error status and that no on-disk temp file (`*.tmp.*`) remains under `fs_storage_root` afterward — this is the wiring proof; the abort-mid-stream *logic* itself is already unit-tested above.
  Run: `FS_E2E_BINARY=<path> pytest testing/e2e/gears/file_storage/lifecycle/ -k test_upload_exceeding_policy_max_size_rejected_mid_stream -v` — passes.

- **Done-check**: `cargo test -p cf-gears-file-storage --lib -- put_stream` green AND `rg -n "async fn put_stream" src/infra/backend/mod.rs` shows the new trait method.

---

### 1.5 — No timeouts/retries on sidecar→control finalize callback

- **Goal / definition of done**: the sidecar's finalize callback to the control plane always fails within a bounded time (never hangs the client's upload request indefinitely), and transient connection failures are retried a small, fixed number of times before giving up.

- **Pre-state check**:
  ```
  rg -n "reqwest::Client::new\(\)|\.timeout\(|\.connect_timeout\(" src/bin/sidecar.rs
  ```
  Expected: exactly one match, `reqwest::Client::new()` at `src/bin/sidecar.rs:97`, and no `.timeout(...)`/`.connect_timeout(...)` calls anywhere in the file — confirmed; `finalize_with_control_plane` (lines 240-283) does one bare `.send().await` with no retry.

- **Implementation steps**:
  1. In `src/bin/sidecar.rs::main`, replace `http: reqwest::Client::new()` (line 97) with a builder call reading two new env vars (default alongside the existing `FS_SIDECAR_*` block at lines 66-80): `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` (default `10`) → `.timeout(Duration::from_secs(n))`, `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` (default `5`) → `.connect_timeout(Duration::from_secs(n))`; `.build().map_err(|e| anyhow::anyhow!("reqwest client: {e}"))?`.
  2. Extract the single `.send().await` call in `finalize_with_control_plane` (`src/bin/sidecar.rs:261-269`) into a small retry loop: up to 3 attempts total, retrying only when `reqwest::Error::is_connect()` or `is_timeout()` is true (never retry on a successful-but-error HTTP status — that path already returns `Err` via `interpret_finalize_response` and represents a real 4xx/5xx from the control plane, not a transport failure); use a short fixed inter-attempt delay (e.g. `tokio::time::sleep(Duration::from_millis(100))`) between attempts — bounded enough that even 3 attempts add well under a second to the test suite's wall-clock budget.
  3. Log each retry attempt at `tracing::warn!` with the attempt number, so operators can distinguish "control plane is flaky" (visible retries, eventual success) from "control plane is down" (all attempts exhausted, 502 to client).
  4. Keep the existing "empty `control_base_url` disables the callback" short-circuit (`src/bin/sidecar.rs:248-250`) ahead of the retry loop — dev/test mode must not pay any timeout/retry cost.

- **Verify after this step**:
  1. `cargo build --bin sidecar -p cf-gears-file-storage` — compiles.
  2. `rg -n "\.timeout\(|\.connect_timeout\(" src/bin/sidecar.rs` — two matches (client builder).
  3. `rg -n "for attempt|is_connect\(\)|is_timeout\(\)" src/bin/sidecar.rs` — matches inside `finalize_with_control_plane`.

- **New/changed tests** — a sidecar-internal `#[cfg(test)] mod tests` block in `src/bin/sidecar.rs` (binary crates support in-file `#[cfg(test)]`; run via `cargo test --bin sidecar`):
  - `finalize_callback_times_out_within_configured_bound` (unit) — bind a local `TcpListener` that accepts the connection but never writes a response (no `sleep` needed — just don't respond); point `control_base_url` at it with a short configured timeout (e.g. 1s so the test itself stays fast); assert `finalize_with_control_plane` returns `Err` in well under the test's own timeout budget (assert via `tokio::time::timeout` around the call in the test, not inside production code, so the test fails fast if production hangs).
  - `finalize_callback_retries_on_connection_refused_then_succeeds` (unit) — start with `control_base_url` pointing at a closed port (connection refused), then after the first attempt's failure bind a listener that returns `200`; assert the overall call eventually succeeds and that exactly the expected number of attempts were made (count connections accepted, or instrument a shared `AtomicUsize` counter in a tiny mock handler) — this is the "mock-call verification" dimension from `12_unit_testing.md`.
  Run: `cargo test --bin sidecar -p cf-gears-file-storage -- finalize_callback` — both pass, total wall-clock well under the suite's 5s budget.

- **Done-check**: `cargo test --bin sidecar -p cf-gears-file-storage -- finalize_callback` green.

---

### 1.6 — No sidecar health/readiness endpoint

- **Goal / definition of done**: `GET /healthz` returns `200` on the actual compiled sidecar binary, so k8s liveness/readiness probes have something to hit.

- **Pre-state check**:
  ```
  rg -n "healthz|readyz" src/bin/sidecar.rs
  ```
  Expected: no matches — confirmed; the `Router` at `src/bin/sidecar.rs:100-116` registers only `upload`, `download`, and `upload_multipart_part`.

- **Implementation steps**:
  1. Add a trivial handler in `src/bin/sidecar.rs`: `async fn healthz() -> &'static str { "ok" }` (200 by default for a `&'static str` `IntoResponse`).
  2. Register it on the `Router`: `.route("/healthz", get(healthz))`, alongside the existing three routes at lines 100-116.
  3. Optional `/readyz`: `async fn readyz(State(state): State<SidecarState>) -> Response` that does a cheap `tokio::fs::metadata(&state.backend_root).await` check (only meaningful for `local-fs`; once 1.7.2's multi-backend dispatch lands, `/readyz` can instead check `state.backends.list()` is non-empty) and returns `200`/`503` accordingly. If `SidecarState` does not currently expose the raw root path (it only holds `Arc<dyn StorageBackend>`), add a lightweight `fn is_ready(&self) -> bool` to `StorageBackend` or skip `/readyz` for this PR and track it as a fast-follow — `/healthz` alone unblocks liveness probes, which is the item's core ask.
  4. To make the router unit-testable in-process (see tests below), factor the `Router::new()...with_state(state)` construction in `main()` into a `fn build_router(state: SidecarState) -> Router` helper the `#[cfg(test)]` module can call directly without binding a real socket.

- **Verify after this step**:
  1. `cargo build --bin sidecar -p cf-gears-file-storage` — compiles.
  2. `rg -n "\"/healthz\"" src/bin/sidecar.rs` — one match.

- **New/changed tests**:
  - `sidecar_healthz_returns_200` (unit, `#[cfg(test)]` in `src/bin/sidecar.rs`) — build a `SidecarState` with a throwaway `InMemoryBackend`/verifier, call `build_router(state).oneshot(Request::get("/healthz").body(Body::empty()).unwrap())`, assert `response.status() == StatusCode::OK`. This is the `Router::oneshot` route-smoke pattern `13_e2e_testing.md` explicitly endorses for unit tests.
  Run: `cargo test --bin sidecar -p cf-gears-file-storage -- sidecar_healthz_returns_200` — passes.
  - E2E (cheap, optional per the plan) — add to `lifecycle/test_file_storage_lifecycle.py`: `test_sidecar_healthz_reachable` — `httpx.get(f"{sidecar_base_url}/healthz")`, assert `200`. **Why not redundant with the unit test**: the unit test only proves the handler function exists and is wired into a `Router` built in-test; it does not prove `bin/sidecar.rs`'s real `main()` actually calls `build_router` and binds it to the listening socket the lifecycle suite's `lifecycle_sidecar` fixture spawns.
  Run: `FS_E2E_BINARY=<path> pytest testing/e2e/gears/file_storage/lifecycle/ -k test_sidecar_healthz_reachable -v` — passes.

- **Done-check**: `cargo test --bin sidecar -p cf-gears-file-storage -- sidecar_healthz_returns_200` green.

---

### 1.3 — Multi-replica deployments break signed URLs (ephemeral signing key)

- **Goal / definition of done**: a deployment that forgets to set `signing_key_seed` fails fast at gear `init()` in any config profile that opts into the guard, instead of silently minting an ephemeral per-replica key.

- **Pre-state check**:
  ```
  rg -n "signing_key_seed" src/gear.rs src/config.rs
  ```
  Expected: `config.rs` declares `signing_key_seed: Option<String>` (line 53) with no companion "require" flag; `gear.rs:85-98` falls back to `Issuer::generate(max_ttl)` with only an `info!` log (lines 91-96) when the seed is absent — no error path exists today. Confirm no config template already guards this: `rg -n "require_signing_key_seed" config/*.yaml src/config.rs` → no matches.

- **Implementation steps**:
  1. Add `pub require_signing_key_seed: bool` to `FileStorageConfig` in `src/config.rs` (next to `signing_key_seed` at line 53), `#[serde(default = "default_require_signing_key_seed")]`, with `fn default_require_signing_key_seed() -> bool { true }` — secure-by-default: a deployment that says nothing must not silently accept an ephemeral key. Add the field to the manual `Debug` impl (line 108-127) and to `Default` (line 130-146).
  2. In `FileStorageConfig::validate()` (`src/config.rs:94-105`), add: if `self.require_signing_key_seed && self.signing_key_seed.is_none()`, `anyhow::bail!("invalid file-storage config: signing_key_seed is required (set require_signing_key_seed: false to allow an ephemeral per-boot key in dev)")`.
  3. **Verified config-template gap** (this corrects the plan's assumption): `config/e2e-local.yaml:392` and `config/e2e-tr-authz.yaml:313` already set a fixed `signing_key_seed`, so they pass `validate()` unchanged once the new default (`true`) lands — no edit needed there. **`config/quickstart.yaml`'s `file-storage.config` block (lines 343-350) does *not* set `signing_key_seed` at all** — with the new default-`true` guard this would now fail `FileStorageGear::init()` on every `quickstart` boot. Fix by adding a fixed dev seed to `config/quickstart.yaml`, matching the pattern already used in `e2e-local.yaml`:
     ```yaml
     signing_key_seed: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
     ```
     (reusing the same well-known dev-only seed value is fine — quickstart is a single local instance, not a security boundary). This is the smaller, more consistent fix than adding `require_signing_key_seed: false` to quickstart, since it also gives quickstart the stable-URL-across-restarts benefit for free.
  4. `gear.rs:85-98` needs no further change beyond what `validate()` already gates — `cfg.validate()` runs before the `Issuer` construction (`gear.rs:64` runs before line 85), so a missing seed with the flag on now aborts `init()` before ever reaching the ephemeral-key branch. Leave the `info!` log at lines 91-96 as the dev-mode (`require_signing_key_seed: false`) explanatory message.

- **Verify after this step**:
  1. `cargo build -p cf-gears-file-storage --lib` — compiles.
  2. `cargo run -p cf-gears-example-server -- --config config/quickstart.yaml` (or however quickstart is normally launched) boots without the new `anyhow::bail!` firing — confirms step 3's fix. (Skip this manual run if not convenient; the config unit test below covers the same logic deterministically.)

- **New/changed tests** — extend `src/config_tests.rs` (mirrors `validate_rejects_zero_sweep_interval_when_sweep_enabled` at line 52):
  - `validate_rejects_missing_signing_key_seed_when_required_flag_set` — `FileStorageConfig { signing_key_seed: None, require_signing_key_seed: true, ..Default::default() }`; assert `cfg.validate().is_err()`.
  - `validate_allows_missing_signing_key_seed_when_required_flag_unset` — same but `require_signing_key_seed: false`; assert `cfg.validate().is_ok()`.
  - `validate_allows_present_signing_key_seed_when_required_flag_set` — positive control, `signing_key_seed: Some(...)`, `require_signing_key_seed: true`; assert `Ok`.
  - `default_require_signing_key_seed_is_true` — `FileStorageConfig::default().require_signing_key_seed == true` (locks in the secure-by-default choice so it can't silently regress).
  Run: `cargo test -p cf-gears-file-storage --lib -- validate_rejects_missing_signing_key_seed validate_allows_missing_signing_key_seed validate_allows_present_signing_key_seed default_require_signing_key_seed` — all pass.
  - **E2E**: none — pure config-validation logic, deterministic, no PG/AuthZ dependency. "Unit only, not a seam" per doctrine.

- **Done-check**: `cargo test -p cf-gears-file-storage --lib -- signing_key_seed` green, and `rg -n "signing_key_seed" config/quickstart.yaml` shows the newly added line.

---

### 1.4 — Quota client hardcoded `None`; background sweep disabled by default

The two halves are independently actionable; do the sweep-default half regardless of the quota SDK's availability.

- **Goal / definition of done** (sweep half only — quota half is blocked, see below): `enable_background_sweep` defaults to `true` outside explicit test/dev profiles, and every existing config template that relies on the old `false` default is updated to say so explicitly (removing ambiguity), without changing behavior for any existing test harness that constructs its own `CleanupEngine` directly (all current unit/integration tests do — see `tests/cleanup_test.rs:build_all`, which never reads `cfg.enable_background_sweep` at all).

- **Pre-state check**:
  ```
  rg -n "quota_client: None|None, // quota_client|None, // usage_reporter" src/gear.rs
  rg -n "default_enable_background_sweep" src/config.rs
  rg -n "enable_background_sweep" config/*.yaml
  rg -ln "QuotaClient" gears --glob '*.rs' | grep -v file-storage
  ```
  Expected: two `None` quota/usage-reporter placeholders in `gear.rs` (confirmed at lines 144-145 for `FileService::new` and line 155 for `MultipartService::new`, both with the `TODO(P2)` comment at lines 135-137); `default_enable_background_sweep() -> bool { false }` at `src/config.rs:187-189`; **zero** `enable_background_sweep` keys in any of `config/quickstart.yaml`, `config/e2e-local.yaml`, `config/e2e-tr-authz.yaml` (confirmed — every deployment today relies on the implicit Rust-level default); **zero** hits for `QuotaClient` anywhere else in the `gears/` workspace outside `file-storage` itself — this **confirms** the plan's blocking condition: no Quota Enforcement gear SDK crate exists yet in this repo as of this branch.

- **Design decision required (surface to the team, do not silently code around it)**: the `None, // quota_client` placeholder (`gear.rs:144`, `gear.rs:155`) is genuinely blocked — no `QuotaClient`-shaped SDK crate exists in the workspace (`gears/system/quota-enforcement/` is docs-only on this branch). Do not fabricate a placeholder client; flag it explicitly (tracked ticket referencing `cpt-cf-file-storage-fr-storage-quota`).
  However, the **`None, // usage_reporter` placeholder (`gear.rs:145`) is NOT blocked**: a `usage-collector-sdk` crate now exists on this branch (`gears/system/usage-collector/usage-collector-sdk`, trait `UsageCollectorClientV1`). Wiring the `usage_reporter` is actionable now and should be split into its own step (paired with Tier 1 item 1.12, which fixes what the reporter reports). Confirm the crate before starting: `rg -n "UsageCollectorClientV1" gears/system/usage-collector/`.

- **Implementation steps (sweep-default half — actionable now)**:
  1. In `src/config.rs`, change `fn default_enable_background_sweep() -> bool { false }` (line 187-189) to `true`, and update its doc comment (`// must be false so tests are deterministic` at line 80 and the field-level comment) — it no longer reflects the new default; rewrite to explain that the Rust-level default is now "on" for any deployment that doesn't say otherwise, and that test/dev harnesses building a config directly (not via YAML) must now explicitly set `enable_background_sweep: false` for determinism.
  2. Add `enable_background_sweep: false` explicitly to `config/e2e-local.yaml`'s `file-storage.config` block (line ~378-392) and to `config/e2e-tr-authz.yaml`'s (line ~301-313) — both are shared-server e2e configs where a background sweep firing unpredictably during a test run (even with a long default `sweep_interval_secs`) is undesirable per `13_e2e_testing.md`'s "no non-deterministic background activity" spirit; being explicit removes any ambiguity about which value they rely on, per the plan's own instruction.
  3. Add `enable_background_sweep: true` explicitly to `config/quickstart.yaml`'s `file-storage.config` block (line ~343-350) — quickstart is the closest thing to a "real-ish" deployment profile in this repo, so it should exercise the intended production default rather than silently opting out.
  4. Audit `src/gear_tests.rs`/`tests/*.rs` for any place that boots a full `FileStorageGear::init()` off a bare `FileStorageConfig::default()` (as opposed to constructing `CleanupEngine` directly, which every current test does per `tests/cleanup_test.rs:build_all`) — confirmed none do (`init()` needs a live `GearCtx`, only reachable from the e2e binary), so no Rust-level test changes are required beyond the new `config_tests.rs` case below.

- **Verify after this step**:
  1. `cargo build -p cf-gears-file-storage --lib` — compiles.
  2. `rg -n "fn default_enable_background_sweep" -A1 src/config.rs` shows `true`.
  3. `rg -n "enable_background_sweep" config/quickstart.yaml config/e2e-local.yaml config/e2e-tr-authz.yaml` shows all three now set it explicitly (`true`/`false`/`false`).

- **New/changed tests**:
  - `default_enable_background_sweep_is_true` (unit, extend `src/config_tests.rs`) — `FileStorageConfig::default().enable_background_sweep == true`.
  Run: `cargo test -p cf-gears-file-storage --lib -- default_enable_background_sweep_is_true` — passes.
  - Quota half: **blocked** — once a real `QuotaClient` SDK crate exists, extend `tests/enforce_test.rs` (already has `quota_client_error_fails_closed`, `quota_exceeded_rejects_create_when_client_present` per lines 372-457) with a mock-SDK-backed test asserting `create_file` rejection on `Denied`, with mock-call-argument verification. Track as a **cross-gear** e2e test under `testing/e2e/cross_gear/test_file_storage_quota_integration.py` once the SDK ships (not file-storage's own single-gear suite — the seam being proven is gear-to-gear wiring per `13_e2e_testing.md`'s cross-gear layout). Until then: nothing to run.

- **Done-check**: `cargo test -p cf-gears-file-storage --lib -- default_enable_background_sweep_is_true` green, and the quota half remains tracked as an open cross-team dependency (not silently marked done).

---

### 1.7 — No durable/distributed storage backend (S3) despite doc claims

This is the largest item — four sub-items plus a dedicated test strategy. Re-verified: `src/infra/backend/mod.rs:11` does say "S3/GCS/etc. are deferred (they require an external SDK + security review)"; `src/bin/sidecar.rs:95` does hardcode a single `Arc::new(LocalFsBackend::new("local-fs", root))` into `SidecarState.backend` with **no** `BackendRegistry` on the sidecar at all; `Claims` (`src/infra/signed_url/mod.rs:85-92`) does carry `pub backend_id: String` (confirmed line 91) which the sidecar never reads today (`rg -n "claims.backend_id" src/bin/sidecar.rs` → no matches, confirmed).

#### 1.7.1 — `S3Backend: StorageBackend` implementation

- **Goal / definition of done**: a new `src/infra/backend/s3.rs` implements every `StorageBackend` trait method (per the interface in `src/infra/backend/mod.rs:49-141`) against a real S3-compatible HTTP API, registered in `mod.rs` alongside `in_memory`/`local_fs`.

- **Pre-state check**:
  ```
  rg -n "mod s3" src/infra/backend/mod.rs
  rg -n "aws-sdk-s3|aws-config" Cargo.toml
  ```
  Expected: no matches for either — confirmed; only `mod in_memory; mod local_fs;` exist at `src/infra/backend/mod.rs:13-14`, and `Cargo.toml` (root of this crate) has no AWS SDK dependency.

- **Implementation steps**:
  1. **Security-review gate first**: per `mod.rs:11`'s comment, get the chosen S3 SDK crate (team choice — e.g. `aws-sdk-s3`, or a lighter `rusty-s3`/`s3` crate if the team prefers avoiding the full AWS SDK's dependency weight) through whatever external-SDK security review process this codebase already applies to other gears' external SDKs (check `docs/` for a review checklist/precedent before writing code — do not skip this step even for a "just testing" spike).
  2. Add the chosen crate + its async runtime feature (e.g. `aws-sdk-s3 = { version = "...", features = [...] }`, `aws-config`) to `Cargo.toml`'s `[dependencies]`.
  3. Create `src/infra/backend/s3.rs`: `pub struct S3Backend { id: String, client: aws_sdk_s3::Client, bucket: String }` with a constructor taking `(id, endpoint: Option<String>, region, bucket, credentials)` per 1.7.3's config shape.
  4. Implement each trait method mapping to the S3 API exactly as scoped in the plan (re-verified against the current trait in `mod.rs:49-141`):
     - `put` → `PutObject` for bodies under a size threshold (delegates to multipart above it — see 1.7.4).
     - `get` → `GetObject`, no `Range` header.
     - `get_range` → **override** the trait's default (`mod.rs:65-76`, which reads the whole blob via `self.get()` then slices — confirmed this default exists and must not be inherited by `S3Backend`) with a real `GetObject` + `Range:` header, so range reads never pull the whole object.
     - `delete` → `DeleteObject`; mirror `local_fs.rs:113-121`'s not-found-is-success idempotent-delete contract, but let a genuine access/bucket error propagate as `Err` (do not swallow non-404 S3 errors).
     - `exists` → `HeadObject`; 404 → `Ok(false)`, anything else → `Err` (mirror `local_fs.rs:123-132`'s split).
     - `list_paths` → `ListObjectsV2`, loop on `continuation_token` until `is_truncated == false`; convert each returned key back to the `"/{file_id}/{version_id}"` convention (mirror how `local_fs.rs:162-167` strips its root prefix — for S3 the "prefix" is simply the bucket key itself if keys are stored without a leading slash, so this is largely a `format!("/{key}")`).
     - `initiate_multipart` → `CreateMultipartUpload`; return its `UploadId` as the opaque `String` handle.
     - `upload_part` → `UploadPart`; return `(backend_etag, part_hash_bytes)` where `backend_etag` is S3's returned `ETag` header with surrounding quotes stripped, and `part_hash_bytes` is computed locally via `hash::sha256` (same primitive already used at `src/bin/sidecar.rs:351`) — **not** S3's ETag, which is an MD5, not this gear's SHA-256 convention.
     - `complete_multipart` → `CompleteMultipartUpload` with the `[(part_number, etag)]` list in ascending order; per the trait doc comment (`mod.rs:107-114`, confirmed it mandates the SHA-256 of the *fully assembled* object) — S3's own returned multipart `ETag` (`md5-of-part-md5s#N` format) is not usable directly, so follow up with one streamed `GetObject` re-read of the completed object to compute the real SHA-256.
     - `abort_multipart` → `AbortMultipartUpload`.
  5. `capabilities()` returns `BackendCapabilities { multipart_native: true, range_native: true, ..Default::default() }` (fields per `mod.rs:33-44`, confirmed `Default` derive exists so `..Default::default()` is valid) — this is the concrete unblock for Tier-0 item 0.2: once an `S3Backend` instance sets `multipart_native: true`, `initiate_multipart_upload` (`src/domain/multipart_service.rs:253-256`) stops 422ing for uploads targeting it.
  6. Register the module: add `mod s3;` and `pub use s3::S3Backend;` in `src/infra/backend/mod.rs` next to lines 13-14/25-26.
  7. Follow the existing `Debug`-redaction template from `src/config.rs:108-127` (which redacts `signing_key_seed`) for any S3 credential fields `S3Backend`/its config struct holds — never derive a naive `Debug` that could print a secret access key.

- **Verify after this step**:
  1. `cargo build -p cf-gears-file-storage --lib` — compiles (will not yet be exercised until 1.7.3 wires it into `gear.rs`).
  2. `rg -n "multipart_native: true" src/infra/backend/s3.rs` — one match.
  3. `rg -n "async fn get_range" src/infra/backend/s3.rs` — confirms the trait default is overridden, not inherited.

- **New/changed tests**: see the dedicated "S3 test strategy with `s3s-fs`" block below — covers 1.7.1 in full.

- **Done-check**: `cargo test -p cf-gears-file-storage --lib -- s3_backend` green once the `s3s-fs`-backed tests below exist.

#### 1.7.2 — Sidecar backend-id dispatch

- **Goal / definition of done**: the sidecar resolves the backend to use **per request** from `claims.backend_id` (verified from the signed token) instead of a single hardcoded `LocalFsBackend` — otherwise an `S3Backend` registered only in the control plane's registry is unreachable no matter how correct 1.7.1 is.

- **Pre-state check**:
  ```
  rg -n "backend: Arc<dyn StorageBackend>" src/bin/sidecar.rs
  rg -n "claims.backend_id" src/bin/sidecar.rs
  rg -n "state.backend\." src/bin/sidecar.rs
  ```
  Expected: `SidecarState.backend: Arc<dyn StorageBackend>` at line 51 (single field, not a registry); zero matches for `claims.backend_id`; three call sites of `state.backend.` — `.put(...)` at line 175 (`upload`), `.put(...)` at line 364 (`upload_multipart_part`'s part write), and `.get_range(...)`/`.get(...)` at lines 411/420 (`download`). All confirmed.

- **Implementation steps**:
  1. In `src/bin/sidecar.rs`, replace `SidecarState.backend: Arc<dyn StorageBackend>` with `SidecarState.backends: file_storage::infra::backend::BackendRegistry` (the same type the control plane's `gear.rs:78` already uses — confirms this is the reused type, not a new sidecar-only abstraction).
  2. In `main()`, replace the single `backend: Arc::new(LocalFsBackend::new("local-fs", root))` construction (line 95) with building a `BackendRegistry` from whatever backend ids/configs the sidecar is given: at minimum `local-fs` (existing `FS_SIDECAR_BACKEND_ROOT` env var, unchanged), plus any configured S3 backend(s) per 1.7.3's sidecar-side config plumbing.
  3. In each of the three handlers (`upload`, `upload_multipart_part`, `download`), move the `state.backend.<method>(...)` call to *after* `claims` has been verified (it already is, in all three — token verification happens before any backend call today), and resolve via `let backend = state.backends.get(&claims.backend_id).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())?;` then call `backend.<method>(...)` instead of `state.backend.<method>(...)`. A `claims.backend_id` naming an id absent from the sidecar's registry (misconfiguration between control plane and sidecar) must surface as a clear 500 with the backend id in the error, not a panic.

- **Verify after this step**:
  1. `cargo build --bin sidecar -p cf-gears-file-storage` — compiles.
  2. `rg -n "state.backend\." src/bin/sidecar.rs` — no matches remain (all replaced by `backend.` from the resolved registry lookup).
  3. `rg -n "claims.backend_id" src/bin/sidecar.rs` — at least 3 matches (one per handler).

- **New/changed tests**:
  - `sidecar_resolves_backend_by_claims_backend_id` (unit, in `src/bin/sidecar.rs`'s `#[cfg(test)]` module per 1.6's `build_router` refactor) — build a `SidecarState` with a two-entry `BackendRegistry` (`"local-fs"` + a second differently-tagged `InMemoryBackend` id, e.g. `"other"`), mint two tokens whose `backend_id` differ, `PUT` through the router for each, and assert (via each backend's own `list_paths()`/`get()`) that the bytes landed in the backend the *token* named, not always the same one — this is the concrete regression test for the dispatch bug, using only in-memory backends so it needs no S3 double.
  Run: `cargo test --bin sidecar -p cf-gears-file-storage -- sidecar_resolves_backend_by_claims_backend_id` — passes.
  - E2E: the "S3 test strategy" block's optional e2e run (below) is what actually proves this dispatch against a real S3-shaped backend over real HTTP.

- **Done-check**: `cargo test --bin sidecar -p cf-gears-file-storage -- sidecar_resolves_backend_by_claims_backend_id` green.

#### 1.7.3 — Config wiring

- **Goal / definition of done**: `FileStorageConfig` supports zero-or-more named S3 backend entries; `gear.rs::init` builds one `S3Backend` per entry and adds it to the registry; the sidecar gets the matching subset of config via its existing env-var pattern.

- **Pre-state check**:
  ```
  rg -n "s3_backends|S3BackendConfig" src/config.rs src/gear.rs
  ```
  Expected: no matches — confirmed; `FileStorageConfig` (`src/config.rs:17-86`) has no backend-table field, and `gear.rs:75-79` hardcodes exactly `local` + `memory`.

- **Implementation steps**:
  1. Add to `src/config.rs`: `#[serde(default)] pub s3_backends: Vec<S3BackendConfig>` and a new struct `pub struct S3BackendConfig { pub id: String, pub endpoint: Option<String>, pub region: String, pub bucket: String, pub access_key_id: Option<String>, pub secret_access_key: Option<String> }` (or a `credentials source` enum if the team prefers env/IMDS/profile chaining over static keys in config — this is a per-team call flagged in 1.7.1's security-review step). Add the new field to the manual `Debug` impl (`src/config.rs:108-127`) with `secret_access_key` redacted the same way `signing_key_seed` is.
  2. In `src/gear.rs::init` (currently building only `local`/`memory` at lines 75-78), after constructing `local`/`memory`, loop `cfg.s3_backends` and build one `Arc<dyn StorageBackend> = Arc::new(S3Backend::new(&entry.id, ...))` per entry, then pass `vec![local, memory, ...s3_instances]` into `BackendRegistry::new(...)` (replacing the current 2-element `vec![local, memory]` at line 78) — `BackendRegistry::new` already supports an arbitrary number of backends and rejects duplicate ids (`mod.rs:161-167`, confirmed), so no registry-side change is needed.
  3. Extend the sidecar's env-var configuration (`src/bin/sidecar.rs:9-15`, `FS_SIDECAR_*` pattern) with the equivalent subset needed for 1.7.2's registry: e.g. `FS_SIDECAR_S3_BACKENDS` as a small JSON array env var (`[{"id":"...","endpoint":"...","region":"...","bucket":"..."}]`, credentials still resolved via the SDK's normal env/IMDS chain rather than embedded in this JSON) parsed in `main()` alongside `FS_SIDECAR_BACKEND_ROOT`.

- **Verify after this step**:
  1. `cargo build -p cf-gears-file-storage --lib --bin sidecar` — compiles.
  2. `rg -n "s3_backends" src/config.rs src/gear.rs src/bin/sidecar.rs` — three files show the new field/loop/env var.

- **New/changed tests**:
  - `config_s3_backends_serde_round_trip` (unit, extend `src/config_tests.rs`) — serialize/deserialize a `FileStorageConfig` with a non-empty `s3_backends` list; assert round-trip equality and that `secret_access_key` is absent from the `Debug` output (`format!("{cfg:?}")` must not contain the literal secret).
  - `gear_registry_includes_configured_s3_backends` (extend `src/gear_tests.rs` if it gains a registry-construction-only test that doesn't need a live `GearCtx`, or a new integration test that stubs enough of `GearCtx` — check first whether such a stub already exists elsewhere in the workspace before inventing one) — construct the registry-building logic with one `s3_backends` entry and assert `BackendRegistry::list()` includes it.
  Run: `cargo test -p cf-gears-file-storage --lib -- config_s3_backends_serde_round_trip gear_registry_includes_configured_s3_backends` — pass.

- **Done-check**: `cargo test -p cf-gears-file-storage --lib -- s3_backends` green.

#### 1.7.4 — Streaming

- **Goal / definition of done**: `S3Backend` never buffers a whole blob; above a configurable threshold it drives its own multipart flow fed by a stream, and reads return a streamed body rather than a fully materialized `Bytes`.

- **Pre-state check**: this depends on 1.2(b)'s trait change landing first — `rg -n "async fn put_stream" src/infra/backend/mod.rs` should already show the new trait method (added in 1.2) before starting this sub-item.

- **Implementation steps**:
  1. Implement `S3Backend::put_stream` (the trait method added in 1.2(b)) to check the stream's declared/observed size against a configurable threshold (e.g. `s3_multipart_threshold_bytes`, default 8 MiB — S3's own minimum part size); below it, buffer just that bounded chunk and issue one `PutObject`; above it, drive `initiate_multipart` → repeated `upload_part` (chunking the incoming stream into part-sized pieces, never holding more than one part in memory at a time) → `complete_multipart`, reusing the exact same methods from 1.7.1.
  2. Change `S3Backend::get`/`get_range` to return a streamed body type (e.g. wrap the SDK's `ByteStream` in whatever this crate's `get`/`get_range` signature evolves to under 1.2(b)'s streaming pass — coordinate so this doesn't require a second trait-signature change after 1.7.1 already shipped a `Bytes`-returning version).
  3. Note in the module doc comment of `s3.rs` that this streaming path is validated against `s3s-fs` in CI (see below) but genuinely streamed multipart against a **real** object store still needs the Tier 4 item 4.10 load/perf validation pass, due to the open upstream `s3s-project/s3s#395` limitation described next.

- **Verify after this step**: same as 1.7.1's `s3s-fs`-backed tests below, specifically the multipart-path assertions.

- **New/changed tests**: covered by the shared `s3s-fs` test strategy below (`multipart` scenarios).

- **Done-check**: `cargo test -p cf-gears-file-storage --lib -- s3_backend` green with the multipart scenarios included.

#### S3 test strategy with `s3s-fs` (applies to 1.7.1–1.7.4)

- **Test double decision (final, do not re-litigate)**: add `s3s-fs = "0.14.1"` as a `[dev-dependencies]` entry in this crate's `Cargo.toml` (confirmed current `[dev-dependencies]` block only has `serde_json`/`uuid`, lines 72-74 — this is a pure addition). `s3s-fs` is the **official upstream** crate from `s3s-project/s3s` on crates.io — a full, in-process, filesystem-backed S3-compatible HTTP **server** (not a `StorageBackend` implementation). Tests spin it up on a local port and point the same S3 client SDK `S3Backend` uses at that local endpoint.
- **Fork-not-needed rationale**: a personal fork (`ffedoroff/s3s`) carrying upstream PRs #585/#586 (CopyObject content-preservation + `MetadataDirective::Replace` fixes) is believed unnecessary because both merged upstream and ship in the official `v0.14.1` release. ⚠️ **VERIFY BEFORE DEPENDING**: the specific version (`0.14.1`), its release/tag date, the merge status of PRs #585/#586, and whether issue #395 is still open are all external facts that cannot be confirmed from this repo — re-check crates.io and `github.com/Nugine/s3s` at implementation time and pin whatever official version actually carries those fixes. Do not treat the version string below as authoritative.
- **Pre-state check**:
  ```
  rg -n "s3s-fs" Cargo.toml
  ```
  Expected: no matches before this step.
- **Implementation steps**:
  1. Add `s3s-fs = "0.14.1"` to `[dev-dependencies]` in `gears/file-storage/file-storage/Cargo.toml` (next to the existing `serde_json`/`uuid` dev-deps at lines 72-74). Run `cargo build -p cf-gears-file-storage --tests` once to confirm it resolves in the workspace lockfile without conflicting with whatever S3 SDK crate 1.7.1 chose.
  2. Create `src/infra/backend/s3_tests.rs` (mirrors `backend_tests.rs`'s `#[path = ...]` wiring pattern already used at `mod.rs:211-213` for `backend_tests`), registered via a new `#[cfg(test)] #[path = "s3_tests.rs"] mod s3_tests;` in `mod.rs`.
  3. In `s3_tests.rs`, add a test helper `async fn start_s3s_fs() -> (SocketAddr, tempfile::TempDir)` that constructs an `s3s_fs::FileSystem` rooted at a fresh temp dir, wraps it in `s3s::service::S3ServiceBuilder`, binds a local `TcpListener` on an ephemeral port (`0`), and spawns the resulting hyper/axum server task — returning the bound address so tests can point `S3Backend::new(..., endpoint: Some(format!("http://{addr}")), ...)` at it. Use a fresh bucket-plus-temp-dir per test (unique per `Uuid::now_v7()`, matching this file's existing `unique_root()` pattern in `backend_tests.rs:8-13`) so tests stay independent per `12_unit_testing.md`.
  4. If a shared `assert_backend_contract(backend: &dyn StorageBackend)` helper doesn't already exist, factor one out of `backend_tests.rs`'s existing per-method assertions (put/get/get_range/delete/exists/list_paths) so it can run against both `LocalFsBackend`/`InMemoryBackend` (already covered) and now `S3Backend` without duplicating assertions — this directly satisfies "generalize the existing per-backend contract tests" from the plan.
  5. Add scenario tests in `s3_tests.rs`:
     - `s3_backend_put_get_round_trip` — `assert_backend_contract`-style put/get, plus a raw read of `s3s-fs`'s backing temp directory to confirm the bytes are physically on disk under the expected key (the "secondary/state artifact" assertion dimension `12_unit_testing.md` requires).
     - `s3_backend_get_range_returns_native_partial_content` — confirms the trait-default override from 1.7.1 step 4 actually issues a ranged `GetObject` (assert correct byte slice; if the SDK/mocking allows, assert the request carried a `Range:` header rather than fetching the whole object).
     - `s3_backend_delete_is_idempotent` — delete twice, second call still `Ok`.
     - `s3_backend_exists_distinguishes_missing_from_error` — `exists` on an absent key → `Ok(false)`.
     - `s3_backend_list_paths_paginates_across_continuation_token` — seed more objects than one `ListObjectsV2` page (or configure a tiny page size if the SDK allows) and assert all are returned.
     - `s3_backend_multipart_initiate_upload_complete_round_trip` — drives `initiate_multipart` → `upload_part` (≥2 parts) → `complete_multipart`; asserts the returned digest equals a directly-computed SHA-256 of the concatenated part bytes, and that `s3s-fs`'s backing file matches byte-for-byte.
     - `s3_backend_multipart_abort_discards_parts` — `initiate_multipart` → `upload_part` → `abort_multipart`; assert a subsequent `get` on the target path is `Err` (object never completed).
  Run: `cargo test -p cf-gears-file-storage --lib -- s3_backend` — all pass, no network dependency, no external CI infra (matches the SQLite `:memory:` self-contained style).
- **Test tier classification**: all of the above are **unit/integration tier**, not e2e — `s3s-fs` is spun up and torn down inside the test process, with no `cf-gears-server`, real PostgreSQL, or real AuthZ involved (same category as SQLite `:memory:` on the DB side, per `12_unit_testing.md`'s philosophy section).
- **Known limitation — flag, don't paper over**: upstream issue `s3s-project/s3s#395` ("Streaming Multipart FileStream") is still **open** as of this writing — `s3s-fs` may not fully support genuinely streamed multipart uploads end-to-end under real network partial-write/backpressure conditions. The tests above validate API shape, single/multi-part correctness, range reads, and copy/metadata semantics in CI with zero external infra, but do **not** by themselves prove `S3Backend`'s streaming multipart path (1.7.4) behaves identically against a real object store. Track real-object-store validation under Tier 4 item 4.10 (load/perf validation) — do not treat `s3s-fs`-only coverage as full production sign-off for the S3 backend.
- **E2E option (optional, not required for 1.7 to land)**: gate a new optional suite on `FS_E2E_S3_ENDPOINT` (skip gracefully via `pytest.skip(...)` when unset, exactly like `13_e2e_testing.md`'s "Optional Test Suites" pattern for `E2E_AUTH_TOKEN_TENANT_B`): run the existing `lifecycle/test_file_storage_lifecycle.py` suite with the sidecar's `FS_SIDECAR_S3_BACKENDS` (from 1.7.3) pointed at a locally-started `s3s-fs` instance instead of `local-fs`, covering one real put→get→multipart-complete round trip through real signed URLs and 1.7.2's `claims.backend_id` dispatch — something the in-process `s3s-fs` unit tests above cannot prove, since they call `S3Backend` directly and never exercise the sidecar's HTTP layer or token-driven dispatch.
  Run (when infra is provided): `FS_E2E_BINARY=<path> FS_E2E_S3_ENDPOINT=http://127.0.0.1:<port> pytest testing/e2e/gears/file_storage/lifecycle/ -v`.

- **Done-check (1.7 overall)**: `cargo test -p cf-gears-file-storage --lib -- s3_backend` green, `rg -n "s3s-fs" Cargo.toml` shows the dev-dependency, and `rg -n "claims.backend_id" src/bin/sidecar.rs` shows the sidecar dispatch from 1.7.2 wired in.

---

### 1.8 — Zero metrics/observability

- **Goal / definition of done**: request latency/status, ingress/egress bytes, sweep results, backend error counts, and quota-denial counts are exported through whichever metrics facade the platform already standardizes on; a request id propagates from control plane to sidecar for cross-plane log correlation.

- **Pre-state check**:
  ```
  rg -n 'metrics::|Counter|counter!|gauge!|histogram!|#\[instrument\]' src/
  rg -n 'metrics|opentelemetry' Cargo.toml
  ```
  Expected: no matches for either — confirmed; `Cargo.toml` has no metrics/OTel crate beyond what `toolkit`/`tracing` already pull transitively, and `SweepResult` (`src/domain/cleanup.rs:42-51`) is only logged via `tracing::info!(?result, ...)` at `gear.rs:188`, never exported as a metric.

- **Implementation steps**:
  1. **Follow the existing platform precedent — do NOT treat this as a greenfield facade choice.** The platform standard is the **OpenTelemetry `Meter` method API**, not the `metrics`-crate macros — so a grep for `counter!`/`histogram!` returns nothing and would falsely read as "no precedent". The concrete model to mirror is **mini-chat**: `gears/mini-chat/mini-chat/src/infra/metrics.rs` defines a `MiniChatMetricsMeter` wrapping `opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, UpDownCounter}`, obtained via `opentelemetry::global::meter_with_scope` (`gears/mini-chat/mini-chat/src/gear.rs:336-337`) and exported through `opentelemetry-prometheus` (`opentelemetry` is already a workspace dependency). Create the analogous `src/infra/metrics.rs` with a `FileStorageMetricsMeter`, injected as a port the way mini-chat does. Confirm before starting: `rg -n "meter_with_scope|opentelemetry" gears/mini-chat/mini-chat/src/`.
  2. Add `#[tracing::instrument(skip(...))]` to the main service entry points: `FileService::create_file`, `bind`, `finalize_upload`/`finalize_upload_by_token`, `download_url`, `delete_file`/`delete_version`, `MultipartService::initiate_multipart_upload`/`complete_multipart_upload`, `CleanupEngine::run_sweep`.
  3. Instrument route-level latency + status for both the control plane's routes (`src/api/rest/handlers.rs`) and the sidecar's three (soon four, per 1.6) routes, using whatever tower/axum metrics middleware the chosen facade provides (often a `tower::Layer` similar in shape to `DefaultBodyLimit` from 1.2(a) — can be added at the same call site).
  4. Export `SweepResult`'s three counters (`abandoned_pending_deleted`, `expired_multipart_aborted`, `retention_expired_deleted` — confirmed field names at `src/domain/cleanup.rs:44-50`) as metrics counters at the same point `gear.rs:188` already logs them, rather than only logging.
  5. Add a request-id header (e.g. `x-request-id`) propagated from the control plane's signed-URL issuance through to the sidecar's finalize callback (`src/bin/sidecar.rs::finalize_with_control_plane`, lines 240-283) so logs on both sides of the plane split can be correlated by the same id.

- **Verify after this step**:
  1. `cargo build -p cf-gears-file-storage --lib --bin sidecar` — compiles.
  2. `rg -n '#\[tracing::instrument' src/domain/service/*.rs src/domain/multipart_service.rs src/domain/cleanup.rs` — matches on the entry points from step 2.
  3. `rg -n "x-request-id" src/bin/sidecar.rs` — at least one match.

- **New/changed tests**: none strictly required beyond compilation, per the plan's own scoping. If the chosen facade exposes an in-memory/testable registry, add one smoke test, e.g. `metrics_counter_increments_on_file_create` (unit) asserting a counter's value increases by exactly 1 after a `create_file` call. Otherwise: note in the PR description "verified via code review that `#[instrument]`/counter calls are present at the listed entry points — no facade-specific test registry available."
  Run (if a registry exists): `cargo test -p cf-gears-file-storage --lib -- metrics_counter_increments_on_file_create`.

- **Done-check**: `cargo build -p cf-gears-file-storage --lib --bin sidecar` clean, plus the three `rg` checks above all showing matches.

---

### 1.9 — Idempotency-key table never GC'd; audit/events outboxes grow unboundedly

- **Goal / definition of done**: `CleanupEngine::run_sweep()` deletes expired `idempotency_keys` rows on its existing cadence; `audit_outbox`/`events_outbox` rows remain untouched by the sweep (correctly gated on `published_at IS NOT NULL`, which stays `NULL` until the Tier 4 EventBroker relay exists) so growth stays visible rather than silently unbounded once 1.8's metrics land.

- **Pre-state check**:
  ```
  rg -n "delete_expired" src/infra/storage/repo/idempotency_repo.rs src/domain/ports.rs
  rg -n "idempotency_keys_expired_idx" src/infra/storage/migrations/m20260701_000001_p2_initial.rs
  ```
  Expected: no `delete_expired` matches (confirmed — `IdempotencyRepo::insert`, `src/infra/storage/repo/idempotency_repo.rs:68-118`, only deletes a lapsed row *for the same key* at lines 87-100, never a bulk sweep); the index does exist, confirmed at lines 114-115 (Postgres) and 228-229 (SQLite) of the P2-initial migration — built for exactly this purpose but unused.

- **Implementation steps**:
  1. Add `pub async fn delete_expired<C: DBRunner>(&self, conn: &C, now: OffsetDateTime) -> Result<u64, DomainError>` to `IdempotencyRepo` (`src/infra/storage/repo/idempotency_repo.rs`): a bulk `Entity::delete_many().filter(Column::ExpiresAt.lte(now)).secure().scope_with(&AccessScope::allow_all()).exec(conn).await` (same `secure()`/`scope_with(&AccessScope::allow_all())` pattern already used by `insert`'s lapsed-row delete at lines 87-100), returning `rows_affected`.
  2. Add `delete_expired_idempotency_keys(&self, now: OffsetDateTime) -> Result<u64, DomainError>` to the `CleanupStore` trait in `src/domain/ports.rs` (alongside the existing nine methods, e.g. after `delete_file_with_event` at line 88-94).
  3. Implement it on `Store` in `src/infra/storage/store/lifecycle.rs` (the file that already hosts `list_abandoned_pending_versions` at line 59 — confirmed this is where sweep-related `Store` inherent methods live) by delegating to `self.repos.idempotency_keys.delete_expired(&self.db, now)` (via whatever the existing `Store` DB-runner accessor pattern is — mirror the neighboring method's exact call shape).
  4. Add the trait-delegation boilerplate in `src/infra/storage/store/traits.rs` (`impl CleanupStore for Store`, confirmed starts at line 18) — one new method, thin delegation like every other one in that `impl` block.
  5. Call it from `CleanupEngine::run_sweep()` in `src/domain/cleanup.rs` as a new step 4 (after the existing three steps at lines 111-117): `result.idempotency_keys_deleted += self.store.delete_expired_idempotency_keys(now).await.unwrap_or_else(|e| { tracing::warn!(error = ?e, "cleanup: failed to delete expired idempotency keys"); 0 });` — add a matching `idempotency_keys_deleted: usize` (or `u64`) field to `SweepResult` (`src/domain/cleanup.rs:42-51`).
  6. Leave `audit_outbox`/`events_outbox` purge unimplemented for now (correctly deferred — `published_at` never gets set until the Tier 4 EventBroker relay exists, per `src/infra/storage/entity/events_outbox.rs:4-5`'s own doc comment, confirmed) but add a defense-in-depth test (below) locking in that the sweep does *not* touch these tables regardless of row age, so a future change can't accidentally start deleting unpublished events.

- **Verify after this step**:
  1. `cargo build -p cf-gears-file-storage --lib` — compiles.
  2. `rg -n "fn delete_expired" src/infra/storage/repo/idempotency_repo.rs` — one match.
  3. `rg -n "delete_expired_idempotency_keys" src/domain/ports.rs src/infra/storage/store/lifecycle.rs src/infra/storage/store/traits.rs src/domain/cleanup.rs` — four files show the new method threaded end-to-end.

- **New/changed tests** — extend `tests/cleanup_test.rs` (uses the existing `build_all`/`build_db` harness at lines 46-120):
  - `run_sweep_deletes_expired_idempotency_rows` — insert one expired and one live `idempotency_keys` row directly via `IdempotencyRepo::insert` (or a direct `ActiveModel` insert) with `expires_at` in the past/future respectively; call `engine.run_sweep()`; assert via direct `idempotency_key::Entity::find()` that the expired row is gone and the live row remains untouched (all fields unchanged) — covers both the "primary outcome" and "secondary artifact" assertion dimensions from `12_unit_testing.md`.
  - `run_sweep_does_not_touch_unpublished_outbox_rows` — seed an old `audit_outbox`/`events_outbox` row with `published_at = None` and an ancient `occurred_at`; call `run_sweep()`; assert the row is still present via direct entity `find()` — defense-in-depth lock-in per the plan's own instruction, since purge logic doesn't exist yet but a future change must not silently start deleting unpublished events.
  Run: `cargo test -p cf-gears-file-storage --test cleanup_test -- run_sweep_deletes_expired_idempotency_rows run_sweep_does_not_touch_unpublished_outbox_rows` — both pass.

- **Done-check**: `cargo test -p cf-gears-file-storage --test cleanup_test -- run_sweep_deletes_expired_idempotency_rows` green.

---

### 1.10 — Declared MIME type is never validated against actual bytes on the production upload path
- **Goal / definition of done**: a tenant policy restricting allowed MIME types
  cannot be bypassed by declaring an allowed type at presign and uploading
  arbitrary content. The stored version's `mime_type` reflects the real content,
  and the per-MIME size ceiling is enforced against the true type.
- **Pre-state check**:
  ```
  rg -n "mime::validate" src/
  ```
  Expected: exactly one non-test call site, in `src/domain/data_plane.rs:71`
  (the in-process `DataPlaneService`, **not** wired to any REST route — there is
  no `put_content` route in `routes.rs`). The real byte path,
  `src/bin/sidecar.rs::upload` (L135-193), enforces only `max_size`/`exact_size`/
  optional `expected_hash` from the token — never content type; and
  `finalize_upload_by_token` (`src/domain/service/write.rs:421-489`) re-checks
  the size ceiling but not MIME.
- **Implementation steps**:
  1. **Decide where to enforce (🛑 small design call)**: the sidecar streams the
     bytes, so content-sniffing naturally belongs there — but the sidecar today
     has no access to the declared MIME beyond what the token carries. Two
     options: (a) put the declared `mime_type` into the signed upload `Claims`
     and have the sidecar run `mime::validate` (sniff magic bytes) on the
     stream, aborting on mismatch (aligns with 1.2's streaming work); or (b)
     re-derive and validate the content type at `finalize_upload_by_token` from
     the read-back blob 0.1 already introduces (cheaper to wire, one place, but
     only catches it post-upload). Recommend (a) for defense at ingress; (b) is
     an acceptable interim that reuses 0.1's read-back.
  2. **File(s)**: per the chosen option — `src/infra/signed_url/mod.rs` (`Claims`
     gains `declared_mime`), `src/bin/sidecar.rs::upload`, and/or
     `src/domain/service/write.rs::finalize_upload_by_token`. Reuse the existing
     `mime::validate` logic (the one already in `data_plane.rs`), do not
     duplicate sniffing. On mismatch return a clean 400/validation, and persist
     the **validated** MIME, not the client's claim.
  3. **Verify**: `cargo build -p file-storage --bin sidecar 2>&1 | tail -20`;
     clippy clean.
- **New/changed tests**:
  - `finalize_rejects_content_not_matching_declared_mime` — presign as
    `image/png`, upload non-PNG bytes; assert rejection and no `available`
    version.
  - `finalize_persists_validated_mime` — positive control; stored `mime_type`
    matches the sniffed type.
  - **E2E** (justified — the trust boundary is the real sidecar stream + token):
    add `test_upload_content_mime_mismatch_is_rejected` to
    `lifecycle/test_file_storage_lifecycle.py`.
- **Done-check**: `cargo test -p file-storage finalize_rejects_content_not_matching_declared_mime` green.

---

### 1.11 — Sidecar range responses violate HTTP, and leak the internal control-plane URL / error body to the client
- **Goal / definition of done**: the sidecar's `download` emits RFC-9110-correct
  range responses (`Content-Range` on every `206`, correct status for
  not-found vs unsatisfiable-range vs I/O error, `Content-Type`/`ETag` present),
  and no sidecar error response forwards the internal control-plane URL or the
  control plane's raw error body to the uploading client.
- **Pre-state check**:
  ```
  sed -n '400,430p' src/bin/sidecar.rs
  sed -n '220,285p' src/bin/sidecar.rs
  ```
  Expected: `download` (L410-426) returns `206` with only `Accept-Ranges` — no
  `Content-Range: bytes start-end/total`; `Err(_) => RANGE_NOT_SATISFIABLE`
  maps **every** backend error (including blob-not-found and I/O) to 416; no
  `Content-Type`/`ETag`. Finalize error paths (L226-231, L276-281) return `502`
  with `format!("control-plane finalize failed ({status}): {body_text}")` and
  `format!("control-plane finalize callback unreachable: {e}")` — the latter's
  `reqwest` error embeds `FS_SIDECAR_CONTROL_URL`.
- **Implementation steps**:
  1. **File**: `src/bin/sidecar.rs`, `download`. Resolve the requested range
     against the actual blob length in the handler (the backend already knows
     the size), emit `Content-Range: bytes {start}-{end}/{total}` on the `206`,
     set `Content-Type` (from the version's stored MIME — thread it via the
     token/claims or a control-plane lookup) and an `ETag`. Map backend errors
     distinctly: blob-not-found → `404`; genuine range-unsatisfiable → `416`
     **with** `Content-Range: bytes */{total}`; I/O error → `500`. WHY: resumable
     downloaders/browsers corrupt reassembly without `Content-Range`, and 416 for
     a missing blob is wrong.
  2. **File**: `src/bin/sidecar.rs`, finalize error paths (L226-231, L276-281).
     Return a generic `502 "finalize failed"` to the client; keep the detailed
     `status`/`body_text`/`reqwest` error **only** in the server-side
     `tracing::error!` (already emitted). WHY: the client must not learn the
     internal control-plane hostname/port or the control plane's error body.
  3. **Verify**: `cargo build -p file-storage --bin sidecar 2>&1 | tail -20`.
- **New/changed tests** — in the sidecar `#[cfg(test)]` module (uses the
  `build_router` refactor from 1.6):
  - `download_range_response_includes_content_range` — request a sub-range via
    the router; assert `206` + correct `Content-Range` + body slice.
  - `download_missing_blob_returns_404_not_416`.
  - `download_unsatisfiable_range_returns_416_with_content_range`.
  - `finalize_failure_does_not_leak_control_plane_url` — force a finalize
    callback failure; assert the client-facing body contains neither the control
    URL nor the raw upstream body.
  - **E2E**: piggyback range assertions on the existing lifecycle download step.
- **Done-check**: `cargo test -p file-storage download_range_response_includes_content_range finalize_failure_does_not_leak_control_plane_url` green.

---

### 1.12 — Usage accounting is structurally asymmetric — stored bytes are never credited
- **Goal / definition of done**: when the `usage_reporter` is wired (unblocked
  now via `usage-collector-sdk` — see 1.4), stored-byte and file-count deltas
  are reported symmetrically: bytes credited on finalize (single-part **and**
  multipart), debited on version/file delete, and cleanup-driven deletions
  report their deltas — so totals can't drift to zero/negative.
- **Pre-state check**:
  ```
  rg -n "report_usage" src/
  ```
  Expected call sites only at: `create.rs:249-254` (+1 file, **0 bytes**),
  `read_ops.rs:242-247` (`delete_file`: −bytes, −1 file), `write.rs:386-397`
  (transfer: ±bytes). **No** `report_usage` in `finalize_upload`/
  `finalize_upload_by_token` (`write.rs:44-107`, `421-489`), none in
  `delete_version` for a non-last version (`read_ops.rs:261-304`), none anywhere
  in `cleanup.rs`. So bytes are never added, only ever subtracted.
- **Implementation steps**:
  1. **File**: `src/domain/service/write.rs` — credit `+size` bytes on the
     successful finalize in both `finalize_upload` and `finalize_upload_by_token`
     (using the read-back-derived `actual_size` from 0.1, so the credited amount
     matches the persisted amount).
  2. **File**: `src/domain/multipart_service.rs::complete_multipart_upload` —
     credit the completed object's total bytes there (multipart finalize does
     not go through `finalize_upload`).
  3. **File**: `src/domain/service/read_ops.rs::delete_version` — debit the
     deleted non-current version's bytes (today only whole-file delete debits).
  4. **File**: `src/domain/cleanup.rs` — report deltas from the sweep:
     retention/expired-file deletes debit bytes + file count; abandoned-pending
     deletes debit the pending version's bytes. Pair with 0.11/0.6 which already
     touch these paths.
  5. Do this in the same PR that wires `usage_reporter` off `None` (1.4), so the
     reporter is correct from its first day. **Verify**: build + clippy clean.
- **New/changed tests** — with a capturing fake `UsageReporter` (same newtype
  approach as `enforce_test.rs`'s quota fakes):
  - `finalize_reports_positive_byte_delta`; `multipart_complete_reports_byte_delta`;
    `delete_version_reports_negative_byte_delta`;
    `sweep_reports_deltas_for_deleted_files`.
  - Invariant test `usage_deltas_sum_to_zero_over_create_upload_delete` — a full
    create→finalize→delete cycle nets to zero bytes and zero files.
  - **E2E**: cross-gear, deferred to the same track as 1.4's quota e2e.
- **Done-check**: `cargo test -p file-storage usage_deltas_sum_to_zero_over_create_upload_delete` green.

---

## Tier 2 — Correctness hardening (step-by-step)

All file paths below are relative to `gears/file-storage/file-storage/`. Line
numbers were re-verified directly against the branch (`feat/file-storage-p2`)
at the time of writing; re-check with `grep -n` before editing since Tier 0/1
work landing first will shift some of them (especially `write.rs` and
`handlers.rs`, both touched by 0.1/0.4).

### 2.1 — Idempotency replay never validates the retried body matches the original

**Goal / DoD**: a `POST /files` retry with the same `idempotency_key` but a
materially different body (`owner_id`, `name`, `gts_file_type`, `mime_type`,
`custom_metadata`) is rejected with `409 Conflict` instead of silently
replaying the original ticket. An identical retry still replays unchanged.

**Pre-state check**:
```
cargo test -p file-storage --test enforce_test idempotency 2>&1 | tail -20
grep -n "request_hash" gears/file-storage/file-storage/src/infra/storage/entity/idempotency_key.rs
```
Expect: no `request_hash` symbol anywhere yet; `create_file` (`src/domain/service/create.rs:106-121`)
returns the stored ticket unconditionally once a live record is found.

**Implementation steps**:

1. **New additive migration** `src/infra/storage/migrations/m20260706_000001_idempotency_request_hash.rs`
   (next in sequence after `m20260701_000002_multipart_plan_columns`; follow
   that file's exact shape — `POSTGRES_UP`/`SQLITE_UP`/`DOWN` constants dispatched
   on `manager.get_database_backend()`):
   ```rust
   const POSTGRES_UP: &str = r"
   ALTER TABLE idempotency_keys
       ADD COLUMN IF NOT EXISTS request_hash bytea NOT NULL DEFAULT '\x';
   ";
   const SQLITE_UP: &str = r"
   ALTER TABLE idempotency_keys ADD COLUMN request_hash BLOB NOT NULL DEFAULT x'';
   ";
   ```
   Nullable-vs-default choice: use `NOT NULL DEFAULT <empty>` (additive-safe,
   matches the `m20260701_000002` precedent of `NOT NULL DEFAULT 0`) rather
   than a nullable column — a genuinely empty hash never legitimately matches a
   freshly computed 32-byte SHA-256, so old rows (there won't be any in
   practice — this table's rows expire within `idempotency_ttl_secs`, default
   86400s) simply fail closed on any future replay rather than silently
   passing. Register it in `src/infra/storage/migrations/mod.rs` (`mod
   m20260706_000001_idempotency_request_hash;` + push into the `Migrator::migrations()` vec, after
   the multipart-plan-columns entry).
   *Why*: matches the plan's "additive only" migration discipline (2.1's own
   fix-approach) and the existing two-migration precedent in this gear.

2. **Entity**: `src/infra/storage/entity/idempotency_key.rs` — add
   `pub request_hash: Vec<u8>,` to `Model` (after `response_etag`, before
   `created_at`, to match column order used elsewhere in the file).

3. **Store-layer plumbing**:
   - `src/infra/storage/store/mod.rs:87-96` (`IdempotencyInsert`) — add
     `pub request_hash: Vec<u8>,`.
   - `src/infra/storage/repo/idempotency_repo.rs::insert` (lines 68-118) — add
     a `request_hash: &[u8]` parameter, set it in the `ActiveModel` (line
     102-113 block).
   - `src/infra/storage/repo/idempotency_repo.rs::get` (lines 29-53) and
     `record_from_model` (lines 121-128) — surface `request_hash` on the
     returned record.
   - `src/domain/idempotency.rs::IdempotencyRecord` (lines 12-19) — add
     `pub request_hash: Vec<u8>,`.
   - `src/infra/storage/store/lifecycle.rs::get_idempotency_key` (line 27) and
     `src/infra/storage/store/files.rs::create_file_with_pending_version_and_event`
     (line 213) — thread the new field through unchanged (they already forward
     whole structs/records).

4. **Domain logic** — `src/domain/service/create.rs::create_file`:
   - Before building the `idempotency` insert (around line 209-230), compute
     `let request_hash = crate::infra::content::hash::sha256_parts(&[owner_kind_str.as_bytes(), owner_id.as_bytes(), new.name.as_bytes(), new.gts_file_type.as_bytes(), new.mime_type.as_bytes(), &canonicalized_metadata_bytes]);`
     — canonicalize `custom_metadata` deterministically first (sort by `key`
     before concatenating `key`+`\0`+`value`+`\0` pairs — the wire order is not
     guaranteed to be stable across two textually-identical-but-reordered
     requests, and the hash must not spuriously mismatch on reorder).
     `sha256_parts` already exists (`src/infra/content/hash.rs:33`).
     ⚠️ **Field-boundary requirement**: confirm `sha256_parts` length-prefixes
     or delimits **every** element it hashes. If it merely concatenates the
     byte slices, then `(name="ab", gts="c")` and `(name="a", gts="bc")` hash
     identically — a caller could dodge the mismatch check by shifting bytes
     across adjacent fields. If `sha256_parts` does not already delimit, either
     fix it there or interpose an unambiguous encoding (length-prefix each field
     with its u32 length, or join with a byte that cannot appear in the
     values). The `\0` delimiting above must apply to **all** fields, not only
     the metadata pairs.
   - On the **replay path** (lines 107-121, the `if let Some(record) = ...`
     block): after fetching `record`, recompute `request_hash` from the
     **current** request the same way, and compare against
     `record.request_hash`. On mismatch, `return Err(DomainError::conflict("idempotency key reused with a different request body"));`
     before deserializing/returning the stored ticket. On match, proceed as
     today.
   - Add `request_hash` to the `IdempotencyInsert` literal built at lines
     220-229.

5. Rebuild the SDK/gear if `cargo check -p file-storage` surfaces any other
   call site missing the new field (expected: none outside the files above,
   since `IdempotencyInsert`/`IdempotencyRecord` are gear-internal types).

**Verify after each step**:
- After step 1: `cargo test -p file-storage --lib migrations 2>&1 | tail -5` and
  a throwaway `sea_orm_migration` smoke: `cargo run --bin <migration-check-if-any>`
  is unnecessary — `migration_test.rs`'s `migrated_db()` helper (step 6 below)
  already runs every registered migration against SQLite `:memory:` on every
  test invocation, so a broken `up()` fails the whole suite immediately.
- After step 2-3: `cargo check -p file-storage` compiles with no dangling
  field errors.
- After step 4: `cargo test -p file-storage --test enforce_test` and
  `cargo test -p file-storage --test multipart_test idempotency` both green.

**New/changed tests** (naming per `{area}_{scenario}`, unit only per the
doctrine — this is pure request-comparison logic plus an additive column, no
PG-specific behavior):
- `tests/multipart_test.rs` (extends the existing idempotency block at
  lines 584-677):
  - `idempotency_replay_with_diverging_owner_returns_conflict` — same key,
    different `owner_id`; assert `DomainError::Conflict` and (secondary
    artifact) exactly one `files` row exists via direct `FileEntity::find()`.
  - `idempotency_replay_with_diverging_name_returns_conflict` — same shape for
    `name`.
  - `idempotency_replay_with_diverging_metadata_returns_conflict` — same
    shape for `custom_metadata`, proving the canonicalization actually
    covers metadata and not just the scalar fields.
  - Keep `idempotency_same_key_returns_same_file` unchanged as the
    identical-retry regression control.
- `tests/migration_test.rs` (new case, mirrors `file_versions_rejects_negative_size`'s
  direct-SQL DDL-probe style):
  - `idempotency_keys_request_hash_column_exists_with_default` — insert a row
    via raw SQL omitting `request_hash`, assert it defaults to an empty blob
    (not a constraint violation) via a subsequent `SELECT`.

**Done-check**: `cargo test -p file-storage` green; the new migration file is
registered in `mod.rs`; `idempotency_replay_with_diverging_owner_returns_conflict`
and its siblings fail on `main`/pre-fix and pass post-fix (sanity-check by
temporarily reverting the comparison in `create_file` and re-running).

---

### 2.2 — `GET /files/{id}/versions` is unbounded; `VersionRepo::get` does a full scan

**Goal / DoD**: `GET /files/{id}/versions` accepts `limit`/`offset` and caps at
`ServiceConfig::max_page_size`, mirroring `GET /files`. `VersionRepo::get`
resolves a single version via a direct SQL predicate instead of fetching every
version of the file and filtering in Rust — this closes the per-file
amplification-DoS surface the verifier (B5) flagged, since `get` sits on the
hot path of `finalize`/`bind`/`download_url`/`complete_multipart_upload`.

**Pre-state check**:
```
sed -n '54,67p' gears/file-storage/file-storage/src/infra/storage/repo/version_repo.rs
sed -n '186,195p' gears/file-storage/file-storage/src/api/rest/handlers.rs
```
Expect: `get()` calls `list_by_file` and does `.find()` in Rust (comment at
lines 62-64 explaining a direct predicate "proved unreliable across the
secure layer"); `list_versions` handler has no `Query<...>` extractor at all.

**Implementation steps**:

1. **Investigate the "unreliable predicate" root cause first** — before adding
   pagination, spend 30-60 minutes reproducing why
   `Entity::find().filter(Condition::all().add(Column::FileId.eq(file_id)).add(Column::VersionId.eq(version_id))).secure().scope_with(scope).one(conn)`
   was abandoned. Compare against the working two-column CAS filters that
   *do* work in the same file (`mark_available`, lines 95-109; `finalize`,
   lines 126-140) — both use `Condition::all()` with two `.add()`s on
   `update_many()`, not `find()`. The likely culprit is `SecureORM`'s
   `.secure().scope_with(scope)` composition behaving differently on `find()`
   vs `update_many()` (e.g. an implicit resource-scope column injection that
   only fires on read queries) — instrument with `sea_orm`'s SQL logging
   (`RUST_LOG=sea_orm=debug`) run against both a `.find().one()` version and
   the current `list_by_file`-then-filter version, and diff the emitted SQL.
   If it is a genuine `toolkit_db::secure` bug, file it upstream and keep the
   Rust-side filter as a documented workaround with a tracking comment instead
   of silently trying to fix it in `file-storage` — that keeps this item's
   effort at M rather than blowing into an upstream-library investigation.
2. If the direct predicate can be fixed: rewrite `VersionRepo::get`
   (`version_repo.rs:55-67`) to
   ```rust
   Entity::find()
       .filter(Condition::all().add(Column::FileId.eq(file_id)).add(Column::VersionId.eq(version_id)))
       .secure().scope_with(scope).one(conn).await.map_err(db_err)?
       .map(Into::into)
   ```
   removing the `list_by_file` delegation. If not (root cause turns out to be
   a hard blocker outside this gear's control), leave the current
   scan-and-filter implementation but add a `tracing::warn!` when a file's
   version count exceeds a threshold (e.g. 100) so the amplification pattern
   is at least observable (ties into Tier 1 item 1.8), and note the deferral
   explicitly in the PR description — do not silently ship the O(n) behavior
   unflagged.
3. **Pagination on `list_by_file`**: `version_repo.rs::list_by_file`
   (lines 70-85) — add `limit: u64, offset: u64` parameters,
   `.limit(limit).offset(offset)` on the `Entity::find()` query builder
   (SeaORM's `QuerySelect` trait, already implicitly in scope via the other
   `use sea_orm::{...}` imports — add `QuerySelect` to the `use` list at the
   top of the file).
4. **Domain layer**: `src/domain/service/read_ops.rs::list_versions`
   (lines 123-135) — add `limit: Option<u64>, offset: u64` parameters; clamp
   the same way `list_files` does (`read_ops.rs:58-60`):
   `let limit = limit.unwrap_or(self.cfg.default_page_size).min(self.cfg.max_page_size);`
   then call `self.store.list_versions(file_id, limit, offset)`. Thread the
   new params through `Store::list_versions` (wherever it forwards to
   `VersionRepo::list_by_file` — check `src/infra/storage/store/versions.rs`).
5. **REST layer**: `src/api/rest/handlers.rs` — add a `ListVersionsQuery`
   struct mirroring `ListQuery` (lines 40-47):
   ```rust
   #[derive(Debug, Deserialize)]
   pub struct ListVersionsQuery { pub limit: Option<u64>, pub offset: Option<u64> }
   ```
   Update `list_versions` (lines 186-195) to take `Query(q): Query<ListVersionsQuery>`
   and pass `q.limit, q.offset.unwrap_or(0)`.
6. **Route/OpenAPI**: `src/api/rest/routes.rs:144-158` — add
   `.query_param_typed("limit", false, "Page size", "integer")` and
   `.query_param_typed("offset", false, "Offset", "integer")` right after
   `.path_param("id", "File UUID")`, exactly matching the `GET /files`
   registration at `routes.rs:247-248`.
7. Regenerate the OpenAPI spec (this gear has an existing regen step — check
   `caf25c0e`'s commit for the invocation, likely a `cargo test`/build-script
   step or `tools/scripts/*` target) and commit the diff.

**Verify after each step**:
- Step 1-2: a throwaway test creating 3 versions for one file and 3 for
  another, calling `get()` for a version in the first file, asserting the
  correct row and no cross-file bleed; `cargo test -p file-storage --lib` green.
- Step 3-4: `cargo test -p file-storage --test service_test` green (extend
  per "New tests" below before this passes meaningfully).
- Step 5-6: `curl -s "$BASE/files/$ID/versions?limit=2&offset=0"` (or the
  route's e2e equivalent) returns exactly 2 items when the file has more.
- Step 7: `git diff` on the generated OpenAPI file shows only the two new
  query params on `file_storage.list_versions`, nothing else.

**New/changed tests**:
- Unit — `tests/service_test.rs` (or new `tests/version_pagination_test.rs`):
  `list_versions_caps_at_max_page_size` — create `max_page_size + 5` versions
  via repeated `presign_version`+finalize, call `list_versions` with no
  explicit limit, assert the returned length equals `max_page_size` (primary
  outcome) and the items are the correct (newest-first) page (secondary
  artifact: compare returned `version_id`s against a direct DB query ordered
  the same way).
- Unit — new `tests/version_repo_test.rs` (repo-level, SQLite `:memory:`):
  `version_repo_get_returns_correct_row_among_many` — seed N versions across
  two different files with the same partial UUID prefix pattern (to stress
  any accidental prefix-matching bug), assert `get(file_id, version_id)`
  returns exactly the target's `size`/`hash_value`/`status` and never a
  version belonging to the other file.
- No E2E — this is a deterministic pagination/query-correctness fix,
  reproducible entirely on SQLite; the amplification-DoS angle is explicitly
  deferred to Tier 4 item 4.10 per the plan's own coverage matrix.

**Done-check**: `cargo test -p file-storage` green; `routes.rs` diff shows the
two new query params; the OpenAPI diff is clean; the version_repo
investigation's outcome (fixed vs. documented-deferral) is recorded in the PR
description either way.

---

### 2.3 — `migrate_backend` has no CAS on the version's current backend pointer

**Goal / DoD**: two concurrent `migrate_backend` calls result in exactly one
winner; the loser gets a distinguishable "concurrent migration" error rather
than silently losing data or double-writing. **Critically, the loser's cleanup
must never delete the blob the winner just committed as the live pointer** —
see the same-target hazard in step 3.

**Pre-state check**:
```
sed -n '273,296p' gears/file-storage/file-storage/src/infra/storage/repo/version_repo.rs
sed -n '278,311p' gears/file-storage/file-storage/src/infra/storage/store/versions.rs
```
Expect: `rebind_backend`'s `Condition::all()` filters only on
`(file_id, version_id)` (no `backend_id`/`backend_path` predicate); the
store-level `rebind_version_backend` wrapper (`store/versions.rs:278-310`)
takes no "expected current backend" parameters either.

**Implementation steps**:

1. `src/infra/storage/repo/version_repo.rs::rebind_backend` (lines 273-296) —
   add `expected_backend_id: &str, expected_backend_path: &str` parameters;
   extend the `Condition::all()` filter (lines 285-289) with
   `.add(Column::BackendId.eq(expected_backend_id)).add(Column::BackendPath.eq(expected_backend_path))`.
   Keep the `Ok(res.rows_affected > 0)` return contract — `0` now means either
   "version gone" (today's meaning) **or** "backend pointer changed
   concurrently" (new meaning); the caller needs to distinguish these (step 3).
2. `src/infra/storage/store/versions.rs::rebind_version_backend`
   (lines 278-310) — thread the two new parameters through to
   `versions.rebind_backend(...)` inside the transaction closure (around
   line 294-298).
3. `src/domain/service/backend.rs::migrate_backend` (lines 35-117) — the
   pre-migration `version.backend_id`/`version.backend_path` are already
   captured at line 54 (`let version = &versions[0];`) before the read at
   line 72 — pass `&version.backend_id, &version.backend_path` into
   `rebind_version_backend` at the call site (lines 94-103). In the `!updated`
   branch (lines 104-110), re-fetch the version by id after the failed CAS and
   branch on **three** cases, not two:
   - version no longer exists → keep today's `DomainError::version_not_found`,
     and run `best_effort_blob_delete` on the destination we wrote (it is
     genuinely orphaned).
   - version exists and its `(backend_id, backend_path)` now equal **the
     destination this call just wrote** → the concurrent winner migrated to the
     *same* target and committed it as the live pointer. **Return `Ok(())`
     (treat as a successful no-op) and DO NOT delete the destination blob** —
     deleting it would destroy the winner's live content. This is the
     data-loss trap: because `dest_path` is deterministic
     (`Self::backend_path(file_id, version_id)`), two racers migrating to the
     same backend write the identical path, so the loser's "cleanup" would hit
     the winner's committed blob.
   - version exists but its `(backend_id, backend_path)` match neither the old
     source nor the destination this call wrote → a different concurrent
     migration won; return `DomainError::conflict("concurrent backend migration in progress")`
     and `best_effort_blob_delete` **only** the destination this call wrote
     (safe: it is not the live pointer).
   Guard the cleanup on "the path I am about to delete is not the current live
   `(backend_id, backend_path)`" as a belt-and-suspenders check regardless of
   branch.

**Verify after each step**:
- Step 1-2: `cargo check -p file-storage` (signature change compiles).
- Step 3: `cargo test -p file-storage --test cleanup_test migrate_backend`
  green (extend per below before this is meaningful); manually trace one
  `migrate_backend` call in a debugger/log line to confirm the two new CAS
  columns appear in the emitted SQL (`RUST_LOG=sea_orm=debug`).

**New/changed tests**:
- Unit only (deterministic CAS-predicate logic, no PG-specific behavior) —
  extend `tests/cleanup_test.rs` (already covers `migrate_backend`,
  lines 517-619):
  `concurrent_migrate_backend_second_racer_is_rejected` — call
  `rebind_backend` directly twice in sequence with the **same** pre-migration
  `backend_id`/`backend_path` as the CAS predicate for both calls (simulating
  two racers who both read the same starting state): assert the first call
  returns `rows_affected == 1` and the second returns `0`; assert (secondary
  artifact) the version's `backend_id`/`backend_path` reflect only the first
  call's target via direct DB read. Add a second case
  `migrate_backend_loser_target_blob_cleaned_up` exercising the full
  `migrate_backend` service method twice **to different targets** with a
  fake/in-memory backend and asserting the loser's target path is absent from
  the backend after the `best_effort_blob_delete` call.
  `migrate_backend_same_target_race_preserves_winner_blob` (regression for the
  step-3 data-loss trap) — run the full `migrate_backend` twice to the **same**
  target: first commits, second's CAS fails; assert the second call returns
  `Ok(())` (no-op) and, crucially, that the destination blob **still exists**
  and matches the winner's bytes via a direct backend `get()`. This test fails
  against a naive fix that unconditionally cleans up the destination.
- No E2E — deterministic CAS logic, no PG-specific SQL semantics involved.

**Done-check**: `cargo test -p file-storage` green;
`concurrent_migrate_backend_second_racer_is_rejected` fails against the
pre-fix code (temporarily drop the two new `.add()` predicates and confirm
both racer calls report `rows_affected == 1`) and passes post-fix.

---

### 2.4 — Policy upsert race: delete-then-insert with no transaction or unique constraint

**Goal / DoD**: two concurrent `PUT /policy` calls for the same
`(tenant_id, scope, scope_owner_id)` leave exactly one row, not two;
`PolicyRepo::get` is deterministic.

**Pre-state check**:
```
sed -n '62,103p' gears/file-storage/file-storage/src/infra/storage/repo/policy_repo.rs
grep -n "policies_scope_idx" gears/file-storage/file-storage/src/infra/storage/migrations/*.rs
```
Expect: `upsert` does `Entity::delete_many()` then `secure_insert` as two
independent statements (no transaction wrapper visible in this function —
confirm whether the surrounding call site wraps it, per step 2 below);
`policies_scope_idx` is a **non-unique** `CREATE INDEX`.

**Implementation steps**:

1. **New additive migration**
   `src/infra/storage/migrations/m20260706_000002_policies_unique_scope.rs`
   (numbered after 2.1's migration; same dual-backend DDL-dispatch shape).
   Postgres: use a single unique index that is valid for both tenant-scope
   (`scope_owner_id IS NULL`) and user-scope rows, since Postgres treats each
   `NULL` as distinct for uniqueness purposes — a plain
   `CREATE UNIQUE INDEX ... (tenant_id, scope, scope_owner_id)` already gives
   the right semantics for user-scope (non-null `scope_owner_id`, standard
   3-column uniqueness) *and* naturally allows multiple tenant-scope rows with
   `NULL` unless additionally constrained — but this gear's own CHECK
   constraint already guarantees at most one *tenant*-scope semantic meaning
   per tenant is intended, so add a **second**, partial unique index scoped to
   `scope = 'tenant'` to close that gap explicitly:
   ```sql
   -- Postgres
   CREATE UNIQUE INDEX IF NOT EXISTS policies_user_scope_unique_idx
       ON policies (tenant_id, scope, scope_owner_id) WHERE scope_owner_id IS NOT NULL;
   CREATE UNIQUE INDEX IF NOT EXISTS policies_tenant_scope_unique_idx
       ON policies (tenant_id, scope) WHERE scope_owner_id IS NULL;
   ```
   SQLite supports partial unique indexes with the same `WHERE` syntax
   (SQLite ≥ 3.8.0). ⚠️ **Precedent caveat**: `retention_rules_file_scope_idx`
   is partial **only on the Postgres side** (`m20260701_000001_p2_initial.rs:64-66`,
   `WHERE scope = 'file'`); its SQLite counterpart (`p2_initial.rs:180-181`) is a
   plain composite index with no `WHERE`. So there is *no* existing SQLite
   partial-index precedent in this gear to copy — the SQLite partial unique
   index here is new ground. It is still valid SQLite, but write it deliberately
   and cover it with the migration test below rather than assuming it mirrors an
   existing pattern. Drop (or leave — it's harmless and still useful
   for the `get()` query's lookup plan) the old non-unique
   `policies_scope_idx`; leaving it is simpler and lower-risk for an additive
   migration.
2. Register the migration in `migrations/mod.rs`.
3. Rewrite `PolicyRepo::upsert` (`policy_repo.rs:62-103`) as a single
   `INSERT ... ON CONFLICT (...) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at`
   using `sea_orm`'s `OnConflict` builder (`sea_orm::sea_query::OnConflict`),
   targeting the appropriate partial index depending on `scope_owner_id`
   (SeaORM's `OnConflict::columns([...]).update_columns([...])` needs the
   conflict target to match one of the two new partial indexes — if SeaORM's
   `OnConflict` API cannot express a partial-index target directly, drop to
   `conn.execute_unprepared(...)` with a hand-written
   `INSERT INTO policies (...) VALUES (...) ON CONFLICT (tenant_id, scope, scope_owner_id) WHERE scope_owner_id IS NOT NULL DO UPDATE SET ...`
   / the tenant-scope equivalent, chosen based on whether `scope_owner_id` is
   `Some`/`None` — mirroring the existing `del = match scope_owner_id { ... }`
   branching already in this function at lines 76-79). Remove the
   `delete_many()` call entirely — the `ON CONFLICT ... DO UPDATE` makes it
   redundant and closes the race window that existed *between* the delete and
   the insert.
4. Verify `secure_insert`/`SecureORM` compatibility with a raw
   `execute_unprepared` path — if `SecureORM`'s row-level scoping needs to
   wrap the insert (as `secure_insert::<Entity>` does today), check whether a
   scope-stamped column (e.g. an implicit tenant column) needs to be included
   in the `VALUES` list manually when bypassing `secure_insert`. If `SecureORM`
   turns out to make a raw `ON CONFLICT` awkward, an acceptable fallback is:
   wrap the existing delete+insert in an explicit DB transaction (`self.db.db().transaction_ref_mapped(...)`,
   the same pattern `rebind_version_backend` already uses in `store/versions.rs:290-292`)
   plus the new unique index as a backstop — the unique index alone already
   prevents the corrupted end-state (two rows), even if the transaction still
   does delete-then-insert rather than a true upsert; the concurrent loser
   would then get a constraint-violation `DomainError::Database` instead of a
   clean second attempt, which is acceptable per the plan ("Two concurrent...
   leaving two rows" is the bug being fixed, not "the loser must retry
   gracefully").

**Verify after each step**:
- Step 1-2: SQLite migration test (below) passes; run the migration against a
  local Postgres if available (`docker compose`/`quickstart` DB) and confirm
  `\d policies` shows both partial unique indexes — this is the PG-specific
  concern the plan flags as an e2e/manual check, not a required automated gate.
- Step 3-4: `cargo test -p file-storage --test enforce_test` (policy tests)
  green; a manual two-sequential-calls test (below) shows one row.

**New/changed tests**:
- Unit — `tests/migration_test.rs`:
  `policies_unique_index_rejects_duplicate_scope_tuple` — insert two rows with
  the same `(tenant_id, 'user', scope_owner_id)` directly via raw SQL; assert
  the second `INSERT` errors with a unique-constraint violation on SQLite
  (mirrors the file's existing direct-SQL DDL-probe tests, e.g.
  `file_versions_rejects_negative_size` at line 184).
- Unit — new `tests/policy_test.rs` (or extend `enforce_test.rs`):
  `policy_upsert_on_conflict_updates_existing_row_not_duplicates` — call
  `PolicyRepo::upsert` twice for the same scope with two different
  `PolicyBody` values; assert (primary outcome) both calls succeed, and
  (secondary artifact) exactly one row exists via a direct `Entity::find().count(conn)`
  and it carries the **second** call's body.
- E2E (narrow exception per the plan) — only if a genuinely
  Postgres-only partial-index quirk cannot be verified on SQLite:
  `test_policy_set_is_idempotent_under_repeated_calls` in
  `test_file_storage_seams.py` — two sequential real `PUT /policy` calls, no
  concurrency, asserting the second succeeds and a subsequent `GET /policy`
  returns the second body. Skip this unless the SQLite test above genuinely
  cannot exercise the same SQL path (it should, since both backends now use a
  `WHERE`-qualified partial unique index).
- Verifier finding B4 (file-scope retention rule not validating the target
  file) is covered by Tier 0 item 0.7's
  `create_retention_rule_file_scope_target_not_writable_is_denied` test — do
  not duplicate here; only add it to this item's test file if 0.7 lands
  without that specific check.

**Done-check**: `cargo test -p file-storage` green; two sequential `upsert`
calls for the same scope leave exactly one row (assert via direct count);
migration applies cleanly on a fresh SQLite `:memory:` DB and (spot-check) a
real Postgres instance.

---

### 2.5 — Canonical error-code contract drift (429/400 vs declared 412/422/507)

**Goal / DoD**: every `DomainError` variant's actual HTTP status is
mechanically pinned by a table-driven test, and that table is cross-checked
against what `routes.rs` declares in its OpenAPI annotations, so the two
sides of the contract cannot silently diverge again. Each of the four
sub-cases below gets an explicit, recorded decision (fix code vs. fix
route/doc) rather than being left ambiguous.

**Pre-state check** (already independently confirmed against
`libs/toolkit-canonical-errors/src/error.rs:504-521`, the ground truth for
canonical status codes):
```
grep -n "fn status_code" -A 16 libs/toolkit-canonical-errors/src/error.rs
```
Confirms: `FailedPrecondition` and `OutOfRange` → **400** (not 412); `InvalidArgument`
→ 400; `ResourceExhausted` → **429**; there is **no** built-in canonical
variant that resolves to 412, 422, or 507 at all. This means "genuinely make
`PreconditionFailed` emit 412" cannot be done by calling a different builder
method on `FileResourceError`/`CanonicalError` — those two only expose
`failed_precondition()` (→400) — it requires either extending
`toolkit-canonical-errors` (a cross-cutting library change, out of this
gear's unilateral control) or constructing the RFC-9457 `Problem` response
directly with a hardcoded status, bypassing the canonical-error derive for
this one variant.

**Design decision required per sub-case** (surface explicitly in the PR
description — do not silently pick one without recording the choice):

1. **`PreconditionFailed` (bind `If-Match` mismatch / delete-file `If-Match`
   mismatch)** — routes.rs declares 412 at `routes.rs:119-124` (bind) and
   `routes.rs:230-234` (delete_file); the real behavior is 400. **Recommended
   decision: fix the route declarations to 400, not the code.** Rationale:
   `toolkit-canonical-errors` is a shared library used by every gear; adding a
   412-capable variant (or a raw-status escape hatch) is a cross-team change
   with its own review cost, and 400 is a defensible (if non-idiomatic) choice
   for a failed precondition under this platform's existing GRPC-style
   canonical-error taxonomy (`FailedPrecondition` deliberately collapses to
   400 by design elsewhere in the platform — verify this against one other
   gear's error mapping to confirm it's a house convention, not an oversight,
   before committing to "fix the route"). If the team decides 412 is
   non-negotiable for conditional-request semantics (it is textbook-correct
   HTTP), escalate a `toolkit-canonical-errors` change as a separate,
   cross-cutting ticket rather than working around it locally in
   `error.rs` (a local hand-rolled `Problem` bypassing the derive would
   fragment this gear's error handling from every other gear's).
2. **`QuotaExceeded` → 429**: correct as implemented; **fix the docs**
   (api.md's 507 claim) and remove any `.error_507`-style declaration if one
   exists on the relevant routes (grep confirmed none currently declared —
   only api.md's prose is wrong; `routes.rs` has no `.error_507` calls at
   all in this gear). No code change.
3. **`PolicySizeExceeded` → 400**: correct as implemented; **fix the docs**
   (api.md's 413/507 claims for policy-driven size rejection — distinguish
   this from the sidecar's genuinely-413 max_size/exact_size stream-abort,
   which is a different, correct 413 documented elsewhere in api.md and not
   in scope here).
4. **Multipart initiate `.error_422(openapi)` (`routes.rs:433`)**: no
   `DomainError` variant maps to 422; `MultipartNotSupported` → `invalid_argument()`
   → 400, already separately declared via `.error_400(openapi)` on the same
   route (`routes.rs:429`). **Recommended decision: drop the `.error_422(openapi)`
   line** — it is pure OpenAPI-declaration noise since 400 already covers the
   only error this route actually returns for "backend doesn't support
   multipart." Do not invent a new 422-mapped variant just to satisfy an
   unused declaration.

**Implementation steps**:

1. Remove `.error_422(openapi)` from the multipart-initiate route
   registration (`routes.rs:433`), per decision 4.
2. Update `routes.rs:119-124` and `routes.rs:230-234` from
   `StatusCode::PRECONDITION_FAILED` to `StatusCode::BAD_REQUEST` (or the
   team's escalated alternative if they choose to pursue a real 412 — in that
   case this step becomes "extend `toolkit-canonical-errors`" instead, which
   is Tier-4-sized and out of scope for this PR; flag it and stop here with
   the doc-only fix).
3. Add the new table-driven test (below) as the durable guardrail — this is
   the actual deliverable that prevents re-drift, more valuable than the
   individual route edits.
4. Update `docs/api.md`'s status-code table (ties to Tier 3 item 3.7 — do
   that doc edit in the same PR or immediately after, per 3.7's own
   sequencing note).

**Verify after each step**:
- Step 1-2: regenerate OpenAPI, diff shows only the expected status-code/
  declaration changes on the three routes touched.
- Step 3: `cargo test -p file-storage --test error_mapping_test` green,
  and (regression check) temporarily revert step 2 and confirm the new test
  fails — proving the test actually pins the contract.

**New/changed tests**:
- Unit — new `tests/error_mapping_test.rs`, table-driven exactly per the unit
  doc's canonical `domain_errors_map_to_correct_status_codes` pattern:
  ```rust
  #[test]
  fn error_domain_error_maps_to_expected_http_status() {
      let cases: Vec<(DomainError, u16)> = vec![
          (DomainError::PreconditionFailed { message: "x".into() }, 400),
          (DomainError::QuotaExceeded { reason: "x".into() }, 429),
          (DomainError::PolicySizeExceeded { limit_bytes: 1, limit_source: "x".into() }, 400),
          (DomainError::MultipartNotSupported { backend_id: "x".into() }, 400),
          // ... every variant in error.rs, exhaustively
      ];
      for (err, expected) in cases {
          let canonical: CanonicalError = err.into();
          assert_eq!(canonical.status_code(), expected);
      }
  }
  ```
  Use an exhaustive `match` somewhere in the test (or a `strum::EnumIter` over
  `DomainError` if it derives one) to force a compile error when a new
  `DomainError` variant is added without a corresponding table row — this is
  what makes the test a durable guardrail rather than a one-time snapshot.
  Add a second small table cross-checking the three routes touched above
  against the same status constants (parse `routes.rs`'s declared
  `StatusCode::*` for `bind`/`delete_file`/multipart-initiate is not
  practical to do by reflection — instead, hard-code the expected pairs as a
  second `vec![("file_storage.bind", 400), ...]` comment-linked to the actual
  route file/line so a reviewer can eyeball the two sides side by side).
- E2E — one thin, representative case, per the plan's "one call per API
  method" rule: extend the existing bind-related call in
  `lifecycle/test_file_storage_lifecycle.py` to additionally assert the
  precondition-failure response carries
  `Content-Type: application/problem+json` and the corrected status — do not
  open a new HTTP request/file just for this; piggyback on whatever lifecycle
  step already exercises a bind conflict. `test_file_storage_seams.py`
  already has `test_unknown_file_returns_problem_json` (404) as the template
  for the `Content-Type` assertion shape.

**Done-check**: `cargo test -p file-storage` green; `error_mapping_test.rs`
enumerates every `DomainError` variant (grep the enum definition and the test
file line counts to confirm parity); api.md updated in the same or an
immediately-following PR (3.7).

---

### 2.6 — Negative size / malformed hash only backstopped by DB CHECK constraints

**Goal / DoD**: a finalize call with `size < 0` or a hash whose decoded byte
length doesn't match the declared algorithm (32 bytes for SHA-256) is
rejected with `DomainError::Validation` (→ 400) before any DB round-trip,
never surfacing as a raw `DomainError::Database` (→ 500).

**Pre-state check**:
```
sed -n '503,557p' gears/file-storage/file-storage/src/api/rest/handlers.rs
sed -n '40,107p' gears/file-storage/file-storage/src/domain/service/write.rs
```
Expect: `handlers.rs:550-551` validates `hash_hex` is valid hex but not its
decoded length; `write.rs:81-89` (`finalize_upload`) and its
`finalize_upload_by_token` sibling (~lines 437-465) only guard the *upper*
size bound (`size > 0 && size.cast_unsigned() > limit`) — a negative `size`
skips this check entirely (the `size > 0` guard is false) and flows straight
to `self.store.finalize_version(...)`, which will only be caught by the
`file_versions` table's `CHECK (size >= 0)` constraint, surfacing as a raw DB
error.

**Implementation steps**:

1. `src/api/rest/handlers.rs::finalize_version` (around line 550-551) — after
   the existing `hex::decode` call, add a length check:
   ```rust
   if hash_value.len() != 32 {
       return Err(DomainError::validation("hash_hex", "must decode to exactly 32 bytes (SHA-256)").into());
   }
   ```
   (32 is hardcoded since this gear is SHA-256-only per Tier 4 item 4.6 —
   revisit if/when BLAKE3 selection lands).
2. `src/domain/service/write.rs::finalize_upload` (lines 44-107) — add a size
   guard immediately on entry (before the policy/backend lookups, so an
   obviously-invalid request never touches the DB or backend registry):
   ```rust
   if size < 0 {
       return Err(DomainError::validation("size", "must be non-negative"));
   }
   ```
   Apply the identical guard to `finalize_upload_by_token` (the sibling
   function, same file, ~lines 421-489) — both are hit from different entry
   points (`handlers.rs` JWT-authenticated path vs. token-authenticated
   `x-fs-token` path) and both currently share the same gap.
3. There is **no** JWT-authenticated finalize route — `finalize_upload`'s only
   callers are `data_plane.rs:90` (in-process sidecar path, which self-computes
   the digest via `hash::sha256` → always 32 bytes, and derives `size` from
   `bytes.len()` → never negative) and the `DataPlanePort` impl
   (`backend.rs:166`). So no independent hash-length check is needed at a second
   entry point; the handler-level check in step 1 (token path) plus the
   defense-in-depth `size < 0` guard in step 2 cover every reachable route.
   Keep the `size < 0` guard in `finalize_upload` anyway — it is cheap
   defense-in-depth against a future caller.

**Verify after each step**:
- Step 1: a request with a valid-hex but wrong-length `hash_hex` (e.g. 16
  bytes) now returns 400 with field violation `hash_hex`, not a downstream
  500.
- Step 2: a request with `size: -1` returns 400 with field violation `size`,
  not a `CHECK` constraint violation surfacing as 500.

**New/changed tests** (unit only — pure input-validation-before-DB-write
logic, deterministic, no PG-specific behavior):
- Extend `tests/enforce_test.rs` near `finalize_oversized_upload_is_rejected`
  (line 202), in the 2.5 table-driven style:
  - `finalize_negative_size_is_rejected_with_400_not_500` — call
    `finalize_upload`/`finalize_upload_by_token` with `size: -1`; assert
    `DomainError::Validation { field: "size", .. }` (primary outcome), and
    (secondary artifact) the `file_versions` row is still `pending` with
    unchanged `size`/`hash_value` via direct entity `find()`.
  - `finalize_truncated_hash_hex_is_rejected` — 16-byte (not 32-byte) valid
    hex; assert `DomainError::Validation { field: "hash_hex", .. }` at the
    handler/service boundary.
  - `finalize_oversized_hash_hex_is_rejected` — 48-byte hex; same assertion,
    covers the "too long" direction the length check now catches.

**Done-check**: `cargo test -p file-storage` green; manually confirm (by
temporarily removing the new guards) that a negative size / wrong-length hash
previously surfaced as `DomainError::Database` → 500, and now surfaces as
`DomainError::Validation` → 400.

---

### 2.7 — `delete_version` vs `bind` race leaves `files.content_id` dangling
**Goal / DoD**: a version cannot be deleted while a concurrent `bind` is making
it the current version; `files.content_id` never points at a nonexistent
version.

**Pre-state check**:
```
sed -n '261,304p' src/domain/service/read_ops.rs
sed -n '185,212p' src/infra/storage/repo/version_repo.rs
grep -n "content_id" src/infra/storage/migrations/m20260624_000001_p1_initial.rs
```
Expect: `delete_version` checks `file.content_id == Some(version_id)` on a
**pre-transaction snapshot** (`read_ops.rs:284-288`), then deletes via
`VersionRepo::delete`, whose filter is only `(file_id, version_id)` with no
`is_current = false` guard (`version_repo.rs:193-212`). `files.content_id` has
**no FK** to `file_versions` (`p1_initial.rs:33-67` — only child→parent cascade),
so nothing at the DB level prevents a dangling pointer.

**Failure scenario**: file has current version A, old version B. T1 `DELETE
.../versions/B` passes the "not current" check. T2 `bind {version_id: B}`
commits (`content_id := B`, `is_current := true`). T1 then deletes B →
`content_id` points at a deleted version; `download-url` 404s a file that claims
content; ETag derives from a dangling id. The reverse interleaving dangles too
(bind's version-exists check at `write.rs:133-141` is outside the transaction).

**Implementation steps**:
1. `src/infra/storage/repo/version_repo.rs::delete` — add `Column::IsCurrent.eq(false)`
   to the delete predicate so a delete inside the transaction can never remove
   the current version. Return `rows_affected` so the caller distinguishes
   "not found / was current" from "deleted".
2. `src/domain/service/read_ops.rs::delete_version` — move the "is this the
   current version?" check **inside** the same transaction as the delete
   (re-read `content_id`/`is_current` transactionally), and treat a `0`
   rows-affected result as `Conflict`/`version_not_found` rather than a silent
   success.
3. **Alternative / additional** (bring to team): add a
   `files.content_id → file_versions(version_id)` FK with `ON DELETE RESTRICT`
   in an additive migration — the DB then structurally forbids the dangle. This
   is the strongest fix; the predicate guard above is the minimal one.

**New/changed tests** — extend `tests/service_test.rs`:
- `delete_current_version_is_rejected` — delete the version `content_id` points
  at; assert `Conflict`/`version_not_found` and the row survives.
- `delete_version_then_bind_cannot_dangle` — deterministic ordering that would
  have dangled pre-fix; assert `content_id` always resolves to an existing row.
- No E2E — deterministic ordering, unit-tier per doctrine.

**Done-check**: `cargo test -p file-storage delete_current_version_is_rejected` green.

---

### 2.8 — Orphan reconciliation leaves permanent zero-version `files` rows
**Goal / DoD**: a file created by `POST /files` whose upload never completes does
not linger forever as a version-less row appearing in `GET /files`; the sweep
removes it.

**Pre-state check**:
```
sed -n '126,198p' src/domain/cleanup.rs
```
Expect: `sweep_abandoned_pending` deletes only the pending **version** row
(+ blob) via `store.delete_version`. Since `POST /files` inserts file + pending
version together (`files.rs:213-300`), abandoning the upload leaves the parent
`files` row (no versions, `content_id NULL`) with no sweep path targeting it —
it keeps showing up in listings as a file that can never serve content.

**Implementation steps**:
1. `src/domain/cleanup.rs`, `sweep_abandoned_pending` — after deleting the
   last/only version of a file whose `content_id IS NULL`, delete the parent
   `files` row too in the same transaction, emitting the `file.deleted`
   event/audit (reuse `delete_file_with_event`). Guard on "no remaining
   versions AND content_id is null" so a file that still has other versions is
   never touched.
2. Add a `SweepResult` counter (`abandoned_files_deleted`) alongside the
   existing three.
3. **Verify**: build + clippy clean.

**New/changed tests** — extend `tests/cleanup_test.rs`:
- `sweep_deletes_abandoned_zero_version_file` — create a file, let its only
  pending version age past the grace window, run the sweep; assert both the
  version **and** the parent `files` row are gone.
- `sweep_keeps_file_with_other_versions` — negative control: a file with one
  abandoned pending version and one `Available` version keeps the file row.
- No E2E — deterministic, unit-tier.

**Done-check**: `cargo test -p file-storage sweep_deletes_abandoned_zero_version_file` green.

---

### 2.9 — Superseded-version retention is dead code (never runs)
**Goal / DoD**: old non-current versions are actually reclaimed per retention
config, or the unused code + its misleading docstrings are removed.

**Pre-state check**:
```
rg -n "list_non_current_versions_older_than|list_non_current_older_than" src/
```
Expect: `Store::list_non_current_versions_older_than`
(`store/lifecycle.rs:73-82`) and its repo query
`list_non_current_older_than` (`version_repo.rs:243-267`, docstring "Used by the
retention-policy sweep for superseded versions") have **no caller** —
`CleanupEngine` never invokes them and they are not on the `CleanupStore` port
(`ports.rs:35-95`). Non-current versions accumulate unboundedly despite the docs.

**Implementation steps** (pick one, bring the choice to the team):
- **Wire it in** (preferred if superseded-version reclamation is intended P2):
  add the method to the `CleanupStore` port + `Store` impl, and call it from
  `run_sweep` with a policy-derived cutoff (per-file/policy `keep_last_n` or
  `max_non_current_age_days`). Add a `SweepResult` counter.
- **Delete it** (if reclamation is P3): remove both methods and the docstrings
  claiming they run, and note the deferral in `DESIGN.md`/the retention feature
  doc so the code and docs agree.

**New/changed tests** — if wired in, extend `tests/cleanup_test.rs`:
- `sweep_reclaims_old_non_current_versions` and a negative control keeping
  recent/current versions. If deleted, no test needed beyond the build.

**Done-check**: either the new sweep test is green, or `rg` shows the dead
methods removed and the docs updated.

---

### 2.10 — Malformed `If-Match-Metadata` is silently dropped → unconditional overwrite
**Goal / DoD**: a present-but-unparseable `If-Match-Metadata` header is a `400`,
not a silent fallthrough that applies the metadata patch without the CAS
predicate.

**Pre-state check**:
```
sed -n '215,225p' src/api/rest/handlers.rs
```
Expect: `handlers.rs:220-221` —
`header_str(&headers, "if-match-metadata").and_then(|s| s.trim().trim_matches('"').parse::<i64>().ok())`
— an unparseable value collapses to `None`, so `touch_meta` runs **without** the
CAS predicate (`file_repo.rs:139-142`) and the patch applies unconditionally.
The advertised optimistic-concurrency guarantee silently degrades for exactly
the clients that tried to use it.

**Implementation steps**:
1. `src/api/rest/handlers.rs` (patch-metadata handler) — distinguish "header
   absent" (→ `None`, unconditional patch is fine) from "header present but
   unparseable" (→ `Err(DomainError::validation("if-match-metadata", "must be an integer version"))`).
   Parse only when the header is present; a parse failure is a 400.

**New/changed tests** — extend `tests/service_test.rs`/handler tests:
- `patch_metadata_malformed_if_match_returns_400`.
- `patch_metadata_absent_if_match_applies_unconditionally` (positive control).
- `patch_metadata_stale_if_match_returns_conflict` (existing CAS behavior lock-in).
- No E2E — deterministic header parsing.

**Done-check**: `cargo test -p file-storage patch_metadata_malformed_if_match_returns_400` green.

---

### 2.11 — `compute_plan` arithmetic overflow / huge allocation from client-controlled `preferred_part_size`
**Goal / DoD**: a client-supplied `preferred_part_size` cannot cause an
overflow panic or a giant `Vec` allocation; it is clamped to a sane range at the
boundary.

**Pre-state check**:
```
sed -n '132,169p' src/domain/multipart.rs
grep -n "preferred_part_size" src/domain/multipart_service.rs
```
Expect: `round_up_to` (`multipart.rs:132-169`) computes `div_ceil * align`,
which overflows `u64` for a near-`u64::MAX` `preferred_part_size` (panic under
overflow-checks; wrap otherwise → tiny `part_size` → enormous `n_parts` →
`Vec::with_capacity` giant allocation at L148-149). Reached from
`InitiateMultipartReq.preferred_part_size` (`multipart_service.rs:304`) with no
upper bound. Currently latent behind `multipart_native` (default local-fs
rejects at `multipart_service.rs:253-256`) — arms the moment a multipart-capable
default backend (in-memory or S3) is configured.

**Implementation steps**:
1. `src/domain/multipart_service.rs::initiate_multipart_upload` — clamp
   `preferred_part_size` to `[min_part_size, max_part_size]` (e.g. 5 MiB … 5 GiB)
   at the DTO/service boundary before it reaches `compute_plan`; reject an
   out-of-range value with `DomainError::validation` (or silently clamp — team
   choice, but reject is clearer).
2. `src/domain/multipart.rs::round_up_to` — use checked arithmetic
   (`checked_mul`/`checked_div_ceil`) and return a domain error rather than
   panicking/wrapping, as defense-in-depth.

**New/changed tests** — extend `tests/multipart_test.rs`:
- `initiate_multipart_rejects_absurd_preferred_part_size` (`u64::MAX`).
- `round_up_to_does_not_overflow_on_max_input` (unit on the helper).
- No E2E — deterministic arithmetic.

**Done-check**: `cargo test -p file-storage round_up_to_does_not_overflow_on_max_input` green.

---

### 2.12 — `transfer_ownership` accepts an arbitrary, unvalidated target owner
**Goal / DoD**: `POST /files/{id}/transfer` cannot reassign a file to a
nonexistent or unauthorized target owner; usage is not credited to an arbitrary
UUID.

**Pre-state check**:
```
sed -n '313,400p' src/domain/service/write.rs
grep -n "transfer" src/api/rest/routes.rs
```
Expect: `transfer_ownership` (`write.rs:313-400`) sets `owner_kind`/`owner_id`
to caller-supplied values (`handlers.rs:482-499`) with no check that
`new_owner_id` is a real principal or same-tenant member; `report_usage` then
credits the arbitrary `new_owner_id` (L392-397). Gated only by the file WRITE
grant.

**Implementation steps**:
1. `src/domain/service/write.rs::transfer_ownership` — validate `new_owner_id`
   (existence / same-tenant membership) before the transfer. If principal
   validation requires a cross-gear lookup (account-management), gate on the
   SDK the way quota/usage do; if that SDK isn't wired, at minimum reject a
   transfer to `owner_id == None`/malformed and require same-tenant.
2. **Design call (🛑)**: if transfers should be privileged, require a distinct
   authorization action rather than reusing the file WRITE grant. Bring to team
   with 0.7's admin-scope decision.

**New/changed tests**:
- `transfer_to_nonexistent_owner_is_rejected`.
- `transfer_to_same_tenant_member_succeeds` (positive control).
- **E2E**: only if the principal-validation SDK is wired; otherwise unit.

**Done-check**: `cargo test -p file-storage transfer_to_nonexistent_owner_is_rejected` green.

---

### 2.13 — `FileService::sign_url` maps `Op::MultipartPart` to a route the sidecar does not serve (dead/wrong mapping)
**Goal / DoD**: `sign_url` cannot mint a multipart-part URL that 404s; the wrong
mapping is removed or corrected before anything starts calling it.

**Pre-state check**:
```
sed -n '171,183p' src/domain/service/mod.rs
grep -n "multipart" src/bin/sidecar.rs
```
Expect: `sign_url` (`service/mod.rs:171-183`) maps `Op::MultipartPart →
"multipart-part"` → `/api/file-storage-data/v1/multipart-part/{file}/{version}?...`,
but the sidecar's only multipart route is
`/api/file-storage-data/v1/multipart/{file}/{version}/parts/{part}`
(`sidecar.rs:112-115`). `MultipartService` builds its own correct URLs
(`multipart_service.rs:374-377`), so this arm is currently dead — but it is a
latent wrong mapping.

**Implementation steps**:
1. `src/domain/service/mod.rs::sign_url` — either make it reject
   `Op::MultipartPart` (if URL construction for parts belongs solely to
   `MultipartService`), or fix the path template to match the real sidecar
   route. Prefer reject-with-error to keep a single source of truth for the
   part URL shape.

**New/changed tests**:
- `sign_url_rejects_or_correctly_maps_multipart_part` — assert whichever
  behavior the fix chooses.
- No E2E — pure mapping logic.

**Done-check**: `cargo test -p file-storage sign_url_rejects_or_correctly_maps_multipart_part` green.

---

## Tier 3 — Documentation reconciliation (step-by-step)

These are editorial fixes, grouped tightly since most items touch few lines
each. Fix-or-fix-code verdicts below restate the plan's own decisions
(already independently confirmed against the current doc text and code) —
this section is about *executing* those verdicts precisely, not
re-litigating them.

### 3.1 — ADR-0004: PASETO `v4.public` + `kid` claimed; code is a bespoke codec

**Verdict**: fix the **ADR status**, not the code (for now) — record the
bespoke codec as an accepted interim implementation with a named follow-up.

**Steps**:
1. In `docs/ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md`, locate
   the "Decision Outcome" section (confirmed at lines 68-76: "Token format =
   PASETO `v4.public`... PASETO footer carries a key id (`kid`)").
2. Add a dated "Implementation note" subsection immediately after Decision
   Outcome stating: the P2 implementation
   (`src/infra/signed_url/mod.rs:9-12`) uses a bespoke
   `base64url(json).base64url(ed25519_sig)` codec instead of PASETO
   `v4.public`, with no `kid` field, as an accepted interim measure; link a
   tracking ticket (Tier 4 item 4.9, which already exists in this plan) for
   the eventual PASETO migration; state explicitly that the bespoke codec
   **must not** be used in any FIPS-constrained deployment until migrated,
   restating the ADR's own FIPS-posture section (lines 104-125) verbatim
   reasoning.
3. Do **not** silently edit the "accepted" Decision Outcome text itself —
   append the note, preserving the historical record of what was originally
   decided (per the plan's own instruction: "do not just edit ADR-0004's
   already-`accepted` text after the fact").

**Verify**: `grep -n "Implementation note" docs/ADR/0004-*.md` shows the new
section; `grep -n "kid" src/infra/signed_url/mod.rs` still shows no `kid`
field (confirms the doc note, not a code change, was made); no other file in
`docs/` still asserts PASETO is shipped without a caveat
(`grep -rn "PASETO" docs/` and manually check each hit is now caveated).

**Depends on**: none; standalone doc edit.

---

### 3.2 — DESIGN.md / ADR-0003: describe a sidecar-driven bind/finalize flow that doesn't exist

**Verdict**: fix the **docs** — the implemented three-request flow (presign →
PUT → finalize, then separate `bind`) is the shipped, defensible design.

**Steps**:
1. `docs/DESIGN.md` — locate the sidecar-calls-bind-directly description
   (confirmed at lines 899 and 1622-1626) and rewrite it to describe: sidecar
   receives a signed PUT URL → streams bytes → calls the token-authenticated
   `POST /files/{id}/versions/{version_id}/finalize` (flips `pending` →
   `available`, does **not** touch `content_id`) → client issues a separate
   `POST /files/{id}/bind` with `If-Match` to swap the pointer. Strike the
   "app-token + on-behalf-of" delegation language entirely, or move it to an
   explicit "considered, not implemented" note if the team wants it preserved
   as a future option.
2. `docs/ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md` — same fix at
   the confirmed location (lines 82-84): "one control + one data request"
   becomes "presign (control) → PUT (data) → finalize (data→control
   callback) → bind (control)" — three control-plane touches, one data-plane
   touch, not the two-request model the ADR currently describes.
3. Cross-check `docs/features/multipart-coordinator.md` and `docs/api.md` for
   any other place restating the two-request framing and fix those too in the
   same PR (grep `"one control"` / `"app-token"` / `"on-behalf-of"` across
   `docs/`).

**Verify**: `grep -rn "app-token\|on-behalf-of" docs/DESIGN.md docs/ADR/0003-*.md`
returns nothing (or only the explicit "considered, not implemented" note if
chosen); a fresh reader of the rewritten section can trace the exact call
sequence against `bin/sidecar.rs`'s real flow without contradiction.

**Depends on**: sequence after Tier 0 item 0.1 lands (0.1 changes the exact
finalize contract — the client-facing `size`/`hash_hex` fields may be removed
if sidecar-only finalize (option 1) is chosen) so this doc rewrite reflects
the *final* contract, not an interim one requiring a second pass.

---

### 3.3 — multipart-coordinator.md describes behavior the code doesn't perform

**Verdict**: split — fix the **code** for the genuinely-better-design claims
(bundled with Tier 0 item 0.2, since both touch
`complete_multipart_upload`/`handlers.rs`); fix the **doc** for the
offset-write model once multipart is backend-scoped; add an immediate
caveat regardless of timing.

**Steps** (the caveat, step 1, is independent and should land now; steps 2-4
are sequenced after/with 0.2):

1. **Immediate, standalone (S effort)**: in
   `docs/features/multipart-coordinator.md`, add a prominent caveat near the
   top (before the numbered flow sections) stating: "As of P2, the only
   registered default backend (`local-fs`) does not set
   `multipart_native: true` (`src/infra/backend/local_fs.rs:61-66`), so every
   `POST /files/{id}/multipart` call 422s [/ 400s per 2.5's fix] today. This
   document describes the intended design; see Tier 0 item 0.2 in the
   remediation plan for the tracked functional gap."
2. **Bundled with 0.2** — once `complete_multipart_upload` is reworked to use
   real reported parts: implement the doc's better-design claims that were
   confirmed as gaps: (a) `If-Match`/412 support on `complete` (doc line
   146-148 vs. current no-`If-Match`, 204-no-body at `handlers.rs:437-445`);
   (b) rich `200 {version_id, content_hash, size}` response instead of `204`;
   (c) `409` with an explicit missing-part-numbers list instead of a bare
   size-mismatch comparison. Update `handlers.rs::complete_multipart` and
   `routes.rs`'s registration (`StatusCode::NO_CONTENT` → `StatusCode::OK`
   with a schema) together.
3. **Doc fix for the offset-write claim** (doc lines 124-126): once 0.2
   assigns multipart to a `multipart_native: true` backend (S3, per Tier 1
   item 1.7, or a `local-fs` multipart implementation), update this section to
   describe the part-object-per-part model actually used
   (`{backend_path}.part.{N}`, `bin/sidecar.rs:363`) for any non-native
   backend, or remove the offset-write description entirely if the team
   decides multipart is S3-only going forward (per 0.2's own fix-approach
   framing).
4. **Test-quality gap (verifier B3)** — this is really a test-code fix, not a
   doc fix, but tracked here since it's `multipart-coordinator.md`-adjacent:
   the misleading `tests/multipart_test.rs` topology (own `BackendRegistry`,
   direct `store.upsert_multipart_part` calls at line 222) must be relabeled
   as "backend-contract tests for `InMemoryBackend`," not evidence that
   real-topology multipart works — add a doc comment at the top of each
   affected test function saying so, and add the
   `multipart_initiate_against_real_default_topology_is_rejected_until_backend_supports_it`
   test specified under 0.2's own coverage matrix so the current real-wiring
   failure is itself asserted rather than silently bypassed.

**Verify**: step 1 — `grep -n "does not set \`multipart_native" docs/features/multipart-coordinator.md`
finds the new caveat. Steps 2-4 — re-run `tests/multipart_test.rs` after 0.2's
fix and confirm the doc's claimed response shapes (`{version_id, content_hash, size}`,
412-on-conflict, 409-with-missing-parts) are now literally true by pointing a
`curl`/e2e request at the real endpoint and diffing the JSON body against the
doc's example.

**Depends on**: step 1 can land today, standalone; steps 2-4 depend on and
should land with Tier 0 item 0.2.

---

### 3.4 — Fabricated/missing endpoints across docs

**Verdict**: fix the doc for `HEAD /files/{id}` (remove) and for the
undocumented-but-real endpoints (add coverage); treat `GET
.../multipart/{upload_id}` (introspect/resume) as a real missing feature
requiring an explicit ship-or-defer decision.

**Steps**:
1. `docs/api.md:60` — delete the `HEAD /files/{id}` row; confirm via
   `grep -n "HEAD /files" src/api/rest/routes.rs` that no such route exists
   and there's no other evidence (PRD, DESIGN) it was ever separately
   planned before removing.
2. Add api.md sections/table rows for the endpoints that exist but are
   undocumented. ⚠️ **Use the real route names** (verified against
   `src/api/rest/routes.rs`): `POST .../versions/{version_id}/finalize` (ties to
   3.8, do after 0.1), `DELETE .../versions/{version_id}`, `GET /policy`,
   `PUT /policy`, `GET /policy/effective` (there is **no** `DELETE /policy`
   route — do not invent one), `POST/GET/DELETE /retention-rules`,
   `POST .../migrate`, and `POST /files/{id}/transfer` (the route is `transfer`,
   **not** `transfer-ownership`). Confirm the full real set via
   `grep -n "OperationBuilder::" src/api/rest/routes.rs` and diff against
   api.md's current table.
3. For `GET /files/{id}/multipart/{upload_id}` (introspect/resume) — this is
   `multipart-coordinator.md`'s only remaining unchecked acceptance-criteria
   line (line 343, not 335). Bring an explicit decision to the team: (a) implement it
   as a fast-follow (M effort — a read-only handler joining
   `multipart_uploads` + `multipart_upload_parts` by `upload_id`, returning
   the plan + received-parts state) before declaring P2 "done," or (b)
   formally re-scope to P3 by editing `DECOMPOSITION.md`'s DoD and the
   FEATURE doc's acceptance-criteria checkbox to say "deferred to P3" with a
   dated note. Either way, update the doc — do not leave the checkbox silently
   unchecked with no resolution note.

**Verify**: `grep -n "HEAD /files" docs/api.md` returns nothing; a route
inventory diff (`grep OperationBuilder:: routes.rs` vs. api.md's table of
contents) shows zero undocumented real routes and zero documented-but-fake
routes; `multipart-coordinator.md:343`'s checkbox has either `[x]` with a
linked PR, or an explicit "deferred to P3, see DECOMPOSITION.md" note.

**Depends on**: sequence *after* Tier 0 item 0.2 (multipart itself is
non-functional until 0.2 lands — documenting an introspect endpoint for a
broken feature is low-value ordering).

---

### 3.5 — migration.sql (doc schema) has DDL drift vs actual SeaORM migrations

**Verdict**: fix the doc — regenerate/hand-sync against the real migration
files.

**Steps**:
1. Diff `docs/migration.sql` against the concatenated `POSTGRES_UP` constants
   of `m20260701_000001_p2_initial.rs` and `m20260701_000002_multipart_plan_columns.rs`
   (and, after 2.1/2.4 land, the two new migrations from this Tier-2 work) —
   specifically: add `multipart_uploads.version_id`/`declared_size`/`part_size`;
   add `events_outbox.owner_id NOT NULL`; correct `idempotency_keys.response_status`
   to `int` (not smallint) and `response_body` to `text` (not jsonb); remove
   or annotate the BLAKE3/XXH3 `hash_algorithm` CHECK-widening claim (see step
   2); add `idempotency_keys.request_hash bytea NOT NULL DEFAULT '\x'` once
   2.1 lands; add the two partial unique indexes from 2.4.
2. For the BLAKE3/XXH3 claim: since Tier 4 item 4.6 explicitly defers BLAKE3
   to a future milestone, **remove** the claim from `migration.sql` rather
   than implementing it now — leave a one-line comment pointing at Tier 4
   item 4.6 for when it's picked up.
3. If `docs/migration.sql` does not already state it is a design reference
   and not the executable migration, add a header comment: `-- This file is a
   design reference kept in sync by hand; the executable migrations are the
   SeaORM files under src/infra/storage/migrations/. Do not run this SQL
   directly.`

**Verify**: line-by-line diff of `docs/migration.sql` against a
`pg_dump --schema-only`-style rendering of a freshly-migrated Postgres (or,
cheaper, against the concatenated `POSTGRES_UP` constants read manually) shows
zero remaining drift; `grep -n "design reference" docs/migration.sql` confirms
the framing note exists.

**Depends on**: land after 2.1 and 2.4's migrations exist, so this sync pass
only needs doing once.

---

### 3.6 — README.md / DECOMPOSITION.md misstate P2 scope

**Verdict**: fix the docs; recommend authoring proper FEATURE docs for the
undocumented P2 subsystems (policy engine, retention/cleanup, audit-trail,
ownership-transfer, backend-migration) rather than a one-line scope
correction, given their compliance weight.

**Steps**:
1. `README.md:47-60` — confirmed current text: "P2/P3 features above (sharing,
   S3/WebDAV facades, policies, audit, multipart, quotas, …) are declared in
   the PRD/DESIGN but not implemented in P1." Rewrite the "Implementation
   status" section to add a "P2 (this branch)" subsection listing what
   actually shipped: policy engine (allowed-types/size/metadata limits,
   tenant+user scope), retention rules + background cleanup sweep,
   idempotent create, audit outbox, events outbox (undrained — link Tier 4
   item 4.1), ownership transfer, backend migration, multipart upload
   (non-functional — link Tier 0 item 0.2). Keep "not yet implemented":
   sharing, WebDAV, quota enforcement wiring (Tier 1 item 1.4), S3 backend
   (Tier 1 item 1.7).
2. `docs/DECOMPOSITION.md:20-28` — the "Decomposition Strategy" section
   currently claims one feature, no shared tables beyond multipart. Author
   FEATURE docs (matching `docs/features/multipart-coordinator.md`'s
   structure — flows, acceptance criteria, `p1`/`p2` tags) for: `policy-engine`,
   `retention-cleanup`, `audit-trail`, `ownership-transfer`,
   `backend-migration`. Each new file lives under `docs/features/` alongside
   the existing multipart doc. This is the larger (M-L) option the plan
   recommends given `cpt-cf-file-storage-fr-audit-trail` and
   `-fr-ownership-transfer`'s compliance weight; if the team instead picks the
   smaller option, at minimum rewrite `DECOMPOSITION.md`'s "Overview" and
   "Decomposition Strategy" paragraphs to acknowledge full P2 scope without
   new per-feature files.
3. Update `DECOMPOSITION.md`'s table of contents to list the new FEATURE
   entries if (a) is chosen.

**Verify**: `grep -n "not implemented in P1" README.md` — the surrounding
text must now correctly state what's implemented in P2; if FEATURE docs were
authored, `ls docs/features/` shows 5 new files each with a checked/unchecked
acceptance-criteria section mirroring `multipart-coordinator.md`'s format.

**Depends on**: none directly, but naturally sequenced after the Tier 0/1
items that change the shipped behavior of these subsystems (0.7 policy authz,
1.4 quota wiring) so the new docs describe the *final* P2 state.

---

### 3.7 — api.md status-code table and multipart/finalize contract details are wrong

**Verdict**: fix the doc, synchronized with Tier 2 item 2.5's decisions.

**Steps**:
1. `docs/api.md` status-code section (confirmed lines 289-298): change the
   quota-exceeded line from `507 Insufficient Storage` to `429 Too Many
   Requests` (matches 2.5 decision 2); the size-limit-exceeded line from
   `413`/`507` to `400` for policy-driven rejections specifically, keeping
   `413` for the sidecar's genuine max_size/exact_size stream-abort case
   (matches 2.5 decision 3) — these are two different rejections and the doc
   must distinguish them clearly, not conflate them under one status.
   `412 Precondition Failed` line (lines 289-290): keep or change to `400`
   depending on which side 2.5 decision 1 lands on — **write this line last**,
   after 2.5 is actually merged, so it reflects the real final behavior.
2. `docs/api.md:73` — fix the `download-url` response shape from
   `{ download_url, etag, metadata }` to `{ download_url, etag, version_id }`
   (confirmed against `handlers.rs:204-208`'s actual `DownloadTicketDto`
   construction).
3. `docs/api.md`'s `409` summary section — add the currently-omitted
   `bind`/`delete_version`/`update_metadata`/`migrate` conflict cases (each
   already returns 409 in code via `DomainError::Conflict`/`AlreadyExists`;
   grep each handler for its `DomainError::conflict(...)` call site and add
   one line per case to the doc's summary table).
4. Multipart's own `413`/`507`/`422` claims (confirmed lines 109, 129, 140,
   204, 206) — apply the same 2.5-derived corrections: `507`→`429` for the
   `max_conns`/quota line, `422`→ whatever 2.5 decision 4 lands on (likely:
   remove the 422 claim, since no code path returns it).

**Verify**: `grep -n "507\|422\|412" docs/api.md` — each remaining hit must
correspond to an actual code path that returns that status (cross-reference
against the `error_mapping_test.rs` table from 2.5); no orphaned status-code
claims remain.

**Depends on**: after 2.5 lands (per 2.5's own note, ideally in the same PR
so there's no doc/code mismatch window).

---

### 3.8 — Undocumented functionality

**Verdict**: fix the docs — add an operational-configuration doc and document
the finalize s2s contract.

**Steps**:
1. Add a new `docs/operations.md` (or a "Operational configuration" section
   in README.md) covering every `FileStorageConfig` field
   (`src/config.rs:17-86`): `default_url_ttl_secs`, `max_url_ttl_secs`,
   `sidecar_base_url`, `default_page_size`, `max_page_size`, `storage_root`,
   `signing_key_seed` (with the multi-replica warning from Tier 1 item 1.3),
   `idempotency_ttl_secs`, `orphan_grace_secs`, `sweep_interval_secs`,
   `enable_background_sweep` — for each: default value (grep the
   `default_*()` fn), production recommendation, and what breaks if
   misconfigured (e.g. `enable_background_sweep = false` in prod means no
   orphan/retention cleanup ever runs, per Tier 1 item 1.4).
2. Document the finalize s2s callback contract in `docs/api.md` as a
   first-class endpoint: `x-fs-token` header, request/response body shape —
   **write this after Tier 0 item 0.1 lands**, since 0.1 will likely change
   this exact contract (removing client-supplied `size`/`hash_hex` if
   sidecar-only finalize is chosen).
3. Document `CleanupEngine`'s background sweep behavior (what it does: three
   sweeps — abandoned-pending, expired-multipart, retention-expiry — and,
   after Tier 2 item 2.1's plumbing and Tier 1 item 1.9's idempotency-key GC,
   a fourth), idempotent-create semantics, and the pluggable
   `SignatureProvider`/`SignatureVerifier` abstraction (`infra/signed_url`).

**Verify**: `docs/operations.md` (or README's new section) has one entry per
`FileStorageConfig` field — `grep -c "^pub " src/config.rs` (fields) should
roughly match the number of documented knobs; api.md's finalize section
matches the actual `FinalizeUploadReq`/response shape post-0.1.

**Depends on**: sequence after Tier 0 item 0.1 (finalize contract doc).

---

### 3.9 — Stale source comments (low-risk, quick wins)

**Verdict**: fix the code comments directly; bundle into any nearby PR rather
than a dedicated one.

**Steps**:
1. `src/domain/policy.rs:3-4` — remove/correct the "does NOT yet enforce on
   uploads (that is M2)" claim (confirmed false — `src/domain/service/create.rs`
   ~L130-158 enforces `check_allowed_mime`/`compute_effective_max_bytes`/
   `check_metadata_limits`/`check_quota` today). NOTE the real enforcement path
   is `domain/service/create.rs`, not `file_service/create.rs`.
2. `config/e2e-local.yaml:390` (repo-root `config/`, **not** `src/config/`) —
   correct "44 chars without padding" to the actual literal length (43 chars,
   `printf '%s' "$SEED" | wc -c` = 43) or simply remove the specific character
   count from the comment to avoid future drift.
3. Leave `events_outbox.rs:4-5`'s "Relay... deferred... P2-M5 TODO" comment
   untouched — it is accurate (Tier 4 item 4.1 confirms this is still
   deferred), not stale.

**Verify**: `grep -n "does NOT yet enforce" src/domain/policy.rs` returns
nothing; the e2e-local.yaml comment's stated length matches
`printf '%s' "$SEED" | wc -c` on the actual configured value.

**Depends on**: none; bundle opportunistically into whichever PR next touches
these files (e.g. 3.6's README pass could pick up policy.rs's comment in
the same commit).

---

### 3.10 — `DELETE /retention-rules/{id}` 404 says "File … not found"
**Verdict**: fix the code — the error mislabels the resource in the RFC-9457
payload.

**Pre-state check**:
```
sed -n '380,386p' src/api/rest/handlers.rs
```
Expect: `handlers.rs:383` returns `Err(DomainError::file_not_found(rule_id).into())`
for a missing retention rule — renders "File {rule_id} not found" with the file
resource type, misleading clients.

**Steps**:
1. Add a `retention_rule_not_found` variant to `DomainError` (or a generic
   not-found carrying the correct resource type) and use it here and in
   `delete_retention_rule`/`get_retention_rule` (which 0.7 also touches — do it
   in the same pass). Map it to 404 in `api/rest/error.rs`.

**New/changed tests**: `delete_missing_retention_rule_returns_retention_not_found`
— assert the problem+json `type`/detail names a retention rule, not a file.

**Done-check**: `cargo test -p file-storage delete_missing_retention_rule_returns_retention_not_found` green.

---

### 3.11 — Inactivity retention deletes actively-read files (behavior contradicts the doc)
**Verdict**: fix code **or** doc — the criterion says one thing, the code does
another; the two must agree.

**Pre-state check**:
```
sed -n '569,575p' src/domain/cleanup.rs
sed -n '174,180p' src/domain/policy.rs
```
Expect: `cleanup.rs:569-575` evaluates inactivity against `last_modified_at`,
which is bumped only by writes (bind/patch/transfer). Downloads (`download_url`,
sidecar GET) never touch any timestamp — yet `InactivityRetention`'s doc
(`policy.rs:174-180`) says "not accessed (**read or written**)". A file
downloaded daily but never rewritten is deleted after `inactivity_days`.

**Steps (pick one, bring to team)**:
- **Track last-access** (if read-activity should reset the clock): add a
  `last_accessed_at` column updated on download (careful: a write on every read
  is a hot-path cost — consider throttled/coarse updates), and evaluate
  inactivity against it. Additive migration.
- **Re-document** (if "not modified" is the real intent): change the
  `InactivityRetention` doc + the retention feature doc to say "not **modified**
  for N days" and rename the field/criterion accordingly so it stops promising
  read-awareness it doesn't have.

**New/changed tests**: if tracking last-access, `inactivity_retention_resets_on_download`;
if re-documenting, no code test — just the doc change + a comment test that the
criterion name matches the timestamp it reads.

**Done-check**: doc and code agree on what "inactivity" measures.

---

## Tier 4 — Deferred / planned (P3 or explicit follow-ups)

Brief per-item sketch plus blocking dependency and readiness check. Not being
implemented now — the deliverable here is a clear tracking entry for each.

### 4.1 — EventBroker relay (audit/events outbox drain)
**Sketch**: once an EventBroker gear SDK exists, add a background task (or
dedicated worker) in `CleanupEngine` polling `audit_outbox`/`events_outbox`
`WHERE published_at IS NULL`, publishing each row via the SDK, then stamping
`published_at`. **Blocking dependency**: EventBroker gear does not exist in
this repo yet. **Readiness check**: EventBroker SDK crate is published and
its publish-API contract (at-least-once? ordering guarantees?) is documented;
until then this item cannot start. **Required flip when unblocked**:
`tests/ownership_test.rs:221,252`'s `assert!(ev.published_at.is_none())`
must change to assert `is_some()` after a sweep/relay tick — do not let it
remain green-by-omission once the relay exists.

### 4.2 — Owner-deletion / Serverless Runtime workflow
**Sketch**: once 4.1 lands, add a consumer that reacts to an owner-deletion
event and runs a configurable Serverless Runtime workflow
(delete/archive/transfer disposition per PRD lines 620-644). **Blocking
dependency**: 4.1 (no event-consumption path exists without it) and
Serverless Runtime gear integration. **Readiness check**: 4.1 shipped, and
the Serverless Runtime gear's workflow-invocation SDK is available. **Flag
for the team**: PRD tags this `p2` despite zero code existing — get an
explicit call on whether P2 can ship without it.

### 4.3 — `enabled_event_types` policy field plumbed but never consulted
**Sketch**: gate each `make_file_event()` call site
(`create.rs:175`, `write.rs:177,346`, `read_ops.rs:224`) on
`policy.enabled_event_types.contains(&event_type)`, resolved via the same
`get_effective_policy_internal` helper these call sites already have access
to. **Blocking dependency**: none — self-contained, low urgency since the gap
only over-emits today (safe direction). **Readiness check**: none required;
can be picked up any time.

### 4.4 — Signed-URL rate/connection caps (`max_rate`/`max_conns`)
**Sketch**: add `max_rate`/`max_conns` fields to `Claims`/`UploadConstraints`
(`infra/signed_url/mod.rs`), enforce in `bin/sidecar.rs` via an in-memory or
Redis-backed limiter keyed by token/IP. **Blocking dependency**: none
technically, but low value until Tier 1 item 1.2's streaming rewrite lands
(no point rate-limiting a handler that's about to change its I/O model).
**Readiness check**: 1.2 merged; a decision on in-memory vs. shared
(Redis/cluster-sdk-backed) limiter state for multi-replica deployments (ties
to Tier 1 item 1.3's multi-replica concerns).

### 4.5 — SDK trait (`FileStorageClientV1`) is a bare placeholder
**Sketch**: implement the full trait surface
(upload/download-seekable-with-Range/delete/metadata/listing/version-listing-
restore/backend-capability-discovery) in `file-storage-sdk/src/api.rs` and
`domain/local_client.rs`, adapting the existing `FileService`/
`MultipartService` methods — mostly a facade task since the domain logic
already exists and is exercised via REST. **Blocking dependency**: none
technically; currently masked because no other gear consumes this trait.
**Flag for the team**: PRD tags this `p1`, not `p2` — confirm whether this
was supposed to precede P2 and got silently carried forward. **Readiness
check**: identify at least one prospective in-process consumer gear to
validate the trait shape against real usage before finalizing signatures.

### 4.6 — BLAKE3 alignment
**Sketch**: either implement BLAKE3 as a selectable algorithm behind a new
capability-discovery surface (`hash_policy` config, `hash_algorithm` request
field, streaming subtree hashing per the original FEATURE doc intent), or
formally amend ADR-0002 to record SHA-256-only as the shipped decision
(mirrors 3.1's ADR-amendment pattern). **Blocking dependency**: none
technically; primarily a scope/priority call. **Readiness check**: team
decision on which path — do not let api.md's `"part_hash_algorithm": "BLAKE3"`
example linger un-actioned regardless of which path is chosen (fix that
example immediately as a trivial doc correction, independent of the larger
decision).

### 4.7 — Blob↔DB reconciliation (backend-blob-without-row orphans)
**Sketch**: once a leader-election primitive is available, add a periodic
`list_paths()`-based enumeration sweep (safe only with leader election since
multiple replicas must not double-run it) that finds backend paths with no
corresponding `file_versions` row and deletes them after a grace period.
**Blocking dependency**: leader election (cluster-sdk, landed in `origin/main`
PR #4123, not yet adopted by file-storage). **Readiness check**: cluster-sdk
integration spike completed and its leader-election API is stable enough to
depend on from a background sweep.

### 4.8 — Encryption at rest
**Sketch**: P3-scoped per DESIGN.md §1.1; when picked up, likely a
backend-level wrapping (encrypt-before-`put`, decrypt-after-`get`) or
delegation to a KMS-backed backend variant. **Blocking dependency**: none
technical, purely a scheduling/priority call. **Readiness check**: in the
meantime, document (ties to Tier 3 item 3.8) that at-rest confidentiality
today depends entirely on volume-level encryption in the deployment
environment, so operators don't assume a guarantee that doesn't exist.

### 4.9 — Signed-URL key rotation (`kid`)
**Sketch**: add `kid` to `Claims`/multi-key verification support in
`infra/signed_url/provider.rs`. **Blocking dependency**: best bundled with
Tier 3 item 3.1's PASETO migration if/when that happens (PASETO's footer is
the natural `kid` carrier) — doing `kid` support against the bespoke codec
first would mean redoing the work when/if PASETO lands. **Readiness check**:
a decision on 3.1 (interim-forever vs. scheduled PASETO migration) — if the
team commits to "bespoke codec is permanent," `kid` support can proceed
independently against the current codec instead of waiting.

### 4.10 — Load/performance validation
**Sketch**: add a minimal load-test harness (k6/locust/vegeta) against a
running control+sidecar pair, covering at minimum metadata-latency (target:
25ms p95 per DESIGN.md) and single-sidecar throughput (target: 2.5 GiB/s),
wired as an optional/manual CI job. **Blocking dependency**: Tier 1 items 1.1
(fsync/atomic write — measuring against a write path about to change is
wasted effort), 1.2 (streaming rewrite — same reasoning), and 1.7 (S3
backend — the plan explicitly ties this item to validating the S3 backend
against a **real** external object store; `s3s-fs`-based unit coverage from
1.7 proves API-shape correctness only, not production-grade streaming
multipart behavior under real network conditions per the open upstream issue
`s3s-project/s3s#395`). **Readiness check**: 1.1, 1.2, and 1.7 all merged;
also a natural point to fold in Tier 2 item 2.2's "attacker inflates version
count via repeated `presign_version` calls" amplification-DoS angle, which
the plan explicitly defers to this item rather than treating as an e2e
correctness seam.

### 4.11 — Backup/restore procedure and runbook/alerting docs
**Sketch**: author an ops runbook covering Postgres+blob-store consistent
backup/restore, the signing-key/multi-replica requirements, and alerting
thresholds. **Blocking dependency**: Tier 1 item 1.8 (metrics — nothing to
alert on without them) and 1.3 (signing-key policy — the runbook needs to
state the final multi-replica signing-key story). **Readiness check**: 1.8
and 1.3 merged; also worth a quick standalone check (independent of this
item's main scope) of whether `quickstart-windows.yaml`'s missing
file-storage section is an intentional gap or an oversight, since it's a
real config gap discoverable today without waiting on anything else.

---

## Suggested PR breakdown & landing order

Tier 2/3/4 work should land after the Tier 0 critical path
(0.7 → 0.9 → 0.10 → 0.11 → 0.1 → 0.4 → 0.8 → 0.3 → 0.2 → 0.6 → 0.5) and can
interleave with Tier 1 per that tier's own suggested path. Within Tier 2/3,
group by shared file and by doc-after-code sequencing constraints.

**Tier 0 PRs** (land first, in critical-path order):
- **PR-T0a: authorization cluster (0.7 + 0.9 + 0.10 + 0.11)** — all four share
  the resource-less-`authorize`/`ADMIN_POLICY`/validation seams in
  `policy_service.rs`/`read_ops.rs`/`create.rs` and the shared
  `ScopedTestAuthorizer` test double. Land as one reviewed unit (or a tight
  stack) so the admin-scope decision (🛑) is made once. 0.10's key-scoping column
  shares an additive migration with 2.1's `request_hash` — coordinate.
- **PR-T0b..T0h**: 0.1, 0.4, 0.8, 0.3, 0.2, 0.6, 0.5 each as their own PR per
  their done-checks, in critical-path order.

**Tier 1 PRs**: the existing 1.1–1.9 path, plus the three new items:
- **1.10** (MIME validation) pairs with 1.2's streaming work (same sidecar path).
- **1.11** (sidecar range HTTP + leak) is standalone; land with 1.6's
  `build_router` refactor so the new tests have a router to drive.
- **1.12** (usage accounting) lands in the same PR that wires `usage_reporter`
  off `None` (the actionable half of 1.4), so the reporter is correct day one.

Within Tier 2/3:

- **PR-T2a: `error_mapping_test.rs` + error-code drift (2.5)** — standalone,
  no migration, touches `error.rs`/`routes.rs` only. Verification gate:
  `cargo test -p file-storage --test error_mapping_test` green and pinned
  against every `DomainError` variant; one e2e Content-Type assertion added
  to an existing lifecycle call (no new HTTP request).
- **PR-T2b: negative size / hash length validation (2.6)** — standalone, tiny,
  pairs naturally with 2.5 in the same PR if reviewers prefer (both are
  input-validation-before-DB-write hardening in the same handler/service
  files). Gate: `finalize_negative_size_is_rejected_with_400_not_500` and the
  two hash-length tests green.
- **PR-T2c: idempotency request-hash migration + replay validation (2.1)** —
  needs its own migration file; land independently since it touches
  `create.rs`/`idempotency_repo.rs`/entity/store layers not shared with
  other Tier 2 items. Gate: `cargo test -p file-storage --test multipart_test idempotency`
  green, new migration registered and SQLite-tested.
- **PR-T2d: policy upsert unique-constraint + `ON CONFLICT` rewrite (2.4)** —
  its own migration; can land in parallel with T2c (different tables). Gate:
  `policies_unique_index_rejects_duplicate_scope_tuple` and
  `policy_upsert_on_conflict_updates_existing_row_not_duplicates` green;
  manual Postgres spot-check of both partial indexes.
- **PR-T2e: version-repo CAS hardening (2.3) + version pagination/`get` fix
  (2.2)** — bundle these two since both touch `version_repo.rs` in adjacent
  functions (`rebind_backend` and `get`/`list_by_file`), minimizing merge
  conflicts. 2.3 **must include** the same-target-race regression test
  (`migrate_backend_same_target_race_preserves_winner_blob`). Gate:
  `concurrent_migrate_backend_second_racer_is_rejected`,
  `migrate_backend_same_target_race_preserves_winner_blob`, and
  `list_versions_caps_at_max_page_size`/`version_repo_get_returns_correct_row_among_many`
  all green; OpenAPI diff reviewed for the new query params.
- **PR-T2f: version-delete/bind race + orphan-file sweep (2.7 + 2.8)** — both
  touch the version lifecycle (`read_ops.rs`/`version_repo.rs`/`cleanup.rs`).
  Gate: `delete_current_version_is_rejected`,
  `sweep_deletes_abandoned_zero_version_file` green.
- **PR-T2g: superseded-version retention (2.9)** — wire-in or delete decision;
  standalone in `cleanup.rs`. Gate: sweep test green, or dead code removed +
  docs updated.
- **PR-T2h: metadata CAS + multipart-plan + transfer + sign_url hardening
  (2.10 + 2.11 + 2.12 + 2.13)** — small, mostly independent validation fixes;
  group by review convenience. Gate: each item's done-check test green.
- **PR-T3a: doc-only, no-code-dependency items (3.1, 3.5, 3.6)** — batch
  together since none require a preceding code change. Gate: grep-based
  verification steps from each item pass; **no `.rs` or config files touched**.
  NOTE: 3.9 is intentionally **excluded** — it edits `src/domain/policy.rs` and
  `config/e2e-local.yaml`, so it cannot satisfy a doc-only gate; per 3.9's own
  guidance, fold it into whichever nearby code PR next touches those files.
  3.5 is listed here but depends on the 2.1/2.4 migrations landing first (see
  the cross-tier reminder) — sequence PR-T2c/PR-T2d before this PR's 3.5 sync.
- **PR-T3b: 3.3's immediate caveat + 3.4's `HEAD`-removal/endpoint-inventory
  fix** — both are quick, independent of any pending code work. Gate: route
  inventory diff shows zero mismatches; multipart-coordinator.md caveat
  present.
- **PR-T3c: 2.5-dependent doc sync (3.7)** — lands immediately after PR-T2a
  merges, ideally as part of the same PR if review bandwidth allows, per
  3.7's own "do this after 2.5, same PR if feasible" note. Gate: every
  remaining status-code claim in api.md cross-references a real code path.
- **PR-T3d: 0.1-dependent doc work (3.2's finalize-flow rewrite, 3.8's
  finalize-contract documentation)** — lands after Tier 0 item 0.1 merges
  (separate PR from 0.1 itself, since 0.1 is code and this is docs-only, but
  gated on it). Gate: DESIGN.md/ADR-0003/api.md all describe the same,
  final finalize contract with no internal contradiction.
- **PR-T3e: 0.2-dependent multipart-coordinator.md behavior fixes (3.3 steps
  2-4) + 3.4's introspect/resume decision** — bundled with or immediately
  after Tier 0 item 0.2's PR, since both touch
  `complete_multipart_upload`/`handlers.rs`. Gate: doc's claimed response
  shapes are literally reproducible via a real request once 0.2 lands.
- **PR-T3f: retention-rule 404 resource type (3.10) + inactivity criterion
  reconciliation (3.11)** — 3.10 adds a `DomainError` variant (fold into 0.7's
  retention-rule PR, which already touches these handlers); 3.11 is a code-or-doc
  decision in `cleanup.rs`/`policy.rs`. Gate: 3.10's error-type test green; 3.11
  code and doc agree on what "inactivity" measures.
- **Tier 4 tracking**: no PRs — file one tracking ticket per item (4.1-4.11)
  referencing this plan section, each tagged with its blocking dependency so
  they surface automatically once that dependency lands (e.g. 4.10 tagged to
  reopen/notify once 1.1, 1.2, and 1.7 all close).

**Cross-tier ordering reminder**: PR-T2c and PR-T2d's migrations should both
be merged before PR-T3a's `migration.sql` doc-sync pass (3.5), so that sync
only has to happen once against the final schema rather than twice.


---
