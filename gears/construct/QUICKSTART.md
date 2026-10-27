# Construct - Quickstart

Record intake for connectors and the profile read for applications and agents. A connector sends one record per request for one tenant; Construct checks it against its GTS type and answers at once with received, repeat or refused. A received record is handed to background processing in the same transaction that keeps its identity: the planner turns it into a plan, the sensitive-data checks and Admission decide what of it is stored, and the profile writer stores it in graph storage. An application or agent then reads the subject's profile by category.

**Processing runs only with a processing identity and a model configured.** Without them, a record that passes the checks is answered `202`, its identity is kept, and a content-free drop event is logged; nothing from the record is processed. A resend of the same record is a repeat. Without a processing identity, the profile read answers `503`.

**Features:**
- One authenticated route, `POST /construct/v1/records?tenant=<tenant id>`, with one permission check (`construct.record` / `send`) for the named tenant
- The record is checked against its GTS type and every type it derives from, through the types registry. The connector record types ship in the SDK and are registered when the types registry starts
- A repeat of a received record identity (tenant, connector, provenance, version) changes nothing; the identity is kept only once processing took the record
- Records are refused when the connector is off, or when personalization is off or an erasure is under way for the record's subject
- Errors are RFC 9457 Problem responses
- The gear supplies its migrations to the runtime, which runs them on boot
- `GET /construct/v1/subjects/{subject_id}/profile`: the subject's facts by category, for the subject itself, or a caller the platform lets act for the subject by naming it (`construct.profile` / `read`), in the caller's tenant or the one named in `tenant`. A category the caller may not read is not listed
- An in-process client, `ConstructClientV1::submit_record` and `read_profile`, runs the same operations
- The gear needs graph storage in its host

Full API documentation: <http://127.0.0.1:8087/cf/docs>

The example server uses the gateway prefix `/cf`. This comes from `gears.api-gateway.config.prefix_path` and is configurable.

## Run it locally

The gear is in this repository under `gears/construct`, and it is not built into the example server. To try it on your machine, link it into the example server for a local run, without committing the change.

1. In `apps/cf-gears-example-server/Cargo.toml`, add the dependency and the feature:

   ```toml
   construct = ["dep:construct"]
   ```

   ```toml
   construct = { package = "cf-gears-construct", path = "../../gears/construct/construct", optional = true }
   ```

2. In `apps/cf-gears-example-server/src/registered_gears.rs`, link the crate:

   ```rust
   #[cfg(feature = "construct")]
   use construct as _;
   ```

3. In `config/quickstart.yaml`, under `gears:`, add the gear's section:

   ```yaml
   construct:
     database:
       server: "sqlite_users"
       file: "construct.db"
   ```

4. Start the server:

   ```bash
   cargo run --bin cf-gears-example-server --no-default-features --features construct,static-tenants,static-authn,static-authz -- --config config/quickstart.yaml run
   ```

The log shows `Applying migration gear="construct"` for `initial_001`, `m002_subject_settings` and `m003_record_ids`. The types registry is part of every example server, so the record types are there.

## Configuration

```yaml
gears:
  construct:
    database:
      server: "sqlite_users"
      file: "construct.db"
    config:
      personalization_default: true  # personalization for a new subject while no settings service answers (default: true)
      connectors_off: []             # connectors that are off, by the subject id (a UUID) of their login (default: none)
      processing_identity: "22222222-0000-4000-8000-000000000001"  # Construct's service account for graph storage (default: none)
      model:                         # the planner's model, an OAGW upstream (default: none, records are dropped)
        upstream_alias: "llm"
        model: "gpt-4.1"
        timeout_secs: 60
      processing:                    # all optional
        queue_capacity: 256
        workers: 4
        max_rounds: 8
        max_tokens: 32000
        max_reruns: 2
      admission:                     # all optional; the same rules for every tenant
        confidence_floor: 0.6
        max_steps: 100
        max_value_chars: 1000
        # trusted_connectors: [...]  # connector identities as UUIDs; absent: every connector is trusted
        # entity_kinds: [...]        # absent: every entity kind is allowed
      profile_category_check: per_category   # required with processing_identity; one_call only for a policy plugin that answers with category constraints
```

An unknown key, a value in `connectors_off`, `processing_identity` or `trusted_connectors` that is not a UUID, a zero in `processing`, a queue capacity above 100000, more than 256 workers, more than 10 reruns, a processing identity without `profile_category_check`, an entity kind that is not one of identity, role, skill or preference, a confidence floor outside 0 to 1, a step cap outside 1 to 10000, a value cap outside 1 to 100000, a model upstream alias that is not 1 to 63 letters, digits, `-` or `_`, or a model name that is not 1 to 128 printable characters with no space, stops the gear from starting. The processing identity needs graph storage's read, write, delete and type registration permissions, and the settings read, in every tenant.

## Examples

The example config runs with authentication off: every request gets the default caller, and the static AuthZ plugin allows every tenant. In a real host the route needs a connector's platform bearer token, a request without one is refused with `401`, and the platform must authorize the connector for the named tenant, or the answer is `403`.

### Send a record

```bash
curl -s -X POST "http://127.0.0.1:8087/cf/construct/v1/records?tenant=00000000-df51-5b42-9538-d2b56b7ee953" \
  -H "Content-Type: application/json" \
  -d '{"type":"gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~","provenance":"chat_engine/thread-42/msg-7","version":"2026-09-10T09:13:00Z","observed_at":"2026-09-10T09:13:05Z","subject_id":"5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b","payload":{"source":"chat_engine","message_id":"msg-7","thread_id":"thread-42","role":"user","text":"Which of these two papers contradict each other?"}}'
```

Response `202`:

```json
{"outcome":"received"}
```

The log shows the drop event, without the record's content:

```text
record dropped event="record_dropped" tenant_id=00000000-df51-5b42-9538-d2b56b7ee953 subject_id=5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b connector=11111111-6a88-4768-9dfc-6bcd5187d9ed record_key=ec8d74b4e17e2460d53844da5611d8930e4cd83f8152997413b3c973059be4ad record_type=gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~ stage="planner" reason="no_planner"
```

The same request again answers `200`, and no second drop event is written:

```json
{"outcome":"repeat"}
```

### Errors

All errors are `application/problem+json`. A refused record is `422`; its field violation names the place in the record (a JSON pointer, or `(record)` for the record as a whole), the broken rule and a reason code, and `resource_name` names the record's type.

| Request | Status |
|---|---|
| The record breaks its type, for example `"role":"system"` | `422`, reason `SCHEMA_VIOLATION` |
| The record names a type the types registry does not know | `422`, reason `UNKNOWN_TYPE` |
| The record carries an `id` | `422`, reason `ID_IN_PUSH` |
| The connector is listed in `connectors_off` | `422`, reason `CONNECTOR_OFF` |
| Personalization is off, or an erasure is under way, for the subject | `422`, reason `PERSONALIZATION_OFF` or `ERASURE_IN_PROGRESS` |
| No `tenant` query parameter, or not a UUID | `400`, reason `invalid_query_string` |
| A body that is not valid JSON | `400` |
| No `Content-Type: application/json` header | `415` |
| The connector has no permission for the tenant | `403`, reason `ACCESS_DENIED` |
| The types registry, the settings service or the policy service does not answer | `503` |

The same record with `"role":"system"` answers `422`:

```json
{"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~","title":"Invalid Argument","status":422,"detail":"Request validation failed","instance":"/construct/v1/records","context":{"field_violations":[{"field":"/payload/role","description":"enum","reason":"SCHEMA_VIOLATION"}],"resource_type":"gts.cf.connectors.core.record.v1~","resource_name":"gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~"}}
```

### Read a profile

```bash
curl -s "http://127.0.0.1:8087/cf/construct/v1/subjects/5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b/profile?tenant=00000000-df51-5b42-9538-d2b56b7ee953"
```

Response `200`, once a record was processed:

```json
{"subject_id":"5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b","categories":[{"entity_kind":"skill","facts":[{"fact_id":"construct:00000000-df51-5b42-9538-d2b56b7ee953:0b7c...","properties":{"skills":{"name":"Rust"}},"origin":{"kind":"connector"},"stored_at":"2026-09-10T09:13:07Z","produced_at":"2026-09-10T09:13:05Z"}]}]}
```

A subject without facts answers `{"subject_id":"…","categories":[]}`; a subject the caller may not act for answers `404`.

## Troubleshooting

| Issue | Solution |
|-------|----------|
| The gear does not start | Check that the config has no unknown key and that every entry in `connectors_off` is a UUID. |
| A received record changes nothing | Without `processing_identity` and `model`, every received record is dropped, with a drop event in the log. With them, look for a drop event naming the stage and the reason. |
| The profile read answers `501` | No `processing_identity` is configured. |
| The profile read answers `503` | Graph storage, the settings service or the policy service cannot answer. |
| `404` on `/cf/construct/...` | The example server was started without `--features construct`. |
| Every record is `UNKNOWN_TYPE` | The record's `type` is not one of the registered connector record types; the SDK registers the base and four derived types. |
