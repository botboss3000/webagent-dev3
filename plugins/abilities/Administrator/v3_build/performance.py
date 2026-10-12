"""Protected performance admission and durable, non-replaying experiments."""
import base64
import concurrent.futures
import hashlib
import io
import tarfile
import json
import math
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import threading
import time
import uuid

DEFAULTS = dict(priority="balanced", attempts=3, total_seconds=900,
                profiling="both", cpu_cores=1, ram_mb=1024, coverage="standard",
                max_slowdown_percent=0, max_memory_regression_percent=0, allow_dependencies=False)
FILES = ("plugin.json", "optimization.json", "optimization.skill.md",
         "webagent-optimization", "measure.py", "workload.py")
ACTIONS = {"benchmark", "profile_cpu", "profile_allocations", "status", "cancel", "artifact"}


def settings(value):
    if not isinstance(value, dict) or set(value) - set(DEFAULTS):
        raise ValueError("unknown optimization setting")
    result = {**DEFAULTS, **value}
    for key, lo, hi in (("attempts", 1, 10), ("total_seconds", 1, 1800),
                        ("cpu_cores", 1, 2), ("ram_mb", 128, 4096),
                        ("max_slowdown_percent", 0, 25), ("max_memory_regression_percent", 0, 25)):
        if type(result[key]) is not int or not lo <= result[key] <= hi:
            raise ValueError("invalid optimization setting: " + key)
    for key, allowed in (("priority", {"balanced", "speed", "memory"}),
                         ("profiling", {"both", "cpu", "allocations", "none"}),
                         ("coverage", {"quick", "standard", "extended"})):
        if result[key] not in allowed:
            raise ValueError("invalid optimization setting: " + key)
    if type(result["allow_dependencies"]) is not bool:
        raise ValueError("invalid optimization dependency scope")
    return result


_PACKAGE_CACHE = None
_PACKAGE_LOCK = threading.RLock()


def _metadata(info):
    return (info.st_dev, info.st_ino, info.st_uid, info.st_gid, info.st_mode,
            info.st_size, info.st_mtime_ns, info.st_ctime_ns, info.st_nlink)


def _validated_package(source):
    """One bounded immutable cache, freshly fenced by pinned metadata every use."""
    global _PACKAGE_CACHE
    package = Path(source) / "plugins/optimization"
    cache_path = str(package.absolute())
    with _PACKAGE_LOCK:
        directory = None
        descriptors = []
        try:
            directory = os.open(package, os.O_RDONLY | os.O_DIRECTORY |
                                os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
            owner = os.fstat(directory)
            if not stat.S_ISDIR(owner.st_mode) or owner.st_mode & 0o022:
                raise PermissionError("Optimization package unavailable")
            before = []
            for name in FILES:
                fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK |
                             os.O_CLOEXEC, dir_fd=directory)
                descriptors.append((name, fd))
                info = os.fstat(fd)
                limit = 32 * 1024 * 1024 if name == "webagent-optimization" else 65536
                if not stat.S_ISREG(info.st_mode) or info.st_uid != owner.st_uid or info.st_mode & 0o022 or info.st_size > limit:
                    raise PermissionError("Optimization package invalid")
                if name == "webagent-optimization" and not info.st_mode & 0o111:
                    raise PermissionError("Optimization executable invalid")
                before.append(_metadata(info))
            signature = (_metadata(owner), tuple(before))
            cached = _PACKAGE_CACHE
            if cached is not None and cached["path"] == cache_path and cached["signature"] == signature:
                digest, drivers = cached["generation"], cached["drivers"]
            else:
                digest = hashlib.sha256()
                drivers = {}
                for (name, fd), info in zip(descriptors, before):
                    # Read only on a changed metadata signature, then retain
                    # bounded driver bytes, never a copy of the executable.
                    limit = 32 * 1024 * 1024 if name == "webagent-optimization" else 65536
                    body = bytearray()
                    while len(body) <= limit:
                        block = os.read(fd, min(65536, limit+1-len(body)))
                        if not block:
                            break
                        body.extend(block)
                    if len(body) != info[5] or len(body) > limit:
                        raise PermissionError("Optimization asset changed")
                    raw = name.encode()
                    digest.update(len(raw).to_bytes(8, "big")); digest.update(raw)
                    digest.update(len(body).to_bytes(8, "big")); digest.update(body)
                    if name in {"measure.py", "workload.py"}:
                        drivers[name] = bytes(body)
                digest = digest.hexdigest()
            # Both open descriptors and their current directory-relative names
            # must still designate the metadata used to select cached bytes.
            for (name, fd), info in zip(descriptors, before):
                if (_metadata(os.fstat(fd)) != info
                        or _metadata(os.stat(name, dir_fd=directory, follow_symlinks=False)) != info):
                    raise PermissionError("Optimization asset changed during validation")
            if (_metadata(os.fstat(directory)) != signature[0]
                    or _metadata(os.stat(package, follow_symlinks=False)) != signature[0]):
                raise PermissionError("Optimization package changed during validation")
            _PACKAGE_CACHE = dict(path=cache_path, signature=signature,
                                  generation=digest, drivers=drivers)
            return digest, dict(drivers)
        except Exception:
            _PACKAGE_CACHE = None
            raise
        finally:
            for _, fd in descriptors:
                os.close(fd)
            if directory is not None:
                os.close(directory)


def generation(source):
    # Readiness is fenced by native admission and its renewable fingerprint.
    # The host independently revalidates every package/asset identity each call.
    return _validated_package(source)[0]


class Performance:
    def __init__(self, broker):
        self.broker = broker
        self.root = broker.state / "performance"
        self.root.mkdir(mode=0o700, exist_ok=True)
        if self.root.is_symlink():
            raise ValueError("symlink performance state forbidden")
        self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=1)
        self.rows = {}
        self.events = {}
        for path in self.root.glob("*.json"):
            if path.is_symlink() or path.stat().st_size > 256 * 1024:
                raise ValueError("invalid performance receipt")
            row = json.loads(path.read_text())
            if not re.fullmatch(r"[a-f0-9]{64}", row.get("id", "")):
                raise ValueError("invalid performance identity")
            self.rows[row["id"]] = row
            if row["status"] in {"queued", "running"}:
                self.kill(row)
                row.update(status="interrupted", reason="broker restarted; no automatic replay")
                self.save(row)

    def save(self, row):
        with self.broker.lock:
            target = self.root / (row["id"] + ".json")
            temporary = self.root / ("." + row["id"] + "-" + uuid.uuid4().hex + ".tmp")
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            try:
                with os.fdopen(fd, "w") as stream:
                    json.dump(row, stream, sort_keys=True)
                    stream.flush(); os.fsync(stream.fileno())
                os.replace(temporary, target)
                fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
                try: os.fsync(fd)
                finally: os.close(fd)
            finally:
                temporary.unlink(missing_ok=True)

    def view(self, row):
        return {k: v for k, v in row.items() if k not in {
            "user_id", "workspace", "lease_until", "access_fingerprint", "plugin_generation", "container", "cache_key"}}

    def valid(self, row):
        if self.broker.stopping:
            raise PermissionError("broker stopping")
        self.broker._lease(row)
        try:
            current = generation(self.broker.runner.source)
        except (OSError, ValueError, TypeError) as exc:
            raise PermissionError("Optimization package unavailable") from exc
        if current != row["plugin_generation"]:
            raise PermissionError("Optimization package generation revoked")

    def kill(self, row):
        # Cleanup must still execute when Runner.cancel is set on shutdown.
        # Its fixed container namespace prevents a corrupted receipt naming
        # any API, loop, production or unrelated Docker container.
        name = row.get("container", "")
        if not re.fullmatch(r"webagent-v3-performance-[a-f0-9]{24}", name):
            return
        try:
            subprocess.run(["docker", "rm", "-f", name], stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           env={"PATH": "/usr/bin:/bin", "HOME": "/tmp", "LANG": "C.UTF-8"},
                           timeout=15, check=False)
        except (OSError, subprocess.TimeoutExpired):
            pass

    def leases(self, trusted, workspace, incoming):
        count = 0
        for row in self.rows.values():
            if not self.broker._owned(row, trusted, workspace) or row["status"] not in {"queued", "running"}:
                continue
            if incoming and row["access_fingerprint"] == incoming[1] and row["lease_until"] > time.time() and not row.get("revoked"):
                row["lease_until"] = incoming[0]
            else:
                row["revoked"] = True
                self.events.setdefault(row["id"], threading.Event()).set()
                self.kill(row)
            self.save(row); count += 1
        return count

    def handle(self, trusted, workspace, args):
        if set(args) - {"task_id", "expected_inputs_sha256", "request_id", "action", "settings", "plugin_generation", "role", "artifact", "offset", "limit"}:
            raise ValueError("invalid performance arguments")
        task, expected, request, action = (args.get(k) for k in
            ("task_id", "expected_inputs_sha256", "request_id", "action"))
        if not isinstance(task, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task):
            raise ValueError("stable isolated task required")
        if not isinstance(request, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", request):
            raise ValueError("stable performance request required")
        if not isinstance(expected, str) or not re.fullmatch(r"[a-f0-9]{64}", expected) or action not in ACTIONS:
            raise ValueError("bounded performance action required")
        lease, fingerprint = self.broker._incoming_lease(trusted)
        gen = args.get("plugin_generation")
        if generation(self.broker.runner.source) != gen:
            raise PermissionError("Optimization package generation unavailable")
        key = hashlib.sha256(self.broker.performance_canonical([trusted["user_id"], workspace, task])).hexdigest()
        identity = hashlib.sha256(self.broker.performance_canonical([key, request])).hexdigest()
        row = self.rows.get(identity)
        if action in {"status", "cancel", "artifact"}:
            if not row or not self.broker._owned(row, trusted, workspace):
                raise ValueError("owned performance receipt required")
            if row["plugin_generation"] != gen:
                raise PermissionError("performance receipt generation changed")
            terminal_read = action in {"status", "artifact"} and row["status"] in {
                "completed", "failed", "cancelled", "interrupted"}
            if row["access_fingerprint"] != fingerprint and not terminal_read:
                raise PermissionError("performance receipt authority changed")
            # A terminal receipt is evidence, not a renewable execution grant.
            # Fresh native admission may read caller-owned evidence without
            # changing the historical lease, fingerprint or cancellation state.
            if action == "artifact":
                return self.artifact(row, args)
            if action == "cancel" and row["status"] in {"queued", "running"}:
                row["revoked"] = True
                self.events.setdefault(identity, threading.Event()).set()
                self.kill(row); self.save(row)
            return self.view(row)
        config = settings(args.get("settings", {}))
        role = args.get("role", "candidate")
        if role not in {"baseline", "candidate"}:
            raise ValueError("performance role must be baseline or candidate")

        if row:
            if row["action"] != action or row["inputs_sha256"] != expected or row["settings"] != config:
                raise ValueError("performance request identity reused with changed inputs")
            if row["plugin_generation"] != gen or row["access_fingerprint"] != fingerprint:
                raise PermissionError("performance receipt generation or authority changed")
            return self.view(row)
        item = self.broker.work.get(key)
        if not item or item.get("mode") != "isolated" or item.get("status") != "open":
            raise ValueError("open isolated performance task required")
        source_root = self.broker.work_dir / "trees" / key
        blob, actual = self.broker.performance_snapshot(source_root)
        if actual != expected:
            raise ValueError("stale performance task inputs")
        self.scope(source_root, blob, config)

        previous = [r for r in self.rows.values() if r["task_key"] == key]
        if len(self.rows) >= 10000:
            raise ValueError("performance receipt retention bound reached")
        if previous and any(r["settings"] != config for r in previous):
            raise ValueError("optimization settings are pinned for the experiment")
        if previous and not config["allow_dependencies"]:
            baseline_root = self.root / previous[0]["id"] / "source/v3"
            for name in ("Cargo.toml", "Cargo.lock"):
                baseline = (baseline_root / name).read_bytes()
                current = (source_root / "v3" / name).read_bytes()
                if current != baseline:
                    raise PermissionError("dependency changes excluded by effective optimization settings")
        originals = [r for r in previous if r["action"] == "benchmark"
                     and r.get("role") == "baseline" and r.get("accepted") is True]
        if role == "candidate" and action == "benchmark" and not originals:
            raise ValueError("successful original baseline required before candidate")
        if action in {"profile_cpu", "profile_allocations"} and not any(
                r["action"] == "benchmark" and r["inputs_sha256"] == expected
                and r.get("accepted") is True for r in previous):
            raise ValueError("successful measured benchmark required before profiling")
        candidates = {r["inputs_sha256"] for r in previous if r["action"] == "benchmark"}
        if action == "benchmark" and not candidates and role != "baseline":
            raise ValueError("original baseline required before a candidate")
        baseline_rows = [r for r in previous if r["action"] == "benchmark" and r.get("role") == "baseline"]
        original_hash = min(baseline_rows, key=lambda r: r["created_at"])["inputs_sha256"] if baseline_rows else None
        if action == "benchmark" and role == "baseline" and original_hash is not None and expected != original_hash:
            raise ValueError("original baseline cannot be replaced")
        candidate = action == "benchmark" and role == "candidate"

        if candidate and sum(r.get("candidate") is True for r in previous) >= config["attempts"]:
            raise ValueError("optimization candidate attempt budget exhausted")
        if action == "profile_cpu" and config["profiling"] not in {"both", "cpu"} or action == "profile_allocations" and config["profiling"] not in {"both", "allocations"}:
            raise ValueError("profiling disabled by effective settings")
        spent = sum(r.get("charged_seconds", r["reserved_seconds"]) for r in previous)
        remaining = config["total_seconds"] - spent
        if remaining <= 0:
            raise ValueError("optimization cumulative runtime budget exhausted")
        image = self.broker.runner.command(["docker", "inspect", "--format", "{{.Image}}", "webagent-dev-v3"], timeout=15).strip()
        if not re.fullmatch(r"sha256:[a-f0-9]{64}", image):
            raise ValueError("pinned performance image unavailable")
        directory = self.root / identity
        directory.mkdir(mode=0o700)
        self.broker.performance_extract(blob, directory / "source")
        # Driver assets belong to the active executable module, independently
        # of the application source being optimized. Compatible folder updates
        # do not require promoting or rebuilding the canonical source assets.
        self.freeze_driver(directory / "driver", gen)
        artifacts = directory / "artifacts"; artifacts.mkdir(mode=0o700)
        cache_key = hashlib.sha256(self.broker.performance_canonical([key, image, gen])).hexdigest()
        cache = self.root / "caches" / cache_key
        if shutil.disk_usage(self.root).free < 2 * 1024 * 1024 * 1024:
            raise ValueError("insufficient disk space for bounded performance cache")
        for name in ("target", "cargo"):
            (cache / name).mkdir(parents=True, exist_ok=True, mode=0o700)

        row = dict(id=identity, task_key=key, task_id=task, request_id=request, action=action,
                   inputs_sha256=expected, settings=config, status="queued", created_at=time.time(),
                   reserved_seconds=min(600, remaining), candidate=candidate, role=role, image=image,
                   user_id=trusted["user_id"], workspace=workspace, lease_until=lease,
                   access_fingerprint=fingerprint, plugin_generation=gen, harness_sha256=gen,
                   cache_key=cache_key, container="webagent-v3-performance-" + identity[:24])
        self.rows[identity] = row; self.events[identity] = threading.Event(); self.save(row)
        self.pool.submit(self.run, row)
        return self.view(row)

    def freeze_driver(self, destination, expected):
        digest, drivers = _validated_package(self.broker.runner.source)
        if digest != expected:
            raise PermissionError("Optimization package changed before driver snapshot")
        destination.mkdir(mode=0o700)
        for name, body in drivers.items():
            fd = os.open(destination / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o400)
            with os.fdopen(fd, "wb") as stream:
                stream.write(body); stream.flush(); os.fsync(stream.fileno())
        if generation(self.broker.runner.source) != expected:
            raise PermissionError("Optimization package changed after driver snapshot")

    def artifact(self, row, args):
        name = args.get("artifact", "result.json")
        offset, limit = args.get("offset", 0), args.get("limit", 16384)
        if type(offset) is not int or not 0 <= offset <= 64 * 1024 * 1024 or type(limit) is not int or not 1 <= limit <= 16384:
            raise ValueError("bounded artifact page required")
        allowed = ["result.json", "correctness.log", *row.get("result", {}).get("profile_artifacts", [])]
        if not isinstance(name, str) or name not in allowed or Path(name).name != name:
            raise PermissionError("only recorded performance artifacts are available")
        if row["status"] not in {"completed", "failed", "cancelled", "interrupted"}:
            raise ValueError("performance artifact is not finalized")
        path = self.root / row["id"] / "artifacts" / name
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_size > 64 * 1024 * 1024:
                raise ValueError("performance artifact exceeds bound")
            os.lseek(fd, offset, os.SEEK_SET)
            body = os.read(fd, limit)
            return dict(request_id=row["request_id"], artifact=name, encoding="base64",
                        data=base64.b64encode(body).decode(), offset=offset,
                        next_offset=offset+len(body), total_bytes=info.st_size,
                        has_more=offset+len(body)<info.st_size)
        finally:
            os.close(fd)

    def scope(self, source_root, blob, config):
        base = source_root / "baseline.tar"
        if base.is_symlink():
            raise ValueError("unsafe performance baseline")
        def protected(name):
            return (name in {"v3/src/runtime_tests.rs", "v3/src/plugin_packages/optimization/measure.py", "v3/src/plugin_packages/optimization/workload.py"} or name.startswith("v3/tests/")
                    or name.endswith(".skill.md") or name.startswith("v3/config/")
                    or name.startswith("v3/data/config/")
                    or name.startswith("v3/plugins/abilities/") and name.endswith(".json")
                    or Path(name).name in {"Cargo.toml", "Cargo.lock"} and not config["allow_dependencies"])
        def members(stream):
            values = {}
            with tarfile.open(fileobj=stream, mode="r:") as archive:
                for member in archive:
                    name = member.name.removeprefix("./")
                    if member.isfile() and protected(name):
                        if member.size > 16 * 1024 * 1024 or name in values:
                            raise ValueError("invalid optimization scope input")
                        values[name] = hashlib.sha256(archive.extractfile(member).read()).hexdigest()
            return values
        with base.open("rb") as stream:
            original = members(stream)
        current = members(io.BytesIO(blob))
        if original != current:
            raise PermissionError("optimization excludes prompt, guidance, configuration and tool changes; dependencies require explicit effective permission")

    def assess(self, row):
        result = row["result"]
        if result.get("status") != "completed" or result.get("correctness") != "passed":
            row["accepted"] = False
            row["comparison"] = "correctness or representative measurement unverified"
            return
        references = [r for r in self.rows.values() if r["id"] != row["id"]
                      and r["task_key"] == row["task_key"] and r["action"] == "benchmark"
                      and r.get("accepted") is True]
        if not references:
            if row.get("role", "baseline") != "baseline":
                row.update(accepted=False, comparison="successful original baseline required")
                return
            if any(type(result.get(key)) not in (int, float) or not math.isfinite(result[key]) or result[key] <= 0
                   for key in ("wall_seconds", "peak_rss_bytes")):
                row.update(accepted=False, comparison="baseline metrics missing or invalid")
                return
            row.update(accepted=True, comparison="original baseline")
            return
        original = min(references, key=lambda r: r["created_at"])
        priority = row["settings"]["priority"]
        objective = "peak_rss_bytes" if priority == "memory" else "wall_seconds"
        def score(value):
            metrics = value["result"]
            if priority == "balanced":
                return metrics.get("wall_seconds", float("inf")) * metrics.get("peak_rss_bytes", float("inf"))
            return metrics.get(objective, float("inf"))
        best = min(references, key=score)
        limits = {"wall_seconds": row["settings"]["max_slowdown_percent"],
                  "peak_rss_bytes": row["settings"]["max_memory_regression_percent"]}
        reasons = []
        for reference in (original, best):
            if reference["image"] != row["image"] or reference["plugin_generation"] != row["plugin_generation"]:
                reasons.append("environment or harness changed")
            for metric, tolerance in limits.items():
                current, before = result.get(metric), reference["result"].get(metric)
                if isinstance(current, bool) or isinstance(before, bool) or not isinstance(current, (int, float)) or not isinstance(before, (int, float)) or not math.isfinite(current) or not math.isfinite(before) or before <= 0 or current < 0:
                    reasons.append("missing comparable " + metric)
                elif current > before * (1 + tolerance / 100):
                    reasons.append(metric + " exceeds regression budget")
        if score(row) >= score(best):
            reasons.append("selected objective did not improve")
        row.update(accepted=not reasons, comparison="; ".join(sorted(set(reasons))) or "improved against original and best",
                   baseline_request_id=original["request_id"], best_request_id=best["request_id"])

    def run(self, row):
        started = time.monotonic()
        directory = self.root / row["id"]
        artifacts = directory / "artifacts"
        stop = self.events[row["id"]]
        def monitor():
            while not stop.wait(.25):
                try: self.valid(row)
                except Exception:
                    row["revoked"] = True
                    stop.set(); self.kill(row)
                    return
        watcher = threading.Thread(target=monitor, daemon=True)
        try:
            self.valid(row)
            row.update(status="running", started_at=time.time()); self.save(row)
            watcher.start()
            c = row["settings"]
            command = ["docker", "run", "--rm", "--name", row["container"],
                "--network", "none", "--read-only", "--cap-drop", "ALL",
                "--security-opt", "no-new-privileges", "--pids-limit", "256",
                "--cpus", str(c["cpu_cores"]), "--memory", str(c["ram_mb"]) + "m",
                "--memory-swap", str(c["ram_mb"]) + "m", "--user", "1000:1000",
                "--tmpfs", "/tmp:rw,nosuid,nodev,size=" + str(c["ram_mb"]) + "m",
                "--mount", "type=bind,src=" + str(directory / "source/v3") + ",dst=/source,readonly",
                "--mount", "type=bind,src=" + str(directory / "source/ui") + ",dst=/ui,readonly",
                "--mount", "type=bind,src=" + str(directory / "driver") + ",dst=/driver,readonly",
                "--mount", "type=bind,src=" + str(artifacts) + ",dst=/artifacts",
                "--mount", "type=bind,src=" + str(self.root / "caches" / row["cache_key"] / "target") + ",dst=/artifacts/target",
                "--mount", "type=bind,src=" + str(self.root / "caches" / row["cache_key"] / "cargo") + ",dst=/artifacts/cargo",
                "-e", "CARGO_HOME=/artifacts/cargo", "-e", "CARGO_NET_OFFLINE=true", "-e", "CARGO_BUILD_JOBS=" + str(c["cpu_cores"]),
                "-e", "CARGO_TARGET_DIR=/artifacts/target", "-w", "/source", row["image"],
                "python3", "/driver/measure.py",
                "--action", row["action"], "--coverage", c["coverage"],
                "--priority", c["priority"], "--output", "/artifacts/result.json",
                "--timeout-seconds", str(row["reserved_seconds"])]
            log = self.broker.runner.command(command, timeout=row["reserved_seconds"])
            self.valid(row)
            path = artifacts / "result.json"
            if path.is_symlink() or path.stat().st_size > 128 * 1024:
                raise ValueError("performance result exceeds bound")
            result = json.loads(path.read_text())
            if not isinstance(result, dict):
                raise ValueError("invalid performance result")
            row["provisional"] = True
            row["comparison_environment"] = "shared development host; timing may include contention"
            row.update(status="completed" if result.get("status") in {"completed", "unsupported"} else "failed",
                       result=result, log_excerpt=log[-4096:])
            if row["action"] == "benchmark":
                self.assess(row)

        except Exception as exc:
            row.update(status="cancelled" if row.get("revoked") or isinstance(exc, PermissionError) else "failed",
                       reason=self.broker.performance_scrub(str(exc))[-4096:])
            path = artifacts / "result.json"
            try:
                if not path.is_symlink() and path.is_file() and path.stat().st_size <= 128 * 1024:
                    result = json.loads(path.read_text())
                    if isinstance(result, dict):
                        row["result"] = result
                        if result.get("status") == "resource_exhausted":
                            resource = result.get("resource")
                            row["exhausted"] = "ram_mb" if resource == "memory" else "total_seconds"
                            row["reason"] = ("effective RAM budget exhausted; increase explicitly to retry"
                                             if resource == "memory" else "effective runtime budget exhausted")
                if getattr(exc, "exit_code", None) == 137 or "out of memory" in str(exc).lower():
                    row["reason"] = "effective RAM budget exhausted; increase explicitly to retry"
                    row["exhausted"] = "ram_mb"
                elif isinstance(exc, TimeoutError):
                    row["reason"] = "effective runtime budget exhausted"
                    row["exhausted"] = "total_seconds"
            except (OSError, ValueError):
                pass
        finally:
            stop.set()
            self.kill(row)
            # Reservations are charged after interruptions; completed work uses
            # its actual wall time. Queued time never consumes runtime.
            row["charged_seconds"] = (min(row["reserved_seconds"], max(1, int(time.monotonic()-started)+1))
                if row["status"] == "completed" else row["reserved_seconds"])
            row["finished_at"] = time.time()
            with self.broker.lock:
                try:
                    self.valid(row)
                except Exception:
                    row.update(status="cancelled", accepted=False,
                               reason="performance authority revoked before final publication")
                self.save(row)

    def shutdown(self):
        for row in self.rows.values():
            if row["status"] in {"queued", "running"}:
                row["revoked"] = True
                self.events.setdefault(row["id"], threading.Event()).set()
                self.kill(row); self.save(row)
        self.pool.shutdown(wait=True)
