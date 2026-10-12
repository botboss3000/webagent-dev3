"""One workspace, no host control socket, Docker access, or application state."""
import asyncio,importlib.util,json,sys,uuid
from pathlib import Path
root=Path(__file__).resolve().parent.parent
spec=importlib.util.spec_from_file_location("build_executor_worker",root/"worker.py")
worker=importlib.util.module_from_spec(spec);sys.modules[spec.name]=worker;spec.loader.exec_module(worker)
async def main():
 request=json.loads(sys.argv[1]);name=request["name"];args=request["arguments"]
 sid=str(uuid.UUID(request["workspace"]));base=Path("/workspaces")/sid
 control=Path("/run/v3/control");control.mkdir(parents=True)
 (control/"dispatch.lock").touch();(control/"shutoff").write_text("0");worker.CONTROL=control
 adapter=worker.administrator_plugin("v3_workspace")
 if name=="v3_cargo_task":
  import shutil
  cache=base/"webagent-v3/.cache/.cargo"
  cache.mkdir(parents=True,exist_ok=True)
  registry=cache/"registry"
  if not registry.exists():shutil.copytree("/opt/optimization-cargo/registry",registry)
 if name=="workspace_seed":result=await worker.seed_workspace_async(base)
 elif name=="source_review":result=worker.source_review(base)
 elif name=="source_patch":result=worker.source_patch(base,args)
 elif name=="v3_cargo_task":result=await adapter.cargo(base,args)
 else:result=await adapter.execute(name,args,base)
 print(json.dumps(result))
if __name__=="__main__":asyncio.run(main())
