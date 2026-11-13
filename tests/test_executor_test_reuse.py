"""Skipping identical passed test executables must never skip a test whose runtime inputs changed."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("executor_under_test", ROOT / "build-service" / "executor.py")
executor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(executor)
digest = executor.Executor.runtime_inputs_digest


class RuntimeInputsDigestTest(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.root = Path(self.folder.name)
        for name, text in {"crates/a/src/lib.rs": "fn a() {}", "src/main.rs": "fn main() {}",
                           "live-files.json": "{}", "abilities/x/adapter.rs": "fn x() {}",
                           "ui/app.css": "a{}"}.items():
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_text(text)
        self.before = digest(self.root)

    def tearDown(self):
        self.folder.cleanup()

    def test_compiled_rust_sources_do_not_change_it(self):
        # They reach tests only through the executables, whose own hashes are in the pass key.
        (self.root / "crates/a/src/lib.rs").write_text("fn a() { changed() }")
        (self.root / "src/main.rs").write_text("fn main() { changed() }")
        self.assertEqual(digest(self.root), self.before)

    def test_files_tests_read_at_runtime_change_it(self):
        for name in ["live-files.json", "abilities/x/adapter.rs", "ui/app.css"]:
            original = (self.root / name).read_text()
            (self.root / name).write_text(original + " ")
            self.assertNotEqual(digest(self.root), self.before, name)
            (self.root / name).write_text(original)
        (self.root / "docs.md").write_text("new")
        self.assertNotEqual(digest(self.root), self.before)


if __name__ == "__main__":
    unittest.main()
