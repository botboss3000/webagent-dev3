"""Supply-chain scanner: the administrator projection, its file guards, the broker read and the heartbeat."""
import importlib.util
import json
import os
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import test_admin_build as fixtures

host = fixtures.host
ROOT = Path(__file__).resolve().parents[1] / "build-service"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


scan = load("supply_chain_scan_fixture", ROOT / "supply_chain_scan.py")
RELEASE = "/home/ar/webagent-dev3/build-jobs/ui-assets/releases/9850cba2-4b43-4a2d-bd06-6a4c3b8d6e0a/v3"


def summary(**extra):
    value = {"status": "ok", "key": "e7210eedbd20e821", "scanned_at": 1795441914.7, "checked_at": 1795441914.7,
             "release": RELEASE, "report": "/x", "images": {"api": "sha256:aa"},
             "ui_libraries": {"recorded": {"marked": "12.0.0"}, "mismatch": []},
             "targets": {"cargo": {"by_severity": {}, "fixable_by_severity": {}, "fixable_high_critical": []},
                         "api": {"by_severity": {"HIGH": 3, "CRITICAL": 1}, "fixable_by_severity": {"HIGH": 1},
                                 "fixable_high_critical": [["apt", "2.6.1", "CVE-2026-109518", "2.6.2"]]}}}
    value.update(extra)
    return json.dumps(value).encode()


class ViewTests(unittest.TestCase):
    def test_projection_keeps_only_bounded_fields(self):
        view = scan.summary_view(summary())
        self.assertEqual(view["release"], "9850cba2-4b43-4a2d-bd06-6a4c3b8d6e0a")
        self.assertEqual(view["targets"]["api"]["fixable_high_critical"],
                         [{"package": "apt", "installed": "2.6.1", "id": "CVE-2026-109518", "fixed": "2.6.2"}])
        self.assertEqual(view["targets"]["api"]["by_severity"], {"CRITICAL": 1, "HIGH": 3})
        self.assertNotIn("report", view)
        self.assertNotIn("images", view)
        self.assertEqual(view["ui_libraries"]["recorded"], {"marked": "12.0.0"})

    def test_failed_scan_keeps_a_short_printable_error(self):
        view = scan.summary_view(summary(status="failed", failed_at=1795441999, error="boom\n\x1b[31m" + "x" * 900))
        self.assertEqual(view["status"], "failed")
        self.assertLessEqual(len(view["error"]), 200)
        self.assertTrue(view["error"].isprintable())

    def test_malformed_reports_are_refused(self):
        bad = [
            b"[]", b"{", b"\xff\xfe",
            summary(status="unknown"),
            summary(scanned_at="yesterday"),
            summary(targets={"API!": {}}),
            summary(targets={"api": {"by_severity": {"SEVERE": 1}}}),
            summary(targets={"api": {"by_severity": {"HIGH": -1}}}),
            summary(targets={"api": {"fixable_high_critical": [["apt", "2.6.1", "CVE"]]}}),
            summary(targets={"api": {"fixable_high_critical": [["<script>", "1", "CVE-1", "2"]]}}),
            summary(targets={"api": {"fixable_high_critical": [["a", "1", "CVE-1", "2"]] * (scan.MAX_FINDINGS + 1)}}),
            summary(targets={f"t{i}": {} for i in range(scan.MAX_TARGETS + 1)}),
            summary(ui_libraries={"recorded": {"marked": "12.0.0\n"}, "mismatch": []}),
        ]
        for raw in bad:
            with self.assertRaises((ValueError, UnicodeDecodeError), msg=raw[:80]):
                scan.summary_view(raw)
        with self.assertRaises(ValueError):
            scan.summary_view(b" " * (scan.SUMMARY_LIMIT + 1))


class ReadTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_missing_malformed_large_and_linked_files_are_states(self):
        uid = os.getuid()
        self.assertEqual(scan.read_summary(self.root, uid), {"available": False, "reason": "missing"})
        (self.root / "latest.json").write_bytes(b"{nope")
        self.assertEqual(scan.read_summary(self.root, uid)["reason"], "malformed")
        (self.root / "latest.json").write_bytes(b" " * (scan.SUMMARY_LIMIT + 1))
        self.assertEqual(scan.read_summary(self.root, uid)["reason"], "too_large")
        (self.root / "latest.json").unlink()
        (self.root / "target.json").write_bytes(summary())
        (self.root / "latest.json").symlink_to(self.root / "target.json")
        self.assertEqual(scan.read_summary(self.root, uid)["reason"], "refused")
        (self.root / "latest.json").unlink()
        (self.root / "latest.json").write_bytes(summary())
        self.assertEqual(scan.read_summary(self.root, uid + 1)["reason"], "refused")
        self.assertTrue(scan.read_summary(self.root, uid)["available"])
        linked = self.root.parent / (self.root.name + "-link")
        linked.symlink_to(self.root)
        try:
            self.assertEqual(scan.read_summary(linked, uid)["reason"], "refused")
        finally:
            linked.unlink()


class BrokerTests(unittest.TestCase):
    tearDown = fixtures.BuildTests.tearDown
    call = fixtures.BuildTests.call

    def setUp(self):
        fixtures.BuildTests.setUp(self)
        self.reports = tempfile.TemporaryDirectory()
        (Path(self.reports.name) / "latest.json").write_bytes(summary())
        self.admin = {"user_id": "admin", "administrator": True, "session_id": "00000000-0000-0000-0000-000000000000",
                      "lease_until": time.time() + 30, "access_fingerprint": "a" * 64}

    def test_read_is_administrator_only_and_takes_no_arguments(self):
        with patch.object(host, "SUPPLY_CHAIN_ROOT", Path(self.reports.name)), \
                patch.dict(host.CONFIG, {"source_uid": os.getuid()}):
            view = self.call("v3_supply_chain", trusted=self.admin, workspace=self.admin["session_id"])
            self.assertTrue(view["available"])
            self.assertEqual(view["targets"]["api"]["fixable_high_critical"][0]["id"], "CVE-2026-109518")
            with self.assertRaises(PermissionError):
                self.call("v3_supply_chain")
            with self.assertRaises(PermissionError):
                self.call("v3_supply_chain", {"path": "/etc/shadow"}, trusted=self.admin,
                          workspace=self.admin["session_id"])
        self.reports.cleanup()


class HeartbeatTests(unittest.TestCase):
    def test_unchanged_release_records_a_check_and_a_failure_is_retried(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            release = root / "releases" / "9850cba2-4b43-4a2d-bd06-6a4c3b8d6e0a" / "v3"
            (release / "abilities").mkdir(parents=True)
            (release / "Cargo.lock").write_text("# lock\n")

            def run(argv, **_):
                return str(release / "abilities") if "Mounts" in " ".join(argv) else "sha256:image"
            with patch.object(scan, "ROOT", root), patch.object(scan, "run", run), \
                    patch.object(scan, "trivy", side_effect=AssertionError("no scan expected")), \
                    patch.object(scan.sys, "argv", ["scan"]):
                images = {role: "sha256:image" for role in scan.CONTAINERS}
                lock = scan.hashlib.sha256(b"# lock\n").hexdigest()
                key = scan.hashlib.sha256(json.dumps([images, lock], sort_keys=True).encode()).hexdigest()[:16]
                (root / "latest.json").write_text(json.dumps({"status": "ok", "key": key, "scanned_at": time.time() - 60}))
                self.assertEqual(scan.main(), 0)
                self.assertGreater(json.loads((root / "latest.json").read_text())["checked_at"], time.time() - 5)
                (root / "latest.json").write_text(json.dumps({"status": "failed", "key": key, "scanned_at": time.time()}))
                with self.assertRaises(AssertionError):
                    scan.main()


if __name__ == "__main__":
    unittest.main()
