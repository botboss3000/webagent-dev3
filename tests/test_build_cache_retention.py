import json, os, time, unittest, shutil
import test_admin_build as fixtures
class CacheRetentionTests(unittest.TestCase):
    tearDown = fixtures.BuildTests.tearDown
    def setUp(self):
        fixtures.BuildTests.setUp(self)
        self.broker.jobs.clear()
        self.broker.active = self.broker.active_ui = None
        self.mounts=[]; self.processes="python3"
        def command(argv, **kwargs):
            if argv[0]=="ps": return self.processes
            if argv[:2]==["docker","ps"]: return "fixture"
            if argv[:2]==["docker","inspect"]: return json.dumps([{"Mounts":self.mounts}])
            if argv[0]=="du": return "1024 " + argv[-1]
            raise AssertionError(argv)
        self.runner.command=command
    def cache(self,owner="a"*64,legacy=False):
        root=(self.broker.work_dir if legacy else self.state)/"checks"/owner
        for name in ("target","cargo","actual-scratch"):
            p=root/name; p.mkdir(parents=True); (p/"keep").write_text("fixture")
            for e in (p/"keep",p): os.utime(e,(time.time()-8000,)*2)
        (root/"evidence.log").write_text("retained")
        return root
    def test_expiry_preserves_source_scratch_and_receipts(self):
        roots=[self.cache(),self.cache("legacy-task",True)]
        self.mounts=[{"Source":str(self.state.parent)}]
        self.broker.work["a"*64]={"status":"open"}
        self.assertEqual(len(self.broker.prune_check_caches()["removed"]),4)
        for root in roots:
            self.assertFalse((root/"target").exists()); self.assertFalse((root/"cargo").exists())
            self.assertTrue((root/"actual-scratch/keep").exists()); self.assertTrue((root/"evidence.log").exists())
        receipts=list((self.state/"cache-pruning").glob("*.json"))
        self.assertEqual(len(receipts),4)
        self.assertTrue(all(json.loads(p.read_text())["status"]=="removed" for p in receipts))
    def test_active_work_defers(self):
        root=self.cache(); self.processes="rustc"
        self.assertEqual(self.broker.prune_check_caches()["status"],"deferred")
        self.processes="python3"; self.broker.jobs["fixture"]={"status":"needs_decision"}
        self.assertEqual(self.broker.prune_check_caches()["status"],"deferred")
        self.assertTrue((root/"target").exists())
    def test_recent_activity_and_mounts_pin(self):
        root=self.cache(); self.broker.work["a"*64]={"last_activity_at":time.time()}
        self.assertEqual(self.broker.prune_check_caches()["removed"],[])
        self.broker.work.clear(); os.utime(root/"target/keep",None)
        self.mounts=[{"Source":str(root/"cargo")}]
        self.assertEqual(self.broker.prune_check_caches()["removed"],[])
    def test_symlink_is_preserved(self):
        root=self.cache(); shutil.rmtree(root/"target")
        (root/"target").symlink_to(root/"actual-scratch",target_is_directory=True)
        self.broker.prune_check_caches()
        self.assertTrue((root/"target").is_symlink()); self.assertTrue((root/"actual-scratch/keep").exists())

    def test_checkout_outputs_expire_and_evidence_remains(self):
        root=self.state.parent/"target"
        for name in ("debug","release","cargo-home","session-naming-check/debug"):
            p=root/name; p.mkdir(parents=True,exist_ok=True); (p/"cache").write_text("compiled")
            for e in (p/"cache",p): os.utime(e,(time.time()-8000,)*2)
        (root/"build-evidence.json").write_text("receipt")
        self.assertEqual(len(self.broker.prune_check_caches()["removed"]),4)
        self.assertTrue((root/"build-evidence.json").exists())
    def test_uncleared_terminal_source_is_preserved(self):
        root=self.broker.work_dir/"trees"/("b"*64)
        root.mkdir(parents=True); (root/"unique.rs").write_text("unique edits")
        self.broker.work["b"*64]={"status":"closed","job_id":"fixture"}
        self.broker.jobs["fixture"]={"status":"uncertain"}
        self.broker.prune_check_caches()
        self.assertTrue((root/"unique.rs").exists())
    def test_clearance_is_operator_only_and_requires_both_assertions(self):
        import copy
        request={"id":"retention-test","name":"v3_build_retention_clearance",
                 "arguments":{"notice_id":"a"*64,"unique_work_preserved":True,"effects_reconciled":True}}
        with self.assertRaises(PermissionError): self.broker.handle(dict(request,trusted=self.trusted,workspace=self.workspace))

    def test_clearance_fences_source_and_job_then_prunes_after_grace(self):
        from unittest.mock import patch
        host=fixtures.host
        key="b"*64; job_id="12345678-1234-1234-1234-123456789abc"
        root=self.broker.work_dir/"trees"/key; root.mkdir(parents=True)
        (root/"unique.rs").write_text("fixture source")
        artifact=self.state/job_id; artifact.mkdir(); (artifact/"log").write_text("fixture artifact")
        job={"job_id":job_id,"status":"uncertain","created_at":time.time()-9000,"finished_at":time.time()-9000}
        self.broker.jobs[job_id]=job
        self.broker.work[key]={"key":key,"task_id":"retention-fixture","status":"closed","job_id":job_id,
            "user_id":"admin","workspace":self.workspace,"created_at":time.time()-9000,"finished_at":time.time()-9000}
        self.broker._save_work()
        self.broker._reconcile_leftover_state()
        notice=self.broker.work[key]["leftover_notice"]["id"]
        trusted={"user_id":"codex-local","session_id":host.LOCAL_WORKSPACE,"local_caller":True}
        request={"id":"clear","name":"v3_build_retention_clearance","trusted":trusted,
            "workspace":host.LOCAL_WORKSPACE,"arguments":{"notice_id":notice,"unique_work_preserved":True,"effects_reconciled":True}}
        with patch.object(host.release,"snapshot",return_value=fixtures.context()):
            self.broker.handle(request)
            self.assertEqual(self.broker.prune_check_caches()["removed"],[])
            job["reason"]="changed"
            self.assertEqual(self.broker.prune_check_caches(now=time.time()+8000)["removed"],[])
            job.pop("reason")
        with patch.object(host.release,"snapshot",return_value=fixtures.context("changed")):
            self.assertEqual(self.broker.prune_check_caches(now=time.time()+8000)["removed"],[])
        with patch.object(host.release,"snapshot",return_value=fixtures.context()):
            removed=self.broker.prune_check_caches(now=time.time()+8000)["removed"]
        self.assertEqual(len(removed),2)
        self.assertFalse(root.exists()); self.assertFalse(artifact.exists())
        self.assertEqual(job["status"],"uncertain")
        self.assertIn(key,self.broker.work)
