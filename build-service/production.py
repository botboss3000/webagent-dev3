"""Production releases: a cleaned snapshot of the dev checkout, built in the stable
(linked) mode and pushed to the public production repository.

Agents and the local operator may request one (build_work action
production_request, or `buildctl production request`). Nothing runs until an
administrator approves it on the Host Admin page, which reaches the broker as
v3_production_decide from the app; the local operator cannot approve. An approved
release exports an allowlist of the requested commit, stamps the version, runs the
credential guard over every file, builds and compiles the tests with the stable
profile, then pushes one commit and tag to the production repository with a
broker-only token. State lives under the broker's state root in production/.
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

REPOSITORY = "https://github.com/botboss3000/webagent.git"
API = "https://api.github.com/repos/botboss3000/webagent"
UPLOADS = "https://uploads.github.com/repos/botboss3000/webagent"
# Broker-only push credential; outside the checkout, never printed or logged.
TOKEN = Path(os.environ.get("WEBAGENT_PRODUCTION_TOKEN", "/etc/webagent/production/github-token"))
# Android release signing key and its password file, broker-only like TOKEN. Without them the
# release ships no APK (an unsigned one cannot be installed).
KEYSTORE = TOKEN.with_name("android-keystore.jks")
KEYSTORE_PASS = TOKEN.with_name("android-keystore-pass")
# Top-level entries of the production repository (live-files.json is read by the app and the
# installer packaging).
ALLOW = ("src", "crates", "ui", "abilities", "config", "plugins", "Cargo.toml", "Cargo.lock",
         "build.rs", "rustfmt.toml", "README.md", "live-files.json")
# The installer image's build context in the exported tree, and the production-only GitHub
# workflow (macOS installer on each release tag) it carries.
INSTALLER = "crates/app-shell/installer"
WORKFLOWS = {"release-macos.yml": INSTALLER + "/release-macos.yml"}
INSTALLER_SECONDS = 3 * 3600
SIDEKICK_WAIT = 30 * 60
# The installers phase: every platform, then the outputs and a manifest into /output.
INSTALLER_SCRIPT = r"""set -e
for platform in linux windows android; do python3 crates/app-shell/package.py "$platform"; done
python3 - <<'COLLECT'
import glob, hashlib, json, shutil
from pathlib import Path
patterns = ["/cache/target/x86_64-unknown-linux-gnu/release/bundle/deb/*.deb",
            "/cache/target/x86_64-unknown-linux-gnu/release/bundle/rpm/*.rpm",
            "/cache/target/x86_64-unknown-linux-gnu/release/bundle/appimage/*.AppImage",
            "/cache/target/x86_64-pc-windows-gnu/release/bundle/nsis/*-setup.exe",
            "crates/app-shell/gen/android/app/build/outputs/apk/**/*-unsigned.apk"]
files = []
for pattern in patterns:
    for path in glob.glob(pattern, recursive=True):
        shutil.copy2(path, "/output/" + Path(path).name)
        body = Path(path).read_bytes()
        files.append({"name": Path(path).name, "bytes": len(body), "sha256": hashlib.sha256(body).hexdigest()})
Path("/output/manifest.json").write_text(json.dumps(files))
COLLECT
"""
# Development-only parts inside allowed folders.
EXCLUDE = ("abilities/Administrator/host_admin", "ui/main-panel/builds", "ui/main-panel/host-admin")
FIRST = "0.1.0"
BUMPS = ("patch", "minor", "major")
# Rust build of the exported tree: the broker's pinned toolchain, at most this share of the host.
BUILD_CPUS = "4"
BUILD_MEMORY = "10g"
BUILD_SECONDS = 4 * 3600
AGENT_ACTIONS = {"production_request": "request", "production_status": "status",
                 "production_list": "list"}


def bump(version, kind):
    """The next semantic version; the first release is FIRST."""
    if version is None:
        return FIRST
    major, minor, patch = (int(part) for part in version.split("."))
    if kind == "major":
        return f"{major + 1}.0.0"
    if kind == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def excluded(relative):
    return any(relative == item or relative.startswith(item + "/") for item in EXCLUDE)


def stamp(tree, version):
    """Write `version` into every place the app reads its version from."""
    def package(path, name):
        text = path.read_text()
        pattern = re.compile(r'(\[package\]\s*\nname\s*=\s*"' + re.escape(name) + r'"\s*\nversion\s*=\s*")[^"]*(")')
        updated, count = pattern.subn(r"\g<1>" + version + r"\2", text, count=1)
        if count != 1:
            raise ValueError(f"version field not found in {path.name}")
        path.write_text(updated)

    def lock(path, name):
        text = path.read_text()
        pattern = re.compile(r'(name = "' + re.escape(name) + r'"\nversion = ")[^"]*(")')
        updated, count = pattern.subn(r"\g<1>" + version + r"\2", text, count=1)
        if count != 1:
            raise ValueError(f"lock entry for {name} not found")
        path.write_text(updated)

    package(tree / "Cargo.toml", "webagent-v3")
    lock(tree / "Cargo.lock", "webagent-v3")
    shell = tree / "crates/app-shell"
    if (shell / "Cargo.toml").is_file():
        package(shell / "Cargo.toml", "webagent-app")
        if (shell / "Cargo.lock").is_file():
            lock(shell / "Cargo.lock", "webagent-app")
        conf = shell / "tauri.conf.json"
        value = json.loads(conf.read_text())
        value["version"] = version
        conf.write_text(json.dumps(value, indent=2) + "\n")


class Production:
    def __init__(self, broker, state, checkout, rust_image, release, runner=subprocess.run):
        self.broker, self.checkout, self.rust_image = broker, Path(checkout), rust_image
        self.release, self.runner = release, runner
        self.root = Path(state) / "production"
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.lock = threading.Lock()
        # A release that was running when the broker stopped did not finish.
        for record in self.records():
            if record["status"] in {"approved", "running"}:
                record.update(status="interrupted", finished_at=time.time(),
                              error="the build service stopped during this release; request it again")
                self.save(record)

    # --- records -------------------------------------------------------------------------
    def path(self, release_id):
        uuid.UUID(release_id)
        return self.root / f"{release_id}.json"

    def save(self, record):
        temporary = self.path(record["id"]).with_suffix(".tmp")
        temporary.write_text(json.dumps(record, indent=1))
        os.replace(temporary, self.path(record["id"]))

    def load(self, release_id):
        path = self.path(release_id)
        if not path.is_file():
            raise ValueError("unknown production release")
        return json.loads(path.read_text())

    def records(self):
        found = []
        for path in self.root.glob("*.json"):
            try:
                found.append(json.loads(path.read_text()))
            except ValueError:
                continue
        return sorted(found, key=lambda r: r.get("requested_at", 0), reverse=True)

    def published(self):
        """The newest published version, or None before the first release."""
        done = [r for r in self.records() if r["status"] == "published"]
        return done[0]["version"] if done else None

    def summary(self, record):
        return {k: record.get(k) for k in ("id", "status", "version", "bump", "notes", "commit",
                                            "requested_by", "requested_at", "decided_by", "decided_at",
                                            "stage", "stages", "error", "finished_at", "pushed_commit",
                                            "installers")}

    # --- entry ---------------------------------------------------------------------------
    def handle(self, name, trusted, args):
        """`v3_production` (request, list, status, retry) or the app-only `v3_production_decide`."""
        if name == "build_work":
            # The app adds its broker context to every build_work call; releases do not use it.
            args = {**{k: v for k, v in args.items() if k not in {"broker_id", "task_id", "request_id"}},
                    "action": AGENT_ACTIONS[args["action"]]}
            name = "v3_production"
        local = trusted.get("local_caller") is True
        administrator = trusted.get("administrator") is True
        if name == "v3_production_decide":
            # Approval comes only from the Host Admin page: an administrator through the app.
            if local or not administrator:
                raise PermissionError("production releases are approved on the Host Admin page")
            return self.decide(args, trusted)
        if name != "v3_production":
            raise ValueError("unknown production operation")
        if not (local or administrator):
            raise PermissionError("administrator or operator required")
        action = args.get("action")
        if action == "request":
            return self.request(args, trusted)
        if action == "list":
            if set(args) - {"action"}:
                raise ValueError("list takes no arguments")
            return {"releases": [self.summary(r) for r in self.records()[:20]],
                    "published_version": self.published()}
        if action == "status":
            if set(args) != {"action", "release_id"}:
                raise ValueError("status needs release_id")
            return self.summary(self.load(args["release_id"]))
        raise ValueError("unknown production action")

    def request(self, args, trusted):
        if set(args) - {"action", "bump", "notes"}:
            raise ValueError("request takes bump and notes")
        kind = args.get("bump", "patch")
        notes = args.get("notes", "")
        if kind not in BUMPS or not isinstance(notes, str) or len(notes) > 4000:
            raise ValueError("bump must be patch, minor or major; notes at most 4000 characters")
        with self.lock:
            if any(r["status"] in {"awaiting_approval", "approved", "running"} for r in self.records()):
                raise ValueError("a production release is already waiting or running")
            commit = self.git("rev-parse", "HEAD").strip()
            record = {"id": str(uuid.uuid4()), "status": "awaiting_approval", "bump": kind,
                      "version": bump(self.published(), kind), "notes": notes, "commit": commit,
                      "requested_by": trusted.get("session_name") or trusted["user_id"],
                      "requested_at": time.time(), "stages": {}}
            self.save(record)
        return {**self.summary(record),
                "next": "waiting for an administrator's approval on the Host Admin page"}

    def decide(self, args, trusted):
        if set(args) != {"release_id", "decision"} or args["decision"] not in {"approve", "deny"}:
            raise ValueError("decision must be approve or deny")
        with self.lock:
            record = self.load(args["release_id"])
            if record["status"] != "awaiting_approval":
                raise ValueError("this release is not waiting for a decision")
            record.update(decided_by=trusted["user_id"], decided_at=time.time(),
                          status="approved" if args["decision"] == "approve" else "denied")
            self.save(record)
        if record["status"] == "approved":
            threading.Thread(target=self.run, args=(record["id"],), daemon=True,
                             name="production-release").start()
        return self.summary(record)

    # --- the release ---------------------------------------------------------------------
    def git(self, *argv, cwd=None, env=None, check=True):
        result = self.runner(["git", "-C", str(cwd or self.checkout), *argv], capture_output=True,
                             text=True, env=env, timeout=600)
        if check and result.returncode != 0:
            raise RuntimeError(f"git {argv[0]} failed: {result.stderr.strip()[-300:]}")
        return result.stdout

    def stage(self, record, name, work):
        record["stage"] = name
        record["stages"][name] = {"started_at": time.time()}
        self.save(record)
        result = work()
        record["stages"][name]["finished_at"] = time.time()
        if isinstance(result, dict):
            record["stages"][name].update(result)
        self.save(record)

    def run(self, release_id):
        record = self.load(release_id)
        record["status"] = "running"
        self.save(record)
        folder = self.root / release_id
        tree = folder / "tree"
        try:
            self.stage(record, "export", lambda: self.export(record["commit"], tree, record["version"]))
            self.stage(record, "credential_guard", lambda: self.guard(tree))
            self.stage(record, "build", lambda: self.build(tree, folder))
            self.stage(record, "push", lambda: self.push(tree, record))
            self.stage(record, "dev_tag", lambda: self.tag(record))
        except Exception as error:  # Recorded for the Host Admin page and the agent.
            record.update(status="failed", finished_at=time.time(), error=str(error)[:500])
            self.save(record)
            return
        # The source release stands; installers are added to it, and a failure here says so
        # without undoing the release (macOS comes from the repository's own workflow).
        try:
            self.stage(record, "installers", lambda: self.installers(tree, folder, record))
            self.stage(record, "upload", lambda: self.upload(folder / "installers", record))
        except Exception as error:
            record["installers"] = {"error": str(error)[:500]}
        record.update(status="published", finished_at=time.time(), stage=None)
        self.save(record)

    def export(self, commit, tree, version):
        if tree.exists():
            shutil.rmtree(tree)
        tree.mkdir(parents=True)
        archive = tree.parent / "export.tar"
        listed = self.git("ls-tree", "--name-only", commit).split()
        present = [name for name in ALLOW if name in listed]
        with open(archive, "wb") as stream:
            result = self.runner(["git", "-C", str(self.checkout), "archive", "--format=tar", commit, *present],
                                 stdout=stream, stderr=subprocess.PIPE, timeout=600)
        if result.returncode != 0:
            raise RuntimeError("git archive failed")
        with tarfile.open(archive) as source:
            members = [m for m in source.getmembers() if not excluded(m.name.rstrip("/"))]
            if any(m.issym() or m.islnk() or m.name.startswith(("/", "..")) or "/../" in m.name for m in members):
                raise ValueError("links or escaping paths in the export")
            source.extractall(tree, members=members, filter="data")
        archive.unlink()
        for name, origin in WORKFLOWS.items():
            if (tree / origin).is_file():
                target = tree / ".github/workflows" / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(tree / origin, target)
        stamp(tree, version)
        return {"files": sum(1 for p in tree.rglob("*") if p.is_file())}

    def guard(self, tree):
        flagged = [str(p.relative_to(tree)) for p in tree.rglob("*")
                   if p.is_file() and p.stat().st_size <= 8 * 1024 * 1024
                   and self.release.possible_credentials(p.read_bytes())]
        if flagged:
            raise ValueError("credential guard flagged: " + ", ".join(flagged[:10]))
        return {"flagged": 0}

    def build(self, tree, folder):
        cache = self.root / "cache"
        for name in ("cargo", "target"):
            (cache / name).mkdir(parents=True, exist_ok=True)
        script = ("set -e; export CARGO_HOME=/cache/cargo CARGO_TARGET_DIR=/cache/target; "
                  "cargo build --locked --profile stable --features stable-abilities; "
                  "cargo test --locked --profile stable --features stable-abilities --no-run")
        result = self.runner(["nice", "-n", "15", "docker", "run", "--rm", "--cpus", BUILD_CPUS,
                              "--memory", BUILD_MEMORY, "--user", f"{os.getuid()}:{os.getgid()}",
                              "-v", f"{tree}:/src:ro", "-v", f"{cache}:/cache", "-w", "/src",
                              self.rust_image, "sh", "-c", script],
                             capture_output=True, text=True, timeout=BUILD_SECONDS)
        (folder / "build.log").write_text(result.stdout[-200_000:] + result.stderr[-200_000:])
        if result.returncode != 0:
            raise RuntimeError("stable build of the exported tree failed; see build.log")
        return {"profile": "stable", "features": "stable-abilities"}

    def push(self, tree, record):
        if not TOKEN.is_file() or not TOKEN.read_text().strip():
            raise RuntimeError(f"production token missing: put it in {TOKEN} (mode 600)")
        repo = self.root / "repository"
        if not (repo / ".git").is_dir():
            repo.mkdir(parents=True, exist_ok=True)
            self.git("init", "-q", "-b", "main", cwd=repo)
            self.git("remote", "add", "origin", REPOSITORY, cwd=repo)
        with tempfile.TemporaryDirectory(dir=self.root) as scratch:
            askpass = Path(scratch) / "askpass"
            askpass.write_text("#!/bin/sh\ncase \"$1\" in *sername*) echo x-access-token;; "
                               "*) cat \"$WEBAGENT_PRODUCTION_TOKEN_FILE\";; esac\n")
            askpass.chmod(0o700)
            env = {**os.environ, "GIT_ASKPASS": str(askpass), "GIT_TERMINAL_PROMPT": "0",
                   "WEBAGENT_PRODUCTION_TOKEN_FILE": str(TOKEN)}
            fetched = self.git("fetch", "-q", "origin", "main", cwd=repo, env=env, check=False)
            has_remote = self.git("rev-parse", "-q", "--verify", "FETCH_HEAD", cwd=repo, check=False).strip()
            if has_remote:
                self.git("checkout", "-q", "-B", "main", "FETCH_HEAD", cwd=repo)
            for entry in repo.iterdir():
                if entry.name != ".git":
                    shutil.rmtree(entry) if entry.is_dir() and not entry.is_symlink() else entry.unlink()
            for entry in tree.iterdir():
                target = repo / entry.name
                shutil.copytree(entry, target) if entry.is_dir() else shutil.copy2(entry, target)
            self.git("add", "-A", cwd=repo)
            author = ["-c", "user.name=WebAgent Release", "-c", "user.email=release@webagent.live"]
            message = f"Release v{record['version']}\n\n{record['notes']}".strip() + "\n"
            self.git(*author, "commit", "-q", "--allow-empty", "-m", message, cwd=repo)
            self.git(*author, "tag", "-a", f"v{record['version']}", "-m", f"Release v{record['version']}", cwd=repo)
            self.git("push", "-q", "origin", "main", f"v{record['version']}", cwd=repo, env=env)
            del fetched
        record["pushed_commit"] = self.git("rev-parse", "HEAD", cwd=repo).strip()
        return {"repository": REPOSITORY, "tag": f"v{record['version']}"}

    def tag(self, record):
        name = f"production-v{record['version']}"
        self.git("tag", name, record["commit"])
        return {"dev_tag": name}

    # --- installers ----------------------------------------------------------------------
    def installer_image(self, tree, folder):
        """The installer image built from the exported tree's recipe, reused while it is unchanged."""
        context = tree / INSTALLER
        digest = hashlib.sha256()
        for path in sorted(context.rglob("*")):
            if path.is_file():
                digest.update(str(path.relative_to(context)).encode() + b"\0" + path.read_bytes())
        tag = "webagent-production-installer:" + digest.hexdigest()[:16]
        found = self.runner(["docker", "image", "inspect", "--format", "{{.Id}}", tag],
                            capture_output=True, text=True, timeout=60)
        if found.returncode != 0:
            built = self.runner(["nice", "-n", "15", "docker", "build", "-q", "-t", tag, str(context)],
                                capture_output=True, text=True, timeout=3600)
            (folder / "installer-image.log").write_text(built.stdout[-50_000:] + built.stderr[-50_000:])
            if built.returncode != 0:
                raise RuntimeError("installer image build failed; see installer-image.log")
            found = self.runner(["docker", "image", "inspect", "--format", "{{.Id}}", tag],
                                capture_output=True, text=True, timeout=60)
        return found.stdout.strip()

    def sidekick(self, settings, *words, stdin=b"", timeout=120, stdout=None):
        argv = self.broker.runner.executor.sidekick_argv(settings, *words)
        # Bytes go in whole; a stream (the saved installer image) is piped through.
        feed = {"input": stdin} if isinstance(stdin, bytes) else {"stdin": stdin}
        result = self.runner(argv, **feed, stdout=stdout or subprocess.PIPE, stderr=subprocess.PIPE,
                             timeout=timeout)
        if result.returncode != 0:
            raise RuntimeError(f"sidekick {words[0]} failed: {result.stderr.decode(errors='replace')[-300:]}")
        return result.stdout

    def installers(self, tree, folder, record):
        """Linux, Windows and Android installers built on the sidekick, the APK signed here."""
        executor = self.broker.runner.executor
        settings = executor.sidekick_settings()
        if not settings or not all(isinstance(settings.get(k), str) and settings[k]
                                   for k in ("host", "user", "key", "known_hosts")):
            raise RuntimeError("no sidekick is configured for installer builds")
        image = self.installer_image(tree, folder)
        status = executor.sidekick_sync(settings, {})
        if status.get("version", 0) < 5:
            raise RuntimeError("the sidekick program is too old for installer builds")
        job = record["id"]
        deadline = time.time() + SIDEKICK_WAIT
        while True:  # A dev test half may hold the sidekick; it frees it within minutes.
            decision = json.loads(self.sidekick(settings, "begin", job))
            if decision.get("decision") == "accepted":
                break
            if decision.get("decision") != "busy" or time.time() > deadline:
                raise RuntimeError(f"sidekick did not take the installer build: {decision}")
            time.sleep(30)
        result = "failed"
        try:
            if image not in status.get("images", []):
                saved = subprocess.Popen(["docker", "save", image], stdout=subprocess.PIPE)
                try:
                    self.sidekick(settings, "load-image", job, image, "installer", stdin=saved.stdout,
                                  timeout=3600)
                finally:
                    saved.wait(600)
            body, digest = executor.sidekick_tree(tree)
            synced = json.loads(self.sidekick(settings, "sync-installer-source", job, stdin=body, timeout=600))
            if synced.get("sha256") != digest:
                raise RuntimeError("the sidekick's installer source does not match the export")
            request = json.dumps({"argv": ["sh", "-c", INSTALLER_SCRIPT], "image": image, "cargo_key": None,
                                  "incremental": "0"}).encode()
            with open(folder / "installers.log", "wb") as log:
                argv = executor.sidekick_argv(settings, "run", job, "installers", str(INSTALLER_SECONDS))
                ran = self.runner(argv, input=request, stdout=log, stderr=subprocess.STDOUT,
                                  timeout=INSTALLER_SECONDS + 300)
            if ran.returncode != 0:
                raise RuntimeError("installer builds failed on the sidekick; see installers.log")
            manifest = json.loads(self.fetch(settings, job, ["installers/manifest.json"], folder)
                                  .joinpath("manifest.json").read_text())
            names = [item["name"] for item in manifest]
            if not names or any("/" in n or n.startswith(".") for n in names):
                raise RuntimeError("the sidekick listed no usable installers")
            output = self.fetch(settings, job, ["installers/" + n for n in names], folder)
            for item in manifest:
                body = (output / item["name"]).read_bytes()
                if hashlib.sha256(body).hexdigest() != item["sha256"]:
                    raise RuntimeError(f"{item['name']} changed in transfer")
            result = "passed"
        finally:
            self.sidekick(settings, "end", job, result)
        notes = self.sign_apks(output, image, record["version"])
        built = sorted(p.name for p in output.iterdir() if p.name != "manifest.json")
        record["installers"] = {"files": built, **notes}
        return {"files": len(built)}

    def fetch(self, settings, job, names, folder):
        """The named sidekick outputs, unpacked into folder/installers."""
        output = folder / "installers"
        output.mkdir(exist_ok=True)
        archive = folder / "fetch.tar"
        with open(archive, "wb") as stream:
            self.sidekick(settings, "fetch", job, stdin=json.dumps(names).encode(), timeout=1800, stdout=stream)
        with tarfile.open(archive) as source:
            for member in source.getmembers():
                if not member.isfile() or not member.name.startswith("installers/") or "/" in member.name[11:]:
                    raise ValueError("unexpected file from the sidekick")
                member.name = member.name[11:]
                source.extract(member, output, filter="data")
        archive.unlink()
        return output

    def sign_apks(self, output, image, version):
        """Sign the APK with the broker-only release key in the installer image; without the key,
        drop it (an unsigned APK cannot be installed) and say so."""
        unsigned = sorted(output.glob("*-unsigned.apk"))
        if not unsigned:
            return {}
        if not (KEYSTORE.is_file() and KEYSTORE_PASS.is_file()):
            for path in unsigned:
                path.unlink()
            return {"apk": f"not built: put the Android release key in {KEYSTORE} and its password in {KEYSTORE_PASS}"}
        source = unsigned[0]
        name = f"WebAgent_{version}_android.apk"
        script = ("set -e; tools=$(ls -d /opt/android/build-tools/* | sort | tail -1); "
                  f"$tools/zipalign -f -p 4 /work/{source.name} /tmp/aligned.apk; "
                  "$tools/apksigner sign --ks /keys/keystore.jks --ks-pass file:/keys/pass "
                  f"--ks-key-alias webagent --out /work/{name} /tmp/aligned.apk")
        signed = self.runner(["docker", "run", "--rm", "--network", "none", "--user", f"{os.getuid()}:{os.getgid()}",
                              "-v", f"{output}:/work", "-v", f"{KEYSTORE}:/keys/keystore.jks:ro",
                              "-v", f"{KEYSTORE_PASS}:/keys/pass:ro", image, "sh", "-c", script],
                             capture_output=True, text=True, timeout=600)
        for path in unsigned:
            path.unlink()
        if signed.returncode != 0:
            return {"apk": "signing failed: " + signed.stderr[-300:]}
        return {}

    def api(self, method, url, body=None, content_type="application/json", length=None):
        token = TOKEN.read_text().strip()
        request = urllib.request.Request(url, data=body, method=method, headers={
            "Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28", "Content-Type": content_type,
            **({"Content-Length": str(length)} if length is not None else {})})
        with urllib.request.urlopen(request, timeout=1800) as reply:
            data = reply.read()
        return json.loads(data) if data else None

    def upload(self, output, record):
        """Create the GitHub release for the tag (the macOS workflow may have made it first) and
        upload the installers, replacing assets of the same name."""
        tag = f"v{record['version']}"
        try:
            release = self.api("GET", f"{API}/releases/tags/{tag}")
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise
            release = self.api("POST", f"{API}/releases", json.dumps({
                "tag_name": tag, "name": f"WebAgent {tag}", "body": record["notes"] or f"Release {tag}"}).encode())
        existing = {asset["name"]: asset["id"] for asset in release.get("assets", [])}
        names = sorted(p.name for p in output.iterdir() if p.name != "manifest.json")
        for name in names:
            if name in existing:
                self.api("DELETE", f"{API}/releases/assets/{existing[name]}")
            path = output / name
            with open(path, "rb") as stream:
                self.api("POST", f"{UPLOADS}/releases/{release['id']}/assets?name={urllib.parse.quote(name)}",
                         stream, "application/octet-stream", path.stat().st_size)
        return {"release": release.get("html_url"), "assets": len(names)}
