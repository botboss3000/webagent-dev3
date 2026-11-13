"""Shared UI ships from v3/ui, pins are checked at planning, and failed image pairs are recoverable."""
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


CSS = "v3/ui/shared/css/app1.css"


class SnapshotTests(unittest.TestCase):
    def test_installed_ability_package_is_never_a_source_input(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            ability = root / "v3/plugins/abilities/Core/mcp"
            (ability / "package").mkdir(parents=True)
            (ability / "package/webagent-mcp").write_bytes(b"installed")
            (ability / "mcp.json").write_bytes(b"{}")
            entries = read(release.snapshot(root, ("v3/plugins/abilities",), NOOP))
        self.assertEqual(list(entries), ["v3/plugins/abilities/Core/mcp/mcp.json"])


class CandidateTests(unittest.TestCase):
    def test_a_shared_ui_edit_ships_in_the_same_candidate(self):
        base = blob({CSS: b"old"})
        task = blob({CSS: b"new"})
        merged, changed = release.candidate(base, task, base, NOOP)
        self.assertEqual(changed, [CSS])
        self.assertEqual(read(merged)[CSS], b"new")

    def test_the_retired_ui_namespace_of_a_live_release_is_dropped(self):
        base = blob({CSS: b"old"})
        live = blob({CSS: b"old", "ui/shared/css/app1.css": b"old"})
        merged, _ = release.candidate(base, blob({CSS: b"new"}), live, NOOP)
        self.assertEqual(sorted(read(merged)), [CSS])


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
