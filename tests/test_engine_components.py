"""Engine publication regression checks: closure, immutable sealing and live root dependency isolation."""
import hashlib,importlib.util,json,tempfile,unittest,types
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location("engine_build",ROOT/"build-service/engine_components.py")
engine=importlib.util.module_from_spec(spec);spec.loader.exec_module(engine)
spec=importlib.util.spec_from_file_location("engine_executor",ROOT/"build-service/executor.py")
executor=importlib.util.module_from_spec(spec);spec.loader.exec_module(executor)
class EngineTests(unittest.TestCase):
    # Construct the coordinated context interface-v2 descriptor used by publication fixtures.
    def descriptor(self):
        return {"schema_version":1,"id":"context-assembly","group":"engine-loop","interface_version":2,"implementation_version":"0.2.0","operation":"context.prepare","modes":["live","linked"],"runtime_requirements":[],"build":{"package":engine.BINARY,"source_inputs":engine.SOURCE_INPUTS,"toolchain":"1.90.0","features":[],"runtime_assets":[]},"executable":"artifact/"+engine.BINARY,"limits":{"max_request_bytes":16777216,"max_response_bytes":16777216,"timeout_ms":30000,"max_concurrency":1}}
    # Preserve structured proof identity in artifact files despite diagnostic-log redaction.
    def test_structured_smoke_artifact_survives_true_environment_redaction(self):
        import test_admin_build as fixtures
        proof={"mode":"linked","equivalence":True,"interface_version":2,"prepared":[{},{}],"background_prepared":[{},{}],"boot_prepared":[{},{}],"assets_prepared":[{},{}],"capability_prepared":[{},{}],"provider_prepared":[{},{}],"service_started":False};raw=json.dumps(proof)
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
    def test_optimized_modes_share_target_and_live_cleans_only_root_release(self):
        linked=executor.Executor.linked_recipe()
        self.assertNotIn("--target-dir",linked);self.assertNotIn("--no-default-features",linked)
        self.assertIn("--release",linked)
        live=executor.Executor.live_recipe();self.assertEqual(live[:2],["sh","-c"])
        self.assertEqual(live[2].splitlines(),[
            "set -e",
            "cargo clean --release --package webagent-v3 --locked --offline",
            "cargo build --workspace --release --locked --offline --no-default-features --features webagent-v3/live-context"])
        docker=(ROOT/"Dockerfile").read_text()
        self.assertNotIn("/build/linked-target",docker)
        self.assertLess(docker.index("/out/stable/webagent-v3 --context-smoke"),docker.index("cargo clean --release --package webagent-v3"))
        self.assertLess(docker.index("cargo clean --release --package webagent-v3"),docker.index("cargo build --workspace --release"))
    def test_linked_dependency_attestation_excludes_debug_and_unresolved_services(self):
        worker=executor.Executor(None,None,{})
        files={"v3/src/main.rs":(0o644,b"main"),"v3/crates/context-assembly/src/lib.rs":(0o644,b"implementation")}
        def package(identity,name):return {"id":identity,"name":name,"source":None,"targets":[{"name":name,"kind":["bin"]}]}
        metadata={"packages":[package("root","webagent-v3"),package("implementation","context-assembly"),package("service","context-service")],"resolve":{"nodes":[{"id":"root","deps":[{"pkg":"implementation","dep_kinds":[{"kind":None}]}]},{"id":"implementation","deps":[]},{"id":"service","deps":[]}]}}
        with tempfile.TemporaryDirectory()as temporary:
            target=Path(temporary);release=target/"release/deps";release.mkdir(parents=True)
            debug=target/"debug/deps";debug.mkdir(parents=True)
            (release/"webagent_v3-aa.d").write_text("target: src/main.rs\n")
            implementation=release/"context_assembly-aa.d";implementation.write_text("target: crates/context-assembly/src/lib.rs\n")
            (release/"context_service-aa.d").write_text("target: crates/context-service/src/main.rs\n")
            (debug/"fixture-aa.d").write_text("target: src/test_only.rs\n")
            self.assertEqual(set(worker.linked_dependencies(metadata,target,files)),set(files))
            self.assertIn("src/test_only.rs",worker.compiler_dependencies(target))
            self.assertIn("crates/context-service/src/main.rs",worker.compiler_dependencies(target))
            implementation.unlink()
            with self.assertRaisesRegex(ValueError,"attestation"):worker.linked_dependencies(metadata,target,files)
    # Require live-service lifecycle evidence and exact prepared-case equivalence with linked mode.
    def test_live_optimized_smoke_requires_lifecycle_and_equal_prepared_cases(self):
        linked={"mode":"linked","equivalence":True,"interface_version":2,"prepared":[{"envelope":"fixture1"},{"envelope":"fixture2"}],"background_prepared":[{},{}],"boot_prepared":[{},{}],"assets_prepared":[{},{}],"capability_prepared":[{},{}],"provider_prepared":[{},{}],"service_started":False}
        live={**linked,"mode":"live","service_reused":True,"service_drained":True}
        self.assertEqual(executor.Executor.live_smoke(json.dumps(live),linked)["mode"],"live")
        for changed in ({**live,"service_reused":False},{**live,"service_drained":False},{**live,"prepared":[{},{}]}):
            with self.assertRaises(ValueError):executor.Executor.live_smoke(json.dumps(changed),linked)
    # Reject incomplete or wrong-mode proofs before accepting linked equivalence.
    def test_optimized_linked_proof_rejects_live_mode_and_failed_equivalence(self):
        self.assertEqual(executor.Executor.linked_smoke('{"mode":"linked","equivalence":true,"interface_version":2,"prepared":[{},{}],"background_prepared":[{},{}],"boot_prepared":[{},{}],"assets_prepared":[{},{}],"capability_prepared":[{},{}],"provider_prepared":[{},{}],"service_started":false}')["mode"],"linked")
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
        import test_admin_build as fixtures
        from unittest.mock import Mock
        for invalid in ("--ignored","name;command","name with spaces","name/other","x"*161):
            with self.assertRaises(ValueError):executor.test_filter(invalid)
        with tempfile.TemporaryDirectory()as temporary:
            runner=types.SimpleNamespace(**{name:getattr(fixtures.host.Runner,name)for name in ("execution_entries","execution_application_blob","execution_source_digest")},compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64})
            worker=executor.Executor(runner,None,{"state_root":temporary});worker.phase=Mock(return_value="checked");worker.dependencies=Mock(return_value="offline ready\n")
            job={"job_id":str(uuid.uuid4()),"test_filter":"engine_components::"};blob=fixtures.context(extra={"v3/src/main.rs":"fn main() {}"})
            worker.check(job,blob,"test",60,lambda path,data:fixtures.host.release.extract(path,data,fixtures.host.context_digest))
            runtime=worker.phase.call_args_list[-1].args[5]
            self.assertEqual(runtime,["cargo","test","--locked","--offline","engine_components::","--","--test-threads=1","--nocapture"])
            self.assertNotIn("engine_components::",worker.phase.call_args_list[0].args[5])
            certificate=json.loads((Path(temporary)/"verification-certificates"/(job["verification_key"]+".json")).read_text())
            self.assertEqual(certificate["binding"]["source_sha256"],worker.verification_source(blob))
            self.assertEqual(certificate["binding"]["test_filter"],"engine_components::")
    # Bind compiler memory evidence to its container and reject unenforced limits or OOM outcomes.
    def test_cap_requires_kernel_enforcement_and_oom_cannot_pass(self):
        import threading,uuid
        from unittest.mock import Mock
        from collections import defaultdict
        cap=executor.COMPILER_MEMORY_CAP
        for scenario in ("success","oom","unenforced"):
            with self.subTest(scenario=scenario),tempfile.TemporaryDirectory()as temporary:
                resources=types.SimpleNamespace(enabled=True,enter=Mock(),attached=Mock(),lock=threading.RLock(),rows=defaultdict(dict),diagnostic_check=lambda _:False)
                runner=types.SimpleNamespace(compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64},command=Mock(side_effect=["a"*64,"started","compiler succeeded"]),job_context=types.SimpleNamespace(),resource_progress=Mock())
                observed={"container_id":"a"*64,"memory_max":cap if scenario!="unenforced"else None,"swap_max":0,"oom_events":0,"oom_kills":0,"peak_memory":1024}
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
        import test_admin_build as fixtures
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory()as temporary:
            runner=types.SimpleNamespace(**{name:getattr(fixtures.host.Runner,name)for name in ("execution_entries","execution_application_blob","execution_source_digest")},compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64})
            worker=executor.Executor(runner,None,{"state_root":temporary});worker.phase=Mock(return_value="checked");worker.dependencies=Mock(return_value="offline ready\n")
            blob=fixtures.context(extra={"v3/src/main.rs":"fn main() {}"})
            extract=lambda path,data:fixtures.host.release.extract(path,data,fixtures.host.context_digest)
            job={"job_id":str(uuid.uuid4())};worker.check(job,blob,"test",60,extract)
            self.assertEqual([(call.args[1],call.args[6])for call in worker.phase.call_args_list],[("compile_test","engine:rust1.90:compile_test"),("test","engine:rust1.90:test")])
            worker.phase.reset_mock();check_job={"job_id":str(uuid.uuid4())};worker.check(check_job,blob,"check",60,extract)
            self.assertEqual(worker.phase.call_args_list[-1].args[6],"engine:rust1.90:check")
            self.assertNotEqual(job["verification_key"],check_job["verification_key"])
    def test_job_view_exposes_bounded_waiting_cause_and_deadline(self):
        import test_admin_build as fixtures
        from unittest.mock import Mock
        broker=object.__new__(fixtures.host.Broker);broker.runner=types.SimpleNamespace(secrets=[]);broker._queue_preflight=Mock(return_value=None);broker.jobs={};broker.active=None;broker.active_ui=None
        job={"job_id":"fixture","status":"running","resource_waiting":{"phase":"test","reason":"headroom","deadline_at":1234,"estimate_bytes":1024}}
        self.assertEqual(broker._view(job)["resource_waiting"],job["resource_waiting"])
    # Permit hot replacement only when both current consumers attest live execution.
    def test_actual_execution_mode_gates_hot_engine_replacement(self):
        import test_admin_build as fixtures
        from unittest.mock import Mock
        runner=fixtures.host.Runner();runner.current_asset_id=Mock(return_value="publication");runner.boot_adoption=Mock(return_value=None)
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
    # Accept identical staging retries and reject a changed source behind the same generation.
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
            changed=dict(files);value=self.descriptor();value["implementation_version"]="0.2.1";changed[manifest]=(0o644,json.dumps(value).encode())
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
    def test_only_rust_test_phases_receive_stack_headroom(self):
        self.assertEqual(executor.TEST_STACK_BYTES,32*1024*1024)
        self.assertEqual(executor.Executor.test_stack_env("test"),["-e","RUST_MIN_STACK=33554432"])
        for phase in ("compile_test","compile_linked","compile_release","linked_smoke","live_smoke","dependency","engine_metadata"):
            self.assertEqual(executor.Executor.test_stack_env(phase),[])
class EngineRebaseAttestationOrderTests(unittest.TestCase):
    """The deferred publication rebase is re-proved before each optimized compile, not after the suite."""
    def test_attestation_precedes_compile_test_and_compile_release(self):
        import test_admin_build as fixtures
        import uuid
        from unittest.mock import Mock,patch
        timeline=[];attested=[]
        runner=types.SimpleNamespace(execution_application_blob=fixtures.host.Runner.execution_application_blob,execution_entries=fixtures.host.Runner.execution_entries,
            execution_source_digest=lambda root:"source-digest",compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64},
            resource_progress=lambda job:None,core_compile_inputs=lambda:{"v3/src/main.rs"},
            attest_release_base=lambda job,blob:attested.append(blob)or timeline.append("attest"))
        with tempfile.TemporaryDirectory()as temporary:
            worker=executor.Executor(runner,None,{"state_root":temporary});job={"job_id":str(uuid.uuid4())}
            created=(Path(temporary)/"execution"/job["job_id"]/"output/cargo/registry");created.mkdir(parents=True)
            def phase(job,name,kind,source,output,argv,profile,timeout=3600,network=False):
                timeline.append(name)
                if name=="compile_linked":
                    (output/"target/release").mkdir(parents=True,exist_ok=True);(output/"target/release/webagent-v3").write_bytes(b"binary")
                if name=="compile_release":
                    (output/"artifacts").mkdir(exist_ok=True);(output/"artifacts/webagent-v3").write_bytes(b"live")
                if name=="engine_metadata":
                    compiler=output/"compiler";compiler.mkdir(exist_ok=True);(compiler/"mold").write_bytes(b"mold")
                    for mode in ("linked","live"):(output/(mode+"-comment.txt")).write_bytes(b"mold 1.10.1")
                return ""
            worker.phase=phase
            worker.validate_compiler_recipe=Mock(return_value=False);worker.cached_engine=Mock(return_value=None)
            worker.seed_cargo_cache=Mock(return_value=False);worker.dependencies=Mock(return_value="offline ready\n")
            worker.retain_test_manifest=Mock();worker.focused_tests=Mock(return_value="")
            worker.linked_dependencies=Mock(return_value={});worker.linked_smoke=Mock(return_value={"mode":"linked"});worker.live_smoke=Mock(return_value={"mode":"live"})
            worker.smoke_artifact=Mock(return_value={});worker.metadata_receipt=Mock(return_value={"packages":[],"resolve":{"nodes":[]}})
            worker.core_dependencies=Mock(return_value={"v3/src/main.rs"});worker.compiler_dependencies=Mock(return_value={"src/main.rs"})
            worker.collect_runtime_artifacts=Mock();worker.context_component_recipe=Mock(return_value="recipe");worker.artifact_hashes=Mock(return_value={})
            worker.retain_engine=Mock();worker.test_proof=Mock(return_value="proof")
            blob=fixtures.context(extra={"v3/src/main.rs":"fn main(){}"})
            with patch.object(executor,"MOLD_SHA256",hashlib.sha256(b"mold").hexdigest()):
                worker.engine(job,blob,lambda root,data:root.mkdir(parents=True,exist_ok=True),lambda data:"d"*64)
            self.assertEqual(timeline.count("attest"),3)
            self.assertIs(attested[0],blob)
            self.assertEqual([name for name in timeline if name in("attest","compile_test","test","compile_release")],
                ["attest","attest","compile_test","test","attest","compile_release"])
if __name__=='__main__':unittest.main()
