# Usage Collector REST Client

> **Separate-binary bridge** — builds a `UsageCollectorRestClient` that forwards `create_usage_record` and `get_module_config` calls to a remote usage-collector REST API, authenticated with a bearer token from `AuthNResolverClient::exchange_client_credentials`.

ModKit module `usage-collector-rest-client`: builds [`UsageCollectorRestClient`](src/infra/rest_client.rs) at init, resolves `dyn AuthNResolverClient` and `dyn AuthZResolverClient` from `ClientHub`, then wires it into `UsageEmitterRuntime` (registered as `dyn UsageEmitterRuntimeV1`). The module also implements `DatabaseCapability` and provides outbox migrations required by `UsageEmitter`.

## Dependencies

- The remote binary must expose the usage-collector REST API at `collector_url`.

## Configuration

`collector_url` and the nested `oauth.client_id` / `oauth.client_secret` are **required** (no defaults). Optional fields:

- `oauth.scopes` — default `[]` (IdP default scopes)
- `emitter` — nested [`UsageEmitterConfig`](../usage-emitter/src/config.rs); all fields are optional:
  - `authorization_max_age` — default `30s`
  - `outbox_queue` — default `"usage-records"`
  - `outbox_partition_count` — default `4` (must be a power of 2 in 1–64)

HTTP transport tuning is not exposed; the client is built with `HttpClientConfig::default()`.

```yaml
modules:
  usage-collector-rest-client:
    config:
      # required
      collector_url: "http://collector.internal:8080"
      oauth:
        client_id: "my-service"
        client_secret: "${MY_SERVICE_SECRET}"
        # optional
        scopes: ["usage-collector:write"]
      # optional
      emitter:
        authorization_max_age: "30s"
        outbox_queue: "usage-records"
        outbox_partition_count: 4
```

## Testing

```bash
cargo test -p cyberware-usage-collector-rest-client
```
