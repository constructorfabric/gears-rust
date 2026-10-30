#!/usr/bin/env python3
"""Unit tests for resolve_release.py (publish release resolver).

    python3 -m unittest discover -s tools/scripts/tests
"""

from __future__ import annotations

import json
import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import resolve_release  # noqa: E402


class ResolveReleaseTest(unittest.TestCase):
    def setUp(self) -> None:
        self.components = resolve_release.load_components()
        # resolve() reads only these from the environment; isolate each test.
        for key in ("GITHUB_REF_NAME", "INPUT_COMPONENT", "INPUT_VERSION"):
            os.environ.pop(key, None)

    def resolve(self, **env):
        os.environ.update({k: v for k, v in env.items() if v is not None})
        return resolve_release.resolve(self.components)

    # --- tag resolution --------------------------------------------------------

    def test_gear_tag_builds_image_and_chart(self):
        component, version = self.resolve(GITHUB_REF_NAME="hello-v0.2.0")
        self.assertEqual(component["name"], "hello")
        self.assertEqual(version, "0.2.0")
        self.assertIn("image", component)
        self.assertIn("chart", component)

    def test_crate_prefix_differs_from_artifact_name(self):
        component, version = self.resolve(GITHUB_REF_NAME="cf-gears-flight-control-v1.3.0")
        self.assertEqual(component["name"], "flight-control")
        self.assertEqual(version, "1.3.0")

    def test_crateless_component_is_dispatch_only(self):
        # toolkit-platform has no crate, so no tag can match it...
        self.assertEqual(self.resolve(GITHUB_REF_NAME="toolkit-platform-v0.4.0"), (None, ""))
        # ...but an explicit dispatch resolves it (chart only, no image).
        component, version = self.resolve(INPUT_COMPONENT="toolkit-platform", INPUT_VERSION="0.4.0")
        self.assertEqual((component["name"], version), ("toolkit-platform", "0.4.0"))
        self.assertNotIn("image", component)

    def test_unrelated_tag_is_a_no_op(self):
        component, version = self.resolve(GITHUB_REF_NAME="cf-gears-llm-gateway-sdk-v0.1.11")
        self.assertIsNone(component)
        self.assertEqual(version, "")

    def test_prerelease_suffix_is_preserved(self):
        _, version = self.resolve(GITHUB_REF_NAME="hello-v0.2.0-rc.1")
        self.assertEqual(version, "0.2.0-rc.1")

    def test_api_contracts_examples_resolve_as_gears(self):
        for tag, name in [
            ("cf-api-contracts-v0.3.0", "api-contracts"),
            ("cf-api-contracts-consumer-v0.3.0", "api-contracts-consumer"),
        ]:
            component, _ = self.resolve(GITHUB_REF_NAME=tag)
            self.assertEqual(component["name"], name)
            self.assertIn("image", component)

    # --- explicit inputs (workflow_dispatch / workflow_call) -------------------

    def test_inputs_strip_leading_v(self):
        component, version = self.resolve(INPUT_COMPONENT="hello", INPUT_VERSION="v0.2.0")
        self.assertEqual(component["name"], "hello")
        self.assertEqual(version, "0.2.0")

    def test_inputs_win_over_tag(self):
        # Explicit inputs take precedence even over a tag that would otherwise match a
        # different component — the reusable workflow_call case, where the event is the
        # caller's (e.g. push) and GITHUB_REF_NAME is a branch/tag.
        component, version = self.resolve(
            GITHUB_REF_NAME="cf-gears-flight-control-v9.9.9",
            INPUT_COMPONENT="hello",
            INPUT_VERSION="1.2.3",
        )
        self.assertEqual(component["name"], "hello")
        self.assertEqual(version, "1.2.3")

    def test_unknown_component_exits(self):
        with self.assertRaises(SystemExit):
            self.resolve(INPUT_COMPONENT="nope", INPUT_VERSION="1.0.0")

    def test_bad_semver_exits(self):
        with self.assertRaises(SystemExit):
            self.resolve(INPUT_COMPONENT="hello", INPUT_VERSION="1.0")

    def test_partial_inputs_exit(self):
        with self.assertRaises(SystemExit):
            self.resolve(INPUT_COMPONENT="hello")  # version missing

    def test_no_inputs_and_no_ref_exits(self):
        with self.assertRaises(SystemExit):
            self.resolve()

    # --- plan (release-plz `releases` -> publish matrix) -----------------------

    def test_plan_keeps_only_publishable_gears(self):
        releases = json.dumps([
            {"package_name": "cf-gears-toolkit", "tag": "cf-gears-toolkit-v0.5.0", "version": "0.5.0"},
            {"package_name": "cf-gears-flight-control", "tag": "cf-gears-flight-control-v1.3.0", "version": "1.3.0"},
            {"package_name": "hello", "tag": "hello-v0.2.1", "version": "0.2.1"},
        ])
        self.assertEqual(
            resolve_release.plan(self.components, releases),
            [
                {"component": "flight-control", "version": "1.3.0"},
                {"component": "hello", "version": "0.2.1"},
            ],
        )

    def test_plan_empty(self):
        self.assertEqual(resolve_release.plan(self.components, "[]"), [])
        self.assertEqual(resolve_release.plan(self.components, ""), [])

    # --- outputs ---------------------------------------------------------------

    def test_outputs_for_a_gear(self):
        component, version = self.resolve(GITHUB_REF_NAME="hello-v0.2.0")
        out = resolve_release.outputs(component, version)
        self.assertEqual(out["component"], "hello")
        self.assertEqual(out["has_image"], "true")
        self.assertEqual(out["has_chart"], "true")
        self.assertIn("GEAR_PACKAGE=hello", out["build_args"].split("\n"))

    def test_outputs_ref_is_the_release_tag_for_a_gear(self):
        # A gear checks out its own release tag (crate prefix + version), so a
        # re-publish builds that version's source and not the dispatched branch.
        component, version = self.resolve(INPUT_COMPONENT="hello", INPUT_VERSION="0.2.0")
        self.assertEqual(resolve_release.outputs(component, version)["ref"], "hello-v0.2.0")
        component, version = self.resolve(
            INPUT_COMPONENT="flight-control", INPUT_VERSION="1.3.0"
        )
        self.assertEqual(
            resolve_release.outputs(component, version)["ref"],
            "cf-gears-flight-control-v1.3.0",
        )

    def test_outputs_ref_is_empty_for_a_crateless_component(self):
        # The platform umbrella has no crate/tag, so ref is empty and the
        # workflow falls back to the default checkout of the dispatched ref.
        component, version = self.resolve(INPUT_COMPONENT="toolkit-platform", INPUT_VERSION="0.4.0")
        self.assertEqual(resolve_release.outputs(component, version)["ref"], "")

    def test_outputs_for_a_chart_only_component(self):
        component, version = self.resolve(INPUT_COMPONENT="toolkit-platform", INPUT_VERSION="0.4.0")
        out = resolve_release.outputs(component, version)
        self.assertEqual(out["has_image"], "")
        self.assertEqual(out["has_chart"], "true")
        self.assertEqual(out["chart_path"], "deploy/helm/toolkit-platform")

    def test_outputs_for_no_match(self):
        out = resolve_release.outputs(None, "")
        self.assertEqual(out["has_image"], "")
        self.assertEqual(out["has_chart"], "")
        self.assertEqual(out["component"], "")
        self.assertEqual(out["ref"], "")

    # --- manifest integrity ----------------------------------------------------

    def test_every_manifest_path_exists(self):
        root = Path(__file__).resolve().parents[3]
        for component in self.components:
            if "image" in component:
                self.assertTrue(
                    (root / component["image"]["dockerfile"]).is_file(),
                    f"missing dockerfile for {component['name']}",
                )
            if "chart" in component:
                self.assertTrue(
                    (root / component["chart"] / "Chart.yaml").is_file(),
                    f"missing chart for {component['name']}",
                )


if __name__ == "__main__":
    unittest.main()
