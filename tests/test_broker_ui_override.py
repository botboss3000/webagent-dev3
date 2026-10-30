"""A failing UI fixture reruns once, then pauses for exact operator acceptance bound to the candidate."""
import importlib.util
from pathlib import Path
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid

import test_admin_build as fixtures

ROOT = Path(__file__).resolve().parents[1] / "build-service"


def load(name):
    spec = importlib.util.spec_from_file_location(name + "_under_test", ROOT / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


publication = load("ui_publication")
A, B = "v3/ui/boot/signed-out.browser.test.mjs", "v3/ui/chat/composer.browser.test.mjs"


def failure(*paths):
    return RuntimeError("fixed command failed: ok 1\n✖ failing tests:\n\n" + "".join(
        "test at " + p.removeprefix("v3/") + ":1:1\n✖ /candidate/" + p + " (5ms)\n" for p in paths))


class Runner:
    def __init__(self, outcomes):
        self.outcomes = list(outcomes); self.calls = []

    def validate_selected_tests(self, job_id, root, evidence):
        self.calls.append(list(evidence["tests"]))
        outcome = self.outcomes.pop(0)
        if isinstance(outcome, Exception): raise outcome
        return {"status": "passed" if evidence["tests"] else "not_applicable", "tests": list(evidence["tests"])}


def unit(outcomes, operation="build_deploy", jobs=None):
    saves = []
    broker = SimpleNamespace(lock=threading.RLock(), _save=saves.append, runner=Runner(outcomes), jobs=jobs or {})
    job = {"job_id": "j", "operation": operation, "inputs_sha256": "1" * 64, "status": "running"}
    return publication.UIPublication(broker, None, None, None, None, None), broker, job, saves


EVIDENCE = {"tests": [A, B], "sha256": "2" * 64}


class RerunAndPauseTests(unittest.TestCase):
    def test_parser_restricts_to_selected_fixtures_and_fails_closed(self):
        self.assertEqual(publication.failing_fixtures(str(failure(A)), [A, B]), [A])
        self.assertIsNone(publication.failing_fixtures(str(failure("v3/ui/other.test.mjs")), [A, B]))
        self.assertIsNone(publication.failing_fixtures("fixed command failed: timeout", [A, B]))

    def test_rerun_passes_only_failing_files_and_continues(self):
        ui, broker, job, _ = unit([failure(A), {"status": "passed"}])
        result = ui.frontend_tests(job, Path("/x"), EVIDENCE, 1, "rev")
        self.assertEqual(broker.runner.calls, [[A, B], [A]])
        self.assertEqual((result["status"], result["rerun"], result["rerun_files"], result["tests"]), ("passed", True, [A], [A, B]))
        history = job["ui_verification_history"]
        self.assertEqual([e["result"]["status"] for e in history], ["failed", "passed"])
        self.assertTrue(history[1]["rerun"]); self.assertEqual(job["status"], "running")

    def test_rerun_failure_pauses_with_files_and_history(self):
        prior = {"status": "completed", "finished_at": 1, "ui_test_evidence": {"status": "passed", "tests": [A, B]}}
        flaky = {"status": "failed", "finished_at": 2, "reason": str(failure(A))}
        ui, broker, job, saves = unit([failure(A, B), failure(A)], jobs={"p": prior, "f": flaky})
        self.assertIsNone(ui.frontend_tests(job, Path("/x"), EVIDENCE, 1, "rev"))
        self.assertEqual((job["status"], job["phase"]), ("needs_decision", "awaiting_ui_test_decision"))
        record = job["ui_test_failure"]
        self.assertEqual((record["files"], record["attempts"], record["inputs_sha256"]), ([A], 2, "1" * 64))
        self.assertLessEqual(len(record["output_tail"]), 4000); self.assertIn(A, job["reason"])
        self.assertEqual(record["history"]["files"][A], {"passed": 1, "failed": 1})
        self.assertEqual(broker.runner.calls, [[A, B], [A, B]]); self.assertIs(saves[-1], job)

    def test_unparseable_or_other_lane_failure_keeps_failing(self):
        ui, _, job, _ = unit([RuntimeError("fixed command failed: killed")])
        with self.assertRaisesRegex(RuntimeError, "killed"): ui.frontend_tests(job, Path("/x"), EVIDENCE, 1, "rev")
        ui, _, job, _ = unit([failure(A), failure(A)], operation="ui_deploy")
        with self.assertRaises(RuntimeError): ui.frontend_tests(job, Path("/x"), EVIDENCE, 1, "rev")
        self.assertNotIn("ui_test_failure", job)

    def test_bound_override_skips_only_accepted_files(self):
        ui, broker, job, _ = unit([{"status": "passed"}])
        job["ui_test_override"] = {"files": [A], "reason": "known flaky boot fixture", "inputs_sha256": "1" * 64}
        result = ui.frontend_tests(job, Path("/x"), EVIDENCE, 1, "rev")
        self.assertEqual(broker.runner.calls, [[B]])
        self.assertEqual((result["status"], result["accepted_files"], result["tests"]), ("accepted_override", [A], [A, B]))

    def test_override_for_other_inputs_reruns_everything(self):
        ui, broker, job, _ = unit([{"status": "passed"}])
        job["ui_test_override"] = {"files": [A], "reason": "known flaky boot fixture", "inputs_sha256": "9" * 64}
        self.assertEqual(ui.frontend_tests(job, Path("/x"), EVIDENCE, 1, "rev")["status"], "passed")
        self.assertEqual(broker.runner.calls, [[A, B]])


class OperatorAcceptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.broker = fixtures.host.Broker(Path(self.temp.name) / "jobs", fixtures.FakeRunner())
        self.local = {"user_id": "codex-local", "session_id": fixtures.host.LOCAL_WORKSPACE, "lease_until": time.time() + 60,
                      "access_fingerprint": "f" * 64, "local_caller": True}
        self.job = {"job_id": str(uuid.uuid4()), "operation": "build_deploy", "status": "needs_decision", "phase": "awaiting_ui_test_decision",
                    "inputs_sha256": "1" * 64, "user_id": "codex-local", "workspace": fixtures.host.LOCAL_WORKSPACE, "access_fingerprint": "f" * 64,
                    "isolated": True, "lease_until": time.time() - 1, "images": {},
                    "ui_test_failure": {"files": [A, B], "attempts": 2, "inputs_sha256": "1" * 64, "output_tail": "", "at": 1}}
        self.broker.jobs[self.job["job_id"]] = self.job

    def tearDown(self):
        self.broker.shutdown(); self.temp.cleanup()

    def accept(self, files, reason="flaky boot fixture, verified manually", trusted=None, workspace=fixtures.host.LOCAL_WORKSPACE):
        return self.broker.handle({"id": "x", "name": "v3_build_adopt_release", "workspace": workspace, "trusted": trusted or self.local,
                                   "arguments": {"job_id": self.job["job_id"], "accept_ui_failure": files, "reason": reason}})

    def test_exact_files_and_reason_record_override_and_requeue(self):
        with patch.object(self.broker, "_start_next") as start:
            result = self.accept([B, A])
        start.assert_called_once()
        override = self.job["ui_test_override"]
        self.assertEqual((override["files"], override["by"], override["inputs_sha256"]), (sorted([A, B]), "codex-local", "1" * 64))
        self.assertIn("history", override)
        self.assertEqual((self.job["status"], self.job["phase"], self.job["sync_resume"]), ("queued", "waiting", True))
        self.assertGreater(self.job["lease_until"], time.time()); self.assertEqual(result["status"], "queued")
        self.assertIn("ui_test_override", self.broker._view(self.job)); self.assertIn("ui_test_failure", self.broker._view(self.job))

    def test_rejections_leave_job_paused(self):
        with patch.object(self.broker, "_start_next"):
            with self.assertRaisesRegex(ValueError, "exactly equal"): self.accept([A])
            with self.assertRaisesRegex(ValueError, "exactly equal"): self.accept([A, B, "v3/ui/x.test.mjs"])
            with self.assertRaisesRegex(ValueError, "10-300"): self.accept([A, B], reason="  short   ")
            with self.assertRaisesRegex(ValueError, "10-300"): self.accept([A, B], reason="x" * 301)
            other = {"user_id": "fixture-user", "session_id": self.local["session_id"], "lease_until": time.time() + 30, "access_fingerprint": "f" * 64}
            with self.assertRaises(PermissionError): self.accept([A, B], trusted=other)
            app = dict(self.local); app.pop("local_caller")
            with self.assertRaises(PermissionError): self.accept([A, B], trusted=app)
            self.job["phase"] = "awaiting_prepared_reviews"
            with self.assertRaisesRegex(ValueError, "not paused"): self.accept([A, B])
            self.job["phase"] = "awaiting_ui_test_decision"; self.job["ui_test_failure"]["inputs_sha256"] = "9" * 64
            with self.assertRaisesRegex(ValueError, "stale"): self.accept([A, B])
        self.assertEqual((self.job["status"], self.job.get("ui_test_override")), ("needs_decision", None))


if __name__ == "__main__":
    unittest.main()
