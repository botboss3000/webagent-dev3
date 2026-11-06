#!/usr/bin/env python3
"""Operator client for the protected V3 development build coordinator.

Agents and people releasing v3 changes: read "Agent guide: releasing and monitoring" in
build-service/README.md first. It covers the full release cycle (begin, workcopy, patch,
apply, check, install-broker, finish, monitor, close), the monitor's exit codes, how to read
its stage tables and what to do when a release fails. Run this on the server:
WEBAGENT_BUILD_CONFIG=/etc/webagent/development/build-service.json python3 -B <service_dir>/buildctl.py --help
"""
import argparse
from pathlib import Path
import os,importlib.util
import json
import signal
import socket
import shutil
import subprocess
import sys
import tarfile
import time
import uuid

spec=importlib.util.spec_from_file_location("build_client_config",Path(__file__).with_name("config.py"))
configuration=importlib.util.module_from_spec(spec);spec.loader.exec_module(configuration)
CONFIG=configuration.load()
SOCKET=str(Path(CONFIG["runtime_root"])/"build.sock")
SERVICE="webagent-build-"+CONFIG["instance"]+".service"
BEATS=Path.home()/".cache"/"webagent-buildctl"
POLL_SECONDS = 15
MAX_WAIT_SECONDS = 30 * 60
TERMINAL = {"completed", "failed", "stale", "cancelled", "uncertain", "interrupted", "needs_decision"}
PENDING_EXIT = 3
DECISION_EXIT = 4


class BrokerUnresponsive(TimeoutError):
    """The broker accepted the request but sent no reply; distinct from a slow build stage."""


def call(name, arguments):
    request_id = str(uuid.uuid4())
    request = {"id": request_id, "name": name, "arguments": arguments}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(75)
        connection.connect(SOCKET)
        connection.sendall(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        try:
            with connection.makefile("rb") as stream:
                raw = stream.readline(2*1024*1024+1)
        except TimeoutError:
            raise BrokerUnresponsive(f"broker unresponsive: no reply to {name} within 75s; "
                                     "slow requests are listed in runtime/build/broker-error.log") from None
    if not raw.endswith(b"\n") or len(raw) > 2*1024*1024:
        raise RuntimeError("bounded broker response unavailable")
    response = json.loads(raw)
    if response.get("id") != request_id:
        raise RuntimeError("broker response ID mismatch")
    if response.get("status") != "success":
        raise RuntimeError(str(response.get("output", "broker request failed")))
    return response["output"]


def _excerpt(value, limit=1024):
    """Limit already-scrubbed broker text without splitting a UTF-8 sequence."""
    return str(value or "").encode("utf-8")[-limit:].decode("utf-8", "ignore")


def wait_task(task_id, timeout, request=call, clock=time.monotonic, pause=time.sleep):
    """Read one durable task and its eventual job; never schedule work."""
    result = {"task_id": task_id, "result": "still_pending"}
    try:
        receipt = next((item for item in request("v3_build_work", {"action": "status"})["own"]
                        if item["task_id"] == task_id), None)
    except (OSError, RuntimeError, ValueError, KeyError, TypeError):
        result["reason"] = "broker status unavailable; resume from this task ID"
        return result, PENDING_EXIT
    if receipt is None:
        result.update(result="failed", reason="task receipt unavailable to this caller")
        return result, 1
    job_id = receipt.get("job_id")
    if not job_id and (receipt.get("synchronization") or {}).get("total"):
        result.update(result="needs_decision", synchronization=receipt["synchronization"])
        return result, DECISION_EXIT
    if not job_id:
        result["reason"] = ("waiting for other registered tasks" if receipt["status"] == "closed"
                            else "task has not finished" if receipt["status"] == "open"
                            else "task was abandoned")
        if receipt["status"] == "abandoned":
            result["result"] = "failed"
            return result, 1
        return result, PENDING_EXIT

    result["job_id"] = job_id
    deadline = clock() + timeout
    while True:
        try:
            job = request("v3_worker_job", {"job_id": job_id})
        except (OSError, RuntimeError, ValueError, KeyError, TypeError):
            result["reason"] = "job record unavailable; resume from this task or job ID"
            return result, PENDING_EXIT
        result["status"] = job.get("status", "unknown")
        result["phase"] = job.get("phase", "unknown")
        if result["status"] in TERMINAL:
            result["reason"] = _excerpt(job.get("reason"))
            result["log_excerpt"] = _excerpt(job.get("log"))
            if result["status"] == "needs_decision":
                result.update(result="needs_decision", synchronization=job.get("synchronization"))
                return result, DECISION_EXIT
            if result["status"] != "completed":
                result["result"] = "failed"
                return result, 1
            try:
                containers = request("v3_build_status", {})["containers"]
            except (OSError, RuntimeError, ValueError, KeyError, TypeError):
                result["reason"] = "container health unavailable; resume from this task or job ID"
                return result, PENDING_EXIT
            result["containers"] = {name: {"state": containers.get(name, {}).get("state"),
                                            "health": containers.get(name, {}).get("health")}
                                    for name in ("api", "loop")}
            if all(entry == {"state": "running", "health": "healthy"}
                   for entry in result["containers"].values()):
                result["result"] = "success"
                return result, 0
            result["result"] = "failed"
            result["reason"] = "completed job but V3 container health check failed"
            return result, 1
        remaining = deadline - clock()
        if remaining <= 0:
            result["reason"] = "wait limit reached; resume from this task or job ID"
            return result, PENDING_EXIT
        pause(min(POLL_SECONDS, remaining))


# Operator release steps on the server itself, so any SSH session can run a full cycle.
def own_task(task_id, request=call):
    item = next((i for i in request("v3_build_work", {"action": "status", "task_id": task_id})["own"] if i["task_id"] == task_id), None)
    if item is None:
        raise ValueError("task receipt unavailable")
    return item


def reservation(task_id, request=call):
    """This task's ownership row (scopes, revision, epoch), or None once nothing is held."""
    for offset in range(0, 400, 40):
        rows = request("build_work", {"action": "reservations", "offset": offset, "limit": 40}).get("tasks", [])
        hit = next((row for row in rows if row.get("label") == task_id), None)
        if hit or not rows:
            return hit
    return None


def heartbeat(task_id, action):
    """Renew the task's ownership every 20 s from a detached process until stopped."""
    folder = BEATS / task_id
    folder.mkdir(parents=True, exist_ok=True)
    pidfile, stop = folder / "heartbeat.pid", folder / "heartbeat.stop"
    def alive():
        try:
            os.kill(int(pidfile.read_text()), 0)
            return True
        except (OSError, ValueError):
            return False
    if action == "status":
        return {"task_id": task_id, "running": alive(), "log": str(folder / "heartbeat.log")}
    if action == "stop":
        stop.touch()
        if alive():
            os.kill(int(pidfile.read_text()), signal.SIGTERM)
        return {"task_id": task_id, "running": False}
    if not alive():
        stop.unlink(missing_ok=True)
        process = subprocess.Popen([sys.executable, "-B", __file__, "_beat", task_id], stdout=subprocess.DEVNULL,
                                   stderr=open(folder / "heartbeat.log", "a"), start_new_session=True)
        pidfile.write_text(str(process.pid))
    return {"task_id": task_id, "running": True, "pid": int(pidfile.read_text())}


def beat(task_id, request=call, pause=time.sleep):
    stop, seq = BEATS / task_id / "heartbeat.stop", 0
    while not stop.exists():
        seq += 1
        # One refused beat must not end renewal and orphan the task.
        try:
            row = reservation(task_id, request) or {}
            if row.get("revision") is not None:
                request("build_work", {"action": "heartbeat", "task_id": task_id, "expected_revision": row["revision"],
                                       "expected_epoch": row.get("epoch", 1), "request_id": f"{task_id}-heartbeat-{uuid.uuid4().hex[:12]}"})
        except (OSError, RuntimeError, ValueError, KeyError, TypeError) as error:
            print(time.strftime("%H:%M:%S"), "beat", seq, "failed:", str(error)[:200], file=sys.stderr, flush=True)
        pause(20)


def install_broker(task_id, allow_stale=False, request=call, run=subprocess.run):
    """Install a broker-only task tree while idle: drain, stop, install, always start again."""
    if not os.environ.get("WEBAGENT_BUILD_CONFIG"):
        raise ValueError("set WEBAGENT_BUILD_CONFIG to the installed broker configuration")
    tree = Path(own_task(task_id, request)["workspace_path"])
    plan = request("v3_build_work", {"action": "preflight", "task_id": task_id, "offset": 0})
    refusal = (plan.get("release_plan") or {}).get("refusal") or ""
    if (plan.get("synchronization") or {}).get("affected"):
        raise ValueError("preflight reports synchronization drift; resolve before installing")
    if refusal and "Install this task tree" not in refusal:
        raise ValueError("preflight refused: " + refusal[:300])
    if request("v3_build_status", {}).get("busy"):
        raise ValueError("broker busy; install only while idle")
    request("build_service_maintenance", {"action": "drain", "task_id": task_id})
    run(["sudo", "-n", "systemctl", "stop", SERVICE], check=True)
    try:
        # lifecycle.py refuses a tree cut before the installed release unless --allow-stale-tree.
        out = run(["sudo", "-n", "python3", "-B", str(tree / "build-service/lifecycle.py"), "install", "--config", os.environ["WEBAGENT_BUILD_CONFIG"],
                   "--install-tree", tree.parent.name, *(["--allow-stale-tree"] if allow_stale else [])],
                  capture_output=True, text=True)
    finally:
        run(["sudo", "-n", "systemctl", "start", SERVICE], check=False)
    if out.returncode:
        raise RuntimeError("install refused: " + (out.stderr or out.stdout).strip()[-600:])
    active = run(["systemctl", "is-active", SERVICE], capture_output=True, text=True).stdout.strip()
    try:
        version = json.loads(out.stdout.strip().splitlines()[-1]).get("version")
    except (IndexError, ValueError, AttributeError):
        version = None
    return {"task_id": task_id, "installed_version": version, "service": active,
            "next": "buildctl finish " + task_id + " --expected <inputs_sha256> publishes it as host_service_deploy"}


def task_changes(tree):
    """V3-relative paths whose bytes differ between the task's baseline.tar and its tree, as the broker computes them."""
    base = {}
    with tarfile.open(tree / "baseline.tar") as archive:
        for member in archive:
            if member.isfile() and member.name.startswith("v3/"):
                base[member.name[3:]] = archive.extractfile(member).read()
    root = tree / "v3"
    current = {str(p.relative_to(root)) for p in root.rglob("*") if p.is_file() and not p.is_symlink()}
    return sorted(name for name in set(base) | current
                  if name not in current or name not in base or (root / name).read_bytes() != base[name])


def close(task_id, message, request=call, run=subprocess.run):
    """Commit exactly the released bytes, then release every claim with that commit; this completes the task."""
    task = own_task(task_id, request)
    if not task.get("job_id"):
        raise ValueError("no finished job to close; run finish first")
    if request("build_service_maintenance", {"action": "status"}).get("task_id") == task_id:
        request("build_service_maintenance", {"action": "resume", "task_id": task_id})
    job = request("v3_worker_job", {"job_id": task["job_id"]})
    if job.get("status") != "completed":
        raise ValueError(f"job {task['job_id']} is {job.get('status')}; reconcile before committing")
    containers = request("v3_build_status", {})["containers"]
    if any(containers.get(r, {}).get("health") != "healthy" for r in ("api", "loop")):
        raise ValueError("deployment not healthy; do not commit")
    tree, checkout = Path(task["workspace_path"]).parent, Path(CONFIG["checkout_root"])
    paths = task_changes(tree)
    if not paths:
        raise ValueError("task changed no files")
    differs = [p for p in paths if ((checkout / p).read_bytes() if (checkout / p).exists() else None)
               != ((tree / "v3" / p).read_bytes() if (tree / "v3" / p).exists() else None)]
    if differs:
        raise ValueError("checkout differs from the released tree: " + ", ".join(differs[:10]))
    git = ["git", "-C", str(checkout)]
    if run(git + ["diff", "--cached", "--name-only"], capture_output=True, text=True, check=True).stdout.strip():
        raise ValueError("checkout has staged changes from another writer; resolve before committing")
    if "broker job" not in message:
        # On the subject line: the last line may be a trailer such as Co-Authored-By.
        subject, _, rest = message.strip().partition("\n")
        message = subject + f" (broker job {task['job_id']})" + ("\n" + rest if rest else "") + "\n"
    run(git + ["add", "--", *paths], check=True)
    run(git + ["commit", "-q", "-F", "-"], input=message, text=True, check=True)
    commit = run(git + ["rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    reply = {}
    # The heartbeat keeps renewing, so read the revision right before each attempt; a row may list part of the scopes.
    for attempt in range(1, 9):
        row = reservation(task_id, request) or {}
        if attempt > 1 and not row.get("scopes"):
            break
        try:
            reply = request("build_work", {"action": "release_claims", "task_id": task_id, "scopes": row.get("scopes", []),
                "reason": f"Release {task['job_id'][:8]} deployed healthy and committed as {commit[:7]}",
                "request_id": f"{task_id}-release-claims-{commit[:12]}-{attempt}", "expected_revision": row.get("revision"),
                "expected_epoch": row.get("epoch"), "commit": commit})
        except RuntimeError as error:
            if "Stale ownership revision" not in str(error) or attempt == 8:
                raise
            time.sleep(1)
            continue
        if reply.get("state") == "complete":
            break
    if reply.get("state") == "complete":
        heartbeat(task_id, "stop")
    return {"task_id": task_id, "commit": commit, "paths": paths, "state": reply.get("state"), "scopes": reply.get("scopes")}


def minutes(seconds):
    return "TBD" if seconds is None else f"{seconds / 60:.1f}m"


LABELS = {"deploy and health checks": "deploy and health", "UI build and tests": "UI checks",
          "package release": "package release (api ‖ loop)"}
LEGEND = "a/b = run at the same time · ‖ = in parallel inside a stage"


def stage_table(progress):
    """The stage tables operators paste: finished, running and upcoming stages against their p50-p90 range,
    then the background work (early UI tests, rollback save) when the job has any."""
    def normal(value):
        return f"({value[0] / 60:.1f}-{value[1] / 60:.1f}m)" if value else "(no baseline)"
    def label(name):
        name = LABELS.get(name, name)
        return name[0].upper() + name[1:]
    rows = ([(s, " ✅", minutes(s.get("seconds"))) for s in progress.get("finished_stages", [])]
            + [(s, " ⏳", minutes(s.get("seconds"))) for s in progress.get("running_stages", [])]
            + [(s, "", "TBD") for s in progress.get("upcoming_stages", [])])
    if all(s.get("id") for s, _, _ in rows):
        rows.sort(key=lambda r: (int(r[0]["id"].rstrip("abcdefgh")), r[0]["id"]))
    mark = {"completed": " ✅", "queued": " ⏳", "running": " ⏳"}.get(progress.get("status"), " ❌")
    lines = ["| Stage | Time (normal) |", "|---|---|"]
    lines += [f"| {s.get('id') or i}. {label(s['stage'])}{flag} | {took} {normal(s.get('normal'))} |" for i, (s, flag, took) in enumerate(rows, 1)]
    lines.append(f"| **Job**{mark} | **{minutes(progress.get('job_seconds'))}** {normal(progress.get('job_normal'))} |")
    if any(s.get("id", "")[-1:].isalpha() for s, _, _ in rows):
        lines += ["", LEGEND]
    background = progress.get("background") or []
    if background:
        lines += ["", "| Background | Time | Needed by | Result |", "|---|---|---|---|"]
        flags = {"completed": " ✅", "running": " ⏳", "failed": " ❌", "interrupted": " ❌"}
        lines += [f"| {b['name'][0].upper() + b['name'][1:]}{flags.get(b['status'], '')} | {minutes(b.get('seconds'))} | "
                  f"{b.get('needed_by') or ''} | {b.get('result') or ''} |" for b in background]
    return "\n".join(lines)


def status_line(progress):
    """One line above the tables: what runs now, or what a queued job waits behind, and the time left."""
    running = progress.get("running_stages", [])
    count = len({str(s.get("id", "")).rstrip("abcdefgh") for s in progress.get("finished_stages", []) + running + progress.get("upcoming_stages", [])})
    if progress.get("status") == "queued":
        ahead = progress.get("queued_behind") or []
        return "queued" + (" behind " + "; ".join(f"{a.get('task_id')} (session \"{a.get('session') or 'unknown'}\", job {a['job_id'][:8]}, {a.get('phase')})"
                                                   for a in ahead) if ahead else "")
    low, high = progress.get("remaining_seconds") or [0, 0]
    left = f"; about {low / 60:.1f}-{high / 60:.1f} min left" if high else ""
    if len(running) > 1:
        return f"stages {'+'.join(str(s.get('id')) for s in running)}/{count} running: " + ", ".join(
            f"{s['stage']} {minutes(s.get('seconds'))}" for s in running) + left
    if not running and progress.get("status") == "running":
        # Started, but between stages (or before the first): name the last one that finished.
        last = (progress.get("finished_stages") or [{}])[-1].get("stage")
        return ("running, between stages after " + last if last else "running, starting its first stage") + left
    return (f"stage {progress.get('stage_id') or progress.get('stage_index')}/{count} {progress.get('stage')} "
            f"{minutes(progress.get('stage_seconds'))}, {progress.get('verdict')}{left}")


def watch(job_id, step=False, request=call, pause=time.sleep):
    """Print the stage table at each stage change; exit 0 completed, 1 other terminal, 3 at a stage change with --step."""
    seen = None
    while True:
        progress = request("v3_build_progress", {"job_id": job_id})
        key = (progress.get("status"), progress.get("stage"), len(progress.get("finished_stages", [])))
        if key != seen:
            print(f"{progress.get('status')}: stage {progress.get('stage_index')} of {progress.get('stage_count')} {progress.get('stage')}, "
                  f"verdict {progress.get('verdict')}\n" + stage_table(progress), flush=True)
            if progress.get("status") in TERMINAL:
                return 0 if progress["status"] == "completed" else 1
            if step and seen is not None:
                return PENDING_EXIT
            seen = key
        pause(POLL_SECONDS)


STALL_SECONDS = 900
LATE_EVERY = 300
LATE_FLOOR = 60  # never call a stage or queue wait late before a minute, however small its p95
UNRESPONSIVE_TRIES = 3  # reply timeouts in a row before monitor reports the broker unresponsive
PARALLEL_HOLD = 120
MONITOR_TIMEOUT = 3600


def monitor(task_id=None, job_id=None, request=call, pause=time.sleep, clock=time.time, state_root=None, timeout=MONITOR_TIMEOUT):
    """Follow one release and return at the next event its agent should report; relaunch until 0, 1 or 2.

    Exit 0 completed with healthy containers; 1 failed, cancelled, uncertain, interrupted, needs a decision,
    or completed unhealthy; 2 no job appeared for the task within timeout; 3 the job queued, its queue
    changed, it started, or a stage finished (it waits up to PARALLEL_HOLD for a parallel partner to start,
    so 3a and 3b show together); 4 a running stage passed max(p95, LATE_FLOOR) (p95 needs 5 recent jobs),
    a job stayed queued LATE_FLOOR, or a live step reported nothing for STALL_SECONDS; late wakes repeat
    every LATE_EVERY while the stage runs or the job stays queued. State between relaunches lives under ~/.cache/webagent-buildctl/monitor/."""
    state_root = Path(state_root or BEATS / "monitor")
    state_path = state_root / ((task_id or job_id) + ".json")
    started = clock()
    try:
        saved = json.loads(state_path.read_text())
    except (OSError, ValueError):
        saved = None
    def save(value):
        state_root.mkdir(parents=True, exist_ok=True)
        state_path.write_text(json.dumps(value))
    def show(progress):
        print(status_line(progress) + "\n\n" + stage_table(progress), flush=True)
    held, quiet = None, 0
    while True:
        if job_id is None:
            task = own_task(task_id, request)
            if task.get("status") == "abandoned":
                print(json.dumps({"task_id": task_id, "result": "task abandoned"}));return 1
            if not task.get("job_id"):
                if clock() - started > timeout:
                    print(json.dumps({"task_id": task_id, "result": "no job appeared", "status": task.get("status")}));return 2
                pause(POLL_SECONDS);continue
            job_id = task["job_id"]
        try:
            progress = request("v3_build_progress", {"job_id": job_id})
            quiet = 0
        except BrokerUnresponsive as error:
            # Back off instead of piling more reads on a busy broker; the job may be fine.
            quiet += 1
            if quiet >= UNRESPONSIVE_TRIES:
                print(str(error) + f"; {quiet} tries over backoff, the job state is unknown, relaunch to check again", flush=True)
                return DECISION_EXIT
            pause(min(POLL_SECONDS * 2 ** quiet, 120))
            continue
        status = progress.get("status")
        if status in TERMINAL:
            job = request("v3_worker_job", {"job_id": job_id})
            healthy = None
            if status == "completed":
                containers = request("v3_build_status", {})["containers"]
                healthy = all(containers.get(r, {}).get("health") == "healthy" for r in ("api", "loop"))
            print(json.dumps({"task_id": task_id, "job_id": job_id, "status": status, "healthy": healthy,
                              "reason": _excerpt(job.get("reason"), 400), "images": job.get("images"),
                              "synchronization": (job.get("synchronization") or {}).get("status")}, sort_keys=True))
            show(progress)
            state_path.unlink(missing_ok=True)
            return 0 if status == "completed" and healthy else 1
        finished = sorted(s["stage"] for s in progress.get("finished_stages", []))
        queue = [[a.get("job_id"), a.get("phase")] for a in progress.get("queued_behind") or []]
        current = {"job_id": job_id, "status": status, "finished": finished, "queue": queue,
                   "alerted": (saved or {}).get("alerted", {}), "stalled": (saved or {}).get("stalled", [])}
        if saved is None or saved.get("job_id") != job_id:
            saved = current;save(current);show(progress)
        changed = (status != saved["status"] or set(finished) - set(saved["finished"])
                   or (status == "queued" and queue != saved["queue"]))
        if changed:
            running_ids = {str(s.get("id", "")).rstrip("abcdefgh") for s in progress.get("running_stages", [])}
            partner = any(str(s.get("id", ""))[-1:].isalpha() and str(s["id"]).rstrip("abcdefgh") in running_ids
                          for s in progress.get("upcoming_stages", []))
            held = held or clock()
            if not partner or clock() - held >= PARALLEL_HOLD:
                save(current);show(progress);return PENDING_EXIT
        alerted, stalled = current["alerted"], current["stalled"]
        for stage in progress.get("running_stages", []):
            name, seconds, late = stage["stage"], stage.get("seconds") or 0, stage.get("late")
            stall = stage["stage"] == progress.get("stage") and (progress.get("idle_seconds") or 0) > STALL_SECONDS and name not in stalled
            due = late and stage.get("samples", 0) >= 5 and late_due(seconds, late, alerted, name)
            if stall or due:
                if stall:
                    stalled.append(name)
                print(f"{'no progress for ' + str(progress.get('idle_seconds')) + 's in' if stall else 'past its normal range:'} "
                      f"{stage.get('id')} {name} at {minutes(seconds)} (late after {minutes(late)})")
                save(current);show(progress);return DECISION_EXIT
        waited = progress.get("job_seconds") or 0
        if status == "queued" and late_due(waited, 0, alerted, "queued"):
            print(f"still queued after {minutes(waited)}")
            save(current);show(progress);return DECISION_EXIT
        idle = progress.get("idle_seconds") or 0
        if status == "running" and not progress.get("running_stages") and idle > STALL_SECONDS and "between stages" not in stalled:
            # No command running and nothing reported: the runner never started its next step.
            stalled.append("between stages")
            print(f"no command or progress for {idle}s after {progress.get('stage')}")
            save(current);show(progress);return DECISION_EXIT
        pause(POLL_SECONDS)


def late_due(seconds, late, alerted, name):
    """True once seconds reach max(late, LATE_FLOOR), then every LATE_EVERY; records the check-in in alerted."""
    first = max(late, LATE_FLOOR)
    if seconds < first + alerted.get(name, 0) * LATE_EVERY:
        return False
    alerted[name] = int((seconds - first) // LATE_EVERY) + 1
    return True


IGNORED = ("__pycache__", ".pytest_cache", "target")


def workcopy(task_id, folder, request=call):
    """Copy the task's isolated tree (its v3 sources) to a private scratch folder to edit."""
    tree = Path(own_task(task_id, request)["workspace_path"]).parent / "v3"
    folder = Path(folder)
    if folder.exists():
        raise ValueError(f"{folder} exists; choose a new folder")
    shutil.copytree(tree, folder, symlinks=True, ignore=shutil.ignore_patterns(*IGNORED, "*.pyc"))
    return {"task_id": task_id, "folder": str(folder), "tree": str(tree)}


def credential_findings(paths, root):
    """Run the checkout's Git Control credential guard over changed files; returns the flagged paths only."""
    adapter = (Path(CONFIG["checkout_root"]) / "plugins/abilities/Administrator/git_control/adapter.py").read_text()
    namespace = {"re": __import__("re")}
    exec(adapter[adapter.index("_DUMMY_CREDENTIALS={"):adapter.index("async def command(")], namespace)
    return [p for p in paths if (root / p).is_file() and namespace["secret"]((root / p).read_bytes())]


def make_patch(task_id, folder, request=call, run=subprocess.run):
    """A unified diff from the task's tree to the edited workcopy, after the credential guard; prints no file content."""
    tree = Path(own_task(task_id, request)["workspace_path"]).parent / "v3"
    folder = Path(folder)
    def files(root):
        return {str(p.relative_to(root)) for p in root.rglob("*") if (p.is_file() or p.is_symlink())
                and not any(part in IGNORED for part in p.relative_to(root).parts) and p.suffix != ".pyc"}
    names = sorted(n for n in files(tree) | files(folder)
                   if not (tree / n).exists() or not (folder / n).exists() or (tree / n).read_bytes() != (folder / n).read_bytes())
    if not names:
        raise ValueError("workcopy matches the task tree; nothing to patch")
    flagged = credential_findings(names, folder)
    if flagged:
        raise ValueError("credential guard blocked: " + ", ".join(flagged[:10]) + "; build fixture values without credential-shaped literals")
    chunks = []
    for name in names:
        a = tree / name if (tree / name).exists() else Path("/dev/null")
        b = folder / name if (folder / name).exists() else Path("/dev/null")
        out = run(["git", "diff", "--no-index", "--full-index", "--binary", "--src-prefix=a/", "--dst-prefix=b/", str(a), str(b)],
                  capture_output=True, text=True).stdout
        for side in (a, b):
            out = out.replace(f"a{side}", f"a/{name}").replace(f"b{side}", f"b/{name}")
        chunks.append(out)
    path = BEATS / task_id / f"{task_id}.patch"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(chunks))
    return {"task_id": task_id, "patch": str(path), "files": names, "credential_guard": "passed"}


def main():
    parser = argparse.ArgumentParser(description="Coordinate V3 development builds with WebAgent")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("status")
    clearance = commands.add_parser("clear-retention", help="record investigated terminal task cleanup clearance")
    clearance.add_argument("notice_id")
    clearance.add_argument("--unique-work-preserved", action="store_true", required=True)
    clearance.add_argument("--effects-reconciled", action="store_true", required=True)
    commands.add_parser("leftovers", help="read this operator caller’s retained-task notices")
    review = commands.add_parser("review-leftover", help="acknowledge one unchanged notice; never delete work")
    review.add_argument("notice_id")
    inputs = commands.add_parser("inputs")
    inputs.add_argument("--task", help="read this task's isolated workspace digest")
    begin = commands.add_parser("begin")
    begin.add_argument("task_id")
    begin.add_argument("summary")
    begin.add_argument("--shared", action="store_true", help="legacy shared checkout coordination")
    finish = commands.add_parser("finish")
    finish.add_argument("task_id")
    finish.add_argument("--package-memory-cap",type=int,help="Explicit enforced dedicated BuildKit ceiling in bytes")
    finish.add_argument("--compiler-memory-cap",type=int,help="Explicit enforced compiler ceiling in bytes")
    finish.add_argument("--expected", required=True, help="inputs_sha256 obtained from inputs")
    finish.add_argument("--sync-decision", choices=("synchronize", "proceed", "cancel"))
    finish.add_argument("--sync-digest", help="exact digest returned by preflight")
    preflight = commands.add_parser("preflight", help="read content-free checkout drift for a task")
    preflight.add_argument("task_id")
    preflight.add_argument("--offset", type=int, default=0)
    decide = commands.add_parser("decide", help="decide an existing paused job; never create a duplicate")
    decide.add_argument("task_id", help="task ID; the job ID with --accept-ui-failure")
    decide.add_argument("sync_decision", nargs="?", choices=("synchronize", "proceed", "cancel"))
    decide.add_argument("--sync-digest")
    decide.add_argument("--accept-ui-failure", action="append", metavar="TEST_FILE",
                        help="operator only: accept exactly the twice-failed UI test files of a paused job")
    decide.add_argument("--reason", help="required with --accept-ui-failure, 10-300 characters")
    abandon = commands.add_parser("abandon")
    abandon.add_argument("task_id")
    progress = commands.add_parser("progress", help="stage k of N with durations against class history; no logs")
    progress.add_argument("job_id")
    progress.add_argument("--table", action="store_true", help="print the Markdown stage table instead of JSON")
    watch_parser = commands.add_parser("watch", help="print the stage table at each stage change until the job ends")
    watch_parser.add_argument("job_id")
    watch_parser.add_argument("--step", action="store_true", help="also exit 3 at each stage change, for background waits")
    mon = commands.add_parser("monitor", help="follow one release in the background; exit 0/1/2 at the end, 3 at a stage or queue change, 4 when late or stalled")
    target = mon.add_mutually_exclusive_group(required=True)
    target.add_argument("--task", help="the task label (follows its finish job)")
    target.add_argument("--job", help="one job ID")
    mon.add_argument("--timeout", type=int, default=MONITOR_TIMEOUT, help="seconds to wait for a task's job to appear (exit 2)")
    copy = commands.add_parser("workcopy", help="copy a task's tree to a private folder to edit on the server")
    copy.add_argument("task_id")
    copy.add_argument("folder")
    diff = commands.add_parser("patch", help="credential-guarded unified diff from the task tree to an edited workcopy")
    diff.add_argument("task_id")
    diff.add_argument("folder")
    install = commands.add_parser("install-broker", help="install a broker-only task tree while idle (drain, stop, install, start)")
    install.add_argument("task_id")
    install.add_argument("--allow-stale-tree", action="store_true", help="deliberate rollback of a newer installed release")
    close_parser = commands.add_parser("close", help="commit the released bytes and release every claim with that commit")
    close_parser.add_argument("task_id")
    close_parser.add_argument("--message-file", required=True, help="commit message file, or - for stdin")
    beat_parser = commands.add_parser("heartbeat", help="renew a task's ownership from a detached process")
    beat_parser.add_argument("task_id")
    beat_parser.add_argument("action", choices=("start", "stop", "status"))
    commands.add_parser("_beat").add_argument("task_id")
    rebuild = commands.add_parser("rebuild", help="release the live checkout as it is (dev: changed packages or build; stable: full linked build)")
    rebuild.add_argument("--mode", choices=("dev", "stable"), default="dev")
    rebuild.add_argument("--request-id", required=True)
    adopt = commands.add_parser("adopt", help="operator continuation for an uncertain release; never replays effects")
    adopt.add_argument("job_id")
    job = commands.add_parser("job")
    job.add_argument("job_id")
    job.add_argument("--log-cursor", type=int, help="Read only retained events after this job cursor")
    cancel = commands.add_parser("cancel", help="request targeted stop; deployment effects require reconciliation")
    cancel.add_argument("job_id")
    cancel.add_argument("--include-batch", action="store_true", help="explicitly stop every member of an admitted shared build")
    cancel.add_argument("--request-id", default=None, help="stable cancellation receipt identity")
    wait = commands.add_parser("wait", help="poll a finished task's shared job without submitting work")
    wait.add_argument("task_id")
    wait.add_argument("--timeout", type=int, default=MAX_WAIT_SECONDS,
                      help="seconds to poll an assigned job, 0-1800 (default: 1800)")
    maintenance=commands.add_parser("maintenance")
    maintenance.add_argument("action",choices=("status","drain","resume"))
    maintenance.add_argument("--task-id")
    schedule=commands.add_parser("schedule",help="structured ownership and resource control through build_work")
    schedule.add_argument("--input",required=True,help="JSON request file, or - for stdin")
    apply=commands.add_parser("apply",help="checked isolated source patch with derived ownership")
    apply.add_argument("task_id");apply.add_argument("--patch",required=True)
    apply.add_argument("--expected",required=True);apply.add_argument("--request-id",required=True)
    check=commands.add_parser("check",help="Run a durable task-scoped compile check without deployment")
    check.add_argument("task_id");check.add_argument("--expected",required=True)
    check.add_argument("--request-id",required=True)
    check.add_argument("--check",choices=("check","test","fmt","clippy","ui_test"),default="check",help="ui_test runs the UI fixtures the task's changed paths select, without deploying")
    check.add_argument("--test-filter",help="check=test only: run the tests whose names contain this text")
    check.add_argument("--feature-profile",choices=("default","live"))
    check.add_argument("--timeout",type=int,default=600)
    check.add_argument("--compiler-memory-cap",type=int)
    args = parser.parse_args()
    if args.command == "decide":
        if args.accept_ui_failure:
            if args.sync_decision or args.sync_digest or not args.reason:
                parser.error("--accept-ui-failure takes a job ID and --reason, without a sync decision")
            print(json.dumps(call("v3_build_adopt_release", {"job_id": args.task_id, "accept_ui_failure": args.accept_ui_failure, "reason": args.reason}), sort_keys=True))
            return 0
        if not args.sync_decision or not args.sync_digest or args.reason:
            parser.error("decide requires a sync decision and --sync-digest")
    if args.command=="check":
        if not 1<=args.timeout<=600:parser.error("--timeout must be between 1 and 600 seconds")
        request={"action":"check","task_id":args.task_id,"check":args.check,"expected_inputs_sha256":args.expected,
            "request_id":args.request_id,"timeout_seconds":args.timeout}
        if args.test_filter is not None:request["test_filter"]=args.test_filter
        if args.feature_profile is not None:request["feature_profile"]=args.feature_profile
        if args.compiler_memory_cap is not None:request["compiler_memory_cap"]=args.compiler_memory_cap
        print(json.dumps(call("build_work",request),sort_keys=True));return 0
    if args.command=="schedule":
        value=json.loads(sys.stdin.read(1024*1024)if args.input=="-"else Path(args.input).read_text())
        print(json.dumps(call("build_work",value),sort_keys=True));return 0
    if args.command=="apply":
        patch=Path(args.patch).read_text()
        print(json.dumps(call("v3_source_apply",{"task_id":args.task_id,"request_id":args.request_id,"expected_inputs_sha256":args.expected,"patch":patch}),sort_keys=True));return 0
    if args.command=="maintenance":
        print(json.dumps(call("build_service_maintenance",{"action":args.action,**({"task_id":args.task_id} if args.task_id else {})}),sort_keys=True));return 0
    if args.command == "_beat":
        beat(args.task_id);return 0
    if args.command == "watch":
        return watch(args.job_id, args.step)
    if args.command == "monitor":
        return monitor(args.task, args.job, timeout=args.timeout)
    if args.command == "workcopy":
        print(json.dumps(workcopy(args.task_id, args.folder), sort_keys=True));return 0
    if args.command == "patch":
        print(json.dumps(make_patch(args.task_id, args.folder), sort_keys=True));return 0
    if args.command == "progress" and args.table:
        print(stage_table(call("v3_build_progress", {"job_id": args.job_id})));return 0
    if args.command == "install-broker":
        print(json.dumps(install_broker(args.task_id, args.allow_stale_tree), sort_keys=True));return 0
    if args.command == "close":
        message = sys.stdin.read(64 * 1024) if args.message_file == "-" else Path(args.message_file).read_text()
        print(json.dumps(close(args.task_id, message), sort_keys=True));return 0
    if args.command == "heartbeat":
        print(json.dumps(heartbeat(args.task_id, args.action), sort_keys=True));return 0
    if args.command == "wait":
        if not 0 <= args.timeout <= MAX_WAIT_SECONDS:
            parser.error("--timeout must be between 0 and 1800 seconds")
        result, code = wait_task(args.task_id, args.timeout)
        print(json.dumps(result, sort_keys=True))
        return code
    if args.command == "inputs":
        if args.task:
            own = call("v3_build_work", {"action": "status", "task_id": args.task})["own"]
            result = next((item for item in own if item["task_id"] == args.task), None)
            if result is None:
                raise ValueError("task receipt unavailable")
        else:
            result = call("v3_build_status", {})
    elif args.command == "cancel":
        job = call("v3_worker_job", {"job_id": args.job_id})
        affected = job.get("cancellation", {}).get("affected_job_ids", [args.job_id])
        if len(affected) > 1 and not args.include_batch:
            raise ValueError("shared build affects " + ", ".join(affected) + "; use --include-batch to stop all members")
        result = call("v3_build_cancel", {"job_id": args.job_id,
                     "request_id": args.request_id or "cancel-" + args.job_id,
                     "expected_job_ids": affected})
        if result.get("job") is None:
            raise ValueError("build job receipt unavailable")
    elif args.command == "clear-retention":
        result = call("v3_build_retention_clearance", {"notice_id": args.notice_id,
            "unique_work_preserved": args.unique_work_preserved, "effects_reconciled": args.effects_reconciled})
    elif args.command == "leftovers":
        result = call("v3_build_leftovers", {})
    elif args.command == "review-leftover":
        result = call("v3_build_leftover_review", {"notice_id": args.notice_id})
    elif args.command == "rebuild":
        result = call("v3_build_rebuild_checkout", {"mode": args.mode, "request_id": args.request_id})
    elif args.command == "adopt":
        result = call("v3_build_adopt_release", {"job_id": args.job_id})
    elif args.command == "progress":
        result = call("v3_build_progress", {"job_id": args.job_id})
    elif args.command == "job":
        result = call("v3_worker_job", {"job_id": args.job_id, **({"log_cursor": args.log_cursor} if args.log_cursor is not None else {})})
    else:
        arguments = {"action": args.command}
        if args.command in ("begin", "finish", "abandon", "preflight", "decide"):
            arguments["task_id"] = args.task_id
        if args.command == "begin":
            arguments["summary"] = args.summary
            if not args.shared:
                arguments["mode"] = "isolated"
        if args.command == "finish":
            arguments["expected_inputs_sha256"] = args.expected
            if args.package_memory_cap is not None:arguments["package_memory_cap"]=args.package_memory_cap
            if args.compiler_memory_cap is not None:arguments["compiler_memory_cap"]=args.compiler_memory_cap
            if args.sync_decision:
                arguments["sync_decision"] = args.sync_decision
                arguments["expected_sync_digest"] = args.sync_digest
        if args.command == "preflight":
            arguments["offset"] = args.offset
        if args.command == "decide":
            arguments.update(sync_decision=args.sync_decision, expected_sync_digest=args.sync_digest)
        result = call("v3_build_work", arguments)
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
