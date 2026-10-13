"""Private native Diagnostics protocol: trusted worker frames, never model-selected authority."""
import asyncio,base64,contextlib,ctypes,importlib.util,json,os,signal,sys,time
from pathlib import Path
PROTOCOL=1
IMPLEMENTATION_VERSION="1.0.1"
MAX_FRAME=2*1024*1024
TOOLS={"login_as_current_user","login_as_current_user_v3","dev_browser_action","dev_browser_record","administrator_artifact_get","close"}
def adapter():
    """Load only the helper inside this pinned package."""
    spec=importlib.util.spec_from_file_location("diagnostics_package_adapter",Path(__file__).with_name("adapter.py"))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
async def exchange(module,request):
    """Validate transport separately from the browser's caller/session validation."""
    if type(request.get("protocol"))is not int or request["protocol"]!=1 or request.get("tool")not in TOOLS:
        raise ValueError("Invalid native Diagnostics frame")
    invocation=request.get("invocation_id")
    if not isinstance(invocation,str)or not 1<=len(invocation)<=80:raise ValueError("Invalid invocation")
    base=Path(request["base"]);trusted=request["trusted"]
    module.validate_trusted(base,trusted)
    name=request["tool"];args=request.get("arguments",{})
    if name=="administrator_artifact_get":
        content=module.read_artifact(base,trusted,args["artifact_id"])
        result={"status":"success","png_base64":base64.b64encode(content).decode()}
    elif name=="close":
        await module.close_all(base);result={"status":"success","closed":True}
    else:result=await module.execute(name,args,base,trusted)
    return {"protocol":1,"invocation_id":invocation,"result":result,"implementation_version":IMPLEMENTATION_VERSION}
async def serve():
    """Keep observations and recorder tasks alive between bounded line-framed calls."""
    module=adapter();loop=asyncio.get_running_loop()
    reader=asyncio.StreamReader(limit=MAX_FRAME+1)
    await loop.connect_read_pipe(lambda:asyncio.StreamReaderProtocol(reader),sys.stdin.buffer)
    stopped=asyncio.Event()
    for sig in (signal.SIGTERM,signal.SIGINT):loop.add_signal_handler(sig,stopped.set)
    base=None;owner=None;expires=0;touched=time.monotonic()
    pending=asyncio.create_task(reader.readline())
    try:
        while not stopped.is_set():
            done,_=await asyncio.wait([pending],timeout=1)
            if not done:
                if base is not None and(time.time()>=expires or time.monotonic()-touched>module.IDLE_SECONDS):break
                continue
            raw=pending.result()
            if not raw:break
            if len(raw)>MAX_FRAME or not raw.endswith(b"\n"):break
            request=json.loads(raw)
            trusted=module.validate_trusted(Path(request["base"]),request["trusted"])
            identity=(trusted["session_id"],trusted["user_id"],trusted["agent_id"],trusted["generation"],trusted["cookie"])
            if owner is not None and identity!=owner:break
            owner=identity;base=Path(request["base"]);expires=trusted["expires_at"];touched=time.monotonic()
            reply=await exchange(module,request)
            data=json.dumps(reply,separators=(",",":")).encode()+b"\n"
            if len(data)>MAX_FRAME:break
            sys.stdout.buffer.write(data);sys.stdout.buffer.flush()
            if request["tool"]=="close":break
            pending=asyncio.create_task(reader.readline())
    finally:
        pending.cancel()
        if base is not None:await module.close_all(base)
def main():
    """Exit with a sanitized failure; Chromium descendants share this process group."""
    libc=ctypes.CDLL(None)
    if libc.prctl(1,signal.SIGTERM,0,0,0)!=0:raise RuntimeError("Parent guard unavailable")
    try:asyncio.run(serve())
    except Exception:raise SystemExit(1) from None
if __name__=="__main__":main()
