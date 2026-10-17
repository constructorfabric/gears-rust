# Feature: Model Client


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Call the Model](#call-the-model)
  - [Read the Chat Completions Answer](#read-the-chat-completions-answer)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Model Interface](#model-interface)
  - [Chat Completions Adapter](#chat-completions-adapter)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-model-client-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-model-client`

## 1. Feature Context

### 1.1 Overview

The model client is the one small interface through which Construct calls language models. The planner and the
sensitive-data checks use it. This document covers the interface and its first adapter, the OpenAI chat completions
API through OAGW. Building the client from the gear's configuration comes with the change that wires the planner in.

### 1.2 Purpose

The rest of Construct must not depend on one model service (`cpt-cf-construct-adr-one-model-interface`). The
interface takes messages, tools and the kind of answer wanted, and returns text, tool calls or a structured value,
with the tokens the call used. The planner needs the tool calls for its agent loop and the tokens for its token cap.
A sensitive-data check needs a structured answer.

**Out of scope**: the prompts, the planner loop and the sensitive-data checks; approving models or endpoints, which
belongs to the LLM gateway; provisioning the OAGW upstream and its credentials, which the deployment does; the LLM
gateway adapter, which comes when the gateway has an implementation; the configuration key and the factory, which come
with their first caller.

**Requirements**: `cpt-cf-construct-fr-fact-decisions` (supported, owned by the Planner)

**Principles**: `cpt-cf-construct-principle-one-model-interface`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-llm-gateway` | The model service behind the interface: the LLM gateway, or any service with the OpenAI chat completions API that the deployment sets. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md), component `cpt-cf-construct-component-model-client`
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.2
- **ADR**: [ADR-0003](../ADR/0003-cpt-cf-construct-adr-one-model-interface.md)
- **Dependencies**: `cpt-cf-construct-feature-gear-foundation`; the OAGW client in ClientHub

## 2. Actor Flows (CDSL)

**Use cases**: None. The model client is internal; no route and no actor call it directly.

## 3. Processes / Business Logic (CDSL)

### Call the Model

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-model-client-call`

1. [ ] - `p1` - The caller builds a request: messages (system, user, an earlier assistant answer with its tool calls,
   tool results), tool specs (name, description, JSON schema of the arguments), the kind of answer (text, or a
   structured value with a name and a schema) and an optional limit on output tokens - `inst-call-request`
2. [ ] - `p1` - The chat completions adapter resolves the OAGW client from ClientHub on each call. Without it, the
   call fails as unavailable - `inst-call-resolve`
3. [ ] - `p1` - It sends one non-streaming POST to `/{upstream_alias}` through OAGW, in the chat completions form:
   tools as functions, a structured answer as `response_format` with a strict JSON schema, the limit as
   `max_completion_tokens` - `inst-call-send`
4. [ ] - `p1` - The whole call has the timeout the client is built with. Past it, the call fails as a timeout -
   `inst-call-timeout`
5. [ ] - `p1` - HTTP 429 and 5xx mean the model service is unavailable; any other non-success status means it refused
   the request - `inst-call-status`
6. [ ] - `p1` - An OAGW failure is read as the gateway's error kind: a timeout is a timeout; a rate limit or an
   unavailable upstream is unavailable; every other kind is refused. The error names the kind only, never the
   gateway's detail - `inst-call-gateway`

### Read the Chat Completions Answer

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-model-client-read-answer`

1. [ ] - `p1` - A refusal or the `content_filter` finish reason means the model refused - `inst-read-refused`
2. [ ] - `p1` - The `length` finish reason means the answer was cut off, so it is unreadable, never a complete answer
   - `inst-read-length`
3. [ ] - `p1` - Tool calls come back with their arguments parsed as JSON. Arguments that are not JSON make the answer
   unreadable. `"tool_calls": null` is no tool calls - `inst-read-tool-calls`
4. [ ] - `p1` - Otherwise the content is the text, or, for a structured answer, a JSON value. Empty content is
   unreadable - `inst-read-content`
5. [ ] - `p1` - The usage gives the input and output tokens. An answer without usage reports no usage, which is not
   zero tokens - `inst-read-usage`
6. [ ] - `p1` - No error carries the prompt, the answer's text or the gateway's detail: errors are fixed texts that
   name the status, the kind or the broken part only - `inst-read-no-content`

## 4. States (CDSL)

None. A call has no state of its own.

## 5. Definitions of Done

### Model Interface

- [x] `p1` - **ID**: `cpt-cf-construct-dod-model-client-interface`

The system **MUST** offer one model interface, `ModelClient::complete`, with its own small request and answer types,
and one error type: timeout, unavailable, refused, unreadable answer.

**Implements**:
- `cpt-cf-construct-algo-model-client-call`

**Touches**:
- Code: `domain::model_client`

**Verified by**: the adapter tests below, which use the interface only.

### Chat Completions Adapter

- [x] `p1` - **ID**: `cpt-cf-construct-dod-model-client-chat-completions`

The system **MUST** call an OpenAI-compatible chat completions server through OAGW, and **MUST** read tool calls,
structured answers, refusals and usage from its answer.

**Implements**:
- `cpt-cf-construct-algo-model-client-call`
- `cpt-cf-construct-algo-model-client-read-answer`

**Touches**:
- Code: `infra::model::chat_completions::ChatCompletionsModel`

**Verified by**: `infra::model::chat_completions_test`, against a fake OAGW client and committed chat completions
answers.

## 6. Acceptance Criteria

- [ ] A chat completions model answers with text, tool calls or a structured value, and reports the tokens used when
      the model service reports them.
- [ ] A slow, unavailable or refusing model service fails the call with the matching error, without any prompt or
      answer text.

## 7. Non-Functional Considerations

- **Privacy**: the record content and the numbered profile reach the configured model service, which is the
  tenant's sub-processor (PRD). The model client logs and returns neither.
- **Timeouts**: one timeout per call, given when the client is built. The planner's caps on rounds and tokens are the
  planner's.
