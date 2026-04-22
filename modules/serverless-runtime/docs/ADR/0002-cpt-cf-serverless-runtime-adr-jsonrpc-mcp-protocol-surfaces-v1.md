---
status: accepted
date: 2026-03-23
decision-makers: serverless-runtime team
---
<!--
 =============================================================================
 ARCHITECTURE DECISION RECORD (ADR) — based on MADR format
 =============================================================================
 PURPOSE: Capture WHY JSON-RPC 2.0 and MCP were added as protocol surfaces
 alongside the existing REST API.

 RULES:
  - ADRs represent actual decision dilemma and decision state
  - DESIGN is the primary artifact ("what"); ADRs annotate DESIGN with rationale ("why")
  - Use single ADR per decision

 STANDARDS ALIGNMENT:
  - MADR (Markdown Any Decision Records)
  - IEEE 42010 (architecture decisions as first-class elements)
  - ISO/IEC 15288 / 12207 (decision analysis process)
 ==============================================================================
 -->
# ADR — JSON-RPC 2.0 and MCP Server as Additional Protocol Surfaces


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: REST only](#option-a-rest-only)
  - [Option B: JSON-RPC 2.0 endpoint only](#option-b-json-rpc-20-endpoint-only)
  - [Option C: JSON-RPC 2.0 endpoint + MCP server endpoint](#option-c-json-rpc-20-endpoint--mcp-server-endpoint)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-runtime-adr-jsonrpc-mcp-protocol-surfaces`
## Context and Problem Statement

The Serverless Runtime exposes function invocation exclusively via a REST API. REST's resource-centric design requires callers to translate function-call semantics — a named function with typed inputs and outputs — into HTTP verbs, resource paths, and status codes. This translation adds overhead and cognitive friction, particularly for LLM agents, which reason naturally in terms of "call function X with arguments Y" rather than in terms of REST resource operations. As LLM agent frameworks have matured, the Model Context Protocol (MCP) has emerged as the dominant standard for exposing callable tools to agents. MCP is built on JSON-RPC 2.0 and provides a structured `tools/list` + `tools/call` interface with built-in support for streaming, human-in-the-loop elicitation, and LLM sampling during execution. This ADR documents the decision of how to extend the Serverless Runtime's invocation surface to serve LLM agent callers and direct JSON-RPC clients efficiently.

## Decision Drivers

* REST's resource+verb model requires a function-to-REST semantic translation layer: callers must map `invoke(function_id, inputs)` onto a specific path, HTTP method, and interpret status codes — an impedance mismatch when the caller's mental model is already a named function call
* LLM agents (Claude, GPT, Gemini, etc.) natively consume tools via MCP's `tools/list` + `tools/call` protocol; requiring agents to use REST means building a per-function REST client or a general adapter, both of which add latency and error surface
* MCP is built on JSON-RPC 2.0, which uses a generalized `{method, params} → {result | error}` signature that maps directly onto `{function_id, inputs} → {output | error}` with no translation layer
* Serverless functions already define typed input/output schemas via `IOSchema` — exactly the metadata MCP requires to emit `tools/list` entries with JSON Schema parameter descriptions
* MCP's Streamable HTTP transport (which uses SSE for server-to-client streaming) enables the server to push intermediate events — progress notifications, elicitation requests (human input), and sampling requests (LLM completions) — inline during execution, which the REST polling model cannot express without additional out-of-band channels
* MCP elicitation and sampling are first-class protocol capabilities: elicitation allows functions to pause and request human input; sampling allows functions to request LLM completions from the client, enabling agentic behaviours that require model-in-the-loop
* Treating REST as the only invocation surface would require all future LLM agent integrations to implement REST↔function-call adapters, increasing maintenance burden and reducing interoperability with the broader MCP ecosystem
* BR-031: function definitions must be usable by automated tools including LLMs in a reliable, self-describing way

## Considered Options

* **Option A**: REST only — keep the existing REST API as the sole invocation surface; agent integrations must implement their own REST↔function-call adapters
* **Option B**: JSON-RPC 2.0 endpoint only — add a single `POST /json-rpc` endpoint accepting `{method: function_id, params: inputs}` without MCP session management or elicitation/sampling support
* **Option C**: JSON-RPC 2.0 endpoint + MCP server endpoint — add both a direct JSON-RPC endpoint and a full MCP server (`/mcp`) with session lifecycle, `tools/list`, `tools/call`, SSE streaming, elicitation, and sampling

## Decision Outcome

Chosen option: **"Option C: JSON-RPC 2.0 endpoint + MCP server endpoint"**, because MCP is the established standard for LLM agent tool integration and is built on JSON-RPC 2.0, so both surfaces share the same underlying invocation semantics. A JSON-RPC-only surface (Option B) would serve direct clients but would leave LLM agents without elicitation and sampling capabilities, requiring a future ADR to add MCP anyway. Option A perpetuates the REST translation overhead for all agent callers and forecloses the elicitation and sampling patterns entirely.

### Consequences

* Function definitions gain two opt-in trait fields — `traits.json_rpc` and `traits.mcp` — that control exposure on each surface; functions without these fields remain REST-only, preserving backward compatibility
* The MCP server requires session lifecycle management (`initialize` → operations → `DELETE`) and SSE transport; this adds a stateful session layer that the REST API does not have
* Elicitation and sampling require `stream_response: true` on the function's MCP traits and an active SSE connection; functions that declare `elicitation_capable: true` or `sampling_capable: true` must be invoked through the MCP surface, not REST or plain JSON-RPC
* Both JSON-RPC and MCP surfaces delegate to the same Invocation Engine — authentication, authorization, quota enforcement, schema validation, and execution lifecycle are shared; the protocol surfaces are transport adapters only
* MCP's `tools/list` response is derived automatically from registered function definitions with `traits.mcp.enabled: true`; no separate tool registry is needed
* MCP protocol version negotiation follows the MCP 2025-03-26 specification; future protocol version upgrades are handled by the MCP server layer independently of the Invocation Engine
* The PRD requirement BR-031 (LLM-manageable definitions) gains a direct runtime expression: functions exposed via MCP are immediately consumable by any MCP-compatible LLM agent without any additional adapter

### Confirmation

* DESIGN.md defines `JsonRpcTraits`, `McpTraits`, `McpToolAnnotations`, `McpSession`, `McpElicitationContext`, and `McpSamplingContext` GTS entities with the fields and semantics specified in this ADR
* The API surface table in DESIGN.md lists the JSON-RPC 2.0 endpoint (`POST /api/serverless-runtime/v1/json-rpc`) and the MCP server endpoint (`/api/serverless-runtime/v1/mcp` with `POST`/`GET`/`DELETE`) alongside the REST endpoints
* Integration tests verify: (1) a function with `traits.json_rpc.enabled: true` is invocable via JSON-RPC and not accessible via JSON-RPC when the trait is absent; (2) a function with `traits.mcp.enabled: true` appears in `tools/list` and is callable via `tools/call`; (3) elicitation and sampling events are delivered on the SSE stream for capable functions
* Code review verifies that both protocol surfaces share the Invocation Engine path and do not bypass authentication, authorization, or quota enforcement

## Pros and Cons of the Options

### Option A: REST only

Keep the existing REST API; agent and JSON-RPC integrations must build their own adapters.

| | Aspect | Note |
|---|--------|------|
| Pro | No new protocol surface to maintain | Simpler server-side scope |
| Pro | No session state required | REST remains fully stateless |
| Con | Every LLM agent integration must implement a REST↔function-call adapter | Multiplication of adapter code across integrations |
| Con | No first-class elicitation or sampling | Cannot support human-in-the-loop or model-in-the-loop execution patterns without out-of-band mechanisms |
| Con | REST's resource model mismatches function-call semantics | Callers pay translation overhead on every invocation |
| Con | Forecloses MCP ecosystem compatibility | MCP clients (Claude Desktop, Cursor, agent frameworks) cannot connect without a proxy |

### Option B: JSON-RPC 2.0 endpoint only

Add `POST /json-rpc` accepting `{method: function_id, params: inputs}` with no MCP session or elicitation/sampling.

| | Aspect | Note |
|---|--------|------|
| Pro | Removes the REST translation layer for direct callers | `{method, params}` maps 1:1 onto `{function_id, inputs}` |
| Pro | No session state required | Simpler than full MCP |
| Pro | Lower initial implementation scope | Subset of Option C |
| Neutral | MCP clients can call `tools/call` over JSON-RPC transport | But without session lifecycle, `tools/list`, elicitation, or sampling they get degraded behaviour |
| Con | Does not satisfy MCP protocol requirements | MCP clients expect `initialize`, `tools/list`, session management; raw JSON-RPC without these is not a valid MCP server |
| Con | Elicitation and sampling not supported | Functions cannot request human input or LLM completions during execution |
| Con | Likely requires a follow-up ADR to add MCP | Incremental approach splits a coherent protocol decision across two records |

### Option C: JSON-RPC 2.0 endpoint + MCP server endpoint

Add both a direct JSON-RPC endpoint and a full MCP server with session management, `tools/list`, `tools/call`, SSE streaming, elicitation, and sampling.

| | Aspect | Note |
|---|--------|------|
| Pro | MCP is the established LLM agent tool protocol | Connects directly to Claude Desktop, Cursor, LangGraph, and other MCP clients without adapters |
| Pro | JSON-RPC generalized signature removes REST translation | `{method, params}` for direct callers; `tools/call` for MCP clients |
| Pro | Elicitation enables human-in-the-loop patterns | Functions can pause and request user input inline on the SSE stream |
| Pro | Sampling enables model-in-the-loop patterns | Functions can request LLM completions from the client agent during execution |
| Pro | Opt-in via traits fields | No impact on functions that do not declare `traits.json_rpc` or `traits.mcp` |
| Pro | Single Invocation Engine for all surfaces | Auth, quota, schema validation, and lifecycle are shared; no duplication |
| Neutral | MCP session state adds server-side tracking | Sessions are scoped to a single client connection and cleaned up on `DELETE` |
| Con | More implementation scope than Option B alone | MCP session lifecycle, SSE transport, elicitation/sampling handling all required |
| Con | Protocol version coupling | Server must track MCP spec version; breaking MCP spec changes require server updates |

## More Information

JSON-RPC 2.0 (https://www.jsonrpc.org/specification) is a lightweight, transport-agnostic remote procedure call protocol that uses `{jsonrpc, method, params, id}` request objects and `{jsonrpc, result | error, id}` response objects. Its generalized function signature is a natural fit for the Serverless Runtime's function invocation model.

The Model Context Protocol (MCP) specification version 2025-03-26 (https://modelcontextprotocol.io) defines a JSON-RPC 2.0 based protocol for exposing tools, resources, and prompts to LLM agents. MCP defines `tools/list` (enumerate available tools with JSON Schema descriptions) and `tools/call` (invoke a tool with named parameters and receive a structured result). The protocol uses Server-Sent Events (SSE) for streaming responses and defines `elicitation/create` for human-input requests and `sampling/createMessage` for LLM completion requests from the server to the client.

Elicitation (MCP `elicitation/create`) and sampling (MCP `sampling/createMessage`) are required capabilities in this decision because they enable the agentic execution patterns that make MCP a compelling protocol — without them, MCP reduces to a more complex form of JSON-RPC with no additional value over Option B.

The decision to require `stream_response: true` for elicitation-capable and sampling-capable functions follows from the protocol: elicitation and sampling requests are delivered inline on the SSE stream; a non-streaming JSON-RPC response has no channel for mid-execution server-to-client requests.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-runtime-fr-runtime-authoring` — BR-031: function definitions must be reliably usable by automated tools including LLMs; MCP exposure via `traits.mcp` satisfies this directly by making functions self-describing tools in MCP's `tools/list`
* `cpt-cf-serverless-runtime-fr-execution-engine` — BR-004: synchronous invocation is a first-class feature; JSON-RPC non-streaming mode provides synchronous response semantics for direct callers
* `cpt-cf-serverless-runtime-fr-runtime-capabilities` — BR-008: workflows/functions can invoke runtime-provided capabilities; sampling enables functions to invoke LLM capabilities provided by the connecting agent client
* `cpt-cf-serverless-runtime-nfr-security` — both JSON-RPC and MCP surfaces must enforce the same authentication, authorization, and tenant isolation as the REST API; the Invocation Engine delegation model ensures this
* `gts.x.core.serverless.jsonrpc_traits.v1~` — design entity controlling JSON-RPC exposure for a function definition
* `gts.x.core.serverless.mcp_traits.v1~` — design entity controlling MCP tool exposure, elicitation capability, and sampling capability for a function definition
* `gts.x.core.serverless.mcp_session.v1~` — design entity representing MCP protocol session state per client connection
* `gts.x.core.serverless.mcp_elicitation_context.v1~` — design entity for elicitation request parameters passed from executor to MCP server layer
* `gts.x.core.serverless.mcp_sampling_context.v1~` — design entity for sampling request parameters passed from executor to MCP server layer
