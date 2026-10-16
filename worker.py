"""Isolated v3 tool worker. No application credentials are loaded here."""
import asyncio
import ctypes
import contextlib
import fnmatch
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
import stat
import shutil
import tempfile
import uuid
from pathlib import Path

CONTROL = Path("/run/v3/control")
LIMIT = 1024 * 1024
CONTEXT_CONFIG_PATH = Path("/state/config/context-compaction.json")
ROOT = Path(os.environ.get("V3_WORKSPACE_ROOT", "/workspaces"))
SOCKET = os.environ.get("V3_TOOL_SOCKET", "/run/v3/worker/tools.sock")
TEMPLATE = Path(os.environ.get("V3_CODE_TEMPLATE", "/opt/v3/source-template"))
LIVE_CHECKOUT = Path("/live-checkout")


def search_limits():
    """The validated data-root config is read on every search, without a restart."""
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


GIT_CONTROL_TOOLS = ["repository_access", "git_tool", "resolve_conflict", "commit_and_push"]

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
    if name not in ("code_index", "git_control", "terminal_control", "v3_workspace", "diagnostics", "remote_tunnel"):
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
           "V3_ALLOWED_LISTEN_PORTS": ",".join(map(str, network_ports(base))),
           "V3_COMMAND_SOURCE_ROOT":str(LIVE_CHECKOUT)}
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



CODEBASE_TOOLS = ["file_list","file_read","file_search","file_edit","file_write","build_work",
                 "state_inspect","prompt_slot_read","prompt_slot_write","shell","python"]
def codebase_package(revision=None):
    active=Path("/opt/v3/assets/current")
    if active.parent.is_dir():
        selected=(active.parent/"releases"/str(uuid.UUID(revision))).resolve(strict=True) if revision else active.resolve(strict=True)
        if selected.parent!=Path("/opt/v3/assets/releases"):raise ValueError("Invalid plugin release pointer")
        folder=selected/"v3/plugins/abilities/Administrator/codebase_admin"
    else:folder=Path(__file__).parent/"plugins/abilities/Administrator/codebase_admin"
    for leaf in ("plugin.json","runner.py","codebase_admin.json"):
        path=folder/leaf
        if path.is_symlink() or not path.is_file() or path.stat().st_size>2*LIMIT:raise ValueError("Codebase Admin package unavailable")
    metadata=json.loads((folder/"plugin.json").read_bytes())
    if (metadata.get("version")!=1 or metadata.get("protocol")!=1 or metadata.get("id")!="codebase_admin"
        or metadata.get("interface")!=1 or metadata.get("runtime")!="python3" or metadata.get("entrypoint")!="runner.py"
        or metadata.get("tools")!=CODEBASE_TOOLS):raise ValueError("Codebase Admin package incompatible")
    if json.loads((folder/"codebase_admin.json").read_bytes()).get("tools")!=CODEBASE_TOOLS:raise ValueError("Codebase Admin catalog incompatible")
    return folder,hashlib.sha256((folder/"runner.py").read_bytes()).hexdigest()
async def codebase_call(name,args,base,operation="execute",revision=None):
    folder,generation=codebase_package(revision)
    request=json.dumps({"version":1,"name":name,"arguments":args,"operation":operation,"base":str(base)}).encode()
    if len(request)>8*LIMIT:raise ValueError("Codebase Admin request oversized")
    process=await asyncio.create_subprocess_exec("/usr/bin/python3","-I","-B",str(folder/"runner.py"),
        stdin=asyncio.subprocess.PIPE,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.DEVNULL,
        start_new_session=True,env={"PATH":"/usr/bin:/bin","LANG":"C.UTF-8"})
    async def exchange():
        process.stdin.write(request);await process.stdin.drain();process.stdin.close()
        chunks=[];size=0
        while chunk:=await process.stdout.read(65536):
            size+=len(chunk)
            if size>8*LIMIT:raise ValueError("Codebase Admin response oversized")
            chunks.append(chunk)
        await process.wait()
        if process.returncode:raise ValueError("Codebase Admin process failed")
        reply=json.loads(b"".join(chunks))
        if reply.get("version")!=1 or not isinstance(reply.get("result"),dict):raise ValueError("Codebase Admin response incompatible")
        result=reply["result"];result["plugin_revision"]=folder.parents[4].name if folder.parents[4].parent.name=="releases" else None;result["implementation_sha256"]=generation
        return result
    try:return await asyncio.wait_for(exchange(),30)
    except (OSError,ValueError,asyncio.TimeoutError):
        if name in ("file_write","file_edit") and operation=="execute":
            return {"status":"uncertain","uncertain":True,"output":"Plugin interrupted; reconcile file contents before retry"}
        raise
    finally:
        await terminate(process)
        with contextlib.suppress(ProcessLookupError):os.killpg(process.pid,signal.SIGKILL)


DIAGNOSTICS_TOOLS=["read_diagnostics","login_as_current_user","dev_browser_url","dev_browser_action","dev_browser_record"]
DIAGNOSTICS_MANIFEST={"version":1,"id":"diagnostics","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py",
    "tools":DIAGNOSTICS_TOOLS,"build":{"inputs":["runner.py","adapter.py"],"dependencies":[],"toolchain":"image-python3",
    "flags":["-I","-B"],"assets":["diagnostics.json","diagnostics.skill.md","tools","skills"]}}
def diagnostics_package():
    """Resolve one immutable native package, checking its full declared executable inputs."""
    releases=Path("/opt/v3/assets/releases")
    selected=(releases.parent/"current").resolve(strict=True)
    if selected.parent!=releases:raise ValueError("Invalid Diagnostics release pointer")
    uuid.UUID(selected.name)
    folder=selected/"v3/plugins/abilities/Administrator/diagnostics"
    if any(p.is_symlink() for p in [folder,folder.parent,folder.parent.parent]):raise ValueError("Unsafe Diagnostics package")
    hashes={}
    for name,bound in (("plugin.json",4096),("runner.py",2*LIMIT),("adapter.py",2*LIMIT),("diagnostics.json",64*1024)):
        path=folder/name
        if path.is_symlink()or not path.is_file()or path.stat().st_size>bound:raise ValueError("Diagnostics package unavailable")
        hashes[name]=hashlib.sha256(path.read_bytes()).hexdigest()
    metadata=json.loads((folder/"plugin.json").read_bytes())
    if json.dumps(metadata,sort_keys=True)!=json.dumps(DIAGNOSTICS_MANIFEST,sort_keys=True):raise ValueError("Diagnostics package incompatible")
    if json.loads((folder/"diagnostics.json").read_bytes()).get("tools")!=DIAGNOSTICS_TOOLS:raise ValueError("Diagnostics catalog incompatible")
    implementation=hashlib.sha256(json.dumps({k:hashes[k]for k in metadata["build"]["inputs"]},sort_keys=True,separators=(",",":")).encode()).hexdigest()
    return folder,selected.name,implementation

class DiagnosticsPackages:
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
        if len(frame)>2*LIMIT:raise ValueError("Diagnostics request oversized")
        process=item["process"]
        process.stdin.write(frame);await process.stdin.drain()
        raw=await process.stdout.readline()
        if not raw.endswith(b"\n")or len(raw)>2*LIMIT:raise ValueError("Diagnostics response oversized")
        reply=json.loads(raw)
        if type(reply.get("protocol"))is not int or reply["protocol"]!=1 or reply.get("invocation_id")!=invocation or not isinstance(reply.get("result"),dict):
            raise ValueError("Diagnostics response incompatible")
        result=reply["result"]
        if result.get("status")not in ("success","error","failed","uncertain"):raise ValueError("Diagnostics status incompatible")
        result.update(plugin_revision=item["revision"],implementation_sha256=item["implementation"],implementation_version=reply.get("implementation_version"))
        return result
    async def call(self,name,args,base,trusted):
        """Keep active profiles pinned; artifact reads never take a browser slot."""
        sid=Path(base).name
        if not isinstance(trusted,dict)or trusted.get("session_id")!=sid or trusted.get("allowed_session_ids")!=[sid]:
            return {"status":"error","error_kind":"browser_invalid_request","output":"Trusted Diagnostics session required"}
        if type(trusted.get("expires_at"))not in (int,float)or not time.time()<trusted["expires_at"]<=time.time()+601:
            await self.drop(sid)
            return {"status":"error","error_kind":"browser_invalid_request","output":"Diagnostics authorization expired"}
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
                    if not artifact and name=="dev_browser_action"and args.get("action")=="close":
                        return {"status":"success","closed":True}
                    if not artifact and len(self.sessions)>=2:
                        return {"status":"error","error_kind":"browser_capacity","output":"Two private Diagnostics browsers are already active; close one before opening another"}
                    folder,revision,implementation=diagnostics_package()
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
            mutating=name=="dev_browser_action"and args.get("action")in("click","type","navigate")
            return {"status":"uncertain"if mutating else "error","uncertain":mutating,
                "error_kind":"browser_transport_failed","output":"Diagnostics process interrupted; inspect before retrying an input action"}
        finally:
            if artifact and item is not None:
                await terminate(item["process"])
                with contextlib.suppress(ProcessLookupError):os.killpg(item["process"].pid,signal.SIGKILL)
DIAGNOSTICS_PACKAGES=DiagnosticsPackages()

async def execute_locked(request, lock):
    """Route approved requests to fixed native adapters or validated isolated packages; release the dispatch lock before effects."""
    if request["name"]=="diagnostics_protocol":
        fcntl.flock(lock,fcntl.LOCK_UN)
        return {"status":"success","output":{"protocol":1,"interface":1,"stateful":True}}
    if request["name"]=="ability_plugin_invoke":
        binding=(request.get("trusted")or{}).get("ability_plugin")
        if not isinstance(binding,dict):raise ValueError("Trusted plugin binding required")
        base=workspace_path(request["workspace"])
        fcntl.flock(lock,fcntl.LOCK_UN)
        import runpy
        bridge=runpy.run_path(str(Path(__file__).with_name("ability_plugin_worker.py")))
        return await bridge["invoke"](binding,request.get("arguments",{}),base,terminate)
    if request["name"]=="ability_plugins_protocol":
        fcntl.flock(lock,fcntl.LOCK_UN)
        return {"status":"success","output":{"protocol":1,"catalog":1,"dispatch":1}}
    if request["name"]=="codebase_admin_protocol":
        fcntl.flock(lock,fcntl.LOCK_UN)
        return {"status":"success","output":{"protocol":1,"interface":1}}
    if request["name"] == "administrator_quiesce" and not (ROOT / str(uuid.UUID(request["workspace"]))).exists():
        return {"status": "success"}
    base = workspace_path(request["workspace"])
    args = request.get("arguments", {})
    name = request["name"]
    if name in ("shell","python"):
        fcntl.flock(lock,fcntl.LOCK_UN)
        planned=await codebase_call(name,args,base,"plan",revision=(request.get("trusted")or{}).get("codebase_revision"))
        if planned["status"]!="success" or planned["output"]["route"]!="command":return planned
        return await command(planned["output"]["argv"],base,min(max(int(request.get("timeout_seconds",120)),1),600),lock)
    if name in ("file_list","file_read","file_search","file_edit","file_write"):
        fcntl.flock(lock,fcntl.LOCK_UN)
        return await codebase_call(name,args,base,revision=(request.get("trusted") or {}).get("codebase_revision"))
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
    if name in ("login_as_current_user", "login_as_current_user_v3", "dev_browser_action", "dev_browser_record"):
        return await DIAGNOSTICS_PACKAGES.call(name,args,base,request.get("trusted"))
    if name == "administrator_artifact_get":
        return await DIAGNOSTICS_PACKAGES.call(name,args,base,request.get("trusted"))
    if name == "administrator_quiesce":
        await stop_workspace_operations(request["workspace"])
        if "terminal_control" in PLUGINS:
            await PLUGINS["terminal_control"].close_all(base)
        await DIAGNOSTICS_PACKAGES.drop(Path(base).name)
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
    return await synchronous(file_operation, request, base, args, name)


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

def file_operation(request, base, args, name):
    raw = args.get("path", ".")
    relative = Path(raw)
    # Resolve ordinary container paths, including parent traversal and symlinks.
    # The OS identity and container mounts determine which targets are reachable.
    default_root = LIVE_CHECKOUT if LIVE_CHECKOUT.is_dir() else base
    target = (relative if relative.is_absolute() else default_root / relative).resolve()
    rootfd = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    basefd = None
    directory = None
    try:
        basefd = os.dup(rootfd)
        parts = target.parts[1:]
        directory = os.dup(basefd)
        for part in parts[:-1]:
            nextfd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = nextfd
        leaf = parts[-1] if parts else "."
        scan_limited = False
        if name == "decision_candidates":
            offset = args.get("offset", 0)
            if type(offset) is not int or not 0 <= offset <= 10000:
                raise ValueError("offset must be between 0 and 10000")
            file_glob = args.get("file_glob")
            recursive = args.get("recursive", False)
            include_content = args.get("include_content", False)
            if (file_glob is not None and (not isinstance(file_glob, str)
                    or not file_glob or len(file_glob) > 128
                    or "/" in file_glob or "\\" in file_glob or "\0" in file_glob)):
                raise ValueError("file_glob must be a basename pattern of at most 128 characters")
            if type(recursive) is not bool or type(include_content) is not bool:
                raise ValueError("recursive and include_content must be booleans")
            if (recursive or include_content) and file_glob is None:
                raise ValueError("recursive and include_content require file_glob")
            fd = os.open(leaf, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                         dir_fd=directory)
            try:
                names, examined = [], 0

                def walk(folder, prefix="", depth=0):
                    nonlocal examined, scan_limited
                    with os.scandir(folder) as entries:
                        for entry in entries:
                            if examined >= 10000:
                                scan_limited = True
                                return
                            examined += 1
                            # Do not follow a directory or file symlink while
                            # collecting candidates for an external model.
                            try:
                                relative = prefix + entry.name
                                if len(relative.encode("utf-8")) > 512:
                                    scan_limited = True
                                    continue
                            except UnicodeEncodeError:
                                scan_limited = True
                                continue
                            kind = ("file" if entry.is_file(follow_symlinks=False)
                                    else "directory" if entry.is_dir(follow_symlinks=False)
                                    else "other")
                            if file_glob is None or (
                                    kind == "file"
                                    and fnmatch.fnmatchcase(entry.name, file_glob)):
                                names.append({"name": relative, "kind": kind})
                            if recursive and kind == "directory":
                                if depth >= 8:
                                    scan_limited = True
                                    continue
                                child = os.open(entry.name,
                                                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                                dir_fd=folder)
                                try:
                                    walk(child, relative + "/", depth + 1)
                                finally:
                                    os.close(child)

                walk(fd)
                names.sort(key=lambda item: item["name"])
                page = names[offset:offset + 32]
                if include_content:
                    for item in page:
                        parts = item["name"].split("/")
                        folder = os.dup(fd)
                        try:
                            for part in parts[:-1]:
                                nextfd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                                 dir_fd=folder)
                                os.close(folder)
                                folder = nextfd
                            source = os.open(parts[-1], os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW,
                                             dir_fd=folder)
                            try:
                                size = os.fstat(source)
                                if not stat.S_ISREG(size.st_mode):
                                    raise ValueError("Candidate changed during inspection")
                                excerpt = os.read(source, 1024)
                                item["snippet"] = excerpt.decode("utf-8", errors="replace")
                                item["content_truncated"] = size.st_size > len(excerpt)
                            finally:
                                os.close(source)
                        finally:
                            os.close(folder)
                next_offset = offset + len(page)
                return {"status": "success", "output": page,
                        "offset": offset, "next_offset": next_offset if next_offset < len(names) else None,
                        "total_scanned": len(names), "entries_examined": examined,
                        "truncated": scan_limited}
            finally:
                os.close(fd)
        raise ValueError("Only core directory decision reads use this helper")
    finally:
        if directory is not None:
            os.close(directory)
        if basefd is not None:
            os.close(basefd)
        os.close(rootfd)


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
    if struct.unpack("3i", peer)[1] != 1100:
        writer.close()
        await writer.wait_closed()
        return
    task = None
    disconnected = None
    request = {}
    try:
        raw = await asyncio.wait_for(reader.readline(), 10)
        request = json.loads(raw)
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
        response["id"] = request.get("id")
        writer.write((json.dumps(response) + "\n").encode())
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
    # Source writes require broker ownership. General commands may read source but only write this workspace.
    source_read=Path(os.environ.get("V3_COMMAND_SOURCE_ROOT",str(LIVE_CHECKOUT)))
    if source_read.is_dir():
        # Grant source inputs individually; the checkout also contains state and other writers' private tasks.
        names=("src","ui","plugins/abilities","plugins/gates","build-service","tests","docs","config",
            "Cargo.toml","Cargo.lock","build.rs","Dockerfile","README.md","AGENTS.md","deploy.sh","launch.py",
            "worker.py","ability_plugin_worker.py","sandbox.py","promote.py","ui-baseline.json","gate.Dockerfile","gate_worker.py",
            "data/config/gates/message_visibility_v1.json","data/config/gates/closing_status_v1.json")
        read_paths=(*read_paths,*(source_read/name for name in names))
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
