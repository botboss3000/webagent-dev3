"""Full broker entry points exercise private staging and real immutable index activation."""
import difflib
import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import time
import unittest
import uuid
from unittest.mock import patch
import test_admin_build as fixtures


class PreparedRunner(fixtures.FakeRunner):
    def __init__(self,assets):
        super().__init__();self.assets=assets;self.hot_assets=True
        self.blob=fixtures.context(extra={"v3/ui/index.html":"<main>fixture</main>\n","v3/plugins/engine/fixture/component.json":json.dumps({"artifact_sha256":"1"*64}),"v3/src/main.rs":"fn main() {}\n","v3/ui/probe/a.css":"body {}\n","v3/ui/probe/b.css":"main {}\n"})
        self.seal_descriptors=False;self.barrier=None;self.prepared=[];self.once=set();self.guard=threading.Lock()
        self.scanner=fixtures.host.maintenance_engine.load_scanner()
        self.asset_id=str(uuid.uuid4())
        self._stage(self.blob,self.asset_id,self.assets)
        fixtures.host.release.activate_assets(self.assets,self.asset_id)

    def build_ability_artifacts(self,job_id,source_root=None):
        """This fixture contains no compiled ability packages."""
        return []

    def preflight_deploy_images(self,images,job_id):
        """Model the already-installed image-pair gate without Docker effects."""
        if set(images)!={"api","loop"}:raise ValueError("Exact deployment pair required")
        return dict(images)

    def core_compile_inputs(self):return {"v3/src/main.rs"}

    def _stage(self,blob,job_id,root):
        destination=fixtures.host.release.stage_assets(root,job_id,blob,fixtures.host.context_digest)
        descriptor=destination/"v3/plugins/engine/fixture/component.json"
        if self.seal_descriptors:descriptor.write_text(json.dumps({"artifact_sha256":"2"*64}))
        provider=getattr(self,"maintenance_provider",lambda _job:[])
        self.scanner.build_published(destination,destination/".code-index",job_id,fixtures.host.context_digest(blob)[0],maintenance=provider(job_id))
        return destination

    def stage_assets(self,blob,job_id,*,asset_root=None,base_release_id=None):
        private=Path(asset_root)if asset_root is not None else self.assets
        with self.guard:
            first=job_id not in self.once;self.once.add(job_id)
            self.prepared.append((job_id,time.monotonic(),str(private)))
        if private!=self.assets:
            assert not(self.assets/"releases"/job_id).exists(),"Final release frozen before publication admission"
            if self.barrier is not None and first:self.barrier.wait(5)
        return self._stage(blob,job_id,private)

    def validate_ui(self,job_id,changed,*,root=None):
        self.calls.append(("validate_candidate",job_id,str(root)))
        assert Path(root or self.assets/"releases"/job_id).is_dir()

    def validate_selected_tests(self,job_id,root,evidence):
        assert not evidence["tests"],"Frontend fixture tests need an explicit test runtime fake"
        return {"status":"not_applicable","tests":[]}

    def current_asset_id(self):return fixtures.host.release.current_asset_id(self.assets)

    def activate_assets(self,job_id):
        fixtures.host.release.activate_assets(self.assets,job_id);self.asset_id=job_id
        if hasattr(self,"maintenance_activated"):self.maintenance_activated(job_id)


class ConcurrentUIEntryTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);self.sid=str(uuid.uuid4());self.serial=0
        self.policy=patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"scheduler":{**fixtures.host.CONFIG.get("scheduler",{}),"mode":"enforce","component_indexing":True,"max_phases":4,"ui_file_reservations":True,"max_ui_preparations":2,"concurrent_ui_preparation":True,"documentation_review_order":"before_deployment","post_deployment_indexing":False}})
        self.asset_patch=patch.object(fixtures.host,"ASSET_ROOT",self.root/"assets")
        self.policy.start();self.asset_patch.start()
        self.runner=PreparedRunner(self.root/"assets");self.broker=fixtures.host.Broker(self.root/"jobs",self.runner)
        self.trusted={"user_id":"codex-local","session_id":self.sid,"lease_until":time.time()+60,"access_fingerprint":"f"*64,"local_caller":True}

    def tearDown(self):
        self.broker.shutdown();self.asset_patch.stop();self.policy.stop();self.tmp.cleanup()

    def call(self,name,args):return self.broker.handle({"name":name,"arguments":args,"workspace":self.sid,"trusted":self.trusted})

    def change(self,task,path,before,after,review=True):
        self.call("v3_build_work",{"action":"begin","task_id":task,"summary":"Concurrent UI fixture","mode":"isolated"})
        capture=self.call("build_work",{"action":"review","task_id":task})
        diff="diff --git a/"+path+" b/"+path+"\n"+"".join(difflib.unified_diff(before.splitlines(True),after.splitlines(True),fromfile="a/"+path,tofile="b/"+path))
        self.call("v3_source_apply",{"task_id":task,"expected_inputs_sha256":capture["workspace_inputs_sha256"],"request_id":task+"-patch","patch":diff})
        if review:self.review(task,path)

    def review(self,task,path,prepared_job_id=None):
        context={"prepared_job_id":prepared_job_id}if prepared_job_id else {}
        suffix="-prepared"if prepared_job_id else""
        self.call("code_index_maintenance",{"action":"sync",**context,"task_id":task,"scope":path,"request_id":task+"-sync"+suffix})
        rows=self.call("code_index_maintenance",{"action":"list","task_id":task,"scope":path,"limit":100})["results"]
        for n,row in enumerate(sorted(rows,key=lambda r:r["unit_id"]=="__file__")):
            claimed=self.call("code_index_maintenance",{"action":"claim",**context,"task_id":task,"entry_id":row["entry_id"],"expected_revision":row["revision"],"request_id":task+"-claim-"+str(n)+suffix})["record"]
            self.call("code_index_maintenance",{"action":"record",**context,"task_id":task,"entry_id":row["entry_id"],"expected_revision":claimed["revision"],"source_hash":claimed["source_hash"],"claim_id":claimed["claim"]["id"],"status":"complete","checks":{k:"verified"for k in ("documentation","connections","breadcrumbs","configuration","index_freshness")},"configuration_assessment":row.get("configuration_assessment")if row.get("configuration_assessment",{}).get("status")=="configured"else{"status":"not_applicable","rationale":"Fixed immutable publication fixture"},"request_id":task+"-review-"+str(n)+suffix})

    def finish(self,task):
        review=self.call("build_work",{"action":"review","task_id":task})
        return self.call("v3_build_work",{"action":"finish","task_id":task,"expected_inputs_sha256":review["workspace_inputs_sha256"]})["job_id"]

    def wait(self,jobs):
        deadline=time.monotonic()+8
        while time.monotonic()<deadline:
            rows=[self.broker.jobs[job]for job in jobs]
            if all(row["status"]not in {"queued","running"}for row in rows):return rows
            time.sleep(.01)
        self.fail("Bounded UI publications did not finish")

    def test_disjoint_same_folder_prepares_overlap_and_both_survive_real_activation(self):
        self.change("ui-alpha-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.change("ui-beta-task","ui/probe/b.css","main {}\n","main { color: blue; }\n")
        self.runner.barrier=threading.Barrier(2)
        jobs=[self.finish("ui-alpha-task"),self.finish("ui-beta-task")]
        rows=self.wait(jobs)
        self.assertEqual([row["status"]for row in rows],["completed","completed"],[(row.get("phase"),row.get("reason"))for row in rows])
        active=self.runner.assets/"releases"/self.runner.current_asset_id()
        self.assertEqual((active/"v3/ui/probe/a.css").read_text(),"body { color: red; }\n")
        self.assertEqual((active/"v3/ui/probe/b.css").read_text(),"main { color: blue; }\n")
        self.assertGreaterEqual(len(self.runner.prepared),3)
        self.assertTrue(all("preparation-attempts"in path for _job,_at,path in self.runner.prepared))
        self.assertFalse(any(call[0]in {"build","stop","start"}for call in self.runner.calls))
        intervals=sorted((row["preparation"]["publication_lock_at"],row["finished_at"])for row in rows)
        self.assertLessEqual(intervals[0][1],intervals[1][0])

    def test_same_file_conflict_is_explicit_before_mutation(self):
        self.change("ui-alpha-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.call("v3_build_work",{"action":"begin","task_id":"ui-conflict-task","summary":"Conflicting fixture","mode":"isolated"})
        value=self.broker.scheduler.ensure(next(i for i in self.broker.work.values()if i["task_id"]=="ui-conflict-task"),self.trusted,self.sid)
        result=self.call("build_work",{"action":"reserve","task_id":"ui-conflict-task","paths":["ui/probe/a.css"],"expected_revision":value["revision"],"expected_epoch":value["epoch"],"request_id":"ui-explicit-conflict"})
        self.assertFalse(result["acquired"])

    def test_missing_reviews_wait_for_sealed_candidate_then_resume_same_job(self):
        self.change("ui-review-task","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        before=self.runner.current_asset_id();job=self.finish("ui-review-task");row=self.wait([job])[0]
        self.assertEqual(row["phase"],"awaiting_prepared_reviews",row.get("reason"))
        self.assertEqual(self.runner.current_asset_id(),before)
        self.assertFalse((self.runner.assets/"releases"/job).exists())
        self.review("ui-review-task","ui/probe/a.css",prepared_job_id=job)
        report=self.call("v3_build_work",{"action":"preflight","task_id":"ui-review-task"})["synchronization"]
        self.call("v3_build_work",{"action":"decide","task_id":"ui-review-task","sync_decision":"proceed","expected_sync_digest":report["digest"]})
        self.assertEqual(self.wait([job])[0]["status"],"completed")

    def test_review_wait_survives_ownership_timeout_without_resubmission(self):
        self.change("ui-review-lease","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        job=self.finish("ui-review-lease");row=self.wait([job])[0]
        self.assertEqual(row["review_wait"]["state"],"active")
        before=self.runner.current_asset_id();key=row["ownership_id"];future=time.time()+301
        self.trusted["lease_until"]=future+600
        with patch.object(fixtures.host.time,"time",lambda:future):
            self.broker.scheduler.housekeeping()
            self.assertEqual(self.broker.scheduler.store.get(key)["state"],"owned")
            self.review("ui-review-lease","ui/probe/a.css",prepared_job_id=job)
        self.assertEqual(self.runner.current_asset_id(),before);self.assertEqual(row["status"],"needs_decision")
        view=self.call("v3_worker_job",{"job_id":job})
        self.assertIn("expires_at",view["review_wait"]);self.assertNotIn("binding",view["review_wait"])

    def test_review_wait_receives_live_revocation(self):
        self.change("ui-review-revoke","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        job=self.finish("ui-review-revoke");row=self.wait([job])[0]
        result=self.call("administrator_host_revoke",{})
        self.assertGreaterEqual(result["updated"],1);self.assertTrue(row["revoked"])
        self.assertEqual(row["review_wait"]["state"],"authorization_expired")
        with self.assertRaises(PermissionError):self.review("ui-review-revoke","ui/probe/a.css",prepared_job_id=job)

    def test_review_wait_receives_fingerprint_changes(self):
        self.change("ui-review-policy","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        job=self.finish("ui-review-policy");row=self.wait([job])[0]
        self.trusted={**self.trusted,"access_fingerprint":"e"*64}
        self.call("administrator_host_lease",{})
        self.assertTrue(row["revoked"])

    def test_review_wait_expired_authority_is_not_revived(self):
        self.change("ui-review-expire","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        job=self.finish("ui-review-expire");row=self.wait([job])[0]
        expired=time.time()-1;row["lease_until"]=expired
        self.call("administrator_host_lease",{})
        self.assertEqual(row["lease_until"],expired)
        self.broker.scheduler.housekeeping()
        self.assertEqual(row["review_wait"]["state"],"authorization_expired")

    def test_descriptor_dependency_review_binds_after_sealing(self):
        metadata={"version":1,"feature":"sealed-fixture","aliases":["artifact metadata"],"purpose":"Render using sealed metadata","span_lines":1,"configuration":{"status":"configured","references":[{"path":"plugins/engine/fixture/component.json","pointers":["/artifact_sha256"]}],"opportunities":[]}}
        changed="/* @code-index "+json.dumps(metadata)+" */\nbody { color: red; }\n"
        self.change("ui-sealed-task","ui/probe/a.css","body {}\n",changed)
        self.runner.seal_descriptors=True
        job=self.finish("ui-sealed-task");row=self.wait([job])[0]
        self.assertEqual(row["phase"],"awaiting_prepared_reviews",row.get("reason"))
        self.review("ui-sealed-task","ui/probe/a.css",prepared_job_id=job)
        report=self.call("v3_build_work",{"action":"preflight","task_id":"ui-sealed-task"})["synchronization"]
        self.call("v3_build_work",{"action":"decide","task_id":"ui-sealed-task","sync_decision":"proceed","expected_sync_digest":report["digest"]})
        completed=self.wait([job])[0];self.assertEqual(completed["status"],"completed",completed.get("reason"))
        active=self.runner.assets/"releases"/job
        blob,_manifest=self.runner.scanner.read_published(active,job)
        import sqlite3
        conn=sqlite3.connect(":memory:");conn.deserialize(blob)
        rows=[json.loads(r[0])for r in conn.execute("SELECT body FROM maintenance_records WHERE file_path='v3/ui/probe/a.css'")];conn.close()
        self.assertTrue(rows);self.assertTrue(all(r["status"]=="complete"for r in rows))

    def recovery(self,task,job,action="restore_failed_ui",request="ui-recovery-exact-request"):
        return self.call("build_work",{"action":action,"task_id":task,"job_id":job,"request_id":request})

    def failed_ledger_activation(self):
        """Lose the shared projection update after the immutable pointer switched."""
        self.change("ui-ledger-recover","ui/probe/a.css","body {}\n","body { color: red; }\n")
        original=self.runner.maintenance_activated
        self.runner.maintenance_activated=lambda _:(_ for _ in ()).throw(RuntimeError("ledger storage full"))
        try:
            job=self.finish("ui-ledger-recover");row=self.wait([job])[0]
        finally:self.runner.maintenance_activated=original
        self.assertEqual(row["status"],"uncertain",row.get("reason"))
        self.assertEqual(self.runner.current_asset_id(),job)
        return job

    def test_exact_activated_release_completes_without_reactivation_and_retry_reuses_receipt(self):
        job=self.failed_ledger_activation();containers=json.loads(json.dumps(self.runner.containers))
        with patch.object(self.runner,"activate_assets",wraps=self.runner.activate_assets)as activate:
            result=self.recovery("ui-ledger-recover",job,"complete_activated_release")
            self.assertTrue(result["source_promoted"]);self.assertFalse(result["activation_replayed"])
            self.assertEqual(result,self.recovery("ui-ledger-recover",job,"complete_activated_release"));activate.assert_not_called()
        self.assertEqual(self.runner.containers,containers)
        self.assertEqual(self.broker.jobs[job]["status"],"completed")
        self.assertEqual(self.broker.code_index_ledger.active_release(),job)
        self.assertFalse(self.broker._has_unresolved_release())

    def test_ui_template_plugin_lane_retains_adoption_and_mode_checks(self):
        from unittest.mock import Mock
        job=self.failed_ledger_activation();self.broker.jobs[job]["operation"]="plugin_deploy"
        self.runner.verify_plugin_adoption=Mock(return_value={"revision":job})
        self.runner.engine_execution_modes=Mock(return_value={"api":"live","loop":"live"})
        result=self.recovery("ui-ledger-recover",job,"complete_activated_release")
        self.assertEqual(result["status"],"completed")
        self.runner.verify_plugin_adoption.assert_called_once_with(job)
        self.runner.engine_execution_modes.assert_called_once_with(job)

    def test_activated_completion_rejects_executable_plugin_inputs(self):
        job=self.failed_ledger_activation();row=self.broker.jobs[job];row["operation"]="plugin_deploy"
        row["changed_paths"].append("v3/plugins/abilities/fixture/runner.py")
        with self.assertRaisesRegex(ValueError,"activated UI recovery"):self.recovery("ui-ledger-recover",job,"complete_activated_release")
        self.assertEqual(row["status"],"uncertain")

    def test_activated_completion_rejects_advanced_pointer_and_expired_authority(self):
        job=self.failed_ledger_activation();row=self.broker.jobs[job]
        row["lease_until"]=0
        with self.assertRaises(PermissionError):self.recovery("ui-ledger-recover",job,"complete_activated_release")
        row["lease_until"]=time.time()+60
        newer=str(uuid.uuid4());self.runner._stage(self.runner.blob,newer,self.runner.assets);self.runner.activate_assets(newer)
        with self.assertRaisesRegex(ValueError,"pointer advanced"):self.recovery("ui-ledger-recover",job,"complete_activated_release")
        self.assertEqual(self.runner.current_asset_id(),newer);self.assertEqual(row["status"],"uncertain")

    def test_activated_completion_preserves_promotion_conflicts_and_retries_exact_intent(self):
        job=self.failed_ledger_activation();original=self.runner.promote_checkout
        with patch.object(self.runner,"promote_checkout",side_effect=ValueError("checkout changed during release")):
            with self.assertRaisesRegex(ValueError,"checkout changed"):self.recovery("ui-ledger-recover",job,"complete_activated_release")
        self.assertEqual(self.broker.jobs[job]["status"],"uncertain")
        with patch.object(self.runner,"promote_checkout",wraps=original)as promote:
            result=self.recovery("ui-ledger-recover",job,"complete_activated_release")
            self.assertEqual(result["status"],"completed");self.assertEqual(promote.call_count,1)

    def test_failed_activation_restores_only_its_predecessor_and_retry_is_exact(self):
        before=self.runner.current_asset_id()
        self.change("ui-failure-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.runner.fail_verify_ui=True
        job=self.finish("ui-failure-task");row=self.wait([job])[0]
        self.assertEqual(row["status"],"uncertain",row.get("reason"))
        self.assertEqual(self.runner.current_asset_id(),job)
        self.runner.fail_verify_ui=False
        result=self.recovery("ui-failure-task",job)
        self.assertTrue(result["effects_reconciled"])
        self.assertEqual(self.runner.current_asset_id(),before)
        self.assertEqual(result,self.recovery("ui-failure-task",job))
        self.assertEqual(self.broker.jobs[job]["status"],"failed")
        self.assertFalse(self.broker._has_unresolved_release())
        active=self.runner.assets/"releases"/before
        self.assertEqual((active/"v3/ui/probe/a.css").read_text(),"body {}\n")

    def test_installed_host_failure_restores_predecessor_without_runtime_restart(self):
        before=self.runner.current_asset_id()
        self.change("host-restore-fixture","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.runner.fail_verify_ui=True
        job=self.finish("host-restore-fixture");self.wait([job])
        self.assertEqual(self.broker.jobs[job]["status"],"uncertain")
        self.broker.jobs[job]["operation"]="host_service_deploy"
        self.runner.fail_verify_ui=False;calls=list(self.runner.calls)
        result=self.recovery("host-restore-fixture",job)
        self.assertTrue(result["effects_reconciled"])
        self.assertEqual(self.runner.current_asset_id(),before)
        self.assertEqual(result,self.recovery("host-restore-fixture",job))
        self.assertFalse(any(c[0]in {"build","stop","start"}for c in self.runner.calls[len(calls):]))

    def test_legacy_ui_corrupt_index_restores_exact_predecessor(self):
        import hashlib
        before=self.runner.current_asset_id()
        self.change("legacy-ui-failure","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.broker.scheduler.config["scheduler"]["concurrent_ui_preparation"]=False
        self.runner.fail_verify_ui=True;job=self.finish("legacy-ui-failure");row=self.wait([job])[0]
        self.assertEqual(row["status"],"uncertain",row.get("reason"));self.runner.fail_verify_ui=False
        self.assertNotIn("preparation",self.broker.jobs[job])
        failed=self.runner.assets/"releases"/job;path=failed/".code-index/index.sqlite";data=bytearray(path.read_bytes());data[:16]=b"invalid database";path.write_bytes(data)
        meta=failed/".code-index/manifest.json";manifest=json.loads(meta.read_bytes());manifest["index_sha256"]=hashlib.sha256(data).hexdigest();meta.write_text(json.dumps(manifest))
        result=self.recovery("legacy-ui-failure",job)
        self.assertTrue(result["effects_reconciled"]);self.assertEqual(self.runner.current_asset_id(),before)
        self.assertEqual(result,self.recovery("legacy-ui-failure",job))

    def test_restore_refuses_to_erase_a_newer_active_release(self):
        self.change("ui-failure-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.runner.fail_verify_ui=True;job=self.finish("ui-failure-task");self.wait([job]);self.runner.fail_verify_ui=False
        newer=str(uuid.uuid4());self.runner._stage(self.runner.blob,newer,self.runner.assets)
        self.runner.activate_assets(newer)
        with self.assertRaisesRegex(ValueError,"pointer advanced"):self.recovery("ui-failure-task",job)
        self.assertEqual(self.runner.current_asset_id(),newer)

    def test_install_intent_before_activation_can_be_reconciled_without_publication(self):
        before=self.runner.current_asset_id()
        self.change("ui-install-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        original=fixtures.host.release.install_prepared_assets
        with patch.object(fixtures.host.release,"install_prepared_assets",side_effect=RuntimeError("install outcome unknown")):
            job=self.finish("ui-install-task");row=self.wait([job])[0]
        self.assertEqual(row["status"],"uncertain",row.get("reason"));self.assertEqual(self.runner.current_asset_id(),before)
        result=self.recovery("ui-install-task",job,"discard_prepared_release")
        self.assertTrue(result["effects_reconciled"]);self.assertEqual(self.runner.current_asset_id(),before)
        self.assertFalse((self.runner.assets/"releases"/job).exists())

    def test_validation_retries_only_when_captured_runtime_changed(self):
        self.change("ui-runtime-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        original=self.runner.validate_ui;first=True
        def validate(job_id,changed,root=None):
            nonlocal first
            if first:
                first=False
                self.runner.containers[fixtures.host.TARGETS[0]]["Id"]+="-new"
                raise RuntimeError("consumer transition")
            return original(job_id,changed,root=root)
        self.runner.validate_ui=validate
        job=self.finish("ui-runtime-task");row=self.wait([job])[0]
        self.assertEqual(row["status"],"completed",row.get("reason"));self.assertEqual(row["preparation"]["attempts"],2)

    def test_historical_rollback_preview_preserves_later_disjoint_source(self):
        self.change("ui-alpha-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        first=self.finish("ui-alpha-task");self.assertEqual(self.wait([first])[0]["status"],"completed")
        self.change("ui-beta-task","ui/probe/b.css","main {}\n","main { color: blue; }\n")
        second=self.finish("ui-beta-task");self.assertEqual(self.wait([second])[0]["status"],"completed")
        self.call("v3_build_work",{"action":"begin","task_id":"ui-rollback-task","summary":"Compensating rollback fixture","mode":"isolated"})
        result=self.call("build_work",{"action":"ui_rollback_preview","task_id":"ui-rollback-task","release_job_id":first})
        self.assertEqual(result["changed_paths"],["v3/ui/probe/a.css"])
        self.assertIn("-body { color: red; }",result["patch"]);self.assertNotIn("b.css",result["patch"])
        self.assertEqual(self.runner.current_asset_id(),second)
        folder=self.broker.state/first
        merged,_=fixtures.host.release.compensating_candidate((folder/"publication-base.tar").read_bytes(),(folder/"candidate.tar").read_bytes(),self.runner.blob,["v3/ui/probe/a.css"],fixtures.host.context_digest)
        files=fixtures.host.release.entries(merged,fixtures.host.context_digest)
        self.assertEqual(files["v3/ui/probe/a.css"][1],b"body {}\n")
        self.assertEqual(files["v3/ui/probe/b.css"][1],b"main { color: blue; }\n")

    def test_compensating_same_file_conflict_is_explicit(self):
        release=fixtures.host.release;validate=fixtures.host.context_digest
        before=self.runner.blob;files=release.entries(before,validate)
        files["v3/ui/probe/a.css"]=(420,b"body {color:red;}\n");after=release.archive(files,validate)
        files["v3/ui/probe/a.css"]=(420,b"body {color:green;}\n");latest=release.archive(files,validate)
        with self.assertRaisesRegex(ValueError,"source conflict"):
            release.compensating_candidate(before,after,latest,["v3/ui/probe/a.css"],validate)

    def test_compensating_rename_and_deletion_preserve_later_additions(self):
        release=fixtures.host.release;validate=fixtures.host.context_digest;before=self.runner.blob
        files=release.entries(before,validate);value=files.pop("v3/ui/probe/a.css");files["v3/ui/probe/renamed.css"]=value
        files.pop("v3/ui/probe/b.css");after=release.archive(files,validate)
        files["v3/ui/probe/later.css"]=(420,b"aside {}\n");latest=release.archive(files,validate)
        merged,paths=release.compensating_candidate(before,after,latest,["v3/ui/probe/a.css","v3/ui/probe/b.css","v3/ui/probe/renamed.css"],validate)
        final=release.entries(merged,validate)
        self.assertNotIn("v3/ui/probe/renamed.css",final);self.assertEqual(final["v3/ui/probe/a.css"],value)
        self.assertIn("v3/ui/probe/b.css",final);self.assertIn("v3/ui/probe/later.css",final)

    def test_ui_publishes_during_backend_build_and_backend_keeps_new_ui(self):
        self.change("backend-alpha-task","src/main.rs","fn main() {}\n","fn main() { let _version=2; }\n")
        self.change("ui-alpha-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        entered=threading.Event();proceed=threading.Event();original=self.runner.build
        def build(blob,job,role):
            if role=="api":entered.set();self.assertTrue(proceed.wait(5))
            return original(blob,job,role)
        self.runner.build=build
        backend=self.finish("backend-alpha-task");self.assertTrue(entered.wait(3))
        ui=self.finish("ui-alpha-task")
        try:self.assertEqual(self.wait([ui])[0]["status"],"completed")
        finally:proceed.set()
        row=self.wait([backend])[0];self.assertEqual(row["status"],"completed",row.get("reason"))
        active=self.runner.assets/"releases"/self.runner.current_asset_id()
        self.assertEqual((active/"v3/ui/probe/a.css").read_text(),"body { color: red; }\n")
        self.assertIn("_version=2",(active/"v3/src/main.rs").read_text())
        self.assertEqual(self.runner.current_asset_id(),backend)

    def test_ui_preparation_retries_after_backend_publication_and_preserves_both(self):
        self.change("backend-alpha-task","src/main.rs","fn main() {}\n","fn main() { let _version=2; }\n")
        self.change("ui-alpha-task","ui/probe/a.css","body {}\n","body { color: red; }\n")
        entered=threading.Event();proceed=threading.Event();original=self.runner.stage_assets
        def stage(blob,job,**kwargs):
            if job==ui and not entered.is_set():entered.set();self.assertTrue(proceed.wait(5))
            return original(blob,job,**kwargs)
        # Finish under the broker lock so the worker cannot read the identifier before assignment.
        with self.broker.lock:
            self.runner.stage_assets=stage;ui=self.finish("ui-alpha-task")
        self.assertTrue(entered.wait(3));backend=self.finish("backend-alpha-task")
        try:self.assertEqual(self.wait([backend])[0]["status"],"completed")
        finally:proceed.set()
        row=self.wait([ui])[0];self.assertEqual(row["status"],"completed",row.get("reason"))
        active=self.runner.assets/"releases"/self.runner.current_asset_id()
        self.assertEqual((active/"v3/ui/probe/a.css").read_text(),"body { color: red; }\n")
        self.assertIn("_version=2",(active/"v3/src/main.rs").read_text());self.assertEqual(row["preparation"]["attempts"],2)

    def private_review_worker(self,task,path,prepared_job_id=None):
        """Run real host review operations and retain failures for the owning test."""
        done=threading.Event();errors=[]
        def review():
            """Signal completion even when authority or freshness rejects the review."""
            try:self.review(task,path,prepared_job_id=prepared_job_id)
            except BaseException as error:errors.append(error)
            finally:done.set()
        thread=threading.Thread(target=review);thread.start()
        return thread,done,errors

    def test_private_reviews_progress_during_other_candidate_preparation(self):
        self.change("review-private-prep","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        self.change("review-other-prep","ui/probe/b.css","main {}\n","main { color: blue; }\n")
        entered=threading.Event();proceed=threading.Event();original=self.runner.stage_assets
        def stage(blob,job,**kwargs):
            """Hold real private staging until independent review has completed."""
            entered.set();self.assertTrue(proceed.wait(5));return original(blob,job,**kwargs)
        self.runner.stage_assets=stage
        other=self.finish("review-other-prep");self.assertTrue(entered.wait(3))
        worker,done,errors=self.private_review_worker("review-private-prep","ui/probe/a.css")
        try:self.assertTrue(done.wait(2),"Private review blocked behind candidate preparation")
        finally:proceed.set();worker.join(5)
        self.assertFalse(errors,errors);self.assertEqual(self.wait([other])[0]["status"],"completed")

    def test_sealed_private_reviews_progress_during_other_activation(self):
        self.change("review-private-sealed","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        own=self.finish("review-private-sealed")
        self.assertEqual(self.wait([own])[0]["phase"],"awaiting_prepared_reviews")
        self.change("review-other-publish","ui/probe/b.css","main {}\n","main { color: blue; }\n")
        entered=threading.Event();proceed=threading.Event();original=self.runner.verify_ui
        def verify(changed=()):
            """Hold the real post-activation publication critical section."""
            entered.set();self.assertTrue(proceed.wait(5));return original(changed)
        self.runner.verify_ui=verify
        other=self.finish("review-other-publish");self.assertTrue(entered.wait(3))
        self.assertEqual(self.runner.current_asset_id(),other)
        worker,done,errors=self.private_review_worker("review-private-sealed","ui/probe/a.css",own)
        try:self.assertTrue(done.wait(2),"Sealed review waited for another publication lock")
        finally:proceed.set();worker.join(5)
        self.assertFalse(errors,errors);self.assertEqual(self.wait([other])[0]["status"],"completed")
        report=self.call("v3_build_work",{"action":"preflight","task_id":"review-private-sealed"})["synchronization"]
        self.call("v3_build_work",{"action":"decide","task_id":"review-private-sealed","sync_decision":"proceed","expected_sync_digest":report["digest"]})
        row=self.wait([own])[0];self.assertEqual(row["status"],"completed",row.get("reason"))
        current=self.runner.assets/"releases"/own/"v3"
        self.assertIn("red",(current/"ui/probe/a.css").read_text())
        self.assertIn("blue",(current/"ui/probe/b.css").read_text())

    def test_reconcile_matching_projection_does_not_acquire_publication(self):
        self.broker._code_index_reconcile()
        with self.broker.deploy_lock:
            done=threading.Event()
            worker=threading.Thread(target=lambda:(self.broker._code_index_reconcile(),done.set()))
            worker.start()
            completed=done.wait(2)
        worker.join(3);self.assertFalse(worker.is_alive())
        self.assertTrue(completed,"Current projection acquired publication lock")

    def test_reconcile_rechecks_pointer_after_waiting_for_publication(self):
        first=self.runner.current_asset_id();second=str(uuid.uuid4())
        import shutil
        shutil.copytree(self.runner.assets/"releases"/first,self.runner.assets/"releases"/second)
        observations=[];entered=threading.Event();done=threading.Event()
        def current():
            """Expose the first observation before the worker waits for serialization."""
            observations.append(self.runner.asset_id);entered.set();return self.runner.asset_id
        self.runner.asset_id=first
        def reconcile():
            """Run global reconciliation across a deliberate active-pointer race."""
            try:self.broker._code_index_reconcile()
            finally:done.set()
        with patch.object(self.runner,"current_asset_id",current),patch.object(self.broker,"_code_index_activated")as activated:
            with self.broker.deploy_lock:
                worker=threading.Thread(target=reconcile);worker.start();self.assertTrue(entered.wait(2))
                self.runner.asset_id=second
            self.assertTrue(done.wait(3));worker.join(3)
            activated.assert_called_once_with(second)
        self.assertEqual(observations,[first,second])

    def test_private_scan_does_not_hold_a_ledger_write_transaction(self):
        self.change("review-scan-boundary","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        ledger=self.broker.code_index_ledger;original=ledger.inventory;observed=[]
        def inventory(*args):
            """A separate writer can reserve SQLite while expensive scanning runs."""
            import sqlite3
            conn=sqlite3.connect(ledger.path,timeout=.1)
            try:conn.execute("BEGIN IMMEDIATE");observed.append(True);conn.rollback()
            finally:conn.close()
            return original(*args)
        with patch.object(ledger,"inventory",inventory):
            self.review("review-scan-boundary","ui/probe/a.css")
        self.assertEqual(observed,[True])

    def test_concurrent_activation_preserves_private_record_and_retry_receipt(self):
        self.change("review-activation-race","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        task="review-activation-race";path="ui/probe/a.css"
        self.call("code_index_maintenance",{"action":"sync","task_id":task,"scope":path,"request_id":"review-race-sync"})
        row=self.call("code_index_maintenance",{"action":"list","task_id":task,"limit":100})["results"][0]
        args={"action":"claim","task_id":task,"entry_id":row["entry_id"],"expected_revision":row["revision"],"request_id":"review-race-claim"}
        entered=threading.Event();proceed=threading.Event();done=threading.Event();errors=[];result=[]
        ledger=self.broker.code_index_ledger;original=ledger._save
        def save(conn,record,actor,event):
            """Hold one private transaction while publication queues its independent projection."""
            if event=="claim":entered.set();self.assertTrue(proceed.wait(3))
            return original(conn,record,actor,event)
        def claim():
            """Capture the exact retry-safe mutation result."""
            try:result.append(self.call("code_index_maintenance",args))
            except BaseException as error:errors.append(error)
            finally:done.set()
        with patch.object(ledger,"_save",save):
            worker=threading.Thread(target=claim);worker.start();self.assertTrue(entered.wait(2))
            activated=threading.Event()
            def activate():
                """Import only the published namespace after the private writer commits."""
                try:self.broker._code_index_activated(self.runner.current_asset_id())
                except BaseException as error:errors.append(error)
                finally:activated.set()
            publisher=threading.Thread(target=activate);publisher.start();proceed.set()
            self.assertTrue(done.wait(3));self.assertTrue(activated.wait(3));worker.join(3);publisher.join(3)
        self.assertFalse(errors,errors)
        self.assertEqual(self.call("code_index_maintenance",args),result[0])
        row=self.call("code_index_maintenance",{"action":"get","task_id":task,"entry_id":row["entry_id"]})["results"][0]
        self.assertEqual(row["status"],"in_progress")
        self.assertEqual(ledger.active_release(),self.runner.current_asset_id())

    def test_private_record_rejects_changed_source_without_global_reconciliation(self):
        self.change("review-source-stale","ui/probe/a.css","body {}\n","body { color: red; }\n",review=False)
        task="review-source-stale"
        self.call("code_index_maintenance",{"action":"sync","task_id":task,"scope":"ui/probe/a.css","request_id":"review-stale-sync"})
        row=self.call("code_index_maintenance",{"action":"list","task_id":task,"limit":100})["results"][0]
        claimed=self.call("code_index_maintenance",{"action":"claim","task_id":task,"entry_id":row["entry_id"],"expected_revision":row["revision"],"request_id":"review-stale-claim"})["record"]
        item=next(i for i in self.broker.work.values()if i["task_id"]==task)
        source=self.broker.work_dir/"trees"/item["key"]/"v3/ui/probe/a.css";source.write_text("body { color: green; }\n")
        with patch.object(self.broker,"_code_index_reconcile",side_effect=AssertionError("Private record reconciled")):
            with self.assertRaisesRegex(ValueError,"Source changed"):
                self.call("code_index_maintenance",{"action":"record","task_id":task,"entry_id":row["entry_id"],"expected_revision":claimed["revision"],"source_hash":claimed["source_hash"],"claim_id":claimed["claim"]["id"],"status":"complete","checks":{k:"verified"for k in ("documentation","connections","breadcrumbs","configuration","index_freshness")},"configuration_assessment":{"status":"not_applicable","rationale":"Test source freshness"},"request_id":"review-stale-record"})

    def test_global_reads_reconcile_but_private_and_immutable_reads_do_not(self):
        self.change("review-read-routing","ui/probe/a.css","body {}\n","body { color: red; }\n")
        with patch.object(self.broker,"_code_index_reconcile",wraps=self.broker._code_index_reconcile)as reconcile:
            self.call("code_index_maintenance",{"action":"list","task_id":"review-read-routing","limit":5})
            self.call("code_index_maintenance",{"action":"list","root":"published","limit":5})
            reconcile.assert_not_called()
            self.call("code_index_maintenance",{"action":"list","task_id":"published","limit":5})
            self.assertEqual(reconcile.call_count,1)
            self.call("code_index_maintenance",{"action":"list","limit":5})
            self.assertEqual(reconcile.call_count,2)
            self.call("code_index_maintenance",{"action":"list","task_key":"published","limit":5})
            self.assertEqual(reconcile.call_count,3)

    def test_private_claim_rejects_changed_metadata_dependency_during_publication(self):
        marker={"version":1,"feature":"review-dependency","aliases":["private review dependency"],"purpose":"Fixture uses sealed artifact identity","span_lines":1,"configuration":{"status":"configured","references":[{"path":"plugins/engine/fixture/component.json","pointers":["/artifact_sha256"]}]}}
        changed="/* @code-index "+json.dumps(marker)+" */\nbody { color: red; }\n"
        task="review-dependency-stale"
        self.change(task,"ui/probe/a.css","body {}\n",changed,review=False)
        self.call("code_index_maintenance",{"action":"sync","task_id":task,"scope":"ui/probe/a.css","request_id":"review-dependency-sync"})
        rows=self.call("code_index_maintenance",{"action":"list","task_id":task,"limit":100})["results"]
        self.assertTrue(rows);self.assertTrue(all(row["dependencies"]for row in rows))
        item=next(i for i in self.broker.work.values()if i["task_id"]==task)
        dependency=self.broker.work_dir/"trees"/item["key"]/"v3/plugins/engine/fixture/component.json"
        dependency.write_text(json.dumps({"artifact_sha256":"3"*64}))
        done=threading.Event();errors=[]
        def claim():
            """Freshness rejects changed metadata without first waiting on publication."""
            try:self.call("code_index_maintenance",{"action":"claim","task_id":task,"entry_id":rows[0]["entry_id"],"expected_revision":rows[0]["revision"],"request_id":"review-dependency-claim"})
            except BaseException as error:errors.append(error)
            finally:done.set()
        with self.broker.deploy_lock:
            worker=threading.Thread(target=claim);worker.start();completed=done.wait(2)
        worker.join(3);self.assertTrue(completed,"Stale private claim waited for publication")
        self.assertEqual(len(errors),1);self.assertIsInstance(errors[0],ValueError)
        self.assertIn("Source metadata changed",str(errors[0]))


class BackendPredecessorRecoveryTests(ConcurrentUIEntryTests):
    """Reconcile original restored backend containers without replaying deployment."""
    def failed_backend(self):
        """Fail after API rename, then model the original containers restored with new PIDs."""
        self.predecessor=self.runner.current_asset_id()
        self.change("backend-recovery-task","src/main.rs","fn main() {}\n","fn main() { let _x=1; }\n")
        self.runner.fail_create=True
        job=self.finish("backend-recovery-task");row=self.wait([job])[0]
        self.assertEqual(row["status"],"uncertain",row.get("reason"));self.assertEqual(row["phase"],"creating_api")
        self.runner.fail_create=False
        api,loop=fixtures.host.TARGETS
        self.runner.containers[api]=self.runner.containers.pop(row["rollback"]["api"])
        for n,name in enumerate((api,loop)):
            self.runner.containers[name]["State"].update(Status="running",Pid=100+n)
        return job
    def recover_backend(self,job):
        return self.recovery("backend-recovery-task",job,"restore_backend_predecessor","backend-exact-recovery-request")
    def test_backend_restored_originals_reconcile_and_exact_retry_preserves_candidate(self):
        job=self.failed_backend();candidate=(self.broker.state/job/"candidate.tar").read_bytes();start=len(self.runner.calls)
        result=self.recover_backend(job)
        self.assertTrue(result["effects_reconciled"]);self.assertEqual(self.runner.current_asset_id(),self.predecessor)
        self.assertEqual(result,self.recover_backend(job));self.assertFalse(self.broker._has_unresolved_release())
        self.assertEqual((self.broker.state/job/"candidate.tar").read_bytes(),candidate)
        self.assertFalse(any(c[0]in {"build","stop","start","create","rename","promote"}for c in self.runner.calls[start:]))
        self.assertNotIn("NEVER-EXPOSE-THIS",json.dumps(result)+json.dumps(self.broker.jobs[job]["publication_recovery"]))
    def test_backend_recovery_rejects_replacement_identity(self):
        job=self.failed_backend();self.runner.containers[fixtures.host.TARGETS[0]]["Id"]+="-replacement"
        with self.assertRaisesRegex(ValueError,"Original predecessor"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job);self.assertNotIn("publication_recovery",self.broker.jobs[job])
    def test_backend_recovery_rejects_replacement_image(self):
        job=self.failed_backend();self.runner.containers[fixtures.host.TARGETS[0]]["Image"]="sha256:"+"3"*64
        with self.assertRaisesRegex(ValueError,"Original predecessor"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)
    def test_backend_recovery_rejects_configuration_without_disclosing_values(self):
        job=self.failed_backend();self.runner.containers[fixtures.host.TARGETS[0]]["Config"]["Env"]=["TOKEN=PRIVATE-CHANGED-CONFIG"]
        with self.assertRaisesRegex(ValueError,"configuration changed")as caught:self.recover_backend(job)
        self.assertNotIn("PRIVATE-CHANGED-CONFIG",str(caught.exception));self.assertEqual(self.runner.current_asset_id(),job)
    def test_backend_recovery_rejects_host_configuration_change(self):
        job=self.failed_backend();self.runner.containers[fixtures.host.TARGETS[1]]["HostConfig"]["Memory"]+=1
        with self.assertRaisesRegex(ValueError,"configuration changed"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)
    def test_backend_recovery_refuses_newer_pointer(self):
        job=self.failed_backend();new=str(uuid.uuid4());self.runner._stage(self.runner.blob,new,self.runner.assets);self.runner.activate_assets(new)
        with self.assertRaisesRegex(ValueError,"pointer advanced"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),new)
    def test_backend_recovery_refuses_unhealthy_original(self):
        job=self.failed_backend();self.runner.containers[fixtures.host.TARGETS[1]]["State"]["Status"]="exited"
        with self.assertRaisesRegex(ValueError,"unhealthy"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)
    def test_backend_recovery_interrupted_after_pointer_switch_supports_exact_retry(self):
        job=self.failed_backend();self.runner.fail_verify_ui=True
        with self.assertRaises(RuntimeError):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),self.predecessor);self.assertFalse(self.broker.jobs[job]["publication_recovery"]["done"])
        self.runner.fail_verify_ui=False;self.assertTrue(self.recover_backend(job)["effects_reconciled"])
    def test_backend_recovery_retry_rejects_changed_process_generation(self):
        job=self.failed_backend();self.runner.fail_verify_ui=True
        with self.assertRaises(RuntimeError):self.recover_backend(job)
        self.runner.fail_verify_ui=False;self.runner.containers[fixtures.host.TARGETS[0]]["State"]["Pid"]+=1
        with self.assertRaisesRegex(ValueError,"process generation changed"):self.recover_backend(job)
        self.assertEqual(self.broker.jobs[job]["status"],"uncertain")
    def test_backend_recovery_rejects_symlink_capture(self):
        job=self.failed_backend();p=self.broker.state/job/"container-config.json";q=p.with_suffix(".saved");p.rename(q);p.symlink_to(q)
        with self.assertRaises(OSError):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)

    def test_backend_recovery_requires_operator_authority(self):
        job=self.failed_backend();self.trusted["local_caller"]=False
        with self.assertRaisesRegex(PermissionError,"Operator recovery"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)
    def test_backend_recovery_honors_foreign_maintenance_drain(self):
        job=self.failed_backend()
        with patch.object(self.broker,"_maintenance_allowed",return_value=False):
            with self.assertRaisesRegex(PermissionError,"drained"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)
    def test_backend_recovery_refuses_unreconciled_checkout(self):
        job=self.failed_backend()
        with patch.object(self.runner,"checkout_matches",return_value=False):
            with self.assertRaisesRegex(ValueError,"Checkout effects"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)
    def test_backend_recovery_refuses_revoked_job(self):
        job=self.failed_backend();self.broker.jobs[job]["revoked"]=True
        with self.assertRaisesRegex(ValueError,"Inactive uncertain"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)
    def test_backend_recovery_rejects_changed_retry_arguments(self):
        job=self.failed_backend();self.runner.fail_verify_ui=True
        with self.assertRaises(RuntimeError):self.recover_backend(job)
        self.runner.fail_verify_ui=False
        with self.assertRaisesRegex(ValueError,"different request"):
            self.recovery("backend-recovery-task",job,"restore_backend_predecessor","different-backend-request")
    def test_backend_recovery_rejects_writable_private_capture(self):
        job=self.failed_backend();(self.broker.state/job/"container-config.json").chmod(0o666)
        with self.assertRaisesRegex(ValueError,"Invalid private"):self.recover_backend(job)
        self.assertEqual(self.runner.current_asset_id(),job)


class CapturedBackendRestorationTests(ConcurrentUIEntryTests):
    """Exercise actual restoration effects against independently identified fake containers."""
    def setUp(self):
        super().setUp()
        import copy,hashlib
        for name,value in self.runner.containers.items():
            value.update(Id=hashlib.sha256(name.encode()).hexdigest(),Name="/"+name)
        self.runner.inspect_recovery=lambda name:copy.deepcopy(self.runner.containers.get(name))
        def resolve(identity):
            if identity in self.runner.containers:return identity
            names=[name for name,value in self.runner.containers.items()if value["Id"]==identity]
            if len(names)!=1:raise ValueError("Fixture identity unavailable")
            return names[0]
        stop,start,rename,create=self.runner.stop,self.runner.start,self.runner.rename,self.runner.create
        self.runner.stop=lambda identity:stop(resolve(identity))
        self.runner.start=lambda identity:start(resolve(identity))
        def move(identity,target):
            rename(resolve(identity),target);self.runner.containers[target]["Name"]="/"+target
        self.runner.rename=move
        def replacement(name,image,previous,release_root=None):
            create(name,image,previous,release_root)
            role="api"if name==fixtures.host.TARGETS[0]else"loop"
            jid=next(j["job_id"]for j in self.broker.jobs.values()if j.get("release_root")==str(release_root));value=self.runner.containers[name]
            value.update(Id=uuid.uuid4().hex+uuid.uuid4().hex,Name="/"+name)
            value["State"]["Status"]="created"
            value["Config"]["Labels"]={"webagent.release.job":jid,"webagent.release.role":role,"webagent.release.instance":fixtures.host.CONFIG["instance"]}
            return value["Id"]
        self.runner.create=replacement

    def failed(self,point="both_created"):
        self.predecessor=self.runner.current_asset_id()
        self.change("captured-backend-task","src/main.rs","fn main() {}\n","fn main() { let _x=3; }\n")
        api,loop=fixtures.host.TARGETS
        if point=="api_create":self.runner.fail_create=True
        elif point=="both_created":self.runner.fail_verify_ui=True
        else:
            method,target={"stop_loop":("stop",loop),"loop_create":("create",loop),"after_api_create":("create",api)}[point]
            original=getattr(self.runner,method)
            def fail(*args,**kwargs):
                if args[0]==target:
                    if point!="loop_create":original(*args,**kwargs)
                    raise RuntimeError("fixture activation checkpoint")
                return original(*args,**kwargs)
            setattr(self.runner,method,fail)
        job=self.finish("captured-backend-task");row=self.wait([job])[0]
        self.assertEqual(row["status"],"uncertain",row.get("reason"))
        self.runner.fail_create=False;self.runner.fail_verify_ui=False
        if point not in {"api_create","both_created"}:setattr(self.runner,method,original)
        return job

    def restore(self,job):
        return self.recovery("captured-backend-task",job,"restore_captured_backend","captured-backend-recovery-request")

    def verify_originals(self,job):
        capture=json.loads((self.broker.state/job/"container-config.json").read_bytes())
        for role,name in zip(("api","loop"),fixtures.host.TARGETS):
            actual=self.runner.containers[name]
            for key in ("Id","Image","Config","HostConfig"):self.assertEqual(actual[key],capture[role][key])
            self.assertEqual(actual["State"]["Status"],"running")
        self.assertEqual(self.runner.current_asset_id(),self.predecessor)
        self.assertFalse(self.broker._has_unresolved_release())

    def test_restores_and_retains_both_failed_candidates(self):
        job=self.failed();candidate=(self.broker.state/job/"candidate.tar").read_bytes();start=len(self.runner.calls)
        result=self.restore(job);self.verify_originals(job)
        self.assertTrue(result["effects_reconciled"]);self.assertEqual(self.restore(job),result)
        for role,name in zip(("api","loop"),fixtures.host.TARGETS):
            failed=self.runner.containers[name+"-failed-build-"+job]
            self.assertEqual(failed["Image"],self.broker.jobs[job]["images"][role]);self.assertEqual(failed["State"]["Status"],"exited")
        self.assertEqual((self.broker.state/job/"candidate.tar").read_bytes(),candidate)
        self.assertFalse(any(c[0]in {"remove","build","create","promote"}for c in self.runner.calls[start:]))
        self.assertNotIn("NEVER-EXPOSE-THIS",json.dumps(result)+json.dumps(self.broker.jobs[job]["publication_recovery"]))

    def test_each_activation_boundary_can_restore(self):
        for point in ("stop_loop","api_create","loop_create","after_api_create"):
            with self.subTest(point=point):
                case=type(self)("test_restores_and_retains_both_failed_candidates");case.setUp()
                try:job=case.failed(point);case.restore(job);case.verify_originals(job)
                finally:case.tearDown()

    def test_crash_after_every_restore_effect_reconciles_exactly(self):
        for boundary in range(1,10):
            with self.subTest(boundary=boundary):
                case=type(self)("test_restores_and_retains_both_failed_candidates");case.setUp()
                try:
                    job=case.failed();count=[0];original={name:getattr(case.runner,name)for name in ("stop","rename","start","activate_assets")}
                    def wrapper(name):
                        def effect(*args,**kwargs):
                            result=original[name](*args,**kwargs);count[0]+=1
                            if count[0]==boundary:raise RuntimeError("lost effect response")
                            return result
                        return effect
                    for name in original:setattr(case.runner,name,wrapper(name))
                    with self.assertRaisesRegex(RuntimeError,"lost effect response"):case.restore(job)
                    for name,value in original.items():setattr(case.runner,name,value)
                    self.assertTrue(case.restore(job)["effects_reconciled"]);case.verify_originals(job)
                    self.assertEqual(case.restore(job)["status"],"failed")
                finally:case.tearDown()

    def test_restart_after_uncertain_rename_uses_durable_intent(self):
        job=self.failed();normal=self.runner.rename
        def lost(*args):normal(*args);raise RuntimeError("lost rename")
        with patch.object(self.runner,"rename",side_effect=lost):
            with self.assertRaises(RuntimeError):self.restore(job)
        state=self.broker.state;self.broker.shutdown();self.broker=fixtures.host.Broker(state,self.runner)
        self.assertTrue(self.restore(job)["effects_reconciled"]);self.verify_originals(job)

    def test_authority_lease_epoch_and_source_fences_precede_effects(self):
        job=self.failed();start=len(self.runner.calls)
        self.trusted["local_caller"]=False
        with self.assertRaisesRegex(PermissionError,"Operator recovery"):self.restore(job)
        self.trusted["local_caller"]=True
        with patch.object(self.runner,"checkout_matches",return_value=False):
            with self.assertRaisesRegex(ValueError,"Checkout effects"):self.restore(job)
        self.broker.jobs[job]["ownership_epoch"]+=1
        with self.assertRaises((ValueError,PermissionError)):self.restore(job)
        self.assertFalse(any(c[0]in {"stop","rename","start"}for c in self.runner.calls[start:]))

    def test_changed_predecessor_config_stops_before_any_mutation(self):
        job=self.failed();loop=fixtures.host.TARGETS[1];self.runner.containers[self.broker.jobs[job]["rollback"]["loop"]]["HostConfig"]["Memory"]+=1
        start=len(self.runner.calls)
        with self.assertRaisesRegex(ValueError,"configuration changed"):self.restore(job)
        self.assertFalse(any(c[0]in {"stop","rename","start"}for c in self.runner.calls[start:]))

    def test_unknown_candidate_and_newer_pointer_are_preserved(self):
        job=self.failed();api=fixtures.host.TARGETS[0];self.runner.containers[api]["Id"]=uuid.uuid4().hex+uuid.uuid4().hex;self.runner.containers[api]["Config"]["Labels"]={}
        with self.assertRaisesRegex(ValueError,"Unrecognized replacement"):self.restore(job)
        new=str(uuid.uuid4());self.runner._stage(self.runner.blob,new,self.runner.assets);self.runner.activate_assets(new)
        with self.assertRaisesRegex(ValueError,"pointer advanced"):self.restore(job)
        self.assertEqual(self.runner.current_asset_id(),new)

    def test_changed_capture_after_partial_restore_requires_reconciliation(self):
        job=self.failed();normal=self.runner.rename
        def lost(*args):normal(*args);raise RuntimeError("lost rename")
        with patch.object(self.runner,"rename",side_effect=lost):
            with self.assertRaises(RuntimeError):self.restore(job)
        path=self.broker.state/job/"container-config.json";value=json.loads(path.read_bytes());value["loop"]["HostConfig"]["Memory"]+=1;path.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError,"binding changed"):self.restore(job)


class BackendBindRecoveryTests(ConcurrentUIEntryTests):
    """Resume only the exact candidate API after a transient host-port bind failure."""
    def failed_bind(self):
        api=fixtures.host.TARGETS[0]
        ports={"8080/tcp":[{"HostIp":"100.112.23.15","HostPort":"8081"}]}
        self.runner.containers[api]["HostConfig"]["PortBindings"]=ports
        self.runner.containers[api]["NetworkSettings"]={"Ports":ports}
        self.change("backend-bind-task","src/main.rs","fn main() {}\n","fn main() { let _x=2; }\n")
        self.runner.fail_verify_ui=True
        job=self.finish("backend-bind-task");row=self.wait([job])[0]
        self.assertEqual((row["status"],row["phase"]),("uncertain","checking_ui"),row.get("reason"))
        self.runner.fail_verify_ui=False
        api=fixtures.host.TARGETS[0]
        self.runner.containers[api]["State"]["Status"]="exited"
        self.runner.containers[api]["NetworkSettings"]={"Ports":{}}
        def remove(name):
            self.runner.calls.append(("remove",name));self.runner.containers.pop(name)
        self.runner.remove=remove
        return job

    def recover_bind(self,job):
        return self.recovery("backend-bind-task",job,"recover_backend_api_bind","backend-bind-recovery-request")

    def test_recreates_only_exact_stopped_candidate_and_promotes_source(self):
        job=self.failed_bind();start=len(self.runner.calls)
        result=self.recover_bind(job)
        self.assertEqual(result["status"],"completed");self.assertTrue(result["api_recreated"])
        calls=self.runner.calls[start:]
        self.assertIn(("remove",fixtures.host.TARGETS[0]),calls)
        self.assertIn(("start",fixtures.host.TARGETS[0]),calls)
        self.assertFalse(any(call[:2]==("start",fixtures.host.TARGETS[1])for call in calls))
        self.assertTrue(self.runner.checkout_matches((self.broker.state/job/"candidate.tar").read_bytes(),self.broker.jobs[job]["changed_paths"]))
        self.assertEqual(result,self.recover_bind(job))


    def test_running_candidate_without_a_port_mapping_is_recreated(self):
        job=self.failed_bind();api=fixtures.host.TARGETS[0]
        self.runner.containers[api]["State"]["Status"]="running"
        start=len(self.runner.calls);self.recover_bind(job);calls=self.runner.calls[start:]
        self.assertIn(("stop",api),calls)
        self.assertIn(("remove",api),calls)

    def test_rejects_changed_candidate_or_unhealthy_loop_before_mutation(self):
        job=self.failed_bind();api,loop=fixtures.host.TARGETS
        self.runner.containers[api]["Image"]="sha256:"+"4"*64
        with self.assertRaisesRegex(ValueError,"Candidate API image changed"):self.recover_bind(job)
        self.assertNotIn("publication_recovery",self.broker.jobs[job])
        self.runner.containers[api]["Image"]=self.broker.jobs[job]["images"]["api"]
        self.runner.containers[loop]["State"]["Status"]="exited"
        with self.assertRaisesRegex(ValueError,"Candidate loop changed or unhealthy"):self.recover_bind(job)
        self.assertNotIn("publication_recovery",self.broker.jobs[job])
