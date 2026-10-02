# CredStore - Quickstart

Stores a credential **record** and its optional **secret** as one item,
scoped to tenants and owners, and resolves it hierarchically — if the
caller's tenant holds no secret under a reference, resolution walks up the
tenant ancestry and returns the nearest inherited secret. `GET
.../credentials/{ref}` never carries the secret unless the caller names it
in `$select` — there is no separate address for the secret alone.

**Features:**
- Tenant-scoped credential storage with hierarchical resolution (`shared`
  credentials are inherited across isolation barriers too)
- Three sharing modes: `private` (owner only), `tenant` (all users in
  tenant), `shared` (cross-tenant)
- Record and secret are one resource: `PUT`/`PATCH` write both together (or
  the record alone); `GET` the record without ever seeing the secret, or
  add `secret` to `$select` to read it
- Six PDP actions — `list`/`read`/`write`/`delete` on the record,
  `read_secret`/`write_secret` on the secret — so a metadata grant never
  implies secret access
- Suppression: `fallback: none` lets a tenant block an inherited secret
  locally without touching the ancestor's credential
- List credential records (`$filter`/`$orderby`/`limit`/`cursor`), or bulk-read
  several secrets at once by selecting `secret`
- Immutable value versions: every write stores a new version in the backend
  and switches the record's pointer to it; no in-place overwrite, no reaper
  and no maintenance job. Deleting a record frees the reference at once and
  purges its backend key through the outbox
- Best-effort audit of secret reads and writes through `event-broker`
  (`audit_publish_failed` metric on failure)
- Access denial returned as `404` (not an error) to prevent credential
  enumeration
- Backend-agnostic: the secret is stored by a plugin selected by `vendor`
  configuration

**Use cases:**
- Storing API keys or credentials per tenant (e.g. `partner-openai-key`)
- Inheriting organization-wide credentials in child tenants without
  duplication
- Sharing credentials across tenant boundaries via `shared` mode
- Rotating a secret without touching its sharing/expiry metadata (a
  `write_secret`-only caller)
- Editing metadata without ever handling the secret (a `write`-only caller)
- Suppressing an inherited credential for one tenant (and its descendants)
  without altering the ancestor's

Full API documentation: <http://127.0.0.1:8087/cf/docs>

The example server uses the gear prefix `/cf`. This comes from `gears.api-gateway.config.prefix_path` and is configurable.

## Configuration

```yaml
gears:
  credstore:
    config:
      vendor: "constructorfabric"  # selects backend plugin by vendor name (default: "constructorfabric"; "constructorfabric" -> static-credstore-plugin, "openbao" -> vault-credstore-plugin)
      hierarchy:
        ancestor_cache_ttl_secs: 300 # ancestor-chain cache TTL (default: 300)
      list:
        max_limit: 200              # cap for a metadata-mode page's `limit` (default: 200)
        secret_mode_cap: 25         # cap on how many references a secret-mode ($select=…,secret) request may match (default: 25)
```

There is no `reaper:` or `gc:` block: the gear runs no resident loop and no
maintenance job, and unknown config keys (including those two) are rejected
at startup.

**Secrets are provisioned only through this API.** A backend plugin (e.g.
`static-credstore-plugin` for development) (`CredStorePluginClientV2`) is a versioned
byte store keyed by `(tenant_id, record_id)`: `put` returns the version, `get`
reads one, `delete_key` drops the key (called by the outbox after a record
delete), and `destroy` is optional (`supports_destroy`). There is no way to
seed a value directly in the plugin's own configuration: the static plugin's
config carries only `vendor` and `priority`, and any other key (including the
withdrawn `secrets` block) fails validation at boot. It is a non-durable
in-memory store for development and tests only and logs a WARN saying so at
startup. Always create/rotate a credential with `PUT`/`PATCH` below so a
record exists.

Two backend plugins currently exist: `static-credstore-plugin` (in-memory,
for dev/test, feature `static-credstore`) and `vault-credstore-plugin`
(Vault/OpenBao KV v2, prototype, feature `vault-credstore`, vendor
`"openbao"`) — see its
[README](plugins/vault-credstore-plugin/README.md) for local setup against
OpenBao.

## PDP actions and permissions

Every permission id below has the prefix
`cf.toolkit.authz.permission.v1~cf.core.credstore.`; only the distinguishing
suffix is shown.

| PDP action | Gates | Permission id suffix |
|---|---|---|
| `list` | `GET /credentials` (metadata mode) | `credential_list.v1` |
| `read` | `GET /credentials/{ref}` (no `$select`, or `$select` naming a record field) | `credential_read.v1` |
| `write` | `PUT`/`PATCH` — metadata fields (`type`, `sharing`, `fallback`, `expires_at`) | `credential_write.v1` |
| `delete` | `DELETE /credentials/{ref}` | `credential_delete.v1` |
| `read_secret` | `GET /credentials[/{ref}]` when `$select` names `secret` | `secret_read.v1` |
| `write_secret` | `PUT`/`PATCH` — `secret` field | `secret_write.v1` |

`PUT` requires `write`, plus `write_secret` when it writes a secret or when
a `null` removes an existing one; `write` alone suffices when its `secret`
is a `null` that creates or leaves a secret-less record (this exemption is
`PUT`-only). `PATCH` requires `write` for metadata keys and `write_secret`
for any `secret` key, string or `null`, unconditionally (both when both
are present). All required actions are evaluated before any side effect.

## Examples

### Create a credential

`PUT` with `If-None-Match: *` writes the record and its secret together,
atomically, and only if the caller's own tenant does not already hold one
under the reference.

```bash
curl -s -X PUT "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H 'If-None-Match: *' \
  -d '{"type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~", "sharing": "tenant", "secret": "sk-abc123"}'
```

Response: **201 Created**
```
Location: /cf/credstore/v1/credentials/partner-openai-key
ETag: "3fa85f64-5717-4562-b3fc-2c963f66afa6.1"
```

`secret` is required in every `PUT` body — its absence is **400**
(`SECRET_REQUIRED`); `type` is required on create — its absence is **400**
(`TYPE_REQUIRED`). `If-None-Match: *` conflicts with **409** if the caller's
own tenant already holds a record under the reference. Replacing an
existing record uses `If-Match` instead — `"<id>.<version>"` for a guarded
replace, or `*` for last-writer-wins — never both headers together (**400**
`PRECONDITION_REQUIRED`).

### Get the credential record

```bash
curl -si "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN"
```

Response: **200 OK** (`ETag`, `Cache-Control: no-store`)
```json
{
  "reference": "partner-openai-key",
  "type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~",
  "sharing": "tenant",
  "fallback": "inherit",
  "status": "active",
  "inheritance": "own",
  "version": 1,
  "updated_at": "2026-09-11T12:00:00Z",
  "owner_id": "5b1b1c8a-2222-4d3e-9a1a-000000000001",
  "expires_at": null
}
```

Never carries the secret unless `$select` names it. `fallback`, `version`,
`updated_at` and `owner_id` are present only while the caller's own tenant
holds a row under the reference (`status` other than `none`); `owner_id` is
populated from the caller's **own** row only, never from an ancestor's —
even while `inheritance` reads `inherited`.

### Get the secret

Naming `secret` in `$select` on the same address discloses it — there is no
separate address for the secret alone.

```bash
curl -si "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key?\$select=reference,type,expires_at,secret" \
  -H "Authorization: Bearer $TOKEN"
```

Response: **200 OK** (`ETag`, `Cache-Control: no-store`)
```json
{
  "reference": "partner-openai-key",
  "type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~",
  "expires_at": null,
  "secret": "sk-abc123"
}
```

A winning record with no secret (`declared`, or `suppressed`) is the
canonical **404** — indistinguishable from "does not exist". Requires
`read_secret`; a `$select` naming both a record field and `secret` requires
`read` and `read_secret` together.

**Expired records.** Expiry applies to the secret, not to the record. Once an
`active` record's `expires_at` has passed, a read without `secret` still
returns it, with `status: "expired"` and its normal `ETag`; a read that
selects `secret` fails **409** with reason `SECRET_EXPIRED` (only for a
caller allowed to read the secret — anyone else gets the usual **404**), and
the answer never falls through to an ancestor's value. In the collection's
secret mode an expired item is returned with `status: "expired"` and no
`secret`. Renew it in place with
`PATCH` `{"expires_at": "<future RFC 3339 instant>"}` (or a replace); a
create-only `PUT` over it is **409** `ALREADY_EXISTS`.

### Rotate the secret

A guarded merge-patch carrying only `secret`; `sharing`/`fallback`/`type`
are left exactly as they were.

```bash
curl -si -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "3fa85f64-5717-4562-b3fc-2c963f66afa6.1"' \
  -d '{"secret": "sk-def456"}'
```

Response: **204 No Content** (`ETag` bumped to
`"3fa85f64-5717-4562-b3fc-2c963f66afa6.2"` — the validator the next write
needs). `Content-Type` must be exactly `application/merge-patch+json` —
anything else is **415** (`UNSUPPORTED_MEDIA_TYPE`). `If-Match` is
mandatory (missing → **400** `IF_MATCH_REQUIRED`; malformed → **400** `INVALID_IF_MATCH`; stale → **409** `OPTIMISTIC_LOCK_FAILURE`);
`If-None-Match` on a `PATCH` is **400**. This call always writes and bumps
`version`, even on identical bytes. Requires `write_secret` only.

### Edit metadata only

No `secret` key at all, so the secret is untouched; a caller holding
`write` but not `write_secret` can make this call. Its `If-Match` is the
`ETag` the rotate above just returned, not the one the record was created
with — every write, including this metadata-only one, bumps `version` and
invalidates the validator that preceded it.

```bash
curl -si -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "3fa85f64-5717-4562-b3fc-2c963f66afa6.2"' \
  -d '{"sharing": "shared"}'
```

Response: **204 No Content** (`ETag` bumped to
`"3fa85f64-5717-4562-b3fc-2c963f66afa6.3"`). A body whose metadata already
matches the current record and carries no `secret` key is a no-op
(**204**, unchanged `ETag`); a body touching nothing at all, `{}`, is
**400** (`EMPTY_PATCH`). `sharing`, `fallback` and `type` reject a
merge-patch `null` (**400** `NULL_NOT_ALLOWED`) — only `expires_at` and
`secret` accept it.

### Remove the secret, keep the record

`{"secret": null}` alone removes the secret — the record becomes `declared`
and what the reference then resolves to is decided by whatever `fallback`
is already set to. `If-Match` is again the `ETag` the previous write
returned — here, the metadata edit's `...3`.

```bash
curl -si -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "3fa85f64-5717-4562-b3fc-2c963f66afa6.3"' \
  -d '{"secret": null}'
```

**Suppress an inherited credential** you already have an own record for, in
one request, by setting `fallback` and clearing the secret together — the
reference then resolves as absent for this tenant (and, if `shared`, its
descendants) instead of falling through to an ancestor's secret, without
altering the ancestor's credential at all:

```bash
curl -si -X PATCH "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/merge-patch+json" \
  -H 'If-Match: "3fa85f64-5717-4562-b3fc-2c963f66afa6.3"' \
  -d '{"fallback": "none", "secret": null}'
```

An alternative to "Remove the secret, keep the record" above, from the
same starting `ETag` — not chained after it. Both are **204 No Content**. Deleting the record (below) removes the
suppression too, so the reference resolves through inheritance again.

**Suppress an inherited credential you do not own** — blocking a partner's
shared credential in one request, without ever holding a secret of your
own: a create-only `PUT` whose `secret` is an explicit `null` creates the
record directly in the secret-less `declared` state with `fallback:
"none"` — no backend call is made, since there is no secret to write.
Requires only the `write` PDP action.

```bash
curl -s -X PUT "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H 'If-None-Match: *' \
  -i \
  -d '{"sharing": "tenant", "type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.basic_auth.v1~", "fallback": "none", "secret": null}'

# 201 Created
# { "reference": "partner-openai-key", "status": "declared", "inheritance": "suppressed", … }
```

### List credential records

```bash
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials?limit=20&\$filter=type+eq+'gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~'" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
```

Response: **200 OK** (`Cache-Control: no-store`) — one reduced record per
reference, never a `secret` field:
```json
{
  "items": [
    {
      "reference": "partner-openai-key",
      "type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~",
      "sharing": "tenant",
      "fallback": "inherit",
      "status": "active",
      "inheritance": "own",
      "version": 1,
      "updated_at": "2026-09-11T12:00:00Z",
      "owner_id": "5b1b1c8a-2222-4d3e-9a1a-000000000001"
    }
  ],
  "page_info": {"next_cursor": null, "prev_cursor": null, "limit": 20}
}
```

`limit` defaults to 50 and is capped at `list.max_limit` (default 200; above
it → **400** `INVALID_LIMIT`); pass the previous page's `page_info.next_cursor`
as `cursor` to continue. `$filter` accepts `reference`/`type` (`eq`/`in`)
and `sharing`/`fallback`/`expires_at`; `$orderby` accepts only `reference`.
Requires `list`.

### Bulk-read secrets (secret mode)

Selecting `secret` in `$select` switches the collection into secret mode:
bounded, unpaginated, one request for several secrets at once.

```bash
curl -s "http://127.0.0.1:8087/cf/credstore/v1/credentials?\$filter=reference+in+('smtp-default','stripe-key')&\$select=reference,type,secret" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
```

Response: **200 OK** — the same page envelope, `page_info.next_cursor`
always `null`; each item additionally carries the decrypted `secret`:
```json
{
  "items": [
    {"reference": "smtp-default", "type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.generic.v1~", "sharing": "tenant", "status": "active", "inheritance": "own", "secret": "smtp-pass"}
  ],
  "page_info": {"next_cursor": null, "prev_cursor": null, "limit": 25}
}
```

`limit`/`cursor` are rejected (**400** `SECRET_MODE_NO_PAGINATION`),
`$orderby` is rejected (**400** `SECRET_MODE_NO_ORDER`), and `$filter` must
be exactly `reference eq/in (...)` or `type eq/in (...)` (**400**
`SECRET_MODE_SELECTOR`). A match set over `list.secret_mode_cap` (default 25)
fails the whole request with **400** `TOO_MANY_MATCHES` rather than
truncating it. A refused or missing item is
omitted, never reported; an expired item, or one whose stored version the
backend cannot return (**409** `SECRET_UNREADABLE` on a point read), comes
back with its metadata and without a secret. Requires `read_secret`, evaluated per item.

### Delete a credential

```bash
curl -si -X DELETE "http://127.0.0.1:8087/cf/credstore/v1/credentials/partner-openai-key" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'If-Match: *'
```

Response: **204 No Content**. `If-Match` is mandatory: a version validator,
or `*` to delete whatever is there. Removes the record in one transaction and releases the reference at once
(a create-only `PUT` right after succeeds); the secret's backend key is purged
asynchronously by the outbox.

## Using the SDK

Consumers inside the platform read a secret directly from `ClientHub`
without going through REST:

```rust,no_run
use credstore_sdk::{CredStoreClientV1, SecretRef};
use toolkit::ClientHub;
use toolkit_security::SecurityContext;

async fn secret_length(
    hub: &ClientHub,
    security: &SecurityContext,
) -> Result<Option<usize>, Box<dyn std::error::Error>> {
    let credstore = hub.get::<dyn CredStoreClientV1>()?;
    let key = SecretRef::new("partner-openai-key")?;
    let secret = credstore.get_secret(security, &key).await?;

    Ok(secret.map(|s| s.secret.as_bytes().len()))
}
```

A missing or inaccessible credential is `Ok(None)`; an explicit denial of
`read_secret` is `Err(CredStoreError::AccessDenied)`. The record read (metadata
only, never the value) is `get_record`. A version the backend cannot return is
`Err(CredStoreError::SecretUnreadable)` (REST **409** `SECRET_UNREADABLE`):
retrying does not help, rewrite the secret (`PUT`/`PATCH`) or delete the record.

For every endpoint's full parameter and schema reference, see
<http://127.0.0.1:8087/cf/docs>.
