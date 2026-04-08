# E2E Testing Guide

This directory contains end-to-end tests for the Hyperspot server.

## Prerequisites

- Python **3.9+** is required.
- E2E tests must run on Python **3.9 and above**.

Install Python dependencies:

```bash
pip install -r testing/e2e/requirements.txt
```

## Running E2E Tests

The `scripts/ci.py` Python script supports two modes: **local** (default) and **Docker**.

### Option 1: Docker Mode (Default, Recommended for CI)

This approach builds a Docker image and runs tests in an isolated environment:

```bash
# Using make
make e2e-docker  # all tests
make e2e-docker-smoke  # smoke only tests (annotated with @pytest.mark.smoke)

# Or directly
python3 scripts/ci.py e2e-docker
```

### Option 2: Local Mode (Faster for Development, advanced usage)

This approach runs tests against a locally running hyperspot-server.
`scripts/ci.py e2e-local` will build and start the local server automatically.

```bash
# Run local E2E (builds release artifacts and starts server automatically)
make e2e-local  # all tests
make e2e-local-smoke  # smoke only tests (annotated with @pytest.mark.smoke)

# Or directly
python3 scripts/ci.py e2e-local
```

#### Advanced Usage

Environment Variables:

- **`E2E_BASE_URL`**: Base URL for the API (default: `http://localhost:8086`) - only used in local mode
- **`E2E_AUTH_TOKEN`**: Optional authentication token for protected endpoints

Why local E2E defaults to `8086`:

- Local E2E uses `config/e2e-local.yaml` and a dedicated E2E-oriented build/run path, which may differ from the usual development default (`8087` via `quickstart`).
- Keeping a stable, dedicated E2E port makes lifecycle management deterministic: `scripts/ci.py` can reliably start, health-check, and stop the service it launched.
- This also makes it safer to kill/restart only the E2E-owned process during test runs, without interfering with another manually started server.

#### Running Individual Tests Against a Running Service locally (w/o Docker)

```bash
# Run the server in one terminal:
make quickstart

# Run the tests in another terminal:
E2E_BASE_URL=http://localhost:8087 python3 -m pytest testing/e2e/modules/nodes_registry
# or for smoke tests only:
E2E_BASE_URL=http://localhost:8087 python3 -m pytest testing/e2e/modules/nodes_registry -m smoke
```

#### Using auth token

```bash
E2E_BASE_URL=http://localhost:8087 E2E_AUTH_TOKEN=your-token python3 -m pytest testing/e2e/
```

### Command Line Options

The `scripts/ci.py` Python script accepts the following options:

- `--docker`: Run tests in Docker environment (default is local mode)
- `--smoke`: Run only tests marked with `@pytest.mark.smoke`
- `--help`: Show help message

## Writing Tests

Tests are written using pytest and httpx. See `modules/file_parser/test_file_parser_info.py` for an example.

Key fixtures available:

- `base_url`: Returns the base URL from `E2E_BASE_URL` environment variable
- `auth_headers`: Returns authorization headers if `E2E_AUTH_TOKEN` is set
- `local_files_root`: Returns the root directory for local file parsing tests
- `file_http_server`: Starts a local HTTP server serving files from `e2e/testdata`

Example:

```python
import httpx
import pytest

@pytest.mark.asyncio
async def test_my_endpoint(base_url, auth_headers):
    async with httpx.AsyncClient(timeout=10.0) as client:
        response = await client.get(
            f"{base_url}/my-endpoint",
            headers=auth_headers,
        )
        assert response.status_code == 200
```

## Quick Reference

| Command                              | Mode   | Description                              |
|--------------------------------------|--------|------------------------------------------|
| `make e2e`                           | Docker | Default: Run tests in Docker              |
| `make e2e-docker`                    | Docker | Run tests in Docker environment          |
| `make e2e-docker-smoke`              | Docker | Run only smoke tests in Docker            |
| `make e2e-local`                     | Local  | Run tests against auto-started local server |
| `make e2e-local-smoke`               | Local  | Run only smoke tests locally              |
| `python3 scripts/ci.py e2e-local`    | Local  | Direct script execution (local)          |
| `python3 scripts/ci.py e2e-local --smoke` | Local | Direct script execution (smoke only) |
| `python3 scripts/ci.py e2e-docker`   | Docker | Direct script execution (Docker)         |

## Troubleshooting

### Server not responding (Local Mode)

If you see "Server not responding" when running local tests:

1. Check build/startup logs in `logs/hyperspot-e2e.log` and `logs/hyperspot-e2e-error.log`
2. Check that the API is reachable on the configured port (default: 8086)
3. Verify the health endpoint: `curl http://localhost:8086/healthz`
4. Rebuild release artifacts: `make build`
5. Or use Docker mode: `make e2e-docker`

### pytest not found

Install the required dependencies:

```bash
pip install -r testing/e2e/requirements.txt
```

### Docker build fails

Make sure Docker is running and you have sufficient disk space:

```bash
docker system df
docker system prune  # if needed
```
