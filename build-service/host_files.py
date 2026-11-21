"""Root host file service: the File Manager's view of the computer outside the checkout.

Runs as root (webagent-host-files.service, installed by the host helper's supervisor_update).
Only the app's uid may call it, and every file operation needs a live grant for the signed-in
administrator. Grants are written only by the host helper after a Host Admin approval with
the host approval passphrase; they last the time set at approval and Host Admin can revoke them.
Each file operation runs first in a child process as the operator account (with its usual
groups, like a login), so it can read and change what that account can; new files are created
ar:ar 600 (folders 700). When that is denied, the operation needs sudo, as in a terminal: it
runs as root once the account's sudo password is confirmed (by running sudo as that account),
then for the next 15 minutes without asking, or straight away while the account has
passwordless sudo. The password is never stored or logged. The locked folders (app data,
sockets, kernel views) stay refused either way (docs/security-spec.md FILES-1). Grants, the
sudo windows and the log stay with the root parent.
The checkout itself stays with the worker (/live-checkout). Every change is logged.

Terminals (FILES-3) need their own grant (terminal_grant). Each runs a login shell on a
pseudo-terminal as the operator account with its usual groups, like a login on the computer
(sudo asks for the password in the shell). They outlive a page reload and close when the grant ends,
after 30 idle minutes, or on request; only their opening and closing are logged.
"""
import base64
import fcntl
import grp
import hashlib
import json
import os
import pty
import pwd
import secrets
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import termios
import threading
import time
import traceback
import uuid
from pathlib import Path

PROFILE = Path(os.environ.get("WEBAGENT_HOST_PROFILE", "/etc/webagent/host-profile.json"))
DEFAULTS = {"state": "/var/lib/webagent-host", "app_uid": 1100,
            "files_socket": "/home/ar/webagent-dev3/runtime/files/files.sock",
            "files_checkout": "/home/ar/webagent-dev3/code",
            "files_locked": ["/home/ar/webagent-dev3/state", "/home/ar/webagent-dev3/runtime", "/proc", "/sys", "/dev"],
            "operator_account": "ar", "terminal_drop_groups": [], "sudo_window_seconds": 900, "terminal_idle_seconds": 1800,
            "terminal_max": 4}
TERMINAL_BUFFER = 262144
TERMINAL_WAIT = 20
CHECKOUT_VIRTUAL = "/live-checkout"
MAX_FILE = 1024 * 1024
MAX_ENTRIES = 2000
MAX_REQUEST = 4 * 1024 * 1024


class Refused(Exception):
    """Shown to the administrator as is."""


class NeedsSudo(Exception):
    """The operator account was denied; the File Manager asks for its sudo password."""


class NeedsGrant(Exception):
    """No live grant for this administrator; Host Admin must approve one."""


def load_profile(path=PROFILE):
    value = dict(DEFAULTS)
    if path.exists():
        value.update(json.loads(path.read_text()))
    return value


def sha256(data):
    return hashlib.sha256(data).hexdigest()


class Terminal:
    """One shell on a pseudo-terminal; a reader thread keeps the newest output."""

    def __init__(self, admin, cwd, pid, fd, clock):
        self.id, self.admin, self.cwd, self.pid, self.fd = secrets.token_hex(8), admin, cwd, pid, fd
        self.opened_at = self.seen = clock()
        self.output, self.base, self.exited = bytearray(), 0, False
        self.changed = threading.Condition()
        # Guards the descriptor's lifetime only: writers hold it while writing, the reader takes
        # it to close. Reads never wait on it, so output keeps draining while a write blocks.
        self.fd_lock = threading.Lock()
        self.fd_closed = False
        threading.Thread(target=self.pump, daemon=True).start()

    def pump(self):
        # Only this thread closes the pseudo-terminal: closing it elsewhere while a read is
        # pending lets the number be reused by another open file, which this read would consume.
        while True:
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                data = b""
            with self.changed:
                if not data:
                    self.exited = True
                    self.changed.notify_all()
            if not data:
                with self.fd_lock:
                    self.fd_closed = True
                    try:
                        os.close(self.fd)
                    except OSError:
                        pass
                return
            with self.changed:
                self.output += data
                if len(self.output) > TERMINAL_BUFFER:
                    drop = len(self.output) - TERMINAL_BUFFER
                    del self.output[:drop]
                    self.base += drop
                self.changed.notify_all()

    def read(self, offset, wait):
        """Output after offset, waiting up to wait seconds for some; offsets count every byte ever written."""
        with self.changed:
            end = self.base + len(self.output)
            if offset >= end and not self.exited and wait:
                self.changed.wait(wait)
            end = self.base + len(self.output)
            start = max(offset, self.base)
            data = bytes(self.output[start - self.base:])
        return {"id": self.id, "data": base64.b64encode(data).decode(), "offset": end,
                "truncated": offset < self.base, "exited": self.exited}

    def close(self):
        """End the shell; the reader sees end of output and closes the pseudo-terminal itself.
        A shell that ignores the hangup is killed two seconds later."""
        for signum, delay in ((signal.SIGHUP, 0), (signal.SIGKILL, 2)):
            def send(signum=signum):
                if not self.exited:
                    try:
                        os.killpg(self.pid, signum)
                    except OSError:
                        pass
            threading.Timer(delay, send).start() if delay else send()
        try:
            os.waitpid(self.pid, os.WNOHANG)
        except OSError:
            pass


class Files:
    def __init__(self, profile, clock=time.time):
        self.profile = profile
        self.clock = clock
        self.state = Path(profile["state"])
        self.app_uid = profile["app_uid"]
        self.checkout = os.path.realpath(profile["files_checkout"])
        self.locked = [os.path.realpath(p) for p in profile["files_locked"]]
        self.terminals = {}
        self.terminals_lock = threading.Lock()
        self.pending_log = None
        self.sudo_until = {}      # administrator -> end of their sudo window
        self.sudo_failures = {}   # administrator -> times of recent wrong passwords

    # Grants and log ----------------------------------------------------------------------
    def granted(self, admin, name="file-grants.json"):
        path = self.state / name
        grants = json.loads(path.read_text()) if path.exists() else {}
        until = grants.get(admin) if isinstance(admin, str) and admin else None
        return bool(until) and until > self.clock()

    def log(self, admin, op, path, **extra):
        if self.pending_log is not None:  # in the operator child: the root parent writes it
            self.pending_log.append([admin, op, path, extra])
            return
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

    # Terminals ---------------------------------------------------------------------------
    def shell_identity(self):
        """The operator account and its groups minus the dropped ones (no sudo, no docker)."""
        account = pwd.getpwnam(self.profile["operator_account"])
        dropped = set()
        for name in self.profile["terminal_drop_groups"]:
            try:
                dropped.add(grp.getgrnam(name).gr_gid)
            except KeyError:
                pass
        groups = [g for g in os.getgrouplist(account.pw_name, account.pw_gid) if g not in dropped]
        return account, groups

    def terminal_open(self, admin, cwd, cols, rows):
        with self.terminals_lock:
            if sum(1 for t in self.terminals.values() if t.admin == admin) >= self.profile["terminal_max"]:
                raise Refused("At most " + str(self.profile["terminal_max"]) + " terminals; close one first")
        account, groups = self.shell_identity()
        folder = account.pw_dir
        if isinstance(cwd, str) and cwd.startswith("/") and "\0" not in cwd and os.path.isdir(cwd):
            folder = os.path.realpath(cwd)
        size = struct.pack("HHHH", max(2, min(int(rows or 24), 500)), max(2, min(int(cols or 80), 500)), 0, 0)
        environment = {"HOME": account.pw_dir, "USER": account.pw_name, "LOGNAME": account.pw_name,
                       "SHELL": account.pw_shell or "/bin/bash", "TERM": "xterm-256color", "LANG": "C.UTF-8",
                       "PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"}
        pid, fd = pty.fork()
        if pid == 0:  # child: become the operator account, then the shell; never returns
            try:
                fcntl.ioctl(0, termios.TIOCSWINSZ, size)
                os.setgroups(groups)
                os.setgid(account.pw_gid)
                os.setuid(account.pw_uid)
                os.chdir(folder)
                shell = account.pw_shell or "/bin/bash"
                os.execve(shell, [shell, "-l"], environment)
            finally:
                os._exit(127)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, size)
        terminal = Terminal(admin, folder, pid, fd, self.clock)
        with self.terminals_lock:
            self.terminals[terminal.id] = terminal
        self.log(admin, "terminal_open", folder, terminal=terminal.id)
        return {"id": terminal.id, "cwd": folder, "opened_at": terminal.opened_at}

    def terminal(self, admin, terminal_id):
        with self.terminals_lock:
            terminal = self.terminals.get(terminal_id)
        if terminal is None or terminal.admin != admin:
            raise Refused("No such terminal")
        terminal.seen = self.clock()
        return terminal

    def terminal_close(self, admin, terminal_id, reason="closed"):
        terminal = self.terminal(admin, terminal_id)
        with self.terminals_lock:
            self.terminals.pop(terminal.id, None)
        terminal.close()
        self.log(admin, "terminal_close", terminal.cwd, terminal=terminal.id, reason=reason)
        return {"id": terminal.id, "status": "closed"}

    def terminal_send(self, admin, terminal_id, data):
        if not isinstance(data, str) or len(data) > 65536:
            raise Refused("Invalid terminal input")
        terminal = self.terminal(admin, terminal_id)
        with terminal.fd_lock:
            if terminal.fd_closed:
                raise Refused("The shell has exited")
            view = memoryview(data.encode())
            while view:
                view = view[os.write(terminal.fd, view):]
        return {"id": terminal.id, "status": "ok"}

    def terminal_resize(self, admin, terminal_id, cols, rows):
        terminal = self.terminal(admin, terminal_id)
        with terminal.fd_lock:
            if terminal.fd_closed:
                raise Refused("The shell has exited")
            fcntl.ioctl(terminal.fd, termios.TIOCSWINSZ,
                        struct.pack("HHHH", max(2, min(int(rows), 500)), max(2, min(int(cols), 500)), 0, 0))
        return {"id": terminal.id, "status": "ok"}

    def terminal_list(self, admin):
        with self.terminals_lock:
            mine = [t for t in self.terminals.values() if t.admin == admin]
        return {"terminals": [{"id": t.id, "cwd": t.cwd, "opened_at": t.opened_at, "exited": t.exited} for t in mine]}

    def reap(self):
        """Close terminals whose grant ended, that sat idle too long, or whose shell exited."""
        with self.terminals_lock:
            terminals = list(self.terminals.values())
        for terminal in terminals:
            reason = None
            if not self.granted(terminal.admin, "terminal-grants.json"):
                reason = "grant ended"
            elif self.clock() - terminal.seen > self.profile["terminal_idle_seconds"]:
                reason = "idle"
            elif terminal.exited and self.clock() - terminal.seen > 60:
                reason = "exited"
            if reason:
                try:
                    self.terminal_close(terminal.admin, terminal.id, reason)
                except Refused:
                    pass

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
            mode = 0o600
        else:
            if not stat.S_ISREG(old.st_mode):
                raise Refused("Only regular files can be saved")
            with open(real, "rb") as stream:
                if sha256(stream.read(MAX_FILE + 1)) != expected:
                    raise Refused("File changed; reload before saving")
            mode = stat.S_IMODE(old.st_mode)
            if old.st_uid != os.geteuid():
                # Someone else's file this account may write: rewrite it in place, keeping its owner.
                with open(real, "r+b") as stream:
                    stream.write(data)
                    stream.truncate()
                    stream.flush()
                    os.fsync(stream.fileno())
                self.log(admin, "write", real, bytes=len(data))
                return {"path": real, "size": len(data), "status": "ok", "sha256": sha256(data)}
        temporary = os.path.join(parent, ".webagent-files-" + uuid.uuid4().hex)
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        try:
            with os.fdopen(fd, "wb") as stream:
                stream.write(data)
                stream.flush()
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
        if kind == "dir":
            os.mkdir(real, 0o700)
        else:
            os.close(os.open(real, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600))
        os.chmod(real, 0o700 if kind == "dir" else 0o600)
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
            raise Refused("Only the app may use the host file service")
        op = message.get("op")
        if op == "locate":
            # Where the checkout sits, so the File Manager can offer its parent; changes nothing.
            return {"checkout": self.checkout, "parent": os.path.dirname(self.checkout)}
        admin = message.get("admin")
        if isinstance(op, str) and op.startswith("terminal_"):
            if not self.granted(admin, "terminal-grants.json"):
                raise NeedsGrant("Host Admin must authorize terminal access")
            if op == "terminal_open":
                return self.terminal_open(admin, message.get("cwd"), message.get("cols"), message.get("rows"))
            if op == "terminal_list":
                return self.terminal_list(admin)
            terminal_id = message.get("id")
            if op == "terminal_read":
                offset, wait = message.get("offset", 0), message.get("wait", 0)
                if type(offset) is not int or offset < 0 or type(wait) not in (int, float):
                    raise Refused("Invalid terminal read")
                return self.terminal(admin, terminal_id).read(offset, max(0, min(wait, TERMINAL_WAIT)))
            if op == "terminal_send":
                return self.terminal_send(admin, terminal_id, message.get("data"))
            if op == "terminal_resize":
                return self.terminal_resize(admin, terminal_id, message.get("cols"), message.get("rows"))
            if op == "terminal_close":
                return self.terminal_close(admin, terminal_id)
            raise Refused("Unsupported terminal action")
        if not self.granted(admin):
            raise NeedsGrant("Host Admin must authorize browsing outside the checkout")
        if op not in ("tree", "read", "write", "create", "rename", "delete"):
            raise Refused("Unsupported File Manager action")
        try:
            return self.as_operator(admin, op, message)
        except PermissionError:
            if not self.sudo_allowed(admin, message.get("sudo_password")):
                raise
        result = self.file_op(admin, op, message)  # as root: the parent process
        return {**result, "as_root": True} if isinstance(result, dict) else result

    def sudo_allowed(self, admin, password):
        """Whether a denied operation may run as root: a live sudo window, passwordless sudo, or
        a correct sudo password for the operator account (checked by sudo itself)."""
        now = self.clock()
        if self.sudo_until.get(admin, 0) > now:
            return True
        account = self.profile["operator_account"]
        run = lambda command, **extra: subprocess.run(command, capture_output=True, timeout=20, **extra).returncode == 0
        if run(["runuser", "-u", account, "--", "sudo", "-n", "-v"]):
            return True
        if not isinstance(password, str) or not password:
            raise NeedsSudo("This needs the sudo password for " + account)
        failures = [t for t in self.sudo_failures.get(admin, []) if t > now - 900]
        if len(failures) >= 5:
            raise Refused("Too many wrong sudo passwords; try again in 15 minutes")
        if len(password) > 1024 or not run(["runuser", "-u", account, "--", "sudo", "-S", "-k", "-v", "-p", ""],
                                           input=(password + "\n").encode()):
            self.sudo_failures[admin] = failures + [now]
            raise NeedsSudo("Wrong sudo password for " + account + "; try again")
        self.sudo_failures.pop(admin, None)
        self.sudo_until[admin] = now + self.profile["sudo_window_seconds"]
        self.log(admin, "sudo", "", reason="password confirmed")
        return True

    def file_op(self, admin, op, message):
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
        return self.delete(admin, message.get("path"))

    def as_operator(self, admin, op, message):
        """Run one file operation in a child that has become the operator account (no sudo or
        docker group); its result, refusal or error and its log lines come back over a pipe."""
        account, groups = self.shell_identity()
        reader, writer = os.pipe()
        pid = os.fork()
        if pid == 0:
            try:
                os.close(reader)
                os.setgroups(groups)
                os.setgid(account.pw_gid)
                os.setuid(account.pw_uid)
                os.umask(0o077)
                self.pending_log = []
                try:
                    reply = {"output": self.file_op(admin, op, message)}
                except Refused as error:
                    reply = {"refused": str(error)}
                except OSError as error:
                    reply = {"errno": error.errno, "error": str(error.strerror or error)}
                except Exception as error:
                    reply = {"error": type(error).__name__, "detail": traceback.format_exc(limit=1)[-200:]}
                reply["log"] = self.pending_log
                data = json.dumps(reply).encode()
                view = memoryview(data)
                while view:
                    view = view[os.write(writer, view):]
            finally:
                os._exit(0)
        os.close(writer)
        chunks = []
        with os.fdopen(reader, "rb") as stream:
            while chunk := stream.read(1 << 20):
                chunks.append(chunk)
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:  # SIGCHLD is ignored in the service: the kernel reaped it
            pass
        if not chunks:
            raise Refused("File operation failed")
        reply = json.loads(b"".join(chunks))
        for entry_admin, entry_op, entry_path, extra in reply.get("log", []):
            self.log(entry_admin, entry_op, entry_path, **extra)
        if "output" in reply:
            return reply["output"]
        if "refused" in reply:
            raise Refused(reply["refused"])
        if reply.get("errno") is not None:
            raise OSError(reply["errno"], reply["error"])  # EACCES/EPERM become PermissionError
        raise Refused("File operation failed (" + str(reply.get("error")) + ")")


def reply_for(files, uid, message):
    try:
        return {"status": "success", "output": files.handle(uid, message)}
    except NeedsGrant as error:
        return {"status": "failed", "code": "authorization_required", "output": str(error)}
    except NeedsSudo as error:
        return {"status": "failed", "code": "sudo_required", "output": str(error)}
    except Refused as error:
        return {"status": "failed", "output": str(error)}
    except FileNotFoundError:
        return {"status": "failed", "output": "No such file or folder"}
    except FileExistsError:
        return {"status": "failed", "output": "An entry with that name already exists"}
    except PermissionError:
        return {"status": "failed", "output": "Permission denied: the operator account cannot do this"}
    except OSError as error:
        return {"status": "failed", "output": type(error).__name__ + ": " + str(error)[:200]}
    except Exception as error:  # a malformed request must not stop the service
        return {"status": "failed", "output": type(error).__name__}


def client(files, connection):
    with connection:
        try:
            connection.settimeout(TERMINAL_WAIT + 30)
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
    files = Files(profile)
    signal.signal(signal.SIGCHLD, signal.SIG_IGN)  # finished shells are reaped by the kernel

    def reaper():
        while True:
            time.sleep(30)
            files.reap()
    threading.Thread(target=reaper, daemon=True).start()
    serve(files, profile["files_socket"])


if __name__ == "__main__":
    main()
