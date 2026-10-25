"""Host-owned review ordering and exact evidence; implementation completion is independent."""
import hashlib
import json
import sqlite3
import subprocess
import re
import uuid
import time
from pathlib import Path


# @code-index {"version":1,"feature":"approved-documentation-deferral","aliases":["post-deployment Code Index reviews"],"purpose":"Protected review order and exact sealed source bindings; defer documentation assertions into independent maintenance. Config: build-service/scheduler-defaults.json -> documentation_review_order.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/documentation_review_order"]}]}}
class ReviewPolicy:
    """Publication freshness and functional checks remain in their existing host gates."""
    actions = {"approve_deferred_documentation", "finish_deferred_documentation", "seed_deferred_renames"}

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

    def defer_by_policy(self):
        """Post-deployment policy publishes without recording documentation or review obligations.

        Config: scheduler.documentation_review_order is protected release policy.
        """
        return self.broker.scheduler.config.get("scheduler", {}).get("documentation_review_order", "before_deployment") == "after_deployment"

    def task_keys(self):
        """Reuse completed assertions only through the scanner's source/dependency checks."""
        with self.broker.lock:
            jobs=list(self.broker.jobs.values())
            successors=[item["successor"]["source_task_key"]for item in self.broker.work.values()
                if item.get("successor",{}).get("source_task_key")]
        # These are lookup candidates, never inherited approvals: projection checks exact
        # source, dependency, configuration and descriptor bindings for each assertion.
        return successors + (getattr(self.broker,"maintenance_activities",None).evidence_keys() if getattr(self.broker,"maintenance_activities",None) else []) + [job["ownership_id"] for job in jobs
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
        """Metadata never gates implementation acceptance or completion."""
        return None

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

    def rename_index_binding(self, job, folder):
        """Read the verified deferred index separately from the activation receipt."""
        binding, _publication = self._binding(job, folder)
        _blob, manifest = self.broker.code_index_ledger.scanner.read_published(folder, job["job_id"])
        if manifest["source_sha256"] != binding["source_sha256"] or not manifest.get("index_sha256"):
            raise ValueError("Deferred rename index source changed")
        return binding, manifest

    def removed_units(self, job):
        """Read every historical removed unit from the verified immutable index."""
        blob, _manifest = self.broker.code_index_ledger.scanner.read_published(
            self.assets / "releases" / job["job_id"], job["job_id"])
        conn = sqlite3.connect(":memory:")
        try:
            conn.deserialize(blob)
            conn.execute("PRAGMA trusted_schema=OFF")
            return [json.loads(row[0]) for row in conn.execute("SELECT body FROM maintenance_records")]
        finally:
            conn.close()

    def rename_rows(self, job, item, commit, renames, binding, manifest):
        """Bind missing removed-file summaries to a verified commit's parent.

        A retired task may have no baseline archive or historical ledger rows.
        The exact promoted commit is verified by the scheduler before this helper;
        its parent supplies immutable deletion preimages, never mutable checkout
        files. Replacements still require separate current completed reviews.
        """
        if not isinstance(renames, list) or not 1 <= len(renames) <= 32:
            raise ValueError("Bounded rename mappings required")
        def git(*args):
            result = subprocess.run(["git", "-C", str(self.broker.runner.source), *args],
                                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10)
            if result.returncode:
                raise ValueError("Verified rename preimage unavailable")
            return result.stdout
        parents = git("rev-list", "--parents", "-n", "1", commit).decode().split()
        if len(parents) != 2:
            raise ValueError("Single-parent verified release required")
        parent = parents[1]
        rows = self.broker.code_index_ledger.snapshot([item["key"]])
        targets = {r["entry_id"]: r for r in rows if r.get("task_key") == item["key"]}
        source = self.assets / "releases" / job["job_id"] / "v3"
        changed = {p.removeprefix("v3/") for p in job["changed_paths"] if p.startswith("v3/")}
        historical = self.removed_units(job)
        result = []
        seen = set()
        for mapping in renames:
            if not isinstance(mapping, dict) or set(mapping) != {"removed_path", "replacement_entry_id"}:
                raise ValueError("Exact rename mapping required")
            path = mapping["removed_path"]
            self.broker.code_index_ledger.scanner.relative_parts(path)
            if path in seen or path not in changed or Path(path).suffix not in self.broker.code_index_ledger.scanner.SUFFIXES:
                raise ValueError("Changed supported removed path required")
            seen.add(path)
            target = targets.get(mapping["replacement_entry_id"])
            if not target or target["removed"] or target["status"] != "complete" or target["unit_id"] != "__file__" or target["file_path"] not in changed or target["file_path"] == path:
                raise ValueError("Current reviewed changed replacement required")
            pre = git("ls-tree", parent, "--", path).decode().split()
            if len(pre) < 4 or pre[0] not in {"100644", "100755"} or pre[1] != "blob":
                raise ValueError("Regular committed rename preimage required")
            if int(git("cat-file", "-s", pre[2])) > self.broker.code_index_ledger.scanner.MAX_FILE_BYTES:
                raise ValueError("Rename preimage exceeds source bound")
            data = git("show", parent + ":" + path)
            if len(data) > self.broker.code_index_ledger.scanner.MAX_FILE_BYTES:
                raise ValueError("Rename preimage exceeds source bound")
            if git("ls-tree", commit, "--", path):
                raise ValueError("Rename source still exists in release commit")
            proof = {"binding": binding, "commit": commit, "parent_commit": parent,
                     "previous_source_hash": hashlib.sha256(data).hexdigest(),
                     "replacement_entry_id": target["entry_id"],
                     "replacement_source_hash": target["source_hash"],
                     "replacement_dependencies": target.get("dependencies", [])}
            row = {"entry_id": hashlib.sha256(json.dumps([item["key"], path, "__file__"], sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest(),
                   "task_key": item["key"], "task_id": item["task_id"], "file_path": path,
                   "unit_id": "__file__", "revision": 0, "status": "needs_review",
                   "checks": {}, "remaining_work": ["Review the verified file move and record explicit rename reconciliation"],
                   "claim": None, "previous_review": None, "source_hash": None,
                   "dependencies": [], "removed": True, "parser_status": "uncertain",
                   "label": "Removed file and verified rename preimage", "start_line": 1,
                   "end_line": max(1, len(data.splitlines())), "features": [],
                   "configuration_assessment": {"status": "unassessed", "references": [], "opportunities": []},
                   "scan": {"gaps": 0, "stale_count": 0, "unindexed_count": 0,
                            "index_sha256": manifest["index_sha256"], "verified_at": int(time.time())},
                   "rename_seed": proof}
            self.broker.code_index_ledger._fresh(source, row)
            self.broker.code_index_ledger._fresh(source, target)
            result.append(row)
            # A file summary cannot hide historical symbol-level obligations.
            # Recreate only units already present in this sealed release index.
            for prior in historical:
                if prior["file_path"].removeprefix("v3/") != path or prior["unit_id"] == "__file__":
                    continue
                if not prior.get("removed") or prior.get("source_hash") is not None:
                    raise ValueError("Historical rename unit is not removed")
                replacement = next((r for r in targets.values()
                    if r["file_path"] == target["file_path"] and r["unit_id"] == prior["unit_id"]), None)
                if not replacement or replacement["removed"] or replacement["status"] != "complete":
                    raise ValueError("Historical rename unit replacement requires review")
                self.broker.code_index_ledger._fresh(source, replacement)
                unit = {**row, "unit_id": prior["unit_id"],
                        "label": prior["label"], "start_line": prior["start_line"],
                        "end_line": prior["end_line"],
                        "entry_id": hashlib.sha256(json.dumps([item["key"], path, prior["unit_id"]], sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest(),
                        "rename_seed": {**proof, "replacement_entry_id": replacement["entry_id"],
                            "replacement_source_hash": replacement["source_hash"],
                            "replacement_dependencies": replacement.get("dependencies", [])}}
                result.append(unit)
        if len(result) > 512:
            raise ValueError("Historical rename inventory exceeds bound")
        return result

    def seed_renames(self, request, args):
        """Seed obligations only; normal attributed claims certify each rename later."""
        if set(args) != {"action", "task_id", "job_id", "request_id", "commit", "renames"} or not request["trusted"].get("local_caller"):
            raise PermissionError("Owner-scoped operator rename recovery required")
        if not isinstance(args["request_id"], str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", args["request_id"]):
            raise ValueError("Stable rename recovery request required")
        trusted = request["trusted"]; workspace = request["workspace"]
        with self.broker.lock:
            self.broker._incoming_lease(trusted)
            item = self.broker.scheduler.item(args, trusted, workspace)
            job = self.broker.jobs.get(args["job_id"])
            if not job or not self.broker._owned(job, trusted, workspace) or job.get("ownership_id") != item["key"] or item.get("job_id") != job["job_id"] or job["status"] != "completed" or not job.get("deferred_documentation"):
                raise PermissionError("Latest owned deferred release required")
            owner = self.broker.scheduler.owner(trusted, workspace)
            self.broker.scheduler.store.assert_owned(item["key"], owner, job["ownership_epoch"], [])
            if any(l.get("task_key") == item["key"] for l in self.broker.code_index_leases.values()):
                raise ValueError("Wait for active task maintenance")
            journal = self.broker.scheduler.journal_path(owner, args["request_id"], "deferred-renames")
            digest = hashlib.sha256(json.dumps(args, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
            if journal.exists():
                previous = json.loads(journal.read_bytes())
                if previous["digest"] != digest:
                    raise ValueError("Rename recovery request_id reused")
                if previous.get("state") == "done":
                    return previous["result"]
            lease = {**trusted, "workspace": workspace, "task_key": item["key"],
                     "deployed_job_id": job["job_id"], "revoked": False}
            lease_key = ("rename-recovery", uuid.uuid4().hex)
            self.broker.code_index_leases[lease_key] = lease
            job["documentation_review_revision"] = job.get("documentation_review_revision", 0) + 1
            job["deferred_documentation"].pop("completion", None)
            self.broker._save(job)
        try:
            # Verify the exact promoted commit, sealed candidate and current health.
            self.broker.scheduler.verified(item, {"job_id": job["job_id"], "commit": args["commit"],
                "acceptance": [{"check": "Verified immutable release commit for rename obligations", "passed": True}]}, deployment_only=True)
            binding, manifest = self.rename_index_binding(job, self.assets / "releases" / job["job_id"])
            if not self.allows(job, self.assets / "releases" / job["job_id"]):
                raise ValueError("Deferred rename source binding changed")
            rows = self.rename_rows(job, item, args["commit"], args["renames"], binding, manifest)
            actor = {"user_id": trusted["user_id"], "session_id": workspace, "transport": "operator"}
            result = self.broker.code_index_ledger.seed_removed(item["key"], rows, actor,
                self.assets / "releases" / job["job_id"] / "v3",
                authorize=lambda: self.broker._code_index_authorize(lease, item["key"]))
            with self.broker.lock:
                self.broker._incoming_lease(trusted)
                self.broker.scheduler.store.assert_owned(item["key"], owner, job["ownership_epoch"], [])
                self.broker.scheduler.write_record(journal, {"digest": digest, "state": "done", "result": result})
            return result
        finally:
            with self.broker.lock:
                self.broker.code_index_leases.pop(lease_key, None)

    def handle(self, request, args):
        """Record a deliberate user approval, never infer one from a build or resume."""
        if args.get("action") == "seed_deferred_renames":
            return self.seed_renames(request, args)
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
