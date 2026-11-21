"""Root host file service: the File Manager's view of the computer outside the checkout.

Runs as root (webagent-host-files.service, installed by the host helper's supervisor_update).
Only the app's uid may call it, and every file operation needs a live grant for the signed-in
administrator. Grants are written only by the host helper after a Host Admin approval with
the host approval passphrase; they expire (24 h by default) and Host Admin can revoke them.
While a grant is live the app can read and change any file except the locked folders (app
data, sockets, kernel views) - an accepted risk recorded in docs/security-spec.md (FILES-1).
The checkout itself stays with the worker (/live-checkout). Every change is logged.
"""
import base64
import hashlib
import json
import os
import shutil
import socket
import stat
import struct
import sys
import threading
import time
import uuid
from pathlib import Path

PROFILE = Path(os.environ.get("WEBAGENT_HOST_PROFILE", "/etc/webagent/host-profile.json"))
DEFAULTS = {"state": "/var/lib/webagent-host", "app_uid": 1100,
            "files_socket": "/home/ar/webagent-dev3/runtime/files/files.sock",
            "files_checkout": "/home/ar/webagent-dev3/code",
            "files_locked": ["/home/ar/webagent-dev3/state", "/home/ar/webagent-dev3/runtime", "/proc", "/sys", "/dev"]}
CHECKOUT_VIRTUAL = "/live-checkout"
MAX_FILE = 1024 * 1024
MAX_ENTRIES = 2000
MAX_REQUEST = 4 * 1024 * 1024


class Refused(Exception):
    """Shown to the administrator as is."""


class NeedsGrant(Exception):
    """No live grant for this administrator; Host Admin must approve one."""


def load_profile(path=PROFILE):
    value = dict(DEFAULTS)
    if path.exists():
        value.update(json.loads(path.read_text()))
    return value


def sha256(data):
    return hashlib.sha256(data).hexdigest()


class Files:
    def __init__(self, profile, clock=time.time):
        self.profile = profile
        self.clock = clock
        self.state = Path(profile["state"])
        self.app_uid = profile["app_uid"]
        self.checkout = os.path.realpath(profile["files_checkout"])
        self.locked = [os.path.realpath(p) for p in profile["files_locked"]]

    # Grants and log ----------------------------------------------------------------------
    def granted(self, admin):
        path = self.state / "file-grants.json"
        grants = json.loads(path.read_text()) if path.exists() else {}
        until = grants.get(admin) if isinstance(admin, str) and admin else None
        return bool(until) and until > self.clock()

    def log(self, admin, op, path, **extra):
        with (self.state / "files-log.jsonl").open("a") as stream:
            stream.write(json.dumps({"at": self.clock(), "admin": admin, "op": op, "path": path, **extra}, sort_keys=True) + "\n")

    # Paths -------------------------------------------------------------------------------
    @staticmethod
    def absolute(raw):
        if not isinstance(raw, str) or not raw.startswith("/") or "\0" in raw or len(raw) > 4096:
            raise Refused("An absolute path is required")
        path = os.path.normpath(raw)
        return "/" + path.lstrip("/")

    @staticmethod
    def inside(path, folder):
        return path == folder or path.startswith(folder.rstrip("/") + "/")

    def lock_reason(self, real):
        if any(self.inside(real, folder) for folder in self.locked):
            return "Locked for security: app data and sockets stay closed"
        if self.inside(real, self.checkout):
            return "Open the app checkout through " + CHECKOUT_VIRTUAL
        return None

    def check(self, real):
        reason = self.lock_reason(real)
        if reason:
            raise Refused(reason)

    def entry(self, raw):
        """An entry to change: its real parent folder plus its own name (not followed)."""
        path = self.absolute(raw)
        if path == "/":
            raise Refused("The root folder cannot be changed")
        parent = os.path.realpath(os.path.dirname(path))
        real = os.path.join(parent, os.path.basename(path))
        self.check(parent)
        self.check(real)
        return real

    def no_locked_inside(self, real):
        """Deleting or moving a folder must not take a locked folder or the checkout with it."""
        for folder in self.locked + [self.checkout]:
            if self.inside(folder, real):
                raise Refused("This folder contains locked app folders")

    def shown(self, real):
        return CHECKOUT_VIRTUAL if real == self.checkout else real

    # Operations --------------------------------------------------------------------------
    def tree(self, raw, show_hidden):
        path = os.path.realpath(self.absolute(raw))
        self.check(path)
        entries = []
        truncated = False
        with os.scandir(path) as listing:
            for item in listing:
                if not show_hidden and item.name.startswith("."):
                    continue
                if len(entries) >= MAX_ENTRIES:
                    truncated = True
                    break
                full = os.path.join(path, item.name)
                try:
                    info = os.stat(full)  # follows a symlink to show what it points at
                except OSError:
                    continue
                if not (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)):
                    continue
                is_dir = stat.S_ISDIR(info.st_mode)
                real = os.path.realpath(full)
                value = {"name": item.name, "path": self.shown(real) if real == self.checkout else full,
                         "is_dir": is_dir, "size": 0 if is_dir else info.st_size}
                reason = self.lock_reason(real)
                if reason and real != self.checkout:
                    value["locked"] = True
                if item.is_symlink():
                    value["symlink"] = True
                entries.append((not is_dir, item.name.lower(), item.name, value))
        entries.sort(key=lambda e: e[:3])
        return {"path": path, "parent": None if path == "/" else os.path.dirname(path),
                "project_root": CHECKOUT_VIRTUAL, "host": True, "truncated": truncated,
                "entries": [e[3] for e in entries]}

    def read(self, raw):
        real = os.path.realpath(self.absolute(raw))
        self.check(real)
        fd = os.open(real, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode):
                raise Refused("Only regular files can be opened")
            if info.st_size > MAX_FILE:
                raise Refused("File exceeds the one MiB editor limit")
            data = stream.read(MAX_FILE + 1)
        if len(data) > MAX_FILE:
            raise Refused("File exceeds the one MiB editor limit")
        try:
            content, encoding, binary = data.decode("utf-8"), "utf-8", False
        except UnicodeDecodeError:
            content, encoding, binary = base64.b64encode(data).decode(), "base64", True
        return {"path": real, "content": content, "encoding": encoding, "binary": binary,
                "size": len(data), "truncated": False, "sha256": sha256(data)}

    def write(self, admin, raw, content, encoding, expected):
        if not isinstance(content, str) or encoding not in ("utf-8", "base64"):
            raise Refused("Invalid file content")
        try:
            data = content.encode() if encoding == "utf-8" else base64.b64decode(content, validate=True)
        except ValueError:
            raise Refused("Invalid file encoding") from None
        if len(data) > MAX_FILE:
            raise Refused("File exceeds the one MiB editor limit")
        if not isinstance(expected, str):
            raise Refused("Expected file digest required")
        real = self.entry(raw)
        parent = os.path.dirname(real)
        try:
            old = os.lstat(real)
        except FileNotFoundError:
            old = None
        if old is None:
            if expected:
                raise Refused("File disappeared; reload before saving")
            folder = os.stat(parent)
            mode, uid, gid = 0o644, folder.st_uid, folder.st_gid
        else:
            if not stat.S_ISREG(old.st_mode):
                raise Refused("Only regular files can be saved")
            with open(real, "rb") as stream:
                if sha256(stream.read(MAX_FILE + 1)) != expected:
                    raise Refused("File changed; reload before saving")
            mode, uid, gid = stat.S_IMODE(old.st_mode), old.st_uid, old.st_gid
        temporary = os.path.join(parent, ".webagent-files-" + uuid.uuid4().hex)
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        try:
            with os.fdopen(fd, "wb") as stream:
                stream.write(data)
                stream.flush()
                os.fchown(stream.fileno(), uid, gid)
                os.fchmod(stream.fileno(), mode)
                os.fsync(stream.fileno())
            if old is None:
                os.link(temporary, real)  # never replaces a file created meanwhile
            else:
                os.replace(temporary, real)
        finally:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass
        self.log(admin, "write", real, bytes=len(data))
        return {"path": real, "size": len(data), "status": "ok", "sha256": sha256(data)}

    def create(self, admin, raw, kind):
        if kind not in ("file", "dir"):
            raise Refused("Invalid entry kind")
        real = self.entry(raw)
        folder = os.stat(os.path.dirname(real))
        if kind == "dir":
            os.mkdir(real, 0o755)
        else:
            os.close(os.open(real, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o644))
        os.lchown(real, folder.st_uid, folder.st_gid)
        os.chmod(real, 0o755 if kind == "dir" else 0o644)
        self.log(admin, "create", real, kind=kind)
        return {"path": real, "kind": kind, "status": "ok"}

    def rename(self, admin, raw, new_raw):
        source, target = self.entry(raw), self.entry(new_raw)
        self.no_locked_inside(source)
        if os.path.lexists(target):
            raise Refused("An entry with that name already exists")
        os.rename(source, target)
        self.log(admin, "rename", source, to=target)
        return {"from": source, "to": target, "status": "ok"}

    def delete(self, admin, raw):
        real = self.entry(raw)
        info = os.lstat(real)
        if stat.S_ISDIR(info.st_mode):
            self.no_locked_inside(real)
            shutil.rmtree(real)  # removes symlinks inside, never what they point at
        else:
            os.unlink(real)
        self.log(admin, "delete", real)
        return {"path": real, "status": "deleted"}

    def handle(self, uid, message):
        if uid != self.app_uid:
            raise PermissionError("only the app may use the host file service")
        op = message.get("op")
        if op == "locate":
            # Where the checkout sits, so the File Manager can offer its parent; changes nothing.
            return {"checkout": self.checkout, "parent": os.path.dirname(self.checkout)}
        admin = message.get("admin")
        if not self.granted(admin):
            raise NeedsGrant("Host Admin must authorize browsing outside the checkout")
        if op == "tree":
            return self.tree(message.get("path") or "/", message.get("show_hidden") is True)
        if op == "read":
            return self.read(message.get("path"))
        if op == "write":
            return self.write(admin, message.get("path"), message.get("content"), message.get("encoding"),
                              message.get("expected_sha256"))
        if op == "create":
            return self.create(admin, message.get("path"), message.get("kind"))
        if op == "rename":
            return self.rename(admin, message.get("path"), message.get("new_path"))
        if op == "delete":
            return self.delete(admin, message.get("path"))
        raise Refused("Unsupported File Manager action")


def reply_for(files, uid, message):
    try:
        return {"status": "success", "output": files.handle(uid, message)}
    except NeedsGrant as error:
        return {"status": "failed", "code": "authorization_required", "output": str(error)}
    except Refused as error:
        return {"status": "failed", "output": str(error)}
    except FileNotFoundError:
        return {"status": "failed", "output": "No such file or folder"}
    except FileExistsError:
        return {"status": "failed", "output": "An entry with that name already exists"}
    except (PermissionError, OSError) as error:
        return {"status": "failed", "output": type(error).__name__ + ": " + str(error)[:200]}
    except Exception as error:  # a malformed request must not stop the service
        return {"status": "failed", "output": type(error).__name__}


def client(files, connection):
    with connection:
        try:
            connection.settimeout(30)
            _, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            raw = connection.makefile("rb").readline(MAX_REQUEST + 1)
            if len(raw) > MAX_REQUEST or not raw.endswith(b"\n"):
                raise ValueError("bounded request required")
            message = json.loads(raw)
            if not isinstance(message, dict):
                raise ValueError("request must be an object")
            reply = reply_for(files, uid, message)
        except Exception as error:
            reply = {"status": "failed", "output": type(error).__name__}
        connection.sendall(json.dumps(reply, sort_keys=True).encode() + b"\n")


def serve(files, path):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(path.parent, 0o755)
    path.unlink(missing_ok=True)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(path))
    os.chmod(path, 0o666)  # every peer is checked by uid in handle()
    listener.listen(16)
    while True:
        connection, _ = listener.accept()
        threading.Thread(target=client, args=(files, connection), daemon=True).start()


def main():
    if os.geteuid() != 0:
        sys.exit("host file service must run as root")
    profile = load_profile()
    serve(Files(profile), profile["files_socket"])


if __name__ == "__main__":
    main()
