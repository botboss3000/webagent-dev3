"""Protected V3 development broker installation and root supervisor.

No /etc, sudoers, arbitrary paths, commands, source databases or workspace data.
Review source, run --install as root, then --start the protected bootstrap copy.
"""
import argparse
import importlib.util
import ast
import io
import ctypes
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import signal
import socket
import stat
import struct
import tarfile
import threading
import subprocess
import sys
import time
import uuid
import sqlite3
import tempfile

INSPECTION_CONTEXT = ("v3/data/user_data/admin", "v3/data/agent_data/shared_default")
MAX_INSPECTION_FILE = 256 * 1024 * 1024


import sys
_module_root = Path(os.environ.get("WEBAGENT_BUILD_MODULE_ROOT", str(Path(__file__).resolve().parent)))
_config_spec = importlib.util.spec_from_file_location("webagent_build_config", _module_root / "config.py")
_config_module = importlib.util.module_from_spec(_config_spec)
_config_spec.loader.exec_module(_config_module)
CONFIG = _config_module.load()

V3=Path(CONFIG["checkout_root"])
SHARED_SOURCE=V3/"build-service/shared-ui"
if not SHARED_SOURCE.exists():SHARED_SOURCE=Path(CONFIG["service_dir"])/"shared-ui"
SOURCE=V3/"build-service"
SERVICE=Path(CONFIG["service_dir"])
RUNTIME=Path(CONFIG["runtime_root"])
JOBS=Path(CONFIG["state_root"])
ARCHIVES=V3/"rollback/administrator"
MAX_IMAGES = 16 * 1024 * 1024 * 1024
RESERVE = 512 * 1024 * 1024


class Privileges:
    def uid(self): return os.geteuid()
    def owner(self, info): return info.st_uid
    def chown(self, fd, uid, gid): os.fchown(fd, uid, gid)


PRIVILEGES = Privileges()
_COPY_STOP = threading.Event()


def root_only():
    if PRIVILEGES.uid() != 0:
        raise PermissionError("protected broker installation requires root")


def directory(path):
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("absolute fixed directory required")
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for component in path.parts[1:]:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def protected_directory(parent, name, mode=0o755, gid=0):
    created = False
    try:
        os.mkdir(name, mode, dir_fd=parent)
        created = True
    except FileExistsError:
        pass
    fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent)
    try:
        if not created and PRIVILEGES.owner(os.fstat(fd)) != 0:
            raise PermissionError("existing service directory is not root-owned")
        if created:
            PRIVILEGES.chown(fd, 0, gid)
        os.fchmod(fd, mode)
        return fd
    except BaseException:
        os.close(fd)
        raise


def read_regular(parent, name, limit, owner=None):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=parent)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > limit:
            raise ValueError("bounded single-link regular file required")
        if owner is not None and PRIVILEGES.owner(before) != owner:
            raise PermissionError("file ownership changed")
        chunks = []
        total = 0
        while data := os.read(fd, 1024 * 1024):
            total += len(data)
            if total > limit:
                raise ValueError("file exceeds bound")
            chunks.append(data)
        after = os.fstat(fd)
        if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ValueError("file changed while reading")
        return b"".join(chunks)
    finally:
        os.close(fd)


def publish(parent, name, body, mode=0o600):
    # Refuse an attacker-created destination instead of following or replacing
    # it silently. Atomic replacement is allowed only for protected files.
    try:
        info = os.stat(name, dir_fd=parent, follow_symlinks=False)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or PRIVILEGES.owner(info) != 0:
            raise PermissionError("protected destination replaced")
    except FileNotFoundError:
        pass
    temporary = ".install-" + uuid.uuid4().hex
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=parent)
    try:
        PRIVILEGES.chown(fd, 0, 0)
        os.fchmod(fd, mode)
        owned_fd = fd
        fd = None
        with os.fdopen(owned_fd, "wb") as stream:
            stream.write(body)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, name, src_dir_fd=parent, dst_dir_fd=parent)
        os.fsync(parent)
    finally:
        if fd is not None:
            os.close(fd)
        try:
            os.unlink(temporary, dir_fd=parent)
        except FileNotFoundError:
            pass


def install(tree=None):
    root_only()
    if tree is not None and not re.fullmatch(r"[a-f0-9]{64}",tree):
        raise ValueError("Exact isolated task tree hash required")
    source_path = (JOBS/"coordination/trees"/tree/"v3/build-service"
                   if tree is not None else SOURCE)
    source = directory(source_path)
    parent = directory(SERVICE.parent)
    service = None
    try:
        host = read_regular(source, "host.py", 2 * 1024 * 1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))
        bootstrap = read_regular(source, "install.py", 2 * 1024 * 1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))
        release = read_regular(source, "release.py", 2 * 1024 * 1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))
        performance = read_regular(source, "performance.py", 2 * 1024 * 1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))
        index_source = directory(source_path.parent / "plugins/abilities/Administrator/code_index")
        try:
            maintenance = read_regular(index_source, "maintenance.py", 2*1024*1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))
            scanner = read_regular(index_source, "adapter.py", 2*1024*1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))
        finally:
            os.close(index_source)
        service = protected_directory(parent,SERVICE.name)
        # Compile without importing or executing source. The installed service
        # subsequently runs its protected copy, never this writable checkout.
        compile(host, "host.py", "exec")
        compile(bootstrap, "bootstrap.py", "exec")
        compile(release, "release.py", "exec")
        compile(performance, "performance.py", "exec")
        compile(scanner, "index-scanner.py", "exec")
        compile(maintenance,"index-maintenance.py","exec")
        publish(service, "host.py", host, 0o644)
        publish(service, "bootstrap.py", bootstrap, 0o644)
        publish(service, "release.py", release, 0o644)
        publish(service, "performance.py", performance, 0o644)
        publish(service, "index-scanner.py", scanner, 0o644)
        publish(service,"index-maintenance.py",maintenance,0o644)
        extra={"index-maintenance.py":hashlib.sha256(maintenance).hexdigest()}
        for name in ("config.py","workspace.py","workspace_support.py","workspace_executor.py","lifecycle.py","reconfigure.py","deploy.py","buildctl.py","ownership.py","scheduler.py","index_queue.py","index_coordinator.py","review_store.py","review_policy.py","post_deployment.py","apt_prefetch.py","packaging_cache.py","image_reuse.py","ui_policy.py","ui_publication.py","ui-test.Dockerfile","compiler.Dockerfile","resources.py","resource_control.py","docker_retention.py","docker_guard_dispatch.py","executor.py","engine_components.py","monitoring_component.py","boot_host.py","scheduler-defaults.json"):
            body=read_regular(source,name,2*1024*1024,owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))
            if name.endswith(".py"):compile(body,name,"exec")
            elif name.endswith(".json"):json.loads(body)
            elif name in {"ui-test.Dockerfile","compiler.Dockerfile"}:body.decode("utf-8","strict")
            else:raise ValueError("Unknown protected installation input")
            publish(service,name,body,0o644)
            extra[name]=hashlib.sha256(body).hexdigest()
            if read_regular(source,name,2*1024*1024,owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))!=body:raise ValueError("Installation raced source edit")
        assets=source_path/"shared-ui"
        if assets.is_dir():
            asset_dir=protected_directory(service,"shared-ui");os.close(asset_dir)
            for file in sorted(assets.rglob("*")):
                if file.is_symlink():raise PermissionError("Shared asset symlink rejected")
                if not file.is_file():continue
                rel=file.relative_to(assets);cursor=SERVICE/"shared-ui"
                for part in rel.parts[:-1]:
                    parentfd=directory(cursor);childfd=protected_directory(parentfd,part);os.close(childfd);os.close(parentfd);cursor=cursor/part
                inputfd=directory(file.parent);body=read_regular(inputfd,file.name,4*1024*1024,owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]));os.close(inputfd)
                outputfd=directory(cursor);publish(outputfd,file.name,body,0o644);os.close(outputfd)
        manifest = {"modules":extra,"performance_sha256": hashlib.sha256(performance).hexdigest(), "host_sha256": hashlib.sha256(host).hexdigest(), "bootstrap_sha256": hashlib.sha256(bootstrap).hexdigest(), "release_sha256": hashlib.sha256(release).hexdigest(), "scanner_sha256":hashlib.sha256(scanner).hexdigest()}
        publish(service, "installed.json", json.dumps(manifest, sort_keys=True).encode())
        if read_regular(source, "performance.py", 2 * 1024 * 1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"])) != performance or read_regular(source, "host.py", 2 * 1024 * 1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"])) != host or read_regular(source, "install.py", 2 * 1024 * 1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"])) != bootstrap or read_regular(source, "release.py", 2 * 1024 * 1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"])) != release:
            raise ValueError("installation raced a source edit; repeat after writers pause")
        index_source = directory(source_path.parent / "plugins/abilities/Administrator/code_index")
        try:
            if read_regular(index_source, "adapter.py", 2*1024*1024, owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"])) != scanner or read_regular(index_source,"maintenance.py",2*1024*1024,owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))!=maintenance:
                raise ValueError("installation raced an index parser edit")
        finally:
            os.close(index_source)
        for name,expected in extra.items():
            if name=="index-maintenance.py":continue # Ability source was rechecked through its pinned directory above.
            if hashlib.sha256(read_regular(source,name,2*1024*1024,owner=(CONFIG["service_uid"] if tree is not None else CONFIG["source_uid"]))).hexdigest()!=expected:raise ValueError("Installation raced a module edit")
        return manifest
    finally:
        os.close(source)
        os.close(parent)
        if service is not None:
            os.close(service)


def copy_artifact(source, destination, name, limit):
    before_fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=source)
    temporary = ".archive-" + uuid.uuid4().hex
    output = None
    try:
        before = os.fstat(before_fd)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or PRIVILEGES.owner(before) != CONFIG["service_uid"] or before.st_mode & 0o077:
            raise PermissionError("rollback artifact must be private and owned by ar")
        if not 0 < before.st_size <= limit:
            raise ValueError("rollback artifact exceeds bound")
        available = shutil.disk_usage("/proc/self/fd/" + str(destination)).free
        if available < before.st_size + RESERVE:
            raise ValueError("insufficient space for protected rollback copy")
        if name == "container-config.json":
            body = os.read(before_fd, before.st_size + 1)
            value = json.loads(body)
            if set(value) != {"api", "loop"} or any(not isinstance(v, dict) or not re.fullmatch(r"sha256:[0-9a-f]{64}", v.get("Image", "")) or not isinstance(v.get("Config"), dict) or not isinstance(v.get("HostConfig"), dict) for v in value.values()):
                raise ValueError("invalid private rollback configuration")
            os.lseek(before_fd, 0, os.SEEK_SET)
        elif name != "rollback-images.tar":
            raise ValueError("artifact not allowlisted")
        output = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=destination)
        PRIVILEGES.chown(output, 0, 0)
        digest = hashlib.sha256()
        total = 0
        while data := os.read(before_fd, 1024 * 1024):
            if _COPY_STOP.is_set():
                raise InterruptedError("rollback copy interrupted; retained originals remain")
            total += len(data)
            if total > limit:
                raise ValueError("rollback artifact grew beyond bound")
            if total == len(data) and data.startswith(b"SQLite format 3"):
                raise ValueError("database files are not rollback image inputs")
            digest.update(data)
            view = memoryview(data)
            while view:
                view = view[os.write(output, view):]
        after = os.fstat(before_fd)
        if total != before.st_size or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ValueError("rollback artifact changed during copy")
        os.fsync(output)
        os.close(output)
        output = None
        # Only root can write the destination directory. Never extract images.
        try:
            old = os.stat(name, dir_fd=destination, follow_symlinks=False)
            if not stat.S_ISREG(old.st_mode) or old.st_nlink != 1 or PRIVILEGES.owner(old) != 0:
                raise PermissionError("protected archive destination replaced")
        except FileNotFoundError:
            pass
        os.replace(temporary, name, src_dir_fd=destination, dst_dir_fd=destination)
        os.fsync(destination)
        return {"bytes": total, "sha256": digest.hexdigest()}
    finally:
        os.close(before_fd)
        if output is not None:
            os.close(output)
        try:
            os.unlink(temporary, dir_fd=destination)
        except FileNotFoundError:
            pass


def archive_rollbacks():
    root_only()
    try:
        jobs = directory(JOBS)
    except FileNotFoundError:
        return {"copied": 0, "failed": 0}
    jobs_info = os.fstat(jobs)
    if PRIVILEGES.owner(jobs_info) != CONFIG["service_uid"] or jobs_info.st_mode & 0o077:
        os.close(jobs)
        raise PermissionError("private ar job state required")
    base = directory(V3)
    rollback = protected_directory(base, "rollback", 0o700)
    archives = protected_directory(rollback, "administrator", 0o700)
    copied = failed = 0
    try:
        # Enumerate only canonical job directories and two exact artifact names.
        for name in sorted(os.listdir(jobs))[:1000]:
            try:
                if str(uuid.UUID(name)) != name:
                    continue
            except ValueError:
                continue
            source = destination = None
            try:
                source = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=jobs)
                info = os.fstat(source)
                if PRIVILEGES.owner(info) != CONFIG["service_uid"] or info.st_mode & 0o077:
                    raise PermissionError("private ar rollback directory required")
                # Skip before deployment has completed its export pair.
                for artifact in ("rollback-images.tar", "container-config.json"):
                    os.stat(artifact, dir_fd=source, follow_symlinks=False)
                destination = protected_directory(archives, name, 0o700)
                manifest = {}
                try:
                    existing = json.loads(read_regular(destination, "manifest.json", 4096, owner=0))
                except FileNotFoundError:
                    existing = {}
                for artifact, bound in (("rollback-images.tar", MAX_IMAGES), ("container-config.json", 1024 * 1024)):
                    source_info = os.stat(artifact, dir_fd=source, follow_symlinks=False)
                    signature = [source_info.st_dev, source_info.st_ino, source_info.st_size, source_info.st_mtime_ns, source_info.st_ctime_ns]
                    if existing.get(artifact, {}).get("signature") == signature:
                        manifest[artifact] = existing[artifact]
                    else:
                        manifest[artifact] = copy_artifact(source, destination, artifact, bound) | {"signature": signature}
                        copied += 1
                publish(destination, "manifest.json", json.dumps(manifest, sort_keys=True).encode())
            except FileNotFoundError:
                pass
            except (OSError, ValueError, TypeError):
                failed += 1
            finally:
                if source is not None:
                    os.close(source)
                if destination is not None:
                    os.close(destination)
        return {"copied": copied, "failed": failed}
    finally:
        for fd in (jobs, archives, rollback, base):
            os.close(fd)



def backup_database(parent, name):
    """Root-only fixed-scope SQLite backup, including committed WAL frames.

    Keep the parent directory pinned, reject links and special files, and
    check source identities again before releasing any bytes to the broker.
    Never checkpoint, chmod, decrypt or otherwise mutate the live database.
    """
    info = os.stat(name, dir_fd=parent, follow_symlinks=False)
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > MAX_INSPECTION_FILE:
        raise ValueError("bounded single-link database required")
    for suffix in ("-wal", "-shm"):
        try:
            sidecar = os.stat(name + suffix, dir_fd=parent, follow_symlinks=False)
        except FileNotFoundError:
            continue
        if not stat.S_ISREG(sidecar.st_mode) or sidecar.st_nlink != 1:
            raise ValueError("database sidecar link rejected")
    uri = Path("/proc/self/fd/" + str(parent) + "/" + name).as_uri() + "?mode=ro"
    deadline = time.monotonic() + 15
    def progress(status, remaining, total):
        if time.monotonic() > deadline or total * page_size > MAX_INSPECTION_FILE:
            raise ValueError("database backup exceeds bound")
    with tempfile.TemporaryDirectory(prefix="v3-inspection-") as temporary:
        destination = Path(temporary) / "backup.db"
        try:
            source = sqlite3.connect(uri, uri=True, timeout=1)
            target = None
            try:
                target = sqlite3.connect(destination)
                page_size = source.execute("PRAGMA page_size").fetchone()[0]
                source.execute("PRAGMA query_only=ON")
                source.backup(target, pages=256, progress=progress, sleep=.01)
            finally:
                source.close()
                if target is not None:
                    target.close()
            after = os.stat(name, dir_fd=parent, follow_symlinks=False)
            if (info.st_dev, info.st_ino) != (after.st_dev, after.st_ino):
                raise ValueError("database identity changed during backup")
            fd = directory(temporary)
            try:
                return read_regular(fd, "backup.db", MAX_INSPECTION_FILE)
            finally:
                os.close(fd)
        except sqlite3.Error as error:
            raise ValueError("consistent database backup unavailable") from error


def snapshot_context(context):
    root_only()
    origin = directory(V3)
    stream = io.BytesIO()
    total = count = 0
    try:
        with tarfile.open(fileobj=stream, mode="w") as archive:
            for name in context:
                if not isinstance(name, str) or name.startswith("/") or ".." in Path(name).parts or not name.startswith(("v3/", "ui/")):
                    raise ValueError("fixed source context rejected")
                if name.startswith("v3/data/") and name not in ("v3/data/config/gates/message_visibility_v1.json", "v3/data/config/gates/closing_status_v1.json", *INSPECTION_CONTEXT):
                    raise ValueError("runtime data outside gate allowlist")
                if name.startswith(("v3/workspaces/", "v3/build-jobs/", "v3/rollback/", "v3/runtime/", "v3/target/")):
                    raise ValueError("runtime directory rejected")
                # Preserve archive names while reading V3 from its independent root.
                inspection = name in INSPECTION_CONTEXT
                source = V3 / name.removeprefix("v3/") if name.startswith("v3/") else SHARED_SOURCE / name
                if name.startswith("v3/data/config/gates/") and not source.exists():source=V3/"config/gates"/source.name
                if name in {"v3/build-service","v3/crates","v3/plugins/engine","v3/services/system-monitoring"} and not source.exists()and not source.is_symlink():continue
                parent_path, leaf = source.parent, source.name
                try:
                    parent = directory(parent_path)
                except FileNotFoundError:
                    if inspection:
                        continue
                    raise
                try:
                    try:
                        info = os.stat(leaf, dir_fd=parent, follow_symlinks=False)
                    except FileNotFoundError:
                        if inspection or name=="v3/ability_plugin_worker.py":
                            continue
                        raise
                    if inspection and not stat.S_ISDIR(info.st_mode):
                        raise ValueError("inspection root must be a directory")
                    if stat.S_ISDIR(info.st_mode):
                        top = os.open(leaf, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent)
                        try:
                            for relative, dirs, files, fd in os.fwalk(".", topdown=True, follow_symlinks=False, dir_fd=top):
                                dirs[:] = [child for child in dirs if child != "__pycache__"]
                                for child in dirs:
                                    if not stat.S_ISDIR(os.stat(child, dir_fd=fd, follow_symlinks=False).st_mode):
                                        raise ValueError("source symlink directory rejected")
                                for child in files:
                                    if child.endswith((".pyc", ".pyo")):
                                        continue
                                    rel = str(Path(name) / relative / child)
                                    if child == "encryption.key":
                                        raise ValueError("encryption key is not an inspection input")
                                    if inspection and child.endswith(("-wal", "-shm", "-journal")):
                                        # Backup produces one self-contained SQLite file.
                                        continue
                                    is_database = child.endswith((".db", ".sqlite", ".sqlite3"))
                                    if not inspection and (is_database or child.endswith((".db-wal", ".db-shm"))):
                                        raise ValueError("database files are not build inputs")
                                    body = backup_database(fd, child) if inspection and is_database else read_regular(fd, child, 16*1024*1024)
                                    mode = 0o440 if inspection else os.stat(child, dir_fd=fd, follow_symlinks=False).st_mode & 0o777
                                    total += len(body); count += 1
                                    if total > 512*1024*1024 or count > 10000:
                                        raise ValueError("fixed source context exceeds bound")
                                    member = tarfile.TarInfo(rel); member.size = len(body); member.mode = mode
                                    archive.addfile(member, io.BytesIO(body))
                        finally:
                            os.close(top)
                    else:
                        body = read_regular(parent, leaf, 16*1024*1024)
                        total += len(body); count += 1
                        if total > 512*1024*1024 or count > 10000:
                            raise ValueError("fixed source context exceeds bound")
                        member = tarfile.TarInfo(name); member.size = len(body); member.mode = info.st_mode & 0o777
                        archive.addfile(member, io.BytesIO(body))
                finally:
                    os.close(parent)
        result = stream.getvalue()
        if len(result) > 512*1024*1024:
            raise ValueError("tar context exceeds bound")
        return result
    finally:
        os.close(origin)


def workspace_directory(workspace):
    root_only()
    if not isinstance(workspace,str) or str(uuid.UUID(workspace))!=workspace:raise ValueError("Canonical workspace ID required")
    parent=directory(Path(CONFIG["workspace_root"]))
    child=None;created=False
    try:
        try:os.mkdir(workspace,0o700,dir_fd=parent);created=True
        except FileExistsError:pass
        child=os.open(workspace,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW|os.O_CLOEXEC,dir_fd=parent)
        meta=os.fstat(child)
        if created:
            os.fchown(child,CONFIG["worker_uid"],CONFIG["app_gid"])
            os.fchmod(child,0o770);os.fsync(child);os.fsync(parent)
        elif meta.st_uid!=CONFIG["worker_uid"] or meta.st_gid!=CONFIG["app_gid"]:
            raise PermissionError("Workspace identity changed")
        return {"workspace":workspace,"created":created}
    finally:
        if child is not None:os.close(child)
        os.close(parent)

def snapshot_server(listener, context, stopping):
    listener.settimeout(.5)
    while not stopping.is_set():
        try:
            conn, _ = listener.accept()
        except socket.timeout:
            continue
        except OSError:
            return
        with conn:
            conn.settimeout(60)
            blob=None
            try:
                _, uid, _ = struct.unpack("3i", conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                if uid != CONFIG["service_uid"]:
                    raise PermissionError("snapshot caller denied")
                request=bytearray()
                while not request.endswith(b"\n") and len(request)<=64:
                    part=conn.recv(65-len(request))
                    if not part:raise PermissionError("Incomplete helper request")
                    request.extend(part)
                request=bytes(request)
                if request in (b"{}\n",b'{"inspection":true}\n'):
                    blob=snapshot_context(context+INSPECTION_CONTEXT if request!=b"{}\n" else context)
                else:
                    value=json.loads(request)
                    if not isinstance(value,dict) or set(value)!={"workspace"}:raise PermissionError("Helper operation denied")
                    blob=json.dumps(workspace_directory(value["workspace"]),separators=(",",":")).encode()
                conn.sendall(struct.pack("!Q", len(blob)))
                conn.sendall(blob)
            except (OSError, ValueError, PermissionError):
                try:
                    conn.sendall(struct.pack("!Q", 0))
                except OSError:
                    pass
            finally:
                # Never retain the last inspection archive while waiting for another request.
                blob=None


def _drop_and_exec(host_fd, listener_fd, bridge_fd):
    account=pwd.getpwnam(CONFIG["service_account"])
    if account.pw_uid!=CONFIG["service_uid"] or account.pw_gid!=CONFIG["service_gid"]:raise PermissionError("Service identity changed")
    os.initgroups(account.pw_name,account.pw_gid)
    os.setgroups(sorted(set(os.getgroups())|{CONFIG["app_gid"],CONFIG["worker_gid"]}))
    os.setgid(account.pw_gid)
    os.setuid(account.pw_uid)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(38, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "broker no_new_privs failed")
    os.set_inheritable(host_fd, True)
    os.set_inheritable(listener_fd, True)
    os.set_inheritable(bridge_fd, True)
    # fd pins the protected code inode even if a source author renames its
    # enclosing checkout directory. No root code is loaded after privilege drop.
    os.execve("/usr/bin/python3", ["python3", "-I", "/proc/self/fd/" + str(host_fd), "--listener-fd", str(listener_fd), "--bridge-listener-fd", str(bridge_fd), "--snapshot-socket", str(RUNTIME/"snapshot.sock")],
              {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": CONFIG["service_home"], "LANG": "C.UTF-8", "WEBAGENT_BUILD_CONFIG":os.environ["WEBAGENT_BUILD_CONFIG"], "WEBAGENT_BUILD_MODULE_ROOT":str(SERVICE)})


def remove_stale_socket(directory_fd,name,owner):
    """Remove only the expected socket through the retained runtime directory FD."""
    try:existing=os.stat(name,dir_fd=directory_fd,follow_symlinks=False)
    except FileNotFoundError:return
    if not stat.S_ISSOCK(existing.st_mode) or existing.st_uid!=owner:raise PermissionError("Snapshot socket replaced")
    os.unlink(name,dir_fd=directory_fd)

def _supervisor(service, ready):
    _COPY_STOP.clear()
    stopping = False
    child = None
    def stop(_signum, _frame):
        nonlocal stopping
        stopping = True
        _COPY_STOP.set()
        if child:
            try:
                os.kill(child, signal.SIGTERM)
            except ProcessLookupError:
                pass
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    runtime_parent=directory(RUNTIME.parent)
    # The app owns the socket; Codex needs directory traversal to reach it.
    # Neither account can replace entries in this protected directory.
    runtime = protected_directory(runtime_parent,RUNTIME.name,0o751,gid=CONFIG["service_gid"])
    os.fchown(runtime,0,CONFIG["service_gid"])  # Also repair an existing protected directory.
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    bridge_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    host_fd = None
    child = None
    snapshot_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    resource_listener = socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
    snapshot_stopping = threading.Event()
    try:
        # Bind through a retained directory descriptor, not a mutable path.
        sockname = "/proc/self/fd/" + str(runtime) + "/build.sock"
        try:
            old = os.stat("build.sock", dir_fd=runtime, follow_symlinks=False)
            if not stat.S_ISSOCK(old.st_mode) or old.st_uid != CONFIG["app_uid"]:
                raise PermissionError("unexpected broker socket")
            os.unlink("build.sock", dir_fd=runtime)
        except FileNotFoundError:
            pass
        listener.bind(sockname)
        os.chown(sockname,CONFIG["app_uid"],CONFIG["service_gid"])
        os.chmod(sockname, 0o660)
        listener.listen(16)
        # Dedicated V2 Python bridge: host worker authenticates HTTP callers,
        # while this socket assigns its own broker identity, never Codex's.
        bridge_name = "/proc/self/fd/" + str(runtime) + "/python.sock"
        try:
            old_bridge = os.stat("python.sock", dir_fd=runtime, follow_symlinks=False)
            if not stat.S_ISSOCK(old_bridge.st_mode) or old_bridge.st_uid != CONFIG["service_uid"]:
                raise PermissionError("unexpected Python bridge socket")
            os.unlink("python.sock", dir_fd=runtime)
        except FileNotFoundError:
            pass
        bridge_listener.bind(bridge_name)
        os.chown(bridge_name,CONFIG["service_uid"],CONFIG["service_gid"])
        os.chmod(bridge_name, 0o600)
        bridge_listener.listen(16)
        host_fd = os.open("host.py", os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=service)
        info = os.fstat(host_fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or PRIVILEGES.owner(info) != 0 or info.st_mode & 0o022:
            raise PermissionError("protected host code replaced")
        host_body = read_regular(service, "host.py", 2*1024*1024, owner=0)
        tree = ast.parse(host_body)
        context = next(ast.literal_eval(node.value) for node in tree.body if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == "CONTEXT" for target in node.targets))
        snapshot_path = "/proc/self/fd/" + str(runtime) + "/snapshot.sock"
        remove_stale_socket(runtime,"snapshot.sock",CONFIG["service_uid"])
        snapshot_listener.bind(snapshot_path)
        os.chown(snapshot_path,CONFIG["service_uid"],CONFIG["service_gid"])
        os.chmod(snapshot_path, 0o600)
        snapshot_listener.listen(4)
        resource_path="/proc/self/fd/"+str(runtime)+"/resource.sock"
        remove_stale_socket(runtime,"resource.sock",CONFIG["service_uid"])
        resource_listener.bind(resource_path)
        os.chown(resource_path,CONFIG["service_uid"],CONFIG["service_gid"]);os.chmod(resource_path,0o600);resource_listener.listen(8)
        resource_body=read_regular(service,"resource_control.py",2*1024*1024,owner=0)
        resource_namespace={"__name__":"protected_build_resources"}
        exec(compile(resource_body,"protected-resource-control.py","exec"),resource_namespace)
        error_fd = os.open("broker-error.log", os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=runtime)
        error_info = os.fstat(error_fd)
        if not stat.S_ISREG(error_info.st_mode) or error_info.st_nlink != 1 or PRIVILEGES.owner(error_info) != 0:
            os.close(error_fd)
            raise PermissionError("private broker error destination replaced")
        os.fchmod(error_fd, 0o600)
        child = os.fork()
        if child == 0:
            os.setsid()
            os.dup2(error_fd, 2)
            os.close(error_fd)
            _drop_and_exec(host_fd, listener.fileno(), bridge_listener.fileno())
        os.close(error_fd)
        threading.Thread(target=snapshot_server, args=(snapshot_listener, context, snapshot_stopping), daemon=True).start()
        threading.Thread(target=resource_namespace["serve"],args=(resource_listener,snapshot_stopping,CONFIG),daemon=True).start()
        publish(runtime, "supervisor.pid", str(os.getpid()).encode())
        publish(runtime, "broker.pid", str(child).encode())
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            pid, _ = os.waitpid(child, os.WNOHANG)
            if pid:
                child = None
                raise RuntimeError("broker exited during startup")
            try:
                probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                probe.settimeout(.2)
                probe.connect(sockname)
                # An unauthorized root probe should receive a bounded denial;
                # that confirms the broker accept loop rather than just bind.
                denial = probe.recv(4096)
                probe.close()
                if denial:
                    break
            except (OSError, TimeoutError):
                time.sleep(.1)
        else:
            raise RuntimeError("broker did not become ready")
        publish(runtime, "ready.json", json.dumps({"ready": True, "supervisor_pid": os.getpid(), "broker_pid": child}).encode())
        os.write(ready, b"ready\n")
        os.close(ready)
        ready = None
        while not stopping:
            pid, _ = os.waitpid(child, os.WNOHANG)
            if pid:
                child = None
                break
            report = archive_rollbacks()
            publish(runtime, "archive-status.json", json.dumps(report).encode())
            for _ in range(50):
                if stopping:
                    break
                time.sleep(.1)
    finally:
        if child:
            try:
                os.kill(child, signal.SIGTERM)
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if os.waitpid(child, os.WNOHANG)[0]:
                        child = None
                        break
                    time.sleep(.1)
                if child:
                    os.killpg(child, signal.SIGKILL)
                    os.waitpid(child, 0)
            except ProcessLookupError:
                pass
        snapshot_stopping.set()
        snapshot_listener.close()
        resource_listener.close()
        publish(runtime, "ready.json", b'{"ready":false}')
        if ready is not None:
            os.close(ready)
        listener.close()
        bridge_listener.close()
        if host_fd is not None:
            os.close(host_fd)
        os.close(runtime)
        os.close(runtime_parent)


def start():
    root_only()
    if Path(__file__).resolve() != SERVICE / "bootstrap.py":
        raise PermissionError("start only the installed protected bootstrap copy")
    service = directory(SERVICE)
    runtime = directory(RUNTIME)
    try:
        manifest = json.loads(read_regular(service, "installed.json", 4096, owner=0))
        for name, key in (("host.py", "host_sha256"), ("bootstrap.py", "bootstrap_sha256"), ("release.py", "release_sha256"), ("index-scanner.py", "scanner_sha256"), ("performance.py", "performance_sha256")):
            body = read_regular(service, name, 2 * 1024 * 1024, owner=0)
            if hashlib.sha256(body).hexdigest() != manifest[key]:
                raise ValueError("installed broker code digest changed")
        for name,expected in manifest.get("modules",{}).items():
            if hashlib.sha256(read_regular(service,name,2*1024*1024,owner=0)).hexdigest()!=expected:raise ValueError("Installed module changed")
        try:
            pid = int(read_regular(runtime,"supervisor.pid",64,owner=0))
            os.kill(pid, 0)
            raise ValueError("broker supervisor already running")
        except (FileNotFoundError, ProcessLookupError):
            pass
        read_fd, write_fd = os.pipe()
        child = os.fork()
        if child == 0:
            os.close(read_fd)
            os.setsid()
            null = os.open("/dev/null", os.O_RDWR)
            for fd in (0, 1, 2):
                os.dup2(null, fd)
            if null > 2:
                os.close(null)
            try:
                _supervisor(service, write_fd)
            except BaseException as error:
                try:
                    publish(runtime,"startup-error.json", json.dumps({"error": type(error).__name__, "reason": str(error)[:512]}).encode())
                except BaseException:
                    pass
                os._exit(1)
            os._exit(0)
        os.close(write_fd)
        import select
        try:
            if not select.select([read_fd], [], [], 12)[0] or os.read(read_fd, 64) != b"ready\n":
                raise RuntimeError("protected broker startup failed; inspect private readiness files")
        finally:
            os.close(read_fd)
        return {"started": True, "supervisor_pid": child}
    finally:
        os.close(service)


def snapshot():
    """Export only the installed broker's fixed build allowlist."""
    root_only()
    if Path(__file__).resolve() != SERVICE / "bootstrap.py":
        raise PermissionError("snapshot only through installed protected bootstrap")
    service = directory(SERVICE)
    try:
        body = read_regular(service, "host.py", 2 * 1024 * 1024, owner=0)
        tree = ast.parse(body)
        context = next(ast.literal_eval(node.value) for node in tree.body
            if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name)
            and target.id == "CONTEXT" for target in node.targets))
        return snapshot_context(context)
    finally:
        os.close(service)


def main():
    parser = argparse.ArgumentParser()
    actions = parser.add_mutually_exclusive_group(required=True)
    actions.add_argument("--install", action="store_true")
    actions.add_argument("--start", action="store_true")
    actions.add_argument("--snapshot", action="store_true")
    actions.add_argument("--foreground",action="store_true")
    parser.add_argument("--install-tree",help="Reviewed isolated task tree hash; operator install only")
    args = parser.parse_args()
    if args.install_tree and not args.install:
        parser.error("--install-tree requires --install")
    if args.foreground:
        root_only()
        if Path(__file__).resolve()!=SERVICE/"bootstrap.py":raise PermissionError("Installed bootstrap required")
        service=directory(SERVICE)
        manifest=json.loads(read_regular(service,"installed.json",8192,owner=0))
        for name,key in (("host.py","host_sha256"),("bootstrap.py","bootstrap_sha256"),("release.py","release_sha256"),("performance.py","performance_sha256"),("index-scanner.py","scanner_sha256")):
            if hashlib.sha256(read_regular(service,name,2*1024*1024,owner=0)).hexdigest()!=manifest[key]:raise ValueError("Installed code changed")
        for name,expected in manifest.get("modules",{}).items():
            if hashlib.sha256(read_regular(service,name,2*1024*1024,owner=0)).hexdigest()!=expected:raise ValueError("Installed module changed")
        ready=os.open("/dev/null",os.O_WRONLY);_supervisor(service,ready);return
    if args.snapshot:
        sys.stdout.buffer.write(snapshot())
        return
    result = install(args.install_tree) if args.install else start()
    # Only content-free hashes/PIDs; no container environment or credentials.
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
