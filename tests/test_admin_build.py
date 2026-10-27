import copy
import sqlite3
import asyncio
import os
import socket
import subprocess
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import threading
import time
import unittest
import uuid
from unittest.mock import patch

PATH = Path(__file__).resolve().parents[1] / "build-service/host.py"
spec = importlib.util.spec_from_file_location("admin_build", PATH)
host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host)


def context(version="one", extra=None):
    files = {"v3/Dockerfile": "FROM test " + version,
             "v3/Cargo.toml": "fixture", "v3/Cargo.lock": "locked",
             "v3/data/config/gates/message_visibility_v1.json": "{}",
             "v3/data/config/gates/closing_status_v1.json": "{}"}
    files.update(extra or {})
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w") as archive:
        for name, body in files.items():
            info = tarfile.TarInfo(name)
            data = body.encode()
            info.size, info.mode = len(data), 0o644
            archive.addfile(info, io.BytesIO(data))
    return stream.getvalue()


class FakeRunner:
    def compiler_runtime(self,*args,**kwargs):
        return {"image":"sha256:"+"7"*64}
    def engine_execution_modes(self,expected_revision=None):
        """Legacy publication fixtures model live execution in both consumers."""
        return {"api":"live","loop":"live"}
    def engine_hot_swappable(self):
        return True
    def build_engine_artifacts(self,job_id,source_root=None):
        """Legacy publication fixtures have no engine component descriptor."""
        return []
    def __init__(self):
        self.blob = context()
        self.source_config=host.CONFIG
        self.secrets = ["NEVER-EXPOSE-THIS"]
        self.calls = []
        self.containers = {name: {"Id": name+"-initial", "Image": "sha256:" + "1" * 64,
                           "State": {"Status": "running", "Health": {"Status": "healthy"}},
                           "Config": {"Env": ["TOKEN=NEVER-EXPOSE-THIS"]}, "HostConfig": {"Memory": 5}}
                           for name in host.TARGETS}
        self.change_during_build = False
        self.low_space = False
        self.block = None
        self.fail_create = False
        self.require_both_started = False
        self.export_entered = None
        self.export_release = None
        self.stop_entered = None
        self.stop_release = None
        self.hot_assets = False
        self.asset_id = None
        self.staged_assets = set()
        self.fail_verify_ui = False

    def check_space(self, minimum):
        self.calls.append(("space", minimum))
        if self.low_space:
            raise ValueError("development build needs more disk space")
        return minimum

    def prepare_build_space(self):
        self.calls.append(("prepare_space", host.MIN_SEQUENCE_FREE))
        return self.check_space(host.MIN_SEQUENCE_FREE)

    def snapshot(self, inspection=False):
        return getattr(self, "inspection_blob", self.blob) if inspection else self.blob

    def inspect(self, name):
        self.calls.append(("inspect", name))
        return copy.deepcopy(self.containers[name])

    def build(self, blob, job, role):
        self.calls.append(("build", role, host.context_digest(blob)[0]))
        if self.block:
            self.block.wait(5)
        if self.change_during_build:
            self.blob = context("changed")
        return "sha256:" + ("a" if role == "api" else "b") * 64, "TOKEN=NEVER-EXPOSE-THIS\nAuthorization: Bearer hidden\n"

    # Simulate candidate-image admission before any consumer replacement effects.
    def preflight_deploy_images(self,images,job_id):
        self.calls.append(("image_preflight",dict(images)))
        if getattr(self,"missing_deploy_role",None):
            raise RuntimeError("Prepared deployment image unavailable")
        return dict(images)

    def export(self, images, destination):
        self.calls.append(("export", images))
        if self.export_entered:
            self.export_entered.set()
            self.export_release.wait(5)
        destination.write_bytes(b"rollback image fixture")

    def stop(self, name):
        self.calls.append(("stop", name))
        self.containers[name]["State"]["Status"] = "exited"
        if self.stop_entered:
            self.stop_entered.set()
            self.stop_release.wait(5)

    def rename(self, name, target):
        self.calls.append(("rename", name, target))
        self.containers[target] = self.containers.pop(name)

    def create(self, name, image, previous, release_root=None, as_name=None):
        self.calls.append(("create", name, image, release_root))
        if self.fail_create:
            raise RuntimeError("TOKEN=NEVER-EXPOSE-THIS create failure")
        self.containers[as_name or name] = copy.deepcopy(previous)
        self.containers[as_name or name]["Image"] = image
        self.containers[as_name or name]["State"]["Status"] = "created"
        if name == host.TARGETS[0] and release_root is not None:
            self.hot_assets = True

    def start(self, name):
        self.calls.append(("start", name))
        self.containers[name]["State"]["Status"] = "running"

    def healthy(self, name, expected):
        self.calls.append(("healthy", name))
        if self.require_both_started:
            started = {call[1] for call in self.calls if call[0] == "start"}
            if not set(host.TARGETS) <= started:
                return False
        return self.containers[name]["Image"] == expected and self.containers[name]["State"]["Status"] == "running"

    def promote_checkout(self, before, after, changed):
        self.calls.append(("promote", tuple(changed)))
        self.blob = after

    def promote_live_ui(self, before, after, changed):
        return False

    def commit_release(self, paths, message):
        self.calls.append(("commit_release", tuple(paths), message))
        return "c" * 40

    def checkout_matches(self, blob, changed):
        return host.release.entries(self.blob, host.context_digest) == host.release.entries(blob, host.context_digest)

    def stage_assets(self, blob, job_id):
        self.calls.append(("stage_assets", job_id))
        self.staged_assets.add(job_id)

    def validate_ui(self, job_id, changed):
        self.calls.append(("validate_ui", tuple(changed)))

    def current_asset_id(self):
        return self.asset_id

    def activate_assets(self, job_id):
        if job_id not in self.staged_assets:
            raise ValueError("unstaged asset fixture")
        self.calls.append(("activate_assets", job_id))
        self.asset_id = job_id

    def ui_hot_swappable(self):
        return self.hot_assets

    def verify_ui(self, changed=()):
        self.calls.append(("verify_ui", tuple(changed)))
        if self.fail_verify_ui:
            raise RuntimeError("UI response mismatch")


class BuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.state = Path(self.temp.name) / "jobs"
        self.runner = FakeRunner()
        self.broker = host.Broker(self.state, self.runner)
        self.workspace = str(uuid.uuid4())
        self.trusted = {"user_id": "fixture-user", "session_id": self.workspace, "lease_until": time.time() + 30, "access_fingerprint": "f" * 64}

    def tearDown(self):
        self.broker.shutdown()
        self.temp.cleanup()

    def call(self, name, args=None, trusted=None, workspace=None):
        return self.broker.handle({"id": "fixture", "name": name, "arguments": args or {}, "timeout_seconds": 60,
                                  "trusted": trusted or self.trusted, "workspace": workspace or self.workspace})

    def build(self, request_id="build-fixture"):
        digest = host.context_digest(self.runner.snapshot())[0]
        return self.call("v3_build_image", {"request_id": request_id, "expected_inputs_sha256": digest})


    def queue_fixture(self, edits, base=None, user="fixture-user", workspace=None):
        job_id = str(uuid.uuid4())
        folder = self.state / job_id
        folder.mkdir()
        baseline = base or context(extra={"v3/README.md": "baseline"})
        after = context(extra=edits)
        (folder / "base.tar").write_bytes(baseline)
        (folder / "task.tar").write_bytes(after)
        job = {"job_id": job_id, "status": "queued", "phase": "waiting",
               "operation": "build_deploy", "isolated": True,
               "created_at": len(self.broker.jobs), "user_id": user,
               "workspace": workspace or self.workspace, "access_fingerprint": "f" * 64}
        self.broker.jobs[job_id] = job
        return job

    def test_queue_preflight_conflicts_identical_edits_and_no_effects(self):
        first = self.queue_fixture({"v3/README.md": "first"})
        second = self.queue_fixture({"v3/README.md": "second"})
        before = copy.deepcopy(self.broker.jobs)
        report = self.broker._view(first)["queue_preflight"]
        self.assertEqual(report["status"], "conflicts")
        self.assertEqual(report["peers"][0]["paths"], ["v3/README.md"])
        self.assertFalse(report["peers"][0]["shared_build_eligible"])
        self.assertEqual(self.broker.jobs, before)
        self.assertEqual(self.runner.calls, [])
        (self.state / second["job_id"] / "task.tar").write_bytes(context(extra={"v3/README.md": "first"}))
        peer = self.broker._queue_preflight(first)["peers"][0]
        self.assertTrue(peer["compatible"])
        self.assertTrue(peer["shared_build_eligible"])

    def test_queue_preflight_disjoint_sequential_and_delete_conflicts(self):
        first = self.queue_fixture({"v3/README.md": "baseline", "v3/src/a.rs": "one"})
        other = self.queue_fixture({"v3/README.md": "baseline", "v3/src/b.rs": "two"})
        self.assertEqual(self.broker._queue_preflight(first)["peers"][0]["overlap_count"], 0)
        self.broker.jobs.clear()
        first = self.queue_fixture({"v3/README.md": "first"})
        sequential = self.queue_fixture({"v3/README.md": "next"},
                                       base=context(extra={"v3/README.md": "first"}))
        self.assertTrue(self.broker._queue_preflight(first)["peers"][0]["compatible"])
        (self.state / first["job_id"] / "task.tar").write_bytes(context())
        self.assertFalse(self.broker._queue_preflight(first)["peers"][0]["compatible"])

    def test_queue_preflight_authority_and_started_build_reasons(self):
        first = self.queue_fixture({"v3/README.md": "first"})
        other = self.queue_fixture({"v3/README.md": "baseline", "v3/src/private.rs": "other"},
                                   user="other-user", workspace=str(uuid.uuid4()))
        self.assertEqual(self.broker._queue_preflight(first)["peer_count"], 0)
        peer = self.broker._queue_preflight(first, administrator=True)["peers"][0]
        self.assertEqual(peer["reason"], "Different caller or authorization")
        other["user_id"], other["workspace"] = first["user_id"], first["workspace"]
        other["status"] = "running"
        self.assertEqual(self.broker._queue_preflight(first)["peers"][0]["reason"], "Build already started")
        other["status"], other["operation"] = "queued", "ui_deploy"
        self.assertEqual(self.broker._queue_preflight(first)["peers"][0]["reason"], "UI releases run separately")

    def test_queue_preflight_bounded_and_unavailable(self):
        first = self.queue_fixture({"v3/README.md": "first"})
        for number in range(24):
            self.queue_fixture({"v3/README.md": "edit-" + str(number)})
        report = self.broker._queue_preflight(first)
        self.assertTrue(report["truncated"])
        self.assertLessEqual(len(host.canonical(report)), 1536)
        (self.state / first["job_id"] / "base.tar").write_bytes(b"invalid archive")
        self.assertEqual(self.broker._queue_preflight(first)["status"], "unavailable")

    def plan_fixture(self):
        task = self.call("v3_build_work", {"action":"begin", "task_id":"plan-task", "summary":"plan", "mode":"isolated"})
        Path(task["workspace_path"], "README.md").write_text("planned edit")
        digest = self.call("v3_build_work", {"action":"status", "task_id":"plan-task"})["own"][0]["workspace_inputs_sha256"]
        return digest

    def test_preflight_reports_the_release_plan_finish_uses(self):
        self.runner.blob = context(extra={"v3/README.md":"released"})
        digest = self.plan_fixture()
        plan = self.call("v3_build_work", {"action":"preflight", "task_id":"plan-task"})["release_plan"]
        self.assertEqual(plan["inputs_sha256"], digest)
        self.assertEqual(plan["changed_paths"], ["v3/README.md"])
        job = self.call("v3_build_work", {"action":"finish", "task_id":"plan-task", "expected_inputs_sha256":digest,
            "expected_paths":["README.md"], "expected_operation":plan["operation"]})
        self.assertEqual(self.broker.jobs[job["job_id"]]["operation"], plan["operation"])

    def test_finish_refuses_a_mismatched_plan_before_any_job(self):
        self.runner.blob = context(extra={"v3/README.md":"released"})
        digest = self.plan_fixture()
        jobs = set(self.broker.jobs)
        for extra, message in (({"expected_paths":["README.md","docs/other.md"]}, "differ from expected_paths"),
                               ({"expected_operation":"host_service_deploy"}, "differs from expected_operation"),
                               ({"expected_paths":[]}, "Bounded expected_paths")):
            with self.assertRaisesRegex(ValueError, message):
                self.call("v3_build_work", {"action":"finish", "task_id":"plan-task", "expected_inputs_sha256":digest, **extra})
        self.assertEqual(set(self.broker.jobs), jobs)
        self.assertEqual("open", self.call("v3_build_work", {"action":"status", "task_id":"plan-task"})["own"][0]["status"])

    def broker_fixture(self, files):
        self.runner.blob = context(extra={"v3/README.md":"released", "v3/build-service/README.md":"broker"})
        task = self.call("v3_build_work", {"action":"begin", "task_id":"broker-task", "summary":"broker", "mode":"isolated"})
        for name, text in files.items():Path(task["workspace_path"], name).write_text(text)
        return self.call("v3_build_work", {"action":"status", "task_id":"broker-task"})["own"][0]["workspace_inputs_sha256"]

    def test_uninstalled_broker_only_change_is_refused_before_any_job(self):
        digest = self.broker_fixture({"build-service/README.md":"broker edit"})
        plan = self.call("v3_build_work", {"action":"preflight", "task_id":"broker-task"})["release_plan"]
        self.assertEqual(plan["operation"], "build_deploy");self.assertTrue(plan["broker_install_required"])
        self.assertIn("Broker-only change is not installed", plan["refusal"])
        jobs = set(self.broker.jobs)
        with self.assertRaisesRegex(ValueError, "Broker-only change is not installed"):
            self.call("v3_build_work", {"action":"finish", "task_id":"broker-task", "expected_inputs_sha256":digest})
        self.assertEqual(set(self.broker.jobs), jobs)
        self.assertEqual("open", self.call("v3_build_work", {"action":"status", "task_id":"broker-task"})["own"][0]["status"])

    def test_mixed_broker_change_builds_and_records_pending_install(self):
        digest = self.broker_fixture({"build-service/README.md":"broker edit", "README.md":"app edit"})
        plan = self.call("v3_build_work", {"action":"preflight", "task_id":"broker-task"})["release_plan"]
        self.assertIsNone(plan["refusal"]);self.assertTrue(plan["broker_install_required"])
        job = self.call("v3_build_work", {"action":"finish", "task_id":"broker-task", "expected_inputs_sha256":digest})
        self.assertEqual(self.broker.jobs[job["job_id"]]["broker_install"]["required"], True)
        self.assertEqual(self.broker.jobs[job["job_id"]]["broker_install"]["paths"], ["v3/build-service/README.md"])

    def drift_fixture(self):
        self.runner.blob = context(extra={"v3/README.md":"released",
                                          "v3/src/release.rs":"must survive"})
        base = self.runner.blob
        seed = self.call("v3_build_work", {"action":"begin", "task_id":"sync-seed", "summary":"seed", "mode":"isolated"})
        Path(seed["workspace_path"],"README.md").write_text("released seed")
        seed_digest=self.call("v3_build_work",dict(action="status",task_id="sync-seed"))["own"][0]["workspace_inputs_sha256"]
        seed_job = self.call("v3_build_work", {"action":"finish", "task_id":"sync-seed",
            "expected_inputs_sha256":seed_digest})
        self.assertEqual("completed", self.wait(seed_job)["status"])
        base = self.runner.blob
        task = self.call("v3_build_work", {"action":"begin", "task_id":"sync-task", "summary":"edit", "mode":"isolated"})
        Path(task["workspace_path"], "README.md").write_text("new task")
        digest = self.call("v3_build_work", {"action":"status", "task_id":"sync-task"})["own"][0]["workspace_inputs_sha256"]
        self.runner.calls.clear()
        return base, digest

    def test_sync_guard_reports_conflicts_without_effects_and_can_cancel(self):
        base, digest = self.drift_fixture()
        self.runner.blob = context(extra={"v3/README.md":"private local work"})
        result = self.call("v3_build_work", {"action":"finish", "task_id":"sync-task", "expected_inputs_sha256":digest})
        self.assertEqual("needs_decision", result["status"])
        report = result["synchronization"]
        self.assertEqual(2, report["total"])
        self.assertEqual(1, report["promotion_conflicts"])
        self.assertEqual(["synchronize","proceed","cancel"], report["choices"])
        self.assertNotIn("private local work", json.dumps(report))
        self.assertFalse(self.runner.calls)
        self.assertEqual("open", self.call("v3_build_work", {"action":"status", "task_id":"sync-task"})["own"][0]["status"])
        choice = dict(action="finish", task_id="sync-task", expected_inputs_sha256=digest,
                      expected_sync_digest=report["digest"], sync_decision="synchronize")
        self.assertEqual("needs_synchronization", self.call("v3_build_work", choice)["status"])
        self.assertFalse(self.runner.calls)
        choice["sync_decision"] = "cancel"
        self.assertEqual("abandoned", self.call("v3_build_work", choice)["status"])
        self.assertFalse(self.runner.calls)

    def test_sync_decision_is_bound_to_checkout_and_task(self):
        base, digest = self.drift_fixture()
        self.runner.blob = context(extra={"v3/README.md":"local"})
        report = self.call("v3_build_work", {"action":"preflight", "task_id":"sync-task"})["synchronization"]
        self.runner.blob = context(extra={"v3/README.md":"changed again"})
        result = self.call("v3_build_work", dict(action="finish", task_id="sync-task",
            expected_inputs_sha256=digest, expected_sync_digest=report["digest"], sync_decision="proceed"))
        self.assertEqual("needs_decision", result["status"])
        self.assertNotEqual(report["digest"], result["synchronization"]["digest"])
        folder = next(i["workspace_path"] for i in self.call("v3_build_work", {"action":"status"})["own"] if i["task_id"]=="sync-task")
        Path(folder,"README.md").write_text("another candidate")
        newer = self.call("v3_build_work", {"action":"preflight", "task_id":"sync-task"})["synchronization"]
        self.assertNotEqual(newer["digest"], result["synchronization"]["digest"])
        self.assertFalse(self.runner.calls)

    def test_sync_proceed_uses_release_and_preserves_other_deployed_files(self):
        base, digest = self.drift_fixture()
        self.runner.blob = context(extra={"v3/README.md":"local"})
        report = self.call("v3_build_work", {"action":"preflight", "task_id":"sync-task"})["synchronization"]
        result = self.call("v3_build_work", dict(action="finish", task_id="sync-task",
            expected_inputs_sha256=digest, expected_sync_digest=report["digest"], sync_decision="proceed"))
        self.assertEqual("completed", self.wait(result)["status"])
        candidate = host.release.entries((self.state/result["job_id"]/"candidate.tar").read_bytes(),host.context_digest)
        self.assertEqual(b"must survive", candidate["v3/src/release.rs"][1])
        self.assertEqual(b"new task", candidate["v3/README.md"][1])

    def test_queued_job_detects_new_drift_and_resumes_same_job(self):
        base, digest = self.drift_fixture()
        self.broker.active = "held"
        result = self.call("v3_build_work",dict(action="finish",task_id="sync-task",expected_inputs_sha256=digest))
        self.runner.blob = context(extra={"v3/README.md":"local"})
        self.broker.active = None
        self.broker._start_next()
        job = self.broker.jobs[result["job_id"]]
        self.assertEqual("needs_decision",job["status"])
        self.assertFalse(self.runner.calls)
        report = self.call("v3_build_work",dict(action="preflight",task_id="sync-task"))["synchronization"]
        stale = self.call("v3_build_work",dict(action="decide",task_id="sync-task",sync_decision="proceed",expected_sync_digest="0"*64))
        self.assertEqual("needs_decision",stale["status"])
        self.runner.blob = base
        report = self.call("v3_build_work",dict(action="preflight",task_id="sync-task"))["synchronization"]
        resumed = self.call("v3_build_work",dict(action="decide",task_id="sync-task",sync_decision="proceed",expected_sync_digest=report["digest"]))
        self.assertEqual(result["job_id"],resumed["job_id"])
        self.assertEqual("completed",self.wait(result)["status"])

    def test_drift_during_build_pauses_before_effects_and_survives_restart(self):
        base, digest = self.drift_fixture()
        self.runner.block = threading.Event()
        result = self.call("v3_build_work",dict(action="finish",task_id="sync-task",expected_inputs_sha256=digest))
        self.runner.blob = context(extra={"v3/README.md":"new local work"})
        self.runner.block.set()
        deadline=time.time()+3
        while self.broker.jobs[result["job_id"]]["status"] in ("running","queued") and time.time()<deadline:time.sleep(.01)
        job=self.broker.jobs[result["job_id"]]
        self.assertEqual("needs_decision",job["status"])
        self.assertFalse(any(c[0] in ("stop","activate_assets","promote") for c in self.runner.calls))
        self.broker.shutdown()
        self.broker=host.Broker(self.state,self.runner)
        self.assertEqual("needs_decision",self.broker.jobs[result["job_id"]]["status"])
        report=self.call("v3_build_work",dict(action="preflight",task_id="sync-task"))["synchronization"]
        cancelled=self.call("v3_build_work",dict(action="decide",task_id="sync-task",sync_decision="cancel",expected_sync_digest=report["digest"]))
        self.assertEqual("cancelled",cancelled["status"])

    def test_sync_report_ignores_acl_masks_but_tracks_executable_status(self):
        base=context()
        entries=host.release.entries(base,host.context_digest)
        acl={name: (0o674,body) for name,(mode,body) in entries.items()}
        acl_blob=host.release.archive(acl,host.context_digest)
        clean=host.release.checkout_drift(base,base,base,(),host.context_digest)
        report=host.release.checkout_drift(base,acl_blob,base,(),host.context_digest)
        self.assertEqual(0,report["total"])
        self.assertEqual(clean["digest"],report["digest"])
        name="v3/Dockerfile"
        acl[name]=(0o774,acl[name][1])
        executable=host.release.archive(acl,host.context_digest)
        self.assertEqual(1,host.release.checkout_drift(base,executable,base,(),host.context_digest)["total"])

    def test_sync_report_is_bounded_and_excludes_runtime_data(self):
        files={"v3/src/f%d.rs"%i:"sensitive contents" for i in range(90)}
        base=context()
        report=host.release.checkout_drift(base,context(extra=files),base,(),host.context_digest)
        page=host.release.checkout_drift(base,context(extra=files),base,(),host.context_digest,40)
        self.assertEqual(90,report["total"])
        self.assertEqual(40,len(report["files"]))
        self.assertEqual(report["digest"],page["digest"])
        self.assertNotIn("sensitive contents",json.dumps(report))
        changed=context(extra={"v3/data/config/gates/closing_status_v1.json":'{"enabled":true}'})
        self.assertEqual(0,host.release.checkout_drift(base,changed,base,(),host.context_digest)["total"])


    def test_paused_batch_keeps_all_task_changes_under_one_decision(self):
        base,digest=self.drift_fixture()
        peer=self.call("v3_build_work",dict(action="begin",task_id="sync-peer",summary="peer",mode="isolated"))
        Path(peer["workspace_path"],"src/peer.rs").write_text("peer survives")
        peer_digest=self.call("v3_build_work",dict(action="status",task_id="sync-peer"))["own"][0]["workspace_inputs_sha256"]
        self.broker.active="held"
        first=self.call("v3_build_work",dict(action="finish",task_id="sync-task",expected_inputs_sha256=digest))
        second=self.call("v3_build_work",dict(action="finish",task_id="sync-peer",expected_inputs_sha256=peer_digest))
        self.runner.block=threading.Event()
        self.broker.active=None
        self.broker._start_next()
        self.runner.blob=context(extra={"v3/README.md":"other local edit"})
        self.runner.block.set()
        deadline=time.time()+3
        while self.broker.active and time.time()<deadline:time.sleep(.01)
        self.assertEqual("needs_decision",self.broker.jobs[first["job_id"]]["status"])
        redirected=self.call("v3_build_work",dict(action="preflight",task_id="sync-peer"))
        self.assertEqual("sync-task",redirected["decision_task_id"])
        report=self.call("v3_build_work",dict(action="preflight",task_id="sync-task"))["synchronization"]
        chosen=self.call("v3_build_work",dict(action="decide",task_id="sync-task",sync_decision="proceed",expected_sync_digest=report["digest"]))
        self.assertEqual(first["job_id"],chosen["job_id"])
        self.assertEqual("completed",self.wait(first)["status"])
        self.assertEqual("completed",self.wait(second)["status"])
        candidate=host.release.entries((self.state/first["job_id"]/"candidate.tar").read_bytes(),host.context_digest)
        self.assertEqual(b"peer survives",candidate["v3/src/peer.rs"][1])
        self.assertEqual(b"new task",candidate["v3/README.md"][1])

    def test_sync_choice_cannot_override_revoked_authority(self):
        base,digest=self.drift_fixture()
        self.broker.active="held"
        result=self.call("v3_build_work",dict(action="finish",task_id="sync-task",expected_inputs_sha256=digest))
        self.runner.blob=context(extra={"v3/README.md":"local"})
        self.broker.active=None;self.broker._start_next()
        job=self.broker.jobs[result["job_id"]];job["revoked"]=True
        report=self.call("v3_build_work",dict(action="preflight",task_id="sync-task"))["synchronization"]
        with self.assertRaisesRegex(PermissionError,"authorization changed"):
            self.call("v3_build_work",dict(action="decide",task_id="sync-task",sync_decision="proceed",expected_sync_digest=report["digest"]))
        self.assertFalse(self.runner.calls)

    def test_live_ui_promotes_only_ui_and_keeps_conflict_check(self):
        root=Path(self.temp.name)/"live-checkout"
        (root/"data/config").mkdir(parents=True)
        (root/"ui").mkdir()
        (root/"data/config/app-settings.json").write_text('{"development":{"live_ui_editing":true}}')
        (root/"ui/app.js").write_text("old")
        (root/"README.md").write_text("old")
        before=context(extra={"v3/ui/app.js":"old","v3/README.md":"old"})
        after=context(extra={"v3/ui/app.js":"new","v3/README.md":"new"})
        runner=host.Runner(source=root)
        self.assertTrue(runner.promote_live_ui(before,after,["v3/ui/app.js","v3/README.md"]))
        self.assertEqual("new",(root/"ui/app.js").read_text())
        self.assertEqual("old",(root/"README.md").read_text())
        (root/"ui/app.js").write_text("another writer")
        with self.assertRaisesRegex(ValueError,"checkout changed"):
            runner.promote_live_ui(before,after,["v3/ui/app.js"])
        self.assertEqual("another writer",(root/"ui/app.js").read_text())

    def test_proceed_does_not_overwrite_promotion_conflict(self):
        before=context(extra={"v3/README.md":"old"})
        after=context(extra={"v3/README.md":"new"})
        root=Path(self.temp.name)/"checkout";root.mkdir();(root/"README.md").write_text("other writer")
        with self.assertRaisesRegex(ValueError,"checkout changed"):
            host.release.promote_checkout(root,before,after,["v3/README.md"],host.context_digest,standalone=True)
        self.assertEqual("other writer",(root/"README.md").read_text())

    def test_dashboard_is_admin_scoped_and_reports_confirmed_milestones(self):
        job_id = self.build()["job_id"]
        terminal = self.wait({"job_id": job_id})
        self.assertEqual("completed", terminal["status"])
        dashboard = {"user_id": "admin", "session_id": "00000000-0000-0000-0000-000000000000",
                     "lease_until": time.time() + 30, "access_fingerprint": "a" * 64}
        view = self.call("v3_build_dashboard", trusted=dashboard,
                         workspace=dashboard["session_id"])
        self.assertEqual(job_id, view["jobs"][0]["job_id"])
        self.assertEqual("passed", view["jobs"][0]["rust_tests"]["status"])
        self.broker.jobs[job_id].pop("rust_tests")
        legacy = self.call("v3_build_dashboard", trusted=dashboard,
                           workspace=dashboard["session_id"])
        self.assertEqual("passed", legacy["jobs"][0]["rust_tests"]["status"])
        self.assertIn("detailed test output was not retained", legacy["jobs"][0]["rust_tests"]["summary"])
        self.assertIn("building_api", [item["phase"] for item in view["jobs"][0]["phase_history"]])
        self.assertEqual("healthy", view["containers"]["api"]["health"])
        self.assertNotIn("NEVER-EXPOSE-THIS", json.dumps(view))
        self.assertNotIn("access_fingerprint", view["jobs"][0])
        result = self.call("v3_build_result", {"job_id": job_id}, trusted=dashboard,
                           workspace=dashboard["session_id"])["job"]
        self.assertEqual(job_id, result["job_id"])
        self.assertEqual("passed", result["rust_tests"]["status"])
        self.assertIn("building_api", [item["phase"] for item in result["phase_history"]])
        self.assertNotIn("access_fingerprint", result)
        self.assertNotIn("NEVER-EXPOSE-THIS", json.dumps(result))
        self.assertIsNone(self.call("v3_build_result", {"job_id": str(uuid.uuid4())},
                                    trusted=dashboard, workspace=dashboard["session_id"])["job"])
        with self.assertRaises(PermissionError):
            self.call("v3_build_result", {"job_id": job_id},
                      trusted=self.trusted, workspace=self.workspace)
        with self.assertRaises(PermissionError):
            self.call("v3_build_dashboard", trusted=self.trusted, workspace=self.workspace)
        with self.assertRaises(PermissionError):
            self.call("v3_build_dashboard", {"unexpected": True}, trusted=dashboard,
                      workspace=dashboard["session_id"])

    def test_progress_reports_stage_against_class_history_without_logs(self):
        def receipt(job_id, status, start, steps, phases, finished=None):
            return {"job_id": job_id, "status": status, "operation": "build_deploy", "user_id": self.trusted["user_id"],
                    "workspace": self.workspace, "created_at": start, "started_at": start, "finished_at": finished,
                    "log": "private build output", "phase_timings": [{"phase": p, "seconds": 120} for p, *_ in steps],
                    "execution_history": [{"phase": p, "started_at": start + t, "elapsed_seconds": d} for p, t, d in steps],
                    "phase_history": [{"phase": p, "at": start + t} for p, t in phases]}
        steps = [("dependency", 0, 10), ("compile_test", 10, 290), ("test", 300, 600), ("compile_linked", 900, 400), ("compile_release", 1300, 400)]
        phases = [("building_api", 0), ("building_loop", 1700), ("frontend_tests", 1800)]
        for i in range(5):
            self.broker.jobs[f"done-{i}"] = receipt(f"done-{i}", "completed", 1000.0 * i, steps, phases, 1000.0 * i + 1900)
        now = time.time()
        self.broker.jobs["live"] = dict(receipt("live", "running", now - 400, steps[:2], phases[:1]),
            execution_observation={"phase": "test", "elapsed_seconds": 100})
        value = self.call("v3_build_progress", {"job_id": "live"})
        self.assertEqual((value["class"], value["stage"], value["stage_index"]), ("build_deploy/compile/tests-ran", "run tests", 3))
        self.assertEqual(value["stage_normal"], [600, 600])
        self.assertEqual([d["stage"] for d in value["finished_stages"]], ["dependencies", "compile tests"])
        self.assertTrue(value["remaining_seconds"][0] > 0 and value["verdict"] == "on track")
        self.assertNotIn("private build output", json.dumps(value))
        done = self.call("v3_build_progress", {"job_id": "done-0"})
        self.assertEqual((done["job_seconds"], done["remaining_seconds"]), (1900, [0, 0]))
        other = dict(self.trusted, user_id="someone-else")
        with self.assertRaises(ValueError):
            self.call("v3_build_progress", {"job_id": "live"}, trusted=other, workspace=self.workspace)
        with self.assertRaises(ValueError):
            self.call("v3_build_progress", {"job_id": "live", "extra": 1})

    def test_progress_shows_overlapping_phases_and_files_late_steps_under_their_phase(self):
        now = 10000.0
        job = {"job_id": "overlap", "status": "running", "operation": "build_deploy", "started_at": now - 600,
               "phase_history": [{"phase": "building_api", "at": now - 600}],
               "execution_history": [{"phase": "compile_test", "started_at": now - 590, "elapsed_seconds": 200}],
               "active_phases": [{"id": "a", "phase": "test", "recipe": "engine:rust1.90:test", "started_at": now - 380},
                                 {"id": "b", "phase": "compile_linked", "recipe": "engine:rust1.90:linked-release", "started_at": now - 380}],
               "execution_observation": {"phase": "test", "elapsed_seconds": 380}}
        value = host.job_progress(job, {}, now)
        self.assertEqual([r["stage"] for r in value["running_stages"]], ["run tests", "linked release build"])
        self.assertEqual((value["stage"], value["stage_seconds"]), ("linked release build", 380))
        self.assertEqual([d["stage"] for d in value["finished_stages"]], ["compile tests"])
        finished = {"job_id": "plugins", "status": "completed", "operation": "build_deploy", "started_at": 0, "finished_at": 400,
                    "phase_history": [{"phase": "building_api", "at": 0}, {"phase": "preparing_plugins", "at": 100}, {"phase": "frontend_tests", "at": 300}],
                    "execution_history": [{"phase": "dependency", "started_at": 1, "elapsed_seconds": 9},
                                          {"phase": "dependency", "started_at": 150, "elapsed_seconds": 5},
                                          {"phase": "plugin_compile", "started_at": 160, "elapsed_seconds": 20}],
                    "active_phases": [{"id": "stale", "phase": "test", "started_at": 5}]}
        spans = host.stage_spans(finished)
        self.assertEqual(spans["dependencies"], [1, 10])
        self.assertEqual(spans["prepare plugins"], [100, 300])
        self.assertNotIn("run tests", spans)

    def test_pending_journal_scan_sees_new_and_rewritten_records(self):
        scheduler = self.broker.scheduler
        folder = self.broker.work_dir / "scheduler-requests"; folder.mkdir(parents=True, exist_ok=True)
        for i in range(3):
            scheduler.write_record(folder / f"{i:064x}.json", {"state": "done", "task_key": "k", "request_id": f"done-{i}"})
        self.assertIsNone(scheduler.pending("k"))
        path = folder / ("f" * 64 + ".json")
        scheduler.write_record(path, {"state": "pending", "task_key": "k", "request_id": "retry-me"})
        self.assertEqual(scheduler.pending("k"), "retry-me")
        self.assertIsNone(scheduler.pending("k", except_path=path))
        scheduler.write_record(path, {"state": "done", "task_key": "k", "request_id": "retry-me"})
        self.assertIsNone(scheduler.pending("k"))
        path.unlink(); self.assertIsNone(scheduler.pending("k"))

    def wait(self, job):
        for _ in range(100):
            value = self.call("v3_worker_job", {"job_id": job["job_id"]})
            if value["status"] not in ("queued", "running"):
                # _run finally releases the busy marker after durable result.
                while self.broker.active:
                    time.sleep(.001)
                return value
            time.sleep(.01)
        self.fail("job did not finish")

    def deploy(self, source_job, request_id="deploy-fixture"):
        return self.call("v3_worker_submit", {"operation": "deploy", "job_id": source_job["job_id"],
                "request_id": request_id, "expected_inputs_sha256": source_job["inputs_sha256"]})

    def test_checked_patch_changes_nested_task_workspace(self):
        root = self.state / "nested-repo"
        root.mkdir()
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        source = root / "v3/build-jobs/coordination/trees/task/v3"
        source.mkdir(parents=True)
        target = source / "README.md"
        target.write_text("before\n")
        patch_text = ("diff --git a/README.md b/README.md\n"
                      "--- a/README.md\n+++ b/README.md\n"
                      "@@ -1 +1 @@\n-before\n+after\n")
        with patch.object(host, "SOURCE", root):
            paths, corrected = host.inspect_source_patch_corrected(source, patch_text + "+ignored trailing line\n")
            self.assertEqual(["README.md"], paths)
            self.assertIn("@@ -1,1 +1,2 @@", corrected)
            self.assertIn("+ignored trailing line\n", corrected)
            self.assertEqual("before\n", target.read_text())
            self.assertEqual(["README.md"], host.inspect_source_patch(source, patch_text))
            host.source_git(source, ["apply", "--whitespace=error", "-"], patch_text.encode())
        self.assertEqual("after\n", target.read_text())
        with patch.object(host, "SOURCE", root), self.assertRaisesRegex(ValueError, "README.md:1"):
            host.source_git(source, ["apply", "--check", "-"], patch_text.encode())

    def test_checked_patch_explains_apply_patch_format_error(self):
        root = self.state / "format-repo"
        root.mkdir()
        source = root / "v3"
        source.mkdir()
        with patch.object(host, "SOURCE", root):
            with self.assertRaisesRegex(ValueError, "apply_patch markers are unsupported"):
                host.inspect_source_patch(source, "*** Begin Patch\n*** Add File: ui/page.json\n+{}\n*** End Patch\n")
            with self.assertRaisesRegex(ValueError, "must start with diff --git"):
                host.inspect_source_patch(source, "--- /dev/null\n+++ b/ui/page.json\n")

    def test_checked_patch_accepts_exact_middle_line_without_context(self):
        root = self.state / "zero-context-repo"
        root.mkdir()
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        source = root / "v3/build-jobs/coordination/trees/task/v3"
        target = source / "ui/app.js"
        target.parent.mkdir(parents=True)
        target.write_text("before\nconst note=text=>1;\nafter\n")
        patch_text = ("diff --git a/ui/app.js b/ui/app.js\n"
                      "--- a/ui/app.js\n+++ b/ui/app.js\n"
                      "@@ -2,1 +2,1 @@\n-const note=text=>1;\n+const note=text=>2;\n")
        stale_text = patch_text.replace("const note=text=>1;", "const note=text=>0;")
        with patch.object(host, "SOURCE", root):
            with self.assertRaisesRegex(ValueError, "patch does not apply"):
                host.inspect_source_patch(source, stale_text)
            self.assertEqual(["ui/app.js"], host.inspect_source_patch(source, patch_text))
            host.source_git(source, ["apply", "--unidiff-zero", "--whitespace=error", "-"],
                            patch_text.encode())
        self.assertEqual("before\nconst note=text=>2;\nafter\n", target.read_text())

    def test_checked_patch_accepts_binary_ui_assets_only(self):
        root = self.state / "binary-repo"
        root.mkdir()
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        source = root / "v3/build-jobs/coordination/trees/task/v3"
        source.mkdir(parents=True)
        scratch = self.state / "binary-scratch"
        scratch.mkdir()
        subprocess.run(["git", "init", "-q", str(scratch)], check=True)
        data = b"\x89PNG\r\n\x1a\n\0" + bytes(range(256)) * 4

        def binary_patch(name):
            target = scratch / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            subprocess.run(["git", "-C", str(scratch), "add", name], check=True)
            text = subprocess.run(["git", "-C", str(scratch), "diff", "--cached", "--binary", "--", name],
                                  check=True, capture_output=True, text=True).stdout
            subprocess.run(["git", "-C", str(scratch), "rm", "-q", "--cached", name], check=True)
            return text

        asset = binary_patch("ui/og-card.png")
        self.assertIn("GIT binary patch", asset)
        with patch.object(host, "SOURCE", root):
            self.assertEqual(["ui/og-card.png"], host.inspect_source_patch(source, asset))
            host.source_git(source, ["apply", "--unidiff-zero", "--whitespace=error", "-"], asset.encode())
            self.assertEqual(data, (source / "ui/og-card.png").read_bytes())
            for name in ("src/og-card.png", "ui/og-card.bin"):
                with self.assertRaisesRegex(ValueError, "limited to ui/ image and font assets"):
                    host.inspect_source_patch(source, binary_patch(name))
            with self.assertRaisesRegex(ValueError, "binary or move patch denied"):
                host.inspect_source_patch(source, "diff --git a/ui/x.png b/ui/x.png\n"
                                          "new file mode 100644\nBinary files /dev/null and b/ui/x.png differ\n")

    def test_loop_live_plugin_mount_preserves_runtime_and_readonly_source(self):
        previous = {"ReadonlyRootfs": True, "Memory": 768 * 1024 * 1024,
                    "Mounts": [{"Type": "bind", "Source": "/state", "Target": "/state"},
                               {"Type": "bind", "Source": "/frozen", "Target": "/live-v3",
                                "ReadOnly": True}]}
        original = copy.deepcopy(previous)
        mounted = host.loop_plugin_mounts(previous)
        self.assertEqual(original, previous)
        self.assertTrue(mounted["ReadonlyRootfs"])
        self.assertEqual(previous["Memory"], mounted["Memory"])
        self.assertEqual(previous["Mounts"], mounted["Mounts"][:-1])
        self.assertEqual({"Type": "bind", "Source": str(host.SOURCE / "plugins"),
                          "Target": "/live-checkout/plugins", "ReadOnly": True},
                         mounted["Mounts"][-1])
        self.assertFalse(any(m["Target"] == "/live-checkout" for m in mounted["Mounts"]))

    def test_loop_live_plugin_mount_replaces_both_inherited_forms(self):
        previous = {"Mounts": [{"Type": "bind", "Source": "/wrong",
                               "Target": "/live-checkout/plugins", "ReadOnly": False}],
                    "Binds": ["/wrong:/live-checkout/plugins:rw", "/state:/state:rw"]}
        first = host.loop_plugin_mounts(previous)
        second = host.loop_plugin_mounts(first)
        self.assertEqual(first, second)
        self.assertEqual(["/state:/state:rw"], second["Binds"])
        self.assertEqual(1, len(second["Mounts"]))
        self.assertTrue(second["Mounts"][0]["ReadOnly"])

    def test_loop_release_mounts_include_read_only_active_guidance(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder) / "release"
            (root / "v3/plugins/abilities").mkdir(parents=True)
            assets = Path(folder) / "assets"
            assets.mkdir()
            old = {"Mounts": [{"Type": "bind", "Source": "/old/abilities",
                              "Target": "/opt/v3/plugins/abilities", "ReadOnly": True},
                             {"Type": "bind", "Source": "/state",
                              "Target": "/state", "ReadOnly": False}]}
            mounted = host.release.release_mounts(old, root, assets)["Mounts"]
            active = [m for m in mounted if m["Target"] == "/opt/v3/assets"]
            self.assertEqual(len(active), 1)
            self.assertEqual(active[0]["Source"], str(assets))
            self.assertTrue(active[0]["ReadOnly"])
            again = host.release.release_mounts({"Mounts": mounted}, root, assets)["Mounts"]
            self.assertEqual(again, mounted)
            self.assertFalse(next(m for m in mounted if m["Target"] == "/state")["ReadOnly"])

    def test_release_mounts_pin_source_and_keep_state_writable(self):
        root = self.state / "release-fixture"
        for name in ("v3/ui", "v3/config", "v3/plugins/abilities",
                     "ui/shared/css", "ui/main-panel/agents", "ui/vendor/fonts",
                     "ui/vendor/lucide"):
            (root / name).mkdir(parents=True)
        (root / "ui/favicon.svg").write_text("svg")
        api = {"Mounts": [
            {"Type": "bind", "Source": "/old", "Target": "/live-v3"},
            {"Type": "bind", "Source": "/checkout", "Target": "/live-checkout"},
            {"Type": "bind", "Source": "/old/ui", "Target": "/opt/v3/ui"},
            {"Type": "bind", "Source": "/state", "Target": "/state"}]}
        mounted = host.release.release_mounts(api, root)["Mounts"]
        by_target = {item["Target"]: item for item in mounted}
        self.assertEqual(str(root / "v3"), by_target["/live-v3"]["Source"])
        self.assertTrue(by_target["/live-v3"]["ReadOnly"])
        self.assertEqual("/checkout", by_target["/live-checkout"]["Source"])
        self.assertEqual("/state", by_target["/state"]["Source"])
        loop = {"Mounts": [{"Type": "bind", "Source": "/old/abilities",
                            "Target": "/opt/v3/plugins/abilities"}]}
        target = host.release.release_mounts(loop, root)["Mounts"][0]
        self.assertEqual(str(root / "v3/plugins/abilities"), target["Source"])

    def test_index_gate_failure_blocks_full_and_ui_effects(self):
        for ui_only in (False, True):
            with self.subTest(ui_only=ui_only):
                self.runner.hot_assets=ui_only
                self.runner.blob=context(extra={"v3/ui/index.html":"<main>baseline</main>"})
                task_id="index-gate-"+str(ui_only)
                task=self.call("v3_build_work",dict(action="begin",task_id=task_id,summary="gate",mode="isolated"))
                path=Path(task["workspace_path"])/("ui/index.html" if ui_only else "README.md")
                path.write_text("<main>edited</main>" if ui_only else "runtime edit")
                digest=self.call("v3_build_work",dict(action="status",task_id=task_id))["own"][0]["workspace_inputs_sha256"]
                self.runner.calls.clear()
                with patch.object(self.runner,"stage_assets",side_effect=ValueError("incomplete published index")):
                    job=self.call("v3_build_work",dict(action="finish",task_id=task_id,expected_inputs_sha256=digest))
                    result=self.wait(job)
                self.assertEqual("failed",result["status"])
                self.assertIn("incomplete published index",result["reason"])
                self.assertFalse(any(c[0] in ("stop","activate_assets","promote") for c in self.runner.calls))

    def test_stable_asset_mount_and_atomic_pointer(self):
        root = self.state / "asset-fixture"
        blob = context(extra={"v3/ui/index.html": "<main>ready</main>",
                              "v3/ui/app.js": "export {}",
                              "ui/shared/css/app1.css": "body {}"})
        first = str(uuid.uuid4())
        second = str(uuid.uuid4())
        release = host.release
        release.stage_assets(root, first, blob, host.context_digest)
        scanner_path=PATH.parent.parent/"plugins/abilities/Administrator/code_index/adapter.py"
        scanner_spec=importlib.util.spec_from_file_location("fixture_scanner",scanner_path)
        scanner=importlib.util.module_from_spec(scanner_spec);scanner_spec.loader.exec_module(scanner)
        scanner.build_published(root/"releases"/first,root/"releases"/first/".code-index",
                                first,host.context_digest(blob)[0])
        self.assertIsNone(release.current_asset_id(root))
        self.assertIsNone(release.activate_assets(root, first))
        self.assertEqual(first, release.current_asset_id(root))
        release.stage_assets(root, second, blob, host.context_digest)
        scanner.build_published(root/"releases"/second,root/"releases"/second/".code-index",
                                second,host.context_digest(blob)[0])
        self.assertEqual(first, release.activate_assets(root, second))
        self.assertEqual(second, release.current_asset_id(root))
        release.stage_assets(root, second, blob, host.context_digest)
        (root / "releases" / second / "v3/ui/app.js").write_text("changed")
        with self.assertRaisesRegex(ValueError, "frozen UI asset"):
            release.stage_assets(root, second, blob, host.context_digest)

        snapshot = self.state / "release-fixture"
        (snapshot / "v3").mkdir(parents=True)
        api = {"Mounts": [
            {"Type": "bind", "Source": "/old", "Target": "/live-v3"},
            {"Type": "bind", "Source": "/checkout", "Target": "/live-checkout"},
            {"Type": "bind", "Source": "/old/ui", "Target": "/opt/v3/ui"},
            {"Type": "bind", "Source": "/state", "Target": "/state"}]}
        mounted = release.release_mounts(api, snapshot, root)["Mounts"]
        targets = {item["Target"]: item for item in mounted}
        self.assertNotIn("/opt/v3/ui", targets)
        self.assertEqual(str(root), targets["/opt/v3/assets"]["Source"])
        self.assertTrue(targets["/opt/v3/assets"]["ReadOnly"])
        self.assertEqual("/state", targets["/state"]["Target"])

    def test_checkout_promotion_checks_conflicts_and_is_idempotent(self):
        root = self.state / "checkout"
        path = root / "v3/README.md"
        path.parent.mkdir(parents=True)
        path.write_text("before")
        before = context(extra={"v3/README.md": "before"})
        after = context(extra={"v3/README.md": "after"})
        promote = host.release.promote_checkout
        self.assertEqual(1, promote(root, before, after, ["v3/README.md"], host.context_digest))
        self.assertEqual("after", path.read_text())
        self.assertEqual(0, promote(root, before, after, ["v3/README.md"], host.context_digest))
        path.write_text("unrelated edit")
        with self.assertRaisesRegex(ValueError, "checkout changed during release"):
            promote(root, before, after, ["v3/README.md"], host.context_digest)
        self.assertEqual("unrelated edit", path.read_text())

    def test_standalone_checkout_promotion_preserves_conflicts(self):
        root = self.state / "standalone"
        root.mkdir()
        path = root / "README.md"
        path.write_text("before")
        before = context(extra={"v3/README.md": "before"})
        after = context(extra={"v3/README.md": "after"})
        release = host.release
        self.assertEqual(1, release.promote_checkout(root, before, after,
            ["v3/README.md"], host.context_digest, standalone=True))
        self.assertEqual("after", path.read_text())
        self.assertTrue(release.checkout_matches(root, after,
            ["v3/README.md"], host.context_digest, standalone=True))
        path.write_text("another writer")
        with self.assertRaisesRegex(ValueError, "checkout changed"):
            release.promote_checkout(root, before, after,
                ["v3/README.md"], host.context_digest, standalone=True)
        self.assertEqual("another writer", path.read_text())

    def test_source_apply_schemas_have_disjoint_arguments(self):
        path = Path(__file__).resolve().parents[1] / "src/engine/tools/native-tools.json"
        schemas = {item["name"]: item["parameters"] for item in json.loads(path.read_text())}
        prepared = schemas["v3_source_apply_prepared"]
        raw = schemas["v3_source_apply"]
        self.assertEqual({"task_id", "request_id", "prepared_id", "expected_inputs_sha256"},
                         set(prepared["properties"]))
        self.assertEqual(set(prepared["properties"]), set(prepared["required"]))
        self.assertEqual({"task_id", "request_id", "patch", "expected_inputs_sha256"},
                         set(raw["properties"]))
        self.assertEqual(set(raw["properties"]), set(raw["required"]))

    def test_prepared_new_files_apply_exact_reviewed_patch(self):
        task = self.call("v3_build_work", {"action": "begin", "task_id": "prepare-task",
            "summary": "Prepare", "mode": "isolated"})
        source = Path(task["workspace_path"])
        files = [
            {"path": "ui/main-panel/session-metrics/page.json",
             "content": '{"id":"session-metrics"}\n'},
            {"path": "ui/main-panel/session-metrics/page.css",
             "content": ".table { overflow: auto; }\n.more { color: red; }\n"},
        ]
        prepared = self.call("v3_source_prepare", {"task_id": "prepare-task",
            "files": files, "expected_inputs_sha256": task["workspace_inputs_sha256"]})
        self.assertIn("@@ -0,0 +1,2 @@", prepared["patch"])
        self.assertEqual(64, len(prepared["patch_sha256"]))
        self.assertFalse((source / files[0]["path"]).exists())
        both = {"task_id": "prepare-task", "request_id": "bad-both",
                "prepared_id": prepared["prepared_id"], "patch": prepared["patch"],
                "expected_inputs_sha256": task["workspace_inputs_sha256"]}
        for tool in ("v3_source_apply", "v3_source_apply_prepared"):
            with self.assertRaisesRegex(ValueError, "arguments do not match tool"):
                self.call(tool, both)
        self.assertFalse((source / files[0]["path"]).exists())
        result = self.call("v3_source_apply_prepared", {"task_id": "prepare-task",
            "request_id": "prepared-apply-1", "prepared_id": prepared["prepared_id"],
            "expected_inputs_sha256": task["workspace_inputs_sha256"]})
        self.assertEqual("success", result["status"])
        self.assertEqual(files[0]["content"], (source / files[0]["path"]).read_text())
        self.assertEqual(files[1]["content"], (source / files[1]["path"]).read_text())
        self.assertEqual(result, self.call("v3_source_receipt", {"task_id": "prepare-task",
            "request_id": "prepared-apply-1"}))
        self.assertEqual(result, self.call("v3_source_apply_prepared", {"task_id": "prepare-task",
            "request_id": "prepared-apply-1", "prepared_id": prepared["prepared_id"],
            "expected_inputs_sha256": task["workspace_inputs_sha256"]}))
        with self.assertRaisesRegex(ValueError, "stale source inputs"):
            self.call("v3_source_apply_prepared", {"task_id": "prepare-task",
                "request_id": "prepared-apply-2", "prepared_id": prepared["prepared_id"],
                "expected_inputs_sha256": task["workspace_inputs_sha256"]})

    def test_prepare_rejects_unsafe_or_existing_source(self):
        task = self.call("v3_build_work", {"action": "begin", "task_id": "prepare-deny",
            "summary": "Prepare", "mode": "isolated"})
        digest = task["workspace_inputs_sha256"]
        for name, content in [("../data/secret", "x\n"), ("data/secret", "x\n"),
                              ("ui/page.json", "missing newline")]:
            with self.assertRaises(ValueError):
                self.call("v3_source_prepare", {"task_id": "prepare-deny",
                    "files": [{"path": name, "content": content}],
                    "expected_inputs_sha256": digest})
        with self.assertRaisesRegex(ValueError, "not a regular source file|patch does not apply|new files"):
            self.call("v3_source_prepare", {"task_id": "prepare-deny",
                "files": [{"path": "Cargo.toml", "content": "replacement\n"}],
                "expected_inputs_sha256": digest})

    def test_hunk_errors_name_file_and_actual_count(self):
        bad = ("diff --git a/ui/main-panel/session-metrics/page.html "
               "b/ui/main-panel/session-metrics/page.html\n"
               "new file mode 100644\n--- /dev/null\n"
               "+++ b/ui/main-panel/session-metrics/page.html\n"
               "@@ -0,0 +1,38 @@\n+one\n+two\n")
        corrected = host.validate_source_hunks(bad)
        self.assertIn("@@ -0,0 +1,2 @@", corrected)
        self.assertTrue(corrected.endswith("+one\n+two\n"))

    def test_isolated_source_list_and_read_errors(self):
        task = self.call("v3_build_work", {"action": "begin", "task_id": "list-task",
            "summary": "List", "mode": "isolated"})
        source = Path(task["workspace_path"])
        page = source / "ui/main-panel/builds/page.json"
        page.parent.mkdir(parents=True)
        page.write_text("{}\n")
        root = self.call("v3_source_list", {"task_id": "list-task", "limit": 2})
        self.assertIn("ui", [entry["path"] for entry in root["entries"]] +
                      [entry["path"] for entry in self.call("v3_source_list",
                        {"task_id": "list-task", "offset": 2})["entries"]])
        listed = self.call("v3_source_list", {"task_id": "list-task",
            "path": "ui/main-panel/builds"})
        self.assertEqual([{"name": "page.json", "path": "ui/main-panel/builds/page.json",
                           "kind": "file"}], listed["entries"])
        self.assertEqual(self.call("v3_source_read", {"task_id": "list-task",
            "path": "ui/main-panel/builds/page.json"})["workspace_inputs_sha256"],
            listed["workspace_inputs_sha256"])
        with self.assertRaisesRegex(ValueError, "use v3_source_list"):
            self.call("v3_source_read", {"task_id": "list-task", "path": "ui/main-panel"})
        with self.assertRaisesRegex(ValueError, "does not exist"):
            self.call("v3_source_read", {"task_id": "list-task", "path": "ui/main-panel/index.html"})
        for path in ("../ui", "data", "ui/../Cargo.toml"):
            with self.assertRaisesRegex(ValueError, "denied"):
                self.call("v3_source_list", {"task_id": "list-task", "path": path})
        with self.assertRaisesRegex(ValueError, "open isolated task"):
            self.call("v3_source_list", {"task_id": "another-task", "path": "ui"})
        with self.assertRaisesRegex(ValueError, "not a directory"):
            self.call("v3_source_list", {"task_id": "list-task", "path": "ui/main-panel/builds/page.json"})

    def test_admin_inspection_text_export_attestation_and_release_exclusion(self):
        files = host.release.entries(self.runner.blob, host.context_digest)
        for directory, filename in (("data/user_data/admin", "admin.db"), ("data/agent_data/shared_default", "shared_default.db")):
            db_path = Path(self.temp.name) / filename
            with sqlite3.connect(db_path) as db:
                db.execute("CREATE TABLE docs (key TEXT, value BLOB)")
                envelope = json.dumps({"format":"v3-admin-plaintext-v1", "value":{"fixture":"usable plaintext"}}).encode()
                db.execute("INSERT INTO docs VALUES (?,?)", ("config:fixture", envelope))
                db.execute("INSERT INTO docs VALUES (?,?)", ("sealed", b"ciphertext\x00fixture"))
            files["v3/" + directory + "/" + filename] = (0o440, db_path.read_bytes())
        self.runner.inspection_blob = host.release.archive(files, host.context_digest)
        admin = {**self.trusted, "user_id": "admin"}
        begun = self.call("v3_build_work", {"action":"begin", "task_id":"inspect-task", "summary":"Inspect", "mode":"isolated"}, trusted=admin)
        source = Path(begun["workspace_path"])
        for directory, filename in (("data/user_data/admin", "admin.db"), ("data/agent_data/shared_default", "shared_default.db")):
            listed = self.call("v3_source_list", {"task_id":"inspect-task", "path":directory}, trusted=admin)
            self.assertEqual(filename, listed["entries"][0]["name"])
            first = self.call("v3_source_read", {"task_id":"inspect-task", "path":directory+"/"+filename, "limit":2}, trusted=admin)
            self.assertEqual("sqlite-text-export", first["format"])
            self.assertTrue(first["has_more"])
            rest = self.call("v3_source_read", {"task_id":"inspect-task", "path":directory+"/"+filename, "offset":first["next_offset"]}, trusted=admin)
            self.assertIn("usable plaintext", "\n".join(rest["lines"]))
            self.assertIn("base64", "\n".join(rest["lines"]))
            patch_text = f"diff --git a/{directory}/note.txt b/{directory}/note.txt\nnew file mode 100644\n--- /dev/null\n+++ b/{directory}/note.txt\n@@ -0,0 +1 @@\n+write denied\n"
            with self.assertRaisesRegex(ValueError, "path denied"):
                self.call("v3_source_apply", {"task_id":"inspect-task", "request_id":"deny-"+filename.replace(".", "_"), "expected_inputs_sha256":begun["workspace_inputs_sha256"], "patch":patch_text}, trusted=admin)
            for tool,args in (("v3_source_prepare", {"files":[{"path":directory+"/note.txt", "content":"write denied\n"}]}),):
                with self.assertRaisesRegex(ValueError, "path denied"):
                    self.call(tool, {"task_id":"inspect-task", "expected_inputs_sha256":begun["workspace_inputs_sha256"], **args}, trusted=admin)
        for denied in ("data", "data/user_data", "data/user_data/other", "data/agent_data", "data/agent_data/shared_default_other", "data/encryption.key", "data/runtime.sqlite"):
            for tool in ("v3_source_list", "v3_source_read"):
                with self.assertRaisesRegex(ValueError, "path denied"):
                    self.call(tool, {"task_id":"inspect-task", "path":denied}, trusted=admin)
        (source / "src").mkdir(exist_ok=True)
        (source / "src/binary.rs").write_bytes(b"binary\0guard")
        with self.assertRaisesRegex(ValueError, "text bound"):
            self.call("v3_source_read", {"task_id":"inspect-task", "path":"src/binary.rs"}, trusted=admin)
        baseline = (source.parent / "baseline.tar").read_bytes()
        task_blob = host.release.snapshot(source.parent, host.TASK_CONTEXT, host.context_digest)
        digest_before = host.context_digest(task_blob)[0]
        candidate, changed = host.release.candidate(baseline, task_blob, baseline, host.context_digest)
        self.assertFalse(any(host.release.inspection_path(p) for p in host.release.entries(candidate, host.context_digest)))
        target = source / "data/user_data/admin/admin.db"
        target.chmod(0o600)
        with sqlite3.connect(target) as db: db.execute("INSERT INTO docs VALUES ('new',NULL)")
        modified = host.release.snapshot(source.parent, host.TASK_CONTEXT, host.context_digest)
        self.assertNotEqual(digest_before, host.context_digest(modified)[0])
        with self.assertRaisesRegex(ValueError, "inspection snapshot changed"):
            host.release.candidate(baseline, modified, baseline, host.context_digest)

    def test_nonadmin_task_never_requests_private_inspection_snapshot(self):
        seen = []
        def snapshot(inspection=False):
            seen.append(inspection)
            return context()
        self.runner.snapshot = snapshot
        self.call("v3_build_work", {"action":"begin", "task_id":"private-denied", "summary":"User source", "mode":"isolated"})
        self.assertNotIn(True, seen)
        with self.assertRaisesRegex(ValueError, "does not exist"):
            self.call("v3_source_list", {"task_id":"private-denied", "path":"data/user_data/admin"})

    def test_isolated_source_read_is_task_scoped_and_bounded(self):
        task = self.call("v3_build_work", {"action": "begin", "task_id": "read-task",
            "summary": "Read", "mode": "isolated"})
        value = self.call("v3_source_read", {"task_id": "read-task",
            "path": "Cargo.toml", "offset": 1, "limit": 20})
        self.assertEqual(["fixture"], value["lines"])
        self.assertEqual(task["workspace_inputs_sha256"], value["workspace_inputs_sha256"])
        with self.assertRaises(ValueError):
            self.call("v3_source_read", {"task_id": "read-task", "path": "data/config/gates/closing_status_v1.json"})
        with self.assertRaises(ValueError):
            self.call("v3_source_read", {"task_id": "read-task", "path": "../Cargo.toml"})
        with self.assertRaises(ValueError):
            self.call("v3_source_read", {"task_id": "read-task", "path": "Cargo.toml", "limit": 1000})

    def test_task_search_and_batched_reads_keep_caller_scope(self):
        task = self.call("v3_build_work", {"action": "begin", "task_id": "search-task",
            "summary": "Search", "mode": "isolated"})
        source = Path(task["workspace_path"])
        (source / "src").mkdir(exist_ok=True)
        (source / "src" / "sample.rs").write_text("needle first\nother\nneedle second\n")
        found = self.call("v3_source_search", {"task_id": "search-task", "query": "needle"})
        self.assertEqual([1, 3], [entry["line"] for entry in found["matches"]])
        batch = self.call("v3_source_read_batch", {"task_id": "search-task",
            "requests": [{"path": "src/sample.rs", "offset": 1, "limit": 1},
                         {"path": "src/sample.rs", "offset": 3, "limit": 1}]})
        self.assertEqual(["needle first", "needle second"],
                         [entry["lines"][0] for entry in batch["results"]])
        with self.assertRaisesRegex(ValueError, "task"):
            self.call("v3_source_search", {"task_id": "other-task", "query": "needle"})
        with self.assertRaisesRegex(ValueError, "source read"):
            self.call("v3_source_read_batch", {"task_id": "search-task",
                "requests": [{"path": "../secret"}]})

    def test_task_cargo_check_verifies_source_digest(self):
        task = self.call("v3_build_work", {"action": "begin", "task_id": "cargo-task",
            "summary": "Check", "mode": "isolated"})
        digest = task["workspace_inputs_sha256"]
        with patch.object(self.runner, "cargo_task", return_value="check passed", create=True) as command:
            result = self.call("v3_cargo_task", {"task_id": "cargo-task", "task": "check",
                "expected_inputs_sha256": digest, "timeout_seconds": 30})
        self.assertEqual("completed", result["status"])
        self.assertEqual(digest, result["inputs_sha256"])
        self.assertIn("check passed", result["log_excerpt"])
        self.assertEqual(30, command.call_args.args[-1])
        with self.assertRaisesRegex(ValueError, "(?i)stale"):
            self.call("v3_cargo_task", {"task_id": "cargo-task", "task": "check",
                "expected_inputs_sha256": "0" * 64})
        with self.assertRaisesRegex(ValueError, "(?i)open isolated"):
            self.call("v3_cargo_task", {"task_id": "other-task", "task": "check",
                "expected_inputs_sha256": digest})
        def change_during_check(*unused):
            Path(task["workspace_path"], "Cargo.toml").write_text("changed")
            return "check passed"
        with patch.object(self.runner, "cargo_task", side_effect=change_during_check, create=True):
            stale = self.call("v3_cargo_task", {"task_id": "cargo-task", "task": "check",
                "expected_inputs_sha256": digest})
        self.assertEqual("stale", stale["status"])

    def test_isolated_tasks_release_in_order_while_other_work_remains_open(self):
        first = self.call("v3_build_work", {"action": "begin", "task_id": "isolated-one",
            "summary": "First", "mode": "isolated"})
        second = self.call("v3_build_work", {"action": "begin", "task_id": "isolated-two",
            "summary": "Second", "mode": "isolated"})
        Path(first["workspace_path"], "src").mkdir()
        Path(first["workspace_path"], "src/first.rs").write_text("first")
        Path(second["workspace_path"], "src").mkdir()
        Path(second["workspace_path"], "src/second.rs").write_text("second")
        status = self.call("v3_build_work", {"action": "status"})["own"]
        selected = self.call("v3_build_work", {"action": "status", "task_id": "isolated-one"})["own"]
        self.assertEqual(["isolated-one"], [item["task_id"] for item in selected])
        with self.assertRaisesRegex(ValueError, "bounded task_id"):
            self.call("v3_build_work", {"action": "status", "task_id": "../wrong"})
        digests = {item["task_id"]: item["workspace_inputs_sha256"] for item in status}
        one = self.call("v3_build_work", {"action": "finish", "task_id": "isolated-one",
            "expected_inputs_sha256": digests["isolated-one"]})
        self.assertIsNotNone(one["job_id"])
        self.assertEqual(1, one["open_count"])
        self.assertEqual("completed", self.wait(one)["status"])
        two = self.call("v3_build_work", {"action": "finish", "task_id": "isolated-two",
            "expected_inputs_sha256": digests["isolated-two"]})
        self.assertEqual("completed", self.wait(two)["status"])
        final = host.release.entries(
            (self.state / two["job_id"] / "candidate.tar").read_bytes(), host.context_digest)
        self.assertIn("v3/src/first.rs", final)
        self.assertIn("v3/src/second.rs", final)
        self.assertEqual(4, len([call for call in self.runner.calls if call[0] == "build"]))
        self.assertEqual(2, len([call for call in self.runner.calls if call[0] == "promote"]))
        self.assertEqual(two["job_id"], self.call("v3_build_work", {
            "action": "finish", "task_id": "isolated-two",
            "expected_inputs_sha256": digests["isolated-two"]})["job_id"])

    def test_isolated_finish_queues_during_another_build(self):
        first = self.call("v3_build_work", {"action": "begin", "task_id": "queued-one",
            "summary": "First", "mode": "isolated"})
        second = self.call("v3_build_work", {"action": "begin", "task_id": "queued-two",
            "summary": "Second", "mode": "isolated"})
        for item, filename in ((first, "one.rs"), (second, "two.rs")):
            folder = Path(item["workspace_path"], "src")
            folder.mkdir()
            (folder / filename).write_text(filename)
        digests = {item["task_id"]: item["workspace_inputs_sha256"]
                   for item in self.call("v3_build_work", {"action": "status"})["own"]}
        self.runner.block = threading.Event()
        one = self.call("v3_build_work", {"action": "finish", "task_id": "queued-one",
            "expected_inputs_sha256": digests["queued-one"]})
        two = self.call("v3_build_work", {"action": "finish", "task_id": "queued-two",
            "expected_inputs_sha256": digests["queued-two"]})
        self.assertEqual("queued", self.call("v3_worker_job", {
            "job_id": two["job_id"]})["status"])
        self.runner.block.set()
        self.assertEqual("completed", self.wait(one)["status"])
        self.assertEqual("completed", self.wait(two)["status"])
        self.assertEqual(4, len([call for call in self.runner.calls if call[0] == "build"]))

    def test_ui_only_release_reuses_images_and_skips_rust_build(self):
        task = self.call("v3_build_work", {"action": "begin", "task_id": "ui-only-task",
            "summary": "UI asset", "mode": "isolated"})
        asset = Path(task["workspace_path"], "ui/fast-lane.css")
        asset.parent.mkdir(exist_ok=True)
        asset.write_text("body { color: red; }")
        digest = self.call("v3_build_work", {"action": "status", "task_id": "ui-only-task"})["own"][0]["workspace_inputs_sha256"]
        receipt = self.call("v3_build_work", {"action": "finish", "task_id": "ui-only-task",
            "expected_inputs_sha256": digest})
        job = self.wait(receipt)
        self.assertEqual("ui_deploy", job["operation"])
        self.assertEqual("completed", job["status"])
        self.assertEqual([], [call for call in self.runner.calls if call[0] == "build"])
        self.assertEqual({"api": "sha256:" + "1" * 64, "loop": "sha256:" + "1" * 64}, job["images"])
        self.assertEqual("not_required", self.broker.jobs[receipt["job_id"]]["rust_tests"]["status"])
        released = host.release.entries(
            (self.state / receipt["job_id"] / "candidate.tar").read_bytes(), host.context_digest)
        self.assertEqual(b"body { color: red; }", released["v3/ui/fast-lane.css"][1])

    def test_ui_release_switches_assets_without_replacing_containers(self):
        first = self.call("v3_build_work", {"action": "begin", "task_id": "runtime-first",
            "summary": "Runtime migration", "mode": "isolated"})
        source = Path(first["workspace_path"], "src/feature.rs")
        source.parent.mkdir(exist_ok=True)
        source.write_text("feature")
        digest = self.call("v3_build_work", {"action": "status", "task_id": "runtime-first"})["own"][0]["workspace_inputs_sha256"]
        full = self.call("v3_build_work", {"action": "finish", "task_id": "runtime-first",
            "expected_inputs_sha256": digest})
        self.assertEqual("completed", self.wait(full)["status"])
        self.assertTrue(self.runner.hot_assets)
        self.assertEqual(full["job_id"], self.runner.asset_id)
        prior_lifecycle = len([call for call in self.runner.calls if call[0] in ("stop", "create")])

        ui = self.call("v3_build_work", {"action": "begin", "task_id": "asset-second",
            "summary": "UI fast lane", "mode": "isolated"})
        asset = Path(ui["workspace_path"], "ui/hot.css")
        asset.parent.mkdir(exist_ok=True)
        asset.write_text("body {}")
        digest = self.call("v3_build_work", {"action": "status", "task_id": "asset-second"})["own"][0]["workspace_inputs_sha256"]
        receipt = self.call("v3_build_work", {"action": "finish", "task_id": "asset-second",
            "expected_inputs_sha256": digest})
        result = self.wait(receipt)
        self.assertEqual("completed", result["status"])
        self.assertEqual("ui_deploy", result["operation"])
        self.assertEqual(receipt["job_id"], self.runner.asset_id)
        self.assertEqual(prior_lifecycle, len([call for call in self.runner.calls
                                               if call[0] in ("stop", "create")]))
        self.assertEqual(2, len([call for call in self.runner.calls if call[0] == "build"]))
        self.assertIn(("verify_ui", ("v3/ui/hot.css",)), self.runner.calls)

    def test_hot_ui_verification_failure_is_uncertain_and_blocks_queue(self):
        self.runner.hot_assets = True
        task = self.call("v3_build_work", {"action": "begin", "task_id": "failed-hot-ui",
            "summary": "UI verification", "mode": "isolated"})
        asset = Path(task["workspace_path"], "ui/fail.css")
        asset.parent.mkdir(exist_ok=True)
        asset.write_text("body {}")
        digest = self.call("v3_build_work", {"action": "status", "task_id": "failed-hot-ui"})["own"][0]["workspace_inputs_sha256"]
        self.runner.fail_verify_ui = True
        receipt = self.call("v3_build_work", {"action": "finish", "task_id": "failed-hot-ui",
            "expected_inputs_sha256": digest})
        result = self.wait(receipt)
        self.assertEqual("uncertain", result["status"])
        self.assertEqual(receipt["job_id"], self.runner.asset_id)
        self.broker.shutdown()
        self.runner.fail_verify_ui = False
        self.broker = host.Broker(self.state, self.runner)
        self.assertEqual("uncertain", self.broker.jobs[receipt["job_id"]]["status"])
        next_task = self.call("v3_build_work", {"action": "begin", "task_id": "after-uncertain",
            "summary": "Later runtime", "mode": "isolated"})
        source = Path(next_task["workspace_path"], "src/later.rs")
        source.parent.mkdir(exist_ok=True)
        source.write_text("later")
        later_digest = self.call("v3_build_work", {"action": "status", "task_id": "after-uncertain"})["own"][0]["workspace_inputs_sha256"]
        later = self.call("v3_build_work", {"action": "finish", "task_id": "after-uncertain",
            "expected_inputs_sha256": later_digest})
        self.assertEqual("queued", self.call("v3_worker_job", {"job_id": later["job_id"]})["status"])

    def test_ui_switches_during_build_and_runtime_rebases_before_deploy(self):
        bootstrap = self.call("v3_build_work", {"action": "begin", "task_id": "bootstrap-ui",
            "summary": "Migrate asset mount", "mode": "isolated"})
        first_file = Path(bootstrap["workspace_path"], "src/bootstrap.rs")
        first_file.parent.mkdir(exist_ok=True)
        first_file.write_text("bootstrap")
        digest = self.call("v3_build_work", {"action": "status", "task_id": "bootstrap-ui"})["own"][0]["workspace_inputs_sha256"]
        first = self.call("v3_build_work", {"action": "finish", "task_id": "bootstrap-ui",
            "expected_inputs_sha256": digest})
        self.assertEqual("completed", self.wait(first)["status"])

        runtime = self.call("v3_build_work", {"action": "begin", "task_id": "running-runtime",
            "summary": "Runtime work", "mode": "isolated"})
        ui = self.call("v3_build_work", {"action": "begin", "task_id": "parallel-ui",
            "summary": "UI work", "mode": "isolated"})
        runtime_file = Path(runtime["workspace_path"], "src/runtime.rs")
        runtime_file.parent.mkdir(exist_ok=True)
        runtime_file.write_text("runtime")
        ui_file = Path(ui["workspace_path"], "ui/parallel.css")
        ui_file.parent.mkdir(exist_ok=True)
        ui_file.write_text("body {}")
        digests = {item["task_id"]: item["workspace_inputs_sha256"]
                   for item in self.call("v3_build_work", {"action": "status"})["own"]}
        self.runner.block = threading.Event()
        full = self.call("v3_build_work", {"action": "finish", "task_id": "running-runtime",
            "expected_inputs_sha256": digests["running-runtime"]})
        for _ in range(100):
            if self.broker.jobs[full["job_id"]]["phase"] == "building_api":
                break
            time.sleep(.01)
        self.assertEqual("building_api", self.broker.jobs[full["job_id"]]["phase"])
        fast = self.call("v3_build_work", {"action": "finish", "task_id": "parallel-ui",
            "expected_inputs_sha256": digests["parallel-ui"]})
        self.assertEqual("running", self.broker.jobs[full["job_id"]]["status"])
        self.assertTrue(self.broker.jobs[fast["job_id"]].get("parallel_ui"))
        for _ in range(100):
            if self.call("v3_worker_job", {"job_id": fast["job_id"]})["status"] == "completed":
                break
            time.sleep(.01)
        self.assertEqual("completed", self.call("v3_worker_job", {"job_id": fast["job_id"]})["status"])
        self.assertEqual("running", self.call("v3_worker_job", {"job_id": full["job_id"]})["status"])
        self.assertEqual(fast["job_id"], self.runner.asset_id)
        self.runner.block.set()
        final = self.wait(full)
        self.assertEqual("completed", final["status"])
        self.assertIn("built_inputs_sha256", self.broker.jobs[full["job_id"]])
        combined = host.release.entries(
            (self.state / full["job_id"] / "candidate.tar").read_bytes(), host.context_digest)
        self.assertIn("v3/ui/parallel.css", combined)
        self.assertIn("v3/src/runtime.rs", combined)

    def test_full_release_freezes_assets_once_after_concurrent_rebase(self):
        """Concurrent asset publication must survive one final immutable freeze."""
        self.runner.blob=context(extra={"v3/ui/index.html":"<html>fixture</html>"})
        bootstrap = self.call("v3_build_work", {"action": "begin", "task_id": "bootstrap-ui",
            "summary": "Migrate asset mount", "mode": "isolated"})
        first_file = Path(bootstrap["workspace_path"], "src/bootstrap.rs")
        first_file.parent.mkdir(exist_ok=True)
        first_file.write_text("bootstrap")
        digest = self.call("v3_build_work", {"action": "status", "task_id": "bootstrap-ui"})["own"][0]["workspace_inputs_sha256"]
        first = self.call("v3_build_work", {"action": "finish", "task_id": "bootstrap-ui",
            "expected_inputs_sha256": digest})
        self.assertEqual("completed", self.wait(first)["status"])

        runtime = self.call("v3_build_work", {"action": "begin", "task_id": "running-runtime",
            "summary": "Runtime work", "mode": "isolated"})
        ui = self.call("v3_build_work", {"action": "begin", "task_id": "parallel-ui",
            "summary": "UI work", "mode": "isolated"})
        runtime_file = Path(runtime["workspace_path"], "src/runtime.rs")
        runtime_file.parent.mkdir(exist_ok=True)
        runtime_file.write_text("runtime")
        ui_file = Path(ui["workspace_path"], "ui/parallel.css")
        ui_file.parent.mkdir(exist_ok=True)
        ui_file.write_text("body {}")
        digests = {item["task_id"]: item["workspace_inputs_sha256"]
                   for item in self.call("v3_build_work", {"action": "status"})["own"]}
        self.runner.block = threading.Event()
        full = self.call("v3_build_work", {"action": "finish", "task_id": "running-runtime",
            "expected_inputs_sha256": digests["running-runtime"]})
        for _ in range(100):
            if self.broker.jobs[full["job_id"]]["phase"] == "building_api":
                break
            time.sleep(.01)
        self.assertEqual("building_api", self.broker.jobs[full["job_id"]]["phase"])
        fast = self.call("v3_build_work", {"action": "finish", "task_id": "parallel-ui",
            "expected_inputs_sha256": digests["parallel-ui"]})
        self.assertEqual("running", self.broker.jobs[full["job_id"]]["status"])
        self.assertTrue(self.broker.jobs[fast["job_id"]].get("parallel_ui"))
        for _ in range(100):
            if self.call("v3_worker_job", {"job_id": fast["job_id"]})["status"] == "completed":
                break
            time.sleep(.01)
        self.assertEqual("completed", self.call("v3_worker_job", {"job_id": fast["job_id"]})["status"])
        self.assertEqual("running", self.call("v3_worker_job", {"job_id": full["job_id"]})["status"])
        self.assertEqual(fast["job_id"], self.runner.asset_id)
        prepared=[];frozen=[];original_stage=self.runner.stage_assets
        compiler_source=Path(self.broker.jobs[full["job_id"]]["release_root"])
        def prepare(job_id,source_root=None):
            prepared.append((job_id,source_root));return []
        def freeze(blob,job_id):
            host.release.stage_assets(Path(self.temp.name)/"immutable-assets",job_id,blob,host.context_digest)
            frozen.append((job_id,host.context_digest(blob)[0]));return original_stage(blob,job_id)
        self.runner.build_engine_artifacts=prepare;self.runner.build_ability_artifacts=prepare
        # This legacy runner models prior admission; ownership fences have separate enforced fixtures.
        self.broker.scheduler.assert_job=lambda job:None
        self.runner.stage_assets=freeze;self.broker.scheduler.mode="enforce"
        self.runner.block.set()
        final = self.wait(full)
        self.assertEqual("completed", final["status"])
        self.assertIn("built_inputs_sha256", self.broker.jobs[full["job_id"]])
        combined = host.release.entries(
            (self.state / full["job_id"] / "candidate.tar").read_bytes(), host.context_digest)
        self.assertIn("v3/ui/parallel.css", combined)
        self.assertIn("v3/src/runtime.rs", combined)

        self.assertEqual([(full["job_id"],compiler_source)]*2,prepared)
        self.assertEqual([(full["job_id"],self.broker.jobs[full["job_id"]]["inputs_sha256"])],frozen)

    def test_mixed_index_switches_during_build_and_runtime_inherits_metadata(self):
        bootstrap = self.call("v3_build_work", {"action": "begin", "task_id": "bootstrap-ui",
            "summary": "Migrate asset mount", "mode": "isolated"})
        first_file = Path(bootstrap["workspace_path"], "src/bootstrap.rs")
        first_file.parent.mkdir(exist_ok=True)
        first_file.write_text("bootstrap")
        digest = self.call("v3_build_work", {"action": "status", "task_id": "bootstrap-ui"})["own"][0]["workspace_inputs_sha256"]
        first = self.call("v3_build_work", {"action": "finish", "task_id": "bootstrap-ui",
            "expected_inputs_sha256": digest})
        self.assertEqual("completed", self.wait(first)["status"])

        runtime = self.call("v3_build_work", {"action": "begin", "task_id": "running-runtime",
            "summary": "Runtime work", "mode": "isolated"})
        ui = self.call("v3_build_work", {"action": "begin", "task_id": "parallel-ui",
            "summary": "UI work", "mode": "isolated"})
        runtime_file = Path(runtime["workspace_path"], "src/runtime.rs")
        runtime_file.parent.mkdir(exist_ok=True)
        runtime_file.write_text("runtime")
        ui_file = Path(ui["workspace_path"], "ui/parallel.css")
        ui_file.parent.mkdir(exist_ok=True)
        ui_file.write_text("body {}")
        metadata=Path(ui["workspace_path"])/"plugins/abilities/Administrator/code_index/code_index.skill.md"
        metadata.parent.mkdir(parents=True,exist_ok=True);metadata.write_text("Validated guidance")
        digests = {item["task_id"]: item["workspace_inputs_sha256"]
                   for item in self.call("v3_build_work", {"action": "status"})["own"]}
        self.runner.block = threading.Event()
        full = self.call("v3_build_work", {"action": "finish", "task_id": "running-runtime",
            "expected_inputs_sha256": digests["running-runtime"]})
        for _ in range(100):
            if self.broker.jobs[full["job_id"]]["phase"] == "building_api":
                break
            time.sleep(.01)
        self.assertEqual("building_api", self.broker.jobs[full["job_id"]]["phase"])
        fast = self.call("v3_build_work", {"action": "finish", "task_id": "parallel-ui",
            "expected_inputs_sha256": digests["parallel-ui"]})
        self.assertEqual("running", self.broker.jobs[full["job_id"]]["status"])
        self.assertEqual(self.broker.jobs[fast["job_id"]]["operation"],"index_deploy")
        self.assertTrue(self.broker.jobs[fast["job_id"]].get("parallel_ui"))
        for _ in range(100):
            if self.call("v3_worker_job", {"job_id": fast["job_id"]})["status"] == "completed":
                break
            time.sleep(.01)
        self.assertEqual("completed", self.call("v3_worker_job", {"job_id": fast["job_id"]})["status"])
        self.assertEqual("running", self.call("v3_worker_job", {"job_id": full["job_id"]})["status"])
        self.assertEqual(fast["job_id"], self.runner.asset_id)
        self.runner.block.set()
        final = self.wait(full)
        self.assertEqual("completed", final["status"])
        self.assertIn("built_inputs_sha256", self.broker.jobs[full["job_id"]])
        combined = host.release.entries(
            (self.state / full["job_id"] / "candidate.tar").read_bytes(), host.context_digest)
        self.assertIn("v3/ui/parallel.css", combined)
        self.assertIn("v3/src/runtime.rs", combined)
        self.assertIn("v3/plugins/abilities/Administrator/code_index/code_index.skill.md",combined)

    def test_ui_release_passes_queued_build_and_later_build_inherits_assets(self):
        tasks = []
        for name in ("first", "second", "ui-third"):
            tasks.append(self.call("v3_build_work", {"action": "begin", "task_id": name + "-task",
                "summary": name, "mode": "isolated"}))
        for task, path in zip(tasks, ("src/first.rs", "src/second.rs", "ui/fast-lane.css")):
            asset = Path(task["workspace_path"], path)
            asset.parent.mkdir(exist_ok=True)
            asset.write_text(path)
        digests = {item["task_id"]: item["workspace_inputs_sha256"]
                   for item in self.call("v3_build_work", {"action": "status"})["own"]}
        self.runner.block = threading.Event()
        receipts = [self.call("v3_build_work", {"action": "finish", "task_id": name + "-task",
            "expected_inputs_sha256": digests[name + "-task"]})
            for name in ("first", "second", "ui-third")]
        self.runner.block.set()
        for receipt in receipts:
            self.assertEqual("completed", self.wait(receipt)["status"])
        promotions = [call[1] for call in self.runner.calls if call[0] == "promote"]
        self.assertEqual([("v3/src/first.rs",), ("v3/ui/fast-lane.css",),
                          ("v3/src/second.rs",)], promotions)
        released = host.release.entries(
            (self.state / receipts[1]["job_id"] / "candidate.tar").read_bytes(), host.context_digest)
        self.assertIn("v3/ui/fast-lane.css", released)
        self.assertEqual(4, len([call for call in self.runner.calls if call[0] == "build"]))

    def test_compatible_queued_tasks_share_one_tested_image_build(self):
        tasks = []
        for name in ("running-one", "queued-two", "queued-three"):
            tasks.append(self.call("v3_build_work", {"action": "begin", "task_id": name,
                "summary": name, "mode": "isolated"}))
        for task, name in zip(tasks, ("running.rs", "second.rs", "third.rs")):
            path = Path(task["workspace_path"], "src", name)
            path.parent.mkdir(exist_ok=True)
            path.write_text(name)
        digests = {item["task_id"]: item["workspace_inputs_sha256"]
                   for item in self.call("v3_build_work", {"action": "status"})["own"]}
        self.runner.block = threading.Event()
        receipts = [self.call("v3_build_work", {"action": "finish", "task_id": name,
            "expected_inputs_sha256": digests[name]})
            for name in ("running-one", "queued-two", "queued-three")]
        self.runner.block.set()
        results = [self.wait(receipt) for receipt in receipts]
        self.assertEqual(["completed"] * 3, [item["status"] for item in results])
        self.assertEqual(receipts[1]["job_id"], results[2]["batch_parent"])
        self.assertEqual([receipts[2]["job_id"]], results[1]["batch_members"])
        self.assertEqual(results[1]["inputs_sha256"], results[2]["inputs_sha256"])
        self.assertEqual(4, len([call for call in self.runner.calls if call[0] == "build"]))
        self.assertEqual(2, len([call for call in self.runner.calls if call[0] == "promote"]))
        released = host.release.entries(
            (self.state / receipts[1]["job_id"] / "candidate.tar").read_bytes(), host.context_digest)
        self.assertIn("v3/src/second.rs", released)
        self.assertIn("v3/src/third.rs", released)

    def test_isolated_conflict_fails_without_second_deployment(self):
        first = self.call("v3_build_work", {"action": "begin", "task_id": "conflict-one",
            "summary": "First", "mode": "isolated"})
        second = self.call("v3_build_work", {"action": "begin", "task_id": "conflict-two",
            "summary": "Second", "mode": "isolated"})
        for item, value in ((first, "one"), (second, "two")):
            Path(item["workspace_path"], "Cargo.toml").write_text(value)
        digests = {item["task_id"]: item["workspace_inputs_sha256"]
                   for item in self.call("v3_build_work", {"action": "status"})["own"]}
        one = self.call("v3_build_work", {"action": "finish", "task_id": "conflict-one",
            "expected_inputs_sha256": digests["conflict-one"]})
        self.assertEqual("completed", self.wait(one)["status"])
        two = self.call("v3_build_work", {"action": "finish", "task_id": "conflict-two",
            "expected_inputs_sha256": digests["conflict-two"]})
        result = self.wait(two)
        self.assertEqual("failed", result["status"])
        self.assertIn("source conflict", result["reason"])
        self.assertEqual(2, len([call for call in self.runner.calls if call[0] == "build"]))

    def test_coordinated_work_batches_one_build_and_deployment(self):
        first = self.call("v3_build_work", {"action": "begin", "task_id": "first-task", "summary": "First change"})
        second = self.call("v3_build_work", {"action": "begin", "task_id": "second-task", "summary": "Second change"})
        self.assertEqual(2, second["open_count"])
        with self.assertRaisesRegex(ValueError, "work remains open"):
            self.build()
        digest = host.context_digest(self.runner.snapshot())[0]
        closed = self.call("v3_build_work", {"action": "finish", "task_id": "first-task",
                                              "expected_inputs_sha256": digest})
        self.assertIsNone(closed["job_id"])
        closed = self.call("v3_build_work", {"action": "finish", "task_id": "second-task",
                                              "expected_inputs_sha256": digest})
        self.assertEqual("completed", self.wait({"job_id": closed["job_id"]})["status"])
        duplicate = self.call("v3_build_work", {"action": "finish", "task_id": "second-task",
                                                 "expected_inputs_sha256": digest})
        self.assertEqual(closed["job_id"], duplicate["job_id"])
        first_duplicate = self.call("v3_build_work", {"action": "finish", "task_id": "first-task",
                                                       "expected_inputs_sha256": digest})
        self.assertEqual(closed["job_id"], first_duplicate["job_id"])
        self.assertEqual(2, len([c for c in self.runner.calls if c[0] == "build"]))
        self.assertEqual(2, len([c for c in self.runner.calls if c[0] == "stop"]))
        self.assertEqual(2, len([c for c in self.runner.calls if c[0] == "healthy"]))
        self.assertEqual(0, self.call("v3_build_work", {"action": "status"})["open_count"])

    def test_coordinated_work_rejects_stale_inputs_without_replacement(self):
        self.call("v3_build_work", {"action": "begin", "task_id": "stale-task", "summary": "Change"})
        old = host.context_digest(self.runner.snapshot())[0]
        self.runner.blob = context("changed")
        with self.assertRaisesRegex(ValueError, "stale work inputs"):
            self.call("v3_build_work", {"action": "finish", "task_id": "stale-task",
                                         "expected_inputs_sha256": old})
        self.assertEqual(1, self.call("v3_build_work", {"action": "status"})["open_count"])
        self.assertFalse(any(c[0] == "stop" for c in self.runner.calls))

    def test_coordinated_build_changed_inputs_never_stops_live_containers(self):
        self.call("v3_build_work", {"action": "begin", "task_id": "moving-task", "summary": "Moving"})
        self.runner.change_during_build = True
        digest = host.context_digest(self.runner.snapshot())[0]
        job = self.call("v3_build_work", {"action": "finish", "task_id": "moving-task",
                                           "expected_inputs_sha256": digest})
        result = self.wait({"job_id": job["job_id"]})
        self.assertEqual("failed", result["status"])
        self.assertFalse(any(c[0] == "stop" for c in self.runner.calls))

    def test_coordinated_work_survives_broker_restart(self):
        self.call("v3_build_work", {"action": "begin", "task_id": "saved-task", "summary": "Saved"})
        self.broker.shutdown()
        self.broker = host.Broker(self.state, self.runner)
        self.assertEqual(1, self.call("v3_build_work", {"action": "status"})["open_count"])
        with self.assertRaisesRegex(ValueError, "work remains open"):
            self.build()

    def test_worker_restart_uses_fixed_local_control_and_refuses_busy_broker(self):
        calls = []
        def control(method, path, body=None):
            calls.append((method, path, body))
            return {"status": "restarting", "previous_pid": 10} if method == "POST" else {"pid": 11}
        with patch.object(host, "worker_control", control), patch.object(host.time, "sleep"):
            value = self.call("v3_build_worker_restart", {"request_id": "stable-restart"})
        self.assertEqual(value["status"], "ok")
        self.assertEqual(calls, [("POST", "/v1/restart", {"request_id": "stable-restart"}),
                                 ("GET", "/v1/status", None)])
        with self.assertRaises(ValueError):
            self.call("v3_build_worker_restart", {"request_id": "../bad"})
        self.broker.active = "another-job"
        with self.assertRaisesRegex(ValueError, "busy"):
            self.call("v3_build_worker_restart", {"request_id": "another-restart"})
        self.broker.active = None
        self.assertEqual(len(calls), 2)

    def test_low_disk_fails_before_build_or_container_replacement(self):
        self.runner.low_space = True
        result = self.wait(self.build())
        self.assertEqual("failed", result["status"])
        self.assertFalse(any(call[0] == "build" for call in self.runner.calls))
        self.assertFalse(any(call[0] == "stop" for call in self.runner.calls))
        self.assertIn(("space", host.MIN_SEQUENCE_FREE), self.runner.calls)

    def test_frozen_build_idempotence_and_secret_scrub(self):
        value = self.wait(self.build())
        self.assertEqual("completed", value["status"])
        self.assertEqual({"api", "loop"}, set(value["images"]))
        self.assertNotIn("NEVER-EXPOSE-THIS", json.dumps(value))
        duplicate = self.build()
        self.assertEqual(value["job_id"], duplicate["job_id"])
        self.assertEqual(2, len([c for c in self.runner.calls if c[0] == "build"]))
        with self.assertRaisesRegex(ValueError, "changed body"):
            self.call("v3_build_image", {"request_id": "build-fixture", "expected_inputs_sha256": "0" * 64})

    def test_changed_inputs_mark_stale_and_block_deploy(self):
        self.runner.change_during_build = True
        value = self.wait(self.build())
        self.assertEqual("stale", value["status"])
        with self.assertRaisesRegex(ValueError, "stale build inputs"):
            self.deploy(value)
        self.assertFalse(any(c[0] == "stop" for c in self.runner.calls))

    def test_busy_and_scope_rejections(self):
        self.runner.block = threading.Event()
        submitted = self.build()
        with self.assertRaisesRegex(ValueError, "busy"):
            self.build("second-build")
        other = str(uuid.uuid4())
        with self.assertRaisesRegex(ValueError, "not available"):
            self.call("v3_worker_job", {"job_id": submitted["job_id"]}, {"user_id": "other", "session_id": other}, other)
        with self.assertRaises(ValueError):
            self.call("v3_build_status", {"source": "/home/ar/webagent"})
        with self.assertRaisesRegex(ValueError, "unsupported"):
            self.call("v3_worker_submit", {"operation": "shell", "command": "docker rm production"})
        self.runner.block.set()
        self.wait(submitted)

    # Require unavailable candidate images to fail preflight while preserving both original consumers.
    def assert_missing_candidate_preserves_consumers(self,role):
        built=self.wait(self.build())
        before=copy.deepcopy(self.runner.containers)
        self.runner.missing_deploy_role=role
        self.runner.calls.clear()
        result=self.wait(self.deploy(built))
        self.assertEqual(result["status"],"failed")
        self.assertEqual(result["phase"],"checking_candidate_images")
        self.assertEqual(self.runner.containers,before)
        self.assertFalse(any(call[0]in {"stop","rename","create","start","activate_assets","promote"}for call in self.runner.calls))
        self.assertTrue(any(call[0]=="image_preflight"for call in self.runner.calls))

    # Cover API-image absence at the preflight boundary without stopping either consumer.
    def test_missing_candidate_api_image_keeps_both_original_consumers_running(self):
        self.assert_missing_candidate_preserves_consumers("api")

    # Cover loop-image absence at the preflight boundary without stopping either consumer.
    def test_missing_candidate_loop_image_keeps_both_original_consumers_running(self):
        self.assert_missing_candidate_preserves_consumers("loop")

    def test_deploy_preserves_rollback_and_private_configuration(self):
        built = self.wait(self.build())
        deployed = self.wait(self.deploy(built))
        self.assertEqual("completed", deployed["status"])
        archive = self.state / deployed["job_id"] / "rollback-images.tar"
        self.assertTrue(archive.exists())
        for rollback in deployed["rollback"].values():
            self.assertEqual("exited", self.runner.containers[rollback]["State"]["Status"])
        self.assertNotIn("NEVER-EXPOSE-THIS", json.dumps(deployed))
        private = self.state / deployed["job_id"] / "container-config.json"
        self.assertEqual(0o600, private.stat().st_mode & 0o777)
        self.assertIn("NEVER-EXPOSE-THIS", private.read_text())
        self.assertFalse(any("webagent-dev-v2" in str(c) for c in self.runner.calls))

    def test_deploy_starts_both_before_health_checks_requiring_loop_heartbeat(self):
        built = self.wait(self.build())
        self.runner.require_both_started = True
        deployed = self.wait(self.deploy(built))
        self.assertEqual("completed", deployed["status"])
        starts = [i for i, call in enumerate(self.runner.calls) if call[0] == "start"]
        checks = [i for i, call in enumerate(self.runner.calls) if call[0] == "healthy"]
        self.assertEqual(2, len(starts))
        self.assertEqual(2, len(checks))
        self.assertLess(max(starts), min(checks))
        self.assertEqual(set(host.TARGETS), {self.runner.calls[i][1] for i in checks})

    def test_superseded_uncertain_release_does_not_hold_queue(self):
        old = {"job_id": str(uuid.uuid4()), "status": "uncertain",
               "operation": "build_deploy", "isolated": True,
               "effects_started": True, "created_at": 1, "finished_at": 2}
        self.broker.jobs[old["job_id"]] = old
        self.assertTrue(self.broker._has_unresolved_release())
        interrupted = {"job_id": str(uuid.uuid4()), "status": "interrupted",
                       "operation": "build_deploy", "created_at": 3}
        self.broker.jobs[interrupted["job_id"]] = interrupted
        self.assertTrue(self.broker._has_unresolved_release())
        completed = {"job_id": str(uuid.uuid4()), "status": "completed",
                     "operation": "build_deploy", "isolated": True,
                     "effects_started": True, "created_at": 4, "finished_at": 5}
        self.broker.jobs[completed["job_id"]] = completed
        self.assertFalse(self.broker._has_unresolved_release())


    def test_uncertain_external_effect_is_not_replayed_on_restart(self):
        built = self.wait(self.build())
        original_start = self.runner.start
        def lost_start(name):
            original_start(name)
            raise RuntimeError("lost start response")
        self.runner.start = lost_start
        deployed = self.wait(self.deploy(built))
        self.assertEqual("uncertain", deployed["status"])
        mutations = len([c for c in self.runner.calls if c[0] in ("stop", "rename", "create", "start")])
        self.broker.shutdown()
        self.broker = host.Broker(self.state, self.runner)
        self.assertEqual("uncertain", self.call("v3_worker_job", {"job_id": deployed["job_id"]})["status"])
        duplicate = self.deploy(built)
        self.assertEqual(deployed["job_id"], duplicate["job_id"])
        self.assertEqual(mutations, len([c for c in self.runner.calls if c[0] in ("stop", "rename", "create", "start")]))

    def test_operator_adopts_only_matching_healthy_uncertain_release(self):
        item = self.call("v3_build_work", {"action": "begin", "task_id": "adopt-release",
            "summary": "Adopt release", "mode": "isolated"})
        source = Path(item["workspace_path"], "src", "adopt.rs")
        source.parent.mkdir()
        source.write_text("adopt")
        digest = self.call("v3_build_work", {"action": "status",
            "task_id": "adopt-release"})["own"][0]["workspace_inputs_sha256"]
        def conflict(*_args):
            raise ValueError("checkout changed during release")
        self.runner.promote_checkout = conflict
        receipt = self.call("v3_build_work", {"action": "finish",
            "task_id": "adopt-release", "expected_inputs_sha256": digest})
        result = self.wait(receipt)
        self.assertEqual("uncertain", result["status"])
        with self.assertRaises(PermissionError):
            self.call("v3_build_adopt_release", {"job_id": receipt["job_id"]})
        local = {"user_id": "codex-local", "session_id": host.LOCAL_WORKSPACE,
                 "local_caller": True, "lease_until": time.time() + 30,
                 "access_fingerprint": "a" * 64}
        self.runner.containers[host.TARGETS[0]]["Image"] = "sha256:" + "0" * 64
        with self.assertRaisesRegex(ValueError, "does not match healthy runtime"):
            self.call("v3_build_adopt_release", {"job_id": receipt["job_id"]},
                      trusted=local, workspace=host.LOCAL_WORKSPACE)
        self.runner.containers[host.TARGETS[0]]["Image"] = result["images"]["api"]
        accepted = self.call("v3_build_adopt_release", {"job_id": receipt["job_id"]},
                             trusted=local, workspace=host.LOCAL_WORKSPACE)
        self.assertTrue(accepted["operator_adopted"])
        self.assertEqual("uncertain", self.broker.jobs[receipt["job_id"]]["status"])
        self.assertFalse(self.broker._has_unresolved_release())
        dashboard = {"user_id": "admin", "session_id": "00000000-0000-0000-0000-000000000000",
                     "lease_until": time.time() + 30, "access_fingerprint": "a" * 64}
        recent = self.call("v3_build_dashboard", trusted=dashboard, workspace=dashboard["session_id"])
        self.assertNotIn(receipt["job_id"], [job["job_id"] for job in recent["jobs"]])
        retained = self.call("v3_build_result", {"job_id": receipt["job_id"]},
                             trusted=dashboard, workspace=dashboard["session_id"])["job"]
        self.assertEqual("uncertain", retained["status"])
        self.broker.shutdown()
        self.broker = host.Broker(self.state, self.runner)
        self.assertFalse(self.broker._has_unresolved_release())
        self.assertEqual(result["inputs_sha256"],
                         host.context_digest(self.broker._current_release_blob())[0])


    def admin(self):
        return {"user_id": "admin", "session_id": host.ADMIN_WORKSPACE, "lease_until": time.time() + 30, "access_fingerprint": "e" * 64}

    def rebuild(self, mode, request_id, trusted=None):
        trusted = trusted or self.admin()
        return self.call("v3_build_rebuild_checkout", {"mode": mode, "request_id": request_id}, trusted=trusted, workspace=trusted["session_id"])

    def deployed_baseline(self):
        """Ship one ordinary isolated release so the checkout can drift from a deployed release."""
        item = self.call("v3_build_work", {"action": "begin", "task_id": "baseline-release", "summary": "Baseline", "mode": "isolated"})
        Path(item["workspace_path"], "src").mkdir(exist_ok=True);Path(item["workspace_path"], "src", "base.rs").write_text("base")
        digest = self.call("v3_build_work", {"action": "status", "task_id": "baseline-release"})["own"][0]["workspace_inputs_sha256"]
        self.assertEqual(self.wait(self.call("v3_build_work", {"action": "finish", "task_id": "baseline-release", "expected_inputs_sha256": digest}))["status"], "completed")

    def drift(self, **files):
        entries = host.release.entries(self.broker._current_release_blob(), host.context_digest)
        entries.update({name: (0o644, body.encode()) for name, body in files.items()})
        self.runner.blob = host.release.archive(entries, host.context_digest)

    def settle(self, job_id):
        for _ in range(500):
            job = self.broker.jobs[job_id]
            if job["status"] not in ("queued", "running") and not self.broker.active and job.get("rebuild_closeout"):
                return job
            time.sleep(.01)
        return self.broker.jobs[job_id]

    def test_rebuild_requires_an_administrator_and_valid_arguments(self):
        with self.assertRaises(PermissionError):
            self.rebuild("dev", "request-one", trusted=self.trusted)
        for args in ({"mode": "fast", "request_id": "request-one"}, {"mode": "dev", "request_id": "x"}, {"mode": "dev"}):
            with self.assertRaises(ValueError):
                self.call("v3_build_rebuild_checkout", args, trusted=self.admin(), workspace=host.ADMIN_WORKSPACE)

    def test_rebuild_without_drift_reports_no_changes(self):
        self.assertEqual(self.rebuild("dev", "request-nodrift")["status"], "no_changes")

    def test_rebuild_releases_checkout_drift_commits_and_closes(self):
        self.deployed_baseline();self.drift(**{"v3/src/rebuilt.rs": "fn rebuilt() {}"})
        first = self.rebuild("dev", "request-drift")
        self.assertEqual((first["status"], first["changed_paths"]), ("closed", ["v3/src/rebuilt.rs"]))
        job = self.settle(first["job_id"])
        self.assertEqual((job["status"], job["release_mode"], job["operation"]), ("completed", "dev", "build_deploy"))
        # Commit verification against real git is covered in test_scheduler_broker; the fake checkout has none.
        self.assertIn("status", job["rebuild_closeout"])
        commits = [c for c in self.runner.calls if c[0] == "commit_release"]
        self.assertEqual(commits[0][1], ("src/rebuilt.rs",));self.assertIn(job["job_id"], commits[0][2])
        again = self.rebuild("dev", "request-drift")
        self.assertEqual(again["job_id"], first["job_id"])
        self.assertEqual(len([c for c in self.runner.calls if c[0] == "commit_release"]), 1)

    def test_only_one_rebuild_runs_at_a_time(self):
        self.deployed_baseline();self.drift(**{"v3/src/rebuilt.rs": "fn rebuilt() {}"})
        gate = threading.Event();self.runner.block = gate
        try:
            self.rebuild("dev", "request-first")
            with self.assertRaisesRegex(ValueError, "already queued or running"):
                self.rebuild("dev", "request-second")
        finally:
            gate.set();self.runner.block = None

    def test_stable_rebuild_needs_the_declared_feature_and_forces_a_full_build(self):
        with self.assertRaisesRegex(ValueError, "stable-abilities"):
            self.rebuild("stable", "request-stable-missing")
        self.deployed_baseline();self.drift(**{"v3/Cargo.toml": "[package]\nname = \"webagent-v3\"\n[features]\nstable-abilities = []\n"})
        result = self.rebuild("stable", "request-stable")
        job = self.settle(result["job_id"])
        self.assertEqual((job["release_mode"], job["operation"]), ("stable", "build_deploy"))
        self.assertEqual(host.executor_module.Executor.live_recipe(True)[-1].split("--features ")[-1], "webagent-v3/live-context,webagent-v3/stable-abilities")
        self.assertNotIn("stable-abilities", host.executor_module.Executor.live_recipe()[-1])

    def test_operator_continues_build_release_stopped_at_ui_check(self):
        self.runner.hot_assets = True
        local = {"user_id": "codex-local", "session_id": host.LOCAL_WORKSPACE, "local_caller": True,
                 "lease_until": time.time() + 60, "access_fingerprint": "a" * 64}
        def work(args):
            return self.call("v3_build_work", args, trusted=local, workspace=host.LOCAL_WORKSPACE)
        work({"action": "begin", "task_id": "seo-ui-check", "summary": "SEO shell", "mode": "isolated"})
        status = work({"action": "status", "task_id": "seo-ui-check"})["own"][0]
        source = Path(status["workspace_path"], "src", "seo.rs")
        source.parent.mkdir(exist_ok=True)
        source.write_text("seo")
        digest = work({"action": "status", "task_id": "seo-ui-check"})["own"][0]["workspace_inputs_sha256"]
        self.runner.fail_verify_ui = True
        receipt = work({"action": "finish", "task_id": "seo-ui-check", "expected_inputs_sha256": digest})
        for _ in range(500):
            job = self.broker.jobs[receipt["job_id"]]
            if job["status"] not in ("queued", "running") and not self.broker.active:
                break
            time.sleep(.01)
        self.assertEqual((job["status"], job["phase"], job["operation"]), ("uncertain", "checking_ui", "build_deploy"))
        mutations = len([c for c in self.runner.calls if c[0] in ("stop", "rename", "create", "start")])
        other = dict(local, user_id="someone-else")
        with self.assertRaises(PermissionError):
            self.call("v3_build_adopt_release", {"job_id": receipt["job_id"]}, trusted=other, workspace=host.LOCAL_WORKSPACE)
        with self.assertRaisesRegex(RuntimeError, "UI response mismatch"):
            self.call("v3_build_adopt_release", {"job_id": receipt["job_id"]}, trusted=local, workspace=host.LOCAL_WORKSPACE)
        self.assertEqual("uncertain", job["status"])
        self.runner.fail_verify_ui = False
        accepted = self.call("v3_build_adopt_release", {"job_id": receipt["job_id"]}, trusted=local, workspace=host.LOCAL_WORKSPACE)
        self.assertEqual((accepted["status"], accepted["effects_replayed"]), ("completed", False))
        self.assertEqual(job["phase"], "finished")
        self.assertEqual(mutations, len([c for c in self.runner.calls if c[0] in ("stop", "rename", "create", "start")]))
        self.assertFalse(self.broker._has_unresolved_release())
        self.assertEqual(job["inputs_sha256"], host.context_digest(self.broker._current_release_blob())[0])

    def test_operator_adopts_healthy_hot_ui_checkout_conflict(self):
        self.runner.hot_assets = True
        task = self.call("v3_build_work", {"action": "begin", "task_id": "adopt-hot-ui",
            "summary": "Adopt hot UI", "mode": "isolated"})
        asset = Path(task["workspace_path"], "ui/hot-adopt.css")
        asset.parent.mkdir(exist_ok=True)
        asset.write_text("body {}")
        digest = self.call("v3_build_work", {"action": "status"})["own"][0]["workspace_inputs_sha256"]
        self.runner.promote_checkout = lambda *_: (_ for _ in ()).throw(ValueError("checkout changed"))
        receipt = self.call("v3_build_work", {"action": "finish",
            "task_id": "adopt-hot-ui", "expected_inputs_sha256": digest})
        result = self.wait(receipt)
        self.assertEqual("uncertain", result["status"])
        self.assertEqual(receipt["job_id"], self.runner.asset_id)
        local = {"user_id": "codex-local", "session_id": host.LOCAL_WORKSPACE,
                 "local_caller": True, "lease_until": time.time() + 30,
                 "access_fingerprint": "a" * 64}
        self.runner.fail_verify_ui = True
        with self.assertRaisesRegex(ValueError, "does not match healthy runtime"):
            self.call("v3_build_adopt_release", {"job_id": receipt["job_id"]},
                      trusted=local, workspace=host.LOCAL_WORKSPACE)
        self.runner.fail_verify_ui = False
        accepted = self.call("v3_build_adopt_release", {"job_id": receipt["job_id"]},
                             trusted=local, workspace=host.LOCAL_WORKSPACE)
        self.assertTrue(accepted["operator_adopted"])
        self.assertFalse(self.broker._has_unresolved_release())


    def test_recovery_reconciles_matching_healthy_deployment(self):
        built = self.wait(self.build())
        deployed = self.wait(self.deploy(built))
        job = self.broker.jobs[deployed["job_id"]]
        job["status"] = "running"
        self.broker._save(job)
        self.broker.shutdown()
        self.broker = host.Broker(self.state, self.runner)
        recovered = self.call("v3_worker_job", {"job_id": deployed["job_id"]})
        self.assertEqual("completed", recovered["status"])
        self.assertIn("recovered", recovered["reason"])

    def test_only_one_broker_and_allowlisted_tar(self):
        with self.assertRaisesRegex(ValueError, "already running"):
            host.Broker(self.state, self.runner)
        for extra in ({"v3/data/session.db": "private"}, {"v3/src/../../data/secrets": "private"}):
            with self.assertRaisesRegex(ValueError, "allowlist"):
                host.context_digest(context(extra=extra))
        with self.assertRaisesRegex(ValueError, "cache"):
            host.context_digest(context(extra={"v3/tests/__pycache__/generated.pyc": "bytecode"}))
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            member = tarfile.TarInfo("v3/src/link")
            member.type, member.linkname = tarfile.SYMTYPE, "/state"
            archive.addfile(member)
        with self.assertRaisesRegex(ValueError, "symlink"):
            host.context_digest(stream.getvalue())

    def test_pending_deploy_revocation_prevents_first_external_mutation(self):
        built = self.wait(self.build())
        self.runner.export_entered = threading.Event()
        self.runner.export_release = threading.Event()
        submitted = self.deploy(built)
        self.assertTrue(self.runner.export_entered.wait(5))
        revoked = self.call("administrator_host_revoke")
        self.assertEqual(1, revoked["updated"])
        self.runner.export_release.set()
        result = self.wait(submitted)
        self.assertEqual("cancelled", result["status"])
        self.assertFalse(any(c[0] in ("stop", "rename", "create", "start") for c in self.runner.calls))
        self.assertTrue((self.state / result["job_id"] / "rollback-images.tar").is_file())

    def test_self_replacement_keeps_bounded_commit_lease(self):
        built = self.wait(self.build())
        self.runner.stop_entered = threading.Event()
        self.runner.stop_release = threading.Event()
        submitted = self.deploy(built, "commit-lease")
        self.assertTrue(self.runner.stop_entered.wait(5))
        before = self.broker.jobs[submitted["job_id"]]["lease_until"]
        self.assertGreater(before - time.time(), 100)
        self.call("administrator_host_lease", trusted={**self.trusted,
                  "lease_until": time.time() + 30})
        self.assertEqual(before, self.broker.jobs[submitted["job_id"]]["lease_until"])
        self.runner.stop_release.set()
        self.assertEqual("completed", self.wait(submitted)["status"])

    def test_revocation_after_first_stop_is_uncertain_without_further_effects(self):
        built = self.wait(self.build())
        self.runner.stop_entered = threading.Event()
        self.runner.stop_release = threading.Event()
        submitted = self.deploy(built)
        self.assertTrue(self.runner.stop_entered.wait(5))
        self.call("administrator_host_revoke")
        self.runner.stop_release.set()
        result = self.wait(submitted)
        self.assertEqual("uncertain", result["status"])
        self.assertEqual(1, len([c for c in self.runner.calls if c[0] == "stop"]))
        self.assertFalse(any(c[0] in ("rename", "start") for c in self.runner.calls))
        # Replacements are only ever created, stopped, before the first consumer stop.
        stop = next(i for i, c in enumerate(self.runner.calls) if c[0] == "stop")
        self.assertTrue(all(i < stop for i, c in enumerate(self.runner.calls) if c[0] == "create"))

    def test_rejected_create_leaves_running_consumers_untouched(self):
        built = self.wait(self.build())
        original = self.runner.create
        def reject_loop(name, *args, **kwargs):
            if name == host.TARGETS[1]:
                raise RuntimeError("development container create failed (400): Duplicate mount point: /live-checkout")
            return original(name, *args, **kwargs)
        self.runner.create = reject_loop
        deployed = self.wait(self.deploy(built))
        self.assertEqual(("failed", "creating_candidates"), (deployed["status"], deployed["phase"]))
        self.assertIn("Duplicate mount point", deployed["reason"])
        self.assertFalse(any(c[0] in ("stop", "start", "activate_assets", "promote") for c in self.runner.calls))
        for name in host.TARGETS:
            self.assertEqual(("running", "sha256:" + "1" * 64), (self.runner.containers[name]["State"]["Status"], self.runner.containers[name]["Image"]))
        # The API candidate created first is quarantined under its failed name, never deleted.
        failed = self.runner.containers[host.TARGETS[0] + "-failed-build-" + deployed["job_id"]]
        self.assertEqual(("created", "sha256:" + "a" * 64), (failed["State"]["Status"], failed["Image"]))
        self.assertNotIn(host.candidate_name(host.TARGETS[0], deployed["job_id"]), self.runner.containers)

    def test_changed_fingerprint_stops_job_and_expired_lease_cannot_revive(self):
        self.runner.block = threading.Event()
        submitted = self.build()
        changed = {**self.trusted, "access_fingerprint": "e" * 64, "lease_until": time.time() + 30}
        result = self.call("administrator_host_lease", trusted=changed)
        self.assertEqual(1, result["updated"])
        self.runner.block.set()
        self.assertEqual("cancelled", self.wait(submitted)["status"])
        with self.assertRaisesRegex(ValueError, "lease"):
            self.call("administrator_host_lease", trusted={**self.trusted, "lease_until": time.time() - 1})

    def test_public_log_and_failure_reason_fit_model_budget(self):
        value = self.wait(self.build())
        job = self.broker.jobs[value["job_id"]]
        job["log"] = "あ" * 100000
        job["reason"] = "あ" * 100000 + "final failure"
        view = self.call("v3_worker_job", {"job_id": value["job_id"]})
        self.assertLessEqual(len(view["log"].encode()), host.PUBLIC_LOG)
        self.assertLessEqual(len(view["reason"].encode()), 4096)
        self.assertTrue(view["reason"].endswith("final failure"))
        self.assertTrue(view["log_truncated"])
        self.assertEqual(300000, view["log_bytes"])
        self.assertLess(len(json.dumps(view, ensure_ascii=False).encode()), 22000)

    def test_atomic_export_is_unpublished_until_command_succeeds(self):
        runner = host.Runner()
        destination = Path(self.temp.name) / "rollback-images.tar"
        def command(argv, **kwargs):
            self.assertFalse(destination.exists())
            Path(argv[argv.index("-o") + 1]).write_bytes(b"image archive")
            return ""
        with patch.object(runner, "command", command):
            runner.export(["sha256:" + "a" * 64], destination)
        self.assertEqual(b"image archive", destination.read_bytes())
        self.assertEqual(0o600, destination.stat().st_mode & 0o777)
        def fail(argv, **kwargs):
            Path(argv[argv.index("-o") + 1]).write_bytes(b"incomplete")
            raise RuntimeError("export failed")
        with patch.object(runner, "command", fail), self.assertRaises(RuntimeError):
            runner.export(["sha256:" + "a" * 64], destination)
        self.assertEqual(b"image archive", destination.read_bytes())
        self.assertFalse(list(destination.parent.glob(".rollback-*")))

    def test_shutdown_revokes_active_job_without_replaying(self):
        self.runner.block = threading.Event()
        submitted = self.build()
        self.broker.request_shutdown()
        self.runner.block.set()
        self.assertEqual("cancelled", self.wait(submitted)["status"])
        with self.assertRaisesRegex(PermissionError, "stopping"):
            self.build("after-shutdown")

    def test_replacement_adopts_defaults_keeps_explicit_credentials(self):
        old = {"Env": ["MODE=old", "TOKEN=default"], "Cmd": ["old"], "Labels": {"image": "old"}}
        new = {"Env": ["MODE=new", "ADDED=yes"], "Cmd": ["new"], "Labels": {"image": "new"}}
        current = {"Env": ["MODE=old", "TOKEN=private", "CUSTOM=yes"], "Cmd": ["old"], "Labels": {"image": "old", "custom": "yes"}}
        value = host.replacement_config(current, old, new)
        self.assertEqual(["new"], value["Cmd"])
        self.assertEqual({"MODE=new", "ADDED=yes", "TOKEN=private", "CUSTOM=yes"}, set(value["Env"]))
        self.assertEqual({"image": "new", "custom": "yes"}, value["Labels"])


    def test_index_metadata_and_ledger_only_use_no_compile_lane(self):
        self.runner.hot_assets=True
        for n,files in enumerate(({}, {"plugins/abilities/Administrator/code_index/code-index-features.json": '{"version":1,"features":[]}'}, {"plugins/abilities/Administrator/code_index/code_index.skill.md": "Reviewed guidance", "ui/index.html": "<main>paired</main>"})):
            task_id="index-hot-"+str(n)
            task=self.call("v3_build_work", {"action":"begin","task_id":task_id,"summary":"Index metadata","mode":"isolated"})
            for path,body in files.items():
                target=Path(task["workspace_path"])/path;target.parent.mkdir(parents=True,exist_ok=True);target.write_text(body)
            digest=self.call("v3_build_work",{"action":"status","task_id":task_id})["own"][0]["workspace_inputs_sha256"]
            job=self.wait(self.call("v3_build_work",{"action":"finish","task_id":task_id,"expected_inputs_sha256":digest}))
            self.assertEqual("index_deploy",job["operation"]);self.assertEqual("completed",job["status"])
        self.assertFalse(any(call[0]=="build" for call in self.runner.calls))
    def test_maintenance_cross_session_writes_require_operator_and_live_authority(self):
        task=self.call("v3_build_work",{"action":"begin","task_id":"index-auth-task","summary":"Reviews","mode":"isolated"})
        root=Path(task["workspace_path"]);(root/"one.py").write_text("def first(): pass\n")
        key=root.parent.name
        sync={"action":"sync","task_id":"index-auth-task","scope":"one.py","request_id":"index-sync-one"}
        result=self.call("code_index_maintenance",sync)
        self.assertEqual(result["task_key"],key)
        rows=self.call("code_index_maintenance",{"action":"list","task_id":"index-auth-task"})["results"]
        self.assertEqual(len(rows),2)
        foreign={**self.trusted,"user_id":"another"}
        with self.assertRaises(PermissionError):self.call("code_index_maintenance",{**sync,"task_key":key},trusted=foreign)
        operator={**foreign,"local_caller":True}
        result=self.call("code_index_maintenance",{**sync,"task_key":key,"request_id":"operator-sync"},trusted=operator)
        self.assertEqual(result["task_key"],key)
        with self.assertRaises(PermissionError):self.call("code_index_maintenance",{**sync,"request_id":"expired-sync"},trusted={**self.trusted,"lease_until":0})

class IndexParserPairingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        (self.source / "host-build-service").mkdir(parents=True)
        self.config_patch=patch.dict(host.CONFIG,{"service_dir":str(self.source/"host-build-service")});self.config_patch.start()
        self.assets = self.root / "assets"
        (self.assets / "releases").mkdir(parents=True)

    def tearDown(self):
        self.config_patch.stop()
        self.temp.cleanup()

    def blob(self, adapter):
        return context(extra={"v3/plugins/abilities/Administrator/code_index/maintenance.py": Path(host._maintenance_path).read_text(),host.INDEX_ADAPTER_MEMBER: adapter.decode("utf-8"),
                              "v3/ui/index.html": "<main>fixture</main>"})

    def runner(self):
        runner = host.Runner(source=self.source, snapshot_socket=None)
        runner.inspect = lambda _name: {"Image": "sha256:" + "1" * 64}
        return runner

    def test_matching_parser_allows_index_stage_and_executor(self):
        adapter = b"# reviewed parser source\n"
        (self.source / "host-build-service/index-scanner.py").write_bytes(adapter)
        runner = self.runner()
        executed = []

        def execute(argv, timeout=120):
            executed.append(argv)
            output_mount = next(arg for arg in argv if arg.endswith(":/output:rw"))
            output = Path(output_mount.split(":", 1)[0])
            (output / "manifest.json").write_text("{}", encoding="utf-8")
            (output / "index.sqlite").write_bytes(b"fixture")

        runner.command = execute
        with patch.object(host, "ASSET_ROOT", self.assets), \
             patch.object(host.release, "verify_index", return_value={"index_version": 2}) as verify:
            job = str(uuid.uuid4())
            destination = runner.stage_assets(self.blob(adapter), job)
        self.assertEqual(destination, self.assets / "releases" / job)
        self.assertTrue((destination / ".code-index/index.sqlite").is_file())
        self.assertEqual(len(executed), 1)
        verify.assert_called_once()

    def test_mismatch_preserves_current_release_and_never_calls_executor(self):
        old = str(uuid.uuid4())
        (self.assets / "releases" / old).mkdir()
        (self.assets / "current").symlink_to("releases/" + old)
        (self.source / "host-build-service/index-scanner.py").write_bytes(b"installed parser v1")
        runner = self.runner()
        executed = []
        runner.command = lambda *args, **kwargs: executed.append(args)
        with patch.object(host, "ASSET_ROOT", self.assets):
            with self.assertRaisesRegex(ValueError, "differs.*Install the reviewed Code Index parser.*restart"):
                runner.stage_assets(self.blob(b"candidate parser v2"), str(uuid.uuid4()))
        self.assertEqual(host.release.current_asset_id(self.assets), old)
        self.assertFalse(executed)
        self.assertEqual({p.name for p in (self.assets / "releases").iterdir()}, {old})


class InheritedSocketTests(unittest.IsolatedAsyncioTestCase):
    async def test_codex_and_webagent_share_work_registry(self):
        with tempfile.TemporaryDirectory() as temp:
            path = str(Path(temp) / "broker.sock")
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(path)
            listener.listen(4)
            broker = host.Broker(Path(temp) / "jobs", FakeRunner())
            service = asyncio.create_task(host.serve(broker, listener))

            async def local(name, arguments):
                reader, writer = await asyncio.open_unix_connection(path)
                writer.write(json.dumps({"id": "local-test", "name": name, "arguments": arguments}).encode() + b"\n")
                await writer.drain()
                response = json.loads(await reader.readline())
                writer.close()
                await writer.wait_closed()
                return response

            try:
                await asyncio.sleep(.02)
                own_session = str(uuid.uuid4())
                trusted = {"user_id": "v3-user", "session_id": own_session,
                           "lease_until": time.time() + 30, "access_fingerprint": "f" * 64}
                def app(action, task_id, **more):
                    return broker.handle({"name": "v3_build_work", "workspace": own_session,
                                          "trusted": trusted, "arguments": {"action": action,
                                          "task_id": task_id, **more}})
                self.assertEqual("success", (await local("v3_build_work", {
                    "action": "begin", "task_id": "codex-task", "summary": "Codex change"}))["status"])
                app("begin", "webagent-task", summary="WebAgent change")
                self.assertEqual(2, (await local("v3_build_work", {"action": "status"}))["output"]["open_count"])
                forbidden = await local("v3_worker_submit", {"operation": "deploy"})
                self.assertEqual("failed", forbidden["status"])
                digest = host.context_digest(broker.runner.snapshot())[0]
                first = await local("v3_build_work", {"action": "finish", "task_id": "codex-task",
                                                       "expected_inputs_sha256": digest})
                self.assertIsNone(first["output"]["job_id"])
                second = app("finish", "webagent-task", expected_inputs_sha256=digest)
                job_id = second["job_id"]
                self.assertEqual("success", (await local("v3_worker_job", {"job_id": job_id}))["status"])
                for _ in range(100):
                    result = broker.handle({"name": "v3_worker_job", "workspace": own_session,
                                            "trusted": trusted, "arguments": {"job_id": job_id}})
                    if result["status"] not in ("queued", "running"):
                        break
                    await asyncio.sleep(.01)
                self.assertEqual("completed", result["status"])
                self.assertEqual(2, len([c for c in broker.runner.calls if c[0] == "build"]))
            finally:
                service.cancel()
                try:
                    await service
                except asyncio.CancelledError:
                    pass
                listener.close()
                broker.shutdown()

    async def test_root_fd_address_needs_no_child_path_cleanup(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = os.open(temp, os.O_RDONLY | os.O_DIRECTORY)
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind("/proc/self/fd/" + str(directory) + "/broker.sock")
            listener.listen(1)
            os.close(directory)  # Closed across the broker's privilege-drop exec.
            broker = type("FixtureBroker", (), {"runner": FakeRunner()})()
            task = asyncio.create_task(host.serve(broker, listener))
            try:
                await asyncio.sleep(.02)
                self.assertFalse(task.done(), "inherited listener failed to start")
            finally:
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
                listener.close()


class RunnerSpaceTests(unittest.TestCase):
    def test_task_cargo_test_uses_binary_test_target(self):
        runner = host.Runner()
        with tempfile.TemporaryDirectory() as root, patch.object(runner, "command", return_value="ok") as command, patch.object(runner,"compiler_runtime",return_value={"image":"sha256:"+"7"*64}):
            host.release.extract(Path(root)/"source",context(),host.context_digest)
            self.assertEqual("ok", runner.cargo_task(Path(root)/"source", Path(root) / "checks", "test", 60))
        argv = command.call_args.args[0]
        self.assertEqual(["cargo", "test", "--locked", "--", "--test-threads=1"], argv[-5:])
        self.assertIn("CARGO_BUILD_JOBS=3",argv)
        self.assertIn("RUSTFLAGS=-C debuginfo=0 -C link-arg=-fuse-ld=mold",argv)
        self.assertIn("sha256:"+"7"*64,argv)

    def test_preflight_accepts_adequate_space_without_pruning(self):
        runner = host.Runner()
        with patch.dict(host.CONFIG, {"scheduler": {}}), \
                patch.object(host.shutil, "disk_usage", return_value=type("Usage", (), {"free": host.MIN_SEQUENCE_FREE})()), \
                patch.object(runner, "command") as command:
            self.assertEqual(host.MIN_SEQUENCE_FREE, runner.prepare_build_space())
        command.assert_not_called()

    def test_preflight_rejects_inadequate_space_without_pruning(self):
        runner = host.Runner()
        with patch.dict(host.CONFIG, {"scheduler": {}}), \
                patch.object(host.shutil, "disk_usage", return_value=type("Usage", (), {"free": host.MIN_SEQUENCE_FREE - 1})()), \
                patch.object(runner, "command") as command:
            with self.assertRaisesRegex(ValueError, "needs 5 GiB free"):
                runner.prepare_build_space()
        command.assert_not_called()


    def test_whole_build_uses_protected_disk_floor_at_exact_boundary(self):
        """The JSON floor governs initial admission; no fixed 16 GiB gate remains."""
        runner = host.Runner()
        self.assertEqual(host.MIN_SEQUENCE_FREE, 5 * 1024 ** 3)
        for minimum in (5 * 1024 ** 3, 6 * 1024 ** 3):
            with patch.dict(host.CONFIG, {"scheduler": {"disk_min_free_bytes": minimum}}), \
                    patch.object(host.shutil, "disk_usage", return_value=type("Usage", (), {"free": minimum})()):
                self.assertEqual(runner.prepare_build_space(), minimum)
            with patch.dict(host.CONFIG, {"scheduler": {"disk_min_free_bytes": minimum}}), \
                    patch.object(host.shutil, "disk_usage", return_value=type("Usage", (), {"free": minimum - 1})()):
                with self.assertRaisesRegex(ValueError, f"needs {minimum // (1024 ** 3)} GiB free"):
                    runner.prepare_build_space()


if __name__ == "__main__":
    unittest.main()
