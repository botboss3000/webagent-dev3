"""Independent monitoring broker boundary tests; never run application services."""
import importlib.util,json,tempfile,unittest
from pathlib import Path
from unittest.mock import Mock
ROOT=Path(__file__).resolve().parents[1]
def load(name):
    spec=importlib.util.spec_from_file_location(name,ROOT/"build-service"/(name+".py"));module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
m=load("monitoring_component");ownership=load("ownership")
class MonitoringTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);self.folder=self.root/m.PREFIX;self.folder.mkdir(parents=True)
        (self.folder/"src").mkdir();(self.folder/"src/main.rs").write_text("fn main() {}\n")
        (self.folder/"Cargo.toml").write_text('[package]\nname="webagent-system-monitoring"\nversion="0.1.0"\nedition="2021"\nbuild=false\n[workspace]\n')
        (self.folder/"Cargo.lock").write_text('version=4\n[[package]]\nname="webagent-system-monitoring"\nversion="0.1.0"\n')
        (self.folder/"component.json").write_text(json.dumps({"schema_version":1,"id":"system-monitoring","interface_version":1,"binary":m.BINARY,"toolchain":"1.99.1"}))
    def tearDown(self):self.tmp.cleanup()
    def test_standalone_inventory_and_source_mutation(self):
        before=m.inventory(self.folder);self.assertIn("src/main.rs",before)
        (self.folder/"src/main.rs").write_text("fn main() {println!(\"ok\");}\n")
        self.assertNotEqual(m.inventory(self.folder),before)
    def test_reject_external_dependencies_and_workspace_coupling(self):
        original=(self.folder/"Cargo.toml").read_text()
        for extra in ['[dependencies]\nx={path="../../.."}\n','[dependencies]\nx={git="https://example.invalid/repo"}\n','[dependencies]\nx={workspace=true}\n','[lib]\nproc-macro=true\n']:
            (self.folder/"Cargo.toml").write_text(original+extra)
            with self.assertRaises(ValueError):m.inventory(self.folder)
    def test_reject_symlinks_and_generated_or_build_inputs(self):
        for name in ["build.rs",".cargo/config.toml","target/output"]:
            p=self.folder/name;p.parent.mkdir(exist_ok=True);p.write_text("x")
            with self.assertRaises(ValueError):m.inventory(self.folder)
            p.unlink()
        (self.folder/"src/link.rs").symlink_to(self.folder/"src/main.rs")
        with self.assertRaisesRegex(ValueError,"symlink"):m.inventory(self.folder)
    def test_descriptor_and_missing_source_fail_closed(self):
        (self.folder/"component.json").write_text("{}")
        with self.assertRaisesRegex(ValueError,"descriptor"):m.inventory(self.folder)
        (self.folder/"Cargo.lock").unlink()
        with self.assertRaisesRegex(ValueError,"Incomplete"):m.inventory(self.folder)
    def test_only_fixed_subtree_is_independent(self):
        claims=ownership.Components(self.root/"v3").scopes(["services/system-monitoring/src/main.rs"])
        self.assertIn("module:service:system-monitoring",claims);self.assertNotIn("module:engine",claims)
        self.assertEqual(ownership.Components(self.root/"v3",["services/system-monitoring/src/main.rs"]).scopes(["services/system-monitoring/src/main.rs"]),claims)
        self.assertEqual(ownership.Components(self.root/"v3").scopes(["services/unknown/src/main.rs"]),["module:workspace","path:services/unknown/src/main.rs"])
    def test_fixed_publication_path(self):
        self.assertTrue(m.package_path(m.PREFIX+"src/main.rs"))
        self.assertFalse(m.package_path("v3/services/system-monitoring-other/src/main.rs"))
    def test_immutable_seal_and_tampering(self):
        assets=self.root/"assets";binary=assets/"artifacts"/("a"*64)/m.BINARY;binary.parent.mkdir(parents=True);binary.write_bytes(b"fixture")
        import hashlib
        output={"component":"system-monitoring","build_key":"a"*64,"artifact_sha256":hashlib.sha256(b"fixture").hexdigest(),"reused":False}
        destination=self.root/"release";m.seal(destination,[output],assets);m.seal(destination,[output],assets)
        self.assertEqual((destination/m.PREFIX/"artifact"/m.BINARY).read_bytes(),b"fixture")
        binary.write_bytes(b"tampered")
        with self.assertRaises(ValueError):m.seal(destination,[output],assets)
    def test_measured_build_cache_and_frozen_source(self):
        import contextlib,types
        assets=self.root/"assets";state=self.root/"state"
        job={"job_id":"fixture","release_root":str(self.root)}
        runner=types.SimpleNamespace(plugin_build_lock=lambda key:contextlib.nullcontext(),job_context=types.SimpleNamespace(job=job),executor=Mock())
        def phase(job,phase,kind,source,cache,command,recipe,timeout,network):
            self.assertEqual((phase,kind,network),("plugin_compile","plugin",False))
            if command[1]=="build":
                target=cache/"target/release";target.mkdir(parents=True);(target/m.BINARY).write_bytes(b"compiled")
        runner.executor.compiler_recipe.return_value="fixture-mold-image"
        runner.executor.phase.side_effect=phase
        executor=types.SimpleNamespace(RUST="pinned-fixture",Executor=types.SimpleNamespace(compiler_dependencies=lambda target:["/build/src/main.rs"],local_dependency=lambda name:"v3/src/main.rs"))
        scheduler=types.SimpleNamespace(Scheduler=types.SimpleNamespace(write_record=lambda path,value:path.write_text(json.dumps(value))))
        outputs=m.build(runner,"fixture",self.root,state,assets,executor,scheduler)
        self.assertFalse(outputs[0]["reused"]);self.assertEqual(runner.executor.phase.call_count,2)
        self.assertTrue(m.build(runner,"fixture",self.root,state,assets,executor,scheduler)[0]["reused"])
        self.assertEqual(runner.executor.phase.call_count,2)
        (assets/"artifacts"/outputs[0]["build_key"]/m.BINARY).chmod(0o644)
        (assets/"artifacts"/outputs[0]["build_key"]/m.BINARY).write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError,"artifact changed"):m.build(runner,"fixture",self.root,state,assets,executor,scheduler)
        (self.folder/"src/main.rs").write_text("fn main() {println!(\"changed\");}\n")
        runner.job_context.job=None
        with self.assertRaisesRegex(ValueError,"Owned frozen"):m.build(runner,"fixture",self.root,state,assets,executor,scheduler)
    def test_checked_source_patch_admits_only_monitoring_service(self):
        import subprocess
        from unittest.mock import patch
        import test_admin_build as fixtures
        repo=self.root/"repo";repo.mkdir()
        subprocess.run(["git","init","-q",str(repo)],check=True)
        source=repo/"v3";source.mkdir()
        for name,allowed in [("services/system-monitoring/src/main.rs",True),("services/unknown/src/main.rs",False),("services/system-monitoring-other/src/main.rs",False)]:
            text="diff --git a/"+name+" b/"+name+"\nnew file mode 100644\n--- /dev/null\n+++ b/"+name+"\n@@ -0,0 +1 @@\n+fn main() {}\n"
            with patch.object(fixtures.host,"SOURCE",repo):
                if allowed:self.assertEqual(fixtures.host.inspect_source_patch(source,text),[name])
                else:
                    with self.assertRaisesRegex(ValueError,"source patch path denied"):fixtures.host.inspect_source_patch(source,text)
    def test_only_pure_monitoring_changes_avoid_ability_runtime(self):
        self.assertTrue(m.only_paths([m.PREFIX+"src/main.rs",m.PREFIX+"Cargo.lock"]))
        for paths in [[],[m.PREFIX+"src/main.rs","v3/plugins/abilities/Other/x.rs"],["v3/services/unknown/src/main.rs"],[m.PREFIX+"src/main.rs","v3/src/main.rs"]]:self.assertFalse(m.only_paths(paths))
    def test_service_release_admission_is_independent_of_ability_protocol(self):
        import test_admin_build as fixtures
        case=fixtures.BuildTests(methodName="runTest");case.setUp()
        try:
            case.runner.hot_assets=True
            case.runner.core_compile_inputs=lambda:{"v3/src/main.rs"}
            case.broker._start_next=lambda:None
            task=case.call("v3_build_work",{"action":"begin","task_id":"monitor-release-fixture","summary":"Independent monitoring artifact","mode":"isolated"})
            source=Path(task["workspace_path"])/"services/system-monitoring/src/main.rs"
            source.parent.mkdir(parents=True);source.write_text("fn main() {}\n")
            digest=case.call("v3_build_work",{"action":"status"})["own"][0]["workspace_inputs_sha256"]
            receipt=case.call("v3_build_work",{"action":"finish","task_id":"monitor-release-fixture","expected_inputs_sha256":digest})
            self.assertEqual(case.broker.jobs[receipt["job_id"]]["operation"],"ui_deploy")
        finally:
            case.broker.active=None;case.tearDown()
if __name__=="__main__":unittest.main()
