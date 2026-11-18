"""Blocker board: a small host service where sessions leave notes on each other's broker tasks.

It runs apart from the broker so notes still work while the broker is down, restarting or
being installed. Callers connect to one Unix socket; the kernel names their Linux user
(SO_PEERCRED), which decides who they are: root is the broker, the app's user is the app
(it names the signed-in administrator), and the checkout owner and the release login are
local operators. Everyone else is refused. Notes are coordination text only: they never
change ownership, claims or jobs.

A note is open until it is retracted by its author, resolved, cleared as stale (with a
reason), archived because its task ended, or archived by the backstop after
backstop_seconds. Archived notes keep who removed them and why, are never returned by a
default read, and are deleted archive_seconds after archiving. A note cleared by someone
else leaves its author a one-time notice on their next read.
"""
import json
import os
import pwd
import socket
import sqlite3
import struct
import sys
import threading
import time
import uuid
from pathlib import Path

LIMIT = 64 * 1024
MAX_TEXT = 1000
MAX_REASON = 300
MAX_OPEN_PER_TASK = 20
MAX_OPEN = 500
DEFAULT_LIMIT = 10
BACKSTOP_SECONDS = 30 * 86400
ARCHIVE_SECONDS = 30 * 86400
LABEL_CHARS = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-")
HARNESSES = {"claude-code", "codex", "opencode"}


def label_ok(value):
    return isinstance(value, str) and 8 <= len(value) <= 80 and set(value) <= LABEL_CHARS


def text_ok(value, limit):
    return isinstance(value, str) and value.strip() and len(value) <= limit and "\0" not in value


class Board:
    """The store and every rule; the socket loop below only authenticates and frames requests."""

    def __init__(self, path, clock=time.time, backstop=BACKSTOP_SECONDS, archive=ARCHIVE_SECONDS):
        self.clock, self.backstop, self.archive = clock, backstop, archive
        self.lock = threading.Lock()
        self.db = sqlite3.connect(str(path), check_same_thread=False, isolation_level=None)
        self.db.row_factory = sqlite3.Row
        self.db.executescript("""
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS notes(id TEXT PRIMARY KEY, to_task TEXT NOT NULL, from_task TEXT,
                author TEXT NOT NULL, session TEXT, verified INTEGER NOT NULL, text TEXT NOT NULL, at REAL NOT NULL,
                archived_at REAL, removed_by TEXT, removal TEXT, reason TEXT);
            CREATE INDEX IF NOT EXISTS notes_open ON notes(to_task) WHERE archived_at IS NULL;
            CREATE TABLE IF NOT EXISTS notices(id TEXT PRIMARY KEY, task TEXT NOT NULL, text TEXT NOT NULL, at REAL NOT NULL);
            CREATE TABLE IF NOT EXISTS tasks(label TEXT PRIMARY KEY, session TEXT, seen REAL NOT NULL);
        """)

    @staticmethod
    def view(row, archived=False):
        keys = ["id", "to_task", "from_task", "author", "session", "verified", "text", "at"]
        if archived:
            keys += ["archived_at", "removed_by", "removal", "reason"]
        value = {key: row[key] for key in keys}
        value["verified"] = bool(value["verified"])
        return value

    def known(self, label):
        return self.db.execute("SELECT 1 FROM tasks WHERE label=?", (label,)).fetchone() is not None

    def post(self, author, request, trusted_task=False):
        to_task, text = request.get("to_task"), request.get("text")
        from_task = request.get("from_task")
        if not label_ok(to_task):
            raise ValueError("to_task must be a broker task label")
        if from_task is not None and not label_ok(from_task):
            raise ValueError("from_task must be a broker task label")
        if not text_ok(text, MAX_TEXT):
            raise ValueError(f"Note text is 1-{MAX_TEXT} characters")
        session = request.get("session") if isinstance(request.get("session"), str) else None
        with self.lock:
            opened = self.db.execute("SELECT count(*) FROM notes WHERE archived_at IS NULL AND to_task=?", (to_task,)).fetchone()[0]
            if opened >= MAX_OPEN_PER_TASK:
                raise ValueError(f"{to_task} already has {MAX_OPEN_PER_TASK} open notes; clear stale ones first")
            if self.db.execute("SELECT count(*) FROM notes WHERE archived_at IS NULL").fetchone()[0] >= MAX_OPEN:
                raise ValueError("The board is full; clear stale notes first")
            verified = trusted_task or (from_task is not None and self.known(from_task))
            note = {"id": uuid.uuid4().hex[:12], "to_task": to_task, "from_task": from_task, "author": author,
                    "session": (session or "")[:80] or None, "verified": int(verified), "text": text.strip(), "at": self.clock()}
            self.db.execute("INSERT INTO notes(id,to_task,from_task,author,session,verified,text,at) VALUES(?,?,?,?,?,?,?,?)",
                            tuple(note.values()))
        return {**note, "verified": bool(verified)}

    def list(self, request):
        """Open notes on `task` (newest `limit`, with a count of the rest); with `notices`, also that task's
        one-time notices, which the read consumes. Without `task`, open notes on every task. `archived`
        reads the archive instead."""
        task, limit = request.get("task"), request.get("limit", DEFAULT_LIMIT)
        if task is not None and not label_ok(task):
            raise ValueError("task must be a broker task label")
        if not isinstance(limit, int) or not 1 <= limit <= 200:
            raise ValueError("limit is 1-200")
        archived = request.get("archived") is True
        where = "archived_at IS NOT NULL" if archived else "archived_at IS NULL"
        args = ()
        if task:
            where += " AND (to_task=? OR from_task=?)" if archived else " AND to_task=?"
            args = (task, task) if archived else (task,)
        with self.lock:
            rows = self.db.execute(f"SELECT * FROM notes WHERE {where} ORDER BY at DESC LIMIT ?", args + (limit,)).fetchall()
            total = self.db.execute(f"SELECT count(*) FROM notes WHERE {where}", args).fetchone()[0]
            notices = []
            if task and not archived and request.get("notices") is True:
                notices = [row["text"] for row in self.db.execute("SELECT text FROM notices WHERE task=? ORDER BY at", (task,))]
                self.db.execute("DELETE FROM notices WHERE task=?", (task,))
        return {"notes": [self.view(row, archived) for row in reversed(rows)], "more": max(0, total - len(rows)), "notices": notices}

    def remove(self, caller, request):
        """retract (the author), resolve, or clear (anyone allowed, with a reason): the note moves to the archive."""
        note_id, kind, reason = request.get("id"), request.get("kind"), request.get("reason")
        if not isinstance(note_id, str) or kind not in {"retract", "resolve", "clear"}:
            raise ValueError("remove takes id and kind retract, resolve or clear")
        if kind == "clear" and not text_ok(reason, MAX_REASON):
            raise ValueError(f"Clearing someone's note needs a reason of 1-{MAX_REASON} characters")
        if reason is not None and not text_ok(reason, MAX_REASON):
            raise ValueError(f"reason is 1-{MAX_REASON} characters")
        with self.lock:
            row = self.db.execute("SELECT * FROM notes WHERE id=? AND archived_at IS NULL", (note_id,)).fetchone()
            if row is None:
                raise ValueError("No open note " + note_id)
            if kind == "retract" and row["author"] != caller:
                raise PermissionError("Only the note's author can retract it; clear it with a reason instead")
            self.archive_note(row, caller, kind, reason)
        return {"archived": note_id, "kind": kind}

    def archive_note(self, row, by, kind, reason):
        self.db.execute("UPDATE notes SET archived_at=?, removed_by=?, removal=?, reason=? WHERE id=?",
                        (self.clock(), by, kind, reason, row["id"]))
        # The author hears once when someone else removed their note, on their next read.
        if row["from_task"] and by != row["author"] and kind != "retract":
            text = f"Your note to {row['to_task']} was {'resolved' if kind == 'resolve' else 'cleared'} by {by}" + (f": {reason}" if reason else "")
            self.db.execute("INSERT INTO notices VALUES(?,?,?,?)", (uuid.uuid4().hex[:12], row["from_task"], text[:500], self.clock()))

    def tasks(self, request):
        """The broker's open task labels (and sessions): notes on any other task are archived as ended."""
        live = request.get("live")
        if not isinstance(live, list) or len(live) > 5000 or not all(isinstance(item, dict) and label_ok(item.get("label")) for item in live):
            raise ValueError("tasks takes live: [{label, session}]")
        now = self.clock()
        with self.lock:
            self.db.execute("DELETE FROM tasks")
            self.db.executemany("INSERT OR REPLACE INTO tasks VALUES(?,?,?)",
                                [(item["label"], str(item.get("session") or "")[:80] or None, now) for item in live])
            labels = {item["label"] for item in live}
            ended = [row for row in self.db.execute("SELECT * FROM notes WHERE archived_at IS NULL") if row["to_task"] not in labels]
            for row in ended:
                self.archive_note(row, "broker", "task-ended", "its task completed or was abandoned")
        return {"archived": len(ended), "tasks": len(labels)}

    def prune(self):
        """Backstop open notes into the archive, then delete archived notes past their keep time."""
        now = self.clock()
        with self.lock:
            old = self.db.execute("SELECT * FROM notes WHERE archived_at IS NULL AND at<?", (now - self.backstop,)).fetchall()
            for row in old:
                self.archive_note(row, "board", "expired", f"open for {self.backstop // 86400} days")
            deleted = self.db.execute("DELETE FROM notes WHERE archived_at IS NOT NULL AND archived_at<?", (now - self.archive,)).rowcount
            self.db.execute("DELETE FROM notices WHERE at<?", (now - self.archive,))
        return {"expired": len(old), "deleted": deleted}

    def import_legacy(self, path):
        """Notes the broker kept in its own board.json move here once; the file is renamed after."""
        path = Path(path)
        if not path.is_file():
            return 0
        try:
            notes = json.loads(path.read_text()).get("notes", [])
        except (OSError, ValueError):
            notes = []
        moved = 0
        with self.lock:
            for note in notes:
                if not label_ok(note.get("to_label")) or not text_ok(note.get("text"), MAX_TEXT):
                    continue
                origin = note.get("origin") or "broker"
                self.db.execute("INSERT OR IGNORE INTO notes(id,to_task,from_task,author,session,verified,text,at) VALUES(?,?,?,?,?,?,?,?)",
                                (str(note.get("id") or uuid.uuid4().hex[:12]), note["to_label"], note.get("from_label"),
                                 origin, note.get("from_session"), 1, note["text"], float(note.get("at") or self.clock())))
                moved += 1
        path.rename(path.with_name(path.name + ".imported"))
        return moved


def roles(cfg):
    """Linux user ID to caller kind: root is the broker, the app's user the app, the checkout owner and the
    release login local operators."""
    value = {0: "broker", cfg["app_uid"]: "app"}
    for name in (cfg.get("source_owner"), "webagent-ops"):
        try:
            value[pwd.getpwnam(name).pw_uid] = "operator"
        except (KeyError, TypeError):
            pass
    return value


def handle(board, kind, request):
    """One authenticated request. The caller's kind decides how its author is written and what it may do."""
    if not isinstance(request, dict):
        raise ValueError("Request must be an object")
    action = request.get("action")
    if kind == "broker":
        # The broker vouches for the author it names: a task it owns and its session.
        author = str(request.get("author") or "broker")[:80]
        trusted_task = request.get("verified") is True
    elif kind == "app":
        user = request.get("user")
        if not isinstance(user, str) or not 1 <= len(user) <= 64 or not set(user) <= LABEL_CHARS | {"."}:
            raise PermissionError("The app must name the signed-in administrator")
        author, trusted_task = "app/" + user, False
    else:
        origin = request.get("origin")
        author, trusted_task = "t480/" + origin if origin in HARNESSES else "t480", False
    if action == "post":
        return board.post(author, request, trusted_task)
    if action == "list":
        return board.list(request)
    if action == "remove":
        return board.remove(author, request)
    if action == "tasks":
        if kind != "broker":
            raise PermissionError("Only the broker reports its tasks")
        return board.tasks(request)
    raise ValueError("Unknown board action")


def serve(cfg, home, socket_path, legacy=None):
    home.mkdir(mode=0o700, parents=True, exist_ok=True)
    board = Board(home / "board.sqlite")
    if legacy:
        board.import_legacy(legacy)
    kinds = roles(cfg)
    socket_path.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    os.chmod(socket_path.parent, 0o755)
    socket_path.unlink(missing_ok=True)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(socket_path))
    # Anyone may connect; the peer's user decides whether anything is answered.
    os.chmod(socket_path, 0o666)
    listener.listen(16)

    def pruning():
        while True:
            try:
                board.prune()
            except sqlite3.Error:
                pass
            time.sleep(3600)

    threading.Thread(target=pruning, daemon=True).start()

    def answer(connection):
        try:
            _, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            kind = kinds.get(uid)
            if kind is None:
                raise PermissionError("This user may not use the board")
            raw = connection.makefile("rb").readline(LIMIT + 1)
            if len(raw) > LIMIT or not raw.endswith(b"\n"):
                raise ValueError("Request too large or unterminated")
            reply = {"status": "success", "output": handle(board, kind, json.loads(raw))}
        except (ValueError, PermissionError, KeyError, sqlite3.Error) as error:
            reply = {"status": "error", "error": str(error)[:300]}
        try:
            connection.sendall(json.dumps(reply).encode() + b"\n")
        except OSError:
            pass
        finally:
            connection.close()

    while True:
        connection, _ = listener.accept()
        connection.settimeout(5)
        threading.Thread(target=answer, args=(connection,), daemon=True).start()


def call(socket_path, request, timeout=5):
    """Client side: one request to the board socket; returns its output or raises its error."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect(str(socket_path))
        connection.sendall(json.dumps(request).encode() + b"\n")
        reply = json.loads(connection.makefile("rb").readline(LIMIT + 1) or b"{}")
    if reply.get("status") != "success":
        raise ValueError(reply.get("error") or "Board refused the request")
    return reply["output"]


def paths(cfg):
    """The board's home (its database), socket and the broker's former notes file for one installation.
    The socket sits beside the broker's runtime folder, in the runtime tree the app container mounts."""
    instance = cfg.get("instance", "development")
    return (Path("/var/lib/webagent") / instance / "board", Path(cfg["runtime_root"]).parent / "board" / "board.sock",
            Path(cfg["state_root"]) / "coordination" / "board.json")


if __name__ == "__main__":
    # Self-contained: the installed broker settings file is read directly, so a broker install that
    # changes config.py never restarts or breaks the board.
    settings = json.loads(Path(os.environ.get("WEBAGENT_BUILD_CONFIG", "/etc/webagent/development/build-service.json")).read_text())
    home, sock, legacy = paths(settings)
    serve(settings, home, sock, legacy=legacy)
