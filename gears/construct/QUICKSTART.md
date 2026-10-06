# Construct - Quickstart

The gear foundation of Construct: the base on which the Construct features are built. It has one route that creates a note in the caller's tenant, a client for other gears (`create_note` and `get_note`), and tenant-scoped storage. The note is a placeholder. It has no DESIGN id and is replaced by Construct's own model, starting with Subject Settings.

**Features:**
- One authenticated route, `POST /construct/v1/foundation-notes`, with a permission check per operation
- Notes are tied to the caller's tenant, taken only from the security context
- Errors are RFC 9457 Problem responses
- The gear supplies its migration to the runtime, which runs it on boot

There is no route to read a note. Reading exists only through the in-process client `ConstructClientV1`.

Full API documentation: <http://127.0.0.1:8087/cf/docs>

The example server uses the gateway prefix `/cf`. This comes from `gears.api-gateway.config.prefix_path` and is configurable.

## Run it locally

The example server does not include Construct. Construct is hosted in its own repository (`rolos/construct-core`). To try the gear on your machine, link it into the example server for a local run, without committing the change.

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
     config:
       max_text_length: 1000
   ```

4. Start the server:

   ```bash
   cargo run --bin cf-gears-example-server --no-default-features --features construct,static-tenants,static-authn,static-authz -- --config config/quickstart.yaml run
   ```

The log shows `Providing construct database migrations` and `Applying migration gear="construct" migration=initial_001`.

## Configuration

```yaml
gears:
  construct:
    database:
      server: "sqlite_users"
      file: "construct.db"
    config:
      max_text_length: 1000  # Longest note text in bytes (default: 1000, allowed: 1 to 65535)
```

An unknown key, or a `max_text_length` of 0 or above 65535, stops the gear from starting.

## Examples

The example config runs with authentication off: every request gets the default tenant. In a real host the route needs a platform bearer token, and a request without one is refused with `401`.

### Create a note

```bash
curl -s -X POST http://127.0.0.1:8087/cf/construct/v1/foundation-notes \
  -H "Content-Type: application/json" \
  -d '{"text":"hello"}'
```

Response `201`:

```json
{"id":"4b351a3a-bf2b-4361-bf76-9a71ff183159","tenant_id":"00000000-df51-5b42-9538-d2b56b7ee953","text":"hello"}
```

### Errors

All errors are `application/problem+json`.

| Request | Status |
|---|---|
| `{"text":"   "}` (blank), a text with a NUL character, or a text longer than `max_text_length` | `400`, with the field and the reason `VALIDATION_ERROR` |
| A body that is not valid JSON | `400`, reason `json_syntax_error` |
| A body of the wrong shape, for example `{"text":5}` | `422`, reason `invalid_json_body` |
| No `Content-Type: application/json` header | `415`, reason `missing_json_content_type` |
| The caller has no permission | `403`, reason `ACCESS_DENIED` |

```bash
curl -s -X POST http://127.0.0.1:8087/cf/construct/v1/foundation-notes \
  -H "Content-Type: application/json" \
  -d '{"text":"   "}'
```

Response `400`:

```json
{"type":"gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~","title":"Invalid Argument","status":400,"detail":"Request validation failed","instance":"/construct/v1/foundation-notes","context":{"field_violations":[{"field":"text","description":"must not be empty","reason":"VALIDATION_ERROR"}],"resource_type":"gts.cf.construct.foundation.note.v1~"}}
```

## Troubleshooting

| Issue | Solution |
|-------|----------|
| `no such table: construct__foundation_notes` | The SQLite file comes from an older build that used another table name. Delete the file, or point `database.file` to a new one. |
| The gear does not start | Check `max_text_length` (1 to 65535) and that the config has no unknown key. |
| `404` on `/cf/construct/...` | The example server was started without `--features construct`. |
