"""Durable, source-bound Code Index review metadata. No source or runtime data writes."""
from __future__ import annotations
import contextlib,hashlib,importlib.util,json,os,re,sqlite3,time,uuid,tempfile
from pathlib import Path
CHECKS=("documentation","connections","breadcrumbs","configuration","index_freshness")
STATES={"pending","in_progress","complete","needs_review"}
MAX_RECORDS=100000
DDL="""
CREATE TABLE IF NOT EXISTS records(entry_id TEXT PRIMARY KEY,task_key TEXT NOT NULL,task_id TEXT NOT NULL,file_path TEXT NOT NULL,unit_id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,UNIQUE(task_key,file_path,unit_id));
CREATE INDEX IF NOT EXISTS record_tasks ON records(task_key,file_path);
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,entry_id TEXT NOT NULL,at INTEGER NOT NULL,body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS receipts(actor TEXT NOT NULL,request_id TEXT NOT NULL,digest TEXT NOT NULL,result TEXT NOT NULL,PRIMARY KEY(actor,request_id));
CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
"""
# Serialize stable structured receipts without formatting-dependent differences.
def canonical(value):return json.dumps(value,sort_keys=True,separators=(",",":"),ensure_ascii=False)
# Bind a structured identity or mutation payload to a SHA-256 identifier.
def digest(value):return hashlib.sha256(canonical(value).encode()).hexdigest()
def load_scanner():
    """Load only the protected paired scanner, or its development sibling."""
    path=Path(__file__).with_name("index-scanner.py")
    if not path.exists():path=Path(__file__).with_name("adapter.py")
    spec=importlib.util.spec_from_file_location("maintenance_source_scanner",path)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
def page(rows,offset=0,limit=50):
    """Return a bounded response with a resumable offset."""
    if type(offset)!=int or offset<0 or type(limit)!=int or not 1<=limit<=100:raise ValueError("Invalid ledger page")
    selected=[];used=0
    for row in rows[offset:offset+limit]:
        size=len(json.dumps(row,sort_keys=True,separators=(",",":")).encode())
        if used+size>48*1024:break
        selected.append(row);used+=size
    if not selected and offset<len(rows):raise ValueError("Maintenance entry exceeds response bound")
    next_offset=offset+len(selected)
    return {"results":selected,"total":len(rows),"offset":offset,"next_offset":next_offset if next_offset<len(rows) else None,"has_more":next_offset<len(rows)}
def validate_checks(checks):
    """Require every completion check and rationale without certifying prose accuracy."""
    if not isinstance(checks,dict) or set(checks)!=set(CHECKS):raise ValueError("Every maintenance check is required")
    for name,value in checks.items():
        if isinstance(value,str):value={"status":value}
        if not isinstance(value,dict) or set(value)-{"status","rationale"}:raise ValueError("Invalid maintenance check")
        if value.get("status")=="verified":continue
        why=value.get("rationale")
        if value.get("status")!="not_applicable" or name=="index_freshness" or not isinstance(why,str) or not 1<=len(why.encode())<=512:
            raise ValueError("Completion requires verified checks or a bounded not-applicable rationale")
def source_inventory(scanner,root,scope,index_folder=None):
    """Scan exact source and return scope-filtered units with freshness evidence."""
    fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    conn=sqlite3.connect(":memory:");conn.row_factory=sqlite3.Row
    try:
        conn.executescript(scanner.DDL)
        scan=scanner.scan(conn,fd,{"action":"scan","scope":"","limit":1,"offset":0})
        report=scanner.coverage(conn,fd,{"action":"report","scope":scope,"limit":1,"offset":0})
        if any(report[k]for k in ("gaps","stale_count","unindexed_count")):raise ValueError("Maintenance scan is not fresh")
        files=[dict(row)for row in conn.execute("SELECT * FROM files ORDER BY path")if not scope or row["path"]==scope or row["path"].startswith(scope.rstrip("/")+"/")]
        for file in files:file["dependencies"]=scanner.maintenance_dependencies(conn,file["path"])
        units={f["path"]:scanner.maintenance_units(conn,f)for f in files}
        index_hash=None
        if index_folder is not None:
            index_folder=Path(index_folder)
            if index_folder.is_symlink():raise ValueError("Task index symlink rejected")
            index_folder.mkdir(mode=0o700,parents=True,exist_ok=True)
            index_fd,path=tempfile.mkstemp(prefix=".index-",suffix=".sqlite",dir=index_folder);os.close(index_fd)
            try:
                output=sqlite3.connect(path)
                try:conn.backup(output)
                finally:output.close()
                index_hash=hashlib.sha256(Path(path).read_bytes()).hexdigest()
                os.replace(path,index_folder/"index.sqlite")
            finally:Path(path).unlink(missing_ok=True)
        return files,units,{"index_sha256":index_hash,"batch_id":scan["batch_id"],"revision":scan["revision"],"verified_at":int(time.time()),"gaps":0,"stale_count":0,"unindexed_count":0}
    finally:conn.close();os.close(fd)

class Ledger:
    """Broker-owned store; reviewer identity and source roots are supplied by the broker."""
    def __init__(self,folder,scanner=None,inventory=None):
        """Open the protected persistent store without following ledger symlinks."""
        self.folder=Path(folder);self.scanner=scanner or load_scanner();self.inventory=inventory or(lambda root,scope,folder:source_inventory(self.scanner,root,scope,folder))
        if self.folder.is_symlink():raise ValueError("Ledger directory symlink rejected")
        self.folder.mkdir(mode=0o700,parents=True,exist_ok=True)
        self.path=self.folder/"ledger.sqlite"
        if self.path.is_symlink():raise ValueError("Ledger database symlink rejected")
        with self.connect()as conn:conn.executescript(DDL)
        os.chmod(self.path,0o600)
    @contextlib.contextmanager
    def connect(self):
        """Commit or roll back one bounded ledger transaction and close its connection."""
        if self.path.exists()and(self.path.is_symlink()or self.path.stat().st_nlink!=1):raise ValueError("Invalid ledger inode")
        if self.path.exists()and self.path.stat().st_size>128*1024*1024:raise ValueError("Ledger storage bound exceeded")
        conn=sqlite3.connect(self.path,timeout=5);conn.row_factory=sqlite3.Row;conn.execute("PRAGMA trusted_schema=OFF")
        try:
            yield conn
            conn.commit()
        except BaseException:
            conn.rollback();raise
        finally:conn.close()
    def _rows(self,conn,key=None):
        """Read ordered review snapshots for the selected task."""
        query="SELECT body FROM records";args=()
        if key is not None:query+=" WHERE task_key=?";args=(key,)
        return [json.loads(row[0])for row in conn.execute(query+" ORDER BY file_path,unit_id,task_key",args)]
    def list(self,args):
        """Expose shared resumable work while withholding private claim handles."""
        allowed={"action","root","task_id","task_key","scope","status","feature","offset","limit","entry_id"}
        if set(args)-allowed:raise ValueError("Unknown maintenance read field")
        with self.connect()as conn:
            rows=self._rows(conn,args.get("task_key"))
        if args.get("task_id"):rows=[r for r in rows if r["task_id"]==args["task_id"]]
        scope=args.get("scope","");self.scanner.relative_parts(scope,True)
        rows=[r for r in rows if not scope or r["file_path"]==scope or r["file_path"].startswith(scope.rstrip("/")+"/")]
        if args.get("status"):rows=[r for r in rows if r["status"]==args["status"]]
        if args.get("feature"):rows=[r for r in rows if args["feature"]in r["features"]]
        if args.get("action")=="get"and not args.get("entry_id"):raise ValueError("get requires entry_id")
        if args.get("entry_id"):rows=[r for r in rows if r["entry_id"]==args["entry_id"]]
        for row in rows:
            if row.get("claim"):row["claim"]={k:v for k,v in row["claim"].items()if k!="id"}
        if args.get("action")=="get" and rows:
            with self.connect()as conn:
                history=[]
                for item in conn.execute("SELECT body FROM events WHERE entry_id=? ORDER BY id DESC LIMIT 8",(rows[0]["entry_id"],)):
                    event=json.loads(item[0]);record=event["record"]
                    history.append({"event":event["event"],**{k:record.get(k)for k in ("revision","status","source_hash","updated_at","reviewer","remaining_work")}})
                rows[0]["history"]=history
        return page(rows,args.get("offset",0),args.get("limit",50))
    def _save(self,conn,row,actor,event):
        """Advance a record revision and append attributed maintenance history."""
        row=dict(row);row["revision"]=row.get("revision",0)+1;row["updated_at"]=int(time.time());row["last_actor"]=actor
        conn.execute("INSERT OR REPLACE INTO records VALUES(?,?,?,?,?,?,?)",(row["entry_id"],row["task_key"],row["task_id"],row["file_path"],row["unit_id"],row["revision"],canonical(row)))
        conn.execute("INSERT INTO events(entry_id,at,body)VALUES(?,?,?)",(row["entry_id"],row["updated_at"],canonical({"event":event,"record":row})))
        return row
    def _fresh(self,root,row):
        """Reject review evidence when source or metadata dependencies changed."""
        fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
        try:
            try:actual=self.scanner.sha(self.scanner.read_source(fd,row["file_path"]))
            except FileNotFoundError:actual=None
            if actual!=row["source_hash"]:raise ValueError("Source changed; sync maintenance before reviewing")
            for dep in row.get("dependencies",[]):
                if self.scanner.sha(self.scanner.read_source(fd,dep["path"]))!=dep["sha256"]:raise ValueError("Source metadata changed; sync maintenance before reviewing")
        finally:os.close(fd)
    def mutate(self,args,actor,task_key,task_id,root,authorize=lambda:None):
        """Apply one authorized retry-safe task mutation with revision checks."""
        authorize()
        allowed={"action","root","task_id","task_key","scope","entry_id","expected_revision","source_hash","request_id","status","checks","remaining_work","reconciliation","claim_id","configuration_assessment"}
        if set(args)-allowed:raise ValueError("Unknown maintenance mutation field")
        action=args.get("action");request=args.get("request_id")
        if action not in {"sync","claim","record","release"}or not isinstance(request,str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",request):raise ValueError("Stable maintenance request_id required")
        who=digest(actor);body_digest=digest([task_key,args])
        with self.connect()as conn:
            receipt=conn.execute("SELECT digest,result FROM receipts WHERE actor=? AND request_id=?",(who,request)).fetchone()
            if receipt:
                if receipt[0]!=body_digest:raise ValueError("request_id reused with different maintenance input")
                return json.loads(receipt[1])
        inventory=None
        if action=="sync":
            scope=args.get("scope","");self.scanner.relative_parts(scope,True)
            inventory=self.inventory(root,scope,self.folder/"task-indexes"/digest(task_key))
        authorize()
        with self.connect()as conn:
            conn.execute("BEGIN IMMEDIATE")
            authorize()
            receipt=conn.execute("SELECT digest,result FROM receipts WHERE actor=? AND request_id=?",(who,request)).fetchone()
            if receipt:
                if receipt[0]!=body_digest:raise ValueError("request_id reused with different maintenance input")
                return json.loads(receipt[1])
            if action=="sync":
                files,units,scan=inventory;existing={(r["file_path"],r["unit_id"]):r for r in self._rows(conn,task_key)};seen=set();changed=0
                published={(r["file_path"],r["unit_id"]):r for r in self._rows(conn,"published")}
                for f in files:
                    for unit in units[f["path"]]:
                        k=(f["path"],unit["unit_id"]);seen.add(k);old=existing.get(k)
                        row=old or {"entry_id":digest([task_key,*k]),"task_key":task_key,"task_id":task_id,"file_path":k[0],"unit_id":k[1],"revision":0,"status":"pending","checks":{},"remaining_work":[],"claim":None,"previous_review":None}
                        if old and(old["source_hash"]!=f["sha256"]or old.get("dependencies",[])!=f["dependencies"]or old["removed"]):
                            row["previous_review"]={"source_hash":old["source_hash"],"checks":old["checks"],"reviewer":old.get("reviewer"),"reviewed_at":old.get("reviewed_at"),"scan":old.get("scan")}
                            row.update(status="needs_review",claim=None,checks={})
                        elif not old:
                            prior=published.get(k)
                            if prior and prior["source_hash"]==f["sha256"]and prior.get("dependencies",[])==f["dependencies"]and prior["status"]=="complete":
                                row.update(status="complete",checks=prior["checks"],reviewer=prior.get("reviewer"),reviewed_at=prior.get("reviewed_at"),assessment_at=prior.get("assessment_at",prior.get("reviewed_at")),remaining_work=prior["remaining_work"])
                                unit["configuration_assessment"]=prior.get("configuration_assessment",unit["configuration_assessment"])
                        if old and old["source_hash"]==f["sha256"]and old.get("dependencies",[])==f["dependencies"]and old.get("configuration_assessment",{}).get("status")!="unassessed":unit["configuration_assessment"]=old["configuration_assessment"]
                        row.update(source_hash=f["sha256"],dependencies=f["dependencies"],removed=False,parser_status=f["parser_status"],scan=scan,**unit)
                        self._fresh(root,row)
                        self._save(conn,row,actor,"sync");changed+=1
                scope=args.get("scope","")
                # Unfinished published deletions must be claimable in a fresh task.
                for k,prior in published.items():
                    if k in seen or k in existing or(scope and not(k[0]==scope or k[0].startswith(scope.rstrip("/")+"/"))):continue
                    if prior.get("removed")and prior["status"]=="complete":continue
                    matching=next((f for f in files if f["path"]==k[0]),None)
                    unchanged=matching and not prior.get("removed")and matching["sha256"]==prior["source_hash"]and matching["dependencies"]==prior.get("dependencies",[])
                    row=dict(prior);row.update(entry_id=digest([task_key,*k]),task_key=task_key,task_id=task_id,revision=0,claim=None,scan=scan)
                    if not unchanged:
                        row.update(status="needs_review",previous_review={"source_hash":prior["source_hash"],"checks":prior["checks"],"reviewer":prior.get("reviewer")},source_hash=matching["sha256"]if matching else None,dependencies=matching["dependencies"]if matching else [],removed=True,checks={})
                    self._fresh(root,row);self._save(conn,row,actor,"resume_published");seen.add(k);changed+=1
                for k,old in existing.items():
                    if k in seen or scope and not(k[0]==scope or k[0].startswith(scope.rstrip("/")+"/")):continue
                    row=dict(old);matching=next((f for f in files if f["path"]==k[0]),None)
                    row["previous_review"]={"source_hash":row["source_hash"],"checks":row["checks"],"reviewer":row.get("reviewer")}
                    row.update(source_hash=matching["sha256"]if matching else None,dependencies=matching["dependencies"]if matching else [],removed=True,status="needs_review",claim=None,checks={},scan=scan)
                    self._save(conn,row,actor,"removed");changed+=1
                if conn.execute("SELECT COUNT(*) FROM records").fetchone()[0]>MAX_RECORDS:raise ValueError("Maintenance record bound exceeded; narrow scope")
                result={"status":"success","task_key":task_key,"task_id":task_id,"updated":changed,"scan":scan,"completion_is_review_assertion":True}
            else:
                found=conn.execute("SELECT body FROM records WHERE entry_id=? AND task_key=?",(args.get("entry_id"),task_key)).fetchone()
                if not found:raise ValueError("Task maintenance entry unavailable")
                row=json.loads(found[0])
                if type(args.get("expected_revision"))!=int or args["expected_revision"]!=row["revision"]:raise ValueError("Maintenance revision conflict")
                self._fresh(root,row);claim=row.get("claim")
                held=claim and claim["expires_at"]>time.time()and (claim["actor"]!=actor or args.get("claim_id")!=claim["id"])
                if held:raise ValueError("Maintenance entry claimed by another reviewer")
                if action=="claim":
                    row.update(status="in_progress",claim={"actor":actor,"id":uuid.uuid4().hex,"expires_at":int(time.time())+1800})
                elif action=="release":
                    row.update(status="needs_review"if row.get("previous_review")else"pending",claim=None)
                    if "remaining_work"in args:row["remaining_work"]=self._notes(args["remaining_work"])
                else:
                    if args.get("source_hash")!=row["source_hash"]:raise ValueError("Maintenance source hash conflict")
                    state=args.get("status")
                    if state not in STATES:raise ValueError("Invalid maintenance state")
                    notes=self._notes(args.get("remaining_work",[]));checks=args.get("checks",{})
                    if not isinstance(checks,dict)or set(checks)-set(CHECKS)or len(canonical(checks).encode())>4096:raise ValueError("Invalid bounded maintenance checks")
                    assessment=self.scanner._configuration(args.get("configuration_assessment",row.get("configuration_assessment")))
                    configuration_check=checks.get("configuration",{})
                    if isinstance(configuration_check,dict)and configuration_check.get("status")=="not_applicable":
                        assessment=self.scanner._configuration({"status":"not_applicable","rationale":configuration_check.get("rationale")})
                    if state=="complete":
                        if assessment["status"]=="unassessed":raise ValueError("Complete maintenance requires a configuration assessment")
                        if not claim or claim["actor"]!=actor or args.get("claim_id")!=claim["id"] or claim["expires_at"]<=time.time():raise ValueError("Claim the maintenance entry before completion")
                        validate_checks(checks)
                        if notes:raise ValueError("Complete maintenance cannot have remaining work")
                        if row["unit_id"]=="__file__":
                            pending=[r for r in self._rows(conn,task_key)if r["file_path"]==row["file_path"]and r["unit_id"]!="__file__"and r["status"]!="complete"]
                            if pending:raise ValueError("Review child units before completing the file")
                        if row["removed"]:
                            reconciliation=args.get("reconciliation",{})
                            if not isinstance(reconciliation,dict)or set(reconciliation)-({"kind","replacement_entry_id"}if reconciliation.get("kind")=="renamed"else{"kind"}):raise ValueError("Invalid bounded reconciliation fields")
                            if reconciliation.get("kind")not in {"deleted","renamed"}:raise ValueError("Removed units require reconciliation")
                            if reconciliation["kind"]=="renamed":
                                replacement=conn.execute("SELECT body FROM records WHERE entry_id=? AND task_key=?",(reconciliation.get("replacement_entry_id"),task_key)).fetchone()
                                if not replacement or json.loads(replacement[0])["removed"]:raise ValueError("Rename replacement is unavailable")
                                target=json.loads(replacement[0]);self._fresh(root,target)
                                reconciliation={**reconciliation,"replacement_file_path":target["file_path"],"replacement_unit_id":target["unit_id"],"replacement_source_hash":target["source_hash"],"replacement_dependencies":target.get("dependencies",[])}
                            row["reconciliation"]=reconciliation
                        row.update(reviewer=actor,reviewed_at=int(time.time()),claim=None)
                    row.update(status=state,checks=checks,configuration_assessment=assessment,remaining_work=notes,assessment_at=int(time.time()))
                result={"status":"success","record":self._save(conn,row,actor,action)}
            authorize()
            conn.execute("INSERT INTO receipts VALUES(?,?,?,?)",(who,request,body_digest,canonical(result)));return result
    @staticmethod
    def _notes(notes):
        """Validate bounded concrete remaining-work notes."""
        if not isinstance(notes,list)or len(notes)>8 or any(not isinstance(n,str)or not 1<=len(n.encode())<=512 for n in notes):raise ValueError("Remaining work must be up to eight bounded notes")
        return notes
    def snapshot(self,keys):
        """Capture published and selected task evidence for final-candidate projection."""
        with self.connect()as conn:
            result=self._rows(conn,"published")
            for key in keys:result.extend(self._rows(conn,key))
            return result
    def active_release(self):
        """Return the canonical ledger's currently projected immutable release."""
        with self.connect()as conn:
            row=conn.execute("SELECT value FROM metadata WHERE key='active_release'").fetchone()
            return row[0]if row else None
    def activate(self,release_id,rows):
        """Restore the matching published projection while retaining task history."""
        with self.connect()as conn:
            conn.execute("BEGIN IMMEDIATE")
            marker=conn.execute("SELECT value FROM metadata WHERE key='active_release'").fetchone()
            if marker and marker[0]==release_id:return
            previous={r["entry_id"]:r for r in self._rows(conn,"published")}
            conn.execute("DELETE FROM records WHERE task_key='published'")
            for row in rows:
                row=dict(row);row["file_path"]=row["file_path"].removeprefix("v3/");row.update(task_key="published",task_id="published",entry_id=digest(["published",row["file_path"],row["unit_id"]]),claim=None)
                old=previous.get(row["entry_id"])
                ignored={"revision","updated_at","last_actor","scan"}
                if old and {k:v for k,v in old.items()if k not in ignored}=={k:v for k,v in row.items()if k not in ignored}:
                    conn.execute("INSERT INTO records VALUES(?,?,?,?,?,?,?)",(old["entry_id"],old["task_key"],old["task_id"],old["file_path"],old["unit_id"],old["revision"],canonical(old)))
                else:self._save(conn,row,{"transport":"broker","release_id":release_id},"publication")
            conn.execute("INSERT OR REPLACE INTO metadata VALUES('active_release',?)",(release_id,))
