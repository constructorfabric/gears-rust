# OPTIONS is routed; a CORS preflight is answered by the gateway

## Setup

Create an HTTP upstream (example: `alias=api.example.com`) and a route that lists `OPTIONS`:

```json
{
  "match": {
    "http": {
      "methods": ["POST", "OPTIONS"],
      "path": "/v1/items"
    }
  }
}
```

## Scenario A: OPTIONS is proxied

```http
OPTIONS /api/oagw/v1/proxy/api.example.com/v1/items HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

### Expected behavior

- The upstream receives `OPTIONS /v1/items`; its answer (for example `Allow`) reaches the client with `X-OAGW-Error-Source: upstream`.
- The same with an `Origin` header but no `Access-Control-Request-Method`: that is not a preflight.

## Scenario B: a CORS preflight is not proxied

```http
OPTIONS /api/oagw/v1/proxy/api.example.com/v1/items HTTP/1.1
Host: oagw.example.com
Origin: https://app.example.com
Access-Control-Request-Method: POST
```

### Expected behavior

- `204` from the gateway with the permissive preflight headers of [10.2](../../plugins/guards/positive-10.2-built-cors-handling.md) (`Access-Control-Allow-Origin: https://app.example.com`, `Access-Control-Allow-Methods: POST`).
- The upstream is not called, even though the route lists `OPTIONS`. The API gateway does not authenticate a preflight, so there is no tenant to resolve the alias for.

## Scenario C: no route for OPTIONS

The route does not list `OPTIONS` (for example, `GET` only).

### Expected behavior

- An `OPTIONS` that is not a preflight returns `404` (route not found), `X-OAGW-Error-Source: gateway`.

## What to check

- With the API gateway's `cors_enabled: true`, the gateway answers every `OPTIONS` itself and OAGW sees none; Scenario A needs it off (the default).
