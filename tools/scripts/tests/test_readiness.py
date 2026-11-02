"""The launchers' startup criterion (`lib/readiness.py`) and its waits."""

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import ci  # noqa: E402
from lib.readiness import application_started, normalize_prefix, probe, rest_prefix  # noqa: E402

PREPARATION = "application/preparation"


def _report(state, unresolved=None):
    """A `ReadinessReport` body; `unresolved=None` leaves the field out."""
    body = {"state": state, "ready": state in ("ready", "degraded")}
    if unresolved is not None:
        body["unresolved_deps"] = unresolved
    return json.dumps(body).encode()


class TestResponseTable(unittest.TestCase):
    """Both `/readyz` shapes: in-process gateway and `OoP` unit share the body."""

    def test_classification(self):
        problem = json.dumps({"status": 503, "title": "Service Unavailable"}).encode()
        cases = [
            ("ready", 200, _report("ready"), True),
            ("ready, any body", 200, b"", True),
            ("preparing", 503, _report("starting", [PREPARATION]), False),
            (
                "preparing, gear gate too",
                503,
                _report("starting", ["billing", PREPARATION]),
                False,
            ),
            ("started, a consumed dependency unresolved", 503, _report("starting", ["billing"]), True),
            ("started, a check unhealthy", 503, _report("starting", []), True),
            # ADR-0005 omits an empty `unresolved_deps`.
            ("started, a check unhealthy, field omitted", 503, _report("starting"), True),
            ("incomplete body: no ready", 503, json.dumps({"state": "starting", "unresolved_deps": []}).encode(), False),
            ("incomplete body: no state", 503, json.dumps({"ready": False, "unresolved_deps": []}).encode(), False),
            ("incomplete body: unresolved_deps not a list", 503, json.dumps({"state": "starting", "ready": False, "unresolved_deps": "x"}).encode(), False),
            ("draining", 503, _report("draining", ["billing"]), False),
            ("draining, nothing unresolved", 503, _report("draining"), False),
            ("status-only 503", 503, b"", False),
            ("problem document 503", 503, problem, False),
            ("not JSON", 503, b"<html>", False),
            ("wrong path", 404, b"", False),
            ("server error", 500, _report("starting"), False),
        ]
        for name, status, body, expected in cases:
            with self.subTest(name):
                self.assertIs(application_started(status, body), expected)


class TestRestPrefix(unittest.TestCase):
    def test_reads_the_gateway_prefix(self):
        with tempfile.TemporaryDirectory() as folder:
            config = Path(folder) / "c.yaml"
            config.write_text('gears:\n  api-gateway:\n    config:\n      prefix_path: "/cf/"\n')
            self.assertEqual(rest_prefix(config), "/cf")
            config.write_text("gears:\n  api-gateway:\n    config:\n      bind_addr: x\n")
            self.assertEqual(rest_prefix(config), "")
            self.assertEqual(rest_prefix(Path(folder) / "missing.yaml"), "")
            config.write_text('gears:\n  api-gateway:\n    config:\n      prefix_path: "cf"\n')
            self.assertEqual(rest_prefix(config), "/cf")

    def test_normalizes_like_the_gateway(self):
        # `ApiGateway::normalize_prefix_path` accepts each of these.
        cases = {
            "": "",
            "/": "",
            "  ": "",
            "///": "",
            "cf": "/cf",
            "/cf": "/cf",
            "/cf/": "/cf",
            " /cf// ": "/cf",
            "//cf//api/": "/cf/api",
            "cf/api": "/cf/api",
        }
        for raw, expected in cases.items():
            with self.subTest(raw=raw):
                self.assertEqual(normalize_prefix(raw), expected)


class _Scripted(BaseHTTPRequestHandler):
    """Serves `routes[path]`: a list of `(status, body, delay)` consumed in order,
    the last one repeated; any other path is `404`."""

    routes: dict = {}

    def do_GET(self):  # noqa: N802
        script = self.routes.get(self.path)
        if not script:
            status, body, delay = 404, b"", 0
        else:
            status, body, delay = script[0] if len(script) == 1 else script.pop(0)
        time.sleep(delay)
        try:
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *args):
        pass


@contextlib.contextmanager
def _server(routes):
    handler = type("Handler", (_Scripted,), {"routes": routes})
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}"
    finally:
        server.shutdown()
        server.server_close()


class _RawStatus(BaseHTTPRequestHandler):
    """A `503` whose headers arrive at once: `declared` body bytes announced,
    `sent` of them written after `delay`, then the connection closes."""

    declared, sent, delay = 2, 2, 0.0

    def do_GET(self):  # noqa: N802
        try:
            self.send_response(503)
            self.send_header("Content-Length", str(self.declared))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.flush()
            time.sleep(self.delay)
            self.wfile.write(b"x" * self.sent)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *args):
        pass


@contextlib.contextmanager
def _raw_server(**attrs):
    handler = type("Handler", (_RawStatus,), attrs)
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}"
    finally:
        server.shutdown()
        server.server_close()


class TestProbe(unittest.TestCase):
    """An error status whose body does not arrive whole is no answer: retried."""

    def test_a_slow_error_body_is_no_answer(self):
        with _raw_server(delay=0.25) as url:
            self.assertIsNone(probe(f"{url}/readyz", timeout=0.05))

    def test_a_truncated_error_body_is_no_answer(self):
        with _raw_server(declared=10, sent=2) as url:
            self.assertIsNone(probe(f"{url}/readyz", timeout=2))

    def test_a_whole_error_body_is_returned(self):
        with _raw_server() as url:
            self.assertEqual(probe(f"{url}/readyz", timeout=2), (503, b"xx"))


def _wait(base_url, **kwargs):
    """`ci.wait_for_health`; `True` when it returned, `False` when it gave up."""
    with contextlib.redirect_stdout(io.StringIO()):
        try:
            ci.wait_for_health(base_url, poll_interval=0.05, **kwargs)
        except SystemExit:
            return False
    return True


class TestCiWaitForHealth(unittest.TestCase):
    def test_waits_for_the_preparation_gate_under_the_prefix(self):
        routes = {
            "/cf/healthz": [(200, b"ok", 0)],
            "/cf/readyz": [
                (503, _report("starting", [PREPARATION]), 0),
                (503, _report("starting", [PREPARATION]), 0),
                (503, _report("starting", ["billing"]), 0),
            ],
        }
        with _server(routes) as url:
            self.assertTrue(_wait(url, timeout_secs=5, path_prefix="/cf"))
        self.assertEqual(len(routes["/cf/readyz"]), 1, "returned on the started report")

    def test_a_root_404_under_a_prefix_is_not_startup(self):
        routes = {
            "/healthz": [(200, b"ok", 0)],
            "/cf/healthz": [(200, b"ok", 0)],
            "/cf/readyz": [(503, _report("starting", [PREPARATION]), 0)],
        }
        with _server(routes) as url:
            self.assertFalse(_wait(url, timeout_secs=0.5))
            self.assertFalse(_wait(url, timeout_secs=0.5, path_prefix="/cf"))

    def test_draining_and_status_only_responses_wait(self):
        for body in (_report("draining", ["billing"]), b""):
            with self.subTest(body=body), _server(
                {"/healthz": [(200, b"ok", 0)], "/readyz": [(503, body, 0)]}
            ) as url:
                self.assertFalse(_wait(url, timeout_secs=0.5))

    def test_a_slow_readiness_probe_is_retried(self):
        routes = {
            "/healthz": [(200, b"ok", 0)],
            # Longer than the probe's 2s timeout once, then answers.
            "/readyz": [(200, b"", 2.5), (200, b"", 0)],
        }
        with _server(routes) as url:
            self.assertTrue(_wait(url, timeout_secs=8))


@unittest.skipUnless(
    importlib.util.find_spec("httpx") and importlib.util.find_spec("pytest"),
    "the E2E orchestrator needs httpx and pytest",
)
class TestOrchestratorWaitHealthy(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        root = Path(__file__).resolve().parents[3]
        spec = importlib.util.spec_from_file_location(
            "e2e_orchestrator", root / "testing" / "e2e" / "lib" / "orchestrator.py"
        )
        cls.orchestrator = importlib.util.module_from_spec(spec)
        # dataclasses resolve annotations through `sys.modules`.
        sys.modules[spec.name] = cls.orchestrator
        spec.loader.exec_module(cls.orchestrator)

    def _wait(self, url, **env):
        port = int(url.rsplit(":", 1)[1])
        gear_env = self.orchestrator.GearTestEnv(port=port, **env)
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                self.orchestrator._wait_healthy(gear_env, poll_interval=0.05)
            except BaseException as e:  # pytest.fail raises a BaseException
                if type(e).__name__ != "Failed":
                    raise
                return False
        return True

    def test_waits_for_the_explicit_prefixed_readiness_path(self):
        routes = {
            "/cf/openapi.json": [(200, b"{}", 0)],
            "/cf/readyz": [
                (503, _report("starting", [PREPARATION]), 0),
                (200, _report("ready"), 0),
            ],
        }
        with _server(routes) as url:
            # The root `/readyz` is 404 under a prefix: never startup.
            self.assertFalse(self._wait(url, health_path="/cf/openapi.json", health_timeout=1))
            self.assertTrue(
                self._wait(
                    url,
                    health_path="/cf/openapi.json",
                    readiness_path="/cf/readyz",
                    health_timeout=5,
                )
            )

    def test_draining_waits(self):
        routes = {
            "/healthz": [(200, b"ok", 0)],
            "/readyz": [(503, _report("draining", ["billing"]), 0)],
        }
        with _server(routes) as url:
            self.assertFalse(self._wait(url, health_timeout=1))

    def test_a_readiness_timeout_is_retried_until_the_deadline(self):
        routes = {
            "/healthz": [(200, b"ok", 0)],
            # Longer than the probe's 3s timeout once, then answers.
            "/readyz": [(200, b"", 3.5), (200, b"", 0)],
        }
        with _server(routes) as url:
            self.assertTrue(self._wait(url, health_timeout=10))


if __name__ == "__main__":
    unittest.main()
