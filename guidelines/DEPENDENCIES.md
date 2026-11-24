# Dependencies Guidelines

This document outlines the preferred dependencies and libraries to use in the Constructor Fabric Gears project.

## Serialization

### YAML

**Use `serde-saphyr` for YAML (de)serialization. Do not add `serde_yaml`.**

- **Cargo.toml**: `serde-saphyr = { workspace = true }` (declared in the root `[workspace.dependencies]`).
- **Reason**: `serde_yaml` is deprecated (its last release is `0.9.34+deprecated`). `serde-saphyr` is a separate,
  maintained crate built on its own YAML parser, not a fork of `serde_yaml`.
- **Enforcement**: by review only. `deny.toml` does not ban `serde_yaml`; it is in `Cargo.lock` as a transitive
  dependency of the workspace `figment` entry (`yaml` feature). The ToolKit config loader itself parses YAML with
  `serde-saphyr` (`StrictYaml` in `libs/toolkit/src/bootstrap/config/mod.rs`).
- **Exception**: `k8s-cluster-plugin` uses `serde_yaml_ng` as a dev-dependency to parse its shipped CRD manifest in
  integration tests.

```rust
use std::collections::HashMap;

let data = HashMap::from([("key", "value")]);
let yaml_string = serde_saphyr::to_string(&data)?;
let parsed: HashMap<String, String> = serde_saphyr::from_str(&yaml_string)?;
```

`serde-saphyr` has no `Value` type like `serde_yaml::Value`. For untyped YAML, deserialize into `serde_json::Value`
or define a struct.

## Errors

- Domain errors are `thiserror` enums (`DomainError` in `<gear>/src/domain/error.rs`), and SDK traits return the
  gear's typed SDK error (`<GearName>Error` in `<gear>-sdk/src/errors.rs`); see the error placement table in
  [05_errors_rfc9457.md](../docs/toolkit_unified_system/05_errors_rfc9457.md).
- `anyhow` appears at the gear lifecycle boundary and in binaries: `Gear::init` returns `anyhow::Result<()>`
  (`libs/toolkit/src/contracts.rs`).

## HTTP clients

- Outbound HTTP uses `toolkit-http` (`toolkit_http::HttpClient`), per
  [ADR toolkit/0001](../docs/adrs/toolkit/0001-toolkit-hyper-tower-http-client.md).
- `reqwest` is a workspace dependency and some gears still use it. It must not enter `toolkit-auth` or other
  security-adjacent crates, directly or transitively (same ADR).

## Concurrency

### Choosing a lock

| Case | Use |
|---|---|
| Short in-memory update, no `.await` and no blocking call while the guard is held (sync or async code) | `std::sync::Mutex` / `RwLock` |
| Data needed across an `.await` | Copy it out, drop the guard, then await; still `std::sync` |
| The awaited work itself must be serialized (e.g. one in-flight token refresh) | `tokio::sync::Mutex` / `RwLock`; say why in the lock's doc comment |
| Blocking I/O or long computation on the data | Copy out under the guard, release it, then run the work in `tokio::task::spawn_blocking()`. Never hold a `std` lock during blocking work, even inside `spawn_blocking`: async tasks waiting for that lock block their executor threads |
| Blocking work that must stay serialized with the data | `tokio::sync::Mutex` held across the `spawn_blocking(..).await`, so waiters yield instead of blocking a thread |

New code uses `std::sync`. `parking_lot` stays a workspace dependency for existing code, which is left as is
(#5897).

### Poisoning

`std` lock calls return a `Result`, and `unwrap`/`expect` are denied outside tests. Decide per lock, not per call site:

- Recover with `PoisonError::into_inner` only if no critical section on that lock can leave the data inconsistent when it panics (each one is a single push, insert or assignment).
- Otherwise return an error.

```rust
data.lock().unwrap_or_else(PoisonError::into_inner).push(42);
```
