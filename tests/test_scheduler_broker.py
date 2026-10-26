"""Broker boundary regressions for ownership, retries, publication and corrections."""
import hashlib,json,subprocess,tempfile,time,unittest,uuid
from pathlib import Path
from unittest.mock import patch
import test_admin_build as fixtures

class SchedulerBrokerTests(unittest.TestCase):
    """Exercise the actual broker handlers with durable SQLite and real checked source patches."""
    def setUp(self):
        self.temporary=tempfile.TemporaryDirectory();self.root=Path(self.temporary.name)
        self.runner=fixtures.FakeRunner();self.runner.blob=fixtures.context(extra={"v3/README.md":"original\n"})
        self.broker=fixtures.host.Broker(self.root/"jobs",self.runner);self.broker.scheduler.mode="enforce"
        self.workspace=str(uuid.uuid4())
        self.trusted={"user_id":"codex-local","session_id":self.workspace,"local_caller":True,
            "lease_until":time.time()+120,"access_fingerprint":"f"*64}
        self.counter=0
    def tearDown(self):
        """Stop fixture threads and remove only fixture state."""
        self.broker.shutdown();self.temporary.cleanup()
    def call(self,name,args):
        """Enter the normal authenticated broker transport boundary."""
        return self.broker.handle({"name":name,"arguments":args,"trusted":self.trusted,"workspace":self.workspace})
    def begin(self,label):
        """Create an isolated source task through the existing broker API."""
        result=self.call("build_work",{"action":"begin","task_id":label,"summary":"Scheduler fixture"})
        key=hashlib.sha256(fixtures.host.canonical(["codex-local",self.workspace,label])).hexdigest()
        return self.broker.work[key]
    def action(self,label,action,**args):
        """Capture the current CAS revision, preserving the original request for retry tests."""
        item=next(i for i in self.broker.work.values()if i["task_id"]==label)
        value=self.broker.scheduler.store.get(item["key"]);self.counter+=1
        request={"task_id":label,"action":action,"request_id":"broker-fixture-"+str(self.counter),
            "expected_revision":value["revision"],"expected_epoch":value["epoch"],**args}
        return self.call("build_work",request),request
    def write(self,item,path,text):
        """Apply a checked patch with the exact current task digest."""
        tree=Path(item["workspace_path"]).parent
        digest=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree));self.counter+=1
        return self.call("file_write",{"task_id":item["task_id"],"path":path,"content":text,
            "expected_inputs_sha256":digest,"request_id":"source-fixture-"+str(self.counter)})
    def publish(self,item):
        """Construct a synchronized terminal fixture receipt and commit its actual promoted bytes."""
        tree=Path(item["workspace_path"]).parent;blob=self.broker.scheduler_task_snapshot(tree)
        files=self.broker.scheduler_entries(blob);job_id=str(uuid.uuid4());folder=self.broker.state/job_id;folder.mkdir()
        (folder/"candidate.tar").write_bytes(blob);(folder/"task.tar").write_bytes(blob)
        live=self.root/"live";live.mkdir(exist_ok=True)
        for name,(_,body)in files.items():
            if name.startswith("v3/"):
                target=live/name[3:];target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(body)
        self.runner.source=live;self.runner.blob=blob
        def git(*args):
            return subprocess.run(["git","-C",str(live),*args],stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True).stdout.decode().strip()
        git("init","-q");git("config","user.email","fixture@example.invalid");git("config","user.name","Scheduler fixture")
        git("add","README.md");git("commit","-qm","fixture publication");commit=git("rev-parse","HEAD")
        job={"job_id":job_id,"task_id":item["task_id"],"user_id":item["user_id"],"workspace":item["workspace"],
            "status":"completed","synchronization":{"status":"synchronized"},"changed_paths":["v3/README.md"],
            "inputs_sha256":self.broker.scheduler_digest(blob),"operation":"plugin_deploy","rust_tests":{"status":"not_required"}}
        self.broker.jobs[job_id]=job;item.update(status="closed",job_id=job_id);self.broker._save_work()
        return job,commit
    def test_reservations_task_filter_is_complete_and_listing_flags_truncation(self):
        item=self.begin("reservation-list-fixture");self.write(item,"README.md","claimed\n")
        store=self.broker.scheduler.store;value=store.get(item["key"])
        scopes=sorted(set(value["scopes"])|{f"path:docs/fixture-{n:02}.md"for n in range(40)})
        self.assertTrue(store.acquire(value["id"],value["owner"],value["revision"],value["epoch"],scopes,"fixture-many-scopes")["acquired"])
        listed=next(t for t in self.call("build_work",{"action":"reservations"})["tasks"]if t["label"]=="reservation-list-fixture")
        self.assertTrue(listed["scopes_truncated"]);self.assertEqual(listed["scopes_count"],len(scopes))
        self.assertEqual(len(listed["scopes"]),32);self.assertIn("takeover_preview",listed["scopes_notice"])
        selected=self.call("build_work",{"action":"reservations","task_id":"reservation-list-fixture"})["tasks"]
        self.assertEqual([t["scopes"]for t in selected],[scopes]);self.assertNotIn("scopes_truncated",selected[0])
        self.assertEqual(self.call("build_work",{"action":"reservations","task_id":"missing-fixture-task"})["tasks"],[])
    def test_begin_persists_baseline_before_registering_task(self):
        """An acknowledged task must retain its baseline across a host restart."""
        import os
        events=[];real_sync=os.fsync;real_save=self.broker._save_work
        def sync(fd):
            events.append(os.readlink("/proc/self/fd/"+str(fd)))
            return real_sync(fd)
        def save():
            baseline=next(self.root.rglob("baseline.tar"))
            self.assertIn(str(baseline),events)
            self.assertIn(str(baseline.parent),events[events.index(str(baseline))+1:])
            self.assertEqual(fixtures.host.context_digest(baseline.read_bytes())[0],
                fixtures.host.context_digest(self.runner.blob)[0])
            return real_save()
        with patch.object(fixtures.host.os,"fsync",side_effect=sync), patch.object(self.broker,"_save_work",side_effect=save):
            self.begin("durable-baseline")

    def test_old_retention_clearance_cannot_release_source_ownership(self):
        """A finished image and legacy cleanup receipt cannot delete an agent's retained correction source."""
        item=self.begin("retention-owner");self.write(item,"README.md","retained correction\n")
        self.publish(item)
        self.assertFalse(self.broker.scheduler.retention_allowed(item["key"]))
        self.assertFalse(self.broker.scheduler.retention_allowed("f"*64))
        current=self.broker.scheduler.store.get(item["key"])
        self.assertEqual(current["state"],"owned")

    def test_dispatch_starts_unrelated_jobs_and_holds_same_module(self):
        """The real admission loop bypasses a held engine job to dispatch independent source work."""
        engine=self.begin("dispatch-engine");other=self.begin("dispatch-ui")
        self.write(engine,"src/dispatch_fixture.rs","// engine fixture\n")
        self.write(other,"ui/main-panel/dispatch-fixture.md","UI fixture\n")
        def enqueue(item,operation,order):
            folder=self.broker.state/str(uuid.uuid4());folder.mkdir()
            blob=self.broker.scheduler_task_snapshot(Path(item["workspace_path"]).parent)
            (folder/"base.tar").write_bytes(self.runner.blob);(folder/"task.tar").write_bytes(blob)
            current=self.broker.scheduler.store.get(item["key"])
            job={"job_id":folder.name,"task_id":item["task_id"],"user_id":item["user_id"],
                "workspace":item["workspace"],"isolated":True,"status":"queued","operation":operation,
                "created_at":order,"lease_until":time.time()+120,"ownership_id":item["key"],
                "ownership_epoch":current["epoch"]}
            self.broker.jobs[job["job_id"]]=job
            return job
        first=enqueue(engine,"build_deploy",1)
        blocked=enqueue(engine,"build_deploy",2)
        independent=enqueue(other,"plugin_deploy",3)
        with patch.object(self.broker.pool,"submit")as submitted, patch.object(self.broker,"_sync_job",return_value=True):
            with self.broker.lock:self.broker._start_next()
            self.assertEqual(submitted.call_count,2)
            admitted={call.args[1]["job_id"]for call in submitted.call_args_list}
            self.assertEqual(admitted,{first["job_id"],independent["job_id"]})
            self.assertEqual(self.broker.active_jobs,admitted)
            self.assertNotIn(blocked["job_id"],self.broker.active_jobs)
            self.assertEqual(blocked["status"],"queued")
        self.broker.active_jobs.clear();self.broker.active=None

    def test_cargo_check_retains_its_claims_after_execution(self):
        """A durable check returns promptly, replays one job, and retains its owner claims after execution."""
        import threading
        item=self.begin("engine-check-owner");other=self.begin("engine-check-waiter")
        tree=Path(item["workspace_path"]).parent
        before=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))
        entered=threading.Event();release=threading.Event();futures=[]
        original_submit=self.broker.pool.submit
        def submit(*args):
            future=original_submit(*args);futures.append(future);return future
        def compile(*args):
            entered.set()
            if not release.wait(5):raise TimeoutError("fixture compiler barrier")
            return "check passed"
        with patch.object(self.runner,"cargo_task",create=True,side_effect=compile)as compiler, patch.object(self.broker.pool,"submit",side_effect=submit):
            args={"task_id":item["task_id"],"task":"check","expected_inputs_sha256":before,"request_id":"retained-check-request"}
            try:
                result=self.call("v3_cargo_task",args)
                self.assertEqual(result["status"],"queued")
                self.assertTrue(entered.wait(2))
                self.assertEqual(self.call("v3_cargo_task",args)["job_id"],result["job_id"])
                with self.assertRaisesRegex(ValueError,"reused"):
                    self.call("v3_cargo_task",{**args,"task":"test"})
                job=self.broker.jobs[result["job_id"]]
                self.assertEqual(job["ownership_id"],item["key"])
                self.assertEqual(item["last_check_job_id"],job["job_id"])
                held=self.broker.scheduler.store.get(item["key"])["scopes"]
                self.assertIn("module:workspace",held);self.assertNotIn("module:engine",held)
                other_tree=Path(other["workspace_path"]).parent
                other_digest=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(other_tree))
                with self.assertRaises(fixtures.host.scheduler_module.Conflict):
                    self.call("v3_cargo_task",{"task_id":other["task_id"],"task":"check","expected_inputs_sha256":other_digest})
            finally:release.set()
            futures[0].result(timeout=5)
            completed=self.call("v3_cargo_task",args)
            self.assertEqual(completed["status"],"completed")
            self.assertEqual(completed,self.call("v3_cargo_task",args))
            self.assertEqual(job["rust_tests"]["status"],"not_run")
            self.assertEqual(compiler.call_count,1)

    def test_checks_on_same_engine_are_queued_without_occupying_workers(self):
        """Checks share build dispatch reservations and retain responsive queue receipts."""
        import threading
        item=self.begin("sequenced-engine-check");entered=threading.Event();release=threading.Event()
        tree=Path(item["workspace_path"]).parent
        before=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))
        def compile(*args):
            entered.set()
            if not release.wait(5):raise TimeoutError("fixture compiler barrier")
            return "passed"
        with patch.object(self.runner,"cargo_task",create=True,side_effect=compile)as compiler:
            first=self.call("v3_cargo_task",{"task_id":item["task_id"],"task":"check",
                "expected_inputs_sha256":before,"request_id":"sequenced-check-first"})
            try:
                self.assertTrue(entered.wait(2))
                second=self.call("v3_cargo_task",{"task_id":item["task_id"],"task":"check",
                    "expected_inputs_sha256":before,"request_id":"sequenced-check-second"})
                self.assertEqual(self.broker.jobs[second["job_id"]]["status"],"queued")
                self.assertNotIn(second["job_id"],self.broker.active_jobs)
                self.assertEqual(compiler.call_count,1)
                status=self.call("build_work",{"action":"status","task_id":item["task_id"]})
                self.assertEqual(status["own"][0]["last_check_job_id"],second["job_id"])
            finally:release.set()
            deadline=time.monotonic()+3
            while time.monotonic()<deadline and self.broker.jobs[second["job_id"]]["status"]in {"queued","running"}:time.sleep(.01)
            self.assertEqual(self.broker.jobs[second["job_id"]]["status"],"completed")
            self.assertEqual(compiler.call_count,2)
    def test_file_request_ids_are_scoped_to_each_task(self):
        """Different tasks may use the same stable edit ID without failing after source effects."""
        for label,path in [("request-scope-first","docs/first.md"),("request-scope-second","ui/second/file.md")]:
            item=self.begin(label);tree=Path(item["workspace_path"]).parent
            digest=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))
            result=self.call("file_write",{"task_id":label,"path":path,"content":"independent\n",
                "expected_inputs_sha256":digest,"request_id":"same-edit-request"})
            self.assertEqual(result["status"],"success")

    def test_implicit_source_write_creates_one_draft_and_replays(self):
        """Source writes without a task create a stable isolated task, leaving live source untouched."""
        args={"path":"README.md","content":"implicit draft\n","request_id":"implicit-fixture-write"}
        result=self.call("file_write",args)
        self.assertTrue(result["task_created"]);self.assertTrue(result["task_id"].startswith("auto-source-"))
        self.assertEqual(result,self.call("file_write",args))
        self.assertEqual(self.runner.blob,fixtures.context(extra={"v3/README.md":"original\n"}))
        self.assertEqual(len(self.broker.work),1)

    def test_current_state_allows_owned_scope_expansion(self):
        """Creation receipts must not reset ownership to revision zero."""
        item=self.begin("current-state-task");self.write(item,"README.md","first\n")
        self.write(item,"docs/scheduler.md","second\n")
        current=self.broker.scheduler.ensure(item,self.trusted,self.workspace)
        self.assertEqual(current["generation"],2);self.assertIn("path:docs/scheduler.md",current["scopes"])
    def test_conflicting_write_has_no_source_effect(self):
        """An unrelated task cannot edit a held module before explicit completion."""
        first=self.begin("owner-first-task");second=self.begin("owner-second-task")
        self.write(first,"README.md","first\n")
        with self.assertRaises(fixtures.host.scheduler_module.Conflict)as rejected:self.write(second,"README.md","second\n")
        self.assertEqual(rejected.exception.details["kind"],"ownership_conflict")
        self.assertEqual(Path(second["workspace_path"],"README.md").read_text(),"original\n")
    def test_completed_build_retains_claims_until_verified_completion(self):
        """Terminal jobs do not free code; exact release acceptance and explicit completion do."""
        item=self.begin("completion-task");other=self.begin("waiting-task")
        self.write(item,"README.md","published\n");job,commit=self.publish(item)
        blocked,_=self.action(other["task_id"],"reserve",paths=["README.md"]);self.assertFalse(blocked["acquired"])
        result,accept=self.action(item["task_id"],"acceptance",job_id=job["job_id"],commit=commit,acceptance=[{"check":"fixture acceptance","passed":True}])
        self.assertEqual(result["state"],"validating");self.assertEqual(self.call("build_work",accept),result)
        self.action(item["task_id"],"heartbeat")
        self.assertEqual(self.broker.scheduler.store.get(item["key"])["state"],"validating")
        with patch.object(self.broker,"_start_next"):
            done,request=self.action(item["task_id"],"complete")
            self.assertEqual(self.call("build_work",request),done)
        self.assertTrue(self.broker.scheduler.retention_allowed(item["key"]))
        available,_=self.action(other["task_id"],"reserve",paths=["README.md"]);self.assertTrue(available["acquired"])
        self.assertEqual(Path(other["workspace_path"],"README.md").read_text(),"published\n")
    def test_release_claims_with_commit_completes_deployed_task(self):
        """One verified release of every claim frees the code and closes the task."""
        item=self.begin("release-close-task");other=self.begin("release-waiting-task")
        self.write(item,"README.md","published\n");job,commit=self.publish(item)
        value=self.broker.scheduler.store.get(item["key"])
        request={"action":"release_claims","task_id":item["task_id"],"scopes":value["scopes"],"reason":"Deployed and committed",
            "commit":commit,"request_id":"release-close","expected_revision":value["revision"],"expected_epoch":value["epoch"]}
        with patch.object(self.broker,"_start_next"):
            done=self.call("build_work",request)
            self.assertEqual(self.call("build_work",request),done)
        self.assertEqual((done["state"],done["scopes"]),("complete",[]))
        self.assertEqual(done["evidence"]["deployment"]["commit"],commit)
        self.assertTrue(self.broker.scheduler.retention_allowed(item["key"]))
        available,_=self.action(other["task_id"],"reserve",paths=["README.md"]);self.assertTrue(available["acquired"])
    def test_acceptance_allows_content_identical_path_absent_from_commit(self):
        """A mode-only released path has nothing to commit; a real change still must be committed."""
        self.runner.blob=fixtures.context(extra={"v3/README.md":"original\n","v3/docs/keep.md":"same\n"})
        item=self.begin("identical-path-task");self.write(item,"README.md","published\n")
        tree=Path(item["workspace_path"]).parent;base=(tree/"baseline.tar").read_bytes()
        job,_=self.publish(item);(self.broker.state/job["job_id"]/"base.tar").write_bytes(base)
        live=self.runner.source
        def git(*args):return subprocess.run(["git","-C",str(live),*args],stdout=subprocess.PIPE,check=True).stdout.decode().strip()
        git("add","docs/keep.md");git("commit","-qm","seed unchanged file")
        (live/"README.md").write_text("published\n");commit=git("rev-parse","HEAD")
        job["changed_paths"]=["v3/README.md","v3/docs/keep.md"]
        with self.assertRaisesRegex(ValueError,"Release path absent"):
            self.action(item["task_id"],"acceptance",job_id=job["job_id"],commit=commit,acceptance=[{"check":"fixture","passed":True}])
        (live/"README.md").write_text("other\n");git("commit","-qam","unrelated");(live/"README.md").write_text("published\n");git("commit","-qam","release")
        result,_=self.action(item["task_id"],"acceptance",job_id=job["job_id"],commit=git("rev-parse","HEAD"),acceptance=[{"check":"fixture","passed":True}])
        self.assertEqual(result["state"],"validating")
    def test_changed_promoted_source_rejects_completion(self):
        """Fresh completion validation catches an edit made after acceptance."""
        item=self.begin("changed-promoted-task");self.write(item,"README.md","published\n");job,commit=self.publish(item)
        self.action(item["task_id"],"acceptance",job_id=job["job_id"],commit=commit,acceptance=[{"check":"fixture","passed":True}])
        (self.runner.source/"README.md").write_text("later change\n")
        with self.assertRaisesRegex(ValueError,"Promoted source changed"):self.action(item["task_id"],"complete")
        self.assertEqual(self.broker.scheduler.store.get(item["key"])["state"],"validating")
    def failed_generation(self,label):
        """Freeze an unpublished draft without changing the published fixture."""
        item=self.begin(label);self.write(item,"README.md","unpublished correction\n")
        tree=Path(item["workspace_path"]).parent;blob=self.broker.scheduler_task_snapshot(tree)
        folder=self.broker.state/str(uuid.uuid4());folder.mkdir();(folder/"task.tar").write_bytes(blob)
        current=self.broker.scheduler.store.get(item["key"])
        job={"job_id":folder.name,"task_id":label,"user_id":item["user_id"],"workspace":item["workspace"],
            "status":"failed","effects_started":False,"ownership_id":item["key"],"source_generation":current["generation"]}
        self.broker.jobs[job["job_id"]]=job;item.update(status="closed",job_id=job["job_id"]);self.broker._save_work()
        return item,job,blob,(tree/"baseline.tar").read_bytes()

    def test_failed_continuation_preserves_draft_baseline_and_receipt(self):
        """Correction retains unpublished bytes and claims while fencing the failed generation."""
        item,job,blob,baseline=self.failed_generation("failed-correction")
        before=self.broker.scheduler.store.get(item["key"]);receipt=dict(job)
        result,request=self.action(item["task_id"],"continue")
        tree=Path(item["workspace_path"]).parent;archive=self.broker.work_dir/"generations"/item["key"]/job["job_id"]
        self.assertEqual(self.broker.scheduler_task_snapshot(tree),blob)
        self.assertEqual((tree/"baseline.tar").read_bytes(),baseline)
        self.assertEqual(self.broker.scheduler_task_snapshot(archive),blob)
        self.assertEqual((archive/"baseline.tar").read_bytes(),baseline)
        self.assertEqual(self.broker.jobs[job["job_id"]],receipt)
        self.assertEqual(self.runner.blob,fixtures.context(extra={"v3/README.md":"original\n"}))
        self.assertEqual(result,self.call("build_work",request))
        self.assertEqual(result["ownership"]["scopes"],before["scopes"])
        self.assertGreater(result["ownership"]["generation"],before["generation"])
        self.write(item,"README.md","fixed unpublished correction\n")

    def test_failed_continuation_recovers_after_filesystem_switch(self):
        """An interrupted failed-draft correction reuses the original generation switch."""
        item,job,blob,baseline=self.failed_generation("failed-switch")
        original=self.broker.scheduler.store.edited;once=[True]
        def after_commit(*args):
            result=original(*args)
            if once[0]:once[0]=False;raise OSError("fixture failed switch")
            return result
        current=self.broker.scheduler.store.get(item["key"])
        request={"action":"continue","task_id":item["task_id"],"request_id":"failed-switch-fixture",
            "expected_revision":current["revision"],"expected_epoch":current["epoch"]}
        with patch.object(self.broker.scheduler.store,"edited",side_effect=after_commit):
            with self.assertRaisesRegex(OSError,"failed switch"):self.call("build_work",request)
        result=self.call("build_work",request)
        self.assertEqual(result,self.call("build_work",request))
        tree=Path(item["workspace_path"]).parent
        self.assertEqual(self.broker.scheduler_task_snapshot(tree),blob)
        self.assertEqual((tree/"baseline.tar").read_bytes(),baseline)
        self.assertEqual(item["previous_jobs"],[job["job_id"]])

    def test_failed_orphan_continuation_reconciliation_preserves_unpublished_source(self):
        """Legacy post-switch recovery binds the failed draft separately from its merge baseline."""
        item,job,blob,baseline=self.failed_generation("failed-orphan-correction")
        scheduler=self.broker.scheduler;store=scheduler.store
        with store.transaction()as db:store.update(db,store.get(item["key"],db),state="orphaned")
        value=store.get(item["key"]);owner=scheduler.owner(self.trusted,self.workspace)
        item["base_inputs_sha256"]=self.broker.scheduler_digest(baseline)
        original={"action":"continue","task_id":item["task_id"],"request_id":"failed-orphan-switch",
            "expected_revision":value["revision"],"expected_epoch":value["epoch"]}
        path=scheduler.journal_path(owner,original["request_id"],item["key"])
        digest=hashlib.sha256(fixtures.host.scheduler_module.ownership.canonical([owner,original]).encode()).hexdigest()
        record={"digest":digest,"state":"pending","request_id":original["request_id"],"task_key":item["key"],"value":value,"previous_job":job["job_id"]}
        scheduler.write_record(path,record)
        with self.assertRaisesRegex(ValueError,"Editing ownership"):
            scheduler.continuation(item,original,value,record,path,owner,value["revision"],value["epoch"],"failed-orphan-fixture")
        request={"action":"reconcile_continuation","task_id":item["task_id"],"original_request_id":original["request_id"],
            "expected_transition_sha256":digest,"expected_revision":value["revision"],"expected_epoch":value["epoch"],"request_id":"failed-orphan-recovery"}
        result=self.call("build_work",request)
        self.assertEqual(result,self.call("build_work",request))
        tree=Path(item["workspace_path"]).parent
        self.assertEqual(self.broker.scheduler_task_snapshot(tree),blob)
        self.assertEqual((tree/"baseline.tar").read_bytes(),baseline)
        self.assertTrue(result["source_preserved"]);self.assertFalse(result["deployment_replayed"])
        self.assertEqual(job["status"],"failed");self.assertEqual(result["ownership"]["scopes"],value["scopes"])

    def test_failed_continuation_rejects_effects_unsettled_or_changed_draft(self):
        """No uncertain publication or changed frozen draft can become editable through recovery."""
        item,job,blob,baseline=self.failed_generation("failed-reject")
        for status,effects in (("failed",True),("running",False),("uncertain",False),("interrupted",False)):
            job.update(status=status,effects_started=effects)
            with self.assertRaisesRegex(ValueError,"pre-effect failed"):self.action(item["task_id"],"continue")
        job.update(status="failed",effects_started=False)
        Path(item["workspace_path"],"README.md").write_text("unexpected draft mutation\n")
        with self.assertRaisesRegex(ValueError,"Retained source generation changed"):self.action(item["task_id"],"continue")
        self.assertEqual(item["status"],"closed")

    def test_continue_retry_recovers_after_filesystem_switch(self):
        """A crash between ownership mutation and task persistence preserves one generation switch."""
        item=self.begin("continue-task");self.write(item,"README.md","published\n");self.publish(item)
        original=self.broker.scheduler.store.edited
        once=[True]
        def after_commit(*args):
            result=original(*args)
            if once[0]:once[0]=False;raise OSError("fixture crash after store commit")
            return result
        current=self.broker.scheduler.store.get(item["key"])
        request={"action":"continue","task_id":item["task_id"],"request_id":"recover-generation-fixture",
            "expected_revision":current["revision"],"expected_epoch":current["epoch"]}
        with patch.object(self.broker.scheduler.store,"edited",side_effect=after_commit):
            with self.assertRaisesRegex(OSError,"fixture crash"):self.call("build_work",request)
        result=self.call("build_work",request);self.assertEqual(result["status"],"open")
        self.assertEqual(len(item["previous_jobs"]),1)
        self.assertEqual(result,self.call("build_work",request))
        self.write(item,"README.md","correction\n")
        self.assertGreater(self.broker.scheduler.store.get(item["key"])["generation"],current["generation"])
    def test_reserve_retry_recovers_after_committed_refresh_failure(self):
        """A post-commit filesystem error retains the original CAS intent for recovery."""
        item=self.begin("reserve-refresh-task")
        current=self.broker.scheduler.store.get(item["key"])
        request={"action":"reserve","task_id":item["task_id"],"paths":["README.md"],
            "request_id":"reserve-refresh-recovery","expected_revision":current["revision"],
            "expected_epoch":current["epoch"]}
        with patch.object(self.broker.scheduler,"mark_waiter_refresh",side_effect=ValueError("fixture refresh failure")):
            with self.assertRaisesRegex(ValueError,"refresh failure"):self.call("build_work",request)
        self.assertIsNotNone(self.broker.scheduler.pending(item["key"]))
        result=self.call("build_work",request)
        self.assertTrue(result["acquired"])
        self.assertIsNone(self.broker.scheduler.pending(item["key"]))
        self.assertEqual(result,self.call("build_work",request))
    def test_restart_migration_preserves_tracked_orphan_without_expanding(self):
        """A dormant active task cannot crash startup or lose its existing reservations."""
        item=self.begin("restart-orphan-migration");self.write(item,"README.md","held draft\n")
        before=self.broker.scheduler.store.get(item["key"])
        self.broker.scheduler.store.clock=lambda:time.time()+301
        self.broker.scheduler.store.orphan()
        orphan=self.broker.scheduler.store.get(item["key"])
        self.assertEqual(orphan["state"],"orphaned")
        self.broker.scheduler.migrate_legacy()
        after=self.broker.scheduler.store.get(item["key"])
        self.assertEqual(after,orphan)
        self.assertEqual(after["scopes"],before["scopes"])
        self.assertTrue(item["scheduler_classified"])
    def test_exact_request_reuse_rejects_changed_arguments(self):
        """An old receipt cannot authorize a different reservation body."""
        item=self.begin("request-reuse-task");_,request=self.action(item["task_id"],"reserve",paths=["README.md"])
        with self.assertRaisesRegex(ValueError,"Request ID reused"):self.call("build_work",{**request,"paths":["src/app.rs"]})
    def test_old_epoch_is_fenced_after_handoff(self):
        """An atomic followup transfer rejects writes from the old logical task."""
        item=self.begin("handoff-old-task");target=self.begin("handoff-new-task");self.write(item,"README.md","first\n")
        result,_=self.action(item["task_id"],"handoff",target_task_id=target["task_id"])
        self.assertEqual(result["previous"]["state"],"abandoned")
        with self.assertRaisesRegex(ValueError,"no longer open"):self.write(item,"README.md","stale\n")
        with self.assertRaises(PermissionError):
            self.broker.scheduler.store.assert_owned(item["key"],self.broker.scheduler.owner(self.trusted,self.workspace),1,[])
        self.write(target,"README.md","followup\n")
    def test_task_calls_renew_owned_heartbeat_without_reviving_orphan(self):
        """P10: any authenticated call naming an owned task is liveness; orphaned ownership still needs recovery."""
        item=self.begin("liveness-owner");self.write(item,"README.md","held\n")
        store=self.broker.scheduler.store;stale=time.time()-400
        with store.transaction()as db:db.execute("UPDATE tasks SET heartbeat=? WHERE id=?",(stale,item["key"]))
        revision=store.get(item["key"])["revision"]
        self.call("build_work",{"action":"status","task_id":item["task_id"]})
        value=store.get(item["key"]);self.assertGreater(value["heartbeat"],stale+300);self.assertEqual(value["revision"],revision)
        with store.transaction()as db:db.execute("UPDATE tasks SET heartbeat=?,state='orphaned' WHERE id=?",(stale,item["key"]))
        self.call("build_work",{"action":"status","task_id":item["task_id"]})
        value=store.get(item["key"]);self.assertEqual(value["state"],"orphaned");self.assertEqual(value["heartbeat"],stale)

if __name__=="__main__":unittest.main()
