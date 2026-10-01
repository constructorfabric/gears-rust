---
status: accepted
date: 2026-09-26
---

# Multiple LLM provider adapters with gear-provisioned OAGW upstreams

**ID**: `cpt-cf-mini-chat-adr-multi-provider-adapters`

## Context and Problem Statement

The P1 design assumed a single OpenAI-compatible provider (OpenAI or Azure OpenAI Responses API) and an OAGW that operators pre-configure with fixed `/outbound/llm/*` routes. DESIGN §2.2 and PRD §4.2 listed "multi-provider support (e.g. Anthropic)" as deferred.

The implementation went further, and this ADR records what was built:

* Mini Chat serves models from several provider APIs.
* Mini Chat registers its own OAGW upstreams and routes at startup.
* Mini Chat routes file and vector-store operations to a storage-capable provider, even when the chat model's own provider has no such API.

## Decision Drivers

* The model catalog must offer models from more than one vendor without a separate service per vendor.
* Credentials never enter Mini Chat. OAGW injects them (`cpt-cf-mini-chat-constraint-no-credentials`).
* Deployments must not depend on hand-maintained OAGW route tables that drift from Mini Chat config.
* RAG (file upload, vector store, `file_search`) must keep working for providers without file APIs.

## Considered Options

* Keep a single OpenAI-compatible adapter and require operators to configure OAGW.
* In-process adapter per provider kind, with provider entries in Mini Chat config and OAGW upstreams provisioned by the gear.

## Decision Outcome

Chosen option: "In-process adapter per provider kind, with provider entries in Mini Chat config and OAGW upstreams provisioned by the gear".

**Adapters.** `ProviderKind` (`mini-chat/src/infra/llm/providers/mod.rs`) selects the adapter. The kinds are:

* `openai_responses` — OpenAI and Azure OpenAI Responses API;
* `openai_chat_completions` — Chat Completions API;
* `vllm_responses` — vLLM Responses API;
* `anthropic_messages` — Anthropic Messages API.

Each catalog model names its `provider_id`, which points at a `providers.<id>` entry.

**Tool support per adapter.** The Chat Completions adapter drops `file_search`, `web_search` and `code_interpreter` and keeps function tools. The vLLM Responses adapter drops all tools. The Anthropic adapter drops `file_search`. The domain service builds the tool list, tool guards, reserve surcharges and daily tool quota checks without knowing the adapter kind; the `web_search` tool is gated only by the catalog `tool_support.web_search`. On an adapter that drops a tool, the surcharge is still reserved, the guard is still sent and the daily quota is still checked, so the catalog `tool_support` must match the adapter.

**Provider entries.** Each entry (`mini-chat/src/config.rs`, `ProviderEntry`) has these fields:

* `kind`, `host`, `port`, `use_http`, `upstream_alias`, `api_path` (with a `{model}` placeholder);
* `auth_plugin_type` and `auth_config` for the OAGW auth plugin;
* `storage_kind`, `storage_backend`, `api_version` (required for `storage_kind = azure`; validated at startup);
* `rag_provider`, the provider used for file and vector-store operations when the LLM provider has none (Anthropic);
* per-tenant `tenant_overrides` (`host`, `upstream_alias`, `auth_plugin_type`, `auth_config`).

`host` and `auth_config` support `${VAR}` expansion, both on the entry and in a tenant override.

**OAGW provisioning.** In `start()` the gear obtains an S2S token via `authn_resolver` client credentials. It then registers an OAGW upstream and route for every provider entry (`mini-chat/src/infra/oagw_provisioning.rs`):

* `init()` fills `upstream_alias` with the host when it is not configured, so the alias is always passed to OAGW. The upstream is created, or reused when it already exists, under that alias. `ProviderResolver` is built in `init()` from these entries and routes by that alias. Registration runs on a copy of the entries: the alias OAGW returns is written into the copy, and the resolver does not see it.
* A deterministically misconfigured entry fails startup.
* An entry whose credstore secret is not yet readable is retried by a background reconcile loop.

Requests are sent through the in-process `oagw_sdk::ServiceGatewayClientV1::proxy_request` to `{alias}{api_path}`.

**Storage dispatch.** `DispatchingFileStorage` and `DispatchingVectorStore` pick the file and vector-store implementation by the provider's `storage_kind`. For chats whose model is served by an `anthropic_messages` provider, uploaded images also get a secondary copy in the Anthropic Files API (`attachments.secondary_*` columns). Documents get no copy, and an image larger than `thumbnail.max_decode_bytes` gets no copy (its bytes are not kept in memory).

The gear therefore declares `deps = [types_registry, authn_resolver, authz_resolver, oagw]` and `capabilities = [db, rest, stateful]`.

### Consequences

* Good, because new vendors are added by an adapter and a config entry, not a new service.
* Good, because OAGW routes cannot drift from Mini Chat config: the gear owns them.
* Good, because credentials stay in OAGW and credstore; Mini Chat only holds references.
* Bad, because the gear now depends on `authn_resolver` (S2S) and `types_registry` at startup, and a misconfigured provider fails the boot.
* Bad, because adapter behaviour differs by vendor. In particular, Anthropic does not get native `file_search` (see `cpt-cf-mini-chat-adr-document-retrieval-scope`), and the Chat Completions and vLLM adapters drop built-in tools while their surcharges and quotas still apply.
* Neutral, because `llm_provider` stays a library inside the gear (`cpt-cf-mini-chat-adr-llm-provider-as-library`); only its adapter set grew.

### Confirmation

* Unit tests per adapter: `openai_responses_tests.rs` and `vllm_responses_tests.rs` in `mini-chat/src/infra/llm/providers/`; the Chat Completions and Anthropic adapters have inline test modules in `openai_chat.rs` and `anthropic_messages.rs`.
* Provisioning tests in `mini-chat/src/infra/oagw_provisioning.rs`.
* E2E tests that use the `provider` fixture (directly or via `provider_chat`) run against both configured providers, `openai` and `azure` (`testing/e2e/suites/mini_chat/conftest.py`). Both E2E providers use `kind: openai_responses` (`config/base.yaml`); the Chat Completions, vLLM and Anthropic adapters have no E2E coverage.

## Pros and Cons of the Options

### Single adapter, operator-configured OAGW

* Good, because it is simpler and matches the original P1 plan.
* Bad, because it blocks non-OpenAI-compatible vendors and couples every deployment to a manual route table.

### Adapter per provider kind, gear-provisioned upstreams

* Good, because it matches the implemented system and the multi-vendor catalog.
* Bad, because it adds startup dependencies and per-vendor behaviour differences.

## More Information

* Supersedes the "multi-provider support is deferred" statements in DESIGN §2.2 and §4 "P1 Scope Boundaries", and the Anthropic out-of-scope item in PRD §4.2.
* Anthropic-specific details: [features/anthropic-provider-support.md](../features/anthropic-provider-support.md).

## Traceability

* **PRD**: [PRD.md](../PRD.md)
* **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-mini-chat-component-llm-provider` — provider adapters and dispatch.
* `cpt-cf-mini-chat-constraint-openai-compatible` — relaxed: providers are adapter-based, not OpenAI-compatible only.
* `cpt-cf-mini-chat-constraint-no-credentials` — credentials stay in OAGW and credstore.
* `cpt-cf-mini-chat-design-model-catalog` — each catalog entry references a `provider_id`.
