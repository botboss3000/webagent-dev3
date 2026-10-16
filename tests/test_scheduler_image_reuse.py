"""Resume only an image whose source, compiled artifacts and recipe still match."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace

module_path=Path(__file__).resolve().parents[1]/"build-service/image_reuse.py"
spec=importlib.util.spec_from_file_location("image_reuse_under_test",module_path)
reuse=importlib.util.module_from_spec(spec);spec.loader.exec_module(reuse)


class ImageCheckpointTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)
        for name in ("executor.py","packaging_cache.py","image_reuse.py"):
            (self.root/name).write_text("protected "+name)
        self.job={"job_id":"a"*36,"images":{}}
        self.image="sha256:"+"b"*64
        self.blob=b"exact candidate source"
        output=self.root/"execution"/self.job["job_id"]
        (output/"output/artifacts").mkdir(parents=True)
        (output/"output/artifacts/binary").write_bytes(b"binary")
        self.hash=hashlib.sha256(b"binary").hexdigest()
        (output/"verified.json").write_text(__import__("json").dumps({
            "inputs_sha256":hashlib.sha256(self.blob).hexdigest(),
            "artifacts":{"binary":self.hash},"log":"tests passed"}))
        self.actual=self.image
        self.calls=[]
        def command(argv,timeout):
            self.calls.append(argv)
            return self.actual+"\n"
        self.runner=SimpleNamespace(
            source_config={"state_root":str(self.root),"service_dir":str(self.root)},
            execution_application_blob=lambda blob:blob,
            context_digest=lambda blob:(hashlib.sha256(blob).hexdigest(),1),
            executor=SimpleNamespace(artifact_hashes=lambda folder:{
                p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in folder.iterdir()}),
            command=command)
        self.job["image_receipts"]={"api":reuse.record(self.runner,self.job,self.blob,"api",self.image,"c"*64)}
        self.job["images"]["api"]=self.image

    def test_resume_uses_identical_immutable_image(self):
        self.assertEqual(self.image,reuse.reuse(self.runner,self.job,self.blob,"api"))
        self.assertEqual(1,len(self.calls))

    def test_changed_source_or_compiler_receipt_fail_before_docker(self):
        with self.assertRaisesRegex(ValueError,"source changed"):
            reuse.reuse(self.runner,self.job,b"changed","api")
        receipt=self.root/"execution"/self.job["job_id"]/"verified.json"
        receipt.write_text(receipt.read_text()+" ")
        with self.assertRaisesRegex(ValueError,"compiler evidence changed"):
            reuse.reuse(self.runner,self.job,self.blob,"api")
        self.assertEqual([],self.calls)

    def test_missing_image_and_recipe_change_fail_closed(self):
        self.actual="sha256:"+"d"*64
        with self.assertRaisesRegex(ValueError,"unavailable"):
            reuse.reuse(self.runner,self.job,self.blob,"api")
        self.actual=self.image
        (self.root/"executor.py").write_text("different recipe")
        with self.assertRaisesRegex(ValueError,"recipe changed"):
            reuse.reuse(self.runner,self.job,self.blob,"api")

    def test_role_checkpoint_cannot_be_relabelled(self):
        self.job["image_receipts"]["loop"]=dict(self.job["image_receipts"]["api"])
        with self.assertRaisesRegex(ValueError,"identity changed"):
            reuse.reuse(self.runner,self.job,self.blob,"loop")
        self.job["image_receipts"]["api"]["role"]="loop"
        with self.assertRaisesRegex(ValueError,"identity changed"):
            reuse.reuse(self.runner,self.job,self.blob,"api")


class FailedRetryModeTests(unittest.TestCase):
    def test_rebased_candidate_requires_explicit_cached_rebuild(self):
        old="a"*64;merged="b"*64
        failed={"status":"failed","operation":"build_deploy","inputs_sha256":old,
                "images":{"api":"sha256:"+"1"*64,"loop":"sha256:"+"2"*64}}
        self.assertEqual(reuse.failed_retry_mode(failed,old,False),"reuse")
        self.assertEqual(reuse.failed_retry_mode(failed,merged,False),"needs_rebuild")
        self.assertEqual(reuse.failed_retry_mode(failed,merged,True),"cached_rebuild")
        failed["effects_started"]=True
        with self.assertRaisesRegex(ValueError,"pre-activation"):
            reuse.failed_retry_mode(failed,merged,True)

    def test_failed_retry_inherits_old_binding_without_reusing_it(self):
        old="a"*64;merged="b"*64
        failed={"status":"failed","operation":"build_deploy","inputs_sha256":merged,
                "images":{},"legacy_image_proof":{"source_sha256":old,"source_job_id":"old"}}
        self.assertEqual(reuse.failed_retry_mode(failed,merged,False),"needs_rebuild")
        self.assertEqual(reuse.failed_retry_mode(failed,merged,True),"cached_rebuild")
        self.assertEqual(reuse.failed_retry_mode(failed,old,False),"reuse_inherited")
        failed["legacy_image_proof"]["source_sha256"]="bad"
        with self.assertRaisesRegex(ValueError,"inconsistent"):
            reuse.failed_retry_mode(failed,merged,True)


class CachedRebuildLineageTests(unittest.TestCase):
    def rows(self):
        source="11111111-1111-1111-1111-111111111111"
        lineage="22222222-2222-2222-2222-222222222222"
        prior={"task_id":"task","source_generation":4,"changed_paths":["v3/a.rs"],
               "retry_mode":"cached_rebuild","images":{},"legacy_image_proof":None}
        proof={"source_job_id":source,"source_sha256":"a"*64,"compiler_receipt_sha256":"b"*64,
               "images":{"api":"sha256:"+"c"*64,"loop":"sha256:"+"d"*64}}
        jobs={lineage:{"task_id":"task","source_generation":4,"changed_paths":["v3/a.rs"],
                       "status":"failed","effects_started":False,"legacy_image_proof":proof}}
        return prior,jobs,lineage,proof

    def test_explicit_lineage_binds_no_effect_cached_rebuild(self):
        prior,jobs,lineage,proof=self.rows()
        self.assertEqual(reuse.cached_rebuild_lineage(prior,jobs,lineage),
                         {"version":1,"lineage_job_id":lineage,"legacy_image_proof":proof})

    def test_lineage_rejects_scope_or_effect_changes(self):
        prior,jobs,lineage,proof=self.rows()
        jobs[lineage]["changed_paths"]=[]
        with self.assertRaisesRegex(ValueError,"scope"):
            reuse.cached_rebuild_lineage(prior,jobs,lineage)
        jobs[lineage]["changed_paths"]=["v3/a.rs"];jobs[lineage]["effects_started"]=True
        with self.assertRaisesRegex(ValueError,"unavailable"):
            reuse.cached_rebuild_lineage(prior,jobs,lineage)
        jobs[lineage]["effects_started"]=False;prior["cached_rebuild_lineage"]={"version":1,"lineage_job_id":lineage,"legacy_image_proof":proof}
        with self.assertRaisesRegex(ValueError,"request changed"):
            reuse.cached_rebuild_lineage(prior,jobs,"33333333-3333-3333-3333-333333333333")


if __name__=="__main__":
    unittest.main()

class PackagingResumeTests(ImageCheckpointTests):
    def prepared(self):
        import uuid,json
        old=self.job["job_id"];identity=str(uuid.uuid4())
        (self.root/"execution"/old).rename(self.root/"execution"/identity)
        self.job.update(job_id=identity,status="failed",operation="build_deploy",inputs_sha256=hashlib.sha256(self.blob).hexdigest(),images={})
        self.job["packaging_preflight"]={"source_sha256":reuse.digest(self.blob),"recipe":reuse.recipe_receipt(self.runner)}
        self.job["compiler_checkpoint"]={"source_sha256":reuse.digest(self.blob),"compiler_receipt_sha256":reuse.source_receipt(self.runner,self.job,self.blob),"recipe":reuse.recipe_receipt(self.runner)}
        return self.job
    def test_packaging_retry_requires_no_image_pair_and_reads_exact_artifacts(self):
        prior=self.prepared();proof=reuse.packaging_checkpoint(self.runner,prior,self.blob)
        next_job={"job_id":"new-job","packaging_resume":proof}
        artifacts=reuse.packaging_artifacts(self.runner,next_job,self.blob)
        self.assertEqual(artifacts,self.root/"execution"/prior["job_id"]/"output/artifacts")
        self.assertEqual(self.calls,[])
    def test_modified_artifact_and_changed_recipe_are_rejected(self):
        prior=self.prepared();proof=reuse.packaging_checkpoint(self.runner,prior,self.blob)
        (self.root/"execution"/prior["job_id"]/"output/artifacts/binary").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError,"bytes changed"):reuse.packaging_artifacts(self.runner,{"job_id":"new-job","packaging_resume":proof},self.blob)
        (self.root/"executor.py").write_text("new recipe")
        with self.assertRaisesRegex(ValueError,"binding changed"):reuse.packaging_artifacts(self.runner,{"job_id":"new-job","packaging_resume":proof},self.blob)
    def test_missing_certificate_or_preflight_and_activation_forbid_resume(self):
        prior=self.prepared()
        prior["effects_started"]=True
        with self.assertRaisesRegex(ValueError,"pre-activation"):reuse.packaging_checkpoint(self.runner,prior,self.blob)
        prior.pop("effects_started");prior["packaging_preflight"]={}
        with self.assertRaisesRegex(ValueError,"preflight"):reuse.packaging_checkpoint(self.runner,prior,self.blob)
    def test_resume_rejects_changed_source(self):
        prior=self.prepared();proof=reuse.packaging_checkpoint(self.runner,prior,self.blob)
        with self.assertRaisesRegex(ValueError,"binding changed"):reuse.packaging_artifacts(self.runner,{"job_id":"new-job","packaging_resume":proof},b"new source")


class PackagingPreflightTests(unittest.TestCase):
    def test_packager_missing_is_named_before_any_compilation(self):
        import test_admin_build as fixtures
        from unittest.mock import Mock
        worker_module=fixtures.host.executor_module
        job={"job_id":"12345678-1234-1234-1234-123456789abc"}
        runner=fixtures.host.Runner.__new__(fixtures.host.Runner)
        runner.resources=SimpleNamespace(enabled=True);runner.job_context=SimpleNamespace(job=job)
        runner.package_cache=SimpleNamespace(enabled=True,ready=lambda blob:{"api":{},"loop":{}})
        runner.retention_enabled=True;runner.resource_progress=lambda job:None
        def retain(reference,identity):
            if reference==worker_module.BUILDKIT:raise RuntimeError("Protected resource operation unavailable")
            return {"retained":True,"image":"sha256:"+"a"*64}
        runner.retain_image=retain
        worker=worker_module.Executor(runner,runner.resources,{})
        worker.engine=Mock(side_effect=AssertionError("Compiler must not run"));runner.executor=worker
        with self.assertRaisesRegex(ValueError,"Required packager dependency missing"):
            runner.build(b"source",job["job_id"],"api")
        worker.engine.assert_not_called()
    def test_both_images_are_pinned_before_cached_runtime_verification(self):
        import test_admin_build as fixtures
        module=fixtures.host.executor_module;calls=[]
        job={"job_id":"12345678-1234-1234-1234-123456789abc"}
        def retain(reference,identity):
            calls.append(reference);return {"retained":True,"image":"sha256:"+"a"*64}
        def ready(blob):
            self.assertEqual(calls,[module.RUST,module.BUILDKIT]);return {"api":{"manifest_digest":"api"},"loop":{"manifest_digest":"loop"}}
        runner=SimpleNamespace(retention_enabled=True,retain_image=retain,package_cache=SimpleNamespace(enabled=True,ready=ready),resource_progress=lambda _:None)
        proof=module.Executor(runner,None,{}).preflight_packaging(job,b"source")
        self.assertEqual(set(proof["images"]),{"compiler","packager"})
        self.assertEqual(set(job["dependency_bases"]),{"api","loop"})

class PackagingRetryBrokerTests(unittest.TestCase):
    def setUp(self):
        import test_scheduler_broker as fixtures
        self.fixture=fixtures
        fixtures.SchedulerBrokerTests.setUp(self)
    def tearDown(self):
        self.fixture.SchedulerBrokerTests.tearDown(self)
    def call(self,*args):return self.fixture.SchedulerBrokerTests.call(self,*args)
    def prepare(self):
        import uuid,json
        item=self.fixture.SchedulerBrokerTests.begin(self,"packaging-resume-fixture")
        self.fixture.SchedulerBrokerTests.write(self,item,"README.md","new source\n")
        tree=Path(item["workspace_path"]).parent;blob=self.broker.scheduler_task_snapshot(tree)
        identity=str(uuid.uuid4());folder=self.broker.state/identity;folder.mkdir()
        for name,data in (("base.tar",self.runner.blob),("task.tar",blob),("candidate.tar",blob)):(folder/name).write_bytes(data)
        value=self.broker.scheduler.store.get(item["key"])
        job={"job_id":identity,"request_key":"original-fixture","status":"failed","operation":"build_deploy","user_id":item["user_id"],"workspace":item["workspace"],"task_id":item["task_id"],"source_generation":value["generation"],"ownership_epoch":value["epoch"],"ownership_id":item["key"],"created_at":1,"inputs_sha256":self.broker.scheduler_digest(blob),"base_inputs_sha256":self.broker.scheduler_digest(self.runner.blob),"changed_paths":["v3/README.md"],"release_root":str(folder/"release"),"images":{}}
        item.update(status="closed",job_id=identity,inputs_sha256=self.broker.scheduler_digest(blob))
        self.broker.jobs[identity]=job
        service=self.root/"service";service.mkdir()
        for name in ("executor.py","packaging_cache.py","image_reuse.py"):(service/name).write_text("verified fixture")
        self.runner.source_config={**self.runner.source_config,"state_root":str(self.broker.state),"service_dir":str(service)}
        self.runner.context_digest=self.fixture.fixtures.host.context_digest
        self.runner.execution_application_blob=self.fixture.fixtures.host.Runner.execution_application_blob
        artifacts=self.broker.state/"execution"/identity/"output/artifacts";artifacts.mkdir(parents=True);(artifacts/"binary").write_bytes(b"binary")
        certificate={"inputs_sha256":self.broker.scheduler_digest(blob),"artifacts":self.runner.executor.artifact_hashes(artifacts),"log":"passed compiler and tests"}
        (artifacts.parent.parent/"verified.json").write_text(json.dumps(certificate))
        job["packaging_preflight"]={"source_sha256":reuse.digest(blob),"recipe":reuse.recipe_receipt(self.runner)}
        job["compiler_checkpoint"]={"source_sha256":reuse.digest(blob),"compiler_receipt_sha256":reuse.source_receipt(self.runner,job,blob),"recipe":reuse.recipe_receipt(self.runner)}
        self.broker.package_cache=SimpleNamespace(enabled=True,ready=lambda _: {})
        return item,job,value
    def test_supported_retry_creates_new_attempt_without_image_pair(self):
        from unittest.mock import patch
        item,prior,value=self.prepare()
        args={"action":"retry_packaging_release","task_id":item["task_id"],"job_id":prior["job_id"],"expected_revision":value["revision"],"expected_epoch":value["epoch"],"request_id":"packaging-broker-retry"}
        with patch.object(self.broker,"_start_next"),patch.object(self.broker,"_sync_report",return_value={"total":0,"status":"synchronized"}):
            result=self.call("build_work",args)
            again=self.call("build_work",args)
            self.assertEqual(result["job_id"],again["job_id"])
            with self.assertRaisesRegex(ValueError,"reused"):self.call("build_work",{**args,"rebuild_if_stale":True})
        new=self.broker.jobs[result["job_id"]]
        self.assertNotEqual(new["job_id"],prior["job_id"])
        self.assertEqual(new["retry_mode"],"packaging_resume")
        self.assertEqual(new["packaging_resume"]["source_job_id"],prior["job_id"])
        self.assertEqual(prior["status"],"failed");self.assertEqual(prior["images"],{})
    def test_changed_baseline_requires_rebuild_before_creating_attempt(self):
        from unittest.mock import patch
        item,prior,value=self.prepare()
        self.runner.blob=self.fixture.fixtures.context(extra={"v3/README.md":"original\n","v3/ui/new.js":"new runtime source"})
        before=set(self.broker.jobs)
        args={"action":"retry_packaging_release","task_id":item["task_id"],"job_id":prior["job_id"],"expected_revision":value["revision"],"expected_epoch":value["epoch"],"request_id":"packaging-broker-stale"}
        with patch.object(self.broker,"_sync_report",return_value={"total":0,"status":"synchronized"}):
            result=self.call("build_work",args)
        self.assertEqual(result["status"],"needs_rebuild")
        self.assertEqual(set(self.broker.jobs),before)
