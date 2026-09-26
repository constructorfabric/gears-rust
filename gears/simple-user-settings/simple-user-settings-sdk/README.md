# Simple User Settings SDK

SDK crate for the simple user settings gear.

## Overview

The `cf-gears-simple-user-settings-sdk` crate provides:

- `SimpleUserSettingsClientV1` trait
- Model types (`SimpleUserSettings`, `SimpleUserSettingsPatch`, `SimpleUserSettingsUpdate`)
- Error type (`SettingsError`)

Consumers obtain the client from `ClientHub`.

```rust,ignore
use simple_user_settings_sdk::SimpleUserSettingsClientV1;

let client = hub.get::<dyn SimpleUserSettingsClientV1>()?;
let settings = client.get_settings(&ctx).await?;
```

## Whose settings: `SettingsOwnerResolver`

Settings are filed under `(user, tenant)`, and by default the user is the token
subject (`ctx.subject_id()`). A deployment where one person can sign in through
more than one identity — an e-mail login and a brokered GitHub login, merged
accounts, an IdP migration — can register a `SettingsOwnerResolver` on the
`ClientHub` so that all of a person's logins share one set of settings:

```rust,ignore
use simple_user_settings_sdk::SettingsOwnerResolver;

let resolver: Arc<dyn SettingsOwnerResolver> = Arc::new(MyResolver);
hub.register(resolver); // in your gear's `init`
```

- With none registered, nothing changes: the subject is the key.
- The resolver returns `Ok(None)` for a caller it has no opinion about, and the
  subject is used; an `Err` fails the request instead of guessing.
- It answers the user half only, so logins are unified within a tenant, not
  across tenants.
- Its answer decides whose settings a request reads and writes and is what
  `SimpleUserSettings::user_id` reports. The trait docs spell out the contract
  (same person, same tenant, stable answers, cheap, cancel-safe) and how to
  re-key rows written before it was enabled.

## License

Licensed under Apache-2.0.
