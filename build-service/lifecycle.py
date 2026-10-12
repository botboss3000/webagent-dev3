"""Operator installation and lifecycle; no agent-controlled paths or commands."""
import argparse,hashlib,importlib.util,json,os,pwd,subprocess
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
 digest.update((reviewed.parent/"plugins/abilities/Administrator/code_index/adapter.py").read_bytes())
 release=digest.hexdigest();value["service_dir"]=str(install_root/"releases"/release);cfg.validate(value)
 config_path=Path(args.config or "/etc/webagent/"+args.instance+"/build-service.json")
 secure_dirs(config_path.parent);secure_dirs(Path(value["service_dir"]).parent)
 account=pwd.getpwnam(value["service_account"])
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
  for relative in ("Cargo.toml","Cargo.lock","build.rs","Dockerfile",".gitignore","README.md","deploy.sh","launch.py","worker.py","sandbox.py","promote.py","ui-baseline.json","gate.Dockerfile","gate_worker.py"):
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
 installer=module("reviewed_service_installer",source/"install.py")
 try:installer.install(args.install_tree)
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
def main():
 parser=argparse.ArgumentParser();parser.add_argument("action",choices=("install","validate","status","start","stop","drain","resume"))
 parser.add_argument("--config",default=os.environ.get("WEBAGENT_BUILD_CONFIG"));parser.add_argument("--instance",default="development")
 parser.add_argument("--checkout-root");parser.add_argument("--service-account",default="webagent-build")
 parser.add_argument("--api-container");parser.add_argument("--loop-container");parser.add_argument("--gate-container")
 parser.add_argument("--bind-address",default="127.0.0.1");parser.add_argument("--port",type=int,default=8081);parser.add_argument("--public-origin",default="http://127.0.0.1:8081")
 parser.add_argument("--install-tree");parser.add_argument("--task-id");args=parser.parse_args()
 if args.action=="install":install(args);return
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
