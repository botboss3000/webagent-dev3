"""Agent-requested exact-source metadata follow-up; no background indexing."""
import hashlib
import json
from pathlib import Path
import threading
import time


# @code-index {"version":1,"feature":"post-deployment-maintenance","aliases":["deploy before indexing","agent-requested release index"],"purpose":"Record exact-source follow-up after deployment; execute indexing only on explicit agent request, and retain source-bound review completion. Config: build-service/scheduler-defaults.json -> post_deployment_indexing and documentation_review_order.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/post_deployment_indexing","/documentation_review_order"]}]}}
class PostDeployment:
    """Track deferred work; indexing runs synchronously only when an agent requests it."""

    def __init__(self, broker, release, assets, validate):
        self.broker=broker;self.release=release;self.assets=Path(assets);self.validate=validate
        self.guard=threading.Lock();self.active=False;self.stopping=False

    def enqueue(self, job):
        """Persist follow-up intent only for a completed immutable source release."""
        folder=self.assets/"releases"/job["job_id"]
        if job["status"]!="completed"or not(folder/".code-index/source-release.json").is_file():return
        with self.broker.lock:
            state=job.setdefault("post_deployment",{"index_status":"queued","attempts":0,"queued_at":time.time()})
            if state["index_status"]=="awaiting_deployment":state.update(index_status="queued",queued_at=time.time())
            if state["index_status"] not in {"queued","running"}:return
            self.broker._save(job)
        self.start()

    def resume(self):
        """Recover only metadata intent; never repeat deployment or source promotion."""
        with self.broker.lock:
            for job in self.broker.jobs.values():
                state=job.get("post_deployment",{})
                if job.get("status")=="completed"and state.get("index_status")in {"running","awaiting_deployment"}:
                    state["index_status"]="queued";self.broker._save(job)
                if job.get("status")=="completed"and not state and (self.assets/"releases"/job["job_id"]/".code-index/source-release.json").is_file():
                    job["post_deployment"]={"index_status":"queued","attempts":0,"queued_at":time.time()}
                    self.broker._save(job)
        self.start()

    def start(self):
        """No automatic work: an agent must explicitly request exact-source indexing."""
        return

    def authorize(self, job):
        """Keep source identity, generation and delegated live authority exact."""
        self.broker._lease(job)
        self.broker.scheduler.assert_job(job)
        state=self.broker.scheduler.store.get(job["ownership_id"])
        if state["generation"]!=job["source_generation"]:raise PermissionError("Post-deployment source generation changed")
        self.broker.scheduler.store.touch(job["ownership_id"],state["owner"],job["ownership_epoch"])

    def run(self, job_id):
        """Construct and attach one verified index; failures leave the app deployed."""
        with self.guard:
            if self.active or self.stopping:raise ValueError("Explicit metadata operation already active or stopping")
            self.active=True
        try:
            while not self.stopping:
                with self.broker.lock:
                    if self.broker._maintenance()["drained"]:break
                    pending=sorted((j for j in self.broker.jobs.values()if j.get("job_id")==job_id and j.get("status")=="completed"and j.get("post_deployment",{}).get("index_status")=="queued"),key=lambda j:j["finished_at"])
                    if not pending:break
                    job=pending[0];state=job["post_deployment"]
                    state.update(index_status="running",started_at=time.time(),attempts=state.get("attempts",0)+1)
                    self.broker._save(job)
                context=getattr(self.broker.runner,"job_context",None)
                try:
                    self.authorize(job)
                    folder=self.assets/"releases"/job["job_id"]
                    self.release.verify_publication(folder,job["job_id"],job["inputs_sha256"])
                    blob=self.broker.post_deployment_candidate(job)
                    if self.validate(blob)[0]!=job["inputs_sha256"]:raise ValueError("Post-deployment source archive changed")
                    if context is not None:context.job=job;context.event=threading.Event()
                    self.broker.runner.index_assets(folder,blob,job["job_id"],base_release_id=job.get("asset_previous"))
                    self.authorize(job)
                    manifest=self.release.verify_index(folder,job["job_id"],job["inputs_sha256"])
                    if any(manifest[k]for k in ("gaps","stale_count","unindexed_count")):raise ValueError("Post-deployment index is not complete and fresh")
                    # Reconcile only the currently active release. A late old index
                    # remains usable historical evidence without replacing a newer one.
                    self.broker._code_index_reconcile()
                    with self.broker.lock:
                        state.update(index_status="indexed",finished_at=time.time(),index_sha256=manifest["index_sha256"],source_sha256=manifest["source_sha256"])
                        state.pop("error",None);self.broker._save(job)
                    self.broker.review_policy.complete_if_ready(job)
                except Exception as error:
                    with self.broker.lock:
                        state.update(index_status="queued"if self.stopping else"failed",finished_at=time.time(),error=type(error).__name__+": metadata follow-up needs retry; inspect protected phase receipts")
                        self.broker._save(job)
                finally:
                    if context is not None:context.job=None;context.event=None
        finally:
            with self.guard:self.active=False

    def retry(self, request, args):
        """Requeue failed metadata with an exact receipt; never redeploy the application."""
        if set(args)!={"action","task_id","job_id","request_id"}:raise ValueError("Exact metadata retry arguments required")
        trusted=request["trusted"];workspace=request["workspace"]
        with self.broker.lock:
            deadline,fingerprint=self.broker._incoming_lease(trusted)
            item=self.broker.scheduler.item(args,trusted,workspace)
            job=self.broker.jobs.get(args["job_id"])
            if not job or item.get("job_id")!=job["job_id"]or not self.broker._owned(job,trusted,workspace)or job["status"]!="completed"or not job.get("post_deployment")or job.get("revoked")or job["access_fingerprint"]!=fingerprint:raise PermissionError("Owned authorized completed release required")
            self.broker.scheduler.assert_job(job)
            owner=self.broker.scheduler.owner(trusted,workspace)
            name=self.broker.scheduler.journal_path(owner,args["request_id"],"post-deployment").name
            folder=self.broker.work_dir/"post-deployment-requests"
            if folder.is_symlink():raise ValueError("Metadata receipt directory symlink")
            folder.mkdir(mode=0o700,exist_ok=True);path=folder/name
            digest=hashlib.sha256(json.dumps(args,sort_keys=True,separators=(",",":")).encode()).hexdigest()
            if path.exists():
                previous=json.loads(path.read_bytes())
                if previous["digest"]!=digest:raise ValueError("Metadata retry request_id reused")
                return previous["result"]
            if job["post_deployment"]["index_status"]=="running":raise ValueError("Metadata job already running")
            job["lease_until"]=deadline
            if job["post_deployment"]["index_status"]!="indexed":
                job["post_deployment"].update(index_status="queued",queued_at=time.time());self.broker._save(job)
            result={"job_id":job["job_id"],"index_status":job["post_deployment"]["index_status"],"deployment_replayed":False}
            self.broker.scheduler.write_record(path,{"digest":digest,"state":"done","result":result})
        self.run(job["job_id"])
        return result

    def close(self):
        """Stop metadata admission before closing resource control and deployment executors."""
        with self.guard:self.stopping=True

class MaintenanceActivities:
    """Explicit release-bound maintenance; no original task lease or automatic indexing."""
    actions={"maintenance_begin","maintenance_status","maintenance_submit","maintenance_receipt","maintenance_cancel","maintenance_close","maintenance_resume"}
    def __init__(self, broker, release, assets):
        import concurrent.futures
        self.broker=broker;self.release=release;self.assets=Path(assets)
        self.folder=broker.work_dir/"maintenance-activities"
        if self.folder.is_symlink():raise ValueError("Maintenance activity directory symlink")
        self.folder.mkdir(mode=0o700,exist_ok=True)
        self.lock=threading.RLock();self.pool=concurrent.futures.ThreadPoolExecutor(max_workers=1)
        self.active={};self.stopping=False
        for path in self.folder.glob("*.json"):
            row=self.read(path)
            for operation in row["operations"].values():
                if operation["status"] in {"queued","running"}:
                    operation.update(status="interrupted",reason="Broker restarted; explicitly resubmit the identical request")
            self.save(row)
    def read(self,path):
        import os,stat
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
        try:
            info=os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_nlink!=1 or info.st_size>8*1024*1024:raise ValueError("Invalid maintenance receipt")
            with os.fdopen(fd,"rb",closefd=False) as stream:return json.load(stream)
        finally:os.close(fd)
    def save(self,row):
        if len(json.dumps(row,separators=(",",":")).encode())>7*1024*1024:raise ValueError("Maintenance activity storage bound reached")
        self.broker.scheduler.write_record(self.folder/(row["activity_id"]+".json"),row)
    def load(self,identity,trusted):
        import re
        if not isinstance(identity,str) or not re.fullmatch("[a-f0-9]{64}",identity):raise ValueError("Exact maintenance activity ID required")
        row=self.read(self.folder/(identity+".json"))
        if row["user_id"]!=trusted["user_id"]:raise PermissionError("Maintenance activity owner required")
        return row
    def view(self,row):
        return {k:row[k] for k in ("activity_id","release_id","source_sha256","inputs_sha256","scope","status","created_at")} | {"operations":{key:{k:v for k,v in value.items() if k not in {"request","result"}} for key,value in row["operations"].items()}}
    def binding(self,row):
        job=self.broker.jobs.get(row["release_id"])
        if not job or job["status"]!="completed" or job["inputs_sha256"]!=row["inputs_sha256"]:raise ValueError("Completed immutable release changed")
        root=self.assets/"releases"/row["release_id"]
        manifest=self.release.verify_publication(root,row["release_id"],row["inputs_sha256"])
        if manifest["source_sha256"]!=row["source_sha256"]:raise ValueError("Maintenance source binding changed")
        return job,root
    def handle(self,request,args):
        import uuid,re
        trusted=request["trusted"];workspace=request["workspace"]
        incoming=self.broker._incoming_lease(trusted)
        self.update_leases(trusted,workspace,incoming)
        action=args.get("action")
        allowed={"action","activity_id","release_id","request_id","operation","operation_id"}
        if action not in self.actions or set(args)-allowed:raise ValueError("Unsupported maintenance activity arguments")
        if action=="maintenance_begin":
            identity=args.get("release_id");job=self.broker.jobs.get(identity)
            if not job or job["status"]!="completed" or (not trusted.get("local_caller") and job["user_id"]!=trusted["user_id"]):raise PermissionError("Authorized completed release required")
            root=self.assets/"releases"/identity
            manifest=self.release.verify_publication(root,identity,job["inputs_sha256"])
            key=hashlib.sha256(json.dumps([trusted["user_id"],identity],separators=(",",":")).encode()).hexdigest()
            with self.lock:
                path=self.folder/(key+".json")
                if path.exists():return self.view(self.load(key,trusted))
                if len(list(self.folder.glob("*.json")))>=1000:raise ValueError("Maintenance activity registry full")
                scope=sorted({p.removeprefix("v3/") for p in job.get("changed_paths",[]) if p.startswith("v3/")})
                row=dict(activity_id=key,release_id=identity,user_id=trusted["user_id"],actor_workspace=workspace,source_sha256=manifest["source_sha256"],inputs_sha256=job["inputs_sha256"],scope=scope,status="open",created_at=time.time(),operations={})
                self.save(row);return self.view(row)
        with self.lock:
            row=self.load(args.get("activity_id"),trusted);key=row["activity_id"]
            if action=="maintenance_status":return self.view(row)
            if action=="maintenance_receipt":
                operation_id=args.get("operation_id")
                if operation_id is None and isinstance(args.get("request_id"),str):operation_id=hashlib.sha256((key+args["request_id"]).encode()).hexdigest()
                operation=row["operations"].get(operation_id)
                if operation is None:raise ValueError("Maintenance operation unavailable")
                return dict(operation)
            if action=="maintenance_resume":
                if any(lease["activity_id"]==key for lease in self.active.values()):raise ValueError("Wait for active maintenance to stop before resuming")
                self.binding(row)
                row["status"]="open";self.save(row);return self.view(row)
            if action in {"maintenance_cancel","maintenance_close"}:
                for operation_id,lease in self.active.items():
                    if lease["activity_id"]==key:lease["revoked"]=True
                row["status"]="cancelled" if action=="maintenance_cancel" else "closed"
                self.save(row);return self.view(row)
            if self.stopping or row["status"]!="open":raise ValueError("Open maintenance activity required")
            if self.broker._maintenance()["drained"]:raise ValueError("Maintenance drain prevents new operations")
            operation=args.get("operation");request_id=args.get("request_id")
            if not isinstance(request_id,str) or not re.fullmatch("[A-Za-z0-9_-]{8,80}",request_id) or not isinstance(operation,dict):raise ValueError("Bounded operation and stable request ID required")
            if len(json.dumps(operation).encode())>1024*1024:raise ValueError("Maintenance operation exceeds bound")
            operation_id=hashlib.sha256((key+request_id).encode()).hexdigest()
            digest=hashlib.sha256(json.dumps(operation,sort_keys=True,separators=(",",":")).encode()).hexdigest()
            old=row["operations"].get(operation_id)
            if old and old["digest"]!=digest:raise ValueError("Maintenance request ID reused")
            if old and old["status"] not in {"interrupted"}:return {k:v for k,v in old.items() if k not in {"request","result"}}
            if len(row["operations"])>=4096 and not old:raise ValueError("Maintenance operation registry full")
            if any(lease["activity_id"]==key for lease in self.active.values()):raise ValueError("Maintenance operation already active; poll its receipt")
            if len(self.active)>=16:raise ValueError("Maintenance admission queue full")
            if operation.get("action") not in {"index","sync","claim","record","release","list","get"}:raise ValueError("Unsupported maintenance operation")
            row["operations"][operation_id]=dict(operation_id=operation_id,request_id=request_id,digest=digest,request=operation,status="queued",progress=0,created_at=time.time())
            lease=dict(user_id=trusted["user_id"],workspace=workspace,lease_until=trusted["lease_until"],access_fingerprint=trusted["access_fingerprint"],revoked=False,activity_id=key,job_id=str(uuid.uuid4()),internal_index=True)
            self.save(row);self.active[operation_id]=lease
            self.pool.submit(self.run,key,operation_id,lease)
            return {k:v for k,v in row["operations"][operation_id].items() if k not in {"request","result"}}
    def authorize(self,key,lease):
        # Touch only this activity. Never extend caller authorization or implementation ownership.
        self.broker._lease(lease)
        if self.stopping:raise PermissionError("Maintenance worker stopping")
        with self.lock:
            row=self.load(key,{"user_id":lease["user_id"]})
            if row["status"]!="open":raise PermissionError("Maintenance activity ended")
    def progress(self,key,operation_id,**changes):
        with self.lock:
            row=self.read(self.folder/(key+".json"))
            row["operations"][operation_id].update(**changes,updated_at=time.time())
            self.save(row)
    def run(self,key,operation_id,lease):
        context=getattr(self.broker.runner,"job_context",None)
        try:
            self.authorize(key,lease)
            with self.lock:row=self.read(self.folder/(key+".json"))
            operation=row["operations"][operation_id];args=operation["request"]
            self.progress(key,operation_id,status="running",phase="verifying_source",started_at=time.time())
            start=time.monotonic();job,root=self.binding(row)
            self.progress(key,operation_id,binding_seconds=time.monotonic()-start)
            if context is not None:context.job=lease;context.event=threading.Event()
            action=args.get("action");start=time.monotonic()
            if action=="index":
                if set(args)!={"action"}:raise ValueError("Index operation accepts no source overrides")
                self.progress(key,operation_id,phase="indexing")
                blob=self.broker.post_deployment_candidate(job)
                if self.broker.scheduler_digest(blob)!=row["inputs_sha256"]:raise ValueError("Maintenance candidate changed")
                self.broker.runner.index_assets(root,blob,row["release_id"],base_release_id=job.get("asset_previous"))
                self.authorize(key,lease)
                manifest=self.release.verify_index(root,row["release_id"],row["inputs_sha256"])
                if any(manifest[k] for k in ("gaps","stale_count","unindexed_count")):raise ValueError("Incomplete or stale maintenance index")
                result=dict(index_sha256=manifest["index_sha256"],source_sha256=manifest["source_sha256"])
                with self.broker.lock:
                    job.setdefault("post_deployment",{}).update(index_status="indexed",index_sha256=manifest["index_sha256"],source_sha256=manifest["source_sha256"],finished_at=time.time(),maintenance_activity_id=key)
                    self.broker._save(job)
            else:
                if set(args)&{"task_id","task_key","root","prepared_job_id","deployed_job_id","request_id"}:raise ValueError("Maintenance context is host-owned")
                actor={"user_id":row["user_id"],"session_id":row["actor_workspace"],"transport":"maintenance"}
                scopes=[args["scope"]] if args.get("scope") else row["scope"]
                if action=="sync":
                    if not set(scopes)<=set(row["scope"]):raise ValueError("Sync scope must be exact released paths")
                    self.progress(key,operation_id,phase="syncing_released_files",total=len(scopes))
                    for number,scope in enumerate(scopes):
                        self.authorize(key,lease)
                        payload={**args,"scope":scope,"request_id":"maintenance-"+operation_id[:40]+"-"+str(number)}
                        result=self.broker.code_index_ledger.mutate(payload,actor,key,"maintenance-"+key[:32],root/"v3",authorize=lambda:self.authorize(key,lease))
                        self.progress(key,operation_id,progress=number+1)
                    result={"synced_paths":scopes}
                elif action in {"list","get"}:
                    self.progress(key,operation_id,phase="reading_reviews")
                    result=self.broker.code_index_ledger.list({**args,"task_key":key})
                else:
                    self.progress(key,operation_id,phase="recording_review")
                    result=self.broker.code_index_ledger.mutate({**args,"request_id":"maintenance-"+operation_id[:48]},actor,key,"maintenance-"+key[:32],root/"v3",authorize=lambda:self.authorize(key,lease))
            self.authorize(key,lease)
            self.progress(key,operation_id,status="completed",phase="finished",result=result,work_seconds=time.monotonic()-start,finished_at=time.time())
        except Exception as error:
            # The ledger's exact request receipt remains the recovery authority.
            self.progress(key,operation_id,status="interrupted",phase="stopped",reason=type(error).__name__+": "+str(error)[:240],finished_at=time.time())
        finally:
            if context is not None:context.job=None;context.event=None
            with self.lock:self.active.pop(operation_id,None)
    def update_leases(self,trusted,workspace,incoming):
        with self.lock:
            matches=[lease for lease in self.active.values() if lease["user_id"]==trusted["user_id"] and lease["workspace"]==workspace]
            for lease in matches:
                if incoming and not lease["revoked"] and lease["lease_until"]>time.time() and lease["access_fingerprint"]==incoming[1]:lease["lease_until"]=incoming[0]
                else:lease["revoked"]=True
            return len(matches)
    def evidence_keys(self):
        with self.lock:
            return [self.read(p)["activity_id"] for p in self.folder.glob("*.json")]

    def close(self):
        self.stopping=True
        with self.lock:
            for lease in self.active.values():lease["revoked"]=True
        self.pool.shutdown(wait=True)
