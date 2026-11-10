# License Enforcement default evaluator

The evaluator that the `license-enforcement` gear uses unless the binary links another one. See
[DESIGN §3.1, Default evaluator](../../docs/DESIGN.md#31-domain-model).

- A binding matches the request subject (same id, on exactly its type or one of its base types),
  or the tenant of the request on the tenant Subject type or a type that derives from it. Minor
  versions do not count.
- A candidate passes when it and its root are active and the check instant is in its window,
  or in its grace period.
- Conditions and recipient rules are not supported: `validate` rejects any non-empty rules.

Link it into the binary with `use le_default_evaluator as _;`.
