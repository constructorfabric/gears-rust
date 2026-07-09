---
title: Out-of-process gears (gRPC)
description: Run a gear as a separate gRPC service behind the same SDK trait, selected by configuration.
sidebar:
  label: Out-of-process (gRPC)
  order: 5
---

A gear can run **in the host process** (resolved through `ClientHub` as a direct call) or
**out-of-process** as its own gRPC service. Consumers are unaffected: they call the same SDK
trait, and configuration decides which backend `ClientHub` hands back. This guide follows the
`calculator` example (`examples/oop-gears/calculator/`).

## The contract is still the SDK trait

Nothing changes about the public contract — it's an ordinary SDK trait:

```rust title="calculator-sdk/src/api.rs"
#[async_trait]
pub trait CalculatorClientV1: Send + Sync {
    async fn add(&self, ctx: &SecurityContext, a: i64, b: i64) -> Result<i64, CalculatorError>;
}
```

## Define the proto and generate code

The wire protocol is a `.proto`, compiled at build time with `tonic-prost-build`:

```protobuf title="calculator-sdk/proto/oop/calculator/v1/accum.proto"
syntax = "proto3";
package oop.calculator.v1;

service CalculatorService {
  rpc Add(AddRequest) returns (AddResponse);
}
message AddRequest  { int64 a = 1; int64 b = 2; }
message AddResponse { int64 sum = 1; }
```

```rust title="calculator-sdk/build.rs"
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&["proto/oop/calculator/v1/accum.proto"], &["proto"])?;
    Ok(())
}
```

## Implement the gRPC client (a backend for the SDK trait)

The SDK provides a gRPC client that implements `CalculatorClientV1`, attaching the
`SecurityContext` to request metadata so identity propagates across the wire:

```rust title="calculator-sdk/src/client.rs"
#[async_trait]
impl CalculatorClientV1 for CalculatorGrpcClient {
    async fn add(&self, ctx: &SecurityContext, a: i64, b: i64) -> Result<i64, CalculatorError> {
        let mut request = tonic::Request::new(AddRequest { a, b });
        attach_secctx(request.metadata_mut(), ctx)?;       // identity → gRPC metadata
        let resp = self.inner.clone().add(request).await?;
        Ok(resp.into_inner().sum)
    }
}
```

A small `wire_client` helper resolves the service endpoint via the directory and registers
the gRPC client under the SDK trait — the mirror image of an in-process `register`:

```rust title="calculator-sdk/src/wiring.rs"
pub async fn wire_client(hub: &ClientHub, resolver: &dyn DirectoryClient) -> Result<()> {
    let endpoint = resolver.resolve_grpc_service(SERVICE_NAME).await?;
    let client = CalculatorGrpcClient::connect(&endpoint.uri).await?;
    hub.register::<dyn CalculatorClientV1>(Arc::new(client));
    Ok(())
}
```

## Serve the gRPC side (the gear)

The gear declares the `grpc` capability and registers a tonic service that extracts the
`SecurityContext` from metadata and calls the domain:

```rust title="calculator/src/gear.rs"
#[toolkit::gear(name = "calculator", capabilities = [grpc])]
pub struct CalculatorGear;

#[async_trait]
impl GrpcServiceCapability for CalculatorGear {
    async fn get_grpc_services(&self, ctx: &GearCtx) -> Result<Vec<RegisterGrpcServiceFn>> {
        let service = ctx.client_hub().get::<Service>()?;
        let svc = CalculatorServiceServer::new(CalculatorServiceImpl::new(service));
        Ok(vec![RegisterGrpcServiceFn {
            service_name: SERVICE_NAME,
            register: Box::new(move |routes| { routes.add_service(svc.clone()); }),
        }])
    }
}
```

## Run as a separate process

An out-of-process gear has its own binary that boots via `run_oop_with_options` — it
self-registers with the directory and runs the normal gear lifecycle:

```rust title="calculator/src/main.rs"
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = OopRunOptions { gear_name: "calculator".into(), ..Default::default() };
    run_oop_with_options(opts).await
}
```

## Switch modes with configuration

The deployment shape is a config decision, not a code change. Selecting `runtime.type: oop`
runs the gear out-of-process; the consumer's `ClientHub` lookup is identical either way.

```yaml
gears:
  calculator:
    runtime:
      type: oop
```

:::note[Directory & discovery]
Out-of-process gears find each other through a directory service (the gRPC hub). The example
ships a master config (`config/oop-example-master.yaml`) wiring the gateway, gRPC hub, and
orchestrator; custom discovery backends are an advanced topic the example doesn't cover.
:::

## See also

- [Gears & composition](../../concepts/gears-and-composition/) — in-process vs out-of-process.
- Full code: `examples/oop-gears/calculator/`.
