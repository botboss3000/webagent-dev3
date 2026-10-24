"""Deterministic pressure transitions plus protected-control negative contracts."""
import hashlib,importlib.util,tempfile,unittest,time,math
from pathlib import Path
def module(name):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).resolve().parents[1]/"build-service"/(name+".py"))
    result=importlib.util.module_from_spec(spec);spec.loader.exec_module(result);return result
m=module("resources");control=module("resource_control")
G=1024**3
class Fake:
    """Model kernel acknowledgments independently of scheduler decisions."""
    def __init__(self):self.calls=[];self.memory=G;self.swap=0;self.frozen=False
    def resource_request(self,value):
        self.calls.append(value);operation=value["operation"]
        if operation=="initialize":return{}
        if operation=="freeze":self.frozen=True
        if operation=="thaw":self.frozen=False
        if operation=="reclaim":self.memory-=min(value["bytes"],self.memory);self.swap+=value["bytes"]
        return {"container_id":"container","frozen":self.frozen,"memory":self.memory,"peak_memory":G,"swap":self.swap,"cpu_usec":10,"pids":1}
class ResourceTests(unittest.TestCase):
    def setUp(self):
        self.root=tempfile.TemporaryDirectory();self.time=100;self.sample={"available":5*G,"total":7*G,"swap_free":10*G,"swap_total":12*G,"pressure":{"cpu":0,"memory":0,"io":0}}
        self.runner=Fake();self.scheduler=m.Resources(self.root.name,self.runner,{"mode":"enforce"},lambda:self.time,lambda:dict(self.sample))
    def tearDown(self):self.scheduler.close();self.root.cleanup()
    def phase(self,name="one",phase="compile_release"):
        self.scheduler.enter(name,{"job_id":"job","lease_until":time.time()+60},phase,"engine-release","plugin")
        self.scheduler.attached(name,"container")
    def test_live_cpu_policy_rebalances_preserves_pause_and_rejects_stale(self):
        from unittest.mock import patch
        with patch.object(m.os,"cpu_count",return_value=4):
            self.phase();before=self.scheduler.resource_policy()
            policy={**before["values"],"cpu_budget_millis":2000,"cpu_priority":"broker"}
            with self.assertRaisesRegex(ValueError,"revision"):self.scheduler.adopt_resource_policy(policy,"stale")
            applied=self.scheduler.adopt_resource_policy(policy,before["sha256"])
            self.assertEqual(self.scheduler.rows["one"]["cpu_millis"],2000)
            self.sample["pressure"]["cpu"]=90;self.scheduler.tick()
            self.assertEqual(self.scheduler.cpu_budget,2);self.assertEqual(self.scheduler.cpu_decision["reason"],"broker_priority")
            self.assertEqual(self.scheduler.snapshot()["resource_policy"]["sha256"],applied["sha256"])
            self.scheduler.act("one","freeze");self.runner.calls=[]
            self.scheduler.adopt_resource_policy(policy,applied["sha256"])
            self.assertFalse(any(c["operation"]in {"cpu","thaw"}for c in self.runner.calls))
            reopened=m.Resources(self.root.name,self.runner,policy,lambda:self.time,lambda:dict(self.sample))
            self.assertEqual(reopened.resource_policy()["sha256"],applied["sha256"])
            self.assertEqual(reopened.rows["one"]["state"],"recovery_required");reopened.close()

    def test_cpu_policy_failure_can_reconcile_and_floors_cannot_be_removed(self):
        from unittest.mock import patch
        with patch.object(m.os,"cpu_count",return_value=4):
            self.phase();before=self.scheduler.resource_policy();policy={**before["values"],"cpu_budget_millis":2000}
            with patch.object(self.runner,"resource_request",side_effect=RuntimeError("lost acknowledgment")):
                with self.assertRaises(RuntimeError):self.scheduler.adopt_resource_policy(policy,before["sha256"])
            self.scheduler.adopt_resource_policy(policy,before["sha256"])
            self.assertEqual(self.scheduler.rows["one"]["cpu_millis"],2000)
            self.scheduler.host["available"]=10*G
            self.scheduler.rows["one"]["phase"]="test"
            self.scheduler.enter("two",{"job_id":"other"},"test","independent-test","test",timeout=.1)
            self.scheduler.attached("two","container")
            current=self.scheduler.resource_policy()
            with self.assertRaisesRegex(ValueError,"floors"):self.scheduler.adopt_resource_policy({**current["values"],"cpu_budget_millis":1000},current["sha256"])
            self.assertEqual(self.scheduler.resource_policy()["sha256"],current["sha256"])

    def test_concurrent_policy_updates_have_one_revision_winner(self):
        from concurrent.futures import ThreadPoolExecutor
        from unittest.mock import patch
        with patch.object(m.os,"cpu_count",return_value=4):
            before=self.scheduler.resource_policy()
            def update(budget):
                try:return self.scheduler.adopt_resource_policy({**before["values"],"cpu_budget_millis":budget},before["sha256"])["values"]["cpu_budget_millis"]
                except ValueError:return "stale"
            with ThreadPoolExecutor(max_workers=2)as pool:results=list(pool.map(update,[1000,2000]))
            self.assertEqual(results.count("stale"),1)
            self.assertIn(self.scheduler.resource_policy()["values"]["cpu_budget_millis"],results)

    def test_enforced_cap_bounds_admission_without_changing_reserve(self):
        cap=4026531840;self.scheduler.host["available"]=cap+self.scheduler.reserve()+1024
        reserve=self.scheduler.reserve();self.scheduler.enter("capped",{"job_id":"job"},"compile_test","cold-profile","engine",timeout=.01,hard_cap=cap)
        self.assertEqual(self.scheduler.rows["capped"]["estimate"],cap);self.assertEqual(self.scheduler.reserve(),reserve)
        with self.assertRaisesRegex(ValueError,"cap"):self.scheduler.enter("bad",{"job_id":"job"},"test","runtime","test",hard_cap=cap)
        self.scheduler.rows["capped"]["state"]="finished"
    def test_package_cap_keeps_measured_profile_and_reserve(self):
        cap=4026531840;profile="buildkit:v0.25.1:role-context-v2:api";self.scheduler.profiles[profile]=[3815112704]
        reserve=self.scheduler.reserve();self.scheduler.host["available"]=cap+reserve+1024
        self.scheduler.enter("package",{"job_id":"job"},"package",profile,"package",timeout=.01,hard_cap=cap)
        self.assertEqual(self.scheduler.rows["package"]["estimate"],cap);self.assertEqual(self.scheduler.profiles[profile],[3815112704]);self.assertEqual(self.scheduler.reserve(),reserve)
        self.scheduler.rows["package"]["state"]="finished"
    def test_test_profiles_migrate_by_phase_without_losing_history(self):
        self.scheduler.profile_migrations={};self.scheduler.profiles={"engine:rust1.90:test":[3*G,G]}
        self.scheduler.rows={"compiler":{"profile":"engine:rust1.90:test","phase":"compile_test","state":"finished","metrics":{"peak_memory":3*G}},"runtime":{"profile":"engine:rust1.90:test","phase":"test","state":"finished","metrics":{"peak_memory":G}},"cancelled":{"profile":"engine:rust1.90:test","phase":"test","state":"failed","metrics":{"peak_memory":4*G}}}
        self.scheduler.migrate_test_profiles();self.scheduler.persist()
        self.assertEqual(self.scheduler.profiles["engine:rust1.90:test"],[G]);self.assertEqual(self.scheduler.profiles["engine:rust1.90:compile_test"],[3*G]);self.assertEqual(self.scheduler.profiles["legacy:engine:rust1.90:test:mixed"],[3*G,G]);self.assertEqual(self.scheduler.rows["compiler"]["original_profile"],"engine:rust1.90:test")
        self.scheduler.migrate_test_profiles();self.assertEqual(self.scheduler.profiles["engine:rust1.90:compile_test"],[3*G])
    def test_waiting_progress_is_durable_and_admission_has_deadline(self):
        import json
        self.scheduler.host["available"]=G;job={"job_id":"job","lease_until":time.time()+60};path=Path(self.root.name)/"job.json";samples=[]
        def progress(value):
            samples.append(dict(value));path.write_text(json.dumps(value))
        self.runner.resource_progress=progress
        with self.assertRaisesRegex(TimeoutError,"admission"):self.scheduler.enter("one",job,"compile_test","engine:rust1.90:check","engine",timeout=.02)
        self.assertEqual(samples[0]["resource_waiting"]["phase"],"compile_test");self.assertNotIn("resource_waiting",json.loads(path.read_bytes()));self.assertEqual(self.scheduler.rows,{})
    def test_diagnostic_check_peak_calibrates_only_completed_recipe(self):
        self.phase(phase="compile_test");self.scheduler.tick();self.scheduler.finish("one",False,diagnostic_complete=True)
        self.assertEqual(self.scheduler.estimate("engine-release","engine"),math.ceil(G*1.2))
        for text in ["error: could not compile SIGKILL","error: could not compile out of memory","error: could not compile timeout","partial error:"]:
            self.assertFalse(self.scheduler.diagnostic_check(text))
        self.assertTrue(self.scheduler.diagnostic_check("error[E0308]: type mismatch; could not compile package"))
    def test_matching_durable_failed_diagnostic_receipt_restores_estimate(self):
        import json,uuid
        identity=str(uuid.uuid4());row={"job_id":identity,"phase":"compile_test","profile":"engine:rust1.90:check","state":"failed","metrics":{"peak_memory":2*G}}
        self.scheduler.rows["failed"]=row;self.scheduler.persist();(Path(self.root.name)/(identity+".json")).write_text(json.dumps({"status":"failed","log":"error[E0308]: mismatch; could not compile package"}))
        reopened=m.Resources(self.root.name,self.runner,{"mode":"enforce"},lambda:self.time,lambda:dict(self.sample))
        self.assertEqual(reopened.estimate("engine:rust1.90:check","engine"),math.ceil(2*G*1.2));reopened.close()
    def test_manual_pause_retries_and_waits_for_explicit_resume(self):
        """An operator pause stays parked despite spare headroom and survives exact-request retries."""
        self.phase();job={"job_id":"job","status":"running","lease_until":time.time()+60}
        args={"action":"pause","phase_id":hashlib.sha256(b"one").hexdigest(),"expected_revision":0,"request_id":"manual-pause-fixture"}
        paused=self.scheduler.control(job,args)
        self.assertEqual(self.scheduler.control(job,args),paused)
        self.scheduler.tick();self.time+=30;self.scheduler.tick();self.assertTrue(self.runner.frozen)
        self.assertEqual(self.scheduler.snapshot()["phases"][args["phase_id"]]["state"],"parked")
        resume={**args,"action":"resume","expected_revision":paused["revision"],"request_id":"manual-resume-fixture"}
        self.scheduler.control(job,resume);self.assertFalse(self.runner.frozen)

    def test_unconfirmed_thaw_blocks_admission_until_explicit_resume(self):
        """A lost thaw acknowledgment cannot leave unaccounted compiler growth eligible for more work."""
        self.phase();self.scheduler.act("one","freeze");self.runner.frozen=False
        self.scheduler.tick()
        row=self.scheduler.rows["one"]
        self.assertEqual(row["state"],"waiting_intervention")
        self.assertFalse(self.scheduler.can_admit(64*1024**2))
        self.time+=30;self.scheduler.tick()
        self.assertEqual(row["state"],"waiting_intervention")
        job={"job_id":"job","status":"running","lease_until":time.time()+60}
        self.scheduler.control(job,{"action":"resume","phase_id":hashlib.sha256(b"one").hexdigest(),
            "expected_revision":row["revision"],"request_id":"reconcile-thaw-fixture"})
        self.assertEqual(row["state"],"running")

    def test_control_rejects_stale_revision_and_revocation(self):
        """Phase IDs do not grant effects; live authority and exact resource revisions remain required."""
        self.phase();job={"job_id":"job","status":"running","lease_until":time.time()+60}
        args={"action":"pause","phase_id":hashlib.sha256(b"one").hexdigest(),"expected_revision":99,"request_id":"stale-control-fixture"}
        with self.assertRaisesRegex(ValueError,"phase changed"):self.scheduler.control(job,args)
        with self.assertRaises(PermissionError):self.scheduler.control({**job,"revoked":True},{**args,"expected_revision":0})
        self.assertFalse(self.runner.frozen)

    def test_unrelated_phases_admit_against_remaining_peak(self):
        self.phase();self.assertTrue(self.scheduler.can_admit(G));self.assertFalse(self.scheduler.can_admit(4*G))
    def test_pause_acknowledgment_and_frozen_timeout_accounting(self):
        self.phase();self.scheduler.act("one","freeze");self.time+=30
        self.assertEqual(self.scheduler.elapsed_pause("one"),30);self.scheduler.act("one","thaw");self.time+=10;self.assertEqual(self.scheduler.elapsed_pause("one"),30)
    def test_tests_cannot_be_parked(self):
        self.phase(phase="test")
        with self.assertRaises(ValueError):self.scheduler.act("one","freeze")
        self.assertFalse(any(c["operation"]=="freeze"for c in self.runner.calls))
    def test_insufficient_reclaim_waits_without_restart(self):
        self.phase();self.sample["available"]=100*1024**2;self.scheduler.tick()
        self.assertEqual(self.scheduler.rows["one"]["state"],"waiting_intervention")
        self.assertFalse(self.scheduler.can_admit(64*1024**2));self.assertTrue(self.runner.frozen)
        self.assertFalse(any(c["operation"]in {"restart","kill"}for c in self.runner.calls))
    def test_resume_requires_stable_headroom(self):
        self.phase();self.scheduler.act("one","freeze");self.scheduler.tick();self.time+=19;self.scheduler.tick();self.assertTrue(self.runner.frozen)
        self.time+=1;self.scheduler.tick();self.assertFalse(self.runner.frozen)
    def test_revoked_job_not_admitted_with_spare_memory(self):
        with self.assertRaises(PermissionError):self.scheduler.enter("one",{"job_id":"job","revoked":True},"compile_release","engine","engine")
    def test_success_records_actual_peak_not_estimate(self):
        self.phase();self.scheduler.tick();self.scheduler.finish("one",True)
        self.assertEqual(self.scheduler.profiles["engine-release"],[G]);self.assertEqual(self.scheduler.estimate("engine-release","engine"),math.ceil(G*1.2))
    def test_restart_requires_reconciliation_and_does_not_thaw(self):
        self.phase();self.scheduler.act("one","freeze");self.scheduler.persist()
        second=m.Resources(self.root.name,self.runner,{"mode":"enforce"},lambda:self.time,lambda:dict(self.sample))
        self.assertEqual(second.rows["one"]["state"],"recovery_required");self.assertFalse(second.can_admit(1));self.assertTrue(self.runner.frozen);second.close()
    def test_cpu_pressure_changes_allocation_without_freezing(self):
        self.phase();self.sample["pressure"]["cpu"]=80;self.scheduler.tick()
        self.assertTrue(any(c["operation"]=="cpu"for c in self.runner.calls));self.assertFalse(self.runner.frozen)
class ControlTests(unittest.TestCase):
    def test_unrelated_names_rejected_before_docker_inspection(self):
        c=control.Control({"instance":"development"})
        for name in ["webagent","webagent-dev-v2","webagent-dev-v3","../../system.slice"]:
            with self.assertRaises(PermissionError):c.resolve(name)


class DeadlineCPUAdmissionTests(unittest.TestCase):
 setUp=ResourceTests.setUp
 tearDown=ResourceTests.tearDown
 def add(self,name,phase):
  self.scheduler.enter(name,{"job_id":name},phase,"profile-"+name,"test",timeout=.05)
  self.scheduler.attached(name,"container")
 def test_pressure_keeps_test_floor_and_bounded_total(self):
  from unittest.mock import patch
  with patch.object(m.os,"cpu_count",return_value=4):
   self.scheduler.host["available"]=20*G;self.scheduler.host["total"]=24*G
   self.sample.update(available=20*G,total=24*G);self.sample["pressure"]["cpu"]=90
   self.add("test","test");self.add("browser","ui_test");self.add("compiler","compile_release")
   self.scheduler.cpu_budget=.5
   for _ in range(3):self.time+=6;self.scheduler.tick()
   rows=self.scheduler.rows
   self.assertGreaterEqual(rows["test"]["cpu_millis"],1000);self.assertGreaterEqual(rows["browser"]["cpu_millis"],1000)
   self.assertLessEqual(sum(r["cpu_millis"]for r in rows.values()),3000)
   self.assertGreaterEqual(rows["compiler"]["cpu_millis"],100)
   self.assertTrue(rows["test"]["cpu_history"]);self.assertNotIn("manual_pause",rows["test"])
 def test_cpu_admission_reports_wait_and_cancels_without_row(self):
  from unittest.mock import patch
  with patch.object(m.os,"cpu_count",return_value=2):
   self.add("existing","test");job={"job_id":"waiting"}
   with self.assertRaisesRegex(TimeoutError,"deadline_test_cpu_budget"):
    self.scheduler.enter("new",job,"ui_test","browser","ui_test",timeout=.02)
   self.assertEqual(job["last_resource_wait"]["resource"],"host_cpu");self.assertNotIn("new",self.scheduler.rows)
   job={"job_id":"cancelled","revoked":True}
   with self.assertRaises(PermissionError):self.scheduler.enter("cancelled",job,"test","tests","test",timeout=.01)
   self.assertNotIn("cancelled",self.scheduler.rows)
 def test_floor_is_applied_before_workload_and_failure_is_not_running(self):
  self.add("test","test")
  self.assertEqual(self.scheduler.rows["test"]["cpu_millis"],1000)
  self.assertEqual([c["cpu_millis"]for c in self.runner.calls if c["operation"]=="cpu"][0],1000)
  self.scheduler.finish("test",True)
  self.scheduler.enter("bad",{"job_id":"bad"},"test","bad","test")
  original=self.runner.resource_request
  def reject(value):
   if value["operation"]=="cpu":raise RuntimeError("helper failed")
   return original(value)
  self.runner.resource_request=reject
  with self.assertRaisesRegex(RuntimeError,"helper failed"):self.scheduler.attached("bad","container")
  self.assertEqual(self.scheduler.rows["bad"]["state"],"admitted")
 def test_cpu_diagnostics_read_kernel_quota_and_throttle_counters(self):
  group=Path(self.root.name)/"cgroup";group.mkdir()
  values={"cpu.stat":"usage_usec 1234\nnr_periods 8\nnr_throttled 3\nthrottled_usec 900\n","cpu.max":"100000 100000","cgroup.events":"frozen 0\n","io.stat":"","memory.events":"oom 0\noom_kill 0\n","memory.max":"max","memory.swap.max":"max"}
  for name,value in values.items():(group/name).write_text(value)
  stats=control.Control.stats(group)
  self.assertEqual(stats["cpu_limit_millis"],1000);self.assertEqual(stats["cpu_throttled_usec"],900);self.assertEqual(stats["cpu_throttled_periods"],3)
  (group/"cpu.max").write_text("max 100000");self.assertIsNone(control.Control.stats(group)["cpu_limit_millis"])


class CPUHeadroomTests(unittest.TestCase):
    """Exercise the real governor with quota pressure and independently measured CPU intervals."""
    def setUp(self):
        from unittest.mock import patch
        self.cpu_patch=patch.object(m.os,"cpu_count",return_value=4);self.cpu_patch.start()
        self.root=tempfile.TemporaryDirectory();self.time=100
        self.sample={"available":20*G,"total":24*G,"swap_free":0,"swap_total":0,
                     "pressure":{"cpu":90,"memory":0,"io":0},"cpu":{"count":4,"ticks":[100]*8}}
        self.runner=Fake();self.scheduler=m.Resources(self.root.name,self.runner,{"mode":"enforce"},lambda:self.time,lambda:dict(self.sample))
        self.scheduler.last_cpu_recovery=self.time
    def tearDown(self):
        self.scheduler.close();self.root.cleanup();self.cpu_patch.stop()
    def step(self,execution=50,idle=350,iowait=0,steal=0,seconds=5):
        ticks=self.sample["cpu"]["ticks"]
        for index,amount in [(0,execution),(3,idle),(4,iowait),(7,steal)]:ticks[index]+=amount
        self.time+=seconds;self.scheduler.tick()
    def test_own_quota_pressure_with_spare_cpu_does_not_ratchet_down_and_recovers(self):
        self.scheduler.cpu_budget=.5
        for _ in range(3):self.step()
        self.assertEqual(self.scheduler.cpu_budget,.5)
        for _ in range(17):self.step()
        self.assertEqual(self.scheduler.cpu_budget,3)
        self.assertEqual(self.scheduler.cpu_decision["reason"],"spare_cpu_recovery")
        self.assertAlmostEqual(self.scheduler.cpu_decision["available_cores"],3.5)
        self.assertEqual(self.sample["pressure"]["cpu"],90)
    def test_real_execution_pressure_reduces_once_then_stops_with_headroom(self):
        self.step(execution=350,idle=50)
        self.assertEqual(self.scheduler.cpu_budget,2.5)
        self.assertEqual(self.scheduler.cpu_decision["reason"],"execution_pressure")
        for _ in range(3):self.step(execution=290,idle=110)
        self.assertEqual(self.scheduler.cpu_budget,2.5)
        self.step(execution=290,idle=110)
        self.assertEqual(self.scheduler.cpu_budget,3)
    def test_stolen_cpu_consumes_headroom_but_iowait_is_not_execution(self):
        self.step(execution=100,idle=50,steal=250)
        self.assertEqual(self.scheduler.cpu_budget,2.5)
        self.assertAlmostEqual(self.scheduler.cpu_decision["steal_cores"],2.5)
        for _ in range(4):self.step(execution=50,idle=0,iowait=350)
        self.assertEqual(self.scheduler.cpu_budget,3)
        self.assertAlmostEqual(self.scheduler.cpu_decision["iowait_cores"],3.5)
    def test_exact_reserved_core_boundary_and_low_psi_do_not_reduce(self):
        self.step(execution=300,idle=100)
        self.assertEqual(self.scheduler.cpu_budget,3)
        self.sample["pressure"]["cpu"]=0
        self.step(execution=400,idle=0)
        self.assertEqual(self.scheduler.cpu_budget,3)
        self.assertEqual(self.scheduler.cpu_decision["reason"],"execution_busy")
    def test_missing_reset_zero_and_inconsistent_counters_hold_budget(self):
        valid={"count":4,"ticks":[100]*8}
        invalid=[None,{}, {"count":4,"ticks":[100]*7},{"count":True,"ticks":[100]*8},
                 {"count":3,"ticks":[200]*8},{"count":4,"ticks":[100]*8},
                 {"count":4,"ticks":[200,99,200,200,200,200,200,200]},
                 {"count":4,"ticks":[-1]*8},{"count":4,"ticks":[True]*8}]
        for current in invalid:
            for budget in (.5,3):
                with self.subTest(current=current,budget=budget):
                    self.scheduler.previous_cpu={"count":4,"ticks":list(valid["ticks"])}
                    self.sample["cpu"]=current;self.scheduler.cpu_budget=budget
                    self.scheduler.last_cpu_recovery=0;self.time+=5;self.scheduler.tick()
                    self.assertEqual(self.scheduler.cpu_budget,budget)
                    self.assertEqual(self.scheduler.cpu_decision["reason"],"counters_unavailable")
    def test_missing_interval_needs_a_fresh_baseline_before_recovery(self):
        self.scheduler.cpu_budget=.5;self.scheduler.last_cpu_recovery=0
        self.sample.pop("cpu");self.time+=5;self.scheduler.tick()
        self.sample["cpu"]={"count":4,"ticks":[100]*8};self.time+=5;self.scheduler.tick()
        self.assertEqual(self.scheduler.cpu_budget,.5)
        self.step();self.assertEqual(self.scheduler.cpu_budget,1)
    def test_subdecision_samples_do_not_shorten_the_execution_interval(self):
        self.scheduler.last_cpu_adjust=self.time
        self.sample["cpu"]["ticks"][0]+=350;self.sample["cpu"]["ticks"][3]+=50
        self.time+=1;self.scheduler.tick()
        self.assertEqual(self.scheduler.cpu_budget,3)
        self.assertEqual(self.scheduler.previous_cpu["ticks"],[100]*8)
        self.time+=4;self.scheduler.tick();self.assertEqual(self.scheduler.cpu_budget,2.5)
    def test_counter_parser_ignores_guest_columns_and_missing_counters_are_unknown(self):
        from unittest.mock import patch
        def read(path):
            if str(path)=="/proc/meminfo":return "MemAvailable: 1000 kB\nMemTotal: 2000 kB\n"
            if str(path)=="/proc/stat":return "cpu 1 2 3 4 5 6 7 8 9000 8000\ncpu0 0 0 0 0\n"
            raise OSError("Optional pressure fixture unavailable")
        with patch.object(m.Path,"read_text",read):
            sample=m.host_sample()
        self.assertEqual(sample["cpu"],{"count":4,"ticks":[1,2,3,4,5,6,7,8]})
        def missing(path):
            if str(path)=="/proc/meminfo":return "MemAvailable: 1000 kB\nMemTotal: 2000 kB\n"
            raise OSError("Unavailable")
        with patch.object(m.Path,"read_text",missing):self.assertIsNone(m.host_sample()["cpu"])
    def test_phase_floors_weights_and_three_core_cap_are_preserved(self):
        for name,phase in [("test","test"),("browser","ui_test"),("compiler","compile_release")]:
            self.scheduler.enter(name,{"job_id":name},phase,"cpu-fixture-"+name,"test",timeout=.05)
            self.scheduler.attached(name,"container")
        self.step(execution=350,idle=50)
        rows=self.scheduler.rows
        self.assertGreaterEqual(rows["test"]["cpu_millis"],1000)
        self.assertGreaterEqual(rows["browser"]["cpu_millis"],1000)
        self.assertGreaterEqual(rows["compiler"]["cpu_millis"],100)
        self.assertLessEqual(sum(row["cpu_millis"]for row in rows.values()),3000)
        self.assertFalse(self.runner.frozen)
        import json
        saved=json.loads(self.scheduler.path.read_text())
        self.assertEqual(saved["cpu_decision"]["reason"],"execution_pressure")
        self.assertEqual(saved["cpu_decision"]["budget_after"],2.5)
