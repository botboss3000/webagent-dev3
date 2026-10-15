"""Measured phase admission and safe compiler parking; never pause tests or publication.

Config: protected build-service.json -> scheduler. Estimates guide uncapped admission; explicit compiler ceilings reserve their full
approved budget. Insufficient reclaim waits for intervention.
"""
import copy,json,math,os,threading,time,re
from pathlib import Path
PARKABLE={"compile_test","compile_release","compile_linked","plugin_compile"}
ESTIMATES={"ui_test":1024**3,"index":1024**3,"engine":4*1024**3,"plugin":2*1024**3,"test":512*1024**2,"package":512*1024**2,"dependency":512*1024**2}
def host_sample():
    """Observe host headroom and pressure without reading any application state."""
    memory={}
    for line in Path("/proc/meminfo").read_text().splitlines():
        key,value=line.split(":",1);memory[key]=int(value.split()[0])*1024
    pressure={}
    for kind in ["cpu","memory","io"]:
        try:
            row=next(line for line in Path("/proc/pressure/"+kind).read_text().splitlines()if line.startswith("some "))
            pressure[kind]=float(dict(p.split("=",1)for p in row.split()[1:])["avg10"])
        except (OSError,ValueError,StopIteration):pressure[kind]=0.
    return {"available":memory["MemAvailable"],"total":memory["MemTotal"],"swap_free":memory.get("SwapFree",0),"swap_total":memory.get("SwapTotal",0),"pressure":pressure}
# @code-index {"version":1,"feature":"controlled-build-scheduler","aliases":["parallel builds","source ownership","compiler parking"],"purpose":"Measured admission and compiler parking. Config: build-service/scheduler-defaults.json -> reserve_bytes, reserve_percent, max_phases, max_parked, memory_pressure_percent; protected overrides.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/mode","/orphan_after_seconds","/reserve_bytes","/reserve_percent","/max_phases","/max_parked","/memory_pressure_percent","/ui_file_reservations","/max_ui_preparations","/component_indexing","/concurrent_ui_preparation","/ui_test_image","/package_ram_bytes","/package_tmpfs_bytes"]}],"opportunities":[]}}
class Resources:
    """Control only broker-created container identities; ownership and execution authority remain separate."""
    def __init__(self,state,runner,policy=None,clock=time.monotonic,sampler=host_sample):
        """Restore measured history and fence unfinished phases for explicit recovery."""
        self.runner=runner;self.policy=policy or{};self.enabled=self.policy.get("mode")=="enforce"
        self.clock=clock;self.sampler=sampler;self.lock=threading.RLock();self.condition=threading.Condition(self.lock)
        self.stop=threading.Event();self.rows={};self.profiles={};self.controls={};self.profile_migrations={};self.host=sampler()
        self.folder=Path(state)/"scheduler";self.folder.mkdir(mode=0o700,exist_ok=True)
        self.path=self.folder/"resources.json"
        if self.path.is_symlink():raise PermissionError("Resource metadata symlink")
        if self.path.exists():
            value=json.loads(self.path.read_bytes());self.profiles=value.get("profiles",{});self.controls=value.get("controls",{});self.profile_migrations=value.get("profile_migrations",{})
            # Recovery never automatically thaws or reruns an unclassified process.
            for name,row in value.get("phases",{}).items():
                if row["state"]not in {"finished","failed"}:
                    row["state"]="recovery_required";row["revision"]=row.get("revision",0)+1
                    row["paused_at"]=None;row["pause_timing_unconfirmed"]=True
                self.rows[name]=row
        self.migrate_test_profiles()
        self.restore_diagnostic_profiles(state)
        self.initialized=False;self.last_save=0.;self.pressure_since=None;self.stable_since=None;self.cpu_budget=max(1,(os.cpu_count()or 1)-1);self.last_cpu_adjust=0.;self.last_cpu_recovery=0.
        self.thread=None
    def migrate_test_profiles(self):
        """Split old mixed test history by actual terminal phase, preserving original samples and rows."""
        if self.profile_migrations.get("test-phase-split-v1"):return
        old=self.profiles.get("engine:rust1.90:test",[])
        if old:self.profiles["legacy:engine:rust1.90:test:mixed"]=list(old)
        runtime=[];compilation=list(self.profiles.get("engine:rust1.90:compile_test",[]))
        for row in self.rows.values():
            if row.get("profile")!="engine:rust1.90:test":continue
            if row.get("phase")=="compile_test":
                row["original_profile"]=row["profile"];row["profile"]="engine:rust1.90:compile_test"
                if row.get("state")=="finished"and row.get("metrics",{}).get("peak_memory",0)>0:compilation.append(row["metrics"]["peak_memory"])
            elif row.get("phase")=="test"and row.get("state")=="finished"and row.get("metrics",{}).get("peak_memory",0)>0:runtime.append(row["metrics"]["peak_memory"])
        self.profiles["engine:rust1.90:test"]=runtime[-20:]
        self.profiles["engine:rust1.90:compile_test"]=compilation[-20:]
        self.profile_migrations["test-phase-split-v1"]=True

    @staticmethod
    def diagnostic_check(text):
        """Recognize completed Rust diagnostics, never killed, OOM, timeout or interrupted phases."""
        lowered=text.lower()
        return "could not compile"in lowered and ("error["in lowered or "error:"in lowered)and not any(word in lowered for word in ("sigkill","signal: 9","killed","out of memory","oom","timed out","timeout","cancelled","interrupted"))
    def restore_diagnostic_profiles(self,state):
        """Recover matching measured check peaks from durable failed-check receipts without changing claims."""
        for row in self.rows.values():
            if row.get("state")!="failed"or row.get("phase")!="compile_test"or row.get("profile")!="engine:rust1.90:check"or not re.fullmatch(r"[a-f0-9-]{36}",row.get("job_id","")):continue
            path=Path(state)/(row["job_id"]+".json")
            if path.is_symlink()or not path.is_file()or path.stat().st_size>2*1024*1024:continue
            value=json.loads(path.read_bytes());peak=row.get("metrics",{}).get("peak_memory",0)
            if value.get("status")=="failed"and self.diagnostic_check(value.get("log",""))and peak>0:
                values=self.profiles.setdefault(row["profile"],[])
                if peak not in values:values.append(peak)

    def start(self):
        """Run monitoring independently of broker request locks and frozen compiler groups."""
        if self.thread is None:
            self.thread=threading.Thread(target=self.loop,name="build-resource-monitor",daemon=True);self.thread.start()
    def close(self):
        """Stop admission and leave recoverable phase metadata; never silently thaw parked work."""
        self.stop.set()
        with self.condition:self.condition.notify_all();self.persist()
        if self.thread is not None:self.thread.join(timeout=2)
    def persist(self):
        """Atomically retain bounded resource profiles and phase identities outside application storage."""
        value={"version":1,"profiles":self.profiles,"phases":self.rows,"controls":self.controls,"profile_migrations":self.profile_migrations}
        temporary=self.path.with_suffix(".new")
        if temporary.is_symlink():raise PermissionError("Resource metadata replaced")
        fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600)
        with os.fdopen(fd,"w")as stream:json.dump(value,stream,separators=(",",":"));stream.flush();os.fsync(stream.fileno())
        os.replace(temporary,self.path)
    def estimate(self,profile,kind):
        """Use an observed conservative peak with margin, retaining cold fallback estimates."""
        values=self.profiles.get(profile,[])
        if not values:return ESTIMATES[kind]
        return max(64*1024**2,math.ceil(max(values)*1.2))
    def reserve(self):
        """Keep configurable host headroom, independent of a build's claimed requirements."""
        return max(self.policy.get("reserve_bytes",768*1024**2),int(self.host["total"]*max(15 if self.enabled else 0,self.policy.get("reserve_percent",10))/100))
    def can_admit(self,estimate):
        """Account for resident RAM and remaining admitted peak demand before starting another phase."""
        active=[r for r in self.rows.values()if r["state"]in {"admitted","running","parked","waiting_intervention"}]
        if len(active)>=self.policy.get("max_phases",4):return False
        remaining=sum(max(0,r["estimate"]-r.get("metrics",{}).get("memory",0))for r in active if r["state"]in {"admitted","running","parked"})
        return self.host["available"]-self.reserve()>=estimate+remaining and not any(r["state"]in {"recovery_required","waiting_intervention","memory_recovery_required"}for r in self.rows.values())
    def enter(self,name,job,phase,profile,kind,timeout=None,hard_cap=None):
        """Wait for measured admission outside the broker lock; revocation still interrupts waiting."""
        ram_packaging=phase=="package"and kind=="package"and bool(self.policy.get("package_tmpfs_bytes",0))
        compiler=phase in {"compile_test","compile_release","compile_linked","plugin_compile"}and kind in {"engine","plugin"}
        allowed_cap=self.policy.get("package_ram_bytes")if ram_packaging else 4026531840
        if hard_cap is not None and(type(hard_cap)is not int or hard_cap%os.sysconf("SC_PAGE_SIZE")or not((compiler and 1024**3<=hard_cap<=10*1024**3)or(phase=="package"and kind=="package"and hard_cap==allowed_cap))):raise ValueError("Unsupported enforced phase memory cap")
        if ram_packaging and(hard_cap is None or self.policy.get("reserve_percent",10)<15):raise ValueError("RAM packaging requires full budget and host reserve")
        # tmpfs is unreclaimable without swap: reserve its full process/cache/scratch ceiling, even after small successful samples.
        estimate=hard_cap if hard_cap is not None else self.estimate(profile,kind);deadline=time.monotonic()+timeout if timeout is not None else None
        deadline_at=time.time()+timeout if timeout is not None else None
        progress=getattr(self.runner,"resource_progress",lambda _:None)
        disk_wait=None
        with self.condition:
            def report():
                """Persist progress outside the resource lock to preserve broker/resource lock ordering."""
                self.condition.release()
                try:progress(job)
                finally:self.condition.acquire()
            if job.get("revoked")or job.get("lease_until",float("inf"))<=time.time():raise PermissionError("Build authority expired")
            component="ui-browser"if kind=="ui_test"else"index:"+job["job_id"]if kind=="index"else job.get("artifact_component","engine"if kind=="engine"else profile)
            def eligible():
                """Require an available component and sufficient measured global headroom."""
                conflict=any(r.get("component")==component and r["state"]in {"admitted","running","parked","waiting_intervention","recovery_required"}for r in self.rows.values())
                task_id=job.get("task_id")
                task_conflict=any(r.get("task_id")==task_id and task_id is not None and(ram_packaging or r.get("ram_packaging"))and r["state"]in {"admitted","running","parked","waiting_intervention","recovery_required"}for r in self.rows.values())
                nonlocal disk_wait
                if conflict or task_conflict or not self.can_admit(estimate):disk_wait=None;return False
                if not self.initialized or hard_cap is not None:
                    # The protected ancestor must fit the complete admitted ceilings too.
                    required=estimate+sum(max(r["estimate"],r.get("metrics",{}).get("memory",0))for r in self.rows.values()if r["state"]in {"admitted","running","parked"})
                    reply=self.runner.resource_request({"operation":"initialize","required_bytes":required})
                    if reply.get("admitted")is False:return False
                    self.initialized=True
                check=getattr(self.runner,"disk_admission",None)
                disk_wait=check(name,job,phase,profile)if callable(check)else None
                return disk_wait is None or disk_wait.get("admitted")is True
            while self.enabled and not eligible():
                if deadline is not None and time.monotonic()>=deadline:
                    job.pop("resource_waiting",None);report();raise TimeoutError("Resource admission exceeded remaining check timeout")
                if self.stop.is_set()or job.get("revoked")or job.get("lease_until",float("inf"))<=time.time():
                    job.pop("resource_waiting",None);report();raise PermissionError("Build admission cancelled or authorization expired")
                waiting={"phase":phase,"reason":"headroom","estimate_bytes":estimate,"available_bytes":self.host["available"],"reserve_bytes":self.reserve(),"deadline_at":deadline_at}
                if disk_wait and not disk_wait.get("admitted"):waiting.update(disk_wait);waiting.pop("admitted",None)
                if job.get("resource_waiting")!=waiting:job["resource_waiting"]=waiting;report()
                self.condition.wait(min(.5,max(0,deadline-time.monotonic()))if deadline is not None else .5)
            if self.stop.is_set()or job.get("revoked")or job.get("lease_until",float("inf"))<=time.time():
                finish=getattr(self.runner,"disk_finish",None)
                if callable(finish):finish(name)
                raise PermissionError("Scheduler stopping")
            job.pop("resource_waiting",None)
            now=self.clock()
            self.rows[name]={"job_id":job["job_id"],"task_id":job.get("task_id"),"ram_packaging":ram_packaging,"phase":phase,"profile":profile,"kind":kind,"component":component,"estimate":estimate,"hard_cap":hard_cap,"priority":job.get("priority",0),"state":"admitted","revision":0,"started_at":time.time(),"started":now,"paused_seconds":0.,"paused_at":None,"metrics":{},"container_id":None}
            if kind=="index"and job.get("internal_index"):
                self.rows[name].update(internal_index=True,owner_user=job["user_id"],owner_workspace=job["workspace"])
            self.persist();report()
    def attached(self,name,identity):
        """Bind the new phase to its exact container identity before the monitor controls it."""
        with self.condition:
            row=self.rows[name];row["container_id"]=identity;row["state"]="running";self.persist();self.condition.notify_all()
    def elapsed_pause(self,name):
        """Adjust execution timers by confirmed frozen intervals, not queue or reclaim guesses."""
        with self.lock:
            row=self.rows.get(name,{})
            return row.get("paused_seconds",0.)+(self.clock()-row["paused_at"]if row.get("paused_at")is not None else 0.)
    def finish(self,name,success,diagnostic_complete=False):
        """Retain a measured phase peak and wake eligible unrelated work."""
        with self.condition:
            row=self.rows[name]
            finish=getattr(self.runner,"disk_finish",None)
            if callable(finish):finish(name)
            if row.get("paused_at")is not None:row["paused_seconds"]+=self.clock()-row["paused_at"];row["paused_at"]=None
            if row.get("memory_change",{}).get("state")in {"intent","uncertain"}:
                success=False
            row["state"]="finished"if success else"failed";row["finished"]=self.clock()
            peak=row.get("metrics",{}).get("peak_memory",0)
            row["diagnostic_complete"]=diagnostic_complete
            if peak>0 and (success or diagnostic_complete):self.profiles[row["profile"]]=(self.profiles.get(row["profile"],[])+[peak])[-20:]
            completed=sorted((n for n,r in self.rows.items()if r["state"]in {"finished","failed"}),key=lambda n:self.rows[n].get("finished",0))
            for old in completed[:-64]:del self.rows[old]
            if len(self.profiles)>512:self.profiles={k:self.profiles[k]for k in sorted(self.profiles)[-512:]}
            self.persist();self.condition.notify_all()
    def act(self,name,operation):
        """Freeze only eligible compilation, retaining task ownership and measurable pause intervals."""
        with self.condition:
            row=self.rows[name]
            if operation=="freeze"and(row["phase"]not in PARKABLE or row["state"]!="running"):raise ValueError("Phase cannot be parked")
            if operation=="thaw"and row["state"]not in {"parked","waiting_intervention"}:raise ValueError("Phase not parked")
            result=self.runner.resource_request({"operation":operation,"container":name})
            if result["container_id"]!=row["container_id"]:raise ValueError("Container identity changed")
            if operation=="freeze":
                if not result["frozen"]:raise ValueError("Freeze not acknowledged")
                row["state"]="parked";row["paused_at"]=self.clock()
            else:
                if result["frozen"]:raise ValueError("Thaw not acknowledged")
                row["paused_seconds"]+=self.clock()-row["paused_at"];row["paused_at"]=None;row["state"]="running"
            row["metrics"]=result;row["revision"]=row.get("revision",0)+1;self.persist();self.condition.notify_all()
    def intervention(self,name,reason):
        """Insufficient reclaim never restarts work or releases its source reservations."""
        with self.condition:
            self.rows[name]["state"]="waiting_intervention";self.rows[name]["reason"]=reason;self.rows[name]["revision"]=self.rows[name].get("revision",0)+1;self.persist()
    def control(self,job,args):
        """Apply a bounded phase control with live owner authority, CAS and durable retry receipts."""
        import hashlib,re
        if args["action"]=="increase_memory":return self.increase_memory(job,args)
        action=args["action"];request=args.get("request_id")
        if not isinstance(request,str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",request):raise ValueError("Stable resource request ID required")
        phase_id=args.get("phase_id")
        if not isinstance(phase_id,str)or not re.fullmatch(r"[a-f0-9]{64}",phase_id):raise ValueError("Opaque phase ID required")
        revision=args.get("expected_revision")
        if type(revision)is not int:raise ValueError("Expected resource revision required")
        normalized=json.dumps([job["job_id"],args],sort_keys=True,separators=(",",":"))
        digest=hashlib.sha256(normalized.encode()).hexdigest()
        token=hashlib.sha256((job["job_id"]+request).encode()).hexdigest()
        with self.condition:
            receipt=self.controls.get(token)
            if receipt:
                if receipt["digest"]!=digest:raise ValueError("Resource request ID reused")
                return copy.deepcopy(receipt["result"])
            name=next((n for n,r in self.rows.items()if r["job_id"]==job["job_id"]and hashlib.sha256(n.encode()).hexdigest()==phase_id),None)
            if name is None:raise ValueError("Phase unavailable")
            row=self.rows[name]
            if row.get("revision",0)!=revision:raise ValueError("Resource phase changed")
            if action=="reconcile":
                if row["state"]!="recovery_required"or job["status"]in {"queued","running"}or job.get("effects_started"):
                    raise ValueError("Only stopped pre-publication phases may be reconciled")
                self.runner.cleanup_phase(name)
                finish=getattr(self.runner,"disk_finish",None)
                if callable(finish):finish(name)
                row.update(state="failed",reason="Explicit orphan phase cleanup; no automatic restart",finished=self.clock(),paused_at=None)
            else:
                if job["status"]!="running"or job.get("revoked")or job.get("lease_until",0)<=time.time():raise PermissionError("Live executing owner authority required")
                if action=="pause":
                    self.act(name,"freeze");row["manual_pause"]=True
                elif action=="resume":
                    if self.host["available"]-self.reserve()<max(0,row["estimate"]-row["metrics"].get("memory",0)):
                        raise ValueError("Insufficient measured headroom for resume")
                    self.act(name,"thaw");row["manual_pause"]=False;row.pop("reason",None);row.pop("unconfirmed_thaw",None)
                elif action=="priority":
                    priority=args.get("priority")
                    if type(priority)is not int or not -2<=priority<=2:raise ValueError("Priority must be between -2 and 2")
                    if row["state"]not in {"running","parked","waiting_intervention"}:raise ValueError("Active phase required")
                    row["priority"]=priority
                else:raise ValueError("Unsupported resource action")
            row["revision"]=row.get("revision",0)+1
            result={"phase_id":phase_id,"job_id":job["job_id"],"state":row["state"],"revision":row["revision"]}
            self.controls[token]={"digest":digest,"result":result}
            self.bound_controls()
            self.persist();self.condition.notify_all();return result


    # @code-index {"version":1,"feature":"compiler-memory-policy","aliases":["compiler memory defaults","task memory overrides","live memory growth"],"purpose":"Protected compiler ceilings and live growth. Config: build-service/scheduler-defaults.json -> compiler_memory_default_bytes, compiler_memory_min_bytes, compiler_memory_max_bytes, allow_live_memory_increase; installation JSON overrides.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/compiler_memory_default_bytes","/compiler_memory_min_bytes","/compiler_memory_max_bytes","/allow_live_memory_increase"]}]}}
    def memory_policy(self):
        """Expose only protected scalar bounds and their canonical broker/helper binding."""
        import hashlib
        keys={"compiler_memory_default_bytes":5*1024**3,"compiler_memory_min_bytes":1024**3,"compiler_memory_max_bytes":10*1024**3,"allow_live_memory_increase":False}
        values={k:self.policy.get(k,v)for k,v in sorted(keys.items())}
        return {"values":values,"sha256":hashlib.sha256(json.dumps(values,sort_keys=True,separators=(",",":")).encode()).hexdigest()}

    def adopt_memory_policy(self,policy,expected):
        """Adopt validated root-owned bounds without changing already admitted job ceilings."""
        with self.condition:
            previous=self.memory_policy()
            values={k:policy[k]for k in previous["values"]}
            if previous["sha256"]!=expected and values!=previous["values"]:raise ValueError("Resource policy revision changed")
            self.policy.update(values);self.persist();self.condition.notify_all()
            return self.memory_policy()

    def reconcile_memory(self,name,observed):
        """Classify an uncertain increase from exact kernel evidence; never repeat the effect."""
        row=self.rows[name];change=row["memory_change"];actual=observed.get("memory_max")
        if observed.get("container_id")!=row["container_id"]or observed.get("swap_max")!=0:raise ValueError("Memory identity or swap binding changed")
        if actual==change["new_cap"]:
            row["hard_cap"]=actual;row["estimate"]=actual;outcome="applied"
        elif actual==change["old_cap"]:
            row["hard_cap"]=actual;row["estimate"]=change["old_estimate"];outcome="not_applied"
        else:raise ValueError("Memory increase requires reconciliation")
        change["state"]=outcome
        if row["state"]=="memory_recovery_required":row["state"]=change["previous_state"];row.pop("reason",None)
        row["revision"]=row.get("revision",0)+1
        evidence={"request_id":change["request_id"],"old_bytes":change["old_cap"],"requested_bytes":change["new_cap"],"observed_memory_max":actual,"observed_swap_max":0,"outcome":outcome,"at":time.time()}
        row.setdefault("memory_history",[]).append(evidence)
        result={"job_id":row["job_id"],"phase_id":change["phase_id"],"revision":row["revision"],"state":outcome,"memory_bytes":actual}
        self.controls[change["token"]]["result"]=result
        self.persist();self.condition.notify_all()
        return copy.deepcopy(result)

    def bound_controls(self):
        """Keep retry storage bounded while retaining every unresolved memory intent."""
        while len(self.controls)>1024:
            removable=next((k for k,v in self.controls.items()if v["result"].get("state")!="uncertain"),None)
            if removable is None:raise ValueError("Unresolved resource control bound reached")
            del self.controls[removable]

    def increase_memory(self,job,args):
        """Reserve full growth before a root-authorized increase; uncertainty keeps that reservation."""
        import hashlib
        request=args.get("request_id");phase_id=args.get("phase_id");revision=args.get("expected_revision")
        if not isinstance(request,str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",request):raise ValueError("Stable resource request ID required")
        if not isinstance(phase_id,str)or not re.fullmatch(r"[a-f0-9]{64}",phase_id)or type(revision)is not int:raise ValueError("Opaque phase ID and revision required")
        cap=args.get("memory_bytes");policy=self.memory_policy();limits=policy["values"]
        if not self.enabled or not limits["allow_live_memory_increase"]:raise PermissionError("Live memory increases disabled")
        if type(cap)is not int or not limits["compiler_memory_min_bytes"]<=cap<=limits["compiler_memory_max_bytes"]or cap%os.sysconf("SC_PAGE_SIZE"):raise ValueError("Compiler memory request outside policy")
        token=hashlib.sha256((job["job_id"]+request).encode()).hexdigest();digest=hashlib.sha256(json.dumps([job["job_id"],args],sort_keys=True,separators=(",",":")).encode()).hexdigest()
        with self.condition:
            receipt=self.controls.get(token)
            if receipt and receipt["digest"]!=digest:raise ValueError("Resource request ID reused")
            if receipt and receipt["result"]["state"]!="uncertain":return copy.deepcopy(receipt["result"])
            name=next((n for n,r in self.rows.items()if r["job_id"]==job["job_id"]and hashlib.sha256(n.encode()).hexdigest()==phase_id),None)
            if name is None:raise ValueError("Phase unavailable")
            row=self.rows[name]
            if job["status"]!="running"or job.get("revoked")or job.get("lease_until",0)<=time.time():raise PermissionError("Live executing owner authority required")
            if receipt:
                result=self.runner.resource_request({"operation":"stats","container":name})
                return self.reconcile_memory(name,result)
            if row.get("revision",0)!=revision:raise ValueError("Resource phase changed")
            if row["state"]not in {"running","parked"}or row["phase"]not in PARKABLE or row["kind"]not in {"engine","plugin"}or not row.get("hard_cap"):raise ValueError("Capped compiler phase required")
            if cap<=row["hard_cap"]:raise ValueError("Only increasing a running compiler ceiling is supported")
            if len(row.get("memory_history",[]))>=32:raise ValueError("Phase memory change bound reached")
            self.host=self.sampler()
            remaining=sum(max(0,r["estimate"]-r.get("metrics",{}).get("memory",0))for r in self.rows.values()if r["state"]in {"admitted","running","parked"})
            available=self.host["available"]-self.reserve()-remaining
            delta=cap-row["estimate"]
            if available<delta or any(r["state"]in {"recovery_required","waiting_intervention","memory_recovery_required"}for r in self.rows.values()):
                result={"job_id":job["job_id"],"phase_id":phase_id,"revision":revision,"state":"waiting_headroom","memory_bytes":row["hard_cap"],"requested_bytes":cap,"available_growth_bytes":max(0,available)}
                self.controls[token]={"digest":digest,"result":result};self.bound_controls();self.persist()
                return copy.deepcopy(result)
            change={"request_id":request,"phase_id":phase_id,"token":token,"state":"intent","old_cap":row["hard_cap"],"old_estimate":row["estimate"],"new_cap":cap,"previous_state":row["state"],"at":time.time()}
            row["memory_change"]=change;row["estimate"]=cap
            self.controls[token]={"digest":digest,"result":{"job_id":job["job_id"],"phase_id":phase_id,"state":"uncertain","revision":revision}}
            self.bound_controls();self.persist()
            required=sum(max(r["estimate"],r.get("metrics",{}).get("memory",0))for r in self.rows.values()if r["state"]in {"admitted","running","parked"})
            try:
                observed=self.runner.resource_request({"operation":"memory","container":name,"memory_bytes":cap,"previous_bytes":change["old_cap"],"reserved_bytes":required,"policy_sha256":policy["sha256"],"lease_until":job["lease_until"]})
                return self.reconcile_memory(name,observed)
            except (OSError,ValueError,RuntimeError,TimeoutError):
                try:return self.reconcile_memory(name,self.runner.resource_request({"operation":"stats","container":name}))
                except (OSError,ValueError,RuntimeError,TimeoutError):
                    change["state"]="uncertain";row.update(state="memory_recovery_required",reason="Memory effect outcome requires exact kernel reconciliation",revision=row.get("revision",0)+1)
                    self.controls[token]["result"]["revision"]=row["revision"]
                    self.persist();self.condition.notify_all()
                    return copy.deepcopy(self.controls[token]["result"])

    def tick(self):
        """Sample real containers, reduce contention, and park/resume with hysteresis."""
        sample=self.sampler()
        status=getattr(self.runner,"cleanup_status",None)
        if callable(status)and self.clock()-getattr(self,"last_disk_sample",-10)>=10:
            try:self.disk_status=status()
            except (OSError,ValueError,RuntimeError,TimeoutError):self.disk_status={"deferred":"Cleanup telemetry unavailable"}
            self.last_disk_sample=self.clock()
        if getattr(self,"disk_status",None)is not None:sample["disk"]=self.disk_status
        with self.condition:self.host=sample;names=[n for n,r in self.rows.items()if r["state"]in {"running","parked","waiting_intervention"}]
        for name in names:
            try:
                result=self.runner.resource_request({"operation":"stats","container":name})
                with self.lock:
                    row=self.rows.get(name)
                    if row and result["container_id"]==row["container_id"]:
                        previous=row.get("last_sample")
                        sampled=self.clock()
                        if previous and sampled>previous["at"]:
                            result["cpu_percent"]=round(max(0,result["cpu_usec"]-previous["cpu_usec"])/(sampled-previous["at"])/10000,1)
                        row["last_sample"]={"at":sampled,"cpu_usec":result["cpu_usec"]};row["metrics"]=result
                        if result["frozen"]and row["state"]=="running":
                            row.update(state="waiting_intervention",paused_at=sampled,reason="Unconfirmed control outcome; explicit resume or cancellation required",revision=row.get("revision",0)+1)
                            self.persist()
                        elif not result["frozen"] and row["state"] in {"parked","waiting_intervention"} and not row.get("unconfirmed_thaw"):
                            row.update(state="waiting_intervention",manual_pause=True,unconfirmed_thaw=True,
                                reason="Unconfirmed thaw outcome; explicit resume or cancellation required",revision=row.get("revision",0)+1)
                            self.persist()
            except (OSError,ValueError,RuntimeError,TimeoutError):continue
        if not self.enabled:return
        now=self.clock()
        if now-self.last_cpu_adjust>=5:
            if sample["pressure"]["cpu"]>20:
                self.cpu_budget=max(.5,self.cpu_budget-.5);self.last_cpu_recovery=now
            elif now-self.last_cpu_recovery>=20:
                self.cpu_budget=min(max(1,(os.cpu_count()or 1)-1),self.cpu_budget+.5);self.last_cpu_recovery=now
            running=[n for n in names if self.rows[n]["state"]=="running"]
            weights={n:1+max(0,self.rows[n]["priority"])+min(2,int((now-self.rows[n]["started"])/900))for n in running}
            for name in running:
                millis=max(100,int(1000*self.cpu_budget*weights[name]/sum(weights.values())))
                self.runner.resource_request({"operation":"cpu","container":name,"cpu_millis":millis})
            self.last_cpu_adjust=now
        pressure=sample["pressure"]["memory"]>self.policy.get("memory_pressure_percent",5)
        if pressure:
            if self.pressure_since is None:self.pressure_since=now
        else:self.pressure_since=None
        low=sample["available"]<self.reserve()or(pressure and now-self.pressure_since>=5)
        with self.lock:
            parked=[n for n,r in self.rows.items()if r["state"]in {"parked","waiting_intervention"}]
            candidates=sorted((n for n,r in self.rows.items()if r["state"]=="running"and r["phase"]in PARKABLE),key=lambda n:(self.rows[n]["priority"],-self.rows[n]["metrics"].get("memory",0),-self.rows[n]["started"]))
        if low:
            self.stable_since=None
            if candidates and len(parked)<self.policy.get("max_parked",2):
                name=candidates[0]
                try:
                    self.act(name,"freeze");before=self.rows[name]["metrics"].get("memory",0)
                    budget=min(before,max(0,sample["swap_free"]-1024**3),sample["swap_total"]//2)
                    attempts=0
                    while budget>0 and attempts<16 and self.sampler()["available"]<self.reserve():
                        amount=min(budget,256*1024**2)
                        self.runner.resource_request({"operation":"reclaim","container":name,"bytes":amount})
                        budget-=amount;attempts+=1
                    after=self.runner.resource_request({"operation":"stats","container":name})
                    with self.lock:self.rows[name]["metrics"]=after
                    if self.sampler()["available"]<self.reserve():self.intervention(name,"Insufficient reclaimed headroom; owner or administrator action required")
                except (OSError,ValueError,RuntimeError,TimeoutError):
                    if self.rows[name]["state"]=="parked":self.intervention(name,"Reclaim unavailable or outcome needs inspection")
        elif sample["available"]>self.reserve()+256*1024**2 and not pressure:
            if self.stable_since is None:self.stable_since=now
            if now-self.stable_since>=20:
                with self.lock:
                    eligible=sorted((n for n,r in self.rows.items()if r["state"]=="parked"and not r.get("manual_pause")),key=lambda n:self.rows[n]["started"])
                for name in eligible:
                    with self.lock:r=self.rows[name];needed=max(0,r["estimate"]-r["metrics"].get("memory",0))
                    if sample["available"]-self.reserve()>=needed:
                        self.act(name,"thaw");self.stable_since=now;break
        with self.condition:
            if now-self.last_save>=10:self.persist();self.last_save=now
            self.condition.notify_all()
    def loop(self):
        """Keep helper failures observable without terminating the protected broker."""
        while not self.stop.wait(.5):
            try:self.tick()
            except (OSError,ValueError,RuntimeError,TimeoutError):
                with self.lock:self.host["controller_error"]="Resource sampling unavailable"
    def snapshot(self,job_id=None):
        """Return only content-free telemetry; frozen processes never hide unfinished ownership."""
        with self.lock:
            import hashlib
            rows={hashlib.sha256(n.encode()).hexdigest():{k:v for k,v in r.items()if k not in {"container_id","last_sample","paused_at","started","finished"}}|{"phase_id":hashlib.sha256(n.encode()).hexdigest(),"paused_seconds":self.elapsed_pause(n)}
                for n,r in self.rows.items()if job_id is None or r["job_id"]==job_id}
            return copy.deepcopy({"enabled":self.enabled,"host":self.host,"reserve":self.reserve(),"memory_policy":self.memory_policy(),"phases":rows,"profiles":self.profiles if job_id is None else{}})
