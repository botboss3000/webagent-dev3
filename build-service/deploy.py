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
 parser=argparse.ArgumentParser();parser.add_argument("action",choices=("build","build-loop","build-gate","start","start-loop","start-gate","start-browser"),default="build",nargs="?")
 args=parser.parse_args();c=cfg.load();root=Path(c["checkout_root"])
 if args.action=="start-browser":return start_browser(c)
 role="gate" if args.action.endswith("gate") else "loop" if args.action.endswith("loop") else "api";name=c[role+"_container"];live=cfg.layout(c)
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
  mounts += [(root,"/code",False),(root/"ui","/opt/v3/ui",True)]
 else:
  gate=Path(live["/runtime/gate"]);gate.mkdir(exist_ok=True);mounts += [(gate,"/runtime/gate",False),(root/"plugins","/code/plugins",True)]+([(root/"abilities","/code/abilities",True)] if (root/"abilities").is_dir() else [])
 for source,target,ro in mounts:argv+=["--mount","type=bind,src="+str(source)+",dst="+target+(",readonly" if ro else "")]
 for key,value in {"V3_PROCESS_ROLE":role,"V3_ABILITIES_ROOT":"/opt/v3/abilities" if (root/"abilities").is_dir() else "/opt/v3/plugins/abilities","V3_PUBLIC_ORIGIN":c["public_origin"],"V3_BUILD_SOCKET":"/runtime/"+Path(c["runtime_root"]).name+"/build.sock"}.items():argv+=["--env",key+"="+value]
 argv+=["--tmpfs","/run:rw,nosuid,nodev,size=16m","--tmpfs","/tmp:rw,nosuid,nodev,size=32m","--stop-timeout","30",name+":local"];run(argv)
BROWSER_SECURITY="/etc/webagent/development/browser/seccomp-browser.json"
def start_browser(c):
 """Create the browser container once, from the image the API runs: every chat's Chromium
 with its own sandbox, reaching only the browser folders and its socket. The broker then
 keeps it on the API image. Host setup (account 1104, AppArmor profile webagent-browser,
 the seccomp file, network webagent-browser and its firewall) must exist first."""
 def run(argv,**kw):return subprocess.run(argv,check=True,**kw)
 name=c["api_container"]+"-browser";live=cfg.layout(c);runtime=Path(live["/runtime"])
 if subprocess.run(["docker","inspect",name],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0:raise RuntimeError("Browser container already exists")
 image=subprocess.run(["docker","inspect","-f","{{.Image}}",c["api_container"]],check=True,capture_output=True,text=True).stdout.strip()
 for required in (BROWSER_SECURITY,"/etc/apparmor.d/webagent-browser"):
  if not Path(required).is_file():raise RuntimeError("Browser host setup missing: "+required)
 run(["docker","network","inspect","webagent-browser"],stdout=subprocess.DEVNULL)
 # The browser folders move to the browser identity; the socket folder lets the worker's
 # group in (setgid, so the socket keeps that group).
 run(["docker","run","--rm","--user","0:0","--network","none","--mount","type=bind,src="+str(runtime)+",dst=/r","--entrypoint","/bin/sh",image,"-c",
  "mkdir -p /r/worker/browser /r/browser && chown -R 1104:1104 /r/worker/browser && chmod 0700 /r/worker/browser && chown 1104:1100 /r/browser && chmod 2750 /r/browser"])
 run(["docker","run","-d","--name",name,"--restart","unless-stopped","--read-only","--user","1104:1104","--group-add","1100",
  "--cap-drop","ALL","--security-opt","no-new-privileges:true","--security-opt","seccomp="+BROWSER_SECURITY,"--security-opt","apparmor=webagent-browser",
  "--network","webagent-browser","--pids-limit","1024","--memory","3g","--cpus","2",
  "--tmpfs","/tmp:rw,nosuid,nodev,size=512m",
  "--mount","type=bind,src="+str(runtime/"worker"/"browser")+",dst=/runtime/worker/browser",
  "--mount","type=bind,src="+str(runtime/"browser")+",dst=/runtime/browser",
  "--env","V3_BROWSER_SANDBOX=1","--env","V3_BROWSER_CLIENT_UID=1103","--env","V3_PUBLIC_ORIGIN="+c["public_origin"],
  "--health-cmd","/usr/local/bin/webagent-v3 --browser-host ping","--health-interval","30s","--health-timeout","10s","--health-start-period","10s","--health-retries","3",
  "--stop-timeout","15",image,"/usr/local/bin/webagent-v3","--browser-host"])
if __name__=="__main__":main()
