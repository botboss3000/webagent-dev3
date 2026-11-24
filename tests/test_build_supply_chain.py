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
            summary(targets={"api": {"fixable_high_critical": [["a", "1", "CVE-1", "2"]] * (scan.MAX_ROWS + 1)}}),
            summary(targets={"api": {"fixable_high_critical": [["a", "1", "CVE-1", "2", "MEDIUM"]]}}),
            summary(targets={"api": {"fixable_high_critical": [], "fixable_not_listed": True}}),
            summary(targets={"api": {"fixable_high_critical": [], "fixable_not_listed": -1}}),
            summary(targets={f"t{i}": {} for i in range(scan.MAX_TARGETS + 1)}),
            summary(ui_libraries={"recorded": {"marked": "12.0.0\n"}, "mismatch": []}),
        ]
        for raw in bad:
            with self.assertRaises((ValueError, UnicodeDecodeError), msg=raw[:80]):
                scan.summary_view(raw)
        with self.assertRaises(ValueError):
            scan.summary_view(b" " * (scan.SUMMARY_LIMIT + 1))

    def test_a_large_report_is_capped_critical_first_with_exact_counts(self):
        # 618 rows as the scanner wrote them before severities were recorded, plus a critical
        # finding in a second image: the critical one is shown first, nothing is refused.
        api = [[f"chromium{'-common' if i % 3 == 1 else '-driver' if i % 3 == 2 else ''}", "156.0.8078.79-1~deb12u1",
                f"CVE-2026-{20000 + i // 3}", "156.0.8078.108-1~deb12u1"] for i in range(618)]
        raw = summary(targets={
            "api": {"by_severity": {"CRITICAL": 6, "HIGH": 612}, "fixable_by_severity": {"CRITICAL": 6, "HIGH": 612},
                    "fixable_high_critical": api},
            "gate": {"by_severity": {"CRITICAL": 1}, "fixable_by_severity": {"CRITICAL": 1},
                     "fixable_high_critical": [["openssl", "3.0.1", "CVE-2026-1", "3.0.2", "CRITICAL"]],
                     "fixable_not_listed": 5}})
        view = scan.summary_view(raw)
        api_view, gate_view = view["targets"]["api"], view["targets"]["gate"]
        self.assertEqual(len(api_view["fixable_high_critical"]) + len(gate_view["fixable_high_critical"]), scan.MAX_FINDINGS)
        self.assertEqual(gate_view["fixable_high_critical"][0]["package"], "openssl")
        self.assertEqual((api_view["fixable_not_shown"], gate_view["fixable_not_shown"]), (618 - 199, 5))
        self.assertEqual(api_view["fixable_by_severity"], {"CRITICAL": 6, "HIGH": 612})
        self.assertEqual(api_view["fixable_high_critical"][0]["id"], "CVE-2026-20000")
        self.assertEqual(scan.summary_view(summary())["targets"]["api"]["fixable_not_shown"], 0)


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
                    patch.object(scan, "optional_images", dict), patch.object(scan.sys, "argv", ["scan"]):
                images = {role: "sha256:image" for role in scan.CONTAINERS}
                lock = scan.hashlib.sha256(b"# lock\n").hexdigest()
                key = scan.hashlib.sha256(json.dumps([images, lock], sort_keys=True).encode()).hexdigest()[:16]
                (root / "latest.json").write_text(json.dumps({"status": "ok", "key": key, "scanned_at": time.time() - 60}))
                self.assertEqual(scan.main(), 0)
                self.assertGreater(json.loads((root / "latest.json").read_text())["checked_at"], time.time() - 5)
                (root / "latest.json").write_text(json.dumps({"status": "failed", "key": key, "scanned_at": time.time()}))
                with self.assertRaises(AssertionError):
                    scan.main()


class ScanTests(unittest.TestCase):
    def test_latest_lists_critical_first_and_counts_rows_beyond_the_cap(self):
        """A full scan with 300 fixable rows: latest.json stays readable, the report folder keeps all."""
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            release = root / "releases" / "9850cba2-4b43-4a2d-bd06-6a4c3b8d6e0a" / "v3"
            (release / "abilities").mkdir(parents=True)
            (release / "Cargo.lock").write_text("# lock\n")
            vulnerabilities = [{"PkgName": "chromium", "InstalledVersion": "1", "VulnerabilityID": f"CVE-2026-{i:05d}",
                                "FixedVersion": "2", "Severity": "HIGH"} for i in range(300)]
            vulnerabilities.append({"PkgName": "zlib", "InstalledVersion": "1", "VulnerabilityID": "CVE-2026-99999",
                                    "FixedVersion": "2", "Severity": "CRITICAL"})

            def run(argv, **_):
                if argv[:2] == ["docker", "save"]:
                    Path(argv[-1]).write_bytes(b"")
                return str(release / "abilities") if "Mounts" in " ".join(argv) else "sha256:image"

            def trivy(args, out, _mounts):
                api = "/in/api.tar" in args and "json" in args
                Path(out).write_text(json.dumps({"Results": [{"Vulnerabilities": vulnerabilities}]} if api else {}))
            with patch.object(scan, "ROOT", root), patch.object(scan, "run", run), patch.object(scan, "trivy", trivy), \
                    patch.object(scan, "optional_images", dict), patch.object(scan.sys, "argv", ["scan"]):
                self.assertEqual(scan.main(), 0)
            latest = json.loads((root / "latest.json").read_text())
            api = latest["targets"]["api"]
            self.assertEqual((len(api["fixable_high_critical"]), api["fixable_not_listed"]), (scan.MAX_FINDINGS, 101))
            self.assertEqual(api["fixable_high_critical"][0], ["zlib", "1", "CVE-2026-99999", "2", "CRITICAL"])
            self.assertEqual(api["fixable_by_severity"], {"HIGH": 300, "CRITICAL": 1})
            report = json.loads((Path(latest["report"]) / "summary.json").read_text())
            self.assertEqual(len(report["targets"]["api"]["fixable_high_critical"]), 301)
            view = scan.summary_view((root / "latest.json").read_bytes())
            self.assertEqual(view["targets"]["api"]["fixable_not_shown"], 101)


if __name__ == "__main__":
    unittest.main()
