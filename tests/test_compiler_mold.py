"""Verify fixed Mold preparation, immutable execution identity and fail-closed migration."""
import contextlib
import hashlib
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch
import test_admin_build as fixtures

host=fixtures.host

class CompilerRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.service=self.root/"service";self.service.mkdir()
        body=(Path(__file__).parents[1]/"build-service/compiler.Dockerfile").read_bytes()
        (self.service/"compiler.Dockerfile").write_bytes(body)
        self.state=self.root/"state";self.state.mkdir()
        self.config=patch.dict(host.CONFIG,{"service_dir":str(self.service)});self.config.start();self.addCleanup(self.config.stop)
        self.state_patch=patch.object(host,"STATE",self.state);self.state_patch.start();self.addCleanup(self.state_patch.stop)
        self.payload=b"independent compiler executable fixture"
        self.hash_patch=patch.object(host.executor_module,"MOLD_SHA256",hashlib.sha256(self.payload).hexdigest());self.hash_patch.start();self.addCleanup(self.hash_patch.stop)
        self.image="sha256:"+"7"*64;self.actual=self.image;self.available=True;self.calls=[];self.before="libc6:amd64\t2.36-9+deb12u14\n"
        self.after=self.before+"mold\t1.10.1+dfsg-1\n"
        self.runner=host.Runner(source=self.root/"source",snapshot_socket=None)
        self.runner.executor=types.SimpleNamespace(packager=lambda *_:contextlib.nullcontext("measured-builder"))
        self.runner.command=self.command;self.runner.resource_progress=lambda _:None
        self.runner.retain_image=lambda image,job_id=None:{"retained":True,"image":image,"job_id":job_id}
        self.job={"job_id":str(__import__("uuid").uuid4()),"log":""}

    def command(self,argv,**unused):
        self.calls.append(argv)
        if argv[:3]==["docker","buildx","build"]:
            self.available=True;Path(argv[argv.index("--iidfile")+1]).write_text(self.image);return "fixed compiler image prepared"
        if argv[:2]==["docker","cp"]:
            source=argv[2].split(":",1)[1]
            content={"/usr/local/share/webagent/compiler/before.tsv":self.before.encode(),"/usr/local/share/webagent/compiler/after.tsv":self.after.encode(),"/usr/local/share/webagent/compiler/version.txt":b"mold 1.10.1 (compatible with GNU ld)\n","/usr/bin/mold":self.payload}[source]
            Path(argv[3]).write_bytes(content);return ""
        if argv[:3]==["docker","image","inspect"]:
            if not self.available:raise RuntimeError("No such image")
            return json.dumps([{"Id":self.actual,"Config":{"Labels":{"webagent.compiler.recipe":host.executor_module.MOLD_COMPILER_DOCKERFILE_SHA256}}}])
        if argv[:2]in (["docker","create"],["docker","rm"]):return ""
        raise AssertionError("Unexpected compiler preparation command: "+repr(argv))

    def test_missing_runtime_fails_without_using_stock_rust_or_building(self):
        with self.assertRaisesRegex(ValueError,"supported dependency preparation"):self.runner.compiler_runtime()
        self.assertEqual(self.calls,[])

    def test_fixed_preparation_retains_exact_image_package_versions_and_linker_bytes(self):
        value=self.runner.compiler_runtime(self.job,prepare=True)
        self.assertEqual(value["image"],self.image);self.assertEqual(value["mold_sha256"],hashlib.sha256(self.payload).hexdigest())
        self.assertEqual(value["before_packages"],self.before);self.assertEqual(value["after_packages"],self.after)
        builds=[argv for argv in self.calls if argv[:3]==["docker","buildx","build"]]
        self.assertEqual(len(builds),1);self.assertIn("measured-builder",builds[0]);self.assertIn("--load",builds[0])
        self.assertFalse(any(argv[:2]==["docker","run"]for argv in self.calls))
        self.runner.compiler_runtime();self.assertEqual(len([argv for argv in self.calls if argv[:3]==["docker","buildx","build"]]),1)
        self.actual="sha256:"+"8"*64
        with self.assertRaisesRegex(ValueError,"image unavailable or changed"):self.runner.compiler_runtime()
        self.actual=self.image;self.available=False
        with self.assertRaisesRegex(ValueError,"supported dependency preparation"):self.runner.compiler_runtime()
        retry={"job_id":str(__import__("uuid").uuid4()),"log":""}
        recovered=self.runner.compiler_runtime(retry,prepare=True)
        self.assertEqual(recovered["image"],self.image)
        self.assertEqual(len(list((self.state/"compiler-runtime").rglob("*.previous.json"))),1)

    def test_unrelated_package_upgrade_is_rejected_without_publishing_receipt(self):
        self.after="libc6:amd64\t2.36-new\nmold\t1.10.1+dfsg-1\n"
        with self.assertRaisesRegex(ValueError,"unrelated package versions"):self.runner.compiler_runtime(self.job,prepare=True)
        self.assertFalse(list((self.state/"compiler-runtime").rglob("receipt.json")))
        self.assertTrue(any(argv[:2]==["docker","rm"]for argv in self.calls))

    def test_linker_corruption_and_protected_recipe_changes_fail_closed(self):
        self.payload=b"unexpected binary"
        with self.assertRaisesRegex(ValueError,"Mold identity changed"):self.runner.compiler_runtime(self.job,prepare=True)
        (self.service/"compiler.Dockerfile").write_text("FROM unreviewed\n")
        with self.assertRaisesRegex(ValueError,"Protected compiler recipe changed"):self.runner.compiler_runtime()


class MoldRecipeTests(unittest.TestCase):
    def test_derived_image_changes_compiler_certificate_and_missing_image_has_no_fallback(self):
        module=host.executor_module
        image={"image":"sha256:"+"1"*64}
        runner=types.SimpleNamespace(compiler_runtime=lambda *args:image)
        worker=module.Executor(runner,None,{})
        first=worker.compiler_recipe();image["image"]="sha256:"+"2"*64
        self.assertNotEqual(first,worker.compiler_recipe())
        self.assertIn("mold1.10.1:complete-tests-v8",first)
        del image["image"]
        with self.assertRaises(KeyError):worker.compiler_recipe()

    def test_runtime_cache_dependency_prefix_is_unchanged_and_alias_is_packaged(self):
        docker=(Path(__file__).parents[1]/"Dockerfile").read_text()
        recipe=host.packaging_cache_module.runtime_recipe(docker,"api",b"fixture helper")
        self.assertNotIn("mold",recipe["packages"])
        self.assertIn("COPY --from=tested_builder /out/compiler/mold /usr/bin/ld.mold",docker)
        compiler=(Path(__file__).parents[1]/"build-service/compiler.Dockerfile").read_text()
        self.assertIn("cc -fuse-ld=mold -Wl,--version",compiler)
        self.assertIn("mold=1.10.1+dfsg-1",compiler)
        self.assertEqual(hashlib.sha256(compiler.encode()).hexdigest(),host.executor_module.MOLD_COMPILER_DOCKERFILE_SHA256)
        stage=docker[docker.index("FROM rust:"):docker.index("FROM debian:")]
        self.assertEqual(hashlib.sha256(stage.encode()).hexdigest(),host.executor_module.MOLD_COMPILER_STAGE_SHA256)

    def test_native_cache_dangling_and_incomplete_identities_never_become_absent(self):
        with tempfile.TemporaryDirectory()as temporary:
            artifact=Path(temporary)/"artifact";artifact.mkdir()
            runner=host.Runner();binary=artifact/"native";receipt=artifact/"compiler-receipt.json"
            for path in (binary,receipt):
                path.symlink_to(Path(temporary)/"absent")
                with self.assertRaisesRegex(ValueError,"identity invalid"):runner.plugin_artifact_verified(artifact,"native","a"*64)
                path.unlink()
            binary.write_bytes(b"occupied immutable executable")
            with self.assertRaisesRegex(ValueError,"incomplete"):runner.plugin_artifact_verified(artifact,"native","a"*64)
            receipt.write_bytes(b"x"*65537)
            with self.assertRaisesRegex(ValueError,"bound invalid"):runner.plugin_artifact_verified(artifact,"native","a"*64)
            real=artifact.with_name("real");artifact.rename(real);artifact.symlink_to(real,target_is_directory=True)
            with self.assertRaisesRegex(ValueError,"identity invalid"):runner.plugin_artifact_verified(artifact,"native","a"*64)

    def test_native_plugin_receipts_do_not_follow_dev_only_engine_recipe_changes(self):
        # The plugin cache key omits the recipe, so a dev-only engine token must not invalidate it.
        executor=host.executor_module
        self.assertTrue(executor.COMPILER_RECIPE.startswith(executor.PLUGIN_COMPILER_RECIPE+":"))
        with tempfile.TemporaryDirectory()as temporary:
            artifact=Path(temporary)/"artifact";artifact.mkdir();binary=artifact/"native";binary.write_bytes(b"\x7fELF native")
            recipe=executor.PLUGIN_COMPILER_RECIPE+":sha256:"+"b"*64+":native-release:locked:offline"
            value={"input_key":"a"*64,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"recipe":recipe,"linker_comment":"mold 1.10.1 (compatible with GNU ld)"}
            (artifact/"compiler-receipt.json").write_text(json.dumps(value))
            self.assertEqual(host.Runner().plugin_artifact_verified(artifact,"native","a"*64),value)

    def test_root_and_native_proof_containers_pass_real_resource_identity_gate(self):
        import ast
        control=host.scheduler_module.sibling("resource_control")
        worker=control.Control({"instance":"development","scheduler":{}})
        found=[]
        for filename in ("executor.py","host.py"):
            tree=ast.parse((Path(__file__).parents[1]/"build-service"/filename).read_text())
            for call in ast.walk(tree):
                if not isinstance(call,ast.Call)or not isinstance(call.func,ast.Attribute)or call.func.attr!="phase"or "mold-proof"not in ast.unparse(call):continue
                phase=ast.literal_eval(call.args[1]);found.append((filename,phase))
                self.assertIn(phase,control.PHASES)
                self.assertEqual(ast.literal_eval(call.args[2]),"dependency")
                name="webagent-build-development-"+"a"*32+"-"+phase+"-"+"b"*8
                value={"Id":"c"*64,"State":{"Status":"created"},"Config":{"Labels":{"webagent.scheduler.instance":"development","webagent.scheduler.job":"a"*32,"webagent.scheduler.phase":phase}},"HostConfig":{"CgroupParent":worker.slice}}
                with patch.object(control,"inspect",return_value=value),patch.object(control.os,"geteuid",return_value=0):
                    result=worker.handle({"operation":"identity","container":name})
                self.assertEqual(result["phase"],phase);self.assertEqual(result["state"],"created")
        self.assertEqual(sorted(found),[("executor.py","engine_metadata"),("host.py","engine_metadata")])


class WorkspaceLinkerActivationTests(unittest.TestCase):
    def workspace_module(self):
        import importlib.util
        spec=importlib.util.spec_from_file_location("mold_workspace_fixture",Path(__file__).parents[1]/"build-service/workspace.py")
        value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value

    def test_capability_follows_immutable_image_and_never_missing_tool_fallback(self):
        workspace=self.workspace_module()
        with tempfile.TemporaryDirectory()as temporary:
            marker=Path(temporary)/"compiler-linker"
            with patch.object(workspace,"COMPILER_LINKER_MARKER",marker):
                self.assertEqual(workspace.cargo_rustflags(),"-C debuginfo=0")
                marker.write_bytes(b"mold\n")
                # Selection depends on declared image capability, never tool existence.
                self.assertEqual(workspace.cargo_rustflags(),host.executor_module.RUSTFLAGS)
                for body in (b"",b"gnu\n",b"mold",b"mold\nextra"):
                    marker.write_bytes(body)
                    with self.assertRaisesRegex(workspace.Rejected,"capability invalid"):workspace.cargo_rustflags()
                marker.unlink();marker.mkdir()
                with self.assertRaisesRegex(workspace.Rejected,"capability invalid"):workspace.cargo_rustflags()
                marker.rmdir();marker.symlink_to(Path(temporary)/"missing")
                with self.assertRaisesRegex(workspace.Rejected,"capability invalid"):workspace.cargo_rustflags()

    def test_new_image_probe_requires_marker_and_binaries(self):
        import ast
        from unittest.mock import MagicMock
        source=(Path(__file__).parents[1]/"build-service/host.py").read_text()
        programs=[node.value for node in ast.walk(ast.parse(source))if isinstance(node,ast.Constant)and isinstance(node.value,str)and 'marker=pathlib.Path("/usr/local/share/webagent/compiler-linker")'in node.value]
        self.assertEqual(len(programs),1)
        program=programs[0].replace("MOLD_BINARY_HASH",host.executor_module.MOLD_SHA256)
        marker=MagicMock();marker.is_symlink.return_value=False;marker.is_file.return_value=True
        marker.stat.return_value.st_size=5;marker.read_bytes.return_value=b"mold\n"
        binary=MagicMock();binary.is_symlink.return_value=False;binary.is_file.return_value=False
        class Paths:
            def Path(self,value):
                if value=="/usr/local/share/webagent/compiler-linker":return marker
                self_outer.assertEqual(value,"/usr/bin")
                return self
            def __truediv__(self,name):
                self_outer.assertIn(name,("mold","ld.mold"));return binary
        self_outer=self
        scope={"pathlib":Paths(),"hashlib":hashlib}
        marker.is_file.return_value=False
        with patch("subprocess.check_output")as command:
            with self.assertRaisesRegex(RuntimeError,"capability invalid"):exec(program,scope)
            command.assert_not_called()
        marker.is_file.return_value=True
        with patch("subprocess.check_output")as command:
            with self.assertRaisesRegex(RuntimeError,"differs from verified compiler"):exec(program,scope)
            command.assert_not_called()
        docker=(Path(__file__).parents[1]/"Dockerfile").read_text()
        self.assertIn("RUN install -d -m 0755 /usr/local/share/webagent && printf 'mold\\n' > /usr/local/share/webagent/compiler-linker",docker)

    def test_new_image_probe_keeps_compiler_stderr_inside_json_contract(self):
        import ast,subprocess,sys
        source=(Path(__file__).parents[1]/"build-service/host.py").read_text()
        program=next(node.value for node in ast.walk(ast.parse(source))if isinstance(node,ast.Constant)and isinstance(node.value,str)and 'marker=pathlib.Path("/usr/local/share/webagent/compiler-linker")'in node.value)
        harness=r'''import hashlib,json,subprocess,sys,types
payload=b"verified compiler fixture"
program=sys.argv[1].replace("MOLD_BINARY_HASH",hashlib.sha256(payload).hexdigest())
class File:
 def __init__(self,marker=False):self.marker=marker
 def is_symlink(self):return False
 def is_file(self):return True
 def stat(self):return types.SimpleNamespace(st_size=5)
 def read_bytes(self):return b"mold\n"if self.marker else payload
 def __str__(self):return "/usr/bin/mold"
class Paths:
 def Path(self,value):return File(True)if value=="/usr/local/share/webagent/compiler-linker"else self
 def __truediv__(self,name):return File()
original=subprocess.check_output
def output(argv,**kwargs):
 if argv[0]=="cc":
  return original([sys.executable,"-c","import sys; print('collect2 diagnostic',file=sys.stderr); print('mold 1.10.1 (compatible with GNU ld)')"],**kwargs)
 return "mold 1.10.1 (compatible with GNU ld)\n"
subprocess.check_output=output
print(json.dumps({"files":1,"runtime_files":5}))
exec(program,{"pathlib":Paths(),"hashlib":hashlib})
'''
        result=subprocess.run([sys.executable,"-c",harness,program],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,timeout=10)
        self.assertEqual(result.returncode,0,result.stdout)
        self.assertEqual(json.loads(result.stdout),{"files":1,"runtime_files":5})
