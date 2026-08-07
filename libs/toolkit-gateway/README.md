# cf-gears-toolkit-gateway

The gateway-provider abstraction for ToolKit Out-of-Process (OoP) edge routing.

This crate defines the `GatewayProvider` trait — the interface the OoP bootstrap
uses to register a gear's **public** routes with an edge gateway — together with
the typed inputs it accepts (`GearName`, `OpenApiSpec`, `Endpoint`) and its error
type (`GatewayError`).

It is a **leaf** crate by design: it does **not** depend on `cf-gears-toolkit`,
so both `cf-gears-toolkit` (the OoP bootstrap) and the `api-gateway` gear can
depend on it without introducing a dependency cycle.

## Route visibility

A provider exposes only the operations a gear marks **public** on the visibility
axis. `cf-gears-toolkit` emits this as the `x-cf-api-visibility: public` OpenAPI
vendor extension (from `OperationSpec.is_public`); this crate mirrors that
well-known key as the `API_VISIBILITY_EXTENSION` constant so it needs no
dependency on `cf-gears-toolkit`.

## Status

Unstable. The `GatewayProvider` trait may change in a minor release while the OoP
gateway story stabilizes.
