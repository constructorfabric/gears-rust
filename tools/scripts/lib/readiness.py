"""When a started server may receive E2E traffic.

The listener and `/healthz` come up with the infrastructure gears, before the
application group is prepared; its routes reply `503` until then. A launcher
therefore waits for the application group to have started, read from
`/readyz` (in-process gateway and `OoP` unit alike):

- `200`: ready.
- `503` with `state: starting` and no `application/preparation` among
  `unresolved_deps`: the application group started; a consumed dependency or
  a healthcheck is not ready yet. A suite that needs it waits for it.
- anything else waits: `starting` with the preparation gate, `draining`, a
  `503` without a complete readiness body (`state` and `ready`; ADR-0005
  omits an empty `unresolved_deps`), `404` (a wrong path, never proof of
  startup), no answer.

Both probes live under the gateway's `prefix_path`.
"""

from __future__ import annotations

import http.client
import json
import re
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import urlopen

# The runtime's gate unresolved until the application group of the process
# started (`toolkit::runtime::PREPARATION_GATE`).
PREPARATION_GATE = "application/preparation"


def application_started(status: int, body: bytes) -> bool:
    """Whether one `/readyz` response shows the application group started."""
    if status == 200:
        return True
    if status != 503:
        return False
    try:
        report = json.loads(body or b"null")
    except ValueError:
        return False
    # A complete `ReadinessReport` (ADR-0005): `state` and `ready` always,
    # `unresolved_deps` omitted when empty.
    if not isinstance(report, dict) or not isinstance(report.get("ready"), bool):
        return False
    if report.get("state") != "starting" or report["ready"]:
        return False
    unresolved = report.get("unresolved_deps", [])
    return isinstance(unresolved, list) and PREPARATION_GATE not in unresolved


def probe(url: str, timeout: float = 2) -> tuple[int, bytes] | None:
    """`(status, body)` of a GET, or `None` when no complete answer came in time
    (no connection, a timeout, a body cut short, an error status included)."""
    try:
        resp = urlopen(url, timeout=timeout)
        status = resp.status
    except HTTPError as e:
        resp, status = e, e.code
    except (URLError, OSError, http.client.HTTPException):
        return None
    with resp:
        try:
            return status, resp.read()
        except (OSError, http.client.HTTPException):
            return None


def rest_prefix(config_path: str | Path) -> str:
    """The gateway's `prefix_path` in a server config file (`""` when unset).

    A missing file has none: the server then fails to start on its own.
    """
    import yaml

    try:
        with open(config_path, encoding="utf-8") as f:
            config = yaml.safe_load(f) or {}
    except FileNotFoundError:
        return ""
    gateway = ((config.get("gears") or {}).get("api-gateway") or {}).get("config") or {}
    return normalize_prefix(gateway.get("prefix_path") or "")


def normalize_prefix(raw: str) -> str:
    """`prefix_path` as the gateway serves it (`ApiGateway::normalize_prefix_path`):
    trimmed, repeated slashes collapsed, no trailing slash, a leading one added.
    A prefix the gateway rejects (unsafe characters) fails the server instead."""
    prefix = re.sub("/{2,}", "/", raw.strip()).rstrip("/")
    if not prefix:
        return ""
    return prefix if prefix.startswith("/") else f"/{prefix}"
