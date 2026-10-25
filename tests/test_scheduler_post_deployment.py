"""Deployment completes before metadata; immutable source and task completion stay verified."""
import json
from pathlib import Path
import threading
import subprocess
import time
import unittest
from unittest.mock import patch
import test_scheduler_review_policy as review_fixtures
import test_build_ui_publication as publication_fixtures

fixtures=publication_fixtures.fixtures


class PostDeploymentTests(unittest.TestCase):
    """Exercise real broker entry points with delayed and failing isolated index workers."""
    setUp=review_fixtures.DeferredDocumentationTests.setUp
    tearDown=review_fixtures.DeferredDocumentationTests.tearDown
    call=review_fixtures.DeferredDocumentationTests.call
    change=review_fixtures.DeferredDocumentationTests.change
    finish=review_fixtures.DeferredDocumentationTests.finish
    wait=review_fixtures.DeferredDocumentationTests.wait
    post_review=review_fixtures.DeferredDocumentationTests.post_review
    complete=review_fixtures.DeferredDocumentationTests.complete

    def configure(self, gate=None, failure=None):
        """Replace only fixture indexing, retaining source validation and publication gates."""
        self.broker.scheduler.config["scheduler"].update(documentation_review_order="after_deployment",post_deployment_indexing=True)
        self.index_started=threading.Event();self.index_calls=[]
        def stage(blob,job_id,asset_root):
            destination=fixtures.host.release.stage_assets(asset_root,job_id,blob,fixtures.host.context_digest)
            fixtures.host.release.write_source_receipt(destination,job_id,fixtures.host.context_digest(blob)[0])
            return destination
        def index(destination,blob,job_id,**kwargs):
            self.index_started.set();self.index_calls.append(job_id)
            if gate is not None and not gate.wait(5):raise TimeoutError("Fixture index wait")
            if failure is not None:failure()
            if (Path(destination)/".code-index/index.sqlite").is_file():
                fixtures.host.release.verify_index(destination,job_id,fixtures.host.context_digest(blob)[0]);return destination
            self.runner.scanner.build_published(destination,Path(destination)/".code-index",job_id,fixtures.host.context_digest(blob)[0],maintenance=self.broker._code_index_snapshot(job_id))
            return destination
        self.runner._stage=stage;self.runner.index_assets=index

    def index_state(self, job, expected):
        """Wait only for the independent metadata result, never resume deployment."""
        end=time.monotonic()+6
        while time.monotonic()<end:
            state=self.broker.jobs[job].get("post_deployment",{}).get("index_status")
            if state==expected:return
            time.sleep(.01)
        self.fail(str(self.broker.jobs[job].get("post_deployment")))

    def test_deployment_and_restart_never_start_indexing(self):
        self.configure()
        self.change("deferred-doc-task","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        job=self.finish("deferred-doc-task")
        self.assertEqual(self.wait([job])[0]["status"],"completed",self.broker.jobs[job].get("reason"))
        self.broker.post_deployment.resume()
        self.assertFalse(self.index_calls)
        folder=self.runner.assets/"releases"/job
        self.assertFalse((folder/".code-index/index.sqlite").exists())
        fixtures.host.release.verify_publication(folder,job,self.broker.jobs[job]["inputs_sha256"])
        before=list(self.runner.calls)
        request={"action":"retry_post_deployment","task_id":"deferred-doc-task","job_id":job,"request_id":"explicit-agent-index"}
        result=self.call("build_work",request)
        self.index_state(job,"indexed")
        self.assertEqual(before,self.runner.calls)
        self.assertEqual(result,self.call("build_work",request))
        self.assertNotIn("deferred_documentation",self.broker.jobs[job])

    def test_post_deployment_recovers_missing_candidate_from_attested_execution_source(self):
        self.configure()
        self.change("deferred-doc-task", "ui/probe/a.css", "body {}\n", "body { color: red; }\n", review=False)
        job=self.finish("deferred-doc-task")
        self.assertEqual(self.wait([job])[0]["status"],"completed")
        candidate=self.broker.state/job/"candidate.tar";blob=candidate.read_bytes();candidate.unlink()
        source=self.broker.state/"execution"/job/"source"
        fixtures.host.release.extract(source,blob,fixtures.host.context_digest)
        result=self.call("build_work", {"action":"retry_post_deployment","task_id":"deferred-doc-task","job_id":job,"request_id":"recover-missing-candidate-001"})
        self.index_state(job,"indexed")
        self.assertFalse(result["deployment_replayed"])
        self.assertEqual(fixtures.host.context_digest(candidate.read_bytes())[0],self.broker.jobs[job]["inputs_sha256"])

    def test_two_deployments_finish_without_any_automatic_index(self):
        self.configure()
        for task,path,text in (("post-index-alpha","ui/probe/a.css","body {}"),("post-index-beta","ui/probe/b.css","main {}")):
            self.change(task,path,text+"\n",text+" /* changed */\n",review=False)
            job=self.finish(task)
            self.assertEqual(self.wait([job])[0]["status"],"completed")
            self.assertFalse((self.runner.assets/"releases"/job/".code-index/index.sqlite").exists())
        self.assertFalse(self.index_calls)

    def test_functional_failure_prevents_deployment_and_index_admission(self):
        self.configure();before=self.runner.current_asset_id()
        self.change("deferred-doc-task","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        with patch.object(self.runner,"validate_ui",side_effect=ValueError("Required functional fixture failed")):
            job=self.finish("deferred-doc-task");self.assertEqual(self.wait([job])[0]["status"],"failed")
        self.assertEqual(before,self.runner.current_asset_id());self.assertFalse(self.index_calls)

    def test_source_receipt_covers_artifacts_excluded_from_the_index(self):
        self.configure()
        self.change("deferred-doc-task","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        original=self.runner.validate_ui
        def corrupt(job,changed,root=None):
            (Path(root)/"v3/ui/probe/opaque.bin").write_bytes(b"changed opaque bytes")
            return original(job,changed,root=root)
        before=self.runner.current_asset_id()
        with patch.object(self.runner,"validate_ui",side_effect=corrupt):
            job=self.finish("deferred-doc-task");self.assertEqual(self.wait([job])[0]["status"],"failed")
        self.assertEqual(before,self.runner.current_asset_id());self.assertFalse(self.index_calls)

    def test_post_deployed_write_reservations_release_without_losing_metadata_identity(self):
        store=self.broker.scheduler.store;owner='fixture-owner'
        task="a"*64;state=store.begin(task,owner,"metadata-scope-fixture","metadata-scope-begin")
        acquired=store.acquire(task,owner,state["revision"],state["epoch"],["module:broker","path:build-service/host.py"],"metadata-scope-acquire")["task"]
        evidence={"release_id":"11111111-1111-4111-8111-111111111111","commit":"c"*40,"generation":acquired["generation"]}
        released=store.release_deployed_scopes(task,owner,acquired["revision"],acquired["epoch"],acquired["scopes"],evidence,"metadata-scope-release")
        self.assertEqual(released["state"],"owned");self.assertEqual(released["scopes"],[])
        self.assertEqual(released["epoch"],acquired["epoch"])
        touched=store.heartbeat(task,owner,released["revision"],released["epoch"],"metadata-scope-heartbeat")
        self.assertEqual(touched["state"],"owned")
        with self.assertRaises(PermissionError):store.assert_owned(task,owner,touched["epoch"],["path:build-service/host.py"])


    def test_workspace_seed_uses_exact_source_while_index_is_pending(self):
        """Private source work can start immediately without waiting for the bootstrap index."""
        gate=threading.Event();self.configure(gate)
        try:
            self.change("deferred-doc-task","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
            job=self.finish("deferred-doc-task");self.assertEqual(self.wait([job])[0]["status"],"completed")
            support=fixtures.host.scheduler_module.sibling("workspace_support")
            support.LIMIT=1024*1024;support.administrator_plugin=lambda _name:self.runner.scanner
            workspace=self.root/"seed-workspace";workspace.mkdir()
            template=self.runner.assets/"releases"/job/"v3"
            result=support._seed_inner({"base":str(workspace),"template":str(template),"stage":"source-seed","published":True})
            self.assertIsNone(result["index_sha256"])
            self.assertEqual((workspace/"source-seed/ui/probe/a.css").read_text(),"body { color: red; }\n")
            self.assertTrue((workspace/"source-seed/.git").is_dir())
            self.assertFalse((workspace/"source-seed/.code-index").exists())
        finally:gate.set()
        self.assertFalse(self.index_calls)


class IndependentMaintenanceTests(unittest.TestCase):
    setUp=PostDeploymentTests.setUp
    tearDown=PostDeploymentTests.tearDown
    call=PostDeploymentTests.call
    configure=PostDeploymentTests.configure
    change=PostDeploymentTests.change
    finish=PostDeploymentTests.finish
    wait=PostDeploymentTests.wait
    def shipped(self):
        self.configure()
        self.change("deferred-doc-task","ui/probe/a.css","body {}\n","body { color:red; }\n",review=False)
        identity=self.finish("deferred-doc-task")
        self.assertEqual(self.wait([identity])[0]["status"],"completed")
        job=self.broker.jobs[identity];key=job["ownership_id"]
        with self.broker.scheduler.store.transaction() as db:
            value=self.broker.scheduler.store.get(key,db)
            self.broker.scheduler.store.update(db,value,state="complete",scopes=[],requested=[],evidence={})
        return identity
    def activity(self,identity):
        return self.call("build_work",{"action":"maintenance_begin","release_id":identity})["activity_id"]
    def submit(self,key,operation,request="maintenance-fixture-sync"):
        return self.call("build_work",{"action":"maintenance_submit","activity_id":key,"operation":operation,"request_id":request})
    def result(self,key,operation):
        until=time.monotonic()+6
        while time.monotonic()<until:
            value=self.call("build_work",{"action":"maintenance_receipt","activity_id":key,"operation_id":operation["operation_id"]})
            if value["status"] not in {"queued","running"}:return value
            time.sleep(.01)
        self.fail("Maintenance operation failed to settle")
    def test_closed_task_sync_and_receipts_do_not_need_original_lease(self):
        identity=self.shipped();key=self.activity(identity)
        calls=list(self.runner.calls)
        with self.broker.deploy_lock:
            queued=self.submit(key,{"action":"sync"})
            done=self.result(key,queued)
        self.assertEqual(done["status"],"completed",done)
        self.assertEqual(done["result"]["synced_paths"],["ui/probe/a.css"])
        self.assertEqual(self.broker.scheduler.store.get(self.broker.jobs[identity]["ownership_id"])["state"],"complete")
        self.assertEqual(calls,self.runner.calls)
        self.assertEqual(self.submit(key,{"action":"sync"})["operation_id"],queued["operation_id"])
        with self.assertRaisesRegex(ValueError,"reused"):self.submit(key,{"action":"list"})
        read=self.result(key,self.submit(key,{"action":"list"},"maintenance-fixture-list"))
        self.assertTrue(read["result"]["results"])
        row=read["result"]["results"][0]
        claim=self.result(key,self.submit(key,{"action":"claim","entry_id":row["entry_id"],"expected_revision":row["revision"]},"maintenance-fixture-claim"))
        self.assertEqual(claim["status"],"completed",claim)
        self.assertTrue(claim["result"]["record"]["claim"]["id"])
        receipt=self.call("build_work",{"action":"maintenance_receipt","activity_id":key,"operation_id":claim["operation_id"]})
        self.assertEqual(receipt["result"],claim["result"])
    def test_index_after_task_completion_has_no_source_claims(self):
        identity=self.shipped();key=self.activity(identity)
        done=self.result(key,self.submit(key,{"action":"index"},"maintenance-fixture-index"))
        self.assertEqual(done["status"],"completed",done)
        self.assertEqual(self.broker.jobs[identity]["post_deployment"]["index_status"],"indexed")
        value=self.broker.scheduler.store.get(self.broker.jobs[identity]["ownership_id"])
        self.assertEqual(value["state"],"complete");self.assertEqual(value["scopes"],[])
    def test_revocation_during_slow_sync_is_recoverable(self):
        identity=self.shipped();key=self.activity(identity)
        entered=threading.Event();proceed=threading.Event();original=self.broker.code_index_ledger.inventory
        def inventory(*args):
            entered.set();proceed.wait(3);return original(*args)
        self.broker.code_index_ledger.inventory=inventory
        queued=self.submit(key,{"action":"sync"})
        self.assertTrue(entered.wait(3))
        self.call("administrator_host_revoke",{})
        proceed.set()
        done=self.result(key,queued)
        self.assertEqual(done["status"],"interrupted")
        self.assertEqual(self.broker.scheduler.store.get(self.broker.jobs[identity]["ownership_id"])["state"],"complete")
    def test_restart_keeps_receipt_without_automatic_replay(self):
        identity=self.shipped();key=self.activity(identity)
        done=self.result(key,self.submit(key,{"action":"sync"}))
        activities=self.broker.maintenance_activities
        activities.close()
        replacement=fixtures.host.post_deployment_module.MaintenanceActivities(self.broker,fixtures.host.release,self.runner.assets)
        self.broker.maintenance_activities=replacement
        receipt=self.call("build_work",{"action":"maintenance_receipt","activity_id":key,"operation_id":done["operation_id"]})
        self.assertEqual(receipt["result"],done["result"])
        self.assertFalse(replacement.active)
    def test_scope_and_owner_cannot_be_overridden(self):
        identity=self.shipped();key=self.activity(identity)
        bad=self.result(key,self.submit(key,{"action":"sync","scope":"src"}))
        self.assertEqual(bad["status"],"interrupted")
        with patch.dict(self.trusted,user_id="someone-else"):
            with self.assertRaises(PermissionError):self.call("build_work",{"action":"maintenance_status","activity_id":key})

    def test_project_finalizes_without_index_or_reviews(self):
        identity=self.shipped()
        self.assertFalse((self.runner.assets/"releases"/identity/".code-index/index.sqlite").exists())
        project="maintenance-project-fixture"
        created=self.call("build_work",{"action":"index_project_create","project_id":project,"request_id":"maintenance-project-create"})
        attached=self.call("build_work",{"action":"index_project_attach","project_id":project,"task_id":"deferred-doc-task","request_id":"maintenance-project-attach","expected_revision":created["revision"]})
        done=self.call("build_work",{"action":"index_project_finalize","project_id":project,"request_id":"maintenance-project-finalize","expected_revision":attached["revision"]})
        self.assertEqual(done["state"],"finalized",done)
        self.assertEqual(done["final_receipt"]["metadata_status"],"separate_maintenance")
    def test_stale_source_cannot_receive_review_evidence(self):
        identity=self.shipped();key=self.activity(identity)
        self.assertEqual(self.result(key,self.submit(key,{"action":"sync"}))["status"],"completed")
        source=self.runner.assets/"releases"/identity/"v3/ui/probe/a.css"
        source.write_text("tampered source")
        value=self.result(key,self.submit(key,{"action":"list"},"stale-source-review-read"))
        self.assertEqual(value["status"],"interrupted")
    def test_restart_marks_pending_operation_interrupted(self):
        identity=self.shipped();key=self.activity(identity)
        activities=self.broker.maintenance_activities
        path=activities.folder/(key+".json");row=activities.read(path)
        row["operations"]["pending-fixture"]={"operation_id":"pending-fixture","status":"running"}
        activities.save(row);activities.close()
        self.broker.maintenance_activities=fixtures.host.post_deployment_module.MaintenanceActivities(self.broker,fixtures.host.release,self.runner.assets)
        receipt=self.call("build_work",{"action":"maintenance_receipt","activity_id":key,"operation_id":"pending-fixture"})
        self.assertEqual(receipt["status"],"interrupted")
        self.assertFalse(self.broker.maintenance_activities.active)
