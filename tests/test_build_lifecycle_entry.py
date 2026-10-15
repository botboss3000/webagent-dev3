"""Lifecycle installation resolves protected bootstrap without weakening fixed paths."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec=importlib.util.spec_from_file_location("lifecycle_entry",Path(__file__).resolve().parents[1]/"build-service/lifecycle.py")
lifecycle=importlib.util.module_from_spec(spec);spec.loader.exec_module(lifecycle)

class LifecycleEntryTests(unittest.TestCase):
    def test_source_tree_uses_fixed_install_script(self):
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);(root/"install.py").write_text("# reviewed installer\n");(root/"bootstrap.py").write_text("# installed alternative\n")
            self.assertEqual(lifecycle.installation_entry(root),root/"install.py")

    def test_installed_layout_uses_fixed_protected_bootstrap(self):
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);(root/"bootstrap.py").write_text("# protected installer\n")
            self.assertEqual(lifecycle.installation_entry(root),root/"bootstrap.py")

    def test_missing_and_symlink_entries_are_rejected(self):
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary)
            with self.assertRaises(FileNotFoundError):lifecycle.installation_entry(root)
            (root/"bootstrap.py").write_text("# protected fixture\n");(root/"install.py").symlink_to(root/"bootstrap.py")
            with self.assertRaises(PermissionError):lifecycle.installation_entry(root)
