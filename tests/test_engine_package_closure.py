"""Fixed package kernel bounds and Cargo test closure regressions."""
import hashlib,json,tempfile,threading,types,unittest,uuid
from pathlib import Path
from unittest.mock import patch
import test_scheduler_executor as fixtures
module=fixtures.module
class ClosureTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name);self.worker=module.Executor(None,None,{"state_root":str(self.root)})
        self.output=self.root/"output";self.artifacts=self.root/"artifacts";self.artifacts.mkdir();debug=self.output/"target/debug";debug.mkdir(parents=True)
        self.rows=[]
        for name,target,source,kind,test in [("deps/unit-0123456789abcdef","unit","/build/src/lib.rs",["lib"],True),("deps/service-0123456789abcdef","service","/build/crates/context-service/tests/service.rs",["test"],True),("webagent-context-service","webagent-context-service","/build/crates/context-service/src/main.rs",["bin"],False)]:
            path=debug/name;path.parent.mkdir(exist_ok=True);path.write_bytes(b"\x7fELF"+name.encode())
            self.rows.append({"reason":"compiler-artifact","package_id":"official","target":{"name":target,"src_path":source,"kind":kind},"profile":{"test":test},"executable":"/output/target/debug/"+name})
        self.write_metadata()
    def tearDown(self):self.temp.cleanup()
    def write_metadata(self):(self.output/"test-artifacts.json").write_text("".join(json.dumps(row)+"\n"for row in self.rows))
    def test_all_tests_and_exact_debug_support_retained(self):
        manifest=self.worker.retain_test_manifest(self.output,self.artifacts)
        self.assertEqual(len(manifest["tests"]),2);self.assertEqual(manifest["support"]["destination"],"target/debug/webagent-context-service")
        self.assertEqual((self.artifacts/manifest["support"]["file"]).read_bytes(),(self.output/"target/debug/webagent-context-service").read_bytes())
        self.assertEqual(self.worker.test_manifest(self.artifacts),manifest)
        replay=self.root/"replay";self.worker.restore_test_support(replay,self.artifacts,manifest)
        self.assertEqual((replay/"target/debug/webagent-context-service").read_bytes(),(self.output/"target/debug/webagent-context-service").read_bytes())
    def test_missing_official_debug_support_or_integration_rejected(self):
        for removed in (1,2):
            with self.subTest(removed=removed),tempfile.TemporaryDirectory()as dest:
                rows=self.rows;self.rows=[r for i,r in enumerate(rows)if i!=removed];self.write_metadata()
                with self.assertRaisesRegex(ValueError,"provenance"):self.worker.retain_test_manifest(self.output,Path(dest))
                self.rows=rows
    def test_wrong_support_package_or_profile_rejected(self):
        self.rows[-1]["package_id"]="other";self.write_metadata()
        with self.assertRaisesRegex(ValueError,"provenance"):self.worker.retain_test_manifest(self.output,self.artifacts)
    def test_wrong_support_test_profile_rejected(self):
        self.rows[-1]["profile"]["test"]=True;self.write_metadata()
        with self.assertRaises(ValueError):self.worker.retain_test_manifest(self.output,self.artifacts)
    def test_missing_integration_and_extra_runnable_rejected(self):
        manifest=self.worker.retain_test_manifest(self.output,self.artifacts)
        service=self.artifacts/next(item["file"]for item in manifest["tests"]if item["target_name"]=="service");saved=service.read_bytes();service.unlink()
        with self.assertRaisesRegex(ValueError,"changed"):self.worker.test_manifest(self.artifacts)
        service.write_bytes(saved);(self.artifacts/"tests/extra-0123456789abcdef").write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError,"inventory"):self.worker.test_manifest(self.artifacts)
    def test_retained_missing_tampered_and_inventory_fail_closed(self):
        manifest=self.worker.retain_test_manifest(self.output,self.artifacts)
        support=self.artifacts/manifest["support"]["file"];original=support.read_bytes();support.chmod(0o755);support.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError,"changed"):self.worker.test_manifest(self.artifacts)
        support.write_bytes(original);support.unlink()
        with self.assertRaisesRegex(ValueError,"changed"):self.worker.test_manifest(self.artifacts)
class PackageTests(unittest.TestCase):
    def run_package(self,first=None,last=None):
        cap=4026531840;identity="a"*64;calls=[];finished=[];entries=[];stats=iter([first or {"container_id":identity,"memory_max":cap,"swap_max":0,"oom_kills":0},last or {"container_id":identity,"memory_max":cap,"swap_max":0,"oom_kills":0}])
        resources=types.SimpleNamespace(enabled=True,lock=threading.RLock(),rows={},attached=lambda *a:None)
        def enter(name,*args,**kwargs):resources.rows[name]={};entries.append(kwargs)
        resources.enter=enter
        def command(argv,**kwargs):calls.append(argv);return identity if argv[1]=="create"else""
        runner=types.SimpleNamespace(command=command,resource_request=lambda _:next(stats),job_context=types.SimpleNamespace(),resource_progress=lambda _:None)
        worker=module.Executor(runner,resources,{"instance":"dev","state_root":"/tmp"});worker.finish_phase=lambda *args:finished.append(args)
        job={"job_id":str(uuid.uuid4()),"package_memory_cap":cap}
        with patch.object(module.subprocess,"run"):
            try:
                with worker.packager(job,"api"):calls.append(["work"])
            finally:self.calls=calls;self.finished=finished;self.entries=entries;self.job=job
    def test_kernel_cap_precedes_work_and_bounded_admission(self):
        self.run_package();create=self.calls[0]
        self.assertEqual(create[create.index("--memory")+1],"4026531840");self.assertEqual(create[create.index("--memory-swap")+1],"4026531840")
        self.assertEqual(self.entries[0],{"timeout":600,"hard_cap":4026531840});self.assertTrue(self.finished[-1][-1]);self.assertEqual(self.job["package_cap_outcomes"][0]["outcome"],"completed")
    def test_wrong_ancestor_or_swap_rejects_before_build(self):
        for bad in ({"container_id":"b"*64,"memory_max":4026531840,"swap_max":0},{"container_id":"a"*64,"memory_max":4026531840,"swap_max":1}):
            with self.assertRaises(ValueError):self.run_package(first=bad)
            self.assertNotIn(["work"],self.calls);self.assertFalse(self.finished[-1][-1])
    def test_oom_never_success_or_learning(self):
        with self.assertRaisesRegex(RuntimeError,"OOM"):self.run_package(last={"container_id":"a"*64,"memory_max":4026531840,"swap_max":0,"oom_kills":1,"oom_events":1})
        self.assertFalse(self.finished[-1][-1]);self.assertEqual(self.job["package_cap_outcomes"][0]["outcome"],"oom")

    def test_event_only_oom_never_success_or_learning(self):
        with self.assertRaisesRegex(RuntimeError,"OOM"):self.run_package(last={"container_id":"a"*64,"memory_max":4026531840,"swap_max":0,"oom_kills":0,"oom_events":1})
        self.assertFalse(self.finished[-1][-1]);self.assertEqual(self.job["package_cap_outcomes"][0]["outcome"],"oom")
