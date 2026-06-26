---
status: accepted
date: 2026-06-25
---
# Pluggable Function Tools via a Generic Registry (exa.ai Web Search)

**ID**: `cpt-cf-mini-chat-adr-pluggable-function-tools`

## Context and Problem Statement

mini-chat already supports one custom LLM function tool — `search_knowledge` (RAG). Its
handling is hard-coded in the agentic provider loop: the loop matches the tool name literally
(`if name == "search_knowledge" { … } else { unexpected_tool_use → fail }`). Adding a second
tool (web search via exa.ai, requested for specific models) by copying that branch would
duplicate the per-tool limit, replay, and graceful-degradation logic for every future tool.

We need a way to (a) add new function tools without touching the loop, (b) enable them
per-model from configuration, and (c) call third-party tool providers (exa.ai) safely.

## Decision Drivers

* Extensibility — new tools should not require new branches in the agentic loop.
* Per-model control — only models that opt in (config) should advertise a given tool.
* Egress & credential management — third-party calls must be auditable and key-managed.
* Blast radius — avoid destabilising the working `search_knowledge` RAG path.

## Considered Options

* **A. Generic `FunctionTool` registry** — a domain trait + a `HashMap<name, Arc<dyn
  FunctionTool>>`; the loop dispatches `function_call`s by name.
* **B. Hard-coded branch per tool** — add an `exa_search` branch beside `search_knowledge`.
* **C. Direct HTTP from the tool** vs. **egress through OAGW** (orthogonal sub-decision).

## Decision Outcome

Chosen: **A (generic registry)** with **egress through OAGW**.

* A domain port `FunctionTool { name, definition() -> LlmFunctionDef, system_prompt_guard(),
  max_calls(), execute() }` decouples the loop from concrete tools. The loop keeps the existing
  `search_knowledge` branch and adds **one** generic fall-through that looks the tool up in the
  per-request registry, enforces `max_calls()` with the same soft-limit notice, executes, and
  injects the `function_call_output`.
* Per-model enablement is a new `enabled_function_tools: Vec<String>` on `ModelCatalogEntry`
  (CCM/policy-snapshot driven, `#[serde(default)]`). `StreamService` resolves the model's list
  against the registry per request.
* `exa_search` is the first registered tool. It reaches exa.ai **through OAGW** (host + apikey
  auth plugin injecting `x-api-key`), reusing the existing `RagHttpClient` JSON-POST primitive —
  consistent with how LLM providers and RAG egress already work
  (`gears/model-registry/docs/ADR/0003-…-oagw-provider-access`).
* Token accounting reuses the existing flat `tool_surcharge_tokens`: when a model has any
  enabled function tool, preflight sets `tools_enabled = true`. No per-tool surcharge for now.
* The function descriptor authoring type is a new function-only `LlmFunctionDef`
  (`definition()` returns it; `assemble_context` wraps it into `LlmTool::Function`).

### Consequences

* Good — new tools are added by implementing `FunctionTool` and registering; the loop is closed
  to modification.
* Good — per-model gating lives in the policy catalog alongside other model capabilities.
* Good — exa credentials stay in OAGW; calls are audited via the existing egress path.
* Good — `search_knowledge` is untouched, so the RAG path is not destabilised.
* Bad / accepted trade-off — two mechanisms temporarily coexist: `search_knowledge` keeps its
  bespoke branch (per-tool metrics, DB increments, `search_result` blocks) while new tools use
  the registry. **Follow-up (phase 2):** migrate `search_knowledge` into the registry and
  collapse `LlmTool::Function { … }` into `Function(LlmFunctionDef)` so there is one tool type
  and one dispatch path.
* Bad / accepted — token cost of a tool's *output* is bounded by config (`max_chars`), not
  preflighted (same as `search_knowledge` today).

### Confirmation

* Unit tests: `assemble_context` appends the tool descriptor + guard; `resolve_function_tools_from`
  filters by the model's `enabled_function_tools`; `ExaSearchTool::format_results` against a
  recorded exa response.
* Code review: the agentic loop has exactly one generic dispatch branch; unknown tool names
  still fail via `unexpected_tool_use`.
* E2E: a model with `enabled_function_tools: ["exa_search"]` triggers a web-search tool call;
  a model without the flag does not advertise the tool.

## Pros and Cons of the Options

* **A. Generic registry** — Good: extensible, testable, single dispatch path. Bad: one
  indirection layer; interim coexistence with the `search_knowledge` branch.
* **B. Hard-coded branch** — Good: trivial for one tool. Bad: duplicates limit/replay/
  degradation logic per tool; the loop grows with every tool.
* **C. Direct HTTP** — Good: fewer moving parts. Bad: bypasses centralised egress, credential
  management, and audit; inconsistent with the rest of the stack. Rejected in favour of OAGW.
