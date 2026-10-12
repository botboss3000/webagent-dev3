"""Installation-owned paths and identities, never model arguments."""
import json,os,pwd,re,stat
from pathlib import Path
def validate(value):
 if not isinstance(value,dict):raise ValueError("Configuration must be an object")
 allowed={"instance","checkout_root","service_account","source_owner","state_root","workspace_root","runtime_root","install_root","service_dir","app_uid","app_gid","worker_uid","worker_gid","api_container","loop_container","gate_container","bind_address","port","public_origin","worker_control"}
 if set(value)-allowed:raise ValueError("Unknown installation field")
 v=dict(value)
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
 root=Path(root or Path(__file__).resolve().parent.parent)
 try:user=pwd.getpwuid(os.getuid()).pw_name if os.getuid()!=0 else "webagent-build"
 except KeyError:user="webagent-build"
 install=Path("/var/lib/webagent/development/build-service")
 return dict(instance="development",checkout_root=str(root),service_account=user,source_owner=user,state_root=str(root/"build-jobs"),workspace_root=str(root/"workspaces"),runtime_root=str(root/"runtime/build"),install_root=str(install),service_dir=str(install/"releases/bootstrap"),app_uid=1100,app_gid=1100,worker_uid=1000,worker_gid=1000,api_container="webagent-dev-v3",loop_container="webagent-dev-v3-loop",gate_container="webagent-dev-v3-gate",bind_address="127.0.0.1",port=8081,public_origin="http://127.0.0.1:8081",worker_control="/var/lib/webagent-v3-build-worker/control.sock")
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
