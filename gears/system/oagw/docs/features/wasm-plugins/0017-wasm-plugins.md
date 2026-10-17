---
status: proposed
date: 2026-10-17
---

# Run OAGW WASM Transformers in an External Plugin Gear

## Context

OAGW can proxy streaming HTTP responses.
Its current native transforms can change headers and metadata.
They cannot process request or response body streams.

The first use case converts Anthropic Messages traffic to Open Responses.
The adapter must retain state across fragmented SSE events.
It must also convert the request and preserve tool-call state.

Provider adapter code can trap, loop, or allocate too much memory.
The proxy must continue to serve unrelated requests after an adapter process fails.

The framework supports separate gear processes and remote clients.
OAGW does not yet use that mechanism for its internal native plugins.

This ADR proposes an extension.
It does not replace the existing [plugin-system ADR](../../ADR/0003-plugin-system.md).
The [extension design](./DESIGN.md) defines the proposed contracts and limits.

## Decision

Add a required stream-transform stage to OAGW.
Implement its WASM backend as an external plugin gear named `oagw-wasm-runner`.

OAGW owns the transformer interface in `oagw-sdk`.
The proposed interface is `OagwTransformBackend`.
The runner implements that interface through a generated gRPC server.

Keep provider connections, routing, credentials, and transport policy in OAGW.
Keep Wasmtime, Proxy-Wasm host functions, and guest memory in the runner process.

Use one bidirectional RPC for one HTTP exchange.
The session covers both request and response.
It stays on one runner instance and one guest instance.

Support deployment as a framework OoP process or a separate Kubernetes deployment.
Use existing discovery, authentication, transport, and lifecycle facilities.
Add the missing gRPC resolving client, consumer registration, and readiness path to the framework prerequisite.
The current consumer path resolves REST endpoints.
It cannot connect this gRPC-only contract without that extension.
Do not add a second supervisor.

Add generic request-stream support to the contract system first.
Keep that framework work in a separate future commit.

Keep LLM Gateway responsible for model resolution, quota, usage accounting, and provider fallback.

### Required Failure Behavior

The new provider transformer is required.
OAGW cannot skip it after a failure.

A guest trap ends the affected session.
A runner crash ends all sessions on that runner.
Neither failure permits automatic replay through another runner.

For a gateway or runtime failure before commit, OAGW returns a canonical gateway error.
After commit, such a failure ends the body with an error.

A normalized provider failure follows a separate path.
The guest emits its public failure event immediately through the ordinary body buffer.
The LLM provider bridge accepts that event and drops the OAGW body.
OAGW cancels provider IO and the RPC without waiting for HTTP EOF.
A later cancellation error cannot replace the accepted provider failure.

A direct OAGW SSE consumer must apply the same terminal-event cancellation rule.
Generic OAGW does not inspect application SSE types.
A raw byte consumer has only transport deadlines without that application behavior.

Existing native transforms keep their current behavior.
The extension does not apply log-and-continue to provider adaptation.

### Stream Completion and Error Origin

Release final request output after the request callback succeeds.
Provider dispatch waits for request output EOS and its acknowledgement.
It does not wait for response completion or `Closed`.

Each normally completed HTTP direction has exactly one output EOS.
Bodyless output sets EOS on its headers.
A final body callback emits an empty EOS frame when it produces no bytes.
An acknowledgement is not an EOS signal.

Hold only final response callback output until `Closed` and successful RPC completion.
Continue draining the RPC into a bounded buffer during that hold.
A normal success snapshot requires clean HTTP EOF.
An early normalized failure does not require a clean transport close.

The bound path must distinguish clean EOF, idle timeout, shutdown, and transport error.
The baseline lifecycle wrapper does not preserve that distinction.
Replace or extend it for bound requests before using it in this pipeline.
Suspend upstream idle measurement during downstream backpressure.

Extract `ErrorSource` before sanitizing bridge headers.
Keep it in OAGW-owned metadata outside the guest view.
Gateway-origin Problems bypass the provider adapter and abort its session.
Preserve their canonical errors and origin.
A provider-origin status failure remains upstream-origin after valid conversion.

### ABI Profile

Use Proxy-Wasm ABI v0.2.1 names and binary conventions.
Define the capability profile as `proxy-wasm-http-v0.2.1-oagw-v1`.

Support HTTP headers, body buffers, context lifecycle, bounded logging, and stream control.
The design lists the exact supported callbacks and host functions.

Reject modules with unsupported imports before admission.
Do not expose network calls, filesystem access, shared data, or provider credentials.
Do not claim full Proxy-Wasm specification conformance.

Keep SSE assembly and provider conversion inside the guest SDK.
Do not add a private SSE callback to the host ABI.

Use the same supported guest binary in an Envoy fixture.
That check proves only the tested capability subset.
It does not prove that all Envoy plugins can run in OAGW.

### Contract Prerequisite

Extend `toolkit-contract`, its macros, and protogen with client-streaming and bidi-streaming.
Preserve existing unary and server-streaming interfaces.

Use the proposed `#[client_streaming]` marker.
Combine it with `#[streaming(open = fallible)]` for bidi methods.

Generate request-stream clients, server adapters, and protobuf declarations.
Preserve security-context handling and canonical errors.
Reject unsupported REST projections.

Keep the established protobuf input mapping.
A named input item becomes the streamed request message directly.
Do not introduce an extra wrapper for `UploadFrame` or `InputFrame`.
Primitive inputs keep the existing synthesized envelope mapping.

Add explicit gRPC consumer selection and a generated gRPC resolving wrapper.
The directory adapter selects a serving instance's registered gRPC service endpoint.
Use that same selection query for dependency readiness.
Do not use a REST endpoint as proof of gRPC readiness.
Pin the selected endpoint for the RPC lifetime.
Endpoint changes affect only new sessions.
Keep optional runner capacity separate from readiness of unrelated OAGW routes.

A producer error must end the RPC.
It must not become successful request EOF.
Do not retry request-stream calls automatically.

The framework commit must contain no OAGW or WASM dependency.

## Alternatives

### Embed Wasmtime in OAGW

This option removes the remote hop.
It also reduces deployment components.

Guest memory checks and execution limits still provide useful protection.
However, a native runtime defect or process abort can stop the proxy.

This option does not provide the selected process failure boundary.
Do not use it for the first release.

### Use Native Provider Plugin Gears

A native provider gear can run in a separate process.
It can also use the same future stream-transform contract.

This option avoids the WASM host and its ABI work.
It is suitable when each adapter has a trusted native implementation.

However, it does not provide the requested portable WASM artifact.
Each adapter also needs its own native build and deployment discipline.

Keep the public backend interface independent of Wasmtime.
That choice permits a native implementation later.
The first backend remains the WASM runner.

### Keep All Transformations in LLM Gateway

This option can use LLM-specific event types directly.
It can simplify provider-specific error handling.

It also couples the generic stream-transform mechanism to LLM Gateway.
OAGW routes cannot reuse that mechanism without an additional interface.

Keep LLM policy in LLM Gateway.
Place the reusable HTTP stream-transform boundary in OAGW.
Place provider translation in the guest.

### Run Plugins Through Envoy External Processing

Envoy provides a useful separation between proxy and external processing.
Its model informs this design.

Adopting Envoy as the data plane would add another proxy architecture.
It would not directly extend the existing Pingora-based OAGW implementation.

Use an OAGW-owned RPC for this extension.
Do not claim compatibility with Envoy's external-processing protocol.
Proxy-Wasm is the separate interface inside the runner.

### Write a Manual Bidi Adapter Only for OAGW

This option can reach a narrow prototype sooner.
It leaves streaming transport code outside the contract generator.

The project needs a reusable framework capability.
Extend the contract chain instead.
Use transport fixtures to prove that generated bidi code works.

## Consequences

### Benefits

- A runner process failure does not directly terminate the proxy process.
- Guest memory and mutable state remain outside OAGW.
- One RPC preserves request-to-response context.
- The backend interface permits future native implementations.
- The generic contract extension can serve other gears.
- The guest SDK can hide SSE byte assembly from adapter authors.
- Immutable digests permit controlled version changes and rollback.

### Costs

The remote path adds serialization, copies, network traffic, and scheduling.
Deployment also needs runner capacity and compatible module artifacts.

Latency depends on frame size, callback frequency, and deployment location.
Measure local-process and separate-pod configurations.
Do not claim a latency target before those measurements.

The limited ABI profile requires its own compatibility fixtures.
Guest authors cannot assume that an arbitrary Envoy module will load.

The completed Open Responses snapshot requires bounded assembly.
The first adapter therefore has an explicit normalized-response size limit.

### Remaining Risks

A runner crash affects every active session in that process.
Use separate deployments for different trust boundaries when necessary.

A pod can share a node with OAGW.
Process isolation does not prevent node-wide resource exhaustion.
Set resource limits and use bounded admission.

A required adapter failure can occur after client-visible output.
The system must report incomplete output accurately.
It must not replay a partially delivered generation.

A terminal event cannot come from a crashed guest.
LLM Gateway must detect an incomplete normalized stream and apply its error contract.
The adapter holds normal success until clean HTTP EOF.
OAGW holds only final response callback output until RPC finalization succeeds.
Final request output remains available before provider dispatch.
A live adapter emits a normalized provider failure before EOF when possible.
The provider bridge then cancels the source stream.
Such application failure remains authoritative if RPC cancellation follows.

## Delivery Order

1. Extend generic toolkit contracts, gRPC discovery, and readiness. Prove the path through real transport.
2. Add the OAGW transformer interface with a remote native fixture.
3. Add the external runner and its restricted Proxy-Wasm host.
4. Add the guest SSE helper and Anthropic adapter.

The baseline LLM SDK has delta-field names that differ from the published Open Responses reference.
Resolve that mismatch before adapter conformance acceptance.
Do not include an unrelated SDK correction in the generic framework commit.

No implementation code changes belong to this documentation task.
The main OAGW design and existing ADRs remain unchanged.

## Confirmation

Acceptance requires the scenarios in the [design](./DESIGN.md#delivery-and-acceptance).

The review must verify these properties:

- Bidi output arrives before request-stream completion.
- Final request output reaches provider dispatch without waiting for `Closed`.
- Zero-byte final output and bodyless headers carry explicit EOS.
- Duplicate or missing output EOS fails the session.
- Only final response output waits for successful RPC completion.
- Slow consumers cause bounded producer progress.
- Clean half-close differs from producer failure.
- Timeout and shutdown cannot become clean HTTP EOF or a success snapshot.
- Downstream backpressure cannot trigger a false upstream idle EOF.
- Gateway-origin Pingora Problems bypass provider conversion and preserve their error source.
- A provider SSE error reaches the consumer while its HTTP connection remains open.
- The provider bridge cancels that source without a duplicate terminal failure.
- Two gRPC-only runners resolve through discovery and use matching readiness checks.
- Endpoint changes preserve active-session affinity without replay.
- Named protobuf input items retain their direct message mapping.
- Caller identity remains off the application wire.
- Guest input contains no provider or framework credentials.
- Guest traps, loops, and memory growth stop within configured limits.
- Runner termination fails affected sessions without replay.
- Unrelated proxy traffic continues after runner failure.
- Required transformation never releases unconverted provider bytes after failure.
- Existing unbound routes and native plugin order remain compatible.

The text follows the [ASD-STE100 writing approach](https://www.asd-ste100.org/about_STE.html).
Technical names retain their defined spelling.
This statement does not claim automated language certification.

## References

- [Detailed extension design](./DESIGN.md).
- [Current OAGW design](../../DESIGN.md).
- [Framework OoP pattern](../../../../../../docs/toolkit_unified_system/09_oop_grpc_sdk_pattern.md).
- [Framework plugin pattern](../../../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md).
- [Proxy-Wasm specification](https://github.com/proxy-wasm/spec).
- [Wasmtime security model](https://docs.wasmtime.dev/security.html).
- [Envoy AI Gateway data plane](https://theagentrouter.ai/docs/concepts/architecture/data-plane/).
