from copy import deepcopy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("benchmark", Path(__file__).parents[1] / "benchmark.py")
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class BenchmarkTest(unittest.TestCase):
    def data(self):
        return {"repo": "owner/repo", "samples": [{"workflow": "CI", "event": "workflow_dispatch", "cache_state": "warm", "conclusion": "success", "url": "url",
            "jobs": [{"name": "test", "conclusion": "success", "seconds": seconds, "labels": ["ubuntu-latest"]}]} for seconds in (100, 500, 102)]}

    def test_compares_medians_and_never_mixes_cold_and_warm(self):
        before = self.data()
        after = deepcopy(before)
        for run in after["samples"]:
            run["jobs"][0]["seconds"] /= 2
        result = benchmark.comparison(before, after)
        self.assertIn("102.0s -> 51.0s (+50.0%)", result)
        after["samples"][0]["cache_state"] = "cold"
        self.assertIn("UNMATCHED", benchmark.comparison(before, after))

    def test_failed_or_cancelled_run_cannot_look_like_acceleration(self):
        after = self.data()
        after["samples"][0]["conclusion"] = "cancelled"
        with self.assertRaises(ValueError):
            benchmark.comparison(self.data(), after)

    def test_different_events_cannot_establish_acceleration(self):
        after = self.data()
        for run in after["samples"]:
            run["event"] = "pull_request"
        with self.assertRaisesRegex(ValueError, "event type"):
            benchmark.comparison(self.data(), after)

    def test_job_duration_includes_post_steps_and_offset_is_not_queue(self):
        run = {"id": 1, "run_attempt": 2, "head_sha": "sha", "html_url": "url", "event": "workflow_dispatch", "name": "CI", "conclusion": "success", "run_started_at": "2026-10-14T10:00:00Z"}
        jobs = [{"id": 2, "name": "test", "conclusion": "success", "started_at": "2026-10-14T10:01:00Z", "completed_at": "2026-10-14T10:04:00Z", "steps": [{"name": "Post cache", "started_at": "2026-10-14T10:03:00Z", "completed_at": "2026-10-14T10:04:00Z"}]}]
        result = benchmark.sample(run, jobs, "warm")
        self.assertEqual(result["jobs"][0]["seconds"], 180)
        self.assertEqual(result["jobs"][0]["steps"][0]["seconds"], 60)
        self.assertEqual(result["jobs"][0]["start_offset_seconds"], 60)
        self.assertEqual(result["runner_seconds"], 180)
        self.assertEqual(result["attempt_elapsed_seconds"], 240)


if __name__ == "__main__":
    unittest.main()
