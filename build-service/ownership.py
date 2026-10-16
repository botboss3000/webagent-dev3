"""Durable source reservations; successful builds never imply agent completion.

Only broker-derived owners, scopes and verified evidence enter this store.
Config: protected build-service.json -> scheduler ownership heartbeat policy.
"""
import contextlib,hashlib,json,os,re,sqlite3,stat,tarfile,time
from pathlib import Path

ACTIVE={"owned","validating","orphaned","takeover_pending"}
def canonical(value):
    """Bind retry receipts to their exact normalized request."""
    return json.dumps(value,sort_keys=True,separators=(",",":"))
def source_path(value):
    """Accept source-relative reservations, excluding state and traversal."""
    if not isinstance(value,str)or len(value)>240 or not re.fullmatch(r"[A-Za-z0-9_./-]+",value):
        raise ValueError("Invalid reservation path")
    value=value.removeprefix("v3/").rstrip("/")
    if any(p in ("",".","..")for p in value.split("/"))or value.split("/")[0]in {"data","workspaces","target","runtime","build-jobs",".git","v2_ref","deployment-secrets"}:
        raise ValueError("Excluded reservation path")
    return value
def overlap(left,right):
    """Directory reservations conflict with every descendant, not string-prefix siblings."""
    return left==right or left.startswith(right+"/")or right.startswith(left+"/")

def scope_overlap(left,right):
    """Retain legacy UI module exclusions when a new task requests only write paths."""
    if left.startswith("module:ui:")and right.startswith("path:ui/"):
        return overlap("ui/"+left.removeprefix("module:ui:"),right[5:])
    if right.startswith("module:ui:")and left.startswith("path:ui/"):
        return scope_overlap(right,left)
    return left==right if left.startswith("module:")or right.startswith("module:")else overlap(left[5:],right[5:])

class Components:
    """Conservative actual build grouping; manifests cannot override compiled dependencies."""
    def __init__(self,root,compile_inputs=(),ui_file_reservations=False):
        """Bind task source declarations to compiler-attested dependency paths."""
        self.root=Path(root);self.compile_inputs={source_path(p)for p in compile_inputs}
        self.ui_file_reservations=ui_file_reservations
    def deleted_isolated(self,path,parts):
        """Read a bounded prior manifest from the broker baseline only when executable source is deleted."""
        target=self.root/path
        if target.exists()or target.is_symlink():return False
        baseline=self.root.parent/"baseline.tar"
        if baseline.is_symlink():raise PermissionError("Baseline manifest symlink rejected")
        if not baseline.exists():return False
        fd=os.open(baseline,os.O_RDONLY|os.O_NOFOLLOW)
        with os.fdopen(fd,"rb")as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):raise PermissionError("Regular sealed baseline required")
            with tarfile.open(fileobj=stream,mode="r:")as archive:
                try:member=archive.getmember("v3/"+"/".join(parts[:4])+"/plugin.json")
                except KeyError:return False
                if not member.isfile()or not 0<member.size<=65536:return False
                with archive.extractfile(member)as body:
                    try:return json.loads(body.read(65537)).get("dispatch")=="isolated"
                    except (ValueError,AttributeError):return False
    def scopes(self,paths):
        """Return canonical module and path claims, retaining core ownership for embedded inputs."""
        output=set()
        for raw in paths:
            path=source_path(raw);parts=path.split("/");output.add("path:"+path)
            if path.startswith("plugins/abilities/")and len(parts)>=4:
                output.add("module:ability:"+"/".join(parts[2:4]))
                manifest=self.root.joinpath(*parts[:4],"plugin.json")
                isolated=False
                if manifest.is_file()and not manifest.is_symlink()and manifest.stat().st_size<=65536:
                    try:isolated=json.loads(manifest.read_bytes()).get("dispatch")=="isolated"
                    except (ValueError,AttributeError):pass
                elif not manifest.exists()and not manifest.is_symlink():
                    isolated=self.deleted_isolated(path,parts)
                if path in self.compile_inputs or(path.endswith(".rs")and not isolated):output.add("module:engine")
            elif path.startswith("plugins/engine/")or path.startswith((
                "crates/context-service/","crates/context-assembly/","crates/context-contracts/",
                "crates/background-service/","crates/background-dispatch/","crates/background-contracts/",
                "crates/boot-service/","crates/boot-supervisor/","crates/boot-contracts/",
                "crates/assets-service/","crates/context-assets/","crates/assets-contracts/",
                "crates/capability-service/","crates/capability-context/","crates/capability-contracts/",
                "crates/provider-service/","crates/provider-projection/","crates/provider-contracts/",
                "crates/engine-service/")):
                # Shared wrapper changes affect each dependent service; contracts also require the host.
                components={
                    "context-assembly":("context-service","context-assembly","context-contracts"),
                    "background-dispatch":("background-service","background-dispatch","background-contracts"),
                    "boot-supervisor":("boot-service","boot-supervisor","boot-contracts"),
                    "context-assets":("assets-service","context-assets","assets-contracts"),
                    "capability-context":("capability-service","capability-context","capability-contracts"),
                    "provider-projection":("provider-service","provider-projection","provider-contracts"),
                }
                shared=path.startswith("crates/engine-service/")
                matched=False
                for component,crates in components.items():
                    if path.startswith("plugins/engine/"+component+"/")or any(path.startswith("crates/"+name+"/")for name in crates)or(shared and component!="context-assembly"):
                        output.add("module:engine-component:"+component);matched=True
                if not matched and path!="plugins/engine/catalog.json":output.add("module:engine")
                if path=="plugins/engine/catalog.json":
                    output.update("module:engine-component:"+component for component in components)
                if path in self.compile_inputs or any(path.startswith("crates/"+name+"/")for name in (
                    "context-contracts","background-contracts","boot-contracts",
                    "assets-contracts","capability-contracts","provider-contracts")):
                    output.add("module:engine")
            elif path.startswith("build-service/")or path.startswith("tests/test_build")or path.startswith("tests/test_scheduler")or path in {"tests/test_code_index_maintenance.py","tests/test_published_code_index.py","tests/test_index_queue.py","tests/test_ui_policy.py","tests/test_admin_build_install.py"}:
                # These Python suites validate broker/index contracts, not the Rust engine.
                output.add("module:broker")
                if any(overlap(path,p)for p in self.compile_inputs):output.add("module:engine")
            elif path.startswith("ui/"):
                if not self.ui_file_reservations:output.add("module:ui:"+ (parts[1]if len(parts)>1 else "shell"))
                # Compiler attestations override UI eligibility, including subtree claims.
                if any(overlap(path,p)for p in self.compile_inputs):output.add("module:engine")
            elif path.startswith("docs/")or(path.startswith("config/")and path.endswith(".json")and path not in self.compile_inputs):
                output.add("module:asset:"+path)
            else:output.add("module:engine")
        return sorted(output)

class Connection(sqlite3.Connection):
    """SQLite context managers commit transactions and also close their owned descriptor."""
    def __exit__(self,*args):
        """Commit or roll back this transaction, then close its owned descriptor."""
        try:return super().__exit__(*args)
        finally:self.close()

# @code-index {"version":1,"feature":"build-ownership-integrity","aliases":["task fencing","verified builds"],"purpose":"Durable claims, epochs and completion transactions; verified acceptance is an invariant.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Identity, exact input verification and completion rules cannot be disabled by model or preference configuration."}}
class Ownership:
    """Transactional claims and fencing outside application storage; all transitions are durable."""
    def __init__(self,path,clock=time.time):
        """Create the private owned coordination database and durable transition tables."""
        self.path=Path(path);self.clock=clock
        if self.path.is_symlink():raise PermissionError("Ownership database symlink")
        self.path.parent.mkdir(parents=True,exist_ok=True)
        fd=os.open(self.path,os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW,0o600);info=os.fstat(fd);os.close(fd)
        if not stat.S_ISREG(info.st_mode)or info.st_nlink!=1 or info.st_uid!=os.getuid()or info.st_mode&0o077:raise PermissionError("Ownership database identity")
        with self.connect()as db:
            db.executescript("""
            CREATE TABLE IF NOT EXISTS tasks(
              id TEXT PRIMARY KEY, owner TEXT NOT NULL, label TEXT NOT NULL,
              state TEXT NOT NULL, revision INTEGER NOT NULL, epoch INTEGER NOT NULL,
              generation INTEGER NOT NULL, heartbeat REAL NOT NULL, created REAL NOT NULL,
              scopes TEXT NOT NULL, requested TEXT NOT NULL, evidence TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS receipts(id TEXT PRIMARY KEY,digest TEXT NOT NULL,result TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY,task TEXT NOT NULL,at REAL NOT NULL,event TEXT NOT NULL);
            PRAGMA user_version=1;
            """)
    def connect(self):
        """Bound SQLite waits and keep writes serialized with explicit transactions."""
        db=sqlite3.connect(self.path,timeout=5,factory=Connection);db.row_factory=sqlite3.Row
        db.execute("PRAGMA busy_timeout=5000");db.execute("PRAGMA foreign_keys=ON")
        return db
    @contextlib.contextmanager
    def transaction(self):
        """Roll back every transition and retry receipt together on failure."""
        db=self.connect()
        try:
            db.execute("BEGIN IMMEDIATE");yield db;db.commit()
        except BaseException:db.rollback();raise
        finally:db.close()
    @staticmethod
    def decode(row):
        """Decode structured metadata without exposing source or application data."""
        value=dict(row)
        for key in ["scopes","requested","evidence"]:value[key]=json.loads(value[key])
        return value
    def get(self,task,db=None):
        """Return one logical task; missing identifiers are explicit failures."""
        if db is None:
            with self.connect()as connection:return self.get(task,connection)
        row=db.execute("SELECT * FROM tasks WHERE id=?",(task,)).fetchone()
        if row is None:raise ValueError("Logical task not found")
        return self.decode(row)
    def list(self,db=None):
        """Bound the visible coordination inventory; omit retry capabilities and source text."""
        if db is None:
            with self.connect()as connection:return self.list(connection)
        return [self.decode(r)for r in db.execute("SELECT * FROM tasks ORDER BY created,id LIMIT 1000")]
    def mutate(self,request,body,operation):
        """Exactly-once durable mutations, including rejected acquisitions that enter the queue."""
        if not isinstance(request,str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",request):raise ValueError("Stable request ID required")
        digest=hashlib.sha256(canonical(body).encode()).hexdigest()
        with self.transaction()as db:
            old=db.execute("SELECT * FROM receipts WHERE id=?",(request,)).fetchone()
            if old:
                if old["digest"]!=digest:raise ValueError("Request ID reused")
                return json.loads(old["result"])
            result=operation(db)
            db.execute("INSERT INTO receipts VALUES(?,?,?)",(request,digest,canonical(result)))
            return result
    def receipt(self,request):
        """Read a private transition receipt during exact-request recovery."""
        with self.connect()as db:
            row=db.execute("SELECT result FROM receipts WHERE id=?",(request,)).fetchone()
            return json.loads(row["result"])if row else None
    def begin(self,task,owner,label,request):
        """Create a logical owner with no implicit source reservation or execution authority."""
        if not re.fullmatch(r"[a-f0-9]{64}",task)or not owner or len(owner)>512 or len(label)>80:raise ValueError("Invalid task identity")
        def operation(db):
            """Insert one owner identity or return its matching existing task."""
            old=db.execute("SELECT * FROM tasks WHERE id=?",(task,)).fetchone()
            if old:
                if old["owner"]!=owner or old["label"]!=label:raise PermissionError("Task owner mismatch")
                return self.decode(old)
            if db.execute("SELECT count(*) FROM tasks").fetchone()[0]>=1000:raise ValueError("Ownership registry full")
            now=self.clock()
            db.execute("INSERT INTO tasks VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",(task,owner,label,"waiting",0,1,0,now,now,"[]","[]","{}"))
            return self.get(task,db)
        return self.mutate(request,["begin",task,owner,label],operation)
    def blockers(self,task,scopes,db):
        """Honor retained claims and live waiting FIFO requests; orphan requests without claims cannot block."""
        result=[]
        for other in self.list(db):
            if other["id"]==task["id"]or other["state"]in {"complete","abandoned"}:continue
            candidates=other["scopes"]if other["state"]in ACTIVE else []
            if other["state"]=="waiting" and (other["created"],other["id"])<(task["created"],task["id"]):
                new=[s for s in scopes if s not in task["scopes"]]
                modules=[s for s in new if s.startswith("module:")]
                queued=other["requested"]
                if modules:
                    if set(modules)&set(queued):result.append({"task_id":other["id"],"label":other["label"],"state":other["state"],"scopes":other["scopes"]});continue
                elif not any(s.startswith("module:")for s in scopes):
                    candidates+=queued
            if any(scope_overlap(a,b)for a in scopes for b in candidates):
                result.append({"task_id":other["id"],"label":other["label"],"state":other["state"],"scopes":other["scopes"]})
        return result
    def check(self,task,owner,revision,epoch,db):
        """Fence stale owners and generations before any reservation or completion mutation."""
        value=self.get(task,db)
        if value["owner"]!=owner:raise PermissionError("Task owner mismatch")
        if value["revision"]!=revision or value["epoch"]!=epoch:raise ValueError("Stale ownership revision or epoch")
        return value
    def update(self,db,value,**changes):
        """Advance one CAS revision and append an audit event in the same transaction."""
        fields={"state","scopes","requested","evidence","epoch","generation","owner","heartbeat"}
        if set(changes)-fields:raise ValueError("Unknown ownership transition")
        changes["revision"]=value["revision"]+1
        values={k:canonical(v)if k in {"scopes","requested","evidence"}else v for k,v in changes.items()}
        db.execute("UPDATE tasks SET "+",".join(k+"=?"for k in values)+" WHERE id=?",[*values.values(),value["id"]])
        db.execute("INSERT INTO events(task,at,event)VALUES(?,?,?)",(value["id"],self.clock(),canonical(changes)))
        return self.get(value["id"],db)
    def acquire(self,task,owner,revision,epoch,scopes,request):
        """Acquire the entire requested scope atomically, retaining old claims on blocked expansion."""
        if not scopes or len(scopes)>256 or len(set(scopes))!=len(scopes)or any(not isinstance(s,str)or not(s.startswith("path:")or s.startswith("module:"))or len(s)>300 for s in scopes):
            raise ValueError("Bounded canonical scope required")
        for s in scopes:
            if s.startswith("path:"):source_path(s[5:])
            elif not re.fullmatch(r"module:[A-Za-z0-9_:/.-]+",s):raise ValueError("Invalid component")
        def operation(db):
            """Acquire the entire scope or persist a blocked request without releasing existing claims."""
            value=self.check(task,owner,revision,epoch,db)
            if value["state"]not in {"waiting","owned"}:raise ValueError("Ownership cannot expand in this state")
            wanted=sorted(set(value["scopes"])|set(scopes));blocked=self.blockers(value,wanted,db)
            if blocked:
                # Follow the actual blocker graph, including directory overlap and longer cycles.
                waiting={other["id"]:other for other in self.list(db)}
                visited=set();queue=[b["task_id"]for b in blocked]
                while queue:
                    current_id=queue.pop()
                    if current_id==value["id"]:raise ValueError("Reservation cycle; replan or administrator handoff required")
                    if current_id in visited:continue
                    visited.add(current_id);other=waiting[current_id]
                    if other["requested"]:
                        queue.extend(b["task_id"]for b in self.blockers(other,sorted(set(other["scopes"])|set(other["requested"])),db))
                current=self.update(db,value,requested=wanted,heartbeat=self.clock())
                return {"task":current,"acquired":False,"blockers":blocked}
            current=self.update(db,value,state="owned",scopes=wanted,requested=[],heartbeat=self.clock())
            return {"task":current,"acquired":True,"blockers":[]}
        return self.mutate(request,["acquire",task,owner,revision,epoch,scopes],operation)
    def assert_owned(self,task,owner,epoch,scopes):
        """Mutation callers must possess every module and path claim; orphaned owners are fenced."""
        value=self.get(task)
        if value["owner"]!=owner or value["epoch"]!=epoch or value["state"]not in {"owned","validating"}:
            raise PermissionError("Active matching ownership required")
        for scope in scopes:
            if not any(scope==held or(scope.startswith("path:")and held.startswith("path:")and overlap(held[5:],scope[5:])and(scope==held or scope.startswith(held+"/")))for held in value["scopes"]):
                raise PermissionError("Source scope not reserved")
        return value
    def edited(self,task,owner,revision,epoch,request):
        """A correction advances the source generation and invalidates previous acceptance."""
        def operation(db):
            """Invalidate acceptance and advance the source generation within the transaction."""
            value=self.check(task,owner,revision,epoch,db)
            if value["state"]not in {"owned","validating"}:raise ValueError("Editing ownership required")
            return self.update(db,value,state="owned",generation=value["generation"]+1,evidence={},heartbeat=self.clock())
        return self.mutate(request,["edited",task,owner,revision,epoch],operation)
    # @code-index {"version":1,"feature":"owner-claim-release","aliases":["release reservations","unblock engine"],"purpose":"Release exactly held owner claims without deleting source; fence old epochs and retain recovery receipts.","span_lines":17,"configuration":{"status":"not_applicable","rationale":"Ownership fencing and authenticated scope release are fixed authority invariants."}}
    def release_scopes(self,task,owner,revision,epoch,scopes,reason,request):
        """Release explicit owner claims, fence old epochs and retain the unpublished draft."""
        if not isinstance(scopes,list)or not 1<=len(scopes)<=256 or any(not isinstance(s,str)for s in scopes)or len(set(scopes))!=len(scopes):raise ValueError("Explicit unique held scopes required")
        if not isinstance(reason,str)or not 1<=len(reason.encode())<=500:raise ValueError("Bounded release reason required")
        def operation(db):
            """Atomically audit the released set; no completion or transfer is implied."""
            value=self.check(task,owner,revision,epoch,db)
            if value["state"]not in {"owned","validating","orphaned"}:raise ValueError("Active ownership required")
            if not set(scopes)<=set(value["scopes"]):raise ValueError("Only exactly held scopes can be released")
            held=sorted(set(value["scopes"])-set(scopes))
            result=self.update(db,value,state="owned"if held else"waiting",scopes=held,requested=[],epoch=epoch+1,evidence={},heartbeat=self.clock())
            db.execute("INSERT INTO events(task,at,event)VALUES(?,?,?)",(task,self.clock(),canonical({"release_scopes":sorted(scopes),"reason":reason,"owner":owner})))
            return result
        return self.mutate(request,["release_scopes",task,owner,revision,epoch,scopes,reason],operation)

    def release_frozen_scopes(self,task,owner,revision,epoch,scopes,binding,request):
        """Release sealed-source write scopes while retaining execution authority only."""
        required={"job_id","inputs_sha256","generation","released_scopes"}
        if not isinstance(binding,dict) or set(binding)!=required or not isinstance(binding["job_id"],str) or not re.fullmatch(r"[0-9a-f-]{36}",binding["job_id"]) or not isinstance(binding["inputs_sha256"],str) or not re.fullmatch(r"[a-f0-9]{64}",binding["inputs_sha256"]) or type(binding["generation"]) is not int or binding["generation"] < 0 or binding["released_scopes"]!=sorted(scopes):
            raise ValueError("Exact frozen execution binding required")
        def operation(db):
            value=self.check(task,owner,revision,epoch,db)
            if value["state"]!="owned" or value["generation"]!=binding["generation"]:raise ValueError("Frozen generation changed")
            if not set(scopes)<=set(value["scopes"]):raise ValueError("Only held scopes can be released")
            held=sorted(set(value["scopes"])-set(scopes))
            result=self.update(db,value,state="owned",scopes=held,requested=[],evidence=value["evidence"],heartbeat=self.clock())
            db.execute("INSERT INTO events(task,at,event)VALUES(?,?,?)",(task,self.clock(),canonical({"release_frozen_scopes":sorted(scopes),"job_id":binding["job_id"],"owner":owner})))
            return result
        return self.mutate(request,["release_frozen_scopes",task,owner,revision,epoch,scopes,binding],operation)

    def release_deployed_scopes(self,task,owner,revision,epoch,scopes,evidence,request):
        """Drop deployed write reservations while retaining read-only completion authority."""
        if not isinstance(scopes,list)or not scopes or len(scopes)>256 or len(scopes)!=len(set(scopes)):raise ValueError("Explicit held scopes required")
        if not isinstance(evidence,dict)or not evidence.get("release_id")or not evidence.get("commit"):raise ValueError("Verified deployed source/commit required")
        def operation(db):
            """A completed publication no longer needs source exclusivity."""
            value=self.check(task,owner,revision,epoch,db)
            if value["state"]not in {"owned","validating"}or evidence.get("generation")!=value["generation"]:raise ValueError("Deployed generation changed")
            if not set(scopes)<=set(value["scopes"]):raise ValueError("Only held scopes can be released")
            held=sorted(set(value["scopes"])-set(scopes))
            result=self.update(db,value,state="owned",scopes=held,requested=[],heartbeat=self.clock(),
                               evidence={"post_deployment_release":evidence["release_id"],"deployment":evidence})
            db.execute("INSERT INTO events(task,at,event)VALUES(?,?,?)",(task,self.clock(),canonical({"deployed_write_scopes_released":sorted(scopes),"release_id":evidence["release_id"],"owner":owner})))
            return result
        return self.mutate(request,["release_deployed_scopes",task,owner,revision,epoch,scopes,evidence],operation)

    def heartbeat(self,task,owner,revision,epoch,request):
        """Renew activity only; a heartbeat never releases ownership or extends tool authorization."""
        def operation(db):
            """Refresh activity while preserving validating ownership and existing reservations."""
            value=self.check(task,owner,revision,epoch,db)
            if value["state"]in {"complete","abandoned","takeover_pending"}:raise ValueError("Ownership not renewable")
            return self.update(db,value,heartbeat=self.clock(),state=value["state"]if value["state"]=="validating"else("owned"if value["scopes"]or value["evidence"].get("post_deployment_release")else"waiting"))
        return self.mutate(request,["heartbeat",task,owner,revision,epoch],operation)
    def touch(self,task,owner,epoch):
        """Refresh activity for a live bounded delegated job without changing the source CAS revision."""
        with self.transaction()as db:
            value=self.get(task,db)
            if value["owner"]==owner and value["epoch"]==epoch and value["state"]in {"owned","validating"}:
                db.execute("UPDATE tasks SET heartbeat=? WHERE id=?",(self.clock(),task))
    def orphan(self,after=300):
        """Lost heartbeats retain reservations and fence writes until explicit owner recovery."""
        with self.transaction()as db:
            for value in self.list(db):
                if (value["state"]in {"owned","validating"}or(value["state"]=="waiting"and value["requested"]))and self.clock()-value["heartbeat"]>after:
                    self.update(db,value,state="orphaned")
    def validate(self,task,owner,revision,epoch,evidence,request):
        """Record broker-verified current-generation acceptance; the caller must verify receipts first."""
        required={"generation","release_id","inputs_sha256","promoted_hashes","checks","acceptance","commit"}
        if not isinstance(evidence,dict)or set(evidence)!=required or not evidence["promoted_hashes"]or not evidence["checks"]or not evidence["acceptance"]or not re.fullmatch(r"[a-f0-9]{40}",evidence["commit"]):
            raise ValueError("Verified release, checks, acceptance and commit required")
        def operation(db):
            """Bind verified evidence to this exact active source generation."""
            value=self.check(task,owner,revision,epoch,db)
            if value["state"]!="owned"or evidence["generation"]!=value["generation"]:raise ValueError("Validation generation changed")
            return self.update(db,value,state="validating",evidence=evidence,heartbeat=self.clock())
        return self.mutate(request,["validate",task,owner,revision,epoch,evidence],operation)
    def complete(self,task,owner,revision,epoch,request):
        """Explicit agent completion is separate from successful compilation or publication."""
        def operation(db):
            """Release claims only after current-generation acceptance and explicit completion."""
            value=self.check(task,owner,revision,epoch,db)
            if value["state"]!="validating"or value["evidence"].get("generation")!=value["generation"]:raise ValueError("Current acceptance required")
            return self.update(db,value,state="complete",scopes=[],requested=[],heartbeat=self.clock())
        return self.mutate(request,["complete",task,owner,revision,epoch],operation)
    def transfer(self,task,owner,revision,epoch,target,new_owner,request,administrator=False,reason=""):
        """Atomically hand off claims; administrators must reconcile effects before invoking takeover."""
        if not re.fullmatch(r"[a-f0-9]{64}",target)or task==target:raise ValueError("Distinct handoff target required")
        if administrator and(not reason or len(reason)>500):raise ValueError("Administrator takeover reason required")
        def operation(db):
            """Fence the previous owner and move all claims atomically to an empty destination."""
            value=self.get(task,db)
            if value["revision"]!=revision or value["epoch"]!=epoch:raise ValueError("Stale takeover")
            if not administrator and value["owner"]!=owner:raise PermissionError("Owner handoff required")
            if value["state"]not in ACTIVE:raise ValueError("Active ownership required")
            destination=self.get(target,db)
            if destination["owner"]!=new_owner or destination["scopes"]or destination["state"]!="waiting":raise ValueError("Empty matching handoff destination required")
            original=self.update(db,value,state="abandoned",epoch=value["epoch"]+1,scopes=[],requested=[],evidence={"transfer_to":target,"administrator":administrator,"reason":reason})
            current=self.update(db,destination,state="owned",scopes=value["scopes"],requested=[],heartbeat=self.clock())
            return {"previous":original,"task":current}
        return self.mutate(request,["transfer",task,owner,revision,epoch,target,new_owner,administrator,reason],operation)
