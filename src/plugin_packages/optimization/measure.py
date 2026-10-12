"""Linux workload measurement inside an already admitted broker container.

This module grants no command authority. The broker freezes the workload and
enforces container CPU, memory and overall task limits before invoking it.
"""
import os
import shutil
import signal
import subprocess
import tempfile
import time


def profiler_availability(kind):
    """Report tooling only; kernel compatibility needs a real profiling run."""
    candidates = {"cpu": ("perf", "valgrind"), "allocation": ("heaptrack",)}
    if kind not in candidates:
        raise ValueError("unknown profiler kind")
    for tool in candidates[kind]:
        path = shutil.which(tool)
        if path:
            return {"available": True, "tool": tool, "path": path,
                    "compatibility": "unverified"}
    return {"available": False, "tool": None, "reason": "compatible tooling not installed"}


def run(argv, timeout_seconds, cancel=None, cwd=None, env=None, output_limit=65536):
    """Reap this specific child with wait4; RSS is Linux KiB converted to bytes.

    Output spools to files to prevent pipe deadlocks and unbounded parent RAM.
    The broker container's storage limit also bounds the spool itself.
    Timeout and revocation kill the entire workload process group.
    """
    if not isinstance(argv, (list, tuple)) or not argv or any(
            not isinstance(x, str) or not x or "\x00" in x for x in argv):
        raise ValueError("argv must be a nonempty string array")
    if not 0 < timeout_seconds <= 600 or not 0 <= output_limit <= 1048576:
        raise ValueError("measurement bounds exceeded")
    if not hasattr(os, "wait4"):
        raise RuntimeError("independent child measurement requires Linux wait4")
    start = time.monotonic()
    status = "completed"
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        child = subprocess.Popen(argv, cwd=cwd, env=env, stdout=out, stderr=err,
                                 start_new_session=True)
        while True:
            pid, wait_status, usage = os.wait4(child.pid, os.WNOHANG)
            if pid:
                # Do not let Popen independently reap this child.
                child.returncode = os.waitstatus_to_exitcode(wait_status)
                break
            cancelled = cancel is not None and cancel()
            if cancelled or time.monotonic() - start >= timeout_seconds:
                status = "cancelled" if cancelled else "timeout"
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                _, wait_status, usage = os.wait4(child.pid, 0)
                child.returncode = os.waitstatus_to_exitcode(wait_status)
                break
            time.sleep(0.01)
        # A successfully exited parent may have left subprocesses behind.
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        result = {"status": status, "exit_code": child.returncode,
                  "wall_seconds": time.monotonic() - start,
                  "cpu_seconds": usage.ru_utime + usage.ru_stime,
                  "peak_rss_bytes": usage.ru_maxrss * 1024}
        truncated = False
        for name, stream in (("stdout", out), ("stderr", err)):
            stream.seek(0)
            data = stream.read(output_limit + 1)
            truncated |= len(data) > output_limit
            result[name] = data[:output_limit].decode("utf-8", errors="replace")
        result["output_truncated"] = truncated
        return result


def main():
    import argparse
    import json
    import pathlib
    from workload import execute
    parser = argparse.ArgumentParser()
    parser.add_argument("--action", choices=("benchmark", "profile_cpu", "profile_allocations"), required=True)
    parser.add_argument("--coverage", choices=("quick", "standard", "extended"), required=True)
    parser.add_argument("--priority", choices=("balanced", "speed", "memory"), required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--timeout-seconds", type=float, default=600)
    args = parser.parse_args()
    if not 0 < args.timeout_seconds <= 600:
        parser.error("timeout must be between zero and 600 seconds")
    output = pathlib.Path(args.output)
    if output.parent != pathlib.Path("/artifacts") or output.name != "result.json":
        parser.error("output must be /artifacts/result.json")
    try:
        result = execute(args.action, args.coverage, args.priority, str(output), args.timeout_seconds)
    except Exception as error:
        result = {"status": "failed", "correctness": "unverified", "reason": str(error)[:1024]}
    encoded = json.dumps(result).encode()
    if len(encoded) > 131072:
        result = {"status": "failed", "correctness": "unverified", "reason": "result exceeded bounded artifact limit"}
        encoded = json.dumps(result).encode()
    temporary = output.with_suffix(".tmp")
    temporary.write_bytes(encoded)
    temporary.replace(output)
    if result["status"] not in ("completed", "unsupported"):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
