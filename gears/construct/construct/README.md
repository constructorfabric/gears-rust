# Construct Gear

Construct keeps a living profile of a subject and serves it to applications.
This crate is the gear's foundation: the shell every Construct feature is built
on. It holds no Construct-specific types yet.

## Overview

The `cf-gears-construct` crate implements the gear runtime and storage.
The public API surface is defined in `cf-gears-construct-sdk` and re-exported here.

## Configuration

```yaml
gears:
  construct:
    config:
      max_text_length: 1000   # longest foundation note text, in bytes
```

## License

Licensed under Apache-2.0.
