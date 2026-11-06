# Webhook Protocol

HTTP contract between Chat Engine and a legacy webhook backend. Chat Engine
reaches such a backend through the first-party `webhook-compat` plugin
([`webhook_compat.rs`](../chat-engine/src/infra/webhook_compat.rs)), which
adapts the SDK's `ChatEngineBackendPlugin` trait to HTTP `POST` calls.

**The Rust types are the contract.** There are no generated or published JSON
schemas. Request and response envelopes are defined in
[`chat-engine-sdk/src/webhook.rs`](../chat-engine-sdk/src/webhook.rs);
streamed events and their payloads are defined in
[`chat-engine-sdk/src/models.rs`](../chat-engine-sdk/src/models.rs)
(`StreamingEvent`, `Message`, `Capability`, `CapabilityValue`). This document
describes that wire in prose.

> This replaces the dotted-event design (`message.new`, `history`,
> `timestamp`) of [ADR-0007](ADR/0007-webhook-event-types.md). The
> implemented wire is what is described here.

## Transport

- One `POST` per plugin hook to the configured `endpoint`, body `application/json`.
- Plugin config keys (`PluginCallContext.plugin_config`): `endpoint` (required),
  `auth` (`{ "bearer": true }` or `{ "api_key_header": "<Header-Name>" }`),
  `auth_value` (token / key; required with `auth`), `default_timeout_ms`.
- Auth is sent as `Authorization: Bearer <auth_value>` or as the named header.
- The per-request timeout is the call deadline when present, else
  `default_timeout_ms`, else 30 s. A deadline that has already elapsed fails
  before any request is sent.
- Unsuccessful HTTP statuses map to plugin errors: `429` rate limited
  (honouring `Retry-After`), `401`/`403` unauthorized, `404` not found,
  `400`/`422` invalid input; see `map_status_to_error` for the rest.

## Request bodies

Every body carries an `event` discriminator, serialised from the
`WebhookEvent` enum in snake_case. `enabled_capabilities` (an array of
`{ "name", "value" }`) is **omitted**, never `null`, when the call carries none.

### Session hooks (`WebhookSessionRequest`)

| `event` | Hook | Response |
|---|---|---|
| `session_type_configured` | `on_session_type_configured` | session response |
| `session_created` | `on_session_created` | session response |
| `session_updated` | `on_session_updated` | session response |
| `session_summary` | `on_session_summary` | NDJSON stream |

```json
{
  "event": "session_created",
  "session_type_id": "<uuid>",
  "session_id": "<uuid>",
  "enabled_capabilities": [{ "name": "model", "value": "gpt-4" }]
}
```

`session_id` is `null` for `session_type_configured` (no session exists yet).

### Message hooks (`WebhookMessageRequest`)

| `event` | Hook | Response |
|---|---|---|
| `message` | `on_message` | NDJSON stream |
| `message_recreate` | `on_message_recreate` | NDJSON stream |

```json
{
  "event": "message",
  "session_id": "<uuid>",
  "message_id": "<uuid>",
  "messages": [ { "...": "Message objects, conversation context" } ],
  "enabled_capabilities": [{ "name": "temperature", "value": 0.7 }]
}
```

`messages` is the conversation context as serialised `Message` values.

## Session response (`WebhookSessionResponse`)

An empty body means no capabilities and no metadata. Otherwise one of:

- a bare capability array: `[ { "name": "model", "value": { ... } } ]`
- an object: `{ "capabilities": [ ... ], "metadata": { ... } }`. Both keys are
  optional, and `metadata: null` counts as absent. `metadata` is merged into the
  session's metadata (ignored for `session_type_configured`).

Anything else is rejected as a malformed response.

## Streaming response (NDJSON)

`message`, `message_recreate` and `session_summary` answer with a stream of
newline-delimited JSON, one `StreamingEvent` per line, discriminated by `type`:

| `type` | Payload fields |
|---|---|
| `start` | `message_id` |
| `chunk` | `message_id`, `chunk` |
| `status` | `message_id`, `code`, `detail?` |
| `part` | `message_id`, `part` (`MessagePartInput`) |
| `citation` | `message_id`, `part_number`, `file_citations`, `link_citations`, `references` |
| `state` | `message_id`, `state` |
| `session_meta` | `message_id`, `patch` |
| `tool` | `message_id`, `tool`, `payload` |
| `complete` | `message_id`, `metadata?`, citation arrays |
| `error` | `message_id`, `error` |

A well-formed stream is one `start`, any number of intermediate events, then
one `complete` (or `error` at any point). A line that does not parse as a
`StreamingEvent` fails the stream with an internal error. A trailing line
without a final `\n` is still parsed.

## Not on the wire

`message.reaction` (`MessageReactionEvent`) is built by the reaction service
but is not dispatched: the plugin trait has no reaction hook yet.

## Verification

The contract is exercised by the mock-server tests in
[`webhook_compat_tests.rs`](../chat-engine/src/infra/webhook_compat_tests.rs).
