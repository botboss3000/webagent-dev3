"""Full broker entry points exercise private staging and real immutable release activation."""
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
        self.seal_descriptors=False;self.after_activation=None;self.barrier=None;self.prepared=[];self.once=set();self.guard=threading.Lock()
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
        fixtures.host.release.write_source_receipt(destination,job_id,fixtures.host.context_digest(blob)[0])
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
        # Test hook: lose a post-switch effect after the pointer already moved.
        if self.after_activation is not None:self.after_activation(job_id)


class ConcurrentUIEntryTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);self.sid=str(uuid.uuid4());self.serial=0
        self.policy=patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"scheduler":{**fixtures.host.CONFIG.get("scheduler",{}),"mode":"enforce","component_indexing":True,"max_phases":4,"ui_file_reservations":True,"max_ui_preparations":2,"concurrent_ui_preparation":True}})
        self.asset_patch=patch.object(fixtures.host,"ASSET_ROOT",self.root/"assets")
        self.policy.start();self.asset_patch.start()
        self.runner=PreparedRunner(self.root/"assets");self.broker=fixtures.host.Broker(self.root/"jobs",self.runner)
        self.trusted={"user_id":"codex-local","session_id":self.sid,"lease_until":time.time()+60,"access_fingerprint":"f"*64,"local_caller":True}

    def tearDown(self):
        self.broker.shutdown();self.asset_patch.stop();self.policy.stop();self.tmp.cleanup()

    def call(self,name,args):return self.broker.handle({"name":name,"arguments":args,"workspace":self.sid,"trusted":self.trusted})

    def change(self,task,path,before,after):
        self.call("v3_build_work",{"action":"begin","task_id":task,"summary":"Concurrent UI fixture","mode":"isolated"})
        capture=self.call("build_work",{"action":"review","task_id":task})
        diff="diff --git a/"+path+" b/"+path+"\n"+"".join(difflib.unified_diff(before.splitlines(True),after.splitlines(True),fromfile="a/"+path,tofile="b/"+path))
        self.call("v3_source_apply",{"task_id":task,"expected_inputs_sha256":capture["workspace_inputs_sha256"],"request_id":task+"-patch","patch":diff})

    def finish(self,task):
        review=self.call("build_work",{"action":"review","task_id":task})
        return self.call("v3_build_work",{"action":"finish","task_id":task,"expected_inputs_sha256":review["workspace_inputs_sha256"]})["job_id"]

    def wait(self,jobs):
        deadline=time.monotonic()+8
        while time.monotonic()<deadline:
            # Match the API lock: never observe status before its atomic receipt is populated.
            with self.broker.lock:
                rows=[self.broker.jobs[job]for job in jobs]
                # A terminal job's thread may still hold the publication lock it releases last.
                if all(row["status"]not in {"queued","running"}for row in rows)and not set(jobs)&self.broker.active_jobs:return rows
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

    def recovery(self,task,job,action="restore_failed_ui",request="ui-recovery-exact-request"):
        return self.call("build_work",{"action":action,"task_id":task,"job_id":job,"request_id":request})

    def failed_post_switch_activation(self):
        """Lose a post-switch effect after the immutable pointer switched."""
        self.change("ui-activated-recover","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.runner.after_activation=lambda _:(_ for _ in ()).throw(RuntimeError("post-switch storage full"))
        try:
            job=self.finish("ui-activated-recover");row=self.wait([job])[0]
        finally:self.runner.after_activation=None
        self.assertEqual(row["status"],"uncertain",row.get("reason"))
        self.assertEqual(self.runner.current_asset_id(),job)
        return job

    def test_exact_activated_release_completes_without_reactivation_and_retry_reuses_receipt(self):
        job=self.failed_post_switch_activation();containers=json.loads(json.dumps(self.runner.containers))
        with patch.object(self.runner,"activate_assets",wraps=self.runner.activate_assets)as activate:
            result=self.recovery("ui-activated-recover",job,"complete_activated_release")
            self.assertTrue(result["source_promoted"]);self.assertFalse(result["activation_replayed"])
            self.assertEqual(result,self.recovery("ui-activated-recover",job,"complete_activated_release"));activate.assert_not_called()
        self.assertEqual(self.runner.containers,containers)
        self.assertEqual(self.broker.jobs[job]["status"],"completed")
        self.assertEqual(self.runner.current_asset_id(),job)
        self.assertFalse(self.broker._has_unresolved_release())

    def test_ui_template_plugin_lane_retains_adoption_and_mode_checks(self):
        from unittest.mock import Mock
        job=self.failed_post_switch_activation();self.broker.jobs[job]["operation"]="plugin_deploy"
        self.runner.verify_plugin_adoption=Mock(return_value={"revision":job})
        self.runner.engine_execution_modes=Mock(return_value={"api":"live","loop":"live"})
        result=self.recovery("ui-activated-recover",job,"complete_activated_release")
        self.assertEqual(result["status"],"completed")
        self.runner.verify_plugin_adoption.assert_called_once_with(job)
        self.runner.engine_execution_modes.assert_called_once_with(job)

    def test_activated_completion_rejects_executable_plugin_inputs(self):
        job=self.failed_post_switch_activation();row=self.broker.jobs[job];row["operation"]="plugin_deploy"
        row["changed_paths"].append("v3/plugins/abilities/fixture/runner.py")
        with self.assertRaisesRegex(ValueError,"activated UI recovery"):self.recovery("ui-activated-recover",job,"complete_activated_release")
        self.assertEqual(row["status"],"uncertain")

    def test_activated_completion_rejects_advanced_pointer_and_expired_authority(self):
        job=self.failed_post_switch_activation();row=self.broker.jobs[job]
        row["lease_until"]=0
        with self.assertRaises(PermissionError):self.recovery("ui-activated-recover",job,"complete_activated_release")
        row["lease_until"]=time.time()+60
        newer=str(uuid.uuid4());self.runner._stage(self.runner.blob,newer,self.runner.assets);self.runner.activate_assets(newer)
        with self.assertRaisesRegex(ValueError,"pointer advanced"):self.recovery("ui-activated-recover",job,"complete_activated_release")
        self.assertEqual(self.runner.current_asset_id(),newer);self.assertEqual(row["status"],"uncertain")

    def test_activated_completion_preserves_promotion_conflicts_and_retries_exact_intent(self):
        job=self.failed_post_switch_activation();original=self.runner.promote_checkout
        with patch.object(self.runner,"promote_checkout",side_effect=ValueError("checkout changed during release")):
            with self.assertRaisesRegex(ValueError,"checkout changed"):self.recovery("ui-activated-recover",job,"complete_activated_release")
        self.assertEqual(self.broker.jobs[job]["status"],"uncertain")
        with patch.object(self.runner,"promote_checkout",wraps=original)as promote:
            result=self.recovery("ui-activated-recover",job,"complete_activated_release")
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

    def test_host_release_over_already_unhealthy_consumers_completes_explicitly(self):
        self.change("host-unhealthy-fixture","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.runner.fail_verify_ui=True
        job=self.finish("host-unhealthy-fixture");self.wait([job])
        self.assertEqual(self.broker.jobs[job]["status"],"uncertain")
        self.runner.fail_verify_ui=False;calls=list(self.runner.calls)
        with self.assertRaisesRegex(ValueError,"host-only release required"):
            self.recovery("host-unhealthy-fixture",job,"complete_unhealthy_host_release","host-unhealthy-request")
        self.broker.jobs[job]["operation"]="host_service_deploy"
        with patch.object(self.runner,"healthy",return_value=False):
            result=self.recovery("host-unhealthy-fixture",job,"complete_unhealthy_host_release","host-unhealthy-request-2")
        self.assertEqual((result["status"],result["consumers_unhealthy"]),("completed",True))
        self.assertEqual(self.runner.current_asset_id(),job);self.assertFalse(self.broker._has_unresolved_release())
        self.assertEqual(result,self.recovery("host-unhealthy-fixture",job,"complete_unhealthy_host_release","host-unhealthy-request-2"))
        self.assertFalse(any(c[0]in {"build","stop","start","create","rename"}for c in self.runner.calls[len(calls):]))

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

    def test_legacy_ui_failure_restores_exact_predecessor(self):
        before=self.runner.current_asset_id()
        self.change("legacy-ui-failure","ui/probe/a.css","body {}\n","body { color: red; }\n")
        self.broker.scheduler.config["scheduler"]["concurrent_ui_preparation"]=False
        self.runner.fail_verify_ui=True;job=self.finish("legacy-ui-failure");row=self.wait([job])[0]
        self.assertEqual(row["status"],"uncertain",row.get("reason"));self.runner.fail_verify_ui=False
        self.assertNotIn("preparation",self.broker.jobs[job])
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


def manual_recovery_only(case):
    """Model a broker that stopped before its automatic restore, leaving recovery to the operator."""
    automatic=patch.object(fixtures.host.ui_publication_module.UIPublication,"auto_restore_backend",side_effect=RuntimeError("broker stopped before its automatic restore"))
    automatic.start();case.addCleanup(automatic.stop)


class BackendPredecessorRecoveryTests(ConcurrentUIEntryTests):
    """Reconcile original restored backend containers without replaying deployment."""
    def setUp(self):
        super().setUp();manual_recovery_only(self)
    def failed_backend(self):
        """Fail after API rename, then model the original containers restored with new PIDs."""
        self.predecessor=self.runner.current_asset_id()
        self.change("backend-recovery-task","src/main.rs","fn main() {}\n","fn main() { let _x=1; }\n")
        api,loop=fixtures.host.TARGETS
        rename=self.runner.rename
        def fail(name,target):
            if target==api and name.startswith(api+"-candidate-build-"):raise RuntimeError("fixture activation failure")
            return rename(name,target)
        self.runner.rename=fail
        job=self.finish("backend-recovery-task");row=self.wait([job])[0]
        self.assertEqual(row["status"],"uncertain",row.get("reason"));self.assertEqual(row["phase"],"activating_api")
        self.runner.rename=rename
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
        if not self._testMethodName.startswith("test_automatic"):manual_recovery_only(self)
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
        def replacement(name,image,previous,release_root=None,as_name=None):
            create(name,image,previous,release_root,as_name)
            role="api"if name==fixtures.host.TARGETS[0]else"loop"
            jid=next(j["job_id"]for j in self.broker.jobs.values()if j.get("release_root")==str(release_root));value=self.runner.containers[as_name or name]
            value.update(Id=uuid.uuid4().hex+uuid.uuid4().hex,Name="/"+(as_name or name))
            value["State"]["Status"]="created"
            value["Config"]["Labels"]={"webagent.release.job":jid,"webagent.release.role":role,"webagent.release.instance":fixtures.host.CONFIG["instance"]}
            return value["Id"]
        self.runner.create=replacement

    def failed(self,point="both_created"):
        self.predecessor=self.runner.current_asset_id()
        self.change("captured-backend-task","src/main.rs","fn main() {}\n","fn main() { let _x=3; }\n")
        api,loop=fixtures.host.TARGETS
        if point=="both_created":self.runner.fail_verify_ui=True
        else:
            method,target={"stop_loop":("stop",loop),"api_activate":("rename",api+"-candidate-build-"),
                           "loop_activate":("rename",loop+"-candidate-build-"),"api_start":("start",api)}[point]
            original=getattr(self.runner,method)
            def fail(*args,**kwargs):
                if args[0].startswith(target)and(method!="start"or args[0]==target):
                    if point!="loop_activate":original(*args,**kwargs)
                    raise RuntimeError("fixture activation checkpoint")
                return original(*args,**kwargs)
            setattr(self.runner,method,fail)
        job=self.finish("captured-backend-task");row=self.wait([job])[0]
        self.assertEqual(row["status"],"uncertain",row.get("reason"))
        self.runner.fail_verify_ui=False
        if point!="both_created":setattr(self.runner,method,original)
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

    def test_automatic_restore_before_new_consumers_are_healthy(self):
        for point in ("stop_loop","api_activate","loop_activate","api_start"):
            with self.subTest(point=point):
                case=type(self)("test_automatic_restore_before_new_consumers_are_healthy");case.setUp()
                try:
                    api,loop=fixtures.host.TARGETS
                    method,target={"stop_loop":("stop",loop),"api_activate":("rename",api+"-candidate-build-"),
                                   "loop_activate":("rename",loop+"-candidate-build-"),"api_start":("start",api)}[point]
                    original=getattr(case.runner,method)
                    def fail(*args,original=original,method=method,target=target,**kwargs):
                        if args[0].startswith(target)and(method!="start"or args[0]==target):raise RuntimeError("fixture activation checkpoint TOKEN=NEVER-EXPOSE-THIS")
                        return original(*args,**kwargs)
                    setattr(case.runner,method,fail)
                    case.predecessor=case.runner.current_asset_id()
                    case.change("captured-backend-task","src/main.rs","fn main() {}\n","fn main() { let _x=3; }\n")
                    job=case.finish("captured-backend-task");row=case.wait([job])[0]
                    setattr(case.runner,method,original)
                    self.assertEqual((row["status"],row["phase"]),("failed","publication_reconciled"),row.get("reason"))
                    self.assertEqual(case.broker.jobs[job]["automatic_rollback"]["status"],"restored")
                    self.assertIn("fixture activation checkpoint",row["reason"]);self.assertIn("restored automatically",row["reason"])
                    case.verify_originals(job)
                    # Each created candidate is quarantined under its failed name, never deleted.
                    for role,name in zip(("api","loop"),fixtures.host.TARGETS):
                        self.assertNotIn(name+"-candidate-build-"+job,case.runner.containers)
                        self.assertEqual(case.runner.containers[name+"-failed-build-"+job]["Image"],case.broker.jobs[job]["images"][role])
                    self.assertNotIn("NEVER-EXPOSE-THIS",json.dumps(case.broker.jobs[job]))
                    self.assertFalse(any(c[0]=="promote"for c in case.runner.calls))
                finally:case.tearDown()

    def test_automatic_restore_skips_after_a_healthy_new_consumer(self):
        loop=fixtures.host.TARGETS[1];healthy=self.runner.healthy
        self.runner.healthy=lambda name,expected:False if name==loop else healthy(name,expected)
        self.change("captured-backend-task","src/main.rs","fn main() {}\n","fn main() { let _x=3; }\n")
        job=self.finish("captured-backend-task");row=self.wait([job])[0]
        self.runner.healthy=healthy
        self.assertEqual((row["status"],row["phase"]),("uncertain","checking_loop"),row.get("reason"))
        self.assertNotIn("automatic_rollback",self.broker.jobs[job])

    def test_automatic_restore_failure_is_recorded_and_operator_resumes_it(self):
        api=fixtures.host.TARGETS[0];start=self.runner.start;rename=self.runner.rename;calls=[0]
        def fail_start(identity):
            if identity==api:raise RuntimeError("fixture start failure")
            return start(identity)
        def lost_restore(identity,target):
            # The restore's first effect fails once; the operator retries the same request.
            if target.endswith("-failed-build-"+identity[:0])or "-failed-build-" in target:
                calls[0]+=1
                if calls[0]==1:raise RuntimeError("lost quarantine response")
            return rename(identity,target)
        self.runner.start=fail_start;self.runner.rename=lost_restore
        self.predecessor=self.runner.current_asset_id()
        self.change("captured-backend-task","src/main.rs","fn main() {}\n","fn main() { let _x=3; }\n")
        job=self.finish("captured-backend-task");row=self.wait([job])[0]
        self.runner.start=start;self.runner.rename=rename
        automatic=self.broker.jobs[job]["automatic_rollback"]
        self.assertEqual((row["status"],automatic["status"]),("uncertain","failed"),row.get("reason"))
        self.assertIn(automatic["request_id"],row["reason"])
        result=self.recovery("captured-backend-task",job,"restore_captured_backend",automatic["request_id"])
        self.assertTrue(result["effects_reconciled"]);self.verify_originals(job)

    def accept_unhealthy(self,job,request="accept-unhealthy-request"):
        return self.recovery("captured-backend-task",job,"accept_unhealthy_predecessor",request)

    def test_unhealthy_restored_predecessor_is_settled_only_by_explicit_acceptance(self):
        # A fault both releases share (for example stored data) keeps the predecessor unhealthy;
        # the queue must still be able to admit the release that repairs it.
        job=self.failed();loop=fixtures.host.TARGETS[1]
        with patch.object(self.runner,"healthy",return_value=False):
            with self.assertRaisesRegex(ValueError,"unhealthy"):self.restore(job)
            self.assertTrue(self.broker._has_unresolved_release())
            self.runner.containers[loop]["State"]["Pid"]=4242  # crash loop restarted it
            start=len(self.runner.calls);result=self.accept_unhealthy(job)
        self.assertTrue(result["effects_reconciled"]);self.assertTrue(result["predecessor_unhealthy"])
        self.assertEqual(self.broker.jobs[job]["status"],"failed");self.assertIn("unhealthy",self.broker.jobs[job]["reason"])
        self.verify_originals(job);self.assertEqual(self.accept_unhealthy(job),result)
        self.assertFalse(any(c[0]in {"build","stop","start","create","rename","promote","remove"}for c in self.runner.calls[start:]))
        for role,name in zip(("api","loop"),fixtures.host.TARGETS):
            self.assertEqual(self.runner.containers[name+"-failed-build-"+job]["Image"],self.broker.jobs[job]["images"][role])

    def test_unhealthy_acceptance_still_requires_the_exact_predecessor(self):
        job=self.failed();api=fixtures.host.TARGETS[0]
        with patch.object(self.runner,"healthy",return_value=False):
            with self.assertRaisesRegex(ValueError,"unhealthy"):self.restore(job)
            self.runner.containers[api]["Id"]=uuid.uuid4().hex+uuid.uuid4().hex
            with self.assertRaisesRegex(ValueError,"Original predecessor"):self.accept_unhealthy(job)
        self.assertEqual(self.broker.jobs[job]["status"],"uncertain");self.assertTrue(self.broker._has_unresolved_release())

    def test_restarting_certified_candidate_is_stopped_before_quarantine(self):
        job=self.failed();api=fixtures.host.TARGETS[0];identity=self.runner.containers[api]["Id"]
        self.runner.containers[api]["State"]["Status"]="restarting"
        start=len(self.runner.calls);result=self.restore(job);self.verify_originals(job)
        failed=self.runner.containers[api+"-failed-build-"+job]
        self.assertEqual(failed["Id"],identity);self.assertEqual(failed["State"]["Status"],"exited")
        calls=self.runner.calls[start:];stop=next(i for i,c in enumerate(calls)if c[0]=="stop"and c[1]==api)
        rename=next(i for i,c in enumerate(calls)if c[0]=="rename"and c[1]==api)
        self.assertLess(stop,rename);self.assertTrue(result["effects_reconciled"])
        self.assertEqual(self.restore(job),result)

    def test_unattested_restarting_candidate_remains_untouched(self):
        job=self.failed();api=fixtures.host.TARGETS[0];value=self.runner.containers[api]
        value["State"]["Status"]="restarting";value["Config"]["Labels"]={};value["Id"]=uuid.uuid4().hex+uuid.uuid4().hex
        start=len(self.runner.calls)
        with self.assertRaisesRegex(ValueError,"Unrecognized replacement"):self.restore(job)
        self.assertFalse(any(c[0]in {"stop","rename","start"}for c in self.runner.calls[start:]))

    def test_each_activation_boundary_can_restore(self):
        for point in ("stop_loop","api_activate","loop_activate","api_start"):
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

class PublicationAdmissionIntegrationTests(ConcurrentUIEntryTests):
    def test_boot_entry_failure_releases_publication_and_next_job_finishes(self):
        self.change("boot-entry-failure","ui/probe/a.css","body {}\n","body { color: red; }\n")
        class Broken:
            def __enter__(self):raise RuntimeError("boot lease entry failed")
            def __exit__(self,*args):raise AssertionError("Unentered guard must not be exited")
        with patch.object(self.broker,"boot_effect_lease",return_value=Broken()):
            job=self.finish("boot-entry-failure");row=self.wait([job])[0]
        self.assertEqual(row["status"],"failed");self.assertIn("boot lease entry failed",row["reason"]);self.assertFalse(self.broker.jobs[job].get("effects_started"))
        self.change("after-entry-failure","ui/probe/b.css","main {}\n","main { color: blue; }\n")
        next_job=self.finish("after-entry-failure");self.assertEqual(self.wait([next_job])[0]["status"],"completed")

    def test_cancellation_during_final_wait_releases_all_locks(self):
        self.change("cancel-publish-wait","ui/probe/a.css","body {}\n","body { color: red; }\n")
        ready=threading.Event();go=threading.Event();normal=self.runner.validate_selected_tests
        def tests(*args):ready.set();go.wait(3);return normal(*args)
        self.runner.validate_selected_tests=tests
        job=self.finish("cancel-publish-wait");self.assertTrue(ready.wait(2))
        with self.broker.deploy_lock:
            go.set();deadline=time.monotonic()+2
            while not self.broker.jobs[job].get("publication_wait")and time.monotonic()<deadline:time.sleep(.01)
            self.assertEqual(self.broker.jobs[job]["publication_wait"]["resource"],"deploy_lock")
            self.call("v3_build_cancel",{"job_id":job,"request_id":"cancel-publication-wait-fixture","expected_job_ids":[job]})
            row=self.wait([job])[0]
            self.assertEqual(row["status"],"cancelled");self.assertFalse(self.broker.jobs[job].get("effects_started"))
        self.runner.validate_selected_tests=normal
        self.change("after-wait-cancel","ui/probe/b.css","main {}\n","main { color: blue; }\n")
        next_job=self.finish("after-wait-cancel");self.assertEqual(self.wait([next_job])[0]["status"],"completed")

    def test_bounded_wait_resumes_same_durable_job(self):
        self.change("resume-publish-wait","ui/probe/a.css","body {}\n","body { color: red; }\n")
        ready=threading.Event();go=threading.Event();normal=self.runner.validate_selected_tests
        def tests(*args):ready.set();go.wait(3);return normal(*args)
        self.runner.validate_selected_tests=tests;self.broker.ui_publication.wait_timeout=.08
        job=self.finish("resume-publish-wait");self.assertTrue(ready.wait(2))
        with self.broker.deploy_lock:
            go.set();row=self.wait([job])[0];self.assertEqual(row["status"],"needs_decision");self.assertFalse(self.broker.jobs[job].get("effects_started"))
        self.runner.validate_selected_tests=normal
        report=self.call("build_work",{"action":"preflight","task_id":"resume-publish-wait"})["synchronization"]
        self.call("build_work",{"action":"decide","task_id":"resume-publish-wait","sync_decision":"proceed","expected_sync_digest":report["digest"]})
        self.assertEqual(self.wait([job])[0]["status"],"completed")


class EarlyBuildWorkTests(unittest.TestCase):
    """Rollback saving and UI fixtures overlap the Rust build; failures only cost the saved time."""
    def broker(self,ui_enabled=True,fail_export=False):
        import types
        temp=tempfile.TemporaryDirectory();self.addCleanup(temp.cleanup);root=Path(temp.name)
        exported=[];prewarmed=[]
        def export(images,path):
            if fail_export:raise RuntimeError("docker save failed")
            exported.append(images);path.write_bytes(b"images")
        executor=fixtures.host.executor_module.Executor(types.SimpleNamespace(job_context=threading.local()),None,{})
        runner=types.SimpleNamespace(executor=executor,inspect=lambda name:{"Image":"sha256:"+name},export=export,secrets=())
        publication=types.SimpleNamespace(enabled=lambda job:ui_enabled,prewarm=lambda job,blob:prewarmed.append(blob)or{"executed_tests":["a"],"reused_tests":["b","c"]})
        broker=types.SimpleNamespace(runner=runner,state=root,ui_publication=publication,lock=threading.RLock())
        for name in ("_start_early","_join_early"):setattr(broker,name,getattr(fixtures.host.Broker,name).__get__(broker))
        return broker,exported,prewarmed
    def test_build_deploy_saves_live_images_and_prewarms_ui(self):
        broker,exported,prewarmed=self.broker()
        job={"job_id":str(uuid.uuid4()),"operation":"build_deploy"}
        ready=broker._join_early(job,broker._start_early(job,b"blob"))
        images,path=ready["rollback"]
        self.assertEqual(images,["sha256:"+name for name in fixtures.host.TARGETS]);self.assertEqual(exported,[images])
        self.assertEqual(path,broker.state/"execution"/job["job_id"]/"rollback-early.tar");self.assertTrue(path.is_file())
        self.assertEqual(prewarmed,[b"blob"]);self.assertEqual(job["early_work"]["ui"],{"executed":1,"reused":2})
        self.assertEqual(sorted(t["phase"]for t in job["phase_timings"]),["early_rollback_save","early_ui_tests"])
    def test_other_operations_and_disabled_publication_skip_overlap(self):
        broker,exported,prewarmed=self.broker(ui_enabled=False)
        self.assertEqual(broker._start_early({"job_id":str(uuid.uuid4()),"operation":"deploy"},b"blob"),{})
        early=broker._start_early({"job_id":str(uuid.uuid4()),"operation":"build_deploy"},b"blob")
        self.assertEqual(set(early),{"rollback"});broker._join_early({},early);self.assertEqual(prewarmed,[])
    def test_failed_early_work_is_recorded_not_raised(self):
        broker,exported,prewarmed=self.broker(fail_export=True)
        job={"job_id":str(uuid.uuid4()),"operation":"build_deploy"}
        ready=broker._join_early(job,broker._start_early(job,b"blob"))
        self.assertNotIn("rollback",ready);self.assertIn("docker save failed",job["early_work"]["rollback"])
        self.assertEqual([(t["phase"],t["outcome"])for t in job["phase_timings"]if t["phase"]=="early_rollback_save"],[("early_rollback_save","failed")])


class PrewarmTests(unittest.TestCase):
    def publication(self,tests):
        import types
        temp=tempfile.TemporaryDirectory();self.addCleanup(temp.cleanup);root=Path(temp.name);seen=[]
        def validate(job_id,folder,evidence):
            # The fixture container runs as another user: every file and directory must be readable by others.
            for path in [folder,*folder.rglob("*")]:self.assertTrue(path.stat().st_mode&0o004,path)
            seen.append((job_id,(folder/"v3/ui/app.test.js").read_text(),evidence["runtime_images"],evidence["tests"]))
            return {"executed_tests":evidence["tests"]}
        files={name:(0o644,data.encode())for name,data in {"v3/Dockerfile":"FROM test","v3/Cargo.toml":"fixture","v3/Cargo.lock":"locked"}.items()}
        files["v3/ui/app.test.js"]=(0o600,b"test\n")  # owner-only in the source, as some fixtures are
        blob=fixtures.host.release.archive(files,fixtures.host.context_digest)
        broker=types.SimpleNamespace(runner=types.SimpleNamespace(validate_selected_tests=validate))
        policy=types.SimpleNamespace(dependencies=lambda entries,changed:{"tests":tests,"sha256":"x"})
        publication=fixtures.host.ui_publication_module.UIPublication(broker,fixtures.host.release,fixtures.host.context_digest,None,root,policy)
        return publication,blob,seen,root
    def test_selected_fixtures_run_on_an_extracted_candidate_that_is_removed(self):
        publication,blob,seen,root=self.publication(["v3/ui/app.test.js"])
        job={"job_id":str(uuid.uuid4()),"changed_paths":["v3/ui/app.test.js"]}
        self.assertEqual(publication.prewarm(job,blob),{"executed_tests":["v3/ui/app.test.js"]})
        self.assertEqual(seen,[(job["job_id"],"test\n",{},["v3/ui/app.test.js"])])
        self.assertEqual(list((root/"preparation-attempts"/job["job_id"]).iterdir()),[])
    def test_no_selected_fixture_runs_nothing(self):
        publication,blob,seen,root=self.publication([])
        self.assertIsNone(publication.prewarm({"job_id":str(uuid.uuid4())},blob));self.assertEqual(seen,[])
