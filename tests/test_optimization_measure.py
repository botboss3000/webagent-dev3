import importlib.util
import pathlib
import sys
import unittest

PATH = pathlib.Path(__file__).resolve().parents[1] / "src/plugin_packages/optimization/measure.py"
SPEC = importlib.util.spec_from_file_location("optimization_measure", PATH)
measure = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(measure)


class MeasurementTests(unittest.TestCase):
    def test_large_then_small_children_have_independent_rss(self):
        large = measure.run([sys.executable, "-c",
            "x=bytearray(96*1024*1024); x[::4096]=b'x'*len(x[::4096]); print(len(x))"], 10)
        small = measure.run([sys.executable, "-c", "print('small')"], 10)
        self.assertEqual(large["exit_code"], 0)
        self.assertEqual(small["exit_code"], 0)
        self.assertGreater(large["peak_rss_bytes"], 80 * 1024 * 1024)
        self.assertLess(small["peak_rss_bytes"], large["peak_rss_bytes"] / 2)
        self.assertEqual(small["stdout"], "small\n")

    def test_cpu_is_measured_for_the_child(self):
        result = measure.run([sys.executable, "-c",
            "import time; t=time.monotonic();\nwhile time.monotonic()-t<0.15: pass"], 10)
        self.assertEqual(result["status"], "completed")
        self.assertGreater(result["cpu_seconds"], .05)
        self.assertGreater(result["wall_seconds"], .1)

    def test_timeout_kills_process_group(self):
        result = measure.run([sys.executable, "-c",
            "import subprocess,time; subprocess.Popen(['sleep','30']); time.sleep(30)"], .1)
        self.assertEqual(result["status"], "timeout")
        self.assertLess(result["wall_seconds"], 2)
        self.assertLess(result["exit_code"], 0)

    def test_revocation_cancels_running_child(self):
        import time
        start = time.monotonic()
        result = measure.run(["sleep", "30"], 10, cancel=lambda: time.monotonic()-start > .05)
        self.assertEqual(result["status"], "cancelled")
        self.assertLess(result["wall_seconds"], 2)

    def test_output_is_bounded_without_pipe_deadlock(self):
        result = measure.run([sys.executable, "-c", "print('x'*200000)"], 10, output_limit=100)
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(len(result["stdout"]), 100)
        self.assertTrue(result["output_truncated"])

    def test_rejects_invalid_bounds_and_kind(self):
        with self.assertRaises(ValueError):
            measure.run(["true"], 601)
        with self.assertRaises(ValueError):
            measure.profiler_availability("unknown")

    def test_coverage_uses_existing_native_tests_and_adds_adverse_and_parallel_cases(self):
        import re
        sys.path.insert(0, str(PATH.parent))
        try:
            import workload
            source = (PATH.parents[3] / "src/runtime_tests.rs").read_text()
            for name in workload.selected_tests("extended"):
                self.assertRegex(source, r"fn " + re.escape(name) + r"\(")
            self.assertTrue(set(workload.QUICK) < set(workload.STANDARD) < set(workload.EXTENDED))
            self.assertIn("parallel_batch_overlaps_reads_denies_write_by_policy_and_preserves_result_order",
                          workload.STANDARD)
        finally:
            sys.path.pop(0)

    def test_missing_tooling_is_explicitly_unsupported_before_build(self):
        from unittest.mock import patch
        sys.path.insert(0, str(PATH.parent))
        try:
            import workload
            with patch.object(workload.shutil, "which", return_value=None):
                result = workload.execute("profile_cpu", "standard", "balanced",
                                          "/artifacts/result.json", 60)
            self.assertEqual(result["status"], "unsupported")
            self.assertEqual(result["correctness"], "unverified")
        finally:
            sys.path.pop(0)

    def test_private_source_hash_detects_change_and_restoration(self):
        import tempfile
        sys.path.insert(0, str(PATH.parent))
        try:
            import workload
            with tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                fixture = root / "prompts.json"
                fixture.write_text('{"prompt":"original"}')
                original = workload.source_hash(root)
                fixture.write_text('{"prompt":"temporary fixture"}')
                self.assertNotEqual(workload.source_hash(root), original)
                fixture.write_text('{"prompt":"original"}')
                self.assertEqual(workload.source_hash(root), original)
                (root / "untracked").write_text("extra")
                self.assertNotEqual(workload.source_hash(root), original)
        finally:
            sys.path.pop(0)

    def test_cgroup_oom_kill_counter_is_read_independently_of_exit_code(self):
        from unittest.mock import patch
        sys.path.insert(0, str(PATH.parent))
        try:
            import workload
            with patch.object(pathlib.Path, "read_text", return_value="low 0\nhigh 0\noom 3\noom_kill 2\n"):
                self.assertEqual(workload.oom_kills(), 2)
        finally:
            sys.path.pop(0)

    def test_correctness_runs_full_locked_suite_before_selected_workloads(self):
        import json
        import tempfile
        from unittest.mock import patch
        sys.path.insert(0, str(PATH.parent))
        try:
            import workload
            calls = []
            copied_sources = []
            def fake_run(argv, *args, **kwargs):
                calls.append(argv)
                stdout = ("test result: ok. 1 passed; 0 failed;\n")
                if "--no-run" in argv:
                    stdout = json.dumps({"reason": "compiler-artifact",
                        "profile": {"test": True}, "executable": "/native/test"})
                return {"status": "completed", "exit_code": 0, "stdout": stdout,
                        "stderr": "", "wall_seconds": .01, "cpu_seconds": .01,
                        "peak_rss_bytes": 1024, "output_truncated": False}
            def fake_copytree(source, destination):
                copied_sources.append(pathlib.Path(source))
                pathlib.Path(destination).mkdir(parents=True)
            real_is_dir = pathlib.Path.is_dir
            def cache_present(path):
                return str(path) == "/opt/optimization-cargo/registry" or real_is_dir(path)
            with tempfile.TemporaryDirectory() as directory:
                with patch.object(workload, "__file__", "/driver/workload.py"), \
                     patch.object(workload, "run", side_effect=fake_run), \
                     patch.object(workload.shutil, "copytree", side_effect=fake_copytree), \
                     patch.object(pathlib.Path, "is_dir", cache_present):
                    result = workload.execute("benchmark", "quick", "balanced",
                                              str(pathlib.Path(directory) / "result.json"), 60)
            self.assertEqual(result["correctness"], "passed")
            self.assertEqual(copied_sources[0], pathlib.Path("/source"),
                             "frozen driver location must not select application inputs")
            self.assertIn("--bin", calls[0], "native executable discovery remains scoped")
            self.assertEqual(calls[1], ["cargo", "test", "--locked", "--offline",
                                      "--", "--test-threads=1"])
            self.assertNotIn("--bin", calls[1], "correctness must include all default targets")
            self.assertTrue(all(command[0] == "/native/test" for command in calls[2:]))
        finally:
            sys.path.pop(0)


    def test_heaptrack_totals_are_independent_of_truncated_hotspot_report(self):
        from unittest.mock import patch
        sys.path.insert(0, str(PATH.parent))
        try:
            import workload
            # Footer captured from the real v3 message receipt Heaptrack artifact.
            # Its ordinary backtrace report exceeds the 64 KiB capture limit.
            footer = ("total runtime: 1.69s.\n"
                      "calls to allocation functions: 948267 (560441/s)\n"
                      "temporary memory allocations: 3679 (2174/s)\n"
                      "peak heap memory consumption: 2.50M\n"
                      "peak RSS (including heaptrack overhead): 60.06M\n"
                      "total memory leaked: 423.20K\n")
            report = {"exit_code": 0, "status": "completed", "stdout": footer,
                      "output_truncated": False}
            with patch.object(workload, "run", return_value=report) as measured:
                totals = workload.heaptrack_totals("actual.heaptrack.gz", 10,
                                                  cwd="/source", env={})
                self.assertEqual(totals, {"allocation_calls": 948267,
                                          "peak_heap_memory": "2.50M"})
                argv = measured.call_args.args[0]
                self.assertTrue(all(flag in argv for flag in
                    ["--print-peaks=0", "--print-allocators=0",
                     "--print-temporary=0", "--print-leaks=0"]))
                self.assertEqual(measured.call_args.kwargs["output_limit"], 4096)
            with patch.object(workload, "run", return_value={**report, "output_truncated": True}):
                self.assertIsNone(workload.heaptrack_totals("actual.heaptrack.gz", 10,
                                                           cwd="/source", env={}))
        finally:
            sys.path.pop(0)


if __name__ == "__main__":
    unittest.main()
