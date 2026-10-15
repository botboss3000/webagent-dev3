"""Partition mutable review evidence by task; keep publication snapshots immutable."""
import collections
import contextlib
import contextvars
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import time
import uuid

CATALOG = """
CREATE TABLE IF NOT EXISTS stores(task_key TEXT PRIMARY KEY,task_id TEXT NOT NULL,leaf TEXT NOT NULL UNIQUE,allocated INTEGER NOT NULL,record_count INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS routes(actor TEXT NOT NULL,request_id TEXT NOT NULL,digest TEXT NOT NULL,task_key TEXT NOT NULL,PRIMARY KEY(actor,request_id));
CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
INSERT OR IGNORE INTO metadata VALUES('budget','1');
"""
MAX_STORES = 1024
TABLE_ORDER = {"records":"entry_id", "events":"id", "receipts":"actor,request_id", "metadata":"key"}


def file_hash(path):
    """Hash bounded blocks without loading a database into operator memory."""
    h = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def durable_json(path, value):
    """Publish host-owned layout intent only after its bytes are durable."""
    path = Path(path)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as stream:
        json.dump(value, stream, sort_keys=True, separators=(",", ":"))
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def regular(path):
    """Reject substituted private state rather than following another inode."""
    value = Path(path).lstat()
    if not stat.S_ISREG(value.st_mode) or value.st_nlink != 1 or value.st_uid != os.geteuid() or value.st_mode & 0o077:
        raise ValueError("Invalid private review-store inode")
    return value


def directory(path):
    """Keep task labels out of private store paths and reject symlink parents."""
    path = Path(path)
    value = path.lstat()
    if not stat.S_ISDIR(value.st_mode) or value.st_uid != os.geteuid() or value.st_mode & 0o077:
        raise ValueError("Invalid private review-store directory")
    return path


def connect_file(path, ddl=None, readonly=False):
    """Open rollback-journal stores so attached allocation commits are atomic."""
    path = Path(path)
    if ddl is not None and not path.exists():
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        os.close(fd)
    regular(path)
    conn = sqlite3.connect("file:" + str(path) + "?mode=" + ("ro" if readonly else "rw"), uri=True, timeout=5)
    conn.row_factory = sqlite3.Row
    try:
        conn.execute("PRAGMA trusted_schema=OFF")
        if conn.execute("PRAGMA journal_mode").fetchone()[0] != "delete":
            raise ValueError("Atomic review allocation requires rollback journals")
        if ddl is not None:
            conn.executescript(ddl)
        return conn
    except BaseException:
        conn.close()
        raise


def row_bytes(row):
    """Bind every original SQLite cell, including compressed history bytes."""
    values = [["blob", v.hex()] if isinstance(v, bytes) else ["value", v] for v in row]
    return json.dumps(values, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"


# @code-index {"version":1,"feature":"partitioned-review-storage","aliases":["task review ledgers","review storage coordinator"],"purpose":"Host-owned per-task review transactions, global retry routes and bounded consistent publication reads. Existing aggregate ledger capacity remains enforced.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Task routing, global retry identity and atomic allocation are fixed integrity invariants; the existing protected ledger capacity remains authoritative."}}
def ledger_class(engine):
    """Retain the paired validation engine; change only persistence and routing."""
    base = engine.Ledger

    class PartitionedLedger(base):
        """Legacy-compatible facade with an explicit, lossless layout migration."""

        def __init__(self, folder, scanner=None, inventory=None, max_bytes=256 * 1024**2):
            """Never silently migrate a live database or reopen its rollback archive."""
            if type(max_bytes) is not int or not 4096 <= max_bytes <= 1024**3:
                raise ValueError("Invalid ledger byte budget")
            self._scope = contextvars.ContextVar("review_store_scope", default="published")
            self._held = contextvars.ContextVar("review_store_barrier", default=None)
            self.folder = Path(folder)
            self.layout_path = self.folder / "review-layout.json"
            self.pending_path = self.folder / "review-layout.pending.json"
            self.max_bytes = max_bytes
            self.scanner = scanner or engine.load_scanner()
            self.inventory = inventory or (lambda root, scope, folder: engine.source_inventory(self.scanner, root, scope, folder))
            self.layout = None
            if self.folder.exists():
                directory(self.folder)
            if self.pending_path.exists():
                self._finish_switch()
            if self.layout_path.exists():
                self.layout = self._read_layout(self.layout_path)
                self.path = self._root() / "coordinator.sqlite"
                regular(self.path)
                with self._catalog() as db:
                    if db.execute("SELECT COUNT(*) FROM stores").fetchone()[0] > MAX_STORES:
                        raise ValueError("Review-store registry bound exceeded")
            else:
                if (self.folder / "ledger.sqlite").is_dir():
                    raise ValueError("Partitioned review store requires a compatible broker")
                super().__init__(folder, scanner=self.scanner, inventory=self.inventory, max_bytes=max_bytes)

        def _read_layout(self, path):
            """Validate deterministic host layout names before resolving paths."""
            regular(path)
            if path.stat().st_size > 65536:
                raise ValueError("Review layout exceeds bound")
            value = json.loads(path.read_text())
            if value.get("version") != 1 or not re.fullmatch(r"reviews-v1-[a-f0-9]{32}", value.get("directory", "")) or not re.fullmatch(r"legacy-review-[a-f0-9]{32}\.sqlite", value.get("legacy_backup", "")):
                raise ValueError("Unsupported review layout")
            directory(self.folder / value["directory"])
            return value

        def _root(self):
            """Resolve the active protected coordinator and store namespace."""
            return directory(self.folder / self.layout["directory"])

        @contextlib.contextmanager
        def _barrier(self, exclusive=False):
            """Allow independent writers; fence only bounded multi-store snapshots."""
            held = self._held.get()
            if held is not None:
                if exclusive and not held:
                    raise ValueError("Cannot upgrade a live review transaction")
                yield
                return
            path = self.folder / "review-layout.lock"
            fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            token = None
            try:
                info = os.fstat(fd)
                if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.geteuid():
                    raise ValueError("Invalid review barrier inode")
                deadline = time.monotonic() + 5
                while True:
                    try:
                        fcntl.flock(fd, (fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH) | fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        if time.monotonic() >= deadline:
                            raise TimeoutError("Review snapshot transaction busy")
                        time.sleep(.01)
                token = self._held.set(exclusive)
                yield
            finally:
                if token is not None:
                    self._held.reset(token)
                os.close(fd)

        @contextlib.contextmanager
        def _using(self, key):
            """Route only a host-selected task key for the current request."""
            token = self._scope.set(key)
            try:
                yield
            finally:
                self._scope.reset(token)

        @contextlib.contextmanager
        def _catalog(self):
            """Keep coordinator writes short and enforce the aggregate byte ceiling."""
            with self._barrier():
                conn = connect_file(self._root() / "coordinator.sqlite")
                try:
                    yield conn
                    if conn.in_transaction:
                        pages = conn.execute("PRAGMA page_count").fetchone()[0] * conn.execute("PRAGMA page_size").fetchone()[0]
                        used = conn.execute("SELECT COALESCE(SUM(allocated),0) FROM stores").fetchone()[0]
                        if pages + used > self.max_bytes:
                            raise ValueError("Aggregate review storage budget would be exceeded")
                    conn.commit()
                except BaseException:
                    conn.rollback()
                    raise
                finally:
                    conn.close()

        def _store_path(self, key, create=False, task_id=None):
            """Register one opaque store without holding the coordinator through scans."""
            if not isinstance(key, str) or not 1 <= len(key.encode()) <= 512:
                raise ValueError("Bounded host review task key required")
            leaf = engine.digest(key)
            with self._catalog() as db:
                row = db.execute("SELECT leaf FROM stores WHERE task_key=?", (key,)).fetchone()
                if row:
                    if row[0] != leaf:
                        raise ValueError("Review-store route changed")
                    path = directory(self._root() / "stores" / leaf) / "ledger.sqlite"
                    regular(path)
                    return path
                if not create:
                    return None
                db.execute("BEGIN IMMEDIATE")
                row = db.execute("SELECT leaf FROM stores WHERE task_key=?", (key,)).fetchone()
                if row:
                    if row[0] != leaf:
                        raise ValueError("Review-store route changed")
                    path = directory(self._root() / "stores" / leaf) / "ledger.sqlite"
                    regular(path)
                    return path
                if db.execute("SELECT COUNT(*) FROM stores").fetchone()[0] >= MAX_STORES:
                    raise ValueError("Review-store registry bound exceeded")
                folder = self._root() / "stores" / leaf
                folder.mkdir(mode=0o700, exist_ok=True)
                directory(folder)
                path = folder / "ledger.sqlite"
                data = connect_file(path, engine.DDL)
                try:
                    if any(data.execute("SELECT COUNT(*) FROM " + name).fetchone()[0] for name in TABLE_ORDER):
                        raise ValueError("Unregistered review store contains evidence")
                finally:
                    data.close()
                for parent in (folder, folder.parent):
                    fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY)
                    try:os.fsync(fd)
                    finally:os.close(fd)
                db.execute("INSERT INTO stores VALUES(?,?,?,?,?)", (key, task_id or key, leaf, path.stat().st_size, 0))
                return path

        @contextlib.contextmanager
        def connect(self):
            """Atomically commit shard data and its shared allocation, without global scan locks."""
            if self.layout is None:
                with super().connect() as conn:
                    yield conn
                return
            with self._barrier():
                key = self._scope.get()
                path = self._store_path(key, create=False)
                if path is None:
                    raise ValueError("Review task store unavailable")
                conn = connect_file(path)
                try:
                    yield conn
                    if conn.in_transaction:
                        allocated = conn.execute("PRAGMA page_count").fetchone()[0] * conn.execute("PRAGMA page_size").fetchone()[0]
                        # ATTACH after the shard's BEGIN IMMEDIATE. Attaching earlier
                        # would reserve every database and serialize independent writers.
                        conn.execute("ATTACH DATABASE ? AS allocation", (str(self.path),))
                        conn.execute("UPDATE allocation.metadata SET value=value WHERE key='budget'")
                        previous = conn.execute("SELECT allocated,record_count FROM allocation.stores WHERE task_key=?", (key,)).fetchone()
                        if previous is None:
                            raise ValueError("Missing review allocation route")
                        used = conn.execute("SELECT COALESCE(SUM(allocated),0) FROM allocation.stores").fetchone()[0]
                        pages = conn.execute("PRAGMA allocation.page_count").fetchone()[0] * conn.execute("PRAGMA allocation.page_size").fetchone()[0]
                        if allocated + used - previous[0] + pages > self.max_bytes:
                            raise ValueError("Aggregate review storage budget would be exceeded")
                        count = conn.execute("SELECT COUNT(*) FROM records").fetchone()[0]
                        total_count = conn.execute("SELECT COALESCE(SUM(record_count),0) FROM allocation.stores").fetchone()[0]
                        if total_count - previous[1] + count > engine.MAX_RECORDS:
                            raise ValueError("Aggregate maintenance record bound exceeded")
                        conn.execute("UPDATE allocation.stores SET allocated=?,record_count=? WHERE task_key=?", (allocated, count, key))
                    conn.commit()
                except BaseException:
                    conn.rollback()
                    raise
                finally:
                    conn.close()

        def _rows(self, conn, key=None):
            """Read published inheritance from its own store, never another task's writer."""
            if self.layout is None or key == self._scope.get() or key is None:
                return super()._rows(conn, key)
            path = self._store_path(key)
            if path is None:
                return []
            other = connect_file(path, readonly=True)
            try:
                return super()._rows(other, key)
            finally:
                other.close()

        def mutate(self, args, actor, task_key, task_id, root, authorize=lambda: None):
            """Route global retry identities before independently validating and committing a task."""
            if self.layout is None:
                return super().mutate(args, actor, task_key, task_id, root, authorize)
            authorize()
            request = args.get("request_id")
            if args.get("action") not in {"sync", "claim", "record", "release"} or not isinstance(request, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", request):
                raise ValueError("Stable maintenance request_id required")
            self._store_path(task_key, create=True, task_id=task_id)
            who = engine.digest(actor)
            body = engine.digest([task_key, args])
            with self._catalog() as db:
                db.execute("BEGIN IMMEDIATE")
                previous = db.execute("SELECT digest,task_key FROM routes WHERE actor=? AND request_id=?", (who, request)).fetchone()
                if previous and tuple(previous) != (body, task_key):
                    raise ValueError("request_id reused with different maintenance input")
                db.execute("INSERT OR IGNORE INTO routes VALUES(?,?,?,?)", (who, request, body, task_key))
            with self._using(task_key):
                return super().mutate(args, actor, task_key, task_id, root, authorize)

        def list(self, args):
            """Preserve global filtering, pagination and history without exposing claim handles."""
            if self.layout is None:
                return super().list(args)
            allowed = {"action","root","task_id","task_key","scope","status","feature","offset","limit","entry_id"}
            if set(args) - allowed:
                raise ValueError("Unknown maintenance read field")
            scope = args.get("scope", "")
            self.scanner.relative_parts(scope, True)
            if args.get("action") == "get" and not args.get("entry_id"):
                raise ValueError("get requires entry_id")
            with self._barrier(exclusive=not bool(args.get("task_key") or args.get("task_id"))):
                with self._catalog() as db:
                    query = "SELECT task_key FROM stores"
                    values = ()
                    if args.get("task_key") is not None:
                        query += " WHERE task_key=?"
                        values = (args["task_key"],)
                    elif args.get("task_id"):
                        query += " WHERE task_id=?"
                        values = (args["task_id"],)
                    keys = [r[0] for r in db.execute(query + " ORDER BY task_key", values)]
                rows = []
                for key in keys:
                    with self._using(key), self.connect() as conn:
                        if args.get("entry_id"):
                            found = conn.execute("SELECT body FROM records WHERE task_key=? AND entry_id=?", (key, args["entry_id"]))
                            rows.extend(json.loads(r[0]) for r in found)
                        else:
                            rows.extend(super()._rows(conn, key))
                rows.sort(key=lambda r: (r["file_path"], r["unit_id"], r["task_key"]))
                rows = [r for r in rows if (not args.get("task_id") or r["task_id"] == args["task_id"]) and (not scope or r["file_path"] == scope or r["file_path"].startswith(scope.rstrip("/") + "/")) and (not args.get("status") or r["status"] == args["status"]) and (not args.get("feature") or args["feature"] in r["features"])]
                for row in rows:
                    if row.get("claim"):
                        row["claim"] = {k:v for k,v in row["claim"].items() if k != "id"}
                if args.get("action") == "get" and rows:
                    with self._using(rows[0]["task_key"]), self.connect() as conn:
                        history = []
                        for item in conn.execute("SELECT body FROM events WHERE entry_id=? ORDER BY id DESC LIMIT 8", (rows[0]["entry_id"],)):
                            event = engine.decode_event(item[0])
                            record = event["record"]
                            history.append({"event":event["event"], **{k:record.get(k) for k in ("revision","status","source_hash","updated_at","reviewer","remaining_work")}})
                        rows[0]["history"] = history
                return engine.page(rows, args.get("offset", 0), args.get("limit", 50))

        def snapshot(self, keys):
            """Capture a consistent cross-store cut; never hold this barrier during parsing."""
            if self.layout is None:
                return super().snapshot(keys)
            with self._barrier(exclusive=True):
                rows = []
                for key in dict.fromkeys(["published", *keys]):
                    if self._store_path(key) is not None:
                        with self._using(key), self.connect() as conn:
                            rows.extend(super()._rows(conn, key))
                return rows

        def compact_history(self, authorize=lambda: None):
            """Compact stores independently while retaining every event and retry receipt."""
            if self.layout is None:
                return super().compact_history(authorize)
            authorize()
            with self._barrier(exclusive=True):
                with self._catalog() as db:
                    stores = [tuple(r) for r in db.execute("SELECT task_key,leaf FROM stores ORDER BY task_key")]
                results = []
                for key, leaf in stores:
                    authorize()
                    ledger = base(self._root() / "stores" / leaf, scanner=self.scanner, max_bytes=self.max_bytes)
                    results.append({"task_key":key, **ledger.compact_history(authorize)})
                with self._catalog() as db:
                    for key, leaf in stores:
                        db.execute("UPDATE stores SET allocated=? WHERE task_key=?", ((self._root() / "stores" / leaf / "ledger.sqlite").stat().st_size, key))
                return {"layout":1, "stores":results, "history_preserved":True}

        def storage_status(self):
            """Expose bounded storage sizes, never review text or opaque claims."""
            if self.layout is None:
                return {"layout":0, "legacy_bytes":self.path.stat().st_size}
            with self._catalog() as db:
                stores = [dict(r) for r in db.execute("SELECT task_key,task_id,allocated FROM stores ORDER BY task_key")]
            return {"layout":1, "coordinator_bytes":self.path.stat().st_size, "store_bytes":sum(r["allocated"] for r in stores), "stores":stores, "aggregate_limit_bytes":self.max_bytes}

        def partition_storage(self, authorize=lambda: None, checkpoint=lambda _phase: None):
            """Prepare verified shards, then durably fence the legacy writer exactly once."""
            authorize()
            if self.layout is not None:
                return self.storage_status()
            with self._barrier(exclusive=True):
                token = uuid.uuid4().hex
                name = "reviews-v1-" + token
                output = self.folder / name
                output.mkdir(mode=0o700)
                (output / "stores").mkdir(mode=0o700)
                old_path = self.path
                old_hash = file_hash(old_path)
                source = connect_file(old_path, readonly=True)
                catalog = connect_file(output / "coordinator.sqlite", CATALOG)
                cache = collections.OrderedDict()
                expected = {}
                task_ids = {}
                def target(key, task_id):
                    """Route original task evidence with at most sixteen open SQLite handles."""
                    if not isinstance(key, str) or not 1 <= len(key.encode()) <= 512:
                        raise ValueError("Legacy review key invalid")
                    if key not in expected:
                        if len(expected) >= MAX_STORES:
                            raise ValueError("Review-store registry bound exceeded")
                        expected[key] = {table:[0, hashlib.sha256()] for table in TABLE_ORDER}
                        task_ids[key] = task_id
                    if key in cache:
                        cache.move_to_end(key)
                        return cache[key]
                    if len(cache) >= 16:
                        _, previous = cache.popitem(last=False)
                        previous.commit()
                        previous.close()
                    folder = output / "stores" / engine.digest(key)
                    folder.mkdir(mode=0o700, exist_ok=True)
                    conn = connect_file(folder / "ledger.sqlite", engine.DDL)
                    conn.execute("BEGIN")
                    cache[key] = conn
                    return conn
                def copy(table, row, key, task_id):
                    """Preserve every typed cell and accumulate its exact migration proof."""
                    conn = target(key, task_id)
                    conn.execute("INSERT INTO " + table + " VALUES(" + ",".join("?" for _ in row) + ")", tuple(row))
                    expected[key][table][0] += 1
                    expected[key][table][1].update(row_bytes(row))
                try:
                    def route(body, event):
                        """Group original rows by task while retaining their per-store order."""
                        value=engine.decode_event(body)["record"]if event else json.loads(body)
                        return value.get("record",value)["task_key"]
                    source.create_function("migration_event_task",1,lambda body:route(body,True),deterministic=True)
                    source.create_function("migration_receipt_task",1,lambda body:route(body,False),deterministic=True)
                    source.execute("BEGIN")
                    target("published", "published")
                    # Global opaque entry IDs interleave tasks. Group first to avoid
                    # reopening/fsyncing stores for almost every historical record.
                    grouped={"records":"task_key,entry_id","events":"migration_event_task(body),id",
                             "receipts":"migration_receipt_task(result),actor,request_id","metadata":"key"}
                    for table, order in grouped.items():
                        for n, row in enumerate(source.execute("SELECT * FROM " + table + " ORDER BY " + order)):
                            if table == "records":
                                key, task_id = row["task_key"], row["task_id"]
                                record = json.loads(row["body"])
                                if any(record[field] != row[field] for field in ("entry_id","task_key","task_id","file_path","unit_id","revision")):
                                    raise ValueError("Legacy review routing differs from its record")
                            elif table == "events":
                                record = engine.decode_event(row["body"])["record"]
                                key, task_id = record["task_key"], record["task_id"]
                            elif table == "receipts":
                                result = json.loads(row["result"])
                                record = result.get("record", result)
                                key, task_id = record["task_key"], record["task_id"]
                                catalog.execute("INSERT INTO routes VALUES(?,?,?,?)", (row["actor"], row["request_id"], row["digest"], key))
                            else:
                                key, task_id = "published", "published"
                            copy(table, row, key, task_id)
                            if n % 256 == 0:
                                authorize()
                    for conn in cache.values():
                        conn.commit()
                        conn.close()
                    cache.clear()
                    counts = {}
                    for key in expected:
                        leaf = engine.digest(key)
                        path = output / "stores" / leaf / "ledger.sqlite"
                        verify = connect_file(path, readonly=True)
                        try:
                            if verify.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                                raise ValueError("Migrated review store integrity failure")
                            counts[key] = {}
                            for table, order in TABLE_ORDER.items():
                                actual = hashlib.sha256()
                                count = 0
                                for row in verify.execute("SELECT * FROM " + table + " ORDER BY " + order):
                                    actual.update(row_bytes(row))
                                    count += 1
                                if count != expected[key][table][0] or actual.digest() != expected[key][table][1].digest():
                                    raise ValueError("Migrated review history or receipt changed")
                                counts[key][table] = count
                        finally:
                            verify.close()
                        catalog.execute("INSERT INTO stores VALUES(?,?,?,?,?)", (key, task_ids[key], leaf, path.stat().st_size, counts[key]["records"]))
                    catalog.commit()
                    size = sum((output / "stores" / engine.digest(key) / "ledger.sqlite").stat().st_size for key in expected) + (output / "coordinator.sqlite").stat().st_size
                    if size > self.max_bytes:
                        raise ValueError("Migration exceeds aggregate review storage budget")
                finally:
                    for conn in cache.values():
                        conn.close()
                    source.close()
                    catalog.close()
                authorize()
                if file_hash(old_path) != old_hash:
                    raise ValueError("Legacy ledger changed during partition preparation")
                files = {p.relative_to(output).as_posix():file_hash(p) for p in sorted(output.rglob("*.sqlite"))}
                for p in output.rglob("*.sqlite"):
                    with p.open("rb") as stream:
                        os.fsync(stream.fileno())
                for parent in sorted([output, *[p for p in output.rglob("*") if p.is_dir()]], key=lambda p:len(p.parts), reverse=True):
                    fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                    try:os.fsync(fd)
                    finally:os.close(fd)
                proof = {"source_sha256":old_hash, "files":files, "counts":counts, "history_and_receipts_preserved":True}
                durable_json(output / "migration.json", proof)
                layout = {"version":1, "directory":name, "legacy_backup":"legacy-review-" + token + ".sqlite"}
                durable_json(self.pending_path, layout)
                checkpoint("prepared")
                self._finish_switch(checkpoint)
                self.layout = self._read_layout(self.layout_path)
                self.path = self._root() / "coordinator.sqlite"
                return {**self.storage_status(), "history_preserved":True, "legacy_backup":layout["legacy_backup"]}

        def _finish_switch(self, checkpoint=lambda _phase: None):
            """Resume only a fully verified migration; an older broker fails closed on the fence."""
            layout = self._read_layout(self.pending_path)
            root = self.folder / layout["directory"]
            proof_path = root / "migration.json"
            regular(proof_path)
            if proof_path.stat().st_size > 4 * 1024**2:
                raise ValueError("Migration proof exceeds bound")
            proof = json.loads(proof_path.read_text())
            for name, value in proof["files"].items():
                if not (name == "coordinator.sqlite" or re.fullmatch(r"stores/[a-f0-9]{64}/ledger\.sqlite", name)):
                    raise ValueError("Invalid migration proof path")
                regular(root / name)
                if file_hash(root / name) != value:
                    raise ValueError("Prepared review migration changed")
            legacy = self.folder / "ledger.sqlite"
            backup = self.folder / layout["legacy_backup"]
            if legacy.is_file():
                regular(legacy)
                if backup.exists() or file_hash(legacy) != proof["source_sha256"]:
                    raise ValueError("Legacy migration source changed")
                os.replace(legacy, backup)
            regular(backup)
            if file_hash(backup) != proof["source_sha256"]:
                raise ValueError("Legacy review backup changed")
            checkpoint("legacy_moved")
            # A directory at the old database name prevents old code from opening
            # a fresh empty ledger or silently writing the archived generation.
            legacy.mkdir(mode=0o500, exist_ok=True)
            if legacy.is_symlink() or not legacy.is_dir():
                raise ValueError("Legacy review writer fence invalid")
            durable_json(self.layout_path, layout)
            checkpoint("layout_published")
            self.pending_path.unlink()
            fd = os.open(self.folder, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)

    return PartitionedLedger
