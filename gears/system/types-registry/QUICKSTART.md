# Types Registry - Quickstart

Stores versioned GTS Type Schemas and Instances, validates dependencies and
materializes schemas during admission.

Features:

- Asynchronous writes: submit, then poll per-entity outcomes
- Optimistic concurrency via `expected_resource_version`
- Single/batch deletion, blocked by live direct registered dependants
- Dry run on every mutation; entity state stays unchanged
- Required `Idempotency-Key`; replay returns the same operation, changed content conflicts
- Reads by GTS identifier or Registry Reference UUID, including tombstones
- Batch reads with one explicit result per key, absence included
- Bounded discovery with an opaque cursor, filtered by `pattern`, `depth` and `kind`
- `$select` on every read: a document-free default, documents only when asked for

Full API documentation for the local E2E configuration: <http://127.0.0.1:8086/cf/docs>

## Surfaces

| Base path | Credential | What it is |
|---|---|---|
| `/types-registry/v2/platform` | `X-ToolKit-Internal-Token` | The database-backed API below: submissions, deletions, operations and entity reads |
| `/types-registry/v2` | `Authorization: Bearer` | The same entity reads for tenants (exact, `:batchGet`, discovery); promoted to `/v1` after migration |
| `/types-registry/v1` | `Authorization: Bearer` | Legacy in-memory API; removed after consumer migration |

Both new route sets use interim `v2` paths while the legacy `/types-registry/v1`
API remains. T38 deletes legacy and atomically promotes them to
`/types-registry/v1/platform` (platform) and `/types-registry/v1` (tenant).

Platform routes require validated internal tokens; tenant routes require bearers.
Sending both returns `401 MIXED_CREDENTIALS`. Only platform callers may register,
delete or read operations. Routes use `exposed = false` but appear in `/cf/docs`.
Authorization is deferred: any valid internal token may mutate.

Through the api-gateway, platform routes need its `internal_auth`; without it they are
refused. For a local server, a shared secret:

```yaml
gears:
  api-gateway:
    config:
      internal_auth:
        provider: shared_secret
        secret: "dev-internal-token"
```

```bash
BASE="$TYPES_REGISTRY_INTERNAL"
PLATFORM="$BASE/types-registry/v2/platform"
TENANT="$BASE/types-registry/v2"
INTERNAL_TOKEN="dev-internal-token"   # the internal_auth secret
TOKEN="<a tenant bearer>"            # for the tenant reads
```

### Existing example server

`apps/cf-gears-example-server` already embeds the registry behind api-gateway.
Use it for local API exploration:

Prepare the same focused configuration used by the registry E2E suite, then start
the existing example server from the repository root:

```bash
.venv/bin/python tools/scripts/run_e2e.py --suite types-registry --dry-run
APP__SERVER__HOME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/types-registry-quickstart.XXXXXX")" \
APP__GEARS__API_GATEWAY__CONFIG__INTERNAL_AUTH__SECRET="${E2E_INTERNAL_TOKEN:-e2e-internal-token}" \
  cargo run -p cf-gears-example-server --no-default-features \
  --features static-tenants,static-authn,static-authz -- \
  --config target/e2e-local-types-registry.yaml

# In another terminal:
TYPES_REGISTRY_INTERNAL="http://127.0.0.1:8086"
BASE="$TYPES_REGISTRY_INTERNAL"
PLATFORM="$BASE/types-registry/v2/platform"
TENANT="$BASE/types-registry/v2"
INTERNAL_TOKEN="${E2E_INTERNAL_TOKEN:-e2e-internal-token}"
TOKEN="e2e-token-tenant-a"
```

The preparation command generates the focused YAML and prints the build/test commands
without running them. The configuration binds the registry to SQLite and configures
the gateway's internal-token authenticator and static tenant AuthN. Platform calls
send only the internal token; tenant reads send only the bearer.

The server command uses a fresh data directory, keeping this walkthrough separate
from databases left by other branches. If setting `E2E_INTERNAL_TOKEN`, export the
same value in both terminals before running these commands.

Run the existing HTTP scenarios with `make e2e-local SUITE=types-registry`.

Remote gears declare
`#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]`
(likewise for `TypesRegistryApi`) and resolve from `ClientHub` after init.
Platform calls use the process token; tenant calls forward the caller's bearer.
See the [SDK README](types-registry-sdk/README.md#both-apis-over-rest-rest-client).

## In-process API and startup seeding

Local gears resolve `PlatformTypesRegistryApi` for their own work and read-only
`TypesRegistryApi` for tenant requests. The [SDK example](types-registry-sdk/README.md#publishing-from-a-gear)
shows `deps = [types_registry]`, init-time reconciliation and read-back.

The registry publishes both contracts from its own `init()`, once its database holds the
startup seed:

- **Seeds:** linked `toolkit-gts` declarations (including bases,
  `#[toolkit_gts::gts_type_schema(..)]`, `gts_instance!`, `gts_instance_raw!`) and
  configured `entities`, using platform admission. INFO `types_registry seed set`
  reports counts; `types_registry seeded the database` reports results. Equal
  restarts keep revisions; changes use CAS; omissions do not delete.
- **Validation:** derivation bases, `$ref` targets and Instance-of types must be
  seeded; `x-gts-ref` values are not looked up. Batches fit `limits.batch_candidates`,
  with later dependencies settled on later passes under `worker.operation_timeout`.
  Failure names the seed and fails boot. Earlier batches remain stored; both APIs
  stay unpublished.
- **Database required:** without `gears.types-registry.database`, or after seed
  failure, neither contract is registered; resolution fails.
- **Uncached:** every read hits the database until T36.
- **Independent catalogues:** legacy `/v1` shares initial declarations but no later
  writes with the database until T38.
- **Multiple instances:** outbox polling can delay admission by one minute. Keep
  `worker.operation_timeout` and consumer deadlines above it.

## Examples

Examples use `cf`, the only vendor allowed by default.

### Register a Type Schema, then poll the outcome

```bash
RECEIPT=$(curl -s -X POST "$PLATFORM/entities" \
  -H "X-ToolKit-Internal-Token: $INTERNAL_TOKEN" \
  -H "Idempotency-Key: register-example-event-1" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [{
          "gts_id": "gts.cf.core.example.event.v1~",
          "content": {
            "$id": "gts://gts.cf.core.example.event.v1~",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "email": { "type": "string" } }
          }
        }]
      }')
```

A Type Schema's `content.$id` must be exactly `gts://` followed by its `gts_id`. A missing, non-string or different `$id` is a synchronous `400` naming the candidate, with a field violation on `entity` (`VALIDATION_FAILED`); no operation is created and no item in the batch is admitted. Instances have no such check.

The **202 Accepted** response includes `Location`, `Retry-After: 1` and the operation ID:

```bash
OPERATION_ID=$(printf '%s' "$RECEIPT" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["operation_id"])')
```

Follow the `Location`:

```bash
curl -s "$PLATFORM/operations/$OPERATION_ID" \
  -H "X-ToolKit-Internal-Token: $INTERNAL_TOKEN" | python3 -m json.tool
```

The completed response contains a `succeeded` item at `resource_version: 1`.

`completed` means every item is terminal. Read the entity, here as a tenant (the platform
route `$PLATFORM/entities/{key}` answers the same with the internal token):

```bash
curl -s "$TENANT/entities/gts.cf.core.example.event.v1~" \
  -H "Authorization: Bearer $TOKEN" \
  | python3 -m json.tool
```

```json
{
    "gts_id": "gts.cf.core.example.event.v1~",
    "gts_uuid": "d226dd5b-14c8-56da-a718-9cf29becaba1",
    "kind": "type_schema",
    "origin": {
        "type": "managed",
        "resource_version": 1,
        "created_at": "2026-09-15T09:15:30Z",
        "updated_at": "2026-09-15T09:15:30Z"
    },
    "lifecycle_status": "active"
}
```

### Select fields

Every read returns this document-free default unless `$select` names fields. Documents
are selected individually and returned flat: `content` (either kind), `resolved_schema`,
`effective_traits`, `effective_traits_schema` (Type Schemas only; absent on an Instance).
A Type Schema's `provenance` group, never in the default, is selected whole as
`provenance` and reads `{"compatibility_check_skipped": false}`: `true` means
`skip_compatibility_check` deliberately waived an applicable cross-minor compatibility
check; `false` means no waiver, not that a check ran (a first minor or a major-0 version
has none). An Instance omits the group even when selected:

```bash
curl -s "$TENANT/entities/gts.cf.core.example.event.v1~?\$select=content,effective_traits" \
  -H "Authorization: Bearer $TOKEN" \
  | python3 -m json.tool
```

Names are case-insensitive and order does not matter. `gts_id`, `gts_uuid`, `kind` and
`lifecycle_status` are always returned. Any other unselected field is omitted; a
selected document that is JSON `null` stays `null`. To compare authored content, select
`content`; there is no content digest. An empty, duplicate, unknown or nested name (`content.title`) is a `400` naming
`$select`, and any other query parameter is refused.

### Read a set of entities in one round trip

`:batchGet` answers every key it is given. A `POST` because a GTS identifier runs to
1024 characters, which a query string cannot carry safely, and portable `GET` has no body.
Each `entity_key` is a GTS identifier or a Registry Reference UUID:

```bash
curl -s -X POST "$TENANT/entities:batchGet" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [
          { "entity_key": "gts.cf.core.example.event.v1~" },
          { "entity_key": "d226dd5b-14c8-56da-a718-9cf29becaba1" },
          { "entity_key": "gts.cf.core.example.missing.v1~" }
        ]
      }' | python3 -m json.tool
```

```json
{
    "items": [
        { "entity_key": "gts.cf.core.example.event.v1~", "status": "found", "etag": "\"A...\"", "entity": { "...": "the default fields" } },
        { "entity_key": "d226dd5b-14c8-56da-a718-9cf29becaba1", "status": "found", "etag": "\"A...\"", "entity": { "...": "..." } },
        { "entity_key": "gts.cf.core.example.missing.v1~", "status": "not_found" }
    ]
}
```

Result order is not contractual, so a caller matches answers to questions by the echoed
`entity_key`. An identifier is echoed as sent; a Registry Reference is echoed lowercase and
hyphenated whatever UUID spelling was sent, so a caller normalizes its own UUID keys before
matching. An absent key is `not_found` inside a `200`, not a `404`: one missing key must not
lose the answers for the others.

A top-level `"$select"` applies to every key and follows the exact read's rules, so a
`found` entity is exactly what `GET /entities/{key}` returns for the same selection:

```bash
curl -s -X POST "$TENANT/entities:batchGet" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{ "$select": "content", "items": [{ "entity_key": "gts.cf.core.example.event.v1~" }] }'
```

`$select` in the query string is refused on this route, as are unknown body fields.

A key named twice collapses onto its first mention. The two spellings of one entity are two
keys and get two results. At most 100 keys per request; an empty `items` is a `400`. The
key count does not bound response bytes once documents are selected.

### Skip an unchanged body

An exact read's `ETag` and a batch result's `etag` are the same value, quotes included, for
that key and `$select`. Send it back unchanged and an unchanged entity costs no body:

```bash
URL="$TENANT/entities/gts.cf.core.example.event.v1~"
AUTH="Authorization: Bearer $TOKEN"
ETAG=$(curl -si "$URL" -H "$AUTH" | awk 'tolower($1) == "etag:" { print $2 }' | tr -d '\r')

curl -si "$URL" -H "$AUTH" -H "If-None-Match: $ETAG"
# 304 Not Modified, the same ETag, no body

curl -s -X POST "$TENANT/entities:batchGet" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "$(python3 -c 'import json, sys; print(json.dumps({"items": [{"entity_key": sys.argv[1], "if_none_match": sys.argv[2]}]}))' \
        gts.cf.core.example.event.v1~ "$ETAG")"
# {"items": [{"entity_key": "...", "status": "unchanged", "etag": "<$ETAG>"}]}
```

A revision, a refreshed dependency or another `$select` yields a new validator; discovery
pages carry none. On `:batchGet` the `If-None-Match` header is refused: validators are per key.

### Discover what exists

`GET /entities` returns **one bounded page** of entities, ordered by canonical
identifier. `lifecycle_status` is `active` by default; `deleted` lists only tombstones and
`all` lists both. Tombstones are always readable by key:

```bash
curl -s "$TENANT/entities?limit=2&pattern=gts.cf.core.*" \
  -H "Authorization: Bearer $TOKEN" \
  | python3 -m json.tool
```

```json
{
    "items": [
        {
            "gts_id": "gts.cf.core.example.event.v1~",
            "gts_uuid": "d226dd5b-14c8-56da-a718-9cf29becaba1",
            "kind": "type_schema",
            "origin": { "type": "managed", "resource_version": 1, "created_at": "...", "updated_at": "..." },
            "lifecycle_status": "active"
        }
    ],
    "page_info": { "next_cursor": "eyJ2IjoxLCJrIjpb...", "limit": 2 }
}
```

Page items take `$select` exactly as the exact read does; the default is document-free and
a page never carries a validator. Select documents on the page directly:

```bash
curl -s "$TENANT/entities?limit=20&pattern=gts.cf.core.*&\$select=content" \
  -H "Authorization: Bearer $TOKEN" \
  | python3 -m json.tool
```

Or page identifiers first and read documents for the keys you pick through `:batchGet`,
in batches of at most 100 keys. Keep the same `$select` on every
continuation and stop when `next_cursor` is absent:

```bash
# Page, collecting identifiers until next_cursor is absent.
CURSOR=""
IDS=""
while :; do
  URL="$TENANT/entities?limit=100&\$select=gts_id"
  [ -z "$CURSOR" ] || URL="$URL&cursor=$CURSOR"
  PAGE=$(curl -s "$URL" -H "Authorization: Bearer $TOKEN")
  IDS="$IDS $(echo "$PAGE" | python3 -c 'import json,sys
for item in json.load(sys.stdin)["items"]: print(item["gts_id"])')"
  CURSOR=$(echo "$PAGE" | python3 -c 'import json,sys
print(json.load(sys.stdin)["page_info"].get("next_cursor") or "")')
  [ -n "$CURSOR" ] || break
done

# Hydrate content, at most 100 keys per batchGet.
echo "$IDS" | python3 -c 'import json,sys
keys = sys.stdin.read().split()
for i in range(0, len(keys), 100):
    print(json.dumps({"$select": "content",
                      "items": [{"entity_key": k} for k in keys[i:i + 100]]}))' \
  | while read -r BODY; do
      curl -s -X POST "$TENANT/entities:batchGet" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" -d "$BODY"
    done
```

Every filter applies before the page limit, so a page with a `next_cursor` is always full,
and only the last page is short or, when nothing matches, empty. `pattern` is exact: a
concrete segment matches any minor unless the pattern names one, in any position.

### Filter by chain depth and kind

`depth` is an **inclusive maximum number of GTS identifier segments**. A one-segment root
such as `gts.cf.core.example.event.v1~` has depth 1; a Type Schema derived from it, or an
Instance of it (`gts.cf.core.example.event.v1~cf.core.example.first.v1`), has depth 2, and
each further tail adds one. So `depth=2` returns depth 1 and depth 2 alike. `kind` is
`type_schema` or `instance`. Both work with or without `pattern`, and all filters apply
before the page limit:

```bash
# Instances directly of one root, not of the types derived from it.
curl -s "$TENANT/entities?pattern=gts.cf.core.example.event.v1~*&depth=2&kind=instance" \
  -H "Authorization: Bearer $TOKEN" \
  | python3 -m json.tool
```

`depth` must be an integer from 1 to 255; `0`, negative, fractional or larger values, an
unknown `kind`, and the v1 spelling `is_schema` are `400`.

`cursor` (alias `$skiptoken`) is opaque, versioned and bound to the `pattern`, `depth`,
`kind`, `lifecycle_status` and normalized `$select` it was issued for. Resuming under any
of them changed — including adding or dropping a filter — or with a token of an unknown
version, is a `400` rather than a page spliced out of two traversals. An absent `$select`
and the explicit default set are the same selection, as are an absent `lifecycle_status`
and `active`.

`limit` (alias `$top`) defaults to 50 and may not exceed 100; `0` or `101` is a `400`.
A caller selecting documents should page smaller. `$filter`, `$orderby`, `$skip`, v1
filters such as `is_schema` or `vendor`, and any other undeclared parameter are refused.

### Rehearse a deletion, then perform it

Dry run persists a pollable prediction without changing entities:

```bash
curl -s -X DELETE \
  "$PLATFORM/entities/gts.cf.core.example.event.v1~?expected_resource_version=1&dry_run=true" \
  -H "X-ToolKit-Internal-Token: $INTERNAL_TOKEN" \
  -H "Idempotency-Key: rehearse-delete-1"
```

To commit, omit `dry_run` and use a new key; reusing the dry-run key returns `409`:

```bash
curl -s -X DELETE \
  "$PLATFORM/entities/gts.cf.core.example.event.v1~?expected_resource_version=1" \
  -H "X-ToolKit-Internal-Token: $INTERNAL_TOKEN" \
  -H "Idempotency-Key: delete-1"
```

The tombstone remains readable with an incremented version. Same-key replay returns
**200** and `Idempotency-Replayed: true`.

Deletion requires a positive `expected_resource_version`. Invalid values return
`400`; mismatches become asynchronous `precondition_failed` outcomes. `If-Match`
is rejected.

For the batch example, create a separate live schema and its instance: the preceding
single deletion has already tombstoned `event.v1~`. Submit both prerequisites together:

```bash
RECEIPT=$(curl -fsS -X POST "$PLATFORM/entities" \
  -H "X-ToolKit-Internal-Token: $INTERNAL_TOKEN" \
  -H "Idempotency-Key: register-batch-delete-example-1" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [
          {
            "gts_id": "gts.cf.core.example.batch_event.v1~",
            "content": {
              "$id": "gts://gts.cf.core.example.batch_event.v1~",
              "$schema": "http://json-schema.org/draft-07/schema#",
              "type": "object",
              "properties": { "email": { "type": "string" } }
            }
          },
          {
            "gts_id": "gts.cf.core.example.batch_event.v1~cf.core.example.first.v1",
            "content": { "email": "first@example.com" }
          }
        ]
      }')
OPERATION_ID=$(printf '%s' "$RECEIPT" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["operation_id"])')

# Admission is asynchronous. Wait at most 60 polls before checking both outcomes.
for attempt in {1..60}; do
  OPERATION=$(curl -fsS "$PLATFORM/operations/$OPERATION_ID" \
    -H "X-ToolKit-Internal-Token: $INTERNAL_TOKEN") || break
  STATUS=$(printf '%s' "$OPERATION" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])')
  [ "$STATUS" = completed ] && break
  sleep 1
done
printf '%s' "$OPERATION" | python3 -c 'import json,sys
op = json.load(sys.stdin)
if (op["status"] != "completed" or len(op["items"]) != 2
    or any(i["status"] != "succeeded" or i["resource_version"] != 1 for i in op["items"])):
    raise SystemExit("Prerequisite admission failed or timed out: " + json.dumps(op))
print("Both batch-deletion prerequisites are active at version 1")'
```

Continue only after both prerequisite items succeed. Batch deletion accepts either
key form and returns outcomes in request order, each echoing
its `entity_key`, a Registry Reference in lowercase hyphenated form. Two keys of one entity
are a duplicate and refuse the batch. This one deletes an Instance by its identifier and its
Type Schema by its Registry Reference; the worker deletes the dependant first:

```bash
curl -s -X POST "$PLATFORM/entities:batchDelete" \
  -H "X-ToolKit-Internal-Token: $INTERNAL_TOKEN" \
  -H "Idempotency-Key: delete-batch-1" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [
          { "entity_key": "gts.cf.core.example.batch_event.v1~cf.core.example.first.v1", "expected_resource_version": 1 },
          { "entity_key": "13ee6458-e7aa-5fcd-991e-85ff32ad8dde", "expected_resource_version": 1 }
        ]
      }'
```

A key that names no entity, in either form, is still accepted; only its item fails
`precondition_failed`.

`dry_run` defaults to `false`: a body field for batches and query parameter for single deletion.
