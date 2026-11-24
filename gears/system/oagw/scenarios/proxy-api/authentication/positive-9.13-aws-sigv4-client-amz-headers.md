# Outbound auth: AWS SigV4 does not trust client signing headers

With `passthrough: all` (or an allowlist naming them) a client could send its own signing headers. Upstream as in [positive-9.11](positive-9.11-aws-sigv4-signed-request.md), with `"headers": { "request": { "passthrough": "all" } }`.

## Scenario A: client values are replaced

```http
POST /api/oagw/v1/proxy/bedrock-runtime.us-east-1.amazonaws.com/model/<id>/invoke HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-Amz-Date: 19700101T000000Z
X-Amz-Content-Sha256: 0000000000000000000000000000000000000000000000000000000000000000
X-Amz-Security-Token: client-token
X-Amz-Target: BedrockRuntime.InvokeModel
```

Expected:
- The upstream receives one `X-Amz-Date`, one `X-Amz-Content-Sha256` and one `X-Amz-Security-Token`, each with the plugin's value (the configured session token), and one `Authorization` with the plugin's signature. The client's `Authorization` is never forwarded.
- `X-Amz-Target` reaches the upstream as sent and is signed.
- With `content_sha256_header: false` the plugin adds no `X-Amz-Content-Sha256`, but a client's one is still replaced with the real hash.

## Scenario B: client session token without `session_token_ref`

The signing phase can replace a header but not remove it. On an upstream without `session_token_ref`, a request that carries `X-Amz-Security-Token` is refused:

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "Request validation failed",
  "instance": "/oagw/v1/proxy/<alias>/<path>",
  "trace_id": "<trace id>",
  "context": {
    "field_violations": [
      {
        "field": "x-amz-security-token",
        "reason": "VALIDATION",
        "description": "the request carries x-amz-security-token but auth.config.session_token_ref is not set; the AWS SigV4 plugin cannot remove the header, so drop it with headers.request.remove"
      }
    ],
    "resource_type": "gts.cf.core.oagw.proxy.v1~"
  }
}
```

Nothing is sent to the upstream. With `"remove": ["x-amz-security-token"]` in `headers.request`, the same request is signed and sent without the header.
