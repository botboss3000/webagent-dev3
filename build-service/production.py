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
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import threading
import time
import uuid
from pathlib import Path

REPOSITORY = "https://github.com/botboss3000/webagent.git"
# Broker-only push credential; outside the checkout, never printed or logged.
TOKEN = Path(os.environ.get("WEBAGENT_PRODUCTION_TOKEN", "/etc/webagent/production/github-token"))
# Top-level entries of the production repository.
ALLOW = ("src", "crates", "ui", "abilities", "config", "plugins", "Cargo.toml", "Cargo.lock",
         "build.rs", "rustfmt.toml", "README.md")
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
    if shell.is_dir():
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
                                            "stage", "stages", "error", "finished_at", "pushed_commit")}

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
            # Installers (Linux, Windows, Android on the sidekick; macOS on GitHub) follow.
            record["stages"]["installers"] = {"status": "not built yet"}
            record.update(status="published", finished_at=time.time(), stage=None)
            self.save(record)
        except Exception as error:  # Recorded for the Host Admin page and the agent.
            record.update(status="failed", finished_at=time.time(), error=str(error)[:500])
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
