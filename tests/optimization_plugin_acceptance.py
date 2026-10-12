"""Real isolated API/loop and executable-plugin lifecycle acceptance.

Run on the v3 server with compiled application/plugin/test binaries. No live
package or runtime state is touched. This does not replace model-loop acceptance
or the separate real-Docker broker revocation acceptance.
"""
import argparse
import json
import os
import pathlib
import secrets
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request


def eventually(predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            result = predicate()
            if result:
                return result
        except (OSError, ValueError, AssertionError):
            pass
        time.sleep(.1)
    raise AssertionError("bounded lifecycle checkpoint did not become true")


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def acceptance(binary, plugin_binary, test_binary, source):
    with tempfile.TemporaryDirectory(prefix="v3-optimization-acceptance-") as tmp:
        root = pathlib.Path(tmp)
        state = root / "state"
        plugins = root / "plugins"
        plugins.mkdir(mode=0o700)
        control = root / "control"
        control.mkdir(mode=0o700)
        (control / "dispatch.lock").write_bytes(b"")
        (control / "shutoff").write_bytes(b"0")
        state.mkdir(mode=0o700)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        origin = "http://127.0.0.1:" + str(port)
        env = os.environ.copy()
        env.update(V3_STATE_ROOT=str(state), V3_PLUGIN_TEST_ROOT=str(plugins),
                   V3_ABILITIES_ROOT=str(source / "plugins/abilities"),
                   V3_CONTEXT_COMPACTION_CONFIG_PATH=str(source / "config/context-compaction.json"),
                   V3_UI_ROOT=str(source / "ui"),
                   V3_CONTROL_ROOT=str(control), V3_LISTEN="127.0.0.1:" + str(port),
                   V3_SECURE_COOKIE="false", V3_TOOL_SOCKET=str(root / "absent-worker.sock"),
                   V3_PROMOTE_SOCKET=str(root / "absent-promotion.sock"))
        processes = []
        logs = []
        cookie = ""
        csrf = ""
        settings = {"priority": "memory", "attempts": 2, "total_seconds": 1800,
                    "profiling": "cpu", "cpu_cores": 2, "ram_mb": 2048,
                    "coverage": "extended", "max_slowdown_percent": 5,
                    "max_memory_regression_percent": 7, "allow_dependencies": True}
        def request(path, payload=None, method=None):
            data = None if payload is None else json.dumps(payload).encode()
            headers = {"Cookie": cookie, "Content-Type": "application/json", "X-CSRF-Token": csrf}
            with urllib.request.urlopen(urllib.request.Request(origin + path, data=data,
                                        headers=headers, method=method), timeout=15) as response:
                payload = response.read(2097153)
                if len(payload) > 2097152:
                    raise AssertionError("administrative catalog exceeds 2 MiB acceptance bound")
                value = json.loads(payload)
                return value, response.headers
        def launch(role):
            log = tempfile.TemporaryFile()
            logs.append(log)
            role_env = dict(env, V3_PROCESS_ROLE=role)
            process = subprocess.Popen([str(binary)], env=role_env, cwd=source,
                                       stdout=log, stderr=log, start_new_session=True)
            processes.append(process)
            return process
        def catalog_has():
            result, _ = request("/api/v3/agents/shared_default/abilities")
            return any(a["id"] == "optimization" and a.get("ready") is True
                       for a in result["abilities"])
        def settings_persisted():
            value, _ = request("/api/v3/agents/shared_default/abilities")
            assert value["settings"]["optimization"] == settings
            return True
        def stop(process):
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=15)
        marker = state / "optimization/plugin.ready"
        def ready():
            item = json.loads(marker.read_text())
            return item if alive(item["pid"]) and catalog_has() else None
        def withdrawn():
            return not marker.exists() and not catalog_has()
        def loop_probe(available):
            probe_env = dict(env, V3_PROCESS_ROLE="loop",
                             OPTIMIZATION_EXPECTED_AVAILABLE=str(available).lower())
            result = subprocess.run([str(test_binary), "--ignored", "--exact",
                        "optimization::tests::probe_loop_generation", "--nocapture"],
                        env=probe_env, cwd=source, capture_output=True, timeout=15)
            if result.returncode or b"1 passed;" not in result.stdout:
                raise AssertionError("native loop capability probe failed: " +
                                     result.stdout.decode(errors="replace")[-2048:] +
                                     result.stderr.decode(errors="replace")[-2048:])
        def health():
            return subprocess.run([str(binary)], env=dict(env, V3_PROCESS_ROLE="health"),
                                  cwd=source, capture_output=True, timeout=5).returncode == 0
        try:
            api = launch("api")
            eventually(lambda: request("/api/v3/auth/setup-status")[0]["initialized"] is False)
            _, headers = request("/api/v3/auth/setup-admin",
                                 {"username": "optimization-test", "password": secrets.token_urlsafe(32)})
            cookie = headers["Set-Cookie"].split(";")[0]
            csrf = request("/api/v3/auth/me")[0]["csrf_token"]
            loop = launch("loop")
            eventually(health)
            assert not catalog_has()
            loop_probe(False)
            assert not marker.exists(), "absent package must not be reinstalled"

            package = plugins / "optimization"
            package.mkdir(mode=0o700)
            originals = source / "src/plugin_packages/optimization"
            names = ("plugin.json", "optimization.json", "optimization.skill.md",
                     "measure.py", "workload.py")
            for name in names:
                shutil.copyfile(originals / name, package / name)
                (package / name).chmod(0o600)
            time.sleep(2.2)
            assert not catalog_has(), "partial package became available"
            shutil.copyfile(plugin_binary, package / "webagent-optimization")
            (package / "webagent-optimization").chmod(0o500)
            first = eventually(ready)
            loop_probe(True)
            configuration, _ = request("/api/v3/agents/shared_default/abilities")
            saved, _ = request("/api/v3/agents/shared_default/abilities/optimization/settings",
                               {"expected_revision": configuration["revision"], "settings": settings},
                               method="PUT")
            assert saved["settings"]["optimization"] == settings
            settings_persisted()

            hidden = plugins / "removed-optimization"
            package.rename(hidden)
            eventually(withdrawn)
            eventually(lambda: not alive(first["pid"]))
            loop_probe(False)
            settings_persisted()
            hidden.rename(package)
            second = eventually(ready)
            assert second["pid"] != first["pid"]
            loop_probe(True)
            settings_persisted()

            manifest = package / "plugin.json"
            good_manifest = manifest.read_bytes()
            manifest.write_text('{"version":99}')
            eventually(withdrawn)
            eventually(lambda: not alive(second["pid"]))
            loop_probe(False)
            manifest.write_bytes(good_manifest)
            third = eventually(ready)

            executable = package / "webagent-optimization"
            replacement = package / "new-executable"
            replacement.write_bytes(executable.read_bytes() + b"\x00")
            replacement.chmod(0o500)
            replacement.replace(executable)
            def updated():
                item = ready()
                return item if item and item["generation"] != third["generation"] else None
            fourth = eventually(updated)
            assert fourth["pid"] != third["pid"]
            eventually(lambda: not alive(third["pid"]))
            loop_probe(True)

            executable.chmod(0o502)
            eventually(withdrawn)
            loop_probe(False)
            executable.chmod(0o500)
            fifth = eventually(ready)
            saved = package / "saved-executable"
            executable.rename(saved)
            executable.symlink_to(saved)
            eventually(withdrawn)
            loop_probe(False)
            executable.unlink()
            saved.rename(executable)
            eventually(ready)
            loop_probe(True)
            assert api.poll() is None and loop.poll() is None, "core roles restarted or exited"
            eventually(health)
            settings_persisted()
            # Separate deliberate private restart after the no-restart journey.
            # A staged private executable already exists with mode 0500 here.
            # Restart must replace it atomically rather than overwrite that inode.
            installed_before_restart = ready()
            core_pids_before_restart = (api.pid, loop.pid)
            stop(loop)
            stop(api)
            eventually(lambda: not alive(installed_before_restart["pid"]))
            api = launch("api")
            eventually(lambda: request("/api/v3/auth/setup-status")[0]["initialized"] is True)
            loop = launch("loop")
            eventually(health)
            installed_after_restart = eventually(ready)
            assert (api.pid, loop.pid) != core_pids_before_restart
            assert installed_after_restart["pid"] != installed_before_restart["pid"]
            assert installed_after_restart["generation"] == installed_before_restart["generation"]
            loop_probe(True)
            settings_persisted()
            last = installed_after_restart
            package.rename(hidden)
            eventually(withdrawn)
            eventually(lambda: not alive(last["pid"]))
            stop(loop)
            stop(api)
            api = launch("api")
            eventually(lambda: request("/api/v3/auth/setup-status")[0]["initialized"] is True)
            loop = launch("loop")
            eventually(health)
            time.sleep(2.2)
            assert not package.exists(), "restart reinstalled removed package"
            assert not marker.exists() and not catalog_has()
            loop_probe(False)
            settings_persisted()
            hidden.rename(package)
            eventually(ready)
            loop_probe(True)
            settings_persisted()
            print(json.dumps({"status": "passed", "journeys": [
                "absent-no-reinstall", "partial-unavailable", "live-add", "remove-child-stop",
                "restore", "invalid-manifest", "atomic-executable-update",
                "writable-executable-rejected", "symlink-rejected",
                "native-loop-capability-consistency", "api-loop-stayed-running",
                "all-ten-settings-save", "settings-survive-removal-restore",
                "private-api-loop-restart-installed-stable-generation",
                "installed-restart-new-plugin-pid", "settings-survive-installed-restart",
                "private-api-loop-restart-no-reinstall", "settings-survive-restart"]}))
        except Exception:
            try:
                if marker.exists():
                    print("private native-loop generation probe follows")
                    loop_probe(True)
                catalog, _ = request("/api/v3/agents/shared_default/abilities")
                print("private optimization catalog metadata",
                      [{key: item.get(key) for key in ("id", "ready", "readiness", "available", "configured", "installed", "effective")}
                       for item in catalog["abilities"] if item["id"] == "optimization"])
            except Exception as probe_error:
                print("bounded private capability diagnostic", str(probe_error)[:4096])
            for directory in (plugins / "optimization", state / "optimization"):
                print("private plugin diagnostic", directory.name,
                      [(p.name, p.stat().st_size, oct(p.lstat().st_mode & 0o777), p.lstat().st_uid)
                       for p in directory.iterdir()] if directory.exists() else "absent")
            for index, log in enumerate(logs):
                log.seek(0, 2)
                length = log.tell()
                log.seek(max(0, length - 4096))
                text = log.read().decode(errors="replace")
                for secret in ("isolated-only-password", cookie, csrf):
                    if secret:
                        text = text.replace(secret, "[redacted]")
                print("isolated role", index, "exit", processes[index].poll(),
                      "startup/acceptance diagnostics:", text)
            raise
        finally:
            for process in reversed(processes):
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            for process in processes:
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            for log in logs:
                log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--plugin-binary", type=pathlib.Path, required=True)
    parser.add_argument("--test-binary", type=pathlib.Path, required=True)
    parser.add_argument("--source", type=pathlib.Path,
                        default=pathlib.Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    acceptance(args.binary.resolve(), args.plugin_binary.resolve(),
               args.test_binary.resolve(), args.source.resolve())
