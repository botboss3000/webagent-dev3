"""Installation-owned paths and identities, never model arguments."""
import json,os,pwd,re,stat
from pathlib import Path
# @code-index {"version":1,"feature":"controlled-build-scheduler","aliases":["parallel builds","source ownership","compiler parking"],"purpose":"Reviewed scheduler defaults merged with protected installation JSON. Config: build-service/scheduler-defaults.json -> mode, orphan_after_seconds and resource thresholds.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/mode","/orphan_after_seconds","/reserve_bytes","/reserve_percent","/max_phases","/max_parked","/memory_pressure_percent","/ui_file_reservations","/max_ui_preparations","/component_indexing","/concurrent_ui_preparation","/ui_test_image","/package_ram_bytes","/package_tmpfs_bytes"]}],"opportunities":[]}}
MEMORY_KEYS={"compiler_memory_default_bytes","compiler_memory_min_bytes","compiler_memory_max_bytes","allow_live_memory_increase"}
# @code-index {"version":1,"feature":"compiler-memory-policy","aliases":["compiler memory defaults","task memory overrides","live memory growth"],"purpose":"Protected compiler ceilings and live growth. Config: build-service/scheduler-defaults.json -> compiler_memory_default_bytes, compiler_memory_min_bytes, compiler_memory_max_bytes, allow_live_memory_increase; installation JSON overrides.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/compiler_memory_default_bytes","/compiler_memory_min_bytes","/compiler_memory_max_bytes","/allow_live_memory_increase"]}]}}
def memory_policy(policy):
 """Select the protected compiler policy; existing job ceilings remain pinned."""
 return {k:policy.get(k,{"compiler_memory_default_bytes":12*1024**3,"compiler_memory_min_bytes":1024**3,"compiler_memory_max_bytes":16*1024**3,"allow_live_memory_increase":False}[k])for k in sorted(MEMORY_KEYS)}
def memory_policy_digest(policy):
 """Bind broker/helper adoption to the same root-owned policy revision."""
 import hashlib
 return hashlib.sha256(json.dumps(memory_policy(policy),sort_keys=True,separators=(",",":")).encode()).hexdigest()
CPU_KEYS={"cpu_budget_millis","cpu_priority"}
RESOURCE_KEYS=MEMORY_KEYS|CPU_KEYS
def cpu_policy(policy):
 return {"cpu_budget_millis":policy.get("cpu_budget_millis",0),"cpu_priority":policy.get("cpu_priority","balanced")}
def resource_policy(policy):
 return {**memory_policy(policy),**cpu_policy(policy)}
def resource_policy_digest(policy):
 import hashlib
 return hashlib.sha256(json.dumps(resource_policy(policy),sort_keys=True,separators=(",",":")).encode()).hexdigest()
def scheduler_defaults():
 """Load reviewed source defaults; protected installation overrides remain the operator authority."""
 path=Path(__file__).with_name("scheduler-defaults.json")
 if path.is_symlink():raise PermissionError("Scheduler defaults symlink")
 return json.loads(path.read_text())
def validate(value):
 """Validate installation authority, topology and the bounded API process/thread budget."""
 if not isinstance(value,dict):raise ValueError("Configuration must be an object")
 allowed={"instance","checkout_root","service_account","source_owner","state_root","workspace_root","runtime_root","install_root","service_dir","app_uid","app_gid","worker_uid","worker_gid","api_container","loop_container","gate_container","bind_address","port","public_origin","worker_control","api_pids_limit","scheduler","code_index_ledger_max_bytes"}
 if set(value)-allowed:raise ValueError("Unknown installation field")
 v=dict(value)
 # Ledger capacity is protected installation policy, never a caller override.
 v.setdefault("code_index_ledger_max_bytes",768*1024**2)
 ledger=v["code_index_ledger_max_bytes"]
 if type(ledger)is not int or not 512*1024**2<=ledger<=1024**3 or ledger%4096:raise ValueError("Invalid review ledger storage budget")
 policy=v.get("scheduler",{})
 if not isinstance(policy,dict)or set(policy)-{"mode","orphan_after_seconds","reserve_bytes","reserve_percent","max_phases","max_parked","memory_pressure_percent","ui_file_reservations","max_ui_preparations","component_indexing","concurrent_ui_preparation","ui_test_image","package_ram_bytes","package_tmpfs_bytes","task_aware_cleanup","disk_min_free_bytes","disk_grace_seconds","disk_cleanup_batch","documentation_review_order","post_deployment_indexing","package_cache_enabled","package_cache_download_workers","package_cache_max_age_seconds","package_cache_retention_seconds","package_cache_max_bytes","package_cache_prepare_memory_bytes","failure_grace_seconds"}-RESOURCE_KEYS:raise ValueError("Invalid scheduler policy")
 policy={**scheduler_defaults(),**policy}
 cpu=cpu_policy(policy)
 if type(cpu["cpu_budget_millis"])is not int or cpu["cpu_budget_millis"]!=0 and not 1000<=cpu["cpu_budget_millis"]<=max(1,(os.cpu_count()or 1)-1)*1000:raise ValueError("CPU budget must retain host headroom")
 if type(cpu["cpu_priority"])is not str or cpu["cpu_priority"]not in {"balanced","broker"}:raise ValueError("Invalid CPU priority")
 policy.update(cpu)
 if policy.get("documentation_review_order","before_deployment")not in {"before_deployment","after_deployment"}:raise ValueError("Invalid documentation review order")
 if policy["mode"]not in {"observe","enforce"}or type(policy["orphan_after_seconds"])is not int or not 60<=policy["orphan_after_seconds"]<=86400:raise ValueError("Invalid ownership policy")
 if type(policy.get("failure_grace_seconds",1800))is not int or not 300<=policy.get("failure_grace_seconds",1800)<=7200:raise ValueError("Invalid failed-release grace window")
 for key,default,low,high in [("reserve_bytes",768*1024**2,256*1024**2,16*1024**3),("reserve_percent",10,5,50),("max_phases",4,1,16),("max_parked",2,0,8),("memory_pressure_percent",5,1,50)]:
  policy.setdefault(key,default)
  if type(policy[key])is not int or not low<=policy[key]<=high:raise ValueError("Invalid scheduler resource policy: "+key)
 if type(policy.get("concurrent_ui_preparation",False))is not bool:raise ValueError("Invalid UI preparation policy")
 if type(policy.get("component_indexing",False))is not bool:raise ValueError("Invalid component index policy")
 if type(policy.get("post_deployment_indexing",False))is not bool:raise ValueError("Invalid post-deployment index policy")
 if policy.get("post_deployment_indexing",False)and policy.get("documentation_review_order")!="after_deployment":raise ValueError("Post-deployment indexing requires post-deployment documentation review")
 if type(policy.get("ui_file_reservations",False))is not bool:raise ValueError("Invalid UI reservation policy")
 if type(policy.get("max_ui_preparations",1))is not int or not 1<=policy.get("max_ui_preparations",1)<=2:raise ValueError("Invalid UI preparation limit")
 if policy["concurrent_ui_preparation"]and(policy["mode"]!="enforce"or not policy["component_indexing"]):raise ValueError("Concurrent UI preparation requires enforced scheduling and measured component indexing")
 if policy.get("ui_test_image")is not None and(not isinstance(policy["ui_test_image"],str)or not re.fullmatch(r"sha256:[a-f0-9]{64}",policy["ui_test_image"])):raise ValueError("UI test runtime requires a pinned image identity")
 ram=policy.get("package_ram_bytes",0);scratch=policy.get("package_tmpfs_bytes",0)
 if type(ram)is not int or type(scratch)is not int or not 0<=ram<=10_000_000_000 or not 0<=scratch<=8_000_000_000:raise ValueError("Invalid RAM packaging budget")
 if ram%os.sysconf("SC_PAGE_SIZE")or scratch%os.sysconf("SC_PAGE_SIZE"):raise ValueError("RAM packaging budgets must be memory-page aligned")
 if bool(ram)!=bool(scratch)or(ram and(scratch<256*1024**2 or scratch>ram-1024**3 or policy["mode"]!="enforce"or policy["reserve_percent"]<15)):raise ValueError("RAM packaging requires bounded scratch, enforced admission and fifteen percent host reserve")
 limits=memory_policy(policy);minimum=limits["compiler_memory_min_bytes"];default=limits["compiler_memory_default_bytes"];maximum=limits["compiler_memory_max_bytes"]
 if any(type(n)is not int or n%os.sysconf("SC_PAGE_SIZE")for n in (minimum,default,maximum))or not 1024**3<=minimum<=default<=maximum<=16*1024**3:raise ValueError("Invalid compiler memory policy")
 if type(limits["allow_live_memory_increase"])is not bool:raise ValueError("Invalid live memory policy")
 if limits["allow_live_memory_increase"]and(policy["mode"]!="enforce"or policy["reserve_percent"]<15):raise ValueError("Live memory increases require enforced admission and fifteen percent host reserve")
 if type(policy.get("package_cache_enabled",False))is not bool:raise ValueError("Invalid package cache policy")
 if policy.get("package_cache_enabled",False)and(policy["mode"]!="enforce"or policy["reserve_percent"]<15):raise ValueError("Persistent packaging requires enforced admission and fifteen percent reserve")
 for key,low,high in [("package_cache_download_workers",1,4),("package_cache_max_age_seconds",3600,30*86400),("package_cache_retention_seconds",3600,90*86400),("package_cache_max_bytes",1024**3,32*1024**3),("package_cache_prepare_memory_bytes",1024**3,6*1024**3)]:
  if type(policy.get(key))is not int or not low<=policy[key]<=high:raise ValueError("Invalid package cache policy: "+key)
 if policy["package_cache_prepare_memory_bytes"]%os.sysconf("SC_PAGE_SIZE"):raise ValueError("Package cache memory budget must be page aligned")
 if type(policy.get("task_aware_cleanup",False))is not bool:raise ValueError("Invalid Docker retention policy")
 for key,low,high in [("disk_min_free_bytes",1024**3,64*1024**3),("disk_grace_seconds",7200,30*86400),("disk_cleanup_batch",1,32)]:
  if type(policy.get(key))is not int or not low<=policy[key]<=high:raise ValueError("Invalid disk policy: "+key)
 v["scheduler"]=policy
 # Legacy installations retain their prior budget until an explicit operator reconfiguration.
 v.setdefault("api_pids_limit",256)
 if type(v["api_pids_limit"])is not int or not 256<=v["api_pids_limit"]<=4096:
  raise ValueError("API process/thread budget must be an integer from 256 through 4096")
 if not re.fullmatch(r"[a-z][a-z0-9_-]{0,47}",v.get("instance","")):raise ValueError("Invalid instance ID")
 account=pwd.getpwnam(v.get("service_account","webagent-build"));owner=pwd.getpwnam(v.get("source_owner",account.pw_name))
 if account.pw_uid==0 or owner.pw_uid==0:raise ValueError("Non-root service and source identities required")
 v.update(service_uid=account.pw_uid,service_gid=account.pw_gid,service_home=account.pw_dir,source_uid=owner.pw_uid)
 for k in ("checkout_root","state_root","workspace_root","runtime_root","install_root","service_dir","worker_control"):
  raw=v.get(k)
  if not isinstance(raw,str) or not raw.startswith("/") or ".." in Path(raw).parts or any(c in raw for c in "\n\r\0,:") or any(c.isspace() for c in raw) or str(Path(raw))!=raw:raise ValueError("Invalid installation path: "+k)
  cursor=Path(raw)
  while cursor!=cursor.parent:
   if cursor.is_symlink():raise ValueError("Symlink installation path: "+k)
   cursor=cursor.parent
 for k in ("app_uid","app_gid","worker_uid","worker_gid"):
  if type(v.get(k)) is not int or not 1<=v[k]<=2147483647:raise ValueError("Invalid container identity")
 if v["app_uid"]==v["service_uid"]:raise ValueError("App and service identities must differ")
 if tuple(v[k] for k in ("app_uid","app_gid","worker_uid","worker_gid"))!=(1100,1100,1000,1000):raise ValueError("Container identities must match the pinned image")
 for k in ("api_container","loop_container","gate_container"):
  if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,80}",v.get(k,"")) or v[k] in {"webagent","webagent-dev-v2"}:raise ValueError("Invalid or excluded container target")
 if len({v["api_container"],v["loop_container"],v["gate_container"]})!=3:raise ValueError("Distinct container targets required")
 if type(v.get("port")) is not int or not 1<=v["port"]<=65535:raise ValueError("Invalid port")
 import ipaddress
 ipaddress.ip_address(v.get("bind_address",""))
 from urllib.parse import urlsplit
 origin=urlsplit(v.get("public_origin",""))
 if origin.scheme not in {"http","https"} or not origin.netloc or origin.username or origin.password or origin.path not in {"","/"} or origin.query or origin.fragment:raise ValueError("Invalid origin")
 return v
def defaults(root=None):
 """Keep legacy installation defaults; resource changes require protected operator configuration."""
 root=Path(root or Path(__file__).resolve().parent.parent)
 try:user=pwd.getpwuid(os.getuid()).pw_name if os.getuid()!=0 else "webagent-build"
 except KeyError:user="webagent-build"
 install=Path("/var/lib/webagent/development/build-service")
 return dict(scheduler=scheduler_defaults(),api_pids_limit=256,code_index_ledger_max_bytes=768*1024**2,instance="development",checkout_root=str(root),service_account=user,source_owner=user,state_root=str(root/"build-jobs"),workspace_root=str(root/"workspaces"),runtime_root=str(root/"runtime/build"),install_root=str(install),service_dir=str(install/"releases/bootstrap"),app_uid=1100,app_gid=1100,worker_uid=1000,worker_gid=1000,api_container="webagent-dev-v3",loop_container="webagent-dev-v3-loop",gate_container="webagent-dev-v3-gate",bind_address="127.0.0.1",port=8081,public_origin="http://127.0.0.1:8081",worker_control="/var/lib/webagent-v3-build-worker/control.sock")
def load(path=None):
 path=path or os.environ.get("WEBAGENT_BUILD_CONFIG")
 if path:
  target=Path(path)
  if target.is_symlink():raise PermissionError("Symlink configuration")
  fd=os.open(target,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC)
  try:
   before=os.fstat(fd)
   if not stat.S_ISREG(before.st_mode) or before.st_nlink!=1 or before.st_uid!=0 or before.st_mode&0o022 or before.st_size>16384:raise PermissionError("Root-owned bounded configuration required")
   raw=os.read(fd,16385);after=os.fstat(fd)
   if (before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=(after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns):raise ValueError("Configuration raced an edit")
   return validate(json.loads(raw))
  finally:os.close(fd)
 # Source-only CLI/test defaults; installed services receive protected config.
 v=defaults();uid=os.getuid() or Path(v["checkout_root"]).stat().st_uid
 v.update(service_uid=uid,service_gid=os.getgid(),source_uid=Path(v["checkout_root"]).stat().st_uid,service_home=str(Path(v["checkout_root"])))
 return v
