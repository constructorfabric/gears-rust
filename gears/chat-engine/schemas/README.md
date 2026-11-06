Created:  2026-03-06 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# Chat Engine JSON Schemas

This directory holds the JSON Schema definitions (Draft 7) for the Chat Engine **webhook protocol** and the LLM Gateway plugin.

REST request/response contracts are **not** defined here. They are defined by the Rust DTOs (`chat-engine/src/api/rest/dto.rs`, `chat-engine-sdk/src/models.rs`) and the generated [`docs/openapi.json`](../docs/openapi.json).

## Structure

- **webhook/** - Webhook protocol events and responses (session lifecycle, message processing), see [`WEBHOOK-PROTOCOL.md`](../docs/WEBHOOK-PROTOCOL.md)
- **llm_gateway/** - LLM Gateway plugin schemas (plugin config, metadata, usage, events)
- **common/**, **message/**, **streaming/** - Only the shared types that the schemas above reference via `$ref` (e.g. `common/Message.json`, `common/Role.json`, `message/MessageGetResponse.json`, `streaming/StreamingCompleteEvent.json`)

References between schemas use relative `$ref` paths:

```json
{
  "properties": {
    "message": {
      "$ref": "../common/Message.json"
    }
  }
}
```

## See Also

- [DESIGN.md](../docs/DESIGN.md) - Full architecture and design documentation
- [PRD.md](../docs/PRD.md) - Product requirements
- [ADR/](../docs/ADR/) - Architecture decision records
