# Construct Gear

Construct keeps a living profile of a subject and serves it to applications.
This crate holds the gear: record intake for connectors
(`POST /construct/v1/records`), the subject settings it reads, and the
tenant-scoped storage behind them.

## Overview

The `cf-gears-construct` crate implements the gear runtime and storage.
The public API surface is defined in `cf-gears-construct-sdk` and re-exported here.

## Configuration

```yaml
gears:
  construct:
    config:
      # Personalization for a new subject while the settings service gives no
      # tenant default (no settings service in the host, or the setting not
      # declared there). Default: true.
      personalization_default: true
      # Connectors that are off, by the subject id (a UUID) of the connector's
      # login. Every other connector is on. A value that is not a UUID stops
      # the gear from starting. Default: none.
      connectors_off: []
```

## License

Licensed under Apache-2.0.
