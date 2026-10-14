"""Second engine registration, metadata-only closure and multi-artifact seal regressions."""
import hashlib,importlib.util,json,tempfile,unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location("engine_bg",ROOT/"build-service/engine_components.py")
engine=importlib.util.module_from_spec(spec);spec.loader.exec_module(engine)
class BackgroundEngineTests(unittest.TestCase):
    """Verify registered background contracts, dependency isolation and immutable multi-component sealing."""
    def test_registered_descriptor_and_content_free_closure(self):
        """Require the metadata-only operation and declared service closure while excluding host implementation."""
        declaration=engine.descriptor(ROOT/"plugins/engine/background-dispatch/component.json")
        self.assertEqual(declaration["operation"],"background.plan")
        closure=engine.closure(ROOT,"background-dispatch")
        self.assertIn("crates/engine-service/src/lib.rs",closure)
        self.assertNotIn("src/background_tasks.rs",closure)
        self.assertNotIn("crates/context-assembly/src/lib.rs",closure)
        for prefix in declaration["build"]["source_inputs"]:
            if prefix.startswith("crates/"):self.assertTrue(engine.package_path("v3/"+prefix+"/src/lib.rs",{}),prefix)
    def test_two_component_sealing_retry_does_not_skip_second(self):
        """Retry sealing a partially sealed generation and require both component artifacts."""
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);outputs=[]
            for identity in engine.REGISTRY:
                declaration=ROOT/"plugins/engine"/identity/"component.json"
                folder=root/"generation"/engine.ROOT/identity;folder.mkdir(parents=True)
                (folder/"component.json").write_bytes(declaration.read_bytes())
                registered=engine.REGISTRY[identity];body=("public-fixture-"+identity).encode()
                binary=root/"artifacts"/identity/registered["binary"];binary.parent.mkdir(parents=True);binary.write_bytes(body)
                outputs.append({"package":identity,"build_key":identity,"artifact_sha256":hashlib.sha256(body).hexdigest()})
            engine.seal(root/"generation",outputs[:1],root)
            engine.seal(root/"generation",outputs,root)
            for output in outputs:
                folder=root/"generation"/engine.ROOT/output["package"]
                self.assertEqual(json.loads((folder/"component.json").read_text())["artifact_sha256"],output["artifact_sha256"])
    def test_registered_contract_rejects_other_operation(self):
        """Reject an execution operation that the metadata-only host interface never registered."""
        with tempfile.TemporaryDirectory()as temporary:
            path=Path(temporary)/"component.json";value=json.loads((ROOT/"plugins/engine/background-dispatch/component.json").read_text())
            value["operation"]="background.execute";path.write_text(json.dumps(value))
            with self.assertRaises(ValueError):engine.descriptor(path)
