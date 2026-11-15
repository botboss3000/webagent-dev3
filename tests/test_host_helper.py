"""Root host helper: approvals, expiry, allow-all windows and caller checks (no root needed)."""
import importlib.util
import os
import tempfile
import unittest
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


if __name__ == "__main__":
    unittest.main()
