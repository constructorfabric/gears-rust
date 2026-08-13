# cf-gears-settings-service-sdk

Public SDK for the `settings-service` gear.

## Setting keys

A setting's key is a GTS **instance** identifier:

```text
gts.cf.settings.types.bool_flag.v1~acme.settings.network.enable_proxy.v1
└────── value type, ends `~` ─────┘└──── instance id, no trailing `~` ───┘
```

- The **left** half is a curated value type from the `gts.cf.settings.types.*~`
  catalog. It is the only part registered in GTS, and it defines the value's shape.
- The **right** half is the setting's own instance id. The setting is an unregistered
  GTS instance; it lives in the Settings DB, not the Registry.

For an admin-authored setting the instance id is
`<vendor>.settings.<category>.<name>.v1`, where `<category>` is the slug of
the category the setting was created in. Because the category is embedded, renaming or
moving a category re-keys every setting beneath it.

Grammar validation is delegated to `gts-id`, the platform's single source of truth for
GTS identifiers. Contract source: `ADR-001-setting-key-gts-instance-id`.

## Status

Phase 1 of the gear-foundation feature: the `SettingKey` value object, the opaque
`SecretHandle`, and the `EffectiveSource` vocabulary. Reader and contribution traits,
the error taxonomy, and the reader degradation contract arrive in phase 2.
