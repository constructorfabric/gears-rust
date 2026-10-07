# Mini Chat

Multi-tenant AI chat gear with SSE streaming, credit-based quota enforcement, and pluggable model policy.

## Overview

The `cf-gears-mini-chat` gear provides:

- **Chat management** — CRUD for chats, turns, messages, and attachments with per-tenant isolation
- **SSE streaming** — real-time token streaming from LLM providers via OAGW proxy
- **Credit quota** — preflight reservation, actual settlement, and tier-based downgrade using integer micro-credit arithmetic
- **Policy plugin** — resolves the model policy plugin (`MiniChatModelPolicyPluginSpecV1`) via types-registry for model catalog, kill switches, and per-user limits
- **Audit plugin** — delivers audit events to the audit plugin (`MiniChatAuditPluginSpecV1`) resolved via types-registry
- **File search / RAG** — document upload, chunking, vector-store retrieval per turn
- **Web search** — optional per-request web search with daily quota

The gear exposes a REST API only; it registers no clients in ClientHub.

The crate also ships two bundled plugins in `src/infra/plugins/`: `static_model_policy` (gear `static-mini-chat-model-policy-plugin`, serves the model catalog and limits from its config) and `static_audit` (gear `static-mini-chat-audit-plugin`, logs audit events).

Dependencies: `types-registry`, `authn-resolver`, `authz-resolver`, `oagw`.

Upload size: the gear sets no request body limit; api-gateway `defaults.body_limit_bytes` (default 16 MiB) caps uploads. To accept 25 MiB documents (`rag.uploaded_file_max_size_kb` default) set it to at least 25 MiB + 64 KiB (26,279,936 bytes), otherwise the gateway answers 413 before the request reaches mini-chat.

## License

Apache-2.0
