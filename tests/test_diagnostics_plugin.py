"""Native package supervisor contracts use real child processes and the existing browser mock."""
import asyncio,importlib.util,json,os,sys,tempfile,time,unittest,uuid
from pathlib import Path
from unittest import mock
ROOT=Path(__file__).parents[1]
spec=importlib.util.spec_from_file_location("diagnostics_worker_fixture",ROOT/"worker.py")
worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)
import test_admin_build as fixtures
class PublicationTests(unittest.TestCase):
    def test_complete_contract_and_executable_lane(self):
        folder=ROOT/"plugins/abilities/Administrator/diagnostics"
        fixtures.host.validate_diagnostics_package(folder)
        prefix=fixtures.host.DIAGNOSTICS_ROOT
        for name in ("adapter.py","runner.py","test_runner.py","plugin.json","diagnostics.skill.md"):
            self.assertTrue(fixtures.host.hot_source_path(prefix+name))
        self.assertFalse(fixtures.host.hot_source_path(prefix+"unreviewed.py"))
        with tempfile.TemporaryDirectory()as tmp:
            import shutil
            dest=Path(tmp)/"package";shutil.copytree(folder,dest)
            manifest=json.loads((dest/"plugin.json").read_text());manifest["protocol"]=True
            (dest/"plugin.json").write_text(json.dumps(manifest))
            with self.assertRaises(ValueError):fixtures.host.validate_diagnostics_package(dest)
class SupervisorTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);self.bridge=worker.DiagnosticsPackages()
        self.folder=self.root/"package";self.folder.mkdir()
        # The stub loads the actual package runner/adapter and injects only WebDriver.
        program="\n".join([
            "import asyncio,importlib.util,runpy,sys",
            "from pathlib import Path",
            "package=Path("+repr(str(ROOT/"plugins/abilities/Administrator/diagnostics"))+")",
            "spec=importlib.util.spec_from_file_location('runner',package/'runner.py')",
            "r=importlib.util.module_from_spec(spec);spec.loader.exec_module(r)",
            "m=r.adapter()",
            "mock=runpy.run_path(str(package/'test_runner.py'))",
            "def factory(*args):",
            " d=mock['Driver']();d.typed=[];return d",
            "m.PRIVATE_ROOT=Path("+repr(str(self.root/"private"))+")",
            "m._SERVICE=m.BrowserService(m.PRIVATE_ROOT,driver_factory=factory)",
            "r.adapter=lambda:m",
            "asyncio.run(r.serve())",
        ])+"\n"
        (self.folder/"runner.py").write_text(program)
        self.revision="old";self.hash="a"*64
        self.select=mock.patch.object(worker,"diagnostics_package",side_effect=lambda:(self.folder,self.revision,self.hash));self.select.start()
    async def asyncTearDown(self):
        for sid in list(self.bridge.sessions):await self.bridge.drop(sid)
        if self.bridge.reaper:
            self.bridge.reaper.cancel()
            await asyncio.gather(self.bridge.reaper,return_exceptions=True)
        self.select.stop();self.tmp.cleanup()
    def identity(self):
        sid=str(uuid.uuid4());base=self.root/sid;base.mkdir()
        return base,{"session_id":sid,"user_id":"admin","agent_id":"shared_default","generation":"fixture","cookie":str(uuid.uuid4()),"expires_at":time.time()+300,"allowed_session_ids":[sid]}
    async def call(self,base,trusted,name="dev_browser_action",**args):
        return await self.bridge.call(name,args,base,trusted)
    async def test_state_recording_hot_revision_and_stateless_artifact(self):
        base,trusted=self.identity()
        login=await self.call(base,trusted,"login_as_current_user")
        self.assertEqual(login["status"],"success",login);pid=self.bridge.sessions[base.name]["process"].pid
        self.revision="new";self.hash="b"*64
        read=await self.call(base,trusted,action="read");self.assertTrue(read["fresh_for_input"])
        self.assertEqual(read["plugin_revision"],"old")
        self.assertEqual((await self.call(base,trusted,action="click",selector="#go"))["status"],"success")
        started=await self.call(base,trusted,"dev_browser_record",action="start");self.assertTrue(started["recording"])
        await asyncio.sleep(1.2)
        stopped=await self.call(base,trusted,"dev_browser_record",action="stop")
        self.assertGreaterEqual(stopped["frames"],1,stopped)
        artifact=stopped["artifacts"][0]["artifact_id"]
        await self.call(base,trusted,action="close")
        self.assertNotIn(base.name,self.bridge.sessions)
        got=await self.call(base,trusted,"administrator_artifact_get",artifact_id=artifact)
        self.assertEqual(got["status"],"success",got);self.assertTrue(got["png_base64"])
        self.assertEqual(got["plugin_revision"],"new");self.assertEqual(len(self.bridge.sessions),0)
        self.assertEqual((await self.call(base,trusted,"login_as_current_user"))["plugin_revision"],"new")
        self.assertNotEqual(self.bridge.sessions[base.name]["process"].pid,pid)
    async def test_two_sessions_capacity_rotation_expiry_and_quiescence(self):
        first=self.identity();second=self.identity();third=self.identity()
        for pair in (first,second):self.assertEqual((await self.call(*pair,"login_as_current_user"))["status"],"success")
        rejected=await self.call(*third,"login_as_current_user")
        self.assertEqual(rejected["error_kind"],"browser_capacity")
        base,trusted=first;old=self.bridge.sessions[base.name]["process"]
        trusted["cookie"]=str(uuid.uuid4())
        self.assertEqual((await self.call(base,trusted,"login_as_current_user"))["status"],"success")
        self.assertIsNotNone(old.returncode)
        trusted["expires_at"]=time.time()-1
        self.assertEqual((await self.call(base,trusted,action="read"))["status"],"error")
        self.assertNotIn(base.name,self.bridge.sessions)
        base,trusted=second;old=self.bridge.sessions[base.name]["process"]
        await self.bridge.drop(base.name);self.assertIsNotNone(old.returncode)
    async def test_broken_transport_for_input_is_uncertain(self):
        base,trusted=self.identity();await self.call(base,trusted,"login_as_current_user")
        process=self.bridge.sessions[base.name]["process"]
        process.kill();await process.wait()
        # Break after admission, rather than silently starting a replacement.
        with mock.patch.object(self.bridge,"exchange",side_effect=EOFError):
            result=await self.call(base,trusted,action="click",selector="#go")
        self.assertEqual(result["status"],"uncertain",result);self.assertNotIn(base.name,self.bridge.sessions)
if __name__=="__main__":unittest.main()
