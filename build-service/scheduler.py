"""Broker integration for durable ownership and verified agent completion.

Ownership is separate from immutable source generations and release receipts.
Config: protected build-service.json -> scheduler.mode and orphan_after_seconds.
"""
import hashlib,importlib.util,json,os,re,subprocess,time
from pathlib import Path
def sibling(name):
    """Load the exact paired installed implementation, never a model-selected path."""
    spec=importlib.util.spec_from_file_location("build_scheduler_"+name,Path(__file__).with_name(name+".py"))
    value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
ownership=sibling("ownership")
class Conflict(ValueError):
    """Preserve machine-readable blockers through the broker transport."""
    def __init__(self,details):
        """Preserve bounded structured blockers through the broker error transport."""
        self.details={"kind":"ownership_conflict",**details};super().__init__(ownership.canonical(self.details))
# @code-index {"version":1,"feature":"controlled-build-scheduler","aliases":["parallel builds","source ownership","compiler parking"],"purpose":"Source reservations and completion. Config: build-service/scheduler-defaults.json -> mode, orphan_after_seconds; protected installation overrides at broker restart.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/mode","/orphan_after_seconds","/reserve_bytes","/reserve_percent","/max_phases","/max_parked","/memory_pressure_percent","/ui_file_reservations","/max_ui_preparations","/component_indexing","/concurrent_ui_preparation","/ui_test_image"]}],"opportunities":[]}}
class Scheduler:
    """Derive authority and scope from the broker; caller assertions never certify a release."""
    actions={"create","resources","pause","resume","priority","reconcile","increase_memory","reservations","reserve","heartbeat","continue","acceptance","complete","handoff","takeover_preview","takeover","recover","reconcile_acquisition"}
    def __init__(self,broker,config):
        """Load reviewed coordination policy and open its private durable ownership store."""
        self.broker=broker;self.config=config
        policy=config.get("scheduler",{})
        self.mode=policy.get("mode","observe")
        self.orphan_after=policy.get("orphan_after_seconds",300)
        self.store=ownership.Ownership(broker.work_dir/"ownership.sqlite")
    @staticmethod
    def owner(trusted,workspace):
        """Caller identity is sealed by Unix-peer or Rust transport, not model arguments."""
        return ownership.canonical([trusted["user_id"],workspace])
    def ensure(self,item,trusted,workspace):
        """Register unclassified legacy tasks only on explicit interaction; do not infer completion."""
        key=item["key"];owner=self.owner(trusted,workspace)
        try:value=self.store.get(key)
        except ValueError:value=self.store.begin(key,owner,item["task_id"],"scheduler-begin-"+key[:48])
        if value["owner"]!=owner:raise PermissionError("Logical task owner changed")
        if "ownership_epoch"not in item:
            item["ownership_epoch"]=value["epoch"];self.broker._save_work()
        return value
    def retention_allowed(self,key):
        """Keep registered task source until explicit verified agent completion; old clearances do not release claims."""
        try:value=self.store.get(key)
        except ValueError:return self.mode!="enforce"
        return value["state"]=="complete"

    def components(self,item):
        """Use current compile attestations and the owned tree's declarations to derive conflicts."""
        root=Path(item["workspace_path"])
        compile_inputs=self.broker.runner.core_compile_inputs()if hasattr(self.broker.runner,"core_compile_inputs")else()
        return ownership.Components(root,compile_inputs,self.config.get('scheduler',{}).get('ui_file_reservations',False))
    def waiter_changes(self,item):
        """Compare application source to its sealed baseline; never modify either source or inspection data."""
        tree=Path(item["workspace_path"]).parent
        baseline=self.broker.scheduler_entries((tree/"baseline.tar").read_bytes())
        current=self.broker.scheduler_entries(self.broker.scheduler_task_snapshot(tree))
        return sorted(p for p in set(baseline)|set(current)if p.startswith("v3/")and not p.startswith("v3/data/")and baseline.get(p)!=current.get(p))

    def require_clean_waiter(self,item,value):
        """Reject an edited waiting draft before acquiring claims that require a source refresh."""
        if value["state"]=="waiting"and value["requested"]and self.waiter_changes(item):
            raise ValueError("Waiting task contains edits; preserve and replan before acquiring current source")

    # @code-index {"version":1,"feature":"edited-acquisition-recovery","aliases":["stuck reservation","edited waiter"],"purpose":"Operator recovery preserves a dirty draft and compensates its exact committed acquisition; authority, ownership and retry bindings are invariants.","span_lines":80,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Recovery authority and exact acquisition/source binding are fixed safety invariants."}}
    def reconcile_acquisition(self,args,trusted,workspace):
        """Compensate one committed, pre-refresh edited-waiter acquisition without changing its draft.

        Operator-only recovery binds the exact original request, committed receipt,
        current ownership CAS and source digest. It releases only that acquisition's
        claims, fences its epoch and settles both journals through durable retry.
        Original SQLite acquisition evidence and all source/baseline bytes remain.
        """
        required={"action","task_id","reserve_request","expected_inputs_sha256","expected_revision","expected_epoch","request_id"}
        if not trusted.get("local_caller")or set(args)!=required:
            raise PermissionError("Exact local-operator acquisition recovery required")
        original=args["reserve_request"]
        if not isinstance(original,dict)or set(original)!={"action","task_id","paths","expected_revision","expected_epoch","request_id"}or original["action"]!="reserve"or original["task_id"]!=args["task_id"]:
            raise ValueError("Exact original reserve request required")
        if type(args["expected_revision"])is not int or type(args["expected_epoch"])is not int or not isinstance(args["expected_inputs_sha256"],str)or not re.fullmatch(r"[a-f0-9]{64}",args["expected_inputs_sha256"]):
            raise ValueError("Current source and ownership binding required")
        with self.broker.lock:
            self.broker._incoming_lease(trusted)
            item=self.item(args,trusted,workspace);owner=self.owner(trusted,workspace);key=item["key"]
            path=self.journal_path(owner,args["request_id"],"acquisition-recovery-"+key)
            original_path=self.journal_path(owner,original["request_id"],key)
            digest=hashlib.sha256(ownership.canonical([owner,args]).encode()).hexdigest()
            def read_journal(p):
                """Read bounded existing broker evidence without following a substituted journal."""
                import stat
                fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC)
                try:
                    meta=os.fstat(fd)
                    if not stat.S_ISREG(meta.st_mode)or meta.st_nlink!=1 or meta.st_size>1024*1024:raise ValueError("Invalid acquisition recovery journal")
                    with os.fdopen(fd,"rb",closefd=False)as stream:return json.loads(stream.read(1024*1024+1))
                finally:os.close(fd)
            record=read_journal(path)if path.exists()or path.is_symlink()else None
            if record:
                if record["digest"]!=digest:raise ValueError("Request ID reused")
                if record["state"]=="done":return record["result"]
            previous=read_journal(original_path)
            original_digest=hashlib.sha256(ownership.canonical([owner,original]).encode()).hexdigest()
            if previous["task_key"]!=key or previous["request_id"]!=original["request_id"]or previous["digest"]!=original_digest:
                raise ValueError("Original acquisition binding differs")
            transition="action-"+hashlib.sha256(ownership.canonical([owner,key,original["request_id"]]).encode()).hexdigest()
            acquisition=self.store.receipt(transition)
            before=previous["value"]
            if not acquisition or acquisition.get("acquired")is not True or before["state"]!="waiting"or not before["requested"]or before["scopes"]or before["owner"]!=owner:
                raise ValueError("Committed empty-waiter acquisition required")
            committed=acquisition["task"]
            if committed["id"]!=key or committed["owner"]!=owner or not committed["scopes"]:
                raise ValueError("Committed acquisition identity differs")
            release_token="acquisition-release-"+hashlib.sha256(ownership.canonical([owner,key,args["request_id"]]).encode()).hexdigest()[:40]
            released=self.store.receipt(release_token)
            if previous["state"]!="pending"and not(previous["state"]=="rejected"and previous.get("reconciled_by")==args["request_id"]and released is not None):
                raise ValueError("Matching pending acquisition required")
            pending=self.pending(key,except_path=path)
            if pending not in (original["request_id"],None)or(pending is None and released is None):
                raise ValueError("Another scheduler transition requires recovery")
            if item.get("status")!="open"or item.get("job_id"):
                raise ValueError("Unpublished open draft required")
            if any(j.get("ownership_id")==key and j["status"]in {"queued","running","needs_decision","uncertain","interrupted"}for j in self.broker.jobs.values())or self.broker.code_index_leases:
                raise ValueError("Reconcile execution and active reviews before acquisition recovery")
            refresh=item.get("acquisition_refresh")
            if refresh!={"revision":committed["revision"]}and not(released is not None and refresh is None):
                raise ValueError("Matching pre-refresh acquisition required")
            folder=self.broker.work_dir/"acquisitions"/key/str(committed["revision"])
            if folder.is_symlink()or(folder.exists()and any(folder.iterdir())):
                raise ValueError("Acquisition filesystem effects require separate reconciliation")
            tree=Path(item["workspace_path"]).parent
            source=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))
            binding={"source_sha256":source,"baseline_sha256":hashlib.sha256((tree/"baseline.tar").read_bytes()).hexdigest(),
                     "original_digest":original_digest,"acquisition_sha256":hashlib.sha256(ownership.canonical(acquisition).encode()).hexdigest()}
            if source!=args["expected_inputs_sha256"]or not self.waiter_changes(item):
                raise ValueError("Preserved edited draft source differs")
            if record:
                if record["binding"]!=binding:raise ValueError("Acquisition recovery source changed")
            else:
                current=self.store.get(key)
                if current["revision"]!=args["expected_revision"]or current["epoch"]!=args["expected_epoch"]or any(current[k]!=committed[k]for k in ["id","owner","epoch","generation","scopes","requested"]):
                    raise ValueError("Stale acquisition ownership binding")
                record={"digest":digest,"state":"pending","request_id":args["request_id"],"task_key":key,"binding":binding}
                self.write_record(path,record)
            if released is None:
                self.broker._incoming_lease(trusted)
                released=self.store.release_scopes(key,owner,args["expected_revision"],args["expected_epoch"],committed["scopes"],"Reconcile committed edited-waiter acquisition; preserve draft and original receipt",release_token)
            current=self.store.get(key)
            if any(current[k]!=released[k]for k in ["id","owner","epoch","generation","scopes","requested"])or current["scopes"]:
                raise ValueError("Released acquisition changed before reconciliation")
            item.pop("acquisition_refresh",None)
            item.update(ownership_epoch=current["epoch"],ownership_revision=current["revision"])
            item.pop("ownership_inputs_sha256",None);self.broker._save_work()
            if self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))!=source or hashlib.sha256((tree/"baseline.tar").read_bytes()).hexdigest()!=binding["baseline_sha256"]:
                raise ValueError("Draft changed during acquisition reconciliation")
            self.write_record(original_path,{**previous,"state":"rejected","error":"Committed acquisition explicitly reconciled and released; replan in a fresh task","reconciled_by":args["request_id"]})
            result={"task":current,"released_scopes":committed["scopes"],"source_preserved":True,**binding,"original_request_id":original["request_id"]}
            self.write_record(path,{**record,"state":"done","result":result})
            self.broker._start_next()
            return result

    def refresh_waiter(self,item,value):
        """A queued writer starts from current published code while preserving its old draft for recovery."""
        pending=item.get("acquisition_refresh")
        if pending is None:
            return
        tree=Path(item["workspace_path"]).parent
        folder=self.broker.work_dir/"acquisitions"/item["key"]/str(pending["revision"]);folder.mkdir(parents=True,exist_ok=True)
        captured=folder/"candidate.tar";prepared=folder/"prepared";archive=folder/"previous"
        if not captured.exists():
            baseline=self.broker.scheduler_entries((tree/"baseline.tar").read_bytes())
            old=self.broker.scheduler_entries(self.broker.scheduler_task_snapshot(tree))
            changed=[p for p in set(baseline)|set(old)if p.startswith("v3/")and not p.startswith("v3/data/")and baseline.get(p)!=old.get(p)]
            if changed:raise ValueError("Waiting task contains edits; preserve and replan before acquiring current source")
            # Pin the same read-only inspection snapshot; it never becomes an application build input.
            live=self.broker.scheduler_preserve_inspection(self.broker._current_release_blob(),tree)
            captured.write_bytes(live)
        blob=captured.read_bytes()
        fresh=tree.exists()and(tree/"baseline.tar").is_file()and(tree/"baseline.tar").read_bytes()==blob
        if not fresh:
            if not prepared.exists():
                if not tree.exists()and not archive.exists():raise ValueError("Acquisition source requires reconciliation")
                old_tree=tree if tree.exists()else archive
                old=self.broker.scheduler_entries(self.broker.scheduler_task_snapshot(old_tree))
                temporary=folder/"preparing"
                if temporary.exists():
                    import shutil
                    shutil.rmtree(temporary)
                self.broker.scheduler_extract(temporary,blob)
                (temporary/"baseline.tar").write_bytes(blob);temporary.rename(prepared)
            if self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(prepared))!=self.broker.scheduler_digest(blob):
                raise ValueError("Prepared acquisition source changed")
            if tree.exists():
                if archive.exists():raise ValueError("Acquisition archive requires reconciliation")
                tree.rename(archive)
            prepared.rename(tree)
        item["base_inputs_sha256"]=self.broker.scheduler_digest(blob);item.pop("acquisition_refresh",None)
        item["acquisition_revision"]=pending["revision"];self.broker._save_work()
    def mark_waiter_refresh(self,item,previous,current):
        """Journal the refresh before replacing any task tree, retaining a stable retry revision."""
        if previous["state"]=="waiting"and previous["requested"]and item.get("acquisition_revision")!=current["revision"]:
            item["acquisition_refresh"]={"revision":current["revision"]};self.broker._save_work()
        self.refresh_waiter(item,current)

    def reserve(self,item,trusted,workspace,paths,request=None,expected=None):
        """Acquire before edits; failed expansion returns concrete blockers without changing source."""
        value=self.ensure(item,trusted,workspace);self.refresh_waiter(item,value);scopes=self.components(item).scopes(paths)
        self.require_clean_waiter(item,value)
        if value["state"]not in {"waiting","owned"}:
            self.store.assert_owned(value["id"],value["owner"],item["ownership_epoch"],scopes)
            return value
        if set(scopes)<=set(value["scopes"]):
            self.store.assert_owned(value["id"],value["owner"],item["ownership_epoch"],scopes);return value
        revision=value["revision"]if expected is None else expected
        token=request or"reserve-"+hashlib.sha256(ownership.canonical([value["id"],revision,scopes]).encode()).hexdigest()[:48]
        result=self.store.acquire(value["id"],value["owner"],revision,item["ownership_epoch"],scopes,token)
        if not result["acquired"]:raise Conflict(result)
        self.mark_waiter_refresh(item,value,result["task"])
        return result["task"]
    def guard(self,item,trusted,workspace,paths,request):
        """Enforced writes require ownership; observe mode records no implicit conflicting claims."""
        if self.mode!="enforce":return
        if item.get("mode")!="isolated":raise ValueError("Legacy shared work must be reconciled into an isolated task")
        self.housekeeping()
        if self.pending(item["key"]):raise ValueError("Scheduler transition requires exact-request recovery before editing")
        value=self.reserve(item,trusted,workspace,paths)
        self.store.assert_owned(value["id"],value["owner"],item["ownership_epoch"],self.components(item).scopes(paths))
        item["ownership_revision"]=value["revision"]
    def edited(self,item,trusted,workspace,request):
        """Invalidate acceptance after a checked source edit; repeated applies return existing receipts."""
        if self.mode!="enforce":return
        value=self.store.get(item["key"])
        current=self.store.edited(value["id"],self.owner(trusted,workspace),value["revision"],item["ownership_epoch"],"edit-"+hashlib.sha256(ownership.canonical([item["key"],request]).encode()).hexdigest()[:48])
        item["source_generation"]=current["generation"];item["ownership_revision"]=current["revision"];self.broker._save_work()
    def submit(self,item,trusted,workspace,paths,inputs):
        """Direct operator patches also require derived claims before any build is admitted."""
        if self.mode!="enforce":return
        value=self.reserve(item,trusted,workspace,paths)
        if item.get("ownership_inputs_sha256")!=inputs:
            current=self.store.edited(value["id"],value["owner"],value["revision"],item["ownership_epoch"],"submit-"+hashlib.sha256((item["key"]+inputs).encode()).hexdigest()[:48])
            item["source_generation"]=current["generation"];item["ownership_inputs_sha256"]=inputs;self.broker._save_work()
    def list(self,trusted,workspace,administrator=False,offset=0,limit=20):
        """Expose reservation metadata only; private source, evidence bodies and chat text stay private."""
        if type(offset)is not int or type(limit)is not int or not 0<=offset<=1000 or not 1<=limit<=50:raise ValueError("Bounded reservation page required")
        self.housekeeping();rows=self.store.list()
        result=[]
        for r in rows:
            if r["state"]in {"complete","abandoned"}:continue
            result.append({k:r[k]for k in ["id","label","state","revision","epoch","generation","heartbeat","created","scopes","requested"]}|{"owned_by_caller":r["owner"]==self.owner(trusted,workspace)})
        legacy=[{"task_id":i["task_id"],"state":"legacy_review_required"}for i in self.broker.work.values()if i.get("mode")=="isolated"and i.get("status")!="abandoned"and not any(r["id"]==i["key"]for r in rows)]
        total=len(result);paged=result[offset:offset+limit]
        for r in paged:
            for key in ("scopes","requested"):
                if len(r[key])>32:r[key+"_count"]=len(r[key]);r[key]=r[key][:32];r[key+"_truncated"]=True
        return {"mode":self.mode,"tasks":paged,"total":total,"offset":offset,"next_offset":offset+len(paged)if offset+len(paged)<total else None,"legacy":legacy[:20]if administrator else [r for r in legacy if any(i["task_id"]==r["task_id"]and self.broker._owned(i,trusted,workspace)for i in self.broker.work.values())]}
    def item(self,args,trusted,workspace,administrator=False):
        """Normal actions require the sealed owner; takeover uses an explicitly administrative lookup."""
        label=args.get("task_id")
        if not isinstance(label,str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",label):raise ValueError("Bounded task ID required")
        if administrator:
            task_key=args.get("task_key")
            if task_key is not None and(not isinstance(task_key,str)or not re.fullmatch(r"[a-f0-9]{64}",task_key)):raise ValueError("Exact takeover task key required")
            found=[i for i in self.broker.work.values()if i["task_id"]==label and(task_key is None or i["key"]==task_key)]
            if len(found)!=1:raise ValueError("Takeover task identity is missing or ambiguous")
            return found[0]
        key=hashlib.sha256(ownership.canonical([trusted["user_id"],workspace,label]).encode()).hexdigest()
        if key not in self.broker.work:raise ValueError("Owned source task required")
        if args.get("task_key")is not None and args["task_key"]!=key:raise PermissionError("Task key is outside this caller namespace")
        return self.broker.work[key]
    def verified(self,item,args):
        """Verify terminal publication, current promoted bytes and exact Git history before acceptance."""
        job=self.broker.jobs.get(args.get("job_id"))
        if not job or job.get("task_id")!=item["task_id"]or job.get("user_id")!=item["user_id"]or job.get("workspace")!=item["workspace"]or job["status"]!="completed"or job.get("batch_parent")or job.get("synchronization",{}).get("status")!="synchronized":
            raise ValueError("Current synchronized completed release required")
        if item.get("job_id")!=job["job_id"]or item["status"]!="closed":raise ValueError("Latest source generation must be published")
        if any(j.get("task_id")==item["task_id"]and j["status"]in {"queued","running","uncertain","needs_decision"}for j in self.broker.jobs.values()):
            raise ValueError("Task has unresolved execution")
        receipt=Path(self.broker.state)/job["job_id"]/"candidate.tar"
        # The broker's already validated archive reader binds paths to the release snapshot.
        files=self.broker.scheduler_entries(receipt.read_bytes())
        promoted={}
        source=self.broker.runner.source
        commit=args.get("commit")
        if not isinstance(commit,str)or not re.fullmatch(r"[a-f0-9]{40}",commit):raise ValueError("Verified release commit required")
        def git(*command):
            """Inspect exact committed objects without changing the shared checkout."""
            result=subprocess.run(["git","-C",str(source),*command],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=10)
            result.check_returncode();return result.stdout
        git("merge-base","--is-ancestor",commit,"HEAD")
        committed=set(git("diff-tree","--root","--no-commit-id","--name-only","-r",commit).decode().splitlines())
        for archive in job.get("changed_paths",[]):
            if not archive.startswith("v3/"):raise ValueError("Read-only shared asset change")
            relative=ownership.source_path(archive);expected=files.get(archive)
            data=None if expected is None else expected[1]
            path=source/relative
            if path.is_symlink()or(path.read_bytes()if path.exists()else None)!=data:raise ValueError("Promoted source changed after release")
            result=subprocess.run(["git","-C",str(source),"show",commit+":"+relative],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=10)
            if(result.stdout if result.returncode==0 else None)!=data:raise ValueError("Commit bytes differ from release")
            if relative not in committed:raise ValueError("Release path absent from selected commit")
            promoted[relative]=None if data is None else hashlib.sha256(data).hexdigest()
        acceptance=args.get("acceptance")
        if not isinstance(acceptance,list)or not 1<=len(acceptance)<=32 or any(not isinstance(a,dict)or set(a)!={"check","passed"}or not isinstance(a["check"],str)or not 1<=len(a["check"])<=200 or a["passed"]is not True for a in acceptance):
            raise ValueError("Bounded explicit acceptance results required")
        if not promoted:raise ValueError("Empty release cannot certify source ownership")
        tree=self.broker.work_dir/"trees"/item["key"]
        if tree.exists()and self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))!=self.broker.scheduler_digest((Path(self.broker.state)/job["job_id"]/"task.tar").read_bytes()):
            raise ValueError("Source generation changed after submission")
        current=self.store.get(item["key"])
        if "source_generation"in job and job["source_generation"]!=current["generation"]:raise ValueError("Published ownership generation changed")
        active=self.broker.scheduler_entries(self.broker._current_release_blob())
        closure=set()
        if job["operation"]=="build_deploy":
            closure={p for p in files if p.startswith("v3/src/")}
            closure.update({"v3/Cargo.toml","v3/Cargo.lock","v3/build.rs","v3/Dockerfile","v3/worker.py","v3/sandbox.py"})
            if hasattr(self.broker.runner,"core_compile_inputs"):closure.update(self.broker.runner.core_compile_inputs())
        else:
            for scope in current["scopes"]:
                if scope.startswith("module:ability:"):
                    prefix="v3/plugins/abilities/"+scope.removeprefix("module:ability:")+"/"
                    closure.update(p for p in files if p.startswith(prefix))
        for name in closure:
            if files.get(name)!=active.get(name):raise ValueError("Release dependency changed; revalidate a current generation")
        self.broker.scheduler_verify_health()
        checks=job.get("rust_tests",{"status":"not_required"})
        if checks.get("status")not in {"passed","not_required"}:raise ValueError("Release checks incomplete")
        return {"generation":self.store.get(item["key"])["generation"],"release_id":job["job_id"],"inputs_sha256":job["inputs_sha256"],"promoted_hashes":promoted,"checks":[checks],"acceptance":acceptance,"commit":commit}
    # @code-index {"version":1,"feature":"prepared-review-lease","aliases":["review ownership expiry","paused source review"],"purpose":"Keep exact authorized sealed-review ownership alive without execution resources. Config: build-service/scheduler-defaults.json -> orphan_after_seconds controls ordinary inactive owners; delegated authorization deadlines remain authoritative.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/orphan_after_seconds"]}]}}
    def review_wait(self,job,*,reset=False,activity=False,authority_until=None):
        """Keep an exact frozen review delegation alive without extending execution authority.

        Calls: ownership.touch only after owner/epoch/generation and staged-index bindings match.
        Config: scheduler.orphan_after_seconds controls ordinary inactivity, not review authorization.
        """
        if job.get("status")!="needs_decision"or job.get("phase")!="awaiting_prepared_reviews":return False
        now=time.time();previous=dict(job.get("review_wait",{}));record=dict(previous)
        state="active"
        try:
            value=self.store.get(job["ownership_id"]);item=self.broker.work.get(value["id"],{})
            if value["owner"]!=self.owner({"user_id":job["user_id"]},job["workspace"])or value["epoch"]!=job["ownership_epoch"]or value["generation"]!=job["source_generation"]:raise ValueError("Review ownership binding changed")
            if value["state"]not in {"owned","validating"}:state="ownership_recovery_required"
            if item.get("job_id")!=job["job_id"]or item.get("status")!="closed":raise ValueError("Review task generation changed")
            path=Path(job["prepared_assets"]);manifest_path=path/".code-index/manifest.json"
            if path.is_symlink()or manifest_path.is_symlink()or not manifest_path.is_file()or manifest_path.stat().st_size>2*1024*1024:raise ValueError("Review index unavailable")
            info=manifest_path.stat();identity=[info.st_dev,info.st_ino,info.st_size,info.st_mtime_ns,info.st_ctime_ns]
            cached=previous.get("binding",{})
            if previous.get("manifest_identity")==identity and cached:
                manifest={"release_id":cached["job_id"],"candidate_sha256":cached["candidate_sha256"],"source_sha256":cached["source_sha256"],"index_sha256":cached["index_sha256"],"gaps":0,"stale_count":0,"unindexed_count":0}
            else:manifest=json.loads(manifest_path.read_bytes())
            if manifest.get("release_id")!=job["job_id"]or manifest.get("candidate_sha256")!=job["inputs_sha256"]or any(manifest.get(k)!=0 for k in ("gaps","stale_count","unindexed_count")):raise ValueError("Review candidate changed")
            if any(not re.fullmatch(r"[a-f0-9]{64}",manifest.get(k,""))for k in ("source_sha256","index_sha256")):raise ValueError("Review index binding invalid")
            binding={"task_id":job["task_id"],"job_id":job["job_id"],"ownership_id":value["id"],"epoch":value["epoch"],"generation":value["generation"],"candidate_sha256":job["inputs_sha256"],"source_sha256":manifest["source_sha256"],"index_sha256":manifest["index_sha256"],"prepared_assets":str(path)}
            if not previous and not(reset or activity):return False
            if previous and previous.get("binding")!=binding and not reset:raise ValueError("Review delegation binding changed")
            if reset or not previous:record={"binding":binding,"started_at":now,"last_activity_at":now}
            deadline=job.get("lease_until",0)
            if authority_until is not None:deadline=min(deadline,authority_until)
            if not(reset or activity):deadline=min(deadline,record.get("expires_at",0))
            record["manifest_identity"]=identity
            record["expires_at"]=deadline
            if job.get("revoked")or deadline<=now:state="authorization_expired"
            if activity and now-record.get("last_activity_at",0)>=1:record["last_activity_at"]=now
            record["state"]=state
            if state=="active":
                self.store.touch(value["id"],value["owner"],value["epoch"])
        except (KeyError,OSError,ValueError,TypeError):
            record["state"]="binding_invalid";state="binding_invalid"
        if record!=previous:
            job["review_wait"]=record;self.broker._save(job)
        return state=="active"

    def housekeeping(self):
        """Keep authorized delegated execution active; orphaned source never becomes available automatically."""
        if self.mode!="enforce":return
        for job in self.broker.jobs.values():
            if job.get("ownership_id")and job["status"]in {"queued","running"}and not job.get("revoked")and job.get("lease_until",0)>time.time():
                self.store.touch(job["ownership_id"],self.owner({"user_id":job["user_id"]},job["workspace"]),job["ownership_epoch"])
            elif job.get("ownership_id")and job.get("status")=="needs_decision"and job.get("phase")=="awaiting_prepared_reviews":
                self.review_wait(job)
        self.store.orphan(self.orphan_after)
    def migrate_legacy(self):
        """Derive unfinished legacy claims at cutover; immutable historical generations stay explicitly classified."""
        if self.mode!="enforce":return
        for item in sorted(self.broker.work.values(),key=lambda i:i.get("created_at",0)):
            job=self.broker.jobs.get(item.get("job_id"),{})
            if item.get("mode")!="isolated"or item["status"]=="abandoned":continue
            if item["status"]!="open"and job.get("status")=="completed":continue
            if item.get("scheduler_classified"):continue
            trusted={"user_id":item["user_id"]};value=self.ensure(item,trusted,item["workspace"])
            tree=Path(item["workspace_path"]).parent
            if tree.exists()and(tree/"baseline.tar").exists():
                original=self.broker.scheduler_entries((tree/"baseline.tar").read_bytes())
                latest=self.broker.scheduler_entries(self.broker.scheduler_task_snapshot(tree))
                paths=[p for p in sorted(set(original)|set(latest))if original.get(p)!=latest.get(p)and not p.startswith("inspection/")]
                # Inspection snapshots are outside all source reservation domains.
                paths=[p for p in paths if p.startswith("v3/")and not p.startswith("v3/data/")]
            else:paths=job.get("changed_paths",[])
            if paths:
                scopes=self.components(item).scopes(paths)
                # A tracked orphan/validated generation already fences its owner.
                # Startup must preserve it, not expand or revive its reservations.
                if value["state"]in {"waiting","owned"}and not set(scopes)<=set(value["scopes"]):
                    result=self.store.acquire(value["id"],value["owner"],value["revision"],value["epoch"],scopes,
                        "legacy-"+hashlib.sha256(ownership.canonical([value["id"],value["revision"],scopes]).encode()).hexdigest())
                    value=result["task"]
                elif value["state"]not in {"waiting","owned"}and not set(scopes)<=set(value["scopes"]):
                    item["ownership_scope_review_required"]=True
            item["scheduler_classified"]=True;item["ownership_epoch"]=value["epoch"]
            if job.get("status")in {"queued","running","interrupted","uncertain","needs_decision"}:
                job.update(ownership_id=value["id"],ownership_epoch=value["epoch"],source_generation=value["generation"])
                self.broker._save(job)
        self.broker._save_work();self.housekeeping()

    def job_scopes(self,job):
        """Compute artifact conflicts from reserved component groups, never from job labels alone."""
        if job.get("ownership_id"):
            value=self.store.get(job["ownership_id"])
            return {s for s in value["scopes"]if s.startswith("module:")}|({"module:engine"}if job["operation"]in {"build","build_deploy","check"}else set())
        # Legacy unclassified executable work is conservative until explicit recovery.
        return {"module:engine"}if job["operation"]=="build_deploy"else{"module:legacy-publication"}
    def assert_job(self,job):
        """A revoked/transferred source epoch cannot publish a previously compiled candidate."""
        value=self.store.get(job["ownership_id"])
        self.store.assert_owned(value["id"],self.owner({"user_id":job["user_id"]},job["workspace"]),job["ownership_epoch"],[])
    def execution_eligible(self,job,active):
        """Independent component work bypasses unrelated blocked work; conflicting artifacts sequence."""
        try:
            scopes=self.job_scopes(job)
            if any(scopes&self.job_scopes(other)for other in active):return False
            def writes(value):
                return [s[5:]for s in self.store.get(value["ownership_id"])["scopes"]if s.startswith("path:")]if value.get("ownership_id")else []
            paths=writes(job)
            if any(ownership.overlap(a,b)for other in active for a in paths for b in writes(other)):return False
            if paths and all(p.startswith("ui/")for p in paths):
                peers=sum(bool(writes(other))and all(p.startswith("ui/")for p in writes(other))for other in active)
                if peers>=self.config.get("scheduler",{}).get("max_ui_preparations",1):return False
            if job.get("ownership_id"):self.assert_job(job)
            return True
        except (ValueError,PermissionError):
            job["waiting_reason"]="ownership recovery required";return False

    def resource_action(self,args,trusted,workspace):
        """Owners control their own jobs; the existing Builds administrator may inspect/control all."""
        if set(args)-{"action","job_id","phase_id","request_id","expected_revision","priority","memory_bytes"}:raise ValueError("Unsupported resource arguments")
        admin=trusted.get("local_caller")is True or(trusted["user_id"]=="admin"and workspace=="00000000-0000-0000-0000-000000000000")
        def find_job(job_id):
            """Recover only sealed index phase ownership, never a caller-supplied PID or path."""
            job=self.broker.jobs.get(job_id)
            if job is not None:return job
            with self.broker.resources.lock:
                row=next((r for r in self.broker.resources.rows.values()if r.get("job_id")==job_id and r.get("internal_index")and r.get("kind")=="index"),None)
                return {"job_id":job_id,"user_id":row["owner_user"],"workspace":row["owner_workspace"]}if row else None
        job=find_job(args.get("job_id"))
        if args["action"]=="resources":
            if args.get("job_id")and(not job or not(admin or self.broker._owned(job,trusted,workspace))):raise PermissionError("Resource job denied")
            if not args.get("job_id")and not admin:
                result=self.broker.resources.snapshot()
                result["phases"]={n:r for n,r in result["phases"].items()if (j:=find_job(r["job_id"]))and self.broker._owned(j,trusted,workspace)}
                result["profiles"]={};return result
            return self.broker.resources.snapshot(args.get("job_id"))
        if not job or not(admin or self.broker._owned(job,trusted,workspace)):raise PermissionError("Resource job denied")
        if args["action"]=="increase_memory":
            if not trusted.get("local_caller"):raise PermissionError("Memory controls require operator channel")
            self.broker._lease(job)
            if job.get("ownership_id"):self.assert_job(job)
        return self.broker.resources.control(job,args)
    def journal_path(self,owner,request,namespace):
        """Derive a private recovery record from the sealed caller and exact request."""
        if not isinstance(request,str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",request):raise ValueError("Stable request ID required")
        folder=self.broker.work_dir/"scheduler-requests";folder.mkdir(mode=0o700,exist_ok=True)
        return folder/(hashlib.sha256(ownership.canonical([owner,namespace,request]).encode()).hexdigest()+".json")
    @staticmethod
    def write_record(path,value):
        """Persist intent before filesystem effects and completion after durable store transitions."""
        temporary=path.with_suffix(".new")
        fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600)
        with os.fdopen(fd,"w")as stream:
            json.dump(value,stream,separators=(",",":"));stream.flush();os.fsync(stream.fileno())
        os.replace(temporary,path)
        fd=os.open(path.parent,os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)
    def pending(self,key,except_path=None):
        """Fence edits while a previous ownership transition needs exact-request recovery."""
        folder=self.broker.work_dir/"scheduler-requests"
        if not folder.exists():return None
        for path in sorted(folder.glob("*.json")):
            if path==except_path:continue
            value=json.loads(path.read_text())
            if value.get("task_key")==key and value["state"]=="pending":return value["request_id"]
        return None
    def handle(self,args,trusted,workspace):
        """Journal mutations so retries survive both ownership and filesystem transition crashes."""
        self.broker._incoming_lease(trusted)
        action=args.get("action")
        if action=="reconcile_acquisition":return self.reconcile_acquisition(args,trusted,workspace)
        if action=="create":
            if set(args)-{"action","task_id","summary"}:raise ValueError("Unsupported creation arguments")
            return self.broker.handle({"name":"v3_build_work","arguments":{"action":"begin","task_id":args.get("task_id"),"summary":args.get("summary","Scheduler followup"),"mode":"isolated"},"trusted":trusted,"workspace":workspace})
        if action in {"resources","pause","resume","priority","reconcile","increase_memory"}:
            return self.resource_action(args,trusted,workspace)
        allowed={"action","task_id","task_key","paths","request_id","expected_revision","expected_epoch","job_id","commit","acceptance","target_task_id","reason","offset","limit"}
        if set(args)-allowed or action not in self.actions:raise ValueError("Unsupported scheduler arguments")
        if action in {"reservations","takeover_preview"}:return self._handle(args,trusted,workspace)
        owner=self.owner(trusted,workspace)
        administrative=action=="takeover"and(trusted.get("local_caller")is True or trusted["user_id"]=="admin")
        item=self.item(args,trusted,workspace,administrative)
        path=self.journal_path(owner,args.get("request_id"),item["key"])
        body=ownership.canonical([owner,args]);digest=hashlib.sha256(body.encode()).hexdigest()
        if path.is_symlink():raise PermissionError("Scheduler request symlink")
        if path.exists():
            record=json.loads(path.read_text())
            if record["digest"]!=digest:raise ValueError("Request ID reused")
            if record["state"]=="done":return record["result"]
            if record["state"]=="rejected":raise ValueError(record["error"])
        else:
            admin=action=="takeover"and(trusted.get("local_caller")is True or trusted["user_id"]=="admin")
            item=self.item(args,trusted,workspace,admin);value=self.store.get(item["key"])if admin else self.ensure(item,trusted,workspace)
            if self.pending(item["key"]):raise ValueError("Previous scheduler request requires recovery")
            if type(args.get("expected_revision"))is not int or type(args.get("expected_epoch"))is not int:raise ValueError("Expected ownership revision and epoch required")
            if value["revision"]!=args["expected_revision"]or value["epoch"]!=args["expected_epoch"]:raise ValueError("Stale ownership revision or epoch")
            record={"digest":digest,"state":"pending","request_id":args["request_id"],"task_key":item["key"],"value":value,"previous_job":item.get("job_id")}
            self.write_record(path,record)
        # The original CAS state remains available even when the SQLite mutation already committed.
        try:result=self._handle(args,trusted,workspace,record,path)
        except (ValueError,PermissionError)as error:
            # SQLite transitions are atomic. Filesystem continuations retain pending intent once preparation starts.
            folder=self.broker.work_dir/"continuations"/record["task_key"]/path.stem
            transition="action-"+hashlib.sha256(ownership.canonical([owner,record["task_key"],args["request_id"]]).encode()).hexdigest()
            committed=self.store.receipt(transition)is not None
            prepared=action=="continue"and(folder/"baseline.tar").exists()
            if not committed and not prepared:
                self.write_record(path,{**record,"state":"rejected","error":str(error)[:512]})
            raise
        self.write_record(path,{**record,"state":"done","result":result});return result
    def finish_transfer(self,item,destination,result):
        """Fence the old draft and refresh an empty followup to the latest published generation."""
        target=result["task"]
        if destination.get("acquisition_revision")!=target["revision"]:
            destination["acquisition_refresh"]={"revision":target["revision"]};self.broker._save_work()
            self.refresh_waiter(destination,target)
        item["status"]="abandoned";self.broker._save_work();return result
    def continuation(self,item,args,value,record,path,owner,revision,epoch,request):
        """Recoverable generation switch; preserve the old tree and pin the new baseline once."""
        job=self.broker.jobs.get(record["previous_job"])
        if not job or job["status"]!="completed":raise ValueError("Previous generation must finish before correction applies")
        folder=self.broker.work_dir/"continuations"/item["key"]/path.stem
        folder.mkdir(parents=True,exist_ok=True)
        baseline=folder/"baseline.tar";prepared=folder/"prepared";tree=self.broker.work_dir/"trees"/item["key"]
        archive=self.broker.work_dir/"generations"/item["key"]/job["job_id"]
        if not baseline.exists():
            if item["status"]!="closed"or item.get("job_id")!=job["job_id"]:raise ValueError("Closed latest generation required")
            if tree.exists():
                previous=(Path(self.broker.state)/job["job_id"]/"task.tar").read_bytes()
                if self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))!=self.broker.scheduler_digest(previous):
                    raise ValueError("Retained source generation changed; reconcile before continuation")
            blob=self.broker.scheduler_preserve_inspection(self.broker._current_release_blob(),tree)
            temporary=baseline.with_suffix(".new");temporary.write_bytes(blob);os.replace(temporary,baseline)
        blob=baseline.read_bytes()
        fresh=tree.exists()and(tree/"baseline.tar").is_file()and(tree/"baseline.tar").read_bytes()==blob
        if not fresh:
            if not prepared.exists():
                temporary=folder/"preparing"
                if temporary.exists():
                    # Only this request's unpublished derived preparation can be replaced after interruption.
                    import shutil
                    shutil.rmtree(temporary)
                self.broker.scheduler_extract(temporary,blob);(temporary/"baseline.tar").write_bytes(blob)
                temporary.rename(prepared)
            if self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(prepared))!=self.broker.scheduler_digest(blob):
                raise ValueError("Prepared correction source changed")
            if tree.exists():
                if archive.exists():raise ValueError("Both original generation and archive exist; reconcile")
                archive.parent.mkdir(parents=True,exist_ok=True);tree.rename(archive)
            prepared.rename(tree)
        changed=self.store.edited(value["id"],owner,revision,epoch,request)
        if job["job_id"]not in item.setdefault("previous_jobs",[]):item["previous_jobs"].append(job["job_id"])
        item.pop("job_id",None);item.pop("inputs_sha256",None);item.pop("ownership_inputs_sha256",None)
        item.update(status="open",base_inputs_sha256=self.broker.scheduler_digest(blob),source_generation=changed["generation"],ownership_revision=changed["revision"])
        self.broker._save_work()
        return {"task_id":item["task_id"],"status":"open","workspace_path":item["workspace_path"],"ownership":changed}

    def _handle(self,args,trusted,workspace,record=None,journal_path=None):
        """CAS actions share the broker's transport and never accept arbitrary commands or cgroup paths."""
        action=args.get("action")
        allowed={"action","task_id","task_key","paths","request_id","expected_revision","expected_epoch","job_id","commit","acceptance","target_task_id","reason","offset","limit"}
        if set(args)-allowed or action not in self.actions:raise ValueError("Unsupported scheduler arguments")
        admin=trusted.get("local_caller")is True or trusted["user_id"]=="admin"
        if action=="reservations":return self.list(trusted,workspace,admin,args.get("offset",0),args.get("limit",20))
        takeover=action in {"takeover","takeover_preview"}
        if takeover and not admin:raise PermissionError("Administrator takeover required")
        item=self.item(args,trusted,workspace,takeover)
        if record is not None:value=record["value"]
        elif takeover:value=self.store.get(item["key"])
        else:value=self.ensure(item,trusted,workspace)
        owner=self.owner(trusted,workspace)
        if action=="takeover_preview":
            return {"task":{k:value[k]for k in ["id","label","state","epoch","revision","scopes"]},"jobs":[{"job_id":j["job_id"],"status":j["status"],"effects_started":bool(j.get("effects_started"))}for j in self.broker.jobs.values()if j.get("task_id")==item["task_id"]and j.get("user_id")==item["user_id"]and j.get("workspace")==item["workspace"]]}
        revision=args.get("expected_revision");epoch=args.get("expected_epoch")
        request="action-"+hashlib.sha256(ownership.canonical([owner,value["id"],args.get("request_id")]).encode()).hexdigest()
        if type(revision)is not int or type(epoch)is not int:raise ValueError("Expected ownership revision and epoch required")
        if action=="reserve":
            paths=args.get("paths")
            if not isinstance(paths,list)or not 1<=len(paths)<=128:raise ValueError("Bounded source paths required")
            self.require_clean_waiter(item,value)
            result=self.store.acquire(value["id"],owner,revision,epoch,self.components(item).scopes(paths),request)
            if result["acquired"]:self.mark_waiter_refresh(item,value,result["task"])
            result["workspace_inputs_sha256"]=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(Path(item["workspace_path"]).parent))
            return result
        if action in {"heartbeat","recover"}:return self.store.heartbeat(value["id"],owner,revision,epoch,request)
        if action not in {"continue","reserve","heartbeat","recover"}:
            replay=self.store.receipt(request)
            if replay is not None:
                if action in {"handoff","takeover"}:
                    destination=self.item({"task_id":args.get("target_task_id")},trusted,workspace)
                    return self.finish_transfer(item,destination,replay)
                return replay
        if action=="acceptance":
            evidence=self.verified(item,args)
            return self.store.validate(value["id"],owner,revision,epoch,evidence,request)
        if action=="complete":
            # Recheck promoted paths, health and commit again; recorded acceptance can go stale.
            evidence=value["evidence"]
            self.verified(item,{"job_id":evidence.get("release_id"),"commit":evidence.get("commit"),"acceptance":evidence.get("acceptance")})
            result=self.store.complete(value["id"],owner,revision,epoch,request);self.broker._start_next();return result
        if action=="continue":
            return self.continuation(item,args,value,record,journal_path,owner,revision,epoch,request)
        if action in {"handoff","takeover"}:
            jobs=[j for j in self.broker.jobs.values()if j.get("task_id")==item["task_id"]and j.get("user_id")==item["user_id"]and j.get("workspace")==item["workspace"]and j["status"]in {"queued","running","uncertain","needs_decision"}]
            if jobs:raise ValueError("Cancel safe jobs and reconcile effects before transferring ownership")
            destination=self.item({"task_id":args.get("target_task_id")},trusted,workspace)
            target=self.ensure(destination,trusted,workspace)
            if self.pending(destination["key"]):raise ValueError("Destination transition requires recovery")
            tree=Path(destination["workspace_path"]).parent
            baseline=self.broker.scheduler_entries((tree/"baseline.tar").read_bytes())
            current=self.broker.scheduler_entries(self.broker.scheduler_task_snapshot(tree))
            if baseline!=current:raise ValueError("Empty unchanged followup source required")
            result=self.store.transfer(value["id"],owner,revision,epoch,target["id"],target["owner"],request,administrator=takeover,reason=args.get("reason",""))
            return self.finish_transfer(item,destination,result)
        raise ValueError("Unsupported ownership action")
