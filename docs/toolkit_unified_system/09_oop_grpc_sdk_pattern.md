# Out-of-Process Gears and gRPC SDK Pattern

ToolKit can run a gear as a separate process and call it over gRPC. Why and how deployment profiles, discovery and inter-process auth are designed is in the designs below; this page covers the code a gear and its SDK need.

**Design references:** [toolkit-oop DESIGN](../arch/toolkit-oop/DESIGN.md) · [ADR-0001 deployment profiles](../arch/toolkit-oop/ADR/0001-cpt-cf-adr-deployment-profiles.md) · [ADR-0005 eventual readiness](../arch/toolkit-oop/ADR/0005-cpt-cf-adr-eventual-readiness.md) · [ADR-0008 two-plane auth](../arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md) · [toolkit-contract-binding DESIGN](../arch/toolkit-contract-binding/DESIGN.md) · [binding ADR-0004 consumer wiring](../arch/toolkit-contract-binding/ADR/0004-cpt-cf-binding-adr-consumer-wiring.md) · [binding ADR-0008 gRPC projection](../arch/toolkit-contract-binding/ADR/0008-cpt-cf-binding-adr-grpc-projection.md) · [binding ADR-0009 client circuit breaker](../arch/toolkit-contract-binding/ADR/0009-cpt-cf-binding-adr-client-circuit-breaker.md)

## Core invariants

- **Rule**: For OoP gears, use the SDK pattern with a single `*-sdk` crate containing API trait, types, gRPC client, and wiring helpers.
- **Rule**: For gRPC: server implementations live in the gear itself and are exposed through the `grpc` capability; the SDK crate provides the client.
- **Rule**: Prefer the generated client: declare the contract with `#[toolkit::grpc_contract]` and consume it with `#[toolkit::consumes(..., transport = grpc)]` (see [Generated gRPC consumer](#generated-grpc-consumer)).
- **Rule**: For hand-written gRPC clients: connect with `toolkit_transport_grpc::client` utilities (`connect_with_stack`, `connect_with_retry`, `connect_lazy`), each taking `(uri, &GrpcClientConfig)`.
- **Rule**: Use `CancellationToken` for coordinated shutdown across the entire process tree.

## RuntimeKind

Gears can run in two modes:

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    #[default]
    Local,  // In-process (default)
    Oop,    // Out-of-process
}
```

## OoP Gear Configuration

### YAML configuration

```yaml
gears:
  calculator:
    runtime:
      type: oop
      execution:
        executable_path: "~/.cf-gears/bin/calculator-oop.exe"
        args: [ ]
        working_directory: null
        environment:
          RUST_LOG: "info"
    config:
      some_setting: "value"
```

### Configuration fields

- `type: oop` — marks the gear as out-of-process
- `executable_path` — path to the gear binary (supports `~` expansion)
- `args` — command-line arguments passed to the executable
- `working_directory` — optional working directory for the process
- `environment` — environment variables to set for the process

## OoP Bootstrap Library

### Bootstrap entry point

From `examples/oop-gears/calculator/calculator/src/main.rs`:

```rust
use toolkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

let opts = OopRunOptions {
    gear_name: "calculator".to_string(),
    verbose: cli.verbose,
    config_path: cli.config,
    ..Default::default()
};

run_oop_with_options(opts).await
```

`OopRunOptions::default()` reads `TOOLKIT_CONFIG_PATH` and `TOOLKIT_DIRECTORY_ENDPOINT` (fallback
`http://127.0.0.1:50051`). The master host sets `TOOLKIT_DIRECTORY_ENDPOINT` and `TOOLKIT_MODULE_CONFIG`;
the config path comes from the gear's `execution.args`.

### OopRunOptions fields

| Field | Description |
|-------|-------------|
| `gear_name` | Directory registration name (e.g., "file-parser") |
| `config_gear_name` | `gears.<key>` section to read config from; `None` means `gear_name` |
| `instance_id` | Instance ID (defaults to random UUID) |
| `directory_endpoint` | DirectoryService gRPC endpoint |
| `config_path` | Path to configuration file |
| `verbose` | Log verbosity |
| `print_config` | Print effective config and exit |
| `heartbeat_interval_secs` | Heartbeat interval (default: 5) |
| `version` | Gear version used for `DirectoryService` registration and the generated `OpenAPI` spec version; `None` means no explicit version |

## OoP Lifecycle

### Startup sequence

`run_oop_with_options` (`libs/toolkit/src/bootstrap/oop.rs`):

1. Creates the process's root `CancellationToken` and hooks OS signals (SIGTERM, SIGINT, Ctrl+C).
2. Loads configuration (the local `config_path` file plus the rendered gear config the master passes in
   the `TOOLKIT_MODULE_CONFIG` env var) and initializes logging.
3. Creates a lazy `DirectoryService` client; it connects on first use, not at bootstrap.
4. With `oop_http` configured: serves HTTP and runs the presence loop, which registers the instance with
   backoff, sends heartbeats and deregisters from `DirectoryService` on shutdown.
5. Without `oop_http` (legacy path, used by the calculator example): only a fixed-interval heartbeat runs
   on a child token; the instance is neither registered nor deregistered by bootstrap.
6. Runs the gear runtime with `ShutdownOptions::Token(root)`.

Registration retries and readiness gating are designed in [ADR-0005](../arch/toolkit-oop/ADR/0005-cpt-cf-adr-eventual-readiness.md#consequences); the component is [toolkit-oop DESIGN § OoP Bootstrap](../arch/toolkit-oop/DESIGN.md#oop-bootstrap).

## SDK Pattern for OoP

### Gear structure with SDK

```text
gears/my-gear/
  ├── my-gear-sdk/                # SDK for consumers
  │   ├── Cargo.toml
  │   ├── build.rs                # Proto compilation
  │   ├── proto/
  │   │   └── my_gear.proto       # gRPC service definition
  │   └── src/
  │       ├── lib.rs              # Re-exports: API trait, proto stubs, SERVICE_NAME
  │       ├── api.rs              # API trait + errors
  │       ├── client.rs           # gRPC client implementing the API trait
  │       └── wiring.rs           # wire_client()
  └── my-gear/                    # Gear implementation + gRPC server
      ├── Cargo.toml
      └── src/
          ├── lib.rs
          ├── gear.rs             # #[toolkit::gear(capabilities = [grpc])] + GrpcServiceCapability
          ├── api/grpc/           # tonic service implementation
          ├── domain/
          └── main.rs             # OoP binary entry point
```

Consumers depend only on `my-gear-sdk`. `examples/oop-gears/calculator/` follows this layout.

## Generated gRPC consumer

When the contract is declared with `#[toolkit::contract]` and projected with `#[toolkit::grpc_contract]`
([binding ADR-0008](../arch/toolkit-contract-binding/ADR/0008-cpt-cf-binding-adr-grpc-projection.md)), the SDK gets a generated client and the consumer needs no
wiring code. The hand-written pattern in the rest of this page remains the alternative for clients the
macros cannot express (client or bidirectional streaming, DNS-derived endpoints, a channel shared by
several services, RPCs without a deadline).

### SDK

The `grpc_contract` projection must be named `<Contract>Grpc`. Built with the `grpc-client` feature, it
generates, among others:

- `<Contract>GrpcClient` — the client for one endpoint (`connect`, `connect_lazy`);
- `<Contract>GrpcResolvingClient` — resolves the provider from the directory on every call;
- `<BASE>_GRPC_SERVICE` — the fully qualified service name (`package.Service`), equal to tonic's
  `NamedService::NAME` for the server.

`#[toolkit::consumes]` looks for the resolving client next to the base trait, so the SDK re-exports both
at the same level:

```rust
// payments-sdk/src/lib.rs
pub use contract::PaymentApi;

#[cfg(feature = "grpc-client")]
pub use grpc::{PAYMENT_API_GRPC_SERVICE, PaymentApiGrpc, PaymentApiGrpcClient, PaymentApiGrpcResolvingClient};
```

The committed `.proto` is generated from the contract; see `toolkit_contract_protogen::check_proto` for
the drift test and `cargo xtask proto-regen` for regeneration.

### Provider

The server stays hand-written. The provider gear declares the `grpc` capability and registers the server
under the generated name, so grpc-hub publishes it in the directory under that name:

```rust
#[async_trait]
impl GrpcServiceCapability for PaymentsGear {
    async fn get_grpc_services(&self, _ctx: &GearCtx) -> anyhow::Result<Vec<RegisterGrpcServiceFn>> {
        let svc = Arc::clone(&self.service);
        Ok(vec![RegisterGrpcServiceFn {
            service_name: payments_sdk::PAYMENT_API_GRPC_SERVICE,
            register: Box::new(move |routes| {
                routes.add_service(PaymentApiGrpcService::new(Arc::clone(&svc)).into_server()); // hand-written
            }),
        }])
    }
}
```

The provider's process must run grpc-hub with an address the directory can register: a routable
`listen_addr`, or an `advertise_addr` when `listen_addr` is unspecified (`0.0.0.0`); otherwise the consumer's readiness probe never finds the service and logs a hint after about
ten rounds.

For a tenant-plane contract (methods take `SecurityContext`) the client sends only the caller's bearer, and grpc-hub does not authenticate it:

- set `internal_auth_enforcement: permissive` on a hub that has `internal_auth`, or it refuses every call;
- `permissive` applies to every service on the process's one hub (a call without an internal token reaches platform-plane services too; a present but invalid token is still refused), so serve only tenant-plane services there and keep platform-plane services (cluster, the directory) in processes whose hub is `required`;
- authenticate the bearer in the server, for example with the in-process `AuthNResolverClient` wrapped in `AuthNResolverBearerAuthenticator`.

Why: [binding ADR-0008 § Known asymmetries with the REST projection](../arch/toolkit-contract-binding/ADR/0008-cpt-cf-binding-adr-grpc-projection.md#known-asymmetries-with-the-rest-projection).

```yaml
gears:
  grpc-hub:
    config:
      listen_addr: "0.0.0.0:50051"
      advertise_addr: "payments:50051"
      internal_auth: { provider: kube, verify: { audiences: ["toolkit-internal"] } }
      internal_auth_enforcement: permissive
```

The `api-contracts` example (`api-contracts/src/grpc_gear.rs`, `config/oop-api-contracts.yaml`) runs this
setup in the OoP E2E suite.

### Consumer

```rust
#[toolkit::consumes(contract = payments_sdk::PaymentApi, from = "payments", transport = grpc)]
#[toolkit::gear(name = "orders")]
pub struct OrdersGear { /* ... */ }
```

`consumes` must sit above `#[toolkit::gear]`: it reads the gear name from the gear attribute, and below it
falls back to the kebab-case struct name (`orders-gear`), so the `gears.orders.clients.consumes` section is
never read.

The consumer depends on `payments-sdk` with `grpc-client`. At startup the runtime registers
`PaymentApiGrpcResolvingClient` as `Arc<dyn PaymentApi>` unless a local implementation is in the hub, and
`/readyz` stays 503 until the directory resolves `PAYMENT_API_GRPC_SERVICE`. The directory resolves by
service name; `from` names the provider for logs, errors and readiness.

Configuration goes under the consumer's `clients.consumes.<dep>`, a section next to the gear's `config`:

```yaml
gears:
  orders:
    clients:
      consumes:
        payments:
          timeout: "5s"                              # per-call deadline (client side; not sent as grpc-timeout)
          retry: { max_attempts: 3 }                 # for #[retryable] methods
          circuit_breaker: { open_for: "30s" }       # on by default; { enabled: false } turns it off
          # grpc_endpoint: "http://payments:50051"  # static address instead of the directory
```

`endpoint` also works for a gRPC-only consumer; a gear that consumes the same dep over both REST and gRPC
must set `grpc_endpoint` for the gRPC client. REST pool and concurrency knobs have no effect on a gRPC
client. See [binding ADR-0004 § Consumer wiring: endpoint override + client tuning](../arch/toolkit-contract-binding/ADR/0004-cpt-cf-binding-adr-consumer-wiring.md#consumer-wiring-endpoint-override--client-tuning) and [binding ADR-0009 § Configuration](../arch/toolkit-contract-binding/ADR/0009-cpt-cf-binding-adr-client-circuit-breaker.md#configuration).

## Hand-written gRPC client

Use this only when the generated client cannot express the contract. The reference is
`examples/oop-gears/calculator/`.

### SDK client

```rust
// calculator-sdk/src/client.rs
use toolkit_transport_grpc::attach_secctx;
use toolkit_transport_grpc::client::{GrpcClientConfig, connect_with_retry};

pub(crate) struct CalculatorGrpcClient {
    inner: CalculatorServiceClient<Channel>,
}

impl CalculatorGrpcClient {
    pub async fn connect(uri: impl Into<String>) -> Result<Self> {
        let cfg = GrpcClientConfig::new("calculator");
        let channel: Channel = connect_with_retry(uri, &cfg).await?;
        Ok(Self {
            inner: CalculatorServiceClient::new(channel),
        })
    }
}

#[async_trait]
impl CalculatorClientV1 for CalculatorGrpcClient {
    async fn add(&self, ctx: &SecurityContext, a: i64, b: i64) -> Result<i64, CalculatorError> {
        let mut client = self.inner.clone();
        let mut request = tonic::Request::new(AddRequest { a, b });
        attach_secctx(request.metadata_mut(), ctx)
            .map_err(|e| CalculatorError::Internal(e.to_string()))?;

        let response = client
            .add(request)
            .await
            .map_err(|status| match status.code() {
                tonic::Code::Unauthenticated => {
                    CalculatorError::Unauthorized(status.message().to_string())
                }
                _ => CalculatorError::Transport(status.message().to_string()),
            })?;
        Ok(response.into_inner().sum)
    }
}
```

`connect_with_stack` connects once; `connect_with_retry` retries per `GrpcClientConfig`;
`connect_lazy` builds a channel that connects on first use (`libs/toolkit-transport-grpc/src/client.rs`).

### SDK wiring

```rust
// calculator-sdk/src/wiring.rs
pub async fn wire_client(hub: &ClientHub, resolver: &dyn DirectoryClient) -> Result<()> {
    let endpoint = resolver.resolve_grpc_service(SERVICE_NAME).await?;
    let client = CalculatorGrpcClient::connect(&endpoint.uri).await?;
    hub.register::<dyn CalculatorClientV1>(Arc::new(client));
    Ok(())
}
```

The consumer calls `wire_client` lazily, after the provider has registered in the directory: the
calculator-gateway example does it on first use with `dyn toolkit::DirectoryClient` from its hub
(`examples/oop-gears/calculator-gateway/calculator-gateway/src/domain/service.rs`).

### Server

The provider declares `capabilities = [grpc]` and returns its tonic services from
`GrpcServiceCapability::get_grpc_services`; grpc-hub serves them and publishes them in the directory.
The gear does not start its own tonic server. See `examples/oop-gears/calculator/calculator/src/gear.rs`
and, for the generated-contract variant, [Provider](#provider) above.

## Testing OoP gears

- The domain service and the tonic service implementation are plain Rust; test them in-process
  (`calculator/src/domain/service_tests.rs`, `calculator/src/gear_tests.rs`).
- Cross-process behaviour is covered by the OoP E2E suite; see [13_e2e_testing.md](13_e2e_testing.md)
  and `examples/oop-gears/README.md` for running the example master and follower.

## Quick checklist

- [ ] Prefer `#[toolkit::grpc_contract]` + `#[toolkit::consumes(..., transport = grpc)]`; write the client by hand only when the macros cannot express it.
- [ ] Create a `*-sdk` crate with the API trait, types and the client.
- [ ] Implement the gRPC server in the gear crate and expose it with `capabilities = [grpc]` + `GrpcServiceCapability`.
- [ ] Hand-written clients: connect with `toolkit_transport_grpc::client` utilities and `GrpcClientConfig`; attach the `SecurityContext` with `attach_secctx`.
- [ ] Build the OoP binary with `OopRunOptions { gear_name, .., ..Default::default() }` and `run_oop_with_options`.
