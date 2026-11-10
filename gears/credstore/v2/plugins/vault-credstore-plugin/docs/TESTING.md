Created:  2026-10-03 by Constructor Tech
Updated:  2026-10-03 by Constructor Tech

# Testing Strategy — Vault CredStore Plugin

> **Status: implemented.** The suite is **206 tests** — 168 unit tests that run in
> every `cargo test`, and 38 Docker-backed integration tests (21 SDK conformance
> checks and 17 Vault behaviour tests) that are `#[ignore]`d, are run by hand and
> never run in CI. This is the test plan for `cf-gears-vault-credstore-plugin-v2`,
> paired with [DESIGN.md](./DESIGN.md).

> **Companion documents:**
> - [PRD.md](./PRD.md) — requirements the tests verify
> - [DESIGN.md](./DESIGN.md) — the KV v2 mapping, the error table and the operator obligations
> - [`credstore_sdk::conformance`](../../../credstore-sdk/src/conformance.rs) — the backend-contract suite this plugin runs

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Layer 1 — Unit Tests (in-crate)](#2-layer-1--unit-tests-in-crate)
- [3. Layer 2 — Conformance Suite (Docker, manual)](#3-layer-2--conformance-suite-docker-manual)
- [4. Layer 3 — Vault Behaviour Tests (Docker, manual)](#4-layer-3--vault-behaviour-tests-docker-manual)
  - [4.1 Container Setup](#41-container-setup)
  - [4.2 Scenarios](#42-scenarios)
- [5. Running the Tests](#5-running-the-tests)
- [6. Static Analysis](#6-static-analysis)
- [7. Cadence and Qualification](#7-cadence-and-qualification)
- [8. Coverage Gaps and Follow-ups](#8-coverage-gaps-and-follow-ups)

<!-- /toc -->

## 1. Overview

```
L3  Vault behaviour tests  (hashicorp/vault in Docker)        — manual, #[ignore]
L2  SDK conformance suite  (hashicorp/vault in Docker)        — manual, #[ignore]
L1  Unit tests             (scripted transport, HTTP mock)    — every PR, sub-second
```

Layers 2 and 3 exist because what the plugin must get right is mostly **what
Vault really answers**: the status and body for a soft-deleted, a destroyed, an
evicted and an absent version, what `version=0` means, how a missing mount, a
revoked token or a sealed server look. Unit tests with mocks can only repeat
what the author believes; the integration tests pin the beliefs to a real
server, so a Vault release that changes an answer shows up as a failing test
instead of a wrong secret.

The integration tests are **manual by decision**: they need Docker and a
~180 MB image, they are never part of CI, and every one of them is `#[ignore]`d
with a reason string. A default `cargo test` (and CI) therefore runs layer 1
only and never starts a container.

## 2. Layer 1 — Unit Tests (in-crate)

Co-located `*_tests.rs` modules, no network beyond a loopback mock server, no
Docker. 168 tests:

| Module | Tests | What is covered |
|---|---|---|
| `config_tests` | 19 | Defaults; explicit values; unknown keys rejected (also inside `retry`); `${VAR}` expansion of `token` and `token_file`; `token_env` is a name and is not expanded; `Debug` hides the token; exactly one token source (none and several rejected, empty values rejected); validation of `address`, `mount`, `path_prefix`, `namespace`, `timeout_secs`, `retry` bounds; errors never contain the token |
| `domain::retry_tests` | 7 | Default policy; no-retry policy; backoff doubles and is capped at 2 s (also for absurd inputs); jitter stays in the upper half of the backoff |
| `domain::wire_tests` | 52 | KV paths and URL joining; base64 coding; request bodies (no `cas`; the `{"max_versions":100}` body of the metadata call); read-body parsing for live, null, soft-deleted and destroyed shapes (the real Vault bodies are constants), foreign entries (`Internal`), malformed bodies (`Internal`, body never echoed); metadata parsing (destroyed vs soft-deleted, numeric sort, junk keys); `error_text` flattening, truncation and redaction; the status classification table for every operation, including plain vs explained `404`, `403`, `400`, `408`, `429`, `5xx`, status boundaries; the classification of the metadata `POST` of `put` (`2xx` succeeds, a plain `404` does not) |
| `domain::service_tests` | 28 | The service over the real HTTP transport and a mock server: headers, bodies, paths, `put` setting the key's `max_versions` to 100 through `POST metadata` and then writing without `cas`, a failed metadata call (`500`, `503`) failing the `put` with no data request, a `403` on the metadata call as `CredentialsRejected` with no data request, retries counted per operation (a `get` is attempted `max_attempts` times, a `put` once), `Retry-After` reaching the error, soft-deleted and `version=0` handling, a missing mount as an error for `get` and `destroy`, `destroy(Below)` selection including soft-deleted versions, no call for `Below(0|1)` and `Exactly(0)`, the encoded secret absent from a `400` message, the token absent from a `403` message |
| `domain::service_retry_tests` | 19 | The retry policy through a scripted fake transport: which failures retry (timeout, connect, no token, `5xx`, `408`, `429`), which do not (`404`, `400`, `403`, `401`), `max_attempts = 1`, giving up after exactly `max_attempts`, `put` never retried for any failure kind, on the metadata call as well as on the data call, the metadata `POST` with `{"max_versions":100}` sent before the data write, a `403` on the metadata call ending the `put` with no data request, each call of `destroy(Below)` having its own budget, selection of versions from the metadata map |
| `domain::client_tests` | 4 | The trait implementation delegates to the service |
| `infra::http_tests` | 18 | Token and namespace headers, body and content type, verbs, connect failure; **403 handling**: token file re-read and one resend with the new token (and its body), no resend for an unchanged file, an unreadable file or an inline token, a second `403` is final, the new token is cached afterwards; a missing token file is a credentials error that sends nothing, and a file that appears later is used; `Retry-After` parsing (seconds, cap, ignored forms) |
| `infra::token_tests` | 17 | Source selection from the config; trimming; unusable tokens (empty, spaces, tabs, non-ASCII) fail startup without echoing the token; environment source (set, unset, empty); lazy file source; caching until a rejection; rotation, unchanged, truncated and deleted file; a cache that another request already refreshed |

## 3. Layer 2 — Conformance Suite (Docker, manual)

`tests/conformance.rs` runs every check of `credstore_sdk::conformance` against
the plugin and a real `hashicorp/vault` dev server: one test per check, 21 in
all, each with its own server and a plugin that authenticates with a token
holding **only the minimal ACL policy** of [DESIGN §4.1](./DESIGN.md#41-security-and-data-protection).
Passing proves two things at once: the plugin meets the backend contract
(exact bytes, distinct immutable versions, idempotent `delete_key` and
`destroy`, ordered versions, destroyed versions not reissued, key isolation,
concurrent puts), and the documented policy is sufficient.

The checks are `put_get_text`, `put_get_binary`, `put_get_empty`,
`put_get_large`, `puts_yield_distinct_immutable_versions`,
`get_never_written_key_is_none`, `get_unissued_version_is_none`,
`keys_are_isolated_across_tenants`, `keys_are_isolated_within_tenant`,
`delete_key_removes_all_versions`, `delete_key_is_idempotent`,
`delete_key_does_not_touch_other_keys`, `concurrent_puts_yield_distinct_versions`,
`destroy_below_removes_only_older_versions`, `destroy_exactly_removes_only_that_version`,
`destroy_is_idempotent`, `destroy_missing_key_is_ok`,
`destroy_does_not_touch_other_keys`, `versions_are_ordered_across_sequential_puts`,
`versions_are_ordered_after_concurrent_puts` and `destroyed_versions_are_not_reissued`.

**How the suite is generated.** `tests/conformance.rs` invokes the SDK's public
`credstore_sdk::credstore_plugin_conformance!` with an `#[ignore = "..."]`
attribute: the macro applies outer attributes to every generated test, so a
default `cargo test` does not start Docker containers. The factory expression
starts a Vault container and returns a small wrapper that owns both the container
and the plugin, so the server lives as long as the test (and a test returns early
when Docker is unreachable, unless `VAULT_CREDSTORE_REQUIRE_DOCKER` is set).

## 4. Layer 3 — Vault Behaviour Tests (Docker, manual)

`tests/vault_behaviour.rs`: tests of what is specific to Vault and to this
plugin, each with its own server and the minimal-policy token (the root token
only where the test says so). Vault's state is inspected and changed through its
HTTP API with the root token.

### 4.1 Container Setup

`tests/common/mod.rs` provides the fixture.

- **Image**: `hashicorp/vault`, tag `2.1.1` by default; both come from
  `test_containers::vault()` (`libs/test-containers`, workspace `docs/TESTING.md`
  section 4.4). Override the tag with `GEARS_TEST_VAULT_TAG` (for example `1.21`).
  Pull happens on first use.
- **Server**: `vault server -dev` with root token `root`, listening on
  `0.0.0.0:8200`, `IPC_LOCK` capability, through `testcontainers`
  (`GenericImage`). The fixture polls `/v1/sys/health` until the server answers.
- **Mount**: a KV v2 mount `credstore-it` configured as production prescribes
  (`delete_version_after = 0s`, `cas_required = false`); the plugin sets the
  key's own `max_versions` to 100 before every `put`, so the mount's value is
  not an obligation. The policy for the metadata path includes `create` and
  `update` for that call.
- **Path prefix**: `it-<random>` per fixture.
- **Policy and token**: the minimal policy of the docs for that mount and
  prefix, and a token created with it (`no_default_policy`).
- **Parallelism**: one container per test; at most four run at once.
- **Docker not reachable**: the fixture prints `SKIPPING: Docker is not reachable
  ...` and the test returns early. Set `VAULT_CREDSTORE_REQUIRE_DOCKER=1` to turn
  that into a failure.
- **Cleanup**: a container is removed when its test ends. If the test process
  is killed, containers can stay behind; remove them with `docker ps` and
  `docker rm -f`.

### 4.2 Scenarios

| Test | What it proves |
|---|---|
| `soft_deleted_version_answers_404_and_reads_as_none` | Vault answers `404` with `data.data = null` and a `deletion_time` (no `errors`) for a soft-deleted version; the plugin returns `None`, neighbours are unaffected, and `undelete` brings the value back |
| `destroyed_version_answers_404_and_reads_as_none` | The same for a destroyed version (`destroyed = true`); a destroyed number, even the newest, is not reissued |
| `unissued_versions_answer_404_and_version_zero_is_never_sent` | Absent version and absent key answer `404 {"errors":[]}` and read as `None`; `?version=0` returns the latest version at Vault (documented) and the plugin never sends it |
| `entries_not_written_by_the_plugin_are_internal` | A hand-written entry without `value`, and one whose `value` is not base64, are a permanent `Internal` error |
| `put_sets_the_key_limit_to_100_whatever_the_mount_says` | With a mount limit of 3 and five `put`s, the key's metadata shows `max_versions = 100` and all five versions read back exactly |
| `exceeding_100_versions_evicts_the_oldest_and_reads_as_none` | 101 `put`s: version 1 answers `404 {"errors":[]}` and reads as `None`, versions 2 and 101 are exact, the metadata lists 100, and `destroy(Below)` still works |
| `a_per_key_max_versions_can_only_raise_the_mount_limit` | Vault's own rule, written with raw calls: the effective limit is the larger of the mount's and the key's `max_versions` (why the plugin's 100 holds under any lower mount value) |
| `default_retention_without_max_versions_is_ten_versions` | Vault's own rule with raw writes: mount `max_versions = 0` and no key limit keep 10 versions; a key written by the plugin on the same mount keeps all 12 |
| `destroy_below_over_many_versions` | 30 versions: `Below(25)` destroys 1 to 24 (a soft-deleted one among them) and keeps 25 to 30 byte-exact; repeated and raised cuts are idempotent; `Exactly` for one version, for a destroyed one and for one never issued |
| `destroy_and_delete_on_missing_keys_succeed` | `destroy` and `delete_key` of a key that was never written succeed, repeatedly |
| `delete_key_removes_every_version` | After `delete_key` the metadata is gone and every version reads as `None`; Vault restarts the numbering at 1 (documented) |
| `token_file_rotation_is_picked_up_after_a_403` | Rotate the file and revoke the old token: the next request succeeds through one re-read and one resend, for a read and for a write; a persistent `403` (revoked, file unchanged) is `CredentialsRejected` for every operation and never contains a token; a new token in the file recovers without restart |
| `token_file_that_appears_after_startup_is_used` | The plugin starts with no token file; calls are `Unavailable` until the file appears |
| `the_documented_policy_is_confined_to_the_prefix` | The minimal policy denies another prefix, another mount and soft delete |
| `a_missing_mount_is_an_error_not_a_miss` | Vault's explained `404` for an unknown mount is an `Internal` error for `put`, `get`, `delete_key` and both `destroy` forms (root token) |
| `a_token_without_the_policy_is_credentials_rejected` | An unknown token yields `403`; the plugin answers `CredentialsRejected` without echoing the token |
| `a_sealed_vault_is_service_unavailable` | A sealed server (`503 Vault is sealed`) is `Unavailable` for every operation, after the bounded retries |

## 5. Running the Tests

Always through the stable toolchain wrapper this repository uses, one cargo
command at a time:

```bash
# Layer 1 (what CI runs): unit tests only, the integration tests are ignored
cargo test -p cf-gears-vault-credstore-plugin-v2

# Layers 2 and 3: needs a running Docker daemon (first run pulls the image)
cargo test -p cf-gears-vault-credstore-plugin-v2 -- --ignored

# Only the conformance suite, or only the behaviour tests
cargo test -p cf-gears-vault-credstore-plugin-v2 --test conformance -- --ignored
cargo test -p cf-gears-vault-credstore-plugin-v2 --test vault_behaviour -- --ignored

# One test
cargo test -p cf-gears-vault-credstore-plugin-v2 --test vault_behaviour -- --ignored soft_deleted

# Another Vault version; fail instead of skipping when Docker is missing
GEARS_TEST_VAULT_TAG=1.21 VAULT_CREDSTORE_REQUIRE_DOCKER=1 \
  cargo test -p cf-gears-vault-credstore-plugin-v2 -- --ignored
```

There is deliberately no `make` target for the integration tests: the Makefile's
integration targets belong to the CI `integration` job, and these tests are not
part of it.

## 6. Static Analysis

```bash
cargo clippy -p cf-gears-vault-credstore-plugin-v2 --all-targets --all-features -- -D warnings
cargo fmt -p cf-gears-vault-credstore-plugin-v2 --check
cargo gears lint --dylint -P cf-gears-vault-credstore-plugin-v2   # DE0301 / DE0308 domain-infra split
make check-release-config
```

The domain layer imports no HTTP types; `reqwest` is confined to
`infra/http.rs`.

## 7. Cadence and Qualification

| Layer | When |
|---|---|
| L1 unit | Every PR (CI) |
| L2 conformance and L3 behaviour | By hand: before releasing the plugin, when `cf-gears-credstore-v2-sdk`'s conformance suite or this plugin's wire logic changes, and when moving to a new Vault major version |

Qualified on `hashicorp/vault` **2.1.1** and **1.21**: all 37 integration tests
pass on both (2026-10-03).

## 8. Coverage Gaps and Follow-ups

| Gap | Why | Mitigation |
|---|---|---|
| OpenBao is not run | Decision: only the `hashicorp/vault` image is used; OpenBao shares the KV v2 implementation | The response shapes are Vault's; run the same tests against an OpenBao image by hand before relying on it |
| TLS and certificate handling | The plugin has no TLS settings; the HTTP client's defaults apply | Operator's choice of an `https://` address; not tested here |
| A real Vault Agent sidecar | The tests rotate the token file by hand | The mechanism under test is "the file changes, Vault answers `403`", which is what an agent causes |
| Token expiry by TTL | Tests revoke tokens instead of waiting for a TTL | Revocation and expiry look the same to Vault clients (`403`) |
| Namespaces | Dev servers of the open-source edition ignore the namespace header | Unit tests check the header is sent |
| `token_env` against Vault | Covered at unit level only | The source is resolved at startup and sent like any other token |
| Standby nodes, HA failover, redirects | Single dev node | Failover is Vault's concern behind one address |
| Network faults (partition, slow responses) | No fault-injection harness; a sealed server and mocks cover the classification | Timeouts are enforced by the HTTP client and covered with mocks |
| Throughput and latency | Not a goal of the suite | Bounds are configuration (`timeout_secs`, `retry`) |
