"""Protected host coordinator for settled-component indexing and project finalization."""
import concurrent.futures
import hashlib
import json
from pathlib import Path
import threading
import time
import sqlite3
import shutil
import uuid


# @code-index {"version":1,"feature":"project-index-coordinator","aliases":["project index coordinator"],"purpose":"Queue immutable settled-component evidence and finalize published project generations. Config: build-service/scheduler-defaults.json -> component_indexing.","span_lines":70,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/component_indexing"]}],"opportunities":[]}}
class IndexCoordinator:
    """Own queue execution; caller arguments never select source roots or executable code."""
    actions = {"index_project_create", "index_project_status", "index_project_attach", "index_checkpoint", "index_project_finalize", "index_project_reopen"}

    def __init__(self, broker, queue_module, release_module, validate, task_context, asset_root, source_path):
        """Bind this coordinator to the exact installed host and durable instance state."""
        self.broker = broker; self.queue_module = queue_module; self.release = release_module
        self.validate = validate; self.task_context = task_context; self.asset_root = asset_root; self.source_path = source_path
        self.queue = queue_module.IndexQueue(broker.work_dir / "project-index")
        self.queue.recover(); self.leases = {}; self.lock = threading.Lock()
        self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=1, thread_name_prefix="component-index")
        self.active = False; self.stopping = False

    def _source(self, item):
        """Drop inspection data; only allowlisted application source enters an index snapshot."""
        blob = self.broker.scheduler_task_snapshot(Path(item["workspace_path"]).parent)
        files, _inspection = self.release.candidate_entries(blob, self.validate)
        source = self.release.archive(files, self.validate)
        return source, files

    def _files(self, files, scope):
        """Bind a component's supported files without serializing source bodies."""
        supported = self.broker.code_index_ledger.scanner.SUFFIXES
        prefix = "v3/" + scope
        result = {p[3:]: hashlib.sha256(body[1]).hexdigest() for p, body in files.items()
                  if (p == prefix or p.startswith(prefix + "/")) and Path(p).suffix in supported and not p.startswith("v3/data/")}
        if len(result) > 512:
            raise ValueError("Checkpoint component exceeds 512 files; split components")
        return result

    def _authorize(self, checkpoint, lease, dependencies=()):
        """Epoch, selected file hashes and metadata dependencies remain authoritative."""
        with self.broker.lock:
            self.broker._lease(lease)
            item = self.broker.work.get(checkpoint["task_key"])
            if not item or item["status"] not in {"open", "closed"}:
                raise PermissionError("Checkpoint task is unavailable")
            state = self.broker.scheduler.store.get(item["key"])
            self.broker.scheduler.store.assert_owned(item["key"], state["owner"], checkpoint["ownership_epoch"], [])
            _blob, files = self._source(item)
            if self._files(files, checkpoint["component"]) != checkpoint["files"]:
                raise ValueError("Component changed; submit a new settled checkpoint")
            for dep in dependencies:
                path = "v3/" + dep["path"].removeprefix("v3/")
                value = files.get(path)
                if value is None or hashlib.sha256(value[1]).hexdigest() != dep["sha256"]:
                    raise ValueError("Component dependency changed; refresh its checkpoint")

    def handle(self, request, args):
        """Expose revisioned operator actions through the existing authenticated broker channel."""
        allowed = {"action", "project_id", "task_id", "component", "request_id", "expected_revision"}
        if set(args) - allowed or args.get("action") not in self.actions:
            raise ValueError("Unsupported project index arguments")
        trusted = request["trusted"]; workspace = request["workspace"]
        if not trusted.get("local_caller"):
            raise PermissionError("Project index coordination currently requires the operator channel")
        owner = self.broker.scheduler.owner(trusted, workspace)
        with self.broker.lock:
            self.broker._incoming_lease(trusted)
            if args["action"] == "index_project_status":
                return self.queue.status(owner, args.get("project_id"))
            if not self.broker._maintenance_allowed(trusted, args):
                raise PermissionError("Build service drained")
            item = None; checkpoint = None; lease = None
            if args["action"] in {"index_project_attach", "index_checkpoint"}:
                item = self.broker.scheduler.item(args, trusted, workspace)
                if item.get("mode") != "isolated":
                    raise ValueError("Isolated project task required")
            if args["action"] == "index_checkpoint":
                if not self.broker.scheduler.config.get("scheduler", {}).get("component_indexing", False):
                    raise ValueError("Component indexing is disabled by protected policy")
                if item["status"] != "open":
                    raise ValueError("Open task required for a settled checkpoint")
                scope = self.source_path(args.get("component"))
                state = self.broker.scheduler.ensure(item, trusted, workspace)
                self.broker.scheduler.store.assert_owned(item["key"], owner, item["ownership_epoch"], [])
                source, files = self._source(item)
                checkpoint = dict(task_key=item["key"], component=scope, source_sha256=self.validate(source)[0],
                                  ownership_epoch=state["epoch"], generation=state["generation"], files=self._files(files, scope), recipe="component-index-v1")
                snapshot = self.queue_module.identity(checkpoint); checkpoint["snapshot_id"] = snapshot
                folder = self.queue.folder / "snapshots" / snapshot
                if not folder.exists():
                    folder.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                    temporary = folder.with_name(snapshot + ".preparing-" + uuid.uuid4().hex)
                    self.release.extract(temporary, source, self.validate); temporary.rename(folder)
                elif self.validate(self.release.snapshot(folder,self.task_context,self.validate))[0] != checkpoint["source_sha256"]:
                    raise ValueError("Checkpoint snapshot changed")
                lease = dict(user_id=trusted["user_id"], workspace=workspace, lease_until=trusted["lease_until"], access_fingerprint=trusted.get("access_fingerprint"), revoked=False)
            result = self.queue.mutate(owner, args, task=item, checkpoint=checkpoint)
            if checkpoint:
                lease.update(job_id=str(uuid.UUID(result["latest_checkpoint"][:32])),internal_index=True,checkpoint_key=result["latest_checkpoint"])
                self.leases[result["latest_checkpoint"]] = lease
        if checkpoint:
            self.kick()
        if args["action"] == "index_project_finalize":
            return self.finalize(owner, args["project_id"])
        return result

    def update_leases(self,trusted,workspace,incoming):
        """Apply the host's session-scoped revocation to queued and running checkpoints."""
        updated=0
        for lease in list(self.leases.values()):
            if lease["user_id"]!=trusted["user_id"]or lease["workspace"]!=workspace:continue
            if incoming and not lease["revoked"]and lease["lease_until"]>time.time()and lease["access_fingerprint"]==incoming[1]:lease["lease_until"]=incoming[0]
            else:lease["revoked"]=True
            updated+=1
        return updated

    def kick(self):
        """Wake one bounded worker only when new ready work exists."""
        with self.lock:
            if not self.active and not self.stopping:
                self.active = True; self.pool.submit(self.run)

    def run(self):
        """Scan immutable private snapshots; do not hold the publication or queue transaction lock."""
        try:
            while not self.stopping:
                checkpoint = self.queue.take()
                if checkpoint is None:
                    break
                try:
                    lease = self.leases.get(checkpoint["key"])
                    if lease is None:
                        raise PermissionError("Checkpoint needs fresh caller authorization")
                    self._authorize(checkpoint, lease)
                    root = self.queue.folder / "snapshots" / checkpoint["snapshot_id"] / "v3"
                    output = self.queue.folder / "outputs" / checkpoint["key"]
                    output.mkdir(mode=0o700,parents=True,exist_ok=True)
                    for previous in self.queue.prepared(project=checkpoint["project"]):
                        prior=self.queue.folder/"outputs"/previous["key"]
                        index=prior/"index.sqlite";receipt=prior/"seed.json"
                        if index.is_symlink()or receipt.is_symlink():raise ValueError("Checkpoint seed symlink")
                        if index.is_file()and receipt.is_file()and hashlib.sha256(index.read_bytes()).hexdigest()==previous["result"]["scan"]["index_sha256"]:
                            shutil.copyfile(index,output/"index.sqlite");shutil.copyfile(receipt,output/"seed.json");break
                    context=getattr(self.broker.runner,"job_context",None)
                    previous=getattr(context,"job",None)if context is not None else None
                    if context is not None:context.job=lease
                    try:prepared = self.broker.code_index_ledger.inventory(root, checkpoint["component"], output)
                    finally:
                        if context is not None:context.job=previous
                    files, units, scan = prepared
                    dependencies = [dep for file in files for dep in file["dependencies"]]
                    authorize = lambda: self._authorize(checkpoint, lease, dependencies)
                    authorize()
                    engine = self.broker.code_index_ledger.__class__
                    ledger = engine(self.broker.code_index_ledger.folder, scanner=self.broker.code_index_ledger.scanner,
                                    inventory=lambda *_args: prepared, max_bytes=self.broker.code_index_ledger.max_bytes)
                    actor = dict(user_id=lease["user_id"], session_id=lease["workspace"], transport="operator")
                    task = self.broker.work[checkpoint["task_key"]]
                    ledger.mutate(dict(action="sync", scope=checkpoint["component"], request_id="checkpoint-sync-" + checkpoint["key"][:48]),
                                  actor, task["key"], task["task_id"], root, authorize)
                    authorize()
                    self.queue.finish(checkpoint["key"], result={"scan":scan,"dependencies":dependencies,"file_count":len(files)})
                except Exception as error:
                    self.queue.finish(checkpoint["key"], reason=type(error).__name__+": checkpoint needs review or fresh authorization")
                finally:
                    self.leases.pop(checkpoint["key"], None)
        finally:
            with self.lock:
                self.active = False
                if not self.stopping and self.queue.has_ready():
                    self.active = True; self.pool.submit(self.run)

    def finalize(self, owner, project_id):
        """Seal completed task lineage against the active immutable source/index pair."""
        project = self.queue.status(owner, project_id)
        if project["state"] != "finalizing":
            return project
        reasons = []; expected = {}; members = []; checkpoint_reconciliation={}
        with self.broker.deploy_lock:
            current = self.broker.runner.current_asset_id()if hasattr(self.broker.runner,"current_asset_id")else None
            if not current or not(self.asset_root/"releases"/current/".code-index/index.sqlite").is_file():
                return self.queue.finalized(owner,project_id,project["revision"],reasons=["A verified published index is required before project finalization"])
            folder = self.asset_root / "releases" / current
            scanner = self.broker.code_index_ledger.scanner
            _index, manifest = scanner.read_published(folder, current)
            for key in project["members"]:
                task = self.broker.work.get(key); state = self.broker.scheduler.store.get(key)
                job = self.broker.jobs.get(task.get("job_id")) if task else None
                if not task or state["state"] != "complete" or not job or job["status"] != "completed":
                    reasons.append("Complete publication, acceptance and commit for " + project["members"][key]); continue
                members.append(job)
            for job in sorted(members,key=lambda j:j.get("finished_at",0)):
                files = self.release.entries((self.broker.state/job["job_id"]/"candidate.tar").read_bytes(),self.validate)
                for path in job.get("changed_paths",[]):
                    expected[path] = files.get(path)
            active = self.release.entries(self.broker._current_release_blob(),self.validate)
            # Only the newest checkpoint of each task/component can represent settled work.
            latest={}
            for checkpoint in project["checkpoints"]:
                latest[(checkpoint["task_key"],checkpoint["component"])]=checkpoint
            for key,label in project["members"].items():
                task=self.broker.work.get(key,{})
                job=self.broker.jobs.get(task.get("job_id"),{})
                for path in job.get("changed_paths",[]):
                    logical=path.removeprefix("v3/")
                    if Path(logical).suffix not in scanner.SUFFIXES:continue
                    matching=[item for(task_key,scope),item in latest.items()if task_key==key and(logical==scope or logical.startswith(scope+"/"))]
                    if not matching:
                        checkpoint_reconciliation[key+":"+logical]="verified_in_final_publication";continue
                    checkpoint=max(matching,key=lambda item:item["created_at"])
                    if checkpoint["state"]in {"queued","running"}:
                        reasons.append("Finish component checkpoint: "+checkpoint["component"]);continue
                    valid=checkpoint["state"]=="prepared"and self._files(active,checkpoint["component"])==checkpoint["files"]
                    for dep in(checkpoint.get("result")or{}).get("dependencies",[]):
                        value=active.get("v3/"+dep["path"].removeprefix("v3/"))
                        valid=valid and value is not None and hashlib.sha256(value[1]).hexdigest()==dep["sha256"]
                    # Integration may supersede private evidence. The exact sealed index below
                    # is the final authority; completed tasks never need their claims resurrected.
                    checkpoint_reconciliation[checkpoint["key"]]="retained"if valid else"verified_in_final_publication"
            for path,value in expected.items():
                if active.get(path)!=value:
                    reasons.append("Integration changed intended source: "+path)
            connection=sqlite3.connect(":memory:")
            try:
                connection.deserialize(_index)
                rows=[json.loads(row[0])for row in connection.execute("SELECT body FROM maintenance_records")]
            finally:connection.close()
            for path in expected:
                logical=path.removeprefix("v3/")
                if Path(logical).suffix not in scanner.SUFFIXES:continue
                applicable=[row for row in rows if row["file_path"].removeprefix("v3/")==logical]
                if not applicable or any(row["status"]!="complete"for row in applicable):
                    reasons.append("Finish applicable reviews: "+logical)
            if any(manifest[k]for k in ("gaps","stale_count","unindexed_count")):
                reasons.append("Published index is not fresh")
            receipt=dict(release_id=current,source_sha256=manifest["source_sha256"],index_sha256=manifest["index_sha256"],generation=project["generation"],checkpoint_reconciliation={state:list(checkpoint_reconciliation.values()).count(state)for state in {"retained","verified_in_final_publication"}})
            return self.queue.finalized(owner,project_id,project["revision"],receipt=receipt,reasons=reasons)

    def publication_seeds(self,job_id):
        """Select private seeds from this caller's explicitly linked project tasks."""
        job=self.broker.jobs.get(job_id,{})
        keys=[key for key,item in self.broker.work.items()if item.get("job_id")in {job_id,*job.get("batch_members",[])}]
        if not keys:return []
        owner=self.broker.scheduler.owner(job,job["workspace"])
        own=[self.broker.code_index_ledger.folder/"task-indexes"/self.queue_module.identity(key)for key in keys]
        own=[str(path)for path in own if(path/"seed.json").is_file()and(path/"index.sqlite").is_file()]
        return (own+[str(self.queue.folder/"outputs"/item["key"])for item in self.queue.prepared(owner=owner,task_keys=keys)])[:64]

    def close(self):
        """Stop admission; unfinished checkpoints retain explicit recovery state."""
        self.stopping = True
        for lease in self.leases.values():lease["revoked"]=True
        self.pool.shutdown(wait=True)
