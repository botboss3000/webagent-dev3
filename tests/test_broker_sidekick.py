"""Broker sidekick (remote Rust test half): live switch, pause, source mirroring, fetch bounds, kept steps and trust checks."""
import hashlib,importlib.util,io,json,os,tarfile,tempfile,time,types,unittest
from pathlib import Path
SERVICE=Path(__file__).parents[1]/"build-service"
def load(name):
    spec=importlib.util.spec_from_file_location("sidekick_"+name,SERVICE/(name+".py"))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
executor=load("executor");helper=load("sidekick");bootstrap=load("sidekick_bootstrap")
SETTINGS={"enabled":True,"host":"100.126.37.96","user":"ar","key":"/etc/webagent/broker-sidekick/id_ed25519","known_hosts":"/etc/webagent/broker-sidekick/known_hosts"}
STATE=tempfile.TemporaryDirectory()
def worker(remote=SETTINGS,state=None):
    return executor.Executor(types.SimpleNamespace(compiler_runtime=lambda job:{"image":"sha256:"+"7"*64}),None,{"scheduler":{"sidekick":remote},"state_root":state or tempfile.mkdtemp(dir=STATE.name)})
def tar(files):
    stream=io.BytesIO()
    with tarfile.open(fileobj=stream,mode="w")as archive:
        for name,body in files.items():
            member=tarfile.TarInfo(name);member.size=len(body);member.mode=0o644;archive.addfile(member,io.BytesIO(body))
    stream.seek(0);return stream
class SettingsTests(unittest.TestCase):
    def test_sidekick_is_used_only_when_enabled_complete_and_not_stable(self):
        self.assertEqual(worker().sidekick({"release_mode":"dev"}),SETTINGS)
        self.assertIsNone(worker().sidekick({"release_mode":"stable"}))
        self.assertIsNone(worker({**SETTINGS,"enabled":False}).sidekick({}))
        self.assertIsNone(worker({k:v for k,v in SETTINGS.items()if k!="key"}).sidekick({}))
        self.assertIsNone(executor.Executor(None,None,{"scheduler":{},"state_root":STATE.name}).sidekick({}))
        legacy=executor.Executor(None,None,{"scheduler":{"remote_tests":SETTINGS},"state_root":tempfile.mkdtemp(dir=STATE.name)})
        self.assertEqual(legacy.sidekick({}),SETTINGS)
    def test_live_switch_and_automatic_pause(self):
        w=worker();job={}
        w.sidekick_update_state(lambda v:v.update(enabled=False,reason="maintenance"))
        self.assertIsNone(w.sidekick(job));self.assertEqual(job["sidekick"]["decision"],"switched_off")
        w.sidekick_update_state(lambda v:v.update(enabled=True))
        w.sidekick_outcome(SETTINGS,executor.SidekickDeclined("busy"));w.sidekick_outcome(SETTINGS,RuntimeError("tests failed"))
        self.assertEqual(w.sidekick_state().get("failures",0),0);self.assertEqual(w.sidekick({}),SETTINGS)
        for n in range(1,4):
            w.sidekick_outcome(SETTINGS,TimeoutError("link dropped"))
            job={};self.assertIsNone(w.sidekick(job))
            self.assertEqual(job["sidekick"]["decision"],"switched_off"if n==3 else"paused")
        self.assertIn("automatically after 3 failures",w.sidekick_state()["reason"])
        w.sidekick_update_state(lambda v:v.update(enabled=True,failures=0,paused_until=0));w.sidekick_outcome(SETTINGS)
        self.assertEqual(w.sidekick({}),SETTINGS)
    def test_switch_state_never_lands_where_the_broker_reads_job_receipts(self):
        w=worker();w.sidekick_update_state(lambda v:v.update(enabled=False))
        root=Path(w.config["state_root"])
        self.assertEqual(list(root.glob("*.json")),[])
        self.assertEqual(w.sidekick_state_path(),root/"coordination/sidekick/state.json")
    def test_disconnect_exit_codes_count_and_test_failures_do_not(self):
        def failed(code):
            error=RuntimeError("fixed command failed");error.exit_code=code;return error
        self.assertTrue(executor.Executor.sidekick_infrastructure(failed(255)))
        self.assertFalse(executor.Executor.sidekick_infrastructure(failed(101)))
        self.assertFalse(executor.Executor.sidekick_infrastructure(executor.SidekickDeclined("off")))
    def test_control_replies_skip_the_log_scrubber_and_phase_logs_do_not(self):
        """The scrubber masks hash-like values; a masked hash broke the status JSON on the first real job."""
        seen=[]
        runner=types.SimpleNamespace(command=lambda argv,**kw:seen.append(kw)or'{"version": 3}',compiler_runtime=lambda job:{})
        w=executor.Executor(runner,None,{"scheduler":{"sidekick":SETTINGS},"state_root":tempfile.mkdtemp(dir=STATE.name)})
        self.assertEqual(w.sidekick_json(SETTINGS,"status"),{"version":3});self.assertTrue(seen[-1]["private_raw"])
        w.persist_phase_log=lambda job,logs:None
        w._sidekick_workload({"job_id":"j"},"test",["x"],"engine:rust1.90:test",60,{"settings":SETTINGS,"image":"i","cargo_key":"k"})
        self.assertNotIn("private_raw",seen[-1])
    def test_ssh_pins_the_host_key_and_uses_only_the_broker_key(self):
        argv=executor.Executor.sidekick_argv(SETTINGS,"status")
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
        body,digest=executor.Executor.sidekick_tree(self.app)
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
        self.assertEqual(executor.Executor.sidekick_test_files(metadata),["target/debug/deps/webagent_v3-0123456789abcdef"])
        metadata.write_text(json.dumps({"reason":"compiler-artifact","executable":"/output/target/debug/../../etc/passwd"}))
        with self.assertRaises(ValueError):executor.Executor.sidekick_test_files(metadata)
    def test_fetch_accepts_exactly_the_requested_files(self):
        w=worker();job={"job_id":"0"*8+"-0000-0000-0000-"+"0"*12}
        def reply(files):
            def call(settings,*words,stdin=None,timeout=0,output_file=None):output_file.write(tar(files).getvalue());return""
            return call
        w.sidekick_call=reply({"target/debug/a":b"a"});w.sidekick_fetch(SETTINGS,job,self.output,["target/debug/a"])
        self.assertEqual((self.output/"target/debug/a").read_bytes(),b"a")
        w.sidekick_call=reply({"target/debug/a":b"a","target/debug/b":b"b"})
        with self.assertRaises(ValueError):w.sidekick_fetch(SETTINGS,job,self.output,["target/debug/a"])
        self.assertEqual([p.name for p in self.output.iterdir()if p.suffix==".tar"],[])
class HalfTests(unittest.TestCase):
    """The sidekick half with its remote verbs and local steps replaced by recorders."""
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.output=Path(self.temp.name)/"output";self.app=Path(self.temp.name)/"app"
        (self.output/"cargo/registry").mkdir(parents=True);self.app.mkdir();(self.app/"main.rs").write_text("x")
        self.job={"job_id":"1"*8+"-0000-0000-0000-"+"0"*12,"dependency_cache":{"key":"a"*64}}
        self.w=worker();self.calls=[];self.staged=None
        manifest={"tests":[{"file":"tests/t-0123456789abcdef","sha256":"f"*64}]}
        self.w.sidekick_fetch=lambda settings,job,output,names:(output/"test-artifacts.json").write_text(json.dumps({"reason":"compiler-artifact","executable":"/output/target/debug/deps/t-0123456789abcdef","package_id":"p","profile":{"test":True}}))
        self.w.retain_test_manifest=lambda output,artifacts:manifest
        self.w.linked_reference=lambda job,app,output,state:state.update(linked="l",proof={"ok":1})or""
        self.w.test_selection=lambda job,blob,app,output,manifest,run_all=False:{"argv":["suite"],"run":[],"skipped":[]}
        self.w.early_suite_argv=lambda output,rows:["suite"]
        self.suite_error=None;self.code="old"
        def phase(job,phase,*args,sidekick=None,**kw):
            self.calls.append(("phase",phase,sidekick is not None))
            if phase=="test"and self.suite_error:raise self.suite_error
            return phase+" log\n"
        self.w.phase=phase
        self.w.sidekick_program=lambda:b"program"
        self.released=[];self.w.sidekick_release=lambda settings,job,result:self.released.append(result)
    def tearDown(self):self.temp.cleanup()
    def call(self,settings,*words,stdin=None,timeout=0,output_file=None):
        self.calls.append(words[0])
        if words[0]=="begin":return json.dumps({"decision":self.begin})
        if words[0]=="status":return json.dumps({"images":["sha256:"+"7"*64],"version":executor.SIDEKICK_PROTOCOL,"accepting":self.accepting,"code_sha256":self.code})
        if words[0]=="update":self.code=words[1];return json.dumps({"updated":words[1]})
        if words[0]=="sync-source":return json.dumps({"sha256":executor.Executor.sidekick_tree(self.app)[1],"changed":1})
        if words[0]=="has-cargo":return json.dumps({"present":True})
        if words[0]=="stage":return json.dumps(self.staged or {"tests/t-0123456789abcdef":"f"*64})
        raise AssertionError(words)
    def run_half(self,begin="accepted",staged=None,accepting=True,state=None):
        self.begin=begin;self.staged=staged;self.accepting=accepting;self.w.sidekick_call=self.call;state={}if state is None else state
        self.w.sidekick_claim(self.job,SETTINGS,state)
        return self.w.sidekick_test_half(self.job,b"blob",self.app,self.output,SETTINGS,state),state
    def test_passing_half_runs_both_phases_remotely_in_order(self):
        logs,state=self.run_half()
        self.assertIn("broker sidekick",logs)
        verbs=[c for c in self.calls if isinstance(c,str)]
        # The release's own sidekick.py is pushed when the sidekick runs another version.
        self.assertEqual(verbs,["status","update","status","begin","status","sync-source","has-cargo","stage"])
        self.calls.clear();self.run_half()
        self.assertNotIn("update",[c for c in self.calls if isinstance(c,str)])
        self.assertEqual([c for c in self.calls if isinstance(c,tuple)],[("phase","compile_test",True),("phase","test",True)])
        self.assertEqual(self.job["sidekick"]["decision"],"passed");self.assertEqual(self.job["focused_verification"]["status"],"not_applicable")
        self.assertIn("selection",state)
    def test_busy_sidekick_or_changed_binaries_stop_the_half_for_a_local_run(self):
        with self.assertRaises(executor.SidekickDeclined):self.run_half(begin="busy")
        with self.assertRaises(executor.SidekickDeclined):self.run_half(accepting=False)
        self.calls.clear()
        with self.assertRaisesRegex(ValueError,"differ from the retained manifest"):self.run_half(staged={"tests/t-0123456789abcdef":"0"*64})
        # The early suite may have started; its result never counts and the sidekick is cancelled.
        self.assertIn("cancelled",self.released)
    def test_a_disconnect_keeps_the_steps_already_passed(self):
        script="set -e\ncargo test --workspace --locked --offline --doc -- --test-threads=8\nexport X=1\ncd /build && /output/artifacts/tests/a-0123456789abcdef --test-threads=8\ncd /build/crates/b && /output/artifacts/tests/b-0123456789abcdef --test-threads=8"
        self.w.test_selection=lambda *a,**k:{"argv":["sh","-c",script],"run":[],"skipped":[]}
        self.w.early_suite_argv=lambda output,rows:["sh","-c",script]
        error=RuntimeError("fixed command failed: doc ok\n"+executor.SUITE_MARK+"doctests\ntest result: ok. 3 passed; 0 failed\n"+executor.SUITE_MARK+"tests/a-0123456789abcdef\nb half output");error.exit_code=255
        self.suite_error=error;state={}
        with self.assertRaises(RuntimeError):self.run_half(state=state)
        self.assertEqual(state["done"],["doctests","tests/a-0123456789abcdef"]);self.assertNotIn("b half",state["partial"])
        rest=executor.Executor.suite_remainder(state["selection"],state["done"])[2]
        self.assertNotIn("--doc",rest);self.assertNotIn("tests/a-",rest);self.assertIn("tests/b-",rest);self.assertIn("export X=1",rest)
class EarlySuiteTests(unittest.TestCase):
    def test_the_early_script_equals_the_selection_the_manifest_gives(self):
        with tempfile.TemporaryDirectory()as d:
            out=Path(d);w=worker()
            (out/"dependency-metadata.json").write_text(json.dumps({"packages":[{"id":"p","source":None,"manifest_path":"/build/crates/x/Cargo.toml"}]}))
            rows=[{"file":"tests/x-0123456789abcdef","package_id":"p"}]
            early=w.early_suite_argv(out,rows)
            run=[{"file":"tests/x-0123456789abcdef","directory":"/build/crates/x"}]
            self.assertEqual(early,executor.Executor.suite_script(run,[],"--test-threads=8"))
            metadata=out/"t.json"
            metadata.write_text("\n".join(json.dumps(r)for r in [{"reason":"compiler-artifact","executable":"/output/target/debug/deps/x-0123456789abcdef","package_id":"p","profile":{"test":True}},
                {"reason":"compiler-artifact","executable":"/output/target/debug/webagent-mcp","package_id":"p","profile":{"test":False}}]))
            self.assertEqual(executor.Executor.sidekick_test_rows(metadata),[{"file":"tests/x-0123456789abcdef","package_id":"p","source":"target/debug/deps/x-0123456789abcdef"}])
class StagePlaceTests(unittest.TestCase):
    def test_sidekick_phases_get_their_stage_time_and_label(self):
        import test_admin_build as fixtures
        host=fixtures.host
        job={"status":"completed","phase_timings":[
            {"phase":"compile_test","seconds":11,"where":"sidekick","started_at":100},
            {"phase":"test","seconds":130,"where":"sidekick","started_at":120}],
            "execution_history":[{"phase":"compile_release","started_at":100,"elapsed_seconds":17}]}
        spans=host.stage_spans(job)
        self.assertEqual(spans["compile tests"],[100,111]);self.assertEqual(spans["run tests"],[120,250])
        self.assertEqual(host.stage_places(job),{"compile tests":"sidekick","run tests":"sidekick"})
        job["execution_history"].append({"phase":"test","started_at":260,"elapsed_seconds":20})
        self.assertEqual(host.stage_places(job)["run tests"],"sidekick → server")
    def test_a_running_replay_is_judged_as_a_replay_once_packaging_starts(self):
        import test_admin_build as fixtures
        job={"operation":"build_deploy","status":"running","started_at":0,
             "phase_timings":[{"phase":"packaging_api","started_at":10,"seconds":30,"outcome":"completed"}],
             "active_phases":[{"phase":"test","started_at":5}]}
        self.assertTrue(fixtures.host.job_progress(job,{},100)["class"].startswith("build_deploy/artifacts-reused/"))
class BootstrapTests(unittest.TestCase):
    def test_update_checks_the_hash_and_keeps_previous_programs(self):
        import sys,hashlib
        with tempfile.TemporaryDirectory()as d:
            bootstrap.CODE=Path(d)/"code";body=b"VERSION=3\n"
            self.assertEqual(bootstrap.current(),bootstrap.FALLBACK)
            for content,ok in ((body,True),(b"other",False)):
                stdin=sys.stdin;sys.stdin=types.SimpleNamespace(buffer=io.BytesIO(content))
                try:
                    if ok:bootstrap.update(hashlib.sha256(body).hexdigest())
                    else:
                        with self.assertRaises(SystemExit):bootstrap.update(hashlib.sha256(body).hexdigest())
                finally:sys.stdin=stdin
            self.assertEqual(bootstrap.current().read_bytes(),body)
class ConfigTests(unittest.TestCase):
    def test_enabled_sidekick_needs_protected_key_paths(self):
        source=(SERVICE/"config.py").read_text()
        self.assertIn('"remote_tests","sidekick"}-RESOURCE_KEYS-TUNING_KEYS',source)
        self.assertEqual(json.loads((SERVICE/"scheduler-defaults.json").read_text())["sidekick"]["enabled"],False)
        self.assertIn('"sidekick.py"',(SERVICE/"install.py").read_text())
if __name__=="__main__":unittest.main()
