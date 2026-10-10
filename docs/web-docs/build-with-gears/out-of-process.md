---
title: Run a gear out-of-process
description: Run a gear as a separate process behind the same SDK trait
sidebar:
  label: Run a gear out-of-process
  order: 10
---

A gear can run **in the host process** through a direct `ClientHub` call or **out-of-process** over REST. For contracts
using `#[toolkit::rest_contract]` and `#[toolkit::consumes]`, consumers call the same base SDK trait in either mode.

This guide follows the `hello` and `api-contracts` examples (`examples/toolkit/hello/`,
`examples/toolkit/api-contracts/`).

## How it works

An out-of-process gear comes up in one of two ways, both using the same binary and the same configuration:

- **Host-spawned (Profile 2, self-hosted):** a `HostRuntime` process runs some gears in-process and spawns
  selected gears as separate processes. The host owns the lifecycle.
- **Standalone (the Profile 3 shape):** an operator or orchestrator runs the gear directly - a separate
  `cargo run`, a systemd unit, or a Kubernetes pod. You provide `TOOLKIT_DIRECTORY_ENDPOINT` and the config;
  nothing spawns or stops it for you.

In the host-spawned shape:

1. The host boots its in-process gears, including the directory service and edge gateway.
2. For each gear configured with `runtime.type: oop`, the host spawns the executable with `--config <path>`,
   `TOOLKIT_DIRECTORY_ENDPOINT`, and `TOOLKIT_MODULE_CONFIG` - a render of the gear's config that its
   `--config` file may override.
3. The out-of-process gear starts its own HTTP server, serves liveness/readiness probes, and self-registers its
   REST endpoint with the directory service.
4. The edge gateway discovers the out-of-process gear through the directory and reverse-proxies its public routes.
5. Inter-gear clients resolve targets through the directory and call them directly, without traversing the edge.

In the standalone local example, steps 3–5 are identical, but you set `TOOLKIT_DIRECTORY_ENDPOINT`, pass `--config`,
and stop the gear. Kubernetes uses the same worker bootstrap with cluster services and ingress.

The default OoP transport is REST; gRPC is an opt-in transport for specific contracts (see
[gRPC as an opt-in transport](#grpc-as-an-opt-in-transport)).

## The SDK trait is the contract

Consumers call the same SDK trait regardless of where the gear runs:

```rust title="hello-sdk/src/api.rs"
#[async_trait]
pub trait HelloApi: Send + Sync {
    async fn ping(&self, ctx: &SecurityContext) -> Result<Pong, HelloError>;
}
```

`#[toolkit::rest_contract]` generates the REST client, and `#[toolkit::consumes(...)]` wires it when no local
implementation is linked. Traits without this wiring are not automatically callable across processes. See
[SDK contracts and ClientHub](../../concepts/sdk-and-clienthub/).

## Implement the REST capability

The gear declares the `rest` capability and registers its routes with `OperationBuilder`:

```rust title="hello/src/gear.rs"
#[toolkit::gear(name = "hello", capabilities = [rest])]
#[derive(Default)]
pub struct HelloGear;

impl RestApiCapability for HelloGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<Router> {
        let service = Arc::new(HelloService::default());
        Ok(rest::register_routes(router, openapi, service))
    }
}
```

## Out-of-process entrypoint

An out-of-process gear has its own binary that boots via `run_oop_with_options`. It self-registers with the
directory service and serves the gear's REST endpoints:

```rust title="hello/hello/src/main.rs"
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let opts = OopRunOptions {
        gear_name: "hello".to_owned(),
        config_path: cli.config,
        verbose: cli.verbose,
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        ..Default::default()
    };

    run_oop_with_options(opts).await
}
```

## Switch modes with configuration

The deployment shape is a config decision, not a code change. What you configure depends on the shape:

- **Host-spawned:** mark the gear `runtime.type: oop` in the host config and point `execution` at its binary.
- **Standalone:** nothing goes in the host config - the host never learns the gear's path. You run the binary
  yourself with `--config` and `TOOLKIT_DIRECTORY_ENDPOINT`.

### Host-spawned config

```yaml title="config/oop-self-hosted.yaml"
gears:
  # ... control-plane gears (api-gateway, grpc-hub, service-discovery, ...) run in-process ...

  hello:
    runtime:
      type: oop
      execution:
        executable_path: "./target/debug/hello-oop"
        args: ["--config", "config/oop-hello.yaml"]
```

The host spawns `execution.executable_path` once its directory is listening and stops the child on shutdown.
`TOOLKIT_MODULE_CONFIG` is the host-rendered base. The worker's `--config` replaces the gear `config` section, merges
logging by key, and merges database settings by field.

### The gear's own config

Whether spawned or standalone, the gear's config is a normal gear config with an `oop_http` section:

```yaml title="config/oop-hello.yaml"
server:
  home_dir: "~/.cf-gears-hello"

oop_http:
  listen_addr: "127.0.0.1:9091"
  advertise_uri: "http://127.0.0.1:9091"
  allow_loopback_advertise: true

gears:
  hello:
    config: {}
```

## Running the examples

### Host-spawned (Profile 2)

`make oop-example` builds `flight-control` and `hello-oop`, then runs `config/oop-self-hosted.yaml`:
`flight-control` spawns `hello-oop` as a child process and stops it on shutdown.

### Standalone (Profile 3 shape)

The same binary runs standalone against a separately started control plane:

```bash
# 1. Start the control plane (directory + edge)
cargo run -p cf-gears-flight-control -- --config config/oop-flight-control.yaml run

# 2. Start the gear
TOOLKIT_DIRECTORY_ENDPOINT=http://127.0.0.1:50051 \
  cargo run -p hello --features oop_module --bin hello-oop -- --config config/oop-hello.yaml
```

The gear self-registers with the directory service started by `flight-control`. The edge gateway at
`http://127.0.0.1:8087` reverse-proxies its public routes. This is the shape Kubernetes packaging
builds on: swap the `cargo run` for a pod and the loopback directory endpoint for a cluster-reachable one.

## gRPC as an opt-in transport

REST is the default and primary transport for OoP gears. A gear may still expose gRPC for performance-critical
contracts or for integration with existing gRPC services; see the
[gRPC SDK pattern](https://github.com/constructorfabric/gears-rust/blob/main/docs/toolkit_unified_system/09_oop_grpc_sdk_pattern.md).

The spawn machinery is transport-agnostic: it launches the binary and lets the gear decide which
transports to serve.

## See also

- [Deployment shapes](../../concepts/deployment-shapes/) - the three deployment shapes.
- [Deploy Gears](../deploy/) - the how-to for each shape.
- Examples: `examples/toolkit/hello/`, `examples/toolkit/api-contracts/`.
