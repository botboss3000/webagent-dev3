"""Read-only shared build result polling for the Codex client."""
import importlib.util
import unittest
import io
from unittest.mock import patch
from pathlib import Path

PATH = Path(__file__).resolve().parents[1] / "build-service/buildctl.py"
spec = importlib.util.spec_from_file_location("buildctl", PATH)
client = importlib.util.module_from_spec(spec)
spec.loader.exec_module(client)


class FakeBroker:
    def __init__(self, receipts, jobs=(), healthy=True):
        self.receipts = list(receipts)
        self.jobs = list(jobs)
        self.healthy = healthy
        self.calls = []

    def __call__(self, name, arguments):
        self.calls.append((name, arguments))
        if name == "v3_build_work":
            return {"own": [self.receipts.pop(0) if len(self.receipts) > 1 else self.receipts[0]]}
        if name == "v3_worker_job":
            value = self.jobs.pop(0) if len(self.jobs) > 1 else self.jobs[0]
            if isinstance(value, Exception):
                raise value
            return value
        if name == "v3_build_status":
            health = "healthy" if self.healthy else "unhealthy"
            return {"containers": {role: {"state": "running", "health": health}
                                   for role in ("api", "loop")}}
        raise AssertionError("polling attempted to create or deploy work")


def receipt(job_id=None):
    return {"task_id": "task-1", "status": "closed", "job_id": job_id}


def job(status="completed", phase="finished", reason="inputs unchanged", log=""):
    return {"job_id": "job-1", "status": status, "phase": phase,
            "reason": reason, "log": log}


class WaitTests(unittest.TestCase):
    def assert_read_only(self, broker):
        self.assertTrue(all(name in {"v3_build_work", "v3_worker_job", "v3_build_status"}
                            for name, _ in broker.calls))
        self.assertTrue(all(args == {"action": "status"} for name, args in broker.calls
                            if name == "v3_build_work"))

    def test_cancel_cli_targets_one_job_with_stable_identity(self):
        with patch.object(client, "call", side_effect=[{"cancellation":{"affected_job_ids":["job-1"]}}, {"job":{"status":"cancelled"}}]) as request, patch("sys.argv", ["buildctl", "cancel", "job-1", "--request-id", "cancel-one"]), patch("sys.stdout", new_callable=io.StringIO):
            self.assertEqual(0, client.main())
        self.assertEqual(2, request.call_count)
        self.assertEqual(("v3_build_cancel", {"job_id":"job-1", "request_id":"cancel-one", "expected_job_ids":["job-1"]}), request.call_args.args)

    def test_cli_requires_explicit_shared_batch_stop(self):
        with patch.object(client, "call", return_value={"cancellation":{"affected_job_ids":["job-1","peer"]}}) as request, patch("sys.argv", ["buildctl", "cancel", "job-1"]):
            with self.assertRaisesRegex(ValueError, "include-batch"):
                client.main()
        self.assertEqual(1, request.call_count)

    def test_early_finisher_discovers_shared_job_on_next_turn(self):
        broker = FakeBroker([receipt(), receipt("job-1")], [job()])
        pending, code = client.wait_task("task-1", 1800, request=broker)
        self.assertEqual(3, code)
        self.assertEqual("waiting for other registered tasks", pending["reason"])
        self.assertEqual(["v3_build_work"], [name for name, _ in broker.calls])
        complete, code = client.wait_task("task-1", 0, request=broker)
        self.assertEqual(0, code)
        self.assertEqual(("job-1", "completed", "finished"),
                         (complete["job_id"], complete["status"], complete["phase"]))
        self.assertEqual("success", complete["result"])
        self.assert_read_only(broker)

    def test_paused_job_returns_decision_report_without_submitting(self):
        report={"total":1,"digest":"f"*64,"choices":["synchronize","proceed","cancel"]}
        broker=FakeBroker([receipt("job-1")],[job("needs_decision","checking_checkout") | {"synchronization":report}])
        result,code=client.wait_task("task-1",30,request=broker)
        self.assertEqual(client.DECISION_EXIT,code)
        self.assertEqual("needs_decision",result["result"])
        self.assertEqual(report,result["synchronization"])
        self.assert_read_only(broker)

    def test_last_finisher_waits_for_terminal_success(self):
        broker = FakeBroker([receipt("job-1")],
                            [job("queued", "queued"), job("running", "building"), job()])
        now = [0]
        delays = []

        def clock():
            return now[0]

        def pause(seconds):
            delays.append(seconds)
            now[0] += seconds

        result, code = client.wait_task("task-1", 30, request=broker, clock=clock, pause=pause)
        self.assertEqual(0, code)
        self.assertEqual([15, 15], delays)
        self.assertEqual("success", result["result"])
        self.assert_read_only(broker)

    def test_failed_job_is_terminal_and_excerpt_is_bounded(self):
        broker = FakeBroker([receipt("job-1")],
                            [job("failed", "building", "error", "x" * 2000)])
        result, code = client.wait_task("task-1", 0, request=broker)
        self.assertEqual(1, code)
        self.assertEqual(("failed", "building"), (result["status"], result["phase"]))
        self.assertLessEqual(len(result["log_excerpt"].encode()), 1024)
        self.assertNotIn("v3_build_status", [name for name, _ in broker.calls])
        self.assert_read_only(broker)

    def test_disconnect_then_resume_from_same_task(self):
        broker = FakeBroker([receipt("job-1")],
                            [ConnectionError("socket closed"), job()])
        pending, code = client.wait_task("task-1", 0, request=broker)
        self.assertEqual((3, "still_pending"), (code, pending["result"]))
        result, code = client.wait_task("task-1", 0, request=broker)
        self.assertEqual((0, "success"), (code, result["result"]))
        self.assert_read_only(broker)

    def test_timeout_and_duplicate_polling_do_not_schedule(self):
        broker = FakeBroker([receipt("job-1")], [job("running", "building")])
        for _ in range(2):
            result, code = client.wait_task("task-1", 0, request=broker)
            self.assertEqual((3, "still_pending"), (code, result["result"]))
            self.assertEqual("job-1", result["job_id"])
        self.assert_read_only(broker)

    def test_completed_job_requires_both_healthy(self):
        broker = FakeBroker([receipt("job-1")], [job()], healthy=False)
        result, code = client.wait_task("task-1", 0, request=broker)
        self.assertEqual((1, "failed"), (code, result["result"]))
        self.assert_read_only(broker)


if __name__ == "__main__":
    unittest.main()


class FeatureProfileCLITests(unittest.TestCase):
    def test_check_forwards_only_bounded_profile_and_receipt_arguments(self):
        argv=["buildctl","check","fixture-task","--expected","a"*64,"--request-id","fixture-request","--feature-profile","live"]
        with patch("sys.argv",argv),patch.object(client,"call",return_value={"status":"queued"})as call,patch("sys.stdout",new=io.StringIO()):
            self.assertEqual(client.main(),0)
        self.assertEqual(call.call_args.args[0],"build_work")
        self.assertEqual(call.call_args.args[1]["feature_profile"],"live")
        self.assertEqual(call.call_args.args[1]["request_id"],"fixture-request")
    def test_cli_rejects_unsupported_profile_before_contacting_broker(self):
        argv=["buildctl","check","fixture-task","--expected","a"*64,"--request-id","fixture-request","--feature-profile","all"]
        with patch("sys.argv",argv),patch.object(client,"call")as call,patch("sys.stderr",new=io.StringIO()):
            with self.assertRaises(SystemExit):client.main()
        call.assert_not_called()


class CursorCliTests(unittest.TestCase):
    def test_job_cursor_is_forwarded_without_mutating_work(self):
        with patch.object(client,"call",return_value={}) as call, patch("sys.argv",["buildctl","job","job-1","--log-cursor","42"]), patch("sys.stdout",new_callable=io.StringIO):
            self.assertEqual(client.main(),0)
        call.assert_called_once_with("v3_worker_job",{"job_id":"job-1","log_cursor":42})
