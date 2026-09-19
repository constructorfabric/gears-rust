# Alias is immutable on update

## Step 1: Create upstream

```http
POST /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": { "endpoints": [{ "scheme": "https", "host": "httpbin.org", "port": 443 }] },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "alias": "httpbin.org"
}
```

## Step 2: Attempt to change the alias

```http
PUT /api/oagw/v1/upstreams/gts.cf.core.oagw.upstream.v1~<uuid> HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": { "endpoints": [{ "scheme": "https", "host": "httpbin.org", "port": 443 }] },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "alias": "httpbin-renamed.org"
}
```

## Expected response

- `400 Bad Request`
- The upstream's `alias` is unchanged — a subsequent `GET` on the same id still returns the original alias.

## What to check

- Every other mutable field (headers, plugins, rate_limit, enabled, …) can be updated freely via `PUT` — see [positive-2.5](positive-2.5-update-upstream.md) — only `alias` is immutable once set.
- `server.endpoints` is the one exception to "freely": it can be changed, but only if the new endpoints' auto-derived alias still matches the existing alias exactly. An endpoint change that would derive a *different* alias (e.g. swapping to a different hostname) is rejected with `400` for the same reason — the alias must not silently change — and a hostname→IP transition is always rejected outright. Delete and re-create the upstream instead of trying to change endpoints across an alias boundary.
