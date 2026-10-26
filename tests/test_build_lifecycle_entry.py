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

class StaleTreeGuardTests(unittest.TestCase):
    """A tree cut before the installed release must not silently revert it."""
    def layout(self,base,tree,installed):
        import io,tarfile
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup);root=Path(temporary.name)
        folder=root/"tree";(folder/"v3/build-service").mkdir(parents=True);release=root/"release";release.mkdir()
        with tarfile.open(folder/"baseline.tar","w")as archive:
            for name,body in base.items():
                info=tarfile.TarInfo("v3/build-service/"+name);info.size=len(body);archive.addfile(info,io.BytesIO(body))
        for name,body in tree.items():(folder/"v3/build-service"/name).write_bytes(body)
        for name,body in installed.items():(release/name).write_bytes(body)
        return folder,release
    def test_tree_cut_before_the_installed_release_is_stale(self):
        tree,release=self.layout(base={"host.py":b"old","executor.py":b"old"},tree={"host.py":b"old","executor.py":b"mine"},
                                 installed={"host.py":b"theirs","executor.py":b"old"})
        self.assertEqual(lifecycle.stale_tree(tree,release),["host.py"])
    def test_tree_based_on_the_installed_release_passes(self):
        tree,release=self.layout(base={"host.py":b"theirs","install.py":b"i"},tree={"host.py":b"mine","install.py":b"i2"},
                                 installed={"host.py":b"theirs","bootstrap.py":b"i","index-maintenance.py":b"x"})
        self.assertEqual(lifecycle.stale_tree(tree,release),[])
    def test_tree_that_already_carries_the_installed_bytes_passes(self):
        tree,release=self.layout(base={"host.py":b"old"},tree={"host.py":b"theirs"},installed={"host.py":b"theirs"})
        self.assertEqual(lifecycle.stale_tree(tree,release),[])
    def test_install_refuses_unless_explicitly_overridden(self):
        source=(Path(__file__).resolve().parents[1]/"build-service/lifecycle.py").read_text()
        self.assertIn("--allow-stale-tree",source)
        self.assertIn('if stale and not args.allow_stale_tree:',source)
