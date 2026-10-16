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
