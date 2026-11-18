"""A UI fixture whose reads the import graph cannot bound reuses its pass while every file it read is unchanged."""
import json
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

import test_admin_build as fixtures

host = fixtures.host


class TracedFixtureReuseTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        # b.test.mjs reads a file by a computed path, so the import graph cannot bound it.
        self.write({"v3/ui/a.js": "export const a=1;", "v3/ui/a.test.mjs": "import './a.js';",
                    "v3/ui/b.test.mjs": "import fs from 'node:fs'; fs.readFileSync(variable);",
                    "v3/ui/page.html": "<p>page</p>", "v3/ui/unrelated.js": "const other=1;"})
        self.trace = {"test": "/candidate/v3/ui/b.test.mjs", "unsafe": "", "listed": ["/candidate/v3/ui/data"],
                      "touched": ["/candidate/v3/ui/page.html", "/candidate/v3/ui/absent.json", "/candidate/v3/favicon.ico"]}
        (self.root / "v3/ui/data").mkdir()
        self.calls = []
        self.job = {"job_id": "first"}
        self.runner = types.SimpleNamespace(job_context=types.SimpleNamespace(job=self.job), ui_test_timeout=lambda tests: 120,
                                            index_command=self.index_command)
        self.config = {**host.CONFIG, "scheduler": {**host.CONFIG.get("scheduler", {}), "ui_test_image": "sha256:" + "1" * 64}}
        state = patch.object(host, "STATE", self.root / "state")
        state.start()
        self.addCleanup(state.stop)

    def write(self, files):
        for name, body in files.items():
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(body)

    def files(self):
        return {str(p.relative_to(self.root)): (0o644, p.read_bytes())
                for p in self.root.rglob("*") if p.is_file() and "state" not in p.relative_to(self.root).parts}

    def index_command(self, argv, **kwargs):
        self.calls.append(argv)
        self.assertTrue(kwargs.get("traces"))
        self.assertIn("NODE_OPTIONS=--require=/candidate/.ui-trace.cjs", argv)
        return "ok", json.dumps([self.trace])

    def run_tests(self):
        evidence = host.ui_policy_module.dependencies(self.files(), ["v3/ui/a.js"])
        evidence["runtime_images"] = {}
        with patch.object(host, "CONFIG", self.config):
            return host.Runner.validate_selected_tests(self.runner, "job-" + str(len(self.calls)), self.root, evidence)

    def test_unread_change_reuses_the_traced_pass(self):
        first = self.run_tests()
        self.assertIn("v3/ui/b.test.mjs", first["executed_tests"])
        self.write({"v3/ui/unrelated.js": "const other=2;"})
        second = self.run_tests()
        self.assertIn("v3/ui/b.test.mjs", second["reused_tests"])
        self.assertTrue(second["fixtures"]["v3/ui/b.test.mjs"]["traced"])
        self.assertEqual(second["fixtures"]["v3/ui/b.test.mjs"]["reused_from"], first["fixtures"]["v3/ui/b.test.mjs"]["verified_job_id"])

    def test_read_change_new_file_listing_change_or_test_change_executes(self):
        changes = [{"v3/ui/page.html": "<p>changed</p>"}, {"v3/ui/absent.json": "{}"},
                   {"v3/ui/data/new.json": "{}"}, {"v3/ui/b.test.mjs": "import fs from 'node:fs'; fs.readFileSync(other);"}]
        for change in changes:
            with self.subTest(change=change):
                self.run_tests()
                self.write(change)
                self.assertIn("v3/ui/b.test.mjs", self.run_tests()["executed_tests"])

    def test_unsafe_or_direct_file_fixtures_are_never_traced(self):
        self.trace["unsafe"] = "subprocess"
        self.run_tests()
        self.write({"v3/ui/unrelated.js": "const other=3;"})
        self.assertIn("v3/ui/b.test.mjs", self.run_tests()["executed_tests"])
        self.trace["unsafe"] = ""
        self.write({"v3/ui/b.test.mjs": "import fs from 'node:fs'; fs.readFileSync(variable); page.goto('file://x');"})
        self.run_tests()
        self.write({"v3/ui/unrelated.js": "const other=4;"})
        self.assertIn("v3/ui/b.test.mjs", self.run_tests()["executed_tests"])

    def test_inventory_marks_missing_and_refuses_symlinks_and_escapes(self):
        inventory = host.Runner.ui_trace_inventory(self.root, {"v3/ui/page.html", "v3/ui/absent.json"}, {"v3/ui/data"})
        self.assertEqual(inventory["files"]["v3/ui/absent.json"], "missing")
        self.assertEqual(inventory["folders"]["v3/ui/data"], [])
        (self.root / "v3/ui/link.js").symlink_to(self.root / "v3/ui/a.js")
        self.assertIsNone(host.Runner.ui_trace_inventory(self.root, {"v3/ui/link.js"}, set()))
        self.assertIsNone(host.Runner.ui_trace_inventory(self.root, {"v3/ui/../../etc/passwd"}, set()))
        self.assertIsNone(host.Runner.ui_trace_inventory(self.root, {"v3/src/main.rs"}, set()))

    def test_tracer_is_written_into_the_mount(self):
        written = []
        def index_command(argv, **kwargs):
            mount = Path(argv[argv.index("--mount") + 1].split("src=", 1)[1].split(",", 1)[0])
            written.append((mount / ".ui-trace.cjs").read_text())
            return "ok", "[]"
        self.runner.index_command = index_command
        self.run_tests()
        self.assertEqual(written, [host.UI_TRACER])


if __name__ == "__main__":
    unittest.main()
