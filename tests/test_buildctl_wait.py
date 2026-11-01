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


class OperatorCycleTests(unittest.TestCase):
    """Server-side operator release steps: install, close, heartbeat and the stage table."""
    def setUp(self):
        import tempfile
        self.folder = tempfile.TemporaryDirectory();root = Path(self.folder.name)
        self.tree = root / "trees" / ("a" * 64);(self.tree / "v3/src").mkdir(parents=True)
        self.checkout = root / "checkout";(self.checkout / "src").mkdir(parents=True)
        import tarfile
        with tarfile.open(self.tree / "baseline.tar", "w") as archive:
            for name, body in {"v3/src/same.rs": b"same", "v3/src/edit.rs": b"old", "v3/src/gone.rs": b"x", "ui/x.css": b"ui"}.items():
                info = tarfile.TarInfo(name);info.size = len(body);archive.addfile(info, io.BytesIO(body))
        for name, body in {"src/same.rs": b"same", "src/edit.rs": b"new", "src/added.rs": b"add"}.items():
            (self.tree / "v3" / name).write_bytes(body);(self.checkout / name).write_bytes(body)
        self.calls = []
    def tearDown(self):self.folder.cleanup()
    def broker(self, job_status="completed", busy=False, refusal=None, state="complete"):
        def request(name, args):
            self.calls.append((name, args))
            if name == "v3_build_work" and args["action"] == "status":
                return {"own": [{"task_id": "task-1", "job_id": "job-1", "workspace_path": str(self.tree / "v3")}]}
            if name == "v3_build_work" and args["action"] == "preflight":
                return {"release_plan": {"refusal": refusal}, "synchronization": {"affected": 0}}
            if name == "v3_worker_job":return {"status": job_status}
            if name == "v3_build_status":return {"busy": busy, "containers": {r: {"health": "healthy"} for r in ("api", "loop")}}
            if name == "build_service_maintenance":return {"task_id": None}
            if args.get("action") == "reservations":
                held = not any(isinstance(a, dict) and a.get("action") == "release_claims" for _, a in self.calls)
                return {"tasks": [{"label": "task-1", "scopes": ["module:broker"], "revision": 7, "epoch": 1}] if held else []}
            if args.get("action") == "release_claims":return {"state": state, "scopes": []}
            raise AssertionError(name)
        return request
    def runner(self, fail=()):
        from types import SimpleNamespace
        def run(argv, **kwargs):
            self.calls.append(("run", argv))
            if any(word in argv for word in fail):return SimpleNamespace(returncode=1, stdout="", stderr="refused stale tree")
            return SimpleNamespace(returncode=0, stdout="abc123\n" if "rev-parse" in argv else "", stderr="")
        return run
    def test_task_changes_match_the_broker_definition(self):
        self.assertEqual(client.task_changes(self.tree), ["src/added.rs", "src/edit.rs", "src/gone.rs"])
    def test_close_commits_exact_paths_then_releases_claims_with_the_commit(self):
        with patch.dict(client.CONFIG, {"checkout_root": str(self.checkout)}), patch.object(client, "heartbeat") as beat:
            result = client.close("task-1", "Change", self.broker(), self.runner())
        add = next(argv for name, argv in self.calls if name == "run" and "add" in argv)
        self.assertEqual(add[-3:], ["src/added.rs", "src/edit.rs", "src/gone.rs"])
        release = next(args for name, args in self.calls if args and isinstance(args, dict) and args.get("action") == "release_claims")
        self.assertEqual((release["commit"], release["scopes"], release["expected_revision"]), ("abc123", ["module:broker"], 7))
        self.assertEqual(result["state"], "complete");beat.assert_called_once_with("task-1", "stop")
    def test_close_refuses_unfinished_job_or_drifted_checkout(self):
        with patch.dict(client.CONFIG, {"checkout_root": str(self.checkout)}):
            with self.assertRaisesRegex(ValueError, "reconcile"):client.close("task-1", "m", self.broker("failed"), self.runner())
            (self.checkout / "src/edit.rs").write_bytes(b"other writer")
            with self.assertRaisesRegex(ValueError, "differs"):client.close("task-1", "m", self.broker(), self.runner())
        self.assertFalse(any(name == "run" and "commit" in argv for name, argv in self.calls))
    def test_install_restarts_the_service_even_when_install_is_refused(self):
        with patch.dict("os.environ", {"WEBAGENT_BUILD_CONFIG": "/etc/fixture.json"}):
            with self.assertRaisesRegex(RuntimeError, "stale"):client.install_broker("task-1", False, self.broker(), self.runner(fail=("install",)))
            commands = [argv[3] if argv[:3] == ["sudo", "-n", "systemctl"] else "install" for name, argv in self.calls if name == "run" and "is-active" not in argv]
            self.assertEqual(commands, ["stop", "install", "start"])
            self.calls.clear()
            with self.assertRaisesRegex(ValueError, "busy"):client.install_broker("task-1", False, self.broker(busy=True), self.runner())
            with self.assertRaisesRegex(ValueError, "refused"):client.install_broker("task-1", False, self.broker(refusal="Possible credentials"), self.runner())
        self.assertFalse(any(name in ("run", "build_service_maintenance") for name, _ in self.calls))
    def test_stage_table_and_watch_step(self):
        running = {"status": "running", "stage": "compile tests", "stage_index": 2, "stage_count": 3, "job_seconds": 90, "job_normal": [60, 120],
                   "finished_stages": [{"stage": "dependencies", "seconds": 6, "normal": [6, 12]}],
                   "running_stages": [{"stage": "compile tests", "seconds": 84, "normal": None}],
                   "upcoming_stages": [{"stage": "run tests", "normal": [300, 600]}]}
        table = client.stage_table(running)
        self.assertIn("| 1. Dependencies ✅ | 0.1m (0.1-0.2m) |", table)
        self.assertIn("| 2. Compile tests ⏳ | 1.4m (no baseline) |", table)
        self.assertIn("| 3. Run tests | TBD (5.0-10.0m) |", table)
        later = {**running, "stage": "run tests", "finished_stages": running["finished_stages"] * 2}
        with patch("sys.stdout", new_callable=io.StringIO):
            replies = iter([running, running, later])
            self.assertEqual(client.watch("job-1", True, lambda n, a: next(replies), lambda s: None), 3)
            self.assertEqual(client.watch("job-1", False, lambda n, a: {**running, "status": "completed"}, lambda s: None), 0)
