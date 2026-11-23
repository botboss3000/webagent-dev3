"""Root host helper: approval-gated host actions behind a peer-checked Unix socket.

Runs as root (webagent-host-helper.service). Callers are identified only by SO_PEERCRED:
the app may request, list, decide and set allow-all windows; the operator account (ar, which
runs operator_shell.py for release keys) may request and read status; root (the host operator
over SSH, `sudo buildctl host ...`) may list, decide and open windows without the app. Nothing
that changes the host runs without an approved decision or a live allow-all window. The app
approves a root-equivalent action ("root" below) or opens a window only with the host approval
passphrase, and only where the profile lists "app": approvers for those approvals,
window_openers for windows (HOSTACT-5). Every request, decision and result is appended to the log. Host-specific values
come from the host profile.
"""
import contextlib
import copy
import difflib
import fcntl
import getpass
import hashlib
import hmac
import fnmatch
import importlib.util
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
            # 0: a request waits for a decision indefinitely; a positive value expires it.
            "request_ttl_seconds": 0, "allow_default_seconds": 3600, "allow_max_seconds": 86400,
            "snapshot_days": 30, "units": [], "containers": [],
            # file_write may touch only these paths; boot/disk paths also need password step-up.
            "editable": ["/etc/webagent/*"], "boot_disk_paths": ["/boot/*", "/etc/default/grub", "/etc/fstab", "/etc/crypttab"],
            "build_config": "/etc/webagent/development/build-service.json",
            "helper_unit": "webagent-host-helper.service", "helper_dir": "/usr/local/lib/webagent-host",
            "unit_dir": "/etc/systemd/system", "health_seconds": 120,
            # broker_install drains after approval, then waits this long for running jobs to end.
            "drain_seconds": 3600,
            # files_grant / terminal_grant: how long an administrator may use the host file service
            # (host_files.py); the approver picks the time, up to the maximum.
            "grant_default_seconds": 3600, "grant_max_seconds": 86400, "files_unit": "webagent-host-files.service",
            # Who may open allow-all windows: "app" (Host Admin page, with the passphrase) and/or
            # "host" (root over SSH: sudo buildctl host allow). Development lists both; production
            # only "host", so no credential for a window ever passes through the app.
            "window_openers": ["host"],
            # Who may approve root-equivalent, boot/disk and File Manager grant requests: "app"
            # (Host Admin page, with the passphrase) and/or "host" (sudo buildctl host approve).
            # Development lists both; production only "host". Denials and ordinary approvals
            # (listed restarts, noop) are not limited by it.
            "approvers": ["host"],
            # Root-owned wrapper for the SSH route, installed with the helper by supervisor_update.
            "operator_command": "/usr/local/sbin/buildctl",
            # rotate_browser_login_token: the root-only stored copy of the Diagnostics browser
            # sign-in key (nothing reads it; the API container's environment is the live copy).
            "browser_login_env": "/etc/webagent/development/browser-login.env"}
CALLERS = ("app", "host")
# Grant actions and the file each one's live grants are kept in (host_files.py reads them too).
GRANTS = {"files_grant": "file-grants.json", "terminal_grant": "terminal-grants.json"}
FILES_UNIT = """[Unit]
Description=WebAgent root host file service (File Manager outside the checkout)
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/python3 -I {path}
Restart=on-failure
UMask=0077

[Install]
WantedBy=multi-user.target
"""
# The host operator's route that never passes through the app. It runs only the root-owned
# installed helper, never a checkout's buildctl.py (which the worker's group can write).
OPERATOR_COMMAND = """#!/bin/sh
# Installed by the WebAgent host helper (supervisor_update): sudo buildctl host pending|approve|deny|allow.
if [ "$1" != host ]; then
  echo "usage: sudo buildctl host pending | approve ID | deny ID | allow HOURS [--session KEY]" >&2
  exit 2
fi
shift
exec /usr/bin/python3 -I {path} operator "$@"
"""
# Undo classes: exact, best_effort or none. "read" actions change nothing and need no approval.
# Allow-all windows never cover "none" or boot/disk actions (step 3 adds the edit actions).
# "root" actions are root-equivalent: an app approval needs the passphrase unless a window
# (itself opened with the passphrase or over SSH) covers the request.
ACTIONS = {
    "host_status": {"read": True, "undo": "exact", "boot_disk": False},
    "noop": {"read": False, "undo": "exact", "boot_disk": False},
    "file_write": {"read": False, "undo": "exact", "boot_disk": False, "root": True},
    "host_revert": {"read": False, "undo": "exact", "boot_disk": False, "root": True},
    "unit_restart": {"read": False, "undo": "best_effort", "boot_disk": False},
    "container_restart": {"read": False, "undo": "best_effort", "boot_disk": False},
    "packages_upgrade": {"read": False, "undo": "best_effort", "boot_disk": False, "root": True},
    # Install supervisor: each runs in its own transient unit (it outlives a helper restart),
    # keeps the last-known-good copy, health-tests the result and reverts it on failure.
    "broker_install": {"read": False, "undo": "exact", "boot_disk": False, "root": True},
    "supervisor_update": {"read": False, "undo": "exact", "boot_disk": False, "root": True},
    # Lets one administrator use the File Manager outside the checkout until it expires or is
    # revoked. It is root-equivalent while live, so it always needs the host approval
    # passphrase and no allow-all window covers it.
    "files_grant": {"read": False, "undo": "exact", "boot_disk": False, "passphrase": True},
    # The same for a terminal in the File Manager: a shell as the operator account without the
    # docker group (sudo asks for its password).
    "terminal_grant": {"read": False, "undo": "exact", "boot_disk": False, "passphrase": True},
    # Replaces the API container's Diagnostics browser sign-in key with a fresh one made here.
    # Always an explicit decision: the passphrase from the app, or sudo buildctl host approve.
    "rotate_browser_login_token": {"read": False, "undo": "best_effort", "boot_disk": False, "root": True,
                                   "passphrase": True},
}
SUPERVISED = ("broker_install", "supervisor_update", "rotate_browser_login_token")
# The one variable rotate_browser_login_token replaces; its value never leaves the helper process.
BROWSER_LOGIN_TOKEN = "V3_BROWSER_LOGIN_TOKEN"
MAX_FILE = 1024 * 1024
MAX_REQUEST = 256 * 1024


def load_profile(path=PROFILE):
    value = dict(DEFAULTS)
    if path.exists():
        value.update(json.loads(path.read_text()))
    for key in ("window_openers", "approvers"):
        callers = value[key]
        if not isinstance(callers, list) or not set(callers) <= set(CALLERS):
            raise ValueError(key + " must list only " + " and ".join(CALLERS))
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
        self.grants_lock = threading.Lock()
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
        if record["state"] == "pending" and record.get("expires_at") and self.clock() >= record["expires_at"]:
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

    def windows(self):
        path = self.state / "allow.json"
        windows = json.loads(path.read_text()) if path.exists() else {}
        return {k: v for k, v in windows.items() if v > self.clock()}

    def allow_window(self, session):
        """Covered by this requester's window or the page-wide one (key "*")."""
        windows = self.windows()
        return bool(session) and session in windows or "*" in windows

    # Actions ---------------------------------------------------------------------------
    def matches(self, path, key):
        return any(fnmatch.fnmatch(path, pattern) for pattern in self.profile[key])

    def check(self, action, args):
        """Validate arguments against the profile; returns whether the action is a boot/disk change."""
        expected = {"file_write": {"path", "content"}, "host_revert": {"request_id"}, "unit_restart": {"unit"},
                    "container_restart": {"container"}, "broker_install": {"task_id", "tree_hash"},
                    "files_grant": {"admin"}, "terminal_grant": {"admin"}}.get(action, set())
        optional = {"allow_stale_tree"} if action == "broker_install" else set()
        if not expected <= set(args) <= expected | optional:
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
            if target["action"] not in ("file_write", "broker_install") or target["state"] != "done" or target.get("reverted_by"):
                raise ValueError("only a completed, not yet reverted file_write or broker_install can be reverted")
            return target["boot_disk"]
        if action == "broker_install":
            task, tree = args["task_id"], args["tree_hash"]
            if not isinstance(task, str) or not 0 < len(task) <= 80 or not all(c.isalnum() or c in "._-" for c in task):
                raise ValueError("broker_install needs a task id")
            if not isinstance(tree, str) or len(tree) != 64 or any(c not in "0123456789abcdef" for c in tree):
                raise ValueError("broker_install needs the exact 64-character tree hash")
            if not isinstance(args.get("allow_stale_tree", False), bool):
                raise ValueError("allow_stale_tree is true or false")
            if not (self.tree_root(tree) / "v3/build-service").is_dir():
                raise ValueError("no task tree with that hash")
        if action in GRANTS:
            admin = args["admin"]
            if not isinstance(admin, str) or not 0 < len(admin) <= 80 or not all(c.isalnum() or c in "._@-" for c in admin):
                raise ValueError(action + " needs the administrator's account name")
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
            if target["action"] == "broker_install":
                return ("Restore the broker release saved before request " + target["id"] + " ("
                        + Path(target["result"]["previous_release"]).name[:12] + ") and restart the broker. The broker must be idle.")
            return "Restore " + target["args"]["path"] + " to its content before request " + target["id"] + "."
        if action == "broker_install":
            current = Path(self.build_config()["service_dir"]).name
            return ("Install broker tree " + args["tree_hash"][:12] + " for task " + args["task_id"]
                    + (" (stale-tree override)" if args.get("allow_stale_tree") else "")
                    + ". Drains the broker once approved, waits for running jobs, stops and restarts it, then resumes. Saves the current release "
                    + current[:12] + " and restores it automatically if the health test fails.")
        if action == "supervisor_update":
            old, new = self.helper_path(), Path(self.build_config()["service_dir"]) / "host_helper.py"
            diff = "".join(difflib.unified_diff(old.read_text().splitlines(True), new.read_text().splitlines(True),
                                                str(old) + " (running)", str(new) + " (installed broker release)"))
            files = Path(self.build_config()["service_dir"]) / "host_files.py"
            running = self.files_path()
            files_diff = "".join(difflib.unified_diff(running.read_text().splitlines(True) if running.exists() else [],
                                                      files.read_text().splitlines(True) if files.exists() else [],
                                                      str(running) + " (running)", str(files) + " (installed broker release)"))
            return ("Replace the host helper (and the host file service) with the installed broker release's copies and "
                    "restart them; the previous helper is restored if it does not answer.\n\n"
                    + (diff[:20000] or "No change: the helper is already current.")
                    + ("\n\n" + files_diff[:20000] if files_diff else ""))
        if action in GRANTS:
            limit = "for the time you set when approving (" + str(round(self.profile["grant_default_seconds"] / 3600)) \
                + " h by default, at most " + str(round(self.profile["grant_max_seconds"] / 3600)) + " h)"
            if action == "terminal_grant":
                return ("Let administrator " + args["admin"] + " open terminals in the File Manager " + limit + ": a shell as "
                        + self.profile["operator_account"] + ", like a login on this computer (sudo asks for its password). "
                        "Revoke it here any time.")
            return ("Let administrator " + args["admin"] + " browse and change files outside the checkout through the "
                    "File Manager " + limit + ", with the file permissions of " + self.profile["operator_account"]
                    + "; anything needing root asks for its sudo password (app data and sockets stay locked). "
                    "Revoke it here any time.")
        if action == "rotate_browser_login_token":
            api = self.build_config().get("api_container", "the API container")
            return ("Rotate the Diagnostics development browser sign-in key (" + BROWSER_LOGIN_TOKEN + "). The helper makes "
                    "a new random key itself, saves it in " + self.profile["browser_login_env"] + " (root only, mode 600) and "
                    "recreates " + api + " with only that value changed: brief app interruption (about a minute). Refused "
                    "while a broker job runs; the broker is drained meanwhile. The old container stays stopped as "
                    + api + "-pre-rotate-<request id> and comes back if the new one is not healthy and ready. Nobody "
                    "sees the key: the result shows only the first 8 characters of its SHA-256.")
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
        if action == "host_revert" and self.load(args["request_id"])["action"] == "broker_install":
            target = self.load(args["request_id"])
            self.require_idle()
            self.restore_broker(self.state / "snapshots" / target["id"])
            target["reverted_by"] = request_id
            self.save(target)
            return {"restored_release": target["result"]["previous_release"], "health": self.broker_health([])}
        if action == "broker_install":
            return self.install_broker(args, request_id, run)
        if action == "supervisor_update":
            return self.update_helper(request_id)
        if action == "rotate_browser_login_token":
            return self.rotate_browser_login(request_id)
        if action in GRANTS:
            seconds = args.get("seconds", self.profile["grant_default_seconds"])
            with self.grants_lock:
                grants = self.grants(action)
                grants[args["admin"]] = self.clock() + seconds
                self.save_grants(action, grants)
            return {"admin": args["admin"], "until": grants[args["admin"]]}
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

    # Install supervisor ------------------------------------------------------------------
    def build_config(self):
        return json.loads(Path(self.profile["build_config"]).read_text())

    def tree_root(self, tree):
        return Path(self.build_config()["state_root"]) / "coordination/trees" / tree

    def helper_path(self):
        return Path(self.profile["helper_dir"]) / "host_helper.py"

    def jobs(self):
        """Job receipt states; the install must keep every one of them."""
        root = Path(self.build_config()["state_root"])
        return {p.name: json.loads(p.read_text()).get("status") for p in root.glob("*.json") if p.name != "maintenance.json"}

    def production_releases(self):
        """Production releases the broker is running (production.py): not queue jobs, but a broker
        install would cut them off, so they count as busy too."""
        root = Path(self.build_config()["state_root"]) / "production"
        found = {}
        for path in root.glob("*.json") if root.is_dir() else ():
            try:
                found["production " + path.stem[:8]] = json.loads(path.read_text()).get("status")
            except ValueError:
                continue
        return found

    def require_idle(self):
        busy = [name for name, status in self.jobs().items() if status in ("running", "needs_decision")]
        busy += [name for name, status in self.production_releases().items() if status in ("approved", "running")]
        if busy:
            raise RuntimeError("broker is busy (" + ", ".join(busy[:3]) + "); drain it and wait until idle")

    def broker_unit(self):
        return "webagent-build-" + self.build_config()["instance"] + ".service"

    def broker_health(self, before):
        """The unit is active, status answers through the installed CLI and no job receipt was lost or changed."""
        config = self.profile["build_config"]
        deadline, problem = self.clock() + self.profile["health_seconds"], "no answer"
        while self.clock() < deadline:
            active = self.runner(["systemctl", "is-active", self.broker_unit()], capture_output=True, text=True, timeout=30).stdout.strip()
            lifecycle = Path(self.build_config()["service_dir"]) / "lifecycle.py"
            answer = self.runner(["python3", "-B", str(lifecycle), "status", "--config", config],
                                 capture_output=True, text=True, timeout=60)
            if active == "active" and answer.returncode == 0:
                after = self.jobs()
                changed = [name for name, status in before.items() if after.get(name) != status]
                if changed:
                    return {"healthy": False, "problem": "job receipts changed: " + ", ".join(changed[:5])}
                return {"healthy": True, "unit": active, "jobs": len(after)}
            problem = "unit " + active + "; status: " + (answer.stderr or answer.stdout)[-200:]
            time.sleep(3)
        return {"healthy": False, "problem": problem}

    def restore_broker(self, folder):
        """Put back the saved build config and unit, then restart the broker on that release."""
        meta = json.loads((folder / "broker.json").read_text())
        # lifecycle.py writes the config 0644; the broker and buildctl read it as non-root users.
        self.write(Path(meta["config"]), (folder / "build-service.json").read_bytes(), 0o644)
        self.write(Path(meta["unit"]), (folder / "unit").read_bytes(), 0o644)
        self.runner(["systemctl", "daemon-reload"], check=True, timeout=60)
        self.runner(["systemctl", "restart", meta["unit_name"]], check=True, timeout=120)

    def maintenance(self, action, task_id=None):
        """Drain, resume or read (task_id None) the broker through its CLI as the operator; the broker
        socket admits only that uid."""
        command = ["runuser", "-u", self.profile["operator_account"], "--", "env",
                   "WEBAGENT_BUILD_CONFIG=" + self.profile["build_config"], "python3", "-B",
                   str(Path(self.build_config()["service_dir"]) / "buildctl.py"), "maintenance", action,
                   *(["--task-id", task_id] if task_id else [])]
        result = self.runner(command, capture_output=True, text=True, timeout=60)
        if result.returncode:
            raise RuntimeError("maintenance " + action + " failed: " + (result.stderr or result.stdout)[-300:])
        return result.stdout

    def install_broker(self, args, request_id, run):
        """Drain only once approved, so the broker keeps building while the request waits for a decision."""
        self.maintenance("drain", args["task_id"])
        try:
            deadline = self.clock() + self.profile["drain_seconds"]
            while True:
                try:
                    self.require_idle()
                    break
                except RuntimeError:
                    if self.clock() >= deadline:
                        raise
                    time.sleep(5)
            return self.install_drained(args, request_id, run)
        finally:
            # The installed (or restored) broker reads the same maintenance file; resume it either way.
            self.maintenance("resume", args["task_id"])

    def install_drained(self, args, request_id, run):
        config_path, unit_name = Path(self.profile["build_config"]), self.broker_unit()
        unit = Path(self.profile["unit_dir"]) / unit_name
        previous = self.build_config()["service_dir"]
        # Last-known-good: the config and unit that point at the running release.
        folder = self.state / "snapshots" / request_id
        folder.mkdir(parents=True, mode=0o700)
        shutil.copyfile(config_path, folder / "build-service.json")
        shutil.copyfile(unit, folder / "unit")
        (folder / "broker.json").write_text(json.dumps({"config": str(config_path), "unit": str(unit), "unit_name": unit_name,
                                                         "release": previous}))
        before = self.jobs()
        lifecycle = Path(previous) / "lifecycle.py"
        command = ["python3", "-B", str(lifecycle), "install", "--config", str(config_path), "--install-tree", args["tree_hash"]]
        run(["systemctl", "stop", unit_name], 120)
        try:
            output = run(command + (["--allow-stale-tree"] if args.get("allow_stale_tree") else []), 900)
        except Exception:
            # A refused install must never leave the broker stopped on a half-written release.
            self.restore_broker(folder)
            raise
        # lifecycle.py install leaves the unit stopped; start it before the health test.
        run(["systemctl", "start", unit_name], 120)
        health = self.broker_health(before)
        if not health["healthy"]:
            self.log({"event": "broker_reverted", "id": request_id, "problem": health["problem"]})
            self.restore_broker(folder)
            raise RuntimeError("health test failed (" + health["problem"] + "); restored release " + Path(previous).name[:12])
        installed = json.loads(output.splitlines()[-1])
        return {"installed_release": installed.get("version"), "previous_release": previous, "health": health}

    # Diagnostics browser sign-in key rotation --------------------------------------------
    def container_runner(self):
        """The installed broker release's container recreation (host.Runner), as reconfigure.py uses it.
        Its Docker create goes over the Docker socket, so no environment value is ever a process argument."""
        os.environ["WEBAGENT_BUILD_CONFIG"] = self.profile["build_config"]
        spec = importlib.util.spec_from_file_location("rotation_host", Path(self.build_config()["service_dir"]) / "host.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module.Runner()

    @contextlib.contextmanager
    def boot_effect_lease(self):
        """Hold the boot supervisor's effect lock, as deploys do, so it never restarts the container mid-swap."""
        path = Path(self.build_config()["install_root"]).parent / "boot-supervisor/effects.lock"
        if not path.exists():
            if path.with_name("ui.sock").exists():
                raise RuntimeError("the boot supervisor's effect lock is missing")
            yield
            return
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        try:
            deadline = self.clock() + 120
            while True:
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    if self.clock() >= deadline:
                        raise RuntimeError("the boot supervisor held its effect lock for 2 minutes") from None
                    time.sleep(1)
            yield
        finally:
            os.close(fd)

    def rotate_browser_login(self, request_id):
        """Refuse while a broker job runs; otherwise drain under this request's own label, swap, resume."""
        label = "token-rotate-" + request_id[:8]
        state = json.loads(self.maintenance("status") or "{}")
        if state.get("drained"):
            raise RuntimeError("not rotated: the broker is drained by " + str(state.get("task_id")) + "; ask again once it resumes")
        if state.get("busy"):
            raise RuntimeError("not rotated: a broker job is running; ask again when the broker is idle")
        try:
            self.require_idle()
        except RuntimeError as error:
            raise RuntimeError("not rotated: " + str(error)) from None
        self.maintenance("drain", label)
        resumed = True
        try:
            try:
                # A job that started between the first check and the drain also refuses.
                self.require_idle()
            except RuntimeError as error:
                raise RuntimeError("not rotated: " + str(error)) from None
            with self.boot_effect_lease():
                result = self.rotate_drained(request_id)
        finally:
            # A failed resume must not hide the outcome; it is logged and, after a rotation, reported.
            try:
                self.maintenance("resume", label)
            except Exception:
                resumed = False
                self.log({"event": "resume_failed", "id": request_id, "task_id": label})
        if not resumed:
            result["broker_resumed"] = False  # run: buildctl maintenance resume --task-id <label>
        return result

    @staticmethod
    def write_secret(path, data):
        """Atomic replace as root:root 0600; the temporary file is created 0600, never wider."""
        temporary = path.with_name("." + path.name + ".webagent-host")
        temporary.unlink(missing_ok=True)
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            if os.geteuid() == 0:
                os.fchown(fd, 0, 0)
            os.fchmod(fd, 0o600)
            os.write(fd, data)
            os.fsync(fd)
        finally:
            os.close(fd)
        os.replace(temporary, path)

    def rotate_drained(self, request_id):
        """Replace only BROWSER_LOGIN_TOKEN in the env file and the API container; restore both on failure.
        No message this raises carries the old or the new value."""
        runner = self.container_runner()
        name = self.build_config()["api_container"]
        before = runner.inspect(name)
        prefix = BROWSER_LOGIN_TOKEN + "="
        env = list(before["Config"].get("Env") or [])
        if sum(item.startswith(prefix) for item in env) != 1:
            raise RuntimeError("not rotated: " + name + " has no single " + BROWSER_LOGIN_TOKEN + "; the browser sign-in is not configured")
        old_value = next(item for item in env if item.startswith(prefix))[len(prefix):]
        new_value = secrets.token_hex(32)
        hidden = [old_value, new_value]
        runner.secrets.extend(hidden)
        env_path = Path(self.profile["browser_login_env"])
        if env_path.is_symlink():
            raise RuntimeError("not rotated: " + str(env_path) + " is a symlink")
        old_file = env_path.read_bytes() if env_path.exists() else None
        lines = (old_file or b"").decode().splitlines(True)
        if lines and not lines[-1].endswith("\n"):
            lines[-1] += "\n"
        line = (prefix + new_value + "\n").encode()
        index = next((i for i, item in enumerate(lines) if item.startswith(prefix)), None)
        new_file = b"".join(line if i == index else item.encode() for i, item in enumerate(lines)) if index is not None \
            else b"".join(item.encode() for item in lines) + line
        selected = copy.deepcopy(before)
        selected["Config"]["Env"] = [prefix + new_value if item.startswith(prefix) else item for item in env]
        backup = name + "-pre-rotate-" + request_id[:12]
        renamed = created = False
        try:
            self.write_secret(env_path, new_file)
            runner.stop(name)
            runner.rename(name, backup)
            renamed = True
            runner.create(name, before["Image"], selected)
            created = True
            runner.start(name)
            if not runner.healthy(name, before["Image"]) or not runner.release_healthy():
                raise RuntimeError("the new " + name + " did not become healthy and ready")
        except Exception as error:
            reason = self.hide(type(error).__name__ + ": " + str(error), hidden)
            outcome = self.restore_api(runner, name, backup, renamed, created, env_path, old_file, before["Image"], hidden)
            self.log({"event": "browser_login_restored", "id": request_id, "outcome": outcome})
            raise RuntimeError("not rotated (" + reason[:200] + "); " + outcome) from None
        return {"rotated": True, "at": self.clock(), "sha256_8": hashlib.sha256(new_value.encode()).hexdigest()[:8]}

    @staticmethod
    def hide(text, hidden):
        for value in hidden:
            text = text.replace(value, "[redacted]")
        return text

    def restore_api(self, runner, name, backup, renamed, created, env_path, old_file, image, hidden):
        """Put the previous container and env file back; returns a value-free outcome sentence."""
        try:
            if old_file is None:
                env_path.unlink(missing_ok=True)
            else:
                self.write_secret(env_path, old_file)
            if created:
                runner.command(["docker", "rm", "-f", name], timeout=60, private_raw=True)
            if renamed:
                try:
                    runner.inspect(name)
                    runner.command(["docker", "rm", "-f", name], timeout=60, private_raw=True)
                except Exception:
                    pass  # no new container under the name: nothing to clear
                runner.rename(backup, name)
            runner.start(name)
            healthy = runner.healthy(name, image) and runner.release_healthy()
            return "previous container restored" + (" and healthy" if healthy else " but not yet healthy: check " + name)
        except Exception as error:
            return ("restoring the previous container failed (" + self.hide(type(error).__name__ + ": " + str(error), hidden)[:150]
                    + "); it is kept as " + (backup if renamed else name))

    def helper_answers(self):
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
                probe.settimeout(5)
                probe.connect(self.profile["socket"])
                probe.sendall(json.dumps({"op": "status", "id": "0" * 32}).encode() + b"\n")
                return probe.makefile("rb").readline().startswith(b"{")
        except OSError:
            return False

    def files_path(self):
        return Path(self.profile["helper_dir"]) / "host_files.py"

    def files_unit_path(self):
        return Path(self.profile["unit_dir"]) / self.profile["files_unit"]

    def update_files_service(self, folder):
        """Install or refresh host_files.py and its unit from the installed broker release; False if current."""
        source = Path(self.build_config()["service_dir"]) / "host_files.py"
        if not source.exists():
            return False
        new, target, unit = source.read_bytes(), self.files_path(), self.files_unit_path()
        unit_text = FILES_UNIT.format(path=target).encode()
        if target.exists() and target.read_bytes() == new and unit.exists() and unit.read_bytes() == unit_text:
            return False
        if target.exists():
            (folder / "host_files.py").write_bytes(target.read_bytes())
        self.write(target, new, 0o644)
        self.write(unit, unit_text, 0o644)
        self.runner(["systemctl", "daemon-reload"], timeout=60)
        self.runner(["systemctl", "enable", self.profile["files_unit"]], timeout=60)
        self.runner(["systemctl", "restart", self.profile["files_unit"]], timeout=60)
        return True

    def update_operator_command(self):
        """Install the root-owned `buildctl` wrapper for the SSH route; False if current."""
        target, text = Path(self.profile["operator_command"]), OPERATOR_COMMAND.format(path=self.helper_path()).encode()
        if target.exists() and target.read_bytes() == text:
            return False
        self.write(target, text, 0o755, 0, 0)
        return True

    def install_extras(self, request_id):
        """What a helper update installs beside the helper itself; True if the file service changed."""
        folder = self.state / "snapshots" / request_id
        folder.mkdir(parents=True, mode=0o700, exist_ok=True)
        # The file service ships beside the helper; a failed file service never blocks the helper update.
        files = self.update_files_service(folder)
        self.update_operator_command()
        return files

    def update_helper(self, request_id):
        target = self.helper_path()
        new = (Path(self.build_config()["service_dir"]) / "host_helper.py").read_bytes()
        old = target.read_bytes()
        if new == old:
            return {"changed": False, "files_service": self.install_extras(request_id)}
        folder = self.state / "snapshots" / request_id
        folder.mkdir(parents=True, mode=0o700, exist_ok=True)
        (folder / "host_helper.py").write_bytes(old)
        self.write(target, new, 0o644)
        # This update runs the old helper's code; the new file installs its own extras, so a
        # helper that adds install-time behaviour needs one supervisor_update, not two.
        result = self.runner(["/usr/bin/python3", "-I", str(target), "install", request_id],
                             capture_output=True, text=True, timeout=300)
        if result.returncode:
            self.write(target, old, 0o644)
            raise RuntimeError("the new helper's install step failed; previous copy restored: "
                               + (result.stderr or result.stdout)[-300:])
        files = result.stdout.strip() == "files_service changed"
        unit = self.profile["helper_unit"]
        self.runner(["systemctl", "restart", unit], timeout=60)
        deadline = self.clock() + 30
        while self.clock() < deadline:
            time.sleep(2)
            if self.helper_answers():
                return {"changed": True, "files_service": files, "sha256": hashlib.sha256(new).hexdigest(),
                        "previous": str(folder / "host_helper.py")}
        self.write(target, old, 0o644)
        self.runner(["systemctl", "restart", unit], timeout=60)
        raise RuntimeError("the new helper did not answer within 30 seconds; previous copy restored")

    def run_approved(self, record):
        """Run in the background; status reads show running until it settles."""
        record["state"] = "running"
        self.save(record)
        if record["action"] in SUPERVISED:
            # Its own transient unit, so a broker or helper restart cannot cut it off.
            self.runner(["systemd-run", "--unit", "webagent-supervisor-" + record["id"][:12], "--collect",
                         "/usr/bin/python3", "-I", str(Path(__file__).resolve()), "supervise", record["id"]],
                        check=True, capture_output=True, timeout=30)
            return
        thread = threading.Thread(target=self.settle, args=(record,), daemon=True)
        thread.start()
        if record["action"] in ("noop", "host_status", *GRANTS):
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
        root = uid == 0
        operator = uid == self.operator_uid or root
        if op == "request" and (app or operator):
            return self.request(message, "app" if app else "ssh")
        if op == "status" and (app or operator):
            record = self.load(message.get("id"))
            if not app and not root and record["requester"]["channel"] != "ssh":
                raise PermissionError("not your request")
            return record
        if op == "list" and (app or root):
            with self.lock:
                records = [self.load(p.stem) for p in sorted((self.state / "requests").glob("*.json"))]
            pending = [r for r in records if r["state"] == "pending"]
            recent = sorted((r for r in records if r["state"] != "pending"), key=lambda r: -r.get("decided_at", 0))[:50]
            return {"pending": pending, "recent": recent, "windows": self.windows(),
                    "window_openers": self.profile["window_openers"], "approvers": self.profile["approvers"],
                    "file_grants": self.grants("files_grant"), "terminal_grants": self.grants("terminal_grant"),
                    "file_log": self.file_log(50)}
        # Root is the host operator over SSH (sudo): its decisions and windows never pass through the app.
        if op == "decide" and (app or root):
            return self.decide(message, "app" if app else "host")
        if op == "allow" and (app or root):
            return self.allow(message, "app" if app else "host")
        if op == "files_revoke" and app:
            return self.files_revoke(message)
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
        if action in GRANTS:
            # Only the File Manager files these (app channel, no chat session): an agent's
            # host_admin call never asks for root-equivalent file access (FILES-2).
            if channel != "app" or requester.get("session") or requester.get("user") != args.get("admin"):
                raise PermissionError(action + " is requested only by the File Manager for its own administrator")
            # One waiting grant per administrator: each further click finds the same request.
            with self.lock:
                for path in sorted((self.state / "requests").glob("*.json")):
                    waiting = self.load(path.stem)
                    if waiting["state"] == "pending" and waiting["action"] == action and waiting["args"] == args:
                        return waiting
        record = {"id": secrets.token_hex(16), "action": action, "args": args, "preview": self.preview(action, args),
                  "undo": spec["undo"], "boot_disk": boot_disk, "passphrase": bool(spec.get("passphrase")),
                  "root": bool(spec.get("root")),
                  "requester": {"channel": channel, **requester},
                  "state": "pending", "created_at": now, "expires_at": now + ttl if (ttl := self.profile["request_ttl_seconds"]) else None}
        with self.lock:
            self.save(record)
            self.log({"event": "requested", "id": record["id"], "action": action, "requester": record["requester"]})
            # An app request is covered by its chat session's window, an SSH request by "ssh:<identity>".
            key = requester.get("session") if channel == "app" else "ssh:" + str(requester.get("identity", ""))
            record["allow_key"] = key
            covered = spec["undo"] != "none" and not boot_disk and not spec.get("passphrase") and self.allow_window(key)
            self.save(record)
            if spec["read"] or covered:
                record.update(state="approved", decided_at=now, decided_by="read-only" if spec["read"] else "allow-all")
                self.log({"event": "approved", "id": record["id"], "by": record["decided_by"]})
                self.run_approved(record)
        return record

    def decide(self, message, caller="app"):
        decision, admin = message.get("decision"), message.get("admin")
        if decision not in ("approve", "deny") or not isinstance(admin, str) or not admin:
            raise ValueError("decision approve or deny and the deciding admin required")
        with self.lock:
            record = self.load(message.get("id"))
            if record["state"] != "pending":
                raise ValueError("request is " + record["state"])
            # Boot and disk approvals need the host approval passphrase, checked here, never by the app.
            # So do File Manager grants (files and terminals outside the checkout) and every
            # root-equivalent action. Root over SSH already holds root and needs no passphrase.
            needs = record["boot_disk"] or record.get("passphrase") or ACTIONS[record["action"]].get("root")
            if decision == "approve" and needs:
                if caller not in self.profile["approvers"]:
                    raise PermissionError("on this host this request is approved only "
                                          + ("over SSH: sudo buildctl host approve " + record["id"] if caller == "app" else "in the app"))
                if caller == "app":
                    self.check_passphrase(message.get("passphrase"))
            if decision == "approve" and record["action"] in GRANTS:
                # The approver sets how long the grant lasts: default 1 h, never above the maximum.
                seconds = message.get("seconds", self.profile["grant_default_seconds"])
                if type(seconds) is not int or not 60 <= seconds <= self.profile["grant_max_seconds"]:
                    raise ValueError("grant time must be between 1 minute and " + str(self.profile["grant_max_seconds"] // 3600) + " h")
                record["args"] = {**record["args"], "seconds": seconds}
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
        """Verify against the salted scrypt hash; 5 wrong tries lock it for 15 minutes. A missing
        or empty passphrase is refused without counting as a try."""
        path = self.passphrase_path()
        if not path.exists():
            raise PermissionError("no host approval passphrase is set; set it at the server console first")
        stored = json.loads(path.read_text())
        now = self.clock()
        failures = [t for t in stored.get("failures", []) if t > now - 900]
        if len(failures) >= 5:
            raise PermissionError("too many wrong passphrases; try again in 15 minutes")
        if not isinstance(given, str) or not given:
            raise PermissionError("type the host approval passphrase")
        digest = hashlib.scrypt(given.encode(), salt=bytes.fromhex(stored["salt"]), n=2 ** 14, r=8, p=1).hex()
        if not hmac.compare_digest(digest, stored["hash"]):
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

    # File Manager outside the checkout ----------------------------------------------------
    def grants(self, action):
        """Live grants for one grant action: administrator -> expiry time. host_files.py reads the same file."""
        path = self.state / GRANTS[action]
        grants = json.loads(path.read_text()) if path.exists() else {}
        return {admin: until for admin, until in grants.items() if until > self.clock()}

    def save_grants(self, action, grants):
        path = self.state / GRANTS[action]
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(grants, sort_keys=True))
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)

    def file_log(self, limit):
        """The newest changes the host file service made, newest first."""
        path = self.state / "files-log.jsonl"
        if not path.exists():
            return []
        with path.open("rb") as stream:
            stream.seek(max(0, path.stat().st_size - 256 * 1024))
            lines = stream.read().decode(errors="replace").splitlines()
        entries = []
        for line in reversed(lines):
            try:
                entries.append(json.loads(line))
            except ValueError:
                continue
            if len(entries) >= limit:
                break
        return entries

    def files_revoke(self, message):
        """Narrowing needs no passphrase: any administrator may end a grant at once."""
        admin, by = message.get("admin"), message.get("by")
        action = {"files": "files_grant", "terminal": "terminal_grant"}.get(message.get("kind", "files"))
        if not isinstance(admin, str) or not admin or not isinstance(by, str) or not by or not action:
            raise ValueError("files_revoke takes the granted admin, the revoking admin and kind files or terminal")
        with self.grants_lock:
            grants = self.grants(action)
            existed = grants.pop(admin, None) is not None
            self.save_grants(action, grants)
        self.log({"event": "files_revoke", "kind": message.get("kind", "files"), "admin": admin, "by": by})
        return {"admin": admin, "kind": message.get("kind", "files"), "revoked": existed}

    def allow(self, message, caller="app"):
        session, seconds, admin = message.get("session"), message.get("seconds", self.profile["allow_default_seconds"]), message.get("admin")
        if not isinstance(session, str) or not session or not isinstance(admin, str) or type(seconds) is not int \
                or not 0 <= seconds <= self.profile["allow_max_seconds"]:
            raise ValueError("allow takes a session, an admin and seconds up to the configured maximum")
        # Opening or extending a window is root-equivalent; ending one (0 seconds) is free, like files_revoke.
        if seconds:
            if caller not in self.profile["window_openers"]:
                raise PermissionError("on this host windows are opened only "
                                      + ("over SSH: sudo buildctl host allow <hours>" if caller == "app" else "in the app"))
            if caller == "app":
                self.check_passphrase(message.get("passphrase"))
        with self.lock:
            path = self.state / "allow.json"
            windows = json.loads(path.read_text()) if path.exists() else {}
            windows = {k: v for k, v in windows.items() if v > self.clock()}
            if seconds:
                windows[session] = self.clock() + seconds
            else:
                windows.pop(session, None)
            path.write_text(json.dumps(windows))
            self.log({"event": "allow", "session": session, "seconds": seconds, "by": admin, "caller": caller})
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


def call(path, message):
    """One request to the running helper; as root it is the host operator (uid 0)."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(60)
        connection.connect(path)
        connection.sendall(json.dumps(message).encode() + b"\n")
        reply = json.loads(connection.makefile("rb").readline(2 * 1024 * 1024))
    if reply.get("status") != "success":
        sys.exit(str(reply.get("output")))
    return reply["output"]


def operator(argv, profile):
    """`sudo buildctl host ...`: read, decide and open windows as root, never through the app.
    Root needs no passphrase; the decisions are logged as host:<sudo user>."""
    import argparse
    parser = argparse.ArgumentParser(prog="sudo buildctl host")
    commands = parser.add_subparsers(dest="op", required=True)
    commands.add_parser("pending", help="waiting requests and live windows")
    for name in ("approve", "deny"):
        decide = commands.add_parser(name, help=name + " one waiting request")
        decide.add_argument("id")
        if name == "approve":
            decide.add_argument("--hours", type=float, help="File Manager grants: how long (default 1)")
    allow = commands.add_parser("allow", help="open an allow-all window for HOURS (0 ends it)")
    allow.add_argument("hours", type=float)
    allow.add_argument("--session", default="*", help='requester key; "*" (default) covers every requester')
    args = parser.parse_args(argv)
    admin, path = "host:" + os.environ.get("SUDO_USER", "root"), profile["socket"]
    left = lambda until: f"until {time.strftime('%m-%d %H:%M', time.localtime(until))} ({int(until - time.time()) // 60} min left)"
    if args.op == "pending":
        page = call(path, {"op": "list"})
        for record in page["pending"]:
            flags = [f for f in ("root", "boot_disk", "passphrase") if record.get(f)]
            print(record["id"], record["action"], json.dumps(record["requester"], sort_keys=True), " ".join(flags))
            print("    " + "\n    ".join(record["preview"].splitlines()[:6]))
        print(f"{len(page['pending'])} waiting. Windows: "
              + (", ".join(f"{key} {left(until)}" for key, until in page["windows"].items()) or "none")
              + ". Openers on this host: " + ", ".join(page.get("window_openers", [])))
    elif args.op in ("approve", "deny"):
        message = {"op": "decide", "id": args.id, "decision": args.op, "admin": admin}
        if getattr(args, "hours", None) is not None:
            message["seconds"] = round(args.hours * 3600)
        record = call(path, message)
        print(record["id"], record["action"], record["state"])
    else:
        result = call(path, {"op": "allow", "session": args.session, "seconds": round(args.hours * 3600), "admin": admin})
        print(f"window {result['session']} " + (left(result["until"]) if result.get("until") else "ended"))


def main():
    if os.geteuid() != 0:
        sys.exit("host helper must run as root")
    os.umask(0o077)
    profile = load_profile()
    if sys.argv[1:2] == ["supervise"] and len(sys.argv) == 3:
        # Started by run_approved through systemd-run: settle one approved supervised request.
        helper = Helper(profile)
        record = helper.load(sys.argv[2])
        if record["state"] != "running" or record["action"] not in SUPERVISED:
            sys.exit("request is not a running supervised action")
        helper.settle(record)
        return
    if sys.argv[1:2] == ["install"] and len(sys.argv) == 3:
        # Run by update_helper from the newly written helper file: install what ships beside it.
        print("files_service changed" if Helper(profile).install_extras(sys.argv[2]) else "files_service current")
        return
    if sys.argv[1:2] == ["operator"]:
        operator(sys.argv[2:], profile)
        return
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
