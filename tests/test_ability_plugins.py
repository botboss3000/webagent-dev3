import importlib.util,json,tempfile,unittest
from pathlib import Path
import test_admin_build as fixtures
ROOT=Path(__file__).resolve().parents[1]
class PublicationTests(unittest.TestCase):
    def test_general_assets_and_core_inputs_use_different_lanes(self):
        for path in ("plugins/abilities/Core/base/base.skill.md","plugins/abilities/Custom/new/new.json","plugins/abilities/Custom/new/skills/extra.skill.md","config/prompts.json"):
            self.assertTrue(fixtures.host.hot_source_path("v3/"+path),path)
        for path in ("src/abilities.rs","worker.py","ability_plugin_worker.py","plugins/abilities/Core/base/adapter.rs","plugins/abilities/Custom/new/runner.py"):
            self.assertFalse(fixtures.host.hot_source_path("v3/"+path),path)
    def test_executable_inputs_require_an_explicit_package_manifest(self):
        prefix="v3/plugins/abilities/Custom/new/"
        blob=fixtures.context(extra={prefix+"plugin.json":json.dumps({"dispatch":"isolated","build":{"inputs":["runner.py"],"assets":["schemas.json"]}})})
        self.assertTrue(fixtures.host.generic_package_path(prefix+"runner.py",blob))
        self.assertFalse(fixtures.host.generic_package_path(prefix+"undeclared.py",blob))
    def test_deleted_executable_is_recognized_from_its_previous_package(self):
        prefix="v3/plugins/abilities/Custom/new/"
        previous=fixtures.context(extra={prefix+"plugin.json":json.dumps({"dispatch":"isolated","build":{"inputs":["runner.py"],"assets":[]}})})
        self.assertTrue(fixtures.host.generic_package_path(prefix+"runner.py",previous))
        self.assertFalse(fixtures.host.generic_package_path(prefix+"runner.py",fixtures.context()))
    def test_native_handlers_cannot_be_reclassified_by_a_new_plugin_manifest(self):
        prefix="v3/plugins/abilities/Core/existing/"
        previous=fixtures.context(extra={prefix+"existing.json":json.dumps({"readiness":"ready","kind":"ability"})})
        self.assertTrue(fixtures.host.native_package_migration(prefix+"runner.py",previous))
        previous=fixtures.context(extra={prefix+"existing.json":json.dumps({"readiness":"pending","kind":"ability"})})
        self.assertFalse(fixtures.host.native_package_migration(prefix+"runner.py",previous))
    def test_bridge_source_and_generated_adoption_probe_parse(self):
        for name in ("worker.py","ability_plugin_worker.py"):
            compile((ROOT/name).read_bytes(),name,"exec")
        runner=fixtures.host.Runner()
        runner.inspect=lambda name:{"Mounts":[{"Destination":"/opt/v3/assets","Source":str(fixtures.host.ASSET_ROOT),"RW":False}],"Config":{"Env":["V3_UI_ROOT=/opt/v3/assets/current/v3/ui"]}}
        def command(args,**kwargs):
            compile(args[-1],"<protocol probe>","exec")
            return json.dumps({"version":2,"id":"plugin-protocol","result":{"status":"success","output":{"protocol":1,"catalog":1,"dispatch":1}}})
        runner.command=command;self.assertTrue(runner.plugin_hot_swappable())
if __name__=="__main__":unittest.main()

class GenerationTests(unittest.TestCase):
    def test_declared_helpers_are_pinned_and_legacy_bindings_remain_readable(self):
        import hashlib,uuid
        from unittest.mock import patch
        module=importlib.util.spec_from_file_location("generation_bridge",ROOT/"ability_plugin_worker.py")
        bridge=importlib.util.module_from_spec(module);module.loader.exec_module(bridge)
        with tempfile.TemporaryDirectory()as temporary:
            releases=Path(temporary)/"releases";package=releases/str(uuid.uuid4())/"v3/plugins/abilities/Custom/fixture"
            package.mkdir(parents=True)
            manifest={"id":"fixture","dispatch":"isolated","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py","schemas":"schemas.json","effect":"write","tools":["fixture_echo"],"build":{"inputs":["runner.py","helper.py"],"assets":[]}}
            raw=json.dumps(manifest).encode();code=b"print('fixture')";schemas=b"[]";helper=b"VALUE = 1"
            for name,value in {"plugin.json":raw,"runner.py":code,"schemas.json":schemas,"helper.py":helper}.items():(package/name).write_bytes(value)
            hashes=[hashlib.sha256(value).hexdigest()for value in (raw,code,schemas)]
            files={name:hashlib.sha256(value).hexdigest()for name,value in {"runner.py":code,"helper.py":helper}.items()}
            identity=lambda value:hashlib.sha256(json.dumps(value,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()).hexdigest()
            binding={"root":str(package),"id":"fixture","name":"fixture_echo","generation_scheme":2,"generation":identity([hashes,files])}
            actual=Path
            with patch.object(bridge,"Path",side_effect=lambda value:releases if str(value)=="/opt/v3/assets/releases"else actual(value)):
                self.assertEqual(bridge.package(binding)[0],package)
                legacy={**binding,"generation_scheme":1,"generation":identity(hashes)}
                self.assertEqual(bridge.package(legacy)[0],package)
                (package/"helper.py").write_bytes(b"VALUE = 2")
                with self.assertRaisesRegex(ValueError,"Stale package binding"):bridge.package(binding)

class ArtifactCacheTests(unittest.TestCase):
    def test_runtime_asset_declarations_reuse_the_exact_retained_rust_binary(self):
        import hashlib,uuid
        from unittest.mock import Mock,patch
        with tempfile.TemporaryDirectory()as temporary:
            assets=Path(temporary)/"assets";state=Path(temporary)/"state";job=str(uuid.uuid4())
            package=assets/"releases"/job/"v3/plugins/abilities/Custom/fixture";(package/"src").mkdir(parents=True)
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
