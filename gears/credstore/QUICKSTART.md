# CredStore - Quickstart

Stores, retrieves, and deletes secrets scoped to tenants and owners, resolved hierarchically: a missing reference walks up the tenant ancestry to the nearest inherited value.

**Features:**
- Tenant-scoped secret storage with hierarchical resolution
- Three sharing modes: `private` (owner only), `tenant` (all tenant users), `shared` (cross-tenant)
- Access denial returns `404` (not an error), preventing secret enumeration
- Backend-agnostic: storage via a plugin selected by `vendor` config

**Use cases:**
- Storing API keys or credentials per tenant (e.g. `partner-openai-key`)
- Inheriting organization-wide secrets in child tenants without duplication
- Sharing secrets across tenant boundaries via `shared` mode

Full API documentation: <http://127.0.0.1:8087/cf/docs>

The example server uses gear prefix `/cf` (from `gears.api-gateway.config.prefix_path`, configurable).

## Configuration

```yaml
gears:
  credstore:
    vendor: "constructorfabric"  # Selects backend plugin by vendor name (default: "constructorfabric")
```

## Examples

The examples below are the **current** surface: everything works today.

### Store a Secret

```bash
curl -s -X POST "http://127.0.0.1:8087/cf/credstore/v1/secrets" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"reference": "partner-openai-key", "value": "sk-abc123", "sharing": "tenant"}'
```

Response: **201 Created** (`Location: …/credstore/v1/secrets/partner-openai-key`)

### Update (Rotate) a Secret

Updates require an `If-Match` precondition: the `ETag` from GET for a guarded compare-and-set, or `*` for last-writer-wins. `PUT` never creates — use `POST` above.

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

`is_inherited: true` means the secret resolved from an ancestor tenant.

### Delete a Secret

```bash
curl -s -X DELETE "http://127.0.0.1:8087/cf/credstore/v1/secrets/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'If-Match: *'
```

Response: **204 No Content** (`If-Match` mandatory here too: `ETag` for a guarded delete, `*` to delete whatever is there)

For additional endpoints, see <http://127.0.0.1:8087/cf/docs>.

## Planned surface (ADR-0004 / ADR-0005 / ADR-0007 / ADR-0008)

This section is a **proposed** direction — [ADR-0004](docs/ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md), [ADR-0005](docs/ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md), [ADR-0007](docs/ADR/0007-cpt-cf-credstore-adr-record-write-verbs.md), [ADR-0008](docs/ADR/0008-cpt-cf-credstore-adr-suppression-fallback.md) — **not implemented**; nothing here works today. It gives a credential and its secret one item shape under `/credstore/v1/credentials…`; the secret is a `$select`able field, not a separate resource.

> **Creating a credential is one request:** `PUT .../credentials/{ref}` with `If-None-Match: *` writes record and secret together. A merge-`PATCH` edits metadata or rotates/removes the secret (`{"secret": null}`) but never creates — a reference with no own record gets a 404. Both check the same `ETag`.

**List credential records** — metadata listing bounded by `limit`, filtered by `$filter`; never carries a secret. Requires `list`.

```text
# NOT IMPLEMENTED — planned, ADR-0005
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials?limit=20&\$filter=type+eq+'gts.cf.core.credstore.credential.v1~cf.core.credstore.basic_auth.v1~'" \
  -H "Authorization: Bearer $TOKEN"
```

**Get one credential record** — point metadata read; carries no secret, but the `ETag` a secret-blind caller uses as the CAS validator. Requires `read`.

```text
# NOT IMPLEMENTED — planned, ADR-0004
curl -si "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN"
# → 200, body has no "secret" field; response carries an ETag header
```

**Read the secret** — name it in `$select` on the point read; there is no separate address. The body carries only the selected fields — here secret, type, expiry — plus the `ETag` header. Requires `read_secret`.

```text
# NOT IMPLEMENTED — planned, ADR-0004
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key?\$select=reference,type,expires_at,secret" \
  -H "Authorization: Bearer $TOKEN"
```

**Create a credential** — record and secret written together under the create-only precondition. Requires `write` and `write_secret`.

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

**Rotate the secret** — a guarded partial update carrying only `secret`, keyed off the record's `ETag`. `PATCH` (RFC 7396) leaves absent fields untouched; it always writes and bumps `version`, even on identical bytes. Requires `write_secret`, not `read_secret` — a secret-blind configurator's write.

```text
# NOT IMPLEMENTED — planned, ADR-0007
curl -s -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "<etag-of-the-record>"' \
  -d '{"secret": "sk-def456"}'
```

**Edit metadata without touching the secret** — a partial update with no `secret` key, usable with `write` but not `write_secret`. Requires `write`.

```text
# NOT IMPLEMENTED — planned, ADR-0007
curl -s -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "<etag-of-the-record>"' \
  -d '{"sharing": "tenant"}'
```

**Remove the secret, keep the record** — a partial update with `secret: null`, the only way to reach the secret-less `declared` state. Metadata and `fallback` stay untouched; `fallback` then decides resolution. Requires `write_secret`.

```text
# NOT IMPLEMENTED — planned, ADR-0007
curl -s -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "<etag-of-the-record>"' \
  -d '{"secret": null}'
```

**Bulk read secrets, explicit selector** — bounded, non-paginated: naming `secret` in `$select` switches the collection into secret mode, scoped by an explicit `reference in (...)` list. Requires `read_secret` per item; `limit`/`cursor` rejected; cap 25.

```text
# NOT IMPLEMENTED — planned, ADR-0005
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials?\$filter=reference+in+('smtp-default','stripe-key','webhook-signing')&\$select=reference,type,secret" \
  -H "Authorization: Bearer $TOKEN"
```

**Bulk read secrets, scoped selector** — same secret mode, scoped by `type`; still capped and per-item authorized. Requires `read_secret` per item; `limit`/`cursor` rejected; cap 25.

```text
# NOT IMPLEMENTED — planned, ADR-0005
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials?\$filter=type+eq+'<gts id>'&\$select=reference,type,secret" \
  -H "Authorization: Bearer $TOKEN"
```

**Suppress an inherited credential you do not own** — a create-only `PUT` with `secret: null` creates the record directly in the secret-less `declared` state with `fallback: none`, without ever holding a secret; no backend call is made. Requires only `write`.

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
