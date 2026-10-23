"""Read-only source status for the existing fixed administrator broker dashboard."""
import os
from pathlib import Path
import selectors
import subprocess
import threading
import time

_PATHS=("src","crates","ui","plugins","build-service","config","tests","Cargo.toml","Cargo.lock","build.rs","deploy.sh")
_LOCK=threading.Lock()
_CACHE={}

def parse_status(raw):
    """Project bounded porcelain records; rename source names never become a second change."""
    fields=iter(raw.split(b"\0"));rows=[]
    for field in fields:
        if len(field)<4:continue
        state=field[:2].decode("ascii",errors="replace")
        path=field[3:].decode("utf-8",errors="replace")
        if "R" in state or "C" in state:next(fields,None)
        rows.append({"state":state,"path":path})
        if len(rows)>100:break
    return rows[:100],len(rows)>100

def snapshot(root):
    """Read only configured source paths; no client paths, Git mutations, hooks or credentials."""
    root=Path(root)
    if root.is_symlink()or not(root/".git").is_dir():
        return {"available":False,"reason":"source_git_unavailable"}
    key=str(root)
    with _LOCK:
        previous=_CACHE.get(key)
        if previous and time.monotonic()-previous[0]<5:return previous[1]
    result=_read(root)
    with _LOCK:
        if len(_CACHE)>8:_CACHE.clear()
        _CACHE[key]=(time.monotonic(),result)
    return result

def _read(root):
    args=["/usr/bin/git","--no-optional-locks","-c","safe.directory="+str(root),"-c","core.fsmonitor=false","-c","core.untrackedCache=false","-c","core.hooksPath=/dev/null","-C",str(root),"status","--porcelain=v1","-z","--untracked-files=normal","--",*_PATHS]
    env={"PATH":"/usr/bin:/bin","LC_ALL":"C","GIT_OPTIONAL_LOCKS":"0","GIT_CONFIG_NOSYSTEM":"1","GIT_CONFIG_GLOBAL":"/dev/null"}
    process=None;selector=selectors.DefaultSelector();streams={"out":bytearray(),"err":bytearray()};truncated=False
    try:
        process=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env)
        selector.register(process.stdout,selectors.EVENT_READ,"out")
        selector.register(process.stderr,selectors.EVENT_READ,"err")
        deadline=time.monotonic()+3
        while selector.get_map():
            remaining=deadline-time.monotonic()
            if remaining<=0:raise TimeoutError()
            for key,_ in selector.select(remaining):
                chunk=os.read(key.fileobj.fileno(),4096)
                if not chunk:selector.unregister(key.fileobj);continue
                target=streams[key.data];target.extend(chunk)
                if key.data=="out" and len(target)>65536:
                    truncated=True;process.kill();break
                if key.data=="err" and len(target)>4096:raise ValueError("git diagnostics limit")
            if truncated:break
        code=process.wait(timeout=max(.01,deadline-time.monotonic()))
        if streams["err"]or(code and not truncated):raise ValueError("git read failed")
        raw=bytes(streams["out"][:65536])
        if truncated and not raw.endswith(b"\0"):raw=raw.rsplit(b"\0",1)[0]+b"\0"
        rows,more=parse_status(raw)
        return {"available":True,"changes":rows,"truncated":truncated or more,"scope":"live development checkout source allowlist"}
    except (OSError,ValueError,TimeoutError,subprocess.TimeoutExpired):
        return {"available":False,"reason":"source_git_read_failed"}
    finally:
        selector.close()
        if process is not None:
            if process.poll()is None:process.kill()
            process.wait()
            process.stdout.close();process.stderr.close()
