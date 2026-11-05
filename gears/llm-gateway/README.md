# LLM Gateway

Unified interface for LLM inference across providers. Stateless, pass-through design.

## Status

The core gear (`llm-gateway/`) is implemented in slices; see [DESIGN.md](docs/DESIGN.md) for the details of each part.

- **Implemented**: Open Responses REST API (sync create-response, SSE streaming, embeddings), request pipeline with PEP authorization, model resolution, provider plugin routing, capability and parameter checks, tool schema resolution, structured output validation, media input checks and the media resolver (inline data; no network access), AI credit quota (lease admission, settle), usage reporting to Usage Collector, timeouts (provider, stream idle and maximum duration, per-dependency), in-flight limits and graceful shutdown.
- **Deviation: keyless Credential Broker.** The broker fetches no provider keys from CredStore: until `ModelInfoV1` exposes a secret reference every model is treated as keyless (authentication is handled by OAGW). The key path is target design (DESIGN § 4.9, § 4.10).
- **Pending (slice 6b)**: the OAGW-backed media fetcher through a configured route. Until then a byte-only plugin given an `https` media URL gets `capability_not_supported`.
- **Not implemented**: provider fallback, pre/post-response interceptors, rate limiting, batch, audit events and the P1 modalities marked below as open.

## Capabilities

### P1 — Core

- [x] Chat completion (sync and streaming)
- [x] Embeddings generation
- [x] Vision (image analysis) — inline data; URL input pending slice 6b
- [ ] Image generation
- [ ] Speech-to-text (transcription)
- [ ] Text-to-speech (synthesis)
- [ ] Video understanding
- [ ] Video generation
- [x] Document understanding — inline data; URL input pending slice 6b
- [x] Tool/function calling
- [x] Structured output (JSON mode)
- [ ] Async jobs (long-running operations)
- [ ] Realtime audio (WebSocket)
- [x] Usage tracking

### P2 — Reliability & Governance

- [ ] Provider fallback
- [x] Timeout enforcement
- [ ] Pre-call interceptor
- [ ] Post-response interceptor
- [x] Per-tenant budget enforcement
- [ ] Rate limiting (tenant/user)

### P3 — Optimization

- [ ] Batch processing

### P4 — Enterprise

- [ ] Audit events

## Gear Structure

```plaintext
gears/llm-gateway/
├── docs/                    # Documentation
│   ├── PRD.md
│   ├── DESIGN.md
│   └── ADR/
├── llm-gateway-sdk/         # Public API traits, models, errors
├── llm-gateway/             # Core gear implementation
└── plugins/                 # (planned)
    ├── providers/
    │   ├── openai_plugin/       # OpenAI-compatible providers
    │   ├── anthropic_plugin/    # Claude API
    │   └── ollama_plugin/       # Local models via Ollama
    ├── hooks/
    │   ├── noop_hook_plugin/    # Default no-op (passthrough)
    │   └── ...                  # Custom hook plugins
    ├── usage/
    │   ├── noop_usage_tracker/  # Default no-op
    │   └── ...                  # Custom usage tracking
    └── audit/
        ├── noop_audit_gear/   # Default no-op
        └── ...                  # Custom audit logging
```

## Documentation

- [PRD.md](docs/PRD.md) — Product requirements, use cases, acceptance criteria
- `DESIGN.md` — Technical architecture, components, sequence diagrams
- API.md — SDK traits, request/response models, errors (see [`llm-gateway-sdk/src/lib.rs`](llm-gateway-sdk/src/lib.rs) for rustdoc)
- PROVIDERS.md — Provider abstraction, capability matrix `TODO`
- CONFIGURATION.md — Gateway and plugin configuration `TODO`

## Dependencies

| Gear | Role |
|--------|------|
| Model Registry | Model catalog, availability checks |
| Outbound API Gateway | External API calls to providers |
| FileStorage | Fetch input media, store generated content |
| Type Registry | Read GTS schemas by ID (tool definitions) |

## Consumers

| Gear | Usage |
|--------|-------|
| Chat Engine | Response generation |
| RAG | Embeddings for semantic search |
