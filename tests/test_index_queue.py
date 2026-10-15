"""Project index state uses real transactions and explicit host evidence."""
import concurrent.futures
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("index_queue", Path(__file__).resolve().parents[1] / "build-service/index_queue.py")
queue = importlib.util.module_from_spec(spec); spec.loader.exec_module(queue)


class ProjectIndexQueueTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.store = queue.IndexQueue(self.temp.name)
        self.owner = "sealed-owner"; self.project = "project-fixture"; self.sequence = 0
        self.call("index_project_create")

    def tearDown(self):
        self.temp.cleanup()

    def call(self, action, task=None, checkpoint=None, **extra):
        self.sequence += 1
        args = dict(action=action, project_id=self.project, request_id="project-request-" + str(self.sequence))
        if action != "index_project_create":
            args["expected_revision"] = self.store.status(self.owner, self.project)["revision"]
        args.update(extra)
        return self.store.mutate(self.owner, args, task, checkpoint)

    def checkpoint(self, component="one", source="a", epoch=1):
        task = {"key": "a" * 64, "task_id": "task-fixture"}
        self.call("index_project_attach", task=task)
        value = dict(task_key=task["key"], component=component, source_sha256=source * 64, dependency_sha256="b" * 64, generation=1, ownership_epoch=epoch, snapshot_id=source * 64, recipe="index-v1")
        return self.call("index_checkpoint", task=task, checkpoint=value)

    def test_identical_checkpoint_coalesces_and_unrelated_work_bypasses_failure(self):
        self.checkpoint(); self.checkpoint(); self.checkpoint("two", "c")
        rows = self.store.status(self.owner, self.project)["checkpoints"]; self.assertEqual(len(rows), 2)
        first = self.store.take(); self.store.finish(first["key"], reason="Needs corrected documentation")
        second = self.store.take(); self.assertNotEqual(first["key"], second["key"])
        self.store.finish(second["key"], result={"index_sha256": "d" * 64})
        self.assertIsNone(self.store.take())
        self.assertEqual([r["state"] for r in self.store.status(self.owner, self.project)["checkpoints"]], ["needs_attention", "prepared"])

    def test_restart_retains_prepared_evidence_and_fences_pending_authority(self):
        self.checkpoint(); first = self.store.take(); self.store.finish(first["key"], result={"index_sha256": "d" * 64})
        self.checkpoint("two", "c"); self.store.take(); self.checkpoint("three", "e")
        self.store = queue.IndexQueue(self.temp.name); self.store.recover()
        states = [r["state"] for r in self.store.status(self.owner, self.project)["checkpoints"]]
        self.assertEqual(states, ["prepared", "needs_attention", "needs_attention"]); self.assertIsNone(self.store.take())

    def test_finalize_seals_generation_and_reopen_retains_evidence(self):
        self.checkpoint(); item = self.store.take(); self.store.finish(item["key"], result={"index_sha256": "d" * 64})
        project = self.call("index_project_finalize")
        receipt = dict(release_id="release-fixture", source_sha256="a" * 64, index_sha256="d" * 64)
        done = self.store.finalized(self.owner, self.project, project["revision"], receipt=receipt)
        self.assertEqual(done["state"], "finalized")
        with self.assertRaisesRegex(ValueError, "Open project"):
            self.checkpoint("late", "e")
        opened = self.call("index_project_reopen"); self.assertEqual(opened["generation"], 2)
        self.assertEqual(len(self.store.status(self.owner, self.project)["checkpoints"]), 1)

    def test_finalization_missing_integration_or_review_remains_actionable(self):
        self.checkpoint(); project = self.call("index_project_finalize")
        result = self.store.finalized(self.owner, self.project, project["revision"], reasons=["Task is not published", "Review is incomplete"])
        self.assertEqual(result["state"], "needs_attention"); self.assertIsNone(result["final_receipt"])

    def test_owner_cas_and_exact_request_retry(self):
        args = dict(action="index_project_attach", project_id=self.project, expected_revision=1, request_id="fixed-project-request")
        task = {"key": "a" * 64, "task_id": "task-fixture"}
        first = self.store.mutate(self.owner, args, task); self.assertEqual(first, self.store.mutate(self.owner, args, task))
        with self.assertRaisesRegex(ValueError, "reused"):
            self.store.mutate(self.owner, {**args, "expected_revision": 2}, task)
        with self.assertRaisesRegex(ValueError, "revision"):
            self.store.mutate(self.owner, {**args, "request_id": "fresh-project-request"}, task)
        with self.assertRaisesRegex(ValueError, "Owned"):
            self.store.status("another-owner", self.project)

    def test_concurrent_membership_changes_require_fresh_revision(self):
        def attach(n):
            try:
                self.store.mutate(self.owner, dict(action="index_project_attach", project_id=self.project, expected_revision=1, request_id="concurrent-attach-" + str(n)), {"key": str(n) * 64, "task_id": "task-fixture-" + str(n)})
                return True
            except ValueError:
                return False
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            self.assertEqual(sum(pool.map(attach, [1, 2])), 1)

    def test_symlink_store_rejected(self):
        with tempfile.TemporaryDirectory() as other:
            link = Path(other) / "queue.sqlite"; link.symlink_to(self.store.path)
            with self.assertRaises(ValueError):
                queue.IndexQueue(other)

class ProjectIndexBrokerTests(unittest.TestCase):
    def setUp(self):
        import test_build_service as foundation
        import test_admin_build as fixtures
        import time
        from unittest.mock import patch
        self.fixtures=fixtures; self.foundation=foundation; self.serial=0
        self.policy=patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"scheduler":{**fixtures.host.CONFIG.get("scheduler",{}),"component_indexing":True,"ui_file_reservations":True}})
        self.policy.start(); foundation.MaintenanceTests.setUp(self)
        self.runner.blob=fixtures.context(extra={"v3/ui/a.js":"function one() {}\n","v3/ui/b.js":"function two() {}\n"})
        self.call("v3_build_work",{"action":"begin","task_id":"project-source-task","summary":"Component fixture","mode":"isolated"})
        item=next(i for i in self.broker.work.values()if i["task_id"]=="project-source-task")
        value=self.broker.scheduler.ensure(item,self.trusted,self.sid)
        self.call("build_work",{"action":"reserve","task_id":"project-source-task","paths":["ui/a.js","ui/b.js"],"expected_revision":value["revision"],"expected_epoch":value["epoch"],"request_id":"project-fixture-reserve"})
        self.project("index_project_create")
        self.project("index_project_attach",task_id="project-source-task")

    call = lambda self,*args,**kwargs: self.foundation.MaintenanceTests.call(self,*args,**kwargs)

    def tearDown(self):
        self.foundation.MaintenanceTests.tearDown(self); self.policy.stop()

    def project(self,action,**extra):
        self.serial+=1
        args={"action":action,"project_id":"broker-project-fixture","request_id":"broker-project-request-"+str(self.serial)}
        if action not in {"index_project_create","index_project_status"}:
            args["expected_revision"]=self.broker.index_coordinator.queue.status(self.broker.scheduler.owner(self.trusted,self.sid),args["project_id"])["revision"]
        args.update(extra)
        return self.call("build_work",args)

    def ready(self,count):
        import time
        deadline=time.monotonic()+5
        while time.monotonic()<deadline:
            value=self.project("index_project_status"); rows=value["checkpoints"]
            if len(rows)==count and all(r["state"]not in {"queued","running"}for r in rows):return value
            time.sleep(.01)
        self.fail("Checkpoint did not reach a bounded terminal state")

    def test_settled_component_scans_privately_and_never_certifies_reviews(self):
        self.project("index_checkpoint",task_id="project-source-task",component="ui/a.js")
        result=self.ready(1);row=result["checkpoints"][0]
        self.assertEqual(row["state"],"prepared",row)
        output=self.broker.index_coordinator.queue.folder/"outputs"/row["key"]
        self.assertTrue((output/"index.sqlite").is_file())
        reviews=self.call("code_index_maintenance",{"action":"list","task_id":"project-source-task","scope":"ui/a.js","limit":50})
        self.assertTrue(reviews["results"]);self.assertTrue(all(r["status"]!="complete"for r in reviews["results"]))
        final=self.project("index_project_finalize")
        self.assertEqual(final["state"],"needs_attention")

    def test_changed_component_does_not_block_an_unrelated_ready_component(self):
        import threading
        entered=threading.Event();proceed=threading.Event();original=self.broker.code_index_ledger.inventory
        def inventory(root,scope,folder):
            if scope=="ui/a.js":entered.set();proceed.wait(3)
            return original(root,scope,folder)
        self.broker.code_index_ledger.inventory=inventory
        self.project("index_checkpoint",task_id="project-source-task",component="ui/a.js")
        self.assertTrue(entered.wait(3))
        item=next(i for i in self.broker.work.values()if i["task_id"]=="project-source-task")
        (Path(item["workspace_path"])/"ui/a.js").write_text("function corrected() {}\n")
        self.project("index_checkpoint",task_id="project-source-task",component="ui/b.js")
        proceed.set();rows=self.ready(2)["checkpoints"]
        self.assertEqual([r["state"]for r in rows],["needs_attention","prepared"])

    def test_operator_authority_and_component_confinement(self):
        from unittest.mock import patch
        for path in ("../outside","data/config/private.json","/etc/passwd"):
            with self.assertRaises(ValueError):self.project("index_checkpoint",task_id="project-source-task",component=path)
        args={"action":"index_project_status","project_id":"broker-project-fixture"}
        with self.assertRaises(PermissionError):self.call("build_work",args,trusted={k:v for k,v in self.trusted.items()if k!="local_caller"})

    def test_host_revocation_fences_running_checkpoint_and_renewal_cannot_revive_it(self):
        import threading
        entered=threading.Event();proceed=threading.Event();original=self.broker.code_index_ledger.inventory
        def inventory(root,scope,folder):
            entered.set();proceed.wait(3)
            return original(root,scope,folder)
        self.broker.code_index_ledger.inventory=inventory
        self.project("index_checkpoint",task_id="project-source-task",component="ui/a.js")
        self.assertTrue(entered.wait(3))
        try:
            self.call("administrator_host_revoke",{})
            self.call("administrator_host_lease",{})
            self.assertTrue(all(lease["revoked"]for lease in self.broker.index_coordinator.leases.values()))
        finally:proceed.set()
        result=self.ready(1)["checkpoints"][0]
        self.assertEqual(result["state"],"needs_attention")
        self.assertIsNone(result["result"])
