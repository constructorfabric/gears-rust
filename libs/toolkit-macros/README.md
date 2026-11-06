# ToolKit Macros

This crate contains the proc-macros used by `toolkit`.

In most crates you should import macros from `toolkit` (it re-exports them):

```rust
use toolkit::{gear, lifecycle};
```

If you depend on `cf-gears-toolkit-macros` directly, the Rust crate name is `toolkit_macros`:

```rust
use toolkit_macros::{gear, lifecycle, grpc_client};
```

## Macros

### `#[gear(...)]`

Attribute macro for declaring a ToolKit gear and registering it via `inventory`.

Parameters:

- **`name = "..."`** (required)
- **`deps = ["..."]`** (optional)
- **`capabilities = [..]`** (optional)
  - Allowed values: `db`, `rest`, `rest_host`, `stateful`, `system`, `grpc_hub`, `grpc`
- **`ctor = <expr>`** (optional)
  - If omitted, the macro uses `Default::default()` (so your type must implement `Default`).
- **`client = <path::to::Trait>`** (optional)
  - Current behavior: compile-time checks (object-safe + `Send + Sync + 'static`) and defines `MODULE_NAME`.
  - It does not generate ClientHub registration helpers.
- **`lifecycle(...)`** (optional, used for `stateful` gears)
  - `entry = "serve"` (default: `"serve"`)
  - `stop_timeout = "30s"` (default: `"30s"`; supports `ms`, `s`, `m`, `h`)
  - `stop_timeout_fn = "method_name"` (mutually exclusive with `stop_timeout`; computes the timeout from the gear at stop)
  - `await_ready` / `await_ready = true|false` (default: `false`)
  - `ready_warn_after = "90s"` (optional; requires `await_ready`, else a compile error) — non-fatal readiness warning period; expiry logs an error but keeps waiting. Default 60s.

Example (stateful, no ready gating):

```rust
use toolkit::Gear;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
#[gear(
    name = "demo",
    capabilities = [stateful],
    lifecycle(entry = "serve", stop_timeout = "1s")
)]
pub struct Demo;

impl Demo {
    async fn serve(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}
```

Example (stateful, with ready gating):

```rust
use toolkit::Gear;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
#[gear(
    name = "demo_ready",
    capabilities = [stateful],
    lifecycle(entry = "serve", await_ready, stop_timeout = "1s")
)]
pub struct DemoReady;

impl DemoReady {
    async fn serve(
        &self,
        _cancel: CancellationToken,
        _ready: toolkit::lifecycle::ReadySignal,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
```

### `#[lifecycle(...)]`

Attribute macro applied to an `impl` block. It generates a `toolkit::lifecycle::Runnable` impl and an `into_gear()` helper.

Parameters:

- **`method = "serve"`** (required)
- **`stop_timeout = "30s"`** (optional)
- **`stop_timeout_fn = "method_name"`** (optional; mutually exclusive with `stop_timeout`)
- **`await_ready` / `await_ready = true|false`** (optional)
- **`ready_warn_after = "90s"`** (optional; requires `await_ready`, else a compile error) — non-fatal readiness warning period

Notes:

- If `await_ready` is enabled, the runner method must accept a `ReadySignal` as the 3rd argument.

### `#[grpc_client(...)]`

Attribute macro applied to an empty struct. It generates a wrapper struct with:

- `connect(uri)` and `connect_with_config(uri, cfg)` using `toolkit_transport_grpc::client::connect_with_stack`
- `from_channel(Channel)`
- `inner_mut()`
- a compile-time check that the generated client type implements the API trait

Parameters:

- **`api = "path::to::Trait"`** (required; string literal path)
- **`tonic = "path::to::TonicClient<Channel>"`** (required; string literal type)
- **`package = "..."`** (optional; currently unused)

Minimal example:

```rust
use toolkit::grpc_client;

#[grpc_client(
    api = "crate::MyApi",
    tonic = "my_proto::my_service_client::MyServiceClient<tonic::transport::Channel>",
)]
pub struct MyGrpcClient;

// You still implement `MyApi` manually for the generated client type.
```

## See also

- [ToolKit unified system](../../docs/toolkit_unified_system/README.md)
- [Gear layout and SDK pattern](../../docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md)
