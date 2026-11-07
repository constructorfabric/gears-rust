"""Regression tests for E2E startup readiness and server/test credentials."""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from urllib.error import HTTPError

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import ci  # noqa: E402


class TestE2EReadiness(unittest.TestCase):
    def test_liveness_does_not_release_tests_before_readiness(self):
        ready = False

        def probe(url, timeout):
            if url.endswith("/readyz") and not ready:
                raise HTTPError(url, 503, "Starting", {}, io.BytesIO())
            return contextlib.nullcontext(SimpleNamespace(status=200))

        def finish_preparation(_seconds):
            nonlocal ready
            ready = True

        with (
            patch.object(ci, "urlopen", side_effect=probe),
            patch.object(ci.time, "sleep", side_effect=finish_preparation),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            ci.wait_for_health("http://server")

        self.assertTrue(ready, "E2E must wait for preparation even when liveness is 200")

    def test_unready_server_times_out_instead_of_starting_tests(self):
        def probe(url, timeout):
            if url.endswith("/readyz"):
                raise HTTPError(url, 503, "Starting", {}, io.BytesIO())
            return contextlib.nullcontext(SimpleNamespace(status=200))

        with (
            patch.object(ci, "urlopen", side_effect=probe),
            patch.object(ci.time, "monotonic", side_effect=[0, 16]),
            contextlib.redirect_stdout(io.StringIO()),
            self.assertRaises(SystemExit) as exited,
        ):
            ci.wait_for_health("http://server")

        self.assertEqual(exited.exception.code, 1)


class TestLocalE2EEnvironment(unittest.TestCase):
    def test_server_receives_endpoint_and_shared_test_credentials(self):
        override = "APP__GEARS__API_GATEWAY__CONFIG__INTERNAL_AUTH__SECRET"
        endpoint_override = "APP__GEARS__API_GATEWAY__CONFIG__ADVERTISE_URI"
        for token, base_url, advertised in (
            (None, None, None),
            ("custom-e2e-token", "http://localhost:18086", None),
            (None, None, "http://gateway:8086"),
        ):
            with (
                self.subTest(token=token, advertised=advertised),
                tempfile.TemporaryDirectory() as folder,
            ):
                binary = Path(folder) / "server"
                binary.touch()
                env = {"E2E_SERVER_BINARY": str(binary)}
                if token is not None:
                    env["E2E_INTERNAL_TOKEN"] = token
                if base_url is not None:
                    env["E2E_BASE_URL"] = base_url
                if advertised is not None:
                    env[endpoint_override] = advertised
                args = SimpleNamespace(
                    config="config/e2e-local.yaml", smoke=False, pytest_args=[]
                )
                with (
                    patch.dict(os.environ, env, clear=True),
                    patch.object(ci, "PROJECT_ROOT", folder),
                    patch.object(ci, "check_pytest"),
                    patch.object(ci, "kill_existing_server"),
                    patch.object(ci, "wait_for_health"),
                    patch.object(ci, "e2e_env_overrides", return_value={}),
                    patch.object(ci, "popen_new_group") as start_server,
                    patch.object(ci, "stop_process_tree"),
                    patch.object(ci, "run_cmd_allow_fail") as run_pytest,
                    contextlib.redirect_stdout(io.StringIO()),
                    self.assertRaises(SystemExit) as exited,
                ):
                    run_pytest.return_value.returncode = 0
                    ci.cmd_e2e_local(args)

                self.assertEqual(exited.exception.code, 0)
                server_env = start_server.call_args.kwargs["env"]
                pytest_env = run_pytest.call_args.kwargs["env"]
                self.assertEqual(
                    server_env[endpoint_override],
                    advertised or base_url or "http://localhost:8086",
                )
                if token is None:
                    # Preserve the literal YAML default when no override is requested.
                    self.assertNotIn(override, server_env)
                    self.assertNotIn("E2E_INTERNAL_TOKEN", pytest_env)
                else:
                    self.assertEqual(server_env[override], token)
                    self.assertEqual(pytest_env["E2E_INTERNAL_TOKEN"], token)


if __name__ == "__main__":
    unittest.main()
