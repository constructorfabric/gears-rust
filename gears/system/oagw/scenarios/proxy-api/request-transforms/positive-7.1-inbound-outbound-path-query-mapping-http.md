# Inbound → outbound path + query mapping (HTTP)

## Upstream configuration

- Upstream `alias=api.openai.com` with `scheme=https`, `protocol=http`.

## Route configuration

- `match.http.path`: `/v1/chat/completions`
- `match.http.path_suffix_mode`: `append`
- `match.http.query_allowlist`: `["version"]`

## Inbound request

```http
POST /api/oagw/v1/proxy/api.openai.com/v1/chat/completions/models/gpt-4?version=2&debug=1 HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"stream":false}
```

## Expected behavior

- Request is rejected because `debug` is not in `query_allowlist`.

Then retry without `debug`:

```http
POST /api/oagw/v1/proxy/api.openai.com/v1/chat/completions/models/gpt-4?version=2 HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"stream":false}
```

## Expected outbound request (to upstream)

```http
POST /v1/chat/completions/models/gpt-4?version=2 HTTP/1.1
Host: api.openai.com
```

- Path suffix is appended.
- A query with a parameter the allowlist does not name is rejected (`400`), not filtered; an accepted query is forwarded as the client sent it ([7.8](positive-7.8-raw-query-forwarded-as-sent.md)).
