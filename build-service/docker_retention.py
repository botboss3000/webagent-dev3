"""Protected, task-aware V3 Docker cleanup. Unknown images and shared caches are preserved."""
import io,tarfile,tempfile
import argparse,contextlib,fcntl,importlib.util,json,os,re,shutil,sqlite3,stat,subprocess,time,uuid
from pathlib import Path
IMAGE=r"sha256:[a-f0-9]{64}"
UUID=r"[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}"
DONE={"completed","cancelled"}
FLOORS={"compile_test":6*1024**3,"compile_linked":6*1024**3,"compile_release":6*1024**3,"package":8*1024**3}
# @code-index {"version":1,"feature":"docker-artifact-retention","aliases":["disk guard","retained build images"],"purpose":"Root-owned exact-artifact retention and disk admission. Config: build-service/scheduler-defaults.json -> disk_min_free_bytes, disk_grace_seconds, disk_cleanup_batch.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/disk_min_free_bytes","/disk_grace_seconds","/disk_cleanup_batch"]}]}}
class Retention:
    """Delete only registered expired V3 outputs; preserve every uncertain dependency."""
    def __init__(self,config,clock=time.time,docker=None,usage=None):
        """Bind protected development paths; injected engine/clock support deterministic tests."""
        if config["instance"]!="development":raise PermissionError("V3 development only")
        self.config=config;self.clock=clock;self.docker=docker or self.command;self.usage=usage or(lambda:shutil.disk_usage("/"))
        self.folder=Path(config["install_root"]).parent/"docker-retention";self.folder.mkdir(mode=0o700,exist_ok=True)
        s=self.folder.lstat()
        if not stat.S_ISDIR(s.st_mode)or s.st_uid!=os.geteuid()or s.st_mode&0o077:raise PermissionError("Protected retention directory required")
        self.path=self.folder/"registry.json"
    @staticmethod
    def command(*args):
        """Keep Docker inspect bodies private and bound engine calls."""
        p=subprocess.run(["docker",*args],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=3)
        if p.returncode:
            if b"No such image:"in p.stderr or b"No such object:"in p.stderr:raise FileNotFoundError("Artifact absent")
            raise RuntimeError("Docker retention operation failed")
        if len(p.stdout)>4*1024*1024:raise ValueError("Engine reply oversized")
        return p.stdout.decode()
    @contextlib.contextmanager
    def locked(self):
        """Serialize short registration/deletion transactions, never a build lifetime."""
        fd=os.open(self.folder/"cleanup.lock",os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW,0o600)
        try:
            s=os.fstat(fd)
            if s.st_uid!=os.geteuid()or not stat.S_ISREG(s.st_mode)or s.st_nlink!=1:raise PermissionError("Protected cleanup lock required")
            deadline=time.monotonic()+4
            while True:
                try:fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);break
                except BlockingIOError:
                    if time.monotonic()>deadline:raise TimeoutError("Cleanup busy")
                    time.sleep(.02)
            yield
        finally:os.close(fd)
    @staticmethod
    def read(path,limit):
        """Reject symlinked, oversized and nonregular persistence."""
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
        with os.fdopen(fd,"rb")as stream:
            s=os.fstat(stream.fileno())
            if not stat.S_ISREG(s.st_mode)or s.st_nlink!=1 or s.st_size>limit:raise ValueError("Retention metadata invalid")
            return json.loads(stream.read(limit+1))
    def load(self):
        """An empty first installation cannot delete discovered unregistered artifacts."""
        d=self.read(self.path,4*1024*1024)if self.path.exists()else {"version":1,"images":{},"containers":{},"phases":{}}
        if d.get("version")!=1 or any(not isinstance(d.get(k),dict)for k in ["images","containers","phases"]):raise ValueError("Retention metadata inconsistent")
        return d
    def save(self,d):
        """Persist intent before effects and fsync completion for exact restart reconciliation."""
        body=json.dumps(d,separators=(",",":")).encode()
        if len(body)>4*1024*1024:raise ValueError("Retention registry full")
        tmp=self.path.with_suffix(".new");fd=os.open(tmp,os.O_CREAT|os.O_WRONLY|os.O_TRUNC|os.O_NOFOLLOW,0o600)
        with os.fdopen(fd,"wb")as stream:stream.write(body);stream.flush();os.fsync(stream.fileno())
        os.replace(tmp,self.path);fd=os.open(self.folder,os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)
    def jobs(self):
        """Reconcile bounded durable job receipts; inconsistency prevents all deletion."""
        paths=[p for p in Path(self.config["state_root"]).glob("*.json")if re.fullmatch(UUID,p.stem)]
        if len(paths)>8192:raise ValueError("Job reconciliation bound exceeded")
        result={};total=0
        for p in paths:
            total+=p.stat().st_size
            if total>64*1024*1024:raise ValueError("Job reconciliation byte bound exceeded")
            j=self.read(p,8*1024*1024)
            if j.get("job_id")!=p.stem or not isinstance(j.get("status"),str):raise ValueError("Job receipt inconsistent")
            result[p.stem]=j
        ownership=Path(self.config["state_root"])/"coordination/ownership.sqlite"
        if ownership.is_symlink():raise PermissionError("Ownership database symlink")
        completed=set()
        if ownership.is_file():
            with contextlib.closing(sqlite3.connect(ownership.as_uri()+"?mode=ro",uri=True,timeout=.5))as db:
                db.execute("PRAGMA query_only=ON")
                completed={row[0]for row in db.execute("SELECT id FROM tasks WHERE state='complete'")}
        recent=sorted((j for j in result.values()if j["status"]=="completed"and j.get("operation")in {"build_deploy","ui_deploy","plugin_deploy","index_deploy","host_service_deploy"}),key=lambda j:j.get("finished_at",0),reverse=True)[:2]
        for j in result.values():
            j["_retention_task_complete"]=j.get("ownership_id")in completed
            j["_retention_recent_release"]=any(j["job_id"]==r["job_id"]for r in recent)
        return result
    @staticmethod
    def hashes(value):
        """Extract only image-like digests; never expose other receipt values."""
        if isinstance(value,str):return {value}if re.fullmatch(IMAGE,value)else set()
        values=value.values()if isinstance(value,dict)else value if isinstance(value,list)else []
        result=set()
        for v in values:result.update(Retention.hashes(v))
        return result
    @staticmethod
    def output(value):
        """Only exclusively canonical V3 job tags authorize image deletion."""
        tags=value.get("RepoTags")or[]
        pattern=r"(?:webagent-v3-(?:api|loop)-|webagent-dev-v3(?:-loop)?:job-)"+UUID+r"(?::latest)?"
        return bool(tags)and all(re.fullmatch(pattern,t)for t in tags)
    def pin(self,image,job_id=None,container=None):
        """Acquire exact dependency retention before use; no deletion authority comes from a caller."""
        if not isinstance(image,str)or not(re.fullmatch(IMAGE,image)or re.fullmatch(r"[a-z0-9][a-z0-9./:_-]{0,180}@sha256:[a-f0-9]{64}",image)):raise ValueError("Exact image reference required")
        if job_id is not None and(not isinstance(job_id,str)or not re.fullmatch(UUID,job_id)):raise ValueError("Exact job ID required")
        with self.locked():
            d=self.load();v=json.loads(self.docker("image","inspect",image))[0]
            if not re.fullmatch(IMAGE,v.get("Id",""))or(re.fullmatch(IMAGE,image)and v["Id"]!=image):raise ValueError("Image identity changed")
            image=v["Id"]
            row=d["images"].setdefault(image,{"created":self.clock(),"jobs":[],"permanent":False})
            row["logical_image_bytes"]=int(v.get("Size",0))
            if job_id and job_id not in row["jobs"]:row["jobs"].append(job_id)
            if job_id is None:row["permanent"]=True
            if len(row["jobs"])>8192:raise ValueError("Image dependency bound exceeded")
            if container:
                name=container.get("Name","").lstrip("/");labels=container.get("Config",{}).get("Labels",{})
                m=re.fullmatch(r"webagent-build-development-([a-f0-9]{32})-([a-z_]+)-([a-f0-9]{8})",name)
                if not m or labels.get("webagent.scheduler.instance")!="development"or labels.get("webagent.scheduler.job")!=m[1]or labels.get("webagent.scheduler.phase")!=m[2]or not re.fullmatch(r"[a-f0-9]{64}",container.get("Id",""))or str(uuid.UUID(m[1]))!=job_id:raise PermissionError("Container retention identity invalid")
                d["containers"].setdefault(container["Id"],{"created":self.clock(),"name":name,"job_id":job_id,"image":image})
            self.save(d);return {"retained":True,"image":image,"job_id":job_id}
    def protected(self,row,jobs):
        """Keep missing/unresolved/pinned jobs and completed jobs inside their grace period."""
        if row.get("permanent"):return True
        keys=row.get("jobs",[row.get("job_id")])
        if not keys or keys==[None]:return True
        for key in keys:
            j=jobs.get(key)
            if not j or j["status"]not in DONE or not j.get("_retention_task_complete")or j.get("_retention_recent_release"):return True
            if self.clock()-j.get("finished_at",self.clock())<self.config["scheduler"].get("disk_grace_seconds",7200):return True
        return False
    def disk(self,name,phase,profile=None,finish=False,measured_bytes=0):
        """Reserve conservative future disk demand, retaining measured output footprints by recipe."""
        if not re.fullmatch(r"webagent-build-development-[a-f0-9]{32}-[a-z_]+-[a-f0-9]{8}",name):raise ValueError("Canonical phase required")
        if type(measured_bytes)is not int or not 0<=measured_bytes<=1024**4:raise ValueError("Invalid disk footprint")
        with self.locked():
            d=self.load();profiles=d.setdefault("disk_profiles",{})
            if finish:
                old=d["phases"].pop(name,None)
                if old and measured_bytes:profiles[old["profile"]]=max(profiles.get(old["profile"],0),measured_bytes)
                self.save(d);return {"released":True}
            if not isinstance(profile,str)or not 1<=len(profile)<=256:raise ValueError("Disk recipe required")
            free=self.usage().free;floor=self.config["scheduler"].get("disk_min_free_bytes",8*1024**3)
            need=max(1024**3 if profile=="resource:memory-probe-v1"else FLOORS.get(phase,1024**3),profiles.get(profile,0))
            remaining=sum(v["bytes"]for n,v in d["phases"].items()if n!=name)
            existing=d["phases"].get(name)
            if existing and(existing["profile"]!=profile or existing["phase"]!=phase or existing["bytes"]!=need):raise ValueError("Disk reservation identity changed")
            admitted=existing is not None or free-floor>=remaining+need
            if admitted:d["phases"][name]={"bytes":need,"profile":profile,"phase":phase};self.save(d)
            return {"admitted":admitted,"reason":"disk","estimate_bytes":need,"available_bytes":free,"reserve_bytes":floor,"reserved_bytes":remaining}
    def reconcile_phases(self,apply=False):
        """Release abandoned disk reservations only after durable terminal state and absent/stopped identity."""
        with self.locked():names=list(self.load()["phases"])[:32]
        recovered=[]
        for name in names:
            with self.locked():
                d=self.load();jobs=self.jobs()
                match=re.fullmatch(r"webagent-build-development-([a-f0-9]{32})-([a-z_]+)-([a-f0-9]{8})",name)
                if not match:raise ValueError("Invalid persisted disk phase")
                job=jobs.get(str(uuid.UUID(match[1])))
                if not job or job["status"]in {"queued","running"}:continue
                try:v=json.loads(self.docker("inspect",name))[0]
                except FileNotFoundError:v=None
                if v is not None:
                    labels=v.get("Config",{}).get("Labels",{})
                    if v.get("Name","").lstrip("/")!=name or labels.get("webagent.scheduler.instance")!="development"or labels.get("webagent.scheduler.job")!=match[1]or labels.get("webagent.scheduler.phase")!=match[2]or v.get("State",{}).get("Status")not in {"exited","dead"}:continue
                recovered.append(name)
                if apply:d["phases"].pop(name,None);self.save(d)
        return recovered
    def cleanup(self,apply=False,force=False):
        """Recheck each exact deletion under the retention lock; never prune shared Docker caches."""
        usage=self.usage();pct=100*(usage.total-usage.free)//usage.total
        report={"applied":apply,"usage_percent":pct,"removed":[],"eligible":[],"protected":[],"preserved":[],"cache_cleanup":"shared builders preserved"}
        if pct<80 and not force:return report
        report["reconciled_disk_phases"]=self.reconcile_phases(apply)
        with self.locked():
            d=self.load();jobs=self.jobs()
            protected=[(i,r)for i,r in d["images"].items()if self.protected(r,jobs)]
            report["protected_count"]=len(protected)
            report["protected"]=[{"image":i,"jobs":r.get("jobs",[])[:16],"job_count":len(r.get("jobs",[])),"logical_image_bytes":r.get("logical_image_bytes",0)}for i,r in protected[:64]]
            candidates=[("container",i)for i,r in d["containers"].items()if not self.protected(r,jobs)]+[("image",i)for i,r in d["images"].items()if not self.protected(r,jobs)]
        for kind,identity in candidates[:8192]:
            if len(report["eligible"])>=self.config["scheduler"].get("disk_cleanup_batch",4):break
            if not re.fullmatch(r"[a-f0-9]{64}"if kind=="container"else IMAGE,identity):raise ValueError("Registry identity invalid")
            with self.locked():
                d=self.load();jobs=self.jobs();row=d[kind+"s"].get(identity)
                if not row or self.protected(row,jobs)or self.clock()-row["created"]<self.config["scheduler"].get("disk_grace_seconds",7200):continue
                try:v=json.loads(self.docker("image","inspect",identity)if kind=="image"else self.docker("inspect",identity))[0]
                except FileNotFoundError:
                    if apply:d[kind+"s"].pop(identity);d.pop("intent",None);self.save(d)
                    continue
                if kind=="image":
                    if any(j["status"]not in DONE and identity in self.hashes(j)for j in jobs.values()):
                        report["preserved"].append({"id":identity,"reason":"durable job reference"});continue
                    if v.get("Id")!=identity or not self.output(v):
                        report["preserved"].append({"id":identity,"reason":"unknown or shared image ownership"});continue
                    if self.docker("ps","-aq","--no-trunc","--filter","ancestor="+identity).strip():
                        report["preserved"].append({"id":identity,"reason":"running or stopped container reference"});continue
                else:
                    labels=v.get("Config",{}).get("Labels",{})
                    if v.get("Id")!=identity or v.get("Name","").lstrip("/")!=row["name"]or v.get("Image")!=row["image"]or labels.get("webagent.scheduler.instance")!="development"or labels.get("webagent.scheduler.job")!=uuid.UUID(row["job_id"]).hex or v.get("State",{}).get("Status")not in {"exited","dead"}:continue
                report["eligible"].append({"kind":kind,"id":identity})
                if apply:
                    d["intent"]={"kind":kind,"id":identity,"at":self.clock()};self.save(d)
                    self.docker(*(["image","rm",identity]if kind=="image"else["rm",identity]))
                    d[kind+"s"].pop(identity);d.pop("intent",None);self.save(d);report["removed"].append({"kind":kind,"id":identity})
            usage=self.usage()
            if 100*(usage.total-usage.free)//usage.total<70:break
        report["preserved_count"]=len(report["preserved"]);report["preserved"]=report["preserved"][:64]
        usage=self.usage();report["target_reached"]=100*(usage.total-usage.free)//usage.total<70
        with self.locked():
            d=self.load();d["last_cleanup"]=report;d["cleanup_history"]=(d.get("cleanup_history",[])+[report])[-128:];self.save(d)
        return report
    def acceptance_probe(self):
        """Exercise real exact-ID deletion against isolated synthetic jobs, never real release metadata."""
        if os.geteuid()!=0:raise PermissionError("Root acceptance operator required")
        report={"live_docker":True,"isolated_fixture_clock":True,"production_registry_modified":False}
        with tempfile.TemporaryDirectory(prefix="acceptance-",dir=self.folder)as folder:
            base=Path(folder);jobs=base/"jobs";jobs.mkdir();(jobs/"coordination").mkdir()
            owner="a"*64;expired=str(uuid.uuid4());held=str(uuid.uuid4())
            with contextlib.closing(sqlite3.connect(jobs/"coordination/ownership.sqlite"))as db,db:
                db.execute("CREATE TABLE tasks(id TEXT PRIMARY KEY,state TEXT)")
                db.execute("INSERT INTO tasks VALUES(?,?)",(owner,"complete"))
            def receipt(key,status):
                (jobs/(key+".json")).write_text(json.dumps({"job_id":key,"ownership_id":owner,"status":status,"finished_at":1}))
            receipt(expired,"completed");receipt(held,"queued")
            clock=[100000.]
            cfg={**self.config,"state_root":str(jobs),"install_root":str(base/"install")}
            guard=Retention(cfg,clock=lambda:clock[0])
            images=[]
            try:
                for key in [expired,held]:
                    archive=io.BytesIO()
                    with tarfile.open(fileobj=archive,mode="w"):pass
                    tag="webagent-v3-api-"+key+":latest"
                    process=subprocess.run(["docker","import","--change","LABEL webagent.cleanup.acceptance="+key,"-",tag],input=archive.getvalue(),capture_output=True,timeout=20)
                    if process.returncode:raise RuntimeError("Disposable image import failed")
                    identity=process.stdout.decode().strip()
                    if not re.fullmatch(IMAGE,identity):raise ValueError("Acceptance image identity invalid")
                    images.append((identity,key));guard.pin(identity,key)
                clock[0]+=7201
                preview=guard.cleanup(force=True)
                expected=[{"kind":"image","id":images[0][0]}]
                if preview["eligible"]!=expected or preview["removed"]:raise ValueError("Acceptance preview eligibility changed")
                result=guard.cleanup(apply=True,force=True)
                if result["removed"]!=expected:raise ValueError("Expired disposable artifact was not removed")
                try:guard.docker("image","inspect",images[0][0])
                except FileNotFoundError:pass
                else:raise ValueError("Expired disposable image remains")
                retained=json.loads(guard.docker("image","inspect",images[1][0]))[0]
                if retained.get("Id")!=images[1][0]:raise ValueError("Required disposable image missing")
                report.update(preview=preview,cleanup=result,required_survived=True,expired_removed=True)
            finally:
                # Only our synthetic jobs become complete; normal eligibility performs teardown.
                for identity,key in images:receipt(key,"completed")
                clock[0]+=7201
                report["teardown"]=guard.cleanup(apply=True,force=True)
        return report

def main():
    """Run only the configured installed root-owned guard; default to a read-only deletion preview."""
    p=argparse.ArgumentParser();p.add_argument("--apply",action="store_true");p.add_argument("--force",action="store_true");p.add_argument("--acceptance-probe",action="store_true");args=p.parse_args()
    if os.geteuid()!=0:raise PermissionError("Root cleanup operator required")
    s=importlib.util.spec_from_file_location("cleanup_config",Path(__file__).with_name("config.py"));c=importlib.util.module_from_spec(s);s.loader.exec_module(c)
    config=c.load("/etc/webagent/development/build-service.json")
    if Path(__file__).resolve()!=Path(config["service_dir"])/"docker_retention.py":raise PermissionError("Installed cleanup required")
    try:
        guard=Retention(config)
        if args.acceptance_probe:
            if args.apply or args.force:raise ValueError("Acceptance has a fixed isolated recipe")
            result=guard.acceptance_probe()
        elif args.apply and not config.get("scheduler",{}).get("task_aware_cleanup",False):result={"applied":False,"deferred":"Task-aware cleanup is disabled"}
        else:result=guard.cleanup(args.apply,args.force)
    except (OSError,ValueError,RuntimeError,TimeoutError,sqlite3.Error,subprocess.SubprocessError):result={"applied":False,"deferred":"retention or engine reconciliation required"}
    print(json.dumps(result))
if __name__=="__main__":main()
