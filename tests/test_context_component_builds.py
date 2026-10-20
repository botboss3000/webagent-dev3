"""Drop-in context component build closures, ownership and typed proof regressions."""
import copy,hashlib,importlib.util,json,tempfile,types,unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
# Load one authoritative broker module for isolated component build regressions.
def load(name):
    spec=importlib.util.spec_from_file_location("component_test_"+name,ROOT/"build-service"/(name+".py"))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
engine=load("engine_components");executor=load("executor");ownership=load("ownership")
NEW={
 "context-assets":("webagent-assets-service","assets-service","context-assets","assets-contracts","assets.plan"),
 "capability-context":("webagent-capability-service","capability-service","capability-context","capability-contracts","capability.prepare"),
 "provider-projection":("webagent-provider-service","provider-service","provider-projection","provider-contracts","provider.project"),
}
class ComponentBuildTests(unittest.TestCase):
    # Reject cross-operation authority, unsupported interfaces and undeclared build inputs.
    def test_fixed_registrations_and_versions_reject_cross_operation_authority(self):
        with tempfile.TemporaryDirectory()as temporary:
            path=Path(temporary)/"component.json"
            for identity,parts in NEW.items():
                value=json.loads((ROOT/"plugins/engine"/identity/"component.json").read_text())
                path.write_text(json.dumps(value));self.assertEqual(engine.descriptor(path),value)
                self.assertEqual(engine.REGISTRY[identity]["binary"],parts[0])
                self.assertEqual(engine.REGISTRY[identity]["operation"],parts[-1])
                for field,bad in (("operation","provider.dispatch"),("interface_version",2),("interface_version",True)):
                    changed={**value,field:bad};path.write_text(json.dumps(changed))
                    with self.assertRaises(ValueError):engine.descriptor(path)
                changed=copy.deepcopy(value);changed["build"]["source_inputs"].append("/etc")
                path.write_text(json.dumps(changed))
                with self.assertRaises(ValueError):engine.descriptor(path)
            value=json.loads((ROOT/"plugins/engine/context-assembly/component.json").read_text())
            self.assertEqual(value["interface_version"],2)
            path.write_text(json.dumps(value));engine.descriptor(path)
            invalid={**value,"implementation_version":"not-semver"};path.write_text(json.dumps(invalid))
            with self.assertRaises(ValueError):engine.descriptor(path)
            value["interface_version"]=1;path.write_text(json.dumps(value));engine.descriptor(path)
            value["interface_version"]=3;path.write_text(json.dumps(value))
            with self.assertRaises(ValueError):engine.descriptor(path)

    # Require exact service dependency closures and reject symlinked source inputs.
    def test_each_service_closure_includes_shared_contracts_and_exact_local_dependencies(self):
        for identity,(_,service,implementation,contract,_)in NEW.items():
            with self.subTest(identity=identity),tempfile.TemporaryDirectory()as temporary:
                root=Path(temporary);(root/"Cargo.toml").write_text("[workspace]\n");(root/"Cargo.lock").write_text("version=4\n")
                for name,deps in [(service,[implementation,"engine-service"]),(implementation,[contract]),(contract,[]),("engine-service",[])]:
                    folder=root/"crates"/name;folder.mkdir(parents=True);(folder/"src").mkdir()
                    (folder/"src/lib.rs").write_text("pub fn fixture() {}\n")
                    (folder/"Cargo.toml").write_text('[package]\nname="'+name+'"\nversion="0.1.0"\n[dependencies]\n'+"".join(dep.replace("-","_")+'={path="../'+dep+'"}\n'for dep in deps))
                before=engine.closure(root,identity)
                expected={"Cargo.toml","Cargo.lock",*[f"crates/{name}/{tail}"for name in(service,implementation,contract,"engine-service")for tail in("Cargo.toml","src/lib.rs")]}
                self.assertEqual(set(before),expected)
                file=root/"crates"/contract/"src/lib.rs";file.write_text("pub fn changed() {}\n")
                self.assertNotEqual(before,engine.closure(root,identity))
                file.unlink();file.symlink_to("/etc/passwd")
                with self.assertRaisesRegex(ValueError,"symlink"):engine.closure(root,identity)

    # Keep shared contracts core-owned while honoring compiler-attested implementation dependencies.
    def test_new_shared_contracts_require_core_and_implementation_respects_compiler_attestation(self):
        with tempfile.TemporaryDirectory()as temporary:
            for identity,(_,service,implementation,contract,_)in NEW.items():
                path=f"crates/{implementation}/src/lib.rs"
                scopes=ownership.Components(temporary).scopes([path])
                self.assertIn("module:engine-component:"+identity,scopes);self.assertNotIn("module:engine",scopes)
                self.assertIn("module:engine",ownership.Components(temporary,[path]).scopes([path]))
                self.assertIn("module:engine",ownership.Components(temporary).scopes([f"crates/{contract}/src/lib.rs"]))
                self.assertTrue(engine.package_path("v3/"+path,{}))
            scopes=ownership.Components(temporary).scopes(["crates/engine-service/src/lib.rs"])
            for identity in NEW:self.assertIn("module:engine-component:"+identity,scopes)

    # Require every new component proof and compare its linked and live prepared cases.
    def test_each_new_optimized_proof_is_required_and_compared_across_modes(self):
        linked={"mode":"linked","interface_version":2,"equivalence":True,"service_started":False,
            **{key:[{"case":1},{"case":2}]for key in executor.SMOKE_CASE_KEYS}}
        live={**linked,"mode":"live","service_reused":True,"service_drained":True}
        self.assertEqual(executor.Executor.live_smoke(json.dumps(live),linked),live)
        for key in("assets_prepared","capability_prepared","provider_prepared"):
            missing=dict(linked);missing.pop(key)
            with self.assertRaises(ValueError):executor.Executor.linked_smoke(json.dumps(missing))
            changed={**live,key:[{},{}]}
            with self.assertRaisesRegex(ValueError,"differ"):executor.Executor.live_smoke(json.dumps(changed),linked)
        with self.assertRaises(ValueError):executor.Executor.linked_smoke(json.dumps({**linked,"interface_version":1}))

    # Retain legacy proof validation without accepting mixed-interface equivalence.
    def test_existing_release_proofs_remain_valid_during_broker_upgrade(self):
        linked={"mode":"linked","interface_version":1,"equivalence":True,"service_started":False,
            **{key:[{"case":1},{"case":2}]for key in executor.SMOKE_CASE_KEYS[:3]}}
        live={**linked,"mode":"live","service_reused":True,"service_drained":True}
        self.assertEqual(executor.Executor.linked_smoke(json.dumps(linked)),linked)
        self.assertEqual(executor.Executor.live_smoke(json.dumps(live),linked),live)
        upgraded={**live,"interface_version":2,**{key:[{},{}]for key in executor.SMOKE_CASE_KEYS[3:]}}
        with self.assertRaisesRegex(ValueError,"differ"):executor.Executor.live_smoke(json.dumps(upgraded),linked)

    # Retain and restore official service test binaries only with matching package provenance.
    def test_official_new_debug_elf_support_is_retained_and_bound_to_test_package(self):
        for identity,(binary,service,_,_,_)in NEW.items():
            with self.subTest(identity=identity),tempfile.TemporaryDirectory()as temporary:
                root=Path(temporary);output=root/"output";artifacts=root/"artifacts";artifacts.mkdir()
                debug=output/"target/debug";debug.mkdir(parents=True);(debug/"deps").mkdir()
                rows=[]
                # Create a bounded public ELF fixture and its matching Cargo compiler-artifact record.
                def row(name,target,source,test,package):
                    (debug/name).write_bytes(b"\x7fELF"+name.encode())
                    rows.append({"reason":"compiler-artifact","package_id":package,"target":{"name":target,"src_path":source,"kind":["test"]if test else["bin"]},"profile":{"test":test},"executable":"/output/target/debug/"+name})
                row("webagent-context-service","webagent-context-service","/build/crates/context-service/src/main.rs",False,"context")
                row("deps/service-0123456789abcdef","service","/build/crates/context-service/tests/service.rs",True,"context")
                row(binary,binary,f"/build/crates/{service}/src/main.rs",False,identity)
                row("deps/stdio-0123456789abcdef","stdio",f"/build/crates/{service}/tests/stdio.rs",True,identity)
                # The explicit debug build can repeat an identical Cargo compiler-artifact.
                rows.append(copy.deepcopy(rows[-2]))
                (output/"test-artifacts.json").write_text("".join(json.dumps(item)+"\n"for item in rows))
                worker=executor.Executor(None,None,{})
                manifest=worker.retain_test_manifest(output,artifacts)
                self.assertEqual(len(manifest["additional_support"]),1)
                self.assertEqual(manifest["additional_support"][0]["destination"],"target/debug/"+binary)
                self.assertEqual(worker.test_manifest(artifacts),manifest)
                replay=root/"replay";worker.restore_test_support(replay,artifacts,manifest)
                self.assertEqual((replay/"target/debug"/binary).read_bytes(),(debug/binary).read_bytes())
                manifest["additional_support"][0]["package_id"]="unrelated"
                (artifacts/"test-manifest.json").write_text(json.dumps(manifest))
                with self.assertRaisesRegex(ValueError,"provenance"):worker.test_manifest(artifacts)

    # Bind the reviewed compiler recipe and both runtime images to all registered service binaries.
    def test_packaged_services_and_reviewed_compiler_stage_are_complete(self):
        docker=(ROOT/"Dockerfile").read_text()
        stage=docker[docker.index("FROM rust:"):docker.index("FROM debian:",docker.index("FROM rust:"))]
        self.assertEqual(hashlib.sha256(stage.encode()).hexdigest(),executor.PARALLEL_COMPILER_STAGE_SHA256)
        for identity,(binary,*_)in NEW.items():
            self.assertIn(binary,executor.BINARIES)
            self.assertIn(f"install -Dm755 /build/target/release/{binary} /out/{binary}",docker)
            self.assertEqual(docker.count(f"/usr/local/lib/webagent/engine/{identity}/{binary}"),2)
        self.assertIn("webagent-background-service",executor.BINARIES)
        self.assertIn("webagent-boot-service",executor.BINARIES)
if __name__=="__main__":unittest.main()
