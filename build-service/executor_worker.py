"""The workspace executor's own helpers: the Cargo/seed supervisor, bounded Git capture and
the trusted source operations. The broker runs these inside disposable workspace executors
and never imports the app's worker.py, so an app release cannot change them."""
import asyncio
import contextlib
import ctypes
import importlib.util
import json
import os
import select
import signal
import subprocess
import sys
import time
import types
from pathlib import Path

SERVICE = Path(__file__).resolve().parent
# The app tree the executor runs in: /opt/v3 in the image, the checkout in tests.
APP = SERVICE.parent
CONTROL = Path("/runtime/control")
LIMIT = 1024 * 1024
ROOT = Path(os.environ.get("V3_WORKSPACE_ROOT", "/workspaces"))
TEMPLATE = Path(os.environ.get("V3_CODE_TEMPLATE", "/opt/v3/source-template"))
PLUGINS = {}


def this():
    """This module as its users see it, also when it was loaded from a file path."""
    return sys.modules.get(__name__) or types.SimpleNamespace(**globals())


def restrictor():
    import runpy
    return runpy.run_path(str(APP / "sandbox.py"))["restrict"]


def administrator_plugin(name):
    # Static ownership, not manifest-driven imports or a model supplied path.
    if name not in ("code_index", "v3_workspace"):
        raise ValueError("unknown installed Administrator adapter")
    if name not in PLUGINS:
        path = (SERVICE / "workspace.py" if name == "v3_workspace"
                else APP / "plugins/abilities/Administrator" / name / "adapter.py")
        spec = importlib.util.spec_from_file_location("v3_admin_" + name, path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        if name == "v3_workspace":
            module.configure(this())
        PLUGINS[name] = module
    return PLUGINS[name]


async def terminate(process):
    if process.returncode is None:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGTERM)
        try:
            await asyncio.wait_for(process.wait(), 5)
        except asyncio.TimeoutError:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            await process.wait()


def git_capture(source, arguments, limit=LIMIT, acceptable=(0,)):
    """Run Git without hooks or external diff helpers and bound its output."""
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
           "HOME": str(source), "GIT_CONFIG_NOSYSTEM": "1",
           "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_OPTIONAL_LOCKS": "0",
           "GIT_NO_REPLACE_OBJECTS": "1"}
    argv = ["git", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null",
            "-c", "diff.external=", *arguments]
    restrict = restrictor()
    process = subprocess.Popen(argv, cwd=source, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, preexec_fn=lambda: restrict(source.parent))
    output = bytearray()
    deadline = time.monotonic() + 30
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([process.stdout], [], [], remaining)[0]:
                raise ValueError("source review timed out")
            chunk = os.read(process.stdout.fileno(), min(65536, limit + 1 - len(output)))
            if not chunk:
                break
            output.extend(chunk)
            if len(output) > limit:
                return bytes(output[:limit]), True
        if process.wait(timeout=1) not in acceptable:
            raise ValueError("source review Git command failed")
        return bytes(output), False
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        process.stdout.close()


def workspace_support():
    if "build_workspace_support" not in PLUGINS:
        spec = importlib.util.spec_from_file_location("build_workspace_support", SERVICE / "workspace_support.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        PLUGINS["build_workspace_support"] = module
    module = PLUGINS["build_workspace_support"]
    module.configure(this())
    return module


def source_review(base): return workspace_support().source_review(base)
def source_patch(base, args): return workspace_support().source_patch(base, args)
def seed_workspace(base): return workspace_support().seed_workspace(base)
async def seed_workspace_async(base): return await workspace_support().seed_workspace_async(base)
def _seed_inner(spec): return workspace_support()._seed_inner(spec)


def descendants():
    parents = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            # Read only public process metadata, never environ or session data.
            text = (entry / "stat").read_text()
            fields = text[text.rfind(")") + 2:].split()
            parents[int(entry.name)] = int(fields[1])
        except (OSError, ValueError, IndexError):
            continue
    found = set()
    changed = True
    while changed:
        changed = False
        for pid, parent in parents.items():
            if pid not in found and (parent == os.getpid() or parent in found):
                found.add(pid)
                changed = True
    return found


def clean_descendants():
    deadline = time.monotonic() + 3
    while True:
        children = descendants()
        if not children:
            break
        signum = signal.SIGTERM if time.monotonic() < deadline else signal.SIGKILL
        for pid in children:
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signum)
        while True:
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
                if pid == 0:
                    break
            except ChildProcessError:
                break
        time.sleep(0.05)
        if time.monotonic() > deadline + 1:
            break


def runner(argv):
    """Run one Cargo job or seed copy confined to the workspace; reap everything it starts."""
    # Orphaned/double-forked children reparent here, rather than container init.
    if ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) != 0:
        raise RuntimeError("cannot enable child subreaper")

    def stop(_signum, _frame):
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        clean_descendants()
        raise SystemExit(143)

    signal.signal(signal.SIGTERM, stop)
    expected_parent = int(os.environ.get("V3_SUPERVISOR_PARENT", str(os.getppid())))
    if ctypes.CDLL(None).prctl(1, signal.SIGTERM, 0, 0, 0) != 0:
        raise RuntimeError("supervisor parent death guard unavailable")
    if os.getppid() != expected_parent:
        stop(signal.SIGTERM, None)
    # Keep the supervisor outside the restricted domain so cancellation can
    # reap double-forked children. Only the executed child is sandboxed.
    restrict = restrictor()
    read_paths = ()
    if len(argv) == 5 and argv[1:4] == ["-I", str(Path(__file__).resolve()), "--seed-inner"]:
        spec = json.loads(argv[4])
        if set(spec) not in ({"base", "template", "stage"}, {"base", "template", "stage", "published"}) or type(spec.get("published", False)) is not bool or Path(spec["base"]).resolve() != Path.cwd() or not spec["stage"].startswith(".v3-seed-") or "/" in spec["stage"]:
            raise ValueError("invalid trusted seed invocation")
        read_paths = (Path(spec["template"]).parent if (Path(spec["template"]).parent / ".code-index").is_dir() else Path(spec["template"]),
                      SERVICE, (APP / "sandbox.py").resolve(),
                      APP / "plugins/abilities/Administrator/code_index/adapter.py")
    # Cargo reads system folders and the checkout's source files and writes only this
    # workspace; state, other workspaces, control sockets and checkout runtime folders stay out.
    if not read_paths:
        # live-checkout is the image's compatibility link to /code; granting it grants all of /code.
        hidden = {"state", "workspaces", "code", "live-checkout", "runtime", "run"}
        private = {"data", "workspaces", "target", "build-jobs", "deployment-secrets", "runtime", "rollback", "gate-runtime"}
        checkout = Path("/code")
        read_paths = tuple(p for p in Path("/").iterdir() if p.name not in hidden) + (tuple(p for p in checkout.iterdir() if p.name not in private) if checkout.is_dir() else ())
    child = subprocess.Popen(argv, stdin=subprocess.DEVNULL,
                             preexec_fn=lambda: restrict(Path.cwd(), read_paths=read_paths, deny_unix=True))
    try:
        result = child.wait()
    finally:
        clean_descendants()
    raise SystemExit(result if result >= 0 else 128 - result)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--runner":
        runner(json.loads(sys.argv[2]))
    elif len(sys.argv) == 3 and sys.argv[1] == "--seed-inner":
        try:
            print(json.dumps(workspace_support()._seed_inner(json.loads(sys.argv[2]))))
        except Exception as error:
            print(json.dumps({"error": str(error) if isinstance(error, ValueError) else "isolated seed rejected"}))
    else:
        raise SystemExit("usage: executor_worker.py --runner ARGV | --seed-inner SPEC")
