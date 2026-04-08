# CRUD endpoint update invalidates load balancer

## Setup

- Upstream `my-service` with one endpoint:
  - `endpoints`: `[{"scheme": "http", "host": "server-a.example.com", "port": 8080}]`
  - `alias`: `my-service`
- Route: `POST /v1/chat`

## Step 1: Proxy request to original endpoint

```http
POST /api/oagw/v1/proxy/my-service/v1/chat HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token>
Content-Type: application/json

{"prompt": "hello"}
```

- Response: `200 OK` from `server-a.example.com`
- LoadBalancer is lazily constructed and cached for this upstream

## Step 2: Update upstream endpoints via management API

```http
PUT /api/oagw/v1/upstreams/{id} HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <admin-token>
Content-Type: application/json

{
  "server": {
    "endpoints": [
      { "scheme": "http", "host": "server-b.example.com", "port": 8080 }
    ]
  },
  "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
  "alias": "my-service"
}
```

- Response: `200 OK`
- LoadBalancer cache entry for this upstream is invalidated

## Step 3: Proxy request after update

```http
POST /api/oagw/v1/proxy/my-service/v1/chat HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token>
Content-Type: application/json

{"prompt": "hello again"}
```

- Response: `200 OK` from `server-b.example.com` (not `server-a`)
- A new LoadBalancer is lazily constructed with the updated endpoint list

## Expected behavior

- After `PUT /api/oagw/v1/upstreams/{id}`, the cached LoadBalancer is invalidated
- Next proxy request uses the updated endpoint configuration
- Old endpoint (`server-a`) receives no further traffic after the update
- New endpoint (`server-b`) receives all subsequent traffic

## Validation

- CRUD operations on upstreams trigger LoadBalancer cache invalidation
- Invalidation is immediate — no stale routing to old endpoints
- Same behavior applies to `POST` (create) and `DELETE` operations
