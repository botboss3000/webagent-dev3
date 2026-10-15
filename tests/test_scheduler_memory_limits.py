"""Resource ceilings, authority fences, exact retries and protected Docker effects."""
import copy,hashlib,importlib.util,json,tempfile,threading,time,unittest,uuid
from pathlib import Path
from unittest.mock import patch

def module(name):
    """Load the paired broker module; draft verification replaces only declared source bodies."""
    spec=importlib.util.spec_from_file_location("memory_"+name,Path(__file__).parents[1]/"build-service"/(name+".py"))
    m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m

resources=module("resources");control=module("resource_control");config=module("config");executor=module("executor");lifecycle=module("lifecycle")
G=1024**3
POLICY={"mode":"enforce","reserve_percent":15,"reserve_bytes":768*1024**2,"compiler_memory_default_bytes":5*G,"compiler_memory_min_bytes":G,"compiler_memory_max_bytes":10*G,"allow_live_memory_increase":True}

class Kernel:
    """Model limits independently of broker reservations and deliberately lose acknowledgments."""
    def __init__(self):
        self.cap=5*G;self.calls=[];self.failure=None;self.unavailable=False
    def resource_request(self,args):
        """A failed reply may follow a real effect; stats are the independent evidence."""
        self.calls.append(dict(args));operation=args["operation"]
        if operation=="initialize":return {"admitted":True,"memory_budget":35*G}
        if operation=="memory":
            if self.failure=="before":raise RuntimeError("effect rejected")
            self.cap=args["memory_bytes"]
            if self.failure in ("reply","unknown"):raise RuntimeError("reply lost")
        if self.unavailable or operation=="stats"and self.failure=="unknown":raise RuntimeError("stats unavailable")
        return {"container_id":"c"*64,"memory_max":self.cap,"swap_max":0,"memory":G,"peak_memory":G,"oom_events":0,"oom_kills":0,"frozen":False}
    def cleanup_phase(self,name):
        """Confirm stopped exact-container cleanup without creating another phase."""
        self.calls.append({"operation":"cleanup","container":name})

class MemoryTests(unittest.TestCase):
    """Full ceilings and atomic growth must survive all resource-control response orders."""
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.kernel=Kernel();self.sample={"available":30*G,"total":40*G,"swap_free":0,"swap_total":0,"pressure":{"cpu":0,"memory":0,"io":0}}
        self.r=resources.Resources(self.tmp.name,self.kernel,copy.deepcopy(POLICY),sampler=lambda:copy.deepcopy(self.sample))
        self.job={"job_id":str(uuid.uuid4()),"status":"running","lease_until":time.time()+120,"task_id":"memory-fixture"}
        self.r.enter("one",self.job,"compile_test","memory-test","engine",hard_cap=5*G);self.r.attached("one","c"*64)
        self.r.rows["one"]["metrics"]={"memory":G};self.pid=hashlib.sha256(b"one").hexdigest()
    def tearDown(self):
        self.r.close();self.tmp.cleanup()
    def request(self,cap=6*G,request="memory-increase-fixture",revision=None):
        """Bind each request to the observed phase revision and exact desired limit."""
        return {"action":"increase_memory","phase_id":self.pid,"expected_revision":self.r.rows["one"]["revision"]if revision is None else revision,"memory_bytes":cap,"request_id":request}
    def test_full_ceiling_overrides_low_historical_peak(self):
        self.r.profiles["memory-test"]=[64*1024**2]
        self.assertEqual(self.r.rows["one"]["estimate"],5*G)
        self.assertGreaterEqual(self.r.reserve(),6*G)
    def test_success_reserves_growth_and_reads_exact_kernel_limit(self):
        result=self.r.control(self.job,self.request())
        self.assertEqual(result["state"],"applied");self.assertEqual(self.r.rows["one"]["estimate"],6*G)
        self.assertEqual(self.kernel.cap,6*G);self.assertEqual(self.r.rows["one"]["hard_cap"],6*G)
        effect=next(a for a in self.kernel.calls if a["operation"]=="memory")
        self.assertEqual(effect["reserved_bytes"],6*G);self.assertEqual(effect["policy_sha256"],self.r.memory_policy()["sha256"])
    def test_exact_retry_never_repeats_effect(self):
        args=self.request();first=self.r.control(self.job,args);self.assertEqual(first,self.r.control(self.job,args))
        self.assertEqual(sum(a["operation"]=="memory"for a in self.kernel.calls),1)
        with self.assertRaisesRegex(ValueError,"reused"):self.r.control(self.job,{**args,"memory_bytes":7*G})
    def test_insufficient_growth_preserves_old_limit_and_returns_wait(self):
        self.sample["available"]=10*G # six reserved for host, four remaining on this compiler
        args=self.request();result=self.r.control(self.job,args)
        self.assertEqual(result["state"],"waiting_headroom");self.assertEqual(self.kernel.cap,5*G)
        self.assertEqual(self.r.rows["one"]["estimate"],5*G)
        self.sample["available"]=30*G;self.assertEqual(result,self.r.control(self.job,args))
        self.assertEqual(self.r.control(self.job,self.request(request="memory-growth-after-wait"))["state"],"applied")
    def test_other_reservations_are_charged_before_growth(self):
        self.r.rows["other"]={**self.r.rows["one"],"estimate":10*G,"metrics":{"memory":0},"component":"other"}
        self.sample["available"]=20*G
        result=self.r.control(self.job,self.request())
        self.assertEqual(result["state"],"waiting_headroom")
    def test_lost_reply_reconciles_new_kernel_limit_without_retry(self):
        self.kernel.failure="reply";result=self.r.control(self.job,self.request())
        self.assertEqual(result["state"],"applied");self.assertEqual(self.r.rows["one"]["estimate"],6*G)
        self.assertEqual(sum(a["operation"]=="memory"for a in self.kernel.calls),1)
    def test_rejected_effect_releases_only_extra_reservation(self):
        self.kernel.failure="before";result=self.r.control(self.job,self.request())
        self.assertEqual(result["state"],"not_applied");self.assertEqual(self.r.rows["one"]["estimate"],5*G)
    def test_unknown_effect_keeps_full_reservation_and_blocks_admission(self):
        self.kernel.failure="unknown";args=self.request();result=self.r.control(self.job,args)
        self.assertEqual(result["state"],"uncertain");self.assertEqual(self.r.rows["one"]["estimate"],6*G)
        self.assertFalse(self.r.can_admit(G));self.assertEqual(self.r.rows["one"]["state"],"memory_recovery_required")
        self.kernel.failure=None;result=self.r.control(self.job,args)
        self.assertEqual(result["state"],"applied");self.assertEqual(sum(a["operation"]=="memory"for a in self.kernel.calls),1)
    def test_restart_retains_unknown_ceiling_until_exact_cleanup(self):
        self.kernel.failure="unknown";self.r.control(self.job,self.request())
        second=resources.Resources(self.tmp.name,self.kernel,copy.deepcopy(POLICY),sampler=lambda:copy.deepcopy(self.sample))
        try:
            row=second.rows["one"];self.assertEqual(row["state"],"recovery_required");self.assertEqual(row["estimate"],6*G);self.assertFalse(second.can_admit(G))
            stopped={**self.job,"status":"interrupted"}
            second.control(stopped,{"action":"reconcile","phase_id":self.pid,"expected_revision":row["revision"],"request_id":"memory-orphan-cleanup"})
            self.assertEqual(second.rows["one"]["state"],"failed")
        finally:second.close()
    def test_two_racing_requests_have_one_cas_winner(self):
        barrier=threading.Barrier(2);results=[];errors=[]
        def grow(cap,request):
            """Race independent requests against the same captured phase revision."""
            barrier.wait()
            try:results.append(self.r.control(self.job,self.request(cap,request,revision=0)))
            except ValueError as error:errors.append(error)
        threads=[threading.Thread(target=grow,args=(cap,"memory-race-"+str(n)))for n,cap in enumerate((6*G,7*G))]
        for thread in threads:thread.start()
        for thread in threads:thread.join(3);self.assertFalse(thread.is_alive())
        self.assertEqual(len(results),1);self.assertEqual(len(errors),1)
        self.assertEqual(sum(a["operation"]=="memory"for a in self.kernel.calls),1)
    def test_revocation_expiry_and_stale_revision_prevent_effect(self):
        for job,args in [({**self.job,"revoked":True},self.request()),({**self.job,"lease_until":0},self.request()),(self.job,self.request(revision=99))]:
            with self.assertRaises((ValueError,PermissionError)):self.r.control(job,args)
        self.assertFalse(any(a["operation"]=="memory"for a in self.kernel.calls))
    def test_limits_reject_bool_alignment_shrink_and_noncompiler(self):
        for cap in (True,6*G+1,11*G,512*1024**2,4*G,5*G):
            with self.assertRaises((ValueError,PermissionError)):self.r.control(self.job,self.request(cap))
        self.r.rows["one"]["phase"]="test"
        with self.assertRaises(ValueError):self.r.control(self.job,self.request())
    def test_policy_disable_and_cas_leave_existing_limits_unchanged(self):
        current=self.r.memory_policy();policy={**current["values"],"compiler_memory_default_bytes":7*G}
        with self.assertRaises(ValueError):self.r.adopt_memory_policy(policy,"stale")
        self.r.adopt_memory_policy(policy,current["sha256"])
        self.assertEqual(self.r.rows["one"]["hard_cap"],5*G)
        self.r.policy["allow_live_memory_increase"]=False
        with self.assertRaises(PermissionError):self.r.control(self.job,self.request())
    def test_terminal_receipt_budget_never_evicts_unresolved_intent(self):
        self.r.controls={"unknown":{"result":{"state":"uncertain"}}}
        self.r.controls.update({str(n):{"result":{"state":"applied"}}for n in range(1100)})
        self.r.bound_controls();self.assertEqual(len(self.r.controls),1024);self.assertIn("unknown",self.r.controls)

class PolicyTests(unittest.TestCase):
    """Protected JSON bounds never become arbitrary runtime arguments."""
    def test_source_defaults_are_five_gib_and_overrides_are_bounded(self):
        values=config.scheduler_defaults();self.assertEqual(values["compiler_memory_default_bytes"],5*G)
        self.assertEqual(executor.compiler_cap(8*G,values),8*G)
        for value in (True,0,512*1024**2,10*G+4096,5*G+1):
            with self.assertRaises(ValueError):executor.compiler_cap(value,values)
    def test_package_ceiling_is_not_changed_by_compiler_bounds(self):
        self.assertEqual(executor.package_cap(4026531840),4026531840)
        with self.assertRaises(ValueError):executor.package_cap(5*G)
    def test_config_rejects_invalid_policy_and_reserve_bypass(self):
        base=config.defaults()
        for patch_policy in ({"compiler_memory_default_bytes":True},{"compiler_memory_default_bytes":11*G},{"compiler_memory_min_bytes":6*G},{"compiler_memory_max_bytes":4*G},{"allow_live_memory_increase":True},{"compiler_memory_max_bytes":10*G+1}):
            with self.subTest(policy=patch_policy):
                with self.assertRaises(ValueError):config.validate({**base,"scheduler":{**base["scheduler"],**patch_policy}})
    def test_lifecycle_rejects_nonroot_and_nonmemory_changes(self):
        with patch.object(lifecycle.os,"geteuid",return_value=1000):
            with self.assertRaises(PermissionError):lifecycle.resource_policy("a","b","memory-test")
        with patch.object(lifecycle.os,"geteuid",return_value=0),patch.object(lifecycle.cfg,"load",side_effect=[{"scheduler":POLICY,"api_container":"before"},{"scheduler":POLICY,"api_container":"after"}]):
            with self.assertRaises(ValueError):lifecycle.resource_policy("a","b","memory-test")
    def test_lifecycle_dry_run_does_not_write_or_restart(self):
        data={"scheduler":copy.deepcopy(POLICY),"api_container":"fixed"}
        candidate={**data,"scheduler":{**POLICY,"compiler_memory_default_bytes":6*G}}
        with patch.object(lifecycle.os,"geteuid",return_value=0),patch.object(lifecycle.cfg,"load",side_effect=[data,candidate]),patch.object(lifecycle,"atomic")as write,patch.object(lifecycle.subprocess,"run")as run:
            result=lifecycle.resource_policy("a","b","memory-test",True)
            self.assertFalse(result["restart_required"]);write.assert_not_called();run.assert_not_called()

class RootMemoryTests(unittest.TestCase):
    """Docker API writes require exact identity, current policy, authority and kernel proof."""
    def setUp(self):
        self.c=control.Control({"instance":"development","scheduler":copy.deepcopy(POLICY)})
        self.identity="c"*64;self.cap=5*G;self.swap=0;self.updates=[]
        self.group=Path("/fake-exact-group")
        self.policy=resources.Resources.memory_policy(type("Policy",(),{"policy":POLICY})())
        self.args={"operation":"memory","container":"fixed","expected_id":self.identity,"memory_bytes":6*G,"previous_bytes":5*G,"reserved_bytes":6*G,"policy_sha256":self.policy["sha256"],"lease_until":time.time()+60}
        self.patches=[patch.object(control.os,"geteuid",return_value=0),patch.object(self.c,"resolve",return_value=(self.group,"compile_test",self.identity)),patch.object(self.c,"memory_policy",return_value=self.policy),patch.object(self.c,"initialize",return_value={"admitted":True}),patch.object(control,"host_memory",return_value={"MemTotal":40*G,"MemAvailable":30*G}),patch.object(self.c,"stats",side_effect=lambda _group:{"memory_max":self.cap,"swap_max":self.swap})]
        for p in self.patches:p.start()
        parent=self
        class Docker:
            """Record only the exact Docker memory update body; emulate the kernel independently."""
            def __init__(self,*args,**kwargs):pass
            def request(self,method,path,body,headers):
                parent.updates.append((method,path,json.loads(body)));parent.cap=json.loads(body)["Memory"]
            def getresponse(self):
                return type("Reply",(),{"status":200,"read":lambda self,_n:b"{}"})()
            def close(self):pass
        self.docker=patch.object(control,"Docker",Docker);self.docker.start()
    def tearDown(self):
        self.docker.stop()
        for p in reversed(self.patches):p.stop()
    def test_only_exact_container_memory_and_zero_swap_are_updated(self):
        result=self.c.handle(self.args)
        self.assertEqual(result["memory_max"],6*G)
        self.assertEqual(self.updates,[("POST","/containers/"+self.identity+"/update",{"Memory":6*G,"MemorySwap":6*G})])
    def test_wrong_identity_policy_expiry_and_shrink_have_no_effect(self):
        for change in ({"expected_id":"d"*64},{"policy_sha256":"0"*64},{"lease_until":0},{"memory_bytes":4*G},{"memory_bytes":6*G+1}):
            with self.assertRaises((PermissionError,ValueError)):self.c.handle({**self.args,**change})
        self.assertEqual(self.updates,[])
    def test_noncompiler_swap_and_aggregate_shortfall_are_rejected(self):
        with patch.object(self.c,"resolve",return_value=(self.group,"test",self.identity)):
            with self.assertRaises(PermissionError):self.c.handle(self.args)
        self.swap=1
        with self.assertRaises(PermissionError):self.c.handle(self.args)
        self.swap=0
        with patch.object(self.c,"initialize",return_value={"admitted":False}):
            with self.assertRaises(PermissionError):self.c.handle(self.args)
        self.assertEqual(self.updates,[])
    def test_target_change_before_update_is_rejected(self):
        with patch.object(self.c,"resolve",side_effect=[(self.group,"compile_test",self.identity),(self.group,"compile_test","d"*64)]):
            with self.assertRaises(PermissionError):self.c.handle(self.args)
        self.assertEqual(self.updates,[])
    def test_host_reserve_is_not_spent_on_growth(self):
        with patch.object(control,"host_memory",return_value={"MemTotal":40*G,"MemAvailable":6*G}):
            with self.assertRaises(PermissionError):self.c.handle(self.args)
        self.assertEqual(self.updates,[])

class BrokerMemoryTests(unittest.TestCase):
    """Check default pinning and operator probes through actual broker authority routes."""
    def setUp(self):
        import test_admin_build as fixtures
        self.f=fixtures;self.tmp=tempfile.TemporaryDirectory();self.sid=str(uuid.uuid4())
        self.config=patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"scheduler":{**fixtures.host.CONFIG["scheduler"],**POLICY}})
        self.config.start();self.runner=fixtures.FakeRunner();self.b=fixtures.host.Broker(Path(self.tmp.name)/"jobs",self.runner)
        self.b.resources.enabled=True;self.b.stopping=True
        self.trusted={"user_id":"codex-local","session_id":self.sid,"local_caller":True,"lease_until":time.time()+120,"access_fingerprint":"f"*64}
        self.call("build_work",{"action":"begin","task_id":"memory-broker-task","summary":"Memory fixture"})
        self.digest=self.call("build_work",{"action":"review","task_id":"memory-broker-task"})["workspace_inputs_sha256"]
    def tearDown(self):
        self.b.shutdown();self.config.stop();self.tmp.cleanup()
    def call(self,name,args):
        """Use the actual trusted operator envelope without a model-selected identity."""
        return self.b.handle({"name":name,"arguments":args,"workspace":self.sid,"trusted":self.trusted})
    def request(self,**kwargs):
        """Queue an unchanged-input diagnostic probe; no engine build can start."""
        return self.call("build_work",{"action":"check","check":"resource_probe","task_id":"memory-broker-task","expected_inputs_sha256":self.digest,"request_id":"memory-probe-default","timeout_seconds":90,**kwargs})
    def test_default_is_pinned_and_retry_survives_default_reload(self):
        first=self.request();job=self.b.jobs[first["job_id"]];self.assertEqual(job["compiler_memory_cap"],5*G)
        self.f.host.CONFIG["scheduler"]["compiler_memory_default_bytes"]=6*G
        second=self.request();self.assertEqual(second["job_id"],first["job_id"]);self.assertEqual(job["compiler_memory_cap"],5*G)
    def test_override_does_not_acquire_engine_claim(self):
        first=self.request(compiler_memory_cap=8*G);job=self.b.jobs[first["job_id"]]
        self.assertEqual(job["compiler_memory_cap"],8*G)
        row=self.b.scheduler.store.get(job["ownership_id"]);self.assertIn("module:broker",row["scopes"]);self.assertNotIn("module:engine",row["scopes"])
    def test_nonoperator_probe_and_override_are_denied(self):
        self.trusted={**self.trusted,"local_caller":False}
        with self.assertRaises(PermissionError):self.request()


class CompletionEvidenceTests(unittest.TestCase):
    """Final attestation failures must release the exact container without masking workload failures."""
    def run_phase(self,final,primary=None):
        from unittest.mock import Mock
        from collections import defaultdict
        import types
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        identity="a"*64;cap=5*G
        rows=defaultdict(dict)
        r=types.SimpleNamespace(enabled=True,enter=Mock(),attached=Mock(),lock=threading.RLock(),rows=rows,diagnostic_check=lambda _:False)
        stats={"container_id":identity,"memory_max":cap,"swap_max":0,"oom_events":0,"oom_kills":0,"peak_memory":1234}
        runner=types.SimpleNamespace(command=Mock(side_effect=["created","started",primary if primary else "compiled"]),job_context=types.SimpleNamespace(),resource_progress=Mock())
        runner.resource_request=Mock(side_effect=[{"container_id":identity,"state":"created","phase":"compile_test"},stats,final if isinstance(final,Exception)else {**stats,**final}])
        worker=executor.Executor(runner,r,{"instance":"development","service_uid":1000,"service_gid":1000});worker.finish_phase=Mock()
        job={"job_id":str(uuid.uuid4()),"compiler_memory_cap":cap}
        args=(job,"compile_test","engine",Path(temporary.name),Path(temporary.name)/"out",["cargo","check"],"memory-fixture",10)
        return worker,job,args
    def test_lost_completion_stats_still_cleans_up_and_cannot_pass(self):
        worker,job,args=self.run_phase(RuntimeError("stats unavailable"))
        with self.assertRaisesRegex(RuntimeError,"completion evidence"):worker.phase(*args)
        worker.finish_phase.assert_called_once()
        self.assertFalse(worker.finish_phase.call_args.args[2])
        self.assertEqual(job["compiler_cap_outcomes"][0]["outcome"],"failed")
    def test_changed_identity_cap_and_swap_are_rejected_after_cleanup(self):
        for change in ({"container_id":"b"*64},{"memory_max":6*G},{"swap_max":1}):
            with self.subTest(change=change):
                worker,job,args=self.run_phase(change)
                with self.assertRaisesRegex(RuntimeError,"completion evidence"):worker.phase(*args)
                worker.finish_phase.assert_called_once();self.assertFalse(worker.finish_phase.call_args.args[2])
    def test_workload_failure_survives_missing_completion_evidence_and_cleanup_failure(self):
        worker,job,args=self.run_phase(RuntimeError("stats unavailable"),primary=RuntimeError("original compiler error"))
        worker.finish_phase.side_effect=RuntimeError("cleanup failed")
        with self.assertRaisesRegex(RuntimeError,"original compiler error"):worker.phase(*args)
        worker.finish_phase.assert_called_once()
    def test_success_records_final_limit_peak_and_oom_counters(self):
        worker,job,args=self.run_phase({})
        self.assertEqual(worker.phase(*args),"compiled")
        self.assertTrue(worker.finish_phase.call_args.args[2])
        result=job["compiler_cap_outcomes"][0]
        self.assertEqual((result["final_memory_max"],result["peak_memory"],result["oom_kills"]),(5*G,1234,0))

class PolicyRecoveryTests(unittest.TestCase):
    """A lost acknowledgment reconciles the durable receipt before another target is admitted."""
    def test_same_target_retry_settles_older_receipt_and_other_target_waits(self):
        import types
        with tempfile.TemporaryDirectory()as temporary:
            path=Path(temporary)/"config.json";candidate=Path(temporary)/"candidate.json"
            data=config.defaults();data.update(service_account="fixture",service_dir="/fixed")
            data["scheduler"].update(POLICY);desired=config.memory_policy_digest(data["scheduler"])
            path.write_text(json.dumps(data));candidate.write_text(json.dumps(data))
            history=Path(temporary)/"resource-policy";history.mkdir()
            old=history/"prior.json";old.write_text(json.dumps({"status":"needs_reconciliation","policy_sha256":desired}))
            def write(target,value):Path(target).write_text(json.dumps(value))
            replies=[json.dumps({"memory_policy":{"sha256":desired}}).encode(),json.dumps({"sha256":desired}).encode()]
            with patch.object(lifecycle.os,"geteuid",return_value=0),patch.object(lifecycle.os,"fstat",return_value=types.SimpleNamespace(st_uid=0,st_nlink=1,st_mode=0o600)),patch.object(lifecycle,"secure_dirs"),patch.object(lifecycle.cfg,"load",return_value=data),patch.object(lifecycle,"atomic",side_effect=write),patch.object(lifecycle.subprocess,"check_output",side_effect=replies):
                result=lifecycle.resource_policy(path,candidate,"memory-policy-fixture")
            self.assertEqual(json.loads(old.read_text())["status"],"reconciled")
            self.assertEqual(json.loads(old.read_text())["reconciled_by"],result["receipt_id"])
            old.write_text(json.dumps({"status":"needs_reconciliation","policy_sha256":"0"*64}))
            with patch.object(lifecycle.os,"geteuid",return_value=0),patch.object(lifecycle.os,"fstat",return_value=types.SimpleNamespace(st_uid=0,st_nlink=1,st_mode=0o600)),patch.object(lifecycle,"secure_dirs"),patch.object(lifecycle.cfg,"load",return_value=data),patch.object(lifecycle.subprocess,"check_output",return_value=replies[0]),patch.object(lifecycle,"atomic")as mutate:
                with self.assertRaisesRegex(ValueError,"Reconcile prior"):lifecycle.resource_policy(path,candidate,"memory-policy-fixture")
                mutate.assert_not_called()

class AggregateBudgetTests(unittest.TestCase):
    """The root pool cannot admit full ceilings by relying on low observed RSS."""
    def test_pool_expansion_accounts_current_rss_and_preserves_host_reserve(self):
        with tempfile.TemporaryDirectory()as temporary:
            pool=Path(temporary);(pool/"memory.current").write_text(str(2*G));(pool/"memory.max").write_text(str(4*G))
            c=control.Control({"instance":"development","scheduler":copy.deepcopy(POLICY)})
            with patch.object(c,"pool",return_value=pool),patch.object(control,"host_memory",return_value={"MemTotal":40*G,"MemAvailable":20*G,"SwapTotal":8*G}),patch.object(control.subprocess,"run")as command:
                result=c.initialize(8*G)
            self.assertTrue(result["admitted"]);self.assertEqual(result["memory_budget"],16*G)
            args=command.call_args.args[0];self.assertIn("MemoryMax="+str(16*G),args)
            self.assertIn("MemorySwapMax="+str(4*G),args)
    def test_insufficient_pool_capacity_never_changes_limit(self):
        with tempfile.TemporaryDirectory()as temporary:
            pool=Path(temporary);(pool/"memory.current").write_text(str(2*G));(pool/"memory.max").write_text(str(10*G))
            c=control.Control({"instance":"development","scheduler":copy.deepcopy(POLICY)})
            with patch.object(c,"pool",return_value=pool),patch.object(control,"host_memory",return_value={"MemTotal":40*G,"MemAvailable":7*G}),patch.object(control.subprocess,"run")as command:
                result=c.initialize(8*G)
            self.assertFalse(result["admitted"]);self.assertEqual(command.call_count,1)
            self.assertEqual(command.call_args.args[0][:2],["systemctl","start"])

class SupervisorLoadingTests(unittest.TestCase):
    """The protected supervisor exec namespace intentionally has no __file__."""
    def test_policy_reload_uses_protected_service_directory_under_supervisor_exec(self):
        body=(Path(__file__).parents[1]/"build-service/resource_control.py").read_text()
        namespace={"__name__":"protected_build_resources"}
        exec(compile(body,"protected-resource-control.py","exec"),namespace)
        with tempfile.TemporaryDirectory()as temporary:
            policy={"instance":"development","service_dir":temporary,"scheduler":copy.deepcopy(POLICY)}
            stub="MEMORY_KEYS="+repr(set(config.MEMORY_KEYS))+"\ndef load(path):return "+repr(policy)+"\n"
            (Path(temporary)/"config.py").write_text(stub)
            instance=namespace["Control"](copy.deepcopy(policy))
            with patch.dict(namespace["os"].environ,{"WEBAGENT_BUILD_CONFIG":"/protected/fixed.json"}):
                result=instance.memory_policy()
            self.assertEqual(result["values"],config.memory_policy(POLICY))
            self.assertEqual(result["sha256"],config.memory_policy_digest(POLICY))

class LedgerCapacityTests(unittest.TestCase):
    """A protected storage ceiling resolves saturation without disabling the guard."""
    def test_default_and_installation_bounds(self):
        self.assertEqual(config.defaults()["code_index_ledger_max_bytes"],768*1024**2)
        for value in (512*1024**2,768*1024**2,1024**3):
            self.assertEqual(config.validate({**config.defaults(),"code_index_ledger_max_bytes":value})["code_index_ledger_max_bytes"],value)
        for value in (True,0,512*1024**2-4096,1024**3+4096,768*1024**2+1):
            with self.subTest(value=value),self.assertRaises(ValueError):config.validate({**config.defaults(),"code_index_ledger_max_bytes":value})
    def test_exhaustion_still_rejects_transaction_and_preserves_prior_state(self):
        import importlib.util
        spec=importlib.util.spec_from_file_location("memory_ledger",Path(__file__).parents[1]/"plugins/abilities/Administrator/code_index/maintenance.py")
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory()as temporary:
            ledger=module.Ledger(temporary,max_bytes=1024*1024)
            with ledger.connect()as db:db.execute("CREATE TABLE capacity_fixture(value BLOB)")
            ledger.max_bytes=ledger.path.stat().st_size
            with self.assertRaisesRegex(ValueError,"storage budget"):
                with ledger.connect()as db:db.execute("INSERT INTO capacity_fixture VALUES(?)",(b"x"*65536,))
            with ledger.connect()as db:self.assertEqual(db.execute("SELECT COUNT(*) FROM capacity_fixture").fetchone()[0],0)
