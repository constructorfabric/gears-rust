# Unresolvable transform plugin is skipped, not blocking

## Setup

Bind a `plugin_ref` that does not correspond to any registered builtin or stored custom plugin:

```json
{
  "plugins": {
    "sharing": "private",
    "items": [
      {
        "plugin_ref": "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.nonexistent.v1",
        "config": {}
      }
    ]
  }
}
```

## Inbound request

```http
POST /api/oagw/v1/proxy/<alias>/echo HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
```

## Expected response

- `200 OK` — the request proceeds and reaches the upstream normally. An unresolvable transform plugin reference is logged and skipped rather than failing the request.

## What to check

- This fail-open behavior is specific to **transform** plugins. It is not a general statement about guard or auth plugins — a guard plugin's job is specifically to reject, so an unresolvable guard reference should not be assumed to fail open the same way; verify independently if that distinction matters for a given integration.
