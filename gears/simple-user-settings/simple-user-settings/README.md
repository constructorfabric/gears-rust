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
      max_field_length: 100          # theme / language
      named_settings_per_user: 256   # named settings one user may hold per tenant
      named_value_max_bytes: 4096    # one named setting's value, as JSON bytes
```

## License

Licensed under Apache-2.0.
