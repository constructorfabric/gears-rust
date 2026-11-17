# Host header replaced by upstream host

## Inbound request

```http
GET /api/oagw/v1/proxy/httpbin.org/get HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Host: evil.example.com

```

## Expected behavior

- Gateway ignores/overrides the inbound `Host` header.
- Outbound request to upstream uses `Host: httpbin.org`.

## What to check

- Use an upstream echo endpoint to verify received `Host`.
- Ensure request is not routed based on inbound host.
- With a non-standard port (for example the e2e mock upstream on `127.0.0.1:<port>`), the upstream receives `Host: <host>:<port>`.

## Scenario B: the port is omitted only when it is the scheme's default

| Endpoint | Outbound `Host` |
|---|---|
| `https://api.example.com:443` | `api.example.com` |
| `http://api.example.com:80` | `api.example.com` |
| `https://api.example.com:8443` | `api.example.com:8443` |
| `http://api.example.com:443` | `api.example.com:443` |
| `https://api.example.com:80` | `api.example.com:80` |
| `wss://api.example.com:80` | `api.example.com:80` |

The default port is 80 for `http` and 443 for `https`, `wss`, `wt` and `grpc` (RFC 9110 §7.2). The alias derived from the endpoint follows the same rule.

## What to check (Scenario B)

- Covered by Rust tests (`set_host_header`, `alias_contribution`): the e2e mock upstream cannot listen on 80 or 443.
