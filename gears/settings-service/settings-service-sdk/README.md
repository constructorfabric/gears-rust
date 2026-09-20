# cf-gears-settings-service-sdk

Public SDK for the `settings-service` gear: the traits a consuming gear calls,
the value objects its keys are made of, and the curated value-type catalogue a
declaration picks from. The implementation lives in
[`cf-gears-settings-service`](../settings-service); nothing here depends on it.

## Setting keys

A setting's key is a GTS **type** identifier, for both authoring parties:

```text
gts.cf.core.settings.setting_type.v1~acme.billing.network.enable_proxy.v1~
└──── gear-owned abstract base ────┘└──── the setting, four tokens ─────┘
```

- The **base** is owned by the Settings gear, defined here and registered at
  gear init.
- The **derived half** is the setting: `<vendor>.<package>.<category>.<name>`
  before its version, the category always the third token. A module supplies
  its own half; an admin-authored setting is composed as
  `<vendor>.settings.<category>.<name>.vN`.
- The trailing `~` makes the key a **type**, not an instance — which is what
  lets an authorization policy name one setting, or a wildcarded subtree of
  settings, as its resource.

The value's *shape* is a separate fact of the declaration, `value_type_id`,
naming one of the catalogue types below. Grammar validation is delegated to
`gts-id`, the platform's single source of truth for GTS identifiers. Contract
source: [ADR-002](../docs/ADR/ADR-002-setting-key-gts-type-id.md), which
supersedes the retired instance-id decision.

`SettingKey` parses and composes those keys (`parse`, `compose`,
`contributed`) and reads their parts without allocating (`base_type`,
`derived_half`, `category_slug`, `leaf_slug`, `major`,
`version_stripped_path`).

## Traits

`SettingsReaderClient` — what a consuming gear resolves from `ClientHub`:

- `get_effective` / `get_effective_bulk` — the effective value at a scope, with
  its source and inheritance trail. A failure is reported as a failure: the
  reader never substitutes the Schema Default.
- `resolve_secret` — the sole path to a secret's plaintext, over the opaque
  `SecretHandle` a read hands out, authorized per setting and audited.

`SettingsContributionClient` — what a gear contributing its own settings calls
in its init: `register_declarations` (an idempotent reconcile, run on every
start) and `retire_declarations`, both per-item so one bad entry does not sink
the set.

Both return `Result<_, CanonicalError>` on the wire; `SettingsError` is the
typed projection a consumer matches on — `Unavailable`, `Retired`, `NotFound`,
`SecretNotConfigured`, `Unauthorized`, `Other` — per
[ADR 0005](../../../docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md).

## Value-type catalogue

`catalogue::CATALOGUE` ships one registered type per shape a setting may take —
`bool_flag`, `string`, `text`, `secret_string`, `integer`, `number`, `port`,
`duration_seconds`, `url`, `hostname`, `email`, `cron`, `regex`, `json` — each
under `gts.cf.core.settings.type_<name>.v1~`, with its JSON Schema and trait
set. A declaration names one of these; the service validates every value
against it, and the traits drive both rendering and the checks the shape alone
cannot express.
