import asyncio,importlib.util,json,os,shutil,tempfile,unittest,uuid
from pathlib import Path
from unittest.mock import patch
import test_admin_build as fixtures
ROOT=Path(__file__).resolve().parents[1]
PACKAGE=ROOT/"plugins/abilities/Administrator/codebase_admin"
class PackageTests(unittest.TestCase):
    def test_complete_and_incompatible_candidates(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder=Path(temporary)/"package";shutil.copytree(PACKAGE,folder)
            self.assertEqual(len(fixtures.host.validate_codebase_package(folder)),64)
            for name in ("plugin.json","codebase_admin.json"):
                path=folder/name;old=path.read_bytes();value=json.loads(old)
                value["tools"]=["shell"];path.write_text(json.dumps(value))
                with self.assertRaises(ValueError):fixtures.host.validate_codebase_package(folder)
                path.write_bytes(old)
            path=folder/"runner.py";path.write_text("syntax error @")
            with self.assertRaises(SyntaxError):fixtures.host.validate_codebase_package(folder)
            path.unlink()
            with self.assertRaises(ValueError):fixtures.host.validate_codebase_package(folder)
    def test_dependency_lane_is_explicit(self):
        for name in ("runner.py","plugin.json","codebase_admin.skill.md","skills/rust_quality.skill.md"):
            self.assertTrue(fixtures.host.hot_source_path(fixtures.host.CODEBASE_ROOT+name))
        for name in ("worker.py","src/abilities.rs","build-service/host.py","plugins/abilities/Administrator/other/runner.py"):
            self.assertFalse(fixtures.host.hot_source_path("v3/"+name))
class ProcessBridgeTests(unittest.IsolatedAsyncioTestCase):
    async def test_worker_dispatch_uses_package_and_returns_bound_identity(self):
        spec=importlib.util.spec_from_file_location("codebase_worker",ROOT/"worker.py")
        worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)
        with tempfile.TemporaryDirectory() as temporary:
            path=Path(temporary)/"sample";path.write_text("bridge")
            result=await worker.codebase_call("file_read",{"path":str(path)},Path(temporary))
            self.assertEqual(result["output"],"bridge")
            self.assertEqual(len(result["implementation_sha256"]),64)
    async def test_invalid_protocol_reports_uncertain_writes(self):
        spec=importlib.util.spec_from_file_location("codebase_worker_bad",ROOT/"worker.py")
        worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)
        with tempfile.TemporaryDirectory() as temporary:
            folder=Path(temporary);(folder/"runner.py").write_text("print('{}')")
            with patch.object(worker,"codebase_package",return_value=(folder,"f"*64)):
                result=await worker.codebase_call("file_write",{},folder)
                self.assertTrue(result["uncertain"]);self.assertEqual(result["status"],"uncertain")
    async def test_cancellation_reaps_the_plugin(self):
        spec=importlib.util.spec_from_file_location("codebase_worker_cancel",ROOT/"worker.py")
        worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)
        with tempfile.TemporaryDirectory() as temporary:
            folder=Path(temporary);runner=folder/"runner.py"
            runner.write_text("import time; time.sleep(60)")
            with patch.object(worker,"codebase_package",return_value=(folder,"f"*64)):
                task=asyncio.create_task(worker.codebase_call("file_read",{},folder))
                await asyncio.sleep(.1);task.cancel()
                with self.assertRaises(asyncio.CancelledError):await asyncio.wait_for(task,3)
if __name__=="__main__":unittest.main()

class BrokerLaneTests(unittest.TestCase):
    setUp=fixtures.BuildTests.setUp
    tearDown=fixtures.BuildTests.tearDown
    call=fixtures.BuildTests.call
    wait=fixtures.BuildTests.wait
    def prepare(self,invalid=False):
        files={fixtures.host.CODEBASE_ROOT+str(path.relative_to(PACKAGE)):path.read_text()
               for path in PACKAGE.rglob("*") if path.is_file() and path.suffix in (".py",".md",".json")}
        self.runner.blob=fixtures.context(extra=files)
        self.runner.hot_assets=True
        self.runner.plugin_hot_swappable=self.runner.ui_hot_swappable
        task=self.call("v3_build_work",{"action":"begin","task_id":"plugin-fixture","summary":"Independent plugin","mode":"isolated"})
        folder=Path(task["workspace_path"])/"plugins/abilities/Administrator/codebase_admin"
        if invalid:(folder/"runner.py").write_text("bad syntax @")
        else:
            source=folder/"runner.py";source.write_text(source.read_text().replace('IMPLEMENTATION_VERSION = "','IMPLEMENTATION_VERSION = "fixture-',1))
        def validate(job_id,changed):
            root=Path(self.broker.jobs[job_id]["release_root"])/fixtures.host.CODEBASE_ROOT
            fixtures.host.validate_codebase_package(root)
        self.runner.validate_ui=validate
        digest=self.call("v3_build_work",{"action":"status","task_id":"plugin-fixture"})["own"][0]["workspace_inputs_sha256"]
        return self.call("v3_build_work",{"action":"finish","task_id":"plugin-fixture","expected_inputs_sha256":digest})
    def test_plugin_lane_preserves_unrelated_processes_and_images(self):
        result=self.wait(self.prepare())
        self.assertEqual(result["operation"],"plugin_deploy");self.assertEqual(result["status"],"completed")
        self.assertFalse([call for call in self.runner.calls if call[0] in ("build","stop","start","create","rename")])
    def test_invalid_candidate_never_activates(self):
        previous=self.runner.asset_id
        result=self.wait(self.prepare(invalid=True))
        self.assertEqual(result["status"],"failed")
        self.assertEqual(self.runner.asset_id,previous)
        self.assertFalse([call for call in self.runner.calls if call[0]=="activate_assets"])


class StableWorkerContractTests(unittest.TestCase):
    def test_hot_release_requires_installed_worker_protocol(self):
        runner=fixtures.host.Runner()
        runner.inspect=lambda name:{"Mounts":[{"Destination":"/opt/v3/assets","Source":str(fixtures.host.ASSET_ROOT),"RW":False}],
                                   "Config":{"Env":["V3_UI_ROOT=/opt/v3/assets/current/v3/ui"]}}
        def command(args,**kwargs):
            compile(args[-1],"<protocol probe>","exec")
            return json.dumps({"status":"success","output":{"protocol":1,"catalog":1,"dispatch":1}})
        runner.command=command
        self.assertTrue(runner.plugin_hot_swappable())
        runner.command=lambda *a,**k:json.dumps({"status":"failed","output":"unknown tool"})
        self.assertFalse(runner.plugin_hot_swappable())
