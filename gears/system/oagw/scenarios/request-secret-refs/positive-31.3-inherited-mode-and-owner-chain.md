# Use an ancestor's request_ref auth

## Setup

Apply [shared setup](README.md#shared-setup), then:

Partner, not Tenant, creates the upstream, with `"sharing": "inherit"`. Tenant has no upstream for the alias, or one with no `auth`.

Also seed these CredStore rows, so one name resolves differently per caller:

| Reference | Owner | Visible to | Type | Value |
|---|---|---|---|---|
| `shared-openai` | Tenant | Tenant | api-key | `key-T2` |
| `shared-openai` | Partner | Partner, Other | api-key | `key-P2` |
| `own-openai` | Tenant | Tenant | api-key | `key-T3` |
| `own-openai` | Other | Other | api-key | `key-O3` |

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Secret-Ref: cred://partner-openai

{"model":"gpt-4o","messages":[]}
```

## Expected response

200 OK.

## Expected outbound request

```http
POST /v1/chat/completions HTTP/1.1
Host: api.example.com
Content-Type: application/json
Authorization: Bearer key-P

{"model":"gpt-4o","messages":[]}
```

## What to check

- Partner's auth applies to Tenant, which does not override it (SREF-HIER-001).
- Partner (the auth owner) and Tenant own secrets that are injected; `platform-openai`, owned by Root, is refused with 403 `SECRET_REF_POLICY_DENIED` (SREF-GUARD-001).
- The guard compares against the auth owner from the merge, not the upstream that won alias resolution: with Tenant's own upstream without auth, the result is the same (SREF-GUARD-003).
- `shared-openai` sent by Tenant injects Tenant's own `key-T2`, and sent by Other injects Partner's shared `key-P2`; both reach the provider (SREF-RES-004). `own-openai` sent by Tenant and by Other through the SDK injects `key-T3` and `key-O3`: each tenant sends its own key (SREF-SDK-002).
- The request makes one tenant-resolver call, as alias resolution already does (SREF-GUARD-007).
- Root enforcing `request_ref` (`"sharing": "enforce"`) applies its default and `request_secret_types` to every descendant (SREF-HIER-003); a descendant override that switches the mode needs the `override_auth` grant (SREF-HIER-004).
