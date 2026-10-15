"""Prepared review leases retain ownership only for the exact authorized frozen candidate."""
import hashlib,importlib.util,json,tempfile,types,unittest,uuid,time
from pathlib import Path
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[1]
def load(name):
    """Load the task's paired broker implementations; tests never access runtime state."""
    spec=importlib.util.spec_from_file_location("review_"+name,ROOT/"build-service"/(name+".py"));m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
scheduler=load("scheduler")
import test_scheduler_broker as broker_fixtures
class ReviewLeaseTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);self.now=1000.;self.counter=0
        self.clock=patch.object(scheduler.time,"time",lambda:self.now);self.clock.start()
        self.store=scheduler.ownership.Ownership(self.root/"ownership.sqlite",lambda:self.now)
        self.key="a"*64;self.owner=scheduler.ownership.canonical(["user","workspace"])
        row=self.store.begin(self.key,self.owner,"review-task","review-begin-0001")
        self.row=self.store.acquire(self.key,self.owner,row["revision"],row["epoch"],["module:broker"],"review-reserve-0001")["task"]
        self.id=str(uuid.uuid4());self.prepared=self.root/"releases"/self.id;(self.prepared/".code-index").mkdir(parents=True)
        self.manifest={"release_id":self.id,"candidate_sha256":"b"*64,"source_sha256":"c"*64,"index_sha256":"d"*64,"gaps":0,"stale_count":0,"unindexed_count":0}
        self.write_manifest()
        self.job={"job_id":self.id,"task_id":"review-task","ownership_id":self.key,"ownership_epoch":self.row["epoch"],"source_generation":self.row["generation"],"user_id":"user","workspace":"workspace","inputs_sha256":"b"*64,"lease_until":4600.,"revoked":False,"status":"needs_decision","phase":"awaiting_prepared_reviews","prepared_assets":str(self.prepared)}
        self.saved=[];self.work={self.key:{"job_id":self.id,"status":"closed"}}
        self.broker=types.SimpleNamespace(jobs={self.id:self.job},work=self.work,_save=lambda job:self.saved.append(json.loads(json.dumps(job))))
        self.s=scheduler.Scheduler.__new__(scheduler.Scheduler);self.s.mode="enforce";self.s.store=self.store;self.s.broker=self.broker;self.s.orphan_after=300
    def tearDown(self):self.clock.stop();self.tmp.cleanup()
    def write_manifest(self):(self.prepared/".code-index/manifest.json").write_text(json.dumps(self.manifest))
    def start(self):self.assertTrue(self.s.review_wait(self.job,reset=True))
    def test_review_survives_normal_orphan_threshold(self):
        self.start()
        for _ in range(5):self.now+=301;self.s.housekeeping();self.assertEqual(self.store.get(self.key)["state"],"owned")
        self.assertEqual(self.job["status"],"needs_decision");self.assertEqual(self.job["phase"],"awaiting_prepared_reviews")
    def test_no_execution_resources_or_automatic_publication(self):
        self.start();self.now+=301;self.s.housekeeping()
        self.assertEqual(set(self.broker.__dict__),{"jobs","work","_save"})
        self.assertNotIn("effects_started",self.job);self.assertEqual(self.job["status"],"needs_decision")
    def test_real_authority_expiry_fences_and_preserves_candidate(self):
        self.start();body=(self.prepared/".code-index/manifest.json").read_bytes();self.now=4601;self.s.housekeeping()
        self.assertEqual(self.job["review_wait"]["state"],"authorization_expired");self.assertEqual(self.store.get(self.key)["state"],"orphaned")
        self.assertEqual((self.prepared/".code-index/manifest.json").read_bytes(),body)
    def test_revocation_does_not_renew(self):
        self.start();self.job["revoked"]=True;self.now+=301;self.s.housekeeping()
        self.assertEqual(self.store.get(self.key)["state"],"orphaned");self.assertEqual(self.job["review_wait"]["state"],"authorization_expired")
    def test_other_decision_states_are_not_renewed(self):
        self.start();self.job["phase"]="waiting_publication_base";self.now+=301;self.s.housekeeping()
        self.assertEqual(self.store.get(self.key)["state"],"orphaned")
    def test_polling_cannot_create_a_review_delegation(self):
        self.now+=301;self.s.housekeeping();self.assertNotIn("review_wait",self.job);self.assertEqual(self.store.get(self.key)["state"],"orphaned")
    def test_authorized_activity_bootstraps_existing_wait(self):
        self.assertTrue(self.s.review_wait(self.job,activity=True,authority_until=1600))
        self.assertEqual(self.job["review_wait"]["expires_at"],1600)
    def test_housekeeping_never_extends_shorter_review_authority(self):
        self.s.review_wait(self.job,activity=True,authority_until=1200);self.now=1301;self.s.housekeeping()
        self.assertEqual(self.job["review_wait"]["expires_at"],1200);self.assertEqual(self.job["review_wait"]["state"],"authorization_expired")
    def test_explicit_authorized_activity_can_renew_within_job_deadline(self):
        self.s.review_wait(self.job,activity=True,authority_until=1200);self.now=1100
        self.assertTrue(self.s.review_wait(self.job,activity=True,authority_until=9000));self.assertEqual(self.job["review_wait"]["expires_at"],4600)
    def test_generation_change_invalidates_delegation(self):
        self.start();v=self.store.get(self.key);self.store.edited(self.key,self.owner,v["revision"],v["epoch"],"review-edit-0001")
        self.assertFalse(self.s.review_wait(self.job));self.assertEqual(self.job["review_wait"]["state"],"binding_invalid")
    def test_epoch_change_invalidates_delegation(self):
        self.start();self.job["ownership_epoch"]+=1;self.assertFalse(self.s.review_wait(self.job))
    def test_owner_change_invalidates_delegation(self):
        self.start();self.job["user_id"]="other";self.assertFalse(self.s.review_wait(self.job))
    def test_newer_task_job_invalidates_delegation(self):
        self.start();self.work[self.key]["job_id"]=str(uuid.uuid4());self.assertFalse(self.s.review_wait(self.job))
    def test_open_task_cannot_masquerade_as_frozen_review(self):
        self.work[self.key]["status"]="open";self.assertFalse(self.s.review_wait(self.job,reset=True))
    def test_changed_index_binding_needs_new_verified_preparation(self):
        self.start();self.manifest["index_sha256"]="e"*64;self.write_manifest();self.assertFalse(self.s.review_wait(self.job))
    def test_changed_candidate_digest_is_rejected(self):
        self.manifest["candidate_sha256"]="e"*64;self.write_manifest();self.assertFalse(self.s.review_wait(self.job,reset=True))
    def test_nonzero_index_gaps_stale_and_unindexed_are_rejected(self):
        for key in ["gaps","stale_count","unindexed_count"]:
            self.manifest[key]=1;self.write_manifest();self.assertFalse(self.s.review_wait(self.job,reset=True));self.manifest[key]=0
    def test_missing_and_symlinked_manifest_are_rejected(self):
        p=self.prepared/".code-index/manifest.json";p.unlink();self.assertFalse(self.s.review_wait(self.job,reset=True))
        outside=self.root/"manifest";outside.write_text(json.dumps(self.manifest));p.symlink_to(outside);self.assertFalse(self.s.review_wait(self.job,reset=True))
    def test_orphaned_owner_is_not_automatically_revived(self):
        self.start();self.now+=301;self.store.orphan(300);self.assertFalse(self.s.review_wait(self.job));self.assertEqual(self.store.get(self.key)["state"],"orphaned")
    def test_restart_reuses_durable_review_binding_without_resetting_deadline(self):
        self.start();stored=json.loads(json.dumps(self.job));self.now+=301;self.broker.jobs[self.id]=stored
        self.s.housekeeping();self.assertEqual(stored["review_wait"]["expires_at"],4600);self.assertEqual(self.store.get(self.key)["state"],"owned")
    def test_renewal_does_not_change_source_revision_or_generation(self):
        self.start();before=self.store.get(self.key);self.now+=301;self.s.housekeeping();after=self.store.get(self.key)
        self.assertEqual((before["revision"],before["generation"],before["epoch"]),(after["revision"],after["generation"],after["epoch"]))
    def test_broker_maintenance_gate_does_not_invalidate_existing_review(self):
        self.start();self.broker.maintenance={"drained":True,"owner":"other"};self.now+=301;self.s.housekeeping()
        self.assertEqual(self.store.get(self.key)["state"],"owned")
    def test_only_rebound_preparation_can_reset_candidate(self):
        self.start();self.manifest["index_sha256"]="e"*64;self.write_manifest();self.assertTrue(self.s.review_wait(self.job,reset=True))
    def test_unchanged_manifest_is_not_reparsed_on_each_heartbeat(self):
        self.start()
        original=Path.read_bytes
        def deny(path,*a,**kw):
            if path.name=="manifest.json":raise AssertionError("Unchanged manifest reparsed")
            return original(path,*a,**kw)
        with patch.object(Path,"read_bytes",deny):
            self.now+=301;self.s.housekeeping()
        self.assertEqual(self.store.get(self.key)["state"],"owned")
    def test_ordinary_jobs_keep_existing_renewal(self):
        self.job["status"]="running";self.now+=301;self.s.housekeeping();self.assertEqual(self.store.get(self.key)["state"],"owned")
class AcquisitionRecoveryTests(unittest.TestCase):
    """Only fixture state is mutated; no application container or live broker is used."""
    def setUp(self):
        self.scheduler_patch=patch.object(broker_fixtures.fixtures.host,"scheduler_module",scheduler);self.scheduler_patch.start()
        broker_fixtures.SchedulerBrokerTests.setUp(self)
    def tearDown(self):
        broker_fixtures.SchedulerBrokerTests.tearDown(self);self.scheduler_patch.stop()
    call=broker_fixtures.SchedulerBrokerTests.call
    begin=broker_fixtures.SchedulerBrokerTests.begin
    action=broker_fixtures.SchedulerBrokerTests.action
    write=broker_fixtures.SchedulerBrokerTests.write
    def release(self,item):
        """Release a fixture owner's current scopes through the normal public API."""
        value=self.broker.scheduler.store.get(item["key"]);self.counter+=1
        return self.call("build_work",{"action":"release_claims","task_id":item["task_id"],"scopes":value["scopes"],"reason":"Fixture phased ownership","expected_revision":value["revision"],"expected_epoch":value["epoch"],"request_id":"fixture-release-"+str(self.counter)})
    def waiting(self):
        """Preserve an edited released draft, then let it wait behind another engine owner."""
        blocker=self.begin("recovery-blocker")
        item=self.begin("edited-waiter");self.write(item,"README.md","preserved unpublished edit\n");self.release(item)
        self.action(blocker["task_id"],"reserve",paths=["src/blocked.rs"])
        result,args=self.action(item["task_id"],"reserve",paths=["crates/pilot/src/lib.rs"])
        self.assertFalse(result["acquired"]);self.release(blocker)
        return item,args
    def stuck(self):
        """Model the installed legacy commit-before-refresh ordering through real broker requests."""
        item,_=self.waiting()
        with patch.object(self.broker.scheduler,"require_clean_waiter",return_value=None):
            with self.assertRaisesRegex(ValueError,"Waiting task contains edits"):
                _,args=self.action(item["task_id"],"reserve",paths=["crates/pilot/src/lib.rs"])
        # action() raised before returning; retain the exact deterministic request it sent.
        current=self.broker.scheduler.store.get(item["key"])
        args={"task_id":item["task_id"],"action":"reserve","request_id":"broker-fixture-"+str(self.counter),"expected_revision":current["revision"]-1,"expected_epoch":current["epoch"],"paths":["crates/pilot/src/lib.rs"]}
        self.original=args;self.item=item
        tree=Path(item["workspace_path"]).parent;self.source=self.broker.scheduler_task_snapshot(tree);self.baseline=(tree/"baseline.tar").read_bytes()
        return item
    def recovery(self,**changes):
        """Bind one exact reserve payload to the current draft and ownership generation."""
        value=self.broker.scheduler.store.get(self.item["key"])
        return {"action":"reconcile_acquisition","task_id":self.item["task_id"],"reserve_request":self.original,"expected_inputs_sha256":self.broker.scheduler_digest(self.source),"expected_revision":value["revision"],"expected_epoch":value["epoch"],"request_id":"edited-acquisition-fixture-recovery",**changes}
    def assert_source(self):
        """Verify all retained bytes and the sealed baseline, not only the edited file."""
        tree=Path(self.item["workspace_path"]).parent
        self.assertEqual(self.broker.scheduler_task_snapshot(tree),self.source)
        self.assertEqual((tree/"baseline.tar").read_bytes(),self.baseline)
    def test_new_dirty_waiter_is_rejected_before_claim_commit(self):
        item,_=self.waiting();value=self.broker.scheduler.store.get(item["key"])
        with self.assertRaisesRegex(ValueError,"Waiting task contains edits"):
            self.action(item["task_id"],"reserve",paths=["crates/pilot/src/lib.rs"])
        current=self.broker.scheduler.store.get(item["key"])
        self.assertEqual(current["scopes"],[]);self.assertEqual(current["revision"],value["revision"])
        self.assertIsNone(self.broker.scheduler.pending(item["key"]))
    def test_committed_acquisition_releases_and_preserves_every_source_byte(self):
        item=self.stuck();args=self.recovery();before=self.broker.scheduler.store.get(item["key"])
        result=self.call("build_work",args)
        self.assertTrue(result["source_preserved"]);self.assertEqual(result["task"]["scopes"],[])
        self.assertEqual(result["task"]["epoch"],before["epoch"]+1);self.assert_source()
        self.assertNotIn("acquisition_refresh",item);self.assertIsNone(self.broker.scheduler.pending(item["key"]))
        self.assertEqual(self.call("build_work",args),result)
        with self.assertRaisesRegex(ValueError,"explicitly reconciled"):self.call("build_work",self.original)
    def test_orphaned_owner_can_reconcile_with_a_fresh_cas(self):
        self.stuck()
        with self.broker.scheduler.store.transaction()as db:
            value=self.broker.scheduler.store.get(self.item["key"],db);self.broker.scheduler.store.update(db,value,state="orphaned")
        result=self.call("build_work",self.recovery());self.assertEqual(result["task"]["scopes"],[]);self.assert_source()
    def test_stale_epoch_or_revision_does_not_release(self):
        self.stuck()
        for changes in [{"expected_revision":0},{"expected_epoch":99}]:
            with self.assertRaisesRegex(ValueError,"Stale acquisition"):self.call("build_work",self.recovery(**changes))
        self.assertTrue(self.broker.scheduler.store.get(self.item["key"])["scopes"]);self.assert_source()
    def test_modified_original_request_is_not_a_recovery_capability(self):
        self.stuck();original={**self.original,"paths":["src/another.rs"]}
        with self.assertRaisesRegex(ValueError,"binding differs"):self.call("build_work",self.recovery(reserve_request=original))
        self.assertTrue(self.broker.scheduler.store.get(self.item["key"])["scopes"])
    def test_operator_and_current_authority_are_required(self):
        self.stuck();self.trusted["local_caller"]=False;self.trusted["lease_until"]=time.time()+30
        with self.assertRaises(PermissionError):self.call("build_work",self.recovery())
        self.trusted["local_caller"]=True;self.trusted["lease_until"]=0
        with self.assertRaisesRegex(ValueError,"authorization lease required"):self.call("build_work",self.recovery())
        self.assertTrue(self.broker.scheduler.store.get(self.item["key"])["scopes"])
    def test_source_change_refuses_release_without_overwriting_the_draft(self):
        self.stuck();(Path(self.item["workspace_path"])/"README.md").write_text("external fixture mutation\n")
        with self.assertRaisesRegex(ValueError,"source differs"):self.call("build_work",self.recovery())
        self.assertTrue(self.broker.scheduler.store.get(self.item["key"])["scopes"])
    def test_started_filesystem_refresh_requires_separate_reconciliation(self):
        self.stuck();current=self.broker.scheduler.store.get(self.item["key"]);folder=self.broker.work_dir/"acquisitions"/self.item["key"]/str(current["revision"])
        (folder/"candidate.tar").write_bytes(b"fixture capture")
        with self.assertRaisesRegex(ValueError,"filesystem effects"):self.call("build_work",self.recovery())
        self.assertTrue(self.broker.scheduler.store.get(self.item["key"])["scopes"]);self.assert_source()
    def test_running_or_uncertain_execution_prevents_release(self):
        self.stuck();self.broker.jobs["fixture-job"]={"ownership_id":self.item["key"],"status":"uncertain"}
        with self.assertRaisesRegex(ValueError,"Reconcile execution"):self.call("build_work",self.recovery())
        self.broker.jobs.clear();self.assert_source()
    def test_crash_after_ownership_commit_reuses_the_compensating_receipt(self):
        self.stuck();args=self.recovery();original=self.broker._save_work
        with patch.object(self.broker,"_save_work",side_effect=RuntimeError("fixture crash after release")):
            with self.assertRaisesRegex(RuntimeError,"fixture crash"):self.call("build_work",args)
        released=self.broker.scheduler.store.get(self.item["key"]);self.assertEqual(released["scopes"],[])
        result=self.call("build_work",args);self.assertEqual(result["task"]["epoch"],released["epoch"]);self.assert_source()
    def test_crash_after_original_settlement_finishes_the_same_recovery(self):
        self.stuck();args=self.recovery();writer=self.broker.scheduler.write_record
        def fail_final(path,value):
            if value.get("state")=="done"and value.get("request_id")==args["request_id"]:raise RuntimeError("fixture final receipt crash")
            return writer(path,value)
        with patch.object(self.broker.scheduler,"write_record",side_effect=fail_final):
            with self.assertRaisesRegex(RuntimeError,"final receipt crash"):self.call("build_work",args)
        released=self.broker.scheduler.store.get(self.item["key"])
        result=self.call("build_work",args);self.assertEqual(result["task"]["epoch"],released["epoch"]);self.assert_source()
    def test_done_replay_never_releases_subsequent_claims(self):
        self.stuck();args=self.recovery();result=self.call("build_work",args)
        self.action(self.item["task_id"],"reserve",paths=["src/later.rs"])
        self.assertEqual(self.call("build_work",args),result)
        self.assertIn("module:engine",self.broker.scheduler.store.get(self.item["key"])["scopes"])
    def test_new_request_payload_cannot_reuse_a_completed_recovery_id(self):
        self.stuck();args=self.recovery();self.call("build_work",args)
        with self.assertRaisesRegex(ValueError,"Request ID reused"):self.call("build_work",{**args,"expected_epoch":99})
    def test_fresh_integration_task_can_acquire_after_recovery(self):
        self.stuck();self.call("build_work",self.recovery());fresh=self.begin("fresh-integration")
        result,_=self.action(fresh["task_id"],"reserve",paths=["crates/pilot/src/lib.rs"])
        self.assertTrue(result["acquired"]);self.assert_source()

if __name__=="__main__":unittest.main()
