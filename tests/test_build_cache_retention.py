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
            if argv[:3]==["docker","ps","-q"]: return "fixture"
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
