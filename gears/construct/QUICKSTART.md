# Construct - Quickstart

Record intake for connectors: a connector sends one record per request for one tenant, and Construct checks it against its GTS type and answers at once with received, repeat or refused. A received record is handed to background processing in the same transaction that keeps its identity.

**Until the Planner exists (Story 5.2), no processing is wired in.** Every record that passes the checks is answered `503`, and nothing is stored, so a connector can send it again once processing exists. A refused record still answers `422`, so the checks can be tried now.

**Features:**
- One authenticated route, `POST /construct/v1/records?tenant=<tenant id>`, with one permission check (`construct.record` / `send`) for the named tenant
- The record is checked against its GTS type and every type it derives from, through the types registry. The connector record types ship in the SDK and are registered when the types registry starts
- A repeat of a received record identity (tenant, connector, provenance, version) changes nothing; the identity is kept only once processing took the record
- Records are refused when the connector is off, or when personalization is off or an erasure is under way for the record's subject
- Errors are RFC 9457 Problem responses
- The gear supplies its migrations to the runtime, which runs them on boot
- An in-process client, `ConstructClientV1::submit_record`, runs the same operation

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
```

An unknown key, or a value in `connectors_off` that is not a UUID, stops the gear from starting.

## Examples

The example config runs with authentication off: every request gets the default caller, and the static AuthZ plugin allows every tenant. In a real host the route needs a connector's platform bearer token, a request without one is refused with `401`, and the platform must authorize the connector for the named tenant, or the answer is `403`.

### Send a record

```bash
curl -s -X POST "http://127.0.0.1:8087/cf/construct/v1/records?tenant=00000000-df51-5b42-9538-d2b56b7ee953" \
  -H "Content-Type: application/json" \
  -d '{"type":"gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~","provenance":"chat_engine/thread-42/msg-7","version":"2026-09-10T09:13:00Z","observed_at":"2026-09-10T09:13:05Z","subject_id":"5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b","payload":{"source":"chat_engine","message_id":"msg-7","thread_id":"thread-42","role":"user","text":"Which of these two papers contradict each other?"}}'
```

Response `503`, because no processing is wired in yet:

```json
{"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~","title":"Service Unavailable","status":503,"detail":"Service temporarily unavailable","instance":"/construct/v1/records","context":{}}
```

The log names the tenant and the connector:

```text
record_intake{tenant_id=00000000-df51-5b42-9538-d2b56b7ee953 connector=11111111-6a88-4768-9dfc-6bcd5187d9ed}: construct::api::rest::error: construct dependency unavailable msg=record processing is not available yet: no planner is wired in
```

The same request again answers `503` too: nothing was stored, so it is not a repeat. Once processing exists, a record that passes the checks answers `202` with `{"outcome":"received"}`, and the same identity again answers `200` with `{"outcome":"repeat"}`.

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
| No processing is wired in yet, or the types registry, the settings service or the policy service does not answer | `503` |

The same record with `"role":"system"` answers `422`:

```json
{"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~","title":"Invalid Argument","status":422,"detail":"Request validation failed","instance":"/construct/v1/records","context":{"field_violations":[{"field":"/payload/role","description":"enum","reason":"SCHEMA_VIOLATION"}],"resource_type":"gts.cf.connectors.core.record.v1~","resource_name":"gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~"}}
```

## Troubleshooting

| Issue | Solution |
|-------|----------|
| The gear does not start | Check that the config has no unknown key and that every entry in `connectors_off` is a UUID. |
| Every valid record is `503` | Expected until the Planner exists: no processing is wired in. |
| `404` on `/cf/construct/...` | The example server was started without `--features construct`. |
| Every record is `UNKNOWN_TYPE` | The record's `type` is not one of the registered connector record types; the SDK registers the base and four derived types. |
