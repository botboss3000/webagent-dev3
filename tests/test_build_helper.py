"""Build helper (remote Rust test half): settings gate, source mirroring, fetch bounds and the trust checks."""
import hashlib,importlib.util,io,json,os,tarfile,tempfile,time,types,unittest
from pathlib import Path
SERVICE=Path(__file__).parents[1]/"build-service"
def load(name):
    spec=importlib.util.spec_from_file_location("helper_"+name,SERVICE/(name+".py"))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
executor=load("executor");helper=load("remote_helper")
SETTINGS={"enabled":True,"host":"100.126.37.96","user":"ar","key":"/etc/webagent/broker-helper/id_ed25519","known_hosts":"/etc/webagent/broker-helper/known_hosts"}
def worker(remote=SETTINGS):
    return executor.Executor(types.SimpleNamespace(compiler_runtime=lambda job:{"image":"sha256:"+"7"*64}),None,{"scheduler":{"remote_tests":remote}})
def tar(files):
    stream=io.BytesIO()
    with tarfile.open(fileobj=stream,mode="w")as archive:
        for name,body in files.items():
            member=tarfile.TarInfo(name);member.size=len(body);member.mode=0o644;archive.addfile(member,io.BytesIO(body))
    stream.seek(0);return stream
class SettingsTests(unittest.TestCase):
    def test_helper_is_used_only_when_enabled_complete_and_not_stable(self):
        self.assertEqual(worker().remote_tests({"release_mode":"dev"}),SETTINGS)
        self.assertIsNone(worker().remote_tests({"release_mode":"stable"}))
        self.assertIsNone(worker({**SETTINGS,"enabled":False}).remote_tests({}))
        self.assertIsNone(worker({k:v for k,v in SETTINGS.items()if k!="key"}).remote_tests({}))
        self.assertIsNone(executor.Executor(None,None,{"scheduler":{}}).remote_tests({}))
    def test_ssh_pins_the_host_key_and_uses_only_the_broker_key(self):
        argv=executor.Executor.remote_argv(SETTINGS,"status")
        self.assertEqual(argv[-2:],["ar@100.126.37.96","status"])
        for option in ("BatchMode=yes","IdentitiesOnly=yes","StrictHostKeyChecking=yes","UserKnownHostsFile="+SETTINGS["known_hosts"]):self.assertIn(option,argv)
        self.assertEqual(argv[argv.index("-i")+1],SETTINGS["key"])
class SourceMirrorTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        self.app=self.root/"app";(self.app/"src").mkdir(parents=True)
        (self.app/"src/main.rs").write_text("fn main(){}");(self.app/"Cargo.lock").write_text("lock")
        (self.app/"run.sh").write_text("#!/bin/sh");(self.app/"run.sh").chmod(0o755)
        self.mirror=self.root/"mirror"
    def tearDown(self):self.temp.cleanup()
    def sync(self):
        body,digest=executor.Executor.helper_tree(self.app)
        return helper.sync_source(io.BytesIO(body),self.mirror),digest
    def test_digest_matches_and_only_changed_files_get_new_mtimes(self):
        result,digest=self.sync()
        self.assertEqual(result["sha256"],digest)
        self.assertEqual((self.mirror/"run.sh").stat().st_mode&0o111,0o111)
        old=time.time()-3600
        for name in ("src/main.rs","Cargo.lock"):os.utime(self.mirror/name,(old,old))
        (self.app/"src/main.rs").write_text("fn main(){println!()}")
        (self.app/"extra.rs").write_text("");result,digest=self.sync()
        self.assertEqual(result["sha256"],digest);self.assertEqual(result["changed"],2)
        self.assertEqual(int((self.mirror/"Cargo.lock").stat().st_mtime),int(old))
        self.assertGreater((self.mirror/"src/main.rs").stat().st_mtime,old+60)
        (self.app/"extra.rs").unlink();result,digest=self.sync()
        self.assertFalse((self.mirror/"extra.rs").exists());self.assertEqual(result["removed"],1);self.assertEqual(result["sha256"],digest)
    def test_escaping_members_are_refused(self):
        for name in ("../escape","/abs"):
            with self.assertRaises(SystemExit):helper.sync_source(tar({name:b"x"}),self.mirror)
class FetchTests(unittest.TestCase):
    def setUp(self):self.temp=tempfile.TemporaryDirectory();self.output=Path(self.temp.name)
    def tearDown(self):self.temp.cleanup()
    def test_test_file_list_stays_in_the_fixed_debug_target(self):
        metadata=self.output/"test-artifacts.json"
        rows=[{"reason":"compiler-artifact","executable":"/output/target/debug/deps/webagent_v3-0123456789abcdef"},{"reason":"compiler-artifact","executable":None},{"reason":"build-finished"}]
        metadata.write_text("\n".join(json.dumps(r)for r in rows))
        self.assertEqual(executor.Executor.remote_test_files(metadata),["target/debug/deps/webagent_v3-0123456789abcdef"])
        metadata.write_text(json.dumps({"reason":"compiler-artifact","executable":"/output/target/debug/../../etc/passwd"}))
        with self.assertRaises(ValueError):executor.Executor.remote_test_files(metadata)
    def test_fetch_accepts_exactly_the_requested_files(self):
        w=worker();job={"job_id":"0"*8+"-0000-0000-0000-"+"0"*12}
        def reply(files):
            def call(settings,*words,stdin=None,timeout=0,output_file=None):output_file.write(tar(files).getvalue());return""
            return call
        w.remote_call=reply({"target/debug/a":b"a"});w.remote_fetch(SETTINGS,job,self.output,["target/debug/a"])
        self.assertEqual((self.output/"target/debug/a").read_bytes(),b"a")
        w.remote_call=reply({"target/debug/a":b"a","target/debug/b":b"b"})
        with self.assertRaises(ValueError):w.remote_fetch(SETTINGS,job,self.output,["target/debug/a"])
        self.assertEqual([p.name for p in self.output.iterdir()if p.suffix==".tar"],[])
class HalfTests(unittest.TestCase):
    """The helper's half with its remote verbs and local steps replaced by recorders."""
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.output=Path(self.temp.name)/"output";self.app=Path(self.temp.name)/"app"
        (self.output/"cargo/registry").mkdir(parents=True);self.app.mkdir();(self.app/"main.rs").write_text("x")
        self.job={"job_id":"1"*8+"-0000-0000-0000-"+"0"*12,"dependency_cache":{"key":"a"*64}}
        self.w=worker();self.calls=[];self.staged=None
        manifest={"tests":[{"file":"tests/t-0123456789abcdef","sha256":"f"*64}]}
        self.w.remote_fetch=lambda settings,job,output,names:(output/"test-artifacts.json").write_text(json.dumps({"reason":"compiler-artifact","executable":"/output/target/debug/deps/t-0123456789abcdef"}))
        self.w.retain_test_manifest=lambda output,artifacts:manifest
        self.w.linked_reference=lambda job,app,output,state:state.update(linked="l",proof={"ok":1})or""
        self.w.test_selection=lambda job,blob,app,output,manifest:{"argv":["suite"],"run":[],"skipped":[]}
        self.w.phase=lambda job,phase,*args,remote=None,**kw:self.calls.append(("phase",phase,remote is not None))or phase+" log\n"
    def tearDown(self):self.temp.cleanup()
    def call(self,settings,*words,stdin=None,timeout=0,output_file=None):
        self.calls.append(words[0])
        if words[0]=="begin":return json.dumps({"decision":self.begin})
        if words[0]=="status":return json.dumps({"images":["sha256:"+"7"*64]})
        if words[0]=="sync-source":return json.dumps({"sha256":executor.Executor.helper_tree(self.app)[1],"changed":1})
        if words[0]=="has-cargo":return json.dumps({"present":True})
        if words[0]=="stage":return json.dumps(self.staged or {"tests/t-0123456789abcdef":"f"*64})
        raise AssertionError(words)
    def run_half(self,begin="accepted",staged=None):
        self.begin=begin;self.staged=staged;self.w.remote_call=self.call;state={}
        return self.w.remote_test_half(self.job,b"blob",self.app,self.output,SETTINGS,state),state
    def test_passing_half_runs_both_phases_remotely_in_order(self):
        logs,state=self.run_half()
        self.assertIn("build helper",logs)
        self.assertEqual([c for c in self.calls if isinstance(c,str)],["begin","status","sync-source","has-cargo","stage"])
        self.assertEqual([c for c in self.calls if isinstance(c,tuple)],[("phase","compile_test",True),("phase","test",True)])
        self.assertEqual(self.job["remote_tests"]["decision"],"passed");self.assertEqual(self.job["focused_verification"]["status"],"not_applicable")
        self.assertIn("selection",state)
    def test_busy_helper_or_changed_binaries_stop_the_half_for_a_local_run(self):
        with self.assertRaisesRegex(ValueError,"busy"):self.run_half(begin="busy")
        self.calls.clear()
        with self.assertRaisesRegex(ValueError,"differ from the retained manifest"):self.run_half(staged={"tests/t-0123456789abcdef":"0"*64})
        self.assertNotIn(("phase","test",True),self.calls)
class ConfigTests(unittest.TestCase):
    def test_enabled_helper_needs_protected_key_paths(self):
        source=(SERVICE/"config.py").read_text()
        self.assertIn('"remote_tests"}-RESOURCE_KEYS-TUNING_KEYS',source)
        self.assertEqual(json.loads((SERVICE/"scheduler-defaults.json").read_text())["remote_tests"],{"enabled":False})
if __name__=="__main__":unittest.main()
