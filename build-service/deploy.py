"""Fixed installation targets; no dependency on a second checkout."""
import argparse,importlib.util,os,subprocess,stat
from pathlib import Path
spec=importlib.util.spec_from_file_location("deploy_config",Path(__file__).with_name("config.py"));cfg=importlib.util.module_from_spec(spec);spec.loader.exec_module(cfg)
def initialize_state(root,c):
 """Create the container-owned state folders; configuration is live source, never seeded."""
 if os.geteuid()!=0:raise PermissionError("First API start requires root to initialize container-owned state")
 # Retained FDs prevent editable checkout paths from redirecting privileged ownership changes.
 installer_spec=importlib.util.spec_from_file_location("bootstrap_paths",Path(__file__).with_name("install.py"))
 installer=importlib.util.module_from_spec(installer_spec);installer_spec.loader.exec_module(installer)
 folder=Path(c.get("app_state_root") or root/"data");parent=installer.directory(folder.parent);state_fd=None
 try:
  for name,previous in ((folder.name,parent),):
   try:os.mkdir(name,0o770,dir_fd=previous)
   except FileExistsError:pass
  state_fd=os.open(folder.name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=parent)
  os.fchown(state_fd,c["app_uid"],c["app_gid"]);os.fchmod(state_fd,0o770)
  os.fsync(state_fd)
 finally:
  if state_fd is not None:os.close(state_fd)
  os.close(parent)

def main():
 parser=argparse.ArgumentParser();parser.add_argument("action",choices=("build","build-loop","build-gate","start","start-loop","start-gate"),default="build",nargs="?")
 args=parser.parse_args();c=cfg.load();root=Path(c["checkout_root"]);role="gate" if args.action.endswith("gate") else "loop" if args.action.endswith("loop") else "api";name=c[role+"_container"];live=cfg.layout(c)
 def run(argv,**kw):return subprocess.run(argv,check=True,**kw)
 if role=="gate":
  gate=Path(live["/runtime/gate"]);gate.mkdir(exist_ok=True)
  if args.action=="build-gate":
   archive=subprocess.Popen(["tar","-C",str(root),"-cf","-","gate.Dockerfile","gate_worker.py","tests/test_gate_worker.py","plugins/gates/publication_size_guard_v1"],stdout=subprocess.PIPE)
   try:run(["docker","build","-f","gate.Dockerfile","-t",name+":local","-"],stdin=archive.stdout)
   finally:archive.stdout.close()
   if archive.wait():raise RuntimeError("Gate source export failed")
  else:
   if subprocess.run(["docker","inspect",name],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0:raise RuntimeError("Gate already exists")
   run(["docker","run","--rm","--user","0:0","--network","none","--mount","type=bind,src="+str(gate)+",dst=/run/v3/gate","--entrypoint","/bin/sh",name+":local","-c","chown 1102:1100 /run/v3/gate && chmod 0770 /run/v3/gate"])
   run(["docker","run","-d","--name",name,"--restart","unless-stopped","--network","none","--read-only","--user","1102:1100","--cap-drop","ALL","--security-opt","no-new-privileges:true","--pids-limit","32","--memory","64m","--cpus","0.25","--mount","type=bind,src="+str(gate)+",dst=/run/v3/gate",name+":local"])
  return
 if args.action.startswith("build"):
  helper=Path(c["service_dir"])/"bootstrap.py"
  proc=subprocess.Popen(["sudo","-n","env","WEBAGENT_BUILD_CONFIG="+os.environ["WEBAGENT_BUILD_CONFIG"],"python3","-I",str(helper),"--snapshot"],stdout=subprocess.PIPE)
  try:run(["docker","buildx","build","--builder","default","--load","--progress=plain","--target",role,"-f","v3/Dockerfile","-t",name+":local","-"],stdin=proc.stdout)
  finally:proc.stdout.close()
  if proc.wait()!=0:raise RuntimeError("Snapshot export failed")
  return
 if subprocess.run(["docker","inspect",name],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0:raise RuntimeError("Container already exists; inspect before replacement")
 state=Path(live["/state"])
 if role=="api":initialize_state(root,c)
 mounts=[(state,"/state",False),(root/"config","/opt/v3/config",True),(Path(c["workspace_root"]),"/workspaces",False),(Path(live["/runtime"]),"/runtime",False)]
 argv=["docker","run","-d","--name",name,"--restart","unless-stopped","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges:true","--pids-limit",str(c["api_pids_limit"]),"--memory","2g","--cpus","2"]
 if role=="api":
  argv+=["--cap-add","CHOWN","--cap-add","FOWNER","--cap-add","SETUID","--cap-add","SETGID","--cap-add","KILL","--publish",c["bind_address"]+":"+str(c["port"])+":8080"]
  mounts += [(root,"/code",False),(root,"/live-v3",True),(root/"ui","/opt/v3/ui",True)]
  for rel in ("shared/css","main-panel/agents","vendor/fonts","vendor/lucide","favicon.svg"):mounts.append((root/"build-service/shared-ui/ui"/rel,"/opt/v3/shared-ui/"+rel,True))
 else:
  gate=Path(live["/runtime/gate"]);gate.mkdir(exist_ok=True);mounts += [(gate,"/runtime/gate",False),(root/"plugins","/code/plugins",True)]
 for source,target,ro in mounts:argv+=["--mount","type=bind,src="+str(source)+",dst="+target+(",readonly" if ro else "")]
 for key,value in {"V3_PROCESS_ROLE":role,"V3_ABILITIES_ROOT":"/opt/v3/plugins/abilities","V3_PUBLIC_ORIGIN":c["public_origin"],"V3_SOURCE_OWNER_UID":str(c["source_uid"]),"V3_BUILD_SOCKET":"/runtime/"+Path(c["runtime_root"]).name+"/build.sock"}.items():argv+=["--env",key+"="+value]
 argv+=["--tmpfs","/run:rw,nosuid,nodev,size=16m","--tmpfs","/tmp:rw,nosuid,nodev,size=32m","--stop-timeout","30",name+":local"];run(argv)
if __name__=="__main__":main()
