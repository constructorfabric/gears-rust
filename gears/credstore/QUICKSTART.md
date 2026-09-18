# CredStore - Quickstart

Stores, retrieves, and deletes secrets scoped to tenants and owners. Secrets are resolved hierarchically — if a secret is not found in the requesting tenant, the gears walks up the tenant ancestry and returns the nearest inherited value.

**Features:**
- Tenant-scoped secret storage with hierarchical resolution
- Three sharing modes: `private` (owner only), `tenant` (all users in tenant), `shared` (cross-tenant)
- Access denial returned as `404` (not an error) to prevent secret enumeration
- Backend-agnostic: storage is delegated to a plugin selected by `vendor` configuration

**Use cases:**
- Storing API keys or credentials per tenant (e.g. `partner-openai-key`)
- Inheriting organization-wide secrets in child tenants without duplication
- Sharing secrets across tenant boundaries via `shared` mode

Full API documentation: <http://127.0.0.1:8087/cf/docs>

The example server uses the gear prefix `/cf`. This comes from `gears.api-gateway.config.prefix_path` and is configurable.

## Configuration

```yaml
gears:
  credstore:
    vendor: "constructorfabric"  # Selects backend plugin by vendor name (default: "constructorfabric")
```

## Examples

The examples below are the **current** surface — everything here works today.

### Store a Secret

```bash
curl -s -X POST "http://127.0.0.1:8087/cf/credstore/v1/secrets" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"reference": "partner-openai-key", "value": "sk-abc123", "sharing": "tenant"}'
```

Response: **201 Created** (`Location: …/credstore/v1/secrets/partner-openai-key`)

### Update (Rotate) a Secret

Updates require an `If-Match` precondition: the current `ETag` from GET for a guarded compare-and-set, or `*` for an explicit last-writer-wins overwrite. A `PUT` never creates — use `POST` above.

```bash
curl -s -X PUT "http://127.0.0.1:8087/cf/credstore/v1/secrets/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H 'If-Match: *' \
  -d '{"value": "sk-def456", "sharing": "tenant"}'
```

Response: **204 No Content** (missing `If-Match` → **400** `IF_MATCH_REQUIRED`; stale `ETag` → **409** `OPTIMISTIC_LOCK_FAILURE`)

### Retrieve a Secret

```bash
curl -s "http://127.0.0.1:8087/cf/credstore/v1/secrets/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
```

**Output:**
```json
{
    "value": "sk-abc123",
    "owner_tenant_id": "a1b2c3d4-0000-0000-0000-000000000000",
    "sharing": "tenant",
    "is_inherited": false
}
```

`is_inherited: true` indicates the secret was resolved from an ancestor tenant.

### Delete a Secret

```bash
curl -s -X DELETE "http://127.0.0.1:8087/cf/credstore/v1/secrets/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'If-Match: *'
```

Response: **204 No Content** (`If-Match` is mandatory here too: an `ETag` for a guarded delete, `*` to delete whatever is there)

For additional endpoints, see <http://127.0.0.1:8087/cf/docs>.

## Planned surface (ADR-0004 / ADR-0005 / ADR-0007 / ADR-0008)

The examples in this section describe a **proposed** direction — [ADR-0004](docs/ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md),
[ADR-0005](docs/ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md),
[ADR-0007](docs/ADR/0007-cpt-cf-credstore-adr-record-write-verbs.md) and
[ADR-0008](docs/ADR/0008-cpt-cf-credstore-adr-suppression-fallback.md) —
and are **not implemented**. Nothing below will work against the server today;
the current surface is `/credstore/v1/secrets…`, shown above. Under the
proposed model a credential and its optional secret share one item shape,
addressed under `/credstore/v1/credentials…`; the secret is a `$select`able
field of that same item, not a separate resource.

> **Creating a credential is one request:** `PUT .../credentials/{ref}` with
> `If-None-Match: *` carries the record and its secret together, written
> atomically. A merge-`PATCH` on the same address —
> `Content-Type: application/merge-patch+json` — edits metadata, rotates the
> secret, or removes it (`{"secret": null}`) without recreating the record; it
> never creates, so a `PATCH` against a reference with no own record is a
> 404. Both verbs are checked against the same `ETag` — there is one
> resource and one validator, not a record and a separate secret
> sub-resource.

**List credential records** — demonstrates the new metadata listing, bounded
with `limit` and filtered with the platform `$filter` syntax; the response
never carries a secret. Requires the `list` PDP action.

```text
# NOT IMPLEMENTED — planned, ADR-0005
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials?limit=20&\$filter=type+eq+'gts.cf.core.credstore.credential.v1~cf.core.credstore.basic_auth.v1~'" \
  -H "Authorization: Bearer $TOKEN"
```

**Get one credential record** — demonstrates the point metadata read; the
response carries no secret but does carry the `ETag`, which a secret-blind
caller uses as the CAS validator for a later write. Requires the `read`
PDP action.

```text
# NOT IMPLEMENTED — planned, ADR-0004
curl -si "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN"
# → 200, body has no "secret" field; response carries an ETag header
```

**Read the secret** — demonstrates reading the secret by naming it in
`$select` on the point read; there is no separate sub-resource address. The
body carries only the fields selected — here the secret with its type and
expiry — and the record's `ETag` in the header. Requires the `read_secret`
PDP action.

```text
# NOT IMPLEMENTED — planned, ADR-0004
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key?\$select=reference,type,expires_at,secret" \
  -H "Authorization: Bearer $TOKEN"
```

**Create a credential** — demonstrates atomic creation: the record and its
secret are written together, under the create-only precondition. Requires
both the `write` and `write_secret` PDP actions.

```text
# NOT IMPLEMENTED — planned, ADR-0007
curl -s -X PUT "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H 'If-None-Match: *' \
  -i \
  -d '{"sharing": "tenant", "type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.basic_auth.v1~", "secret": "sk-abc123"}'

# 201 Created
# Location: /cf/credstore/v1/credentials/partner-openai-key
# ETag: "7f3a…-…-…c1.1"     <- the validator later writes need
```

**Rotate the secret** — demonstrates a guarded partial update carrying only
`secret`, keyed off the record's `ETag` (from the record read above, or from
the `201` of creation above). `PATCH` follows RFC 7396 merge-patch
semantics — fields absent from the body are untouched — so this call
changes nothing but the secret; it always writes and bumps `version`, even on
identical bytes. Requires the `write_secret` PDP action, and notably not
`read_secret` — this is the write a secret-blind configurator performs.

```text
# NOT IMPLEMENTED — planned, ADR-0007
curl -s -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "<etag-of-the-record>"' \
  -d '{"secret": "sk-def456"}'
```

**Edit metadata without touching the secret** — demonstrates a partial update
that carries no `secret` key at all: the secret is left exactly as it was, so
this call can be made by a caller holding `write` but not `write_secret`.
Requires the `write` PDP action.

```text
# NOT IMPLEMENTED — planned, ADR-0007
curl -s -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "<etag-of-the-record>"' \
  -d '{"sharing": "tenant"}'
```

**Remove the secret, keep the record** — demonstrates the only way to reach
the secret-less `declared` state: a partial update whose `secret` is `null`.
The record's metadata and `fallback` are untouched; what the
reference then resolves to is decided by `fallback`, not by this call.
Requires the `write_secret` PDP action.

```text
# NOT IMPLEMENTED — planned, ADR-0007
curl -s -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "<etag-of-the-record>"' \
  -d '{"secret": null}'
```

**Bulk read secrets, explicit selector** — demonstrates the bounded,
non-paginated bulk secret read: naming `secret` in `$select` on the collection
switches it into secret mode, scoped here by an explicit `reference in (...)`
list. Requires `read_secret`, evaluated per item; `limit`/`cursor` are
rejected; cap 25.

```text
# NOT IMPLEMENTED — planned, ADR-0005
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials?\$filter=reference+in+('smtp-default','stripe-key','webhook-signing')&\$select=reference,type,secret" \
  -H "Authorization: Bearer $TOKEN"
```

**Bulk read secrets, scoped selector** — demonstrates the same secret
mode scoped by `type` instead of an explicit reference list; still capped,
still per-item authorized. Requires `read_secret`, evaluated per item;
`limit`/`cursor` are rejected; cap 25.

```text
# NOT IMPLEMENTED — planned, ADR-0005
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials?\$filter=type+eq+'<gts id>'&\$select=reference,type,secret" \
  -H "Authorization: Bearer $TOKEN"
```

**Suppress an inherited credential you do not own** — demonstrates blocking
a partner's shared credential in one request, without ever holding a secret
of your own: a create-only `PUT` whose `secret` is an explicit `null`
creates the record directly in the secret-less `declared` state with
`fallback: none` — no backend call is made, since there is no secret to
write. Requires only the `write` PDP action.

```text
# NOT IMPLEMENTED — planned, ADR-0008
curl -s -X PUT "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H 'If-None-Match: *' \
  -i \
  -d '{"sharing": "tenant", "type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.basic_auth.v1~", "fallback": "none", "secret": null}'

# 201 Created
# { "reference": "partner-openai-key", "status": "declared", "inheritance": "suppressed", … }
```
