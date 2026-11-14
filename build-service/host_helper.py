"""Root host helper: approval-gated host actions behind a peer-checked Unix socket.

Runs as root (webagent-host-helper.service). Callers are identified only by SO_PEERCRED:
the app may request, list, decide and set allow-all windows; the operator account (ar, which
runs operator_shell.py for release keys) may request and read status. Nothing that changes
the host runs without an approved decision or a live allow-all window. Every request,
decision and result is appended to the log. Host-specific values come from the host profile.
"""
import json
import os
import pwd
import secrets
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path

PROFILE = Path(os.environ.get("WEBAGENT_HOST_PROFILE", "/etc/webagent/host-profile.json"))
DEFAULTS = {"host_id": socket.gethostname(), "socket": "/home/ar/webagent-dev3/runtime/host/helper.sock",
            "state": "/var/lib/webagent-host", "app_uid": 1100, "operator_account": "ar",
            "request_ttl_seconds": 1800, "allow_default_seconds": 3600, "allow_max_seconds": 86400,
            "snapshot_days": 30, "units": [], "containers": []}
# Undo classes: exact, best_effort or none. "read" actions change nothing and need no approval.
# Allow-all windows never cover "none" or boot/disk actions (step 3 adds the edit actions).
ACTIONS = {
    "host_status": {"read": True, "undo": "exact", "boot_disk": False},
    "noop": {"read": False, "undo": "exact", "boot_disk": False},
}
MAX_REQUEST = 256 * 1024


def load_profile(path=PROFILE):
    value = dict(DEFAULTS)
    if path.exists():
        value.update(json.loads(path.read_text()))
    return value


class Helper:
    def __init__(self, profile, clock=time.time, runner=subprocess.run):
        self.profile = profile
        self.clock = clock
        self.runner = runner
        self.state = Path(profile["state"])
        (self.state / "requests").mkdir(parents=True, exist_ok=True, mode=0o700)
        (self.state / "snapshots").mkdir(parents=True, exist_ok=True, mode=0o700)
        self.lock = threading.Lock()
        self.app_uid = profile["app_uid"]
        self.operator_uid = pwd.getpwnam(profile["operator_account"]).pw_uid

    # Durable records -----------------------------------------------------------------
    def log(self, event):
        with (self.state / "log.jsonl").open("a") as stream:
            stream.write(json.dumps({"at": self.clock(), "host_id": self.profile["host_id"], **event}, sort_keys=True) + "\n")

    def path(self, request_id):
        if not isinstance(request_id, str) or not request_id.isalnum() or len(request_id) != 32:
            raise ValueError("invalid request id")
        return self.state / "requests" / (request_id + ".json")

    def load(self, request_id):
        path = self.path(request_id)
        if not path.exists():
            raise ValueError("unknown request")
        record = json.loads(path.read_text())
        if record["state"] == "pending" and self.clock() >= record["expires_at"]:
            record.update(state="expired", decided_at=record["expires_at"])
            self.save(record)
            self.log({"event": "expired", "id": request_id})
        return record

    def save(self, record):
        path = self.path(record["id"])
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(record, sort_keys=True))
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)

    def allow_window(self, session):
        path = self.state / "allow.json"
        windows = json.loads(path.read_text()) if path.exists() else {}
        return windows.get(session or "", 0) > self.clock()

    # Actions ---------------------------------------------------------------------------
    def preview(self, action, args):
        """The exact effect shown on the approval card."""
        if action == "noop":
            return "Test request: changes nothing on the host."
        return "Read host status: uptime, disk use, listed units and containers."

    def execute(self, action, args):
        if action == "noop":
            return {"changed": False}
        def run(command):
            result = self.runner(command, capture_output=True, text=True, timeout=30)
            return result.stdout.strip()
        disk = shutil.disk_usage("/")
        return {"host_id": self.profile["host_id"], "uptime": run(["uptime", "-p"]),
                "disk": {"total": disk.total, "free": disk.free},
                "units": {u: run(["systemctl", "is-active", u]) for u in self.profile["units"]},
                "containers": {c: run(["docker", "inspect", "-f", "{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}", c])
                               for c in self.profile["containers"]}}

    def run_approved(self, record):
        try:
            record.update(state="done", result=self.execute(record["action"], record["args"]))
        except Exception as error:  # the log keeps the failure; the caller reads it through status
            record.update(state="failed", error=type(error).__name__ + ": " + str(error)[:300])
        record["finished_at"] = self.clock()
        self.save(record)
        self.log({"event": record["state"], "id": record["id"], "action": record["action"]})

    # Operations ------------------------------------------------------------------------
    def handle(self, uid, message):
        op = message.get("op")
        app = uid == self.app_uid
        operator = uid in (self.operator_uid, 0)
        if op == "request" and (app or operator):
            return self.request(message, "app" if app else "ssh")
        if op == "status" and (app or operator):
            record = self.load(message.get("id"))
            if not app and record["requester"]["channel"] != "ssh":
                raise PermissionError("not your request")
            return record
        if op == "list" and app:
            with self.lock:
                records = [self.load(p.stem) for p in sorted((self.state / "requests").glob("*.json"))]
            pending = [r for r in records if r["state"] == "pending"]
            recent = sorted((r for r in records if r["state"] != "pending"), key=lambda r: -r.get("decided_at", 0))[:50]
            return {"pending": pending, "recent": recent}
        if op == "decide" and app:
            return self.decide(message)
        if op == "allow" and app:
            return self.allow(message)
        raise PermissionError("operation not allowed for this caller")

    def request(self, message, channel):
        action, args = message.get("action"), message.get("args") or {}
        if action not in ACTIONS or not isinstance(args, dict):
            raise ValueError("unknown host action")
        requester = message.get("requester") or {}
        if not isinstance(requester, dict) or set(requester) - {"identity", "session", "user"}:
            raise ValueError("requester takes identity, session and user")
        spec = ACTIONS[action]
        now = self.clock()
        record = {"id": secrets.token_hex(16), "action": action, "args": args, "preview": self.preview(action, args),
                  "undo": spec["undo"], "boot_disk": spec["boot_disk"], "requester": {"channel": channel, **requester},
                  "state": "pending", "created_at": now, "expires_at": now + self.profile["request_ttl_seconds"]}
        with self.lock:
            self.save(record)
            self.log({"event": "requested", "id": record["id"], "action": action, "requester": record["requester"]})
            # An app request is covered by its chat session's window, an SSH request by "ssh:<identity>".
            key = requester.get("session") if channel == "app" else "ssh:" + str(requester.get("identity", ""))
            record["allow_key"] = key
            covered = spec["undo"] != "none" and not spec["boot_disk"] and self.allow_window(key)
            self.save(record)
            if spec["read"] or covered:
                record.update(state="approved", decided_at=now, decided_by="read-only" if spec["read"] else "allow-all")
                self.log({"event": "approved", "id": record["id"], "by": record["decided_by"]})
                self.run_approved(record)
        return record

    def decide(self, message):
        decision, admin = message.get("decision"), message.get("admin")
        if decision not in ("approve", "deny") or not isinstance(admin, str) or not admin:
            raise ValueError("decision approve or deny and the deciding admin required")
        with self.lock:
            record = self.load(message.get("id"))
            if record["state"] != "pending":
                raise ValueError("request is " + record["state"])
            record.update(state="approved" if decision == "approve" else "denied", decided_at=self.clock(), decided_by=admin)
            self.save(record)
            self.log({"event": record["state"], "id": record["id"], "by": admin})
            if decision == "approve":
                self.run_approved(record)
        return record

    def allow(self, message):
        session, seconds, admin = message.get("session"), message.get("seconds", self.profile["allow_default_seconds"]), message.get("admin")
        if not isinstance(session, str) or not session or not isinstance(admin, str) or type(seconds) is not int \
                or not 0 <= seconds <= self.profile["allow_max_seconds"]:
            raise ValueError("allow takes a session, an admin and seconds up to the configured maximum")
        with self.lock:
            path = self.state / "allow.json"
            windows = json.loads(path.read_text()) if path.exists() else {}
            windows = {k: v for k, v in windows.items() if v > self.clock()}
            if seconds:
                windows[session] = self.clock() + seconds
            else:
                windows.pop(session, None)
            path.write_text(json.dumps(windows))
            self.log({"event": "allow", "session": session, "seconds": seconds, "by": admin})
        return {"session": session, "until": windows.get(session)}


def serve(helper, path):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(path.parent, 0o755)
    path.unlink(missing_ok=True)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(path))
    os.chmod(path, 0o666)  # every peer is checked by uid below
    listener.listen(16)
    while True:
        connection, _ = listener.accept()
        threading.Thread(target=client, args=(helper, connection), daemon=True).start()


def client(helper, connection):
    with connection:
        try:
            connection.settimeout(30)
            _, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            raw = connection.makefile("rb").readline(MAX_REQUEST + 1)
            if len(raw) > MAX_REQUEST or not raw.endswith(b"\n"):
                raise ValueError("bounded request required")
            reply = {"status": "success", "output": helper.handle(uid, json.loads(raw))}
        except Exception as error:
            reply = {"status": "failed", "output": type(error).__name__ + ": " + str(error)[:300]}
        connection.sendall(json.dumps(reply, sort_keys=True).encode() + b"\n")


def main():
    if os.geteuid() != 0:
        sys.exit("host helper must run as root")
    os.umask(0o077)
    profile = load_profile()
    serve(Helper(profile), profile["socket"])


if __name__ == "__main__":
    main()
