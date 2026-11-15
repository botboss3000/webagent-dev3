"""Root host helper: approval-gated host actions behind a peer-checked Unix socket.

Runs as root (webagent-host-helper.service). Callers are identified only by SO_PEERCRED:
the app may request, list, decide and set allow-all windows; the operator account (ar, which
runs operator_shell.py for release keys) may request and read status. Nothing that changes
the host runs without an approved decision or a live allow-all window. Every request,
decision and result is appended to the log. Host-specific values come from the host profile.
"""
import difflib
import getpass
import hashlib
import hmac
import fnmatch
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
            "snapshot_days": 30, "units": [], "containers": [],
            # file_write may touch only these paths; boot/disk paths also need password step-up.
            "editable": ["/etc/webagent/*"], "boot_disk_paths": ["/boot/*", "/etc/default/grub", "/etc/fstab", "/etc/crypttab"]}
# Undo classes: exact, best_effort or none. "read" actions change nothing and need no approval.
# Allow-all windows never cover "none" or boot/disk actions (step 3 adds the edit actions).
ACTIONS = {
    "host_status": {"read": True, "undo": "exact", "boot_disk": False},
    "noop": {"read": False, "undo": "exact", "boot_disk": False},
    "file_write": {"read": False, "undo": "exact", "boot_disk": False},
    "host_revert": {"read": False, "undo": "exact", "boot_disk": False},
    "unit_restart": {"read": False, "undo": "best_effort", "boot_disk": False},
    "container_restart": {"read": False, "undo": "best_effort", "boot_disk": False},
    "packages_upgrade": {"read": False, "undo": "best_effort", "boot_disk": False},
}
MAX_FILE = 1024 * 1024
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
    def matches(self, path, key):
        return any(fnmatch.fnmatch(path, pattern) for pattern in self.profile[key])

    def check(self, action, args):
        """Validate arguments against the profile; returns whether the action is a boot/disk change."""
        expected = {"file_write": {"path", "content"}, "host_revert": {"request_id"}, "unit_restart": {"unit"},
                    "container_restart": {"container"}}.get(action, set())
        if set(args) != expected:
            raise ValueError(action + " takes " + (", ".join(sorted(expected)) or "no arguments"))
        if action == "file_write":
            path = args["path"]
            if not isinstance(path, str) or not path.startswith("/") or os.path.normpath(path) != path or not isinstance(args["content"], str):
                raise ValueError("file_write needs an absolute normalized path and text content")
            if len(args["content"].encode()) > MAX_FILE:
                raise ValueError("file_write content is limited to 1 MiB")
            boot_disk = self.matches(path, "boot_disk_paths")
            if not boot_disk and not self.matches(path, "editable"):
                raise PermissionError("path is not editable under the host profile: " + path)
            if Path(path).is_symlink():
                raise PermissionError("file_write never follows a symlink")
            return boot_disk
        if action == "host_revert":
            target = self.load(args["request_id"])
            if target["action"] != "file_write" or target["state"] != "done" or target.get("reverted_by"):
                raise ValueError("only a completed, not yet reverted file_write can be reverted")
            return target["boot_disk"]
        if action == "unit_restart" and args["unit"] not in self.profile["units"]:
            raise PermissionError("unit is not listed in the host profile")
        if action == "container_restart" and args["container"] not in self.profile["containers"]:
            raise PermissionError("container is not listed in the host profile")
        return False

    def preview(self, action, args):
        """The exact effect shown on the approval card."""
        if action == "noop":
            return "Test request: changes nothing on the host."
        if action == "file_write":
            path = Path(args["path"])
            old = path.read_text(errors="replace") if path.is_file() else ""
            diff = "".join(difflib.unified_diff(old.splitlines(True), args["content"].splitlines(True),
                                                str(path) + (" (current)" if path.exists() else " (new file)"), str(path) + " (proposed)"))
            return diff or "No change: the file already has this content."
        if action == "host_revert":
            target = self.load(args["request_id"])
            return "Restore " + target["args"]["path"] + " to its content before request " + target["id"] + "."
        if action == "unit_restart":
            return "Restart systemd unit " + args["unit"] + "."
        if action == "container_restart":
            return "Restart container " + args["container"] + " (brief interruption)."
        if action == "packages_upgrade":
            return "apt-get update, then apt-get upgrade -y. The installed package list is saved first for a best-effort downgrade."
        return "Read host status: uptime, disk use, listed units and containers."

    def snapshot(self, request_id, path):
        """Save the current bytes and mode of path (or its absence) before a write."""
        folder = self.state / "snapshots" / request_id
        folder.mkdir(parents=True, mode=0o700)
        if path.exists():
            info = path.stat()
            shutil.copyfile(path, folder / "content")
            (folder / "meta.json").write_text(json.dumps({"path": str(path), "existed": True, "mode": info.st_mode & 0o7777,
                                                          "uid": info.st_uid, "gid": info.st_gid}))
        else:
            (folder / "meta.json").write_text(json.dumps({"path": str(path), "existed": False}))

    @staticmethod
    def write(path, data, mode=0o644, uid=None, gid=None):
        """Atomic replace in the same directory; a new file belongs to the helper (root)."""
        temporary = path.with_name("." + path.name + ".webagent-host")
        temporary.write_bytes(data)
        os.chown(temporary, os.geteuid() if uid is None else uid, os.getegid() if gid is None else gid)
        os.chmod(temporary, mode)
        os.replace(temporary, path)

    def execute(self, action, args, request_id=None):
        def run(command, timeout=30):
            result = self.runner(command, capture_output=True, text=True, timeout=timeout)
            if result.returncode:
                raise RuntimeError(" ".join(command[:3]) + " failed: " + (result.stderr or result.stdout)[-300:])
            return result.stdout.strip()
        if action == "noop":
            return {"changed": False}
        if action == "file_write":
            path = Path(args["path"])
            self.snapshot(request_id, path)
            if path.exists():
                info = path.stat()
                self.write(path, args["content"].encode(), info.st_mode & 0o7777, info.st_uid, info.st_gid)
            else:
                self.write(path, args["content"].encode())
            return {"path": str(path), "bytes": len(args["content"].encode()), "snapshot": request_id}
        if action == "host_revert":
            target = self.load(args["request_id"])
            folder = self.state / "snapshots" / target["id"]
            meta = json.loads((folder / "meta.json").read_text())
            path = Path(meta["path"])
            if meta["existed"]:
                self.write(path, (folder / "content").read_bytes(), meta["mode"], meta["uid"], meta["gid"])
            else:
                path.unlink(missing_ok=True)
            target["reverted_by"] = request_id
            self.save(target)
            return {"path": str(path), "restored_from": target["id"]}
        if action == "unit_restart":
            run(["systemctl", "restart", args["unit"]], 120)
            return {"unit": args["unit"], "state": run(["systemctl", "is-active", args["unit"]])}
        if action == "container_restart":
            run(["docker", "restart", args["container"]], 180)
            return {"container": args["container"]}
        if action == "packages_upgrade":
            folder = self.state / "snapshots" / request_id
            folder.mkdir(parents=True, mode=0o700)
            (folder / "packages.txt").write_text(run(["dpkg-query", "-W", "-f", "${Package}=${Version}\\n"]))
            run(["apt-get", "update"], 600)
            output = run(["env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "upgrade", "-y"], 3600)
            return {"upgraded": output[-1500:], "package_list": str(folder / "packages.txt")}
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
        """Run in the background; status reads show running until it settles."""
        record["state"] = "running"
        self.save(record)
        thread = threading.Thread(target=self.settle, args=(record,), daemon=True)
        thread.start()
        if record["action"] in ("noop", "host_status"):
            thread.join()
            record.update(self.load(record["id"]))

    def settle(self, record):
        try:
            record.update(state="done", result=self.execute(record["action"], record["args"], record["id"]))
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
        boot_disk = spec["boot_disk"] or self.check(action, args)
        now = self.clock()
        record = {"id": secrets.token_hex(16), "action": action, "args": args, "preview": self.preview(action, args),
                  "undo": spec["undo"], "boot_disk": boot_disk, "requester": {"channel": channel, **requester},
                  "state": "pending", "created_at": now, "expires_at": now + self.profile["request_ttl_seconds"]}
        with self.lock:
            self.save(record)
            self.log({"event": "requested", "id": record["id"], "action": action, "requester": record["requester"]})
            # An app request is covered by its chat session's window, an SSH request by "ssh:<identity>".
            key = requester.get("session") if channel == "app" else "ssh:" + str(requester.get("identity", ""))
            record["allow_key"] = key
            covered = spec["undo"] != "none" and not boot_disk and self.allow_window(key)
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
            # Boot and disk approvals need the host approval passphrase, checked here, never by the app.
            if decision == "approve" and record["boot_disk"]:
                self.check_passphrase(message.get("passphrase"))
            record.update(state="approved" if decision == "approve" else "denied", decided_at=self.clock(), decided_by=admin)
            self.save(record)
            self.log({"event": record["state"], "id": record["id"], "by": admin})
            if decision == "approve":
                self.run_approved(record)
        return record

    # Host approval passphrase -----------------------------------------------------------
    def passphrase_path(self):
        return self.state / "passphrase.json"

    def check_passphrase(self, given):
        """Verify against the salted scrypt hash; 5 wrong tries lock it for 15 minutes."""
        path = self.passphrase_path()
        if not path.exists():
            raise PermissionError("no host approval passphrase is set; set it at the server console first")
        stored = json.loads(path.read_text())
        now = self.clock()
        failures = [t for t in stored.get("failures", []) if t > now - 900]
        if len(failures) >= 5:
            raise PermissionError("too many wrong passphrases; try again in 15 minutes")
        digest = hashlib.scrypt(str(given or "").encode(), salt=bytes.fromhex(stored["salt"]), n=2 ** 14, r=8, p=1).hex()
        if not isinstance(given, str) or not hmac.compare_digest(digest, stored["hash"]):
            stored["failures"] = failures + [now]
            path.write_text(json.dumps(stored))
            self.log({"event": "passphrase_failed"})
            raise PermissionError("wrong host approval passphrase")
        if stored.get("failures"):
            stored["failures"] = []
            path.write_text(json.dumps(stored))

    def set_passphrase(self, value):
        if len(value) < 12:
            raise ValueError("use at least 12 characters")
        salt = secrets.token_bytes(16)
        digest = hashlib.scrypt(value.encode(), salt=salt, n=2 ** 14, r=8, p=1).hex()
        path = self.passphrase_path()
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps({"salt": salt.hex(), "hash": digest, "set_at": self.clock()}))
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
        self.log({"event": "passphrase_set"})

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
    if sys.argv[1:] == ["passphrase"]:
        # Console only: set or change the host approval passphrase (never over the app).
        first = getpass.getpass("New host approval passphrase: ")
        if first != getpass.getpass("Repeat it: "):
            sys.exit("the two entries differ; nothing changed")
        Helper(profile).set_passphrase(first)
        print("Host approval passphrase saved.")
        return
    serve(Helper(profile), profile["socket"])


if __name__ == "__main__":
    main()
