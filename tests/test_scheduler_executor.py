"""Independent cache assertions: actual compiled inputs and complete artifact bytes govern reuse."""
import hashlib,importlib.util,tempfile,types,unittest,unittest.mock,uuid
from pathlib import Path
import test_admin_build as fixtures
spec=importlib.util.spec_from_file_location("scheduler_executor",Path(__file__).parents[1]/"build-service/executor.py")
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
class CacheTests(unittest.TestCase):
    """Use real archives/files to distinguish runtime assets from compiled dependencies."""
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name);self.job={"job_id":str(uuid.uuid4())}
        self.extra={"v3/src/main.rs":"fn main() {}","v3/config/embedded.json":"compiled value",
            "v3/plugins/abilities/Core/example/example.skill.md":"guidance"}
        self.blob=fixtures.context(extra=self.extra)
        self.worker=module.Executor(types.SimpleNamespace(execution_entries=fixtures.host.Runner.execution_entries,compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64}),None,{"state_root":str(self.root)})
        self.artifacts=self.root/"execution"/self.job["job_id"]/"output/artifacts";self.artifacts.mkdir(parents=True)
        (self.artifacts/"webagent-v3").write_bytes(b"verified compiler output")
        registry=self.artifacts/"optimization-cargo/registry";registry.mkdir(parents=True);(registry/"cache").write_bytes(b"verified registry")
        tests=self.artifacts/"tests";tests.mkdir();(tests/"webagent_v3-0123456789abcdef").write_bytes(b"verified test output")
        support=self.artifacts/"test-support/debug/webagent-context-service";support.parent.mkdir(parents=True);support.write_bytes(b"debug official service")
        service=tests/"service-0123456789abcdef";service.write_bytes(b"integration service")
        def entry(path,target,source,kind):
            return {"file":path.relative_to(self.artifacts).as_posix(),"sha256":hashlib.sha256(path.read_bytes()).hexdigest(),"package_id":"official","target_name":target,"source_path":source,"kind":kind}
        import json
        manifest={"schema_version":1,"tests":[entry(tests/"webagent_v3-0123456789abcdef","webagent-v3","/build/src/main.rs",["bin"]),entry(service,"service","/build/crates/context-service/tests/service.rs",["test"])],"support":{**entry(support,"webagent-context-service","/build/crates/context-service/src/main.rs",["bin"]),"destination":"target/debug/webagent-context-service","profile":"debug"}}
        (self.artifacts/"test-manifest.json").write_text(json.dumps(manifest))
        self.worker.retain_engine(self.job,self.blob,{"/build/src/main.rs","/build/config/embedded.json"},self.worker.artifact_hashes(self.artifacts))
    def tearDown(self):
        """Remove only this test's private artifact/state directory."""
        self.temp.cleanup()
    def candidate(self,extra):
        """Preserve required archive inputs and vary explicit dependency/asset bytes."""
        return fixtures.context(extra={**self.extra,**extra})
    def output(self):
        """Give each attempted lookup an independent copy destination."""
        path=self.root/uuid.uuid4().hex;path.mkdir();return path
    def test_runtime_guidance_change_reuses_all_verified_artifacts(self):
        """A guidance edit does not change core compilation or drop registry/test attestations."""
        cached=self.worker.cached_engine(self.candidate({"v3/plugins/abilities/Core/example/example.skill.md":"new guidance"}),self.output())
        self.assertIsNotNone(cached)
        self.assertEqual(self.worker.artifact_hashes(cached[0]),self.worker.artifact_hashes(self.artifacts))
    def test_changed_or_new_rust_input_requires_compilation(self):
        """Added code is as significant as changed existing code for reuse decisions."""
        for changed in [{"v3/src/main.rs":"fn main(){panic!()}"},
            {"v3/src/new.rs":"fn new(){}"},{"v3/Cargo.lock":"different lock"}]:
            self.assertIsNone(self.worker.cached_engine(self.candidate(changed),self.output()))
    def test_embedded_json_is_a_compile_dependency(self):
        """File extensions cannot conceal a compiler-declared JSON dependency."""
        self.assertIsNone(self.worker.cached_engine(self.candidate({"v3/config/embedded.json":"changed compile value"}),self.output()))
    def test_relative_cargo_dependencies_include_embedded_inputs(self):
        """Real Cargo relative paths retain the same complete local dependency closure."""
        self.worker.retain_engine(self.job,self.blob,{"src/main.rs","config/../config/embedded.json",
            "/output/cargo/registry/src/external/lib.rs"},self.worker.artifact_hashes(self.artifacts))
        self.assertIsNone(self.worker.cached_engine(self.candidate({"v3/config/embedded.json":"changed"}),self.output()))
    def test_missing_or_escaped_local_dependency_fails_closed(self):
        """A successful compiler cannot attest files outside its captured application."""
        for name in ("src/missing.rs","../private"):
            with self.assertRaises(ValueError):
                self.worker.retain_engine(self.job,self.blob,{name},self.worker.artifact_hashes(self.artifacts))
    def test_dep_info_comments_are_not_source_paths(self):
        """Actual Cargo environment records cannot be confused with local file dependencies."""
        target=self.root/"dep-info";folder=target/"debug/deps";folder.mkdir(parents=True)
        (folder/"fixture.d").write_text("/output/fixture: src/main.rs config/embedded.json\n# env-dep:CARGO_MANIFEST_DIR=/build\n# env-dep:OUT_DIR=/output/generated\n")
        self.assertEqual(self.worker.compiler_dependencies(target),{"src/main.rs","config/embedded.json"})
    def previous_cargo_cache(self):
        """Create terminal phase evidence and an exact captured source for a private cache fixture."""
        import threading
        previous=str(uuid.uuid4());state=self.root/(previous+".json")
        state.write_text(__import__("json").dumps({"job_id":previous,"status":"failed","compiler_recipe":self.worker.compiler_recipe()}))
        folder=self.root/previous;folder.mkdir();(folder/"candidate.tar").write_bytes(self.blob)
        execution=self.root/"execution"/previous
        fixtures.host.release.extract(execution/"source",self.blob,fixtures.host.context_digest)
        target=execution/"output/target";deps=target/"debug/deps";deps.mkdir(parents=True)
        (deps/"fixture.d").write_text("output: src/main.rs config/embedded.json\n# env-dep:CARGO_MANIFEST_DIR=/build\n")
        (deps/"dependency.rlib").write_bytes(b"compatible dependency objects")
        cargo=execution/"output/cargo";cargo.mkdir();(cargo/"registry-cache").write_bytes(b"locked registry bytes")
        self.worker.runner=types.SimpleNamespace(**{name:getattr(fixtures.host.Runner,name)
            for name in ("execution_entries","execution_application_blob","execution_source_digest","execution_source_digest_blob")})
        self.worker.runner.compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64}
        self.worker.resources=types.SimpleNamespace(lock=threading.RLock(),rows={name:{
            "job_id":previous,"phase":name,"state":"finished","profile":profile}
            for name,profile in {"compile_test":"engine:rust1.90:compile_test",
                "test":"engine:rust1.90:test","compile_release":"engine:rust1.90:release"}.items()})
        return previous,execution
    def test_compatible_dependency_cache_is_copied_without_certifying_artifacts(self):
        """A post-compilation failure may seed dependency objects, while root outputs still require rebuilding."""
        previous,folder=self.previous_cargo_cache();output=self.output()
        self.assertTrue(self.worker.seed_cargo_cache(self.job,self.blob,output))
        self.assertEqual(self.job["cargo_cache_reuse"],previous)
        self.assertEqual((output/"target/debug/deps/dependency.rlib").read_bytes(),b"compatible dependency objects")
        self.assertFalse((output/"artifacts").exists())
    def test_missing_or_different_compiler_recipe_never_seeds_old_objects(self):
        import json
        previous,_=self.previous_cargo_cache();receipt=self.root/(previous+".json")
        for recipe in (None,module.RUST+":single-job:debug0:complete-tests-v6"):
            value={"job_id":previous,"status":"failed"}
            if recipe is not None:value["compiler_recipe"]=recipe
            receipt.write_text(json.dumps(value));output=self.output()
            self.assertFalse(self.worker.seed_cargo_cache(self.job,self.blob,output))
            self.assertFalse((output/"cargo").exists());self.assertFalse((output/"target").exists())
    def test_rejected_verified_resume_does_not_relabel_old_compiler_cache(self):
        import json
        from unittest.mock import patch
        old_recipe=module.RUST+":single-job:debug0:complete-tests-v6"
        self.job["compiler_recipe"]=old_recipe
        self.worker.runner.execution_application_blob=fixtures.host.Runner.execution_application_blob
        receipt={"inputs_sha256":fixtures.host.context_digest(self.worker.runner.execution_application_blob(self.blob))[0],"artifacts":self.worker.artifact_hashes(self.artifacts),"test_proof":{"executor_sha256":"old executor"},"log":"old tests"}
        (self.artifacts.parent.parent/"verified.json").write_text(json.dumps(receipt))
        with patch.object(self.worker,"validate_compiler_recipe",return_value=False):
            with self.assertRaisesRegex(ValueError,"Retained verification"):
                self.worker.engine(self.job,self.blob,None,lambda blob:fixtures.host.context_digest(blob)[0])
        self.assertEqual(self.job["compiler_recipe"],old_recipe)
    def test_completed_artifact_recipe_names_parallel_compiler(self):
        import json
        receipt=next((self.root/"engine-artifacts").glob("*.json"))
        self.assertEqual(json.loads(receipt.read_text())["recipe"],self.worker.compiler_recipe())
        self.assertIn(":mold1.10.1:",self.worker.compiler_recipe())
    def test_dependency_cache_rejects_changed_embedded_input_and_incomplete_phase(self):
        """Measured phase completion alone cannot override the compiler input closure."""
        self.previous_cargo_cache()
        self.assertFalse(self.worker.seed_cargo_cache(self.job,self.candidate({"v3/config/embedded.json":"changed"}),self.output()))
        self.worker.resources.rows["test"]["state"]="failed"
        self.assertFalse(self.worker.seed_cargo_cache(self.job,self.blob,self.output()))
    def test_dependency_cache_rejects_source_drift_and_symlinks(self):
        """Caches are private captured inputs; filesystem substitution cannot seed a compiler."""
        previous,folder=self.previous_cargo_cache()
        link=folder/"output/target/debug/deps/escaped";link.symlink_to("/etc/passwd")
        with self.assertRaisesRegex(ValueError,"symlink"):
            self.worker.seed_cargo_cache(self.job,self.blob,self.output())
        link.unlink();(folder/"source/v3/src/main.rs").write_text("changed source")
        output=self.output()
        self.assertFalse(self.worker.seed_cargo_cache(self.job,self.blob,output))
        self.assertFalse((output/"cargo").exists());self.assertFalse((output/"target").exists())
        self.assertEqual(self.job["cargo_cache_rejections"],[{"job_id":previous,"reason":"previous_execution_source_differs_from_candidate"}])
    def test_rebased_candidate_skips_previous_cache_without_copying_it(self):
        previous,folder=self.previous_cargo_cache();output=self.output()
        path=folder/"source/v3/ui/chat/chat.css";path.parent.mkdir(parents=True);path.write_text("original UI snapshot")
        self.assertFalse(self.worker.seed_cargo_cache(self.job,self.blob,output))
        self.assertNotIn("cargo_cache_reuse",self.job)
        self.assertFalse((output/"cargo").exists());self.assertFalse((output/"target").exists())
        self.assertEqual(self.job["cargo_cache_rejections"],[{"job_id":previous,"reason":"previous_execution_source_differs_from_candidate"}])
    def test_runtime_roles_package_only_declared_artifacts_and_stream_to_disk(self):
        """Loop contexts exclude API registry and retained tests; API keeps its required closure."""
        import tarfile
        dockerfile=(Path(__file__).parents[1]/"Dockerfile").read_text()
        blob=fixtures.context(extra={**self.extra,"v3/Dockerfile":dockerfile})
        for name in ("core-inputs.txt",*module.BINARIES):
            (self.artifacts/name).write_bytes(b"verified output")
        folder=self.artifacts/"modules";folder.mkdir();(folder/"descriptor.json").write_bytes(b"verified module")
        compiler=self.artifacts/"compiler";compiler.mkdir()
        for name in ("mold","copyright","LICENSE.gz"):(compiler/name).write_bytes(b"verified compiler support")
        for role in ("api","loop"):
            path=self.root/(role+".tar")
            self.assertEqual(self.worker.package(blob,self.artifacts,fixtures.host.Runner.execution_entries,role,path),path)
            with tarfile.open(path)as archive:
                names={name.removeprefix("scheduler-out/")for name in archive.getnames()if name.startswith("scheduler-out/")}
                recipe=archive.extractfile("v3/Dockerfile").read().decode()
                self.assertIn("FROM scratch AS tested_builder"if role=="loop"else"FROM "+module.RUST+" AS tested_builder",recipe)
            self.assertNotIn("tests/webagent_v3-0123456789abcdef",names)
            if role=="loop":self.assertEqual(names,{"webagent-v3","core-inputs.txt","webagent-context-service","webagent-background-service","webagent-boot-service","webagent-assets-service","webagent-capability-service","webagent-provider-service"})
            else:
                self.assertIn("optimization-cargo/registry/cache",names);self.assertIn("modules/descriptor.json",names)
                self.assertTrue({"compiler/mold","compiler/copyright","compiler/LICENSE.gz"}<=names)
    def test_runtime_python_copy_normalization_keeps_private_archive_modes(self):
        import io,tarfile
        dockerfile=(Path(__file__).parents[1]/"Dockerfile").read_text()
        files=fixtures.host.Runner.execution_entries(self.blob)
        files["v3/Dockerfile"]=(0o644,dockerfile.encode())
        for name in ("worker.py","ability_plugin_worker.py","sandbox.py","launch.py","promote.py"):
            files["v3/"+name]=(0o700,b"pass\n")
        blob=fixtures.host.release.archive(files,fixtures.host.context_digest);before=hashlib.sha256(blob).hexdigest()
        output=self.worker.package(blob,self.artifacts,fixtures.host.Runner.execution_entries)
        with tarfile.open(fileobj=io.BytesIO(output))as archive:
            text=archive.extractfile("v3/Dockerfile").read().decode()
            self.assertIn("COPY --chmod=0755 v3/worker.py v3/ability_plugin_worker.py v3/sandbox.py v3/launch.py v3/promote.py /opt/v3/",text)
            self.assertEqual(archive.getmember("v3/worker.py").mode,0o700)
            self.assertEqual(archive.extractfile("v3/worker.py").read(),b"pass\n")
        self.assertEqual(hashlib.sha256(blob).hexdigest(),before)
        self.assertEqual(module.Executor.runtime_python_modes(text),text)
    def test_unknown_runtime_python_copy_contract_fails_closed(self):
        for text in ("COPY v3/worker.py /other/", "COPY --chmod=0777 v3/worker.py /opt/v3/"):
            with self.assertRaisesRegex(ValueError,"COPY contract"):module.Executor.runtime_python_modes(text)

    def test_exact_suite_proof_reuses_tests_but_changed_fixture_runs_them(self):
        from unittest.mock import patch
        stable=self.artifacts/"stable";stable.mkdir()
        for name in ("smoke.json","mode-equivalence.json","linker-proof.json"):(stable/name).write_text("{}")
        self.worker.runner=types.SimpleNamespace(**{name:getattr(fixtures.host.Runner,name)for name in
            ("execution_entries","execution_application_blob","execution_source_digest","execution_source_digest_blob")},compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64})
        hashes=self.worker.artifact_hashes(self.artifacts)
        self.worker.retain_engine(self.job,self.blob,{"src/main.rs","config/embedded.json"},hashes,"test result: ok. 2 passed; 0 failed\n")
        for candidate,reuse in [(self.blob,True),(self.candidate({"v3/config/runtime-fixture.json":"changed"}),False)]:
            job={"job_id":str(uuid.uuid4())}
            with patch.object(self.worker,"validate_compiler_recipe",return_value=False),patch.object(self.worker,"phase",return_value="test result: ok. 2 passed; 0 failed\n")as phase:
                artifacts,log=self.worker.engine(job,candidate,lambda p,b:fixtures.host.release.extract(p,b,fixtures.host.context_digest),fixtures.host.context_digest)
            self.assertEqual(phase.call_count,0 if reuse else 2)
            self.assertEqual("test_reuse"in job,reuse)
            self.assertEqual(self.worker.artifact_hashes(artifacts),hashes)

    def test_registry_or_test_tampering_is_rejected(self):
        """Validate every packaged artifact rather than just the main executable."""
        (self.artifacts/"optimization-cargo/registry/cache").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError,"Cached compiler output changed"):self.worker.cached_engine(self.blob,self.output())

if __name__=="__main__":unittest.main()


class FeatureProfileTests(unittest.TestCase):
    def test_allowlist_keeps_default_commands_and_rejects_arbitrary_args(self):
        self.assertEqual(module.check_argv("check"),["cargo","check","--locked","--offline"])
        self.assertEqual(module.check_argv("test"),["cargo","test","--locked","--offline","--no-run"])
        self.assertEqual(module.check_argv("check","live"),["cargo","check","--locked","--offline","--no-default-features","--features","live-context"])
        for bad in ["all","--all-features",{},[],True]:
            with self.assertRaisesRegex(ValueError,"Unsupported"):module.check_profile(bad)
        with self.assertRaisesRegex(ValueError,"only to compile"):module.check_argv("test","live")
    def test_executor_passes_fixed_profile_to_admitted_compiler_phase(self):
        from unittest.mock import patch
        for profile in [None,"default","live"]:
            with tempfile.TemporaryDirectory()as directory:
                runner=types.SimpleNamespace(execution_application_blob=lambda b:b,execution_source_digest=lambda p:"fixed")
                worker=module.Executor(runner,None,{"state_root":directory});job={"job_id":str(uuid.uuid4()),"feature_profile":profile}
                def extract(source,blob):source.mkdir();(source/"v3").mkdir()
                with patch.object(worker,"verification_binding",return_value={}),patch.object(worker,"cached_check_result",return_value=("fixture",None)),patch.object(worker,"record_check_result"),patch.object(worker,"dependencies",return_value="dependencies checked"),patch.object(worker,"phase",return_value="checked")as phase:
                    worker.check(job,b"snapshot","check",30,extract)
                compile_call=phase.call_args_list[0].args
                self.assertEqual(compile_call[5],module.check_argv("check",profile))
                self.assertEqual(compile_call[6],"engine:rust1.90:check"+(":live-context-v1"if profile=="live"else""))
                self.assertEqual(compile_call[1:3],("compile_test","engine"))

class RealFeatureProfileTests(unittest.TestCase):
    @unittest.skipUnless(__import__("os").environ.get("WEBAGENT_TEST_REAL_RUST_PROFILE")=="1","Explicit pinned-toolchain fixture")
    def test_default_compiles_but_live_rejects_optional_implementation_reference(self):
        import subprocess,os
        with tempfile.TemporaryDirectory()as directory:
            root=Path(directory);(root/"src").mkdir();(root/"implementation/src").mkdir(parents=True)
            (root/"Cargo.toml").write_text('[package]\nname="profile-fixture"\nversion="0.1.0"\nedition="2021"\n[workspace]\nmembers=["implementation"]\n[features]\ndefault=["dep:optional-implementation"]\nlive-context=[]\n[dependencies]\noptional-implementation={path="implementation",optional=true}\n')
            (root/"src/main.rs").write_text('fn main(){let _ = optional_implementation::value();}\n')
            (root/"implementation/Cargo.toml").write_text('[package]\nname="optional-implementation"\nversion="0.1.0"\nedition="2021"\n')
            (root/"implementation/src/lib.rs").write_text('pub fn value()->u8{1}\n')
            command=["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges","--memory","512m","--memory-swap","512m","--cpus","1","--pids-limit","128","--user",str(os.getuid())+":"+str(os.getgid()),"--mount","type=bind,src="+directory+",dst=/workspace","--tmpfs","/tmp:rw,size=64m","-e","CARGO_HOME=/workspace/cargo","-e","CARGO_TARGET_DIR=/workspace/target","-w","/workspace",module.RUST]
            def run(argv):return subprocess.run(command+argv,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,timeout=60)
            lock=run(["cargo","generate-lockfile","--offline"]);self.assertEqual(lock.returncode,0,lock.stdout)
            default=run(module.check_argv("check"));self.assertEqual(default.returncode,0,default.stdout)
            live=run(module.check_argv("check","live"));self.assertNotEqual(live.returncode,0)
            self.assertIn("optional_implementation",live.stdout);self.assertIn("E0433",live.stdout)


class NativeBuilderPhaseTests(unittest.TestCase):
    """The real builder must provision verified bytes and test before publishing a binary."""
    def exercise(self, *, fail_tests=False, alter_registry=False, fail_proof=False):
        import json
        from unittest.mock import patch
        temp=tempfile.TemporaryDirectory();self.addCleanup(temp.cleanup)
        root=Path(temp.name);assets=root/'assets';state=root/'state';job_id=str(uuid.uuid4())
        package=assets/'releases'/job_id/'v3/plugins/abilities/Custom/fixture';(package/'src').mkdir(parents=True)
        inputs={'Cargo.toml':'[package]\nname="fixture_runner"\nversion="0.1.0"\nedition="2021"\n','Cargo.lock':'version = 4\n','src/main.rs':'fn main() {}'}
        for name,value in inputs.items():(package/name).write_text(value)
        (package/'plugin.json').write_text(json.dumps({'id':'fixture','dispatch':'isolated','runtime':'executable','entrypoint':'fixture_runner','build':{'toolchain':'rust-1.90','inputs':list(inputs),'dependencies':[],'flags':[],'assets':[]}}))
        phases=[];runner=fixtures.host.Runner();runner.resources=types.SimpleNamespace(enabled=True);runner.job_context=types.SimpleNamespace(job={'job_id':job_id})
        def provision(cache):
            registry=cache/'cargo/registry';registry.mkdir(parents=True);(registry/'crate').write_bytes(b'verified crate')
            phases.append('provision');return {'hashes':module.Executor.artifact_hashes(registry)}
        def phase(job, phase, kind, source, cache, argv, profile, timeout):
            self.assertEqual(job['job_id'],job_id);phases.append(phase)
            if phase=='engine_metadata':
                self.assertEqual(kind,'dependency');self.assertEqual(argv[:3],['readelf','-p','.comment'])
                return 'GNU ld fixture'if fail_proof else '  [ 0] mold 1.10.1 (compatible with GNU ld)'
            self.assertEqual(kind,'plugin');self.assertIn('--locked',argv);self.assertIn('--offline',argv)
            if phase=='plugin_test' and fail_tests:raise ValueError('native test failed')
            if phase=='plugin_compile':
                binary=cache/'target/release/fixture_runner';binary.parent.mkdir(parents=True);binary.write_bytes(b'tested executable')
                if alter_registry:(cache/'cargo/registry/crate').write_bytes(b'altered crate')
        runner.executor=types.SimpleNamespace(compiler_recipe=lambda:"fixture-mold-image",dependencies=lambda job,source,cache,**kw:provision(cache),phase=phase,artifact_hashes=module.Executor.artifact_hashes)
        with patch.object(fixtures.host,'ASSET_ROOT',assets),patch.object(fixtures.host,'STATE',state):
            if fail_tests or alter_registry or fail_proof:
                with self.assertRaisesRegex(ValueError,'native test failed' if fail_tests else 'lacks Mold'if fail_proof else 'changed verified dependency'):
                    runner.build_ability_artifacts(job_id)
                self.assertEqual(list((assets/'artifacts').glob('*/fixture_runner')) if (assets/'artifacts').exists() else [],[])
            else:
                result=runner.build_ability_artifacts(job_id);self.assertEqual(len(result),1);self.assertFalse(result[0]['reused'])
        return phases
    def test_verified_registry_and_complete_tests_precede_native_compile(self):
        self.assertEqual(self.exercise(),['provision','plugin_test','plugin_compile','engine_metadata'])
    def test_native_test_failure_never_publishes_an_artifact(self):
        self.assertEqual(self.exercise(fail_tests=True),['provision','plugin_test'])
    def test_compiler_registry_mutation_never_publishes_an_artifact(self):
        self.assertEqual(self.exercise(alter_registry=True),['provision','plugin_test','plugin_compile','engine_metadata'])
    def test_new_native_gnu_linker_comment_never_publishes(self):
        self.assertEqual(self.exercise(fail_proof=True),['provision','plugin_test','plugin_compile','engine_metadata'])


class FrozenSourcePermissionsTests(unittest.TestCase):
    """Permission normalization applies to approved copies, never private inspection artifacts."""
    def test_private_authoring_modes_become_read_only_container_readable_copies(self):
        import stat
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/'v3/ui/file.js';source.parent.mkdir(parents=True);source.write_bytes(b'approved source');source.chmod(0o600)
            gate=root/'v3/data/config/gates/fixture.json';gate.parent.mkdir(parents=True);gate.write_text('{}');gate.chmod(0o660)
            executable=root/'v3/ui/run';executable.write_bytes(b'fixture executable');executable.chmod(0o755)
            before=fixtures.host.release.asset_source_digest(root)
            fixtures.host.Runner().readable_assets(root)
            self.assertEqual(stat.S_IMODE(source.stat().st_mode),0o444);self.assertEqual(stat.S_IMODE(gate.stat().st_mode),0o444);self.assertEqual(stat.S_IMODE(executable.stat().st_mode),0o555)
            self.assertEqual(source.read_bytes(),b'approved source');self.assertEqual(fixtures.host.release.asset_source_digest(root),before)
    def test_inspection_data_is_rejected_before_any_permissions_change(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/'source';source.write_text('source');source.chmod(0o600)
            private=root/'v3/data/user_data/admin/private.db';private.parent.mkdir(parents=True);private.write_bytes(b'inspection fixture');private.chmod(0o600)
            with self.assertRaisesRegex(PermissionError,'Private inspection'):
                fixtures.host.Runner().readable_assets(root)
            self.assertEqual(source.stat().st_mode&0o777,0o600);self.assertEqual(private.stat().st_mode&0o777,0o600)
    def test_symlink_is_rejected_before_any_permissions_change(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);source=root/'source';source.write_text('source');source.chmod(0o600);(root/'escape').symlink_to('/etc')
            with self.assertRaisesRegex(PermissionError,'symlink'):
                fixtures.host.Runner().readable_assets(root)
            self.assertEqual(source.stat().st_mode&0o777,0o600)


class PublishedRuntimeMountTests(unittest.TestCase):
    def test_runtime_mounts_use_verified_readable_publication_not_private_task(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary);private=base/'private/release';private.mkdir(parents=True,mode=0o700)
            assets=base/'assets';job_id=str(uuid.uuid4());published=assets/'releases'/job_id
            runner=fixtures.host.Runner();runner.job_context=types.SimpleNamespace(job={'job_id':job_id,'release_root':str(private),'inputs_sha256':'a'*64})
            with patch.object(fixtures.host,'ASSET_ROOT',assets),patch.object(fixtures.host.release,'verify_publication') as verify:
                self.assertEqual(runner.published_runtime_source(private),published)
                verify.assert_called_once_with(published,job_id,'a'*64)
            self.assertEqual(private.stat().st_mode&0o777,0o700)
    def test_runtime_mount_rejects_wrong_source_and_unverified_publication(self):
        from unittest.mock import patch
        runner=fixtures.host.Runner();runner.job_context=types.SimpleNamespace(job={'job_id':str(uuid.uuid4()),'release_root':'/owned/release','inputs_sha256':'a'*64})
        with patch.object(fixtures.host.release,'verify_publication',side_effect=ValueError('receipt mismatch')) as verify:
            with self.assertRaisesRegex(ValueError,'Owned frozen'):runner.published_runtime_source('/other')
            verify.assert_not_called()
            with self.assertRaisesRegex(ValueError,'receipt mismatch'):runner.published_runtime_source('/owned/release')


class SharedDependencyTests(unittest.TestCase):
    """Downloads may cross source generations; compiled/test evidence must not follow them."""
    def setUp(self):
        import contextlib
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
        self.source=self.root/"source";self.source.mkdir();(self.source/"Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
        self.payload=b"locked crate archive";sha=hashlib.sha256(self.payload).hexdigest()
        (self.source/"Cargo.lock").write_text('version=4\n[[package]]\nname="example"\nversion="1.0.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\nchecksum="'+sha+'"\n')
        self.runner=types.SimpleNamespace(plugin_build_lock=lambda key:contextlib.nullcontext(),resource_progress=lambda job:None)
        self.worker=module.Executor(self.runner,None,{"state_root":str(self.root/"state")});self.calls=[]
        def phase(job,phase,kind,source,output,argv,profile,timeout=600,network=False):
            self.calls.append((argv,network));output.mkdir(parents=True,exist_ok=True)
            if argv[:2]==["cargo","fetch"]:
                p=output/"cargo/registry/cache/crates-io/example-1.0.0.crate";p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(self.payload)
            return "phase complete\n"
        self.worker.phase=phase
    def output(self):
        p=self.root/uuid.uuid4().hex;p.mkdir();return p
    def test_prepare_then_other_job_reuses_downloads_offline_after_source_edit(self):
        a={"job_id":str(uuid.uuid4())};self.worker.dependencies(a,self.source,self.output(),prepare=True)
        self.assertEqual(a["dependency_cache"]["decision"],"prepared")
        (self.source/"src").mkdir();(self.source/"src/main.rs").write_text("fn main(){changed()}")
        self.calls.clear();b={"job_id":str(uuid.uuid4())};self.worker.dependencies(b,self.source,self.output())
        self.assertEqual(a["dependency_cache"]["key"],b["dependency_cache"]["key"])
        self.assertEqual(b["dependency_cache"]["decision"],"hit");self.assertFalse(any(network for _,network in self.calls))
        self.assertNotIn("compiler_artifact_reuse",b)
    def test_ordinary_miss_fails_before_network_or_compilation(self):
        job={"job_id":str(uuid.uuid4())}
        with self.assertRaisesRegex(ValueError,"check=dependencies"):self.worker.dependencies(job,self.source,self.output())
        self.assertEqual(self.calls,[]);self.assertEqual(job["dependency_cache"]["reason"],"no_verified_dependency_entry")
    def test_lock_manifest_and_toolchain_change_invalidate_download_key(self):
        from unittest.mock import patch
        first=self.worker.dependency_binding(self.source)
        (self.source/"Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.2.0"\n')
        self.assertNotEqual(first,self.worker.dependency_binding(self.source))
        second=self.worker.dependency_binding(self.source)
        with patch.object(module,"RUST",module.RUST+"-changed"):self.assertNotEqual(second,self.worker.dependency_binding(self.source))
        (self.source/"Cargo.lock").write_text((self.source/"Cargo.lock").read_text()+"\n# changed lock\n")
        self.assertNotEqual(second,self.worker.dependency_binding(self.source))
    def test_cache_corruption_fails_closed_without_fetch(self):
        job={"job_id":str(uuid.uuid4())};self.worker.dependencies(job,self.source,self.output(),prepare=True)
        cache=self.root/"state/cargo-dependencies"/job["dependency_cache"]["key"]
        next((cache/"cargo").rglob("*.crate")).write_bytes(b"tampered")
        self.calls.clear()
        with self.assertRaisesRegex(ValueError,"cache changed"):self.worker.dependencies({},self.source,self.output())
        self.assertEqual(self.calls,[])
    def test_bad_or_missing_locked_archives_cannot_publish(self):
        self.payload=b"wrong archive"
        with self.assertRaisesRegex(ValueError,"checksum mismatch"):self.worker.dependencies({},self.source,self.output(),prepare=True)
        self.assertFalse(any((self.root/"state/cargo-dependencies").glob("*/receipt.json")))
    def test_source_symlink_and_unsupported_registry_are_rejected(self):
        (self.source/"Cargo.lock").unlink();(self.source/"Cargo.lock").symlink_to(self.source/"Cargo.toml")
        with self.assertRaisesRegex(ValueError,"symlink"):self.worker.dependency_binding(self.source)
    def test_preparation_is_atomic_after_interruption(self):
        from unittest.mock import patch
        original=self.worker.phase
        def fail(*args,**kwargs):
            result=original(*args,**kwargs)
            if args[4][0:2]!=["cargo","fetch"]:raise RuntimeError("offline validation interrupted")
            return result
        with patch.object(self.worker,"phase",side_effect=RuntimeError("fetch interrupted")):
            with self.assertRaises(RuntimeError):self.worker.dependencies({},self.source,self.output(),prepare=True)
        self.assertFalse(any((self.root/"state/cargo-dependencies").glob("*/receipt.json")))
        self.worker.dependencies({},self.source,self.output(),prepare=True)
    def test_old_private_registry_is_preserved_instead_of_certified(self):
        output=self.output();p=output/"cargo/registry/old";p.parent.mkdir(parents=True);p.write_text("unverified old scratch")
        self.worker.dependencies({},self.source,output,prepare=True)
        self.assertEqual(next(output.glob("previous-cargo-*/registry/old")).read_text(),"unverified old scratch")

    def test_parallel_compatible_preparations_fetch_once(self):
        import threading,concurrent.futures
        mutex=threading.Lock();self.runner.plugin_build_lock=lambda key:mutex
        jobs=[{"job_id":str(uuid.uuid4())}for _ in range(2)];outputs=[self.output(),self.output()]
        with concurrent.futures.ThreadPoolExecutor(max_workers=2)as pool:
            results=[pool.submit(self.worker.dependencies,job,self.source,out,prepare=True)for job,out in zip(jobs,outputs)]
            for result in results:result.result(timeout=5)
        self.assertEqual(sum(bool(network)for _,network in self.calls),1)
        self.assertEqual({j["dependency_cache"]["decision"]for j in jobs},{"prepared","hit"})

    def test_cargo_home_configuration_is_not_inherited_or_shared(self):
        output=self.output();cargo=output/"cargo";cargo.mkdir();(cargo/"config.toml").write_text("untrusted prior compiler configuration")
        self.worker.dependencies({},self.source,output,prepare=True)
        self.assertFalse((cargo/"config.toml").exists())
        self.assertEqual(next(output.glob("previous-cargo-*/config.toml")).read_text(),"untrusted prior compiler configuration")

class DependencySchedulerTests(unittest.TestCase):
    from test_scheduler_broker import SchedulerBrokerTests as base
    setUp=base.setUp
    tearDown=base.tearDown
    call=base.call
    begin=base.begin
    write=base.write
    def test_dependency_check_uses_existing_authority_without_expanding_source_claims(self):
        from unittest.mock import patch
        item=self.begin("dependency-authority-fixture")
        self.write(item,"build-service/example.py","# owned broker source\n")
        before=self.broker.scheduler.store.get(item["key"])
        self.broker.resources.enabled=True
        tree=Path(item["workspace_path"]).parent;digest=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))
        with patch.object(self.broker,"_start_next"):
            result=self.call("build_work",{"action":"check","task_id":item["task_id"],"check":"dependencies", "request_id":"dependency-authority-check","expected_inputs_sha256":digest})
        job=self.broker.jobs[result["job_id"]]
        self.assertEqual(job["check_task"],"dependencies")
        self.assertEqual(self.broker.scheduler.store.get(item["key"])["scopes"],before["scopes"])
        self.assertEqual(self.broker.scheduler.job_scopes(job),set())
        self.assertTrue(self.broker.scheduler.execution_eligible(job,[{**job,"operation":"build_deploy","check_task":"check"}]))
        self.broker.resources.enabled=False


class VerificationEvidenceTests(unittest.TestCase):
    def setUp(self):
        import json
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
        self.runner=types.SimpleNamespace(execution_entries=fixtures.host.Runner.execution_entries,resource_progress=lambda job:None,compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64})
        self.worker=module.Executor(self.runner,None,{"state_root":str(self.root)})
        self.blob=fixtures.context(extra={"v3/src/main.rs":"fn main() {}","v3/config/fixture.json":"{}"})
        self.job={"job_id":str(uuid.uuid4()),"compiler_memory_cap":5*1024**3}
    def certificate(self,task="check",profile="default",job=None):
        import json
        job=job or self.job;binding=self.worker.verification_binding(job,self.blob,task,profile)
        key,_=self.worker.cached_check_result(binding);self.worker.record_check_result(job,binding,key,"successful verification")
        (self.root/(job["job_id"]+".json")).write_text(json.dumps({**job,"status":"completed","operation":"check"}))
        return binding,key
    def test_only_completed_matching_receipts_reuse(self):
        import json
        binding,key=self.certificate();self.assertEqual(self.worker.cached_check_result(binding)[1]["job_id"],self.job["job_id"])
        for status in ("failed","cancelled","running","interrupted","stale"):
            (self.root/(self.job["job_id"]+".json")).write_text(json.dumps({**self.job,"status":status,"operation":"check"}))
            self.assertIsNone(self.worker.cached_check_result(binding)[1])
    def test_feature_filter_cap_and_fixture_changes_do_not_reuse(self):
        binding,key=self.certificate(task="test")
        for changed in ({**binding,"feature_profile":"live"},{**binding,"test_filter":"focused"},{**binding,"compiler_memory_cap":10*1024**3}):
            self.assertIsNone(self.worker.cached_check_result(changed)[1])
        altered=fixtures.context(extra={"v3/src/main.rs":"fn main() {}","v3/config/fixture.json":"{\"changed\":true}"})
        self.assertNotEqual(binding,self.worker.verification_binding(self.job,altered,"test","default"))
    def test_corrupt_certificate_fails_closed(self):
        import json
        binding,key=self.certificate();p=self.root/"verification-certificates"/(key+".json");value=json.loads(p.read_text());value["binding"]["task"]="different";p.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError,"binding changed"):self.worker.cached_check_result(binding)
    def test_focused_package_selection_and_root_filters_are_bounded(self):
        metadata={"packages":[{"source":None,"name":"component","manifest_path":"/build/crates/component/Cargo.toml"}]}
        plan=self.worker.focused_test_plan(["v3/crates/component/src/lib.rs","v3/src/api/auth.rs","v3/src/api/user.rs","v3/src/main.rs","v3/src/chat.rs"],metadata)
        self.assertEqual([p["test"]for p in plan if "test"in p],[r["test"]for r in module.HOST_FOCUSED_REGISTRY]);self.assertEqual(plan[-1],{"package":"component"})
        self.assertEqual(self.worker.focused_test_plan(["v3/config/settings.json"],metadata),[])
    def test_phase_failure_and_success_have_truthful_progress(self):
        from unittest.mock import patch
        args=(self.job,"test","test",self.root,self.root,["fixed"],"fixed-recipe")
        with patch.object(self.worker,"_phase_workload",return_value="ok"):
            self.assertEqual(self.worker.phase(*args),"ok")
        self.assertEqual(self.job["phase_timings"][-1]["outcome"],"completed");self.assertNotIn("active_verification",self.job)
        with patch.object(self.worker,"_phase_workload",side_effect=RuntimeError("test failed")):
            with self.assertRaises(RuntimeError):self.worker.phase(*args)
        self.assertEqual(self.job["phase_timings"][-1]["outcome"],"failed");self.assertNotIn("active_verification",self.job)
    def test_focus_failure_stops_before_required_suite_and_reports_failure(self):
        from unittest.mock import patch
        import json
        (self.root/"dependency-metadata.json").write_text("{}")
        self.job["changed_paths"]=["v3/crates/modules/models/host/provider/mod.rs"];self.runner.execution_source_digest=lambda root:"captured-fixture"
        with patch.object(self.worker,"metadata_receipt",return_value={"packages":[]}),patch.object(self.worker,"phase",side_effect=RuntimeError("behavior regression")):
            with self.assertRaisesRegex(ValueError,"behavior regression"):self.worker.focused_tests(self.job,self.root,self.root)
        proof=self.job["focused_verification"];self.assertEqual(proof["required_suite"],"workspace");self.assertEqual(proof["runs"][0]["status"],"failed")


class HostFocusedVerificationTests(unittest.TestCase):
    def setUp(self):
        import json
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
        self.source=self.root/"source";self.output=self.root/"output";self.output.mkdir()
        self.blob=fixtures.context(extra={"v3/src/main.rs":"fn main() {}","v3/config/prompts.json":"{}"})
        fixtures.host.release.extract(self.source,self.blob,fixtures.host.context_digest)
        self.runner=types.SimpleNamespace(**{name:getattr(fixtures.host.Runner,name)for name in ("execution_source_digest","execution_application_blob","execution_entries")},resource_progress=lambda job:None,compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64})
        self.worker=module.Executor(self.runner,None,{"state_root":str(self.root/"state")});self.calls=[]
        self.job={"job_id":str(uuid.uuid4()),"inputs_sha256":fixtures.host.context_digest(self.blob)[0],"changed_paths":["v3/src/engine_components.rs"]}
        (self.output/"dependency-metadata.json").write_text('{"packages":[]}')
        def phase(*args):self.calls.append(args);return "test result: ok. 1 passed; 0 failed; 0 ignored\n"
        self.worker.phase=phase
    def run_focus(self):return self.worker.focused_tests(self.job,self.source/"v3",self.output)
    def test_host_tests_precede_three_component_packages(self):
        metadata={"packages":[{"source":None,"name":"component"+str(n),"manifest_path":"/build/crates/component"+str(n)+"/Cargo.toml"}for n in range(4)]}
        changed=["v3/crates/component"+str(n)+"/src/lib.rs"for n in range(4)]
        plan=self.worker.focused_test_plan(changed+["v3/src/engine_components.rs"],metadata)
        self.assertEqual([p["test"]for p in plan if "test"in p],[r["test"]for r in module.HOST_FOCUSED_REGISTRY])
        self.assertEqual([p["package"]for p in plan if "test"not in p],["component0","component1","component2"])
    def test_moved_engine_sources_retain_host_regressions(self):
        for path in ["v3/src/engine/loop/run.rs", "v3/src/engine/context/tool_output.rs", "v3/src/engine/components.rs", "v3/crates/modules/engine/src/runtime/run.rs", "v3/crates/modules/engine/host/mod.rs"]:
            plan=self.worker.focused_test_plan([path],{})
            self.assertEqual([row["id"] for row in plan],[row["id"] for row in module.HOST_FOCUSED_REGISTRY])
    def test_each_registry_selects_only_relevant_host_checks(self):
        plan=self.worker.focused_test_plan(["v3/crates/modules/models/host/provider/mod.rs"],{})
        self.assertEqual([p["id"]for p in plan],["host-prompt-restoration"])
        plan=self.worker.focused_test_plan(["v3/crates/modules/models/src/provider.rs"],{})
        self.assertEqual([p["id"]for p in plan],["host-prompt-restoration"])
        plan=self.worker.focused_test_plan(["v3/plugins/engine/context-assets/component.json"],{})
        self.assertEqual([p["id"]for p in plan],["host-offline-descriptor-identity"])
        for path in ["v3/ui/chat/a.js","v3/worker.py","v3/docs/context.md","v3/src/unrelated.rs","v3/crates/unrelated/src/lib.rs"]:
            self.assertEqual(self.worker.focused_test_plan([path],{}),[])
    def test_exact_names_profile_input_recipe_counts_and_timing(self):
        self.run_focus();proof=self.job["focused_verification"]
        self.assertEqual(proof["status"],"passed");self.assertEqual(proof["inputs_sha256"],self.job["inputs_sha256"])
        self.assertEqual(proof["policy_sha256"],module.HOST_FOCUSED_POLICY_SHA256)
        self.assertEqual(len(proof["runs"]),2)
        for run,call in zip(proof["runs"],self.calls):
            self.assertEqual(run["matched"],1);self.assertTrue(run["inputs_unchanged"]);self.assertEqual(run["feature_profile"],"default")
            self.assertIn("--exact",call[5]);self.assertIn("--lib"if run.get("target_kind")=="lib"else"--bin",call[5]);self.assertGreater(call[7],0);self.assertLessEqual(call[7],120)
            self.assertGreaterEqual(run["seconds"],0);self.assertEqual(run["status"],"passed")
        self.assertFalse((self.root/"state/verification-certificates").exists())
    def test_zero_and_ignored_only_results_fail_before_next_check(self):
        for log in ["test result: ok. 0 passed; 0 failed; 0 ignored", "test result: ok. 0 passed; 0 failed; 1 ignored", "compiled successfully"]:
            self.calls=[]
            def zero(*args):self.calls.append(args);return log
            self.worker.phase=zero
            with self.assertRaisesRegex(ValueError,"Zero matched assertions"):self.run_focus()
            self.assertEqual(len(self.calls),1);self.assertEqual(self.job["focused_verification"]["status"],"failed")
    def test_content_and_mode_mutation_reject_successful_assertions(self):
        path=self.source/"v3/config/prompts.json";original=path.read_bytes();mode=path.stat().st_mode&0o777
        for mutation in [lambda:path.write_bytes(b"changed"),lambda:path.chmod(mode^0o100)]:
            def changed(*args):mutation();return "test result: ok. 1 passed; 0 failed"
            self.worker.phase=changed
            with self.assertRaisesRegex(ValueError,"including file modes"):self.run_focus()
            run=self.job["focused_verification"]["runs"][0];self.assertEqual(run["status"],"input_mutated");self.assertFalse(run["inputs_unchanged"])
            path.write_bytes(original);path.chmod(mode)
    def test_cancellation_is_not_a_reusable_pass(self):
        def cancel(*args):raise PermissionError("fixture cancellation")
        self.worker.phase=cancel
        with self.assertRaises(PermissionError):self.run_focus()
        self.assertEqual(self.job["focused_verification"]["status"],"cancelled")
        self.assertEqual(self.job["focused_verification"]["runs"][0]["status"],"cancelled")
    def test_early_failure_stops_full_and_optimized_pipeline(self):
        self.pipeline(False)
    def test_success_continues_into_full_suite_then_optimized_pipeline(self):
        self.pipeline(True)
    def pipeline(self,success):
        from unittest.mock import patch
        job={**self.job,"job_id":str(uuid.uuid4())};phases=[]
        def dependency(job,application,output,**kwargs):
            (output/"dependency-metadata.json").write_text('{"packages":[]}');return "offline ready\n"
        def phase(job,phase,kind,source,output,argv,recipe,*args):
            phases.append((phase,recipe))
            if "focused-v2"in recipe and not success:raise RuntimeError("host regression")
            if phase=="compile_linked":raise RuntimeError("optimized pipeline reached")
            return "test result: ok. 1 passed; 0 failed\n"
        manifest={"tests":[{"file":"tests/webagent_v3-0123456789abcdef","target_name":"webagent-v3","source_path":"/build/src/main.rs","kind":["bin"],"package_id":"path+file:///build#webagent-v3@0.1.0"},{"file":"tests/webagent_engine-fedcba9876543210","target_name":"webagent_engine","source_path":"/build/crates/modules/engine/src/lib.rs","kind":["lib"],"package_id":"path+file:///build/crates/modules/engine#webagent-engine@0.1.0"}]}
        with patch.object(self.worker,"dependency_snapshot",return_value=""),patch.object(self.worker,"retain_test_manifest",return_value=manifest)as retain,patch.object(self.worker,"test_manifest",return_value=manifest),patch.object(self.worker,"validate_compiler_recipe",return_value=False),patch.object(self.worker,"cached_engine",return_value=None),patch.object(self.worker,"seed_cargo_cache",return_value=False),patch.object(self.worker,"dependencies",side_effect=dependency),patch.object(self.worker,"phase",side_effect=phase):
            with self.assertRaisesRegex((ValueError,RuntimeError),"optimized pipeline reached"if success else"host regression"):
                self.worker.engine(job,self.blob,lambda root,blob:fixtures.host.release.extract(root,blob,fixtures.host.context_digest),fixtures.host.context_digest)
        retain.assert_called_once()
        required=[p for p in phases if p[1]=="engine:rust1.90:test"]
        self.assertEqual(bool(required),success);self.assertEqual(any(p[0]=="compile_linked"for p in phases),success)
        self.assertFalse((self.root/"state/execution"/job["job_id"]/"verified.json").exists())

    def test_missing_required_source_is_reported_as_failed_input_guard(self):
        def deleted(*args):(self.source/"v3/Cargo.lock").unlink();return "test result: ok. 1 passed; 0 failed"
        self.worker.phase=deleted
        with self.assertRaisesRegex(ValueError,"provider::tests::.*including file modes"):self.run_focus()
        self.assertEqual(self.job["focused_verification"]["runs"][0]["status"],"input_mutated")

    def test_retained_host_binary_uses_exact_test_with_no_compiler(self):
        from unittest.mock import patch
        manifest={"tests":[{"file":"tests/webagent_v3-0123456789abcdef","target_name":"webagent-v3","source_path":"/build/src/main.rs","kind":["bin"],"package_id":"path+file:///build#webagent-v3@0.1.0"},{"file":"tests/webagent_engine-fedcba9876543210","target_name":"webagent_engine","source_path":"/build/crates/modules/engine/src/lib.rs","kind":["lib"],"package_id":"path+file:///build/crates/modules/engine#webagent-engine@0.1.0"}]}
        with patch.object(self.worker,"test_manifest",return_value=manifest):self.worker.focused_tests(self.job,self.source/"v3",self.output,retained=self.output/"artifacts")
        self.assertEqual(len(self.calls),2)
        for call,expected in zip(self.calls,["webagent_v3-0123456789abcdef","webagent_engine-fedcba9876543210"]):self.assertEqual(call[5][0],"/output/artifacts/tests/"+expected);self.assertIn("--exact",call[5]);self.assertNotIn("cargo",call[5])
        self.assertEqual(self.job["focused_verification"]["reuse"],"verified_retained_test_binaries")

    def test_registered_engine_library_rejects_missing_wrong_or_ambiguous_retained_target(self):
        from unittest.mock import patch
        root={"file":"tests/webagent_v3-0123456789abcdef","target_name":"webagent-v3","source_path":"/build/src/main.rs","kind":["bin"],"package_id":"path+file:///build#webagent-v3@0.1.0"}
        library={"file":"tests/webagent_engine-fedcba9876543210","target_name":"webagent_engine","source_path":"/build/crates/modules/engine/src/lib.rs","kind":["lib"],"package_id":"path+file:///build/crates/modules/engine#webagent-engine@0.1.0"}
        for rows in [[],[{**library,"kind":["bin"]}],[{**library,"package_id":"path+file:///build/crates/foreign#webagent-engine@0.1.0"}],[library,dict(library)]]:
            self.calls=[]
            with patch.object(self.worker,"test_manifest",return_value={"tests":[root,*rows]}):
                with self.assertRaisesRegex(ValueError,"Registered retained host test binary unavailable"):self.worker.focused_tests(self.job,self.source/"v3",self.output,retained=self.output/"artifacts")
            self.assertEqual(len(self.calls),1)
            self.assertEqual(self.job["focused_verification"]["status"],"failed")

    def test_registered_engine_library_rejects_zero_matches(self):
        def phase(*args):
            self.calls.append(args)
            return "test result: ok. 0 passed; 0 failed" if "--lib" in args[5] else "test result: ok. 1 passed; 0 failed"
        self.worker.phase=phase
        with self.assertRaisesRegex(ValueError,"Zero matched assertions"):self.run_focus()
        self.assertEqual(len(self.calls),2)
        self.assertEqual(self.job["focused_verification"]["runs"][-1]["package"],"webagent-engine")

    def test_invalid_retained_manifest_is_not_left_running(self):
        from unittest.mock import patch
        with patch.object(self.worker,"test_manifest",side_effect=ValueError("Invalid retained manifest")):
            with self.assertRaisesRegex(ValueError,"Invalid retained manifest"):self.worker.focused_tests(self.job,self.source/"v3",self.output,retained=self.output/"artifacts")
        self.assertEqual(self.job["focused_verification"]["status"],"failed");self.assertFalse(self.calls)

class ExecutionObservationProbeTests(unittest.TestCase):
    def test_probe_is_fixed_source_free_and_has_no_application_mount(self):
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as directory:
            worker=module.Executor(None,None,{"state_root":directory})
            worker.phase=Mock(return_value="fixed proof")
            job={"job_id":str(uuid.uuid4()),"check_task":"execution_probe"}
            self.assertEqual(worker.probe(job),"fixed proof")
            args=worker.phase.call_args.args
            self.assertEqual(args[1:3],("test","test"))
            self.assertEqual(list(args[3].iterdir()),[])
            self.assertEqual(args[6],"resource:execution-probe-v1")
            self.assertIn("rustc --test /output/observation.rs",args[5][2])
            self.assertIn("--test-threads=1",args[5][2])
            self.assertNotIn("/build/",args[5][2])
            self.assertLessEqual(args[7],90)

class DependencySnapshotTests(unittest.TestCase):
    """Compiled third-party objects come only from stubbed workspaces and reach jobs as verified copies."""
    def setUp(self):
        import contextlib,json
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        self.source=self.root/"source";(self.source/"src").mkdir(parents=True);(self.source/"crates/chat/src").mkdir(parents=True)
        (self.source/"Cargo.toml").write_text('[package]\nname="webagent-v3"\n[workspace]\nmembers=["crates/chat"]\n')
        (self.source/"Cargo.lock").write_text("version = 4\n");(self.source/"crates/chat/Cargo.toml").write_text('[package]\nname="webagent-chat"\n')
        (self.source/"src/main.rs").write_text("fn main(){task_code()}");(self.source/"crates/chat/src/lib.rs").write_text("pub fn task_code(){}")
        self.runner=types.SimpleNamespace(compiler_runtime=lambda *args:{"image":"sha256:"+"7"*64},plugin_build_lock=lambda name:contextlib.nullcontext())
        self.worker=module.Executor(self.runner,None,{"state_root":str(self.root/"state"),"scheduler":{"disk_min_free_bytes":0}})
        headroom=unittest.mock.patch.object(module,"SNAPSHOT_HEADROOM",0);headroom.start();self.addCleanup(headroom.stop)
        binding=self.worker.dependency_binding(self.source)
        registry=self.root/"state/cargo-dependencies"/hashlib.sha256(json.dumps(binding,sort_keys=True).encode()).hexdigest()/"cargo/registry"
        registry.mkdir(parents=True);(registry/"serde.crate").write_bytes(b"locked")
        self.members=[("webagent-v3",[("webagent-v3",["bin"],"/build/src/main.rs"),("build-script-build",["custom-build"],"/build/build.rs")]),
            ("webagent-chat",[("webagent_chat",["lib"],"/build/crates/chat/src/lib.rs"),("flow",["test"],"/build/crates/chat/tests/flow.rs")])]
        self.calls=[];self.leftover=False
    def tearDown(self):
        self.temp.cleanup()
    def metadata(self,output,members=None):
        import json
        packages=[{"id":name+" 0.1.0","name":name,"targets":[{"name":label,"kind":kind,"src_path":path}for label,kind,path in targets]}for name,targets in members or self.members]
        packages.append({"id":"serde 1.0","name":"serde","targets":[{"name":"serde","kind":["lib"],"src_path":"/output/cargo/registry/src/serde/lib.rs"}]})
        output.mkdir(parents=True,exist_ok=True)
        (output/"dependency-metadata.json").write_text(json.dumps({"workspace_members":[p["id"]for p in packages if p["name"]!="serde"],"packages":packages}))
        return output
    def phase(self,job,phase,kind,source,output,argv,profile,timeout=3600,network=False):
        self.calls.append((phase,kind,source,argv[-1]))
        self.assertEqual((source/"src/main.rs").read_text(),"#![allow(warnings)]\nfn main(){}\n")
        self.assertEqual((source/"crates/chat/src/lib.rs").read_text(),"#![allow(warnings)]\n")
        self.assertTrue((output/"cargo/registry/serde.crate").is_file())
        deps=output/"target/release/deps";deps.mkdir(parents=True);(deps/"libserde-0123.rlib").write_bytes(b"third party")
        fingerprint=output/"target/debug/.fingerprint/serde-4567";fingerprint.mkdir(parents=True);(fingerprint/"lib-serde").write_text("x")
        if self.leftover:(deps/"libwebagent_chat-89ab.rlib").write_bytes(b"stub")
        return "built\n"
    def run_job(self):
        job={"job_id":str(uuid.uuid4())};output=self.metadata(self.root/uuid.uuid4().hex)
        self.worker.phase=self.phase
        log=self.worker.dependency_snapshot(job,self.source,output)
        return job,output,log
    def test_stub_replaces_every_workspace_target_and_copies_exact_manifests(self):
        stub=self.root/"stub";stub.mkdir()
        names,packages=self.worker.snapshot_stub(self.source,self.metadata(self.root/"meta")/"dependency-metadata.json",stub)
        self.assertEqual(packages,["webagent-chat","webagent-v3"])
        self.assertIn("flow",names);self.assertIn("webagent_chat",names)
        self.assertEqual((stub/"Cargo.toml").read_bytes(),(self.source/"Cargo.toml").read_bytes())
        self.assertEqual((stub/"build.rs").read_text(),"#![allow(warnings)]\nfn main(){}\n")
        self.assertNotIn("task_code",(stub/"crates/chat/src/lib.rs").read_text())
    def test_stub_rejects_targets_outside_the_workspace(self):
        for path in ("/build/../escape.rs","/etc/escape.rs","/build/Cargo.toml","/build/src/../../x.rs"):
            stub=self.root/uuid.uuid4().hex;stub.mkdir()
            with self.assertRaises(ValueError):
                self.worker.snapshot_stub(self.source,self.metadata(self.root/uuid.uuid4().hex,[("webagent-v3",[("webagent-v3",["bin"],path)])])/"dependency-metadata.json",stub)
    def test_leftovers_name_stub_products_and_ignore_third_party(self):
        target=self.root/"target"
        for name in ("debug/.fingerprint/webagent-v3-0011","release/deps/libwebagent_chat-22.rlib","debug/webagent-v3","release/deps/serde-33.d","debug/build/libsqlite3-sys-44"):
            path=target/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text("x")
        self.assertEqual(self.worker.snapshot_leftovers(target,["webagent-v3","webagent_chat"]),
            ["debug/.fingerprint/webagent-v3-0011","debug/webagent-v3","release/deps/libwebagent_chat-22.rlib"])
    def test_prepared_snapshot_is_reused_as_a_verified_private_copy(self):
        job,output,log=self.run_job()
        self.assertEqual(job["dependency_snapshot"]["decision"],"prepared");self.assertEqual(len(self.calls),1)
        phase,kind,_,script=self.calls[0]
        self.assertEqual((phase,kind),("dependency","engine"))
        self.assertIn("cargo clean --release --locked --offline -p webagent-chat -p webagent-v3",script)
        self.assertEqual((output/"target/release/deps/libserde-0123.rlib").read_bytes(),b"third party")
        second,copy,_=self.run_job()
        self.assertEqual(second["dependency_snapshot"]["decision"],"hit");self.assertEqual(len(self.calls),1)
        self.assertEqual(self.worker.artifact_hashes(copy/"target"),self.worker.artifact_hashes(output/"target"))
        (copy/"target/release/deps/libserde-0123.rlib").write_bytes(b"poisoned private copy")
        third,fresh,_=self.run_job()
        self.assertEqual(third["dependency_snapshot"]["decision"],"hit")
        self.assertEqual((fresh/"target/release/deps/libserde-0123.rlib").read_bytes(),b"third party")
    def test_changed_shared_snapshot_or_surviving_stub_leaves_cold_target(self):
        self.leftover=True
        job,output,log=self.run_job()
        self.assertEqual(job["dependency_snapshot"]["decision"],"unavailable");self.assertFalse((output/"target").exists())
        self.assertEqual([p for p in (self.root/"state/compiled-dependencies").iterdir()],[])
        self.leftover=False;self.run_job()
        shared=next((self.root/"state/compiled-dependencies").iterdir())
        (shared/"target/release/deps/libserde-0123.rlib").write_bytes(b"tampered")
        job,output,log=self.run_job()
        self.assertEqual(job["dependency_snapshot"]["decision"],"unavailable");self.assertFalse((output/"target").exists())
    def test_changed_lockfile_selects_a_new_snapshot_and_cancellation_propagates(self):
        self.run_job()
        (self.source/"Cargo.lock").write_text("version = 4\n# changed\n")
        job,output,_=self.run_job()
        self.assertEqual(job["dependency_snapshot"]["decision"],"unavailable")
        self.assertIn("Verified downloads unavailable",job["dependency_snapshot"]["detail"])
        def cancelled(*args,**kwargs):raise PermissionError("cancelled")
        self.worker.dependency_binding=cancelled
        with self.assertRaises(PermissionError):self.worker.dependency_snapshot({"job_id":"x"},self.source,self.metadata(self.root/"c"))
