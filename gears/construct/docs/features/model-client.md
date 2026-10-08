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
  - [Adapter Chosen by Configuration](#adapter-chosen-by-configuration)
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
API through OAGW. The LLM gateway adapter follows in a later change, behind the same interface.

### 1.2 Purpose

The rest of Construct must not depend on one model service (`cpt-cf-construct-adr-one-model-interface`). The
interface takes messages, tools and the kind of answer wanted, and returns text, tool calls or a structured value,
with the tokens the call used. The planner needs the tool calls for its agent loop and the tokens for its token cap.
A sensitive-data check needs a structured answer.

**Out of scope**: the prompts, the planner loop and the sensitive-data checks; approving models or endpoints, which
belongs to the LLM gateway; provisioning the OAGW upstream and its credentials, which the deployment does; the LLM
gateway adapter, which comes next.

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
4. [ ] - `p1` - The whole call has a timeout from the configuration. Past it, the call fails as a timeout -
   `inst-call-timeout`
5. [ ] - `p1` - HTTP 429 and 5xx mean the model service is unavailable; any other non-success status means it refused
   the request - `inst-call-status`

### Read the Chat Completions Answer

- [ ] `p1` - **ID**: `cpt-cf-construct-algo-model-client-read-answer`

1. [ ] - `p1` - A refusal or the `content_filter` finish reason means the model refused - `inst-read-refused`
2. [ ] - `p1` - Tool calls come back with their arguments parsed as JSON. Arguments that are not JSON make the answer
   unreadable - `inst-read-tool-calls`
3. [ ] - `p1` - Otherwise the content is the text, or, for a structured answer, a JSON value - `inst-read-content`
4. [ ] - `p1` - The usage gives the input and output tokens - `inst-read-usage`
5. [ ] - `p1` - No error carries the prompt or the answer's text: errors name the status or the broken part only -
   `inst-read-no-content`

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
answers: the request form, a structured answer's schema, parsed tool calls and usage, a structured answer, a refusal,
429, 502 and 400, an unreadable answer, arguments that are not JSON, a timeout, no OAGW client, and no prompt or
answer text in an error.

### Adapter Chosen by Configuration

- [x] `p1` - **ID**: `cpt-cf-construct-dod-model-client-config`

The system **MUST** choose the adapter by configuration only: `model.adapter` with its own keys. An unknown adapter or
key **MUST** stop the gear from starting.

**Implements**:
- `cpt-cf-construct-algo-model-client-call`

**Touches**:
- Code: `config::ModelConfig`, `infra::model::model_client`

**Verified by**: the config tests `the_chat_completions_model_is_read_with_a_default_timeout` and
`an_unknown_adapter_or_model_key_is_rejected`, and `the_configured_adapter_calls_the_configured_upstream_and_model`.

## 6. Acceptance Criteria

- [ ] A configured chat completions model answers with text, tool calls or a structured value, and reports the
      tokens used.
- [ ] A slow, unavailable or refusing model service fails the call with the matching error, without any prompt or
      answer text.
- [ ] Changing the adapter needs a configuration change only.

## 7. Non-Functional Considerations

- **Privacy**: the record content and the numbered profile reach the configured model service, which is the
  tenant's sub-processor (PRD). The model client logs and returns neither.
- **Timeouts**: one timeout per call, 30 seconds by default (`model.timeout_ms`). The planner's caps on rounds and
  tokens are the planner's.
