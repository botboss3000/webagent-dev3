"""Prepare immutable UI candidates concurrently, serializing only admitted publication effects."""
import json
import os
from pathlib import Path
import sqlite3
import time
import uuid


# @code-index {"version":1,"feature":"concurrent-ui-publication","aliases":["concurrent ui publication"],"purpose":"Prepare private candidates and serialize exact source/index publication. Config: build-service/scheduler-defaults.json -> concurrent_ui_preparation, max_ui_preparations.","span_lines":70,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/concurrent_ui_preparation","/max_ui_preparations"]}],"opportunities":[]}}
class UIPublication:
    """Keep stale preparation outside publication; host authorization owns every effect."""
    def __init__(self,broker,release,validate,context,asset_root,policy):
        """Bind preparation and recovery to this protected host's fixed release authority."""
        self.broker=broker;self.release=release;self.validate=validate
        self.context=context;self.assets=asset_root;self.policy=policy

    def enabled(self,job):
        """Existing queued generations retain their admitted release contract."""
        return bool(job.get("isolated")and job.get("publication_policy_version")==1 and self.broker.scheduler.config.get("scheduler",{}).get("concurrent_ui_preparation",False))

    def runtime(self):
        """Capture retained consumer identities without serializing credentials or configuration."""
        return {role:{"id":value["Id"],"image":value["Image"],"pid":value.get("State",{}).get("Pid")}for role,name in zip(("api","loop"),self.broker.publication_targets)for value in [self.broker.runner.inspect(name)]}

    def reviews(self,folder,changed):
        """Require current applicable reviews in the exact sealed candidate index."""
        scanner=self.broker.code_index_ledger.scanner
        blob,manifest=scanner.read_published(folder,folder.name)
        conn=sqlite3.connect(":memory:")
        try:
            conn.deserialize(blob)
            rows=[json.loads(row[0])for row in conn.execute("SELECT body FROM maintenance_records")]
        finally:conn.close()
        pending=[]
        for path in changed:
            if Path(path).suffix not in scanner.SUFFIXES:continue
            selected=[row for row in rows if row["file_path"]==path]
            if not selected or any(row["status"]!="complete"for row in selected):pending.append(path)
        return manifest,pending

    def admit(self,job,blob,tested_runtime=False):
        """Return with publication and boot-effect locks held only for a current prepared candidate."""
        clock=time.monotonic;started=clock();job.setdefault("preparation",{})
        for attempt in range(1,4):
            with self.broker.deploy_lock:
                with self.broker.lock:self.broker._batch_lease(job)
                if self.broker._has_unresolved_release(job["job_id"]):raise ValueError("Reconcile uncertain publication first")
                live=self.broker._current_release_blob();base=self.validate(live)[0]
                revision=self.broker.runner.current_asset_id();runtime=self.runtime()
            blob=self.broker._rebase_after_ui(job,blob,tested_runtime=tested_runtime,live_blob=live)
            if self.validate(self.release.snapshot(Path(job["release_root"]),self.context,self.validate))[0]!=job["inputs_sha256"]:
                raise ValueError("Preparation source changed")
            self.broker._phase(job,"preparing_ui")
            folder=self.assets/"preparation-attempts"/job["job_id"]/str(uuid.uuid4())
            index_started=clock()
            destination=self.broker.runner.stage_assets(blob,job["job_id"],asset_root=folder,base_release_id=revision)
            indexed=clock()-index_started
            validation_started=clock()
            try:
                self.broker.runner.validate_ui(job["job_id"],job.get("changed_paths",()),root=destination)
            except Exception:
                # A coordinated backend switch may temporarily remove a consumer.
                # Retry only when the exact captured publication/runtime binding changed.
                with self.broker.deploy_lock:
                    stale=(self.broker.runner.current_asset_id()!=revision or
                           self.validate(self.broker._current_release_blob())[0]!=base or self.runtime()!=runtime)
                if stale:continue
                raise
            validated=clock()-validation_started
            manifest,pending=self.reviews(destination,job.get("changed_paths",()))
            evidence=self.policy.dependencies(self.release.entries(blob,self.validate),job.get("changed_paths",()))
            evidence["runtime_images"]=dict(job.get("images",{}))if tested_runtime else{role:identity["image"]for role,identity in runtime.items()}
            test_started=clock()
            test_result=self.broker.runner.validate_selected_tests(job["job_id"],destination,evidence)
            with self.broker.lock:
                job["ui_test_evidence"]=test_result
                job["preparation"]["frontend_tests_seconds"]=clock()-test_started
                job["preparation"].update(attempts=attempt,indexing_seconds=indexed,validation_seconds=validated,
                    total_seconds=clock()-started,parsed_files=manifest.get("indexing",{}).get("parsed_files"),
                    reused_files=manifest.get("indexing",{}).get("reused_files"),dependency_sha256=evidence["sha256"],
                    conservative_validation=evidence["conservative"],required_tests=evidence["tests"])
                job["prepared_assets"]=str(destination);self.broker._save(job)
            if pending:
                with self.broker.lock:
                    job.update(status="needs_decision",phase="awaiting_prepared_reviews",reason="Complete applicable reviews against the sealed prepared candidate before resuming")
                    job["preparation"]["pending_review_paths"]=pending[:32]
                    if not self.broker.scheduler.review_wait(job,reset=True):raise PermissionError("Prepared review delegation unavailable")
                    self.broker._save(job)
                return None
            wait_started=clock();self.broker.deploy_lock.acquire();guard=None;held=True
            try:
                guard=self.broker.boot_effect_lease();guard.__enter__()
                if self.broker.runner.current_asset_id()!=revision or self.validate(self.broker._current_release_blob())[0]!=base or self.runtime()!=runtime:
                    closing=guard;guard=None;held=False
                    self.release_locks(closing);continue
                with self.broker.lock:self.broker._batch_lease(job)
                if self.broker._has_unresolved_release(job["job_id"]):raise ValueError("Reconcile uncertain publication first")
                if not self.broker._sync_job(job,live,blob,job.get("changed_paths",())):
                    closing=guard;guard=None;held=False
                    self.release_locks(closing);return None
                if self.validate(self.release.snapshot(Path(job["release_root"]),self.context,self.validate))[0]!=job["inputs_sha256"]:
                    raise ValueError("Frozen preparation source changed")
                self.release.verify_index(destination,job["job_id"],job["inputs_sha256"])
                before=self.broker.state/job["job_id"]/"publication-base.tar"
                if before.is_symlink():raise ValueError("Publication baseline symlink")
                if before.exists()and before.read_bytes()!=live:raise ValueError("Prepared publication intent needs reconciliation")
                if not before.exists():
                    fd=os.open(before,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
                    with os.fdopen(fd,"wb")as stream:stream.write(live);stream.flush();os.fsync(stream.fileno())
                    fd=os.open(before.parent,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
                    try:os.fsync(fd)
                    finally:os.close(fd)
                with self.broker.lock:
                    job["asset_previous"]=revision;job["consumer_before"]=runtime;job["preparation"]["runtime_before"]=runtime
                    job["preparation"].update(publication_wait_seconds=clock()-wait_started,publication_lock_at=time.time())
                    # Installing the final immutable revision is durable publication intent.
                    job.update(phase="installing_prepared_assets",effects_started=True)
                    self.broker._save(job)
                self.release.install_prepared_assets(self.assets,job["job_id"],destination,job["inputs_sha256"])
                job["prepared_publication"]=True
                return blob,guard
            except BaseException:
                if held:self.release_locks(guard)
                raise
        with self.broker.lock:
            job.update(status="needs_decision",phase="waiting_publication_base",reason="Publication inputs changed during three preparation attempts; resume this job after contention settles")
            self.broker._save(job)
        return None

    def run(self,job,blob,locked):
        """Use the shared coordinator while preserving host-owned activation and source promotion."""
        admitted=self.admit(job,blob)
        if admitted is None:return
        blob,guard=admitted;acquired=time.monotonic()
        try:
            locked(job,blob)
        finally:
            try:
                with self.broker.lock:
                    job["preparation"]["publication_lock_seconds"]=time.monotonic()-acquired
                    self.broker._save(job)
            finally:self.release_locks(guard)

    def release_locks(self,guard):
        """A failed receipt write or boot lease cleanup must not strand publication."""
        try:
            if guard is not None:guard.__exit__(None,None,None)
        finally:self.broker.deploy_lock.release()

    recovery_actions={"discard_prepared_release","restore_failed_ui","restore_backend_predecessor"}

    def legacy_recovery_binding(self,job,folder):
        """Recover old UI receipts using their recorded container IDs/images and exact predecessor."""
        import hashlib
        before=self.broker._current_release_blob()
        current=self.runtime();recorded=job.get("consumer_before",{})
        if set(recorded)!={"api","loop"}or any({k:current[role][k]for k in ("id","image")}!=recorded[role]for role in recorded):
            raise ValueError("Legacy runtime receipt binding changed")
        bound=job.get("legacy_recovery_runtime",current)
        if current!=bound:raise ValueError("Legacy recovery runtime changed")
        predecessor=job.get("asset_previous")
        manifest=self.release.verify_index(self.assets/"releases"/predecessor,predecessor,job["base_inputs_sha256"])
        if self.validate(before)[0]!=manifest["candidate_sha256"]:raise ValueError("Legacy predecessor source changed")
        baseline=folder/"publication-base.tar"
        if not baseline.exists():
            fd=os.open(baseline,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
            with os.fdopen(fd,"wb")as stream:stream.write(before);stream.flush();os.fsync(stream.fileno())
            fd=os.open(folder,os.O_RDONLY|os.O_DIRECTORY)
            try:os.fsync(fd)
            finally:os.close(fd)
        job["legacy_recovery_runtime"]=bound;self.broker._save(job)
        failed=self.assets/"releases"/job["job_id"];index=failed/".code-index"
        paths=[index/"manifest.json",index/"index.sqlite"]
        if failed.is_symlink()or index.is_symlink()or any(p.is_symlink()or not p.is_file()for p in paths):raise ValueError("Legacy failed index path invalid")
        if paths[0].stat().st_size>2*1024**2 or paths[1].stat().st_size>self.broker.code_index_ledger.scanner.MAX_DB_BYTES:raise ValueError("Legacy failed index exceeds bounds")
        receipt=json.loads(paths[0].read_bytes());scanner=self.broker.code_index_ledger.scanner
        # A corrupt index is the reason for restoring; require its exact hashed source/bytes, not readable tables.
        if receipt.get("release_id")!=job["job_id"]or receipt.get("candidate_sha256")!=job["inputs_sha256"]or receipt.get("source_sha256")!=scanner.source_digest(failed)or receipt.get("index_sha256")!=hashlib.sha256(paths[1].read_bytes()).hexdigest():raise ValueError("Legacy failed source/index receipt changed")
        return bound

    def backend_predecessor_binding(self,job,folder,recorded,previous):
        """Certify already-restored original containers; never recreate or adopt candidate binaries.

        Process IDs may change during restoration. Container IDs, immutable images,
        Config and HostConfig must match the private pre-effect capture exactly.
        A recovery retry additionally pins its post-restoration process generation.
        No private configuration is returned or interpolated into errors.
        """
        import stat
        if job.get("operation")!="build_deploy" or job.get("publication_policy_version")!=1:
            raise ValueError("Reviewed backend publication required")
        if not isinstance(recorded,dict)or set(recorded)!={"api","loop"}:
            raise ValueError("Predecessor runtime receipt required")
        fd=os.open(folder/"container-config.json",os.O_RDONLY|os.O_NOFOLLOW)
        with os.fdopen(fd,"rb")as stream:
            info=os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode)or info.st_nlink!=1 or info.st_uid!=os.geteuid()or info.st_mode&0o022 or info.st_size>8*1024**2:
                raise ValueError("Invalid private predecessor capture")
            raw=stream.read(8*1024**2+1)
        if len(raw)>8*1024**2:raise ValueError("Private predecessor capture exceeds bounds")
        captured=json.loads(raw)
        if not isinstance(captured,dict)or set(captured)!={"api","loop"}:
            raise ValueError("Invalid private predecessor capture")
        current=self.runtime()
        for role,name in zip(("api","loop"),self.broker.publication_targets):
            old=captured[role];live=self.broker.runner.inspect(name)
            if any(old.get(key)!=recorded[role].get(field)or live.get(key)!=old.get(key)
                   for key,field in (("Id","id"),("Image","image"))):
                raise ValueError("Original predecessor container required")
            if any(key not in old or live.get(key)!=old[key]for key in ("Config","HostConfig")):
                raise ValueError("Predecessor container configuration changed")
            if not self.broker.runner.healthy(name,recorded[role]["image"]):
                raise ValueError("Restored predecessor unhealthy")
        if self.runtime()!=current or previous and previous.get("runtime")!=current:
            raise ValueError("Restored predecessor process generation changed")
        return current

    def recover(self,request,args):
        """Reconcile durable intent with exact effects before admitting another publication."""
        if set(args)!={"action","task_id","job_id","request_id"}or args.get("action")not in self.recovery_actions:
            raise ValueError("Exact release recovery arguments required")
        import hashlib
        import re
        if not isinstance(args["request_id"],str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",args["request_id"]):raise ValueError("Stable recovery request_id required")
        trusted=request["trusted"];workspace=request["workspace"]
        if not trusted.get("local_caller"):raise PermissionError("Operator recovery required")
        digest=hashlib.sha256(json.dumps(args,sort_keys=True,separators=(",",":")).encode()).hexdigest()
        with self.broker.deploy_lock,self.broker.boot_effect_lease(),self.broker.lock:
            self.broker._incoming_lease(trusted)
            if not self.broker._maintenance_allowed(trusted,args):raise PermissionError("Build service drained for another task")
            item=self.broker.scheduler.item(args,trusted,workspace)
            job=self.broker.jobs.get(args["job_id"])
            if not job or item.get("job_id")!=job["job_id"]or not self.broker._owned(job,trusted,workspace):raise PermissionError("Owned latest release required")
            previous=job.get("publication_recovery")
            if previous and previous["digest"]!=digest:raise ValueError("Release recovery already has a different request")
            if previous and previous.get("done"):return previous["result"]
            if job["status"]!="uncertain"or not job.get("effects_started")or job.get("revoked")or job["job_id"]in self.broker.active_jobs:
                raise ValueError("Inactive uncertain publication intent required")
            state=self.broker.scheduler.ensure(item,trusted,workspace)
            self.broker.scheduler.store.assert_owned(item["key"],state["owner"],job["ownership_epoch"],[])
            folder=self.broker.state/job["job_id"]
            legacy=job.get("publication_policy_version",0)==0 and job["operation"]=="ui_deploy" and args["action"]=="restore_failed_ui"
            bound=self.legacy_recovery_binding(job,folder)if legacy else job.get("preparation",{}).get("runtime_before")
            backend=args["action"]=="restore_backend_predecessor"
            if backend:bound=self.backend_predecessor_binding(job,folder,bound,previous)
            elif not bound or self.runtime()!=bound:raise ValueError("Runtime binding changed; explicit runtime reconciliation required")
            folder=self.broker.state/job["job_id"]
            baseline=folder/"publication-base.tar"
            if baseline.is_symlink()or not baseline.is_file():raise ValueError("Durable publication baseline required")
            before=baseline.read_bytes();candidate=(folder/"candidate.tar").read_bytes()
            if self.validate(before)[0]!=job["base_inputs_sha256"]or self.validate(candidate)[0]!=job["inputs_sha256"]:raise ValueError("Recovery source binding changed")
            if self.validate(self.broker._current_release_blob())[0]!=job["base_inputs_sha256"]:raise ValueError("A newer release receipt exists; use a compensating release")
            predecessor=job.get("asset_previous")
            if not predecessor:raise ValueError("Verified predecessor required")
            self.release.verify_index(self.assets/"releases"/predecessor,predecessor)
            active=self.broker.runner.current_asset_id()
            discard=args["action"]=="discard_prepared_release"
            if backend:
                if active not in {job["job_id"],predecessor}:raise ValueError("Active pointer advanced; use a compensating release")
                if not self.broker.runner.checkout_matches(before,job.get("changed_paths",())):raise ValueError("Checkout effects need reconciliation")
                self.release.verify_index(self.assets/"releases"/job["job_id"],job["job_id"],job["inputs_sha256"])
            elif discard:
                if active!=predecessor:raise ValueError("Prepared release is active or publication advanced")
                if not self.broker.runner.checkout_matches(before,job.get("changed_paths",())):raise ValueError("Checkout effects need reconciliation")
            else:
                if job["operation"]!="ui_deploy"or any(not path.startswith("v3/ui/")for path in job.get("changed_paths",())):raise ValueError("Only retained-runtime UI activation can be restored")
                # A retry may observe the pointer already restored by this same durable intent.
                if active!=job["job_id"]and not(previous and active==predecessor):raise ValueError("Active pointer advanced; use a compensating release")
                if not legacy:self.release.verify_index(self.assets/"releases"/job["job_id"],job["job_id"],job["inputs_sha256"])
            job["publication_recovery"]={"digest":digest,"request_id":args["request_id"],"action":args["action"],"done":False}
            if backend:job["publication_recovery"]["runtime"]=bound
            self.broker._save(job)
            if backend:
                if self.runtime()!=bound:raise ValueError("Restored predecessor process generation changed")
                if active==job["job_id"]:self.broker.runner.activate_assets(predecessor)
            elif not discard:
                # Only current==failed may be replaced; the serialized coordinator fences other writers.
                if active==job["job_id"]:self.broker.runner.activate_assets(predecessor)
                self.broker.runner.promote_checkout(candidate,before,job.get("changed_paths",()))
            self.broker.runner.verify_ui(job.get("changed_paths",()))
            for role,name in zip(("api","loop"),self.broker.publication_targets):
                if not self.broker.runner.healthy(name,bound[role]["image"]):raise ValueError("Recovered consumer unhealthy")
            if backend and self.runtime()!=bound:raise ValueError("Restored predecessor process generation changed")
            if self.broker.runner.current_asset_id()!=predecessor or not self.broker.runner.checkout_matches(before,job.get("changed_paths",())):raise ValueError("Recovery effects not reconciled")
            result={"job_id":job["job_id"],"status":"failed","effects_reconciled":True,"restored_release_id":predecessor}
            job.update(status="failed",phase="publication_reconciled",effects_started=False,effects_reconciled_at=time.time(),reason="Unsuccessful release reconciled to its exact predecessor; candidate and evidence retained")
            job["publication_recovery"].update(done=True,result=result);self.broker._save(job)
            self.broker._start_next()
            return result

    def rollback_preview(self,request,args):
        """Prepare an inverse patch on a fresh isolated task; normal claims and publication still apply."""
        if set(args)!={"action","task_id","release_job_id"}or not request["trusted"].get("local_caller"):
            raise ValueError("Operator rollback preview requires a target release and isolated task")
        import difflib
        with self.broker.deploy_lock,self.broker.lock:
            self.broker._incoming_lease(request["trusted"])
            item=self.broker.scheduler.item(args,request["trusted"],request["workspace"])
            target=self.broker.jobs.get(args["release_job_id"],{})
            if item["status"]!="open"or item.get("mode")!="isolated":raise ValueError("Open isolated rollback task required")
            if target.get("status")!="completed"or target.get("operation")!="ui_deploy"or target.get("batch_members"):
                raise ValueError("Successful individual UI release required")
            folder=self.broker.state/target["job_id"];before=folder/"publication-base.tar"
            if before.is_symlink()or not before.is_file():raise ValueError("Receipt-bound publication predecessor unavailable")
            before=before.read_bytes();after=(folder/"candidate.tar").read_bytes()
            if self.validate(before)[0]!=target["base_inputs_sha256"]or self.validate(after)[0]!=target["inputs_sha256"]:raise ValueError("Rollback receipt source mismatch")
            snapshot=self.broker.scheduler_task_snapshot(Path(item["workspace_path"]).parent)
            baseline=(Path(item["workspace_path"]).parent/"baseline.tar").read_bytes()
            if self.validate(snapshot)[0]!=self.validate(baseline)[0]:raise ValueError("Rollback task must have no pending edits")
            merged,changed=self.release.compensating_candidate(before,after,snapshot,target.get("changed_paths",()),self.validate)
            current=self.release.entries(snapshot,self.validate);wanted=self.release.entries(merged,self.validate);patches=[]
            for path in changed:
                if not path.startswith("v3/ui/"):raise ValueError("Only UI source can use this rollback preview")
                old=current.get(path);new=wanted.get(path)
                if old==new:continue
                logical=path.removeprefix("v3/")
                a=old[1].decode("utf-8","strict")if old else ""
                b=new[1].decode("utf-8","strict")if new else ""
                header="diff --git a/"+logical+" b/"+logical+"\n"
                if old is None:header+="new file mode "+format(new[0]|0o100000,"o")+"\n"
                elif new is None:header+="deleted file mode "+format(old[0]|0o100000,"o")+"\n"
                elif old[0]!=new[0]:header+="old mode "+format(old[0]|0o100000,"o")+"\nnew mode "+format(new[0]|0o100000,"o")+"\n"
                diff="".join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile="a/"+logical if old else "/dev/null",tofile="b/"+logical if new else "/dev/null"))
                # Git requires an explicit marker for the final unterminated line.
                diff="".join(line if line.endswith("\n")else line+"\n\\ No newline at end of file\n"for line in diff.splitlines(True))
                patches.append(header+diff)
            patch="".join(patches)
            if not patch or len(patch.encode())>2*1024*1024:raise ValueError("Rollback patch empty or exceeds checked-patch bound")
            return {"task_id":item["task_id"],"release_job_id":target["job_id"],"expected_inputs_sha256":self.validate(snapshot)[0],"patch":patch,"changed_paths":changed,"publication_required":True}
