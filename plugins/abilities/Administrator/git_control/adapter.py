"""Typed session-workspace Git. Rust owns permission and network credentials."""
import asyncio,contextvars,hashlib,json,os,re,runpy,signal,stat,uuid
from pathlib import Path
from urllib.parse import urlsplit
LIMIT=65536
SCAN_LINE_LIMIT=1024*1024
# Git hashes/scans whole files; the editor/conflict reader keeps its smaller cap.
GIT_SOURCE_LIMIT=16*1024*1024
NETWORK_TRANSPORT=None
_SANDBOX_ROOT=contextvars.ContextVar("git_workspace_root",default=None)
_RESTRICT=runpy.run_path(str(Path(__file__).resolve().parents[4]/"sandbox.py"))["restrict"]
READS={"status","diff","log","show","branch_list","tag_list","stash_list"}
OPS=READS|{"branch_create","branch_delete","tag_create","tag_delete","checkout","merge","stash_push","stash_pop","add","reset","fetch","pull","push"}
class Rejected(ValueError):
 def __init__(self,message,kind="git_rejected",diagnostics=None):
  super().__init__(message);self.kind=kind;self.diagnostics=diagnostics
class Uncertain(Exception):pass
def exact(args,keys,required=()):
 if not isinstance(args,dict) or set(args)-set(keys) or any(k not in args for k in required):raise Rejected("Invalid typed Git arguments")
def text(v,limit=2000):
 if not isinstance(v,str) or not v or len(v.encode())>limit or "\0" in v:raise Rejected("Invalid Git text")
 return v
def components(raw):
 raw=text(raw,512)
 if raw.startswith("/") or "\\" in raw or any(p in ("",".","..",".git") for p in raw.split("/")):raise Rejected("Path outside workspace source")
 return raw.split("/")
def path(base,raw,missing=False):
 p=base
 for part in components(raw):
  p=p/part
  if p.is_symlink():raise Rejected("Symlink paths unavailable")
 if not missing and not p.exists():raise Rejected("Source path unavailable")
 if p.exists() and not p.is_file():raise Rejected("Exact regular source files required")
 return p
def _directory(fd, component):
 try:return os.open(component,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW|os.O_CLOEXEC,dir_fd=fd)
 except OSError as error:raise Rejected("Source directory unavailable or replaced") from error

def _source_parent(repo,raw):
 parts=components(raw)
 base=Path(_SANDBOX_ROOT.get() or repo.parent)
 try:relative=repo.relative_to(base)
 except ValueError:raise Rejected("Repository escaped current workspace")
 try:fd=os.open(base,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW|os.O_CLOEXEC)
 except OSError as error:raise Rejected("Workspace directory replaced") from error
 try:
  for component in (*relative.parts,*parts[:-1]):
   if component in ("",".","..") or "/" in component:raise Rejected("Invalid source directory")
   next_fd=_directory(fd,component);os.close(fd);fd=next_fd
  return fd,parts[-1]
 except BaseException:os.close(fd);raise

def _read_at(parent,name,missing=False,max_bytes=1048576):
 try:fd=os.open(name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK|os.O_CLOEXEC,dir_fd=parent)
 except FileNotFoundError:
  if missing:return None,None
  raise Rejected("Source file missing")
 except OSError as error:raise Rejected("Source file replaced or unavailable") from error
 try:
  before=os.fstat(fd)
  if not stat.S_ISREG(before.st_mode):raise Rejected("Regular source file required")
  if before.st_size>max_bytes:raise Rejected(f"Source file exceeds {max_bytes//1048576} MiB read limit")
  chunks=[];total=0
  while data:=os.read(fd,65536):
   total+=len(data)
   if total>max_bytes:raise Rejected("Source file exceeds read bound")
   chunks.append(data)
  after=os.fstat(fd)
  if (before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=(after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns):raise Rejected("Source changed during read")
  return b"".join(chunks),before.st_mode&0o777
 finally:os.close(fd)

def read_source(repo,raw,missing=False,max_bytes=1048576):
 parent,name=_source_parent(repo,raw)
 try:return _read_at(parent,name,missing,max_bytes)
 except Rejected as error:raise Rejected(f'{raw}: {error}') from error
 finally:os.close(parent)

def _before_publish(repo,raw):
 # Test seam only. Neither model arguments nor configuration can replace it.
 pass

def replace_source(repo,raw,data,expected):
 parent,name=_source_parent(repo,raw);temporary=".git-resolve-"+uuid.uuid4().hex
 try:
  original,mode=_read_at(parent,name)
  if hashlib.sha256(original).hexdigest()!=expected:raise Rejected("Conflict changed during read")
  fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW|os.O_CLOEXEC,mode,dir_fd=parent)
  os.fchmod(fd,mode)
  with os.fdopen(fd,"wb") as stream:stream.write(data);stream.flush();os.fsync(stream.fileno())
  _before_publish(repo,raw)
  current_parent,_=_source_parent(repo,raw)
  try:
   a,b=os.fstat(parent),os.fstat(current_parent)
   if (a.st_dev,a.st_ino)!=(b.st_dev,b.st_ino):raise Rejected("Conflict parent replaced; reload source")
  finally:os.close(current_parent)
  current,_=_read_at(parent,name)
  if hashlib.sha256(current).hexdigest()!=expected:raise Rejected("Conflict changed before publication")
  os.replace(temporary,name,src_dir_fd=parent,dst_dir_fd=parent)
  os.fsync(parent)
 finally:
  try:os.unlink(temporary,dir_fd=parent)
  except FileNotFoundError:pass
  os.close(parent)

def repository(base,raw):
 p=Path(base).resolve()
 if raw!="__root__":
  for part in components(raw):
   p=p/part
   if p.is_symlink():raise Rejected("Symlink repository unavailable")
 elif p.is_symlink():raise Rejected("Symlink repository unavailable")
 g=p/".git"
 if not p.is_dir() or not g.is_dir() or g.is_symlink():raise Rejected("Ordinary workspace repository required")
 for name in ["commondir","objects/info/alternates","objects/info/http-alternates"]:
  if (g/name).exists() or (g/name).is_symlink():raise Rejected("External Git object/worktree access unavailable")
 if not (g/"config").is_file() or (g/"config").is_symlink() or (g/"config").stat().st_size>LIMIT:raise Rejected("Invalid Git configuration")
 return p
def ref(v):
 v=text(v,200)
 if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]*",v) or any(s in v for s in ("..","//","@{")) or v.endswith(("/",".lock",".")):raise Rejected("Invalid Git reference")
 return v
# Public dummy credential used by regression fixtures, not a path exemption.
_DUMMY_CREDENTIALS={
 b"SECRET_CANARY",
 b"abcdefghijklmnop12345", b"another-secret", b"discovery-key", b"first-secret",
 b"fixture-audio-key", b"fixture-no-network-key", b"fixture-only",
 b"fixture-password", b"fixture-token", b"isolated-key", b"must-not-project",
 b"never-forward", b"never-leak-key", b"second-secret", b"test-key-no-network",
 b"test-password-only", b"valid-fixture-secret",
}
def secret(data):
 data=re.sub(rb"sk-1234567890abcdef(?![A-Za-z0-9_-])",b"<public-redaction-fixture>",data)
 if re.search(rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----|sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|AKIA[A-Z0-9]{16}",data,re.I):return True
 for match in re.finditer(rb"""(?:https?|ssh)://[^\s/@:"\']+:[^\s/@"\']+@[^\s/"']+""",data,re.I):
  # Reserved documentation host with deliberately trivial URL credentials.
  if match.group().lower() not in {
   b"https://u:p@example.test", b"https://user:password@example.invalid",
   # Deliberately trivial credentials in the Debian URL rejection fixture.
   b"https://user:password@deb.debian.org",
  }:return True
 # Complete literals, not prefixes of variable names and function calls.
 pattern=rb"""(?<![A-Za-z0-9_-])(?:api[_-]?key|access[_-]?token|password|secret)["']?\s*[:=]\s*(?:["']([A-Za-z0-9_./+!=-]{12,})["']|([A-Za-z0-9_./+!=-]{12,})(?=[\t ]*(?:\r?$|["'])))"""
 for match in re.finditer(pattern,data,re.I|re.M):
  value=match.group(1) or match.group(2)
  if value not in _DUMMY_CREDENTIALS:return True
 return False
async def command(repo,argv,mutable=False,env_extra=None,local=False,timeout=30,scan_only=False,reviewed_lines=None):
 env={"PATH":"/usr/bin:/bin","LANG":"C.UTF-8","HOME":str(repo.parent),"GIT_CONFIG_NOSYSTEM":"1","GIT_CONFIG_GLOBAL":"/dev/null","GIT_OPTIONAL_LOCKS":"0","GIT_TERMINAL_PROMPT":"0","GIT_EDITOR":":","GIT_SEQUENCE_EDITOR":":","GIT_NO_REPLACE_OBJECTS":"1"}
 if env_extra:
  if not isinstance(env_extra,dict) or any(k not in {"GIT_ASKPASS","V3_GIT_URL","V3_GIT_USERNAME","V3_GIT_TOKEN"} or not isinstance(v,str) for k,v in env_extra.items()):raise Rejected("Invalid trusted transport")
  env.update(env_extra)
 opts=["-c","safe.directory="+str(repo),"-c","core.hooksPath=/dev/null","-c","core.fsmonitor=false","-c","diff.external=","-c","commit.gpgsign=false","-c","tag.gpgsign=false","-c","credential.helper=","-c","credential.useHttpPath=true","-c","http.followRedirects=false","-c","http.sslVerify=true","-c","protocol.ext.allow=never","-c","protocol.file.allow="+("always" if local else "never")]
 proc=await asyncio.create_subprocess_exec("git",*opts,*argv,cwd=repo,env=env,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.STDOUT,start_new_session=True,preexec_fn=lambda:(_RESTRICT(_SANDBOX_ROOT.get() or repo.parent,read_paths=(str(Path(__file__).resolve().parent/"askpass.py"),)) if env_extra else _RESTRICT(_SANDBOX_ROOT.get() or repo.parent)))
 output=bytearray()
 async def drain():
  pending=bytearray();new_line=None
  def scan_line(line):
   nonlocal new_line
   if len(line)>SCAN_LINE_LIMIT:raise Rejected("Outgoing history line exceeds scan bound")
   hunk=re.match(rb"@@ -[0-9]+(?:,[0-9]+)? \+([0-9]+)",line)
   if hunk:
    new_line=int(hunk.group(1))
    end=line.find(b"@@",2)
    if end>=0:line=line[:end+2]+b"\n"
   location=new_line if line.startswith(b"+") and not line.startswith(b"+++") else None
   if reviewed_lines and location is not None and hashlib.sha256(line).hexdigest() in reviewed_lines:
    line=b"+<reviewed synthetic assertion>\n"
   elif line.startswith(b"-") and not line.startswith(b"---"):
    # Removed text is not sent in the new tree; its introducing commit is scanned separately.
    line=b"-<removed line>\n"
   output.extend(line)
   if secret(bytes(output)):
    raise Rejected("Outgoing history withheld by credential guard; inspect outgoing commits for credential-shaped content","git_credential_guard",{"line":location} if location is not None else None)
   if re.search(rb"\s{4096}",output):raise Rejected("Outgoing history whitespace exceeds scan bound")
   if len(output)>LIMIT:del output[:-LIMIT]
   if new_line is not None and (line.startswith(b"+") and not line.startswith(b"+++") or line.startswith(b" ")):new_line+=1
  while chunk:=await proc.stdout.read(8192):
   if scan_only:
    pending.extend(chunk)
    while (index:=pending.find(b"\n"))>=0:
     line=bytes(pending[:index+1]);del pending[:index+1];scan_line(line)
    if len(pending)>SCAN_LINE_LIMIT:raise Rejected("Outgoing history line exceeds scan bound")
   else:
    output.extend(chunk)
    if len(output)>LIMIT:raise Rejected("Git output exceeds limit")
  if scan_only and pending:scan_line(bytes(pending))
  await proc.wait()
 try:await asyncio.wait_for(drain(),timeout)
 except BaseException as error:
  if proc.returncode is None:
   try:os.killpg(proc.pid,signal.SIGTERM)
   except ProcessLookupError:pass
   try:await asyncio.wait_for(proc.wait(),2)
   except asyncio.TimeoutError:
    try:os.killpg(proc.pid,signal.SIGKILL)
    except ProcessLookupError:pass
    await proc.wait()
  # Close the captured pipe after an early scan rejection as well as the child.
  try:await asyncio.wait_for(proc.communicate(),2)
  except asyncio.TimeoutError:pass
  if isinstance(error,asyncio.CancelledError):raise
  if mutable:raise Uncertain("Interrupted Git effect uncertain; inspect repository before retry") from error
  if scan_only and isinstance(error,Rejected):raise
  raise Rejected("Git read timed out or exceeded output limit") from error
 if secret(bytes(output)) or env_extra and env_extra.get("V3_GIT_TOKEN") and env_extra["V3_GIT_TOKEN"].encode() in output:
  if mutable:raise Uncertain("Git output withheld after possible effect; reconcile repository before retry")
  raise Rejected("Git output withheld by credential guard")
 if proc.returncode:
  # Only explicit pre-transfer HTTPS failures are known to have no effect.
  # Emit fixed guidance, never remote stderr (which may contain credentials).
  if env_extra and argv[0]=="push":
   if b"fatal: Authentication failed for " in output:
    raise Rejected("GitHub authentication rejected the saved repository key; update it in the repository configuration or Vault","git_auth_failed")
   if b"fatal: unable to access " in output and b"The requested URL returned error: 403" in output:
    raise Rejected("GitHub denied push access; check the saved key's repository access and write permission","git_push_forbidden")
   if b"remote: Repository not found." in output:
    raise Rejected("GitHub repository unavailable; check the repository URL and the saved key's access","git_repository_unavailable")
  if mutable:raise Uncertain("Git mutation did not finish cleanly; reconcile local and remote state before retry")
  raise Rejected("Git operation rejected; inspect workspace status")
 return b"" if scan_only else bytes(output)
async def validate(repo,bare=False):
 for item in (await command(repo,["config","--local","--null","--list"])).split(b"\0"):
  key=item.split(b"\n",1)[0].decode().lower()
  if not key:continue
  if key.startswith(("include.","includeif.","filter.","credential.","url.","http.","https.")) or key in {"core.sshcommand","core.gitproxy","core.worktree","extensions.worktreeconfig","extensions.partialclone"} or key=="core.bare" and not item.endswith(b"\ntrue" if bare else b"\nfalse") or re.match(r"(?:merge\..*\.driver|remote\..*\.(?:uploadpack|receivepack|vcs|promisor|proxy|pushurl))$",key):raise Rejected("Unsafe Git configuration")
 metadata=repo if bare else repo/".git"
 for count,entry in enumerate(metadata.rglob("*")):
  if count>10000 or entry.is_symlink():raise Rejected("Unsafe or oversized Git metadata")
 if bare:
  if (await command(repo,["rev-parse","--is-bare-repository"])).strip()!=b"true":raise Rejected("Workspace remote must be bare")
  if any(p.is_file() and not p.name.endswith(".sample") for p in (metadata/"hooks").glob("*")):raise Rejected("Workspace remote hooks unavailable")
async def head(repo):return (await command(repo,["rev-parse","--verify","HEAD"])).decode().strip()
async def review_digest(repo):
 parts=[await head(repo),(await command(repo,["status","--porcelain=v1","-z"])).hex(),(await command(repo,["diff","--no-ext-diff","--no-textconv","HEAD","--"])).hex()]
 for raw in sorted(filter(None,(await command(repo,["ls-files","--others","--exclude-standard","-z"])).split(b"\0"))):
  name=raw.decode();data,_=read_source(repo,name,max_bytes=GIT_SOURCE_LIMIT)
  parts.append([name,hashlib.sha256(data).hexdigest()])
 return hashlib.sha256(json.dumps(parts,sort_keys=True).encode()).hexdigest()
async def snapshot(base,repo="webagent-v3"):
 _SANDBOX_ROOT.set(Path(base).resolve())
 p=repository(base,repo);await validate(p)
 result={"head":await head(p),"status":(await command(p,["status","--porcelain=v1"])).decode(),"review_sha256":await review_digest(p)}
 try:
  result["ahead"]=int((await command(p,["rev-list","--count","@{upstream}..HEAD"])).decode());result["has_upstream"]=True
 except Rejected:
  result["ahead"]=int((await command(p,["rev-list","--count","HEAD","--not","--remotes"])).decode());result["has_upstream"]=False
 return result
def notice(value,previous=None):
 digest=hashlib.sha256(json.dumps(value,sort_keys=True).encode()).hexdigest()
 return {"snapshot_sha256":digest,"notify":digest!=previous and (bool(value["status"]) or bool(value.get("ahead"))),"dirty":bool(value["status"]),"head":value["head"],"ahead":value.get("ahead")}
async def transport(base,repo,remote,operation):
 remote=ref(remote);url=(await command(repo,["remote","get-url",remote])).decode().strip()
 if secret(url.encode()) or any(c.isspace() for c in url):raise Rejected("Unsafe configured remote")
 parsed=urlsplit(url)
 scp=bool(re.fullmatch(r"[A-Za-z0-9_.-]+@[A-Za-z0-9.-]+:[A-Za-z0-9_./-]+",url))
 if not parsed.scheme and not scp:
  target=(repo/url).resolve() if not url.startswith("/") else Path(url).resolve()
  if not target.is_relative_to(Path(base).resolve()) or not target.is_dir():raise Rejected("Remote outside workspace")
  if (target/"commondir").exists() or (target/"objects/info/alternates").exists():raise Rejected("Unsafe workspace remote")
  await validate(target,bare=True)
  return str(target),{},True
 if parsed.scheme not in ("https","ssh") and not scp:raise Rejected("Remote transport unavailable")
 if parsed.password or parsed.scheme=="https" and parsed.username:raise Rejected("Credential-bearing URL refused")
 if NETWORK_TRANSPORT is None:raise Rejected("Remote requires trusted credential transport")
 env=await NETWORK_TRANSPORT(repo,remote,operation)
 if not isinstance(env,dict):raise Rejected("Configured remote not authorized")
 return remote,env,False
async def outgoing_guard_location(repo,remote,scan_head):
 """Locate one blocked outgoing diff without returning Git content or secrets."""
 try:
  commits=(await command(repo,["rev-list","--max-count=33",scan_head,"--not","--remotes="+ref(remote)])).decode().splitlines()
  for sha in commits[:32]:
   if not re.fullmatch(r"[a-f0-9]{40,64}",sha):return {"status":"unavailable"}
   paths=(await command(repo,["diff-tree","--root","--no-commit-id","--name-only","-r","-z",sha])).split(b"\0")
   for raw in paths[:256]:
    if not raw:continue
    try:
     name=raw.decode("utf-8")
     await command(repo,["show","--format=","--no-ext-diff","--no-textconv","-p",sha,"--",name],scan_only=True,timeout=30)
    except Rejected as error:
     if error.kind!="git_credential_guard":continue
     safe=bool(re.fullmatch(r"[A-Za-z0-9._/-]{1,240}",name)) and not secret(raw)
     result={"status":"located","commit":sha,"match_class":"credential_pattern"}
     if safe:result["path"]=name
     else:result["path_sha256"]=hashlib.sha256(raw).hexdigest();result["path_withheld"]=True
     return result
  return {"status":"unlocated","search_bounded":len(commits)>32}
 except (Rejected,UnicodeError,ValueError):
  return {"status":"unavailable"}

# SHA256 of full added diff lines in reviewed, already committed regression tests.
# The exception cannot be selected by tool arguments and never applies to another commit/path.
_REVIEWED_HISTORY_LINES={
 # Models tests exercise encrypted storage and device-flow fencing with literal dummy tokens.
 ("22934cce71b2157b3e257b02f266256f5e4a1833","crates/modules/models/src/model_io_tests.rs"):frozenset({
  "9a06dea7e24d76373a11501db5bfd1b881850f95c4cb85231b1b2ac379d4ed74",
 }),
 ("22934cce71b2157b3e257b02f266256f5e4a1833","crates/modules/models/src/runtime.rs"):frozenset({
  "d8361830b286f433a795dd4c4dde6560da19cfdef8cc6c64cf34d371272ed359",
  "6749b873c877027b761e26054e497da416b04d26f3005cc4b30ca342c672c24d",
 }),
 # Manager fixture supplies a dummy token only to its temporary loopback mock provider.
 ("4f2442caad651fed6d64cb7147f48c512a352246","crates/modules/manager/src/tests.rs"):frozenset({
  "9232707750cebd746de222b1620b3a04c17b31500b8e0ae973ffb06cb5d8b4b1",
 }),
 # UI observer fixture uses a public sentinel to prove private arguments stay hidden.
 ("f2d332c246ce23dae46b6267b5301a59182df87f","ui/chat/core/chat-notifications.browser.test.cjs"):frozenset({
  "6ef6c6a7a40bbb800a8a9b78261065a47f9d524d661cfcce61ee8202761596d6",
 }),
 ("0aa6da670fba3fed8018af1c125eaca164f6f3a0","plugins/abilities/Administrator/git_control/test_runner.py"):frozenset({
  "5251d40c76a281311caf4b366d595672db2b280d6c9454d50da72aa03dfc1a5c",
  "d20e527b2b21a4eaeb67130f34d20c2884f3a6680ce6d6296822212b0c333784",
 }),
 ("ec514257598a923a9ef6b55723e2dc1d41cf16cc","plugins/abilities/Administrator/git_control/test_runner.py"):frozenset({
  "1cb448b9870982d4ffc892f4ade5af24c52ac6d4e60eecce42d4c65bb91a507e",
 })
}
async def scan_outgoing_history(repo,remote,scan_head):
 commits=(await command(repo,["rev-list","--max-count=257",scan_head,"--not","--remotes="+ref(remote)])).decode().splitlines()
 if len(commits)>256:raise Rejected("Outgoing history exceeds credential scan commit bound","git_credential_guard",{"status":"unavailable","reason":"commit_bound"})
 for sha in commits:
  if not re.fullmatch(r"[a-f0-9]{40,64}",sha):raise Rejected("Invalid outgoing commit","git_credential_guard")
  try:await command(repo,["show","--no-patch","--format=fuller",sha],scan_only=True)
  except Rejected as error:
   if error.kind=="git_credential_guard":raise Rejected(str(error),error.kind,{"status":"located","commit":sha,"match_class":"credential_pattern"}) from error
   raise
  paths=(await command(repo,["diff-tree","-m","--root","--no-renames","--no-commit-id","--name-only","-r","-z",sha])).split(b"\0")
  if len(paths)>257:raise Rejected("Outgoing commit exceeds credential scan path bound","git_credential_guard",{"status":"located","commit":sha,"reason":"path_bound"})
  for raw in filter(None,paths):
   name=raw.decode("utf-8","surrogateescape")
   safe=bool(re.fullmatch(r"[A-Za-z0-9._/-]{1,240}",name)) and not secret(raw)
   try:
    await command(repo,["show","-m","--format=","--no-ext-diff","--no-textconv","--no-renames","--unified=0","-p",sha,"--",name],scan_only=True,reviewed_lines=_REVIEWED_HISTORY_LINES.get((sha,name)),timeout=30)
   except Rejected as error:
    if error.kind!="git_credential_guard":raise
    location={"status":"located","commit":sha,"match_class":"credential_pattern"}
    if safe:location["path"]=name
    else:location["path_sha256"]=hashlib.sha256(raw).hexdigest();location["path_withheld"]=True
    if error.diagnostics and type(error.diagnostics.get("line"))is int:location["line"]=error.diagnostics["line"]
    raise Rejected(str(error),error.kind,location) from error

async def network(base,repo,remote,operation):
 target,env,local=await transport(base,repo,remote,operation)
 if operation=="push":
  # Scan exact outgoing commits and paths; reviewed fixture lines are immutable identities.
  scan_head=await head(repo)
  await scan_outgoing_history(repo,remote,scan_head)
  await fence(repo,scan_head)
 branch=ref((await command(repo,["symbolic-ref","--quiet","--short","HEAD"])).decode().strip()) if operation!="fetch" else ""
 argv=["fetch","--no-tags","--prune",ref(remote)] if operation=="fetch" else (["pull","--ff-only","--no-rebase",target,branch] if operation=="pull" else ["push","--no-verify",target,"HEAD:refs/heads/"+branch])
 output=await command(repo,argv,mutable=True,env_extra=env,local=local,timeout=120)
 if local and operation=="push":
  await command(repo,["update-ref","refs/remotes/"+ref(remote)+"/"+branch,await head(repo)],mutable=True)
 return output
def paths(repo,values,max_paths=64):
 if not isinstance(values,list) or not values or len(values)>max_paths or any(not isinstance(v,str) for v in values) or len(set(values))!=len(values):raise Rejected(f"Specify 1 to {max_paths} unique exact paths")
 for v in values:path(repo,v,True)
 return values
async def fence(repo,expected):
 if not isinstance(expected,str) or not re.fullmatch("[a-f0-9]{40,64}",expected) or await head(repo)!=expected:raise Rejected("HEAD changed; review before retry")
def default_repository(args):
 result=dict(args)
 if result.get("repo") in ("",None):result.pop("repo",None)
 if "remote" in result and result["remote"] in ("",None):result.pop("remote")
 return result
def operation_arguments(args,op):
 common={"operation","repo","expected_head"}
 applicable={
  "status":set(),"diff":{"paths","cached"},"log":{"paths","ref","limit"},
  "show":{"paths","ref"},"branch_list":set(),"tag_list":set(),"stash_list":set(),
  "branch_create":{"name","ref"},"branch_delete":{"name"},"tag_create":{"name","ref"},
  "tag_delete":{"name"},"checkout":{"ref","paths"},"merge":{"ref"},"stash_push":{"message","paths"},
  "stash_pop":set(),"add":{"paths"},"reset":{"paths"},"fetch":{"remote"},"pull":{"remote"},"push":{"remote"}}
 allowed=common|applicable[op]
 for key,value in args.items():
  inert=value in ("",None) or key=="paths" and value==[] or key=="cached" and value is False or key=="limit" and type(value) is int and 1<=value<=100
  if key not in allowed and not inert:
   raise Rejected("Arguments do not apply to this Git operation")
 result={key:value for key,value in args.items() if key in allowed}
 result=default_repository(result)
 if op in ("log","show","branch_create","tag_create") and result.get("ref") in ("",None):result.pop("ref",None)
 if op=="stash_push" and result.get("message") in ("",None):result.pop("message",None)
 return result
async def git_tool(base,args):
 exact(args,{"operation","repo","paths","ref","name","message","remote","limit","cached","expected_head"},{"operation"})
 op=args["operation"]
 if op not in OPS:raise Rejected("Unsupported typed Git operation")
 args=operation_arguments(args,op)
 p=repository(base,args.get("repo","webagent-v3"));await validate(p)
 if op not in READS:await fence(p,args.get("expected_head"))
 ps=paths(p,args["paths"]) if "paths" in args and args["paths"] not in ([],None) else []
 count=args.get("limit",20)
 if type(count) is not int or not 1<=count<=100 or "cached" in args and type(args["cached"]) is not bool:raise Rejected("Invalid Git option")
 if op=="status":
  value=await snapshot(base,args.get("repo","webagent-v3"));value["notice"]=notice(value);return value
 if op in ("fetch","pull","push"):return {"text":(await network(base,p,args.get("remote","origin"),op)).decode()}
 name=ref(args.get("name")) if op in ("branch_create","branch_delete","tag_create","tag_delete") else "main"
 selected=ref(args.get("ref")) if op in ("checkout","merge") else ref(args.get("ref","HEAD"))
 argv={"diff":["diff","--no-ext-diff","--no-textconv",*(["--cached"] if args.get("cached") else []),"--",*ps],"log":["log","--no-show-signature",f"-{count}","--format=%H %s",selected,"--",*ps],"show":["show","--no-ext-diff","--no-textconv","--no-show-signature",selected,"--",*ps],"branch_list":["branch","--list"],"branch_create":["branch",name,selected],"branch_delete":["branch","-d",name],"tag_list":["tag","--list"],"tag_create":["tag",name,selected],"tag_delete":["tag","-d",name],"checkout":["checkout",selected,"--",*ps] if ps else ["checkout",selected],"merge":["merge","--no-edit","--no-ff",selected],"stash_list":["stash","list"],"stash_push":["stash","push","-m",text(args.get("message","Workspace checkpoint")),"--",*ps],"stash_pop":["stash","pop"],"add":["add","--",*ps],"reset":["reset","HEAD","--",*ps]}[op]
 if op in ("add","reset") and not ps:raise Rejected("Exact paths required")
 return {"text":(await command(p,argv,mutable=op not in READS)).decode()}
async def resolve_conflict(base,args):
 args=default_repository(args)
 exact(args,{"repo","path","strategy","expected_sha256"},{"path","strategy","expected_sha256"})
 p=repository(base,args.get("repo","webagent-v3"));await validate(p)
 data,_=read_source(p,args["path"])
 if hashlib.sha256(data).hexdigest()!=args["expected_sha256"]:raise Rejected("Conflict file changed")
 strategy=args["strategy"]
 if strategy not in ("ours","theirs","both"):raise Rejected("Invalid conflict strategy")
 if not await command(p,["ls-files","--unmerged","--",args["path"]]):raise Rejected("Path is not unresolved")
 if strategy=="both":
  out=[];left=[];right=[];mode="plain";found=False
  for line in data.splitlines(keepends=True):
   if line.startswith(b"<<<<<<< ") and mode=="plain":mode="ours";left=[];right=[];found=True
   elif line.startswith(b"=======") and mode=="ours":mode="theirs"
   elif line.startswith(b">>>>>>> ") and mode=="theirs":out.extend(left+right);mode="plain"
   elif line.startswith((b"<<<<<<< ",b">>>>>>> ",b"=======")):raise Rejected("Malformed conflict markers")
   elif mode=="ours":left.append(line)
   elif mode=="theirs":right.append(line)
   else:out.append(line)
  if mode!="plain" or not found:raise Rejected("Incomplete conflict markers")
  result=b"".join(out)
 else:result=await command(p,["show",(":"+"2" if strategy=="ours" else ":3")+":"+args["path"]])
 if secret(result):raise Rejected("Conflict content contains possible credentials")
 replace_source(p,args["path"],result,args["expected_sha256"])
 return {"path":args["path"],"sha256":hashlib.sha256(result).hexdigest(),"staged":False}
async def commit_and_push(base,args):
 args=default_repository(args)
 exact(args,{"repo","paths","message","expected_head","expected_review_sha256","remote"},{"paths","message","expected_head","expected_review_sha256"})
 p=repository(base,args.get("repo","webagent-v3"));await validate(p);await fence(p,args["expected_head"])
 if await review_digest(p)!=args["expected_review_sha256"]:raise Rejected("Review changed")
 ps=paths(p,args["paths"]);message=text(args["message"])
 if secret(message.encode()):raise Rejected("Commit message contains possible credentials")
 for name in ps:
  data,_=read_source(p,name,True,max_bytes=GIT_SOURCE_LIMIT)
  if data is not None and secret(data):raise Rejected("Source contains possible credentials")
 await transport(base,p,args.get("remote","origin"),"push")
 await fence(p,args["expected_head"])
 if await review_digest(p)!=args["expected_review_sha256"]:raise Rejected("Review changed during remote authorization")
 if any(n.decode() not in ps for n in (await command(p,["diff","--cached","--name-only","-z"])).split(b"\0") if n):raise Rejected("Unselected staged paths present")
 await command(p,["add","--",*ps],mutable=True)
 await command(p,["-c","user.name=WebAgent","-c","user.email=webagent@localhost","commit","--no-verify","-m",message,"--",*ps],mutable=True)
 committed=await head(p)
 try:output=await network(base,p,args.get("remote","origin"),"push")
 except Exception as error:raise Uncertain("Commit created ("+committed+"); reconcile remote before retry") from error
 return {"commit":committed,"text":output.decode()}
async def execute(name,args,base):
 token=_SANDBOX_ROOT.set(Path(base).resolve())
 try:
  if name=="git_tool":output=await git_tool(base,args)
  elif name=="resolve_conflict":output=await resolve_conflict(base,args)
  elif name=="commit_and_push":output=await commit_and_push(base,args)
  else:raise Rejected("Unknown Git Control tool")
  return {"status":"success","output":output}
 except asyncio.CancelledError:raise
 except Uncertain as error:return {"status":"error","error_kind":"git_effect_uncertain","uncertain":True,"output":str(error)}
 except Exception as error:
  result={"status":"error","error_kind":error.kind if isinstance(error,Rejected) else "git_rejected","output":str(error) if isinstance(error,Rejected) else "Workspace Git rejected"}
  if isinstance(error,Rejected) and error.diagnostics is not None:result["diagnostics"]=error.diagnostics
  return result
 finally:_SANDBOX_ROOT.reset(token)
