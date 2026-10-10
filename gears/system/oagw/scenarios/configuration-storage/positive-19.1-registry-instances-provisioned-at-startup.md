# Registry instances provisioned at startup

OAGW stores its configuration in a database (`gears.oagw.database`). Upstream and route instances in the types registry, here from the types-registry `entities` configuration, become stored rows when OAGW starts (ADR-0018, Startup Provisioning from the Types Registry).

## Setup

Types-registry `entities`:

```yaml
- $id: "gts.cf.core.oagw.upstream.v1~5e0a1900-0000-4000-8000-000000000001"
  alias: "e2e-registry-upstream"
  server:
    endpoints: [{ host: "127.0.0.1", port: 19876, scheme: "http" }]
  protocol: "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"
  tags: ["e2e-registry"]
- $id: "gts.cf.core.oagw.route.v1~5e0a1900-0000-4000-8000-000000000002"
  upstream_id: "gts.cf.core.oagw.upstream.v1~5e0a1900-0000-4000-8000-000000000001"
  match:
    http: { methods: ["GET"], path: "/v1/models" }
  tags: ["e2e-registry"]
- $id: "gts.cf.core.oagw.route.v1~5e0a1900-0000-4000-8000-000000000003"
  upstream_id: "5e0a1900-0000-4000-8000-000000000001"
  match:
    http: { methods: ["GET"], path: "/v1/bare-uuid" }
```

No instance has `tenant_id`, so all belong to the root tenant. A route's `upstream_id` may be a GTS identifier or a bare UUID. `enabled` and `priority` are left to their defaults (`true` and `0`).

## Expected behavior

- `GET /upstreams/gts.cf.core.oagw.upstream.v1~5e0a1900-0000-4000-8000-000000000001` returns `200` with the instance's alias, endpoint, protocol and tags, in the root tenant. The instance UUID is the row ID.
- `GET /routes/gts.cf.core.oagw.route.v1~5e0a1900-0000-4000-8000-000000000002` returns `200`, with `upstream_id` naming the registry upstream, `enabled` true and `priority` 0. The route given a bare UUID names the same upstream.
- They appear in `GET /upstreams` and `GET /routes?upstream_id=...`.
- `GET /proxy/e2e-registry-upstream/v1/models` reaches the upstream (`200`, `X-OAGW-Error-Source: upstream`), and so does `/v1/bare-uuid` (the upstream's own `404`).
- OAGW's database file holds the upstream and both routes with `managed_by = registry`.
- Restarting does not duplicate the rows; that, and how a changed or removed instance is applied at the next boot, is covered by the gear's Rust tests.
