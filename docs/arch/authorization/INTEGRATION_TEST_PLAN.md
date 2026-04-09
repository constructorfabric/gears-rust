<!-- Created: 2026-04-07 by Constructor Tech -->

# AuthZ + Resource Group Integration Test Plan

Design-time test plan for verifying the RG ↔ AuthZ interaction locally in hyperspot-server. Covers three phases: tenant scoping, group-based predicates, and MTLS bypass.

For background on how AuthZ uses RG data, see [RESOURCE_GROUP_MODEL.md](./RESOURCE_GROUP_MODEL.md). For concrete SQL-level scenarios, see [AUTHZ_USAGE_SCENARIOS.md](./AUTHZ_USAGE_SCENARIOS.md) scenarios S14–S21.

---

## Current State

| Component | Status | Notes |
|-----------|--------|-------|
| RG Module | Planned | This branch documents the intended `ClientHub` contracts (`dyn ResourceGroupClient` + `dyn ResourceGroupReadHierarchy`) but does not add implementation code yet |
| AuthZ Resolver | Existing | Plugin discovery, `PolicyEnforcer`, `AccessScope` → SecureORM already exist in the platform |
| Static AuthZ Plugin | Existing | Returns `In(owner_tenant_id, [tid])` — tenant predicates only |
| **PolicyEnforcer in RG handlers** | **Planned** | Target design: `GroupService` will call `enforcer.access_scope()` for list/get/hierarchy |
| **AccessScope → SecureORM in RG repo** | **Planned** | Target design: `GroupRepository.list_groups`, `find_by_id`, `list_hierarchy` will accept `&AccessScope` |
| **Rust integration tests** | **Planned** | Target inventory: 24 tests covering enforcer flow + tenant scoping + full-chain verification |
| **E2E HTTP tests** | **Planned** | Target inventory: pytest CRUD, hierarchy, membership, tenant isolation |
| Group predicates (`in_group`, `in_group_subtree`) | Planned | Requires new predicate types and RG-aware PDP behavior |

---

## Planned File Layout

```
testing/e2e/modules/resource_group/        ← E2E tests (pytest, HTTP against running server)
  conftest.py                              ← Fixtures: base_url, auth headers, type/group factories
  test_authz_tenant_scoping.py             ← Phase 1: CRUD + tenant isolation + hierarchy + membership

modules/system/resource-group/
  resource-group/tests/                    ← Rust integration tests (in-process, no HTTP)
    authz_integration_test.rs              ← PolicyEnforcer + mock AuthZ: 9 tests
    tenant_scoping_test.rs                 ← AccessScope scoping: 10 tests
```

Follows existing project conventions: `testing/e2e/modules/{module}/` for HTTP-level tests (see `oagw/`, `mini_chat/`, `types_registry/`), `modules/.../tests/` for Rust in-process tests.

---

## Prerequisites

- Rust stable (MSRV 1.92.0)
- Docker (for PostgreSQL)
- `protoc` installed (`brew install protobuf` on macOS)

### Start PostgreSQL

```bash
docker run -d --name rg-postgres \
  -e POSTGRES_USER=hyperspot \
  -e POSTGRES_PASSWORD=hyperspot \
  -e POSTGRES_DB=resource_group \
  -p 5433:5432 postgres:16-alpine
```

### Server Configuration

In `config/quickstart.yaml`, the resource-group module requires PostgreSQL:

```yaml
modules:
  resource-group:
    database:
      dsn: "postgres://hyperspot:hyperspot@127.0.0.1:5433/resource_group"
      pool:
        max_conns: 5
        acquire_timeout: "30s"
    config: {}
```

### Build and Run

```bash
# Without AuthZ (dev mode, auth_disabled: true)
cargo run --bin hyperspot-server -- --config config/quickstart.yaml run

# With AuthZ (auth_disabled: false + static plugins)
cargo run --bin hyperspot-server \
  --features static-authn,static-authz \
  -- --config config/quickstart.yaml run
```

### Target Test Commands

```bash
# Rust integration tests (no server/DB required)
cargo test -p cf-resource-group --test authz_integration_test --test tenant_scoping_test

# E2E tests (requires running server + PostgreSQL)
E2E_BASE_URL=http://localhost:8087 pytest testing/e2e/modules/resource_group/ -v
```

---

## Phase 1: Tenant Scoping via PolicyEnforcer _(Planned)_

**Goal**: Verify that RG endpoints apply `AccessScope` from AuthZ pipeline, filtering results by `tenant_id` from `SecurityContext`.

### Target implementation summary

The intended AuthZ → RG chain is:

1. **Module init** (`module.rs`): resolves `dyn AuthZResolverClient` from ClientHub, creates `PolicyEnforcer`
2. **GroupService** (`group_service.rs`): receives `PolicyEnforcer`; all CRUD methods (`list_groups`, `get_group`, `update_group`, `delete_group`, `list_group_hierarchy`) call `enforcer.access_scope(&ctx, &RG_GROUP_RESOURCE, action, resource_id)`
3. **GroupRepository** (`group_repo.rs`): `list_groups`, `find_by_id`, `list_hierarchy` accept `&AccessScope` and pass it to `SecureORM` via `.secure().scope_with(scope)`
4. **Handlers** (`handlers/groups.rs`): pass `&ctx` to service methods (no longer `_ctx`)
5. **Error handling** (`error.rs`): `DomainError::AccessDenied` → HTTP 403

### Target AuthZ flow

```
Request → API Gateway (AuthN) → SecurityContext{tenant=T1}
  → RG Handler(list_groups) → GroupService.list_groups(&ctx, &query)
    → PolicyEnforcer.access_scope(&ctx, RG_GROUP_RESOURCE, "list", None)
      → Static AuthZ Plugin → decision=true, constraints=[In(owner_tenant_id, [T1])]
    → AccessScope{owner_tenant_id IN (T1)}
    → GroupRepository.list_groups(&conn, &scope, &query)
      → SecureORM → WHERE tenant_id IN ('T1')
  → Response: groups from T1 only
```

### Planned Rust integration tests (24 tests)

**`authz_integration_test.rs`** (9 tests — mock AuthZ, no DB):
- `enforcer_tenant_scoping_produces_correct_access_scope` — mock PDP → correct scope
- `enforcer_different_tenants_get_different_scopes` — tenant isolation at scope level
- `enforcer_deny_all_returns_denied_error` — deny flow
- `enforcer_allow_all_no_constraints_returns_allow_all` — unconstrained path
- `enforcer_allow_all_with_required_constraints_fails` — fail-closed
- `enforcer_passes_resource_id_to_pdp` — request params verification
- `enforcer_works_for_all_crud_actions` — all 5 CRUD actions
- `full_chain_list_groups_calls_enforcer_with_correct_params` — capturing mock verifies PDP receives correct params
- `full_chain_deny_all_blocks_list_groups` — deny-all PDP blocks operation

**`tenant_scoping_test.rs`** (10 tests — AccessScope shape, no DB):
- AccessScope construction, isolation, `tenant_only()`, `deny_all()`, `for_resource()`

**`tenant_filtering_db_test.rs`** (5 tests — **full chain with real SQLite DB**):
- `tenant_isolation_list_groups` — two tenants create groups, each sees only own via `WHERE tenant_id IN (...)`
- `tenant_isolation_get_group_cross_tenant_invisible` — cross-tenant `get_group` → not found
- `tenant_isolation_hierarchy_scoped` — hierarchy filtered by tenant scope
- `tenant_isolation_update_cross_tenant_blocked` — cross-tenant `update_group` → blocked
- `tenant_isolation_delete_cross_tenant_blocked` — cross-tenant `delete_group` → blocked; own-tenant delete succeeds

### Planned E2E HTTP tests (9 tests)

**`test_authz_tenant_scoping.py`**:
- `test_create_and_get_type` — type CRUD
- `test_create_and_get_group` — group with tenant_id from SecurityContext
- `test_list_groups_returns_created_groups` — list returns own groups
- `test_group_has_tenant_id_from_security_context` — consistent tenant_id across groups
- `test_child_group_inherits_parent_tenant` — parent-child tenant enforcement
- `test_group_hierarchy_returns_parent_and_children` — hierarchy traversal
- `test_delete_group` — delete + 404 verification
- `test_membership_add_and_list` — membership CRUD
- `test_tenant_isolation_same_token_sees_own_groups` — same-tenant visibility

---

## Phase 2: Group-Based Predicates _(Planned)_

**Goal**: `InGroup` / `InGroupSubtree` predicates compile to SQL subqueries against `resource_group_membership` and `resource_group_closure` tables.

### Target implementation summary

1. **Predicate types** (`authz-resolver-sdk/src/constraints.rs`): add `InGroupPredicate` (group_ids) and `InGroupSubtreePredicate` (ancestor_ids) to `Predicate` enum with serde support (`"op":"in_group"`, `"op":"in_group_subtree"`)

2. **ScopeFilter variants** (`modkit-security/src/access_scope.rs`): `InGroupScopeFilter`, `InGroupSubtreeScopeFilter` carry property + group/ancestor UUIDs. Well-known table constants in `rg_tables` module (`MEMBERSHIP_TABLE`, `CLOSURE_TABLE`, column names)

3. **PEP compiler** (`authz-resolver-sdk/src/pep/compiler.rs`): compiles `InGroup`/`InGroupSubtree` predicates into corresponding `ScopeFilter` variants via `json_to_scope_value`

4. **SecureORM** (`modkit-db/src/secure/cond.rs`): `build_constraint_condition` generates subquery SQL:
   - `InGroup` → `col IN (SELECT resource_id FROM resource_group_membership WHERE group_id IN (...))`
   - `InGroupSubtree` → `col IN (SELECT resource_id FROM resource_group_membership WHERE group_id IN (SELECT descendant_id FROM resource_group_closure WHERE ancestor_id IN (...)))`

### SQL generated (S14 scenario)

```sql
WHERE owner_tenant_id IN ('T1')
  AND id IN (
    SELECT resource_id FROM resource_group_membership
    WHERE group_id IN ('ProjectA-uuid')
  )
```

### Tests

**`constraints.rs`** (3 planned unit tests): serialization roundtrip for InGroup, InGroupSubtree, mixed constraint

**`compiler.rs`** (6 unit tests): InGroup → InGroup filter, InGroupSubtree → InGroupSubtree filter, tenant + InGroup combined, empty In values fail-closed, empty InGroup group_ids fail-closed, empty InGroupSubtree ancestor_ids fail-closed

**`cond.rs`** (3 planned unit tests): InGroup subquery condition, InGroupSubtree nested subquery, tenant + InGroup AND condition

**`tenant_filtering_db_test.rs`** (2 planned DB tests):
- `group_based_in_group_predicate_produces_combined_scope` — mock AuthZ with InGroup + tenant → correct AccessScope with 2 filters
- `group_based_membership_data_correctly_stored` — full S14 data: ProjectA/B, task memberships, verify isolation

### What remains for production use

- **RG-aware AuthZ plugin**: static-authz-plugin currently only returns tenant predicates. A real plugin needs to resolve user→group access from an external policy source and emit `InGroup`/`InGroupSubtree` predicates
- **Domain entity integration**: consuming modules may project `resource_group` + `resource_group_closure` for hierarchy queries. `resource_group_membership` projection should only be added when profiling confirms the two-request pattern (RG Membership API → domain service) causes unacceptable latency — this table is 10×+ larger than other projections. In a monolith with a shared DB, no projections are needed at all. By default, domain services rely on PDP capability degradation: PDP resolves group memberships and returns explicit resource IDs via `in` predicates

---

## Phase 3: MTLS Authentication Mode _(Planned)_

**Goal**: Verify that AuthZ plugin can read RG hierarchy via MTLS-authenticated request (microservice deployment mode), bypassing AuthZ evaluation.

### Target implementation summary

1. **MTLS routing logic** (`auth.rs`): `determine_auth_mode()` should check client CN + endpoint allowlist → `AuthMode::Mtls` or `AuthMode::Jwt`, using `MtlsConfig`, `AllowedEndpoint`, and path pattern matching.

2. **Rust unit tests** (`auth.rs`): 12 tests covering JWT fallback, MTLS allowed/rejected, edge cases (empty CN, PUT to hierarchy, multiple clients/endpoints, DELETE blocked).

3. **E2E test plan** (`test_mtls_auth.py`): 4 tests with `pytest.skip` when cert infrastructure unavailable:
   - `test_mtls_allowed_endpoint_hierarchy_200`
   - `test_mtls_disallowed_endpoint_post_groups_403`
   - `test_jwt_hierarchy_full_authz`
   - `test_mtls_invalid_cert_cn_rejected`

### What remains for production deployment

- Certificate generation (CA + client certs)
- API Gateway TLS termination configuration (forward client CN header)
- E2E test execution requires cert infrastructure (`E2E_MTLS_CERT_DIR`)

#### 3.1 Certificate infrastructure

Generate self-signed certs for dev:

```bash
# CA
openssl req -x509 -newkey rsa:2048 -keyout ca-key.pem -out ca.pem -days 365 -nodes \
  -subj "/CN=rg-mtls-ca"

# AuthZ plugin client cert
openssl req -newkey rsa:2048 -keyout plugin-key.pem -out plugin.csr -nodes \
  -subj "/CN=authz-resolver-plugin"
openssl x509 -req -in plugin.csr -CA ca.pem -CAkey ca-key.pem -out plugin.pem -days 365
```

#### 3.2 RG MTLS configuration

```yaml
modules:
  resource-group:
    config:
      mtls:
        ca_cert: "certs/ca.pem"
        allowed_clients: ["authz-resolver-plugin"]
        allowed_endpoints:
          - method: GET
            path: "/api/resource-group/v1/groups/{group_id}/hierarchy"
```

#### 3.3 API Gateway TLS termination

Configure API Gateway to forward client certificate CN header to RG module for MTLS mode detection.

### Test scenario

```bash
# MTLS request to allowed endpoint (hierarchy) — AuthZ bypassed
curl --cert plugin.pem --key plugin-key.pem --cacert ca.pem \
  https://127.0.0.1:8087/cf/resource-group/v1/groups/{group_id}/hierarchy
# Expected: 200 OK with hierarchy data

# MTLS request to disallowed endpoint (POST groups) — rejected
curl --cert plugin.pem --key plugin-key.pem --cacert ca.pem \
  -X POST https://127.0.0.1:8087/cf/resource-group/v1/groups
# Expected: 403 Forbidden

# JWT request to hierarchy endpoint — full AuthZ applied
curl -H "Authorization: Bearer test" \
  http://127.0.0.1:8087/cf/resource-group/v1/groups/{group_id}/hierarchy
# Expected: 200 OK with AuthZ-scoped results
```

### Verification

- MTLS + allowed endpoint → 200, no AuthZ evaluation in logs
- MTLS + disallowed endpoint → 403
- JWT + same endpoint → 200, AuthZ evaluation logged
- Invalid cert CN → 403

---

## Effort Estimate

| Phase | Scope | Effort | Status |
|-------|-------|--------|--------|
| Phase 1 | Tenant scoping via PolicyEnforcer | 2–3 hours | **Planned** |
| Phase 2 | Group predicates (in_group/in_group_subtree) | 1–2 days | **Planned** |
| Phase 3 | MTLS verification | 2–3 hours | **Planned** |

---

## References

- [RESOURCE_GROUP_MODEL.md](./RESOURCE_GROUP_MODEL.md) — How AuthZ uses RG data
- [AUTHZ_USAGE_SCENARIOS.md](./AUTHZ_USAGE_SCENARIOS.md) — SQL-level scenarios (S14–S21 for groups)
- [RG DESIGN](../../../modules/system/resource-group/docs/DESIGN.md) — RG module design, auth modes, init sequence
- [AuthZ DESIGN](./DESIGN.md) — Core authorization design
- [RG PRD](../../../modules/system/resource-group/docs/PRD.md) — Product requirements
