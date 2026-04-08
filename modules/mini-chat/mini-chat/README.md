# Mini Chat

Multi-tenant AI chat module with SSE streaming, credit-based quota enforcement, and pluggable model policy.

## Overview

The `cf-mini-chat` module provides:

- **Chat management** — CRUD for chats, turns, messages, and attachments with per-tenant isolation
- **SSE streaming** — real-time token streaming from LLM providers via OAGW proxy
- **Credit quota** — preflight reservation, actual settlement, and tier-based downgrade using integer micro-credit arithmetic
- **Policy plugin** — discovers `minichat-policy-plugin` via types-registry for model catalog, kill switches, and per-user limits
- **File search / RAG** — document upload, chunking, vector-store retrieval per turn
- **Web search** — optional per-request web search with daily quota
- **ClientHub integration** — registers services for inter-module use

Dependencies: `types-registry`, `authz-resolver`, `oagw`.

## License

Apache-2.0
