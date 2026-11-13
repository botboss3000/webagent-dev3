import importlib.util,json,tempfile,unittest
from pathlib import Path
import test_admin_build as fixtures
ROOT=Path(__file__).resolve().parents[1]
class PublicationTests(unittest.TestCase):
    def test_general_assets_and_core_inputs_use_different_lanes(self):
        for path in ("abilities/Core/base/skills/base.skill.md","abilities/Custom/new/new.json","abilities/Custom/new/skills/extra.skill.md","config/prompts.json"):
            self.assertTrue(fixtures.host.hot_source_path("v3/"+path),path)
        for path in ("src/abilities.rs","sandbox.py","abilities/Core/base/adapter.rs","abilities/Custom/new/runner.py"):
            self.assertFalse(fixtures.host.hot_source_path("v3/"+path),path)
    def test_executable_inputs_require_an_explicit_package_manifest(self):
        prefix="v3/abilities/Custom/new/"
        blob=fixtures.context(extra={prefix+"plugin.json":json.dumps({"dispatch":"isolated","build":{"inputs":["runner.py"],"assets":["schemas.json"]}})})
        self.assertTrue(fixtures.host.generic_package_path(prefix+"runner.py",blob))
        self.assertFalse(fixtures.host.generic_package_path(prefix+"undeclared.py",blob))
    def test_deleted_executable_is_recognized_from_its_previous_package(self):
        prefix="v3/abilities/Custom/new/"
        previous=fixtures.context(extra={prefix+"plugin.json":json.dumps({"dispatch":"isolated","build":{"inputs":["runner.py"],"assets":[]}})})
        self.assertTrue(fixtures.host.generic_package_path(prefix+"runner.py",previous))
        self.assertFalse(fixtures.host.generic_package_path(prefix+"runner.py",fixtures.context()))
    def test_native_handlers_cannot_be_reclassified_by_a_new_plugin_manifest(self):
        prefix="v3/abilities/Core/existing/"
        previous=fixtures.context(extra={prefix+"existing.json":json.dumps({"readiness":"ready","kind":"ability"})})
        self.assertTrue(fixtures.host.native_package_migration(prefix+"runner.py",previous))
        previous=fixtures.context(extra={prefix+"existing.json":json.dumps({"readiness":"pending","kind":"ability"})})
        self.assertFalse(fixtures.host.native_package_migration(prefix+"runner.py",previous))
    def test_generated_adoption_probe_parses(self):
        runner=fixtures.host.Runner()
        runner.inspect=lambda name:{"Mounts":[{"Destination":"/opt/v3/assets","Source":str(fixtures.host.ASSET_ROOT),"RW":False}],"Config":{"Env":["V3_UI_ROOT=/opt/v3/assets/current/v3/ui"]}}
        def command(args,**kwargs):
            compile(args[-1],"<protocol probe>","exec")
            return json.dumps({"version":2,"id":"plugin-protocol","result":{"status":"success","output":{"protocol":1,"catalog":1,"dispatch":1}}})
        runner.command=command;self.assertTrue(runner.plugin_hot_swappable())
if __name__=="__main__":unittest.main()

class ArtifactCacheTests(unittest.TestCase):
    def test_runtime_asset_declarations_reuse_the_exact_retained_rust_binary(self):
        import hashlib,uuid
        from unittest.mock import Mock,patch
        with tempfile.TemporaryDirectory()as temporary:
            assets=Path(temporary)/"assets";state=Path(temporary)/"state";job=str(uuid.uuid4())
            package=assets/"releases"/job/"v3/abilities/Custom/fixture";(package/"src").mkdir(parents=True)
            inputs={"Cargo.toml":b'[package]\nname="fixture_runner"\nversion="0.1.0"\nedition="2021"\n',"Cargo.lock":b"version = 4\n","src/main.rs":b"fn main() {}"}
            for name,value in inputs.items():(package/name).write_bytes(value)
            build={"toolchain":"rust-1.99","inputs":list(inputs),"dependencies":[],"flags":[],"assets":[]}
            manifest={"id":"fixture","dispatch":"isolated","runtime":"executable","entrypoint":"fixture_runner","build":build}
            (package/"plugin.json").write_text(json.dumps(manifest))
            hashes={name:hashlib.sha256(value).hexdigest()for name,value in inputs.items()}
            old=hashlib.sha256(fixtures.host.canonical({"build":build,"entrypoint":"fixture_runner","inputs":hashes})).hexdigest()
            retained=assets/"artifacts"/old/"fixture_runner";retained.parent.mkdir(parents=True);retained.write_bytes(b"retained artifact");retained.chmod(0o555)
            (retained.parent/"compiler-receipt.json").write_text(json.dumps({"input_key":old,"artifact_sha256":hashlib.sha256(retained.read_bytes()).hexdigest(),"recipe":fixtures.host.executor_module.RUST+":release:locked:offline:debug0"}))
            original_receipt=(retained.parent/"compiler-receipt.json").read_bytes()
            runner=fixtures.host.Runner();runner.command=Mock(side_effect=AssertionError("Unexpected compilation"))
            with patch.object(fixtures.host,"ASSET_ROOT",assets),patch.object(fixtures.host,"STATE",state):
                first=runner.build_ability_artifacts(job)[0]
                self.assertTrue(first["reused"])
                self.assertEqual(first["linker"],"legacy-default")
                self.assertEqual(first["compiler_recipe"],fixtures.host.executor_module.RUST+":release:locked:offline:debug0")
                manifest["build"]["assets"]=["new-guidance.md"];(package/"new-guidance.md").write_text("New guidance")
                (package/"plugin.json").write_text(json.dumps(manifest))
                second=runner.build_ability_artifacts(job)[0]
            self.assertTrue(second["reused"]);self.assertEqual(first["build_key"],second["build_key"])
            self.assertEqual(second["linker"],"legacy-default")
            self.assertEqual(json.loads((assets/"artifacts"/second["build_key"]/"compiler-receipt.json").read_bytes())["recipe"],first["compiler_recipe"])
            self.assertEqual((retained.parent/"compiler-receipt.json").read_bytes(),original_receipt)
            self.assertEqual(json.loads((assets/"artifacts"/second["build_key"]/"compiler-receipt.json").read_bytes()),{"input_key":second["build_key"],"artifact_sha256":hashlib.sha256(b"retained artifact").hexdigest(),"recipe":first["compiler_recipe"]})
            self.assertEqual(retained.read_bytes(),b"retained artifact")
            self.assertEqual((assets/"artifacts"/second["build_key"]/"fixture_runner").read_bytes(),b"retained artifact")
            runner.command.assert_not_called()
            binary=assets/"artifacts"/second["build_key"]/"fixture_runner";binary.chmod(0o755);binary.write_bytes(b"tampered")
            with patch.object(fixtures.host,"ASSET_ROOT",assets),patch.object(fixtures.host,"STATE",state),self.assertRaisesRegex(ValueError,"differs from verified"):
                runner.build_ability_artifacts(job)
