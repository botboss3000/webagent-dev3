"""Isolated v3 tool worker. No application credentials are loaded here."""
import asyncio
import ctypes
import contextlib
import fnmatch
import hashlib
import fcntl
import socket
import struct
import subprocess
import sys
import time
import json
import importlib.util
import os
import select
import signal
import stat
import shutil
import tempfile
import uuid
from pathlib import Path


LIMIT=1024*1024
FRAME_LIMIT=8*LIMIT
CONTEXT_CONFIG_PATH=Path("/live-checkout/crates/modules/context/context-compaction.json")
LIVE_CHECKOUT=Path("/live-checkout")
def search_limits():
    """The live compaction file (live-files.json) is read on every search, without a restart."""
    try:
        config = json.loads(CONTEXT_CONFIG_PATH.read_text())
    except FileNotFoundError:
        config = {}
    output = config.get("search_output_bytes", 32768)
    line = config.get("search_line_bytes", 2048)
    if (type(output) is not int or not 4096 <= output <= 262144
            or type(line) is not int or not 128 <= line <= 8192 or line > output):
        raise ValueError("invalid context search limits")
    return output, line

def file_operation(request, base, args, name):
    # checkout_edit is the one direct live-checkout writer; policy can deny it alone.
    checkout = name == "checkout_edit"
    if checkout:
        name = "file_write" if "content" in args else "file_edit"
    raw = args.get("path", ".")
    relative = Path(raw)
    # Resolve ordinary container paths, including parent traversal and symlinks.
    # The OS identity and container mounts determine which targets are reachable.
    default_root = LIVE_CHECKOUT if LIVE_CHECKOUT.is_dir() else base
    target = (relative if relative.is_absolute() else default_root / relative).resolve()
    if name in ("file_write","file_edit")and LIVE_CHECKOUT.is_dir()and not checkout:
        live=LIVE_CHECKOUT.resolve()
        if target==live or target.is_relative_to(live)or any(parent.exists()and parent.samefile(live)for parent in target.parents):
            raise ValueError("Source writes require broker-owned task routing; direct source effects denied")
    rootfd = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    basefd = None
    directory = None
    try:
        basefd = os.dup(rootfd)
        parts = target.parts[1:]
        directory = os.dup(basefd)
        for part in parts[:-1]:
            if name == "file_write":
                try:
                    os.mkdir(part, mode=0o755 if checkout else 0o770, dir_fd=directory)
                except FileExistsError:
                    pass
                else:
                    if checkout:
                        os.chmod(part, 0o755, dir_fd=directory)
                    os.fsync(directory)
            nextfd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = nextfd
        leaf = parts[-1] if parts else "."
        scan_limited = False
        if name == "file_edit":
            if leaf == ".":
                raise ValueError("path must name a file")
            old_text, new_text = args["old_text"], args["new_text"]
            if not isinstance(old_text, str) or not old_text:
                raise ValueError("old_text must be nonempty text")
            if not isinstance(new_text, str):
                raise ValueError("new_text must be text")
            expected_hash = args.get("expected_sha256")
            if expected_hash is not None and (
                not isinstance(expected_hash, str) or len(expected_hash) != 64
                or any(c not in "0123456789abcdef" for c in expected_hash)
            ):
                raise ValueError("invalid expected_sha256")
            fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
            try:
                original_stat = os.fstat(fd)
                if not stat.S_ISREG(original_stat.st_mode):
                    raise ValueError("only regular files are supported")
                with os.fdopen(fd, "rb", closefd=False) as source:
                    original = source.read(LIMIT + 1)
            finally:
                os.close(fd)
            if len(original) > LIMIT:
                raise ValueError("file content exceeds limit")
            digest = hashlib.sha256(original).hexdigest()
            if expected_hash is not None and digest != expected_hash:
                raise ValueError("file changed since inspection")
            before = old_text.encode()
            if original.count(before) != 1:
                raise ValueError("old_text must match exactly once")
            updated = original.replace(before, new_text.encode(), 1)
            if len(updated) > LIMIT:
                raise ValueError("file content exceeds limit")
            temporary = f".v3-edit-{uuid.uuid4().hex}"
            tempfd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                             stat.S_IMODE(original_stat.st_mode), dir_fd=directory)
            try:
                with os.fdopen(tempfd, "wb", closefd=False) as target:
                    target.write(updated)
                    target.flush()
                    os.fsync(target.fileno())
                os.fchmod(tempfd, stat.S_IMODE(original_stat.st_mode))
                current = os.stat(leaf, dir_fd=directory, follow_symlinks=False)
                if not stat.S_ISREG(current.st_mode) or (
                    current.st_dev, current.st_ino, current.st_mtime_ns, current.st_size
                ) != (
                    original_stat.st_dev, original_stat.st_ino,
                    original_stat.st_mtime_ns, original_stat.st_size
                ):
                    raise ValueError("file changed since inspection")
                os.replace(temporary, leaf, src_dir_fd=directory, dst_dir_fd=directory)
                os.fsync(directory)
            finally:
                os.close(tempfd)
                with contextlib.suppress(FileNotFoundError):
                    os.unlink(temporary, dir_fd=directory)
            return {"status": "success", "output": "edited",
                    "previous_sha256": digest, "sha256": hashlib.sha256(updated).hexdigest()}
        if name == "file_write":
            if leaf == ".":
                raise ValueError("path must name a file")
            content = args["content"].encode()
            if len(content) > LIMIT:
                raise ValueError("file content exceeds limit")
            try:
                previous = os.stat(leaf, dir_fd=directory, follow_symlinks=False)
            except FileNotFoundError:
                previous = None
            if previous is not None and not stat.S_ISREG(previous.st_mode):
                raise ValueError("only regular files are supported")
            mode = stat.S_IMODE(previous.st_mode) if previous else (0o644 if checkout else 0o660)
            temporary = f".v3-write-{uuid.uuid4().hex}"
            tempfd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                             0o600, dir_fd=directory)
            try:
                with os.fdopen(tempfd, "wb", closefd=False) as target:
                    target.write(content)
                    target.flush()
                    os.fchmod(tempfd, mode)
                    os.fsync(target.fileno())
                if previous is None:
                    # linkat publishes atomically and refuses a concurrently created path.
                    os.link(temporary, leaf, src_dir_fd=directory, dst_dir_fd=directory,
                            follow_symlinks=False)
                    os.unlink(temporary, dir_fd=directory)
                else:
                    current = os.stat(leaf, dir_fd=directory, follow_symlinks=False)
                    if not stat.S_ISREG(current.st_mode) or (
                        current.st_dev, current.st_ino, current.st_mtime_ns, current.st_size
                    ) != (
                        previous.st_dev, previous.st_ino, previous.st_mtime_ns, previous.st_size
                    ):
                        raise ValueError("file changed during write")
                    os.replace(temporary, leaf, src_dir_fd=directory, dst_dir_fd=directory)
                try:
                    os.fsync(directory)
                except OSError:
                    return {"status": "uncertain",
                            "output": "File published; durability confirmation failed"}
            finally:
                os.close(tempfd)
                with contextlib.suppress(FileNotFoundError):
                    os.unlink(temporary, dir_fd=directory)
            output = "written"
        elif name == "file_read":
            offset = args.get("offset", 1)
            limit = args.get("limit", 2000)
            if type(offset) is not int or offset < 1 or offset > 10_000_000:
                raise ValueError("offset must be a positive line number")
            if type(limit) is not int or not 1 <= limit <= 2000:
                raise ValueError("limit must be between 1 and 2000 lines")
            fd = os.open(leaf, os.O_NOFOLLOW | os.O_NONBLOCK | os.O_RDONLY, dir_fd=directory)
            if not stat.S_ISREG(os.fstat(fd).st_mode):
                os.close(fd)
                raise ValueError("only regular files are supported")
            output, output_size, total, next_offset, truncated = [], 0, 0, None, False
            size = 0
            with os.fdopen(fd, "rb") as file:
                if os.fstat(file.fileno()).st_size > 32 * LIMIT:
                    raise ValueError("file exceeds inspection limit")
                while True:
                    line = file.readline(LIMIT + 1)
                    if not line:
                        break
                    size += len(line)
                    if size > 32 * LIMIT or len(line) > LIMIT:
                        raise ValueError("file exceeds inspection limit")
                    total += 1
                    if total < offset or next_offset is not None:
                        continue
                    if total >= offset + limit or output_size + len(line) > LIMIT:
                        next_offset = total
                        truncated = True
                        continue
                    output.append(line)
                    output_size += len(line)
            return {"status": "success",
                    "output": b"".join(output).decode(errors="replace"),
                    "offset": offset, "limit": limit, "total_lines": total,
                    "next_offset": next_offset, "truncated": truncated}
        elif name in ("file_list", "file_search"):
            if name == "file_list":
                fd = os.open(leaf, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                             dir_fd=directory)
                try:
                    names = []
                    with os.scandir(fd) as entries:
                        for entry in entries:
                            if len(names) >= 10000:
                                scan_limited = True
                                break
                            names.append(entry.name)
                    offset, limit = args.get("offset", 0), args.get("limit", 100)
                    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 100:
                        raise ValueError("bounded directory page required")
                    names.sort()
                    selected = names[offset:offset + limit]
                    return {"status": "success", "output": "\n".join(selected),
                        "offset": offset, "next_offset": offset + len(selected),
                        "has_more": offset + len(selected) < len(names), "truncated": scan_limited}
                finally:
                    os.close(fd)
            else:
                query = args.get("query", "")
                regex = args.get("regex", False)
                comments_only = args.get("comments_only", False)
                file_glob = args.get("file_glob", "")
                if (not isinstance(query, str) or len(query) > 200
                    or type(regex) is not bool or type(comments_only) is not bool
                    or not isinstance(file_glob, str) or len(file_glob) > 200):
                    raise ValueError("invalid search options")
                if not query and not comments_only:
                    raise ValueError("query must not be empty")
                if any(char in file_glob for char in ("\\", "\x00")):
                    raise ValueError("invalid file glob")
                fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                             dir_fd=directory)
                search_output_bytes, search_line_bytes = search_limits()
                found, size = [], 0
                remaining = [10000, 32 * LIMIT]
                deadline = time.monotonic() + 2
                markers = {
                    ".py": ("#",), ".sh": ("#",), ".toml": ("#",),
                    ".yaml": ("#",), ".yml": ("#",),
                    ".rs": ("//", "/*", "*", "*/"),
                    ".js": ("//", "/*", "*", "*/"),
                    ".ts": ("//", "/*", "*", "*/"),
                    ".css": ("/*", "*", "*/"),
                    ".c": ("//", "/*", "*", "*/"),
                    ".h": ("//", "/*", "*", "*/"),
                }

                def scan_file(open_fd, label):
                    nonlocal size, scan_limited
                    if file_glob and not (fnmatch.fnmatchcase(label, file_glob)
                                          or fnmatch.fnmatchcase(Path(label).name, file_glob)):
                        return
                    if remaining[1] <= 0:
                        scan_limited = True
                        return
                    raw_data = os.read(open_fd, min(LIMIT + 1, remaining[1]))
                    remaining[1] -= len(raw_data)
                    if len(raw_data) > LIMIT:
                        scan_limited = True
                        raw_data = raw_data[:LIMIT]
                    lines = raw_data.decode(errors="replace").splitlines()
                    if regex and query:
                        try:
                            result = subprocess.run(
                                ["rg", "--line-number", "--no-heading", "--color", "never",
                                 "--max-count", "1001", "--max-columns", "1",
                                 "--max-columns-preview", "-e", query, "-"],
                                input=raw_data, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE,
                                timeout=max(0.01, deadline - time.monotonic()),
                                check=False)
                        except subprocess.TimeoutExpired:
                            scan_limited = True
                            return
                        if result.returncode not in (0, 1):
                            raise ValueError("invalid search regex")
                        hit_lines = set()
                        for row in result.stdout.splitlines():
                            number = row.split(b":", 1)[0]
                            if number.isdigit():
                                hit_lines.add(int(number))
                        if len(hit_lines) > 1000:
                            scan_limited = True
                    else:
                        hit_lines = None
                    comment_prefixes = markers.get(Path(label).suffix.lower(), ())
                    for index, line in enumerate(lines, 1):
                        if time.monotonic() >= deadline:
                            scan_limited = True
                            return
                        if comments_only and not line.lstrip().startswith(comment_prefixes):
                            continue
                        if query and (index not in hit_lines if hit_lines is not None
                                      else query not in line):
                            continue
                        if len(line.encode()) > search_line_bytes:
                            start = max(0, line.find(query) - search_line_bytes // 3) if query and not regex else 0
                            line = line[start:start + search_line_bytes] + " …[line clipped]"
                            scan_limited = True
                        result = f"{label}:{index}:{line}"
                        encoded_size = len(result.encode()) + bool(found)
                        if size + encoded_size > search_output_bytes or len(found) >= 200:
                            scan_limited = True
                            return
                        found.append(result)
                        size += encoded_size

                def scan_dir(parent, prefix, depth=0):
                    nonlocal scan_limited
                    if depth > 32:
                        scan_limited = True
                        return
                    names = []
                    with os.scandir(parent) as entries:
                        for entry in entries:
                            if len(names) >= remaining[0]:
                                scan_limited = True
                                break
                            names.append(entry.name)
                    for entry in sorted(names):
                        if depth == 0 and raw in {"/live-checkout", "/live-checkout/v3"} and entry in {
                                "build-jobs", "workspaces", "target", "v2_ref", "data",
                                "rollback", "runtime", "gate-runtime", "host-build-service"}:
                            continue
                        if (remaining[0] <= 0 or remaining[1] <= 0
                            or time.monotonic() >= deadline or size >= search_output_bytes):
                            scan_limited = True
                            return
                        remaining[0] -= 1
                        try:
                            child = os.open(entry, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                                            dir_fd=parent)
                        except OSError:
                            continue
                        try:
                            mode = os.fstat(child).st_mode
                            label = str(Path(prefix) / entry)
                            if stat.S_ISDIR(mode):
                                scan_dir(child, label, depth + 1)
                            elif stat.S_ISREG(mode):
                                scan_file(child, label)
                        finally:
                            os.close(child)
                try:
                    mode = os.fstat(fd).st_mode
                    if stat.S_ISREG(mode):
                        if not query and not comments_only:
                            raise ValueError("query must not be empty")
                        scan_file(fd, raw)
                    elif stat.S_ISDIR(mode):
                        if not query:
                            raise ValueError("a query is required for directory search")
                        scan_dir(fd, raw if raw != "." else "")
                    else:
                        raise ValueError("search path must be a regular file or directory")
                    output = "\n".join(found)
                finally:
                    os.close(fd)
        else:
            raise ValueError("unknown tool")
        encoded = output.encode()
        return {"status": "success", "output": encoded[:LIMIT].decode(errors="replace"),
                "truncated": len(encoded) > LIMIT or scan_limited}
    finally:
        if directory is not None:
            os.close(directory)
        if basefd is not None:
            os.close(basefd)
        os.close(rootfd)

IMPLEMENTATION_VERSION = "1.1.0"
TOOLS = ["file_list","file_read","file_search","file_edit","file_write","build_work",
         "state_inspect","prompt_slot_read","prompt_slot_write","shell","python","checkout_edit"]
# Runtime state and other writers' material never change through a source edit.
CHECKOUT_PRIVATE = {"data","workspaces","target",".git","build-jobs","deployment-secrets",
                    "runtime","rollback","gate-runtime"}
def route(name,args):
    if name not in TOOLS or not isinstance(args,dict):raise ValueError("Unsupported Codebase Admin contract")
    if name in ("state_inspect","prompt_slot_read","prompt_slot_write"):return {"route":"core","arguments":args}
    if name=="build_work" or args.get("task_id") is not None:
        if name not in ("build_work","file_list","file_read","file_search","file_edit","file_write"):raise ValueError("Task scope unsupported")
        return {"route":"broker","arguments":{k:v for k,v in args.items() if v is not None}}
    if name=="checkout_edit":
        raw=args.get("path")
        if not isinstance(raw,str)or not raw:raise ValueError("path is required")
        relative=raw.removeprefix(str(LIVE_CHECKOUT)+"/")
        if relative.startswith("/")or any(p in ("",".","..")for p in relative.split("/"))or relative.split("/")[0]in CHECKOUT_PRIVATE:
            raise ValueError("Path must be a live-checkout source path outside runtime state")
        if ("content" in args)==("old_text" in args):raise ValueError("Pass content to write a file, or old_text and new_text to edit one")
        return {"route":"file","arguments":{k:v for k,v in args.items()if v is not None}|{"path":str(LIVE_CHECKOUT/relative)}}
    if name in ("shell","python"):
        return {"route":"command","argv":["/bin/sh","-c",args["command"]] if name=="shell" else ["/usr/bin/python3","-I","-c",args["code"]]}
    if name in ("file_edit","file_write"):
        raw=args.get("path","")
        if isinstance(raw,str)and(raw.startswith("/live-checkout/")or(raw and not raw.startswith("/"))):
            relative=raw.removeprefix("/live-checkout/")
            if any(p in ("",".","..")for p in relative.split("/"))or relative.split("/")[0]in {"data","workspaces","target",".git","build-jobs","deployment-secrets"}:
                raise ValueError("Source path excluded; use the owning runtime/workspace API")
            return {"route":"broker","arguments":{k:v for k,v in args.items()if v is not None}|{"path":relative}}
    live={k:v for k,v in args.items() if v is not None and k!="task_id"}
    if "request_id" in live or "expected_inputs_sha256" in live:raise ValueError("Task write arguments require task_id; live fallback denied")
    return {"route":"file","arguments":live}
def invoke(request):
    if type(request.get("version")) is not int or request["version"]!=1:raise ValueError("Unsupported Codebase Admin protocol")
    name,args=request["name"],request["arguments"];selected=route(name,args)
    if request.get("operation")=="plan":return {"status":"success","output":selected}
    if selected["route"]!="file":raise ValueError("Only file effects execute in the plugin process")
    return file_operation(request,Path(request["base"]),selected["arguments"],name)
def main():
    raw=sys.stdin.buffer.read(FRAME_LIMIT+1)
    try:
        if len(raw)>FRAME_LIMIT:raise ValueError("Plugin request oversized")
        result=invoke(json.loads(raw))
    except Exception as error:result={"status":"failed","output":str(error)[:4096]}
    result["implementation_version"]=IMPLEMENTATION_VERSION
    encoded=json.dumps({"version":1,"result":result}).encode()
    if len(encoded)>2*LIMIT-16384:encoded=b'{"version":1,"result":{"status":"failed","output":"Plugin response oversized"}}'
    sys.stdout.buffer.write(encoded)
if __name__=="__main__":main()
