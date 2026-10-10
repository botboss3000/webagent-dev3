import copy
import json
import time
import unittest
from unittest.mock import patch
import test_admin_build as fixtures
host = fixtures.host

class LeftoverTests(unittest.TestCase):
    tearDown = fixtures.BuildTests.tearDown
    call = fixtures.BuildTests.call
    def setUp(self):
        fixtures.BuildTests.setUp(self)
        self.task_key = "a" * 64
        self.item = {"task_id":"leftover-fixture","user_id":self.trusted["user_id"],
            "workspace":self.workspace,"status":"open","mode":"isolated",
            "summary":"Unique retained edits","created_at":time.time()-90000}
        self.broker.work[self.task_key] = self.item
        self.broker.work_activity[self.task_key] = self.broker._work_content(self.item)
        self.broker._save_work()
    def notices(self):
        return self.call("v3_build_leftovers")
    def test_deduplication_and_restart(self):
        first = self.notices()["notices"][0]
        self.assertEqual(first["status"],"stale_open")
        self.assertEqual(self.notices()["notices"][0]["id"],first["id"])
        self.broker.shutdown()
        self.broker = host.Broker(self.state,self.runner)
        self.assertEqual(self.notices()["notices"][0]["id"],first["id"])
        self.assertEqual(self.runner.calls,[])
    def test_foreign_owner_and_expired_lease(self):
        notice_id = self.notices()["notices"][0]["id"]
        other = dict(self.trusted,user_id="other-user")
        self.assertEqual(self.call("v3_build_leftovers",trusted=other)["total"],0)
        with self.assertRaises(PermissionError):
            self.call("v3_build_leftover_review",{"notice_id":notice_id},trusted=other)
        other = dict(self.trusted,session_id="11111111-1111-4111-8111-111111111111")
        self.assertEqual(self.call("v3_build_leftovers",trusted=other,workspace=other["session_id"])["total"],0)
        with self.assertRaises(ValueError):
            self.call("v3_build_leftovers",trusted=dict(self.trusted,lease_until=0))
    def test_exact_review_preserves_work_and_new_activity_changes_revision(self):
        notice_id = self.notices()["notices"][0]["id"]
        tree = self.broker.work_dir/"trees"/self.task_key
        tree.mkdir(parents=True)
        source = tree/"unique.txt"
        source.write_text("unpublished work")
        jobs = copy.deepcopy(self.broker.jobs)
        for _ in range(2):
            self.assertEqual(self.call("v3_build_leftover_review",{"notice_id":notice_id})["status"],"reviewed")
        self.assertEqual(self.notices()["total"],0)
        self.assertEqual(source.read_text(),"unpublished work")
        self.assertEqual(self.item["status"],"open")
        self.assertEqual(self.broker.jobs,jobs)
        self.item["summary"] = "New activity"
        self.broker._save_work()
        self.assertEqual(self.notices()["total"],0)
        with self.assertRaises(ValueError):
            self.call("v3_build_leftover_review",{"notice_id":notice_id})
        self.item["last_activity_at"] -= 90000
        self.broker._save_work()
        self.assertNotEqual(self.notices()["notices"][0]["id"],notice_id)
    def test_outcomes_require_reconciliation(self):
        self.item.update(status="closed",job_id="11111111-1111-4111-8111-111111111111")
        job = {"status":"failed","created_at":1,"finished_at":2}
        self.broker.jobs[self.item["job_id"]] = job
        self.broker._save_work()
        for status in ("failed","interrupted","uncertain","stale"):
            job["status"] = status
            self.assertEqual(self.notices()["notices"][0]["status"],status)
        job["operator_adopted_at"] = time.time()
        self.assertEqual(self.notices()["total"],0)
        job.pop("operator_adopted_at")
        for status in ("completed","cancelled","running","queued","needs_decision"):
            job["status"] = status
            self.assertEqual(self.notices()["total"],0)
    def test_abandon_and_policy_validation(self):
        first = self.notices()["notices"][0]["id"]
        policy = self.broker.work_dir/"leftover-notice-policy.json"
        policy.write_text(json.dumps({"stale_after_seconds":80000}))
        self.assertNotEqual(self.notices()["notices"][0]["id"],first)
        self.item["status"] = "abandoned"
        self.broker._save_work()
        self.assertEqual(self.notices()["total"],0)
        for invalid in ([],None,{"stale_after_seconds":True},{"stale_after_seconds":59},
                {"stale_after_seconds":86400,"delete":True}):
            policy.write_text(json.dumps(invalid))
            with self.assertRaises(ValueError): self.notices()
    def test_operator_fallback_and_bounded_space_accounting(self):
        self.item["user_id"] = "codex-local"
        self.broker.work_activity[self.task_key] = self.broker._work_content(self.item)
        self.broker._reconcile_leftover_state()
        row = self.broker._leftover_views(administrator=True)[0]
        self.assertEqual(row["origin"],"codex")
        self.assertIsNone(row["session_id"])
        (self.broker.work_dir/"trees"/self.task_key).mkdir(parents=True)
        with patch.object(self.runner,"command",create=True,return_value="1234\tfixture\n") as du:
            self.broker.reconcile_leftovers()
        self.assertEqual(du.call_args.kwargs["timeout"],2)
        self.assertEqual(self.item["leftover_notice"]["retained_bytes"],1234)

if __name__ == "__main__": unittest.main()
