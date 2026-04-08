# Dependencies Guidelines

This document outlines the preferred dependencies and libraries to use in the CyberFabric project.

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

### Synchronous locks

**Always use `parking_lot::Mutex` and `parking_lot::RwLock` for synchronous locks, not `std::sync::Mutex` / `std::sync::RwLock`.**

- **Reason**: `parking_lot` locks are faster (no syscall on uncontended path), have smaller lock metadata than `std`, never poison on panic, and return the guard directly from `lock()` — no `Result` unwrapping needed.
- **Workspace dep**: `parking_lot` is already declared in the workspace `Cargo.toml`.

```rust
use parking_lot::Mutex;

let data = Mutex::new(Vec::new());
// Blocking lock — returns MutexGuard directly, no unwrap needed
data.lock().push(42);
```

### When to use `tokio::sync` instead

Use `tokio::sync::Mutex` or `tokio::sync::RwLock` only when you need to **hold the lock across an `.await` point**. If the critical section is purely synchronous (no `.await` inside), prefer `parking_lot` even in async code — it avoids the overhead of the async mutex and won't block the executor as long as the critical section is short.

If a synchronous lock guard must be held while performing blocking I/O or expensive computation, wrap the call in `tokio::task::spawn_blocking()` to avoid stalling the async executor.
