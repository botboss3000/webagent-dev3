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
        self.assertEqual("done", self.request(session="s1")["state"])
        self.assertEqual("pending", self.request(session="s2")["state"])
        self.now += 601
        self.assertEqual("pending", self.request(session="s1")["state"])
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

    def test_log_records_every_step(self):
        record = self.request()
        self.helper.handle(APP, {"op": "decide", "id": record["id"], "decision": "approve", "admin": "admin"})
        events = [line for line in (Path(self.temp.name) / "log.jsonl").read_text().splitlines()]
        self.assertEqual(3, len(events))


if __name__ == "__main__":
    unittest.main()
