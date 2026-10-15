"""Host-owned project batches and checkpoint queue; no source or publication authority."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import time


def canonical(value):
    """Bind persisted identities and receipts to structured values."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def identity(value):
    """Keep caller and component labels out of filesystem paths."""
    return hashlib.sha256(canonical(value).encode()).hexdigest()


# @code-index {"version":1,"feature":"project-index-queue","aliases":["component settled","index batch"],"purpose":"Persist owner-bound project generations and immutable component checkpoints. Authority and source snapshots are supplied by the protected host.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Ownership, snapshot identity and finalization are fixed integrity invariants."}}
class IndexQueue:
    """Short transactions retain evidence across task generations and broker restarts."""

    def __init__(self, folder):
        """Open a private bounded store without following caller-controlled links."""
        self.folder = Path(folder)
        if self.folder.is_symlink():
            raise ValueError("Index queue directory symlink")
        self.folder.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.path = self.folder / "queue.sqlite"
        with self.connect() as db:
            db.executescript("""
                CREATE TABLE IF NOT EXISTS projects(key TEXT PRIMARY KEY,owner TEXT NOT NULL,body TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS checkpoints(key TEXT PRIMARY KEY,project TEXT NOT NULL,state TEXT NOT NULL,body TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS receipts(key TEXT PRIMARY KEY,digest TEXT NOT NULL,body TEXT NOT NULL);
            """)
        self.path.chmod(0o600)

    @contextlib.contextmanager
    def connect(self):
        """Commit one bounded transaction, closing it before scans or review work."""
        if self.path.exists() and (self.path.is_symlink() or self.path.stat().st_nlink != 1 or self.path.stat().st_size > 128 * 1024**2):
            raise ValueError("Invalid index queue storage")
        db = sqlite3.connect(self.path, timeout=5)
        db.execute("PRAGMA trusted_schema=OFF")
        try:
            yield db
            if db.execute("PRAGMA page_count").fetchone()[0] * db.execute("PRAGMA page_size").fetchone()[0] > 128 * 1024**2:
                raise ValueError("Index queue storage budget exceeded")
            db.commit()
        except BaseException:
            db.rollback()
            raise
        finally:
            db.close()

    def _project(self, db, owner, project_id):
        """Resolve only a project in the authenticated caller's namespace."""
        if not isinstance(project_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", project_id):
            raise ValueError("Bounded project_id required")
        key = identity([owner, project_id])
        row = db.execute("SELECT body FROM projects WHERE key=? AND owner=?", (key, owner)).fetchone()
        if not row:
            raise ValueError("Owned index project unavailable")
        return json.loads(row[0])

    def _save(self, db, project):
        """Advance the project CAS revision without overwriting immutable receipts."""
        project["revision"] += 1
        db.execute("UPDATE projects SET body=? WHERE key=?", (canonical(project), project["key"]))
        return project

    def status(self, owner, project_id):
        """Expose bounded progress without source text, claim handles or arbitrary paths."""
        with self.connect() as db:
            project = self._project(db, owner, project_id)
            rows = [json.loads(row[0]) for row in db.execute("SELECT body FROM checkpoints WHERE project=? ORDER BY rowid", (project["key"],))]
        return {**project, "checkpoints": rows}

    def mutate(self, owner, args, task=None, checkpoint=None):
        """Persist host-validated membership or checkpoint intent exactly once."""
        request = args.get("request_id")
        if not isinstance(request, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", request):
            raise ValueError("Stable index request_id required")
        project_id = args.get("project_id")
        if not isinstance(project_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", project_id):
            raise ValueError("Bounded project_id required")
        key = identity([owner, project_id]); receipt = identity([owner, request]); digest = identity(args)
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            old = db.execute("SELECT digest,body FROM receipts WHERE key=?", (receipt,)).fetchone()
            if old:
                if old[0] != digest:
                    raise ValueError("Index request_id reused")
                return json.loads(old[1])
            action = args["action"]
            if action == "index_project_create":
                if db.execute("SELECT 1 FROM projects WHERE key=?", (key,)).fetchone():
                    raise ValueError("Index project already exists")
                if db.execute("SELECT COUNT(*) FROM projects").fetchone()[0] >= 1000:
                    raise ValueError("Index project limit exceeded")
                project = dict(key=key, project_id=project_id, revision=0, generation=1, state="open", members={}, final_receipt=None)
                db.execute("INSERT INTO projects VALUES(?,?,?)", (key, owner, canonical(project)))
            else:
                project = self._project(db, owner, project_id)
                if type(args.get("expected_revision")) is not int or project["revision"] != args["expected_revision"]:
                    raise ValueError("Index project revision conflict")
                if action == "index_project_reopen":
                    if project["state"] != "finalized":
                        raise ValueError("Finalized project required")
                    project.update(state="open", generation=project["generation"] + 1, final_receipt=None)
                elif action in {"index_project_attach", "index_checkpoint"}:
                    if project["state"] not in {"open", "needs_attention"}:
                        raise ValueError("Open project required")
                    if not task or not isinstance(task.get("key"), str):
                        raise ValueError("Host-validated task required")
                    if action == "index_project_attach":
                        if len(project["members"]) >= 128 and task["key"] not in project["members"]:
                            raise ValueError("Project membership limit exceeded")
                        project["members"][task["key"]] = task["task_id"]
                    else:
                        if task["key"] not in project["members"] or not checkpoint:
                            raise ValueError("Attach task before checkpoint")
                        if db.execute("SELECT COUNT(*) FROM checkpoints WHERE project=?", (key,)).fetchone()[0] >= 512:
                            raise ValueError("Project checkpoint limit exceeded")
                        ck = identity([key, checkpoint])
                        if not db.execute("SELECT 1 FROM checkpoints WHERE key=?", (ck,)).fetchone():
                            body = {**checkpoint, "key": ck, "project": key, "state": "queued", "created_at": time.time(), "result": None, "reason": None}
                            db.execute("INSERT INTO checkpoints VALUES(?,?,?,?)", (ck, key, "queued", canonical(body)))
                        previous = db.execute("SELECT body FROM checkpoints WHERE key=?", (ck,)).fetchone()
                        item = json.loads(previous[0])
                        if item["state"] == "needs_attention":
                            item.update(state="queued", reason=None, result=None)
                            db.execute("UPDATE checkpoints SET state='queued',body=? WHERE key=?", (canonical(item), ck))
                        project["latest_checkpoint"] = ck
                    project["state"] = "open"
                elif action == "index_project_finalize":
                    if project["state"] not in {"open", "needs_attention"}:
                        raise ValueError("Project cannot finalize")
                    if not project["members"]:
                        raise ValueError("Attach intended tasks before finalizing")
                    project["state"] = "finalizing"
                else:
                    raise ValueError("Unsupported index project action")
            result = self._save(db, project)
            db.execute("INSERT INTO receipts VALUES(?,?,?)", (receipt, digest, canonical(result)))
            return result

    def progress(self,key,waiting):
        """Expose content-free admission waits without adding fake release jobs."""
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row=db.execute("SELECT body FROM checkpoints WHERE key=? AND state='running'",(key,)).fetchone()
            if row:
                item=json.loads(row[0]);item["resource_waiting"]=waiting
                db.execute("UPDATE checkpoints SET body=? WHERE key=?",(canonical(item),key))

    def prepared(self,project=None,owner=None,task_keys=()):
        """Return bounded reusable checkpoints selected by host-derived project membership."""
        with self.connect() as db:
            rows=db.execute("SELECT c.body,p.owner,p.body FROM checkpoints c JOIN projects p ON p.key=c.project WHERE c.state='prepared' ORDER BY c.rowid DESC").fetchall()
            selected=[]
            for body,who,metadata in rows:
                item=json.loads(body);membership=json.loads(metadata)["members"]
                if project is not None and item["project"]!=project:continue
                if owner is not None and(who!=owner or not set(task_keys)&set(membership)):continue
                selected.append(item)
                if len(selected)>=64:break
            return selected

    def has_ready(self):
        """Avoid periodic polling while retaining work queued during worker shutdown."""
        with self.connect() as db:
            return db.execute("SELECT 1 FROM checkpoints WHERE state='queued' LIMIT 1").fetchone() is not None

    def take(self):
        """One broker worker takes ready work; blocked components never hold a global scan lock."""
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT key,body FROM checkpoints WHERE state='queued' ORDER BY rowid LIMIT 1").fetchone()
            if not row:
                return None
            item = json.loads(row[1]); item["state"] = "running"
            db.execute("UPDATE checkpoints SET state='running',body=? WHERE key=?", (canonical(item), row[0]))
            return item

    def finish(self, key, result=None, reason=None):
        """Save scan evidence only after host freshness and authority checks succeed."""
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT body FROM checkpoints WHERE key=?", (key,)).fetchone()
            if not row:
                raise ValueError("Checkpoint unavailable")
            item = json.loads(row[0])
            if item["state"] != "running":
                raise ValueError("Checkpoint is not running")
            item.update(state="needs_attention" if reason else "prepared", result=result, reason=str(reason)[:512] if reason else None)
            db.execute("UPDATE checkpoints SET state=?,body=? WHERE key=?", (item["state"], canonical(item), key))
            return item

    def recover(self):
        """A restart cannot replay source authority retained only by a prior process."""
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            rows = db.execute("SELECT key,body FROM checkpoints WHERE state IN ('queued','running')").fetchall()
            for key, body in rows:
                item = json.loads(body); item.update(state="needs_attention", reason="Broker restarted; capture a fresh authorized checkpoint")
                db.execute("UPDATE checkpoints SET state='needs_attention',body=? WHERE key=?", (canonical(item), key))
            for key, body in db.execute("SELECT key,body FROM projects").fetchall():
                project = json.loads(body)
                if project["state"] == "finalizing":
                    project.update(state="needs_attention", final_receipt=None); self._save(db, project)

    def finalized(self, owner, project_id, revision, receipt=None, reasons=None):
        """Seal only the generation the host verified against an exact published release."""
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            project = self._project(db, owner, project_id)
            if project["revision"] != revision or project["state"] != "finalizing":
                raise ValueError("Finalization generation changed")
            project.update(state="needs_attention" if reasons else "finalized", final_receipt=receipt if not reasons else None, remaining_work=list(reasons or [])[:32])
            if not reasons and (not isinstance(receipt, dict) or not all(receipt.get(k) for k in ("release_id", "source_sha256", "index_sha256"))):
                raise ValueError("Exact published finalization receipt required")
            return self._save(db, project)
