"""Regression tests for the local E2E launcher's server/test credentials."""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import ci  # noqa: E402


class TestLocalE2EInternalToken(unittest.TestCase):
    def test_server_and_pytest_receive_the_same_internal_token(self):
        override = "APP__GEARS__API_GATEWAY__CONFIG__INTERNAL_AUTH__SECRET"
        for token in (None, "custom-e2e-token"):
            with self.subTest(token=token), tempfile.TemporaryDirectory() as folder:
                binary = Path(folder) / "server"
                binary.touch()
                env = {"E2E_SERVER_BINARY": str(binary)}
                if token is not None:
                    env["E2E_INTERNAL_TOKEN"] = token
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
                if token is None:
                    # Preserve the literal YAML default when no override is requested.
                    self.assertNotIn(override, server_env)
                    self.assertNotIn("E2E_INTERNAL_TOKEN", pytest_env)
                else:
                    self.assertEqual(server_env[override], token)
                    self.assertEqual(pytest_env["E2E_INTERNAL_TOKEN"], token)


if __name__ == "__main__":
    unittest.main()
