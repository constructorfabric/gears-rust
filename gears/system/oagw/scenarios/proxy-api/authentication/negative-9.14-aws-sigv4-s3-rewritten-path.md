# Outbound auth: AWS SigV4 for S3 on a path the gateway rewrote

The gateway normalizes the request path before route matching: it collapses `//`, resolves `.` and `..` and drops a trailing `/`; a route path that ends with `/` adds one to the request it matches exactly. Most AWS services normalize the path the same way. Amazon S3 does not: `a//b`, `a/./b` and `folder/` are object keys of their own. A request whose path the gateway rewrote would be signed correctly and address another object, so the plugin refuses it when the path is signed as sent (`uri_encoding: single`, the default for the S3 signing names `s3`, `s3-object-lambda`, `s3-outposts` and `s3express`).

## Upstream configuration

```json
{
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.aws_sigv4.v1",
    "config": {
      "region": "us-east-1",
      "service": "s3",
      "access_key_id_ref": "cred://aws-access-key-id",
      "secret_access_key_ref": "cred://aws-secret-access-key"
    }
  }
}
```

## Scenario A: rewritten path refused

```http
PUT /api/oagw/v1/proxy/<alias>/my-bucket/reports//2026/q3.csv HTTP/1.1
```

Also `.../my-bucket/a/./b`, `.../my-bucket/a/../b`, `.../my-bucket/folder/`, and `.../my-bucket/dir` on a route `/my-bucket/dir/`, which would be sent as `dir/`. A WebSocket handshake is refused the same way.

Expected:
- `400` `invalid_argument`, field violation `path`, reason `VALIDATION`.
- Nothing is sent to the upstream and no credential is read.

## Scenario B: paths the gateway keeps

- `PUT .../my-bucket/reports/2026/q3.csv` and an encoded `/` in a key (`.../my-bucket/a%2Fb`) are signed and sent as received.
- Keys with characters a client may send unencoded (`a:b`, `photo(1).jpg`, `x!y*z`, `k=v@w,u`) are sent as received and signed as S3 checks them: the path decoded and encoded again (`a%3Ab`, `photo%281%29.jpg`).
- A trailing `/` the route path itself ends with (route `/my-bucket/dir/`, request `.../my-bucket/dir/`) is sent and signed: the upstream gets the path the client sent.
- With `uri_encoding: double` (every service but S3) a rewritten path is signed and sent normalized: the service normalizes it the same way.

Covered by Rust tests (`infra/proxy/aws_sigv4_tests.rs`, `infra/plugin/aws_sigv4_auth_tests.rs`, `domain/proxy_path.rs`). E2E (`test_aws_sigv4_auth.py`, part A) covers `//` and a trailing `/`; the HTTP client resolves `.` and `..` before sending.
