"""Exercise approval, exact freshness and completion through real broker entry points."""
import json
from pathlib import Path
import unittest
from unittest.mock import patch
import test_build_ui_publication as publication_fixtures
fixtures=publication_fixtures.fixtures


class DeferredDocumentationTests(unittest.TestCase):
    """Use disposable private UI candidates; no functional gate is mocked away."""
    setUp = publication_fixtures.ConcurrentUIEntryTests.setUp
    tearDown = publication_fixtures.ConcurrentUIEntryTests.tearDown
    call = publication_fixtures.ConcurrentUIEntryTests.call
    change = publication_fixtures.ConcurrentUIEntryTests.change
    review = publication_fixtures.ConcurrentUIEntryTests.review
    finish = publication_fixtures.ConcurrentUIEntryTests.finish
    wait = publication_fixtures.ConcurrentUIEntryTests.wait

    def pending_job(self):
        self.change("deferred-doc-task", "ui/probe/a.css", "body {}\n", "body { color: red; }\n", review=False)
        job = self.finish("deferred-doc-task")
        row = self.wait([job])[0]
        self.assertEqual(row["phase"], "awaiting_prepared_reviews", row.get("reason"))
        return job

    def approve(self, job, request="approve-docs-001", **updates):
        args = {"action": "approve_deferred_documentation", "task_id": "deferred-doc-task",
                "job_id": job, "request_id": request,
                "expected_inputs_sha256": self.broker.jobs[job]["inputs_sha256"],
                "approval_note": "Fixture: user reviewed this exact candidate and approved deferred documentation."}
        args.update(updates)
        return self.call("build_work", args)

    def resume(self, job):
        report = self.call("v3_build_work", {"action": "preflight", "task_id": "deferred-doc-task"})["synchronization"]
        self.call("v3_build_work", {"action": "decide", "task_id": "deferred-doc-task", "sync_decision": "proceed", "expected_sync_digest": report["digest"]})
        return self.wait([job])[0]

    def post_review(self, job):
        task = "deferred-doc-task"
        context = {"deployed_job_id": job}
        self.call("code_index_maintenance", {"action": "sync", **context, "task_id": task, "scope": "ui/probe/a.css", "request_id": "post-review-sync-001"})
        rows = self.call("code_index_maintenance", {"action": "list", "task_id": task, "scope": "ui/probe/a.css", "limit": 100})["results"]
        for n, row in enumerate(sorted(rows, key=lambda r: r["unit_id"] == "__file__")):
            claimed = self.call("code_index_maintenance", {"action": "claim", **context, "task_id": task,
                "entry_id": row["entry_id"], "expected_revision": row["revision"], "request_id": "post-claim-" + str(n)})["record"]
            self.call("code_index_maintenance", {"action": "record", **context, "task_id": task,
                "entry_id": row["entry_id"], "expected_revision": claimed["revision"],
                "source_hash": claimed["source_hash"], "claim_id": claimed["claim"]["id"],
                "status": "complete", "checks": {k: "verified" for k in ("documentation", "connections", "breadcrumbs", "configuration", "index_freshness")},
                "configuration_assessment": {"status": "not_applicable", "rationale": "Fixed immutable publication fixture"},
                "request_id": "post-record-" + str(n)})

    def complete(self, job, request="finish-deferred-001"):
        return self.call("build_work", {"action": "finish_deferred_documentation",
            "task_id": "deferred-doc-task", "job_id": job, "request_id": request})

    def test_explicit_approval_publishes_then_requires_reviews_before_acceptance(self):
        job = self.pending_job()
        approval = self.approve(job)
        self.assertEqual(approval, self.approve(job))
        self.assertEqual(self.resume(job)["status"], "completed")
        row = self.broker.jobs[job]
        with self.assertRaisesRegex(ValueError, "Complete deferred"):
            self.broker.review_policy.check_complete(row)
        with self.assertRaisesRegex(ValueError, "Complete deferred"):
            self.complete(job)
        frozen = (self.runner.assets / "releases" / job / ".code-index/index.sqlite").read_bytes()
        self.post_review(job)
        receipt = self.complete(job)
        self.assertGreater(receipt["applicable_reviews"], 0)
        self.broker.review_policy.check_complete(row)
        self.assertEqual(frozen, (self.runner.assets / "releases" / job / ".code-index/index.sqlite").read_bytes())
        self.assertFalse(any(call[0] in {"build", "stop", "start"} for call in self.runner.calls))

    def test_approval_rejects_nonoperator_stale_input_and_changed_retry(self):
        job = self.pending_job()
        with patch.dict(self.trusted, local_caller=False):
            with self.assertRaises(PermissionError):
                self.approve(job)
        with self.assertRaises(ValueError):
            self.approve(job, expected_inputs_sha256="0" * 64)
        self.approve(job)
        with self.assertRaisesRegex(ValueError, "request_id reused"):
            self.approve(job, approval_note="different approval")

    def test_integration_change_requires_new_approval(self):
        job = self.pending_job()
        self.approve(job)
        self.change("other-ui-task", "ui/probe/b.css", "main {}\n", "main { color: blue; }\n")
        other = self.finish("other-ui-task")
        self.assertEqual(self.wait([other])[0]["status"], "completed")
        self.assertEqual(self.resume(job)["phase"], "awaiting_prepared_reviews")
        self.assertEqual(self.runner.current_asset_id(), other)

    def test_approved_candidate_still_runs_required_validation(self):
        job = self.pending_job()
        self.approve(job)
        before = self.runner.current_asset_id()
        with patch.object(self.runner, "validate_ui", side_effect=ValueError("Required safety fixture failed")):
            row = self.resume(job)
        self.assertEqual(row["status"], "failed")
        self.assertEqual(self.runner.current_asset_id(), before)

    def test_postdeployment_reviews_do_not_take_publication_lock(self):
        job = self.pending_job()
        self.approve(job)
        self.assertEqual(self.resume(job)["status"], "completed")
        import concurrent.futures
        with self.broker.deploy_lock, concurrent.futures.ThreadPoolExecutor() as pool:
            pool.submit(self.post_review, job).result(timeout=4)
        self.complete(job)

    def test_review_mutation_invalidates_completion_and_completion_race_is_rejected(self):
        job = self.pending_job()
        self.approve(job)
        self.assertEqual(self.resume(job)["status"], "completed")
        self.post_review(job)
        self.complete(job)
        args = {"action": "sync", "task_id": "deferred-doc-task", "deployed_job_id": job,
                "scope": "ui/probe/a.css", "request_id": "invalidate-completion-001"}
        self.call("code_index_maintenance", args)
        with self.assertRaisesRegex(ValueError, "Complete deferred"):
            self.broker.review_policy.check_complete(self.broker.jobs[job])
        with self.assertRaisesRegex(ValueError, "Complete deferred"):
            self.complete(job)
        original = self.broker.review_policy.projection
        def race(*values):
            result = original(*values)
            self.call("code_index_maintenance", {**args, "request_id": "race-completion-sync-001"})
            return result
        with patch.object(self.broker.review_policy, "projection", race):
            with self.assertRaisesRegex(ValueError, "Reviews changed"):
                self.complete(job, "finish-after-race-001")
        self.complete(job, "finish-after-race-002")

    def test_postdeployment_reviews_require_original_owned_approved_candidate(self):
        job = self.pending_job()
        with self.assertRaises(PermissionError):
            self.call("code_index_maintenance", {"action": "sync", "task_id": "deferred-doc-task",
                "deployed_job_id": job, "request_id": "premature-post-sync-001"})
        self.approve(job)
        self.assertEqual(self.resume(job)["status"], "completed")
        self.call("administrator_host_revoke", {})
        with self.assertRaises(PermissionError):
            self.post_review(job)

    def test_partition_install_action_preserves_reviews_and_normal_publication(self):
        task="review-storage-maintenance"
        self.call("v3_build_work",{"action":"begin","task_id":task,"summary":"Disposable review migration fixture","mode":"isolated"})
        item=next(item for item in self.broker.work.values()if item["task_id"]==task)
        state=self.broker.scheduler.ensure(item,self.trusted,self.sid)
        self.call("build_work",{"action":"reserve","task_id":task,"paths":["build-service/review_store.py"],"expected_revision":state["revision"],"expected_epoch":state["epoch"],"request_id":"reserve-review-migration"})
        self.call("build_service_maintenance",{"action":"drain","task_id":task})
        args={"action":"partition_review_storage","task_id":task,"request_id":"partition-review-migration"}
        result=self.call("build_work",args)
        self.assertEqual(result["layout"],1)
        self.assertEqual(result,self.call("build_work",args))
        self.call("build_service_maintenance",{"action":"resume"})
        job=self.pending_job()
        self.approve(job)
        self.assertEqual(self.resume(job)["status"],"completed")
        self.post_review(job)
        self.complete(job)
        self.assertEqual(self.broker.code_index_ledger.storage_status()["layout"],1)

    def test_failed_storage_copy_retains_retry_without_fencing_source_edits(self):
        task="failed-review-migration"
        self.call("v3_build_work",{"action":"begin","task_id":task,"summary":"Disposable interrupted storage fixture","mode":"isolated"})
        item=next(item for item in self.broker.work.values()if item["task_id"]==task)
        state=self.broker.scheduler.ensure(item,self.trusted,self.sid)
        self.call("build_work",{"action":"reserve","task_id":task,"paths":["build-service/review_store.py"],"expected_revision":state["revision"],"expected_epoch":state["epoch"],"request_id":"reserve-failed-migration"})
        self.call("build_service_maintenance",{"action":"drain","task_id":task})
        args={"action":"partition_review_storage","task_id":task,"request_id":"retry-failed-migration"}
        with patch.object(self.broker.code_index_ledger,"partition_storage",side_effect=RuntimeError("Interrupted private copy")):
            with self.assertRaisesRegex(RuntimeError,"Interrupted private copy"):
                self.call("build_work",args)
        self.assertIsNone(self.broker.scheduler.pending(item["key"]))
        journals=list((self.broker.work_dir/"review-storage-requests").glob("*.json"))
        self.assertEqual(len(journals),1)
        self.assertEqual(json.loads(journals[0].read_bytes())["state"],"pending")
        self.assertEqual(self.call("build_work",args)["layout"],1)
        self.assertEqual(json.loads(journals[0].read_bytes())["state"],"done")

class DefaultPostDeploymentReviewTests(unittest.TestCase):
    """Protected default records exact obligations without a per-release approval."""
    setUp = DeferredDocumentationTests.setUp
    tearDown = DeferredDocumentationTests.tearDown
    call = DeferredDocumentationTests.call
    change = DeferredDocumentationTests.change
    finish = DeferredDocumentationTests.finish
    wait = DeferredDocumentationTests.wait
    post_review = DeferredDocumentationTests.post_review
    complete = DeferredDocumentationTests.complete


    def test_policy_default_publishes_then_requires_review_completion(self):
        self.broker.scheduler.config["scheduler"]["documentation_review_order"] = "after_deployment"
        self.change("deferred-doc-task", "ui/probe/a.css", "body {}\n", "body { color: red; }\n", review=False)
        job = self.finish("deferred-doc-task")
        self.assertEqual(self.wait([job])[0]["status"], "completed")
        row = self.broker.jobs[job]
        self.assertEqual(row["deferred_documentation"]["review_order_source"], "protected_policy")
        with self.assertRaisesRegex(ValueError, "Complete deferred"):
            self.broker.review_policy.check_complete(row)
        with self.assertRaisesRegex(ValueError, "Complete deferred"):
            self.complete(job)
        frozen = (self.runner.assets / "releases" / job / ".code-index/index.sqlite").read_bytes()
        self.post_review(job)
        self.complete(job)
        self.broker.review_policy.check_complete(row)
        self.assertEqual(frozen, (self.runner.assets / "releases" / job / ".code-index/index.sqlite").read_bytes())

    def test_policy_default_preserves_required_validation_failure(self):
        self.broker.scheduler.config["scheduler"]["documentation_review_order"] = "after_deployment"
        self.change("deferred-doc-task", "ui/probe/a.css", "body {}\n", "body { color: red; }\n", review=False)
        before = self.runner.current_asset_id()
        with patch.object(self.runner, "validate_ui", side_effect=ValueError("Required safety fixture failed")):
            job = self.finish("deferred-doc-task")
            self.assertEqual(self.wait([job])[0]["status"], "failed")
        self.assertEqual(self.runner.current_asset_id(), before)

    def test_review_order_configuration_rejects_unknown_values(self):
        configuration = fixtures.host._config_module
        value = configuration.defaults()
        value["scheduler"] = {**value["scheduler"], "documentation_review_order": "skip_reviews"}
        with self.assertRaisesRegex(ValueError, "documentation review order"):
            configuration.validate(value)
