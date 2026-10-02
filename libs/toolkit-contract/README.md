# ToolKit Contract

Contract definitions, transport bindings, and generated-client runtime support for Gears / ToolKit.

## Overview

The `cf-gears-toolkit-contract` crate provides:

- Contract traits and static descriptors (`Contract`, `ServiceContract`, `ContractDescriptor`, `ServiceDescriptor`)
- A transport-neutral contract IR with HTTP and gRPC binding metadata, validation, idempotency, and streaming semantics
- Contract declaration and code-generation macros: `contract`, `rest_contract`, `grpc_contract`, `consumes`, and `provides`
- REST query-parameter types and the `QueryParams` derive, shared by generated clients, server routes, and OpenAPI specifications
- A per-call policy stack for cross-cutting behavior such as tracing, metrics, and authorization
- Client wiring and transport tuning for local, REST, and gRPC contract implementations
- gRPC representation traits and protobuf conversion helpers

## Features

- `runtime-client`: enables runtime helpers shared by generated REST clients
- `rest-client`: enables generated REST clients and directory-resolving client wrappers
- `rest-server`: enables generated REST route integration and query extraction
- `grpc-client`: enables runtime helpers for generated gRPC clients
- `canonical-errors`: re-exports the RFC 9457 `Problem` envelope and enables transport-error conversion
- `otel`: enables OpenTelemetry trace propagation and RED client metrics for generated clients

## License

Licensed under Apache-2.0.
