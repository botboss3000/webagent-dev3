"""Versioned OCI runtime dependencies and a private, persistent v3 BuildKit cache."""
import contextlib
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import tarfile
import threading
import time
import uuid

ROLES = ("api", "loop")
ALIASES = {"api": "webagent_runtime_api", "loop": "webagent_runtime_loop"}
DIGEST = re.compile(r"sha256:([a-f0-9]{64})")
KEY = re.compile(r"[a-f0-9]{64}")


def sha(data):
    """Bind canonical bytes without relying on mtimes or Docker tags."""
    return hashlib.sha256(data).hexdigest()


def atomic(path, value):
    """Commit host-owned cache metadata only after data and directory fsync."""
    path = Path(path)
    if path.is_symlink():
        raise ValueError("Cache metadata symlink")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".new")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(json.dumps(value, sort_keys=True, separators=(",", ":")).encode())
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def stages(dockerfile):
    """Recognize the existing pinned runtime contract; unknown stage graphs require review."""
    matches = list(re.finditer(r"^FROM ([^\n]+) AS ([a-z_]+)\s*$", dockerfile, re.M))
    result = {}
    for i, match in enumerate(matches):
        name = match[2]
        if name in result:
            raise ValueError("Duplicate Docker stage")
        result[name] = {"image": match[1], "header": match[0].strip(),
                        "body": dockerfile[match.end():matches[i + 1].start() if i + 1 < len(matches) else len(dockerfile)]}
    if set(result) != {"preview_tunnel", "tested_builder", "api", "loop"}:
        raise ValueError("Unsupported runtime dependency stage graph")
    return result


def runtime_recipe(dockerfile, role, helper, generation=1, workers=4):
    """Extract only OS/toolchain dependencies into a versioned, independently prepared image."""
    if role not in ROLES or type(generation) is not int or not 1 <= generation <= 1000000:
        raise ValueError("Fixed runtime role and generation required")
    if type(workers) is not int or not 1 <= workers <= 4:
        raise ValueError("Bounded download workers required")
    graph = stages(dockerfile)
    selected = graph[role]
    if not re.fullmatch(r"debian:bookworm-slim@sha256:[a-f0-9]{64}", selected["image"]):
        raise ValueError("Pinned Debian bookworm runtime required")
    copy = re.search(r"^COPY ", selected["body"], re.M)
    if copy is None:
        raise ValueError("Runtime COPY boundary missing")
    setup = " ".join(selected["body"][:copy.start()].replace("\\\n", " ").split())
    match = re.fullmatch(r"RUN apt-get update && apt-get install -y --no-install-recommends (.*?) && rm -rf /var/lib/apt/lists/\* (&& groupadd .+)", setup)
    if not match:
        raise ValueError("Unsupported runtime package installation boundary")
    packages = shlex.split(match[1])
    if not packages or any(not re.fullmatch(r"[a-z0-9][a-z0-9+.-]*", p) for p in packages):
        raise ValueError("Unrecognized runtime package list")
    if not re.fullmatch(r"[A-Za-z0-9 /&_.-]+", match[2]):
        raise ValueError("Unrecognized runtime account setup")
    dependency_copies = []
    for line in selected["body"][copy.start():].splitlines():
        if line.startswith("COPY --from=preview_tunnel "):
            dependency_copies.append(line)
        elif line.startswith("COPY --from=tested_builder /usr/local/rustup/"):
            dependency_copies.append(line.replace("--from=tested_builder", "--from=runtime_toolchain"))
    prefix = ""
    if role == "api":
        if not {"python3", "curl", "ca-certificates"} <= set(packages):
            raise ValueError("API prefetch bootstrap is not declared")
        prefix = graph["preview_tunnel"]["header"] + "\n" + graph["tested_builder"]["header"].replace(" AS tested_builder", " AS runtime_toolchain") + "\n"
        if not re.fullmatch(r"rust:1\.99-bookworm@sha256:[a-f0-9]{64}", graph["tested_builder"]["image"]):
            raise ValueError("Pinned compiler/runtime toolchain required")
    name = ALIASES[role]
    archive_id = "webagent-v3-bookworm-" + selected["image"].split("sha256:")[1][:12]
    mounts = ("--mount=type=cache,id=" + archive_id + "-archives,target=/var/cache/apt,sharing=locked "
              "--mount=type=cache,id=" + archive_id + "-lists,target=/var/lib/apt/lists,sharing=locked ")
    if role == "api":
        mounts += "--mount=type=bind,source=scheduler-seed,target=/scheduler-seed,readonly "
    setup_commands = (
        "rm -f /etc/apt/apt.conf.d/docker-clean && "
        "printf 'APT::Keep-Downloaded-Packages \"true\";\\nBinary::apt::APT::Keep-Downloaded-Packages \"true\";\\n' > /etc/apt/apt.conf.d/keep-cache && "
        "printf 'Acquire::Retries \"2\";\\nAcquire::http::Timeout \"15\";\\nAcquire::https::Timeout \"15\";\\n' > /etc/apt/apt.conf.d/webagent-network && "
        "apt-get update && ")
    if role == "api":
        setup_commands += (
            "apt-get install -y --no-install-recommends python3 ca-certificates curl && "
            "python3 /scheduler-cache/apt_prefetch.py --workers " + str(workers) + " " + " ".join(packages) + " && "
            "apt-get install -y --no-download --no-install-recommends " + " ".join(packages) + " ")
    else:
        setup_commands += "apt-get install -y --no-install-recommends " + " ".join(packages) + " "
    setup_commands += match[2] + " && mkdir -p /usr/local/share/webagent && printf '%s\\n' \"$WEBAGENT_RUNTIME_BASE_GENERATION\" > /usr/local/share/webagent/runtime-base-generation"
    rendered = prefix + "FROM " + selected["image"] + " AS " + name + "\nARG WEBAGENT_RUNTIME_BASE_GENERATION=1\n"
    if role == "api":
        rendered += "COPY scheduler-cache/apt_prefetch.py /scheduler-cache/apt_prefetch.py\n"
    rendered += "RUN " + mounts + setup_commands + "\n"
    rendered += "\n".join(dependency_copies) + ("\n" if dependency_copies else "")
    identity = {"protocol": 1, "role": role, "generation": generation, "workers": workers,
                "stage": rendered, "helper_sha256": sha(helper) if role == "api" else None}
    key = sha(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode())
    rendered += 'LABEL webagent.runtime-base.role="' + role + '" webagent.runtime-base.recipe="' + key + '"\n'
    return {"key": key, "role": role, "generation": generation, "stage": name,
            "dockerfile": rendered, "packages": packages, "source_prefix_sha256": sha(setup.encode()),
            "helper_sha256": identity["helper_sha256"]}


def package_dockerfile(dockerfile, role):
    """Package only from pinned local OCI bases; no APT or registry base remains in this graph."""
    if role not in ROLES:
        raise ValueError("Fixed packaging role required")
    graph = stages(dockerfile)
    output = "FROM scratch AS tested_builder\nCOPY scheduler-out /out\n"
    for current in ROLES:
        body = graph[current]["body"]
        start = re.search(r"^COPY ", body, re.M)
        if start is None:
            raise ValueError("Runtime source-copy boundary missing")
        lines = []
        for line in body[start.start():].splitlines(keepends=True):
            if line.startswith("COPY --from=preview_tunnel ") or line.startswith("COPY --from=tested_builder /usr/local/rustup/"):
                continue
            lines.append(line)
        output += "FROM " + ALIASES[current] + " AS " + current + "\n" + "".join(lines).rstrip() + "\n\n"
    return output


def read_json(path, limit=2 * 1024**2):
    """Bound metadata and reject links before parsing untrusted image descriptors."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise ValueError("OCI metadata exceeds bounds")
        return json.loads(stream.read(limit + 1))


def verify_layout(folder, role, recipe_key, sync=False):
    """Verify every referenced OCI blob, platform and recipe label before exposing a base."""
    folder = Path(folder)
    if folder.is_symlink() or (folder / "blobs").is_symlink() or (folder / "blobs/sha256").is_symlink():
        raise ValueError("OCI directory symlink")
    if read_json(folder / "oci-layout").get("imageLayoutVersion") != "1.0.0":
        raise ValueError("Unsupported OCI layout")
    index = read_json(folder / "index.json")
    if index.get("schemaVersion") != 2 or len(index.get("manifests", [])) != 1:
        raise ValueError("Single-platform OCI base required")
    seen = set()
    total = 0
    def blob(descriptor, metadata=False):
        nonlocal total
        match = DIGEST.fullmatch(descriptor.get("digest", ""))
        if not match or type(descriptor.get("size")) is not int or not 0 <= descriptor["size"] <= 2 * 1024**3:
            raise ValueError("OCI descriptor invalid")
        if match[1] in seen:
            raise ValueError("Repeated OCI descriptor")
        seen.add(match[1])
        if len(seen) > 128:
            raise ValueError("OCI graph exceeds bounds")
        path = folder / "blobs/sha256" / match[1]
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size != descriptor["size"]:
                raise ValueError("OCI blob size changed")
            digest = hashlib.sha256()
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
            if digest.hexdigest() != match[1]:
                raise ValueError("OCI blob checksum changed")
            if sync:
                os.fsync(stream.fileno())
        total += descriptor["size"]
        if total > 4 * 1024**3:
            raise ValueError("OCI base exceeds storage bound")
        return read_json(path) if metadata else None
    root = index["manifests"][0]
    manifest = blob(root, True)
    if manifest.get("schemaVersion") != 2 or not isinstance(manifest.get("layers"), list):
        raise ValueError("OCI image manifest required")
    config = blob(manifest["config"], True)
    if config.get("architecture") != "amd64" or config.get("os") != "linux":
        raise ValueError("Expected Linux amd64 runtime base")
    labels = config.get("config", {}).get("Labels", {})
    if labels.get("webagent.runtime-base.role") != role or labels.get("webagent.runtime-base.recipe") != recipe_key:
        raise ValueError("OCI base recipe changed")
    for entry in manifest["layers"]:
        blob(entry)
    diff_ids = config.get("rootfs", {}).get("diff_ids", [])
    if len(diff_ids) != len(manifest["layers"]) or any(not DIGEST.fullmatch(d) for d in diff_ids):
        raise ValueError("OCI root filesystem binding incomplete")
    return {"manifest_digest": root["digest"], "config_digest": manifest["config"]["digest"],
            "diff_ids": diff_ids, "bytes": total}


# @code-index {"version":1,"feature":"deployment-dependency-cache","aliases":["persistent BuildKit","versioned runtime bases"],"purpose":"Prepare checksum-bound OCI dependency images separately and retain disk-backed BuildKit/APT state. Config: build-service/scheduler-defaults.json -> package_cache_enabled, package_cache_download_workers, package_cache_max_age_seconds, package_cache_retention_seconds, package_cache_max_bytes.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/package_cache_enabled","/package_cache_download_workers","/package_cache_max_age_seconds","/package_cache_retention_seconds","/package_cache_max_bytes"]}]}}
class PackagingCache:
    """Host-only cache authority; callers supply no paths, images, mirrors or build commands."""
    def __init__(self, broker, config, entries, validate):
        self.broker = broker
        self.runner = broker.runner
        self.config = config
        self.entries = entries
        self.validate = validate
        self.folder = broker.state / "dependency-cache"
        if self.folder.is_symlink():
            raise ValueError("Dependency cache directory symlink")
        self.folder.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.manifest = self.folder / "state.json"
        self.guard = threading.RLock()
        self.preparation_guard = threading.Lock()
        self.volume = "webagent-development-buildkit-79cc6476ab1a-v1"

    @property
    def enabled(self):
        """Protected policy alone activates cache use by application packaging."""
        return self.config.get("scheduler", {}).get("package_cache_enabled", False)

    def state(self):
        """A missing first-use manifest is empty; corrupt persistence never authorizes reuse."""
        if not self.manifest.exists():
            return {"version": 1, "generation": 1, "bases": {}}
        result = read_json(self.manifest)
        if result.get("version") != 1 or type(result.get("generation")) is not int or not isinstance(result.get("bases"), dict):
            raise ValueError("Dependency cache state invalid")
        return result

    def helper(self):
        """Use reviewed installed helper bytes, never a candidate-provided download program."""
        path = Path(self.config["service_dir"]) / "apt_prefetch.py"
        if path.is_symlink():
            raise ValueError("Protected APT helper symlink")
        return path.read_bytes()

    def recipes(self, blob, generation):
        """UI/source edits outside runtime dependency prefixes cannot invalidate a base."""
        files = self.entries(blob, self.validate)
        dockerfile = files["v3/Dockerfile"][1].decode()
        helper = self.helper()
        workers = self.config.get("scheduler", {}).get("package_cache_download_workers", 4)
        return {role: runtime_recipe(dockerfile, role, helper, generation, workers) for role in ROLES}

    def check(self, row, recipe, sync=False):
        """Rehash immutable OCI inputs and reject expired or mismatched preparations."""
        if row.get("key") != recipe["key"] or row.get("role") != recipe["role"]:
            raise ValueError("Runtime cache recipe changed")
        if time.time() - row["created_at"] > self.config.get("scheduler", {}).get("package_cache_max_age_seconds", 604800):
            raise ValueError("Runtime dependency refresh required")
        root = self.folder / "bases" / recipe["key"]
        actual = verify_layout(root, recipe["role"], recipe["key"], sync)
        if any(actual[k] != row[k] for k in ("manifest_digest", "config_digest", "diff_ids", "bytes")):
            raise ValueError("Runtime base receipt changed")
        return row

    def ready(self, blob):
        """Return exact local image inputs only when every required runtime base is verified."""
        with self.guard:
            state = self.state()
            desired = self.recipes(blob, state["generation"])
            return {role: self.check(state["bases"][recipe["key"]], recipe)
                    for role, recipe in desired.items()}

    @contextlib.contextmanager
    def claim(self, job):
        """One daemon owns this disk database; no publication or cleanup lock is held here."""
        active = self.enabled or getattr(self.runner.job_context, "preparing_runtime_base", False)
        if not active:
            yield None
            return
        fd = os.open(self.folder / "buildkit.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        acquired = False
        try:
            deadline = time.monotonic() + 600
            while not acquired:
                self.broker._lease(job)
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    acquired = True
                except BlockingIOError:
                    event = getattr(self.runner.job_context, "event", None)
                    if time.monotonic() >= deadline or event is not None and event.is_set():
                        raise ValueError("Persistent package cache busy")
                    job["resource_waiting"] = {"reason": "package_cache_busy", "since": time.time()}
                    self.broker._resource_progress(job)
                    time.sleep(.25)
            if job.get("resource_waiting", {}).get("reason") == "package_cache_busy":
                job.pop("resource_waiting", None)
                self.broker._resource_progress(job)
            busy = self.runner.command(["docker", "ps", "--filter", "volume=" + self.volume, "--format", "{{.ID}}"], timeout=10).strip()
            if busy:
                raise ValueError("Existing cache daemon requires exact resource reconciliation")
            self.runner.command(["docker", "volume", "create", "--label", "webagent.cache.kind=packaging-v1",
                                 "--label", "webagent.cache.instance=development", self.volume], timeout=10)
            values = json.loads(self.runner.command(["docker", "volume", "inspect", self.volume], timeout=10, private_raw=True))
            labels = values[0].get("Labels", {})
            if labels.get("webagent.cache.kind") != "packaging-v1" or labels.get("webagent.cache.instance") != "development":
                raise ValueError("Existing volume ownership unknown; preserved")
            yield self.volume
        finally:
            os.close(fd)

    def seed_files(self):
        """Only checksummed, explicitly recorded Debian archives can seed a new base."""
        result = {}
        total = 0
        folder = self.folder / "archives"
        if not folder.exists():
            return result
        if folder.is_symlink():
            raise ValueError("Archive seed directory symlink")
        for metadata in sorted(folder.glob("*.deb.json"))[:1024]:
            row = read_json(metadata, 16384)
            name = metadata.name[:-5]
            if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9+.:~_-]*\.deb", name):
                raise ValueError("Unconfined archive seed")
            path = folder / name
            if path.is_symlink() or not path.is_file() or type(row.get("bytes")) is not int or not 0 < row["bytes"] <= 512 * 1024**2:
                raise ValueError("Archive seed metadata invalid")
            if path.stat().st_size != row["bytes"] or sha(path.read_bytes()) != row.get("sha256"):
                raise ValueError("Archive seed checksum changed")
            total += row["bytes"]
            if total > 2 * 1024**3:
                raise ValueError("Archive seeds exceed cache bounds")
            result["scheduler-seed/" + name] = path
        return result

    def context(self, destination, recipe):
        """Stage only the dependency recipe, protected helper and verified optional seeds."""
        files = {"Dockerfile": recipe["dockerfile"].encode(), "scheduler-seed/.empty": b""}
        if recipe["role"] == "api":
            files["scheduler-cache/apt_prefetch.py"] = self.helper()
            files.update(self.seed_files())
        with tarfile.open(destination, "w") as archive:
            for name, body in sorted(files.items()):
                member = tarfile.TarInfo(name)
                member.mode = 0o644
                if isinstance(body, Path):
                    member.size = body.stat().st_size
                    with body.open("rb") as stream:
                        archive.addfile(member, stream)
                else:
                    member.size = len(body)
                    archive.addfile(member, io.BytesIO(body))

    def prewarm(self, job, blob, refresh=False):
        """Serialize refresh generations while ordinary cache reads remain available."""
        with self.preparation_guard:
            return self._prewarm(job,blob,refresh)

    def _prewarm(self, job, blob, refresh=False):
        """Prepare complete versioned OCI artifacts before application packaging is admitted."""
        if self.config["instance"] != "development":
            raise PermissionError("V3 development cache only")
        with self.guard:
            state = self.state()
            if refresh:
                state["generation"] += 1
                atomic(self.manifest, state)
            desired = self.recipes(blob, state["generation"])
        output = self.folder / "preparations" / job["job_id"]
        output.mkdir(mode=0o700, parents=True, exist_ok=True)
        result = {}
        for role, recipe in desired.items():
            with self.guard:
                state = self.state()
                row = state["bases"].get(recipe["key"])
            if row:
                result[role] = self.check(row, recipe)
                continue
            final = self.folder / "bases" / recipe["key"]
            if final.exists():
                raise ValueError("Unrecorded base layout requires explicit cache reconciliation")
            temporary = final.with_name("." + recipe["key"] + "-" + job["job_id"] + ".preparing")
            if temporary.exists():
                raise ValueError("Interrupted base preparation preserved; use a new refresh request")
            temporary.parent.mkdir(parents=True, exist_ok=True)
            context = output / (role + ".tar")
            if context.exists():
                raise ValueError("Existing cache preparation context requires reconciliation")
            self.context(context, recipe)
            self.runner.job_context.preparing_runtime_base = True
            try:
                self.broker._phase(job, "preparing_runtime_base_" + role)
                with self.runner.executor.packager(job, role) as builder:
                    with context.open("rb") as stream:
                        log = self.runner.command(["docker", "buildx", "build", "--builder", builder,
                            "--platform", "linux/amd64", "--provenance=false", "--progress=plain",
                            "--build-arg", "WEBAGENT_RUNTIME_BASE_GENERATION=" + str(recipe["generation"]),
                            "--target", recipe["stage"], "--output", "type=oci,tar=false,dest=" + str(temporary),
                            "-f", "Dockerfile", "-"], stdin=stream, timeout=900)
                    job["log"] = job.get("log", "") + log
                    self.broker._resource_progress(job)
            finally:
                self.runner.job_context.preparing_runtime_base = False
                context.unlink(missing_ok=True)
            actual = verify_layout(temporary, role, recipe["key"], sync=True)
            for folder in (temporary / "blobs/sha256", temporary / "blobs", temporary):
                fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
            os.rename(temporary, final)
            row = {**actual, "key": recipe["key"], "role": role, "generation": recipe["generation"],
                   "created_at": time.time(), "job_id": job["job_id"],
                   "source_prefix_sha256": recipe["source_prefix_sha256"], "helper_sha256": recipe["helper_sha256"]}
            with self.guard:
                state = self.state()
                state["bases"][recipe["key"]] = row
                atomic(self.manifest, state)
            result[role] = row
            job["dependency_bases"] = result.copy()
            self.broker._resource_progress(job)
        return result

    def package_inputs(self, job, blob):
        """Pin complete OCI inputs to this attempt; no download occurs in application packaging."""
        rows = self.ready(blob)
        job["dependency_bases"] = rows
        self.broker._resource_progress(job)
        arguments = []
        for role, row in rows.items():
            path = self.folder / "bases" / row["key"]
            arguments += ["--build-context", ALIASES[role] + "=oci-layout://" + str(path) + "@" + row["manifest_digest"]]
        return arguments

    def verify_consumption(self, job, blob):
        """Exercise both pinned OCI bases through the real dedicated BuildKit path without network."""
        self.broker._phase(job, "verifying_dependency_cache")
        names = self.package_inputs(job, blob)
        dockerfile = (
            "FROM webagent_runtime_api AS checked_api\\n"
            "RUN test -x /usr/bin/chromium && test -x /usr/bin/python3\\n"
            "FROM webagent_runtime_loop AS checked_loop\\n"
            "RUN test -x /usr/bin/tini\\n"
            "FROM scratch AS verified\\n"
            "COPY --from=checked_api /etc/debian_version /api-version\\n"
            "COPY --from=checked_loop /etc/debian_version /loop-version\\n"
        ).replace("\\n", "\n")
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            body = dockerfile.encode()
            item = tarfile.TarInfo("Dockerfile")
            item.mode = 0o644
            item.size = len(body)
            archive.addfile(item, io.BytesIO(body))
        stream.seek(0)
        self.runner.job_context.preparing_runtime_base = True
        try:
            with self.runner.executor.packager(job, "api") as builder:
                log = self.runner.command(
                    ["docker", "buildx", "build", "--builder", builder,
                     "--platform", "linux/amd64", "--network", "none",
                     "--provenance=false", "--progress=plain", "--target", "verified",
                     "--output", "type=cacheonly", *names, "-f", "Dockerfile", "-"],
                    stdin=stream, timeout=300)
        finally:
            self.runner.job_context.preparing_runtime_base = False
        job["dependency_base_consumption"] = {"status": "verified",
            "recipes": {role: row["key"] for role, row in self.ready(blob).items()}}
        self.broker._resource_progress(job)
        return log

    def status(self):
        """Expose bounded readiness facts while preserving the cache's internal file authority."""
        with self.guard:
            state = self.state()
            return {"enabled": self.enabled, "generation": state["generation"], "volume": self.volume,
                    "base_count": len(state["bases"]),
                    "bases": [{k: row[k] for k in ("key", "role", "created_at", "manifest_digest", "bytes")}
                              for row in list(state["bases"].values())[-32:]],
                    "retention_seconds": self.config.get("scheduler", {}).get("package_cache_retention_seconds", 604800),
                    "max_bytes": self.config.get("scheduler", {}).get("package_cache_max_bytes", 8 * 1024**3)}
