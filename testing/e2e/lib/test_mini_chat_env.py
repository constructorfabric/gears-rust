"""The mini-chat rig must wait for application readiness before provisioning."""

import contextlib
import io
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

E2E_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(E2E_ROOT))

from lib import orchestrator  # noqa: E402
from suites.mini_chat import conftest as mini_chat  # noqa: E402


def mini_chat_environment():
    request = SimpleNamespace(
        config=SimpleNamespace(getoption=lambda name: "offline")
    )
    # The fixture's argument contract is real; opening a provider socket is
    # unrelated to checking which endpoint releases application setup.
    with patch.object(mini_chat, "MockProviderServer"):
        return mini_chat.gear_test_env.__wrapped__(request)


class TestMiniChatEnvironment(unittest.TestCase):
    def test_fixture_uses_the_supported_health_check_for_application_readiness(self):
        env = mini_chat_environment()

        self.assertIsInstance(env, orchestrator.GearTestEnv)
        self.assertEqual(env.health_path, "/cf/readyz")

    def test_openapi_availability_does_not_bypass_pending_application_readiness(self):
        env = mini_chat_environment()
        elapsed = [0.0]
        readiness = iter([503, 503, 200])

        def get(url, **kwargs):
            # A successful documentation response does not establish that the
            # application is ready. Only /readyz releases the setup gate.
            status = next(readiness) if url.endswith("/cf/readyz") else 200
            return SimpleNamespace(status_code=status)

        def sleep(seconds):
            elapsed[0] += seconds

        with (
            patch("httpx.get", side_effect=get) as requests,
            patch.object(orchestrator.time, "monotonic", side_effect=lambda: elapsed[0]),
            patch.object(orchestrator.time, "sleep", side_effect=sleep),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            orchestrator._wait_healthy(env)

        self.assertEqual(requests.call_count, 3)
        self.assertEqual(
            {call.args[0] for call in requests.call_args_list},
            {"http://localhost:8087/cf/readyz"},
        )


if __name__ == "__main__":
    unittest.main()
