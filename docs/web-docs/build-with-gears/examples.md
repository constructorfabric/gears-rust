---
title: Examples
description: Worked, runnable example gears in the framework repository — a full gear, out-of-process REST gears, and provider/plugin patterns.
sidebar:
  label: Examples
  order: 15
---

Every pattern in this section maps to runnable code in the framework repository. Read these alongside the how-to pages — the docs abridge; the examples compile.

## Full gear walkthrough

- **`examples/toolkit/users-info/`** — the complete gear followed by [Build your first gear](../your-first-gear/) and [Gear anatomy](../gear-anatomy/): an SDK, a domain service with authorization, secure multi-tenant persistence, a REST surface with OData, and runtime wiring.

## Out-of-process gears (REST-first)

- **`examples/toolkit/hello/`** — a minimal gear that can run in-process or as a REST-first OoP worker, selected by config.
- **`examples/toolkit/api-contracts/`** — a provider/consumer pair demonstrating inter-gear REST contract calls across the OoP boundary. See [Run a gear out-of-process](../out-of-process/).

## Out-of-process gears (gRPC opt-in)

- **`docs/toolkit_unified_system/09_oop_grpc_sdk_pattern.md`** — the gRPC SDK pattern for gears that opt in to gRPC transport.

## Object-oriented / composition patterns

- **`examples/toolkit/api-contracts/`** — composition patterns showing gears calling gears through `ClientHub` and over the OoP boundary. See [SDK contracts and ClientHub](../../concepts/sdk-and-clienthub/).

## Toolkit feature examples

- **`examples/toolkit/type_safe_api_builder.rs`** — declaring REST operations with `OperationBuilder`.
- **`examples/toolkit/lifecycle_example.rs`** — the stateful lifecycle and background tasks. See [Runtime and lifecycle](../../concepts/runtime-and-lifecycle/).

## FIPS

- **`examples/cf-gears-fips-probe/`** — verifying the FIPS-aware crypto path. See [Compliance and FIPS](../../concepts/compliance-and-fips/).

## Gear quickstarts

Some shipped gears include minimal curl-based quickstarts:

- [File Parser QUICKSTART.md](https://github.com/constructorfabric/gears-rust/blob/main/gears/file-parser/QUICKSTART.md)
- [Nodes Registry QUICKSTART.md](https://github.com/constructorfabric/gears-rust/blob/main/gears/system/nodes-registry/QUICKSTART.md)
- [Tenant Resolver QUICKSTART.md](https://github.com/constructorfabric/gears-rust/blob/main/gears/system/tenant-resolver/QUICKSTART.md)

## See also

- [Install and run](../) — boot the example server with `make example`.
- [Reusable service gears](../../capabilities/service-gears/) — the shipped example gears.
