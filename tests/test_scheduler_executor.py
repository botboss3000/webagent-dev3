"""Independent cache assertions: actual compiled inputs and complete artifact bytes govern reuse."""
import hashlib,importlib.util,tempfile,types,unittest,uuid
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
        self.worker=module.Executor(types.SimpleNamespace(execution_entries=fixtures.host.Runner.execution_entries),None,{"state_root":str(self.root)})
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
        state.write_text(__import__("json").dumps({"job_id":previous,"status":"failed"}))
        folder=self.root/previous;folder.mkdir();(folder/"candidate.tar").write_bytes(self.blob)
        execution=self.root/"execution"/previous
        fixtures.host.release.extract(execution/"source",self.blob,fixtures.host.context_digest)
        target=execution/"output/target";deps=target/"debug/deps";deps.mkdir(parents=True)
        (deps/"fixture.d").write_text("output: src/main.rs config/embedded.json\n# env-dep:CARGO_MANIFEST_DIR=/build\n")
        (deps/"dependency.rlib").write_bytes(b"compatible dependency objects")
        cargo=execution/"output/cargo";cargo.mkdir();(cargo/"registry-cache").write_bytes(b"locked registry bytes")
        self.worker.runner=types.SimpleNamespace(**{name:getattr(fixtures.host.Runner,name)
            for name in ("execution_entries","execution_application_blob","execution_source_digest","execution_source_digest_blob")})
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
        with self.assertRaisesRegex(ValueError,"source changed"):
            self.worker.seed_cargo_cache(self.job,self.blob,self.output())
    def test_runtime_roles_package_only_declared_artifacts_and_stream_to_disk(self):
        """Loop contexts exclude API registry and retained tests; API keeps its required closure."""
        import tarfile
        dockerfile=(Path(__file__).parents[1]/"Dockerfile").read_text()
        blob=fixtures.context(extra={**self.extra,"v3/Dockerfile":dockerfile})
        for name in ("core-inputs.txt",*module.BINARIES):
            (self.artifacts/name).write_bytes(b"verified output")
        folder=self.artifacts/"modules";folder.mkdir();(folder/"descriptor.json").write_bytes(b"verified module")
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
                with patch.object(worker,"phase",return_value="checked")as phase:
                    worker.check(job,b"snapshot","check",30,extract)
                compile_call=phase.call_args_list[1].args
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
    def exercise(self, *, fail_tests=False, alter_registry=False):
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
            self.assertEqual(job['job_id'],job_id);self.assertEqual(kind,'plugin');self.assertIn('--locked',argv);self.assertIn('--offline',argv);phases.append(phase)
            if phase=='plugin_test' and fail_tests:raise ValueError('native test failed')
            if phase=='plugin_compile':
                binary=cache/'target/release/fixture_runner';binary.parent.mkdir(parents=True);binary.write_bytes(b'tested executable')
                if alter_registry:(cache/'cargo/registry/crate').write_bytes(b'altered crate')
        runner.executor=types.SimpleNamespace(provision_native_registry=provision,phase=phase,artifact_hashes=module.Executor.artifact_hashes)
        with patch.object(fixtures.host,'ASSET_ROOT',assets),patch.object(fixtures.host,'STATE',state):
            if fail_tests or alter_registry:
                with self.assertRaisesRegex(ValueError,'native test failed' if fail_tests else 'changed verified dependency'):
                    runner.build_ability_artifacts(job_id)
                self.assertEqual(list((assets/'artifacts').glob('*/fixture_runner')) if (assets/'artifacts').exists() else [],[])
            else:
                result=runner.build_ability_artifacts(job_id);self.assertEqual(len(result),1);self.assertFalse(result[0]['reused'])
        return phases
    def test_verified_registry_and_complete_tests_precede_native_compile(self):
        self.assertEqual(self.exercise(),['provision','plugin_test','plugin_compile'])
    def test_native_test_failure_never_publishes_an_artifact(self):
        self.assertEqual(self.exercise(fail_tests=True),['provision','plugin_test'])
    def test_compiler_registry_mutation_never_publishes_an_artifact(self):
        self.assertEqual(self.exercise(alter_registry=True),['provision','plugin_test','plugin_compile'])


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
