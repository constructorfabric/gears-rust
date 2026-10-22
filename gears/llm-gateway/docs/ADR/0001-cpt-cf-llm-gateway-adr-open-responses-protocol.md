---
description: "Use Open Responses as the single public protocol of the LLM Gateway gear for REST and SDK, with Gateway-owned types under the `cf_gears:` prefix."
status: accepted
date: 2026-10-19
---
# Open Responses as the Single Public Protocol for REST and SDK


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Decision Scope](#decision-scope)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
  - [Review and Supersession](#review-and-supersession)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Open Responses as the single public contract, with Gateway-owned types under the `cf_gears:` prefix](#open-responses-as-the-single-public-contract-with-gateway-owned-types-under-the-cf_gears-prefix)
  - [OpenAI Chat Completions as the public contract](#openai-chat-completions-as-the-public-contract)
  - [A custom platform schema](#a-custom-platform-schema)
  - [Open Responses plus a Chat Completions compatibility layer](#open-responses-plus-a-chat-completions-compatibility-layer)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-llm-gateway-adr-open-responses-protocol`
## Context and Problem Statement

The LLM Gateway gives platform gears one API over many LLM providers. It needs one public request, response, and streaming contract that both REST clients and Rust consumers use. The contract must carry text, tool calls, reasoning, and provider-specific features, and it must stay stable while providers change. Which protocol should be the public contract, and how should Gateway-specific features be added to it?

## Decision Drivers

* Backward compatibility within a major version (`cpt-cf-llm-gateway-nfr-compatibility-v1`, a p2 requirement).
* One contract for REST and SDK, so the REST layer is a projection of the SDK models (`cpt-cf-llm-gateway-principle-sdk-first-v1`).
* Provider-specific items, tools, and parameters must pass through without loss (`cpt-cf-llm-gateway-principle-normalize-and-preserve-v1`).
* The Gateway is stateless and does not rely on provider-side conversation state, so the protocol must work with the full input sent on each request.
* Streaming must use typed, ordered events that relay without buffering whole responses.
* Reuse of an existing open specification and its client tooling, instead of a new schema that the platform maintains alone.
* Consumers that use OpenAI-style APIs today (llm-connectors) should have a short path to the Gateway.

## Considered Options

* Open Responses as the single public contract, with Gateway-owned types under the `cf_gears:` prefix.
* OpenAI Chat Completions as the public contract.
* A custom platform schema.
* Open Responses plus a Chat Completions compatibility layer in the Gateway.

## Decision Outcome

Chosen option: "Open Responses as the single public contract, with Gateway-owned types under the `cf_gears:` prefix", because it is the only option that gives typed items, typed streaming events, and a defined extension path in an existing open specification, without a second protocol for the Gateway to maintain. The Chat Completions compatibility layer is not adopted now. It stays open as a separate decision tied to llm-connectors migration planning.

### Decision Scope

This ADR decides:

* Open Responses is the wire contract of the Gateway for create response (sync and streaming), on REST and in the SDK.
* Embeddings use a Gateway-defined request and response (the SDK `EmbeddingRequest` and `EmbeddingResponse`) on `/llm-gateway/v1/embeddings`, outside the Open Responses specification, and follow the same versioning and `cf_gears:` extension rules.
* Gateway namespace: Gateway-owned types, tools, items, and stream events use it (for example `cf_gears:web_search` and `cf_gears:response.data.done`), so they cannot collide with Open Responses or provider names. Gateway extension fields on Open Responses objects (such as `ignored_parameters`) are additive optional fields defined in the SDK models.
* Input items, output items, tools, and stream events of types the Gateway does not know are carried as `Other` (`Extension`) values in the SDK models and relayed unchanged, never dropped or rewritten. Provider-specific request parameters travel in `extra_fields`, subject to the model's `allow_extra_params`.
* The contract evolves by additive changes inside a version; breaking changes need a new version.

This ADR does not decide:

* The error envelope. RFC 9457 Problem with the Open Responses `code` is a separate ADR.
* Whether a Chat Completions compatibility layer is built, and when.
* The Open Responses specification revision to pin.

### Consequences

* REST paths are versioned (`/llm-gateway/v1/...`) and SDK traits carry the `V1` suffix. A breaking change creates a new version beside the old one.
* Within a version, changes are additive only: new optional fields, new event types, new extension types. Consumers must ignore fields and event types they do not know.
* Gateway-specific behavior is expressed as additive fields and `cf_gears:` types inside the Open Responses model. Embeddings are the only Gateway-defined operation outside Open Responses, and they follow the same versioning and `cf_gears:` extension rules.
* Items, tools, and stream events of unknown types are carried as `Other` values through request, plugin, and response paths unchanged, in line with normalize-and-preserve. Unknown top-level request fields are not part of the contract and are ignored on deserialization.
* The SDK models are the normative form of this contract and the REST layer projects them (`cpt-cf-llm-gateway-principle-sdk-first-v1`).
* The Gateway tracks one Open Responses specification revision. The revision to pin cannot be decided from the current sources and is a follow-up. Until it is pinned, fixtures and conformance checks have no fixed reference.
* Async (background) responses are P2 and outside the P1 DESIGN. Open Responses names the flag `background`, and the PRD and DESIGN use that name when async jobs are designed; PRD UC-013 currently says `async=true`, and aligning it is a P2 follow-up.
* Stateless operation follows from the protocol use: `previous_response_id` and `store` are not part of the Gateway request model, and the SDK input item `item_reference` (a reference to a stored item) is rejected with `capability_not_supported`, because the Gateway keeps no state.
* Gateway additions to Open Responses objects (ignored parameters, provider details, AI credit cost, completeness flags) are additive optional fields in the SDK models; provider-specific parameters use the SDK `extra_fields` mechanism.
* Existing consumers of llm-connectors use an OpenAI Chat Completions style API and need a client change to call the Gateway. The size of that change, and whether a compatibility layer is needed, is an open question in PRD § 13 and DESIGN § 4.10.
* Performance: relaying typed events adds no buffering of whole responses; no measurable cost is expected beyond mapping at the plugin boundary.
* Security: the typed contract gives one place to validate input; the verbatim preservation rule means unknown content is relayed, so it must not be interpreted or logged in full by the Gateway.
* Reliability: additive-only evolution lets old and new instances serve the same clients during rolling upgrades.
* Data: P1 adds no persistent data. From P2, async and batch job records persisted by the Gateway on behalf of consumers hold Open Responses shaped results.
* Integration: provider plugins map each provider API to and from this contract; the mapping effort is paid once in the plugin, not by every consumer.
* Operations: contract changes are reviewed as API changes and released with a version policy.
* Testing: conformance can be tested with recorded fixtures, see Confirmation.
* Compliance, UX, and business: not applicable beyond the above; the decision adds no data processing or user interface, and its business effect is limited to the migration cost noted above.

### Confirmation

* SDK tests serialize the request, response, and streaming event models to, and deserialize them from, recorded Open Responses payloads for create response, sync and streaming.
* Code review of the SDK models confirms that every Gateway-owned tool, item, and stream event type uses the `cf_gears:` prefix.
* REST paths are registered under `/llm-gateway/v1`, and the OpenAPI document is generated from the SDK models.

### Review and Supersession

Review this ADR when a new Open Responses revision breaks compatibility with the pinned one, when the Chat Completions compatibility question is resolved, or when a second public protocol is proposed. A change of protocol supersedes this ADR through a new ADR.

## Pros and Cons of the Options

### Open Responses as the single public contract, with Gateway-owned types under the `cf_gears:` prefix

One protocol on REST and in the SDK; Gateway-owned types use the `cf_gears:` prefix.

* Good, because typed items and typed streaming events cover text, tool calls, and reasoning in one model.
* Good, because the specification defines extension points, so Gateway features do not need a private protocol.
* Good, because one contract for REST and SDK removes drift between them.
* Good, because the protocol does not require provider-side state, which fits the stateless Gateway.
* Neutral, because it is newer than Chat Completions, so fewer third-party clients exist today.
* Bad, because the Gateway depends on an external specification and must track its revisions.
* Bad, because consumers that speak Chat Completions must change their client.

### OpenAI Chat Completions as the public contract

The public API follows the Chat Completions request, response, and chunk format.

* Good, because it is widely known and has many client libraries.
* Good, because llm-connectors consumers would need little change.
* Bad, because messages and chunks do not model typed items such as reasoning and provider tool calls without vendor-specific fields.
* Bad, because the format is shaped around one vendor, and the Gateway would need to invent its own extensions.
* Bad, because newer provider features reach it later than the Responses style API.

### A custom platform schema

The platform defines and owns its request, response, and event schema.

* Good, because the platform controls every detail and its release timing.
* Good, because there is no external specification to track.
* Bad, because the platform must design, document, and maintain the schema and client tooling alone.
* Bad, because every consumer learns a new format and no existing client works.

### Open Responses plus a Chat Completions compatibility layer

Open Responses is the contract, and the Gateway also serves a Chat Completions endpoint that it translates.

* Good, because llm-connectors consumers could migrate without client changes.
* Neutral, because it can be added later without changing the Open Responses contract.
* Bad, because it creates a second public protocol that the Gateway must test, version, and keep in step with the first.
* Bad, because translation between the two formats is lossy for typed items and streaming events.
* Bad, because the need is not confirmed; PRD § 13 leaves it open and the DESIGN excludes it.

## More Information

* Related pending decisions: the Open Responses revision to pin, the error envelope (RFC 9457 Problem with the Open Responses `code`), and the Chat Completions compatibility layer.
* Related DESIGN principles: `cpt-cf-llm-gateway-principle-normalize-and-preserve-v1` and `cpt-cf-llm-gateway-principle-stateless-processing-v1`.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-llm-gateway-nfr-compatibility-v1` (p2) — versioned paths, additive-only evolution, and Open Responses alignment follow from this decision.
* `cpt-cf-llm-gateway-fr-chat-completion-v1` — request and response formats follow the Open Responses contract.
* `cpt-cf-llm-gateway-fr-streaming-v1` — streaming uses Open Responses events.
* `cpt-cf-llm-gateway-fr-embeddings-v1` — embeddings use a Gateway-defined request and response outside Open Responses, under the same versioning and `cf_gears:` extension rules.
* `cpt-cf-llm-gateway-interface-rest-api-v1` — the REST API is a projection of the Open Responses based SDK models.
* `cpt-cf-llm-gateway-interface-client-sdk-v1` — the SDK carries the Open Responses models as the code form of the contract.
* `cpt-cf-llm-gateway-principle-sdk-first-v1` — this decision defines the protocol of the SDK-first contract.
* `cpt-cf-llm-gateway-principle-normalize-and-preserve-v1` — items, tools, and stream events of unknown types are carried as `Other` values and relayed unchanged.
* `cpt-cf-llm-gateway-principle-stateless-processing-v1` — the request model carries no provider-side state fields, and `item_reference` input items are rejected.
* `cpt-cf-llm-gateway-design-llm-gateway-v1` — the public protocol of the overall design.
