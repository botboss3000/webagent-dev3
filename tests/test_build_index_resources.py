"""Measured index execution retains exact container identity and recovery evidence."""
import importlib.util
from pathlib import Path
import tempfile
import threading
import unittest
import uuid
from unittest.mock import patch
import test_admin_build as fixtures


class MeasuredIndexExecutionTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name)
        (self.root/"index-scanner.py").write_text("# protected parser fixture\n")
        self.runner=fixtures.host.Runner.__new__(fixtures.host.Runner)
        self.runner.job_context=threading.local()
        self.runner.job_context.job={"job_id":str(uuid.uuid4()),"user_id":"operator","workspace":"fixture","internal_index":True}
        self.calls=[];self.cleaned=[];self.oom=False;self.identity="a"*64;self.fail_exec=False
        class Resources:
            enabled=True
            lock=threading.RLock()
            rows={}
            def enter(inner,name,job,phase,profile,kind,timeout):
                self.calls.append(("admit",name,phase,kind,timeout));inner.rows[name]={}
            def attached(inner,name,identity):self.calls.append(("attached",identity))
        self.runner.resources=Resources()
        class Executor:
            def finish_phase(inner,name,identity,success):self.cleaned.append((name,identity,success))
        self.runner.executor=Executor()
        def command(args,**kwargs):
            self.calls.append(("command",args,kwargs))
            if args[:2]==["docker","exec"]and self.fail_exec:raise TimeoutError("bounded scan")
            return "scan output"if args[:2]==["docker","exec"]else "untrusted creation chatter"
        def resource(args):
            if args["operation"]=="identity":return {"container_id":self.identity,"state":"created","phase":next(call[2]for call in reversed(self.calls)if call[0]=="admit")}
            return {"peak_memory":1234,"cpu_usec":234,"oom_events":int(self.oom),"oom_kills":0}
        self.runner.command=command;self.runner.resource_request=resource
        self.policy=patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"service_dir":str(self.root),"scheduler":{**fixtures.host.CONFIG.get("scheduler",{}),"component_indexing":True}})
        self.policy.start()
        self.argv=["docker","run","--rm","--network","none","--memory","1g","--entrypoint","python3","sha256:fixture","-B","-c","print('fixed recipe')"]

    def tearDown(self):self.policy.stop();self.tmp.cleanup()

    def test_admission_precedes_creation_and_exact_identity_precedes_execution(self):
        self.assertEqual(self.runner.index_command(self.argv),"scan output")
        self.assertEqual(self.calls[0][0],"admit")
        create=next(call[1]for call in self.calls if call[0]=="command"and call[1][:2]==["docker","create"])
        self.assertIn("--cgroup-parent",create);self.assertIn("--memory-swap",create)
        self.assertNotIn("--rm",create);self.assertEqual(create[-3:],["sleep","sha256:fixture","infinity"])
        self.assertTrue(any(call==("attached","a"*64)for call in self.calls))
        self.assertEqual(self.cleaned[0][1:],("a"*64,True))
        self.assertEqual(next(iter(self.runner.resources.rows.values()))["metrics"]["peak_memory"],1234)
        self.assertIsNone(self.runner.job_context.phase_container)

    def test_oom_is_not_successful_evidence(self):
        self.oom=True
        with self.assertRaisesRegex(RuntimeError,"memory budget"):self.runner.index_command(self.argv)
        self.assertFalse(self.cleaned[0][2])

    def test_timeout_cleans_only_verified_container(self):
        self.fail_exec=True
        with self.assertRaises(TimeoutError):self.runner.index_command(self.argv)
        self.assertEqual(self.cleaned[0][1:],("a"*64,False))
        self.assertIsNone(self.runner.job_context.phase_container)

    def test_invalid_creation_identity_retains_uncertain_outcome(self):
        self.identity="pull diagnostics\n"+"a"*64
        with self.assertRaisesRegex(ValueError,"identity"):self.runner.index_command(self.argv)
        self.assertEqual(self.cleaned[0][1:],(None,False))
        self.assertFalse(any(c[0]=="command"and c[1][:2]==["docker","start"]for c in self.calls))

    def test_legacy_policy_preserves_existing_execution_recipe(self):
        with patch.dict(fixtures.host.CONFIG["scheduler"],component_indexing=False):self.runner.index_command(self.argv)
        self.assertEqual(self.calls[0][0],"command");self.assertEqual(self.calls[0][1],self.argv)
        self.assertFalse(self.cleaned)

    def test_selected_frontend_tests_reuse_only_matching_dependency_runtime_and_recipe(self):
        (self.root/"ui_policy.py").write_text("# protected fixture recipe\n")
        candidate=self.root/"candidate";test=candidate/"v3/ui/a.test.cjs";test.parent.mkdir(parents=True);test.write_text("fixed fixture\n")
        evidence={"tests":["v3/ui/a.test.cjs"],"sha256":"b"*64}
        with patch.dict(fixtures.host.CONFIG["scheduler"],ui_test_image="sha256:"+"c"*64):
            first=self.runner.validate_selected_tests(self.runner.job_context.job["job_id"],candidate,evidence)
            self.runner.job_context.job["ui_test_evidence"]=first
            commands=len(self.calls)
            reused=self.runner.validate_selected_tests(self.runner.job_context.job["job_id"],candidate,evidence)
            self.assertTrue(reused["reused"]);self.assertEqual(len(self.calls),commands)
            refreshed=self.runner.validate_selected_tests(self.runner.job_context.job["job_id"],candidate,{**evidence,"sha256":"d"*64})
            self.assertFalse(refreshed["reused"]);self.assertGreater(len(self.calls),commands)
        create=next(call[1]for call in self.calls if call[0]=="command"and call[1][:2]==["docker","create"])
        self.assertIn("none",create);self.assertIn("--read-only",create);self.assertIn("--user",create)
        self.assertIn("type=bind,src="+str(candidate)+",dst=/candidate,readonly",create)
        command=next(call[1]for call in self.calls if call[0]=="command"and call[1][:2]==["docker","exec"])
        self.assertIn("--test-concurrency=1",command);self.assertIn("--experimental-vm-modules",command)

    def test_selected_frontend_tests_require_protected_pinned_runtime(self):
        with patch.dict(fixtures.host.CONFIG["scheduler"],ui_test_image=None):
            with self.assertRaisesRegex(ValueError,"pinned UI test runtime"):
                self.runner.validate_selected_tests("fixture",self.root,{"tests":["v3/ui/a.test.cjs"],"sha256":"b"*64})
        self.assertFalse(self.calls)

    def test_browser_admission_is_single_worker_while_index_work_can_proceed(self):
        import test_scheduler_resources as resource_fixture
        case=resource_fixture.ResourceTests();case.setUp()
        try:
            scheduler=case.scheduler
            scheduler.enter("browser-one",{"job_id":"one"},"ui_test","ui-runtime","ui_test",timeout=.02)
            with self.assertRaises(TimeoutError):
                scheduler.enter("browser-two",{"job_id":"two"},"ui_test","ui-runtime","ui_test",timeout=.02)
            scheduler.enter("private-index",{"job_id":"three","internal_index":True,"user_id":"operator","workspace":"fixture"},"index_scan","index-recipe","index",timeout=.02)
            self.assertEqual(scheduler.rows["private-index"]["owner_workspace"],"fixture")
            self.assertEqual(scheduler.rows["browser-one"]["component"],"ui-browser")
            self.assertEqual(scheduler.rows["private-index"]["component"],"index:three")
        finally:case.tearDown()

    def test_published_seed_mount_retains_its_receipt_release_basename(self):
        import json
        active=str(uuid.uuid4());assets=self.root/"assets";published=assets/"releases"/active;published.mkdir(parents=True)
        source=published/"v3";source.mkdir();(source/"fixture.py").write_text("value = 1\n")
        scanner=fixtures.host.maintenance_engine.load_scanner()
        scanner.build_published(published,published/".code-index",active,"a"*64)
        scanner.read_published(published,active)
        self.runner.inspect=lambda name:{"Image":"sha256:"+"f"*64}
        self.runner.current_asset_id=lambda:active
        observed=[]
        def scan(args,**kwargs):
            observed.append(args)
            output=Path(next(value.removesuffix(":/output:rw")for value in args if value.endswith(":/output:rw")))
            (output/"inventory.json").write_text(json.dumps([[],{},{}]))
            (output/"index.sqlite").write_bytes(b"fixture")
            (output/"seed.json").write_text("{}")
        self.runner.index_command=scan
        with patch.object(fixtures.host,"ASSET_ROOT",assets):
            result=self.runner.maintenance_inventory(self.root,"",self.root/"private-index")
        self.assertIn(str(published)+":/published/"+active+":ro",observed[0])
        self.assertIn(repr("/published/"+active),observed[0][-1])
        self.assertEqual(result[2]["parser_environment"],"sha256:"+"f"*64)


    def test_frontend_runtime_provisioning_is_measured_and_never_uses_application_context(self):
        import contextlib,json
        recipe=self.root/"ui-test.Dockerfile";recipe.write_text("FROM fixed-test-runtime\n")
        identity=str(uuid.uuid4());job={"job_id":identity,"package_memory_cap":4026531840};calls=[]
        @contextlib.contextmanager
        def packager(value,role):
            self.assertIs(value,job);self.assertEqual(role,"generic");calls.append("measured");yield "fixed-measured-builder"
        self.runner.executor.packager=packager
        def command(argv,**kwargs):
            calls.append(argv)
            if argv[:3]==["docker","buildx","build"]:Path(argv[argv.index("--iidfile")+1]).write_text("sha256:"+"d"*64)
            return ""
        self.runner.command=command
        with patch.object(fixtures.host,"STATE",self.root/"state"):
            result=self.runner.prepare_ui_test_runtime(job)
        build=next(call for call in calls if isinstance(call,list)and call[:3]==["docker","buildx","build"])
        self.assertIn("fixed-measured-builder",build);self.assertEqual({path.name for path in Path(build[-1]).iterdir()},{"Dockerfile","image.id"})
        self.assertEqual(result["image"],"sha256:"+"d"*64);self.assertFalse(result["application_images_changed"])


class HistoryMaintenanceAuthorityTests(unittest.TestCase):
    def setUp(self):
        import test_build_service as foundation
        self.foundation=foundation;foundation.MaintenanceTests.setUp(self)
        self.call("v3_build_work",{"action":"begin","task_id":"history-maintenance-task","summary":"Lossless maintenance fixture","mode":"isolated"})
        self.item=next(i for i in self.broker.work.values()if i["task_id"]=="history-maintenance-task")
        state=self.broker.scheduler.ensure(self.item,self.trusted,self.sid)
        self.call("build_work",{"action":"reserve","task_id":self.item["task_id"],"paths":["plugins/abilities/Administrator/code_index/maintenance.py"],"expected_revision":state["revision"],"expected_epoch":state["epoch"],"request_id":"reserve-history-maintenance"})
        self.args={"action":"compact_index_history","task_id":self.item["task_id"],"request_id":"history-maintenance-exact"}

    def tearDown(self):self.foundation.MaintenanceTests.tearDown(self)
    def call(self,*args,**kwargs):return self.foundation.MaintenanceTests.call(self,*args,**kwargs)

    def test_idle_owned_drain_and_exact_retry_preserve_review_history(self):
        with self.assertRaisesRegex(ValueError,"Idle task-owned drain"):self.call("build_work",self.args)
        self.call("build_service_maintenance",{"action":"drain","task_id":self.item["task_id"]})
        result=self.call("build_work",self.args)
        self.assertTrue(result["history_preserved"])
        self.assertEqual(result,self.call("build_work",self.args))
        self.assertEqual(len(list(self.broker.code_index_ledger.folder.glob("history-backup-*.sqlite"))),1)

    def test_application_and_active_review_cannot_compact(self):
        self.call("build_service_maintenance",{"action":"drain","task_id":self.item["task_id"]})
        with self.assertRaises(PermissionError):
            self.call("build_work",self.args,trusted={k:v for k,v in self.trusted.items()if k!="local_caller"})
        self.broker.code_index_leases["active-fixture"]={}
        with self.assertRaisesRegex(ValueError,"Idle task-owned drain"):self.call("build_work",self.args)
        self.broker.code_index_leases.clear()
        self.assertFalse(list(self.broker.code_index_ledger.folder.glob("history-backup-*")))

    def test_foreign_drain_preserves_owner_heartbeat_without_allowing_source_work(self):
        self.call("build_service_maintenance",{"action":"drain","task_id":"another-maintenance-task"})
        state=self.broker.scheduler.store.get(self.item["key"])
        result=self.call("build_work",{"action":"heartbeat","task_id":self.item["task_id"],"expected_revision":state["revision"],"expected_epoch":state["epoch"],"request_id":"heartbeat-during-foreign-drain"})
        self.assertTrue(result)
        with self.assertRaisesRegex(PermissionError,"drained"):
            self.call("build_work",{"action":"reserve","task_id":self.item["task_id"],"paths":["build-service/config.py"],"expected_revision":state["revision"],"expected_epoch":state["epoch"],"request_id":"source-work-during-foreign-drain"})

    def test_runtime_provisioning_requires_idle_drain_and_retains_exact_job_receipt(self):
        import threading
        state=self.broker.scheduler.store.get(self.item["key"])
        self.call("build_work",{"action":"reserve","task_id":self.item["task_id"],"paths":["build-service/ui-test.Dockerfile"],"expected_revision":state["revision"],"expected_epoch":state["epoch"],"request_id":"runtime-provision-broker-reservation"})
        args={"action":"prepare_ui_test_runtime","task_id":self.item["task_id"],"request_id":"fixed-runtime-provision-request"}
        with self.assertRaisesRegex(ValueError,"Idle task-owned drain"):self.call("build_work",args)
        self.call("build_service_maintenance",{"action":"drain","task_id":self.item["task_id"]})
        self.runner.job_context=threading.local();self.runner.prepare_ui_test_runtime=lambda job:{"image":"sha256:"+"d"*64,"application_images_changed":False}
        service=Path(self.temp.name)/"protected-recipe";service.mkdir();(service/"ui-test.Dockerfile").write_text("FROM fixed-test-runtime\n")
        with patch.object(self.broker.resources,"enabled",True),patch.dict(fixtures.host.CONFIG,service_dir=str(service)):
            result=self.call("build_work",args);retry=self.call("build_work",args)
        self.assertEqual(result,retry);self.assertEqual(result["status"],"completed")
        self.assertFalse(self.broker.active_jobs);self.assertFalse(any(c[0]in {"build","stop","start"}for c in self.runner.calls))
