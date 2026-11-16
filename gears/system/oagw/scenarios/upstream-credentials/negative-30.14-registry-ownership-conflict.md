# Reject invalid registry ownership (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "declaration": {
    "upstream_id": "<upstream-uuid>",
    "name": "registry-key",
    "secret_ref": "cred://provider-research"
  },
  "parent_managed_by": "api"
}
```

## Inbound request

Submit this fixture payload through the existing Types Registry provisioning path, then run reconciliation. This scenario defines no new registry endpoint.

```json
{"upstream_id":"<upstream-uuid>","name":"registry-key","secret_ref":"cred://provider-research"}
```

## Expected response

Provisioning result. Validate the resource response or operation result described below.

## What to check

- Reconcile reports invalid configuration and writes no credential under an API parent, including a concurrent parent delete.
- With a registry parent, provisioning succeeds. If an API credential already holds the name, report credential_name_conflict and preserve it; retry on later passes.
- Duplicate declarations keep last valid rows; inaccessible secrets retry. Replay preserves UUID, suspension, approvals and unchanged generation.
- A surviving API child retains its registry parent during cleanup. Removed and reappearing declarations get new UUIDs.
- Old API parent deletion cascades API credentials without FK failure or orphans. Old registry writers stay fenced during rollback.
