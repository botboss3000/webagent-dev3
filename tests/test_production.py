"""Production release requests, approval rules, export and version stamping."""
import importlib.util
import io
import json
import subprocess
import tarfile
import tempfile
import types
import unittest
from pathlib import Path

MODULE = Path(__file__).resolve().parents[1] / "build-service/production.py"
spec = importlib.util.spec_from_file_location("production_under_test", MODULE)
production = importlib.util.module_from_spec(spec)
spec.loader.exec_module(production)

ADMIN = {"user_id": "admin", "session_id": "s", "administrator": True}
AGENT = {"user_id": "admin", "session_id": "s", "administrator": True, "session_name": "Chat"}
LOCAL = {"user_id": "t480", "session_id": "s", "local_caller": True}
OTHER = {"user_id": "someone", "session_id": "s"}


def tar_of(files):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w") as archive:
        for name, body in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(body)
            archive.addfile(info, io.BytesIO(body))
    return stream.getvalue()


class FakeGit:
    """Answers the git calls the module makes against the dev checkout."""

    def __init__(self, files):
        self.files, self.calls = files, []

    def __call__(self, argv, **options):
        self.calls.append(argv)
        command = argv[3]
        if command == "rev-parse":
            return types.SimpleNamespace(returncode=0, stdout="abc123\n", stderr="")
        if command == "ls-tree":
            top = sorted({name.split("/")[0] for name in self.files})
            return types.SimpleNamespace(returncode=0, stdout="\n".join(top) + "\n", stderr="")
        if command == "archive":
            wanted = argv[6:]
            chosen = {n: b for n, b in self.files.items() if n.split("/")[0] in wanted}
            options["stdout"].write(tar_of(chosen))
            return types.SimpleNamespace(returncode=0, stdout="", stderr=b"")
        return types.SimpleNamespace(returncode=0, stdout="", stderr="")


CARGO = b'[package]\nname = "webagent-v3"\nversion = "0.0.0"\nedition = "2021"\n'
LOCK = b'[[package]]\nname = "webagent-v3"\nversion = "0.0.0"\n'
FILES = {
    "Cargo.toml": CARGO, "Cargo.lock": LOCK, "src/main.rs": b"fn main() {}\n",
    "abilities/Core/base/base.json": b"{}", "abilities/Administrator/host_admin/host_admin.json": b"{}",
    "abilities/Administrator/codebase_admin/codebase_admin.json": b"{}",
    "ui/main-panel/builds/builds.js": b"", "ui/main-panel/chat/chat.js": b"",
    "build-service/host.py": b"", "tests/test_x.py": b"", "AGENTS.md": b"",
}


class ProductionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.git = FakeGit(FILES)
        release = types.SimpleNamespace(possible_credentials=lambda body: b"secret-token" in body)
        self.production = production.Production(None, self.temp.name, "/checkout", "rust:pinned",
                                                release, runner=self.git)

    def tearDown(self):
        self.temp.cleanup()

    def test_versions_start_at_0_1_0_and_bump_semantically(self):
        self.assertEqual(production.bump(None, "major"), "0.1.0")
        self.assertEqual(production.bump("0.1.0", "patch"), "0.1.1")
        self.assertEqual(production.bump("0.1.4", "minor"), "0.2.0")
        self.assertEqual(production.bump("0.2.3", "major"), "1.0.0")

    def test_agents_request_but_only_the_app_administrator_decides(self):
        made = self.production.handle("build_work", AGENT, {"action": "production_request", "notes": "first",
                                                                "broker_id": "b1", "request_id": "r1"})
        self.assertEqual((made["status"], made["version"], made["commit"]), ("awaiting_approval", "0.1.0", "abc123"))
        self.assertEqual(made["requested_by"], "Chat")
        with self.assertRaises(ValueError):
            self.production.handle("v3_production", LOCAL, {"action": "request"})
        with self.assertRaises(PermissionError):
            self.production.handle("v3_production", OTHER, {"action": "list"})
        decision = {"release_id": made["id"], "decision": "deny"}
        with self.assertRaises(PermissionError):
            self.production.handle("v3_production_decide", LOCAL, decision)
        with self.assertRaises(PermissionError):
            self.production.handle("v3_production_decide", {**LOCAL, "administrator": True}, decision)
        denied = self.production.handle("v3_production_decide", ADMIN, decision)
        self.assertEqual(denied["status"], "denied")
        with self.assertRaises(ValueError):
            self.production.handle("v3_production_decide", ADMIN, decision)
        listed = self.production.handle("v3_production", LOCAL, {"action": "list"})
        self.assertEqual([r["status"] for r in listed["releases"]], ["denied"])
        self.assertIsNone(listed["published_version"])

    def test_export_keeps_only_production_files_and_stamps_the_version(self):
        tree = Path(self.temp.name) / "x/tree"
        self.production.export("abc123", tree, "0.1.0")
        names = sorted(str(p.relative_to(tree)) for p in tree.rglob("*") if p.is_file())
        self.assertEqual(names, ["Cargo.lock", "Cargo.toml", "abilities/Administrator/codebase_admin/codebase_admin.json",
                                 "abilities/Core/base/base.json", "src/main.rs", "ui/main-panel/chat/chat.js"])
        self.assertIn('version = "0.1.0"', (tree / "Cargo.toml").read_text())
        self.assertIn('version = "0.1.0"', (tree / "Cargo.lock").read_text())

    def test_credential_guard_stops_the_release(self):
        tree = Path(self.temp.name) / "tree"
        (tree / "src").mkdir(parents=True)
        (tree / "src/a.rs").write_bytes(b"let x = secret-token;")
        with self.assertRaisesRegex(ValueError, "src/a.rs"):
            self.production.guard(tree)

    def test_a_release_running_at_restart_is_marked_interrupted(self):
        made = self.production.handle("v3_production", LOCAL, {"action": "request"})
        record = self.production.load(made["id"])
        record["status"] = "running"
        self.production.save(record)
        again = production.Production(None, self.temp.name, "/checkout", "rust:pinned", None, runner=self.git)
        self.assertEqual(again.load(made["id"])["status"], "interrupted")
        # A new request is accepted once nothing is waiting or running.
        self.assertEqual(again.handle("v3_production", LOCAL, {"action": "request"})["status"], "awaiting_approval")

    def test_stamp_updates_the_app_shell(self):
        tree = Path(self.temp.name) / "shell"
        (tree / "crates/app-shell").mkdir(parents=True)
        (tree / "Cargo.toml").write_bytes(CARGO)
        (tree / "Cargo.lock").write_bytes(LOCK)
        (tree / "crates/app-shell/Cargo.toml").write_text('[package]\nname = "webagent-app"\nversion = "0.0.1"\n')
        (tree / "crates/app-shell/tauri.conf.json").write_text(json.dumps({"version": "0.0.1"}))
        production.stamp(tree, "1.2.3")
        self.assertIn('version = "1.2.3"', (tree / "crates/app-shell/Cargo.toml").read_text())
        self.assertEqual(json.loads((tree / "crates/app-shell/tauri.conf.json").read_text())["version"], "1.2.3")


if __name__ == "__main__":
    unittest.main()
