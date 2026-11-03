# File Parser Gear

File parsing gear for Gears / ToolKit.

## Overview

The `cf-gears-file-parser` crate implements the `file-parser` gear and registers REST routes.

All document extraction is handled by a single unified backend — [`kreuzberg =4.9.4`](https://github.com/kreuzberg-dev/kreuzberg) — which replaces the previous per-format library set (`tl`, `pdf-extract`, `calamine`, `pptx-to-md`).

Supported formats:

| Extension(s)              | Format                          |
|---------------------------|---------------------------------|
| `pdf`                     | PDF                             |
| `html`, `htm`             | HTML                            |
| `xlsx`, `xls`, `xlsm`, `xlsb` | Excel spreadsheets         |
| `pptx`                    | PowerPoint presentations        |

## Configuration

```yaml
gears:
  file-parser:
    config:
      max_file_size_mb: 100
      # Required. Only files under this directory are accessible via parse-local.
      # Symlinks that resolve outside this directory are also blocked.
      allowed_local_base_dir: /data/documents
```

### Security: Local Path Restrictions

The `parse-local` endpoints validate requested file paths before any filesystem access:

1. Paths containing `..` components are always rejected.
2. The requested path is canonicalized (symlinks resolved) and must fall under `allowed_local_base_dir`.
3. `allowed_local_base_dir` is **required** — the gear will fail to start if it is missing or the path cannot be resolved.

## License

This gear is licensed under **Apache-2.0**.

### Third-party dependency: kreuzberg

This gear depends on [`kreuzberg`](https://github.com/kreuzberg-dev/kreuzberg), pinned at **`=4.10.4`** (MIT).

| Version range | License |
|---------------|---------|
| `≤ 4.7.4` | MIT |
| `4.8.0` – `4.9.x` | [Elastic License 2.0 (EL-2.0)](https://www.elastic.co/licensing/elastic-license) |
| `4.10.x` (including `=4.10.4` used here) | MIT |

The exact `=` pin stays because upstream has changed license before, so any version bump
must re-check the license. `deny.toml` has no exception for kreuzberg, so a license that is
not on the allow-list would fail `cargo deny check`.
