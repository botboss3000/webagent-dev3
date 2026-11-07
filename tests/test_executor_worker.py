"""The workspace executor's source operations (seed, review, patch), through the broker's
own build-service/executor_worker.py as the executor container runs them."""
import asyncio
import fcntl
import hashlib
import importlib.util
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "executor_worker", Path(__file__).parents[1] / "build-service/executor_worker.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class ExecutorWorkerTests(unittest.IsolatedAsyncioTestCase):
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

    def workspace_path(self):
        path = worker.ROOT / self.session
        path.mkdir(mode=0o770, exist_ok=True)
        return path

    async def run_tool(self, name, args, timeout=120):
        """As the executor runs one request: admission under the dispatch lock, then the effect."""
        base = self.workspace_path()
        with (worker.CONTROL / "dispatch.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if (worker.CONTROL / "shutoff").read_text().strip() != "0":
                raise ValueError("execution disabled")
        if name == "workspace_seed":
            return await worker.seed_workspace_async(base)
        if name == "source_review":
            return await asyncio.to_thread(worker.source_review, base)
        return await asyncio.to_thread(worker.source_patch, base, args)

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
        base = self.workspace_path()
        outside = Path(self.tmp.name) / "outside"; outside.mkdir()
        (outside / "main.rs").write_text("OUTSIDE")
        script = r"""
import importlib.util,runpy,sys,json,shutil
from pathlib import Path
workerpath,base,template,outside=sys.argv[1:]
spec=importlib.util.spec_from_file_location("seedworker",workerpath)
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
# Load the source operations before the sandbox closes the build-service folder.
m.workspace_support()
real=shutil.copytree
def race(src,dst,*args,**kwargs):
    if Path(src)==Path(template):
        (Path(dst)/"src").symlink_to(outside,target_is_directory=True)
    return real(src,dst,*args,**kwargs)
shutil.copytree=race
runpy.run_path(str(Path(workerpath).parent.parent/"sandbox.py"))["restrict"](base,read_paths=(template,))
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
        base = self.workspace_path()
        outside = Path(self.tmp.name) / "hook-outside"
        script = r"""
import importlib.util,runpy,sys,subprocess
from pathlib import Path
workerpath,base,template,outside=sys.argv[1:]
spec=importlib.util.spec_from_file_location("seedworker",workerpath)
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
# Load the source operations before the sandbox closes the build-service folder.
m.workspace_support()
real=subprocess.Popen
stage=Path(base)/".v3-seed-fixture"
def race(argv,*args,**kwargs):
    if "commit" in argv:
        hook=stage/".git/hooks/pre-commit"
        hook.write_text("#!/bin/sh\nprintf UNSAFE > "+outside+"\n")
        hook.chmod(0o755)
    return real(argv,*args,**kwargs)
m.subprocess.Popen=race
runpy.run_path(str(Path(workerpath).parent.parent/"sandbox.py"))["restrict"](base,read_paths=(template,))
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

    async def test_workspace_seed_skips_engine_artifacts_only(self):
        # Releases carry compiled engine services over the per-file limit; they are
        # build outputs, so the seed leaves them out. Large source still fails.
        artifact = worker.TEMPLATE / "plugins/engine/x/artifact/service"
        artifact.parent.mkdir(parents=True)
        artifact.write_bytes(b"x" * (worker.LIMIT + 100_000))
        result = await self.run_tool("workspace_seed", {})
        self.assertEqual(result["status"], "success")
        path = worker.ROOT / self.session / "webagent-v3"
        self.assertTrue((path / "src/main.rs").exists())
        self.assertFalse((path / "plugins/engine/x/artifact").exists())

    async def test_workspace_seed_refuses_large_source_file(self):
        large = worker.TEMPLATE / "plugins/engine/x/src/big.rs"
        large.parent.mkdir(parents=True)
        large.write_bytes(b"x" * (worker.LIMIT + 100_000))
        with self.assertRaisesRegex(ValueError, "exceeds limit"):
            await self.run_tool("workspace_seed", {})
        self.assertFalse((worker.ROOT / self.session / "webagent-v3").exists())



if __name__ == "__main__":
    unittest.main()
