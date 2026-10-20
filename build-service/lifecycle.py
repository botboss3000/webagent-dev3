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
  with os.fdopen(fd,"w") as stream:json.dump(value,stream,sort_keys=True);stream.write("\n");stream.flush();os.fsync(stream.fileno())
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
def install(args):
 if os.geteuid()!=0:raise PermissionError("Installation requires root")
 source=Path(__file__).resolve().parent;checkout=Path(args.checkout_root or source.parent)
 owner=pwd.getpwuid(checkout.stat().st_uid).pw_name
 try:pwd.getpwnam(args.service_account)
 except KeyError:
  if args.service_account!="webagent-build":raise
  subprocess.run(["useradd","--system","--create-home","--shell","/usr/sbin/nologin","--groups","docker",args.service_account],check=True)
 value=cfg.defaults(checkout)
 value.update(instance=args.instance,service_account=args.service_account,source_owner=owner,api_container=args.api_container or ("webagent-dev-v3" if args.instance=="development" else args.instance+"-api"),loop_container=args.loop_container or ("webagent-dev-v3-loop" if args.instance=="development" else args.instance+"-loop"),gate_container=args.gate_container or ("webagent-dev-v3-gate" if args.instance=="development" else args.instance+"-gate"),bind_address=args.bind_address,port=args.port,public_origin=args.public_origin)
 install_root=Path("/var/lib/webagent")/args.instance/"build-service";value["install_root"]=str(install_root)
 if args.config and Path(args.config).exists():
  value={k:v for k,v in cfg.load(args.config).items() if k in value}
  checkout=Path(value["checkout_root"]);install_root=Path(value["install_root"])
 unit_name="webagent-build-"+value["instance"]+".service"
 if subprocess.run(["systemctl","is-active","--quiet",unit_name]).returncode==0:raise RuntimeError("Drain and stop the service before installing reviewed code")
 for receipt in Path(value["state_root"]).glob("*.json"):
  if receipt.name=="maintenance.json":continue
  job=json.loads(receipt.read_text())
  if job.get("status") in {"queued","running","needs_decision"}:raise RuntimeError("Build queue must be idle before installation")
 if args.install_tree and (len(args.install_tree)!=64 or any(c not in "0123456789abcdef" for c in args.install_tree)):raise ValueError("Exact reviewed tree hash required")
 reviewed=Path(value["state_root"])/"coordination/trees"/args.install_tree/"v3/build-service" if args.install_tree else source
 digest=hashlib.sha256()
 for p in sorted(reviewed.glob("*.py")):
  if p.is_symlink():raise PermissionError("Reviewed source symlink")
  digest.update(p.name.encode()+b"\0"+p.read_bytes())
 digest.update(b"scheduler-defaults.json\0"+(reviewed/"scheduler-defaults.json").read_bytes())
 digest.update(b"ui-test.Dockerfile\0"+(reviewed/"ui-test.Dockerfile").read_bytes())
 digest.update(b"compiler.Dockerfile\0"+(reviewed/"compiler.Dockerfile").read_bytes())
 digest.update((reviewed.parent/"plugins/abilities/Administrator/code_index/adapter.py").read_bytes())
 digest.update((reviewed.parent/"plugins/abilities/Administrator/code_index/maintenance.py").read_bytes())
 release=digest.hexdigest();value["service_dir"]=str(install_root/"releases"/release);cfg.validate(value)
 config_path=Path(args.config or "/etc/webagent/"+args.instance+"/build-service.json")
 secure_dirs(config_path.parent);secure_dirs(Path(value["service_dir"]).parent)
 ledger_root=Path(value["install_root"]).parent/"code-index-maintenance"
 secure_dirs(ledger_root.parent)
 if ledger_root.is_symlink():raise PermissionError("Ledger root symlink")
 ledger_root.mkdir(mode=0o700,exist_ok=True)
 account=pwd.getpwnam(value["service_account"])
 os.chown(ledger_root,account.pw_uid,account.pw_gid);os.chmod(ledger_root,0o700)
 if account.pw_uid!=pwd.getpwnam(value["source_owner"]).pw_uid:
  import shutil
  if not shutil.which("setfacl"):raise RuntimeError("Install the Linux acl package before using a separate service account")
  subprocess.run(["setfacl","-m","u:"+account.pw_name+":rwx",str(checkout)],check=True)
  for relative in ("src","ui","build-service","plugins/abilities","plugins/gates","tests","docs","config"):
   path=checkout/relative
   if path.is_symlink():raise PermissionError("Source ACL path is a symlink")
   if not path.exists():continue
   subprocess.run(["setfacl","-R","-m","u:"+account.pw_name+":rwX",str(path)],check=True)
   for directory in [path,*[p for p in path.rglob("*") if p.is_dir() and not p.is_symlink()]]:
    if directory.is_dir():subprocess.run(["setfacl","-m","d:u:"+account.pw_name+":rwX,d:u:"+value["source_owner"]+":rwX",str(directory)],check=True)
  for relative in ("Cargo.toml","Cargo.lock","build.rs","Dockerfile",".gitignore","README.md","deploy.sh","launch.py","worker.py","ability_plugin_worker.py","sandbox.py","promote.py","ui-baseline.json","gate.Dockerfile","gate_worker.py"):
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
 print(json.dumps({"installed":True,"version":release,"config":str(config_path),"unit":unit_name}))
def resource_policy(config_path,candidate_path,task_id,dry_run=False):
 """Atomically change only protected memory policy; reload broker/helper without a restart."""
 if os.geteuid()!=0:raise PermissionError("Memory policy installation requires root")
 previous=cfg.load(config_path);candidate=cfg.load(candidate_path)
 if {k:v for k,v in previous.items()if k!="scheduler"}!={k:v for k,v in candidate.items()if k!="scheduler"}or {k:v for k,v in previous["scheduler"].items()if k not in cfg.MEMORY_KEYS}!={k:v for k,v in candidate["scheduler"].items()if k not in cfg.MEMORY_KEYS}:raise ValueError("Only memory policy may change live")
 desired=cfg.memory_policy_digest(candidate["scheduler"])
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
  active=call({"action":"resources"})["memory_policy"]
  request={"action":"reload_memory_policy","expected_policy_sha256":active["sha256"],"policy_sha256":desired}
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
 parser.add_argument("--api-container");parser.add_argument("--loop-container");parser.add_argument("--gate-container")
 parser.add_argument("--bind-address",default="127.0.0.1");parser.add_argument("--port",type=int,default=8081);parser.add_argument("--public-origin",default="http://127.0.0.1:8081")
 parser.add_argument("--install-tree");parser.add_argument("--task-id");parser.add_argument("--candidate-config");parser.add_argument("--dry-run",action="store_true");args=parser.parse_args()
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
