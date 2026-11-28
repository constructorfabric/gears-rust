# Streaming request bodies are not buffered

## Setup

An upstream with a `POST` route; `max_body_size_bytes` larger than the
upload, and an auth plugin that does not hash the body: any plugin but
`aws_sigv4`. `aws_sigv4` with `unsigned_payload: "true"` streams only a
body with a `Content-Length` (scenario A); it reads a chunked body whole to
hash it (scenario E).

## Scenario A: upload with a Content-Length

```http
POST /api/oagw/v1/proxy/<alias>/upload HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/octet-stream
Content-Length: 8388608

<8 MiB>
```

Expected:

- The gateway forwards the body as it arrives; memory does not grow with
  the upload size.
- The upstream gets `Content-Length: 8388608` (HTTP/1.1) or
  `content-length: 8388608` (HTTP/2), not chunked framing.

## Scenario B: chunked upload

The same request with `Transfer-Encoding: chunked` and no `Content-Length`.

Expected: the body reaches the upstream chunked (HTTP/1.1) or as DATA frames
(HTTP/2).

## Scenario C: chunked upload over the limit

A chunked upload larger than `max_body_size_bytes`.

Expected:

- `413 Payload Too Large`, `application/problem+json`,
  `X-OAGW-Error-Source: gateway`, reason `PAYLOAD_TOO_LARGE`.
- The upload to the upstream is aborted without its final chunk: the
  upstream gets no complete request.

## Scenario D: the upstream answers before the body is read

The upstream answers `413` (or `401`) after the request head, without
reading the body, and closes the connection after a lingering close.

Expected: the client gets the upstream's `413`, its body and
`X-OAGW-Error-Source: upstream`, while its upload is still open.

## Scenario E: a route whose signer hashes the body

On an `aws_sigv4` upstream without `unsigned_payload`, the gateway reads the
body whole (within `max_body_size_bytes`) to hash it, then sends it with
`Content-Length`. With `unsigned_payload: "true"` a body with a
`Content-Length` streams as in scenario A and is signed as
`UNSIGNED-PAYLOAD`; a chunked body (no `Content-Length`) is still read whole
and hashed.

## Scenario F: the circuit breaker is not closed

While the upstream's circuit breaker is open or half-open, a streamed body is
read whole (within `max_body_size_bytes` and the request deadline) before the
request takes its circuit breaker permit. A client that keeps its upload open
then holds no half-open probe slot: another caller's request becomes the probe
and closes the circuit.
