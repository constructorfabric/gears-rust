#!/usr/bin/env python3
"""Unit tests for release_retry.py (retried release-plz release).

    python3 -m unittest discover -s tools/scripts/tests
"""

from __future__ import annotations

import io
import json
import os
import sys
import tarfile
import unittest
import unittest.mock
import urllib.error
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import release_retry  # noqa: E402


def package(name: str, version: str, publish=None) -> dict:
    return {"id": f"{name}@{version}", "name": name, "version": version, "publish": publish}


METADATA = {
    "workspace_members": ["lib@1.0.0", "gear@0.3.0", "app@0.1.5", "example@0.2.0"],
    "packages": [
        package("lib", "1.0.0"),
        package("gear", "0.3.0"),
        package("app", "0.1.5", publish=[]),  # git_only in release-plz.toml
        package("example", "0.2.0", publish=[]),  # never released
    ],
}
RELEASABLE = release_retry.releasable_packages(METADATA, {"app"})


class ReleasablePackagesTest(unittest.TestCase):
    def test_publishable_and_git_only_packages_are_releasable(self):
        self.assertEqual(sorted(RELEASABLE), ["app-v0.1.5", "gear-v0.3.0", "lib-v1.0.0"])

    def test_git_only_package_is_not_published_to_crates_io(self):
        self.assertFalse(RELEASABLE["app-v0.1.5"]["crates_io"])
        self.assertTrue(RELEASABLE["lib-v1.0.0"]["crates_io"])

    def test_git_only_set_comes_from_package_entries(self):
        config = {"package": [{"name": "app", "git_only": True}, {"name": "lib"}]}
        self.assertEqual(release_retry.git_only_packages(config), {"app"})

    def test_real_workspace_config_names_git_only_packages(self):
        import tomllib

        config = tomllib.loads(release_retry.RELEASE_CONFIG.read_text())
        self.assertIn("cf-gears-flight-control", release_retry.git_only_packages(config))


class ReleasesFromTagsTest(unittest.TestCase):
    def test_only_manifest_versions_of_releasable_packages_count(self):
        new = {"gear-v0.3.0", "gear-v0.2.9", "example-v0.2.0", "other-v1.0.0", "weird"}
        self.assertEqual(
            release_retry.releases_from_tags(new, RELEASABLE),
            [{"package_name": "gear", "version": "0.3.0", "tag": "gear-v0.3.0"}],
        )

    def test_merge_keeps_first_entry_per_tag(self):
        first = [{"package_name": "gear", "version": "0.3.0", "tag": "gear-v0.3.0", "prs": [1]}]
        second = [
            {"package_name": "gear", "version": "0.3.0", "tag": "gear-v0.3.0"},
            {"package_name": "app", "version": "0.1.5", "tag": "app-v0.1.5"},
        ]
        merged = release_retry.merge_releases(first, second)
        self.assertEqual([r["tag"] for r in merged], ["gear-v0.3.0", "app-v0.1.5"])
        self.assertEqual(merged[0]["prs"], [1])

    def test_parse_releases_treats_empty_as_no_releases(self):
        self.assertEqual(release_retry.parse_releases(""), [])
        self.assertEqual(release_retry.parse_releases(None), [])
        self.assertEqual(release_retry.parse_releases("[]"), [])


class WaitSecondsTest(unittest.TestCase):
    def test_fixed_delay_when_budget_is_fine(self):
        rate = {"remaining": 4000, "reset": 2000}
        self.assertEqual(release_retry.wait_seconds(300, 3600, 1000, rate, 500), 300)

    def test_waits_for_reset_when_budget_is_spent(self):
        rate = {"remaining": 0, "reset": 2000}
        self.assertEqual(release_retry.wait_seconds(300, 3600, 1000, rate, 500), 1030)

    def test_reset_wait_is_capped(self):
        rate = {"remaining": 0, "reset": 9000}
        self.assertEqual(release_retry.wait_seconds(300, 3600, 1000, rate, 500), 3600)

    def test_fixed_delay_when_budget_is_unknown(self):
        self.assertEqual(release_retry.wait_seconds(600, 3600, 1000, None, 500), 600)

    def test_reset_in_the_past_keeps_fixed_delay(self):
        rate = {"remaining": 0, "reset": 500}
        self.assertEqual(release_retry.wait_seconds(300, 3600, 1000, rate, 500), 300)


PUBLISHED_AT = "b" * 40


def crate_bytes(name: str, version: str, vcs_info: dict | None) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        files = {"Cargo.toml": b"[package]\n"}
        if vcs_info is not None:
            files[".cargo_vcs_info.json"] = json.dumps(vcs_info).encode()
        for path, data in files.items():
            info = tarfile.TarInfo(f"{name}-{version}/{path}")
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


class VcsCommitTest(unittest.TestCase):
    def test_reads_the_commit_cargo_publish_recorded(self):
        crate = crate_bytes("gear", "0.3.0", {"git": {"sha1": PUBLISHED_AT}, "path_in_vcs": "gears/gear"})
        self.assertEqual(release_retry.vcs_commit(crate, "gear", "0.3.0"), (PUBLISHED_AT, False))

    def test_reports_a_dirty_worktree(self):
        crate = crate_bytes("gear", "0.3.0", {"git": {"sha1": PUBLISHED_AT, "dirty": True}})
        self.assertEqual(release_retry.vcs_commit(crate, "gear", "0.3.0"), (PUBLISHED_AT, True))

    def test_crate_without_vcs_info_has_no_commit(self):
        crate = crate_bytes("gear", "0.3.0", None)
        self.assertEqual(release_retry.vcs_commit(crate, "gear", "0.3.0"), (None, False))


class CreateTagTest(unittest.TestCase):
    @staticmethod
    def http_error(code: int) -> urllib.error.HTTPError:
        return urllib.error.HTTPError("https://api.github.com", code, "error", None, None)

    def create(self, *responses):
        with unittest.mock.patch.object(release_retry, "http_json", side_effect=responses) as http:
            return release_retry.create_tag("owner/repo", "t", "gear-v0.3.0", PUBLISHED_AT), http

    def test_created(self):
        created, _ = self.create({})
        self.assertTrue(created)

    def test_422_with_existing_ref_means_already_tagged(self):
        created, http = self.create(self.http_error(422), {"ref": "refs/tags/gear-v0.3.0"})
        self.assertFalse(created)
        self.assertEqual(http.call_args.args[0], "https://api.github.com/repos/owner/repo/git/ref/tags/gear-v0.3.0")

    def test_422_without_the_ref_is_an_error(self):
        with self.assertRaises(urllib.error.HTTPError) as raised:
            self.create(self.http_error(422), self.http_error(404))
        self.assertEqual(raised.exception.code, 422)


class HealUntaggedTest(unittest.TestCase):
    def setUp(self):
        patcher = unittest.mock.patch.dict(os.environ, {"GITHUB_REPOSITORY": "owner/repo", "GH_TOKEN": "t"})
        patcher.start()
        self.addCleanup(patcher.stop)

    def heal(self, tags, published, create_result=True, commit=(PUBLISHED_AT, False), known=True):
        with unittest.mock.patch.object(
            release_retry, "create_tag", return_value=create_result
        ) as create:
            result = release_retry.heal_untagged(
                RELEASABLE,
                tags,
                lambda name, version: (name, version) in published,
                lambda name, version: commit,
                lambda sha: known,
            )
        return result, create

    def test_published_crate_without_tag_is_tagged_at_its_publish_commit(self):
        (healed, errors), create = self.heal(
            {"app-v0.1.5", "lib-v1.0.0"}, published={("gear", "0.3.0")}
        )
        self.assertEqual(healed, [{"package_name": "gear", "version": "0.3.0", "tag": "gear-v0.3.0"}])
        self.assertEqual(errors, [])
        create.assert_called_once_with("owner/repo", "t", "gear-v0.3.0", PUBLISHED_AT)

    def test_unknown_publish_commit_is_an_error_not_a_guess(self):
        for commit, known in (((None, False), True), ((PUBLISHED_AT, False), False)):
            with self.subTest(commit=commit, known=known):
                (healed, errors), create = self.heal(
                    {"app-v0.1.5", "lib-v1.0.0"},
                    published={("gear", "0.3.0")},
                    commit=commit,
                    known=known,
                )
                self.assertEqual(healed, [])
                self.assertEqual(len(errors), 1)
                self.assertIn("gear-v0.3.0", errors[0])
                create.assert_not_called()

    def test_failed_tag_creation_is_an_error(self):
        with unittest.mock.patch.object(
            release_retry, "create_tag", side_effect=CreateTagTest.http_error(422)
        ):
            healed, errors = release_retry.heal_untagged(
                RELEASABLE,
                {"app-v0.1.5", "lib-v1.0.0"},
                lambda name, version: True,
                lambda name, version: (PUBLISHED_AT, False),
                lambda sha: True,
            )
        self.assertEqual(healed, [])
        self.assertEqual(len(errors), 1)

    def test_unpublished_crate_without_tag_is_left_alone(self):
        (healed, errors), create = self.heal({"app-v0.1.5", "lib-v1.0.0"}, published=set())
        self.assertEqual((healed, errors), ([], []))
        create.assert_not_called()

    def test_tag_created_concurrently_is_not_released_twice(self):
        (healed, errors), _ = self.heal(
            {"app-v0.1.5", "lib-v1.0.0"}, published={("gear", "0.3.0")}, create_result=False
        )
        self.assertEqual((healed, errors), ([], []))

    def test_git_only_package_without_tag_is_an_error(self):
        (healed, errors), create = self.heal({"gear-v0.3.0", "lib-v1.0.0"}, published=set())
        self.assertEqual(healed, [])
        self.assertEqual(len(errors), 1)
        self.assertIn("app-v0.1.5", errors[0])
        create.assert_not_called()


if __name__ == "__main__":
    unittest.main()
