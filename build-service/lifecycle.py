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
def stale_tree(tree,installed):
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
  if candidate.read_bytes()!=current and base.get(name)!=current:stale.append(name)
 return stale
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
 if args.install_tree and args.config and Path(args.config).exists():
  installed=Path(json.loads(Path(args.config).read_text()).get("service_dir",""))
  if installed.is_dir():
   stale=stale_tree(reviewed.parent.parent,installed)
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

GATE_BASE="debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251"
GATE_INPUTS=("gate_worker.py","tests/test_gate_worker.py","plugins/gates/publication_size_guard_v1/gate.json","plugins/gates/publication_size_guard_v1/handler.py")
def gate_recipe():
 """Refresh only the gate OS image while retaining the captured gate implementation."""
 return ("FROM "+GATE_BASE+"\nRUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y --no-install-recommends && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends python3 ca-certificates libpcre2-8-0 perl-base libexpat1 && rm -rf /var/lib/apt/lists/* && groupadd -g 1100 v3 && useradd -u 1102 -g 1100 -M -s /usr/sbin/nologin v3gate\nCOPY . /opt/v3/\nRUN chmod -R a+rX /opt/v3 && python3 -B -m unittest discover -s /opt/v3/tests -p test_gate_worker.py\nUSER 1102:1100\nENV V3_GATE_SOCKET=/run/v3/gate/gate.sock\nHEALTHCHECK --interval=10s --timeout=2s --start-period=5s --retries=3 CMD python3 -c 'import os,socket; p=os.environ[\"V3_GATE_SOCKET\"]; assert os.path.exists(p) and socket.socket(socket.AF_UNIX).connect_ex(p)==0'\nCMD [\"/usr/bin/python3\",\"-I\",\"/opt/v3/gate_worker.py\"]\n")
def gate_boundary(value,root):
 """Refuse unfamiliar gate authority instead of widening an existing container's access."""
 h=value["HostConfig"];mounts=value["Mounts"]
 if (value["Config"]["User"]!="1102:1100"or not h["ReadonlyRootfs"]or h["NetworkMode"]!="none"
     or h.get("Privileged")or h.get("CapAdd")or h.get("Devices")or h.get("DeviceRequests")
     or h.get("CapDrop")!=["ALL"]or h.get("SecurityOpt")!=["no-new-privileges:true"]
     or h.get("Memory")!=64*1024**2 or h.get("PidsLimit")!=32 or h.get("NanoCpus")!=250000000
     or len(mounts)!=1 or any(mounts[0].get(k)!=v for k,v in {"Type":"bind","Source":str(Path(root)/"gate-runtime"),"Destination":"/run/v3/gate","RW":True}.items())):
  raise ValueError("Gate isolation differs from the fixed maintenance contract")
def gate_container_config(previous,image,request_id):
 """Retain the captured runtime contract; only image and receipt identity change."""
 import copy,re
 if not re.fullmatch(r"sha256:[a-f0-9]{64}",image):raise ValueError("Immutable gate image required")
 result=copy.deepcopy(previous["Config"]);result.update(Image=image,Hostname="")
 result["Labels"]={**(result.get("Labels")or{}),"webagent.gate-security":request_id}
 result["HostConfig"]=copy.deepcopy(previous["HostConfig"])
 return result

def gate_builder_limits(info):
 """Attest the dedicated builder's kernel cgroup cap and reject OOM before activation."""
 pid=info['State']['Pid']
 if type(pid)is not int or pid<1:raise RuntimeError("Builder PID unavailable")
 groups=[line.split(':',2)[2]for line in Path('/proc/'+str(pid)+'/cgroup').read_text().splitlines()if line.startswith('0::')]
 if len(groups)!=1 or '..'in Path(groups[0]).parts:raise RuntimeError("Builder cgroup unavailable")
 import re
 identity=info.get('Id')
 if not isinstance(identity,str)or not re.fullmatch(r'[a-f0-9]{64}',identity):raise RuntimeError("Builder identity unavailable")
 member=Path('/sys/fs/cgroup')/groups[0].lstrip('/')
 # BuildKit's init subgroup inherits its exact Docker scope's limits.
 group=next((p for p in [member,*member.parents]if p.name=='docker-'+identity+'.scope'),None)
 if group is None:raise RuntimeError("Builder PID escaped the exact Docker scope")
 for p in [group,*group.parents]:
  if p==Path('/sys/fs/cgroup'):break
  if p.is_symlink():raise RuntimeError("Builder cgroup symlink")
 maximum=(group/'memory.max').read_text().strip();swap=(group/'memory.swap.max').read_text().strip()
 events={k:int(v)for k,v in (line.split()for line in (group/'memory.events').read_text().splitlines())}
 if maximum!=str(2*1024**3)or swap!='0'or events.get('oom',0)or events.get('oom_kill',0):raise RuntimeError("Builder kernel cap or OOM verification failed")
 return {'scope':str(group),'memory_max':int(maximum),'swap_max':int(swap),'oom':events.get('oom',0),'oom_kill':events.get('oom_kill',0)}

def gate_security(config_path,task_id,request_id,dry_run=False,rollback=False):
 """Run root-only, idle-broker gate maintenance with fixed inputs and exact rollback identities."""
 import copy,io,re,shutil,tarfile,http.client,socket
 if os.geteuid()!=0:raise PermissionError("Gate maintenance requires the protected root operator")
 if not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",task_id or"")or not re.fullmatch(r"[a-f0-9-]{36}",request_id or""):raise ValueError("Exact task and receipt IDs required")
 c=cfg.load(config_path);folder=Path(c["service_dir"])
 if Path(__file__).resolve()!=folder/"lifecycle.py":raise PermissionError("Use the reviewed protected lifecycle installation")
 operation=module("gate_reconfiguration",folder/"reconfigure.py")
 state=operation.operator(c,config_path,["maintenance","status"])
 if state.get("busy")or state.get("queued")or not state.get("drained")or state.get("task_id")!=task_id:raise RuntimeError("Reserve an idle broker drain for this exact task")
 def command(argv,timeout=90):return subprocess.check_output(argv,timeout=timeout)
 def inspect(name):return json.loads(command(["docker","inspect",name]))[0]
 def optional(name):
  p=subprocess.run(["docker","inspect",name],capture_output=True)
  return json.loads(p.stdout)[0]if p.returncode==0 else None
 def healthy(name,image):
  for _ in range(90):
   v=inspect(name)
   if v["Image"]!=image:raise RuntimeError("Gate image identity changed")
   if v["State"]["Status"]=="running"and v["State"].get("Health",{}).get("Status")=="healthy":return
   time.sleep(1)
  raise RuntimeError("Gate health was not confirmed")
 name=c["gate_container"];old=inspect(name);gate_boundary(old,c["checkout_root"])
 if dry_run:return {"status":"validated","task_id":task_id,"gate_image":old["Image"],"retained_services":[c["api_container"],c["loop_container"]],"compile_required":False}
 parent=Path(c["install_root"]).parent/"gate-security";secure_dirs(parent)
 root=parent/request_id
 if root.is_symlink():raise PermissionError("Gate receipt symlink")
 root.mkdir(mode=0o700,exist_ok=True);receipt=root/"receipt.json"
 def save(value):atomic(receipt,value);receipt.chmod(0o600)
 if receipt.exists():
  record=json.loads(receipt.read_bytes())
  if record["task_id"]!=task_id:raise ValueError("Gate receipt belongs to another task")
  if record["status"]=="completed":
   if old["Id"]!=record.get("candidate_id")or old["Image"]!=record["image"]:raise RuntimeError("Completed gate receipt is no longer current")
   healthy(name,record["image"]);return {"status":"completed","receipt_id":request_id,"image":record["image"],"replayed":True}
  if not rollback:raise RuntimeError("Reconcile this retained gate receipt with --rollback before another attempt")
 else:
  for other in parent.glob('*/receipt.json'):
   if other!=receipt and json.loads(other.read_bytes()).get('status')not in {'completed','rolled_back','failed_no_application_effects'}:raise RuntimeError("Reconcile earlier gate maintenance before another request")
  if rollback:raise ValueError("No gate receipt to roll back")
  healthy(name,old["Image"])
  others={r:inspect(c[r+"_container"])["Id"]for r in ("api","loop")}
  record={"task_id":task_id,"request_id":request_id,"status":"preparing","previous":old,"retained":others};save(record)
 def retained():
  if any(inspect(c[r+"_container"])["Id"]!=cid for r,cid in record["retained"].items()):raise RuntimeError("Retained API or loop identity changed")
 backup=name+"-gate-security-"+request_id;failed=name+"-failed-gate-security-"+request_id
 def restore():
  retained();previous=record["previous"];current=optional(name)
  if current and current["Id"]==previous["Id"] and any(current.get(k)!=previous[k]for k in ("Image","Config","HostConfig")):raise RuntimeError("Captured predecessor changed")
  if current and current["Id"]!=previous["Id"]:
   if record.get("candidate_id")and current["Id"]!=record["candidate_id"]:raise RuntimeError("Candidate gate identity changed")
   if current["Image"]!=record.get("image")or(current["Config"].get("Labels")or{}).get("webagent.gate-security")!=request_id:raise RuntimeError("Foreign gate replacement; manual reconciliation required")
   if optional(failed):raise RuntimeError("Failed gate quarantine already occupied")
   command(["docker","stop",current["Id"]]);command(["docker","rename",current["Id"],failed])
  if not current or current["Id"]!=previous["Id"]:
   original=inspect(backup)
   if any(original.get(k)!=previous[k]for k in ("Id","Image","Config","HostConfig")):raise RuntimeError("Captured predecessor changed")
   command(["docker","rename",original["Id"],name])
  command(["docker","start",previous["Id"]]);healthy(name,previous["Image"]);retained();record["status"]="rolled_back";save(record)
 if rollback:
  restore();return {"status":"rolled_back","receipt_id":request_id}
 builder="webagent-gate-security-"+request_id;stage=None;builder_created=False
 try:
  # The idle drain excludes competing broker builds; protect host reserve and bound this builder ancestor.
  memory=dict(line.split(":",1)for line in Path('/proc/meminfo').read_text().splitlines())
  if int(memory['MemAvailable'].split()[0])*1024<10*1024**3 or shutil.disk_usage(root).free<16*1024**3:raise RuntimeError("Gate build needs 8 GiB host reserve plus 2 GiB RAM and 16 GiB free disk")
  pinned=module("gate_fixed_executor",folder/"executor.py").BUILDKIT
  pinned_id=json.loads(command(["docker","image","inspect",pinned]))[0]["Id"];command(["docker","image","inspect",GATE_BASE])
  source=root/"source";source.mkdir(mode=0o700)
  stage=command(["docker","create","--network","none","--entrypoint","/bin/true",old["Image"]]).decode().strip()
  hashes={}
  for relative in GATE_INPUTS:
   raw=command(["docker","cp",stage+":/opt/v3/"+relative,"-"])
   if len(raw)>2*1024**2:raise ValueError("Gate input archive bound")
   with tarfile.open(fileobj=io.BytesIO(raw))as archive:
    items=archive.getmembers()
    if len(items)!=1 or not items[0].isfile()or items[0].size>1024**2:raise ValueError("Gate input must be one bounded regular file")
    data=archive.extractfile(items[0]).read()
   path=source/relative;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(data);path.chmod(0o644);hashes[relative]=hashlib.sha256(data).hexdigest()
  (source/"Dockerfile").write_text(gate_recipe());hashes['Dockerfile']=hashlib.sha256((source/"Dockerfile").read_bytes()).hexdigest();record.update(inputs=hashes,status="building");save(record)
  command(["docker","buildx","create","--name",builder,"--driver","docker-container","--driver-opt","image="+pinned,"--driver-opt","memory=2147483648","--driver-opt","memory-swap=2147483648","--driver-opt","cpu-period=100000","--driver-opt","cpu-quota=200000"]);builder_created=True
  command(["docker","buildx","inspect",builder,"--bootstrap"],120)
  info=inspect("buildx_buildkit_"+builder+"0")
  if info['Image']!=pinned_id or info['HostConfig']['Memory']!=2*1024**3 or info['HostConfig']['MemorySwap']!=2*1024**3:raise RuntimeError("Gate builder cap not enforced")
  record['builder_kernel_before']=gate_builder_limits(info)
  record['builder']={'id':info['Id'],'image':info['Image'],'memory':info['HostConfig']['Memory'],'memory_swap':info['HostConfig']['MemorySwap']};save(record)
  with (root/"build.log").open('wb')as log:
   subprocess.run(["docker","buildx","build","--builder",builder,"--load","--progress=plain","--iidfile",str(root/"image.id"),str(source)],stdout=log,stderr=subprocess.STDOUT,timeout=900,check=True)
  record['builder_kernel_after']=gate_builder_limits(inspect("buildx_buildkit_"+builder+"0"));save(record)
  image=(root/"image.id").read_text().strip();gate_container_config(old,image,request_id)
  if {p:hashlib.sha256((source/p).read_bytes()).hexdigest()for p in hashes}!=hashes:raise RuntimeError("Gate build inputs changed")
  command(["docker","run","--rm","--network","none","--read-only","--user","1102:1100","--cap-drop","ALL","--security-opt","no-new-privileges:true","--memory","64m","--memory-swap","64m","--pids-limit","32","--tmpfs","/tmp:rw,nosuid,nodev,size=16m,uid=1102,gid=1100",image,"python3","-B","-m","unittest","discover","-s","/opt/v3/tests","-p","test_gate_worker.py"])
  verify="import pathlib,hashlib,json; p=pathlib.Path('/opt/v3'); print(json.dumps({n:hashlib.sha256((p/n).read_bytes()).hexdigest()for n in "+repr(GATE_INPUTS)+"}))"
  actual=json.loads(command(["docker","run","--rm","--network","none","--read-only","--user","1102:1100","--cap-drop","ALL","--security-opt","no-new-privileges:true","--memory","64m","--memory-swap","64m","--pids-limit","32",image,"python3","-I","-c",verify]))
  if actual!={p:hashes[p]for p in GATE_INPUTS}:raise RuntimeError("Packaged gate code differs from captured implementation")
  record['packaged_code_verified']=True;save(record)
  retained();current=inspect(name)
  if any(current.get(k)!=old[k]for k in ("Id","Image","Config","HostConfig")):raise RuntimeError("Gate changed during build")
  record.update(image=image,status="activating");save(record)
  command(["docker","stop",old['Id']]);command(["docker","rename",old['Id'],backup])
  class DockerConnection(http.client.HTTPConnection):
   def connect(self):
    self.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);self.sock.settimeout(30);self.sock.connect('/var/run/docker.sock')
  conn=DockerConnection('localhost',timeout=30)
  try:
   body=json.dumps(gate_container_config(old,image,request_id));conn.request('POST','/containers/create?name='+name,body,{'Content-Type':'application/json'});response=conn.getresponse();value=json.loads(response.read())
   if response.status!=201:raise RuntimeError("Gate container creation rejected")
   candidate=value['Id'];record['candidate_id']=candidate;save(record)
  finally:conn.close()
  command(["docker","start",candidate]);healthy(name,image);gate_boundary(inspect(name),c['checkout_root']);retained()
  record.update(status="completed",completed_at=time.time());save(record)
  return {'status':'completed','receipt_id':request_id,'image':image,'rollback_container':backup,'retained_api_loop':True}
 except BaseException:
  record['status']='needs_reconciliation';save(record)
  if record.get('image'):restore()
  else:record['status']='failed_no_application_effects';save(record)
  raise
 finally:
  if stage:command(["docker","rm",stage])
  if builder_created:command(["docker","buildx","rm",builder])

def main():
 parser=argparse.ArgumentParser();parser.add_argument("action",choices=("install","validate","status","start","stop","drain","resume","reconfigure","resource-policy","gate-security"))
 parser.add_argument("--config",default=os.environ.get("WEBAGENT_BUILD_CONFIG"));parser.add_argument("--instance",default="development")
 parser.add_argument("--checkout-root");parser.add_argument("--service-account",default="webagent-build")
 parser.add_argument("--api-container");parser.add_argument("--loop-container");parser.add_argument("--gate-container")
 parser.add_argument("--bind-address",default="127.0.0.1");parser.add_argument("--port",type=int,default=8081);parser.add_argument("--public-origin",default="http://127.0.0.1:8081")
 parser.add_argument("--install-tree");parser.add_argument("--task-id");parser.add_argument("--candidate-config");parser.add_argument("--dry-run",action="store_true");parser.add_argument("--request-id");parser.add_argument("--rollback",action="store_true")
 parser.add_argument("--allow-stale-tree",action="store_true",help="install even if the tree lacks changes in the installed release");args=parser.parse_args()
 if args.action=="gate-security":
  if not args.config or not args.task_id or not args.request_id:parser.error("Protected configuration, task and receipt IDs required")
  print(json.dumps(gate_security(args.config,args.task_id,args.request_id,args.dry_run,args.rollback)));return
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
