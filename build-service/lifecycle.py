"""Operator installation and lifecycle; no agent-controlled paths or commands."""
import argparse,hashlib,importlib.util,json,os,pwd,subprocess,uuid,time,fcntl
from pathlib import Path
def module(name,path):
 spec=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
cfg=module("service_config",Path(__file__).with_name("config.py"))
def atomic(path,value):
 path=Path(path)
 if path.is_symlink():raise PermissionError("Protected destination replaced")
 temporary=path.with_name("."+path.name+".new")
 fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
 try:
  with os.fdopen(fd,"w") as stream:stream.write(value if isinstance(value,str) else json.dumps(value,sort_keys=True)+"\n");stream.flush();os.fsync(stream.fileno())
  os.chmod(temporary,0o644);os.replace(temporary,path)
 finally:temporary.unlink(missing_ok=True)
def secure_dirs(path):
 cursor=Path("/")
 for part in Path(path).parts[1:]:
  cursor=cursor/part
  if cursor.is_symlink():raise PermissionError("Symlink protected directory")
  if not cursor.exists():cursor.mkdir(mode=0o755)
  if cursor.stat().st_uid!=0 or cursor.stat().st_mode&0o022:raise PermissionError("Protected parent writable by another identity")
def installation_entry(folder):
 """Resolve the fixed installer filename in a source tree or protected installation."""
 folder=Path(folder)
 if folder.is_symlink():raise PermissionError("Installer directory symlink")
 for name in ("install.py","bootstrap.py"):
  candidate=folder/name
  if candidate.is_symlink():raise PermissionError("Installer entry symlink")
  if candidate.is_file():return candidate
 raise FileNotFoundError("Reviewed lifecycle installer unavailable")
def committed_blobs(checkout,name):
 """Git blob ids build-service/<name> has had in the checkout's history up to HEAD; empty when git fails."""
 out=subprocess.run(["git","-c","safe.directory="+str(checkout),"-C",str(checkout),"log","--format=","--raw","--no-abbrev",
                     "HEAD","--","build-service/"+name],capture_output=True,text=True)
 return {field for line in out.stdout.splitlines() for field in line.split()[2:4]} if out.returncode==0 else set()
def stale_tree(tree,installed,history=lambda name:set()):
 """Broker files a tree would revert: it differs from the installed release and did not start from it.

 A task tree is cut from the checkout at begin (baseline.tar). If another release
 was installed afterwards, the tree still carries the older bytes for files it never
 edited; installing it would silently drop that release's changes."""
 import tarfile
 tree,installed=Path(tree),Path(installed)
 with tarfile.open(tree/"baseline.tar") as archive:
  base={m.name[len("v3/build-service/"):]:archive.extractfile(m).read() for m in archive.getmembers()
        if m.isfile() and m.name.startswith("v3/build-service/") and "/" not in m.name[len("v3/build-service/"):]}
 stale=[]
 for path in sorted(installed.iterdir()):
  name="install.py" if path.name=="bootstrap.py" else path.name
  candidate=tree/"v3/build-service"/name
  if not path.is_file() or path.is_symlink() or not candidate.is_file():continue
  current=path.read_bytes()
  # A release can commit broker files without installing them; a tree cut after that commit
  # moves forward from the installed bytes when they are an earlier committed version.
  blob=hashlib.sha1(b"blob %d\0"%len(current)+current).hexdigest()
  if candidate.read_bytes()!=current and base.get(name)!=current and blob not in history(name):stale.append(name)
 return stale
BOOT_PIN="50-reviewed-broker-release.conf"
def boot_pin(value,unit_root="/etc/systemd/system"):
 """Plan repointing the boot host's release drop-in at value's release: (unit, drop-in, old, new, boot host), or None when absent.

 Only a drop-in naming exactly one boot_host.py under this installation's releases is rewritten;
 any other content fails closed rather than overwriting an operator edit."""
 import re
 unit="webagent-boot-"+value["instance"]+".service";folder=Path(unit_root)/(unit+".d");path=folder/BOOT_PIN
 if folder.is_symlink() or path.is_symlink():raise PermissionError("Boot drop-in symlink")
 if not path.exists():return None
 if not path.is_file():raise PermissionError("Boot drop-in is not a regular file")
 old=path.read_text();pattern=re.compile(re.escape(str(Path(value["install_root"])/"releases"))+r"/[0-9a-f]{64}/boot_host\.py(?=\s|$)")
 if len(pattern.findall(old))!=1:raise ValueError("Boot drop-in does not pin exactly one reviewed boot host")
 host=Path(value["service_dir"])/"boot_host.py"
 return unit,path,old,pattern.sub(lambda _:str(host),old),host
def repoint_boot(plan,backup,run=subprocess.run):
 """Back up and atomically rewrite the boot pin, reload systemd and restart only the boot host (app containers untouched)."""
 unit,path,old,new,host=plan
 if old==new:return {"drop_in":str(path),"changed":False}
 if host.is_symlink() or not host.is_file():raise FileNotFoundError("Installed boot host unavailable")
 atomic(backup,old);atomic(path,new)
 run(["systemctl","daemon-reload"],check=True)
 # An operator-stopped boot host stays stopped. A running or failed one restarts on the new
 # release; a stale pin may have left it start-limited, which reset-failed clears.
 state=run(["systemctl","is-active",unit],check=False,capture_output=True,text=True).stdout.strip()
 if state=="inactive":return {"drop_in":str(path),"changed":True,"previous":str(backup),"restarted":None}
 run(["systemctl","reset-failed",unit],check=False);run(["systemctl","restart",unit],check=True)
 return {"drop_in":str(path),"changed":True,"previous":str(backup),"restarted":unit}
def install_board(value,service_dir,unit_root="/etc/systemd/system",run=subprocess.run,lib_root="/var/lib/webagent"):
 """Install the blocker board service from this release: its code at a fixed path and its unit.

 The board restarts only when its code or unit changed, so a broker install that leaves
 board_host.py alone never interrupts it (it is how sessions talk while the broker is away)."""
 source=Path(service_dir)/"board_host.py"
 if not source.is_file() or source.is_symlink():return {"board":"absent"}
 folder=Path(lib_root)/value["instance"]/"board-service"
 if folder.is_symlink():raise PermissionError("Board service directory symlink")
 folder.mkdir(mode=0o755,parents=True,exist_ok=True);os.chmod(folder,0o755)
 target=folder/"board_host.py";body=source.read_bytes()
 changed=not target.exists() or target.read_bytes()!=body
 if changed:
  copy=folder/"board_host.py.new";copy.write_bytes(body);copy.chmod(0o644);os.replace(copy,target)
 name="webagent-board-"+value["instance"]+".service";unit=Path(unit_root)/name
 if unit.is_symlink():raise PermissionError("Board unit symlink")
 text=("[Unit]\nDescription=WebAgent blocker board "+value["instance"]+"\n\n[Service]\nType=simple\n"
       "Environment=WEBAGENT_BUILD_CONFIG="+os.environ.get("WEBAGENT_BUILD_CONFIG","/etc/webagent/development/build-service.json")+"\n"
       "ExecStart=/usr/bin/python3 -I "+str(target)+"\nRestart=always\nRestartSec=2\nUMask=0077\n\n[Install]\nWantedBy=multi-user.target\n")
 if not unit.exists() or unit.read_text()!=text:
  unit.write_text(text);unit.chmod(0o644);changed=True;run(["systemctl","daemon-reload"],check=True)
 run(["systemctl","enable",name],check=False)
 state=run(["systemctl","is-active",name],check=False,capture_output=True,text=True).stdout.strip()
 if changed or state!="active":
  run(["systemctl","reset-failed",name],check=False);run(["systemctl","restart",name],check=True)
 return {"board":name,"changed":changed,"restarted":changed or state!="active"}
def install(args):
 if os.geteuid()!=0:raise PermissionError("Installation requires root")
 source=Path(__file__).resolve().parent;checkout=Path(args.checkout_root or source.parent)
 owner=pwd.getpwuid(checkout.stat().st_uid).pw_name
 try:pwd.getpwnam(args.service_account)
 except KeyError:
  if args.service_account!="webagent-build":raise
  subprocess.run(["useradd","--system","--create-home","--shell","/usr/sbin/nologin","--groups","docker",args.service_account],check=True)
 value=cfg.defaults(checkout)
 value.update(instance=args.instance,service_account=args.service_account,source_owner=owner,api_container=args.api_container or ("webagent-dev-v3" if args.instance=="development" else args.instance+"-api"),loop_container=args.loop_container or ("webagent-dev-v3-loop" if args.instance=="development" else args.instance+"-loop"),bind_address=args.bind_address,port=args.port,public_origin=args.public_origin)
 install_root=Path("/var/lib/webagent")/args.instance/"build-service";value["install_root"]=str(install_root)
 if args.config and Path(args.config).exists():
  value={k:v for k,v in cfg.load(args.config).items() if k in value}
  checkout=Path(value["checkout_root"]);install_root=Path(value["install_root"])
 unit_name="webagent-build-"+value["instance"]+".service"
 if subprocess.run(["systemctl","is-active","--quiet",unit_name]).returncode==0:raise RuntimeError("Drain and stop the service before installing reviewed code")
 for receipt in Path(value["state_root"]).glob("*.json"):
  if receipt.name=="maintenance.json":continue
  job=json.loads(receipt.read_text())
  # Jobs queued behind the drain have not started; isolated ones survive the restart.
  if job.get("status") in {"running","needs_decision"}or(job.get("status")=="queued"and job.get("effects_started")):raise RuntimeError("Build queue must be idle before installation")
 if args.install_tree and (len(args.install_tree)!=64 or any(c not in "0123456789abcdef" for c in args.install_tree)):raise ValueError("Exact reviewed tree hash required")
 reviewed=Path(value["state_root"])/"coordination/trees"/args.install_tree/"v3/build-service" if args.install_tree else source
 if args.install_tree and args.config and Path(args.config).exists():
  installed=Path(json.loads(Path(args.config).read_text()).get("service_dir",""))
  if installed.is_dir():
   stale=stale_tree(reviewed.parent.parent,installed,lambda name:committed_blobs(checkout,name))
   if stale and not args.allow_stale_tree:
    raise RuntimeError("Tree would revert the installed broker release ("+installed.name[:8]+") in: "+", ".join(stale)
                       +"; begin a fresh task from checkout HEAD and re-apply, or pass --allow-stale-tree for a deliberate rollback")
 digest=hashlib.sha256()
 for p in sorted(reviewed.glob("*.py")):
  if p.is_symlink():raise PermissionError("Reviewed source symlink")
  digest.update(p.name.encode()+b"\0"+p.read_bytes())
 digest.update(b"scheduler-defaults.json\0"+(reviewed/"scheduler-defaults.json").read_bytes())
 digest.update(b"ui-test.Dockerfile\0"+(reviewed/"ui-test.Dockerfile").read_bytes())
 digest.update(b"compiler.Dockerfile\0"+(reviewed/"compiler.Dockerfile").read_bytes())
 release=digest.hexdigest();value["service_dir"]=str(install_root/"releases"/release);cfg.validate(value)
 boot=boot_pin(value);boot_home=Path(value["install_root"]).parent/"boot-supervisor"
 if boot:secure_dirs(boot_home)
 config_path=Path(args.config or "/etc/webagent/"+args.instance+"/build-service.json")
 secure_dirs(config_path.parent);secure_dirs(Path(value["service_dir"]).parent)
 secure_dirs(Path(value["install_root"]).parent)
 account=pwd.getpwnam(value["service_account"])
 if account.pw_uid!=pwd.getpwnam(value["source_owner"]).pw_uid:
  import shutil
  if not shutil.which("setfacl"):raise RuntimeError("Install the Linux acl package before using a separate service account")
  subprocess.run(["setfacl","-m","u:"+account.pw_name+":rwx",str(checkout)],check=True)
  for relative in ("src","ui","build-service","abilities","plugins/gates","tests","docs","config"):
   path=checkout/relative
   if path.is_symlink():raise PermissionError("Source ACL path is a symlink")
   if not path.exists():continue
   subprocess.run(["setfacl","-R","-m","u:"+account.pw_name+":rwX",str(path)],check=True)
   for directory in [path,*[p for p in path.rglob("*") if p.is_dir() and not p.is_symlink()]]:
    if directory.is_dir():subprocess.run(["setfacl","-m","d:u:"+account.pw_name+":rwX,d:u:"+value["source_owner"]+":rwX",str(directory)],check=True)
  for relative in ("Cargo.toml","Cargo.lock","build.rs","Dockerfile",".gitignore","README.md","deploy.sh","sandbox.py","live-files.json","rustfmt.toml"):
   file=checkout/relative
   if file.is_symlink():raise PermissionError("Source ACL file is a symlink")
   if file.is_file():subprocess.run(["setfacl","-m","u:"+account.pw_name+":rw",str(file)],check=True)
 for key in ("state_root","workspace_root"):
  p=Path(value[key]);p.mkdir(parents=True,exist_ok=True)
  if key=="state_root":os.chown(p,account.pw_uid,account.pw_gid);os.chmod(p,0o700)
 runtime=Path(value["runtime_root"]);runtime.parent.mkdir(parents=True,exist_ok=True)
 if not runtime.exists():runtime.mkdir(mode=0o751)
 if runtime.is_symlink():raise PermissionError("Runtime symlink")
 os.chown(runtime,0,account.pw_gid);os.chmod(runtime,0o751)
 previous=config_path.read_bytes() if config_path.exists() else None
 if previous:
  history=config_path.parent/"history";secure_dirs(history)
  archived=history/(hashlib.sha256(previous).hexdigest()+".json")
  if not archived.exists():atomic(archived,json.loads(previous))
  atomic(config_path.with_name("build-service.previous.json"),json.loads(previous))
 atomic(config_path,value);os.environ["WEBAGENT_BUILD_CONFIG"]=str(config_path);os.environ["WEBAGENT_BUILD_MODULE_ROOT"]=str(source)
 try:
  installer=module("reviewed_service_installer",installation_entry(source))
  installer.install(args.install_tree)
 except BaseException:
  if previous:atomic(config_path,json.loads(previous))
  else:config_path.unlink(missing_ok=True)
  raise
 unit_name="webagent-build-"+value["instance"]+".service"
 text="[Unit]\nDescription=WebAgent build service "+value["instance"]+"\nAfter=docker.service\nRequires=docker.service\n\n[Service]\nType=simple\nEnvironment=WEBAGENT_BUILD_CONFIG="+str(config_path)+"\nExecStart=/usr/bin/python3 -I "+value["service_dir"]+"/bootstrap.py --foreground\nRestart=on-failure\nTimeoutStopSec=90\nUMask=0077\n\n[Install]\nWantedBy=multi-user.target\n"
 unit=Path("/etc/systemd/system")/unit_name
 if unit.is_symlink():raise PermissionError("Unit symlink")
 unit.write_text(text);unit.chmod(0o644);subprocess.run(["systemctl","daemon-reload"],check=True)
 # Release keys run a root-owned copy of the operator shell (sudoers pins its path); keep it current.
 # The root host helper is not copied here: its supervisor_update action replaces it after approval.
 for folder,name,unit in (("/usr/local/lib/webagent-ops","operator_shell.py",None),):
  folder=Path(folder)
  if not folder.is_dir() or folder.is_symlink():continue
  body=(Path(value["service_dir"])/name).read_bytes();target=folder/name
  if target.exists() and target.read_bytes()==body:continue
  copy=folder/(name+".new");copy.write_bytes(body);copy.chmod(0o644);os.replace(copy,target)
  if unit:subprocess.run(["systemctl","try-restart",unit],check=False)
 boot_host=repoint_boot(boot,boot_home/(BOOT_PIN+".previous")) if boot else {"drop_in":"absent","changed":False,"note":"no "+BOOT_PIN+" drop-in; boot host left unchanged"}
 board=install_board(value,value["service_dir"])
 print(json.dumps({"installed":True,"version":release,"config":str(config_path),"unit":unit_name,"boot_host":boot_host,"board":board}))
def resource_policy(config_path,candidate_path,task_id,dry_run=False):
 """Atomically change only protected CPU/memory policy; reload broker/helper without a restart."""
 if os.geteuid()!=0:raise PermissionError("Memory policy installation requires root")
 previous=cfg.load(config_path);candidate=cfg.load(candidate_path)
 if {k:v for k,v in previous.items()if k!="scheduler"}!={k:v for k,v in candidate.items()if k!="scheduler"}or {k:v for k,v in previous["scheduler"].items()if k not in cfg.RESOURCE_KEYS}!={k:v for k,v in candidate["scheduler"].items()if k not in cfg.RESOURCE_KEYS}:raise ValueError("Only CPU and memory policy may change live")
 desired=cfg.resource_policy_digest(candidate["scheduler"])
 if dry_run:return {"validated":True,"policy_sha256":desired,"restart_required":False}
 if not isinstance(task_id,str)or not __import__("re").fullmatch(r"[A-Za-z0-9_-]{8,80}",task_id):raise ValueError("Bounded operator task ID required")
 folder=Path(config_path).parent;secure_dirs(folder)
 lockpath=folder/"resource-policy.lock"
 fd=os.open(lockpath,os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW,0o600)
 with os.fdopen(fd,"a")as lock:
  info=os.fstat(lock.fileno())
  if info.st_uid!=0 or info.st_nlink!=1 or info.st_mode&0o022:raise PermissionError("Protected memory-policy lock required")
  fcntl.flock(lock,fcntl.LOCK_EX)
  # Catch a concurrent configuration writer rather than overwrite its topology.
  if cfg.load(config_path)!=previous:raise ValueError("Installation configuration changed")
  def call(args):
   command=["sudo","-n","-u",previous["service_account"],"env","WEBAGENT_BUILD_CONFIG="+str(config_path),"python3","-B",str(Path(previous["service_dir"])/"buildctl.py"),"schedule","--input","-"]
   return json.loads(subprocess.check_output(command,input=json.dumps(args).encode(),timeout=30))
  active=call({"action":"resources"})["resource_policy"]
  request={"action":"reload_resource_policy","expected_policy_sha256":active["sha256"],"policy_sha256":desired}
  history=folder/"resource-policy";secure_dirs(history)
  unresolved=[]
  for path in sorted(history.glob("*.json")):
   old=json.loads(path.read_bytes())
   if old.get("status")in {"prepared","needs_reconciliation"}:
    if old.get("policy_sha256")!=desired:raise ValueError("Reconcile prior memory policy before changing its target")
    unresolved.append((path,old))
  receipt_id=str(uuid.uuid4());receipt=history/(receipt_id+".json")
  raw={k:v for k,v in candidate.items()if k in cfg.defaults().keys()}
  record={"id":receipt_id,"task_id":task_id,"status":"prepared","request":request,"before_sha256":active["sha256"],"policy_sha256":desired,"at":time.time()}
  atomic(receipt,record);atomic(config_path,raw)
  try:
   result=call(request)
   if result["sha256"]!=desired:raise ValueError("Memory reload acknowledgment changed")
   record.update(status="completed",result=result);atomic(receipt,record)
   for path,old in unresolved:
    old.update(status="reconciled",reconciled_by=receipt_id,result=result);atomic(path,old)
  except BaseException:
   # The protected desired JSON remains authoritative. Never overwrite a newer writer
   # or replay a runtime memory effect; repeat this policy command to reconcile adoption.
   record.update(status="needs_reconciliation");atomic(receipt,record);raise
  return {"status":"completed","receipt_id":receipt_id,"policy_sha256":desired,"restart_required":False}

def main():
 parser=argparse.ArgumentParser();parser.add_argument("action",choices=("install","validate","status","start","stop","drain","resume","reconfigure","resource-policy"))
 parser.add_argument("--config",default=os.environ.get("WEBAGENT_BUILD_CONFIG"));parser.add_argument("--instance",default="development")
 parser.add_argument("--checkout-root");parser.add_argument("--service-account",default="webagent-build")
 parser.add_argument("--api-container");parser.add_argument("--loop-container")
 parser.add_argument("--bind-address",default="127.0.0.1");parser.add_argument("--port",type=int,default=8081);parser.add_argument("--public-origin",default="http://127.0.0.1:8081")
 parser.add_argument("--install-tree");parser.add_argument("--task-id");parser.add_argument("--candidate-config");parser.add_argument("--dry-run",action="store_true")
 parser.add_argument("--allow-stale-tree",action="store_true",help="install even if the tree lacks changes in the installed release");args=parser.parse_args()
 if args.action=="install":install(args);return
 if args.action=="resource-policy":
  if not args.config or not args.candidate_config or not args.task_id:parser.error("Exact current/candidate protected configs and task ID required")
  print(json.dumps(resource_policy(args.config,args.candidate_config,args.task_id,args.dry_run)));return
 if args.action=="reconfigure":
  if not args.config or not args.candidate_config or not args.task_id:parser.error("Exact current/candidate protected configs and task ID required")
  operation=module("scoped_reconfiguration",Path(__file__).with_name("reconfigure.py"))
  print(json.dumps(operation.apply(args.config,args.candidate_config,args.task_id,args.dry_run)));return
 if not args.config:parser.error("--config is required")
 config=cfg.load(args.config);os.environ["WEBAGENT_BUILD_CONFIG"]=args.config
 if args.action=="validate":print(json.dumps({"valid":True,"instance":config["instance"]}));return
 if args.action in {"start","stop"}:
  if os.geteuid()!=0:raise PermissionError("Lifecycle requires root")
  subprocess.run(["systemctl",args.action,"webagent-build-"+config["instance"]+".service"],check=True);return
 if os.geteuid()==0:
  account=pwd.getpwnam(config["service_account"]);os.initgroups(account.pw_name,account.pw_gid);os.setgid(account.pw_gid);os.setuid(account.pw_uid)
 client=module("build_service_client",Path(config["service_dir"])/"buildctl.py")
 print(json.dumps(client.call("build_service_maintenance",{"action":"status" if args.action=="status" else args.action,**({"task_id":args.task_id} if args.task_id else {})}),sort_keys=True))
if __name__=="__main__":main()
