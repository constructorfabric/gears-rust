# PR #1401 Review: Findings and Gaps

**PR:** https://github.com/cyberfabric/cyberfabric-core/pull/1401
**Title:** `docs(resource-group): add design docs and artifacts.toml ignore`
**Reviewed:** 2026-04-03
**Scope:** +5511/−362 lines across 32 files (documentation-heavy, one Python code change)

---

## Summary

PR #1401 adds extensive design documentation for the resource-group module, updates authorization architecture docs, adds testing guides, and fixes a TOC generation bug. During review, **3 critical gaps**, **4 notable issues**, and **1 reclassified finding** were identified.

---

## Critical Findings

### C1: `INTEGRATION_TEST_PLAN.md` claims "Done" status for non-existent code

**File:** `docs/arch/authorization/INTEGRATION_TEST_PLAN.md`

The "Current State" table marks the following as **Done**:

| Item | Claimed Status | Actual Status |
|------|---------------|---------------|
| PolicyEnforcer in RG handlers | Done | No implementation code exists |
| AccessScope → SecureORM in RG repo | Done | No implementation code exists |
| Rust integration tests (24 tests) | Done | Referenced test files do not exist |
| E2E HTTP tests (9 tests) | Done | Referenced test files do not exist |

The effort estimate table marks all three phases (tenant scoping, group predicates, MTLS) as **Done**.

Referenced test files that do not exist:
- `modules/system/resource-group/resource-group/tests/authz_integration_test.rs`
- `modules/system/resource-group/resource-group/tests/tenant_scoping_test.rs`
- `modules/system/resource-group/resource-group/tests/tenant_filtering_db_test.rs`
- `testing/e2e/modules/resource_group/test_authz_tenant_scoping.py`
- `testing/e2e/modules/resource_group/conftest.py`

The `modules/system/resource-group/` directory contains only a `docs/` folder — there is no implementation code at all.

**Impact:** Misleads anyone using the document to assess integration readiness.

**Recommendation:** Change status labels to "Planned" or "Designed" and clearly separate what is documented from what is implemented.

---

### C2: Architecture change — `resource_group_membership` projection removed from domain services

**Files affected:**
- `docs/arch/authorization/AUTHZ_USAGE_SCENARIOS.md`
- `docs/arch/authorization/DESIGN.md`
- `docs/arch/authorization/RESOURCE_GROUP_MODEL.md`
- `modules/system/authz-resolver/README.md`

This PR introduces a fundamental architecture change: `resource_group_membership` is now declared as **not projected to domain services** (rationale: too large at ~455M rows, ~110 GB at scale).

**What changed (verified against `origin/main`):**

| Aspect | Before PR (origin/main) | After PR |
|--------|------------------------|----------|
| DESIGN.md diagram | `resource_group_membership` listed as "Local Projection" for domain services | Removed from diagram |
| Capabilities table | `group_membership` and `group_hierarchy` listed without restriction | Marked "(not projected to domain services)" |
| Scenarios S14–S17 | Standard patterns for domain services | Relabeled "(reference)" — RG-internal only |
| Scenario S19 capabilities | `["group_membership"]` | `[]` |
| S19 response predicate | `in_group` with group IDs | `in` with explicit task UUIDs |
| S20, S21 | Used `in_group`/`in_group_subtree` | Degraded to `in` with explicit resource IDs |
| RESOURCE_GROUP_MODEL.md | "not recommended for projection" (advisory) | "not projected" (hard constraint) |

On `origin/main`, the membership table was presented as an optional projection that domain services **could** adopt. After this PR, it is architecturally excluded — domain services must rely on PDP capability degradation (explicit `in` predicates with resolved resource IDs).

**Impact:** Changes the authorization contract for all domain services that might consume group-based predicates. The commit message says "add design docs" but this is a fundamental architecture decision.

**Recommendation:** Call out explicitly in the PR description as an architecture decision. Consider a separate ADR documenting the rationale and migration path.

---

### C3: `PatchGroupRequest` struct referenced but never defined

**File:** `modules/system/resource-group/docs/rust-traits.md`

Line 181 adds `patch_group()` to the `ResourceGroupClient` trait:

```rust
async fn patch_group(
    &self, ctx: &SecurityContext, group_id: Uuid, request: PatchGroupRequest,
) -> Result<ResourceGroup, ResourceGroupError>;
```

`PatchGroupRequest` is never defined as a struct in `rust-traits.md` or anywhere else in the repository. All other request types have explicit struct definitions:
- `CreateGroupRequest` — defined
- `UpdateGroupRequest` — defined
- `AddMembershipRequest` — defined
- `RemoveMembershipRequest` — defined
- **`PatchGroupRequest` — missing**

**Impact:** SDK contract is incomplete. Consumers cannot implement this interface from the docs alone.

**Recommendation:** Add the `PatchGroupRequest` struct definition using `Option<Option<T>>` fields per DESIGN.md PATCH semantics (omitted = unchanged, explicit `null` = clear field).

---

## Notable Findings

### N1: Inconsistent PR forward-references

**Files:** `AUTHZ_USAGE_SCENARIOS.md`, `INTEGRATION_TEST_PLAN.md`

| File | Text | PRs Referenced |
|------|------|---------------|
| AUTHZ_USAGE_SCENARIOS.md:108 | `group_hierarchy (Phase 2 — planned in PRs #1405, #1406)` | #1405, #1406 |
| AUTHZ_USAGE_SCENARIOS.md:109 | `group_membership (Phase 2 — planned in PRs #1405, #1406)` | #1405, #1406 |
| INTEGRATION_TEST_PLAN.md:161 | `Phase 2: Group-Based Predicates (Planned — implemented in PRs #1406–#1407)` | #1406, #1407 |
| INTEGRATION_TEST_PLAN.md:206 | `Phase 3: MTLS Authentication Mode (Planned — implemented in PR #1407)` | #1407 |

#1405 appears in one document but not the other. #1407 is claimed for both Phase 2 and Phase 3 in INTEGRATION_TEST_PLAN.

**Recommendation:** Align PR references across both documents.

---

### N2: Broken manual TOC link in `AUTHZ_USAGE_SCENARIOS.md`

**File:** `docs/arch/authorization/AUTHZ_USAGE_SCENARIOS.md`

Line 26 TOC entry:
```markdown
[When to Use `resource_group_membership`](#when-to-use-resource_group_membership)
```

Actual heading at line 139:
```markdown
### `resource_group_membership` — RG-Internal Only
```

The anchor `#when-to-use-resource_group_membership` does not match the actual heading anchor `#resource_group_membership--rg-internal-only`. This is a manual TOC (no `<!-- toc -->` markers), so it won't auto-regenerate.

**Recommendation:** Update line 26 to match the renamed heading.

---

### N3: OpenAPI `PageInfo` schema out of sync with Rust struct

**Files:** `modules/system/resource-group/docs/rust-traits.md`, `modules/system/resource-group/docs/openapi.yaml`

The Rust `PageInfo` struct was updated:
```rust
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub limit: u64,               // was i32
    pub has_next_page: bool,      // NEW
    pub has_previous_page: bool,  // NEW
}
```

The OpenAPI schema still defines only `next_cursor`, `prev_cursor`, and `limit: integer` — missing `has_next_page` and `has_previous_page`. The E2E testing guide (`13_e2e_testing.md`) references `page["page_info"]["has_next_page"]` in examples, creating a three-way inconsistency.

**Recommendation:** Update OpenAPI schema to match the Rust struct.

---

### N4: Credstore `credentials_storage` plugin removed from design docs

**Files:** `modules/credstore/docs/DESIGN.md`, `modules/credstore/docs/PRD.md`

All references to the `credentials_storage` Rust microservice plugin are removed: plugin component, deployment topology, KMS integration, `KeyProvider` abstraction, external key management interface, encryption & key management PRD section, GTS type instance.

**Mitigating context:** The plugin was documentation-only on `origin/main` — no implementation code, no Cargo.toml, no plugin directory existed. Design docs said "will be documented" and "planned" (future tense).

**Impact:** Legitimate scope reduction of an unimplemented component, but removes the entire future design surface area for encrypted credential storage with KMS integration.

**Recommendation:** Acknowledge the descope in the PR description.

---

## Reclassified Finding

### R1: `toc.py` underscore fix — correct behavior (not a bug)

**File:** `.cypilot/.core/skills/cypilot/scripts/cypilot/utils/toc.py`

Initially flagged as breaking anchor compatibility. After verification:

- The old code stripped ALL underscores, producing **incorrect** GitHub anchors (e.g., `parent_message_id` → `parentmessageid`, but GitHub generates `parent_message_id`)
- 8 broken TOC anchors across 7 chat-engine ADR files on `origin/main` are fixed by this PR
- The fix correctly preserves word-internal underscores while stripping emphasis underscores

**Remaining risk:** 130 files use `<!-- toc -->` markers. Files not regenerated after this fix may have stale (but already-broken) anchors. No unit tests exist for `github_anchor()`.

**Recommendation:** Add unit tests for `github_anchor()`. Consider regenerating TOCs for all 130 files.

---

## Minor Observations

- **Delete response codes** changed from `200` to `204 No Content` for force-delete and membership delete — correct REST semantics but a contract change worth noting
- **New PATCH endpoint** (`PATCH /groups/{group_id}`) added to DESIGN.md with `Option<Option<T>>` semantics — a new API endpoint tracked via `cpt-cf-resource-group-fr-partial-update-group`
- **`update-docs.md`** uses `**` glob patterns in `grep` commands that require bash `globstar` — lower severity since this is a Claude command file (AI agent instructions), not a shell script
