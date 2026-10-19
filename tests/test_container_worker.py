"""Deployment acceptance tests; run inside v3 container as uid 1100."""
import asyncio
import json
import os
import unittest
import uuid
from pathlib import Path

SOCKET = "/run/v3/worker/tools.sock"


class ContainerWorkerTests(unittest.IsolatedAsyncioTestCase):
    async def test_worker_identity_and_live_source_access(self):
        if os.getuid() != 1100:
            self.skipTest("run as application uid 1100 inside v3 container")
        app_pid = next(int(p.name) for p in Path("/proc").iterdir() if p.name.isdigit() and (p / "comm").read_text().strip() == "webagent-v3")
        reader, writer = await asyncio.open_unix_connection(SOCKET)
        request = {"version": 2, "id": "isolation-test", "workspace": str(uuid.uuid4()),
                   "name": "python", "arguments": {"code": (
                       "import os,json; "
                       "result={'uid':os.getuid(),'groups':os.getgroups()}; "
                       "\ntry: os.listdir('/state'); result['state_access']=True"
                       "\nexcept PermissionError: result['state_access']=False"
                       "\ntry: os.close(os.open('/run/v3/control/shutoff',os.O_WRONLY)); result['control_write']=True"
                       "\nexcept PermissionError: result['control_write']=False"
                       f"\ntry: open('/proc/{app_pid}/environ','rb'); result['app_environ_access']=True"
                       "\nexcept PermissionError: result['app_environ_access']=False"
                       "\nresult['source_access']=os.path.isfile('/live-checkout/Cargo.toml') and os.path.isdir('/live-checkout/src')"
                       "\nresult['v2_source_access']=os.path.exists('/live-checkout/app') or os.path.exists('/live-checkout/v3')"
                       "\nresult['secret_env']=any(k.startswith('V3_ADMIN') for k in os.environ)"
                       "\nprint(json.dumps(result))")},
                   "timeout_seconds": 10}
        writer.write((json.dumps(request) + "\n").encode())
        await writer.drain()
        envelope = json.loads(await reader.readline())
        self.assertEqual(envelope["version"], 2)
        self.assertEqual(envelope["id"], request["id"])
        response = envelope["result"]
        writer.close()
        await writer.wait_closed()
        self.assertEqual(response["status"], "success", response)
        result = json.loads(response["output"])
        self.assertEqual(result["uid"], 1000)
        # launch.py retains the checkout group; application state stays inaccessible.
        self.assertEqual(result["groups"], [1001])
        self.assertTrue(result["source_access"])
        self.assertFalse(result["v2_source_access"])
        self.assertFalse(result["state_access"])
        self.assertFalse(result["control_write"])
        self.assertFalse(result["secret_env"])
        self.assertFalse(result["app_environ_access"])

    async def test_non_application_peer_rejected(self):
        if os.getuid() == 1100:
            self.skipTest("run separately as checkout-owner uid 1000")
        reader, writer = await asyncio.open_unix_connection(SOCKET)
        writer.write((json.dumps({"version":2,"id":"forbidden","name":"shell",
            "workspace":str(uuid.uuid4()),"arguments":{"command":"printf bypass"}}) + "\n").encode())
        try:
            await writer.drain()
            response = await reader.readline()
            self.assertEqual(response, b"")
        except (ConnectionResetError, BrokenPipeError):
            pass
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except (ConnectionResetError, BrokenPipeError):
                pass


if __name__ == "__main__":
    unittest.main()
