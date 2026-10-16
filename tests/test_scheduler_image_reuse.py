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


if __name__=="__main__":
    unittest.main()
