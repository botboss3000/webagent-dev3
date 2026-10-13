#!/usr/bin/env python3
"""Codex-side client for the protected V3 development build coordinator."""
import argparse
from pathlib import Path
import os,importlib.util
import json
import socket
import sys
import time
import uuid

spec=importlib.util.spec_from_file_location("build_client_config",Path(__file__).with_name("config.py"))
configuration=importlib.util.module_from_spec(spec);spec.loader.exec_module(configuration)
SOCKET=str(Path(configuration.load()["runtime_root"])/"build.sock")
POLL_SECONDS = 15
MAX_WAIT_SECONDS = 30 * 60
TERMINAL = {"completed", "failed", "stale", "cancelled", "uncertain", "interrupted", "needs_decision"}
PENDING_EXIT = 3
DECISION_EXIT = 4


def call(name, arguments):
    request_id = str(uuid.uuid4())
    request = {"id": request_id, "name": name, "arguments": arguments}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(75)
        connection.connect(SOCKET)
        connection.sendall(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        with connection.makefile("rb") as stream:
            raw = stream.readline(2*1024*1024+1)
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
    finish.add_argument("--expected", required=True, help="inputs_sha256 obtained from inputs")
    finish.add_argument("--sync-decision", choices=("synchronize", "proceed", "cancel"))
    finish.add_argument("--sync-digest", help="exact digest returned by preflight")
    preflight = commands.add_parser("preflight", help="read content-free checkout drift for a task")
    preflight.add_argument("task_id")
    preflight.add_argument("--offset", type=int, default=0)
    decide = commands.add_parser("decide", help="decide an existing paused job; never create a duplicate")
    decide.add_argument("task_id")
    decide.add_argument("sync_decision", choices=("synchronize", "proceed", "cancel"))
    decide.add_argument("--sync-digest", required=True)
    abandon = commands.add_parser("abandon")
    abandon.add_argument("task_id")
    job = commands.add_parser("job")
    job.add_argument("job_id")
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
    args = parser.parse_args()
    if args.command=="schedule":
        value=json.loads(sys.stdin.read(1024*1024)if args.input=="-"else Path(args.input).read_text())
        print(json.dumps(call("build_work",value),sort_keys=True));return 0
    if args.command=="apply":
        patch=Path(args.patch).read_text()
        print(json.dumps(call("v3_source_apply",{"task_id":args.task_id,"request_id":args.request_id,"expected_inputs_sha256":args.expected,"patch":patch}),sort_keys=True));return 0
    if args.command=="maintenance":
        print(json.dumps(call("build_service_maintenance",{"action":args.action,**({"task_id":args.task_id} if args.task_id else {})}),sort_keys=True));return 0
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
    elif args.command == "job":
        result = call("v3_worker_job", {"job_id": args.job_id})
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
