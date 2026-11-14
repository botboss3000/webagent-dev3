"""Per-key operator identities: the forced-command shell's allowlist and the broker's identity rule."""
import asyncio
import importlib.util
import json
import socket
import tempfile
import unittest
from pathlib import Path

import test_admin_build as fixtures

host = fixtures.host
spec = importlib.util.spec_from_file_location("operator_shell", Path(__file__).resolve().parents[1] / "build-service" / "operator_shell.py")
shell = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shell)


class ShellTests(unittest.TestCase):
    def test_allowed_commands_pass_and_others_are_refused(self):
        self.assertEqual(["status"], shell.command("laptop2", "buildctl status"))
        self.assertEqual(["apply", "t", "--patch", "-", "--expected", "x", "--request-id", "r"],
                         shell.command("laptop2", "apply t --patch - --expected x --request-id r"))
        for refused in ("install-broker t", "workcopy t /tmp/x", "maintenance drain", "close t --message-file m", "", "sh -c id"):
            with self.assertRaises(ValueError, msg=refused):
                shell.command("laptop2", refused)

    def test_file_arguments_must_be_stdin(self):
        for refused in ("schedule --input /home/ar/.ssh/id_ed25519", "apply t --patch /etc/shadow --expected x --request-id r",
                        "schedule --input=/etc/passwd", "schedule --input - --input /etc/passwd"):
            with self.assertRaises(ValueError, msg=refused):
                shell.command("laptop2", refused)
        self.assertEqual(["schedule", "--input", "-"], shell.command("laptop2", "schedule --input -"))

    def test_identity_shape(self):
        for bad in ("", "Laptop", "a b", "x;y"):
            with self.assertRaises(ValueError):
                shell.command(bad, "status")


class BrokerIdentityTests(unittest.IsolatedAsyncioTestCase):
    async def request(self, payload):
        with tempfile.TemporaryDirectory() as temp:
            path = str(Path(temp) / "broker.sock")
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(path)
            listener.listen(4)
            broker = host.Broker(Path(temp) / "jobs", fixtures.FakeRunner())
            seen = {}
            original = broker.handle
            def handle(request):
                seen.update(request["trusted"])
                return original(request)
            broker.handle = handle
            service = asyncio.create_task(host.serve(broker, listener))
            try:
                await asyncio.sleep(.02)
                reader, writer = await asyncio.open_unix_connection(path)
                writer.write(json.dumps({"id": "operator-test", "name": "v3_build_work", "arguments": {"action": "status"}, **payload}).encode() + b"\n")
                await writer.drain()
                response = json.loads(await reader.readline())
                writer.close()
                return response, seen
            finally:
                service.cancel()
                broker.shutdown()

    async def test_listed_identity_is_used_and_unknown_is_refused(self):
        host.scheduler_module.LOCAL_OPERATORS.add("laptop2")
        try:
            response, trusted = await self.request({"operator": "laptop2"})
            self.assertEqual("success", response["status"], response)
            self.assertEqual("laptop2", trusted["user_id"])
            response, _ = await self.request({"operator": "stranger"})
            self.assertNotEqual("success", response["status"])
            response, trusted = await self.request({})
            self.assertEqual(host.scheduler_module.LOCAL_OPERATOR, trusted["user_id"])
        finally:
            host.scheduler_module.LOCAL_OPERATORS.discard("laptop2")


if __name__ == "__main__":
    unittest.main()
