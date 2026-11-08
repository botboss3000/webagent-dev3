"""Isolated v3 tool worker. No application credentials are loaded here."""
import asyncio
import ctypes
import contextlib
import hashlib
import fcntl
import socket
import struct
import subprocess
import sys
import time
import json
import importlib.util
import os
import signal
import shutil
import tempfile
import uuid
from pathlib import Path

CONTROL = Path("/runtime/control")
LIMIT = 1024 * 1024
CONTEXT_CONFIG_PATH = Path("/code/crates/modules/context/context-compaction.json")
ROOT = Path(os.environ.get("V3_WORKSPACE_ROOT", "/workspaces"))
SOCKET = os.environ.get("V3_TOOL_SOCKET", "/runtime/worker/tools.sock")
# The Rust tool worker front is the only caller in the container; tests call directly.
CLIENT_UID = int(os.environ.get("V3_TOOL_CLIENT_UID", "1100"))
LIVE_CHECKOUT = Path("/code")


def search_limits():
    """The live compaction file (live-files.json) is read on every search, without a restart."""
    try:
        config = json.loads(CONTEXT_CONFIG_PATH.read_text())
    except FileNotFoundError:
        config = {}
    output = config.get("search_output_bytes", 32768)
    line = config.get("search_line_bytes", 2048)
    if (type(output) is not int or not 4096 <= output <= 262144
            or type(line) is not int or not 128 <= line <= 8192 or line > output):
        raise ValueError("invalid context search limits")
    return output, line


PLUGINS = {}
ACTIVE_OPERATIONS = {}

async def synchronous(function, *args):
    # Cancellation must wait for bounded synchronous writes to settle before
    # acknowledging quiescence; cancelling to_thread alone leaves work running.
    task = asyncio.create_task(asyncio.to_thread(function, *args))
    try:
        return await asyncio.shield(task)
    except asyncio.CancelledError:
        while not task.done():
            try:
                await asyncio.shield(task)
            except asyncio.CancelledError:
                continue
            except Exception:
                break
        raise

async def stop_workspace_operations(workspace):
    current = asyncio.current_task()
    tasks = [task for task, owner in list(ACTIVE_OPERATIONS.items())
             if owner == workspace and task is not current]
    for task in tasks:
        task.cancel()
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=True)


def administrator_plugin(name):
    # Static ownership, not manifest-driven imports or a model supplied path.
    if name != "code_index":
        raise ValueError("unknown installed Administrator adapter")
    if name not in PLUGINS:
        path = Path(__file__).parent/"plugins/abilities/Administrator"/name/"adapter.py"
        spec = importlib.util.spec_from_file_location("v3_admin_" + name, path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        PLUGINS[name] = module
    return PLUGINS[name]


PORTS = {}

def network_ports(base):
    key = str(Path(base).resolve())
    directory = CONTROL.parent / "worker" / "port-claims"
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    lock_path = directory / "lock"
    with lock_path.open("a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = directory / (hashlib.sha256(key.encode()).hexdigest() + ".json")
        if path.exists():
            ports = json.loads(path.read_text())
            if not isinstance(ports,list) or len(ports)!=4 or any(type(p)!=int or not 20000<=p<=29999 for p in ports):
                raise ValueError("invalid workspace port claims")
            PORTS[key] = ports
            return ports
        claimed = set()
        for claim in directory.glob("*.json"):
            claimed.update(json.loads(claim.read_text()))
        ports = []
        for port in range(20000,30000):
            if port in claimed:
                continue
            with socket.socket() as listener:
                try: listener.bind(("127.0.0.1",port))
                except OSError: continue
            ports.append(port)
            if len(ports)==4: break
        if len(ports)!=4: raise ValueError("workspace preview ports exhausted")
        fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
        with os.fdopen(fd,"w") as file:
            json.dump(ports,file);file.flush();os.fsync(file.fileno())
        PORTS[key] = ports
        return ports

def workspace_path(session):
    if str(uuid.UUID(session)) != session:
        raise ValueError("invalid workspace identifier")
    root = ROOT.resolve()
    path = root / session
    path.mkdir(mode=0o770, exist_ok=True)
    if path.is_symlink() or path.resolve().parent != root:
        raise ValueError("invalid workspace")
    return path


def scoped(base, value, create=False):
    path = (base / value).resolve()
    if path != base and base not in path.parents:
        raise ValueError("path escapes workspace")
    if create:
        path.parent.mkdir(parents=True, exist_ok=True)
    return path


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


async def command(argv, base, timeout, lock):
    env = {"PATH": "/opt/rust/bin:/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8",
           "HOME": str(base), "TMPDIR": str(base), "CARGO_HOME": str(base / ".cargo"),
           "CARGO_BUILD_JOBS": "1", "CARGO_INCREMENTAL": "0",
           "RUSTFLAGS": "-C debuginfo=0",
           "V3_ALLOWED_LISTEN_PORTS": ",".join(map(str, network_ports(base)))}
    process = await asyncio.create_subprocess_exec(
        "/usr/bin/python3", "-I", str(Path(__file__).resolve()), "--runner", json.dumps(argv),
        cwd=base,
        env=env, start_new_session=True,
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT)
    fcntl.flock(lock, fcntl.LOCK_UN)
    data = bytearray()
    truncated = False

    async def drain():
        nonlocal truncated
        while chunk := await process.stdout.read(65536):
            remaining = LIMIT - len(data)
            data.extend(chunk[:max(remaining, 0)])
            truncated |= len(chunk) > remaining
        await process.wait()

    status = "success"
    try:
        await asyncio.wait_for(drain(), timeout)
        if process.returncode != 0:
            status = "failed"
    except asyncio.TimeoutError:
        status = "timeout"
        await terminate(process)
    except asyncio.CancelledError:
        await terminate(process)
        raise
    finally:
        # Kill descendants even if the immediate shell exited.
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
    return {"status": status, "output": data.decode(errors="replace"),
            "exit_code": process.returncode, "truncated": truncated}


async def execute_locked(request, lock):
    """Route approved requests to fixed native adapters or validated isolated packages; release the dispatch lock before effects."""
    if request["name"] == "core_visual_capture":
        fcntl.flock(lock, fcntl.LOCK_UN)
        path = Path(__file__).parent / "plugins/abilities/Core/visualizer/capture.py"
        process = await asyncio.create_subprocess_exec("/usr/bin/python3", "-I", str(path),
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
            env={"PATH":"/usr/bin:/bin","LANG":"C.UTF-8","HOME":"/tmp","TMPDIR":"/tmp"},start_new_session=True)
        try:
            raw = json.dumps(request.get("arguments", {})).encode()
            if len(raw) > 786432:raise ValueError("Capture input too large")
            output, _ = await asyncio.wait_for(process.communicate(raw), 25)
            if len(output) > 1900000:raise ValueError("Capture output too large")
            return json.loads(output)
        finally:
            await terminate(process)
            # The browser group may outlive its completed capture-process leader.
            with contextlib.suppress(ProcessLookupError):os.killpg(process.pid,signal.SIGKILL)
    if request["name"]=="ability_plugins_protocol":
        fcntl.flock(lock,fcntl.LOCK_UN)
        return {"status":"success","output":{"protocol":1,"catalog":1,"dispatch":1}}
    if request["name"] == "administrator_quiesce" and not (ROOT / str(uuid.UUID(request["workspace"]))).exists():
        return {"status": "success"}
    base = workspace_path(request["workspace"])
    args = request.get("arguments", {})
    name = request["name"]
    # The lock covers dispatch only; controls never wait for tool completion.
    fcntl.flock(lock, fcntl.LOCK_UN)
    if name.startswith("code_index_") and name in {
            "code_index_scan", "code_index_lookup", "code_index_coverage",
            "code_index_trace", "code_index_progress", "code_index_maintenance"}:
        module = administrator_plugin("code_index")
        if args.get("root") == "published":
            # Resolve once; concurrent activation cannot mix source and index.
            selected = Path("/opt/v3/assets/current").resolve(strict=True)
            if selected.parent != Path("/opt/v3/assets/releases") or not selected.is_dir():
                raise ValueError("Published release unavailable")
            uuid.UUID(selected.name)
            return await synchronous(module.execute_published, name, args, selected)
        # A scan can run for tens of seconds and publish an index. Execute it
        # under the existing subprocess supervisor so cancellation reaps it.
        spec = json.dumps({"base":str(base),"name":name,"arguments":args})
        result = await command([sys.executable, "-I", str(Path(__file__).resolve()),
                                "--index-inner", spec], base, 60, lock)
        if result["status"] != "success":
            return result
        return json.loads(result["output"])
    if name == "administrator_quiesce":
        await stop_workspace_operations(request["workspace"])
        return {"status": "success"}
    raise ValueError("Unknown tool")


async def execute(request):
    task = asyncio.current_task()
    ACTIVE_OPERATIONS[task] = request["workspace"]
    try:
        with (CONTROL / "dispatch.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if (CONTROL / "shutoff").read_text().strip() != "0":
                raise ValueError("execution disabled")
            return await execute_locked(request, lock)
    finally:
        ACTIVE_OPERATIONS.pop(task, None)


async def client(reader, writer):
    peer = writer.get_extra_info("socket").getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
    if struct.unpack("3i", peer)[1] != CLIENT_UID:
        writer.close()
        await writer.wait_closed()
        return
    task = None
    disconnected = None
    request = {}
    try:
        raw = await asyncio.wait_for(reader.readline(), 10)
        request = json.loads(raw)
        if request.get("version") != 2:
            raise ValueError("Worker RPC protocol 2 required; no operation dispatched")
        task = asyncio.create_task(execute(request))
        disconnected = asyncio.create_task(reader.read(1))
        done, _ = await asyncio.wait([task, disconnected], return_when=asyncio.FIRST_COMPLETED)
        if disconnected in done:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task
            writer.close()
            await writer.wait_closed()
            return
        response = task.result()
    except asyncio.CancelledError:
        response = {"status": "failed", "output": "Session not found"}
    except Exception as error:
        response = {"status": "failed", "output": str(error)[:4096]}
    finally:
        if disconnected:
            disconnected.cancel()
    try:
        envelope = {"version": 2, "id": request.get("id"), "result": response}
        writer.write((json.dumps(envelope) + "\n").encode())
        await writer.drain()
    except (BrokenPipeError, ConnectionResetError):
        pass
    finally:
        writer.close()
        await writer.wait_closed()


async def main():
    with contextlib.suppress(FileNotFoundError):
        os.unlink(SOCKET)
    server = await asyncio.start_unix_server(client, SOCKET, limit=2 * LIMIT)
    os.chmod(SOCKET, 0o660)
    async with server:
        await server.serve_forever()


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
    import runpy
    restrict = runpy.run_path(str(Path(__file__).with_name("sandbox.py")))["restrict"]
    read_paths = ()
    if len(argv)==5 and argv[1:4]==["-I",str(Path(__file__).resolve()),"--index-inner"]:
        spec=json.loads(argv[4])
        if set(spec)!={"base","name","arguments"}or Path(spec["base"]).resolve()!=Path.cwd():raise ValueError("Invalid trusted index invocation")
        read_paths=(Path(__file__).resolve(),Path(__file__).with_name("sandbox.py").resolve(),Path(__file__).parent/"plugins/abilities/Administrator/code_index",Path(__file__).parent/"build-service")
    # Commands read system folders and the checkout's source files and write only this
    # workspace; state, other workspaces, control sockets and checkout runtime folders stay
    # out, as in the Rust runner. Internal index invocations keep their narrow grants.
    if not read_paths:
        # live-checkout is the image's compatibility link to /code; granting it grants all of /code.
        hidden={"state","workspaces","code","live-checkout","runtime","run"}
        private={"data","workspaces","target","build-jobs","deployment-secrets","runtime","rollback","gate-runtime"}
        checkout=Path("/code")
        read_paths=tuple(p for p in Path("/").iterdir() if p.name not in hidden)+(tuple(p for p in checkout.iterdir() if p.name not in private) if checkout.is_dir() else ())
    child = subprocess.Popen(argv, stdin=subprocess.DEVNULL,
                             preexec_fn=lambda: restrict(Path.cwd(), read_paths=read_paths,deny_unix=True))
    try:
        result = child.wait()
    finally:
        clean_descendants()
    raise SystemExit(result if result >= 0 else 128 - result)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--runner":
        runner(json.loads(sys.argv[2]))
    elif len(sys.argv) == 3 and sys.argv[1] == "--index-inner":
        spec = json.loads(sys.argv[2])
        try:
            result = administrator_plugin("code_index").execute(
                spec["name"], spec["arguments"], Path(spec["base"]))
        except Exception as error:
            result = {"status":"failed", "output":str(error)[:4096]}
        print(json.dumps(result))
    else:
        asyncio.run(main())
