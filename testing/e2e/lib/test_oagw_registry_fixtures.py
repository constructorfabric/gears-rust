"""Regression coverage for the configurations produced by focused E2E profiles."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

import httpx

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools" / "scripts"))

import run_e2e  # noqa: E402
from testing.e2e.suites.oagw import helpers as oagw  # noqa: E402


class TestOagwRegistryDatabase(unittest.TestCase):
    def test_default_and_sqlite_profiles_keep_the_embedded_registry_database(self):
        for profile in (None, "sqlite"):
            with self.subTest(profile=profile), tempfile.TemporaryDirectory() as folder:
                global_config = run_e2e.load_yaml(run_e2e.DEFAULT_GLOBAL_CONFIG)
                global_config["generated_config_dir"] = folder
                suite = run_e2e.merged_suite(
                    "oagw",
                    global_config,
                    run_e2e.suite_manifest_path("oagw", None),
                    profile,
                )
                generated = run_e2e.write_focused_config(
                    "oagw", profile, suite, global_config
                )
                config = run_e2e.load_yaml(generated)

                # Migrated resolvers publish before application init. An embedded
                # registry only provides their platform contract with a database,
                # even while OAGW continues reading its legacy catalogue.
                database = config["gears"]["types-registry"].get("database")
                self.assertIsInstance(database, dict)
                self.assertTrue(database.get("file"))
                server = config["database"]["servers"][database["server"]]
                self.assertEqual(server["engine"], "sqlite")

                if profile == "sqlite":
                    self.assertEqual(
                        config["gears"]["oagw"]["database"]["server"],
                        database["server"],
                    )
                    self.assertEqual(
                        config["gears"]["oagw"]["config"]["registry_provisioning"]["recheck_interval"],
                        "1s",
                        "post-startup fixture publication must reconcile within the setup budget",
                    )
                entities = config["gears"]["types-registry"]["config"].get("entities", [])
                self.assertFalse(
                    any("gts.cf.core.oagw." in entity["$id"] for entity in entities),
                    "legacy UUID-tail fixtures must not enter managed startup admission",
                )


class TestOagwLegacyFixtureSetup(unittest.TestCase):
    def test_setup_uses_the_legacy_contract_with_the_suites_caller(self):
        requests = []

        def respond(request):
            requests.append(request)
            return httpx.Response(200, json={"summary": {"succeeded": 3, "failed": 0}})

        with httpx.Client(transport=httpx.MockTransport(respond)) as client:
            oagw.seed_legacy_registry_fixtures(
                client, "http://oagw.test", {"Authorization": "Bearer suite-token"}
            )

        self.assertEqual(len(requests), 1)
        request = requests[0]
        self.assertEqual(request.method, "POST")
        self.assertEqual(str(request.url), "http://oagw.test/types-registry/v1/entities")
        self.assertEqual(request.headers["Authorization"], "Bearer suite-token")
        entities = json.loads(request.content)["entities"]
        self.assertEqual(
            {entity["$id"] for entity in entities},
            {oagw.REGISTRY_UPSTREAM_ID, oagw.REGISTRY_ROUTE_ID, oagw.BARE_UUID_ROUTE_ID},
        )
        bare = next(entity for entity in entities if entity["$id"] == oagw.BARE_UUID_ROUTE_ID)
        self.assertEqual(bare["upstream_id"], oagw.REGISTRY_UPSTREAM_ID.rsplit("~", 1)[-1])

    def test_http_refusals_and_partial_registration_fail_setup_without_fallback(self):
        for status, body in [
            (403, {}),
            (200, {"summary": {"succeeded": 2, "failed": 1}}),
            (200, {}),
        ]:
            with self.subTest(status=status, body=body):
                requests = []

                def respond(request):
                    requests.append(request)
                    return httpx.Response(status, json=body)

                with httpx.Client(transport=httpx.MockTransport(respond)) as client:
                    with self.assertRaises((httpx.HTTPStatusError, RuntimeError)):
                        oagw.seed_legacy_registry_fixtures(client, "http://oagw.test", {})
                self.assertEqual(len(requests), 1)


if __name__ == "__main__":
    unittest.main()
