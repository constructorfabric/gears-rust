# Create minimal HTTP upstream (single endpoint)

## Request

```http
POST /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "httpbin.org", "port": 443 }
    ]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"
}
```

## Expected response

- `201 Created`
- Response body contains:
  - `enabled: true` (default)
  - `alias: "httpbin.org"` (standard port 443 omitted)

```json
{
  "id": "gts.cf.core.oagw.upstream.v1~<uuid>",
  "enabled": true,
  "alias": "httpbin.org",
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "httpbin.org", "port": 443 }
    ]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"
}
```

## Port omitted

`port` may be left out; the endpoint gets its scheme's default, 80 for `http` and 443 for `https`, `wss`, `wt` and `grpc`. The response, and a later `GET`, carry the port:

```json
{ "server": { "endpoints": [ { "scheme": "http", "host": "plain.example.com" } ] }, "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1" }
```

returns `"endpoints": [ { "scheme": "http", "host": "plain.example.com", "port": 80 } ]` and `alias: "plain.example.com"` (`http` requires the gear option `allow_http_upstream`).
