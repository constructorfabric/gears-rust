<!-- Created: 2026-07-27 by Constructor Tech -->

# DB Behavior Audit — file-storage (Step 4)

Step 4 of the DB-behavior audit program: build a defect-detection system for
`gears/file-storage/file-storage/`, validate it against ground truth (an
independent review, `tmp-review0.md`, findings F1–F10), and produce a full
inventory + remediation. Methodology:
[`14_db_behavior_testing.md`](../../../../docs/toolkit_unified_system/14_db_behavior_testing.md)
(branch `docs/db-behavior-testing`); reference implementation:
[resource-group's own Step 1 audit](../../../system/resource-group/docs/analysis/DB_BEHAVIOR_AUDIT.md)
(branch `audit/rg-db-behavior`). The executable system lives in:

- `file-storage/tests/common/query_recorder.rs` — SQL trace recorder (copied
  verbatim from resource-group's own; see §7 for the shared-crate note).
- `file-storage/tests/db_behavior_audit_test.rs` — dynamic trace tests
  (SQLite) + a structural rule.
- `file-storage/tests/pg_concurrency_test.rs` — real-PostgreSQL concurrency
  harness (`testcontainers`), F1/F2/F9/F10 + a post-state invariant checker.
- `file-storage/tests/contract_drift_test.rs` — FS-06/F4 and FS-12 contract-
  drift pins.

## 0. Branch dependency (read before merging)

This audit runs on `audit/fs-db-behavior`, branched from
`docs-file-storage-final-state` (`aba5d9de`, PR #4231, not yet merged into
`main`) — **not** from `main` and **not** from any resource-group (RG)
branch. One platform commit was cherry-picked from RG's own audit branch,
each as its own commit with an explicit note in the message:

- `7064fdf3` (`test-support(toolkit-db): expose metric-callback attach +
  tx-boundary probe`, from `fix/rg-db-remediation`) — **mandatory**: this is
  the `toolkit-db` `test-support` surface `QueryRecorder` depends on
  (`connect_db_with_metric_callback`, `secure::in_transaction_for_testing`).
  Without it there is no way to attach a metric callback before a connection
  is wrapped into a `DBProvider` (see `06_authn_authz_secure_orm.md`'s
  security model), so this cherry-pick has no alternative.
- `92149362` (RG-15 contention fix) and `1ec463a9` (`secure_insert_many`)
  were **not** cherry-picked — neither was needed. file-storage never uses
  `transaction_with_retry`/`TxConfig::serializable()` at all (§1.1), so
  RG-15's error-shape-swallowing fix (which is specifically about
  `is_retryable_contention`/`transaction_with_retry`'s interaction) has no
  applicable call site here; `secure_insert_many` was not needed by any
  remediation in Part B (see §4).

**Consequence for merge order**: `libs/toolkit-db/src/secure/db.rs` (or
wherever `7064fdf3` touches) is the only file this audit's branch and any RG
branch both modify. Merging the RG branch (`audit/rg-db-behavior` or its
successor) into `main` **before** this branch will not conflict — the
cherry-picked commit here is identical in content to the one already on the
RG branch, so a subsequent merge/rebase of this branch onto a `main` that
already has RG's version will fast-forward cleanly on that file. Merging
this branch first, then RG's, is equally clean for the same reason. There is
no ordering hazard either way, only the (mild) redundancy of the same
cherry-picked commit potentially appearing in both branches' history until
one is rebased onto the other's result.

## 1. Methodology

Same three complementary mechanisms as RG's Step 1, plus a fourth this
gear's own findings required:

### 1.1 Dynamic SQL trace analysis (SQLite, deterministic)

`QueryRecorder` — copied verbatim from
`gears/system/resource-group/resource-group/tests/common/query_recorder.rs`
(branch `audit/rg-db-behavior`), with two additions: `total_elapsed()` and
`failed_statements()`, added to make the previously-unused `raw_sql`/
`elapsed`/`failed` fields on `RecordedQuery` genuinely load-bearing (rather
than suppressing clippy's dead-code lint) — `dump()` was also changed to
show `raw_sql` (bound values) instead of normalized SQL for failed
statements. **This is now duplicated verbatim in two gears' `tests/` trees**
— flagged, as in RG's own report, as a shared-test-crate candidate: a
second independent copy is exactly the "one instance isn't enough evidence
of the right shared shape" bar `14_db_behavior_testing.md` names being
cleared. A follow-up should factor it into a shared test-support crate
(candidate location: a new `libs/toolkit-db-test-support` or similar,
gated the same way `test-support` already is).

Rules built on the trace, same three as RG's:

- **`no-tx-write`**: `rec.writes_outside_tx()`.
- **`n-plus-one`** (scale-invariance): run at small N and large N, assert
  statement count for a `(kind, table)` shape is equal.
- **`redundant-io`**: `rec.redundant_reads_after_write()`.

**A genuine architectural difference from RG, found on first read**:
file-storage uses `db.transaction_ref_mapped(...)` **exclusively** —
`grep -rn "transaction_with_retry\|TxConfig" gears/file-storage/file-storage/src/`
returns **zero matches**. There is no `SERIALIZABLE`+retry anywhere in this
gear; its concurrency-safety strategy is single-statement optimistic-CAS
`UPDATE ... WHERE <expected-state>` predicates under the connection's
default isolation. Consequence: the `no-retry-serializable` class, as
literally defined (`transaction_ref_mapped_with_config` vs
`transaction_with_retry` — a specific pair of method names this gear never
calls either of, in the "wrong" or the "right" shape), **does not apply**
here — there is no positive example anywhere in this codebase to validate a
rule against. This is not "the gear happens to score 0 defects on this
class" — it is a different, and not inherently wrong, architecture (see
§6 Method Boundaries for what this means for the "10/10" framing).

### 1.2 Structural + static rules

Only one static rule was needed (the other, `no-retry-serializable`, does
not apply — see above):

- **`external-call-in-tx`**: `structural_rule_infra_storage_never_imports_infra_backend`
  walks every `.rs` file under `src/infra/storage/` (the only place
  `transaction_ref_mapped` is ever opened — confirmed by reading every
  `Store::*` transaction closure directly) and asserts zero textual
  references to `infra::backend`/`StorageBackend`. This is **stronger**
  evidence than RG's own version of this rule (a source-text heuristic over
  closure bodies matching a client-type shape): it is a real crate-internal
  module boundary — if `infra::storage` genuinely never imports
  `infra::backend`, no closure defined anywhere in that tree can reach a
  backend call, full stop, not just "this specific textual scan didn't spot
  one this time." Confirmed by fault injection (§3) that the rule does fire
  the moment that boundary is violated.
  
  **This directly corrects the coordinator's own initial framing**: the
  kickoff message called `external-call-in-tx` "probably the biggest
  finding class" for this gear, by analogy with RG-09. It is not — it is a
  **clean bill of health**, and a stronger one than RG's own (RG-09 was a
  real, found defect; here the defect class is structurally impossible by
  construction). Reported prominently and honestly rather than searching
  harder for a match to the expectation.

### 1.3 Real PostgreSQL concurrency harness

`tests/pg_concurrency_test.rs`, same `testcontainers` pattern as RG's own
(pinned `16-alpine`, shared container via `OnceCell`, graceful skip without
Docker — re-verified directly by pointing `DOCKER_HOST` at a closed port:
all 18 tests pass having done nothing, ~0.03s total, each printing its own
skip reason). Runs automatically as part of `cargo test -p
cf-gears-file-storage` — no `#[ignore]`, no environment variable.

Two of the four scenarios (F2, and the invariant-checker's own dedicated
test) needed real barrier/gate synchronization; F1 and F10 are
**deterministic by construction** (no barrier needed at all — see §2 for
why) and F9 is a **sequential temporal gap**, not a tight concurrent race
(also §2) — included in the PG suite for parity and real-dialect
confirmation, not because it needs gating.

F2's reproduction (the coordinator's own explicit focus — "the earlier
proposed one-line fix is insufficient") needed a purpose-built
`MultipartStore` decorator (`GatedMultipartStore`) with **three**
`tokio::sync::Notify` gates rather than `sleep`-based timing, because a
naive "gate only when B starts checking" design left "which side's real DB
commit lands first" up to the tokio scheduler — confirmed empirically: the
first cut of this harness produced the *other*, still-interesting
interleaving (the stale completer, not the freshly-woken one, lost) on its
very first run. A third gate (forcing B's own redundant `finalize_version`
call to wait until A's has *returned*, not just *started*) made the
intended interleaving reliable across repeated runs (verified 3x
consecutively, plus every run since).

### 1.4 Contract-drift comparison

New for this gear (the coordinator's own instruction: "check
`concurrency-and-failure-model.md` and ADRs against code"). Two shapes:

- **Behavioral drift** (FS-06/F4): a documented property ("every retry is
  safe") reproduced as false for a specific, real input via the domain
  layer directly (`contract_drift_test.rs`).
- **Documentation drift** (FS-12, new finding): a doc claim
  (`concurrency-and-failure-model.md`'s Race Catalog item 2) pinned via
  `include_str!` against the doc text itself, cross-referenced to the
  PostgreSQL test that mechanically falsifies it.

F5–F8 (also contract corrections, all in `tmp-review0.md`'s ground truth)
were verified by direct code reading against the doc/PRD claims they
correct, but were **not** given new tests: F5 (multipart+idempotency_key
rejected pre-DB) and F6 (manual bind emits no header) are request-
validation/HTTP-response-shape concerns with zero DB-transaction or
concurrency dimension — squarely `14_db_behavior_testing.md`'s own "What
Does NOT Belong Here" boundary (E2E/unit tests' job). F7 is already fully
corroborated by the existing capability-reject / negative-control pair in
`db_behavior_audit_test.rs` (they differ *only* in backend topology, which
is exactly what F7 says the gate keys on). F8 is a documented absence of a
route, verified by reading `routes.rs`, not a DB-behavior defect.

## 2. Validation matrix — ground-truth defects × general rule

**8/10** of `tmp-review0.md`'s F1–F10 are rediscovered by a general,
class-level rule (F3 is latent per the Runtime Caveat — see below; F5/F6/F8
are contract corrections outside this layer's scope by design, see §1.4).
Every F-number maps to an FS-ID (§4 has the full inventory including new
findings FS-11–FS-14).

| F# | FS-ID | Class | Location | Rule that reopens it | Executable evidence |
|----|-------|-------|----------|----------------------|----------------------|
| F1 | FS-01 | no automatic compensation (orphan) | `handlers.rs:201` (`create_file_bare`), `multipart_service.rs:479-482` (capability reject before any pending version exists), `cleanup.rs:321-352` (only reclaim path, never triggered) | direct reproduction + real sweep | `multipart_initiate_capability_reject_leaves_orphan_bare_file` (SQLite); `f1_capability_reject_orphan_survives_real_sweep` + `f1_backend_initiation_failure_orphan_survives_real_sweep` (real PG, both capability-reject AND backend-error halves, sweep run for real) |
| F2 | FS-02 | check-then-act-without-constraint (owner-fencing gap) | `multipart_repo.rs:244-260` (`finish_complete`'s CAS filters `state='completing'` only, not `lease_owner`); `multipart_service.rs:1294-1313` (finalize CAS fenced by pending-status only) | raw-SQL CAS-shape inspection (no general mechanized rule exists for this class — see `14_db_behavior_testing.md`'s own stated boundary) + real-PG deterministic race | `multipart_finish_complete_cas_omits_lease_owner` (SQLite, pins the WHERE-clause shape); `f2_stale_completer_strands_session_after_owner_unfenced_release` (real PG, full mechanism: both callers error, session stranded at `in_progress` with the version genuinely `available`/bound underneath, confirmed still-stuck across a third retry, confirmed permanently `aborted`-while-bound after the eventual sweep) |
| F3 | FS-03 | latent (usage undercounting on crash-recovery) | `multipart_service.rs:1027-1034` (takeover fast-path returns early, bypassing the byte-credit call at `:1361-1373`) | none mechanized — **latent per the Runtime Caveat**: `gear.rs` wires `usage_reporter: None` in the shipped build (confirmed by reading `gear.rs:189-217,230,236,264`), so there is no live reporter behavior to observe a miscount against at all today. Re-classify once a reporter is wired; not testable as a live defect before then | — (see §6 Method Boundaries) |
| F4 | FS-06 | behavioral drift (retry-safety edge) | `multipart_service.rs:796-806` (If-Match check) runs before `:809-833` (session-state replay check) | direct reproduction against the domain layer | `fs06_f4_completed_retry_with_stale_if_match_precondition_fails_instead_of_replaying` (SQLite); negative control `negative_control_fs06_completed_retry_without_if_match_replays_correctly` |
| F9 | FS-04 | check-then-act-without-constraint (CAS target staleness) | `multipart_service.rs:1273-1274` (`expected_content_id: file.content_id`, captured once at complete's start, not re-validated); `file_repo.rs:110-114` (`bind_content_cas`) | raw-SQL CAS-shape inspection + deterministic sequential reproduction (no barrier needed — see below) | `multipart_complete_auto_bind_cas_targets_observed_pointer_not_null` (SQLite, pins the CAS SQL shape); `f9_autobind_no_if_match_clobbers_prior_rebind` (real PG) + negative control `negative_control_f9_autobind_with_correct_if_match_rejects_stale_rebind` |
| F10 | FS-05 | ordering defect within one `run_sweep()` pass ("second path into F1") | `multipart_repo.rs:455-472` (`has_in_progress_for_file`, step-1-vs-step-2 ordering); `cleanup.rs:151-163` | direct reproduction (backdated timestamps, real sweep) — deterministic, no barrier needed (see below) | `f10_expired_session_orphans_parent_permanently_across_sweeps` (real PG): confirms the file survives the FIRST sweep pass as a version-less orphan, and confirms a SECOND sweep pass never revisits it either (the existing unit test this defect's sibling scenario resembles, `sweep_reclaims_version_after_session_expires` in `cleanup_test.rs`, never actually checked the parent file's fate — a real gap in existing coverage this audit found) |

**F9 and F10 are not barrier races** — both were deliberately implemented
without `tokio::sync::Notify`/`Barrier` gating, and this is a documented
judgment call, not an oversight. F9's exploitable window is the ordinary
(often long, user-driven) span between a multipart session's *initiate*
and its *complete* — a **temporal**, not concurrent, gap; nothing needs to
race for the bug to manifest, only for time to pass with an intervening
write. F10 is an **ordering defect within a single, sequential function
call** (`run_sweep`'s own step 1 running before step 2) — again nothing
needs to race, the defect is deterministic given the right pre-existing
timestamps. Both are included in the PostgreSQL suite anyway (real
dialect, real FK/cascade behavior), just via direct construction rather
than `Barrier`-synchronized pairs.

## 3. Fault-injection validation

For each rule class with a mechanized detector, a synthetic defect was
injected into a currently-*correct* operation elsewhere in the gear,
confirmed to newly fail the relevant test, then reverted (`git checkout
--`, confirmed via `git status`/`git diff` showing no residue) **before any
commit**. All four injected defects were reverted; none were committed.

| Class | Injected change (temporary, reverted) | Rule / test that caught it | Negative control |
|-------|------|------|------|
| `no-tx-write` | `Store::delete_file_with_event` (`infra/storage/store/files.rs`): replaced the `transaction_ref_mapped` closure with three sequential calls on a bare `self.db.conn()` | `trace_delete_file` (`writes_outside_tx()`) — failed, full untransacted trace printed (files DELETE, audit_outbox INSERT, events_outbox INSERT, all `in_tx=false`) | Same test passes on the unmodified file (verified before injecting and again after reverting) |
| `n-plus-one` | `MultipartRepo::delete_parts_for_upload` (`infra/storage/repo/multipart_repo.rs`): replaced the single batched `DELETE ... WHERE upload_id = ?` with a `list` then one `DELETE` per part | Temporary scale test (2 vs 8 parts through `abort_multipart_upload`): failed, 2 vs 8 `DELETE multipart_upload_parts` statements | Same temporary test passes (2 vs 2) on the unmodified file — confirmed by reverting and re-running, not merely by symmetry |
| `redundant-io` | `Store::create_file_with_pending_version` (`infra/storage/store/files.rs`): added `let _ = files.get(tx, &AccessScope::allow_all(), file_id).await?;` immediately after the file `INSERT`, inside the same transaction | Temporary check on `rec.redundant_reads_after_write()` after `create_file`: went from empty to non-empty | Same check is empty on the unmodified file |
| `external-call-in-tx` | `infra/storage/store/mod.rs`: added a dead function referencing `crate::infra::backend::StorageBackend` by name | `structural_rule_infra_storage_never_imports_infra_backend` — failed, correctly named the offending file | Same test passes on the unmodified tree (this is also the suite's every-day negative state — the rule passes on every commit in this branch's history) |

No class was skipped for lack of an injection site (unlike RG's
`redundant-io`, which had no clean call site to inject into — file-storage's
repo layer has several genuinely redundant-io-free write paths, e.g.
`create_file_with_pending_version` above, to inject into).

### 3.1 Negative controls beyond the fault-injection table

- **Read paths produce no write statements**:
  `negative_control_read_paths_produce_no_write_statements` (`get_file` +
  `list_versions`) asserts zero `Insert`/`Update`/`Delete` statements.
- **A capability-native backend does not falsely trip the F1 repro**:
  `negative_control_multipart_native_backend_initiate_succeeds` — identical
  call sequence to the F1 repro, only the backend topology differs
  (`InMemoryBackend` vs `LocalFsBackend`); proves the capability gate itself
  (not the audit's detector) is what correctly rejects only when it should.
- **A protected CAS is not flagged just because it's a CAS**:
  `bind_content_cas` itself (the gear's central concurrency-safety
  primitive) is exercised by `trace_full_upload_finalize_and_bind` and
  produces zero `writes_outside_tx()` findings — the detector does not
  treat "this statement has a WHERE clause narrowing on state" as
  suspicious by itself, only the *specific*, already-identified missing
  columns in FS-02/FS-04's CAS predicates.
- **Real-PostgreSQL invariant checker distinguishes healthy from broken**:
  `invariant_checker_distinguishes_healthy_file_from_known_orphan` runs the
  same `find_content_id_violations`/`is_versionless_orphan` checker against
  both a correctly-bound file (zero violations, not an orphan) and F1's
  exact reproduction (correctly flagged as an orphan, zero *content_id*
  violations — the checker's two functions have distinct, non-overlapping
  jobs, verified directly rather than assumed) — the same detector, not two
  different ones tuned to always agree with the label.
- **F9's negative control**: `negative_control_f9_autobind_with_correct_if_match_rejects_stale_rebind`
  — supplying the correct If-Match turns the clobber into a clean
  `PreconditionFailed`.
- **F4's negative control**: `negative_control_fs06_completed_retry_without_if_match_replays_correctly`
  — a no-If-Match retry (the common case) correctly replays.

## 4. Full findings inventory

FS-01 through FS-10 map onto `tmp-review0.md`'s F1–F10 (§2's table has the
exact mapping and rule); FS-11 onward are new, found during this audit's
own code reading and test construction, mirroring how RG's own audit
surfaced RG-11 through RG-16 beyond its ground truth. Severity is this
audit's own judgment (impact × likelihood under the gear's actual write
surface — see §6), not a CVSS-style score. **Status** reflects Part B of
this audit (§5): `Open` (not yet fixed, this audit is diagnostic-only for
that finding), `Fixed` (remediated in this branch, own commit), or
`Deferred` (a documented, reasoned decision not to fix now).

| ID | Class | Severity | Location | Repro | Status |
|----|-------|----------|----------|-------|--------|
| FS-01 | no automatic compensation (orphan) | **High** | `handlers.rs:201`, `multipart_service.rs:479-482`, `cleanup.rs:321-352` | `multipart_initiate_capability_reject_leaves_orphan_bare_file`; `f1_capability_reject_orphan_survives_real_sweep`; `f1_backend_initiation_failure_orphan_survives_real_sweep` | See §5 |
| FS-02 | check-then-act-without-constraint | **Medium** (worst case: session stranded until expiry) | `multipart_repo.rs:143-175,244-260`; `multipart_service.rs:1294-1321` | `multipart_finish_complete_cas_omits_lease_owner`; `f2_stale_completer_strands_session_after_owner_unfenced_release` | See §5 |
| FS-03 | latent (usage undercount) | Medium-High (latent) | `multipart_service.rs:1027-1034,1361-1373` | none (latent per Runtime Caveat) | Open — no reporter to fix against yet |
| FS-04 | check-then-act-without-constraint | Medium | `multipart_service.rs:1273-1274`; `file_repo.rs:110-114` | `multipart_complete_auto_bind_cas_targets_observed_pointer_not_null`; `f9_autobind_no_if_match_clobbers_prior_rebind` | See §5 |
| FS-05 | ordering defect ("second path into F1") | Medium | `multipart_repo.rs:455-472`; `cleanup.rs:151-163` | `f10_expired_session_orphans_parent_permanently_across_sweeps` | See §5 |
| FS-06 | behavioral drift | Medium | `multipart_service.rs:796-833` | `fs06_f4_completed_retry_with_stale_if_match_precondition_fails_instead_of_replaying` | Deferred (§5) |
| FS-07 | contract correction | Low | `handlers.rs:178-188` | verified by code reading (out of this layer's scope — see §1.4) | N/A (doc-only) |
| FS-08 | contract correction | Low | `handlers.rs:895-922` | verified by code reading (out of this layer's scope) | N/A (doc-only) |
| FS-09 | contract correction | Low | `multipart_service.rs:479-482`; `in_memory.rs:64-72`; `local_fs.rs:245-251` | `multipart_initiate_capability_reject_leaves_orphan_bare_file` + `negative_control_multipart_native_backend_initiate_succeeds` | N/A (doc-only) |
| FS-10 | contract correction (documented gap) | Low | `routes.rs:539-564` | verified by code reading | N/A (doc-only / feature gap) |
| FS-11 | n-plus-one (new) | Low-Medium | `store/metadata.rs::patch_metadata_atomic` — one INSERT/DELETE per patch entry | `scale_metadata_patch_inserts_do_not_grow_with_entry_count` (`#[ignore]`d) | Deferred (§5) |
| FS-12 | contract-drift (new) | Low (documentation only — see below) | `concurrency-and-failure-model.md`'s Race Catalog item 2 | `fs12_concurrency_doc_race_catalog_item_2_claim_is_falsified_by_pg_suite` | Open — doc fix tracked alongside FS-02 |
| FS-13 | mechanical evidence for FS-02 (new) | Low (standalone) | `multipart_repo.rs::acquire_complete_lease`'s CAS runs with `in_tx=false` | `trace_multipart_complete` (pinned, not ignored — this is the intentional shape) | N/A (not a standalone defect — see below) |
| FS-14 | n-plus-one (new) | Low | `store/files.rs::create_file_with_pending_version{,_with_event,_with_idempotency}` — one `upsert` per initial `custom_metadata` entry, same shape as FS-11 at a sibling call site | not separately tested (bounded by the `metadata-limits` policy's `max_pairs`, same bound FS-11 has — see §5) | Deferred, folded into FS-11's remediation decision |

### FS-02 in detail (the coordinator's specific focus)

`tmp-review0.md` explicitly flags "the earlier proposed one-line fix is
insufficient," and this audit's own reading confirms why: `finish_complete`
filtering on `lease_owner` alone (the "one-line fix") does not close the
gap, because the *decisive* gate is the version-finalize CAS three steps
earlier (`multipart_service.rs:1294`), which is fenced only by
`status = 'pending'`, never by lease ownership at all. Making
`finish_complete` owner-scoped without also fencing finalize would just
relocate the failure: the stale completer would still win the finalize
(bind the content correctly!) and then get a clean rejection at the
owner-scoped finish step instead of the current unqualified one — better
(a clean rejection instead of a stranding), but still not "assembly runs at
most once" (duplicate backend work is still possible), and the *taken-over*
completer's own error-handling still needs to not release a lease it no
longer legitimately holds. See §5 for the actual fix and what remains
deferred.

### FS-13 is not a standalone defect

`acquire_complete_lease`'s own CAS `UPDATE` runs on a bare connection
(`in_tx=false`, confirmed by `trace_multipart_complete`'s pinned assertion)
— but a single, self-contained `UPDATE ... WHERE` statement is atomic
regardless of whether it is wrapped in an application-level transaction;
this is not a `no-tx-write` violation the way FS-01/FS-anything-else would
be (there is no *second* statement in the same logical operation that a
crash between the two could leave inconsistent). It is listed because it is
**mechanical, independent evidence for FS-02**: the completion workflow is
genuinely several independently-atomic CAS steps with no overarching
transaction tying them together, exactly the shape FS-02 describes at the
domain level — the trace makes that shape directly visible in the SQL, not
just inferable from reading the Rust.

## 5. Part B — Remediation

<!-- REMEDIATION-STATUS-PLACEHOLDER: filled in as fixes land; see commit
     list in the final report message. -->

## 6. What this method does not cover

Same honest disclosure as RG's own report, generalized, plus what's
specific to this gear:

- **The `no-retry-serializable` class does not apply to this gear at all**
  (§1.1) — not "0 defects found," but "no positive/negative example pair
  exists to validate a rule against," since file-storage never uses
  `transaction_with_retry`/`TxConfig::serializable()` anywhere. Its
  concurrency-safety strategy (single-statement optimistic CAS) is a
  legitimately different architecture from RG's (SERIALIZABLE + application
  retry), not a worse one — but it means this method's "10/10" framing from
  RG's report does not translate directly: this audit's own validation
  matrix (§2) covers 8 of 10 ground-truth items with a mechanized rule
  (F3 latent, F5/F6/F8 correctly out of this layer's scope by design).
- **`external-call-in-tx` is structurally impossible here, not merely
  unobserved** (§1.2) — the module-boundary argument is stronger evidence
  than a source-text heuristic, but it is still bounded by the same caveat
  RG's report names: this proves no closure *defined inside*
  `infra::storage` can reach a backend call; it says nothing about a
  transaction opened by some *future* module this boundary doesn't cover.
- **`check-then-act-without-constraint` still has no general mechanized
  rule** (as `14_db_behavior_testing.md` itself states) — FS-02/FS-04's CAS
  gaps were found and pinned by direct SQL-shape inspection of a
  *known*-from-code-reading site, the same limitation RG's report names for
  its own tenant-root-uniqueness example. A constraint-inventory
  cross-reference tool (future work, not built here either) would be needed
  to find a *new*, unknown instance of this class mechanically.
- **The `tokio::spawn` boundary is exercised here, unlike RG.** RG's report
  notes zero `tokio::spawn`/`task::spawn` call sites in its own `src/`,
  so the `in_tx` probe's one structural blind spot (task-locals don't
  propagate across a spawn boundary) was never actually exercised there.
  file-storage's `complete_multipart_upload` **does** spawn a detached task
  for the assembly (`multipart_service.rs:896`) specifically so a client
  disconnect cannot cancel in-flight work — but the calling code still
  `.await`s the spawned task's `JoinHandle` on the ordinary success path, so
  every statement the spawned task issues is still captured by the
  recorder in every test in this suite (confirmed directly: FS-13's own
  finding depends on exactly this visibility). The blind spot this
  structural property implies — a **detached** spawn that outlives the
  caller and keeps writing after a test's assertions have already run — is
  not exercised by anything in this gear's normal request path, since
  nothing abandons the `JoinHandle` without awaiting it outside of a real
  client disconnect (not simulated in this suite). Three other
  `tokio::spawn` sites exist (`gear.rs`'s background sweep loop,
  `report_usage`'s fire-and-forget sinks ×3) — none of them write through
  `Store`/`transaction_ref_mapped`, so none engage this boundary either.
- **F3/FS-03 has no live behavior to check.** `gear.rs` wires
  `usage_reporter: None` in the shipped build — there is no reporter call
  whose output could be wrong, so nothing here asserts "usage accounting is
  broken," only that the code path *would* undercount *if* a reporter were
  wired (verified by reading the code, not by observing a live miscount).
- **SQLite's own `INSERT ... RETURNING` gap inflates absolute statement
  counts the same way RG's report documents** — every SQLite `ActiveModel::
  insert()` this audit's SQLite-backed tests measure issues an implicit
  extra `SELECT` PostgreSQL wouldn't. Does not change any scale-invariance
  verdict (constant per-row overhead, not a function of N).
- **Cost of a single large statement, predicate correctness (vs presence),
  `EXPLAIN`/query plans, deadlock ordering, pool starvation, migration
  drift** — none of these are examined here, for the same reasons RG's
  report gives; this method proves transactionality/scaling-shape/
  concurrency-safety properties, not performance or plan-level correctness.
- **Static rules are text heuristics.** The one static rule this gear needs
  (`structural_rule_infra_storage_never_imports_infra_backend`) is a plain
  `grep`-shaped substring scan over `include_str!`'d source — no AST, no
  type resolution. It is a real module-boundary argument (§1.2), but a
  file that referenced `infra::backend`/`StorageBackend` only via a fully
  different spelling this scan doesn't match (a type alias under a third
  name, say) would evade it; none currently exist.

## 7. Related documents

- [`docs/toolkit_unified_system/14_db_behavior_testing.md`](../../../../docs/toolkit_unified_system/14_db_behavior_testing.md)
  — the methodology, marked v1 pending revision after this audit; the
  architectural difference in §1.1 (no `transaction_with_retry` anywhere in
  this gear) and the `tokio::spawn`-is-actually-exercised note in §6 are the
  two concrete candidates for that revision.
- [resource-group's Step 1 report](../../../system/resource-group/docs/analysis/DB_BEHAVIOR_AUDIT.md)
  (branch `audit/rg-db-behavior`) — the reference implementation this audit
  followed; §1's shared-test-crate flag applies to both reports equally now.
- [`gears/file-storage/docs/concurrency-and-failure-model.md`](../concurrency-and-failure-model.md)
  — the doc FS-06/F4 and FS-12 correct; both discrepancies are pinned as
  executable tests (`contract_drift_test.rs`) rather than left as prose.
- `tmp-review0.md` (repo root, not committed) — the ground truth this audit
  validated against; read in full once, findings transcribed as FS-IDs
  throughout this report and the test suites.

## 8. Discrepancies found versus `tmp-review0.md`

Listed honestly, per this program's own discipline (Step 3's practice of
correcting an assumption when the code disagrees, applied here to a written
review rather than a coordinator's framing):

1. **The coordinator's kickoff message assumed `external-call-in-tx` would
   be "probably the biggest finding class" for this gear, by analogy with
   RG-09.** It is not — see §1.2. This is the single most consequential
   correction this audit makes to the *framing* it started from (not to
   `tmp-review0.md` itself, which does not make this claim).
2. **`tmp-review0.md`'s F2 write-up says the earlier proposed one-line fix
   ("filter `finish_complete` on `lease_owner`") is insufficient, without
   fully spelling out the mechanism** — this audit's own PostgreSQL
   reproduction (§2, §4) traces the exact three-step interleaving that
   makes it insufficient (stale completer wins finalize; taken-over
   completer's own failed redundant finalize releases the lease back to
   `in_progress` while the session is still nominally `completing`;
   original completer's finish CAS then fails against a state that is no
   longer `completing` *and* is not yet `Completed` either) and additionally
   discovered (FS-12) that this same interleaving falsifies a documentation
   claim `tmp-review0.md` does not itself cite (Race Catalog item 2).
3. **F9's exact mechanism is a temporal gap, not F9's own text
   mischaracterizing it as one** — `tmp-review0.md`'s F9 write-up already
   correctly describes this as "content was (re)bound between the multipart
   create and its complete," which this audit confirms precisely (§2);
   noted here only because this audit's own PostgreSQL test for F9
   deliberately does *not* use barrier gating (unlike F2), and that design
   choice is explained in §2/§1.3 as following directly from `tmp-review0
   .md`'s own correct framing, not as a discrepancy from it.
4. **No discrepancy found in F1/F3/F4/F5/F6/F7/F8/F10's substance** — all
   verified against the code exactly as described, at the cited lines,
   during this audit's independent read.

## 9. Self-check against acceptance criteria

- `cargo test -p cf-gears-file-storage`: green (SQLite audit/contract-drift
  tests fast, well under a second per binary; the PG harness binary runs in
  ~4s when Docker is present, ~0.03s when it gracefully skips).
- `cargo fmt --check` / `cargo clippy -p cf-gears-file-storage --tests -- -D
  warnings`: clean, verified after every commit in §0's list.
- 8/10 ground-truth defects rediscovered by general rules (§2); the
  remaining 2 (F3, and F5/F6/F8 collectively) are latent or correctly
  out-of-layer-scope by design, not misses.
- A fault-injection table covering every rule class with a mechanized
  detector (§3) — no class skipped for lack of an injection site.
- Negative controls against protected operations, read paths, and (real
  PostgreSQL) an SSI-adjacent invariant checker (§3.1).
- Every known-and-not-yet-fixed defect has a real, currently-failing (or,
  where non-ignorable/scale-independent, currently-passing-and-pinned)
  assertion — see §4/§5 for exact test names and statuses.
- Method boundaries stated plainly (§6), including two gear-specific
  findings (the `no-retry-serializable` non-applicability and the
  `tokio::spawn` boundary actually being exercised here) that
  `14_db_behavior_testing.md` v1 does not yet capture.
