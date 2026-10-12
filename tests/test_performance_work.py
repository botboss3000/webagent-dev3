"""Admission contract tests; real Docker/plugin journeys run separately."""
import hashlib
import importlib.util
import io
import json
import tarfile
import os
import subprocess
import threading
import uuid
from pathlib import Path
import tempfile
import time
import types
import unittest
from unittest import mock

MODULE = Path(__file__).resolve().parents[1] / "build-service/performance.py"
SPEC = importlib.util.spec_from_file_location("performance_test_subject", MODULE)
performance = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(performance)


class PerformancePolicyTests(unittest.TestCase):
    def test_all_settings_and_invalid_bounds(self):
        configured = dict(priority="memory", attempts=10, total_seconds=1800,
                          profiling="allocations", cpu_cores=2, ram_mb=4096,
                          coverage="extended", max_slowdown_percent=25,
                          max_memory_regression_percent=25, allow_dependencies=True)
        self.assertEqual(performance.settings(configured), configured)
        for key, value in (("attempts", True), ("attempts", 0), ("attempts", 11),
                           ("total_seconds", 1801), ("cpu_cores", 3),
                           ("ram_mb", 127), ("ram_mb", 4097),
                           ("priority", "cpu"), ("coverage", "none"),
                           ("profiling", "bad"), ("max_slowdown_percent", -1),
                           ("allow_dependencies", 1), ("command", "echo")):
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                performance.settings({key: value})

    def test_ready_generation_and_removal_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); package = root / "plugins/optimization"
            package.mkdir(parents=True); package.chmod(0o700)
            digest = hashlib.sha256()
            for name in performance.FILES:
                body = b"fixture"
                (package / name).write_bytes(body)
                (package / name).chmod(0o700 if name == "webagent-optimization" else 0o600)
                raw = name.encode()
                digest.update(len(raw).to_bytes(8, "big")); digest.update(raw)
                digest.update(len(body).to_bytes(8, "big")); digest.update(body)
            marker = root / "data/optimization/plugin.ready"; marker.parent.mkdir(parents=True)
            marker.write_text(json.dumps(dict(generation=digest.hexdigest(), updated_at=time.time())))
            self.assertEqual(performance.generation(root), digest.hexdigest())
            (package / "measure.py").write_bytes(b"changed")
            self.assertNotEqual(performance.generation(root), digest.hexdigest())
            (package / "measure.py").unlink()
            with self.assertRaises(OSError): performance.generation(root)

    def test_generation_rejects_unsafe_asset_and_stale_ready(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); package = root / "plugins/optimization"; package.mkdir(parents=True); package.chmod(0o700)
            for name in performance.FILES:
                (package / name).write_bytes(b"x"); (package / name).chmod(0o700)
            (package / "optimization.json").chmod(0o666)
            with self.assertRaises(PermissionError): performance.generation(root)

    def subject(self):
        subject = object.__new__(performance.Performance)
        subject.rows = {}
        return subject

    def row(self, name, wall, memory, priority="speed"):
        return dict(id=name, task_key="task", action="benchmark", created_at=len(name),
                    image="image", plugin_generation="generation", request_id=name,
                    settings=performance.settings({"priority": priority}),
                    result=dict(status="completed", correctness="passed",
                                wall_seconds=wall, peak_rss_bytes=memory))

    def test_compare_original_and_best_and_correctness(self):
        subject = self.subject()
        original = self.row("original", 10, 100)
        subject.assess(original); subject.rows["original"] = original
        better = self.row("better", 8, 90)
        subject.assess(better); subject.rows["better"] = better
        self.assertTrue(better["accepted"])
        slower = self.row("slower", 9, 80)
        subject.assess(slower); self.assertFalse(slower["accepted"])
        memory_regression = self.row("memory", 7, 101)
        subject.assess(memory_regression); self.assertFalse(memory_regression["accepted"])
        incorrect = self.row("incorrect", 1, 1)
        incorrect["result"]["correctness"] = "failed"
        subject.assess(incorrect); self.assertFalse(incorrect["accepted"])
        changed = self.row("changed", 7, 80); changed["image"] = "different"
        subject.assess(changed); self.assertFalse(changed["accepted"])

    def test_cargo_tests_get_disposable_application_copy(self):
        spec = importlib.util.spec_from_file_location("cargo_copy_host", MODULE.with_name("host.py"))
        host = importlib.util.module_from_spec(spec); spec.loader.exec_module(host)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); source = root / "source"; source.mkdir()
            for name in ("Dockerfile", "Cargo.toml", "Cargo.lock",
                         "data/config/gates/message_visibility_v1.json",
                         "data/config/gates/closing_status_v1.json", "config/prompts.json"):
                path = source / "v3" / name; path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("original")
            private = source / "v3/task-inspection/admin/chat.db"; private.parent.mkdir(parents=True)
            private.write_text("private")
            runner = host.Runner(source)
            calls = []
            def command(argv, **kwargs):
                calls.append(argv)
                working = argv[argv.index("-w") + 1]
                self.assertTrue(working.startswith("/check/scratch-"))
                scratch = root / "checks" / working.removeprefix("/check/")
                prompt = scratch / "config/prompts.json"
                self.assertEqual(prompt.read_text(), "original")
                self.assertFalse((scratch / "task-inspection/admin/chat.db").exists())
                self.assertFalse(any("dst=/build" in value for value in argv))
                prompt.write_text("temporary test mutation")
                return "passed"
            runner.command = command
            self.assertEqual(runner.cargo_task(source, root / "checks", "test", 10), "passed")
            self.assertEqual((source / "v3/config/prompts.json").read_text(), "original")
            self.assertEqual(list((root / "checks").glob("scratch-*")), [])
            self.assertEqual(len(calls), 1)

    def test_scope_compares_original_task_before_first_measurement(self):
        def archive(values):
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w") as tar:
                for name, value in values.items():
                    body = value.encode(); info = tarfile.TarInfo(name); info.size = len(body)
                    tar.addfile(info, io.BytesIO(body))
            return output.getvalue()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            original = {"v3/Cargo.lock": "locked", "v3/src/nested/Cargo.lock": "nested-locked", "v3/config/prompts.json": "original",
                        "v3/src/main.rs": "old"}
            (root / "baseline.tar").write_bytes(archive(original))
            subject = self.subject()
            subject.scope(root, archive({**original, "v3/src/main.rs": "optimized"}), performance.settings({}))
            with self.assertRaises(PermissionError):
                subject.scope(root, archive({**original, "v3/Cargo.lock": "changed"}), performance.settings({}))
            with self.assertRaises(PermissionError):
                subject.scope(root, archive({**original, "v3/src/nested/Cargo.lock": "changed"}), performance.settings({}))
            subject.scope(root, archive({**original, "v3/Cargo.lock": "changed"}),
                          performance.settings({"allow_dependencies": True}))
            with self.assertRaises(PermissionError):
                subject.scope(root, archive({**original, "v3/config/prompts.json": "changed"}),
                              performance.settings({"allow_dependencies": True}))

    def test_candidate_cannot_become_baseline_when_original_failed(self):
        subject = self.subject()
        candidate = self.row("candidate", 1, 1); candidate["role"] = "candidate"
        subject.assess(candidate)
        self.assertFalse(candidate["accepted"])

    def test_package_cache_reuses_bytes_but_not_stale_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); package = root / "plugins/optimization"
            package.mkdir(parents=True); package.chmod(0o700)
            for name in performance.FILES:
                (package / name).write_bytes(b"AAAA"); (package / name).chmod(0o700)
            performance._PACKAGE_CACHE = None
            with mock.patch.object(performance.os, "read", wraps=os.read) as reader:
                first = performance.generation(root)
                initial_reads = reader.call_count
                self.assertGreater(initial_reads, 0)
                self.assertEqual(performance.generation(root), first)
                self.assertEqual(reader.call_count, initial_reads)
                driver = package / "measure.py"; before = driver.stat()
                driver.write_bytes(b"BBBB")
                os.utime(driver, ns=(before.st_atime_ns, before.st_mtime_ns))
                self.assertEqual(driver.stat().st_size, before.st_size)
                self.assertEqual(driver.stat().st_mtime_ns, before.st_mtime_ns)
                changed = performance.generation(root)
                self.assertNotEqual(changed, first)
                self.assertGreater(reader.call_count, initial_reads)
                count = reader.call_count
                self.assertEqual(performance.generation(root), changed)
                self.assertEqual(reader.call_count, count)
                driver.unlink()
                with self.assertRaises(OSError):performance.generation(root)
                self.assertIsNone(performance._PACKAGE_CACHE)

    def test_package_cache_rejects_fifo_without_waiting(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); package = root / "plugins/optimization"
            package.mkdir(parents=True); package.chmod(0o700)
            for name in performance.FILES:
                (package / name).write_bytes(b"fixture"); (package / name).chmod(0o700)
            performance.generation(root)
            driver = package / "measure.py"; driver.unlink(); os.mkfifo(driver, 0o600)
            started = time.monotonic()
            with self.assertRaises(PermissionError):performance.generation(root)
            self.assertLess(time.monotonic()-started, 1)
            self.assertIsNone(performance._PACKAGE_CACHE)

    def test_fresh_authority_reads_terminal_receipts_without_renewing_active_jobs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); package = root / "plugins/optimization"
            package.mkdir(parents=True); package.chmod(0o700)
            for name in performance.FILES:
                (package / name).write_text("fixture"); (package / name).chmod(0o700)
            canonical = lambda value:json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
            trusted = {"user_id": "admin"}; workspace = "workspace"; task = "task-testing"; request = "request01"
            key = hashlib.sha256(canonical(["admin", workspace, task])).hexdigest()
            identity = hashlib.sha256(canonical([key, request])).hexdigest()
            gen = performance.generation(root)
            row = dict(id=identity, status="completed", user_id="admin", workspace=workspace,
                plugin_generation=gen, access_fingerprint="a"*64, lease_until=time.time()-100,
                request_id=request, task_id=task, inputs_sha256="b"*64, result={})
            subject = self.subject(); subject.rows = {identity:row}; subject.root = root / "state"
            artifact = subject.root / identity / "artifacts"; artifact.mkdir(parents=True)
            (artifact / "result.json").write_bytes(b"{}")
            subject.broker = types.SimpleNamespace(
                runner=types.SimpleNamespace(source=root),
                _incoming_lease=lambda caller:(time.time()+20, "f"*64),
                _owned=lambda record,caller,sid:record["user_id"]==caller["user_id"] and record["workspace"]==sid,
                performance_canonical=canonical)
            arguments = dict(task_id=task, expected_inputs_sha256="b"*64, request_id=request,
                             action="status", plugin_generation=gen)
            before = dict(row)
            self.assertEqual(subject.handle(trusted,workspace,arguments)["status"], "completed")
            page = subject.handle(trusted,workspace,{**arguments,"action":"artifact"})
            self.assertEqual(page["data"], "e30=")
            self.assertEqual(row,before)
            for state in ("queued","running"):
                row["status"] = state
                before = dict(row)
                for action in ("status","artifact","cancel"):
                    with self.subTest(state=state,action=action), self.assertRaises(PermissionError):
                        subject.handle(trusted,workspace,{**arguments,"action":action})
                    self.assertEqual(row,before)
            row["status"]="completed"
            with self.assertRaises(PermissionError):
                subject.handle(trusted,workspace,{**arguments,"action":"cancel"})
            with self.assertRaises(ValueError):
                subject.handle({"user_id":"other"},workspace,arguments)

    def test_driver_snapshot_uses_installed_assets_and_mutation_revokes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); package = root / "plugins/optimization"
            package.mkdir(parents=True); package.chmod(0o700)
            for name in performance.FILES:
                body = b"updated live driver" if name == "measure.py" else b"fixture"
                (package / name).write_bytes(body); (package / name).chmod(0o700)
            canonical = root / "task/v3/src/plugin_packages/optimization/measure.py"
            canonical.parent.mkdir(parents=True); canonical.write_bytes(b"old canonical driver")
            subject = self.subject()
            subject.broker = types.SimpleNamespace(runner=types.SimpleNamespace(source=root),
                stopping=False, _lease=lambda row:None)
            expected = performance.generation(root)
            destination = root / "frozen-driver"
            subject.freeze_driver(destination, expected)
            self.assertEqual((destination / "measure.py").read_bytes(), b"updated live driver")
            self.assertNotEqual((destination / "measure.py").read_bytes(), canonical.read_bytes())
            row = dict(plugin_generation=expected)
            subject.valid(row)
            (package / "measure.py").write_bytes(b"another update")
            with self.assertRaises(PermissionError):subject.valid(row)
            self.assertEqual((destination / "measure.py").read_bytes(), b"updated live driver")
            with self.assertRaises(PermissionError):subject.freeze_driver(root / "next-driver", expected)

    def test_candidate_hash_cannot_be_relabelled_as_original_baseline(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); package = root / "plugins/optimization"
            package.mkdir(parents=True); package.chmod(0o700)
            for name in performance.FILES:
                (package / name).write_text("fixture"); (package / name).chmod(0o700)
            canonical = lambda value:json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
            trusted = {"user_id": "admin"}; workspace = "workspace"; task = "task-testing"
            key = hashlib.sha256(canonical(["admin", workspace, task])).hexdigest()
            config = performance.settings({"allow_dependencies": True})
            original = dict(id="original", task_key=key, action="benchmark", role="baseline",
                accepted=True, created_at=1, inputs_sha256="a"*64, settings=config)
            candidate = dict(id="candidate", task_key=key, action="benchmark", role="candidate",
                accepted=True, created_at=2, inputs_sha256="b"*64, settings=config)
            subject = self.subject(); subject.rows = {"original":original, "candidate":candidate}
            subject.scope = lambda *args:None
            subject.broker = types.SimpleNamespace(
                runner=types.SimpleNamespace(source=root),
                _incoming_lease=lambda caller:(time.time()+20, "f"*64),
                performance_canonical=canonical,
                work={key:{"mode":"isolated", "status":"open"}}, work_dir=root,
                performance_snapshot=lambda source:(b"", "b"*64))
            with self.assertRaisesRegex(ValueError, "original baseline cannot be replaced"):
                subject.handle(trusted, workspace, dict(task_id=task, expected_inputs_sha256="b"*64,
                    request_id="rename", action="benchmark", role="baseline", settings=config,
                    plugin_generation=performance.generation(root)))

    def test_artifact_pages_only_recorded_files(self):
        subject = self.subject()
        with tempfile.TemporaryDirectory() as temporary:
            subject.root = Path(temporary)
            artifact = subject.root / "row/artifacts"; artifact.mkdir(parents=True)
            (artifact / "result.json").write_bytes(b"abcdef")
            row = dict(id="row", request_id="request", status="completed", result={})
            page = subject.artifact(row, {"offset": 2, "limit": 2})
            self.assertEqual(page["data"], "Y2Q=")
            self.assertTrue(page["has_more"])
            for name in ("../result.json", "target", "/etc/passwd"):
                with self.assertRaises(PermissionError):
                    subject.artifact(row, {"artifact": name})
            with self.assertRaises(ValueError):
                subject.artifact(row, {"limit": 16385})

    def test_missing_metrics_cannot_be_an_accepted_candidate(self):
        subject = self.subject()
        baseline = self.row("baseline", 10, 100)
        subject.assess(baseline); subject.rows["baseline"] = baseline
        missing = self.row("missing", 8, 80); missing["result"].pop("peak_rss_bytes")
        subject.assess(missing)
        self.assertFalse(missing["accepted"])


@unittest.skipUnless(os.environ.get("OPTIMIZATION_DOCKER_ACCEPTANCE") == "1",
                     "explicit disposable Docker acceptance")
class ActualDockerRevocation(unittest.TestCase):
    def test_removal_cancels_inflight_and_queued_without_replay(self):
        class Runner:
            def command(self, argv, timeout=30):
                result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
                if result.returncode:
                    raise RuntimeError(result.stderr[-4096:])
                return result.stdout
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / "plugins/optimization"; package.mkdir(parents=True); package.chmod(0o700)
            for name in performance.FILES:
                body = "import time,pathlib\npathlib.Path('/artifacts/started').write_text('installed')\ntime.sleep(60)\n" if name == "measure.py" else "fixture"
                (package / name).write_text(body)
                (package / name).chmod(0o700)
            runner = Runner(); runner.source = root
            broker = types.SimpleNamespace(state=root / "state", runner=runner,
                stopping=False, lock=threading.RLock(), performance_scrub=lambda value:value)
            broker.state.mkdir()
            def lease(row):
                if row.get("revoked") or row["lease_until"] <= time.time():
                    raise PermissionError("revoked")
            broker._lease = lease
            subject = performance.Performance(broker)
            image = runner.command(["docker", "inspect", "--format", "{{.Image}}", "webagent-dev-v3"]).strip()
            generation = performance.generation(root)
            cache_key = "fixture"
            for name in ("target", "cargo"):
                (subject.root / "caches" / cache_key / name).mkdir(parents=True)
            rows = []
            for number in range(2):
                identity = hashlib.sha256(uuid.uuid4().bytes).hexdigest()
                directory = subject.root / identity
                source = directory / "source/v3/src/plugin_packages/optimization"
                source.mkdir(parents=True); (directory / "source/ui").mkdir()
                (directory / "artifacts").mkdir()
                (source / "measure.py").write_text("raise AssertionError('canonical driver must not execute')\n")
                subject.freeze_driver(directory / "driver", generation)
                row = dict(id=identity, status="queued", settings=performance.settings({}),
                    image=image, action="benchmark", task_key="fixture", cache_key=cache_key,
                    reserved_seconds=60, lease_until=time.time()+90, plugin_generation=generation,
                    container="webagent-v3-performance-" + identity[:24])
                subject.rows[identity] = row; subject.events[identity] = threading.Event()
                subject.save(row); rows.append(row)
                subject.pool.submit(subject.run, row)
            try:
                deadline = time.time() + 20
                while time.time() < deadline:
                    result = subprocess.run(["docker", "inspect", "--format", "{{.State.Running}}",
                                              rows[0]["container"]], capture_output=True, text=True)
                    if (result.returncode == 0 and result.stdout.strip() == "true"
                            and (subject.root / rows[0]["id"] / "artifacts/started").exists()):
                        break
                    time.sleep(.1)
                else:
                    self.fail("actual first Docker child did not start")
                self.assertEqual(rows[1]["status"], "queued")
                package.rename(root / "removed-package")
                deadline = time.time() + 20
                while time.time() < deadline and any(row["status"] in {"queued", "running"} for row in rows):
                    time.sleep(.1)
                self.assertEqual([row["status"] for row in rows], ["cancelled", "cancelled"])
                for row in rows:
                    result = subprocess.run(["docker", "inspect", row["container"]], capture_output=True)
                    self.assertNotEqual(result.returncode, 0)
                subject.shutdown()
                restarted = performance.Performance(broker)
                self.assertEqual([row["status"] for row in restarted.rows.values()], ["cancelled", "cancelled"])
                restarted.shutdown()
            finally:
                subject.shutdown()


if __name__ == "__main__":
    unittest.main()
