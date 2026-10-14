"""Engine publication regression checks: closure, immutable sealing and live root dependency isolation."""
import hashlib,importlib.util,json,tempfile,unittest,types
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location("engine_build",ROOT/"build-service/engine_components.py")
engine=importlib.util.module_from_spec(spec);spec.loader.exec_module(engine)
spec=importlib.util.spec_from_file_location("engine_executor",ROOT/"build-service/executor.py")
executor=importlib.util.module_from_spec(spec);spec.loader.exec_module(executor)
class EngineTests(unittest.TestCase):
    def descriptor(self):
        return {"schema_version":1,"id":"context-assembly","group":"engine-loop","interface_version":1,"implementation_version":"0.1.0","operation":"context.prepare","modes":["live","linked"],"runtime_requirements":[],"build":{"package":engine.BINARY,"source_inputs":engine.SOURCE_INPUTS,"toolchain":"1.90.0","features":[],"runtime_assets":[]},"executable":"artifact/"+engine.BINARY,"limits":{"max_request_bytes":16777216,"max_response_bytes":16777216,"timeout_ms":30000,"max_concurrency":1}}
    def test_structured_smoke_artifact_survives_true_environment_redaction(self):
        import test_admin_build as fixtures
        proof={"mode":"linked","equivalence":True,"interface_version":1,"prepared":[{},{}],"background_prepared":[{},{}],"boot_prepared":[{},{}],"service_started":False};raw=json.dumps(proof)
        self.assertIn("[redacted]",fixtures.host.scrub(raw,["true"]))
        with tempfile.TemporaryDirectory()as temporary:
            path=Path(temporary)/"linked-smoke-result.json";path.write_text(raw)
            self.assertEqual(executor.Executor.linked_smoke(executor.Executor.smoke_artifact(path)),proof)
            self.assertEqual(executor.Executor.smoke_recipe("linked")[-1].splitlines()[-1],"/output/stable/webagent-v3 --context-smoke > /output/stable/linked-smoke-result.json")
            self.assertIn("/output/stable/live-smoke-result.json",executor.Executor.smoke_recipe("live")[-1])
    def test_smoke_artifact_rejects_symlink_hardlink_and_oversize(self):
        import os
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);path=root/"proof.json";path.write_text("{}")
            alias=root/"alias";alias.symlink_to(path)
            with self.assertRaises(ValueError):executor.Executor.smoke_artifact(alias)
            alias.unlink();os.link(path,alias)
            with self.assertRaises(ValueError):executor.Executor.smoke_artifact(path)
            alias.unlink();path.unlink();os.mkfifo(path)
            with self.assertRaises(ValueError):executor.Executor.smoke_artifact(path)
            path.unlink();path.write_text("x"*65537)
            with self.assertRaises(ValueError):executor.Executor.smoke_artifact(path)
    def test_successful_phase_logs_survive_later_proof_failure(self):
        from unittest.mock import Mock
        persisted=[];runner=types.SimpleNamespace(resource_progress=lambda job:persisted.append(dict(job)))
        worker=executor.Executor(runner,None,{});job={"job_id":"fixture","log":""}
        worker.persist_phase_log(job,"test result: ok; fixture [redacted]\n")
        with self.assertRaises(ValueError):worker.linked_smoke('{"mode":"linked","equivalence":false}')
        self.assertIn("test result: ok",persisted[-1]["log"]);self.assertIn("[redacted]",persisted[-1]["log"])
        worker.persist_phase_log(job,"x"*(2*1024*1024));self.assertLessEqual(len(job["log"].encode()),1024*1024)
    def test_linked_target_and_dependency_attestation_are_separate_from_live(self):
        from unittest.mock import Mock
        argv=executor.Executor.linked_recipe();self.assertEqual(argv[argv.index("--target-dir")+1],"/output/linked-target")
        worker=executor.Executor(None,None,{})
        worker.compiler_dependencies=Mock(return_value={"src/main.rs","crates/context-assembly/src/lib.rs"})
        files={"v3/src/main.rs":(0o644,b"main"),"v3/crates/context-assembly/src/lib.rs":(0o644,b"implementation")}
        self.assertIn("v3/crates/context-assembly/src/lib.rs",worker.linked_dependencies(Path("linked-target"),files));self.assertEqual(worker.compiler_dependencies.call_args.args[0],Path("linked-target"))
        worker.compiler_dependencies.return_value={"src/main.rs"}
        with self.assertRaisesRegex(ValueError,"attestation"):worker.linked_dependencies(Path("linked-target"),files)
    def test_live_optimized_smoke_requires_lifecycle_and_equal_prepared_cases(self):
        linked={"mode":"linked","equivalence":True,"interface_version":1,"prepared":[{"envelope":"fixture1"},{"envelope":"fixture2"}],"background_prepared":[{},{}],"boot_prepared":[{},{}],"service_started":False}
        live={**linked,"mode":"live","service_reused":True,"service_drained":True}
        self.assertEqual(executor.Executor.live_smoke(json.dumps(live),linked)["mode"],"live")
        for changed in ({**live,"service_reused":False},{**live,"service_drained":False},{**live,"prepared":[{},{}]}):
            with self.assertRaises(ValueError):executor.Executor.live_smoke(json.dumps(changed),linked)
    def test_optimized_linked_proof_rejects_live_mode_and_failed_equivalence(self):
        self.assertEqual(executor.Executor.linked_smoke('{"mode":"linked","equivalence":true,"interface_version":1,"prepared":[{},{}],"background_prepared":[{},{}],"boot_prepared":[{},{}],"service_started":false}')["mode"],"linked")
        for value in ({"mode":"live","equivalence":True},{"mode":"linked","equivalence":False},{"mode":"linked"}):
            with self.assertRaisesRegex(ValueError,"smoke"):executor.Executor.linked_smoke(json.dumps(value))
    def test_command_timeout_keeps_scrubbed_output_and_private_inspect_stays_empty(self):
        import test_admin_build as fixtures
        runner=fixtures.host.Runner();runner.secrets=["fixture-sensitive-marker"]
        argv=["python3","-u","-c","print('progress fixture-sensitive-marker',flush=True);import time;time.sleep(2)"]
        for private in (False,True):
            with self.subTest(private=private),self.assertRaisesRegex(TimeoutError,"fixed build task timeout")as result:
                runner.command(argv,timeout=.05,private_raw=private)
            if private:self.assertEqual(result.exception.log,"")
            else:
                self.assertIn("progress",result.exception.log);self.assertNotIn("fixture-sensitive-marker",result.exception.log)
    def test_filtered_diagnosis_passes_only_a_bounded_name_to_runtime(self):
        import uuid
        from unittest.mock import Mock
        for invalid in ("--ignored","name;command","name with spaces","name/other","x"*161):
            with self.assertRaises(ValueError):executor.test_filter(invalid)
        with tempfile.TemporaryDirectory()as temporary:
            runner=types.SimpleNamespace(execution_application_blob=lambda blob:blob,execution_source_digest=lambda _:"same")
            worker=executor.Executor(runner,None,{"state_root":temporary});worker.phase=Mock(return_value="checked");job={"job_id":str(uuid.uuid4()),"test_filter":"engine_components::"}
            worker.check(job,b"fixture","test",60,lambda path,blob:path.mkdir(parents=True))
            runtime=worker.phase.call_args_list[-1].args[5]
            self.assertEqual(runtime,["cargo","test","--locked","--offline","engine_components::","--","--test-threads=1","--nocapture"])
            self.assertNotIn("engine_components::",worker.phase.call_args_list[1].args[5])
    def test_cap_requires_kernel_enforcement_and_oom_cannot_pass(self):
        import threading,uuid
        from unittest.mock import Mock
        from collections import defaultdict
        cap=executor.COMPILER_MEMORY_CAP
        for scenario in ("success","oom","unenforced"):
            with self.subTest(scenario=scenario),tempfile.TemporaryDirectory()as temporary:
                resources=types.SimpleNamespace(enabled=True,enter=Mock(),attached=Mock(),lock=threading.RLock(),rows=defaultdict(dict),diagnostic_check=lambda _:False)
                runner=types.SimpleNamespace(command=Mock(side_effect=["a"*64,"started","compiler succeeded"]),job_context=types.SimpleNamespace(),resource_progress=Mock())
                observed={"memory_max":cap if scenario!="unenforced"else None,"swap_max":0,"oom_events":0,"oom_kills":0,"peak_memory":1024}
                runner.resource_request=Mock(side_effect=[{"container_id":"a"*64,"state":"created","phase":"compile_test"},observed,{**observed,"oom_events":1 if scenario=="oom"else 0,"oom_kills":1 if scenario=="oom"else 0}])
                worker=executor.Executor(runner,resources,{"instance":"development","service_uid":1000,"service_gid":1000});worker.finish_phase=Mock();job={"job_id":str(uuid.uuid4()),"compiler_memory_cap":cap}
                args=(job,"compile_test","engine",Path(temporary),Path(temporary)/"output",["cargo","check"],"engine:rust1.90:check",10)
                if scenario=="success":self.assertEqual(worker.phase(*args),"compiler succeeded")
                elif scenario=="oom":
                    with self.assertRaisesRegex(RuntimeError,"OOM"):worker.phase(*args)
                    self.assertFalse(worker.finish_phase.call_args.args[2]);self.assertFalse(worker.finish_phase.call_args.args[3]);self.assertEqual(job["compiler_cap_outcomes"][0]["outcome"],"oom")
                else:
                    with self.assertRaisesRegex(ValueError,"kernel"):worker.phase(*args)
                    self.assertEqual(runner.command.call_count,2)
                create=runner.command.call_args_list[0].args[0];self.assertEqual(create[create.index("--memory")+1],str(cap));self.assertEqual(create[create.index("--memory-swap")+1],str(cap));self.assertEqual(resources.enter.call_args.kwargs["hard_cap"],cap)
    def test_typed_cap_rejects_other_values_and_is_compiler_only(self):
        self.assertEqual(executor.compiler_cap(4026531840),4026531840)
        for value in (True,"4026531840",1024,4026531841):
            with self.assertRaises(ValueError):executor.compiler_cap(value)
    def test_check_and_test_compile_recipes_have_separate_runtime_profiles(self):
        import types,uuid
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory()as temporary:
            runner=types.SimpleNamespace(execution_application_blob=lambda blob:blob,execution_source_digest=lambda _:"same")
            worker=executor.Executor(runner,None,{"state_root":temporary});worker.phase=Mock(return_value="checked")
            worker.check({"job_id":str(uuid.uuid4())},b"fixture","test",60,lambda path,blob:path.mkdir(parents=True))
            self.assertEqual([(call.args[1],call.args[6])for call in worker.phase.call_args_list],[("dependency","engine:rust1.90:dependency"),("compile_test","engine:rust1.90:compile_test"),("test","engine:rust1.90:test")])
            worker.phase.reset_mock();worker.check({"job_id":str(uuid.uuid4())},b"fixture","check",60,lambda path,blob:path.mkdir(parents=True))
            self.assertEqual(worker.phase.call_args_list[-1].args[6],"engine:rust1.90:check")
    def test_job_view_exposes_bounded_waiting_cause_and_deadline(self):
        import test_admin_build as fixtures
        from unittest.mock import Mock
        broker=object.__new__(fixtures.host.Broker);broker.runner=types.SimpleNamespace(secrets=[]);broker._queue_preflight=Mock(return_value=None);broker.jobs={};broker.active=None;broker.active_ui=None
        job={"job_id":"fixture","status":"running","resource_waiting":{"phase":"test","reason":"headroom","deadline_at":1234,"estimate_bytes":1024}}
        self.assertEqual(broker._view(job)["resource_waiting"],job["resource_waiting"])
    def test_actual_execution_mode_gates_hot_engine_replacement(self):
        import test_admin_build as fixtures
        from unittest.mock import Mock
        runner=fixtures.host.Runner();runner.current_asset_id=Mock(return_value="publication")
        for mode in ("live","linked",None,"unknown"):
            receipt={r:{"revision":"publication","mode":mode}for r in ("api","loop")};runner.command=Mock(return_value=json.dumps(receipt))
            self.assertEqual(runner.engine_hot_swappable(),mode=="live")
        runner.command=Mock(return_value=json.dumps({r:{"revision":"stale","mode":"live"}for r in ("api","loop")}));self.assertFalse(runner.engine_hot_swappable())
        runner.command=Mock(return_value=json.dumps({r:{"revision":"publication","mode":"linked"}for r in ("api","loop")}))
        self.assertEqual(runner.engine_execution_modes("publication"),{"api":"linked","loop":"linked"})
    def test_catalog_records_withdrawal_and_reinstall_clears_it(self):
        import uuid
        with tempfile.TemporaryDirectory()as temporary:
            assets=Path(temporary);prior=assets/"releases/prior";folder=prior/engine.ROOT;folder.mkdir(parents=True);(folder/"catalog.json").write_text(json.dumps({"schema_version":1,"revision":str(uuid.uuid4()),"components":[{"id":"context-assembly","descriptor_sha256":"a"*64,"artifact_sha256":"b"*64}],"withdrawn":[]}));(assets/"current").symlink_to(prior)
            removed=assets/"releases/removed";value=engine.seal_catalog(removed,str(uuid.uuid4()),assets)
            self.assertEqual(value["components"],[]);self.assertEqual(value["withdrawn"],["context-assembly"])
            (assets/"current").unlink();(assets/"current").symlink_to(removed)
            installed=assets/"releases/installed";folder=installed/engine.ROOT/engine.ID;folder.mkdir(parents=True);declaration=self.descriptor();binary=folder/"artifact"/engine.BINARY;binary.parent.mkdir();binary.write_bytes(b"fixture");declaration["artifact_sha256"]=hashlib.sha256(binary.read_bytes()).hexdigest();(folder/"component.json").write_text(json.dumps(declaration))
            value=engine.seal_catalog(installed,str(uuid.uuid4()),assets);self.assertEqual(value["withdrawn"],[]);self.assertEqual(value["components"][0]["descriptor_sha256"],hashlib.sha256((folder/"component.json").read_bytes()).hexdigest())
    def test_cargo_cache_reuse_requires_manifests_recipe_and_integrity(self):
        with tempfile.TemporaryDirectory()as temporary:
            assets=Path(temporary)/"assets";state=Path(temporary)/"state";key="a"*64;prior=state/"engine-component-builds"/key;receipt=assets/"artifacts"/key/"engine-receipt.json";receipt.parent.mkdir(parents=True)
            for name in ("cargo","target"):(prior/name).mkdir(parents=True);(prior/name/"fixture").write_bytes(name.encode())
            inputs={"Cargo.toml":"b"*64,"Cargo.lock":"c"*64,"crates/context-service/src/main.rs":"d"*64};hashes={name:executor.Executor.artifact_hashes(prior/name)for name in ("cargo","target")};receipt.write_text(json.dumps({"input_key":key,"recipe":"fixture-recipe","inputs":inputs,"cargo_cache_hashes":hashes}))
            fresh=state/"engine-component-builds/new";fresh.mkdir();changed={**inputs,"crates/context-service/src/main.rs":"e"*64}
            self.assertEqual(engine.seed_cache(fresh,assets,state,changed,"fixture-recipe",executor),key)
            self.assertIsNone(engine.seed_cache(state/"different",assets,state,{**changed,"Cargo.lock":"f"*64},"fixture-recipe",executor))
            (prior/"target/fixture").write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError,"changed"):engine.seed_cache(state/"other",assets,state,changed,"fixture-recipe",executor)
    def test_descriptor_rejects_unsupported_operation_and_recipe(self):
        with tempfile.TemporaryDirectory()as temporary:
            path=Path(temporary)/"component.json";value=self.descriptor();path.write_text(json.dumps(value));self.assertEqual(engine.descriptor(path),value)
            value["operation"]="arbitrary.execute";path.write_text(json.dumps(value))
            with self.assertRaisesRegex(ValueError,"contract"):engine.descriptor(path)
    def test_local_dependency_closure_includes_contract_changes(self):
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);(root/"Cargo.toml").write_text('[workspace]\n');(root/"Cargo.lock").write_text('version=4\n')
            for crate,dependency in [("context-service","../context-assembly"),("context-assembly","../context-contracts"),("context-contracts",None)]:
                folder=root/"crates"/crate;folder.mkdir(parents=True);(folder/"src").mkdir();(folder/"src/lib.rs").write_text('pub fn fixture() {}\n')
                (folder/"Cargo.toml").write_text('[package]\nname="'+crate+'"\nversion="0.1.0"\n'+('[dependencies]\nfixture={path="'+dependency+'"}\n'if dependency else''))
            before=engine.closure(root);contract=root/"crates/context-contracts/src/lib.rs";contract.write_text('pub fn changed() {}\n')
            self.assertNotEqual(before,engine.closure(root));self.assertIn('crates/context-contracts/src/lib.rs',before)
            contract.unlink();contract.symlink_to('/etc/passwd')
            with self.assertRaisesRegex(ValueError,"symlink"):engine.closure(root)
    def test_sealed_binary_is_generation_local_and_tampering_rejected(self):
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);generation=root/"releases/one";folder=generation/engine.ROOT/engine.ID;folder.mkdir(parents=True);(folder/"component.json").write_text(json.dumps(self.descriptor()))
            artifact=root/"artifacts/key";artifact.mkdir(parents=True);binary=artifact/engine.BINARY;binary.write_bytes(b'fixture artifact');sha=hashlib.sha256(binary.read_bytes()).hexdigest();output={"package":engine.ID,"build_key":"key","artifact_sha256":sha}
            engine.seal(generation,[output],root);engine.seal(generation,[output],root)
            self.assertEqual((folder/'artifact'/engine.BINARY).read_bytes(),b'fixture artifact');self.assertEqual(json.loads((folder/'component.json').read_bytes())["artifact_sha256"],sha)
            binary.write_bytes(b'tampered')
            with self.assertRaisesRegex(ValueError,"changed"):engine.seal(generation,[output],root)
    def test_staged_generation_retries_preserve_source_and_sealed_artifact(self):
        import uuid
        spec=importlib.util.spec_from_file_location("engine_release",ROOT/"build-service/release.py")
        release=importlib.util.module_from_spec(spec);spec.loader.exec_module(release)
        with tempfile.TemporaryDirectory()as temporary:
            root=Path(temporary);job=str(uuid.uuid4());manifest=engine.ROOT+engine.ID+"/component.json"
            files={"v3/ui/index.html":(0o644,b"fixture"),manifest:(0o644,json.dumps(self.descriptor()).encode())};blob=release.archive(files,lambda _:None)
            generation=release.stage_assets(root,job,blob,lambda _:None);artifact=root/"artifacts/key";artifact.mkdir(parents=True);binary=artifact/engine.BINARY;binary.write_bytes(b"fixture")
            output={"package":engine.ID,"build_key":"key","artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest()};engine.seal(generation,[output],root)
            self.assertEqual(release.stage_assets(root,job,blob,lambda _:None),generation)
            changed=dict(files);value=self.descriptor();value["implementation_version"]="0.2.0";changed[manifest]=(0o644,json.dumps(value).encode())
            with self.assertRaisesRegex(ValueError,"changed"):release.stage_assets(root,job,release.archive(changed,lambda _:None),lambda _:None)
    def test_metadata_artifact_preserves_large_json_and_rejects_links(self):
        with tempfile.TemporaryDirectory()as temporary:
            path=Path(temporary)/"metadata.json";value={"fixture":"x"*(1024*1024+1)};path.write_text(json.dumps(value))
            self.assertEqual(executor.Executor.metadata_receipt(path),value)
            path.unlink();path.symlink_to("/etc/passwd")
            with self.assertRaisesRegex(ValueError,"identity"):executor.Executor.metadata_receipt(path)
    def test_core_depinfo_ignores_service_only_implementation(self):
        with tempfile.TemporaryDirectory()as temporary:
            target=Path(temporary);deps=target/"release/deps";deps.mkdir(parents=True)
            (deps/"webagent_v3-aa.d").write_text('target: src/main.rs\n');(deps/"contracts-aa.d").write_text('target: crates/context-contracts/src/lib.rs\n');(deps/"service-aa.d").write_text('target: crates/context-service/src/main.rs\n')
            def package(identity,name):return {"id":identity,"name":name,"source":None,"targets":[{"name":name,"kind":["bin"]}]}
            metadata={"packages":[package('root','webagent-v3'),package('contracts','contracts'),package('service','service')],"resolve":{"nodes":[{"id":"root","deps":[{"pkg":"contracts","dep_kinds":[{"kind":None}]}]},{"id":"contracts","deps":[]},{"id":"service","deps":[]}]}}
            actual=executor.Executor.core_dependencies(metadata,target)
            self.assertEqual(actual,{'src/main.rs','crates/context-contracts/src/lib.rs'})
if __name__=='__main__':unittest.main()
