"""Executable-package regression checks; run before atomic activation."""
import importlib.util,json,subprocess,tempfile,unittest,hashlib,os
from pathlib import Path
from unittest.mock import patch
ROOT=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location("codebase_runner",ROOT/"runner.py")
runner=importlib.util.module_from_spec(spec);spec.loader.exec_module(runner)
class PluginTests(unittest.TestCase):
    def test_effects_and_task_scope_are_separate(self):
        self.assertEqual(runner.route("file_write",{"task_id":"task-a","content":"x"})["route"],"broker")
        self.assertEqual(runner.route("build_work",{"action":"finish"})["route"],"broker")
        self.assertEqual(runner.route("prompt_slot_write",{})["route"],"core")
        for tool in ("file_write","file_read"):
            with self.assertRaises(ValueError):runner.route(tool,{"request_id":"task-a"})
        with self.assertRaises(ValueError):runner.route("docker",{})
        with self.assertRaises(ValueError):runner.route("shell",{"task_id":"task-a"})
    def test_process_file_journey_and_contract(self):
        with tempfile.TemporaryDirectory() as temporary:
            def call(name,args,version=1):
                result=subprocess.run(["python3","-I","-B",str(ROOT/"runner.py")],
                    input=json.dumps({"version":version,"name":name,"arguments":args,"base":temporary}).encode(),
                    stdout=subprocess.PIPE,check=True,timeout=5)
                envelope=json.loads(result.stdout);self.assertEqual(envelope["version"],1);return envelope["result"]
            path=str(Path(temporary)/"sample.txt")
            self.assertEqual(call("file_write",{"path":path,"content":"before\n"})["status"],"success")
            self.assertEqual(call("file_read",{"path":path})["output"],"before\n")
            self.assertEqual(call("file_edit",{"path":path,"old_text":"before","new_text":"after","expected_sha256":"0"*64})["status"],"failed")
            self.assertEqual(call("file_edit",{"path":path,"old_text":"before","new_text":"after"})["status"],"success")
            self.assertIn("after",call("file_search",{"path":path,"query":"after"})["output"])
            self.assertIn("sample.txt",call("file_list",{"path":temporary})["output"])
            self.assertEqual(call("file_read",{"path":path},2)["status"],"failed")
            self.assertEqual(call("file_read",{"path":path},True)["status"],"failed")
    def test_atomic_write_failure_preserves_original(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);path=root/"sample";path.write_text("original")
            with patch.object(runner.os,"replace",side_effect=OSError("injected")):
                with self.assertRaises(OSError):runner.file_operation({},root,{"path":str(path),"content":"replacement"},"file_write")
            self.assertEqual(path.read_text(),"original");self.assertFalse(list(root.glob(".v3-write-*")))
    def test_plan_command_is_fixed_and_state_has_no_plugin_effect(self):
        for tool,args,executable in (("shell",{"command":"true"},"/bin/sh"),("python",{"code":"pass"},"/usr/bin/python3")):
            result=runner.invoke({"version":1,"operation":"plan","name":tool,"arguments":args})
            self.assertEqual(result["output"]["argv"][0],executable)
        with self.assertRaises(ValueError):runner.invoke({"version":1,"name":"prompt_slot_write","arguments":{}})
if __name__=="__main__":unittest.main()
