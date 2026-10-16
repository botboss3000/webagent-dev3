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
        self.post_review(job);self.complete(job)

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
