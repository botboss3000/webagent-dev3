"""Trusted source operations, run inside disposable workspace executors."""
import asyncio,ctypes,fcntl,hashlib,json,os,select,shutil,signal,stat,subprocess,sys,tempfile,time,uuid
from pathlib import Path
def configure(worker):
 global WORKER_PATH
 WORKER_PATH=Path(worker.__file__).resolve()
 for name in ("CONTROL","LIMIT","ROOT","TEMPLATE","git_capture","administrator_plugin","terminate"):
  globals()[name]=getattr(worker,name)
def source_review(base):
    """Show tracked and new source changes without altering the Git index."""
    source = base / "webagent-v3"
    if source.is_symlink() or not source.is_dir() or source.resolve().parent != base.resolve():
        raise ValueError("source snapshot unavailable")
    git_dir = source / ".git"
    if git_dir.is_symlink() or not git_dir.is_dir():
        raise ValueError("source baseline unavailable")
    status, limited = git_capture(
        source, ["status", "--short", "--untracked-files=all",
                 "--ignore-submodules=all"], 64 * 1024)
    status_output = status.decode(errors="replace")
    baseline, limited_baseline = git_capture(source, ["rev-parse", "--verify", "HEAD"], 128)
    baseline = baseline.decode().strip()
    if (limited_baseline or len(baseline) not in (40, 64)
        or any(char not in "0123456789abcdef" for char in baseline)):
        raise ValueError("source baseline unavailable")

    def report(raw_diff, truncated):
        text = raw_diff.decode(errors="replace")
        return {"status": "success", "status_output": status_output,
                "baseline_commit": baseline, "diff": text, "truncated": truncated,
                "diff_sha256": None if truncated else hashlib.sha256(text.encode()).hexdigest(),
                "review_sha256": None if truncated else hashlib.sha256(
                    baseline.encode() + b"\0" + text.encode()).hexdigest()}

    if limited:
        return report(b"", True)
    tracked, limited = git_capture(
        source, ["diff", "--no-ext-diff", "--no-textconv", "--binary",
                 "--ignore-submodules=all", "HEAD", "--", "."])
    diff = bytearray(tracked)
    if limited:
        return report(diff, True)
    untracked, limited = git_capture(
        source, ["ls-files", "--others", "--exclude-standard", "-z"], 64 * 1024)
    if limited:
        return report(diff, True)
    paths = [os.fsdecode(item) for item in untracked.split(b"\0") if item]
    incomplete = len(paths) > 128
    for raw in paths[:128]:
        relative = Path(raw)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("invalid source path")
        path = source / relative
        try:
            info = path.lstat()
        except FileNotFoundError:
            incomplete = True
            continue
        if not (stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode)) or info.st_size > LIMIT:
            incomplete = True
            continue
        remaining = LIMIT - len(diff)
        if remaining <= 0:
            incomplete = True
            break
        patch, limited = git_capture(
            source, ["diff", "--no-index", "--no-ext-diff", "--no-textconv",
                     "--binary", "--", "/dev/null", raw], remaining, (1,))
        diff.extend(patch)
        if limited:
            incomplete = True
            break
    return report(diff, incomplete)


def source_patch(base, args):
    """Apply a checked text patch only within this session's seeded source."""
    source = base / "webagent-v3"
    if source.is_symlink() or not source.is_dir() or source.resolve().parent != base.resolve():
        raise ValueError("source snapshot unavailable")
    git_dir = source / ".git"
    if git_dir.is_symlink() or not git_dir.is_dir():
        raise ValueError("source baseline unavailable")
    patch = args.get("patch")
    if not isinstance(patch, str) or not patch.strip():
        raise ValueError("patch is required")
    encoded = patch.encode()
    if len(encoded) > LIMIT:
        raise ValueError("patch exceeds limit")
    expected = args.get("expected_review_sha256")
    if not isinstance(expected, str) or len(expected) != 64 or any(
        char not in "0123456789abcdef" for char in expected
    ):
        raise ValueError("invalid expected review digest")
    current = source_review(base)
    if current["truncated"] or current["review_sha256"] != expected:
        raise ValueError("source changed since review")
    for line in patch.splitlines():
        if line.startswith(("GIT binary patch", "Binary files ", "rename from ",
                            "rename to ", "copy from ", "copy to ")):
            raise ValueError("binary, rename, and copy patches are unsupported")
        if line.startswith(("new file mode ", "deleted file mode ",
                            "old mode ", "new mode ")):
            if line.rsplit(" ", 1)[-1] not in ("100644", "100755"):
                raise ValueError("unsupported file mode")
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb", prefix=".v3-patch-", dir=base, delete=False
        ) as file:
            temporary = Path(file.name)
            file.write(encoded)
            file.flush()
            os.fsync(file.fileno())
        numstat, limited = git_capture(
            source, ["apply", "--numstat", "-z", str(temporary)], 64 * 1024)
        if limited or not numstat.endswith(b"\0"):
            raise ValueError("invalid or oversized patch path list")
        records = [record for record in numstat.split(b"\0") if record]
        if not records or len(records) > 64:
            raise ValueError("patch must change 1 to 64 files")
        paths = []
        forbidden = {".git", "target", "data", "workspaces",
                     ".cache", ".code-index", "deployment-secrets"}
        for record in records:
            fields = record.split(b"\t", 2)
            if len(fields) != 3 or not fields[0].isdigit() or not fields[1].isdigit():
                raise ValueError("unsupported patch path or binary content")
            raw = fields[2].decode("utf-8")
            parts = raw.split("/")
            if (not raw or raw.startswith("/") or "\\" in raw
                or any(not part or part in (".", "..") or part in forbidden for part in parts)
                or any(ord(char) < 32 or ord(char) == 127 for char in raw)
                or raw in paths):
                raise ValueError("patch path escapes permitted source")
            cursor = source
            for index, part in enumerate(parts):
                cursor = cursor / part
                if cursor.is_symlink():
                    raise ValueError("patch path contains symlink")
                if cursor.exists() and index < len(parts) - 1 and not cursor.is_dir():
                    raise ValueError("patch path has non-directory parent")
                if cursor.exists() and index == len(parts) - 1 and not cursor.is_file():
                    raise ValueError("patch path is not a regular file")
            paths.append(raw)
        _, limited = git_capture(
            source, ["apply", "--check", "--whitespace=error", str(temporary)], 64 * 1024)
        if limited:
            raise ValueError("patch check output exceeds limit")
        with (CONTROL / "dispatch.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if (CONTROL / "shutoff").read_text().strip() != "0":
                raise ValueError("execution disabled")
            try:
                _, limited = git_capture(
                    source, ["apply", "--whitespace=error", str(temporary)], 64 * 1024)
                if limited:
                    raise ValueError("patch output exceeds limit")
            except Exception:
                return {"status": "uncertain", "output": "Patch outcome requires review",
                        "paths": paths, "patch_sha256": hashlib.sha256(encoded).hexdigest()}
        try:
            after = source_review(base)
        except Exception:
            return {"status": "uncertain", "output": "Patch applied; review unavailable",
                    "paths": paths, "patch_sha256": hashlib.sha256(encoded).hexdigest()}
        return {"status": "success", "output": "patch applied", "paths": paths,
                "patch_sha256": hashlib.sha256(encoded).hexdigest(),
                "diff_sha256": after["diff_sha256"],
                "review_sha256": after["review_sha256"],
                "truncated": after["truncated"]}
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _seed_capture(path, include_git=False):
    digest = hashlib.sha256()
    count = size = 0
    def walk(fd, prefix):
        nonlocal count, size
        for name in sorted(os.listdir(fd)):
            if not prefix and (name == ".code-index" or name == ".git" and not include_git):
                continue
            info = os.stat(name, dir_fd=fd, follow_symlinks=False)
            if stat.S_ISDIR(info.st_mode):
                child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                try:
                    walk(child, prefix + name + "/")
                finally:
                    os.close(child)
            elif stat.S_ISREG(info.st_mode):
                child = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fd)
                try:
                    before = os.fstat(child)
                    if not stat.S_ISREG(before.st_mode) or (before.st_dev, before.st_ino) != (info.st_dev, info.st_ino):
                        raise ValueError("seed input changed")
                    with os.fdopen(child, "rb", closefd=False) as stream:
                        data = stream.read(LIMIT + 1)
                    after = os.fstat(child)
                finally:
                    os.close(child)
                count += 1
                size += len(data)
                if len(data) > LIMIT or count > 10000 or size > 64 * LIMIT:
                    raise ValueError("source template exceeds limit")
                named = os.stat(name, dir_fd=fd, follow_symlinks=False)
                if before.st_mtime_ns != after.st_mtime_ns or before.st_mode != after.st_mode or len(data) != before.st_size or (named.st_dev, named.st_ino) != (before.st_dev, before.st_ino):
                    raise ValueError("seed input changed")
                digest.update((prefix + name).encode()); digest.update(b"\0")
                digest.update(str(before.st_mode & 0o777).encode()); digest.update(b"\0"); digest.update(data)
            else:
                raise ValueError("invalid source template entry")
    if isinstance(path, int):
        walk(path, "")
    else:
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            walk(fd, "")
        finally:
            os.close(fd)
    return {"sha256": digest.hexdigest(), "files": count, "bytes": size}


def _seed_inner(spec):
    # All copy and Git work occurs in an already restricted child. TEMPLATE
    # alone is readable outside the workspace, never writable by this child.
    base, template = Path(spec["base"]), Path(spec["template"])
    stage = base / spec["stage"]
    before = _seed_capture(template, True)
    if before["files"] == 0 or (template / ".git").exists():
        raise ValueError("invalid source template")
    stage.mkdir(mode=0o700)
    shutil.copytree(template, stage, dirs_exist_ok=True, ignore=shutil.ignore_patterns(".code-index"))
    if _seed_capture(template, True) != before or _seed_capture(stage, True) != before:
        raise ValueError("seed copy changed")
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "HOME": str(stage),
           "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}
    options = ["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false"]
    def git(*argv):
        process = subprocess.Popen(["git", *options, *argv], cwd=stage, env=env,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        output = bytearray(); deadline = time.monotonic() + 20
        try:
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not select.select([process.stdout], [], [], remaining)[0]:
                    raise ValueError("seed Git timeout")
                data = os.read(process.stdout.fileno(), 65536)
                if not data:
                    break
                output.extend(data)
                if len(output) > LIMIT:
                    raise ValueError("seed Git output exceeds limit")
            if process.wait(timeout=1):
                raise ValueError("isolated seed Git rejected")
            return output.decode().strip()
        finally:
            if process.poll() is None:
                process.kill(); process.wait()
            process.stdout.close()
    git("init", "--quiet", "--initial-branch=main")
    config = git("config", "--local", "--name-only", "--list").splitlines()
    if any(key not in {"core.repositoryformatversion", "core.filemode", "core.bare", "core.logallrefupdates"} for key in config):
        raise ValueError("unexpected seed Git configuration")
    (stage/".git/info/exclude").write_text(".code-index/\n")
    git("add", "-A")
    git("-c", "user.name=WebAgent v3", "-c", "user.email=webagent-v3@localhost", "commit", "--quiet", "-m", "Imported v3 source snapshot")
    baseline = git("rev-parse", "HEAD")
    if git("status", "--porcelain") or _seed_capture(stage) != before or _seed_capture(template, True) != before:
        raise ValueError("seed baseline changed")
    index = template.parent/".code-index"
    index_digest = None
    ready_index=index.is_dir()and(index/"manifest.json").is_file()and(index/"index.sqlite").is_file()
    if index.is_symlink():raise ValueError("Published index directory symlink")
    if spec.get("published") and not ready_index:
        # A source-only release is usable while its separate index job is pending.
        receipt=template.parent/".code-index/source-release.json"
        if receipt.is_symlink()or not receipt.is_file()or receipt.stat().st_size>4096:raise ValueError("Published source/index pair unavailable")
        published=json.loads(receipt.read_bytes())
        module=administrator_plugin("code_index")
        if published.get("publication_mode")!="post_deployment_indexing_v1"or published.get("release_id")!=template.parent.name or published.get("source_sha256")!=module.source_digest(template.parent):raise ValueError("Published source receipt mismatch")
    if ready_index:
        # Source and bootstrap index use the same already-resolved template.
        module = administrator_plugin("code_index")
        module.seed_private_index(template.parent,stage)
        index_digest = module.read_published(stage)[1]["index_sha256"]
    os.chmod(stage, 0o770)
    info = stage.stat(follow_symlinks=False)
    return {"stage": spec["stage"], "identity": [info.st_dev, info.st_ino],
            "snapshot": _seed_capture(stage, True), "source": before, "baseline_commit": baseline,
            "index_sha256":index_digest}


def _seed_spec(base):
    base = Path(base)
    if base.is_symlink() or not base.is_dir():
        raise ValueError("invalid workspace")
    base = base.resolve(strict=True)
    target = base / "webagent-v3"
    if target.exists() or target.is_symlink():
        raise ValueError("workspace source already initialized")
    if TEMPLATE.is_symlink():
        raise ValueError("source template unavailable")
    try:
        selected = TEMPLATE.resolve(strict=True)
    except OSError as exc:
        raise ValueError("source template unavailable") from exc
    if not selected.is_dir():
        raise ValueError("source template unavailable")
    published = selected.parent.parent.name == "releases"
    if published:
        uuid.UUID(selected.parent.name)
        if not (selected.parent/".code-index").is_dir():
            raise ValueError("Published source/index pair unavailable")
    return {"base": str(base), "template": str(selected), "stage": ".v3-seed-" + uuid.uuid4().hex,
            "published":published}


def _seed_argv(spec):
    worker = str(WORKER_PATH)
    inner = [sys.executable, "-I", worker, "--seed-inner", json.dumps(spec)]
    return [sys.executable, "-I", worker, "--runner", json.dumps(inner)]


def _seed_finish(spec, raw):
    result = json.loads(raw)
    if "error" in result:
        raise ValueError(result["error"])
    if result.get("stage") != spec["stage"]:
        raise ValueError("invalid seed response")
    basefd = os.open(spec["base"], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    stagefd = None
    try:
        stagefd = os.open(spec["stage"], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=basefd)
        info = os.fstat(stagefd)
        if [info.st_dev, info.st_ino] != result["identity"] or _seed_capture(stagefd, True) != result["snapshot"]:
            raise ValueError("seed changed before publication")
        if result.get("index_sha256") is not None:
            actual=administrator_plugin("code_index").read_published(
                Path(spec["base"])/spec["stage"])[1]["index_sha256"]
            if actual != result["index_sha256"]:
                raise ValueError("Seed index changed before publication")
        elif (Path(spec["base"])/spec["stage"]/".code-index").exists():
            raise ValueError("Unexpected seed index")
        with (CONTROL / "dispatch.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if (CONTROL / "shutoff").read_text().strip() != "0":
                raise ValueError("execution disabled")
            named = os.stat(spec["stage"], dir_fd=basefd, follow_symlinks=False)
            if (named.st_dev, named.st_ino) != (info.st_dev, info.st_ino):
                raise ValueError("seed directory changed")
            libc = ctypes.CDLL(None, use_errno=True)
            if libc.renameat2(basefd, spec["stage"].encode(), basefd, b"webagent-v3", 1) != 0:
                raise ValueError("workspace source already initialized or publication unavailable")
            os.fsync(basefd)
        return {"status": "success", "output": "source seeded", "path": "webagent-v3",
                "files": result["source"]["files"], "source_sha256": result["source"]["sha256"],
                "baseline_commit": result["baseline_commit"]}
    finally:
        if stagefd is not None:
            os.close(stagefd)
        os.close(basefd)


def _seed_cleanup(spec):
    stage = Path(spec["base"]) / spec["stage"]
    if stage.is_symlink():
        stage.unlink(missing_ok=True)
    elif stage.exists():
        shutil.rmtree(stage)


def seed_workspace(base):
    """Synchronous trusted caller compatibility; all unsafe work stays child-side."""
    spec = _seed_spec(base)
    process = subprocess.Popen(_seed_argv(spec), cwd=spec["base"],
        env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "V3_SUPERVISOR_PARENT": str(os.getpid())}, stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        raw, _ = process.communicate(timeout=90)
        if process.returncode or len(raw) > LIMIT:
            raise ValueError("isolated seed failed")
        return _seed_finish(spec, raw)
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL); process.wait()
        _seed_cleanup(spec)


async def seed_workspace_async(base):
    spec = _seed_spec(base)
    spawn = asyncio.create_task(asyncio.create_subprocess_exec(*_seed_argv(spec), cwd=spec["base"],
        env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "V3_SUPERVISOR_PARENT": str(os.getpid())}, stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL, start_new_session=True))
    try:
        process = await asyncio.shield(spawn)
    except asyncio.CancelledError:
        process = await asyncio.shield(spawn)
        await terminate(process)
        _seed_cleanup(spec)
        raise
    try:
        raw, _ = await asyncio.wait_for(process.communicate(), 90)
        if process.returncode or len(raw) > LIMIT:
            raise ValueError("isolated seed failed")
        return _seed_finish(spec, raw)
    finally:
        if process.returncode is None:
            await terminate(process)
        _seed_cleanup(spec)


