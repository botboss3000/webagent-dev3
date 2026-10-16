"""Safety contracts for the prepared V3 retention implementation."""
import concurrent.futures,contextlib,importlib.util,json,os,sqlite3,tempfile,threading,time,unittest,uuid
from pathlib import Path
from collections import namedtuple
spec=importlib.util.spec_from_file_location("retention",os.environ.get("WEBAGENT_RETENTION_TEST_MODULE",str(Path(__file__).resolve().parents[1]/"build-service/docker_retention.py")))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
Usage=namedtuple("Usage","total used free")
G=1024**3
class CleanupTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);(self.root/"jobs").mkdir();self.now=100000.;self.free=15*G
        self.config={"instance":"development","install_root":str(self.root/"broker"),"state_root":str(self.root/"jobs"),"scheduler":{"disk_min_free_bytes":8*G,"disk_grace_seconds":7200,"disk_cleanup_batch":4}}
        (self.root/"jobs/coordination").mkdir()
        with contextlib.closing(sqlite3.connect(self.root/"jobs/coordination/ownership.sqlite"))as db,db:
            db.execute("CREATE TABLE tasks(id TEXT PRIMARY KEY,state TEXT)")
            db.execute("INSERT INTO tasks VALUES(?,?)",("f"*64,"complete"))
        self.job=str(uuid.uuid4());self.image="sha256:"+"a"*64;self.images={};self.containers={};self.refs=set();self.calls=[]
        self.receipt(self.job);self.add_image(self.image);self.guard=m.Retention(self.config,lambda:self.now,self.docker,lambda:Usage(100*G,100*G-self.free,self.free))
    def tearDown(self):self.tmp.cleanup()
    def receipt(self,key,status="completed",**kw):
        value={"job_id":key,"status":status,"ownership_id":"f"*64,"finished_at":1000,**kw};(self.root/"jobs"/(key+".json")).write_text(json.dumps(value))
    def add_image(self,key,tags=None):
        self.images[key]={"Id":key,"RepoTags":tags or["webagent-v3-api-"+self.job+":latest"]}
    def docker(self,*args):
        self.calls.append(args)
        if args[:2]==("image","inspect"):
            if args[2]not in self.images:raise FileNotFoundError()
            return json.dumps([self.images[args[2]]])
        if args[0]=="inspect":
            value=self.containers.get(args[1])or next((v for v in self.containers.values()if v["Name"].lstrip("/")==args[1]),None)
            if value is None:raise FileNotFoundError()
            return json.dumps([value])
        if args[0]=="ps":return "reference"if args[-1].split("=",1)[-1]in self.refs else""
        if args[:2]==("image","rm"):self.images.pop(args[2]);return""
        if args[0]=="rm":self.containers.pop(args[1]);return""
        raise AssertionError(args)
    def pin_old(self):
        self.guard.pin(self.image,self.job);self.now+=7201
    def test_unknown_image_is_never_adopted_or_deleted(self):
        self.assertEqual(self.guard.cleanup(True)["removed"],[]);self.assertIn(self.image,self.images)
    def test_expired_registered_output_deleted_exactly_without_force(self):
        self.pin_old();r=self.guard.cleanup(True);self.assertEqual(r["removed"],[{"kind":"image","id":self.image}]);self.assertIn(("image","rm",self.image),self.calls)
        self.assertFalse(any("prune"in c for c in self.calls))
    def test_dry_run_has_candidates_but_no_docker_deletion(self):
        self.pin_old();r=self.guard.cleanup();self.assertEqual(len(r["eligible"]),1);self.assertIn(self.image,self.images)
    def test_queued_running_review_waiting_and_unresolved_jobs_remain(self):
        self.pin_old()
        for state in ["queued","running","needs_decision","failed","uncertain","interrupted","unknown"]:
            self.receipt(self.job,state);self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_unfinished_logical_task_preserves_completed_build(self):
        self.pin_old()
        with contextlib.closing(sqlite3.connect(self.root/"jobs/coordination/ownership.sqlite"))as db,db:db.execute("UPDATE tasks SET state='owned'")
        self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_missing_ownership_metadata_preserves_completed_build(self):
        self.pin_old();(self.root/"jobs/coordination/ownership.sqlite").unlink()
        self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_recent_completed_job_retains_old_image(self):
        self.pin_old();self.receipt(self.job,finished_at=self.now);self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_new_registration_has_grace(self):
        self.guard.pin(self.image,self.job);self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_missing_job_metadata_preserves_image(self):
        self.pin_old();(self.root/"jobs"/(self.job+".json")).unlink();self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_corrupt_receipt_stops_deletion(self):
        self.pin_old();(self.root/"jobs"/(self.job+".json")).write_text("{")
        with self.assertRaises(ValueError):self.guard.cleanup(True)
        self.assertIn(self.image,self.images)
    def test_symlinked_receipt_stops_deletion(self):
        self.pin_old();p=self.root/"jobs"/(self.job+".json");p.rename(self.root/"saved");p.symlink_to(self.root/"saved")
        with self.assertRaises(OSError):self.guard.cleanup(True)
    def test_active_receipt_reference_protects_completed_output(self):
        self.pin_old();self.receipt(str(uuid.uuid4()),"queued",images={"api":self.image});self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_foreign_or_mixed_tags_preserved(self):
        self.pin_old()
        for tags in [["production:latest"],["webagent-v3-api-"+self.job+":latest","foreign:latest"],[]]:
            self.images[self.image]["RepoTags"]=tags;self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_any_container_reference_preserves_rollback_image(self):
        self.pin_old();self.refs.add(self.image);self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_recipe_pin_is_permanent(self):
        self.guard.pin(self.image);self.now+=999999;self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_registry_restart_preserves_dependency(self):
        self.pin_old();self.receipt(self.job,"running");g=m.Retention(self.config,lambda:self.now,self.docker,lambda:Usage(100*G,85*G,15*G));self.assertEqual(g.cleanup(True)["removed"],[])
    def test_missing_deleted_image_reconciles_metadata(self):
        self.pin_old();self.images.pop(self.image);self.assertEqual(self.guard.cleanup(True)["removed"],[])
        self.assertNotIn(self.image,self.guard.load()["images"])
    def test_batch_limit(self):
        for n in range(6):
            image="sha256:"+("%064x"%(n+1));self.add_image(image);self.guard.pin(image,self.job)
        self.now+=7201;self.assertEqual(len(self.guard.cleanup(True)["removed"]),4)
    def test_trigger_and_best_effort_target(self):
        self.pin_old();self.free=30*G;self.assertEqual(self.guard.cleanup(True)["removed"],[])
        self.free=15*G;r=self.guard.cleanup(True);self.assertFalse(r["target_reached"])
    def test_missing_registry_cannot_delete(self):
        self.guard.path.unlink(missing_ok=True);self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_production_instance_rejected(self):
        with self.assertRaises(PermissionError):m.Retention({**self.config,"instance":"production"})
    def test_registration_waits_for_deletion_lock(self):
        pool=concurrent.futures.ThreadPoolExecutor()
        try:
            with self.guard.locked():
                future=pool.submit(self.guard.pin,self.image,self.job);time.sleep(.03);self.assertFalse(future.done())
            self.assertTrue(future.result(timeout=2)["retained"])
        finally:pool.shutdown()
    def container(self,status="exited"):
        identity="b"*64;name="webagent-build-development-"+uuid.UUID(self.job).hex+"-compile_test-12345678"
        value={"Id":identity,"Name":"/"+name,"Image":self.image,"Config":{"Labels":{"webagent.scheduler.instance":"development","webagent.scheduler.job":uuid.UUID(self.job).hex,"webagent.scheduler.phase":"compile_test"}},"State":{"Status":status}}
        self.containers[identity]=value;self.guard.pin(self.image,self.job,value);return identity
    def test_protected_candidates_read_job_history_once(self):
        identity=self.container();self.now+=7201;self.receipt(self.job,"running")
        from unittest.mock import patch
        with patch.object(self.guard,"jobs",wraps=self.guard.jobs)as jobs:
            self.assertEqual(self.guard.cleanup(True)["removed"],[])
            self.assertEqual(jobs.call_count,1)
        self.assertIn(identity,self.containers);self.assertIn(self.image,self.images)
    def test_expired_phase_container_removed_then_image(self):
        identity=self.container();self.now+=7201;r=self.guard.cleanup(True)
        self.assertEqual(r["removed"][0],{"kind":"container","id":identity})
    def test_running_phase_container_survives(self):
        identity=self.container("running");self.now+=7201;self.refs.add(self.image)
        self.assertEqual(self.guard.cleanup(True)["removed"],[]);self.assertIn(identity,self.containers)
    def test_changed_container_labels_survive(self):
        identity=self.container();self.now+=7201;self.containers[identity]["Config"]["Labels"]["webagent.scheduler.instance"]="production";self.refs.add(self.image)
        self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_bad_container_registration_rejected(self):
        identity="c"*64
        with self.assertRaises(PermissionError):self.guard.pin(self.image,self.job,{"Id":identity,"Name":"/webagent","Config":{"Labels":{}}})
        self.assertFalse(self.guard.path.exists())
    def test_engine_error_preserves_candidate(self):
        self.pin_old();old=self.guard.docker
        def fail(*args):
            if args[:2]==("image","inspect"):raise RuntimeError("Engine unavailable")
            return old(*args)
        self.guard.docker=fail
        with self.assertRaises(RuntimeError):self.guard.cleanup(True)
        self.assertIn(self.image,self.images)
    def test_delete_intent_survives_uncertain_engine_response(self):
        self.pin_old();old=self.guard.docker
        def fail(*args):
            if args[:2]==("image","rm"):self.images.pop(args[2]);raise TimeoutError("Response lost")
            return old(*args)
        self.guard.docker=fail
        with self.assertRaises(TimeoutError):self.guard.cleanup(True)
        self.assertEqual(self.guard.load()["intent"]["id"],self.image)
        self.guard.docker=old;self.guard.cleanup(True);self.assertNotIn("intent",self.guard.load())
    def test_invalid_registry_stops_deletion(self):
        self.guard.path.write_text('{"version":2}')
        with self.assertRaises(ValueError):self.guard.cleanup(True)
    def test_terminal_reference_does_not_override_another_jobs_pin(self):
        self.pin_old();other=str(uuid.uuid4());self.receipt(other,"running");self.guard.pin(self.image,other)
        self.assertEqual(self.guard.cleanup(True)["removed"],[])
    def test_disk_reservation_shared_and_cancelled_without_oversubscription(self):
        name="webagent-build-development-"+uuid.uuid4().hex+"-compile_test-12345678"
        r=self.guard.disk(name,"compile_test","recipe");self.assertTrue(r["admitted"])
        other="webagent-build-development-"+uuid.uuid4().hex+"-package-12345678"
        self.assertFalse(self.guard.disk(other,"package","package")["admitted"])
        self.guard.disk(name,None,finish=True)
        self.free=30*G;self.assertTrue(self.guard.disk(other,"package","package")["admitted"])
    def test_runtime_base_disk_profiles_keep_global_free_floor(self):
        name="webagent-build-development-"+uuid.uuid4().hex+"-package-12345678"
        self.free=11*G
        api=self.guard.disk(name,"package","buildkit:v0.25.1:runtime-base-v1:api")
        self.assertTrue(api["admitted"])
        self.assertEqual(3*G,api["estimate_bytes"])
        self.guard.disk(name,None,finish=True)
        self.free=9*G
        loop=self.guard.disk(name,"package","buildkit:v0.25.1:runtime-base-v1:loop")
        self.assertTrue(loop["admitted"])
        self.assertEqual(512*1024**2,loop["estimate_bytes"])
        self.guard.disk(name,None,finish=True)
        self.free=8*G
        self.assertFalse(self.guard.disk(name,"package","buildkit:v0.25.1:role-context-v2:loop:disk-cache-v1")["admitted"])

    def test_restart_reconciles_only_terminal_absent_disk_phase(self):
        name="webagent-build-development-"+uuid.UUID(self.job).hex+"-compile_test-12345678"
        self.guard.disk(name,"compile_test","recipe")
        self.receipt(self.job,"running");self.assertEqual(self.guard.reconcile_phases(True),[])
        self.receipt(self.job,"interrupted");self.assertEqual(self.guard.reconcile_phases(True),[name])
        self.assertEqual(self.guard.load()["phases"],{})
    def test_missing_job_does_not_release_disk_reservation(self):
        name="webagent-build-development-"+uuid.uuid4().hex+"-compile_test-12345678"
        self.guard.disk(name,"compile_test","recipe");self.assertEqual(self.guard.reconcile_phases(True),[])
        self.assertIn(name,self.guard.load()["phases"])
    def test_measured_output_footprint_raises_recipe_floor(self):
        self.free=80*G;name="webagent-build-development-"+uuid.uuid4().hex+"-compile_test-12345678"
        self.guard.disk(name,"compile_test","recipe");self.guard.disk(name,None,finish=True,measured_bytes=9*G)
        self.assertEqual(self.guard.disk(name,"compile_test","recipe")["estimate_bytes"],9*G)
    def test_cleanup_observes_registration_completed_before_deletion_lock(self):
        self.pin_old();other=str(uuid.uuid4());self.receipt(other,"queued")
        with concurrent.futures.ThreadPoolExecutor()as pool:
            with self.guard.locked():
                future=pool.submit(self.guard.cleanup,True)
                time.sleep(.03);self.assertFalse(future.done())
                state=self.guard.load();state["images"][self.image]["jobs"].append(other);self.guard.save(state)
            self.assertEqual(future.result(timeout=3)["removed"],[])
        self.assertIn(self.image,self.images)
    def test_existing_disk_reservation_rejects_changed_recipe(self):
        name="webagent-build-development-"+uuid.uuid4().hex+"-compile_test-12345678"
        self.guard.disk(name,"compile_test","original")
        with self.assertRaises(ValueError):self.guard.disk(name,"compile_test","changed")
        self.assertEqual(self.guard.load()["phases"][name]["profile"],"original")
    def test_preserved_container_reference_has_diagnostic_reason(self):
        self.pin_old();self.refs.add(self.image);report=self.guard.cleanup(True)
        self.assertEqual(report["preserved"][0]["reason"],"running or stopped container reference")

    def test_fixed_source_free_probe_does_not_reserve_compiler_outputs(self):
        self.free=10*G
        name="webagent-build-development-"+uuid.uuid4().hex+"-compile_test-12345678"
        probe=self.guard.disk(name,"compile_test","resource:memory-probe-v1")
        self.assertTrue(probe["admitted"]);self.assertEqual(probe["estimate_bytes"],G)
        self.guard.disk(name,None,finish=True)
        real=self.guard.disk(name,"compile_test","engine:rust1.90:compile_test")
        self.assertFalse(real["admitted"]);self.assertEqual(real["estimate_bytes"],6*G)

class DiskAdmissionTests(unittest.TestCase):
    def setUp(self):
        import test_scheduler_resources as fixtures
        self.tmp=tempfile.TemporaryDirectory();self.allowed=False;self.releases=[];self.waits=[]
        outer=self
        class Runner(fixtures.Fake):
            def disk_admission(self,*args):return {"admitted":outer.allowed,"reason":"disk","available_bytes":1,"estimate_bytes":2}
            def disk_finish(self,name):outer.releases.append(name)
            def resource_progress(self,job):outer.waits.append(dict(job))
        self.runner=Runner()
        sample={"available":30*G,"total":32*G,"swap_free":G,"swap_total":G,"pressure":{"cpu":0,"memory":0,"io":0}}
        self.resources=fixtures.m.Resources(self.tmp.name,self.runner,{"mode":"enforce"},sampler=lambda:dict(sample))
    def tearDown(self):self.resources.close();self.tmp.cleanup()
    def test_disk_wait_is_visible_and_deadline_leaves_no_admitted_phase(self):
        with self.assertRaises(TimeoutError):self.resources.enter("phase",{"job_id":"job"},"compile_test","recipe","engine",timeout=.01)
        self.assertEqual(self.resources.rows,{})
        self.assertEqual(self.waits[0]["resource_waiting"]["reason"],"disk")
    def test_disk_wait_resumes_when_capacity_returns(self):
        def release():
            time.sleep(.02)
            with self.resources.condition:self.allowed=True;self.resources.condition.notify_all()
        thread=threading.Thread(target=release);thread.start()
        self.resources.enter("phase",{"job_id":"job"},"compile_test","recipe","engine",timeout=1)
        thread.join();self.assertIn("phase",self.resources.rows)
        self.resources.finish("phase",True);self.assertIn("phase",self.releases)
    def test_initialization_failure_precedes_disk_reservation(self):
        self.allowed=True;attempts=[]
        self.runner.disk_admission=lambda *args:attempts.append(args)or{"admitted":True}
        def fail(*args):raise RuntimeError("pool initialization failed")
        self.runner.resource_request=fail
        with self.assertRaises(RuntimeError):self.resources.enter("phase",{"job_id":"job"},"compile_test","recipe","engine",timeout=.1)
        self.assertEqual(attempts,[])
        self.assertEqual(self.releases,[])
        self.assertEqual(self.resources.rows,{})

    def test_revocation_after_disk_approval_releases_reservation(self):
        job={"job_id":"job"}
        def approve(*args):job["revoked"]=True;return {"admitted":True}
        self.runner.disk_admission=approve
        with self.assertRaises(PermissionError):self.resources.enter("phase",job,"compile_test","recipe","engine",timeout=.1)
        self.assertEqual(self.releases,["phase"])
class CleanupLockAdmissionTests(unittest.TestCase):
    """Contention waits; corruption and release failures remain visible."""
    def control(self,error):
        from types import SimpleNamespace
        spec=importlib.util.spec_from_file_location("retention_resource_control",Path(__file__).resolve().parents[1]/"build-service/resource_control.py")
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        control=module.Control.__new__(module.Control)
        control.config={"scheduler":{"task_aware_cleanup":True}}
        def disk(*args,**kw):raise error
        control.retention=lambda:SimpleNamespace(disk=disk)
        return module,control
    def test_disk_admit_lock_contention_becomes_wait(self):
        from unittest.mock import patch
        module,control=self.control(TimeoutError("Retention lock busy"))
        with patch.object(module.os,"geteuid",return_value=0):
            self.assertEqual(control.handle({"operation":"disk_admit"}),{"admitted":False,"reason":"cleanup_busy"})
    def test_disk_admit_corruption_still_fails_closed(self):
        from unittest.mock import patch
        module,control=self.control(ValueError("Invalid retention registry"))
        with patch.object(module.os,"geteuid",return_value=0),self.assertRaises(ValueError):
            control.handle({"operation":"disk_admit"})
    def test_disk_finish_lock_failure_remains_visible(self):
        from unittest.mock import patch
        module,control=self.control(TimeoutError("Retention lock busy"))
        with patch.object(module.os,"geteuid",return_value=0),self.assertRaises(TimeoutError):
            control.handle({"operation":"disk_finish"})

class IndexSeedSafetyTests(unittest.TestCase):
    """Optional cache rejection must not certify a corrupt published pair."""
    def test_invalid_published_seed_is_not_reused(self):
        from unittest.mock import patch
        from types import SimpleNamespace
        import test_admin_build as fixtures
        runner=fixtures.host.Runner.__new__(fixtures.host.Runner)
        def invalid(root):raise ValueError("Published source/index binding mismatch")
        with patch.object(fixtures.host.maintenance_engine,"load_scanner",return_value=SimpleNamespace(read_published=invalid)):
            self.assertIsNone(runner.verified_index_seed(Path("/invalid-published-fixture")))
    def test_verified_seed_can_be_reused(self):
        from unittest.mock import patch
        from types import SimpleNamespace
        import test_admin_build as fixtures
        runner=fixtures.host.Runner.__new__(fixtures.host.Runner);root=Path("/verified-published-fixture");seen=[]
        with patch.object(fixtures.host.maintenance_engine,"load_scanner",return_value=SimpleNamespace(read_published=lambda p:seen.append(p)or(b"index",{}))):
            self.assertEqual(runner.verified_index_seed(root),root)
        self.assertEqual(seen,[root])
    def test_no_seed_requires_no_published_read(self):
        import test_admin_build as fixtures
        self.assertIsNone(fixtures.host.Runner.__new__(fixtures.host.Runner).verified_index_seed(None))

if __name__=="__main__":unittest.main(verbosity=2)
