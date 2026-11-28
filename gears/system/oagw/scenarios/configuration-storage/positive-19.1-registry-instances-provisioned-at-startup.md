# Registry instances provisioned at startup

OAGW stores its configuration in a database (`gears.oagw.database`). Upstream and route instances in the types registry, here published through its platform API after startup, become stored rows at OAGW's next reconcile (ADR-0018, Startup Provisioning from the Types Registry).

## Setup

Instances published to the types registry (`POST /types-registry/v2/platform/entities`), each body carrying its `id`:

```yaml
- id: "gts.cf.core.oagw.upstream.v1~cf.e2e.oagw.registry_upstream.v1"
  alias: "e2e-registry-upstream"
  server:
    endpoints: [{ host: "127.0.0.1", port: 19876, scheme: "http" }]
  protocol: "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"
  tags: ["e2e-registry"]
- id: "gts.cf.core.oagw.route.v1~cf.e2e.oagw.registry_route.v1"
  upstream_id: "gts.cf.core.oagw.upstream.v1~cf.e2e.oagw.registry_upstream.v1"
  match:
    http: { methods: ["GET"], path: "/v1/models" }
  tags: ["e2e-registry"]
- id: "gts.cf.core.oagw.route.v1~cf.e2e.oagw.bare_uuid_route.v1"
  upstream_id: "40acc0bf-bcbf-5fcf-8a3a-d72c78ba2d62"  # the upstream's registry UUID
  match:
    http: { methods: ["GET"], path: "/v1/bare-uuid" }
```

No instance has `tenant_id`, so all belong to the root tenant. A route's `upstream_id` may be the upstream's instance identifier or the bare UUID of its row. A row's ID is its instance's registry UUID (UUID v5 of the identifier). `enabled` and `priority` are left to their defaults (`true` and `0`).

## Expected behavior

- `GET /upstreams/gts.cf.core.oagw.upstream.v1~40acc0bf-bcbf-5fcf-8a3a-d72c78ba2d62` returns `200` with the instance's alias, endpoint, protocol and tags, in the root tenant.
- `GET /routes/gts.cf.core.oagw.route.v1~038aaf41-2025-59c4-8831-803af73a3e8b` returns `200`, with `upstream_id` naming the registry upstream, `enabled` true and `priority` 0. The route given a bare UUID names the same upstream.
- They appear in `GET /upstreams` and `GET /routes?upstream_id=...`.
- `GET /proxy/e2e-registry-upstream/v1/models` reaches the upstream (`200`, `X-OAGW-Error-Source: upstream`), and so does `/v1/bare-uuid` (the upstream's own `404`).
- OAGW's database file holds the upstream and both routes with `managed_by = registry`.
- Restarting does not duplicate the rows; that, and how a changed or removed instance is applied at the next boot, is covered by the gear's Rust tests.
