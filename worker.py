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
import select
import signal
import shutil
import tempfile
import uuid
from pathlib import Path

CONTROL = Path("/run/v3/control")
LIMIT = 1024 * 1024
CONTEXT_CONFIG_PATH = Path("/live-checkout/crates/modules/context/context-compaction.json")
ROOT = Path(os.environ.get("V3_WORKSPACE_ROOT", "/workspaces"))
SOCKET = os.environ.get("V3_TOOL_SOCKET", "/run/v3/worker/tools.sock")
# The Rust tool worker front is the only caller in the container; tests call directly.
CLIENT_UID = int(os.environ.get("V3_TOOL_CLIENT_UID", "1100"))
TEMPLATE = Path(os.environ.get("V3_CODE_TEMPLATE", "/opt/v3/source-template"))
LIVE_CHECKOUT = Path("/live-checkout")


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


GIT_CONTROL_TOOLS = ['repository_access', 'git_tool', 'resolve_conflict', 'commit_and_push', 'source_remote_read', 'source_remote_commit', 'source_site_create', 'source_site_status', 'source_preview_start', 'source_preview_stop', 'source_publish', 'source_rollback']

def git_control_package():
    """Resolve one immutable, validated Git Control package per invocation."""
    active=Path("/opt/v3/assets/current")
    if active.parent.is_dir():
        selected=active.resolve(strict=True)
        if selected.parent!=Path("/opt/v3/assets/releases") or not selected.is_dir():
            raise ValueError("Invalid Git Control release pointer")
        uuid.UUID(selected.name)
        folder=selected/"v3/plugins/abilities/Administrator/git_control"
    else:
        folder=Path(__file__).parent/"plugins/abilities/Administrator/git_control"
    for leaf in ("plugin.json", "transport.py", "adapter.py", "file_manager.py", "askpass.py", "git_control.json"):
        item=folder/leaf
        if item.is_symlink() or not item.is_file() or item.stat().st_size>2*LIMIT:
            raise ValueError("Git Control package unavailable")
    manifest=json.loads((folder/"plugin.json").read_bytes())
    if (manifest.get("version")!=1 or manifest.get("protocol")!=1 or manifest.get("interface")!=1
        or manifest.get("id")!="git_control" or manifest.get("entrypoint")!="transport.py"
        or manifest.get("tools")!=GIT_CONTROL_TOOLS or manifest.get("dispatch") is not None):
        raise ValueError("Git Control package incompatible")
    if json.loads((folder/"git_control.json").read_bytes()).get("tools")!=GIT_CONTROL_TOOLS:
        raise ValueError("Git Control catalog incompatible")
    return folder

def git_file_manager():
    """Load the File Manager Git companion from the selected package revision."""
    folder=git_control_package()
    cached=PLUGINS.get("file_manager")
    if cached is not None and cached[0]==folder:
        return cached[1]
    spec=importlib.util.spec_from_file_location("v3_file_manager",folder/"file_manager.py")
    module=importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    PLUGINS["file_manager"]=(folder,module)
    return module

def administrator_plugin(name):
    # Static ownership, not manifest-driven imports or a model supplied path.
    if name not in ("code_index", "git_control", "terminal_control", "v3_workspace", "remote_tunnel"):
        raise ValueError("unknown installed Administrator adapter")
    if name=="git_control":
        folder=git_control_package()
        cached=PLUGINS.get("git_control")
        if cached is not None and cached[0]==folder:
            return cached[1]
        spec=importlib.util.spec_from_file_location("v3_admin_git_control",folder/"transport.py")
        module=importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        PLUGINS["git_control"]=(folder,module)
        return module
    if name not in PLUGINS:
        path = (Path(__file__).parent/"build-service/workspace.py" if name=="v3_workspace" else Path(__file__).parent/"plugins/abilities/Administrator"/name/"adapter.py")
        spec = importlib.util.spec_from_file_location("v3_admin_" + name, path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        if name == "v3_workspace":
            module.configure(sys.modules[__name__])
        if name == "terminal_control":
            module.configure(network_ports)
        if name == "remote_tunnel":
            module.configure(terminal_listener_owned)
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

def terminal_listener_owned(base,port):
    if port not in network_ports(base): return False
    module=PLUGINS.get("terminal_control")
    if module is None: return False
    roots={t.process.pid for t in list(module._TERMINALS.values())
           if t.base==Path(base).resolve() and t.process.returncode is None}
    parents={}
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                raw=(entry/"stat").read_text();parents[int(entry.name)]=int(raw[raw.rfind(")")+2:].split()[1])
            except (OSError,ValueError,IndexError): pass
    owned=set(roots)
    while True:
        expanded=owned|{pid for pid,parent in parents.items() if parent in owned}
        if expanded==owned: break
        owned=expanded
    inodes=set()
    for line in Path("/proc/net/tcp").read_text().splitlines()[1:]:
        fields=line.split()
        if len(fields)>9 and fields[3]=="0A" and int(fields[1].split(":")[1],16)==port:
            if fields[1].split(":")[0] in ("0100007F","00000000"): inodes.add(fields[9])
    if not inodes:return False
    for pid in owned:
        try:
            for fd in Path(f"/proc/{pid}/fd").iterdir():
                try:
                    target=os.readlink(fd)
                    if target.startswith("socket:[") and target[8:-1] in inodes:return True
                except OSError: pass
        except OSError:pass
    return False


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



BROWSER_TOOLS=["browser_read","browser_input","browser_record"]
BROWSER_MANIFEST={"version":1,"id":"browser_control","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py",
    "tools":BROWSER_TOOLS,"build":{"inputs":["runner.py","adapter.py","egress.py"],"dependencies":[],"toolchain":"image-python3",
    "flags":["-I","-B"],"assets":["browser_control.json","browser_control.skill.md","tools"]}}
# Diagnostics' sign-in runs in the same per-session browser process.
BROWSER_FRAMES=("login_as_current_user","login_as_current_user_v3",*BROWSER_TOOLS)
def browser_package():
    """Resolve one immutable native package, checking its full declared executable inputs."""
    releases=Path("/opt/v3/assets/releases")
    selected=(releases.parent/"current").resolve(strict=True)
    if selected.parent!=releases:raise ValueError("Invalid Browser Control release pointer")
    uuid.UUID(selected.name)
    folder=selected/"v3/plugins/abilities/Web/browser_control"
    if any(p.is_symlink() for p in [folder,folder.parent,folder.parent.parent]):raise ValueError("Unsafe Browser Control package")
    hashes={}
    for name,bound in (("plugin.json",4096),("runner.py",2*LIMIT),("adapter.py",2*LIMIT),("egress.py",2*LIMIT),("browser_control.json",64*1024)):
        path=folder/name
        if path.is_symlink()or not path.is_file()or path.stat().st_size>bound:raise ValueError("Browser Control package unavailable")
        hashes[name]=hashlib.sha256(path.read_bytes()).hexdigest()
    metadata=json.loads((folder/"plugin.json").read_bytes())
    if json.dumps(metadata,sort_keys=True)!=json.dumps(BROWSER_MANIFEST,sort_keys=True):raise ValueError("Browser Control package incompatible")
    if json.loads((folder/"browser_control.json").read_bytes()).get("tools")!=BROWSER_TOOLS:raise ValueError("Browser Control catalog incompatible")
    implementation=hashlib.sha256(json.dumps({k:hashes[k]for k in metadata["build"]["inputs"]},sort_keys=True,separators=(",",":")).encode()).hexdigest()
    return folder,selected.name,implementation

class BrowserPackages:
    """Trusted worker owns global capacity and pinned session-process lifecycle."""
    def __init__(self):
        self.sessions={};self.guard=asyncio.Lock();self.reaper=None
    async def reap(self):
        """Enforce leases even when no later tool call arrives."""
        try:
            while self.sessions:
                await asyncio.sleep(1)
                for sid,item in list(self.sessions.items()):
                    if item["process"].returncode is not None or time.time()>=item["expires"] or time.monotonic()-item["touched"]>180:
                        await self.drop(sid,item)
        finally:self.reaper=None
    async def drop(self,sid,item=None):
        """Remove only the expected generation and reap its entire process group."""
        current=self.sessions.get(sid)
        if current is None or(item is not None and current is not item):return
        self.sessions.pop(sid,None)
        await terminate(current["process"])
        with contextlib.suppress(ProcessLookupError):os.killpg(current["process"].pid,signal.SIGKILL)
    async def start(self,folder,revision,implementation,base,trusted):
        """Create a package process without passing credentials through argv or environment."""
        process=await asyncio.create_subprocess_exec("/usr/bin/python3","-I","-B",str(folder/"runner.py"),
            stdin=asyncio.subprocess.PIPE,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.DEVNULL,
            limit=2*LIMIT+1,start_new_session=True,env={"PATH":"/usr/bin:/bin","LANG":"C.UTF-8"})
        return {"process":process,"revision":revision,"implementation":implementation,"lock":asyncio.Lock(),
            "authority":tuple(trusted.get(k)for k in ("session_id","user_id","agent_id","generation","cookie")),
            "expires":trusted["expires_at"],"touched":time.monotonic()}
    async def exchange(self,item,name,args,base,trusted):
        """Match bounded response IDs; a broken transport never authorizes replay."""
        invocation=str(uuid.uuid4())
        payload={"protocol":1,"invocation_id":invocation,"tool":name,"arguments":args,"base":str(base),"trusted":trusted}
        frame=json.dumps(payload,separators=(",",":")).encode()+b"\n"
        if len(frame)>2*LIMIT:raise ValueError("Browser request oversized")
        process=item["process"]
        process.stdin.write(frame);await process.stdin.drain()
        raw=await process.stdout.readline()
        if not raw.endswith(b"\n")or len(raw)>2*LIMIT:raise ValueError("Browser response oversized")
        reply=json.loads(raw)
        if type(reply.get("protocol"))is not int or reply["protocol"]!=1 or reply.get("invocation_id")!=invocation or not isinstance(reply.get("result"),dict):
            raise ValueError("Browser response incompatible")
        result=reply["result"]
        if result.get("status")not in ("success","error","failed","uncertain"):raise ValueError("Browser status incompatible")
        result.update(plugin_revision=item["revision"],implementation_sha256=item["implementation"],implementation_version=reply.get("implementation_version"))
        return result
    async def call(self,name,args,base,trusted):
        """Keep active profiles pinned; artifact reads never take a browser slot."""
        sid=Path(base).name
        if not isinstance(trusted,dict)or trusted.get("session_id")!=sid or trusted.get("allowed_session_ids")!=[sid]:
            return {"status":"error","error_kind":"browser_invalid_request","output":"Trusted browser session required"}
        if type(trusted.get("expires_at"))not in (int,float)or not time.time()<trusted["expires_at"]<=time.time()+601:
            await self.drop(sid)
            return {"status":"error","error_kind":"browser_invalid_request","output":"Browser authorization expired"}
        artifact=name=="administrator_artifact_get"
        item=None
        try:
            async with self.guard:
                identity=tuple(trusted.get(k)for k in ("session_id","user_id","agent_id","generation","cookie"))
                old=self.sessions.get(sid)
                if old is not None and(old["authority"]!=identity or old["process"].returncode is not None):
                    await self.drop(sid,old)
                item=None if artifact else self.sessions.get(sid)
                if item is None:
                    if not artifact and name=="browser_read"and args.get("action")=="close":
                        return {"status":"success","closed":True}
                    if not artifact and len(self.sessions)>=2:
                        return {"status":"error","error_kind":"browser_capacity","output":"Two private browsers are already active; close one before opening another"}
                    folder,revision,implementation=browser_package()
                    item=await self.start(folder,revision,implementation,base,trusted)
                    if not artifact:
                        self.sessions[sid]=item
                        if self.reaper is None:self.reaper=asyncio.create_task(self.reap())
            async with item["lock"]:
                item["touched"]=time.monotonic()
                result=await asyncio.wait_for(self.exchange(item,name,args,base,trusted),25)
                if result["status"]=="uncertain"or result.get("closed")or(result["status"]!="success"and name in ("login_as_current_user","login_as_current_user_v3")):
                    if not artifact:await self.drop(sid,item)
                return result
        except asyncio.CancelledError:
            if not artifact:await self.drop(sid,item)
            raise
        except Exception:
            if not artifact:await self.drop(sid,item)
            mutating=name=="browser_input" or (name=="browser_read" and args.get("action")=="navigate")
            return {"status":"uncertain"if mutating else "error","uncertain":mutating,
                "error_kind":"browser_transport_failed","output":"Browser process interrupted; inspect before retrying an input action"}
        finally:
            if artifact and item is not None:
                await terminate(item["process"])
                with contextlib.suppress(ProcessLookupError):os.killpg(item["process"].pid,signal.SIGKILL)
BROWSER_PACKAGES=BrowserPackages()

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
    if request["name"]=="browser_protocol":
        fcntl.flock(lock,fcntl.LOCK_UN)
        return {"status":"success","output":{"protocol":1,"interface":1,"stateful":True}}
    if request["name"]=="ability_plugins_protocol":
        fcntl.flock(lock,fcntl.LOCK_UN)
        return {"status":"success","output":{"protocol":1,"catalog":1,"dispatch":1}}
    if request["name"]=="codebase_admin_protocol":
        fcntl.flock(lock,fcntl.LOCK_UN)
        return {"status":"success","output":{"protocol":1,"interface":1}}
    if request["name"] in ("administrator_quiesce", "administrator_resources") and not (ROOT / str(uuid.UUID(request["workspace"]))).exists():
        return {"status": "success", "output": {"terminals": 0, "tunnels": 0}}
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
    if name == "administrator_git_watch":
        module=administrator_plugin("git_control").adapter
        value=await module.snapshot(base)
        return {"status":"success","output":module.notice(value)}
    if name in ("git_tool", "resolve_conflict", "commit_and_push"):
        return await administrator_plugin("git_control").execute(name, args, base, request.get("trusted"))
    if name == "administrator_file_manager":
        return await synchronous(git_file_manager().execute, args)
    if name == "administrator_file_git":
        return await git_file_manager().git_action(args, request.get("trusted"), administrator_plugin("git_control"))
    if name in ("terminal_open", "terminal_read", "terminal_send", "terminal_wait", "terminal_list", "terminal_close"):
        return await administrator_plugin("terminal_control").execute(name, args, base)
    if name in ("workspace_seed","source_review","source_patch","v3_workspace_status","v3_workspace_action","v3_cargo_task") and os.environ.get("V3_BUILD_EXECUTOR")!="1":
        raise PermissionError("Source workspace operations require the host build service")
    if name in ("v3_workspace_status", "v3_workspace_action"):
        return await administrator_plugin("v3_workspace").execute(name, args, base)
    if name == "v3_cargo_task":
        return await administrator_plugin("v3_workspace").cargo(base, args)
    if name in BROWSER_FRAMES or name == "administrator_artifact_get":
        return await BROWSER_PACKAGES.call(name,args,base,request.get("trusted"))
    if name == "administrator_resources":
        # Counts persistent resources that survive a conversation stop.
        terminals = PLUGINS["terminal_control"].count(base) if "terminal_control" in PLUGINS else 0
        tunnels = PLUGINS["remote_tunnel"].count(base) if "remote_tunnel" in PLUGINS else 0
        return {"status": "success", "output": {"terminals": terminals, "tunnels": tunnels}}
    if name == "administrator_quiesce":
        await stop_workspace_operations(request["workspace"])
        if "terminal_control" in PLUGINS:
            await PLUGINS["terminal_control"].close_all(base)
        await BROWSER_PACKAGES.drop(Path(base).name)
        if "remote_tunnel" in PLUGINS:
            await PLUGINS["remote_tunnel"].close_workspace(base)
        return {"status": "success"}
    if name in ("start_tunnel", "get_tunnel_url"):
        return await administrator_plugin("remote_tunnel").execute(name, args, base)
    if name == "workspace_seed":
        return await seed_workspace_async(base)
    if name == "source_review":
        return await synchronous(source_review, base)
    if name == "source_patch":
        return await synchronous(source_patch, base, args)
    raise ValueError("Unknown tool")


def git_capture(source, arguments, limit=LIMIT, acceptable=(0,)):
    """Run Git without hooks or external diff helpers and bound its output."""
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
           "HOME": str(source), "GIT_CONFIG_NOSYSTEM": "1",
           "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_OPTIONAL_LOCKS": "0",
           "GIT_NO_REPLACE_OBJECTS": "1"}
    argv = ["git", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null",
            "-c", "diff.external=", *arguments]
    import runpy
    restrict=runpy.run_path(str(Path(__file__).with_name("sandbox.py")))["restrict"]
    process = subprocess.Popen(argv, cwd=source, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT,preexec_fn=lambda:restrict(source.parent))
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
    path=Path(__file__).parent/"build-service/workspace_support.py"
    if "build_workspace_support" not in PLUGINS:
        spec=importlib.util.spec_from_file_location("build_workspace_support",path)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);PLUGINS["build_workspace_support"]=module
    module=PLUGINS["build_workspace_support"];module.configure(sys.modules.get(__name__) or __import__("types").SimpleNamespace(**globals()));return module
def source_review(base):return workspace_support().source_review(base)
def source_patch(base,args):return workspace_support().source_patch(base,args)
def _seed_capture(*args):return workspace_support()._seed_capture(*args)
def _seed_inner(spec):return workspace_support()._seed_inner(spec)
def seed_workspace(base):return workspace_support().seed_workspace(base)
async def seed_workspace_async(base):return await workspace_support().seed_workspace_async(base)

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
    if len(argv) == 5 and argv[1:4] == ["-I", str(Path(__file__).resolve()), "--seed-inner"]:
        spec = json.loads(argv[4])
        if set(spec) not in ({"base", "template", "stage"}, {"base", "template", "stage", "published"}) or type(spec.get("published",False)) is not bool or Path(spec["base"]).resolve() != Path.cwd() or not spec["stage"].startswith(".v3-seed-") or "/" in spec["stage"]:
            raise ValueError("invalid trusted seed invocation")
        read_paths = (Path(spec["template"]).parent if (Path(spec["template"]).parent/".code-index").is_dir() else Path(spec["template"]), Path(__file__).resolve(), Path(__file__).with_name("sandbox.py").resolve(),
                      Path(__file__).parent/"plugins/abilities/Administrator/code_index/adapter.py",Path(__file__).parent/"build-service")
    if len(argv)==5 and argv[1:4]==["-I",str(Path(__file__).resolve()),"--index-inner"]:
        spec=json.loads(argv[4])
        if set(spec)!={"base","name","arguments"}or Path(spec["base"]).resolve()!=Path.cwd():raise ValueError("Invalid trusted index invocation")
        read_paths=(Path(__file__).resolve(),Path(__file__).with_name("sandbox.py").resolve(),Path(__file__).parent/"plugins/abilities/Administrator/code_index",Path(__file__).parent/"build-service")
    # Codebase Admin commands read the whole container, including state, and write only this
    # workspace. Checkout writes go through checkout_edit or broker tasks, never commands.
    # Internal seed and index invocations keep their narrow grants.
    if not read_paths:
        read_paths=(Path("/"),)
    child = subprocess.Popen(argv, stdin=subprocess.DEVNULL,
                             preexec_fn=lambda: restrict(Path.cwd(), read_paths=read_paths,deny_unix=True))
    try:
        result = child.wait()
    finally:
        clean_descendants()
    raise SystemExit(result if result >= 0 else 128 - result)


workspace_support()

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
    elif len(sys.argv) == 3 and sys.argv[1] == "--seed-inner":
        try:
            print(json.dumps(_seed_inner(json.loads(sys.argv[2]))))
        except Exception as error:
            print(json.dumps({"error": str(error) if isinstance(error, ValueError) else "isolated seed rejected"}))
    else:
        asyncio.run(main())
