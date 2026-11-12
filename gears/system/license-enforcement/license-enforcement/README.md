# License Enforcement

Stores grants and packs, gives the management operations on them, and answers `is_licensed`
as a backend plugin of `license-resolver`. The design is in [`../docs/DESIGN.md`](../docs/DESIGN.md),
the contract in [`../license-enforcement-sdk`](../license-enforcement-sdk/README.md).

The binary links exactly one admission plugin and one evaluator; the gear does not start
otherwise. The defaults are [`issue-admission-plugin`](../plugins/issue-admission-plugin) and
[`default-evaluator`](../plugins/default-evaluator).

## Not implemented yet

These return `unimplemented`:

- `import_evidence`, and evidence that replaces other evidence (supersession);
- recipient rules: `issue` refuses a grant with them, because the gear does not build the
  recipient projection that `delegate` evaluates them against;
- conditions added at `delegate`: the evaluator cannot combine them with the parent conditions
  yet, so a sub-grant takes the conditions of its parent. `replace` moves a delegation only to a
  successor whose conditions equal those of the sub-grant; it changes together with this item.

## Configuration

```yaml
license-enforcement:
  database:
    server: "sqlite_users"
    file: "license_enforcement.db"
    params:
      # SQLite only: mutating operations wait for the write lock instead of
      # failing at once beside another writer; checks stay deferred.
      transaction_mode: "immediate"
  config:
    vendor: "constructorfabric"    # license-resolver selects its backend by vendor
    priority: 50                   # lower wins among backends of the same vendor
    idempotency_window_secs: 86400
    default_page_size: 50
    max_page_size: 1000
    max_candidates: 100
    retention_days: 400
    sweep_interval_secs: 3600
```

The values under `config` are the defaults. The gear does not start when a value is out of range.
On SQLite, set `transaction_mode: immediate`. Otherwise the database layer (`toolkit-db`) begins each transaction as
`deferred`, and a mutating operation fails at once, instead of waiting, when another connection holds the write lock.

| Key | Meaning | Range |
|---|---|---|
| `vendor` | Vendor of the backend that the gear registers in `license-resolver`; `license-resolver` selects its backend by vendor. | any string |
| `priority` | Priority among the backends of the same vendor; the lower value wins. 50 wins over the default of the static license plugin (100). | `i16` |
| `idempotency_window_secs` | How long the answer to a mutating request is kept: a repeat from the same caller (the same tenant and subject of the security context) with the same idempotency token in this window gets that answer. | 1 s ..= one year |
| `default_page_size` | Page size of a list without `limit`. | 1 ..= `max_page_size` |
| `max_page_size` | Largest `limit` of a list. | `default_page_size` ..< `u64::MAX` |
| `max_candidates` | Most candidate grants that one check reads. | 1 ..= 1000 |
| `retention_days` | How long a terminated grant is kept, counted from its last change, before the sweeper deletes it. A grant never goes before the grants delegated from it. A terminated root (an issued grant) goes with every grant left in its tree, and only when all of them are terminated. A terminated sub-grant (a grant delegated from another grant) goes on its own once no grant delegated from it is left. | 1 ..= 36 500, and `retention_days` × 86 400 ≥ `idempotency_window_secs` |
| `sweep_interval_secs` | Pause between two runs of the sweeper, which deletes terminated grants after `retention_days` and the expired answers of `idempotency_window_secs`. | 1 s ..= one week |

## Known limits

- No operation writes audit events or emits metrics yet. The history of changes made before
  audit exists cannot be restored later, so turn the gear on only in development environments
  until then.
- A grant keeps the id of the plugin that admitted its evidence. A binary that links another
  plugin denies every check on such grants as `chain_invalid`. To change the plugin, issue the
  grants again.
- The default of `max_candidates` is an estimate, not a measurement: the check benchmark of the
  NFR does not exist yet.
- `MAX_EVIDENCE_LEN` of the SDK applies when `import_evidence` is implemented.
- `renew` of a grant or a pack returns `unimplemented` when the linked plugin verifies evidence: the
  window comes from the evidence there, so a renewal needs new evidence. That is `replace` when the
  plugin also admits `issue` requests, and evidence supersession (not implemented yet) otherwise.
- `replace` moves a delegation only to a successor whose `max_depth` is one more than that of the
  sub-grant. A new plan with a deeper delegation does not take the existing delegations: the
  grantors recall them first, or the vendor keeps the depth.
