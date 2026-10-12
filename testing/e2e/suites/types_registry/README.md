# Types Registry e2e suite

The suite covers asynchronous admission (`202` + `GET /operations/{id}`), exact and batch reads, and discovery through numbered scenarios. The [legacy suite](legacy/README.md) covers the original synchronous v1 API (`200` + a `results` array) and has no reserved scenario IDs.

## Scenario index

Every section in `scenarios/` is listed below. `Scenario ID range` reserves IDs for the section, including unused numbers; it does not report implemented test counts. IDs increase consecutively within each section; unused numbers in its range are reserved for future scenarios.

Every scenario has a test. Scenarios proved by one workflow share a test function, as each scenario document states. Refusal to restore or reuse a tombstone is covered by TR-REG-305.

| # | Scenario Group | Scenario ID range | Test file | Comment |
|---|---|---|---|---|
| 1 | [Successful registration](scenarios/registration.md#successful-registration) | `TR-REG-001`–`TR-REG-099` | [test_registration_success.py](test_registration_success.py) | Implemented |
| 2 | [Partial success and refusals](scenarios/registration.md#partial-success-and-refusals) | `TR-REG-101`–`TR-REG-199` | [test_registration_refusals.py](test_registration_refusals.py) | Implemented |
| 3 | [Request identity and idempotency](scenarios/registration.md#request-identity-and-idempotency) | `TR-REG-201`–`TR-REG-299` | [test_registration_idempotency.py](test_registration_idempotency.py) | Implemented; TR-REG-207 runs in the TR-REG-701 test |
| 4 | [Content revisions and optimistic preconditions](scenarios/registration.md#content-revisions-and-optimistic-preconditions) | `TR-REG-301`–`TR-REG-399` | [test_registration_revisions.py](test_registration_revisions.py) | Implemented |
| 5 | [Dependency graph and partial admission](scenarios/registration.md#dependency-graph-and-partial-admission) | `TR-REG-401`–`TR-REG-499` | [test_registration_graph.py](test_registration_graph.py) | Implemented |
| 6 | [Compatibility and dependent safety](scenarios/registration.md#compatibility-and-dependent-safety) | `TR-REG-501`–`TR-REG-599` | [test_registration_compatibility.py](test_registration_compatibility.py) | Implemented |
| 7 | [Minor versions and version-family shape](scenarios/registration.md#minor-versions-and-version-family-shape) | `TR-REG-601`–`TR-REG-699` | [test_registration_versions.py](test_registration_versions.py) | Implemented |
| 8 | [Dry-run admission](scenarios/registration.md#dry-run-admission) | `TR-REG-701`–`TR-REG-799` | [test_registration_dry_run.py](test_registration_dry_run.py) | Implemented; TR-REG-706 runs in the TR-REG-405 test |
| 9 | [Managed schema dialect and unstable major-zero profile](scenarios/registration.md#managed-schema-dialect-and-unstable-major-zero-profile) | `TR-REG-801`–`TR-REG-899` | [test_registration_schema_profile.py](test_registration_schema_profile.py) | Implemented |
| 10 | [Deployment policy and compatibility waiver](scenarios/registration.md#deployment-policy-and-compatibility-waiver) | `TR-REG-901`–`TR-REG-999` | [test_registration_deployment_policy.py](test_registration_deployment_policy.py) | Implemented; TR-REG-902 requires the force-disabled profile |
| 11 | [Successful deletion](scenarios/deletion.md#successful-deletion) | `TR-DEL-001`–`TR-DEL-099` | [test_deletion_success.py](test_deletion_success.py) | Implemented |
| 12 | [Preconditions and refusals](scenarios/deletion.md#preconditions-and-refusals) | `TR-DEL-101`–`TR-DEL-199` | [test_deletion_refusals.py](test_deletion_refusals.py) | Implemented |
| 13 | [Dependency graph and partial deletion](scenarios/deletion.md#dependency-graph-and-partial-deletion) | `TR-DEL-201`–`TR-DEL-299` | [test_deletion_graph.py](test_deletion_graph.py) | Implemented; TR-DEL-208 runs in the TR-DEL-403 test, TR-DEL-209 in TR-DEL-302 |
| 14 | [Request identity and idempotency](scenarios/deletion.md#request-identity-and-idempotency) | `TR-DEL-301`–`TR-DEL-399` | [test_deletion_idempotency.py](test_deletion_idempotency.py) | Implemented |
| 15 | [Dry-run deletion](scenarios/deletion.md#dry-run-deletion) | `TR-DEL-401`–`TR-DEL-499` | [test_deletion_dry_run.py](test_deletion_dry_run.py) | Implemented; TR-DEL-404 runs in the TR-DEL-201 test |
| 16 | [Tombstones and subsequent use](scenarios/deletion.md#tombstones-and-subsequent-use) | `TR-DEL-501`–`TR-DEL-599` | [test_deletion_lifecycle.py](test_deletion_lifecycle.py) | Implemented |
| 17 | [Version-family lifecycle](scenarios/deletion.md#version-family-lifecycle) | `TR-DEL-601`–`TR-DEL-699` | [test_deletion_versions.py](test_deletion_versions.py) | Implemented |
| 18 | [Exact read](scenarios/reading.md#exact-read) | `TR-READ-001`–`TR-READ-099` | [test_reading.py](test_reading.py) | Implemented |
| 19 | [Batch read](scenarios/reading.md#batch-read) | `TR-READ-101`–`TR-READ-199` | [test_reading.py](test_reading.py) | Implemented |
| 20 | [Consistency across read routes](scenarios/reading.md#consistency-across-read-routes) | `TR-READ-201`–`TR-READ-299` | [test_reading.py](test_reading.py) | Implemented |
| 21 | [Conditional reads](scenarios/reading.md#conditional-reads) | `TR-READ-301`–`TR-READ-399` | [test_reading.py](test_reading.py) | Implemented |
| 22 | [Paged discovery and returned fields](scenarios/discovery.md#paged-discovery-and-returned-fields) | `TR-DISC-001`–`TR-DISC-099` | [test_discovery.py](test_discovery.py) | Implemented |
| 23 | [Filters](scenarios/discovery.md#filters) | `TR-DISC-101`–`TR-DISC-199` | [test_discovery.py](test_discovery.py) | Implemented |
| 24 | [Cursor continuation](scenarios/discovery.md#cursor-continuation) | `TR-DISC-201`–`TR-DISC-299` | [test_discovery.py](test_discovery.py) | Implemented |
| 25 | [Read discovered entities with batchGet](scenarios/discovery.md#read-discovered-entities-with-batchget) | `TR-DISC-301`–`TR-DISC-399` | [test_discovery.py](test_discovery.py) | Implemented |

The shared base config (`config/e2e-local.yaml`) enables compatibility force and allows vendor `acme` only under `gts.acme.e2e.*`, so the unscoped `make e2e-local` server runs TR-REG-903 and TR-REG-904 too. TR-REG-902 uses the force-disabled profile; every other run skips it.

`conftest.py` provides HTTP/fixture setup, `given_registered` and scenario-ID binding; `helpers.py` provides submit-and-poll and read helpers, expected-body builders and comparators. Pytest collects both sets.

## Test scope

E2E tests serve as executable documentation of the registry's client contract. Write an e2e scenario when it:

- demonstrates a client workflow, such as discovery followed by reading returned references, or reading again after a completed mutation;
- verifies an HTTP contract through the running server, such as conditional reads with ETag and a bodyless `304`;
- explains a rule that affects client behavior, such as tombstones remaining readable or a dependency refresh changing a derived schema's validator.

Keep representative success and failure cases. Limited overlap with Rust tests and repeated endpoint calls are intentional exceptions to the [general E2E guide](../../../../docs/toolkit_unified_system/13_e2e_testing.md): they make complete client workflows readable and verifiable.

Leave exhaustive parameter combinations, malformed inputs, numeric boundaries, and token encoding to Rust unit and integration tests. SQL behavior, query counts, snapshot consistency and races also belong there, where they can be observed and controlled directly. Backend-specific guarantees require tests on those databases; the local HTTP launcher uses SQLite.

## Execution rules

- `registry_api` defaults to `v2` today. Set `TYPES_REGISTRY_API_VERSION` explicitly when running against another version; there is no fallback.
- Each test gets a fresh `cf.e2e.r<uuid>.` namespace. Fixture loaders rewrite IDs and `$ref` targets, not schema constraints or values. TR-REG-901 and TR-REG-904 change the vendor and package after loading to exercise the configured policy region.
- Submissions use a fresh `Idempotency-Key` by default. Replay and conflict scenarios intentionally reuse the key they are testing.
- Registration outcomes match by GTS ID. Deletion outcomes preserve request order and use `assert_operation(..., ordered=True)`.
- Read responses are compared whole. `assert_exact` compares `{"status", "etag", "body"}` as one JSON value; a `304` has no `body`. An expected `"<etag>"` stands for a validated ETag that is none of the known ones, so a changed ETag is part of the expectation. `assert_batch` matches results by echoed key, because batch order is not contractual. `walk` follows every cursor and `assert_pages` compares the whole walk.
- Only unpredictable values are masked, after validation: operation IDs, receipt status, timestamps, new ETags, cursors, implementation versions, trace IDs and non-contractual messages.
- Tests contain complete expected bodies; scenario docs state intent.

## Run

From the repository root, with the existing Python environment:

```sh
# Whole suite.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry

# Through make.
make e2e-local SUITE=types-registry

# Scenario tests only.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry -- -m scenario

# Disabled-force refusal (TR-REG-902).
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry --profile force-disabled -- -k test_disabled_force_rejects_commit_and_dry_run

# Legacy tests only.
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry -- -m "not scenario"

# Collection only; verifies scenario-ID bindings.
.venv/bin/python -m pytest testing/e2e/suites/types_registry --collect-only
```

Select subsets by marker: arguments after `--` are appended after the suite path. Scenario tests carry the `scenario` marker; legacy tests do not.

The local launcher serves authenticated v2 directly; no edge gateway is required.
