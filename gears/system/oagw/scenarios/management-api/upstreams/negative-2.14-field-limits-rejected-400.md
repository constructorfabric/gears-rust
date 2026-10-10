# Field limits rejected (400)

Tags, route paths, gRPC names, the protocol, auth type and plugin references are stored in columns sized to their limits, and query parameters in the route's JSON, so their size is bounded and control characters are rejected. A value one past a limit is rejected and nothing is stored; a value at the limit is accepted.

## Scenario A: upstream tags

Create an upstream with:

1. 65 tags (the limit is 64).
2. A tag of 129 bytes (the limit is 128 bytes, not characters: 65 two-byte characters exceed it).
3. A tag containing a control character (for example a tab).

Expected for each: `400 Bad Request`, `application/problem+json`, `type` = `...invalid_argument...`, and `detail` names the limit (`at most 64 tags are allowed, got 65`, `tags[0] must not exceed 128 bytes, got 129`, `tags[0] must not contain control characters`). No upstream is stored. 64 tags, or a tag of 128 bytes, is accepted.

## Scenario B: route match fields

Create a route with:

1. `match.http.path` of 2049 bytes (the limit is 2048 bytes, not characters: 1024 two-byte characters after the `/` exceed it).
2. `match.grpc.service` of 257 bytes, on a gRPC upstream (the limit is 256 bytes).
3. `match.grpc.method` of 257 bytes, on a gRPC upstream (the limit is 256 bytes).
4. `match.http.query_allowlist` with 65 parameters (the limit is 64).
5. A `match.http.query_allowlist` parameter of 129 bytes (the limit is 128 bytes).

Expected for each: `400 Bad Request`, `application/problem+json`, `type` = `...invalid_argument...`, and `detail` names the field and the limit (for example `match.http.path must not exceed 2048 bytes, got 2049`). No route is stored. A value at the limit is accepted.

## Scenario C: references

1. Create an upstream whose `protocol` is 257 bytes (the limit is 256 bytes).
2. Create an upstream whose `auth.type` is 257 bytes (the limit is 256 bytes).
3. Create an upstream whose `plugins.items[0].plugin_ref` contains a NUL character.
4. Create a route whose `plugins.items[0].plugin_ref` is 257 bytes (the limit is 256 bytes).

Expected for each: `400 Bad Request`, `application/problem+json`, `type` = `...invalid_argument...`, and `detail` names the field and the limit (for example `protocol must not exceed 256 bytes, got 257`). Nothing is stored. A value at the limit is accepted.
