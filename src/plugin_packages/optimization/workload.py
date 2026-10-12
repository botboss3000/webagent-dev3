"""Fixed representative v3 native workloads; user input never becomes a command."""
import json
import hashlib
import os
import pathlib
import re
import shutil
import time
from measure import run

QUICK = (
    "message_receipts_are_idempotent_and_conflicting_reuse_is_rejected",
    "publication_commits_one_terminal_message_and_private_receipt",
    "lightweight_queue_keeps_history_durable_and_fifo_across_updates",
)
STANDARD = QUICK + (
    "changed_policy_invalidates_pending_action_approval",
    "recovery_holds_uncertain_writes_and_requeues_safe_reads",
    "parallel_batch_overlaps_reads_denies_write_by_policy_and_preserves_result_order",
)
EXTENDED = STANDARD + (
    "ability_revocation_keeps_parallel_write_outcomes_uncertain",
    "failed_context_summary_keeps_original_input_for_resume",
    "provider_recovery_retries_stream_failure_and_publishes_one_final_reply",
    "parallel_batch_rejects_duplicate_provider_call_ids_before_dispatch",
)


def selected_tests(coverage):
    return {"quick": QUICK, "standard": STANDARD, "extended": EXTENDED}[coverage]


def heaptrack_totals(path, timeout_seconds, *, cwd, env):
    # Backtraces can exceed the bounded hotspot capture before the footer.
    # Request a separate compact report so totals never depend on that prefix.
    command = ["heaptrack_print", "-f", str(path), "--print-peaks=0",
               "--print-allocators=0", "--print-temporary=0", "--print-leaks=0"]
    report = run(command, timeout_seconds, cwd=cwd, env=env, output_limit=4096)
    if (report["exit_code"] != 0 or report["status"] != "completed"
            or report["output_truncated"]):
        return None
    text = report["stdout"]
    calls = re.search(r"calls to allocation functions: ([0-9]+)", text)
    peak = re.search(r"peak heap memory consumption: ([^\n]+)", text)
    if not calls or not peak:
        return None
    return {"allocation_calls": int(calls.group(1)),
            "peak_heap_memory": peak.group(1).strip()}


def source_hash(root):
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        if path.is_file():
            relative = str(path.relative_to(root)).encode()
            digest.update(len(relative).to_bytes(8, "big"))
            digest.update(relative)
            content = hashlib.sha256()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1048576), b""):
                    content.update(chunk)
            digest.update(content.digest())
    return digest.hexdigest()


def oom_kills():
    try:
        values = dict(line.split() for line in pathlib.Path("/sys/fs/cgroup/memory.events").read_text().splitlines())
        return int(values["oom_kill"])
    except (OSError, ValueError, KeyError):
        return None


def execute(action, coverage, priority, output, timeout_seconds):
    """Build frozen sources, then measure correctness-asserting native tests.

    Cargo compilation is charged to runtime and resources but kept out of the
    reported workload timings. Profiling overhead is never a plain benchmark.
    """
    started = time.monotonic()
    initial_oom = oom_kills()
    def exhausted():
        current = oom_kills()
        return initial_oom is not None and current is not None and current > initial_oom
    tool = None
    if action == "profile_cpu":
        tool = shutil.which("valgrind")
    elif action == "profile_allocations":
        tool = shutil.which("heaptrack")
    if action != "benchmark" and tool is None:
        return {"status": "unsupported", "correctness": "unverified",
                "reason": "compatible profiler tooling not installed", "action": action}
    admitted_source = pathlib.Path("/source")
    artifact = pathlib.Path(output).parent
    artifact.mkdir(parents=True, exist_ok=True)
    # Native correctness tests deliberately mutate then restore source-local
    # config. Their private writable copy never changes admitted broker inputs.
    source = artifact / "source"
    if source.exists():
        return {"status": "failed", "correctness": "unverified", "action": action,
                "reason": "private workload source already exists"}
    shutil.copytree(admitted_source, source)
    for path in source.rglob("*"):
        if path.is_dir():
            path.chmod(path.stat().st_mode | 0o700)
        elif path.is_file():
            path.chmod(path.stat().st_mode | 0o600)
    shared_ui = artifact / "ui"
    if pathlib.Path("/ui").is_dir() and not shared_ui.exists():
        shared_ui.symlink_to("/ui", target_is_directory=True)
    initial_source_hash = source_hash(source)
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(artifact / "target")
    cargo_home = artifact / "cargo"
    cargo_home.mkdir(exist_ok=True)
    cache = pathlib.Path("/opt/optimization-cargo/registry")
    if not cache.is_dir():
        return {"status": "unsupported", "correctness": "unverified",
                "reason": "locked offline Cargo registry not installed", "action": action}
    registry = cargo_home / "registry"
    if not registry.exists():
        shutil.copytree(cache, registry)
    env["CARGO_HOME"] = str(cargo_home)
    env["PATH"] = "/opt/rust/bin:" + env.get("PATH", "/usr/bin:/bin")
    def remaining():
        return max(.001, timeout_seconds - (time.monotonic() - started))
    build = run(["cargo", "test", "--locked", "--offline", "--no-run",
                 "--message-format=json", "--bin", "webagent-v3"],
                remaining(), cwd=str(source), env=env, output_limit=1048576)
    if build["exit_code"] != 0 or build["status"] != "completed":
        build["stdout"] = build["stdout"][-4096:]
        build["stderr"] = build["stderr"][-4096:]
        return {"status": "resource_exhausted" if exhausted() or build["status"] == "timeout" else "build_failed",
                "correctness": "unverified", "build": build, "action": action,
                "resource": "memory" if exhausted() else "runtime" if build["status"] == "timeout" else None}
    executable = None
    for line in build["stdout"].splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if value.get("reason") == "compiler-artifact" and value.get("profile", {}).get("test"):
            executable = value.get("executable") or executable
    if executable is None:
        return {"status": "build_failed", "correctness": "unverified",
                "reason": "native test executable was not emitted", "action": action}
    # Revalidate the complete application correctness suite for every frozen
    # source, separately from timed selected workloads and profiler overhead.
    correctness = run(["cargo", "test", "--locked", "--offline",
                       "--", "--test-threads=1"],
                      remaining(), cwd=str(source), env=env, output_limit=1048576)
    (artifact / "correctness.log").write_text(correctness["stdout"] + correctness["stderr"])
    if source_hash(source) != initial_source_hash:
        return {"status": "failed", "correctness": "failed", "action": action,
                "reason": "correctness suite changed frozen private source inputs",
                "correctness_log": "correctness.log"}
    if correctness["exit_code"] != 0 or correctness["status"] != "completed" or "test result: ok." not in correctness["stdout"]:
        return {"status": "resource_exhausted" if exhausted() else "failed",
                "correctness": "failed", "action": action,
                "resource": "memory" if exhausted() else None,
                "reason": "complete locked correctness suite failed or exhausted budget",
                "correctness_exit_code": correctness["exit_code"],
                "correctness_log": "correctness.log"}
    results = []
    for test in selected_tests(coverage):
        if remaining() <= .001:
            return {"status": "timeout", "correctness": "unverified", "results": results,
                    "action": action}
        argv = [executable, "runtime_tests::" + test, "--exact", "--test-threads=1"]
        if action == "profile_cpu":
            argv = [tool, "--tool=callgrind", "--error-exitcode=97",
                    "--callgrind-out-file=" + str(artifact / (test + ".callgrind"))] + argv
        elif action == "profile_allocations":
            argv = [tool, "-o", str(artifact / (test + ".heaptrack"))] + argv
        result = run(argv, remaining(), cwd=str(source), env=env, output_limit=1024)
        result["test"] = test
        results.append(result)
        # A stale filter that runs zero tests must never claim correctness.
        if result["exit_code"] != 0 or result["status"] != "completed" or "1 passed;" not in result["stdout"]:
            return {"status": "resource_exhausted" if exhausted() else "failed",
                    "correctness": "failed", "results": results,
                    "resource": "memory" if exhausted() else None, "action": action}
    artifacts = []
    if action != "benchmark":
        suffix = "*.callgrind" if action == "profile_cpu" else "*.heaptrack*"
        artifacts = [p.name for p in artifact.glob(suffix) if p.is_file() and p.stat().st_size]
        if len(artifacts) < len(results):
            return {"status": "unsupported", "correctness": "passed", "results": results,
                    "reason": "profiler did not produce complete native artifacts", "action": action}
    summaries = []
    for filename in artifacts:
        if remaining() <= .001:
            return {"status": "timeout", "correctness": "passed", "action": action,
                    "reason": "profile summary exhausted runtime"}
        path = artifact / filename
        if action == "profile_cpu":
            command = ["callgrind_annotate", "--auto=no", "--threshold=90",
                       "--tree=both", str(path)]
        else:
            command = ["heaptrack_print", "-f", str(path), "--peak-limit=3",
                       "--sub-peak-limit=3", "--print-temporary=0"]
        summary = run(command, remaining(), cwd=str(source), env=env, output_limit=65536)
        if summary["exit_code"] != 0 or summary["status"] != "completed":
            return {"status": "unsupported", "correctness": "passed", "action": action,
                    "reason": "compatible profile analyzer failed", "profile_artifacts": artifacts}
        text = summary["stdout"]
        item = {"artifact": filename, "hotspots": text[:8192],
                "summary_truncated": len(text) > 8192 or summary["output_truncated"]}
        if action == "profile_cpu":
            with path.open(errors="replace") as stream:
                for line in stream:
                    if line.startswith("summary:"):
                        parts = line.split()
                        if len(parts) > 1 and parts[1].isdigit():
                            item["instructions"] = int(parts[1])
                        break
            if not item.get("instructions"):
                return {"status": "unsupported", "correctness": "passed", "action": action,
                        "reason": "instruction counters not emitted", "profile_artifacts": artifacts}
            item["metric"] = "instrumented instruction count, not CPU elapsed time"
        else:
            if remaining() <= 0:
                return {"status": "timeout", "correctness": "passed", "action": action,
                        "reason": "allocation totals exhausted runtime"}
            totals = heaptrack_totals(path, remaining(), cwd=str(source), env=env)
            if totals is None:
                return {"status": "unsupported", "correctness": "passed", "action": action,
                        "reason": "allocation summary format unsupported", "profile_artifacts": artifacts}
            item.update(totals)
            item["metric"] = "heap allocations, distinct from plain workload RSS"
        summaries.append(item)
    if source_hash(source) != initial_source_hash:
        return {"status": "failed", "correctness": "failed", "action": action,
                "reason": "measured workloads changed private source inputs"}
    return {"status": "completed", "correctness": "passed", "action": action,
            "coverage": coverage, "priority": priority, "results": results,
            "wall_seconds": sum(x["wall_seconds"] for x in results),
            "cpu_seconds": sum(x["cpu_seconds"] for x in results),
            "peak_rss_bytes": max(x["peak_rss_bytes"] for x in results),
            "profile_artifacts": artifacts, "profile_summaries": summaries,
            "correctness_log": "correctness.log",
            "source_input_hash": initial_source_hash,
            "source_inputs_unchanged": source_hash(source) == initial_source_hash,
            "metric_scope": "plain workload" if action == "benchmark" else "instrumented profile"}
