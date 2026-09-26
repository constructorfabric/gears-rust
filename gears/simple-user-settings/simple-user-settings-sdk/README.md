# Simple User Settings SDK

SDK crate for the simple user settings gear.

## Overview

The `cf-gears-simple-user-settings-sdk` crate provides:

- `SimpleUserSettingsClientV1` trait — the fixed `theme` and `language`
- `NamedSettingsClientV1` trait — any number of keyed JSON values
- Model types (`SimpleUserSettings`, `SimpleUserSettingsPatch`, `SimpleUserSettingsUpdate`, `NamedSetting`)

Consumers obtain the clients from `ClientHub`.

```rust,ignore
use simple_user_settings_sdk::SimpleUserSettingsClientV1;

let client = hub.get::<dyn SimpleUserSettingsClientV1>()?;
let settings = client.get_settings(&ctx).await?;
```

## Named settings

For any preference beyond `theme` and `language`: a key the product chooses and a
JSON value, filed under the same user and tenant as the fixed fields.

```rust,ignore
use serde_json::json;
use simple_user_settings_sdk::NamedSettingsClientV1;

let named = hub.get::<dyn NamedSettingsClientV1>()?;
named.put_named_setting(&ctx, "portal.projects.view", json!("table")).await?;
let view = named.get_named_setting(&ctx, "portal.projects.view").await?; // Option<NamedSetting>
let all = named.list_named_settings(&ctx).await?;                        // ordered by key
named.delete_named_setting(&ctx, "portal.projects.view").await?;         // true if it existed
named.delete_all_named_settings(&ctx).await?;                             // how many were removed
```

Keys are 1–128 characters from `A–Z a–z 0–9 . _ - :`; namespace them yourself
(a dotted product prefix is the usual shape). Values and the number of keys per
user are bounded by the gear's configuration.

The store is one per user and tenant, shared by every product in the deployment
that writes to it. There is no per-product namespace, quota or policy: the key
count bound (`named_settings_per_user`, default 256) is one budget for all of
them, and every key is authorized as the same resource as `theme`/`language`.
It is meant for small UI preferences. A product that needs its own quota, or a
different access policy for its keys, needs its own store. Over REST the same operations are
`GET|PUT|DELETE /simple-user-settings/v1/named-settings/{key}` and
`GET|DELETE /simple-user-settings/v1/named-settings` (list all, delete all).

## License

Licensed under Apache-2.0.
