"""Tests for the local models relay: a fake model server on a local port stands in for the sidekick."""
import importlib.util
import json
import os
import socket
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

spec = importlib.util.spec_from_file_location("local_models_relay", Path(__file__).resolve().parents[1] / "build-service" / "local_models_relay.py")
relay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(relay)


class FakeModel(BaseHTTPRequestHandler):
    """Answers like a Jev-compatible server; the test sets `mode` to make it misbehave."""
    mode = "ok"
    seen = []

    def log_message(self, *args):
        pass

    def reply(self, status, body, headers=()):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def authorized(self):
        if self.headers.get("Authorization") != "Bearer secret-key":
            self.reply(401, {"detail": "bad key"})
            return False
        return True

    def do_GET(self):
        if self.authorized():
            self.reply(200, {"ok": True})

    def do_POST(self):
        if not self.authorized():
            return
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        FakeModel.seen.append(body)
        if self.mode == "slow":
            time.sleep(1.5)
        if self.mode == "redirect":
            return self.reply(307, b"", [("Location", "http://127.0.0.1:1/elsewhere")])
        if self.mode == "huge":
            return self.reply(200, b"[" + b"0," * relay.RESPONSE_LIMIT + b"0]")
        if self.mode == "422":
            return self.reply(422, {"detail": "question 'asked': no 'instructions'"})
        self.reply(200, {"model": body["model"], "answers": {"asked": {"choice": "requested"}}})


def registry(port, **extra):
    model = {"id": "laya", "name": "Laya", "capabilities": ["decision"], "api": "system_one",
             "url": f"http://127.0.0.1:{port}", "upstream_model": "english", "key_file": "laya.key",
             "max_input_tokens": 512, "needs_instructions": True, **extra}
    return json.dumps({"schema_version": 1, "models": [model]})


BODY = {"state": {"x": 1}, "questions": {"asked": {"type": "noul"}}}


class RelayTest(unittest.TestCase):
    def setUp(self):
        FakeModel.mode, FakeModel.seen = "ok", []
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), FakeModel)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.port = self.server.server_address[1]
        self.dir = tempfile.TemporaryDirectory()
        root = Path(self.dir.name)
        (root / "keys").mkdir()
        (root / "keys" / "laya.key").write_text("secret-key\n")
        self.registry = root / "local-models.json"
        self.registry.write_text(registry(self.port))
        self.logs = []
        self.relay = relay.Relay(self.registry, root / "keys", log=self.logs.append)
        # The fake model listens on 127.0.0.1, which the real address rule refuses.
        patcher = mock.patch.object(relay, "url_allowed", lambda url: url.startswith("http://127.0.0.1:"))
        patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.dir.cleanup()

    def call(self, request):
        return self.relay.handle(json.dumps(request).encode() + b"\n")

    def test_decide_forwards_with_key_and_upstream_model(self):
        reply = self.call({"op": "decide", "model": "laya", "body": {**BODY, "model": "spoofed"}})
        self.assertEqual(reply, {"ok": True, "answer": {"model": "english", "answers": {"asked": {"choice": "requested"}}}})
        self.assertEqual(FakeModel.seen[0]["model"], "english")

    def test_models_lists_registry_with_up_state(self):
        reply = self.call({"op": "models"})
        self.assertTrue(reply["ok"])
        self.assertEqual(reply["models"], [{"id": "laya", "name": "Laya", "capabilities": ["decision"], "api": "system_one",
                                            "max_input_tokens": 512, "needs_instructions": True, "up": True}])
        self.assertNotIn("url", reply["models"][0])

    def test_models_reports_down_and_bad_key(self):
        (Path(self.dir.name) / "keys" / "laya.key").write_text("wrong")
        self.assertFalse(self.call({"op": "models"})["models"][0]["up"])
        self.server.shutdown()
        self.server.server_close()
        self.assertFalse(self.call({"op": "models"})["models"][0]["up"])

    def test_unknown_model_and_non_decision_model(self):
        self.assertEqual(self.call({"op": "decide", "model": "nope", "body": BODY})["kind"], "unknown_model")
        self.registry.write_text(registry(self.port, capabilities=["text"]))
        self.assertEqual(self.call({"op": "decide", "model": "laya", "body": BODY})["kind"], "unknown_model")

    def test_upstream_errors_map_to_kinds(self):
        FakeModel.mode = "422"
        reply = self.call({"op": "decide", "model": "laya", "body": BODY})
        self.assertEqual((reply["kind"], reply["status"]), ("rejected", 422))
        FakeModel.mode = "redirect"
        reply = self.call({"op": "decide", "model": "laya", "body": BODY})
        self.assertEqual((reply["kind"], reply["status"]), ("rejected", 307))
        FakeModel.mode = "huge"
        self.assertEqual(self.call({"op": "decide", "model": "laya", "body": BODY})["kind"], "too_large")
        (Path(self.dir.name) / "keys" / "laya.key").write_text("wrong")
        FakeModel.mode = "ok"
        reply = self.call({"op": "decide", "model": "laya", "body": BODY})
        self.assertEqual((reply["kind"], reply["status"]), ("rejected", 401))

    def test_timeout_and_unreachable(self):
        FakeModel.mode = "slow"
        with mock.patch.object(relay, "UPSTREAM_TIMEOUT", 0.5):
            self.assertEqual(self.call({"op": "decide", "model": "laya", "body": BODY})["kind"], "timeout")
        self.server.shutdown()
        self.server.server_close()
        self.assertEqual(self.call({"op": "decide", "model": "laya", "body": BODY})["kind"], "unavailable")

    def test_request_limits_and_malformed_requests(self):
        big = json.dumps({"op": "decide", "model": "laya", "body": {"state": "x" * relay.REQUEST_LIMIT, "questions": {}}})
        self.assertEqual(self.relay.handle(big.encode() + b"\n")["kind"], "too_large")
        self.assertEqual(self.relay.handle(b'{"op":"models"}')["kind"], "too_large")
        self.assertEqual(self.relay.handle(b"not json\n")["kind"], "invalid")
        self.assertEqual(self.call({"op": "shell"})["kind"], "invalid")
        self.assertEqual(self.call({"op": "decide", "model": "laya", "body": {"state": 1}})["kind"], "invalid")

    def test_registry_reload_keeps_last_good(self):
        self.assertIn("laya", self.relay.registry())
        self.registry.write_text("{broken")
        os.utime(self.registry, ns=(time.time_ns(), time.time_ns() + 10**9))
        self.assertIn("laya", self.relay.registry())
        self.assertIn("keeping the last good one", self.logs[-1])
        self.registry.write_text(registry(self.port, id="kev-0.8b"))
        self.assertEqual(list(self.relay.registry()), ["kev-0.8b"])

    def test_registry_validation(self):
        bad =[{"url": "http://10.0.0.5:8000"}, {"url": "file:///etc/passwd"}, {"key_file": "../x"}, {"id": "Bad Id"},
               {"capabilities": ["chat"]}, {"api": "raw"}, {"max_input_tokens": 0}, {"health_path": "health"}]
        for change in bad:
            with self.subTest(change=change), mock.patch.object(relay, "url_allowed", RealUrlAllowed):
                with self.assertRaises(ValueError):
                    relay.parse_registry(registry(self.port, **change))

    def test_address_rule(self):
        self.assertTrue(RealUrlAllowed("http://100.126.37.96:8000"))
        self.assertTrue(RealUrlAllowed("https://api.example.test"))
        for url in ("http://127.0.0.1:8000", "http://192.168.1.2", "http://example.test", "ftp://100.126.37.96"):
            self.assertFalse(RealUrlAllowed(url), url)


RealUrlAllowed = relay.url_allowed


class SocketTest(unittest.TestCase):
    def serve(self, uids):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        path = Path(folder.name) / "local-models" / "relay.sock"
        empty = Path(folder.name) / "registry.json"
        empty.write_text('{"schema_version":1,"models":[]}')
        threading.Thread(target=relay.serve, args=(relay.Relay(empty, folder.name, log=lambda _: None), path, uids), daemon=True).start()
        for _ in range(100):
            if path.exists():
                break
            time.sleep(0.02)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.connect(str(path))
            connection.sendall(b'{"op":"models"}\n')
            return json.loads(connection.makefile("rb").readline()), oct(path.stat().st_mode & 0o777)

    def test_only_allowed_user_is_served(self):
        reply, mode = self.serve({os.getuid()})
        self.assertEqual((reply, mode), ({"ok": True, "models": []}, "0o666"))
        reply, _ = self.serve({os.getuid() + 1})
        self.assertFalse(reply["ok"])
        self.assertIn("may not use", reply["error"])


if __name__ == "__main__":
    unittest.main()
