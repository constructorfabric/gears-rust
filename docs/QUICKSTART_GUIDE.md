# Gears Server - Quickstart Guide

Start Gears example server and verify it works. For project overview, see [README.md](../README.md).

---

## Start the Server

```bash
# With example gears (tenant-resolver, users-info)
make example

# Or minimal (no example gears)
make quickstart
```

Server runs on `http://127.0.0.1:8087/cf/`.
The example configuration also sets `gears.api-gateway.config.prefix_path: "/cf"` in `config/quickstart.yaml`, so API docs and endpoints are exposed under `/cf`.
Change `prefix_path` if you want a different base path, or set it to an empty string to serve the API at the root.

---

## Verify It's Running

```bash
curl -s http://127.0.0.1:8087/cf/health
# {"status": "healthy", "timestamp": "..."}
```

---

## API Documentation

### Interactive Documentation

Open <http://127.0.0.1:8087/cf/docs> in your browser for the full API reference with interactive testing.

### OpenAPI Spec

```bash
curl -s http://127.0.0.1:8087/cf/openapi.json > openapi.json
```

### Gear Examples

Each gear has a QUICKSTART.md with minimal curl examples:

- [File Parser](../gears/file-parser/QUICKSTART.md) - Parse documents into structured blocks
- [Nodes Registry](../gears/system/nodes-registry/QUICKSTART.md) - Hardware and system info
- [Tenant Resolver](../gears/system/tenant-resolver/QUICKSTART.md) - Multi-tenant hierarchy

> **Note:** Gear quickstarts show basic usage only. Use `/cf/docs` for complete API documentation in the example setup. This path is configurable via `api_gateway.prefix_path`.

---

## Stop the Server

```bash
pkill -f cf-gears-server
```

---

## Troubleshooting

| Issue | Solution |
|-------|----------|
| Port 8087 in use | `pkill -f cf-gears-server` |
| Empty tenant-resolver | Use `make example` instead of `make quickstart` |
| Connection refused | Server not running - check logs |

---

## Further Reading

- [/cf/docs](http://127.0.0.1:8087/cf/docs) - Full API reference
- [ARCHITECTURE_MANIFEST.md](ARCHITECTURE_MANIFEST.md) - Architecture principles
- [TOOLKIT_UNIFIED_SYSTEM/README.md](./toolkit_unified_system/README.md) - Gear system
