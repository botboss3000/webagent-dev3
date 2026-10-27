"""Cancellation races use fake deployments; subprocess tests use disposable processes."""
import importlib.util
import copy
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("build_fixtures", Path(__file__).with_name("test_admin_build.py"))
fixtures = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixtures)
host = fixtures.host


class CancelTests(unittest.TestCase):
    setUp = fixtures.BuildTests.setUp
    tearDown = fixtures.BuildTests.tearDown
    call = fixtures.BuildTests.call
    build = fixtures.BuildTests.build
    deploy = fixtures.BuildTests.deploy
    wait = fixtures.BuildTests.wait

    def cancel(self, job, **kwargs):
        stored = self.broker.jobs.get(job["job_id"])
        affected = self.broker._view(stored)["cancellation"]["affected_job_ids"] if stored else [job["job_id"]]
        return self.call("v3_build_cancel", {"job_id": job["job_id"], "request_id": "cancel-fixture", "expected_job_ids": affected}, **kwargs)

    def entered_build(self):
        for _ in range(100):
            if any(c[0] == "build" for c in self.runner.calls):
                return
            time.sleep(.01)
        self.fail("build did not start")

    def test_active_cancel_is_durable_and_idempotent(self):
        self.runner.block = threading.Event()
        job = self.build()
        self.entered_build()
        first = self.cancel(job)
        self.assertEqual("stop_requested", first["disposition"])
        persisted = json.loads((self.state / (job["job_id"] + ".json")).read_text())
        self.assertTrue(persisted["revoked"])
        self.assertEqual("cancel-fixture", persisted["cancel_request"]["request_id"])
        self.assertEqual(first, self.cancel(job))
        self.runner.block.set()
        result = self.wait(job)
        self.assertEqual("cancelled", result["status"])
        self.assertFalse(result["cancellation"]["available"])
        self.assertFalse(any(c[0] in ("stop", "create", "activate_assets") for c in self.runner.calls))
        self.assertEqual("cancelled", self.cancel(job)["disposition"])
        self.assertEqual(1, len([c for c in self.runner.calls if c[0] == "build"]))

    def test_other_owner_denied_admin_can_stop_host_job(self):
        self.runner.block = threading.Event()
        job = self.build()
        self.entered_build()
        with self.assertRaises(PermissionError):
            self.cancel(job, trusted={**self.trusted, "user_id": "other"})
        self.assertFalse(self.broker.jobs[job["job_id"]]["revoked"])
        admin = {**self.trusted, "user_id": "admin", "session_id": "00000000-0000-0000-0000-000000000000"}
        self.cancel(job, trusted=admin, workspace=admin["session_id"])
        self.runner.block.set()
        self.assertEqual("cancelled", self.wait(job)["status"])

    def test_queued_cancel_never_runs_and_survives_restart(self):
        receipts = []
        self.broker.stopping = True  # deterministic admission barrier
        for task_id in ("queued-one", "queued-two"):
            item = self.call("v3_build_work", {"action": "begin", "mode": "isolated", "task_id": task_id, "summary": task_id})
            source = Path(item["workspace_path"], "src/new_" + task_id + ".rs")
            source.parent.mkdir(exist_ok=True)
            source.write_text(task_id)
            digest = self.call("v3_build_work", {"action": "status", "task_id": task_id})["own"][0]["workspace_inputs_sha256"]
            # finishing while stopping is forbidden; hold the scheduler instead
            self.broker.stopping = False
            with patch.object(self.broker, "_start_next"):
                receipts.append(self.call("v3_build_work", {"action": "finish", "task_id": task_id, "expected_inputs_sha256": digest}))
            self.broker.stopping = True
        result = self.cancel(receipts[0])
        self.assertEqual("cancelled", result["disposition"])
        self.assertEqual("queued", self.broker.jobs[receipts[1]["job_id"]]["status"])
        self.broker.shutdown()
        self.broker = host.Broker(self.state, self.runner)
        self.assertEqual("cancelled", self.broker.jobs[receipts[0]["job_id"]]["status"])
        self.assertEqual("completed", self.wait(receipts[1])["status"])
        self.assertEqual(2, len([c for c in self.runner.calls if c[0] == "build"]))

    def test_cancel_after_stop_retains_uncertainty_and_rollback(self):
        built = self.wait(self.build())
        self.runner.stop_entered = threading.Event()
        self.runner.stop_release = threading.Event()
        job = self.deploy(built)
        self.assertTrue(self.runner.stop_entered.wait(5))
        self.assertEqual("deployment_stop", self.cancel(job)["job"]["cancellation"]["mode"])
        self.runner.stop_release.set()
        self.assertEqual("uncertain", self.wait(job)["status"])
        self.assertTrue((self.state / job["job_id"] / "rollback-images.tar").exists())
        self.assertEqual(1, len([c for c in self.runner.calls if c[0] == "stop"]))
        self.assertFalse(any(c[0] in ("rename", "start") for c in self.runner.calls))
        # Stopped replacements are created only before the first consumer stop.
        stop = next(i for i, c in enumerate(self.runner.calls) if c[0] == "stop")
        self.assertTrue(all(i < stop for i, c in enumerate(self.runner.calls) if c[0] == "create"))
        self.broker.shutdown()
        calls = list(self.runner.calls)
        self.broker = host.Broker(self.state, self.runner)
        self.assertEqual(calls, self.runner.calls)
        self.assertEqual("uncertain", self.broker.jobs[job["job_id"]]["status"])

    def test_cancel_before_deployment_effects(self):
        built = self.wait(self.build())
        self.runner.export_entered = threading.Event()
        self.runner.export_release = threading.Event()
        job = self.deploy(built)
        self.assertTrue(self.runner.export_entered.wait(5))
        self.cancel(job)
        self.runner.export_release.set()
        self.assertEqual("cancelled", self.wait(job)["status"])
        self.assertFalse(any(c[0] == "stop" for c in self.runner.calls))

    def test_batch_member_stops_entire_candidate(self):
        self.runner.block = threading.Event()
        parent = self.build()
        self.entered_build()
        with self.broker.lock:
            member = copy.deepcopy(self.broker.jobs[parent["job_id"]])
            member_id = str(host.uuid.uuid4())
            member.update(job_id=member_id, batch_parent=parent["job_id"])
            self.broker.jobs[member_id] = member
            self.broker.jobs[parent["job_id"]]["batch_members"] = [member_id]
            self.broker._save(member)
        result = self.cancel(member)
        self.assertEqual([parent["job_id"], member_id], result["affected_job_ids"])
        self.runner.block.set()
        self.assertEqual("cancelled", self.wait(parent)["status"])
        self.assertEqual("cancelled", self.broker.jobs[member_id]["status"])
        self.assertTrue(self.broker.jobs[member_id]["cancel_request"])

    def test_admission_hold_can_be_cancelled(self):
        job = self.wait(self.build())
        stored = self.broker.jobs[job["job_id"]]
        stored.update(status="needs_decision", effects_started=False)
        self.assertTrue(self.broker._view(stored)["cancellation"]["available"])
        self.assertEqual("cancelled", self.cancel(job)["disposition"])

    def test_running_cancel_request_is_not_replayed_on_restart(self):
        job = self.wait(self.build())
        stored = self.broker.jobs[job["job_id"]]
        stored.update(status="running", cancel_request={"request_id":"crash-stop", "requested_at":time.time()}, revoked=True)
        self.broker._save(stored)
        self.broker.shutdown()
        calls = list(self.runner.calls)
        self.broker = host.Broker(self.state, self.runner)
        self.assertEqual("cancelled", self.broker.jobs[job["job_id"]]["status"])
        self.assertEqual(calls, self.runner.calls)

    def test_stale_batch_confirmation_cannot_cancel_new_members(self):
        self.runner.block = threading.Event()
        parent = self.build()
        self.entered_build()
        with self.broker.lock:
            member_id = str(host.uuid.uuid4())
            member = copy.deepcopy(self.broker.jobs[parent["job_id"]])
            member.update(job_id=member_id, batch_parent=parent["job_id"])
            self.broker.jobs[member_id] = member
            self.broker.jobs[parent["job_id"]]["batch_members"] = [member_id]
        with self.assertRaisesRegex(ValueError, "batch changed"):
            self.call("v3_build_cancel", {"job_id": parent["job_id"], "request_id":"stale", "expected_job_ids":[parent["job_id"]]})
        self.assertFalse(self.broker.jobs[parent["job_id"]]["revoked"])
        self.runner.block.set()
        self.assertEqual("completed", self.wait(parent)["status"])

    def test_terminal_job_is_unchanged_and_unknown_job_is_missing(self):
        job = self.wait(self.build())
        before = copy.deepcopy(self.broker.jobs[job["job_id"]])
        self.assertEqual("completed", self.cancel(job)["disposition"])
        self.assertEqual(before, self.broker.jobs[job["job_id"]])
        self.assertIsNone(self.cancel({"job_id": str(host.uuid.uuid4())})["job"])


class ProcessCancelTests(unittest.TestCase):
    def test_job_signal_stops_process_and_keeps_other_thread_running(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = host.Runner(source=directory)
            event = threading.Event()
            started = threading.Event()
            error = []
            marker = Path(directory) / "started"
            code = "import os,time,pathlib; pathlib.Path(" + repr(str(marker)) + ").write_text(str(os.getpid())); time.sleep(60)"
            def run():
                runner.job_context.job = {"effects_started": False}
                runner.job_context.event = event
                started.set()
                try:
                    runner.command([sys.executable, "-u", "-c", code])
                except Exception as value:
                    error.append(value)
            thread = threading.Thread(target=run)
            thread.start()
            self.assertTrue(started.wait(1))
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists())
            event.set()
            thread.join(7)
            self.assertFalse(thread.is_alive())
            self.assertIsInstance(error[0], PermissionError)
            self.assertFalse(runner.cancel.is_set())
            pid = int(marker.read_text())
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)
            self.assertEqual("ok", runner.command([sys.executable, "-c", "print('ok')"]).strip())

    def test_private_inspect_cancellation_never_publishes_partial_raw_output(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = host.Runner(source=directory)
            event = threading.Event()
            marker = Path(directory) / "ready"
            error = []
            code = "import time,pathlib; print('UNLEARNED_PRIVATE_VALUE',flush=True); pathlib.Path(" + repr(str(marker)) + ").touch(); time.sleep(60)"
            def run():
                runner.job_context.job = {"effects_started": False}
                runner.job_context.event = event
                try:
                    runner.command([sys.executable, "-u", "-c", code], private_raw=True)
                except Exception as value:
                    error.append(value)
            thread = threading.Thread(target=run)
            thread.start()
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists())
            event.set()
            thread.join(7)
            self.assertFalse(thread.is_alive())
            self.assertIsInstance(error[0], PermissionError)
            self.assertEqual("", error[0].log)

    def test_after_effects_signal_does_not_interrupt_inflight_command(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = host.Runner(source=directory)
            runner.job_context.job = {"effects_started": True}
            runner.job_context.event = threading.Event()
            runner.job_context.event.set()
            self.assertEqual("done", runner.command([sys.executable, "-c", "print('done')"]).strip())


if __name__ == "__main__":
    unittest.main()
