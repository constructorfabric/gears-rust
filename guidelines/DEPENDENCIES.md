# Dependencies Guidelines

This document outlines the preferred dependencies and libraries to use in the Constructor Fabric Gears project.

## Serialization

### YAML

**Always use `serde-saphyr` for YAML serialization/deserialization, not `serde_yaml`.**

- **Package name in Cargo.toml**: `serde-saphyr`
- **Import in Rust code**: `use serde_saphyr;`
- **Reason**: `serde_yaml` is deprecated and unmaintained. `serde-saphyr` is the actively maintained fork.

#### Example Usage

```rust
use serde_saphyr;
use std::collections::HashMap;

// Serialization
let data = HashMap::from([("key", "value")]);
let yaml_string = serde_saphyr::to_string(&data)?;

// Deserialization
let parsed: HashMap<String, String> = serde_saphyr::from_str(&yaml_string)?;
```

**Note**: `serde-saphyr` does not provide a `Value` type like `serde_yaml` did. For generic YAML parsing, use `HashMap<String, serde_json::Value>` or define a specific struct.

## Concurrency

### Choosing a lock

| Case | Use |
|---|---|
| Short in-memory update, no `.await` and no blocking call while the guard is held (sync or async code) | `std::sync::Mutex` / `RwLock` |
| Data needed across an `.await` | Copy it out, drop the guard, then await; still `std::sync` |
| The awaited work itself must be serialized (e.g. one in-flight token refresh) | `tokio::sync::Mutex` / `RwLock`; say why in the lock's doc comment |
| Blocking I/O or long computation on the data | Copy out under the guard, release it, then run the work in `tokio::task::spawn_blocking()`. Never hold a `std` lock during blocking work, even inside `spawn_blocking`: async tasks waiting for that lock block their executor threads |
| Blocking work that must stay serialized with the data | `tokio::sync::Mutex` held across the `spawn_blocking(..).await`, so waiters yield instead of blocking a thread |

Existing `parking_lot` code is left as is; new code uses `std::sync`.

### Poisoning

`std` lock calls return a `Result`, and `unwrap`/`expect` are denied outside tests. Decide per lock, not per call site:

- Recover with `PoisonError::into_inner` only if no critical section on that lock can leave the data inconsistent when it panics (each one is a single push, insert or assignment).
- Otherwise return an error.

```rust
data.lock().unwrap_or_else(PoisonError::into_inner).push(42);
```
