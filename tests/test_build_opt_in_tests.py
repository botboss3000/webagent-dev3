"""Opt-in test groups: the listed-group check mode, its admission, results, nightly run and timer."""
import importlib.util
import json
import os
import tempfile
import threading
import time
import types
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

import test_admin_build as fixtures

host = fixtures.host
ROOT = Path(__file__).resolve().parents[1] / "build-service"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


executor = load("opt_in_executor", ROOT / "executor.py")
lifecycle = load("opt_in_lifecycle", ROOT / "lifecycle.py")
buildctl = load("opt_in_buildctl", ROOT / "buildctl.py")

PANIC = "thread 'multi_loop_tests::killed_loop' panicked at src/multi_loop_tests.rs:412:9:\nrun claimed twice"


def libtest(results, failure_text=""):
    lines = ["running %d tests" % len(results)]
    lines += ["test %s ... %s" % (name, outcome) for name, outcome in results]
    if failure_text:
        lines += ["", "failures:", "", "---- multi_loop_tests::killed_loop stdout ----", failure_text, "",
                  "failures:", "    multi_loop_tests::killed_loop", ""]
    failed = sum(outcome == "FAILED" for _, outcome in results)
    lines.append("test result: %s. %d passed; %d failed; 0 ignored" % ("FAILED" if failed else "ok", len(results) - failed, failed))
    return "\n".join(lines) + "\n"


class GroupListTests(unittest.TestCase):
    def test_only_listed_groups_are_accepted(self):
        self.assertEqual(executor.test_group_request("multi_loop"), ("multi_loop", 1, 1800))
        for group in ("unknown", "multi_loop --nocapture", "", None, 3):
            with self.assertRaises(ValueError):
                executor.test_group_request(group)

    def test_repeat_and_run_timeout_bounds(self):
        self.assertEqual(executor.test_group_request("multi_loop", 30, 60)[1:], (30, 60))
        for repeat in (0, 31, -1, "2", True, 2.0):
            with self.assertRaises(ValueError):
                executor.test_group_request("multi_loop", repeat)
        for timeout in (59, 3601, "600"):
            with self.assertRaises(ValueError):
                executor.test_group_request("multi_loop", 1, timeout)

    def test_the_group_fixes_the_whole_cargo_command(self):
        self.assertEqual(executor.test_group_argv("multi_loop"),
                         ["cargo", "test", "--locked", "--offline", "--bin", "webagent-v3", "multi_loop", "--",
                          "--test-threads=1", "--ignored"])
        self.assertEqual(executor.test_group_argv("multi_loop", compile_only=True)[-1], "--no-run")

    def test_group_runs_take_the_lowest_cpu_weight(self):
        worker = executor.Executor(None, None, {})
        self.assertEqual(worker.cpu_weight_args("test", {"check_task": "test_group"}), ["--cpu-shares", "2"])
        self.assertEqual(worker.cpu_weight_args("compile_test", {"check_task": "test_group"}), ["--cpu-shares", "2"])
        self.assertEqual(worker.cpu_weight_args("test", {"check_task": "test"}), [])


class ReportTests(unittest.TestCase):
    def test_failure_output_keeps_the_panic_even_when_long(self):
        noise = "child loop log line\n" * 20000
        report = executor.test_run_report(libtest([("multi_loop_tests::a", "ok"), ("multi_loop_tests::killed_loop", "FAILED")],
                                                  noise + PANIC))
        self.assertEqual(report["tests"], {"multi_loop_tests::a": "passed", "multi_loop_tests::killed_loop": "failed"})
        self.assertIn("run claimed twice", report["failure_output"])
        self.assertIn("bytes cut", report["failure_output"])
        self.assertLessEqual(len(report["failure_output"].encode()),
                             executor.FAILURE_HEAD_BYTES + executor.FAILURE_TAIL_BYTES + 64)

    def test_a_run_without_failures_section_keeps_its_tail(self):
        report = executor.test_run_report("running 5 tests\ntest multi_loop_tests::a ... ok\n" + PANIC)
        self.assertIn("run claimed twice", report["failure_output"])


class FakeRunner:
    def __init__(self):
        self.job_context = types.SimpleNamespace(event=None)

    def execution_application_blob(self, blob):
        return blob

    def execution_source_digest(self, source):
        return "same"


class ExecutorTests(unittest.TestCase):
    def run_group(self, outcomes, repeat):
        with tempfile.TemporaryDirectory() as folder:
            worker = executor.Executor(FakeRunner(), None, {"state_root": folder})
            calls = []
            worker.dependencies = lambda *a, **k: ""

            def phase(job, name, kind, source, output, argv, profile, timeout=3600, **_):
                calls.append((name, argv, timeout))
                if name == "compile_test":
                    return "compiled\n"
                outcome = outcomes[len([c for c in calls if c[0] == "test"]) - 1]
                if outcome == "pass":
                    return libtest([("multi_loop_tests::a", "ok"), ("multi_loop_tests::killed_loop", "ok")])
                if outcome == "timeout":
                    error = TimeoutError("fixed build task timeout")
                    error.log = "running 2 tests\ntest multi_loop_tests::a ... ok\n" + PANIC
                    raise error
                raise RuntimeError("fixed command failed: " + libtest(
                    [("multi_loop_tests::a", "ok"), ("multi_loop_tests::killed_loop", "FAILED")], PANIC))
            worker.phase = phase
            job = {"job_id": str(uuid.uuid4()), "test_group": "multi_loop", "test_repeat": repeat,
                   "test_run_timeout": 900, "inputs_sha256": "a" * 64, "check_task": "test_group"}
            try:
                log = worker.test_group(job, b"blob", lambda path, blob: (path / "v3").mkdir(parents=True))
                error = None
            except RuntimeError as raised:
                log, error = None, raised
            return job, calls, log, error

    def test_every_repetition_runs_and_failures_keep_their_output(self):
        job, calls, log, error = self.run_group(["pass", "fail", "pass", "timeout"], 4)
        self.assertEqual([c[0] for c in calls], ["compile_test", "test", "test", "test", "test"])
        self.assertTrue(all(c[2] == 900 for c in calls[1:]))
        result = job["test_group_result"]
        self.assertEqual([r["status"] for r in result["runs"]], ["passed", "failed", "passed", "timed_out"])
        self.assertEqual((result["passed_runs"], result["failed_runs"]), (2, 2))
        self.assertEqual(result["runs"][1]["tests"]["multi_loop_tests::killed_loop"], "failed")
        self.assertIn("run claimed twice", result["runs"][1]["failure_output"])
        self.assertIn("run claimed twice", result["runs"][3]["failure_output"])
        self.assertNotIn("failure_output", result["runs"][0])
        self.assertIsNotNone(error)
        self.assertIn("2 of 4 repetitions passed", str(error))
        self.assertIn("repetition 2: failed", str(error))

    def test_all_passing_completes(self):
        job, calls, log, error = self.run_group(["pass"] * 3, 3)
        self.assertIsNone(error)
        self.assertIn("3 of 3 repetitions passed", log)

    def test_a_run_that_reports_no_test_is_a_failure(self):
        with tempfile.TemporaryDirectory() as folder:
            worker = executor.Executor(FakeRunner(), None, {"state_root": folder})
            worker.dependencies = lambda *a, **k: ""
            worker.phase = lambda job, name, *a, **k: "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored\n"
            job = {"job_id": str(uuid.uuid4()), "test_group": "multi_loop", "check_task": "test_group"}
            with self.assertRaises(RuntimeError):
                worker.test_group(job, b"blob", lambda path, blob: (path / "v3").mkdir(parents=True))


class BrokerTests(unittest.TestCase):
    tearDown = fixtures.BuildTests.tearDown
    call = fixtures.BuildTests.call

    def setUp(self):
        fixtures.BuildTests.setUp(self)
        self.local = dict(self.trusted, local_caller=True)

    def test_only_the_local_operator_may_request_and_only_listed_groups(self):
        with self.assertRaises(PermissionError):
            self.call("build_work", {"action": "test_group", "group": "multi_loop", "request_id": "fixture-tg-1"})
        self.broker.resources.enabled = True
        for args in ({"group": "anything"}, {"group": "multi_loop", "repeat": 31}, {"group": "multi_loop", "repeat": 0},
                     {"group": "multi_loop", "run_timeout_seconds": 5}):
            with self.assertRaises(ValueError):
                self.call("build_work", {"action": "test_group", "request_id": "fixture-tg-1", **args}, trusted=self.local)
        with self.assertRaises(ValueError):
            self.call("build_work", {"action": "test_group", "group": "multi_loop", "request_id": "fixture-tg-1",
                                     "cargo_args": ["--", "--nocapture"]}, trusted=self.local)

    def queued_group(self):
        job_id = str(uuid.uuid4())
        job = {"job_id": job_id, "operation": "check", "check_task": "test_group", "status": "queued", "created_at": time.time(),
               "user_id": "fixture-user", "workspace": self.workspace, "task_id": None, "phase_history": []}
        self.broker.jobs[job_id] = job
        self.broker.check_runs[job_id] = (job, None, None, b"", "a" * 64, "test_group", 900, threading.Event())
        return job

    def test_a_running_release_build_defers_the_group(self):
        build = {"job_id": str(uuid.uuid4()), "operation": "build_deploy", "status": "running", "created_at": time.time() - 60}
        self.broker.jobs[build["job_id"]] = build
        job = self.queued_group()
        started = []
        with patch.object(self.broker, "_has_unresolved_release", return_value=False), \
                patch.object(self.broker.pool, "submit", side_effect=lambda *a: started.append(a)):
            self.broker._start_next()
            self.assertEqual(started, [])
            self.assertEqual(job["status"], "queued")
            self.assertEqual(job["waiting_reason"], "waits until no release build runs")
            build["status"] = "completed"
            self.broker._start_next()
        self.assertEqual(len(started), 1)
        self.assertEqual(job["status"], "running")

    def test_published_group_takes_only_the_check_token(self):
        self.assertEqual(self.broker.scheduler.job_scopes({"operation": "check", "check_task": "test_group"}),
                         {"execution:rust-check"})


class SummaryReadTests(unittest.TestCase):
    def write(self, folder, value):
        (Path(folder) / "latest.json").write_text(json.dumps(value))
        return host.opt_in_tests_summary(Path(folder), os.getuid())

    def test_projection_and_refusals(self):
        good = {"status": "failed", "checked_at": 1795500000.5, "groups": [
            {"group": "multi_loop", "status": "failed", "job_id": str(uuid.uuid4()), "inputs_sha256": "b" * 64,
             "tests": 5, "failed_tests": ["multi_loop_tests::killed_loop"]}]}
        with tempfile.TemporaryDirectory() as folder:
            view = self.write(folder, good)
            self.assertTrue(view["available"])
            self.assertEqual(view["groups"][0]["failed_tests"], ["multi_loop_tests::killed_loop"])
            for bad in (dict(good, status="exploded"), dict(good, extra=1),
                        dict(good, groups=[dict(good["groups"][0], failed_tests=["rm -rf /"])]),
                        dict(good, groups=[dict(good["groups"][0], output="panic text")])):
                self.assertEqual(self.write(folder, bad), {"available": False, "reason": "malformed"})
            self.assertEqual(host.opt_in_tests_summary(Path(folder), os.getuid() + 1)["reason"], "refused")
            os.unlink(Path(folder) / "latest.json")
            self.assertEqual(host.opt_in_tests_summary(Path(folder), os.getuid())["reason"], "missing")


class NightlyTests(unittest.TestCase):
    def run_nightly(self, job, queue_limit=3 * 3600):
        calls = []
        clock = [1795500000.0]

        def request(name, args):
            calls.append((name, args))
            if name == "build_work":
                return {"job_id": "11111111-2222-3333-4444-555555555555", "inputs_sha256": "c" * 64}
            if name == "v3_worker_job":
                return {"job": job}
            return {"job": {}}

        def sleep(seconds):
            clock[0] += seconds
        with tempfile.TemporaryDirectory() as folder:
            summary = buildctl.nightly_tests(request, now=lambda: clock[0], sleep=sleep, root=folder, groups=["multi_loop"],
                                             queue_limit=queue_limit)
            written = json.loads((Path(folder) / "latest.json").read_text())
            self.assertTrue(host.opt_in_tests_summary(Path(folder), os.getuid())["available"])
        self.assertEqual(summary, written)
        return summary, calls

    def test_a_failed_run_names_its_failed_tests(self):
        job = {"status": "failed", "test_group_result": {"runs": [
            {"status": "failed", "tests": {"multi_loop_tests::a": "passed", "multi_loop_tests::killed_loop": "failed"}}]}}
        summary, calls = self.run_nightly(job)
        self.assertEqual(summary["status"], "failed")
        self.assertEqual(summary["groups"][0]["failed_tests"], ["multi_loop_tests::killed_loop"])
        self.assertEqual(calls[0][1]["action"], "test_group")
        self.assertNotIn("task_id", calls[0][1])

    def test_a_passing_run(self):
        job = {"status": "completed", "test_group_result": {"runs": [{"status": "passed", "tests": {"multi_loop_tests::a": "passed"}}]}}
        summary, _ = self.run_nightly(job)
        self.assertEqual((summary["status"], summary["groups"][0]["tests"]), ("passed", 1))

    def test_a_run_still_queued_behind_builds_is_cancelled_and_skipped(self):
        summary, calls = self.run_nightly({"status": "queued"}, queue_limit=120)
        self.assertEqual(summary["status"], "skipped")
        self.assertIn("v3_build_cancel", [name for name, _ in calls])


class TimerUnitTests(unittest.TestCase):
    def test_timer_runs_nightly_tests_as_the_owner_at_low_priority(self):
        with tempfile.TemporaryDirectory() as folder:
            calls = []
            run = lambda argv, **_: calls.append(argv)
            first = lifecycle.install_nightly_tests({"instance": "development"}, "/srv/release", "ar", unit_root=folder, run=run)
            service = (Path(folder) / "webagent-dev-opt-in-tests.service").read_text()
            timer = (Path(folder) / "webagent-dev-opt-in-tests.timer").read_text()
            self.assertIn("User=ar", service)
            self.assertIn("/srv/release/buildctl.py nightly-tests", service)
            self.assertIn("Nice=19", service)
            self.assertIn("OnCalendar=", timer)
            self.assertTrue(first["changed"])
            self.assertIn(["systemctl", "enable", "--now", "webagent-dev-opt-in-tests.timer"], calls)
            again = lifecycle.install_nightly_tests({"instance": "development"}, "/srv/release", "ar", unit_root=folder, run=run)
            self.assertFalse(again["changed"])


if __name__ == "__main__":
    unittest.main()
