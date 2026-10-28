"""Trusted generic package bridge; executed children are workspace-sandboxed."""
import asyncio,contextlib,hashlib,json,os,signal,subprocess,sys,uuid
from pathlib import Path

def package(binding):
    """Resolve and verify an immutable package and admitted executable generation."""
    root=Path(binding["root"])
    releases=Path("/opt/v3/assets/releases")
    resolved=root.resolve(strict=True)
    if resolved!=root or releases not in root.parents:
        raise ValueError("Plugin must use an immutable published package")
    release=root.relative_to(releases).parts
    if len(release)!=6 or release[1:4]!=("v3","plugins","abilities"):
        raise ValueError("Invalid package location")
    uuid.UUID(release[0])
    ident=binding["id"]
    if release[-1]!=ident:raise ValueError("Plugin identity mismatch")
    def read(name,bound):
        path=root/name
        if not name or Path(name).is_absolute() or any(p in (".","..") for p in Path(name).parts) or path.is_symlink() or not path.is_file() or path.stat().st_size>bound or not path.resolve().is_relative_to(root):
            raise ValueError("Unsafe package file")
        return path.read_bytes()
    raw=read("plugin.json",65536);manifest=json.loads(raw)
    if manifest["id"]!=ident or type(manifest["protocol"])is not int or manifest["protocol"]!=1 or manifest.get("dispatch")!="isolated" or manifest.get("effect")!="write":
        raise ValueError("Unsupported package")
    executable=root/manifest["entrypoint"]
    if manifest.get("build",{}).get("toolchain")=="rust-1.99":
        build=manifest["build"]
        inputs={name:hashlib.sha256(read(name,16*1024*1024)).hexdigest()for name in sorted(build["inputs"])}
        compile_build=dict(build)
        if binding.get("generation_scheme",1)==2:compile_build.pop("assets",None)
        identity={"build":compile_build,"entrypoint":manifest["entrypoint"],"inputs":inputs}
        key=hashlib.sha256(json.dumps(identity,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()).hexdigest()
        executable=Path("/opt/v3/assets/artifacts")/key/manifest["entrypoint"]
        if executable.is_symlink()or not executable.is_file()or executable.stat().st_size>32*1024*1024:raise ValueError("Plugin artifact unavailable")
        code=executable.read_bytes()
    else:code=read(manifest["entrypoint"],32*1024*1024)
    schemas_raw=read(manifest["schemas"],256*1024)
    hashes=[hashlib.sha256(x).hexdigest() for x in (raw,code,schemas_raw)]
    scheme=binding.get("generation_scheme",1)
    if type(scheme)is not int or scheme not in (1,2):raise ValueError("Unsupported binding generation scheme")
    identity=hashes
    if scheme==2:
        build=manifest.get("build",{})
        if not isinstance(build,dict):raise ValueError("Invalid package build declaration")
        files={}
        for field in ("inputs","assets"):
            values=build.get(field,[])
            if not isinstance(values,list):raise ValueError("Invalid runtime input list")
            for name in values:
                files[name]=hashlib.sha256(read(name,16*1024*1024)).hexdigest()
                if len(files)>1024:raise ValueError("Runtime input bound exceeded")
        identity=[hashes,files]
    generation=hashlib.sha256(json.dumps(identity,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()).hexdigest()
    if generation!=binding["generation"] or binding["name"] not in manifest["tools"]:
        raise ValueError("Stale package binding")
    if manifest["runtime"]=="python3":argv=["/usr/bin/python3","-I","-B",str(root/manifest["entrypoint"])]
    elif manifest["runtime"]=="executable":argv=[str(executable)]
    else:raise ValueError("Unsupported runtime")
    return root,argv

async def invoke(binding,args,base,terminate):
    """Exchange one bounded protocol frame in the session workspace; uncertainty never implies a safe retry."""
    package(binding)
    env={"PATH":"/usr/local/bin:/usr/bin:/bin","LANG":"C.UTF-8","HOME":str(base),"TMPDIR":str(base)}
    process=await asyncio.create_subprocess_exec("/usr/bin/python3","-I","-B",str(Path(__file__).resolve()),"--supervise",json.dumps(binding,separators=(",",":")),
        cwd=base,env=env,start_new_session=True,stdin=asyncio.subprocess.PIPE,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.PIPE)
    data=json.dumps({"protocol":1,"invocation_id":binding["invocation_id"],"tool":binding["name"],"arguments":args},separators=(",",":")).encode()+b"\n"
    async def bounded(stream,bound):
        result=bytearray()
        while chunk:=await stream.read(65536):
            result.extend(chunk)
            if len(result)>bound:raise ValueError("Plugin output exceeds limit")
        return bytes(result)
    async def exchange():
        process.stdin.write(data);await process.stdin.drain();process.stdin.close()
        out,err=await asyncio.gather(bounded(process.stdout,2*1024*1024-16384),bounded(process.stderr,16384))
        await process.wait()
        if process.returncode:raise ValueError("Plugin process failed")
        result=json.loads(out)
        if type(result.get("protocol"))is not int or result["protocol"]!=1 or result.get("invocation_id")!=binding["invocation_id"] or result.get("status")not in ("success","failed","uncertain"):
            raise ValueError("Malformed plugin response")
        return {"status":result["status"],"uncertain":result["status"]=="uncertain","output":result.get("output"),"plugin_revision":binding["generation"],"invocation_id":binding["invocation_id"]}
    try:return await asyncio.wait_for(exchange(),120)
    except asyncio.CancelledError:raise
    except Exception:return {"status":"uncertain","output":"Plugin interrupted or returned invalid output; reconcile before retrying","uncertain":True}
    finally:
        await terminate(process)
        with contextlib.suppress(ProcessLookupError):os.killpg(process.pid,signal.SIGKILL)

def supervise(binding):
    """Apply workspace confinement and reap every descendant after completion or cancellation."""
    import ctypes,runpy,time
    libc=ctypes.CDLL(None)
    if libc.prctl(36,1,0,0,0)!=0:raise RuntimeError("Subreaper unavailable")
    if libc.prctl(1,signal.SIGTERM,0,0,0)!=0:raise RuntimeError("Parent guard unavailable")
    root,argv=package(binding)
    restrict=runpy.run_path(str(Path(__file__).with_name("sandbox.py")))["restrict"]
    child=None
    def cleanup():
        # Kill every descendant after the immediate process exits or is cancelled.
        parents={}
        for p in Path("/proc").iterdir():
            if p.name.isdigit():
                try:raw=(p/"stat").read_text();parents[int(p.name)]=int(raw[raw.rfind(")")+2:].split()[1])
                except (OSError,ValueError,IndexError):pass
        owned={os.getpid()}
        while True:
            expanded=owned|{pid for pid,parent in parents.items() if parent in owned}
            if expanded==owned:break
            owned=expanded
        for pid in owned-{os.getpid()}:
            with contextlib.suppress(ProcessLookupError):os.kill(pid,signal.SIGKILL)
    def stop(*_):
        cleanup();raise SystemExit(143)
    signal.signal(signal.SIGTERM,stop)
    try:
        child=subprocess.Popen(argv,stdin=sys.stdin.buffer,preexec_fn=lambda:restrict(Path.cwd(),read_paths=(root,Path(argv[-1]))))
        code=child.wait()
    finally:cleanup()
    raise SystemExit(code if code>=0 else 128-code)
if __name__=="__main__" and len(sys.argv)==3 and sys.argv[1]=="--supervise":supervise(json.loads(sys.argv[2]))
