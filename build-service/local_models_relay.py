"""Local models relay: lets the app reach self-hosted models on the private network without opening the firewall.

The app's containers may not connect to Tailscale addresses (the host firewall drops it), so this root
service on the host forwards their decision requests. It listens on a Unix socket in the runtime tree the
containers mount; the kernel names each caller's Linux user (SO_PEERCRED) and only the app's user is served.

Which models exist, where they answer and which key file they use come from a root-owned registry the app
cannot write; the keys stay in root-only files and never enter the app. A request names a registered model
id, never an address. One JSON request per line, one JSON reply per line:

    {"op":"models"}                                   -> {"ok":true,"models":[{id,name,capabilities,api,
                                                          max_input_tokens,needs_instructions,up}]}
    {"op":"decide","model":ID,"body":{state,questions}} -> {"ok":true,"answer":{...}}
    failure                                           -> {"ok":false,"kind":KIND,"error":TEXT[,"status":N]}

KIND is unknown_model, unavailable, timeout, rejected (the model answered with an HTTP error, in status),
too_large or invalid (a malformed request).
"""
import ipaddress
import json
import os
import socket
import struct
import sys
import threading
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REQUEST_LIMIT = 64 * 1024
RESPONSE_LIMIT = 512 * 1024
UPSTREAM_TIMEOUT = 30
HEALTH_TIMEOUT = 2
CAPABILITIES = {"decision", "text"}
APIS = {"system_one", "openai_chat"}
ID_CHARS = set("abcdefghijklmnopqrstuvwxyz0123456789._-")
TAILSCALE = ipaddress.ip_network("100.64.0.0/10")


class Failure(Exception):
    def __init__(self, kind, error, status=None):
        super().__init__(error)
        self.kind, self.status = kind, status


def url_allowed(url):
    """Only https addresses, or plain http to a Tailscale address, may be registered."""
    parts = urllib.parse.urlsplit(url)
    if parts.scheme == "https" and parts.hostname:
        return True
    if parts.scheme != "http" or not parts.hostname:
        return False
    try:
        return ipaddress.ip_address(parts.hostname) in TAILSCALE
    except ValueError:
        return False


def parse_registry(text):
    """The registry's models by id; raises ValueError naming the first problem."""
    data = json.loads(text)
    if not isinstance(data, dict) or data.get("schema_version") != 1 or not isinstance(data.get("models"), list):
        raise ValueError("registry needs schema_version 1 and a models list")
    models = {}
    for entry in data["models"]:
        if not isinstance(entry, dict):
            raise ValueError("each model is an object")
        model_id = entry.get("id")
        if not isinstance(model_id, str) or not 1 <= len(model_id) <= 64 or not set(model_id) <= ID_CHARS:
            raise ValueError(f"bad model id {model_id!r}")
        if model_id in models:
            raise ValueError(f"duplicate model id {model_id}")
        capabilities = entry.get("capabilities")
        if not isinstance(capabilities, list) or not capabilities or not set(capabilities) <= CAPABILITIES:
            raise ValueError(f"{model_id}: capabilities must be some of {sorted(CAPABILITIES)}")
        if entry.get("api") not in APIS:
            raise ValueError(f"{model_id}: api must be one of {sorted(APIS)}")
        if not isinstance(entry.get("url"), str) or not url_allowed(entry["url"]):
            raise ValueError(f"{model_id}: url must be https, or http to a Tailscale address")
        key_file = entry.get("key_file")
        if not isinstance(key_file, str) or not key_file or "/" in key_file or key_file.startswith("."):
            raise ValueError(f"{model_id}: key_file must be a file name in the key folder")
        limit = entry.get("max_input_tokens")
        if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= 1_000_000:
            raise ValueError(f"{model_id}: max_input_tokens must be a positive integer")
        health = entry.get("health_path", "/health")
        if not isinstance(health, str) or not health.startswith("/") or len(health) > 64 or "?" in health:
            raise ValueError(f"{model_id}: health_path must be a path such as /health")
        models[model_id] = {
            "id": model_id, "name": str(entry.get("name") or model_id)[:80], "capabilities": capabilities,
            "api": entry["api"], "url": entry["url"].rstrip("/"), "upstream_model": str(entry.get("upstream_model") or model_id),
            "key_file": key_file, "max_input_tokens": limit, "needs_instructions": entry.get("needs_instructions") is True,
            "health_path": health,
        }
    return models


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


class Relay:
    """The registry and the two operations; the socket loop below only authenticates and frames requests."""

    def __init__(self, registry_path, key_dir, log=lambda line: print(line, file=sys.stderr, flush=True)):
        self.registry_path, self.key_dir, self.log = Path(registry_path), Path(key_dir), log
        self.lock = threading.Lock()
        self.models, self.stamp = {}, None

    def registry(self):
        """The models, re-read when the file changes; a bad or missing file keeps the last good registry."""
        try:
            stat = self.registry_path.stat()
            stamp = (stat.st_mtime_ns, stat.st_size, stat.st_ino)
        except OSError as error:
            stamp = ("missing", str(error))
        with self.lock:
            if stamp != self.stamp:
                self.stamp = stamp
                try:
                    self.models = parse_registry(self.registry_path.read_text())
                    self.log(f"registry loaded: {', '.join(self.models) or 'no models'}")
                except (OSError, ValueError) as error:
                    self.log(f"registry not loaded, keeping the last good one: {error}")
            return dict(self.models)

    def key(self, model):
        try:
            return (self.key_dir / model["key_file"]).read_text().strip()
        except OSError:
            raise Failure("unavailable", f"{model['id']}: its key file cannot be read") from None

    def fetch(self, model, method, path, payload=None, timeout=None):
        timeout = timeout or UPSTREAM_TIMEOUT
        request = urllib.request.Request(model["url"] + path, data=payload, method=method,
                                         headers={"Authorization": "Bearer " + self.key(model), "Content-Type": "application/json"})
        try:
            with OPENER.open(request, timeout=timeout) as response:
                body = response.read(RESPONSE_LIMIT + 1)
        except urllib.error.HTTPError as error:
            with error:
                detail = error.read(300).decode("utf-8", "replace")
            raise Failure("rejected", f"{model['id']} answered HTTP {error.code}: {detail}", error.code) from None
        except (TimeoutError, socket.timeout):
            raise Failure("timeout", f"{model['id']} did not answer within {timeout} s") from None
        except urllib.error.URLError as error:
            if isinstance(error.reason, (TimeoutError, socket.timeout)):
                raise Failure("timeout", f"{model['id']} did not answer within {timeout} s") from None
            raise Failure("unavailable", f"{model['id']} cannot be reached: {error.reason}") from None
        except OSError as error:
            raise Failure("unavailable", f"{model['id']} cannot be reached: {error}") from None
        if len(body) > RESPONSE_LIMIT:
            raise Failure("too_large", f"{model['id']} answered more than {RESPONSE_LIMIT} bytes")
        return body

    def up(self, model):
        try:
            self.fetch(model, "GET", model["health_path"], timeout=HEALTH_TIMEOUT)
            return True
        except Failure:
            return False

    def list_models(self):
        models = list(self.registry().values())
        with ThreadPoolExecutor(max_workers=max(1, min(8, len(models)))) as pool:
            states = list(pool.map(self.up, models))
        keys = ("id", "name", "capabilities", "api", "max_input_tokens", "needs_instructions")
        return [{**{key: model[key] for key in keys}, "up": up} for model, up in zip(models, states)]

    def decide(self, model_id, body):
        model = self.registry().get(model_id) if isinstance(model_id, str) else None
        if model is None:
            raise Failure("unknown_model", f"no registered model {model_id!r}")
        if "decision" not in model["capabilities"] or model["api"] != "system_one":
            raise Failure("unknown_model", f"{model_id} is not a decision model")
        if not isinstance(body, dict) or not isinstance(body.get("questions"), dict) or "state" not in body:
            raise Failure("invalid", "body needs state and questions")
        payload = json.dumps({**body, "model": model["upstream_model"]}).encode()
        try:
            return json.loads(self.fetch(model, "POST", "/v1/systemone", payload))
        except ValueError:
            raise Failure("unavailable", f"{model_id} answered something that is not JSON") from None

    def handle(self, raw):
        """One request line to one reply object."""
        try:
            if len(raw) > REQUEST_LIMIT or not raw.endswith(b"\n"):
                raise Failure("too_large", f"request over {REQUEST_LIMIT} bytes or unterminated")
            try:
                request = json.loads(raw)
            except ValueError:
                raise Failure("invalid", "request is not JSON") from None
            op = request.get("op") if isinstance(request, dict) else None
            if op == "models":
                return {"ok": True, "models": self.list_models()}
            if op == "decide":
                return {"ok": True, "answer": self.decide(request.get("model"), request.get("body"))}
            raise Failure("invalid", "op must be models or decide")
        except Failure as failure:
            reply = {"ok": False, "kind": failure.kind, "error": str(failure)[:400]}
            if failure.status is not None:
                reply["status"] = failure.status
            return reply


def serve(relay, socket_path, allowed_uids):
    socket_path = Path(socket_path)
    socket_path.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    os.chmod(socket_path.parent, 0o755)
    socket_path.unlink(missing_ok=True)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(socket_path))
    # Anyone may connect; the peer's user decides whether anything is answered.
    os.chmod(socket_path, 0o666)
    listener.listen(16)
    relay.registry()

    def answer(connection):
        try:
            _, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if uid not in allowed_uids:
                reply = {"ok": False, "kind": "invalid", "error": "This user may not use the local models relay"}
            else:
                reply = relay.handle(connection.makefile("rb").readline(REQUEST_LIMIT + 1))
            connection.settimeout(10)
            connection.sendall(json.dumps(reply).encode() + b"\n")
        except OSError:
            pass
        finally:
            connection.close()

    while True:
        connection, _ = listener.accept()
        connection.settimeout(5)
        threading.Thread(target=answer, args=(connection,), daemon=True).start()


if __name__ == "__main__":
    # The installed broker settings name the app's user and the runtime tree the containers mount.
    settings = json.loads(Path(os.environ.get("WEBAGENT_BUILD_CONFIG", "/etc/webagent/development/build-service.json")).read_text())
    instance = settings.get("instance", "development")
    serve(Relay(f"/etc/webagent/{instance}/local-models.json", f"/etc/webagent/{instance}/local-models"),
          Path(settings["runtime_root"]).parent / "local-models" / "relay.sock", {settings["app_uid"]})
