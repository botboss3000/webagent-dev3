"""Root host file service: grants, locked folders, the checkout hand-off and the change log (no root needed)."""
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
        profile = {**files_module.DEFAULTS, "state": str(self.state), "app_uid": APP,
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
        self.ok("write", path=made + "/a.txt", content="a", encoding="utf-8", expected_sha256="")
        self.assertIn("exists", self.call("rename", path=notes, new_path=made + "/a.txt")["output"])
        self.ok("rename", path=made + "/a.txt", new_path=made + "/b.txt")
        self.ok("delete", path=made)
        self.assertFalse(os.path.exists(made))
        log = [json.loads(line) for line in (self.state / "files-log.jsonl").read_text().splitlines()]
        self.assertEqual(["write", "create", "write", "rename", "delete"], [entry["op"] for entry in log])
        self.assertTrue(all(entry["admin"] == "admin" for entry in log))

    def test_editor_limit_and_binary_files(self):
        self.grant()
        big = self.home / "projects" / "big.bin"
        big.write_bytes(b"\0" * (files_module.MAX_FILE + 1))
        self.assertIn("one MiB", self.call("read", path=str(big))["output"])
        small = self.home / "projects" / "small.bin"
        small.write_bytes(b"\xff\x00")
        self.assertEqual("base64", self.ok("read", path=str(small))["encoding"])


if __name__ == "__main__":
    unittest.main()
