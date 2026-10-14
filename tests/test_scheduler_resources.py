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
