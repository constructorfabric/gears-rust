<!-- Created: 2026-04-07 by Constructor Tech -->
<!-- Updated: 2026-04-28 by Constructor Tech -->

# E2E Testing Guide

This document defines the philosophy and patterns for end-to-end (E2E) tests across all ToolKit gears. Suites live in `testing/e2e/suites/<suite>/`. How to run them, the `e2e.yaml` format, shared-server versus self-managed suites and the `make` targets are in [`testing/e2e/README.md`](../../testing/e2e/README.md); CI lanes are in [15 § 5](15_testing_and_ci.md#5-end-to-end-e2e-tests).

---

## Philosophy

### What Is an E2E Test in This Project

An E2E test is an HTTP request to a **running** `cf-gears-example-server` with a **real PostgreSQL** database, traversing the full chain: TCP → HTTP router → AuthN middleware → AuthZ PolicyEnforcer → Service → Repository → PostgreSQL → Response serialization → HTTP response. Pytest sends requests from the outside, exactly like a real client.

That is the target definition, not the default setup: the shared `make e2e-local` config, [`config/e2e-local.yaml`](../../config/e2e-local.yaml), puts most gears on SQLite. See [Check which venue you actually have](#coverage-goal-one-call-per-api-method) before counting a case as PostgreSQL coverage.

This is **not** "another way to verify business logic." Business logic is verified by unit/integration tests (see [`12_unit_testing.md`](12_unit_testing.md)). E2E tests verify the **seams between components**, not the components themselves.

### Scope: Single Gear vs. Cross-Gear

E2E tests live at two levels, and both matter:

**Single-gear E2E** — verifies that the gear's own integration seams work end-to-end: routing, JSON wire format, real AuthZ wiring, PostgreSQL-specific SQL. Each gear has its own suite under `testing/e2e/suites/<gear>/`. These tests are the baseline.

**Cross-gear E2E** — verifies that **2–5 gears work correctly together**. This is the primary reason E2E tests exist at all. Unit tests verify each gear in isolation (mocking its dependencies). Only an E2E test can catch bugs that appear at the boundary between gears: gear A calls gear B's SDK, gear B reads from a table that gear C seeded, the combined result flows through gear D's API. None of these seams are visible to any single gear's unit tests.

There is no separate cross-gear directory. A cross-gear scenario is either its own suite under `testing/e2e/suites/` (for example `scope_enforcement`, which overlays api-gateway route policies on the shared config) or a profile of an existing suite (`resource_group`'s `tr-authz` profile runs the AuthZ → tenant-resolver → resource-group chain; `make e2e-tr-authz`).

The rule still applies: **each cross-gear test targets exactly one integration seam** between the gears. Do not write a cross-gear test that simultaneously verifies intra-gear behavior — that belongs in each gear's own unit tests.

### Three Questions Before Adding a Test

Every E2E test must pass all three:

1. **"Can this bug only manifest during real HTTP interaction?"**
   If yes — it's an E2E test. If the bug is catchable by calling a Rust function directly — it's a unit test and does not belong here.

2. **"Can this bug only manifest on PostgreSQL but not on SQLite?"**
   FK constraints, SERIALIZABLE transactions, domain types, JSONB behavior — all differ from SQLite. If the bug depends on the DB dialect — it's an E2E test.

3. **"If we remove this test, does integration confidence decrease?"**
   If not — the test is unnecessary. A test that duplicates unit coverage adds no confidence — it adds execution time and flake surface.

### Integration Seams to Test

E2E tests cover integration seams — points where two independently correct components can break when connected. Common seams:

| Seam | What breaks between components | Why unit tests are blind |
|------|-------------------------------|-------------------------|
| **Handler ↔ JSON wire** | `#[serde(rename)]` typo, missing field, camelCase mismatch | Unit tests operate on Rust structs, not JSON bytes |
| **Gear init ↔ AuthZ** | `PolicyEnforcer` not created, `AccessScope` not passed to service | Unit tests mock PolicyEnforcer; real wiring only exists in `gear.rs` |
| **Service ↔ PostgreSQL** | FK enforcement, SERIALIZABLE isolation, domain types | Unit tests run on SQLite — different FK behavior, no domain types |
| **Error handler ↔ HTTP** | `Content-Type: application/problem+json` not set, stack trace leaked | Unit tests assert `DomainError` variant, not HTTP headers |
| **Cursor codec ↔ HTTP** | Base64 encode/decode roundtrip, URL-encoding, offset drift | Unit tests test pagination logic; codec only runs in the handler layer |
| **OData filter ↔ SQL** | Full parse→SQL→result chain | Unit tests verify FilterField names/kinds, not the full pipeline |

> **Note on routing**: route registration can be tested in unit tests with axum's `Router::oneshot()` — the router is instantiated in-process, no real server needed. A route smoke test (`assert non-405 for every registered path`) belongs in the gear's REST unit tests, not in E2E. E2E adds value here only if you need to verify routing behavior that depends on real server middleware or TLS termination.

Each E2E test targets **exactly one seam**. If the seam is already covered by a unit test, there is no E2E test for it.

### What NOT to Test via E2E

- **Domain validation** (field format, invariants, placement rules) — pure logic, deterministic; unit tests cover this
- **Error variant construction and mapping** — `DomainError` variants and their `CanonicalError` mapping; unit tests cover this
- **DTO struct↔struct conversions** — unit tests cover this
- **OData filter field names and kinds** — unit tests cover this
- **Seeding idempotency** — pure domain logic; unit tests cover this
- **AccessScope construction** — unit tests cover this
- **Service-level PATCH logic** — unit tests cover partial update logic

All of the above is **deterministic logic** that works identically whether called via HTTP or called directly. Running it through HTTP increases execution time and brittleness without adding integration confidence.

---

## E2E Test Requirements

### Coverage Goal: One Call Per API Method

Every important API endpoint should be called **at most once** across the entire E2E suite in a **positive (happy-path) scenario**. Negative scenarios are rare exceptions — only when the negative behavior is a genuine integration seam (e.g., real FK constraint on PostgreSQL, real AuthZ denial with a live token) that cannot be caught by unit tests.

**Corner case and edge-case coverage in E2E is only acceptable if it could not be achieved in unit tests.** Before adding an E2E test for an edge case, verify that the same scenario cannot be written as a unit test against SQLite or a mock. If it can — it goes there, not here.

**Divergence between SQLite and PostgreSQL is on its own sufficient grounds for a PostgreSQL venue**, and is the most common reason a case legitimately cannot move down a tier. Which of the two venues below it selects depends on the seam the case actually needs — HTTP E2E when it needs the transport, a direct Rust integration suite when the divergence is in the SQL or the schema. Numeric column widths (`SMALLINT` vs `INTEGER` range and comparison), `CHECK` constraint enforcement, FK actions, domain types, isolation levels and type affinity all behave differently, and SQLite's dynamic typing hides mismatches rather than surfacing them. If you can name the way the two backends differ, the test belongs here.

That permission does not loosen the restraint above it. E2E carries far fewer tests than the unit suite by design, and every case has to be justified by what a unit test cannot reach; where two cases would cover the same ground, the coverage checklist below is what removes the duplicate. "It is database-related" is not the criterion — "it behaves differently on PostgreSQL, and here is how" is.

**Check which venue you actually have.** The definition above is normative: a suite is only an E2E suite if it runs against real PostgreSQL. A pytest harness that seeds SQLite directly and drives HTTP over it is a useful integration suite, but it **cannot discharge any PostgreSQL-specific claim** — the very divergences listed above are the ones it cannot see. So confirm before routing, and know that there are two venues, not one:

- **This suite**, when it genuinely runs against PostgreSQL. Use it when the case needs the full HTTP chain as well as the real database.
- **A feature-gated Rust suite against real PostgreSQL via `testcontainers`**, calling repository or service code directly — in-process, no HTTP, no running server. Use it when the divergence is in the SQL or the schema rather than in the transport, which is the common case: dialect rejections, column widths, `CHECK` and FK enforcement, isolation. It gives a far more precise diagnosis than an HTTP status can, and it costs seconds rather than a server boot.

The second is in use, not hypothetical. The established wiring is a `--features integration` suite behind `#![cfg(feature = "integration")]`, a lane in `INTEGRATION_LANES` in [`gears-rust.mk`](../../gears-rust.mk) (run with `make test-integration LANE=<lane>`), and a matching step in `.github/workflows/ci.yml`'s `integration` job. `resource-group` (lane `rg-pg`) is the worked example, and it writes the deviation from this guide down in its own docs rather than leaving it implicit — do the same if you adopt the pattern.

Only when neither venue exists is the behaviour genuinely untested, and that is what belongs in the backlog rather than in an assumption.

After adding or removing any E2E test, **check the coverage checklist**: verify which HTTP methods (GET, POST, PUT, PATCH, DELETE) are called across all tests in the gear suite. If a method is already exercised in test A, test B does not need to call it again. Remove redundant calls without hesitation.

### Priority Order (in case of conflict)

**Priority 1 — Stability (no flaking).** This is the single most important requirement. An E2E test that occasionally fails without a code change is worse than no test at all — it erodes trust in the entire suite and causes teams to ignore real failures. A flaky test must be fixed or deleted immediately. Zero tolerance.

The rules that protect stability are in [Anti-Flaking Practices](#anti-flaking-practices): own data, no `sleep`, hard timeouts, order independence, skip when the server is down.

**Priority 2 — Speed.** A standard CRUD E2E test (create + read + assert) should complete in **under 2 seconds** running single-threaded against a local server. This is an important goal, but it is secondary to stability. A test that takes 3 seconds and never flakes is better than a test that takes 1 second and flakes 1-in-50 runs.

Tactics: see [How Many Requests Per Test](#how-many-requests-per-test-the-tradeoff).

**Priority 3 — Positive scenarios only, each API called once.** Test the happy path. Verify that the integration seam works end-to-end for the normal case. If `POST /entities` is already called in `test_dto_roundtrip`, `test_authz_tenant_filter` does not need to create another entity via a fresh POST — it can reuse a fixture or accept that `POST` is already covered. Keep the suite minimal: fewer calls, fewer points of failure, faster runtime.

---

## Anti-Flaking Practices

These are the stability rules for every suite. Other sections link here instead of repeating them.

### 1. Data Isolation — the Foundation

**Every test must own its data.** A test that reads data created by another test will fail non-deterministically as soon as test ordering changes or parallelism is introduced.

```python
# BAD — depends on data from another test or a shared fixture
async def test_list_returns_entities(client):
    r = await client.get("/cf/<gear>/v1/entities")
    assert len(r.json()["items"]) > 0  # relies on someone else creating data

# GOOD — test creates its own data, asserts on it specifically
async def test_list_returns_entities(client, create_entity):
    entity = await create_entity(name=f"test-{uuid.uuid4()}")
    r = await client.get("/cf/<gear>/v1/entities")
    ids = [i["id"] for i in r.json()["items"]]
    assert entity["id"] in ids
```

Rules:
- Use **unique identifiers** (UUID, timestamp counter) in all created entity names/codes — prevents collisions across parallel workers and re-runs
- **Do not share mutable fixtures** between tests — factory fixtures must be function-scoped, not session-scoped
- **Do not clean up** between tests — prefer unique data per run over delete-after. Cleanup logic itself can fail and cause false negatives

### 2. No `time.sleep()` — Ever

A fixed sleep either waits too little (race condition) or too long (wasted time, and still occasionally too little under load).

```python
# BAD
await asyncio.sleep(0.5)  # hope the server has processed it by now
r = await client.get(f"/cf/.../entities/{entity_id}")

# GOOD — if you genuinely need to wait for async state, poll with a timeout
async def wait_for_status(client, url, expected_status, timeout=5.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        r = await client.get(url)
        if r.status_code == expected_status:
            return r
        await asyncio.sleep(0.05)
    pytest.fail(f"Timed out waiting for {expected_status} at {url}")
```

In practice, if your test needs `time.sleep()`, it usually means the test is exercising an async/eventual-consistency seam that should be redesigned — either make the operation synchronous from the API perspective, or accept that this is an E2E anti-pattern and move the check elsewhere.

### 3. Hard Timeouts — Fail Fast, Never Hang

A hanging test is worse than a failing test: it blocks CI, wastes wall-clock time, and obscures the real failure.

[`testing/e2e/pytest.ini`](../../testing/e2e/pytest.ini) sets `timeout = 10` (per-test hard kill via `pytest-timeout`). Give every HTTP client a per-request timeout of 5s:

```python
# every httpx call
REQUEST_TIMEOUT = 5.0
async with httpx.AsyncClient(timeout=REQUEST_TIMEOUT) as client:
    r = await client.get(url)
```

If a test exceeds 10s, it is **broken**, not slow. Investigate and fix — do not raise the timeout.

### 4. Test Order Independence

Tests must pass in any order. A test that only fails when run after another test has a hidden dependency that must be eliminated. CI does not shuffle test order, so check this locally (for example by running the failing test alone with `pytest -k`).

### 5. Environment Readiness — Skip, Don't Fail

If the server is not up, do not generate N individual test failures. Detect once and skip the entire gear.

```python
# conftest.py
@pytest.fixture(scope="session", autouse=True)
async def check_server_reachable(client):
    try:
        r = await client.get("/health", timeout=3.0)
        r.raise_for_status()
    except Exception as e:
        pytest.skip(f"Server not reachable, skipping gear: {e}")
```

### 6. Timezone and Time Agnosticism

Do not assert on absolute timestamps or assume a specific timezone. Assert on relative properties instead.

```python
# BAD
assert data["created_at"].startswith("2026-04-")

# GOOD
from datetime import datetime, timezone
created = datetime.fromisoformat(data["created_at"])
assert created <= datetime.now(timezone.utc)
assert created > datetime.now(timezone.utc).replace(year=2020)  # sanity bound
```

### 7. Idempotent Factory Fixtures

Factory fixtures must be safe to call multiple times and must not fail if the entity already exists (e.g., from a previous interrupted run).

```python
@pytest.fixture
def make_entity(client):
    async def _create(**kwargs):
        kwargs.setdefault("name", f"test-{uuid.uuid4()}")
        r = await client.post("/cf/.../entities", json=kwargs)
        assert r.status_code == 201, r.text
        return r.json()
    return _create
```

### 8. Minimal HTTP Round-Trips Per Test

Each HTTP call is another chance for jitter or a timeout. See [How Many Requests Per Test](#how-many-requests-per-test-the-tradeoff).

### 9. Quarantine, Don't Ignore

When a flaky test is found: quarantine it immediately (mark with `@pytest.mark.skip(reason="flaky: #ticket")`) and file a ticket. Do not leave it in the suite — a flaky test that is "usually green" will eventually mask a real failure.

Do not add automatic reruns. Retrying a flaky test hides the root cause and doubles the execution time for that test.

---

## Test Infrastructure

### File Layout

```
testing/e2e/suites/<suite_name>/
├── e2e.yaml                      ← suite manifest: features, launcher, config overrides (testing/e2e/README.md)
├── conftest.py                   ← helpers, factory fixtures
├── test_authz_tenant_scoping.py  ← AuthZ + tenant isolation seams (if applicable)
├── test_mtls_auth.py             ← MTLS certificate verification (if applicable; p2 — deferred, not implemented yet)
├── test_integration_seams.py     ← Core integration seam tests (per gear)
```

Keep the number of files small. A single `test_integration_seams.py` per gear is preferred over splitting into many files.

### Dependencies and pytest configuration

Dependencies are listed in [`testing/e2e/requirements.txt`](../../testing/e2e/requirements.txt) (`pytest`, `httpx`, `pytest-asyncio`, `pytest-timeout`, plus suite-specific packages). Do not add a test plugin without adding it there.

[`testing/e2e/pytest.ini`](../../testing/e2e/pytest.ini) sets:

- `asyncio_mode = auto` — every `async def test_*` runs without `@pytest.mark.asyncio`.
- `timeout = 10` — per-test hard timeout via `pytest-timeout`.
- the `smoke` marker.

### Shared Helpers (conftest.py)

```python
REQUEST_TIMEOUT = 5.0  # every httpx call uses this

async def assert_response_shape(data: dict, required_fields: list[str]):
    """Verify JSON wire format contains required fields with correct types."""
    for field in required_fields:
        assert field in data, f"Missing field: {field}"
```

Gear-specific helpers (e.g., `assert_group_shape`, `create_type_fixture`) live in the gear's `conftest.py`.

---

## What to Assert in E2E Tests

### Assert Beyond the Status Code

A response code alone is almost never enough. Every E2E assertion should cover three layers:

**1. HTTP status code** — the minimum. Always assert it explicitly with a helpful message:
```python
assert r.status_code == 201, f"Expected 201, got {r.status_code}: {r.text}"
```

**2. Important headers** — headers are part of the contract and frequently broken by middleware changes:
```python
# Content-Type must match what clients expect
assert "application/json" in r.headers["content-type"]

# For error responses — RFC 9457 contract
assert "application/problem+json" in r.headers["content-type"]

# For created resources — Location header if applicable
assert "location" in r.headers
```

**3. Response body fields** — assert the fields that matter for the seam being tested:
```python
data = r.json()

# Primary fields — the core of what this test verifies
assert data["id"] is not None
assert data["name"] == "Expected Name"

# Secondary fields — verify the response is complete and correct
assert "created_at" in data
assert data["tenant_id"] == expected_tenant_id

# Absence of internal fields — important for security
assert "internal_surrogate_id" not in data
assert "password_hash" not in data
```

Secondary fields are worth checking even if they are not the primary focus of the test — they catch regressions in serialization, field renaming, and response shape changes.

### How Many Requests Per Test: The Tradeoff

Keep the number of HTTP requests per test as small as possible. Each extra request is a potential source of flakiness and adds to execution time. The default should be: **write → assert the write response → done**.

However, some operations require verification requests to be meaningful. The rule is: **think about what this test must prove, then add only the requests needed to prove it**.

**Example: a "move resource" test**

A move operation changes where a resource lives. The write response returning `200` only proves the server accepted the request. It does not prove the move actually worked. To prove the move:

```python
# Step 1 — create resource at original location
r = await client.post("/entities", json={"parent_id": parent_a})
assert r.status_code == 201
entity_id = r.json()["id"]

# Step 2 — move to new location
r = await client.put(f"/entities/{entity_id}", json={"parent_id": parent_b})
assert r.status_code == 200

# Step 3 — verify: accessible at new location (this proves the move worked)
r = await client.get(f"/entities/{entity_id}")
assert r.status_code == 200
assert r.json()["parent_id"] == parent_b

# Step 4 — verify: old parent no longer lists this child (proves the old link was removed)
r = await client.get(f"/entities/{parent_a}/children")
assert r.status_code == 200
child_ids = [c["id"] for c in r.json()["items"]]
assert entity_id not in child_ids
```

Steps 3 and 4 are extra requests, but they are the difference between a test that proves the move worked and a test that only proves the server didn't crash. They are worth it.

**When extra requests are NOT worth it:**

- A `POST` returns the created entity — do not `GET` it again just to verify the same fields
- A `DELETE` returns `204` — do not `GET` the deleted resource unless the specific seam being tested is "deleted resources return 404"
- A list result already contains the item — do not make a separate `GET /entities/{id}`

The guiding question: *"If I remove this request, does the test still prove the seam works?"* If yes — remove it. If no — keep it.

---

## Core Test Patterns

### Route Smoke Test

**Purpose**: Verify that all endpoints are registered at correct method + path on a real server.

**Why not in unit tests**: Unit tests call service methods directly. If a handler is not registered in `gear.rs`, or mounted on the wrong path, unit tests pass but the API is broken.

```python
async def test_route_smoke_all_endpoints(client):
    """
    Seam: Route registration — handlers mounted on correct method + path.
    """
    # Each path returns something other than 404/405
    responses = await asyncio.gather(
        client.head("/cf/<gear>/v1/entities"),
        client.options("/cf/<gear>/v1/entities"),
    )
    for r in responses:
        assert r.status_code not in (404, 405), f"Endpoint not registered: {r.url}"
```

### DTO JSON Shape Test

**Purpose**: Verify that JSON field names, types, and presence match the OpenAPI contract.

**Why not in unit tests**: Unit tests verify `From<DomainModel>` for DTO structs (Rust struct conversion). They do NOT test the JSON wire format: `#[serde(rename)]` typos, `#[serde(skip_serializing_if)]` behavior, camelCase conventions. A serde attribute typo passes unit tests but breaks clients.

```python
async def test_dto_roundtrip_json_shape(client, create_entity):
    entity = await create_entity(name="Shape Test")
    r = await client.get(f"/cf/<gear>/v1/entities/{entity['id']}")
    assert r.status_code == 200
    data = r.json()
    # Assert exact field names, not just "response is 200"
    assert isinstance(data["id"], str)
    assert "name" in data
    assert "created_at" in data
    # Assert NO unexpected internal fields
    assert "internal_surrogate_id" not in data
```

### AuthZ Tenant Filter Test

**Purpose**: Verify the full AuthZ pipeline wiring — SecurityContext → PolicyEnforcer → AccessScope → `WHERE tenant_id IN (...)`.

**Why not in unit tests**: Unit tests mock the PDP and check that `access_scope()` returns the correct scope. They do NOT verify the **real wiring** in `gear.rs` where PolicyEnforcer is created from ClientHub and injected into the service.

```python
async def test_authz_tenant_filter_applied(client):
    """
    Seam: AuthZ → SecureORM full chain — own data visible, scoped correctly.
    """
    r = await client.post("/cf/<gear>/v1/entities", json={"name": "AuthZ Test"})
    assert r.status_code == 201
    entity_id = r.json()["id"]

    r = await client.get("/cf/<gear>/v1/entities")
    assert r.status_code == 200
    ids = [item["id"] for item in r.json()["items"]]
    assert entity_id in ids
```

### Error Response RFC 9457 Test

**Purpose**: Verify that the real server middleware chain produces `application/problem+json` responses with correct format.

**Why not in unit tests**: Unit tests verify the `DomainError → CanonicalError` mapping in isolation. They do NOT verify that the real server sets the correct `Content-Type` header in HTTP responses.

```python
async def test_error_response_rfc9457(client):
    """
    Seam: Error middleware — DomainError → HTTP status + Content-Type + no internal leaks.
    """
    import uuid
    r = await client.get(f"/cf/<gear>/v1/entities/{uuid.uuid4()}")
    assert r.status_code == 404
    assert "application/problem+json" in r.headers.get("content-type", "")
    body = r.json()
    assert body.get("status") == 404
    assert "stack" not in body
    assert "trace" not in body
    assert "backtrace" not in body
```

### Cursor Pagination Test

**Purpose**: Verify that the cursor encode/decode roundtrip works over HTTP.

**Why not in unit tests**: Unit tests test `Page<T>` construction and `PageInfo` fields. They do NOT test the cursor codec: base64 encoding, URL-safe encoding, offset drift across pages.

```python
async def test_pagination_cursor_roundtrip(client, create_entities):
    await create_entities(count=5)

    all_ids = []
    cursor = None
    while True:
        params = {"limit": 2}
        if cursor:
            params["cursor"] = cursor
        r = await client.get("/cf/<gear>/v1/entities", params=params)
        assert r.status_code == 200
        page = r.json()
        all_ids.extend(item["id"] for item in page["items"])
        cursor = page["page_info"].get("next_cursor")
        if not cursor:
            break

    assert len(all_ids) == len(set(all_ids)), "Duplicate items across pages"
    assert len(all_ids) >= 5, "Missing items across pages"
```

---

## Optional Test Suites

Some E2E tests are conditional on infrastructure availability:

| Suite | Skip condition | Description |
|-------|---------------|-------------|
| **Cross-tenant isolation** | never skipped; `E2E_AUTH_TOKEN_TENANT_B` defaults to `e2e-token-tenant-b` (`config/e2e-local.yaml`) | Tests requiring two real tokens from different tenants |
| **MTLS** _(p2 — deferred, not implemented yet)_ | `E2E_MTLS_CERT_DIR` not set | Certificate-based authentication verification |

All core integration seam tests run with a single token, no special infrastructure.

---

## Anti-Patterns (DO NOT Test Here)

| Don't test | Why | Where it's covered |
|---|---|---|
| Domain validation (field format, invariant) | Pure domain logic, deterministic | Unit tests: service-level |
| Error variant construction | Pure error mapping | Unit tests: `error_tests.rs` beside the error module |
| OData filter field names/kinds | Static mapping correctness | Unit tests: `*_tests.rs` beside the filter schema |
| DTO `From` impl correctness | Struct conversion | Unit tests: `dto_tests.rs` |
| Seeding idempotency | Domain logic, no HTTP | Unit tests: service-level |
| AccessScope construction | Pure logic | Unit tests: service-level, mock PDP |
| Service-level PATCH logic | Domain logic | Unit tests: service-level |
| Individual `DomainError` variants | Error construction + mapping | Unit tests: `error_tests.rs` |

A test that can be removed without reducing integration confidence should not exist.

---

## Acceptance Criteria (gear E2E suite)

**Suite-level:**
- Core tests in a single file `test_integration_seams.py`
- Total suite runtime < 15 seconds (excluding optional suites)
- Zero flakes on 10 consecutive runs of the suite
- Follows [Anti-Flaking Practices](#anti-flaking-practices) (timeouts, no sleep, own data)

**Per-test quality:**
- Each test targets exactly one integration seam (documented in test docstring)
- Route smoke test requires no data setup — fastest possible
- DTO roundtrip test verifies exact JSON key names, not just "response is 200"
- PostgreSQL-specific tests (closure SQL, FK cascade) verify state values, not just "returns 200"
- Error format test checks `Content-Type: application/problem+json` header
- Pagination test asserts no duplicates AND no missing items across pages

**Isolation:**
- Optional tests (cross-tenant, MTLS — the latter is `p2`, deferred / not implemented yet) skip gracefully when infrastructure unavailable
- No test duplicates unit test domain logic — if removing the test doesn't reduce integration confidence, the test shouldn't exist

See also: [`14_db_behavior_testing.md`](14_db_behavior_testing.md) — concurrency correctness under the `Service ↔ PostgreSQL` seam (transaction boundaries, retry firing, invariant survival under concurrent writers) is deliberately out of E2E's stability-first scope and lives there instead.
