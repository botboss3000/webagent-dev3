"""Root host helper: approvals, expiry, allow-all windows and caller checks (no root needed)."""
import importlib.util
import os
import tempfile
import unittest
from unittest import mock
from pathlib import Path

spec = importlib.util.spec_from_file_location("host_helper", Path(__file__).resolve().parents[1] / "build-service" / "host_helper.py")
helper_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper_module)
APP, OPERATOR, OTHER = 1100, os.getuid(), 4242


class HelperTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.now = 1000.0
        import pwd
        profile = {**helper_module.DEFAULTS, "state": self.temp.name, "app_uid": APP,
                   "operator_account": pwd.getpwuid(OPERATOR).pw_name}
        self.helper = helper_module.Helper(profile, clock=lambda: self.now,
                                           runner=lambda *a, **k: type("R", (), {"stdout": "up"})())

    def tearDown(self):
        self.temp.cleanup()

    def request(self, action="noop", uid=OPERATOR, session=None):
        return self.helper.handle(uid, {"op": "request", "action": action, "requester": {"identity": "t480", **({"session": session} if session else {})}})

    def test_write_waits_for_an_app_decision(self):
        record = self.request()
        self.assertEqual("pending", record["state"])
        with self.assertRaises(PermissionError):
            self.helper.handle(OPERATOR, {"op": "decide", "id": record["id"], "decision": "approve", "admin": "me"})
        done = self.helper.handle(APP, {"op": "decide", "id": record["id"], "decision": "approve", "admin": "admin"})
        self.assertEqual(("done", "admin"), (done["state"], done["decided_by"]))
        self.assertEqual("done", self.helper.handle(OPERATOR, {"op": "status", "id": record["id"]})["state"])

    def test_denied_and_expired_requests_never_run(self):
        denied = self.request()
        self.assertEqual("denied", self.helper.handle(APP, {"op": "decide", "id": denied["id"], "decision": "deny", "admin": "admin"})["state"])
        waiting = self.request()
        self.now += 10 ** 6
        self.assertEqual("pending", self.helper.handle(APP, {"op": "status", "id": waiting["id"]})["state"])  # no expiry by default
        self.helper.profile["request_ttl_seconds"] = 1800
        late = self.request()
        self.now += 1801
        self.assertEqual("expired", self.helper.handle(APP, {"op": "status", "id": late["id"]})["state"])
        with self.assertRaisesRegex(ValueError, "expired"):
            self.helper.handle(APP, {"op": "decide", "id": late["id"], "decision": "approve", "admin": "admin"})

    def test_reads_run_at_once_and_allow_all_covers_a_session_until_it_ends(self):
        self.assertEqual("done", self.request("host_status")["state"])
        self.helper.handle(APP, {"op": "allow", "session": "s1", "seconds": 600, "admin": "admin"})
        self.helper.handle(APP, {"op": "allow", "session": "ssh:t480", "seconds": 600, "admin": "admin"})
        app = lambda session: self.helper.handle(APP, {"op": "request", "action": "noop", "requester": {"session": session}})
        self.assertEqual("done", app("s1")["state"])
        self.assertEqual("pending", app("s2")["state"])
        self.assertEqual("done", self.request()["state"])  # SSH identity t480
        self.now += 601
        self.assertEqual("pending", app("s1")["state"])
        self.helper.handle(APP, {"op": "allow", "session": "*", "seconds": 600, "admin": "admin"})  # page-wide
        self.assertEqual(("done", "done"), (app("s9")["state"], self.request()["state"]))
        self.assertIn("*", self.helper.handle(APP, {"op": "list"})["windows"])
        with self.assertRaises(ValueError):
            self.helper.handle(APP, {"op": "allow", "session": "s1", "seconds": 86401, "admin": "admin"})

    def test_unknown_callers_and_actions_are_refused(self):
        with self.assertRaises(PermissionError):
            self.request(uid=OTHER)
        with self.assertRaises(PermissionError):
            self.helper.handle(OPERATOR, {"op": "list"})
        with self.assertRaises(ValueError):
            self.request("rm_rf")
        self.assertEqual([], self.helper.handle(APP, {"op": "list"})["pending"])

    def wait(self, request_id):
        import time
        for _ in range(200):
            record = self.helper.handle(APP, {"op": "status", "id": request_id})
            if record["state"] not in ("running", "approved"):
                return record
            time.sleep(.01)
        return record

    def approve(self, record, **more):
        self.helper.handle(APP, {"op": "decide", "id": record["id"], "decision": "approve", "admin": "admin", **more})
        return self.wait(record["id"])

    def test_file_write_snapshots_and_reverts_exactly(self):
        target = Path(self.temp.name) / "etc" / "app.conf"
        target.parent.mkdir()
        target.write_text("old\n")
        self.helper.profile["editable"] = [str(target.parent) + "/*"]
        write = self.helper.handle(APP, {"op": "request", "action": "file_write", "args": {"path": str(target), "content": "new\n"}, "requester": {"session": "s"}})
        self.assertIn("-old", write["preview"]); self.assertIn("+new", write["preview"])
        self.assertEqual("pending", write["state"])
        self.assertEqual("done", self.approve(write)["state"])
        self.assertEqual("new\n", target.read_text())
        revert = self.helper.handle(APP, {"op": "request", "action": "host_revert", "args": {"request_id": write["id"]}, "requester": {"session": "s"}})
        self.assertEqual("done", self.approve(revert)["state"])
        self.assertEqual("old\n", target.read_text())
        with self.assertRaises(ValueError):  # a write is reverted once
            self.helper.handle(APP, {"op": "request", "action": "host_revert", "args": {"request_id": write["id"]}, "requester": {"session": "s"}})

    def test_paths_outside_the_profile_and_boot_paths(self):
        with self.assertRaises(PermissionError):
            self.helper.handle(APP, {"op": "request", "action": "file_write", "args": {"path": "/etc/passwd", "content": "x"}, "requester": {"session": "s"}})
        boot = Path(self.temp.name) / "boot.cfg"
        self.helper.profile["boot_disk_paths"] = [str(boot)]
        self.helper.handle(APP, {"op": "allow", "session": "s", "seconds": 600, "admin": "admin"})
        record = self.helper.handle(APP, {"op": "request", "action": "file_write", "args": {"path": str(boot), "content": "x"}, "requester": {"session": "s"}})
        self.assertEqual(("pending", True), (record["state"], record["boot_disk"]))  # allow-all never covers boot/disk
        with self.assertRaisesRegex(PermissionError, "no host approval passphrase"):
            self.helper.handle(APP, {"op": "decide", "id": record["id"], "decision": "approve", "admin": "admin"})
        self.helper.set_passphrase("correct horse battery")
        with self.assertRaisesRegex(PermissionError, "wrong"):
            self.helper.handle(APP, {"op": "decide", "id": record["id"], "decision": "approve", "admin": "admin", "passphrase": "guess"})
        self.assertEqual("done", self.approve(record, passphrase="correct horse battery")["state"])

    def test_file_grants_need_the_passphrase_and_can_be_revoked(self):
        self.helper.handle(APP, {"op": "allow", "session": "*", "seconds": 600, "admin": "admin"})
        ask = lambda: self.helper.handle(APP, {"op": "request", "action": "files_grant", "args": {"admin": "alex"}, "requester": {"user": "alex"}})
        record = ask()
        self.assertEqual(("pending", True), (record["state"], record["passphrase"]))  # allow-all never covers it
        self.assertEqual(record["id"], ask()["id"])  # a second click finds the same request
        with self.assertRaises(ValueError):
            self.helper.handle(APP, {"op": "request", "action": "files_grant", "args": {"admin": "a/b"}, "requester": {"user": "a/b"}})
        for requester in ({"user": "alex", "session": "chat-1"}, {"user": "someone"}):  # agents and other admins
            with self.assertRaises(PermissionError):
                self.helper.handle(APP, {"op": "request", "action": "files_grant", "args": {"admin": "alex"}, "requester": requester})
        with self.assertRaises(PermissionError):
            self.helper.handle(OPERATOR, {"op": "request", "action": "files_grant", "args": {"admin": "alex"}, "requester": {"user": "alex"}})
        with self.assertRaisesRegex(PermissionError, "no host approval passphrase"):
            self.helper.handle(APP, {"op": "decide", "id": record["id"], "decision": "approve", "admin": "admin"})
        self.helper.set_passphrase("correct horse battery")
        with self.assertRaisesRegex(ValueError, "grant time"):
            self.approve(record, passphrase="correct horse battery", seconds=86401)
        self.assertEqual("done", self.approve(record, passphrase="correct horse battery")["state"])
        page = self.helper.handle(APP, {"op": "list"})
        self.assertEqual(self.now + 3600, page["file_grants"]["alex"])  # 1 h unless the approver sets a time
        terminal = self.helper.handle(APP, {"op": "request", "action": "terminal_grant", "args": {"admin": "alex"}, "requester": {"user": "alex"}})
        self.assertEqual("done", self.approve(terminal, passphrase="correct horse battery", seconds=7200)["state"])
        self.assertEqual(self.now + 7200, self.helper.handle(APP, {"op": "list"})["terminal_grants"]["alex"])
        self.assertTrue(self.helper.handle(APP, {"op": "files_revoke", "kind": "terminal", "admin": "alex", "by": "admin"})["revoked"])
        self.assertEqual({}, self.helper.handle(APP, {"op": "list"})["terminal_grants"])
        self.now += 3601
        self.assertEqual({}, self.helper.handle(APP, {"op": "list"})["file_grants"])  # expired
        self.now -= 3601
        with self.assertRaises(PermissionError):
            self.helper.handle(OPERATOR, {"op": "files_revoke", "admin": "alex", "by": "me"})
        self.assertTrue(self.helper.handle(APP, {"op": "files_revoke", "admin": "alex", "by": "admin"})["revoked"])
        self.assertEqual({}, self.helper.handle(APP, {"op": "list"})["file_grants"])

    def test_file_log_reads_newest_first(self):
        import json
        with (Path(self.temp.name) / "files-log.jsonl").open("w") as stream:
            for n in range(3):
                stream.write(json.dumps({"op": "write", "n": n}) + "\n")
        self.assertEqual([2, 1], [e["n"] for e in self.helper.file_log(2)])

    def test_wrong_passphrases_lock_for_fifteen_minutes(self):
        self.helper.set_passphrase("correct horse battery")
        for _ in range(5):
            with self.assertRaisesRegex(PermissionError, "wrong"):
                self.helper.check_passphrase("nope")
        with self.assertRaisesRegex(PermissionError, "too many"):
            self.helper.check_passphrase("correct horse battery")
        self.now += 901
        self.helper.check_passphrase("correct horse battery")
        with self.assertRaises(ValueError):
            self.helper.set_passphrase("short")

    def test_units_and_containers_come_from_the_profile(self):
        with self.assertRaises(ValueError):
            self.request("unit_restart")  # missing args refused first

    def test_unlisted_unit_is_refused(self):
        with self.assertRaises(PermissionError):
            self.helper.handle(APP, {"op": "request", "action": "unit_restart", "args": {"unit": "ssh.service"}, "requester": {"session": "s"}})

    def test_log_records_every_step(self):
        record = self.request()
        self.helper.handle(APP, {"op": "decide", "id": record["id"], "decision": "approve", "admin": "admin"})
        events = [line for line in (Path(self.temp.name) / "log.jsonl").read_text().splitlines()]
        self.assertEqual(3, len(events))



class SupervisorTests(unittest.TestCase):
    """broker_install keeps the last-known-good release and restores it when the health test fails."""
    TREE = "ab" * 32

    def setUp(self):
        import json, pwd
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.state_root = root / "jobs"
        (self.state_root / "coordination/trees" / self.TREE / "v3/build-service").mkdir(parents=True)
        (self.state_root / "job1.json").write_text(json.dumps({"status": "completed"}))
        self.config = root / "build-service.json"
        self.config.write_text(json.dumps({"service_dir": "/releases/old", "state_root": str(self.state_root), "instance": "development"}))
        (root / "units").mkdir()
        self.unit = root / "units" / "webagent-build-development.service"
        self.unit.write_text("ExecStart=/releases/old/bootstrap.py\n")
        self.commands, self.healthy = [], True
        profile = {**helper_module.DEFAULTS, "state": str(root / "host"), "app_uid": APP, "health_seconds": 120,
                   "operator_account": pwd.getpwuid(OPERATOR).pw_name, "build_config": str(self.config), "unit_dir": str(root / "units")}
        self.helper = helper_module.Helper(profile, clock=lambda: 1000.0, runner=self.fake)

    def tearDown(self):
        self.temp.cleanup()

    def fake(self, command, **_):
        import json
        self.commands.append(command)
        out = ""
        if "install" in command:  # the installer repoints config and unit at the new release
            self.config.write_text(self.config.read_text().replace("/releases/old", "/releases/new"))
            self.unit.write_text("ExecStart=/releases/new/bootstrap.py\n")
            out = json.dumps({"installed": True, "version": "new"})
        if command[:2] == ["systemctl", "is-active"]:
            out = "active" if self.healthy else "failed"
        return type("R", (), {"returncode": 0, "stdout": out, "stderr": ""})()

    def install(self):
        request = self.helper.handle(APP, {"op": "request", "action": "broker_install", "requester": {"session": "s"},
                                           "args": {"task_id": "t-1", "tree_hash": self.TREE}})
        self.assertIn("old", request["preview"])
        run = lambda command, timeout=30: self.fake(command).stdout
        return request, lambda: self.helper.install_broker(request["args"], request["id"], run)

    def test_a_running_production_release_counts_as_busy(self):
        import json
        (self.state_root / "production").mkdir()
        record = self.state_root / "production" / "f7749ee1-5023-45eb-afec-79b2ca271d05.json"
        record.write_text(json.dumps({"status": "running"}))
        with self.assertRaisesRegex(RuntimeError, "production f7749ee1"):
            self.helper.require_idle()
        record.write_text(json.dumps({"status": "published"}))
        self.helper.require_idle()

    def test_healthy_install_keeps_the_new_release(self):
        request, install = self.install()
        result = install()
        self.assertEqual(("new", "/releases/old", True), (result["installed_release"], result["previous_release"], result["health"]["healthy"]))
        self.assertIn("/releases/new", self.config.read_text())
        # lifecycle.py install leaves the unit stopped; the helper starts it before the health test.
        self.assertIn(["systemctl", "start", "webagent-build-development.service"], self.commands)
        # Drained only once approved, resumed after the install.
        maintenance = [c[c.index("maintenance") + 1] for c in self.commands if "maintenance" in c]
        self.assertEqual(["drain", "resume"], maintenance)
        self.assertEqual("runuser", self.commands[0][0])

    def test_failed_health_test_restores_the_previous_release(self):
        self.healthy = False
        self.helper.profile["health_seconds"] = 0
        request, install = self.install()
        with self.assertRaisesRegex(RuntimeError, "restored release"):
            install()
        self.assertIn("/releases/old", self.config.read_text())
        self.assertIn("/releases/old", self.unit.read_text())
        self.assertIn(["systemctl", "restart", "webagent-build-development.service"], self.commands)
        # The broker and buildctl read the restored config as non-root users.
        self.assertEqual(0o644, self.config.stat().st_mode & 0o777)
        # A failed install still resumes the broker.
        self.assertIn("resume", self.commands[-1])

    def test_busy_broker_and_bad_trees_are_refused(self):
        import json
        with self.assertRaisesRegex(ValueError, "tree"):
            self.helper.handle(APP, {"op": "request", "action": "broker_install", "requester": {"session": "s"},
                                     "args": {"task_id": "t-1", "tree_hash": "cd" * 32}})
        (self.state_root / "job2.json").write_text(json.dumps({"status": "running"}))
        self.helper.profile["drain_seconds"] = 0
        request, install = self.install()
        with self.assertRaisesRegex(RuntimeError, "busy"):
            install()
        self.assertEqual("resume", self.commands[-1][self.commands[-1].index("maintenance") + 1])
        self.assertFalse(any("install" in c for c in self.commands))

    def test_install_waits_for_a_running_job_to_end(self):
        import json
        job = self.state_root / "job2.json"
        job.write_text(json.dumps({"status": "running"}))
        ticks = iter(range(1000, 10**6, 5))
        self.helper.clock = lambda: next(ticks)
        sleeps = []
        def sleep(_):
            sleeps.append(1)
            job.write_text(json.dumps({"status": "completed"}))
        request, install = self.install()
        with mock.patch.object(helper_module.time, "sleep", sleep):
            result = install()
        self.assertEqual((1, "new"), (len(sleeps), result["installed_release"]))


if __name__ == "__main__":
    unittest.main()
