"""Host-owned review ordering, exact evidence bindings and mandatory completion."""
import hashlib
import json
import sqlite3
import time
from pathlib import Path


# @code-index {"version":1,"feature":"approved-documentation-deferral","aliases":["post-deployment Code Index reviews"],"purpose":"Protected review order and exact sealed source bindings; defer only documentation assertions and require them before task completion. Config: build-service/scheduler-defaults.json -> documentation_review_order.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/documentation_review_order"]}]}}
class ReviewPolicy:
    """Publication freshness and functional checks remain in their existing host gates."""
    actions = {"approve_deferred_documentation", "finish_deferred_documentation"}

    def __init__(self, broker, release, asset_root):
        self.broker = broker
        self.release = release
        self.assets = Path(asset_root)

    def _binding(self, job, folder):
        """Verify the immutable pair before deriving approval or review bindings."""
        manifest = self.release.verify_publication(folder, job["job_id"], job["inputs_sha256"])
        if manifest.get("publication_mode") != "post_deployment_indexing_v1" and any(manifest[name] for name in ("gaps", "stale_count", "unindexed_count")):
            raise ValueError("Documentation deferral requires a complete fresh index")
        return {"job_id": job["job_id"], "inputs_sha256": job["inputs_sha256"],
                "source_sha256": manifest["source_sha256"],
                "source_generation": job.get("source_generation"),
                "ownership_epoch": job.get("ownership_epoch")}, manifest

    def allows(self, job, folder):
        """A rebase, descriptor change or ownership generation invalidates approval."""
        approval = job.get("deferred_documentation")
        if not approval or job.get("revoked"):
            return False
        binding, _manifest = self._binding(job, folder)
        if approval.get("binding") != binding:
            return False
        state = self.broker.scheduler.store.get(job["ownership_id"])
        return state["epoch"] == binding["ownership_epoch"] and state["generation"] == binding["source_generation"] and state["state"] == "owned"

    def defer_by_policy(self, job, folder, pending):
        """Bind post-deployment review obligations to this exact validated candidate.

        Config: scheduler.documentation_review_order is protected release policy.
        Functional/safety checks and complete fresh indexing run before this call.
        """
        order = self.broker.scheduler.config.get("scheduler", {}).get("documentation_review_order", "before_deployment")
        if order != "after_deployment" or not pending:
            return False
        binding, _manifest = self._binding(job, folder)
        with self.broker.lock:
            self.broker._lease(job)
            self.broker.scheduler.assert_job(job)
            state = self.broker.scheduler.store.get(job["ownership_id"])
            if state["state"] != "owned" or state["epoch"] != binding["ownership_epoch"] or state["generation"] != binding["source_generation"]:
                raise PermissionError("Post-deployment review ownership changed")
            previous = job.get("deferred_documentation", {})
            # Reuse an identical obligation; never carry completion across a rebase.
            if previous.get("binding") != binding:
                previous = {"binding": binding, "approved_at": time.time(),
                            "review_order_source": "protected_policy",
                            "deferred_checks": ["Code Index reviews", "documentation reviews"]}
            previous["pending_paths"] = list(pending)
            job["deferred_documentation"] = previous
            self.broker._save(job)
        return True

    def task_keys(self):
        """Reuse completed assertions only through the scanner's source/dependency checks."""
        with self.broker.lock:jobs=list(self.broker.jobs.values())
        return [job["ownership_id"] for job in jobs
                if job.get("deferred_documentation") and job.get("ownership_id")
                and job.get("status") == "completed"]

    def projection(self, folder, job, task_keys):
        """Project current assertions onto an in-memory copy; frozen indexes never change."""
        binding, manifest = self._binding(job, folder)
        blob, manifest = self.broker.code_index_ledger.scanner.read_published(folder, job["job_id"])
        if manifest["source_sha256"] != binding["source_sha256"]:raise ValueError("Post-deployment index source changed")
        conn = sqlite3.connect(":memory:")
        conn.row_factory = sqlite3.Row
        try:
            conn.deserialize(blob)
            original = [json.loads(row[0]) for row in conn.execute("SELECT body FROM maintenance_records")]
            evidence = self.broker.code_index_ledger.snapshot(list(dict.fromkeys(task_keys)))
            self.broker.code_index_ledger.scanner.project_maintenance(conn, original + evidence)
            rows = [json.loads(row[0]) for row in conn.execute("SELECT body FROM maintenance_records")]
        finally:
            conn.close()
        return binding, manifest, rows

    def pending(self, job, rows):
        """Retain deletion and rename reconciliation alongside live file reviews."""
        scanner = self.broker.code_index_ledger.scanner
        pending = []
        for path in job.get("changed_paths", []):
            logical = path.removeprefix("v3/")
            if Path(logical).suffix not in scanner.SUFFIXES:
                continue
            selected = [row for row in rows if row["file_path"].removeprefix("v3/") == logical]
            if not selected or any(row["status"] != "complete" for row in selected):
                pending.append(logical)
        return pending

    def require_complete(self, job):
        """Acceptance and claim release cannot forget approved post-deployment obligations."""
        if not job.get("deferred_documentation"):
            return None
        folder = self.assets / "releases" / job["job_id"]
        if not self.allows(job, folder):
            raise ValueError("Deferred documentation approval or ownership is stale")
        binding, manifest, rows = self.projection(folder, job, [job["ownership_id"]])
        pending = self.pending(job, rows)
        if pending:
            raise ValueError("Complete deferred Code Index/documentation reviews: " + ", ".join(pending[:8]))
        selected = [row for row in rows if row["file_path"].removeprefix("v3/") in
                    {path.removeprefix("v3/") for path in job.get("changed_paths", [])}]
        receipt = {"binding": binding, "index_sha256": manifest["index_sha256"],
                   "reviews_sha256": hashlib.sha256(json.dumps(selected, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
                   "completed_at": time.time(), "applicable_reviews": len(selected)}
        return receipt

    def check_complete(self, job):
        """Check the persisted completion cut without waiting on a ledger under host locks."""
        if job.get("post_deployment",{}).get("index_status") in {"queued","running","failed"}:
            raise ValueError("Complete post-deployment indexing before task completion")
        approval = job.get("deferred_documentation")
        if not approval:
            return
        receipt = approval.get("completion")
        binding = approval["binding"]
        if job.get("revoked") or not receipt or receipt.get("binding") != binding or receipt.get("review_revision") != job.get("documentation_review_revision", 0):
            raise ValueError("Complete deferred Code Index/documentation reviews before acceptance")
        if any(binding.get(name) != job.get(name) for name in ("job_id", "inputs_sha256", "source_generation", "ownership_epoch")):
            raise ValueError("Deferred documentation completion is stale")

    def complete_if_ready(self, job):
        """Derive completion from existing fresh assertions; never invent a review."""
        if not job.get("post_deployment")or not job.get("deferred_documentation"):return False
        try:
            with self.broker.lock:
                if job.get("revoked"):return False
                self.broker.scheduler.assert_job(job)
                revision=job.get("documentation_review_revision",0)
                if job["post_deployment"]["index_status"]!="indexed"or any(l.get("task_key")==job["ownership_id"]for l in self.broker.code_index_leases.values()):return False
            result=self.require_complete(job)
            with self.broker.lock:
                if job.get("revoked"):return False
                self.broker.scheduler.assert_job(job)
                if revision!=job.get("documentation_review_revision",0)or any(l.get("task_key")==job["ownership_id"]for l in self.broker.code_index_leases.values()):return False
                result["review_revision"]=revision
                job["deferred_documentation"]["completion"]=result
                self.broker._save(job)
            return True
        except (ValueError,PermissionError):
            return False

    def handle(self, request, args):
        """Record a deliberate user approval, never infer one from a build or resume."""
        required = {"action", "task_id", "job_id", "request_id"}
        approve = args.get("action") == "approve_deferred_documentation"
        if approve:
            required |= {"expected_inputs_sha256", "approval_note"}
        trusted = request["trusted"]
        workspace = request["workspace"]
        if set(args) != required or not trusted.get("local_caller"):
            raise PermissionError("Explicit operator-recorded review approval required")
        with self.broker.lock:
            self.broker._incoming_lease(trusted)
            item = self.broker.scheduler.item(args, trusted, workspace)
            job = self.broker.jobs.get(args["job_id"])
            if not job or job.get("ownership_id") != item["key"] or item.get("job_id") != job["job_id"] or not self.broker._owned(job, trusted, workspace):
                raise PermissionError("Latest owned candidate required")
            owner = self.broker.scheduler.owner(trusted, workspace)
            self.broker.scheduler.store.assert_owned(item["key"], owner, job["ownership_epoch"], [])
            journal = self.broker.scheduler.journal_path(owner, args["request_id"], "deferred-docs")
            digest = hashlib.sha256(json.dumps(args, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
            if journal.exists():
                previous = json.loads(journal.read_bytes())
                if previous["digest"] != digest:
                    raise ValueError("Documentation approval request_id reused")
                if previous.get("state") == "done":
                    if not approve:self.check_complete(job)
                    return previous["result"]
            if approve:
                note = args["approval_note"]
                if not isinstance(note, str) or not 1 <= len(note) <= 1000 or args["expected_inputs_sha256"] != job["inputs_sha256"]:
                    raise ValueError("Exact candidate and bounded user approval note required")
                if job["status"] != "needs_decision" or job["phase"] != "awaiting_prepared_reviews" or not self.broker.scheduler.review_wait(job, activity=True, authority_until=trusted["lease_until"]):
                    raise ValueError("Sealed candidate awaiting documentation review required")
                folder = Path(job["prepared_assets"])
            else:
                if not job.get("deferred_documentation") or job["status"] != "completed" or job.get("synchronization", {}).get("status") != "synchronized":
                    raise ValueError("Completed synchronized deployment required")
                folder = self.assets / "releases" / job["job_id"]
            review_revision = job.get("documentation_review_revision", 0)
            if not approve and any(lease.get("task_key") == item["key"] for lease in self.broker.code_index_leases.values()):
                raise ValueError("Wait for active task review recording before completion")
        # Index verification and multi-store reads cannot hold the authority lock.
        binding, _manifest = self._binding(job, folder)
        if approve:
            result = {"binding": binding, "approved_by": trusted["user_id"],
                      "approval_note": args["approval_note"], "approved_at": time.time(),
                      "deferred_checks": ["Code Index reviews", "documentation reviews"]}
        else:
            result = self.require_complete(job)
        with self.broker.lock:
            self.broker._incoming_lease(trusted)
            self.broker.scheduler.store.assert_owned(item["key"], owner, job["ownership_epoch"], [])
            if any(binding.get(name) != job.get(name) for name in ("job_id", "inputs_sha256", "source_generation", "ownership_epoch")):
                raise ValueError("Candidate changed during approval")
            if approve:
                if job["status"] != "needs_decision" or job["phase"] != "awaiting_prepared_reviews" or str(folder) != job.get("prepared_assets"):raise ValueError("Prepared candidate changed during approval")
                job["deferred_documentation"] = result
                self.broker._save(job)
            else:
                if review_revision != job.get("documentation_review_revision", 0) or any(lease.get("task_key") == item["key"] for lease in self.broker.code_index_leases.values()):
                    raise ValueError("Reviews changed while completing documentation")
                result["review_revision"] = review_revision
                job["deferred_documentation"]["completion"] = result
                self.broker._save(job)
            self.broker.scheduler.write_record(journal, {"digest": digest, "state": "done", "result": result})
        return result
