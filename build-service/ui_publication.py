"""Prepare immutable UI candidates concurrently, serializing only admitted publication effects."""
import json
import os
from pathlib import Path
import sqlite3
import time
import uuid
import threading
import contextlib

class PublicationWaitTimeout(TimeoutError):
    """Pre-effect publication admission can resume through the existing decision lane."""



# @code-index {"version":1,"feature":"concurrent-ui-publication","aliases":["concurrent ui publication"],"purpose":"Prepare private candidates and serialize exact source/index publication. Config: build-service/scheduler-defaults.json -> concurrent_ui_preparation, max_ui_preparations.","span_lines":70,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/concurrent_ui_preparation","/max_ui_preparations"]}],"opportunities":[]}}
class UIPublication:
    """Keep stale preparation outside publication; host authorization owns every effect."""
    def __init__(self,broker,release,validate,context,asset_root,policy):
        """Bind preparation and recovery to this protected host's fixed release authority."""
        self.broker=broker;self.release=release;self.validate=validate
        self.context=context;self.assets=asset_root;self.policy=policy
        self.wait_timeout=300.;self.holder=None

    def enabled(self,job):
        """Existing queued generations retain their admitted release contract."""
        return bool(job.get("isolated")and job.get("publication_policy_version")==1 and self.broker.scheduler.config.get("scheduler",{}).get("concurrent_ui_preparation",False))

    def runtime(self):
        """Capture retained consumer identities without serializing credentials or configuration."""
        return {role:{"id":value["Id"],"image":value["Image"],"pid":value.get("State",{}).get("Pid")}for role,name in zip(("api","loop"),self.broker.publication_targets)for value in [self.broker.runner.inspect(name)]}

    def reviews(self,folder,changed):
        """Require current applicable reviews in the exact sealed candidate index."""
        scanner=self.broker.code_index_ledger.scanner
        if (folder/".code-index/source-release.json").is_file():
            manifest=self.release.verify_publication(folder,folder.name)
            return manifest,[path for path in changed if Path(path).suffix in scanner.SUFFIXES]
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

    def cancelled(self,job):
        """Check cancellation without holding a publication or authority lock."""
        event=self.broker.job_cancellations.get(job["job_id"])
        if self.broker.stopping or job.get("revoked")or(event is not None and event.is_set())or job.get("lease_until",float("inf"))<=time.time():
            raise PermissionError("Publication wait cancelled or authorization expired")

    def wait_for(self,job,resource,probe,timeout=None,owner=None,purpose="publication"):
        """Bound a nonblocking acquisition; never wait while holding host authority."""
        if getattr(self.broker.lock,"_is_owned",lambda:False)():raise RuntimeError("Publication acquisition while broker authority lock is held")
        timeout=self.wait_timeout if timeout is None else timeout
        started=time.monotonic();since=time.time();last=0.;waiting=False;outcome="failed"
        try:
            while True:
                self.cancelled(job)
                if probe():outcome="acquired";return
                waiting=True;now=time.monotonic()
                if now-started>=timeout:outcome="timed_out";raise PublicationWaitTimeout("Publication wait timed out: "+resource)
                if now-last>=1:
                    blocker=owner()if owner else None
                    with self.broker.lock:
                        self.broker._batch_lease(job)
                        job["phase"]="waiting_publication"
                        job["publication_wait"]={"resource":resource,"owner":blocker,"purpose":purpose,"reason":"Publication serialization"if resource=="deploy_lock"else"Boot supervisor or publication effect lease", "since":since,"deadline_at":since+timeout,"elapsed_seconds":round(now-started,3),"state":"waiting"}
                        self.broker._save(job)
                    last=now
                time.sleep(min(.1,max(0,timeout-(time.monotonic()-started))))
        except PermissionError:
            outcome="cancelled";raise
        finally:
            if waiting:
                with self.broker.lock:
                    value=job.get("publication_wait",{"resource":resource,"since":since,"purpose":purpose})
                    value={**value,"state":outcome,"elapsed_seconds":round(time.monotonic()-started,3)}
                    job["publication_wait"]=value
                    job.setdefault("publication_wait_history",[]).append(dict(value));job["publication_wait_history"]=job["publication_wait_history"][-16:]
                    self.broker._save(job)

    def acquire(self,job,purpose="publication"):
        """Track known owners without taking the broker lock across lock acquisition."""
        acquired=False
        def probe():
            nonlocal acquired
            acquired=self.broker.deploy_lock.acquire(blocking=False);return acquired
        try:
            self.wait_for(job,"deploy_lock",probe,owner=lambda:dict(self.holder)if self.holder else None,purpose=purpose)
            self.cancelled(job)
            self.holder={"job_id":job["job_id"],"thread_id":threading.get_ident(),"since":time.time()}
        except BaseException:
            if acquired:self.broker.deploy_lock.release()
            raise

    @contextlib.contextmanager
    def capture(self,job):
        self.acquire(job,"capture_publication_base")
        try:yield
        finally:self.release_locks(None)

    def admit(self,job,blob,tested_runtime=False):
        try:return self._admit(job,blob,tested_runtime)
        except TimeoutError as error:
            if not isinstance(error,PublicationWaitTimeout)and not getattr(error,"recoverable_admission",False):raise
            with self.broker.lock:
                job.update(status="needs_decision",phase="waiting_publication_base",reason=str(error)+"; no publication effects started; resume this durable job after contention settles")
                self.broker._save(job)
            return None

    def _admit(self,job,blob,tested_runtime=False):
        """Return with publication and boot-effect locks held only for a current prepared candidate."""
        clock=time.monotonic;started=clock();job.setdefault("preparation",{})
        for attempt in range(1,4):
            with self.capture(job):
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
            host_only=job.get("operation")=="host_service_deploy"
            validation_started=clock()
            if not host_only:
                try:
                    self.broker.runner.validate_ui(job["job_id"],job.get("changed_paths",()),root=destination)
                except Exception:
                    # A coordinated backend switch may temporarily remove a consumer.
                    # Retry only when the exact captured publication/runtime binding changed.
                    with self.capture(job):
                        stale=(self.broker.runner.current_asset_id()!=revision or
                               self.validate(self.broker._current_release_blob())[0]!=base or self.runtime()!=runtime)
                    if stale:continue
                    raise
            validated=0.0 if host_only else clock()-validation_started
            manifest,pending=self.reviews(destination,job.get("changed_paths",()))
            evidence=self.policy.dependencies(self.release.entries(blob,self.validate),job.get("changed_paths",()))
            evidence["runtime_images"]=dict(job.get("images",{}))if tested_runtime else{role:identity["image"]for role,identity in runtime.items()}
            if host_only:
                # Installed broker bytes do not alter served UI assets. Preserve the
                # dependency fingerprint while declaring the UI suite inapplicable.
                evidence.update(tests=[],conservative=False)
            test_started=clock()
            if not host_only:self.broker._phase(job,"frontend_tests")
            if host_only:
                test_result={"status":"not_required","tests":[],"reason":"host-only broker candidate leaves UI assets unchanged"}
            else:
                with self.broker.host_phase(job,"frontend_tests"):
                    test_result=self.broker.runner.validate_selected_tests(job["job_id"],destination,evidence)
            with self.broker.lock:
                job.setdefault("ui_verification_history",[]).append({"attempt":attempt,"base_release_id":revision,"result":test_result})
                job["ui_test_evidence"]=test_result
                job["preparation"]["frontend_tests_seconds"]=clock()-test_started
                job["preparation"].update(attempts=attempt,indexing_seconds=0 if manifest.get("publication_mode")=="post_deployment_indexing_v1"else indexed,
                    source_preparation_seconds=indexed if manifest.get("publication_mode")=="post_deployment_indexing_v1"else 0,
                    index_deferred=manifest.get("publication_mode")=="post_deployment_indexing_v1",validation_seconds=validated,
                    total_seconds=clock()-started,parsed_files=manifest.get("indexing",{}).get("parsed_files"),
                    reused_files=manifest.get("indexing",{}).get("reused_files"),dependency_sha256=evidence["sha256"],
                    conservative_validation=evidence["conservative"],required_tests=evidence["tests"])
                job["prepared_assets"]=str(destination);self.broker._save(job)
            if self.broker.review_policy.defer_by_policy():pending=[]
            if pending and self.broker.review_policy.allows(job,destination):
                with self.broker.lock:
                    job["deferred_documentation"]["pending_paths"]=pending
                    self.broker._save(job)
                pending=[]
            if pending:
                with self.broker.lock:
                    job.update(status="needs_decision",phase="awaiting_prepared_reviews",reason="Complete applicable reviews against the sealed prepared candidate before resuming")
                    job["preparation"]["pending_review_paths"]=pending[:32]
                    if not self.broker.scheduler.review_wait(job,reset=True):raise PermissionError("Prepared review delegation unavailable")
                    self.broker._save(job)
                return None
            self.broker._phase(job,"waiting_publication")
            wait_started=clock();self.acquire(job);guard=None;held=True
            try:
                entering=self.broker.boot_effect_lease(job=job);entering.__enter__();guard=entering
                if self.broker.runner.current_asset_id()!=revision or self.validate(self.broker._current_release_blob())[0]!=base or self.runtime()!=runtime:
                    with self.broker.lock:
                        job["preparation"].setdefault("stale_attempts",[]).append({"attempt":attempt,"reason":"Published base, asset pointer or runtime changed","captured_release_id":revision,"observed_release_id":self.broker.runner.current_asset_id(),"at":time.time()})
                        self.broker._save(job)
                    closing=guard;guard=None;held=False
                    self.release_locks(closing);continue
                with self.broker.lock:self.broker._batch_lease(job)
                if self.broker._has_unresolved_release(job["job_id"]):raise ValueError("Reconcile uncertain publication first")
                if not self.broker._sync_job(job,live,blob,job.get("changed_paths",())):
                    closing=guard;guard=None;held=False
                    self.release_locks(closing);return None
                if self.validate(self.release.snapshot(Path(job["release_root"]),self.context,self.validate))[0]!=job["inputs_sha256"]:
                    raise ValueError("Frozen preparation source changed")
                self.release.verify_publication(destination,job["job_id"],job["inputs_sha256"])
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
        finally:
            self.holder=None
            self.broker.deploy_lock.release()

    recovery_actions={"discard_prepared_release","restore_failed_ui","restore_backend_predecessor","restore_captured_backend","recover_backend_api_bind","complete_activated_release"}

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
        manifest=self.release.verify_publication(self.assets/"releases"/predecessor,predecessor,job["base_inputs_sha256"])
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

    def backend_predecessor_capture(self,folder):
        """Read the private pre-effect capture without exposing configuration values."""
        import stat
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
        return captured

    def backend_predecessor_binding(self,job,folder,recorded,previous):
        """Certify already-restored original containers; never recreate or adopt candidate binaries.

        Process IDs may change during restoration. Container IDs, immutable images,
        Config and HostConfig must match the private pre-effect capture exactly.
        A recovery retry additionally pins its post-restoration process generation.
        No private configuration is returned or interpolated into errors.
        """
        if job.get("operation")!="build_deploy" or job.get("publication_policy_version")!=1:
            raise ValueError("Reviewed backend publication required")
        if not isinstance(recorded,dict)or set(recorded)!={"api","loop"}:
            raise ValueError("Predecessor runtime receipt required")
        captured=self.backend_predecessor_capture(folder)
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

    def restore_captured_backend(self,job,folder,recorded,digest,request_id):
        """Restore only captured identities; reconcile every effect from a durable intent.

        Failed replacements are quarantined, never deleted. Names are discovery
        handles; mutations target exact Docker IDs to fence concurrent replacement.
        """
        import hashlib,re
        if job.get("operation")!="build_deploy"or job.get("publication_policy_version")!=1:
            raise ValueError("Captured backend publication required")
        if not isinstance(recorded,dict)or set(recorded)!={"api","loop"}:raise ValueError("Predecessor runtime receipt required")
        captured=self.backend_predecessor_capture(folder);runner=self.broker.runner
        fingerprint=hashlib.sha256(json.dumps(captured,sort_keys=True,separators=(",",":")).encode()).hexdigest()
        previous=job.get("publication_recovery")or{}
        plan=previous.get("restore")
        if plan and plan["capture_sha256"]!=fingerprint:raise ValueError("Captured predecessor binding changed")
        if previous.get("runtime"):
            return self.backend_predecessor_binding(job,folder,recorded,previous)
        roles={};observed={}
        for role,name in zip(("api","loop"),self.broker.publication_targets):
            old=captured[role];backup=name+"-pre-build-"+job["job_id"];failed=name+"-failed-build-"+job["job_id"]
            if job.get("rollback",{}).get(role)!=backup:raise ValueError("Captured rollback name changed")
            if not re.fullmatch(r"[a-f0-9]{64}",old.get("Id",""))or old.get("Id")!=recorded[role].get("id")or old.get("Image")!=recorded[role].get("image"):
                raise ValueError("Captured predecessor identity changed")
            values={n:runner.inspect_recovery(n)for n in (name,backup,failed)}
            originals=[(n,v)for n,v in values.items()if v and v.get("Id")==old["Id"]]
            if len(originals)!=1 or originals[0][0]not in {name,backup}:raise ValueError("Exact captured predecessor unavailable")
            old_name,live=originals[0]
            if any(key not in old or live.get(key)!=old[key]for key in ("Image","Config","HostConfig")):
                raise ValueError("Captured predecessor configuration changed")
            if live.get("State",{}).get("Status")not in {"running","exited","created"}:raise ValueError("Captured predecessor state requires reconciliation")
            if values[backup]and values[backup]["Id"]!=old["Id"]:raise ValueError("Rollback name occupied by another container")
            replacements=[v for n,v in values.items()if v and v["Id"]!=old["Id"]]
            if len(replacements)>1:raise ValueError("Ambiguous failed replacement")
            candidate=replacements[0]if replacements else None
            if candidate:
                labels=candidate.get("Config",{}).get("Labels")or{}
                certified=job.get("candidate_containers",{}).get(role)==candidate.get("Id")
                marked=all(labels.get(k)==v for k,v in {"webagent.release.job":job["job_id"],"webagent.release.role":role,"webagent.release.instance":self.broker.runner.source_config["instance"]}.items())
                if candidate.get("Image")!=job.get("images",{}).get(role)or not(certified or marked)or not re.fullmatch(r"[a-f0-9]{64}",candidate.get("Id","")):
                    raise ValueError("Unrecognized replacement container; explicit reconciliation required")
                if candidate.get("State",{}).get("Status")not in {"running","restarting","exited","created"}:raise ValueError("Failed replacement state requires reconciliation")
            value={"id":old["Id"],"image":old["Image"],"failed_name":failed,"candidate_id":candidate["Id"]if candidate else None}
            if plan and plan["roles"].get(role)!=value:raise ValueError("Captured restoration identities changed")
            roles[role]=value;observed[role]=(name,backup,failed,values,old_name,candidate)
        runner.preflight_deploy_images({role:value["image"]for role,value in roles.items()},job["job_id"])
        if plan is None:
            job["publication_recovery"]={"digest":digest,"request_id":request_id,"action":"restore_captured_backend","done":False,
                "restore":{"capture_sha256":fingerprint,"roles":roles,"milestones":[]}}
            self.broker._save(job)
        recovery=job["publication_recovery"]
        def effect(role,operation,identity,call):
            recovery["restore"]["intent"]={"role":role,"operation":operation,"container_id":identity};self.broker._save(job)
            call()
            recovery["restore"]["milestones"].append({"role":role,"operation":operation,"container_id":identity})
            recovery["restore"].pop("intent",None);self.broker._save(job)
        # All roles/configurations are checked before the first mutation.
        for role,(name,backup,failed,values,old_name,candidate)in observed.items():
            if candidate and candidate["State"]["Status"]in {"running","restarting"}:
                effect(role,"stop_failed",candidate["Id"],lambda v=candidate:runner.stop(v["Id"]))
            if candidate and values[name]and values[name]["Id"]==candidate["Id"]:
                effect(role,"quarantine_failed",candidate["Id"],lambda v=candidate,n=failed:runner.rename(v["Id"],n))
            if old_name==backup:
                effect(role,"restore_name",roles[role]["id"],lambda v=roles[role],n=name:runner.rename(v["id"],n))
        predecessor=job["asset_previous"]
        if runner.current_asset_id()==job["job_id"]:
            effect("assets","restore_pointer",None,lambda:runner.activate_assets(predecessor))
        elif runner.current_asset_id()!=predecessor:raise ValueError("Active pointer advanced during restoration")
        # Start both before checking either: API readiness depends on loop heartbeat.
        for role,name in zip(("api","loop"),self.broker.publication_targets):
            value=runner.inspect_recovery(name)
            if not value or value["Id"]!=roles[role]["id"]:raise ValueError("Restored identity changed before start")
            if value["State"]["Status"]!="running":effect(role,"start_predecessor",value["Id"],lambda v=value:runner.start(v["Id"]))
        bound=self.backend_predecessor_binding(job,folder,recorded,None)
        recovery["runtime"]=bound;self.broker._save(job)
        return bound

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
            discard=args["action"]=="discard_prepared_release"
            # A cancellation after durable intent but before activation must remain
            # discardable after restart. Other revoked candidates stay fenced.
            if (job["status"]!="uncertain" or not job.get("effects_started")
                    or (job.get("revoked") and not discard)
                    or job["job_id"] in self.broker.active_jobs):
                raise ValueError("Inactive uncertain publication intent required")
            state=self.broker.scheduler.ensure(item,trusted,workspace)
            self.broker.scheduler.store.assert_owned(item["key"],state["owner"],job["ownership_epoch"],[])
            folder=self.broker.state/job["job_id"]
            legacy=job.get("publication_policy_version",0)==0 and job["operation"]=="ui_deploy" and args["action"]=="restore_failed_ui"
            bound=self.legacy_recovery_binding(job,folder)if legacy else job.get("preparation",{}).get("runtime_before")
            restore=args["action"]=="restore_captured_backend"
            backend=restore or args["action"]=="restore_backend_predecessor"
            bind_recovery=args["action"]=="recover_backend_api_bind"
            if backend and not restore:bound=self.backend_predecessor_binding(job,folder,bound,previous)
            elif not restore and not bind_recovery and(not bound or self.runtime()!=bound):raise ValueError("Runtime binding changed; explicit runtime reconciliation required")
            folder=self.broker.state/job["job_id"]
            baseline=folder/"publication-base.tar"
            if baseline.is_symlink()or not baseline.is_file():raise ValueError("Durable publication baseline required")
            before=baseline.read_bytes();candidate=(folder/"candidate.tar").read_bytes()
            if self.validate(before)[0]!=job["base_inputs_sha256"]or self.validate(candidate)[0]!=job["inputs_sha256"]:raise ValueError("Recovery source binding changed")
            if self.validate(self.broker._current_release_blob())[0]!=job["base_inputs_sha256"]:raise ValueError("A newer release receipt exists; use a compensating release")
            if args["action"]=="complete_activated_release":
                # Complete only the exact already-active, reviewed UI snapshot.
                # This does not activate again, rebuild, or adopt a different runtime.
                template_only=all(path.startswith("v3/ui/")or path=="v3/config/chat.json"for path in job.get("changed_paths",()))
                if job["operation"]not in {"ui_deploy","plugin_deploy"}or not template_only or not job.get("hot_assets")or job.get("publication_policy_version")!=1 or job.get("phase")not in {"switching_ui","checking_ui","promoting_checkout"}:
                    raise ValueError("Retained-runtime activated UI recovery required")
                self.broker._lease(job)
                self.broker.scheduler.assert_job(job)
                if self.broker.runner.current_asset_id()!=job["job_id"]:raise ValueError("Active pointer advanced; cannot complete this activation")
                active_root=self.assets/"releases"/job["job_id"]
                self.release.verify_publication(active_root,job["job_id"],job["inputs_sha256"])
                self.reviews(active_root,job.get("changed_paths",()))
                job["publication_recovery"]={"digest":digest,"request_id":args["request_id"],"action":args["action"],"done":False}
                self.broker._save(job)
                self.broker._code_index_activated(job["job_id"])
                self.broker.runner.verify_ui(job.get("changed_paths",()))
                for role,name in zip(("api","loop"),self.broker.publication_targets):
                    if not self.broker.runner.healthy(name,bound[role]["image"]):raise ValueError("Recovered consumer unhealthy")
                if job["operation"]=="plugin_deploy":
                    # UI JSON templates use the plugin lane; retain its consumer
                    # adoption and engine-mode proof even though no executable changed.
                    job["consumer_adoption"]=self.broker.runner.verify_plugin_adoption(job["job_id"])
                    job["engine_execution_modes"]=self.broker.runner.engine_execution_modes(job["job_id"])
                # Checked promotion preflights all paths and tolerates exact before/after
                # bytes after a partial promotion; unrelated checkout bytes are preserved.
                self.broker.runner.promote_checkout(before,candidate,job.get("changed_paths",()))
                if self.runtime()!=bound or self.broker.runner.current_asset_id()!=job["job_id"]or not self.broker.runner.checkout_matches(candidate,job.get("changed_paths",())):
                    raise ValueError("Activated release effects need reconciliation")
                job["synchronization"]=self.broker._sync_report(candidate,candidate,job.get("changed_paths",()))
                if job["synchronization"]["total"]:raise ValueError("Checkout synchronization changed")
                result={"job_id":job["job_id"],"status":"completed","effects_reconciled":True,"activation_replayed":False,"source_promoted":True}
                job.update(status="completed",phase="finished",finished_at=time.time(),effects_reconciled_at=time.time(),reason="Exact active source/index and retained consumers verified; source promotion reconciled")
                job["publication_recovery"].update(done=True,result=result)
                self.broker._save(job);self.broker._start_next()
                return result
            if args["action"]=="recover_backend_api_bind":
                if (job.get("operation")!="build_deploy" or job.get("publication_policy_version")!=1
                        or job.get("phase")!="checking_ui" or not isinstance(job.get("images"),dict)
                        or set(job["images"])!={"api","loop"}):
                    raise ValueError("Exact uncertain backend API check required")
                if self.broker.runner.current_asset_id()!=job["job_id"]:
                    raise ValueError("Candidate asset pointer is not active")
                if not self.broker.runner.checkout_matches(before,job.get("changed_paths",())):
                    raise ValueError("Checkout effects need reconciliation")
                captured=self.backend_predecessor_capture(folder)
                api,loop=self.broker.publication_targets
                current_api=self.broker.runner.inspect(api)
                current_loop=self.broker.runner.inspect(loop)
                expected_ports=captured["api"].get("HostConfig",{}).get("PortBindings")
                if not isinstance(expected_ports,dict) or not expected_ports:
                    raise ValueError("Recorded API port binding required")
                live_ports=(current_api.get("NetworkSettings") or {}).get("Ports")
                bound=live_ports==expected_ports
                previous_recovery=job.get("publication_recovery")
                resumed=(previous_recovery and previous_recovery.get("action")==args["action"]
                         and current_api.get("State",{}).get("Status")=="running" and bound)
                if current_api.get("Image")!=job["images"]["api"]:
                    raise ValueError("Candidate API image changed")
                if current_loop.get("Image")!=job["images"]["loop"] or not self.broker.runner.healthy(loop,job["images"]["loop"]):
                    raise ValueError("Candidate loop changed or unhealthy")
                if not resumed:
                    if current_api.get("State",{}).get("Status") not in {"exited","running"}:
                        raise ValueError("Candidate API state cannot be recovered")
                    job["publication_recovery"]={"digest":digest,"request_id":args["request_id"],"action":args["action"],"done":False}
                    self.broker._save(job)
                    if current_api.get("State",{}).get("Status")=="running":
                        self.broker.runner.stop(api)
                    self.broker.runner.remove(api)
                    self.broker.runner.create(api,job["images"]["api"],captured["api"],job["release_root"])
                    self.broker.runner.start(api)
                recovered_api=self.broker.runner.inspect(api)
                if (recovered_api.get("NetworkSettings") or {}).get("Ports")!=expected_ports:
                    raise ValueError("Candidate API port binding unavailable after recovery")
                if not self.broker.runner.healthy(api,job["images"]["api"]):
                    raise ValueError("Candidate API remains unhealthy after exact bind recovery")
                self.broker.runner.verify_ui(job.get("changed_paths",()))
                self.broker.runner.promote_checkout(before,candidate,job.get("changed_paths",()))
                if (not self.broker.runner.checkout_matches(candidate,job.get("changed_paths",()))
                        or self.broker.runner.current_asset_id()!=job["job_id"]
                        or not self.broker.runner.healthy(api,job["images"]["api"])):
                    raise ValueError("Candidate recovery effects need reconciliation")
                result={"job_id":job["job_id"],"status":"completed","effects_reconciled":True,
                        "api_recreated":not resumed,"source_promoted":True}
                job.update(status="completed",phase="finished",finished_at=time.time(),effects_reconciled_at=time.time(),
                           reason="Exact candidate API recovered after transient bind failure; source promoted")
                job["publication_recovery"].update(done=True,result=result)
                self.broker._save(job);self.broker._start_next()
                return result
            predecessor=job.get("asset_previous")
            if not predecessor:raise ValueError("Verified predecessor required")
            self.release.verify_publication(self.assets/"releases"/predecessor,predecessor)
            active=self.broker.runner.current_asset_id()
            discard=args["action"]=="discard_prepared_release"
            if backend:
                if active not in {job["job_id"],predecessor}:raise ValueError("Active pointer advanced; use a compensating release")
                if not self.broker.runner.checkout_matches(before,job.get("changed_paths",())):raise ValueError("Checkout effects need reconciliation")
                self.release.verify_publication(self.assets/"releases"/job["job_id"],job["job_id"],job["inputs_sha256"])
                if restore:
                    bound=self.restore_captured_backend(job,folder,bound,digest,args["request_id"])
                    active=self.broker.runner.current_asset_id()
            elif discard:
                if active!=predecessor:raise ValueError("Prepared release is active or publication advanced")
                if not self.broker.runner.checkout_matches(before,job.get("changed_paths",())):raise ValueError("Checkout effects need reconciliation")
            else:
                host_only=job["operation"]=="host_service_deploy"and job.get("hot_assets")and job.get("publication_policy_version")==1
                if not host_only and(job["operation"]!="ui_deploy"or any(not path.startswith("v3/ui/")for path in job.get("changed_paths",()))):raise ValueError("Only retained-runtime UI or installed-host activation can be restored")
                # A retry may observe the pointer already restored by this same durable intent.
                if active!=job["job_id"]and not(previous and active==predecessor):raise ValueError("Active pointer advanced; use a compensating release")
                if not legacy:self.release.verify_publication(self.assets/"releases"/job["job_id"],job["job_id"],job["inputs_sha256"])
            job["publication_recovery"]={**(job.get("publication_recovery")or{}),"digest":digest,"request_id":args["request_id"],"action":args["action"],"done":False}
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
