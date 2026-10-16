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
                    blob=(self.broker.state/job["job_id"]/"candidate.tar").read_bytes()
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
