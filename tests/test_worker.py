import asyncio
import importlib.util
import fcntl
import threading
import hashlib
import os
import sys
import subprocess
import tempfile
import unittest
from unittest import mock
import uuid
from pathlib import Path

spec = importlib.util.spec_from_file_location("worker", Path(__file__).parents[1] / "worker.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)



class WorkerTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        worker.ROOT = Path(self.tmp.name) / "workspaces"
        worker.ROOT.mkdir()
        worker.TEMPLATE = Path(self.tmp.name) / "source-template"
        worker.TEMPLATE.mkdir()
        (worker.TEMPLATE / "Cargo.toml").write_text("[package]\nname = \"test\"\nversion = \"0.1.0\"\n")
        (worker.TEMPLATE / "src").mkdir()
        (worker.TEMPLATE / "src/main.rs").write_text("fn main() {}\n")
        worker.CONTROL = Path(self.tmp.name) / "control"
        worker.CONTROL.mkdir()
        (worker.CONTROL / "dispatch.lock").touch()
        (worker.CONTROL / "shutoff").write_text("0")
        self.session = str(uuid.uuid4())

    def tearDown(self):
        self.tmp.cleanup()

    async def run_tool(self, name, args, timeout=120):
        return await worker.execute({"workspace": self.session, "name": name,
                                     "arguments": args, "timeout_seconds": timeout})

    async def run_command(self, argv, timeout=120):
        # The supervisor that still runs workspace seeding and Code Index scans.
        with (worker.CONTROL / "dispatch.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            return await worker.command(argv, worker.workspace_path(self.session), timeout, lock)

    async def test_code_index_private_dispatch_runs_under_supervisor(self):
        base=worker.workspace_path(self.session)
        (base/"main.rs").write_text("fn main() {}\n")
        result=await self.run_tool("code_index_scan",{"root":".","action":"discover"})
        self.assertEqual(result["status"],"success",result)
        self.assertTrue((base/".code-index").is_dir())

    async def test_quiesce_cancels_owned_work_and_waits_for_cleanup(self):
        started, cleaned = asyncio.Event(), asyncio.Event()
        async def operation(request, lock):
            fcntl.flock(lock, fcntl.LOCK_UN)
            if request["name"] == "slow":
                started.set()
                try:
                    await asyncio.sleep(30)
                finally:
                    await asyncio.sleep(0.01)
                    cleaned.set()
                return {"status":"success"}
            await worker.stop_workspace_operations(request["workspace"])
            return {"status":"success"}
        with mock.patch.object(worker, "execute_locked", side_effect=operation):
            task=asyncio.create_task(self.run_tool("slow",{}))
            await started.wait()
            result=await self.run_tool("administrator_quiesce",{})
            self.assertEqual(result["status"],"success")
            self.assertTrue(cleaned.is_set())
            self.assertTrue(task.cancelled())
            self.assertFalse(worker.ACTIVE_OPERATIONS)

    async def test_cancelled_synchronous_call_waits_for_thread_settlement(self):
        started, release, settled = threading.Event(), threading.Event(), threading.Event()
        def write():
            started.set()
            release.wait(2)
            settled.set()
        task=asyncio.create_task(worker.synchronous(write))
        await asyncio.to_thread(started.wait)
        task.cancel()
        await asyncio.sleep(0.01)
        self.assertFalse(task.done())
        release.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertTrue(settled.is_set())

    async def test_timeout(self):
        result = await self.run_command(["/bin/sh", "-c", "sleep 30"], 1)
        self.assertEqual(result["status"], "timeout")

    async def test_cancel_kills_descendant(self):
        task = asyncio.create_task(self.run_command(["/bin/sh", "-c", "sleep 2; touch survived"]))
        await asyncio.sleep(0.15)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        await asyncio.sleep(2.1)
        self.assertFalse((worker.ROOT / self.session / "survived").exists())

    async def test_cancel_kills_double_fork_new_session(self):
        code = (
            "import os,time\n"
            "if os.fork()==0:\n"
            " os.setsid()\n"
            " if os.fork()!=0: os._exit(0)\n"
            " time.sleep(2); open('detached-survived','w').write('bad'); os._exit(0)\n"
            "time.sleep(30)\n"
        )
        task = asyncio.create_task(self.run_command(["/usr/bin/python3", "-c", code]))
        await asyncio.sleep(0.2)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        await asyncio.sleep(2.1)
        self.assertFalse((worker.ROOT / self.session / "detached-survived").exists())

    async def test_completion_cleans_detached_child(self):
        code = "import os,time\nif os.fork()==0:\n os.setsid(); time.sleep(2); open('orphan-survived','w').write('bad')\n"
        result = await self.run_command(["/usr/bin/python3", "-c", code])
        self.assertEqual(result["status"], "success")
        await asyncio.sleep(2.1)
        self.assertFalse((worker.ROOT / self.session / "orphan-survived").exists())

    async def test_shutoff(self):
        (worker.CONTROL / "shutoff").write_text("1")
        with self.assertRaisesRegex(ValueError, "disabled"):
            await self.run_tool("terminal_list", {})

    async def test_output_is_bounded(self):
        result = await self.run_command(["/usr/bin/python3", "-c", "print('x' * 2000000)"])
        self.assertTrue(result["truncated"])
        self.assertLessEqual(len(result["output"]), worker.LIMIT)


if __name__ == "__main__":
    unittest.main()


class WorkerRpcEnvelopeTests(unittest.IsolatedAsyncioTestCase):
    async def client_response(self, version, payload):
        import json
        import struct
        reader = mock.Mock()
        reader.readline = mock.AsyncMock(return_value=json.dumps({"version": version, "id": "routing", "workspace": str(uuid.uuid4())}).encode() + b"\n")
        async def disconnected(_):
            await asyncio.sleep(3600)
        reader.read = disconnected
        writer = mock.Mock()
        writer.get_extra_info.return_value.getsockopt.return_value = struct.pack("3i", 1, 1100, 1100)
        writer.drain = mock.AsyncMock()
        writer.wait_closed = mock.AsyncMock()
        with mock.patch.object(worker, "execute", new=mock.AsyncMock(return_value=payload)) as execute:
            await worker.client(reader, writer)
        return json.loads(writer.write.call_args.args[0]), execute.call_count

    async def test_wire_envelope_preserves_root_and_nested_domain_ids(self):
        payload = {"status": "success", "id": "domain", "output": {"id": "nested", "timestamp": 7}}
        reply, calls = await self.client_response(2, payload)
        self.assertEqual(calls, 1)
        self.assertEqual(reply, {"version": 2, "id": "routing", "result": payload})

    async def test_old_protocol_is_rejected_before_execution(self):
        reply, calls = await self.client_response(1, {"status": "success"})
        self.assertEqual(calls, 0)
        self.assertEqual(reply["result"]["status"], "failed")
        self.assertIn("protocol 2 required", reply["result"]["output"])
