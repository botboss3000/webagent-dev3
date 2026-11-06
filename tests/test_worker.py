import os
os.environ["V3_BUILD_EXECUTOR"]="1"
import asyncio
import importlib.util
import fcntl
import threading
import hashlib
import os
import sys
import subprocess
import tempfile
import unittest
from unittest import mock
import uuid
from pathlib import Path

spec = importlib.util.spec_from_file_location("worker", Path(__file__).parents[1] / "worker.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)



class WorkerTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        worker.ROOT = Path(self.tmp.name) / "workspaces"
        worker.ROOT.mkdir()
        worker.TEMPLATE = Path(self.tmp.name) / "source-template"
        worker.TEMPLATE.mkdir()
        (worker.TEMPLATE / "Cargo.toml").write_text("[package]\nname = \"test\"\nversion = \"0.1.0\"\n")
        (worker.TEMPLATE / "src").mkdir()
        (worker.TEMPLATE / "src/main.rs").write_text("fn main() {}\n")
        worker.CONTROL = Path(self.tmp.name) / "control"
        worker.CONTROL.mkdir()
        (worker.CONTROL / "dispatch.lock").touch()
        (worker.CONTROL / "shutoff").write_text("0")
        self.session = str(uuid.uuid4())

    def tearDown(self):
        self.tmp.cleanup()

    async def run_tool(self, name, args, timeout=120):
        return await worker.execute({"workspace": self.session, "name": name,
                                     "arguments": args, "timeout_seconds": timeout})

    async def run_command(self, argv, timeout=120):
        # The supervisor that still runs workspace seeding and Code Index scans.
        with (worker.CONTROL / "dispatch.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            return await worker.command(argv, worker.workspace_path(self.session), timeout, lock)

    async def test_code_index_private_dispatch_runs_under_supervisor(self):
        base=worker.workspace_path(self.session)
        (base/"main.rs").write_text("fn main() {}\n")
        result=await self.run_tool("code_index_scan",{"root":".","action":"discover"})
        self.assertEqual(result["status"],"success",result)
        self.assertTrue((base/".code-index").is_dir())

    async def test_quiesce_cancels_owned_work_and_waits_for_cleanup(self):
        started, cleaned = asyncio.Event(), asyncio.Event()
        async def operation(request, lock):
            fcntl.flock(lock, fcntl.LOCK_UN)
            if request["name"] == "slow":
                started.set()
                try:
                    await asyncio.sleep(30)
                finally:
                    await asyncio.sleep(0.01)
                    cleaned.set()
                return {"status":"success"}
            await worker.stop_workspace_operations(request["workspace"])
            return {"status":"success"}
        with mock.patch.object(worker, "execute_locked", side_effect=operation):
            task=asyncio.create_task(self.run_tool("slow",{}))
            await started.wait()
            result=await self.run_tool("administrator_quiesce",{})
            self.assertEqual(result["status"],"success")
            self.assertTrue(cleaned.is_set())
            self.assertTrue(task.cancelled())
            self.assertFalse(worker.ACTIVE_OPERATIONS)

    async def test_cancelled_synchronous_call_waits_for_thread_settlement(self):
        started, release, settled = threading.Event(), threading.Event(), threading.Event()
        def write():
            started.set()
            release.wait(2)
            settled.set()
        task=asyncio.create_task(worker.synchronous(write))
        await asyncio.to_thread(started.wait)
        task.cancel()
        await asyncio.sleep(0.01)
        self.assertFalse(task.done())
        release.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertTrue(settled.is_set())

    async def test_source_patch_applies_reviewed_multifile_diff(self):
        await self.run_tool("workspace_seed", {})
        source = worker.ROOT / self.session / "webagent-v3"
        review = await self.run_tool("source_review", {})
        patch = (
            "diff --git a/src/main.rs b/src/main.rs\n"
            "--- a/src/main.rs\n"
            "+++ b/src/main.rs\n"
            "@@ -1 +1 @@\n"
            "-fn main() {}\n"
            "+fn main() { println!(\"changed\"); }\n"
            "diff --git a/new.txt b/new.txt\n"
            "new file mode 100644\n"
            "--- /dev/null\n"
            "+++ b/new.txt\n"
            "@@ -0,0 +1 @@\n"
            "+new source\n"
        )
        result = await self.run_tool("source_patch", {
            "patch": patch, "expected_review_sha256": review["review_sha256"]})
        self.assertEqual(result["status"], "success")
        self.assertEqual(result["paths"], ["src/main.rs", "new.txt"])
        self.assertIn("changed", (source / "src/main.rs").read_text())
        self.assertEqual((source / "new.txt").read_text(), "new source\n")
        self.assertEqual(result["review_sha256"],
                         (await self.run_tool("source_review", {}))["review_sha256"])

    async def test_source_patch_rejects_stale_or_invalid_patch_without_partial_edit(self):
        await self.run_tool("workspace_seed", {})
        source = worker.ROOT / self.session / "webagent-v3"
        original = (source / "src/main.rs").read_text()
        review = await self.run_tool("source_review", {})
        patch = (
            "diff --git a/src/main.rs b/src/main.rs\n"
            "--- a/src/main.rs\n"
            "+++ b/src/main.rs\n"
            "@@ -1 +1 @@\n"
            "-fn main() {}\n"
            "+fn main() { println!(\"changed\"); }\n"
            "diff --git a/Cargo.toml b/Cargo.toml\n"
            "--- a/Cargo.toml\n"
            "+++ b/Cargo.toml\n"
            "@@ -1 +1 @@\n"
            "-not present\n"
            "+changed\n"
        )
        with self.assertRaisesRegex(ValueError, "review"):
            await self.run_tool("source_patch", {
                "patch": patch, "expected_review_sha256": "0" * 64})
        with self.assertRaises(ValueError):
            await self.run_tool("source_patch", {
                "patch": patch, "expected_review_sha256": review["review_sha256"]})
        self.assertEqual((source / "src/main.rs").read_text(), original)

    async def test_source_patch_review_token_is_bound_to_baseline(self):
        await self.run_tool("workspace_seed", {})
        source = worker.ROOT / self.session / "webagent-v3"
        before = await self.run_tool("source_review", {})
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@localhost",
             "commit", "--allow-empty", "-qm", "new baseline"],
            cwd=source, check=True)
        after = await self.run_tool("source_review", {})
        self.assertEqual(before["diff_sha256"], after["diff_sha256"])
        self.assertNotEqual(before["review_sha256"], after["review_sha256"])
        patch = (
            "diff --git a/new.txt b/new.txt\n"
            "new file mode 100644\n"
            "--- /dev/null\n"
            "+++ b/new.txt\n"
            "@@ -0,0 +1 @@\n"
            "+new\n"
        )
        with self.assertRaisesRegex(ValueError, "review"):
            await self.run_tool("source_patch", {
                "patch": patch, "expected_review_sha256": before["review_sha256"]})
        self.assertFalse((source / "new.txt").exists())

    async def test_source_patch_rejects_git_metadata_and_symlink_paths(self):
        await self.run_tool("workspace_seed", {})
        source = worker.ROOT / self.session / "webagent-v3"
        config = (source / ".git/config").read_bytes()
        outside = Path(self.tmp.name) / "escape.txt"
        (source / "src/link").symlink_to(self.tmp.name, target_is_directory=True)
        review = await self.run_tool("source_review", {})
        for path in (".git/config", "../escape.txt", "src/link/escape.txt"):
            patch = (
                f"diff --git a/{path} b/{path}\n"
                "new file mode 100644\n"
                "--- /dev/null\n"
                f"+++ b/{path}\n"
                "@@ -0,0 +1 @@\n"
                "+unsafe\n"
            )
            with self.assertRaises(ValueError):
                await self.run_tool("source_patch", {
                    "patch": patch, "expected_review_sha256": review["review_sha256"]})
        self.assertEqual((source / ".git/config").read_bytes(), config)
        self.assertFalse(outside.exists())

    async def test_source_patch_obeys_global_shutoff(self):
        await self.run_tool("workspace_seed", {})
        source = worker.ROOT / self.session / "webagent-v3"
        review = await self.run_tool("source_review", {})
        patch = (
            "diff --git a/blocked.txt b/blocked.txt\n"
            "new file mode 100644\n"
            "--- /dev/null\n"
            "+++ b/blocked.txt\n"
            "@@ -0,0 +1 @@\n"
            "+blocked\n"
        )
        (worker.CONTROL / "shutoff").write_text("1")
        with self.assertRaisesRegex(ValueError, "disabled"):
            await self.run_tool("source_patch", {
                "patch": patch, "expected_review_sha256": review["review_sha256"]})
        self.assertFalse((source / "blocked.txt").exists())

    async def test_source_patch_apply_error_is_uncertain(self):
        await self.run_tool("workspace_seed", {})
        review = await self.run_tool("source_review", {})
        patch = (
            "diff --git a/uncertain.txt b/uncertain.txt\n"
            "new file mode 100644\n"
            "--- /dev/null\n"
            "+++ b/uncertain.txt\n"
            "@@ -0,0 +1 @@\n"
            "+uncertain\n"
        )
        original = worker.git_capture

        def fail_during_apply(source, arguments, *args):
            if arguments[:2] == ["apply", "--whitespace=error"]:
                raise OSError("simulated worker loss")
            return original(source, arguments, *args)

        with mock.patch.object(worker, "git_capture", side_effect=fail_during_apply):
            result = await self.run_tool("source_patch", {
                "patch": patch, "expected_review_sha256": review["review_sha256"]})
        self.assertEqual(result["status"], "uncertain")
        self.assertIn("review", result["output"])

    async def test_source_review_includes_tracked_and_new_files_without_changing_index(self):
        await self.run_tool("workspace_seed", {})
        source = worker.ROOT / self.session / "webagent-v3"
        (source / "src/main.rs").write_text("fn main() { println!(\"changed\"); }\n")
        (source / "new.txt").write_text("new source\n")
        before = subprocess.check_output(["git", "status", "--porcelain", "-z"], cwd=source)
        result = await self.run_tool("source_review", {})
        after = subprocess.check_output(["git", "status", "--porcelain", "-z"], cwd=source)
        self.assertEqual(result["status"], "success")
        self.assertFalse(result["truncated"])
        self.assertEqual(result["diff_sha256"], hashlib.sha256(result["diff"].encode()).hexdigest())
        self.assertEqual(result["review_sha256"], hashlib.sha256(
            result["baseline_commit"].encode() + b"\0" + result["diff"].encode()).hexdigest())
        self.assertIn("changed", result["diff"])
        self.assertIn("new source", result["diff"])
        self.assertIn("src/main.rs", result["status_output"])
        self.assertIn("new.txt", result["status_output"])
        self.assertEqual(before, after)
        subprocess.run(["git", "restore", "--worktree", "src/main.rs"], cwd=source, check=True)
        (source / "new.txt").unlink()
        subprocess.run(["git", "apply", "--check", "-"], cwd=source,
                       input=result["diff"].encode(), check=True)

    async def test_source_review_reports_oversized_untracked_file(self):
        await self.run_tool("workspace_seed", {})
        source = worker.ROOT / self.session / "webagent-v3"
        (source / "large.txt").write_bytes(b"x" * (worker.LIMIT + 1))
        result = await self.run_tool("source_review", {})
        self.assertTrue(result["truncated"])
        self.assertIsNone(result["diff_sha256"])
        self.assertIn("large.txt", result["status_output"])
        self.assertLessEqual(len(result["diff"].encode()), worker.LIMIT)

    async def test_workspace_seed_creates_clean_source_baseline(self):
        result = await self.run_tool("workspace_seed", {})
        path = worker.ROOT / self.session / "webagent-v3"
        self.assertEqual(result["status"], "success")
        self.assertEqual(result["path"], "webagent-v3")
        self.assertEqual((path / "src/main.rs").read_text(), "fn main() {}\n")
        self.assertEqual(len(result["baseline_commit"]), 40)
        status = subprocess.check_output(["git", "status", "--porcelain"], cwd=path)
        self.assertEqual(status, b"")
        (path / "src/main.rs").write_text("fn main() { println!(\"edited\"); }\n")
        with self.assertRaisesRegex(ValueError, "already initialized"):
            await self.run_tool("workspace_seed", {})
        self.assertIn("edited", (path / "src/main.rs").read_text())

    async def test_workspace_seed_obeys_shutoff_before_publish(self):
        original = worker.workspace_support()._seed_finish
        def stop_before_publish(spec, raw):
            (worker.CONTROL / "shutoff").write_text("1")
            return original(spec, raw)
        with mock.patch.object(worker.workspace_support(), "_seed_finish", side_effect=stop_before_publish):
            with self.assertRaisesRegex(ValueError, "execution disabled"):
                await self.run_tool("workspace_seed", {})
        self.assertFalse((worker.ROOT / self.session / "webagent-v3").exists())

    async def test_seed_cancelled_spawn_is_reaped_without_publication(self):
        original = asyncio.create_subprocess_exec
        launched, release = asyncio.Event(), asyncio.Event()
        children = []
        async def delayed(*args, **kwargs):
            process = await original(*args, **kwargs)
            children.append(process); launched.set()
            await release.wait()
            return process
        with mock.patch.object(worker.asyncio, "create_subprocess_exec", delayed):
            task = asyncio.create_task(self.run_tool("workspace_seed", {}))
            await launched.wait(); task.cancel(); release.set()
            with self.assertRaises(asyncio.CancelledError):
                await asyncio.wait_for(task, 8)
        self.assertIsNotNone(children[0].returncode)
        base = worker.ROOT / self.session
        self.assertFalse((base / "webagent-v3").exists())
        self.assertEqual([], list(base.glob(".v3-seed-*")))

    async def test_seed_child_rejects_raced_stage_directory_escape(self):
        base = worker.workspace_path(self.session)
        outside = Path(self.tmp.name) / "outside"; outside.mkdir()
        (outside / "main.rs").write_text("OUTSIDE")
        script = r"""
import importlib.util,runpy,sys,json,shutil
from pathlib import Path
workerpath,base,template,outside=sys.argv[1:]
spec=importlib.util.spec_from_file_location("seedworker",workerpath)
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
real=shutil.copytree
def race(src,dst,*args,**kwargs):
    if Path(src)==Path(template):
        (Path(dst)/"src").symlink_to(outside,target_is_directory=True)
    return real(src,dst,*args,**kwargs)
m.shutil.copytree=race
runpy.run_path(str(Path(workerpath).with_name("sandbox.py")))["restrict"](base,read_paths=(template,))
try:
    m._seed_inner({"base":base,"template":template,"stage":".v3-seed-fixture"})
    print("UNSAFE_SUCCESS")
except Exception:
    print("REJECTED")
"""
        result = await asyncio.to_thread(subprocess.run,
            [sys.executable, "-I", "-c", script, str(Path(worker.__file__).resolve()), str(base), str(worker.TEMPLATE), str(outside)],
            capture_output=True, timeout=8, check=True)
        self.assertEqual(b"REJECTED\n", result.stdout)
        self.assertEqual("OUTSIDE", (outside / "main.rs").read_text())

    async def test_seed_child_ignores_raced_precommit_hook(self):
        base = worker.workspace_path(self.session)
        outside = Path(self.tmp.name) / "hook-outside"
        script = r"""
import importlib.util,runpy,sys,subprocess
from pathlib import Path
workerpath,base,template,outside=sys.argv[1:]
spec=importlib.util.spec_from_file_location("seedworker",workerpath)
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
real=subprocess.Popen
stage=Path(base)/".v3-seed-fixture"
def race(argv,*args,**kwargs):
    if "commit" in argv:
        hook=stage/".git/hooks/pre-commit"
        hook.write_text("#!/bin/sh\nprintf UNSAFE > "+outside+"\n")
        hook.chmod(0o755)
    return real(argv,*args,**kwargs)
m.subprocess.Popen=race
runpy.run_path(str(Path(workerpath).with_name("sandbox.py")))["restrict"](base,read_paths=(template,))
result=m._seed_inner({"base":base,"template":template,"stage":".v3-seed-fixture"})
print("SEEDED" if result["baseline_commit"] else "FAILED")
"""
        result = await asyncio.to_thread(subprocess.run,
            [sys.executable, "-I", "-c", script, str(Path(worker.__file__).resolve()), str(base), str(worker.TEMPLATE), str(outside)],
            capture_output=True, timeout=8, check=True)
        self.assertEqual(b"SEEDED\n", result.stdout)
        self.assertFalse(outside.exists())

    async def test_workspace_seed_rejects_template_symlink(self):
        (worker.TEMPLATE / "escape").symlink_to("/etc/passwd")
        with self.assertRaisesRegex(ValueError, "invalid source template"):
            await self.run_tool("workspace_seed", {})
        self.assertFalse((worker.ROOT / self.session / "webagent-v3").exists())

    async def test_timeout(self):
        result = await self.run_command(["/bin/sh", "-c", "sleep 30"], 1)
        self.assertEqual(result["status"], "timeout")

    async def test_cancel_kills_descendant(self):
        task = asyncio.create_task(self.run_command(["/bin/sh", "-c", "sleep 2; touch survived"]))
        await asyncio.sleep(0.15)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        await asyncio.sleep(2.1)
        self.assertFalse((worker.ROOT / self.session / "survived").exists())

    async def test_cancel_kills_double_fork_new_session(self):
        code = (
            "import os,time\n"
            "if os.fork()==0:\n"
            " os.setsid()\n"
            " if os.fork()!=0: os._exit(0)\n"
            " time.sleep(2); open('detached-survived','w').write('bad'); os._exit(0)\n"
            "time.sleep(30)\n"
        )
        task = asyncio.create_task(self.run_command(["/usr/bin/python3", "-c", code]))
        await asyncio.sleep(0.2)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        await asyncio.sleep(2.1)
        self.assertFalse((worker.ROOT / self.session / "detached-survived").exists())

    async def test_completion_cleans_detached_child(self):
        code = "import os,time\nif os.fork()==0:\n os.setsid(); time.sleep(2); open('orphan-survived','w').write('bad')\n"
        result = await self.run_command(["/usr/bin/python3", "-c", code])
        self.assertEqual(result["status"], "success")
        await asyncio.sleep(2.1)
        self.assertFalse((worker.ROOT / self.session / "orphan-survived").exists())

    async def test_control_lock_released_while_source_review_runs(self):
        started, release = threading.Event(), threading.Event()
        original = worker.source_review

        def delayed(*args):
            started.set()
            release.wait(2)
            return {"status": "success"}

        worker.source_review = delayed
        with mock.patch.dict(os.environ, {"V3_BUILD_EXECUTOR": "1"}):
            task = asyncio.create_task(self.run_tool("source_review", {}))
            try:
                self.assertTrue(await asyncio.to_thread(started.wait, 1))
                with (worker.CONTROL / "dispatch.lock").open("rb") as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    (worker.CONTROL / "shutoff").write_text("1")
                    fcntl.flock(lock, fcntl.LOCK_UN)
                self.assertFalse(task.done())
            finally:
                release.set()
                await task
                worker.source_review = original

    async def test_shutoff(self):
        (worker.CONTROL / "shutoff").write_text("1")
        with self.assertRaisesRegex(ValueError, "disabled"):
            await self.run_tool("terminal_list", {})

    async def test_output_is_bounded(self):
        result = await self.run_command(["/usr/bin/python3", "-c", "print('x' * 2000000)"])
        self.assertTrue(result["truncated"])
        self.assertLessEqual(len(result["output"]), worker.LIMIT)


if __name__ == "__main__":
    unittest.main()


class WorkerRpcEnvelopeTests(unittest.IsolatedAsyncioTestCase):
    async def client_response(self, version, payload):
        import json
        import struct
        reader = mock.Mock()
        reader.readline = mock.AsyncMock(return_value=json.dumps({"version": version, "id": "routing", "workspace": str(uuid.uuid4())}).encode() + b"\n")
        async def disconnected(_):
            await asyncio.sleep(3600)
        reader.read = disconnected
        writer = mock.Mock()
        writer.get_extra_info.return_value.getsockopt.return_value = struct.pack("3i", 1, 1100, 1100)
        writer.drain = mock.AsyncMock()
        writer.wait_closed = mock.AsyncMock()
        with mock.patch.object(worker, "execute", new=mock.AsyncMock(return_value=payload)) as execute:
            await worker.client(reader, writer)
        return json.loads(writer.write.call_args.args[0]), execute.call_count

    async def test_wire_envelope_preserves_root_and_nested_domain_ids(self):
        payload = {"status": "success", "id": "domain", "output": {"id": "nested", "timestamp": 7}}
        reply, calls = await self.client_response(2, payload)
        self.assertEqual(calls, 1)
        self.assertEqual(reply, {"version": 2, "id": "routing", "result": payload})

    async def test_old_protocol_is_rejected_before_execution(self):
        reply, calls = await self.client_response(1, {"status": "success"})
        self.assertEqual(calls, 0)
        self.assertEqual(reply["result"]["status"], "failed")
        self.assertIn("protocol 2 required", reply["result"]["output"])
