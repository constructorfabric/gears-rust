# Simple User Settings Gear

Simple settings gear.

## Overview

The `cf-gears-simple-user-settings` crate implements the gear runtime and storage.
The public API surface is defined in `cf-gears-simple-user-settings-sdk` and is re-exported here.

## Configuration

```yaml
gears:
  simple-user-settings:
    config:
      max_field_length: 100
      # Bound on one call to the deployment's SettingsOwnerResolver, if one is
      # registered; a call that runs over fails the request with 503.
      owner_resolver_timeout_ms: 2000
```

A deployment can decide which user key settings are filed under by registering
a `SettingsOwnerResolver` on the `ClientHub`; see the SDK README.

## License

Licensed under Apache-2.0.
