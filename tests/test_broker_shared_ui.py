"""Shared UI ships from its task source, pins are checked at planning, and failed image pairs are recoverable."""
import ast
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1] / "build-service"


def load(name):
    spec = importlib.util.spec_from_file_location(name + "_under_test", ROOT / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = load("release")
reuse = load("image_reuse")
NOOP = lambda blob: None


def blob(files):
    return release.archive({name: (0o644, body) for name, body in files.items()}, NOOP)


def read(data):
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        return {m.name: archive.extractfile(m).read() for m in archive if m.isfile()}


CSS = "ui/shared/css/app1.css"
SOURCE = "v3/build-service/shared-ui/" + CSS


class SharedUiContextTests(unittest.TestCase):
    def test_release_names_match_the_host_build_context(self):
        tree = ast.parse((ROOT / "host.py").read_text())
        context = next(node.value for node in tree.body if isinstance(node, ast.Assign)
                       and getattr(node.targets[0], "id", None) == "CONTEXT")
        names = tuple(item.value for item in context.elts if item.value.startswith("ui/"))
        self.assertEqual(names, release.SHARED_UI_CONTEXT)


class SnapshotTests(unittest.TestCase):
    def test_ui_is_read_from_the_tree_shared_ui_source(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for name, body in ((SOURCE, b"new"), (CSS, b"stale sibling")):
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_bytes(body)
            entries = read(release.snapshot(root, (CSS, "v3/build-service"), NOOP))
        self.assertEqual(entries[CSS], b"new")
        self.assertEqual(entries[SOURCE], b"new")

    def test_older_roots_without_shared_ui_keep_their_sibling(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / CSS).parent.mkdir(parents=True)
            (root / CSS).write_bytes(b"old")
            self.assertEqual(read(release.snapshot(root, (CSS,), NOOP))[CSS], b"old")


class CandidateTests(unittest.TestCase):
    def test_a_shared_ui_edit_ships_in_the_same_candidate(self):
        base = blob({SOURCE: b"old", CSS: b"old"})
        task = blob({SOURCE: b"new", CSS: b"old"})
        merged, changed = release.candidate(base, task, base, NOOP)
        self.assertEqual(changed, [SOURCE])
        self.assertEqual(read(merged)[CSS], b"new")

    def test_ui_differences_alone_are_not_task_changes(self):
        base = blob({SOURCE: b"same", CSS: b"stale"})
        task = blob({SOURCE: b"same", CSS: b"same"})
        with self.assertRaises(ValueError):
            release.candidate(base, task, base, NOOP)


class BaselineTests(unittest.TestCase):
    def pinned(self, body, pin):
        return blob({CSS: body, "v3/ui-baseline.json": json.dumps(
            {"assets": {CSS: hashlib.sha256(pin).hexdigest()}}).encode()})

    def test_matching_pins_pass(self):
        self.assertEqual(release.ui_baseline_mismatch(self.pinned(b"css", b"css")), [])

    def test_changed_pinned_asset_is_named(self):
        self.assertEqual(release.ui_baseline_mismatch(self.pinned(b"edited", b"css")), [CSS])

    def test_no_baseline_means_no_pins(self):
        self.assertEqual(release.ui_baseline_mismatch(blob({CSS: b"css"})), [])


class ImageWindowTests(unittest.TestCase):
    def history(self, *names):
        return [{"phase": name, "at": index * 10} for index, name in enumerate(names)]

    def test_a_job_that_failed_in_ui_tests_keeps_its_image_windows(self):
        windows = reuse.image_windows(self.history(
            "building_api", "building_loop", "preparing_plugins", "preparing_ui", "frontend_tests"))
        self.assertEqual(windows, {"api": (0, 10), "loop": (10, 20)})

    def test_packaging_sub_phases_extend_the_loop_window(self):
        windows = reuse.image_windows(self.history(
            "building_api", "building_loop", "verifying_dependency_cache", "preparing_plugins"))
        self.assertEqual(windows["loop"], (10, 30))

    def test_the_original_resume_history_still_qualifies(self):
        windows = reuse.image_windows(self.history(
            "building_api", "building_loop", "preparing_plugins", "preparing_ui", "building_api"))
        self.assertEqual(windows, {"api": (0, 10), "loop": (10, 20)})

    def test_histories_without_both_image_builds_fail_closed(self):
        for names in (("building_api",), ("building_api", "building_loop"), ("building_loop", "building_api")):
            with self.assertRaises(ValueError):
                reuse.image_windows(self.history(*names))


if __name__ == "__main__":
    unittest.main()
