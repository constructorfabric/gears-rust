# mini-chat gear

AI chat gear. Provides a REST API for chats, messages with SSE streaming, turns (retry, edit, delete), attachments, reactions, models and quota status.

## Directory Structure

```
gears/mini-chat/
├── mini-chat/          # Main gear crate
│   └── src/
│       ├── api/        # REST handlers, routes, DTOs, SSE
│       ├── domain/     # Business logic, services, repository traits
│       └── infra/      # DB entities/repos, LLM providers, outbox handlers,
│                       # static model-policy and audit plugins (infra/plugins/)
├── mini-chat-sdk/      # SDK crate (contract types, plugin API, GTS IDs)
├── deploy/             # Dockerfile and Helm chart
├── scripts/
│   └── smoke-test-api.py            # API smoke test (stdlib-only Python)
└── docs/               # PRD, DESIGN, ADRs, feature docs, E2E scenarios
```

## Plugins

The SDK defines two plugin specs, resolved through types-registry:

- `MiniChatModelPolicyPluginSpecV1` — model catalog, kill switches, user limits, usage publication.
- `MiniChatAuditPluginSpecV1` — audit events.

Static implementations of both ship in `mini-chat/src/infra/plugins/`.

## LLM Providers

Provider entries in `mini-chat.config.providers` select an adapter by `kind` (`ProviderKind` in `mini-chat/src/infra/llm/providers/mod.rs`):

| `kind` | API | Typical backend |
|---|---|---|
| `openai_responses` | Responses API (`/v1/responses`) | OpenAI, Azure OpenAI |
| `openai_chat_completions` | Chat Completions API (`/v1/chat/completions`) | OpenAI-compatible endpoints |
| `vllm_responses` | Responses API (`/v1/responses`) | vLLM |
| `anthropic_messages` | Messages API (`/v1/messages`) | Anthropic Platform, Microsoft Foundry |

Anthropic chats use `rag_provider` for file storage and vector stores; document search is not available for them. See [ADR-0005](docs/ADR/0005-cpt-cf-mini-chat-adr-multi-provider-adapters.md) and [features/anthropic-provider-support.md](docs/features/anthropic-provider-support.md).

## Running Locally

```bash
make mini-chat
```

This starts the server at `http://127.0.0.1:8087` with SQLite, mock auth, and single-tenant mode.

### Configuration

Config file: **`config/mini-chat.yaml`**

#### Setting up Azure OpenAI credentials

Export two environment variables before starting the server:

```bash
export AZURE_OPENAI_API_KEY="<your-api-key>"
export AZURE_OPENAI_API_HOST="<your-resource>.openai.azure.com"
```

The config references these via `${AZURE_OPENAI_API_KEY}` and `${AZURE_OPENAI_API_HOST}` — no need to edit the YAML for basic setup.

#### Per-tenant provider overrides (optional)

Each provider entry in `mini-chat.config.providers` can include a `tenant_overrides` map to give specific tenants their own host and/or auth. See the commented examples in `config/mini-chat.yaml`.

## API

Routes are mounted under the gear's `url_prefix` (default `/mini-chat`, `mini-chat.config.url_prefix`). The API gateway prepends its own `prefix_path` (`gears.api-gateway.config.prefix_path`, empty by default). Deployments usually set it to `/cf` (the Helm chart in `deploy/helm` does), which gives:

```
{gateway}/cf/mini-chat/v1/...
```

`config/mini-chat.yaml` sets no `prefix_path`, so `make mini-chat` serves `http://127.0.0.1:8087/mini-chat/v1`. `config/quickstart.yaml` sets `/cf`: `http://127.0.0.1:8087/cf/mini-chat/v1`.

Paths below are relative to `{prefix_path}{url_prefix}`:

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/v1/models` | List available models |
| GET | `/v1/models/{id}` | Get model details |
| POST | `/v1/chats` | Create a chat |
| GET | `/v1/chats` | List chats |
| GET | `/v1/chats/{id}` | Get a chat |
| PATCH | `/v1/chats/{id}` | Update a chat title |
| DELETE | `/v1/chats/{id}` | Delete a chat |
| GET | `/v1/chats/{id}/messages` | List messages |
| POST | `/v1/chats/{id}/messages:stream` | Send a message (SSE) |
| PUT | `/v1/chats/{id}/messages/{msg_id}/reaction` | Set a reaction |
| DELETE | `/v1/chats/{id}/messages/{msg_id}/reaction` | Remove a reaction |
| POST | `/v1/chats/{id}/attachments` | Upload an attachment (`multipart/form-data`) |
| GET | `/v1/chats/{id}/attachments/{attachment_id}` | Get attachment metadata |
| DELETE | `/v1/chats/{id}/attachments/{attachment_id}` | Delete an attachment |
| GET | `/v1/chats/{id}/turns/{request_id}` | Get a turn |
| POST | `/v1/chats/{id}/turns/{request_id}/retry` | Retry the latest turn (SSE) |
| PATCH | `/v1/chats/{id}/turns/{request_id}` | Edit the latest turn (SSE) |
| DELETE | `/v1/chats/{id}/turns/{request_id}` | Delete a turn |
| GET | `/v1/quota/status` | Quota status of the current user |

The routes are declared in `mini-chat/src/api/rest/routes/`. The OpenAPI schema is generated from them: see [`docs/api/api.json`](../../docs/api/api.json) at the repository root (operation ids `mini_chat.*`), or `{prefix_path}/docs` and `{prefix_path}/openapi.json` on a running server with `enable_docs: true`.

## Smoke Test

```bash
# All steps (requires a valid API key for SSE streaming)
python3 gears/mini-chat/scripts/smoke-test-api.py

# Skip SSE streaming (no real API key needed)
python3 gears/mini-chat/scripts/smoke-test-api.py --no-sse
```

## Documentation

- [PRD](docs/PRD.md)
- [Design](docs/DESIGN.md)
- [ADRs](docs/ADR/) — accepted decisions, including P1 scope and known deviations from DESIGN
- [Feature docs](docs/features/)
- [OpenAPI schema (generated)](../../docs/api/api.json)
