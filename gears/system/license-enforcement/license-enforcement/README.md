# License Enforcement

Stores grants and packs, gives the management operations on them, and answers `is_licensed`
as a backend plugin of `license-resolver`. The design is in [`../docs/DESIGN.md`](../docs/DESIGN.md),
the contract in [`../license-enforcement-sdk`](../license-enforcement-sdk/README.md).

The binary links exactly one admission plugin and one evaluator; the gear does not start
otherwise. The defaults are [`issue-admission-plugin`](../plugins/issue-admission-plugin) and
[`default-evaluator`](../plugins/default-evaluator).

## Implemented

- `issue_grant`, `issue_pack` through the `issue` intake;
- `bind_grant`, `unbind_grant`;
- `get_grant`, `get_pack`, `list_grants`;
- the check: `is_licensed` of license-resolver.

Every other operation of the contract returns `unimplemented` for now.

## Configuration

```yaml
license-enforcement:
  database:
    server: "sqlite_users"
    file: "license_enforcement.db"
  config:
    vendor: "constructorfabric"    # license-resolver selects its backend by vendor
    priority: 50                   # lower wins among backends of the same vendor
    idempotency_window_secs: 86400
    max_candidates: 100            # most candidate grants that one check reads
```

## Known limits

- `issue`, `bind`, and `unbind` write no audit events and emit no metrics yet. The history of
  these changes before audit exists cannot be restored later, so turn the gear on only in
  development environments until then.
- Nothing deletes expired idempotency records yet: a record goes only when its tenant sends the
  same token again, so the table grows with every mutation.
- A grant keeps the id of the plugin that admitted its evidence. A binary that links another
  plugin denies every check on such grants as `chain_invalid`. To change the plugin, issue the
  grants again.
- The default of `max_candidates` is an estimate, not a measurement: the check benchmark of the
  NFR does not exist yet.
- The size limits of the SDK (`license_enforcement_sdk::limits`) apply to `issue_grant`,
  `issue_pack`, and `bind_grant`. `MAX_EVIDENCE_LEN` and the token limit of the other operations
  apply when those operations are implemented.
