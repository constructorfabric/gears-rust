---
status: accepted
date: 2026-09-30
---

# ADR-0003: Construct calls models through one small interface with two adapters

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [One small model interface with two adapters](#one-small-model-interface-with-two-adapters)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-construct-adr-one-model-interface`

## Context and Problem Statement

Construct calls large language models (LLMs) in two places. The planner builds a plan, the set of changes for one record, by running an agent loop: a model calls tools in rounds. The loop runs over the record and the profile, the graph of facts about one subject. The model sees a numbered profile: the profile's values shown as numbers instead of IDs. The sensitive-data checks make one model call for each sensitive-data kind that has a model check, on the values a plan would store; the values go in as a tool call result, not in the prompt. If a model call fails or hits a cap, the record is dropped: a dropped record is a received record from which nothing is stored.

How does Construct call models without tying itself to one model service?

## Decision Drivers

- **Not tied to one service.** The rest of Construct must not depend on one model service.
- **Small and local models must work.** The model never sees an ID, so small or local models can do the job. The interface must therefore also reach local servers, such as vLLM and Ollama, through the chat completions adapter.
- **Outbound calls go through OAGW.** OAGW is the platform's outbound API gateway. It carries every call from a gear to an outside service. The chat completions adapter's calls go through OAGW; the LLM gateway adapter calls the platform's LLM gateway.

## Considered Options

This decision is constrained, and no other option is compared here.

- One small model interface with two adapters

## Decision Outcome

Chosen option: "One small model interface with two adapters", because it keeps the rest of Construct apart from any one model service.

The interface is small: messages and tools go in; text, tool calls or a structured answer come out. An adapter is the code that turns this interface into one service's API. There are two:

- **The chat completions adapter comes first.** It speaks the OpenAI chat completions API, through OAGW. It works with OpenAI, Azure, vLLM, Ollama, LiteLLM and most other servers.
- **The LLM gateway adapter follows when the gateway runs.** It speaks the API of the [LLM gateway](../../../llm-gateway/docs/PRD.md), the platform's model service.

Which adapter is used is configuration.

### Consequences

- The planner and the sensitive-data model checks depend only on the interface. A pattern check calls no model.
- Adding an adapter changes no other component.
- Each record's payload with its schema, and the numbered profile, go to the configured endpoint.
- Every planner round carries the whole current profile. Large profiles make each round heavy, whichever endpoint is configured. The sensitive-data checks see only the values the plan would store.
- Quality lives in the prompts, and in the patterns of the pattern checks. The PRD's reference evaluation set proves that the plan decides well, and the PRD's reference test set proves that the checks find special-category content, whichever endpoint is configured.
- Construct approves no models and no endpoints. Model policy belongs to the LLM gateway. With the chat completions adapter, the deployment's configuration sets the endpoint.

### Confirmation

- A design review checks [DESIGN.md](../DESIGN.md) against this ADR.
- Later, code review checks that only the model client knows which service is behind it.

## Pros and Cons of the Options

### One small model interface with two adapters

The model client offers the interface and holds both adapters. See `cpt-cf-construct-component-model-client`.

- Good, because the planner and the checks never know which service is behind the interface.
- Good, because the chat completions adapter works with most model servers, so Construct runs before the LLM gateway runs.
- Good, because adding an adapter changes no other component.
- Good, because the chat completions adapter also reaches local servers, such as vLLM and Ollama, so small or local models can run the planner and the checks.
- Good, because the chat completions adapter's calls go through OAGW, like every other outbound call on the platform, and the LLM gateway adapter calls the platform's LLM gateway.
- Neutral, because the choice of adapter and endpoint is configuration, not code.
- Bad, because Construct must build and keep two adapters.
- Bad, because record content goes to whichever endpoint the configuration names.

## More Information

- The model client and its callers: [DESIGN.md](../DESIGN.md), sections 3.2 and 3.5.
- Outbound calls: [OAGW DESIGN](../../../system/oagw/docs/DESIGN.md).
- The model service: [LLM gateway PRD](../../../llm-gateway/docs/PRD.md).
- Construct runs the model step itself: `cpt-cf-construct-adr-construct-is-a-gear`.
- Security and data: record content and the numbered profile leave Construct only through the configured adapter. The model never gets an ID.
- Performance: every planner round carries the whole current profile, so large profiles make rounds heavy. The sensitive-data checks see only the values the plan would store.
- Testing: the PRD's reference evaluation set and reference test set score the outcome on the configured endpoint.
- Compliance: Construct approves no models and no endpoints; model policy belongs to the LLM gateway.
- Reliability: if a model call of the planner or of a sensitive-data check fails or hits a cap, the record is dropped with an audit event, and nothing from it is stored ([DESIGN.md](../DESIGN.md), section 3.2). This decision adds no retry or fallback.
- Operations: the adapter and the endpoint are set by configuration.
- If this decision changes, a later ADR supersedes this one.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-construct-fr-fact-decisions` — the planner calls models through the interface to decide fact changes.
- `cpt-cf-construct-fr-sensitive-data-guardrails` — the model checks call models through the same interface.
- `cpt-cf-construct-fr-mcp-manage-facts` — what an agent passes on runs through the same planner and model interface.
- `cpt-cf-construct-nfr-guardrail-detection` — the reference test set scores the checks on the configured endpoint.
- `cpt-cf-construct-principle-one-model-interface` — the principle this decision realizes.
- `cpt-cf-construct-principle-model-never-sees-ids` — lets small or local models do the job.
- `cpt-cf-construct-principle-model-proposes-code-decides` — the model only proposes, whichever service is behind it.
- `cpt-cf-construct-component-model-client` — holds the interface and the two adapters.
- `cpt-cf-construct-component-planner` — depends only on the interface.
- `cpt-cf-construct-component-sensitive-data-checks` — its model checks depend only on the interface.
