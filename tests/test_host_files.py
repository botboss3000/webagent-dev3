"""Root host file service: grants, locked folders, the checkout hand-off and the change log (no root needed)."""
import base64
import hashlib
import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("host_files", Path(__file__).resolve().parents[1] / "build-service" / "host_files.py")
files_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(files_module)
APP, OTHER = 1100, 4242


class FilesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.root = root
        self.state = root / "host-state"
        self.state.mkdir()
        self.home = root / "home"
        for name in ("code", "state", "runtime", "projects"):
            (self.home / name).mkdir(parents=True)
        (self.home / "state" / "encryption.key").write_text("secret")
        (self.home / "projects" / "notes.txt").write_text("hello\n")
        self.now = 1000.0
        import pwd
        from unittest import mock
        # File operations run as the operator account; here the test user, setgroups stubbed (no root).
        patch = mock.patch.object(files_module.os, "setgroups", lambda groups: None)
        patch.start()
        self.addCleanup(patch.stop)
        profile = {**files_module.DEFAULTS, "state": str(self.state), "app_uid": APP,
                   "operator_account": pwd.getpwuid(os.getuid()).pw_name,
                   "files_checkout": str(self.home / "code"),
                   "files_locked": [str(self.home / "state"), str(self.home / "runtime")]}
        self.files = files_module.Files(profile, clock=lambda: self.now)

    def tearDown(self):
        self.temp.cleanup()

    def grant(self, admin="admin", until=None):
        (self.state / "file-grants.json").write_text(json.dumps({admin: until or self.now + 3600}))

    def call(self, op, uid=APP, admin="admin", **args):
        return files_module.reply_for(self.files, uid, {"op": op, "admin": admin, **args})

    def ok(self, op, **args):
        reply = self.call(op, **args)
        self.assertEqual("success", reply["status"], reply)
        return reply["output"]

    def test_only_the_app_uid_is_served(self):
        self.grant()
        self.assertEqual("failed", self.call("tree", uid=OTHER, path=str(self.home))["status"])

    def test_locate_needs_no_grant(self):
        self.assertEqual(str(self.home), self.ok("locate")["parent"])

    def test_no_grant_expired_grant_or_other_admin_asks_for_authorization(self):
        reply = self.call("tree", path=str(self.home))
        self.assertEqual("authorization_required", reply.get("code"))
        self.grant(until=self.now - 1)
        self.assertEqual("authorization_required", self.call("tree", path=str(self.home)).get("code"))
        self.grant(admin="someone-else")
        self.assertEqual("authorization_required", self.call("tree", path=str(self.home)).get("code"))

    def test_tree_shows_locked_folders_and_maps_the_checkout(self):
        self.grant()
        tree = self.ok("tree", path=str(self.home))
        entries = {e["name"]: e for e in tree["entries"]}
        self.assertTrue(entries["state"].get("locked"))
        self.assertTrue(entries["runtime"].get("locked"))
        self.assertEqual("/live-checkout", entries["code"]["path"])
        self.assertNotIn("locked", entries["projects"])
        self.assertEqual(str(self.home.parent), tree["parent"])

    def test_locked_folders_and_the_checkout_cannot_be_read_or_changed(self):
        self.grant()
        key = str(self.home / "state" / "encryption.key")
        self.assertIn("Locked", self.call("read", path=key)["output"])
        self.assertIn("Locked", self.call("tree", path=str(self.home / "state"))["output"])
        self.assertIn("Locked", self.call("write", path=key, content="x", encoding="utf-8", expected_sha256="")["output"])
        self.assertIn("checkout", self.call("create", path=str(self.home / "code" / "x"), kind="file")["output"])
        # A symlink does not open a way into a locked folder.
        (self.home / "projects" / "link").symlink_to(self.home / "state")
        self.assertIn("Locked", self.call("read", path=str(self.home / "projects" / "link" / "encryption.key"))["output"])
        # Folders holding a locked folder cannot be deleted or moved.
        self.assertIn("locked", self.call("delete", path=str(self.home))["output"])
        self.assertIn("locked", self.call("rename", path=str(self.home), new_path=str(self.root / "moved"))["output"])
        self.assertTrue((self.home / "state" / "encryption.key").exists())

    def test_read_write_create_rename_delete_and_log(self):
        self.grant()
        notes = str(self.home / "projects" / "notes.txt")
        read = self.ok("read", path=notes)
        self.assertEqual("hello\n", read["content"])
        stale = self.call("write", path=notes, content="x", encoding="utf-8", expected_sha256="0" * 64)
        self.assertIn("changed", stale["output"])
        os.chmod(notes, 0o640)
        saved = self.ok("write", path=notes, content="bye\n", encoding="utf-8", expected_sha256=read["sha256"])
        self.assertEqual(hashlib.sha256(b"bye\n").hexdigest(), saved["sha256"])
        self.assertEqual(0o640, os.stat(notes).st_mode & 0o777)  # edits keep the mode
        made = str(self.home / "projects" / "new")
        self.ok("create", path=made, kind="dir")
        self.assertEqual(0o700, os.stat(made).st_mode & 0o777)  # new folders 700, files 600
        self.ok("write", path=made + "/a.txt", content="a", encoding="utf-8", expected_sha256="")
        self.assertEqual((os.getuid(), 0o600), (os.stat(made + "/a.txt").st_uid, os.stat(made + "/a.txt").st_mode & 0o777))
        self.assertIn("exists", self.call("rename", path=notes, new_path=made + "/a.txt")["output"])
        self.ok("rename", path=made + "/a.txt", new_path=made + "/b.txt")
        self.ok("delete", path=made)
        self.assertFalse(os.path.exists(made))
        log = [json.loads(line) for line in (self.state / "files-log.jsonl").read_text().splitlines()]
        self.assertEqual(["write", "create", "write", "rename", "delete"], [entry["op"] for entry in log])
        self.assertTrue(all(entry["admin"] == "admin" for entry in log))

    def test_operations_run_as_the_operator_account(self):
        from unittest import mock
        self.grant()
        seen = []
        with mock.patch.object(files_module.os, "setuid", lambda uid: seen.append(uid)):
            self.ok("tree", path=str(self.home))
        self.assertEqual([], seen)  # the change happened in the child, not in this process
        private = self.home / "projects" / "private.txt"
        private.write_text("x")
        os.chmod(private, 0)
        if os.geteuid() != 0:
            self.assertIn("Permission denied", self.call("read", path=str(private))["output"])

    def test_editor_limit_and_binary_files(self):
        self.grant()
        big = self.home / "projects" / "big.bin"
        big.write_bytes(b"\0" * (files_module.MAX_FILE + 1))
        self.assertIn("one MiB", self.call("read", path=str(big))["output"])
        small = self.home / "projects" / "small.bin"
        small.write_bytes(b"\xff\x00")
        self.assertEqual("base64", self.ok("read", path=str(small))["encoding"])


class TerminalTests(unittest.TestCase):
    """Shells run as the operator account; here the test user, with setgroups stubbed (no root)."""

    def setUp(self):
        import pwd
        from unittest import mock
        self.temp = tempfile.TemporaryDirectory()
        self.state = Path(self.temp.name)
        self.now = 1000.0
        profile = {**files_module.DEFAULTS, "state": str(self.state), "app_uid": APP,
                   "operator_account": pwd.getpwuid(os.getuid()).pw_name, "terminal_max": 2}
        self.files = files_module.Files(profile, clock=lambda: self.now)
        patch = mock.patch.object(files_module.os, "setgroups", lambda groups: None)
        patch.start()
        self.addCleanup(patch.stop)

    def tearDown(self):
        for terminal in list(self.files.terminals.values()):
            terminal.close()
        self.temp.cleanup()

    def call(self, op, admin="admin", **args):
        return files_module.reply_for(self.files, APP, {"op": op, "admin": admin, **args})

    def grant(self, admin="admin"):
        (self.state / "terminal-grants.json").write_text(json.dumps({admin: self.now + 3600}))

    def test_terminals_need_their_own_grant(self):
        (self.state / "file-grants.json").write_text(json.dumps({"admin": self.now + 3600}))
        self.assertEqual("authorization_required", self.call("terminal_open").get("code"))

    def test_open_send_read_close_and_log(self):
        self.grant()
        opened = self.call("terminal_open", cwd=self.temp.name, cols=80, rows=24)["output"]
        self.assertEqual(os.path.realpath(self.temp.name), opened["cwd"])
        self.call("terminal_send", id=opened["id"], data="echo from-$((40+2))\n")
        seen, offset = b"", 0
        for _ in range(20):
            read = self.call("terminal_read", id=opened["id"], offset=offset, wait=1)["output"]
            seen += base64.b64decode(read["data"])
            offset = read["offset"]
            if b"from-42" in seen:
                break
        self.assertIn(b"from-42", seen)
        (self.state / "terminal-grants.json").write_text(json.dumps({"admin": self.now + 3600, "other": self.now + 3600}))
        self.assertEqual("No such terminal", self.call("terminal_read", admin="other", id=opened["id"])["output"])
        self.assertEqual(1, len(self.call("terminal_list")["output"]["terminals"]))
        self.call("terminal_open")
        self.assertIn("At most 2", self.call("terminal_open")["output"])
        self.call("terminal_close", id=opened["id"])
        ops = [json.loads(line)["op"] for line in (self.state / "files-log.jsonl").read_text().splitlines()]
        self.assertEqual(["terminal_open", "terminal_open", "terminal_close"], ops)

    def test_reaper_closes_terminals_when_the_grant_ends(self):
        self.grant()
        opened = self.call("terminal_open")["output"]
        self.now += 3601
        self.files.reap()
        self.assertEqual({}, self.files.terminals)
        log = [json.loads(line) for line in (self.state / "files-log.jsonl").read_text().splitlines()]
        self.assertEqual(("terminal_close", "grant ended", opened["id"]), (log[-1]["op"], log[-1]["reason"], log[-1]["terminal"]))


if __name__ == "__main__":
    unittest.main()
