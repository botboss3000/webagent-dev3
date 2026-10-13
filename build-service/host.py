import difflib
"""Fixed development-only build broker. No model-controlled commands or paths.

Install a protected copy, not this writable source, as the host service. Root
creates its listener then drops to ar with the Docker group. A root-owned
snapshot helper supplies only the allowlisted context and two gate JSON files.
"""
import argparse
import asyncio
import concurrent.futures
import contextlib
import hashlib
import fcntl
import http.client
import io
import importlib.util
import inspect
import json
import os
from pathlib import Path
import re
import signal
import shutil
import socket
import stat
import struct
import subprocess
import tarfile
import threading
import time
import uuid
import sqlite3
import base64
from urllib.parse import quote


import sys
_module_root = Path(os.environ.get("WEBAGENT_BUILD_MODULE_ROOT", str(Path(__file__).resolve().parent)))
_config_spec = importlib.util.spec_from_file_location("webagent_build_config", _module_root / "config.py")
_config_module = importlib.util.module_from_spec(_config_spec)
_config_spec.loader.exec_module(_config_module)
CONFIG = _config_module.load()

SOURCE = Path(CONFIG["checkout_root"])
_maintenance_path=_module_root/"index-maintenance.py"
if not _maintenance_path.exists():
    if _module_root != SOURCE/"build-service":raise RuntimeError("Protected maintenance engine unavailable")
    _maintenance_path=SOURCE/"plugins/abilities/Administrator/code_index/maintenance.py"
_maintenance_spec=importlib.util.spec_from_file_location("code_index_maintenance_engine",_maintenance_path)
maintenance_engine=importlib.util.module_from_spec(_maintenance_spec);_maintenance_spec.loader.exec_module(maintenance_engine)

STATE = Path(CONFIG["state_root"])
SOCKET = Path(CONFIG["runtime_root"])/"build.sock"
BRIDGE_SOCKET = Path(CONFIG["runtime_root"])/"python.sock"
BRIDGE_WORKSPACE = "94c353d5-70bd-44b2-9af0-757c4a979fc9"
CONTEXT = (
    "v3/Dockerfile", "v3/Cargo.toml", "v3/Cargo.lock", "v3/build.rs", "v3/.gitignore",
    "v3/src", "v3/ui", "v3/plugins/abilities", "v3/plugins/gates", "v3/worker.py",
    "v3/sandbox.py", "v3/ability_plugin_worker.py", "v3/launch.py", "v3/promote.py", "v3/ui-baseline.json", "v3/tests",
    "v3/README.md", "v3/docs", "v3/deploy.sh", "v3/config", "v3/build-service",
    "v3/data/config/gates/message_visibility_v1.json",
    "v3/data/config/gates/closing_status_v1.json",
    "ui/shared/css/app1.css", "ui/shared/css/app2.css", "ui/shared/css/app3.css",
    "ui/shared/css/design-system.css", "ui/shared/css/index.css",
    "ui/main-panel/agents/agents.html", "ui/main-panel/agents/agents.css",
    "ui/vendor/fonts", "ui/vendor/lucide", "ui/favicon.svg")
UI_SOURCE_PREFIX = "v3/ui/"
INDEX_METADATA_ROOT="v3/plugins/abilities/Administrator/code_index/"
INDEX_METADATA_FILES={INDEX_METADATA_ROOT+"code-index-features.json",INDEX_METADATA_ROOT+"code_index.skill.md",INDEX_METADATA_ROOT+"skills/comment_maintenance.skill.md"}
INDEX_METADATA_FILES.update(INDEX_METADATA_ROOT+"tools/"+name+".skill.md"for name in ("code_index_scan","code_index_lookup","code_index_coverage","code_index_trace","code_index_progress","code_index_maintenance"))
CODEBASE_ROOT="v3/plugins/abilities/Administrator/codebase_admin/"
CODEBASE_TOOLS=["file_list","file_read","file_search","file_edit","file_write","build_work",
                "state_inspect","prompt_slot_read","prompt_slot_write","shell","python"]
def codebase_source_path(path):
    if not path.startswith(CODEBASE_ROOT):return False
    relative=path[len(CODEBASE_ROOT):]
    return relative in {"runner.py","test_runner.py","plugin.json","codebase_admin.json","codebase_admin.skill.md"} or (
        relative in {"tools/"+name+".skill.md" for name in CODEBASE_TOOLS}
        or relative in {"skills/rust_quality.skill.md","skills/live_plugin_publication.skill.md","skills/ui_theming.skill.md","skills/ui_toggle_list.skill.md"})
DIAGNOSTICS_ROOT="v3/plugins/abilities/Administrator/diagnostics/"
DIAGNOSTICS_MANIFEST={'version': 1, 'id': 'diagnostics', 'protocol': 1, 'interface': 1, 'runtime': 'python3', 'entrypoint': 'runner.py', 'tools': ['read_diagnostics', 'login_as_current_user', 'dev_browser_url', 'dev_browser_action', 'dev_browser_record'], 'build': {'inputs': ['runner.py', 'adapter.py'], 'dependencies': [], 'toolchain': 'image-python3', 'flags': ['-I', '-B'], 'assets': ['diagnostics.json', 'diagnostics.skill.md', 'tools', 'skills']}}
def diagnostics_source_path(path):
    """Only the installed native Diagnostics package may use this executable hot lane."""
    if not path.startswith(DIAGNOSTICS_ROOT):return False
    relative=path[len(DIAGNOSTICS_ROOT):]
    return relative in {"runner.py","adapter.py","test_runner.py","plugin.json","diagnostics.json","diagnostics.skill.md","skills/journey_testing.skill.md"} or relative.startswith("tools/")and relative.endswith(".skill.md")
def validate_diagnostics_package(folder):
    """Validate a complete native contract before activating either consumer."""
    folder=Path(folder)
    for name in ("plugin.json","runner.py","adapter.py","test_runner.py","diagnostics.json","diagnostics.skill.md"):
        path=folder/name
        if path.is_symlink()or not path.is_file()or path.stat().st_size>2*1024*1024:raise ValueError("Incomplete Diagnostics package")
    if canonical(json.loads((folder/"plugin.json").read_bytes()))!=canonical(DIAGNOSTICS_MANIFEST):raise ValueError("Incompatible Diagnostics contract")
    declaration=json.loads((folder/"diagnostics.json").read_bytes())
    if declaration.get("tools")!=DIAGNOSTICS_MANIFEST["tools"]or declaration.get("kind")!="ability"or declaration.get("readiness")!="ready":raise ValueError("Incompatible Diagnostics catalog")
    for path in folder.rglob("*"):
        if path.is_symlink():raise ValueError("Diagnostics package symlink")
        if path.is_file()and path.suffix==".py":compile(path.read_bytes(),str(path),"exec")

ABILITY_ROOT="v3/plugins/abilities/"
def ability_asset_path(path):
    if not path.startswith(ABILITY_ROOT):return False
    # Rust adapters and legacy Python helpers still have core/image consumers.
    return path.endswith((".json",".md")) or codebase_source_path(path) or diagnostics_source_path(path)
def generic_package_path(path,blob):
    if not path.startswith(ABILITY_ROOT):return False
    parts=path[len(ABILITY_ROOT):].split("/")
    if len(parts)<3:return False
    entries=release.entries(blob,context_digest)
    prefix=ABILITY_ROOT+"/".join(parts[:2])+"/"
    raw=entries.get(prefix+"plugin.json")
    if raw is None:return False
    manifest=json.loads(raw[1])
    if manifest.get("dispatch")!="isolated":return False
    return "/".join(parts[2:]) in manifest.get("build",{}).get("inputs",[])+manifest.get("build",{}).get("assets",[])+["plugin.json"]
def native_package_migration(path,baseline):
    if not path.startswith(ABILITY_ROOT):return False
    parts=path[len(ABILITY_ROOT):].split("/")
    if len(parts)<3:return False
    prefix=ABILITY_ROOT+"/".join(parts[:2])+"/"
    entries=release.entries(baseline,context_digest)
    raw=entries.get(prefix+parts[1]+".json")
    if raw is None:return False
    installed=entries.get(prefix+"plugin.json")
    if installed is not None and json.loads(installed[1]).get("dispatch")=="isolated":return False
    declaration=json.loads(raw[1])
    return declaration.get("readiness")in("ready","partial")or declaration.get("kind")in("virtual_core","service")or bool(declaration.get("required_module"))
HOT_OPERATIONS={"ui_deploy","index_deploy","plugin_deploy"}
# Limit no-compile publication to UI assets and explicit Code Index metadata paths.
def hot_source_path(path):return path.startswith(UI_SOURCE_PREFIX)or path in INDEX_METADATA_FILES or ability_asset_path(path) or (path.startswith("v3/config/") and path.endswith(".json")) or path=="v3/src/agent_loop/tools/administrator-schemas.json" or path=="v3/tests/test_codebase_plugin.py"

EXPECTED_CODEBASE_MANIFEST={'version': 1, 'id': 'codebase_admin', 'protocol': 1, 'runtime': 'python3', 'entrypoint': 'runner.py', 'interface': 1, 'tools': ['file_list', 'file_read', 'file_search', 'file_edit', 'file_write', 'build_work', 'state_inspect', 'prompt_slot_read', 'prompt_slot_write', 'shell', 'python'], 'build': {'inputs': ['runner.py'], 'dependencies': [], 'toolchain': 'image-python3', 'flags': ['-I', '-B'], 'assets': ['codebase_admin.json', 'codebase_admin.skill.md', 'tools', 'skills']}}
EXPECTED_CODEBASE_DECLARATION={'display_name': 'Codebase Admin', 'kind': 'ability', 'icon': 'folder-cog', 'color': '#bb9af7', 'description': 'Inspect and edit live development files or isolated source tasks, run checks, and release reviewed builds.', 'skill_summary': 'Use file tools without task_id for explicit live edits and runtime configuration; use one task_id for source changes intended for a build. Task creation is automatic. Finish explicitly to deploy.', 'migration_note': 'Unified file and protected broker workflow; runtime authorization remains authoritative.', 'order': 1, 'placeholder': False, 'tools': ['file_list', 'file_read', 'file_search', 'file_edit', 'file_write', 'build_work', 'state_inspect', 'prompt_slot_read', 'prompt_slot_write', 'shell', 'python'], 'readiness': 'ready', 'config': {'bias': {'key': 'destructive_caution', 'label': 'Caution on destructive actions', 'default': 'balanced', 'levels': {'cautious': 'Before any file delete, overwrite, bulk edit, or non-read-only shell command, STOP and ask the user to confirm in plain language what will change — even if they gave a broad instruction. Prefer the smallest reversible step and never act on your own initiative without an explicit go-ahead. (Restarting the server after a change is exempt — use restart_server.)', 'balanced': "Carry out the user's explicit file/shell commands directly, but ask first whenever YOU decide on your own initiative to delete, overwrite, or run something irreversible, and when an action is ambiguous or far-reaching.", 'trusted': "Act decisively on the user's intent without pausing to re-confirm routine writes, edits, or shell commands; only stop for clearly irreversible or repo-wide destructive actions (mass delete, force-push)."}}}}
def validate_codebase_package(folder):
    folder=Path(folder)
    for name in ("plugin.json","runner.py","test_runner.py","codebase_admin.json","codebase_admin.skill.md"):
        path=folder/name
        bound={"plugin.json":4096,"runner.py":2*1024*1024,"test_runner.py":2*1024*1024,"codebase_admin.json":64*1024}.get(name)
        if path.is_symlink() or not path.is_file() or (bound is not None and path.stat().st_size>bound):
            raise ValueError("Incomplete or oversized Codebase Admin package")
    manifest=json.loads((folder/"plugin.json").read_bytes())
    if canonical(manifest)!=canonical(EXPECTED_CODEBASE_MANIFEST):raise ValueError("Incompatible Codebase Admin plugin contract")
    declaration=json.loads((folder/"codebase_admin.json").read_bytes())
    if declaration.get("tools")!=CODEBASE_TOOLS or declaration.get("kind")!="ability" or declaration.get("readiness")!="ready":raise ValueError("Incompatible Codebase Admin catalog")
    for field,bound in (("display_name",100),("description",1000),("skill_summary",2000),("migration_note",2000)):
        value=declaration.get(field)
        if value is None and field in ("skill_summary","migration_note"):continue
        if not isinstance(value,str) or len(value.encode("utf-8"))>bound or (field in ("display_name","description") and not value):
            raise ValueError("Invalid Codebase Admin catalog metadata")
    for field in ("icon","color"):
        if declaration.get(field) is not None and not isinstance(declaration[field],str):
            raise ValueError("Invalid Codebase Admin catalog metadata")
    if type(declaration.get("order",0)) is not int or not -(2**31)<=declaration.get("order",0)<2**31:
        raise ValueError("Invalid Codebase Admin order")
    if declaration.get("config") is not None and not isinstance(declaration["config"],dict):
        raise ValueError("Invalid Codebase Admin configuration")
    mutable={"display_name","description","skill_summary","migration_note","icon","color","config","order"}
    if canonical({k:v for k,v in declaration.items() if k not in mutable})!=canonical({k:v for k,v in EXPECTED_CODEBASE_DECLARATION.items() if k not in mutable}):
        raise ValueError("Codebase Admin interface/settings change needs coordinated installation")
    compile((folder/"runner.py").read_bytes(),str(folder/"runner.py"),"exec")
    for path in folder.rglob("*"):
        if path.is_symlink():raise ValueError("Plugin symlink forbidden")
        if path.is_file() and path.suffix==".md":
            if not path.read_text(encoding="utf-8").strip():raise ValueError("Empty Codebase Admin guidance")
    return hashlib.sha256((folder/"runner.py").read_bytes()).hexdigest()

RUST_UI_FIXTURES = {"v3/ui/chat/chat_ui.json"}
ASSET_ROOT = STATE / "ui-assets"
TARGETS = (CONFIG["api_container"], CONFIG["loop_container"])
LOCAL_WORKSPACE = "71440aad-a6ba-4ba8-adc8-1fcf66250be6"
WORKER_CONTROL = Path(CONFIG["worker_control"])
MAX_LOG = 1024 * 1024
PUBLIC_LOG = 16 * 1024
MAX_CONTEXT = 128 * 1024 * 1024
MIN_DEPLOY_FREE = 8 * 1024 * 1024 * 1024
MIN_SEQUENCE_FREE = 16 * 1024 * 1024 * 1024

_release_file = (_module_root / "release.py"
                 if str(__file__).startswith("/proc/self/fd/") else Path(__file__).with_name("release.py"))
_release_spec = importlib.util.spec_from_file_location(
    "v3_build_release", _release_file)
release = importlib.util.module_from_spec(_release_spec)
_release_spec.loader.exec_module(release)
TASK_CONTEXT = CONTEXT + release.INSPECTION_CONTEXT

_performance_file = (_module_root / "performance.py"
                     if str(__file__).startswith("/proc/self/fd/") else Path(__file__).with_name("performance.py"))
_performance_spec = importlib.util.spec_from_file_location("v3_build_performance", _performance_file)
performance = importlib.util.module_from_spec(_performance_spec)
_performance_spec.loader.exec_module(performance)

MAX_TASK_CONTEXT = 512 * 1024 * 1024


INDEX_ADAPTER_MEMBER = "v3/plugins/abilities/Administrator/code_index/adapter.py"
MAX_INDEX_PARSER_BYTES = 2 * 1024 * 1024


def _protected_parser_sha256(path):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    except OSError as exc:
        raise ValueError("Protected Code Index parser unavailable; install the reviewed parser and restart the protected broker while builds are idle.") from exc
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= MAX_INDEX_PARSER_BYTES:
            raise ValueError("Protected Code Index parser is not a bounded regular file; install the reviewed parser and restart the protected broker while builds are idle.")
        digest = hashlib.sha256()
        total = 0
        while True:
            block = os.read(fd, 65536)
            if not block:
                break
            total += len(block)
            if total > MAX_INDEX_PARSER_BYTES:
                raise ValueError("Protected Code Index parser exceeds its bound; install the reviewed parser and restart the protected broker while builds are idle.")
            digest.update(block)
        if total != info.st_size:
            raise ValueError("Protected Code Index parser changed while being read; retry after installation settles.")
        return digest.hexdigest()
    finally:
        os.close(fd)


def require_paired_index_parser(blob, protected_path):
    """Fail closed unless source and protected indexing code are byte-identical."""
    guidance = "Install the reviewed Code Index parser and restart the protected broker while builds are idle before finishing a build."
    try:
        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:") as archive:
            matches = [member for member in archive
                       if member.name.removeprefix("./") == INDEX_ADAPTER_MEMBER]
            if len(matches) != 1 or not matches[0].isfile() or not 0 < matches[0].size <= MAX_INDEX_PARSER_BYTES:
                raise ValueError("Candidate Code Index parser is missing, ambiguous, or exceeds its bound. " + guidance)
            source = archive.extractfile(matches[0])
            if source is None:
                raise ValueError("Candidate Code Index parser could not be read. " + guidance)
            candidate = source.read(MAX_INDEX_PARSER_BYTES + 1)
            if len(candidate) != matches[0].size or len(candidate) > MAX_INDEX_PARSER_BYTES:
                raise ValueError("Candidate Code Index parser changed or exceeds its bound. " + guidance)
    except (tarfile.TarError, OSError) as exc:
        raise ValueError("Candidate Code Index parser could not be verified. " + guidance) from exc
    installed = _protected_parser_sha256(protected_path)
    if hashlib.sha256(candidate).hexdigest() != installed:
        raise ValueError("Candidate Code Index parser differs from the installed protected parser. " + guidance)


def canonical(value):
    """Serialize stable structured receipts without formatting-dependent differences."""
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def scrub(text, secrets=()):
    for value in sorted(set(secrets), key=len, reverse=True):
        if len(value) >= 4:
            text = text.replace(value, "[redacted]")
    text = re.sub(r"(?i)(authorization\s*[:=]\s*|bearer\s+|cookie\s*[:=]\s*)[^\r\n]+", r"\1[redacted]", text)
    text = re.sub(r"(?i)((?:password|secret|token|api[_-]?key)\s*[=:]\s*)[^\s,;]+", r"\1[redacted]", text)
    return text[-MAX_LOG:]


def context_digest(blob):
    if len(blob) > MAX_TASK_CONTEXT:
        raise ValueError("build context exceeds bound")
    entries = {}
    with tarfile.open(fileobj=io.BytesIO(blob), mode="r:") as archive:
        for member in archive:
            name = member.name.rstrip("/")
            if name.startswith("./"):
                name = name[2:]
            parts = Path(name).parts
            if name.startswith("/") or ".." in parts or not any(name == p or name.startswith(p + "/") for p in TASK_CONTEXT):
                raise ValueError("snapshot escaped build allowlist")
            if "__pycache__" in parts or name.endswith((".pyc", ".pyo")):
                raise ValueError("generated Python cache is not a build input")
            if member.isdir():
                continue
            if not member.isfile() or name in entries:
                raise ValueError("snapshot symlink or duplicate forbidden")
            if member.size > (release.MAX_INSPECTION_FILE if release.inspection_path(name) else 16 * 1024 * 1024):
                raise ValueError("snapshot file exceeds bound")
            body = archive.extractfile(member).read()
            entries[name] = (member.mode & 0o777, len(body), hashlib.sha256(body).hexdigest())
    for required in ("v3/Dockerfile", "v3/Cargo.toml", "v3/Cargo.lock",
                     "v3/data/config/gates/message_visibility_v1.json", "v3/data/config/gates/closing_status_v1.json"):
        if required not in entries:
            raise ValueError("snapshot missing required input")
    if "v3/ui-baseline.json" in entries:
        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:") as archive:
            baseline = json.load(archive.extractfile("v3/ui-baseline.json"))
        for name, expected in baseline["assets"].items():
            if name not in entries or entries[name][2] != expected:
                raise ValueError("shared UI baseline changed")
    return hashlib.sha256(canonical(entries)).hexdigest(), len(entries)



def read_sqlite_snapshot(path, offset, limit):
    """Paginated text export from an immutable task backup, never a live DB.

    No model SQL, extensions, decryption key or writable SQLite connection.
    BLOBs are explicit base64 chunks; plaintext JSON envelopes stay visible.
    """
    if offset > 1000000 or path.stat().st_size > release.MAX_INSPECTION_FILE:
        raise ValueError("database export exceeds bound")
    db = sqlite3.connect(path.as_uri() + "?mode=ro&immutable=1", uri=True, timeout=1)
    deadline = time.monotonic() + 5
    db.set_progress_handler(lambda: int(time.monotonic() > deadline), 1000)
    db.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, 16 * 1024 * 1024)
    db.execute("PRAGMA query_only=ON")
    def lines():
        yield "SQLite task snapshot: read-only; BLOB encoding is base64; sealed values remain ciphertext."
        tables = db.execute("SELECT name,sql FROM sqlite_schema WHERE type='table' ORDER BY name").fetchall()
        if len(tables) > 256:
            raise ValueError("database table count exceeds bound")
        for table, schema in tables:
            yield json.dumps({"table": table, "schema": schema}, ensure_ascii=True)
            if table.startswith("sqlite_") or "CREATE VIRTUAL TABLE" in (schema or "").upper():
                yield json.dumps({"table": table, "rows": "not exported: internal or virtual table"})
                continue
            quoted = '"' + table.replace('"', '""') + '"'
            cursor = db.execute("SELECT * FROM " + quoted)
            columns = [item[0] for item in cursor.description]
            for row_index, row in enumerate(cursor):
                if time.monotonic() > deadline or row_index >= 100000:
                    raise ValueError("database export scan exceeds bound; narrow the page")
                yield json.dumps({"table": table, "row": row_index, "columns": columns})
                for column, value in zip(columns, row):
                    if isinstance(value, bytes):
                        try:
                            parsed = json.loads(value)
                        except (ValueError, UnicodeError):
                            parsed = None
                        if isinstance(parsed, dict) and parsed.get("format") == "v3-admin-plaintext-v1":
                            value = value.decode("utf-8")
                            encoding = "plaintext-envelope-unverified"
                        else:
                            value = base64.b64encode(value).decode("ascii")
                            encoding = "base64"
                    elif isinstance(value, str):
                        encoding = "text"
                    else:
                        yield json.dumps({"column": column, "value": value})
                        continue
                    for part in range(0, max(1, len(value)), 512):
                        yield json.dumps({"column": column, "encoding": encoding,
                                          "part": part // 512, "text": value[part:part + 512]}, ensure_ascii=True)
    try:
        selected = []
        consumed = 0
        for number, line in enumerate(lines(), 1):
            consumed = number
            if time.monotonic() > deadline:
                raise ValueError("database export scan exceeds bound")
            if number < offset:
                continue
            if len(selected) >= limit or len(canonical(selected + [line])) > 16000:
                if not selected:
                    raise ValueError("database export line exceeds result bound")
                return {"lines": selected, "total_lines": None, "has_more": True,
                        "next_offset": offset + len(selected), "format": "sqlite-text-export"}
            selected.append(line)
            consumed = number
        return {"lines": selected, "total_lines": consumed, "has_more": False,
                "next_offset": offset + len(selected), "format": "sqlite-text-export"}
    except sqlite3.Error as error:
        raise ValueError("read-only database export failed or exceeded bound") from error
    finally:
        db.close()


def source_git(root, arguments, patch):
    """Use fixed Git arguments and environment; never execute repository hooks."""
    env = {"PATH": "/usr/bin:/bin", "HOME": "/tmp", "LANG": "C.UTF-8",
           "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
           "GIT_OPTIONAL_LOCKS": "0", "GIT_NO_REPLACE_OBJECTS": "1"}
    root = Path(root)
    try:
        prefix = root.relative_to(SOURCE).as_posix()
    except ValueError:
        prefix = ""
    if prefix and arguments[0] == "apply":
        # Git invoked below its parent repository silently skips paths outside
        # the current prefix. Anchor the patch at the checkout root instead.
        arguments = ["apply", "--directory=" + prefix, *arguments[1:]]
        cwd = SOURCE
    else:
        cwd = root
    command = ["git", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null",
               "-c", "diff.external=", *arguments]
    result = subprocess.run(command, cwd=cwd, env=env, input=patch, capture_output=True, timeout=30)
    if result.returncode or len(result.stdout) > 65536:
        detail = scrub(result.stderr.decode("utf-8", "replace"))[-500:]
        if arguments[0] == "apply" and "--check" in arguments and detail:
            raise ValueError("checked source patch rejected: " + detail)
        raise ValueError("checked source patch rejected")
    return result.stdout


def validate_source_hunks(patch):
    """Recompute hunk counts from the patch body; return the corrected patch.

    Header counts are derivable from the body, so a miscounted header is
    corrected here instead of rejected; the checked git apply in
    inspect_source_patch remains the gate for context mismatches. Malformed
    headers and malformed body lines still raise with the file location.
    """
    path = "unknown source"
    lines = patch.split("\n")
    trailing_newline = lines[-1] == ""
    if trailing_newline:
        lines.pop()
    corrected = []
    index = 0
    while index < len(lines):
        line = lines[index]
        if line.startswith("diff --git "):
            path = scrub(line.split(" b/", 1)[-1])[:240]
        if not line.startswith("@@ "):
            corrected.append(line)
            index += 1
            continue
        match = re.match(r"@@ -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? @@", line)
        if not match:
            raise ValueError(f"source patch malformed hunk header in {path}")
        old_start, new_start = match.group(1), match.group(3)
        body_lines = []
        old_count = new_count = 0
        index += 1
        while index < len(lines) and not lines[index].startswith(("diff --git ", "@@ ")):
            body = lines[index]
            if body.startswith("+"):
                new_count += 1
            elif body.startswith("-"):
                old_count += 1
            elif body.startswith(" "):
                old_count += 1
                new_count += 1
            elif body == "\\ No newline at end of file":
                pass
            else:
                raise ValueError(f"source patch malformed line in {path} after hunk {line[:80]}")
            body_lines.append(body)
            index += 1
        corrected.append(
            f"@@ -{old_start},{old_count} +{new_start},{new_count} @@" + line[match.end():])
        corrected.extend(body_lines)
    result = "\n".join(corrected)
    if trailing_newline:
        result += "\n"
    return result


def prepare_new_source_files(files):
    """Generate exact Git hunks for bounded new text files; do not edit source."""
    if not isinstance(files, list) or not 1 <= len(files) <= 8:
        raise ValueError("prepare requires one to eight new files")
    parts = []
    seen = set()
    total = 0
    for entry in files:
        if not isinstance(entry, dict) or set(entry) != {"path", "content"}:
            raise ValueError("prepare entries require path and content")
        name, content = entry["path"], entry["content"]
        if (not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*", name)
                or any(part in (".", "..") for part in name.split("/")) or name in seen):
            raise ValueError("prepare source path denied")
        if (not isinstance(content, str) or not content or not content.endswith("\n")
                or "\0" in content or "\r" in content):
            raise ValueError("prepare content must be nonempty LF-terminated text")
        total += len(content.encode("utf-8"))
        if total > 16000:
            raise ValueError("prepare text exceeds 16000-byte bound")
        seen.add(name)
        lines = content.split("\n")[:-1]
        parts.append(f"diff --git a/{name} b/{name}\nnew file mode 100644\n"
                     f"--- /dev/null\n+++ b/{name}\n@@ -0,0 +1,{len(lines)} @@\n"
                     + "".join("+" + line + "\n" for line in lines))
    return "".join(parts)


def source_patch_text_counts(patch):
    """Count all text changes, including lines Git would ignore after a short hunk."""
    added = removed = 0
    in_hunk = False
    for line in patch.split("\n"):
        if line.startswith("diff --git "):
            in_hunk = False
        elif line.startswith("@@ "):
            in_hunk = True
        elif in_hunk and line.startswith("+"):
            added += 1
        elif in_hunk and line.startswith("-"):
            removed += 1
    return added, removed


def inspect_source_patch(root, patch):
    """Confine a checked text diff to V3 source; proposals stay separate until here."""
    paths, _corrected = inspect_source_patch_corrected(root, patch)
    return paths


def inspect_source_patch_corrected(root, patch):
    """inspect_source_patch, also returning the count-corrected patch text.

    Handlers that act on a patch (the checked apply, the receipt hash, a
    stored prepared patch) must use the corrected text: it is the text
    whose hunk counts validate_source_hunks recomputed and whose context
    the checked git apply in this function verified.
    """
    if not root.is_dir() or root.is_symlink() or root.stat().st_uid != CONFIG["service_uid"]:
        raise ValueError("live V3 checkout unavailable")
    if patch.startswith("*** Begin Patch"):
        raise ValueError("source patch must be a Git unified diff starting with diff --git; apply_patch markers are unsupported")
    if not patch.startswith("diff --git a/"):
        raise ValueError("source patch must start with diff --git a/<V3-relative-path> b/<V3-relative-path>")
    if any(line.startswith(("GIT binary patch", "Binary files ", "rename from ",
                            "rename to ", "copy from ", "copy to "))
           for line in patch.splitlines()):
        raise ValueError("binary or move patch denied")
    for line in patch.splitlines():
        if line.startswith(("new file mode ", "deleted file mode ", "old mode ", "new mode ")):
            if line.rsplit(" ", 1)[-1] not in ("100644", "100755"):
                raise ValueError("unsupported source file mode")
    patch = validate_source_hunks(patch)
    encoded = patch.encode()
    output = source_git(root, ["apply", "--numstat", "-z", "-"], encoded)
    records = [part for part in output.split(b"\0") if part]
    if not output.endswith(b"\0") or not 1 <= len(records) <= 64:
        raise ValueError("invalid source patch path list")
    counted_added = counted_removed = 0
    paths = []
    allowed_files = {"Cargo.toml", "Cargo.lock", "build.rs", "Dockerfile",
                     ".gitignore", "README.md", "deploy.sh", "launch.py",
                     "worker.py", "ability_plugin_worker.py", "sandbox.py", "promote.py", "ui-baseline.json"}
    allowed_dirs = {"src", "plugins", "tests", "ui", "docs", "config", "build-service"}
    for record in records:
        fields = record.split(b"\t", 2)
        if len(fields) != 3 or not fields[0].isdigit() or not fields[1].isdigit():
            raise ValueError("unsupported source patch")
        counted_added += int(fields[0])
        counted_removed += int(fields[1])
        name = fields[2].decode("utf-8")
        try:
            prefix = root.relative_to(SOURCE).as_posix() + "/"
        except ValueError:
            prefix = ""
        if prefix:
            if not name.startswith(prefix):
                raise ValueError("source patch escaped task workspace")
            name = name[len(prefix):]
        parts = name.split("/")
        if (name in paths or name.startswith("/") or "\\" in name
                or any(ord(char) < 32 or ord(char) == 127 for char in name)
                or any(not part or part in (".", "..") for part in parts)
                or not (name in allowed_files or parts[0] in allowed_dirs and len(parts) > 1)):
            raise ValueError("source patch path denied")
        cursor = root
        for index, part in enumerate(parts):
            cursor = cursor / part
            if cursor.is_symlink() or (cursor.exists() and
                    (not cursor.is_dir() if index < len(parts) - 1 else not cursor.is_file())):
                raise ValueError("source patch path is not a regular source file")
        paths.append(name)
    if (counted_added, counted_removed) != source_patch_text_counts(patch):
        raise ValueError("source patch hunk line counts mismatch")
    # Check and execution must accept the same zero-context hunks; the digest
    # and old text still bind each edit to the reviewed task snapshot.
    source_git(root, ["apply", "--check", "--unidiff-zero", "--whitespace=error", "-"], encoded)
    return paths, patch


def replacement_config(current, old_defaults, new_defaults):
    """Preserve explicit overrides while adopting the new image's defaults."""
    result = dict(current)
    for key in ("Entrypoint", "Cmd", "User", "WorkingDir", "Healthcheck", "StopSignal", "Shell", "ExposedPorts", "Volumes"):
        if current.get(key) == old_defaults.get(key):
            if key in new_defaults:
                result[key] = new_defaults[key]
            else:
                result.pop(key, None)
    def env_map(values):
        return dict(value.split("=", 1) for value in (values or []))
    current_env, old_env, new_env = (env_map(value.get("Env")) for value in (current, old_defaults, new_defaults))
    for key, value in current_env.items():
        if old_env.get(key) != value:
            new_env[key] = value
    result["Env"] = [key + "=" + value for key, value in sorted(new_env.items())]
    labels = dict(new_defaults.get("Labels") or {})
    for key, value in (current.get("Labels") or {}).items():
        if (old_defaults.get("Labels") or {}).get(key) != value:
            labels[key] = value
    result["Labels"] = labels
    return result


class WorkerControlHTTP(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(str(WORKER_CONTROL))


def worker_control(method, path, body=None):
    conn = WorkerControlHTTP("localhost", timeout=5)
    payload = None if body is None else canonical(body)
    try:
        conn.request(method, path, payload, {"Content-Type": "application/json"})
        response = conn.getresponse()
        raw = response.read(4096)
        if response.status == 409:
            raise ValueError("Build worker has an active job or is restarting")
        if response.status not in (200, 202):
            raise RuntimeError("Build worker control unavailable")
        return json.loads(raw)
    finally:
        conn.close()


class DockerHTTP(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect("/var/run/docker.sock")


def loop_plugin_mounts(previous):
    """Expose live removable packages without granting checkout write access."""
    host = dict(previous)
    target = "/live-checkout/plugins"
    host["Mounts"] = [dict(item) for item in host.get("Mounts") or []
                      if item.get("Target") != target]
    # Replace either Docker mount representation to prevent inherited shadowing.
    if host.get("Binds") is not None:
        host["Binds"] = [item for item in host["Binds"]
                         if len(item.split(":")) < 2 or item.split(":")[1] != target]
    host["Mounts"].append({"Type": "bind", "Source": str(SOURCE / "plugins"),
                           "Target": target, "ReadOnly": True})
    return host


@contextlib.contextmanager
def pinned_directory(parent,name,owner=None):
    """Hold a directory inode until Docker has mounted it; never follow a symlink."""
    parent_fd=os.open(parent,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW|os.O_CLOEXEC)
    fd=None
    try:
        fd=os.open(name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW|os.O_CLOEXEC,dir_fd=parent_fd)
        if owner is not None and os.fstat(fd).st_uid!=owner:raise PermissionError("Workspace identity changed")
        yield "/proc/"+str(os.getpid())+"/fd/"+str(fd)
    finally:
        if fd is not None:os.close(fd)
        os.close(parent_fd)

class Runner:
    def __init__(self, source=SOURCE, snapshot_helper=None, snapshot_socket=None):
        self.source, self.snapshot_helper = Path(source), snapshot_helper
        self.source_config=CONFIG
        self.secrets = []
        self.snapshot_socket = snapshot_socket or str(Path(CONFIG["runtime_root"])/"snapshot.sock")
        self.cancel = threading.Event()
        self.job_context = threading.local()

    def command(self, argv, stdin=None, timeout=3600, output_file=None, private_raw=False):
        # Explicit arguments only; the model never supplies argv. Environment
        # has no application tokens, cookies or provider credentials.
        env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": CONFIG["service_home"], "LANG": "C.UTF-8"}
        with subprocess.Popen(argv, cwd=self.source, env=env, stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True) as proc:
            output = bytearray()
            def feed():
                try:
                    if stdin is not None:
                        proc.stdin.write(stdin)
                        proc.stdin.close()
                except (BrokenPipeError, OSError):
                    pass
            writer = threading.Thread(target=feed, daemon=True)
            writer.start()
            deadline = time.monotonic() + timeout
            import select
            try:
                while True:
                    job = getattr(self.job_context, "job", None)
                    event = getattr(self.job_context, "event", None)
                    if self.cancel.is_set() or (event is not None and event.is_set()
                                                and not job.get("effects_started")):
                        error = PermissionError("host job stop requested; reconcile any interrupted effects")
                        # Private Docker inspection may contain values not yet
                        # learned by the scrubber. Never publish partial raw data.
                        error.log = "" if private_raw else scrub(bytes(output).decode("utf-8", "replace"), self.secrets)
                        raise error
                    if time.monotonic() > deadline:
                        raise TimeoutError("fixed build task timeout")
                    if select.select([proc.stdout], [], [], .2)[0]:
                        data = os.read(proc.stdout.fileno(), 65536)
                        if not data:
                            break
                        if output_file is not None:
                            output_file.write(data)
                        else:
                            output.extend(data)
                            if len(output) > MAX_LOG:
                                if private_raw:
                                    raise ValueError("private inspect exceeds bound")
                                del output[:-MAX_LOG]
                    elif proc.poll() is not None:
                        continue
                code = proc.wait(timeout=5)
                writer.join(timeout=5)
                text = scrub(bytes(output).decode("utf-8", "replace"), self.secrets)
                if code:
                    error = RuntimeError("fixed command failed: " + text)
                    error.exit_code = code
                    raise error
                return bytes(output).decode("utf-8", "strict") if private_raw else text
            finally:
                event = getattr(self.job_context, "event", None)
                job = getattr(self.job_context, "job", None)
                stopping = self.cancel.is_set() or (event is not None and event.is_set() and not job.get("effects_started"))
                if proc.poll() is None or stopping:
                    try:
                        os.killpg(proc.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        pass
                    # The leader may exit on TERM while descendants survive.
                    # Reap the entire original group, never a shared runner PID.
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    proc.wait()

    def ensure_workspace(self,workspace):
        # Caller identity and SID are already validated; paths/UIDs never come from arguments.
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as connection:
            connection.settimeout(10);connection.connect(self.snapshot_socket)
            connection.sendall(canonical({"workspace":workspace})+b"\n")
            def receive(count):
                data=bytearray()
                while len(data)<count:
                    part=connection.recv(count-len(data))
                    if not part:raise ValueError("Workspace helper disconnected")
                    data.extend(part)
                return bytes(data)
            size=struct.unpack("!Q",receive(8))[0]
            if not 0<size<=256:raise PermissionError("Workspace initialization rejected")
            result=json.loads(receive(size))
            if result.get("workspace")!=workspace:raise PermissionError("Workspace helper scope changed")
            return result

    def snapshot(self, inspection=False):
        # Helper path is service configuration, never a tool argument. It must
        # be root-owned and immutable to the development source identity.
        if self.snapshot_socket:
            conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            conn.settimeout(60)
            try:
                conn.connect(self.snapshot_socket)
                conn.sendall(b'{"inspection":true}\n' if inspection else b"{}\n")
                def receive(count):
                    data = bytearray()
                    while len(data) < count:
                        part = conn.recv(min(count-len(data), 1024*1024))
                        if not part:
                            raise ValueError("protected snapshot helper unavailable")
                        data.extend(part)
                    return bytes(data)
                size = struct.unpack("!Q", receive(8))[0]
                if not 0 < size <= (MAX_TASK_CONTEXT if inspection else MAX_CONTEXT):
                    raise ValueError("protected snapshot size rejected")
                blob = receive(size)
                context_digest(blob)
                return blob
            finally:
                conn.close()
        if inspection:
            raise ValueError("inspection requires protected snapshot socket")
        if self.snapshot_helper:
            helper = Path(self.snapshot_helper)
            info = helper.stat()
            if info.st_uid != 0 or info.st_mode & 0o022:
                raise ValueError("snapshot helper must be protected root-owned code")
            argv = ["sudo", "-n", str(helper)]
        else:
            argv = ["sudo", "-n", "tar", "--exclude=__pycache__", "--exclude=*.pyc", "--exclude=*.pyo", "-cf", "-", *CONTEXT]
        class BoundedContext(io.BytesIO):
            def write(self, data):
                if self.tell() + len(data) > MAX_CONTEXT:
                    raise ValueError("build context exceeds bound")
                return super().write(data)
        result = BoundedContext()
        self.command(argv, timeout=60, output_file=result)
        blob = result.getvalue()
        context_digest(blob)
        return blob

    def promote_checkout(self, before, after, changed):
        return release.promote_checkout(self.source, before, after, changed, context_digest, standalone=True)

    def promote_live_ui(self, before, after, changed):
        # The app deliberately serves the checkout in live-editing mode.
        # Once this release is active/healthy, promote only conflict-free UI
        # before checking served bytes. Final full promotion stays checked.
        settings = self.source / "data/config/app-settings.json"
        try:
            enabled = json.loads(settings.read_text())["development"]["live_ui_editing"] is True
        except (OSError, ValueError, KeyError, TypeError):
            enabled = False
        if enabled:
            self.promote_checkout(before, after, [p for p in changed if p.startswith(UI_SOURCE_PREFIX)])
        return enabled

    def checkout_matches(self, blob, changed):
        return release.checkout_matches(self.source, blob, changed, context_digest, standalone=True)

    def readable_assets(self,destination):
        # Only allowlisted, frozen source/index artifacts cross into application
        # containers. Private task inspection snapshots are never published here.
        for path in [destination,*destination.rglob("*")]:
            if path.is_symlink():raise PermissionError("Published asset symlink forbidden")
            if path.is_dir():path.chmod(0o755)
            elif not path.is_file() or path.stat().st_mode&0o444!=0o444:raise PermissionError("Published asset is not readable source")

    def stage_assets(self, blob, job_id):
        scanner = Path(CONFIG["service_dir"])/"index-scanner.py"
        require_paired_index_parser(blob, scanner)
        with tarfile.open(fileobj=io.BytesIO(blob))as archive:
            try:engine=archive.extractfile("v3/plugins/abilities/Administrator/code_index/maintenance.py").read()
            except KeyError:raise ValueError("Candidate maintenance engine missing")from None
        if hashlib.sha256(engine).hexdigest()!=hashlib.sha256(Path(_maintenance_path).read_bytes()).hexdigest():raise ValueError("Install the reviewed maintenance engine while the broker is idle before publication")
        destination = release.stage_assets(ASSET_ROOT, job_id, blob, context_digest)
        self.readable_assets(destination)
        index = destination / ".code-index"
        for orphan in destination.glob(".index-stage-*"):
            uuid.UUID(orphan.name.removeprefix(".index-stage-"))
            if orphan.is_symlink() or not orphan.is_dir():
                raise ValueError("Invalid interrupted index staging directory")
            shutil.rmtree(orphan)
        if index.exists():
            release.verify_index(destination, job_id, context_digest(blob)[0])
            return destination
        provider=getattr(self,"maintenance_provider",None)
        reviews=provider(job_id)if callable(provider)else []
        input_folder=Path(getattr(self,"maintenance_snapshot_folder",STATE/"code-index-maintenance"/"snapshots"));input_folder.mkdir(mode=0o700,parents=True,exist_ok=True)
        input_path=input_folder/(job_id+".json")
        payload=canonical(reviews)
        if len(payload)>64*1024*1024:raise ValueError("Maintenance snapshot exceeds bound")
        fd=os.open(input_path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600)
        with os.fdopen(fd,"wb")as stream:stream.write(payload);stream.flush();os.fsync(stream.fileno())
        # Candidate Python is never executed; the matched protected parser is mounted read-only.
        temporary = destination / (".index-stage-" + str(uuid.uuid4()))
        temporary.mkdir(mode=0o755)
        try:
            image = self.inspect(TARGETS[0])["Image"]
            program = ("import runpy,json; m=runpy.run_path('/scanner.py'); "
                       "m['build_published']('/source','/output'," + repr(job_id) +
                       "," + repr(context_digest(blob)[0]) + ",json.load(open('/maintenance.json')))")
            self.command(["docker", "run", "--rm", "--network", "none",
                "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                "--user", str(CONFIG["service_uid"])+":"+str(CONFIG["service_gid"]), "--memory", "1g", "--pids-limit", "64",
                "--tmpfs", "/tmp:rw,size=64m,mode=1777",
                "-v", str(input_path)+":/maintenance.json:ro",
                "-v", str(scanner)+":/scanner.py:ro",
                "-v", str(destination)+":/source:ro",
                "-v", str(temporary)+":/output:rw", "--entrypoint", "python3",
                image, "-B", "-c", program], timeout=120)
            fd = os.open(temporary, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:os.fsync(fd)
            finally:os.close(fd)
            os.rename(temporary, index)
            fd = os.open(destination, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:os.fsync(fd)
            finally:os.close(fd)
            release.verify_index(destination, job_id, context_digest(blob)[0])
            self.readable_assets(destination)
        finally:
            if temporary.exists():
                shutil.rmtree(temporary)
        return destination

    def maintenance_inventory(self,root,scope,folder):
        """Scan with the protected engine in the same API-image parser environment as publication."""
        folder=Path(folder);folder.mkdir(mode=0o700,parents=True,exist_ok=True)
        invocation=folder/("scan-"+uuid.uuid4().hex);invocation.mkdir(mode=0o700)
        scanner=Path(CONFIG["service_dir"])/"index-scanner.py"
        engine=Path(CONFIG["service_dir"])/"index-maintenance.py"
        image=self.inspect(TARGETS[0])["Image"]
        program=("import runpy,json; m=runpy.run_path('/index-maintenance.py'); "
                 "value=m['source_inventory'](m['load_scanner'](),'/source',"+repr(scope)+",'/output'); "
                 "json.dump(value,open('/output/inventory.json','w'))")
        try:
            self.command(["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL",
                "--security-opt","no-new-privileges","--user",str(CONFIG["service_uid"])+":"+str(CONFIG["service_gid"]),
                "--memory","1g","--pids-limit","64","--tmpfs","/tmp:rw,size=64m,mode=1777",
                "-v",str(root)+":/source:ro","-v",str(invocation)+":/output:rw",
                "-v",str(scanner)+":/index-scanner.py:ro","-v",str(engine)+":/index-maintenance.py:ro",
                "--entrypoint","python3",image,"-I","-B","-c",program],timeout=120)
            result=invocation/"inventory.json"
            if result.is_symlink()or result.stat().st_size>64*1024*1024:raise ValueError("Maintenance inventory exceeds bound")
            files,units,scan=json.loads(result.read_bytes());scan["parser_environment"]=image
            os.replace(invocation/"index.sqlite",folder/"index.sqlite")
            shutil.rmtree(invocation)
            return files,units,scan
        except BaseException:
            shutil.rmtree(invocation);raise

    def build_ability_artifacts(self,job_id):
        root=ASSET_ROOT/"releases"/job_id
        outputs=[]
        for declaration in sorted((root/ABILITY_ROOT).glob("*/*/plugin.json")):
            manifest=json.loads(declaration.read_bytes())
            if manifest.get("dispatch")!="isolated"or manifest.get("build",{}).get("toolchain")!="rust-1.90":continue
            package=declaration.parent;build=manifest["build"];entry=manifest.get("entrypoint")
            if manifest.get("runtime")!="executable"or not isinstance(entry,str)or not re.fullmatch(r"[a-z][a-z0-9_]{0,63}",entry)or build.get("dependencies")!=[]or build.get("flags")!=[]:
                raise ValueError("Unsupported standalone Rust build contract")
            names=build.get("inputs")
            if not isinstance(names,list)or len(names)>1024 or not {"Cargo.toml","Cargo.lock"}<=set(names)or len(names)!=len(set(names)):raise ValueError("Locked declared Rust inputs required")
            inputs={}
            for name in names:
                if not isinstance(name,str)or not re.fullmatch(r"[A-Za-z0-9_./-]+",name)or any(p in ("",".","..")for p in name.split("/")):raise ValueError("Unsafe build input")
                path=package/name
                if path.is_symlink()or not path.is_file()or path.stat().st_size>16*1024*1024:raise ValueError("Invalid build input")
                inputs[name]=hashlib.sha256(path.read_bytes()).hexdigest()
            for source in package.rglob("*.rs"):
                if str(source.relative_to(package))not in inputs:raise ValueError("Undeclared Rust source")
                text=source.read_text()
                for match in re.finditer(r"include(?:_str|_bytes)?!\s*\(\s*([^)]+)\)",text):
                    raw=match[1].strip()
                    if not re.fullmatch(r'"[^"]+"',raw):raise ValueError("Dynamic compile include is not supported")
                    include=(source.parent/raw[1:-1]).resolve()
                    if not include.is_relative_to(package)or str(include.relative_to(package))not in inputs:raise ValueError("Undeclared compile include")
            # Runtime assets belong to the bundle generation, not executable compilation.
            compile_build={k:v for k,v in build.items()if k!="assets"}
            key=hashlib.sha256(canonical({"build":compile_build,"entrypoint":entry,"inputs":inputs})).hexdigest()
            legacy_key=hashlib.sha256(canonical({"build":build,"entrypoint":entry,"inputs":inputs})).hexdigest()
            artifact=ASSET_ROOT/"artifacts"/key;binary=artifact/entry
            if binary.is_file():
                outputs.append({"package":manifest["id"],"build_key":key,"reused":True});continue
            legacy=ASSET_ROOT/"artifacts"/legacy_key/entry
            if legacy.is_file()and not legacy.is_symlink():
                artifact.mkdir(parents=True,exist_ok=True);temporary=artifact/("."+entry+".new")
                shutil.copyfile(legacy,temporary);temporary.chmod(0o555);os.replace(temporary,binary)
                outputs.append({"package":package.name,"build_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"reused":True,"migrated_from":legacy_key})
                continue
            cache=STATE/"plugin-builds"/key;cache.mkdir(parents=True,exist_ok=True)
            # Only the declared application package enters the builder. No credentials,
            # state, workspaces, control sockets or network enter compilation.
            source=cache/"source";source.mkdir(exist_ok=True)
            for name in names:
                target=source/name;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes((package/name).read_bytes())
            self.command(["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges",
                "--user",str(CONFIG["service_uid"])+":"+str(CONFIG["service_gid"]),"--pids-limit","128","--memory","2g","--cpus","1",
                "--tmpfs","/tmp:rw,size=128m,mode=1777","--mount","type=bind,src="+str(source)+",dst=/package,readonly",
                "--mount","type=bind,src="+str(cache)+",dst=/output","-w","/package",
                "-e","CARGO_HOME=/output/cargo","-e","CARGO_TARGET_DIR=/output/target","-e","CARGO_BUILD_JOBS=1","-e","CARGO_PROFILE_RELEASE_DEBUG=0",
                "rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f",
                "cargo","build","--release","--locked","--offline","--bin",entry],timeout=600)
            output=cache/"target/release"/entry
            if output.is_symlink()or not output.is_file()or output.stat().st_size>32*1024*1024:raise ValueError("Invalid plugin artifact")
            artifact.mkdir(parents=True,exist_ok=True);temporary=artifact/("."+entry+".new")
            shutil.copyfile(output,temporary);temporary.chmod(0o555);os.replace(temporary,binary)
            outputs.append({"package":manifest["id"],"build_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"reused":False})
        return outputs

    def validate_ui(self, job_id, changed):
        root = ASSET_ROOT / "releases" / job_id
        (ASSET_ROOT/"artifacts").mkdir(parents=True,exist_ok=True)
        artifacts=self.build_ability_artifacts(job_id)
        active=getattr(getattr(self,"job_context",None),"job",None)
        if active is not None:active["plugin_artifacts"]=artifacts
        if any(codebase_source_path(name) for name in changed):
            validate_codebase_package(root / CODEBASE_ROOT)
            image=self.inspect(TARGETS[0])["Image"]
            # Validate in the retained consumer runtime, not host Python.
            # No application state, credentials, network or sockets enter this process.
            self.command(["docker","run","--rm","--network","none","--read-only",
                "--cap-drop","ALL","--security-opt","no-new-privileges",
                "--user",str(CONFIG["worker_uid"])+":"+str(CONFIG["worker_gid"]),
                "--pids-limit","64","--memory","256m","--cpus","1",
                "--tmpfs","/tmp:rw,size=64m,mode=1777",
                "--mount","type=bind,src="+str(root / CODEBASE_ROOT)+",dst=/package,readonly",
                "-w","/tmp","--entrypoint","python3",image,"-I","-B","/package/test_runner.py"],timeout=30)
        if any(diagnostics_source_path(name) for name in changed):
            validate_diagnostics_package(root / DIAGNOSTICS_ROOT)
            if (active is None or active.get("operation")!="build_deploy") and not self.diagnostics_hot_swappable():
                raise ValueError("Installed worker lacks native Diagnostics package support")
            image=(active["images"]["api"] if active is not None and active.get("operation")=="build_deploy" else self.inspect(TARGETS[0])["Image"])
            self.command(["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges",
                "--user",str(CONFIG["worker_uid"])+":"+str(CONFIG["worker_gid"]),"--pids-limit","64","--memory","256m",
                "--tmpfs","/tmp:rw,size=64m,mode=1777","--mount","type=bind,src="+str(root / DIAGNOSTICS_ROOT)+",dst=/package,readonly",
                "--entrypoint","python3",image,"-I","-B","/package/test_runner.py"],timeout=60)
        if any(name.startswith(ABILITY_ROOT) or name.startswith("v3/config/") or name=="v3/src/agent_loop/tools/administrator-schemas.json" for name in changed):
            # A foundation release introduces the validator itself. Validate with
            # its already-tested candidate API image; hot updates use the retained consumer.
            image=(active["images"]["api"] if active is not None
                and active.get("operation")=="build_deploy" else self.inspect(TARGETS[0])["Image"])
            result=self.command(["docker","run","--rm","--network","none","--read-only",
                "--cap-drop","ALL","--security-opt","no-new-privileges",
                "--user",str(CONFIG["app_uid"])+":"+str(CONFIG["app_gid"]),
                "--pids-limit","64","--memory","256m","--tmpfs","/tmp:rw,size=64m,mode=1777",
                "--mount","type=bind,src="+str(root)+",dst=/candidate,readonly",
                "--mount","type=bind,src="+str(ASSET_ROOT/"artifacts")+",dst=/opt/v3/assets/artifacts,readonly",
                "--entrypoint","/usr/local/bin/webagent-v3",image,"--validate-assets=/candidate/v3"],timeout=30)
            receipt=json.loads(result)
            program=("import json,pathlib; root=pathlib.Path('/candidate/v3/plugins/abilities'); "
                "packages=[p for p in root.glob('*/*/plugin.json') if json.loads(p.read_bytes()).get('dispatch')=='isolated']; "
                "[(compile(source.read_bytes(),str(source),'exec')) for p in packages "
                "if json.loads(p.read_bytes()).get('runtime')=='python3' for source in p.parent.rglob('*.py')]; print('syntax valid')")
            self.command(["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges",
                "--user",str(CONFIG["worker_uid"])+":"+str(CONFIG["worker_gid"]),"--pids-limit","64","--memory","256m",
                "--tmpfs","/tmp:rw,size=64m,mode=1777","--mount","type=bind,src="+str(root)+",dst=/candidate,readonly",
                "--entrypoint","python3",image,"-I","-B","-c",program],timeout=30)
            if not isinstance(receipt.get("catalog_version"),str):raise ValueError("Installed catalog validator unavailable")
        for name in changed:
            if not name.startswith(UI_SOURCE_PREFIX):
                path=root/name
                if path.is_file()and path.suffix==".json":json.loads(path.read_text(encoding="utf-8"))
                continue
            path = root / name
            if not path.is_file():
                continue
            if name.endswith(".json"):
                json.loads(path.read_text(encoding="utf-8"))
            elif name.endswith(".js"):
                self.command(["node", "--check", str(path)], timeout=10)

    def current_asset_id(self):
        return release.current_asset_id(ASSET_ROOT)

    def activate_assets(self, job_id):
        previous=release.activate_assets(ASSET_ROOT, job_id)
        callback=getattr(self,"maintenance_activated",None)
        if callable(callback):callback(job_id)
        return previous

    def ui_hot_swappable(self):
        api = self.inspect(TARGETS[0])
        mounts = api.get("Mounts") or []
        env = api.get("Config", {}).get("Env") or []
        return (any(item.get("Destination") == "/opt/v3/assets" and
                    item.get("Source") == str(ASSET_ROOT) and not item.get("RW")
                    for item in mounts) and
                "V3_UI_ROOT=/opt/v3/assets/current/v3/ui" in env)

    def core_compile_inputs(self):
        import posixpath
        raw=self.command(["docker","exec",TARGETS[0],"cat","/usr/local/lib/webagent/core-inputs.txt"],timeout=10)
        paths=set()
        for path in raw.splitlines():
            if path.startswith("/build/"):path=path[len("/build/"):]
            normalized=posixpath.normpath(path)
            if normalized.startswith(("src/","plugins/","config/")):paths.add("v3/"+normalized)
        if "v3/src/main.rs"not in paths:raise ValueError("Installed compiler dependency manifest unavailable")
        return paths
    def diagnostics_hot_swappable(self):
        """Parse fixed private capability metadata before environment-value redaction.

        Raw output stays local to this exact comparison; no frame is published.
        """
        program=("import socket,json; s=socket.socket(socket.AF_UNIX); s.settimeout(10); "
            "s.connect('/run/v3/worker/tools.sock'); "
            "s.sendall(json.dumps({'id':'diagnostics-protocol','name':'diagnostics_protocol',"
            "'workspace':'"+LOCAL_WORKSPACE+"','arguments':{}}).encode()+bytes([10])); "
            "print(s.makefile('rb').readline(4096).decode())")
        try:
            reply=json.loads(self.command(["docker","exec","--user",str(CONFIG["app_uid"]),TARGETS[0],"python3","-I","-B","-c",program],timeout=15,private_raw=True))
            return reply.get("status")=="success" and reply.get("output")=={"protocol":1,"interface":1,"stateful":True}
        except Exception:return False
    def plugin_hot_swappable(self):
        for name in TARGETS:
            value=self.inspect(name)
            if not any(m.get("Destination")=="/opt/v3/assets" and m.get("Source")==str(ASSET_ROOT)
                       and not m.get("RW") for m in value.get("Mounts",[])):
                return False
            if "V3_UI_ROOT=/opt/v3/assets/current/v3/ui" not in value.get("Config",{}).get("Env",[]):
                return False
        # Ask the installed worker contract, not a filename or source-byte guess.
        program=("import socket,json; s=socket.socket(socket.AF_UNIX); s.settimeout(10); "
            "s.connect('/run/v3/worker/tools.sock'); "
            "s.sendall(json.dumps({'id':'plugin-protocol','name':'ability_plugins_protocol',"
            "'workspace':'"+LOCAL_WORKSPACE+"','arguments':{}}).encode()+bytes([10])); "
            "print(s.makefile('rb').readline(4096).decode())")
        try:
            reply=json.loads(self.command(["docker","exec","--user",str(CONFIG["app_uid"]),
                TARGETS[0],"python3","-I","-B","-c",program],timeout=15))
            return reply.get("status")=="success" and reply.get("output")=={"protocol":1,"catalog":1,"dispatch":1}
        except (OSError,RuntimeError,ValueError):return False

    def verify_plugin_adoption(self,job_id):
        # A healthy shared server may queue readiness behind catalog work. Keep
        # each attempt bounded while allowing the protected consumer to respond.
        deadline=time.monotonic()+45
        program=("import json,pathlib; root=pathlib.Path('/state/publication'); "
                 "print(json.dumps({r:json.loads((root/(r+'.json')).read_text()) for r in ['api','loop']}))")
        while time.monotonic()<deadline:
            # Readiness acquires an API snapshot; the scheduler acquires the loop snapshot.
            connection=http.client.HTTPConnection({"0.0.0.0":"127.0.0.1","::":"::1"}.get(CONFIG["bind_address"],CONFIG["bind_address"]),CONFIG["port"],timeout=15)
            try:
                connection.request("GET","/health/ready");response=connection.getresponse();ready=json.loads(response.read(4096))
                receipt=json.loads(self.command(["docker","exec","--user",str(CONFIG["app_uid"]),TARGETS[0],"python3","-I","-B","-c",program],timeout=10,private_raw=True))
                if response.status==200 and all(receipt[r]["revision"]==job_id and receipt[r]["catalog_version"]==ready["catalog_version"]for r in ("api","loop")):
                    return {r:{"revision":job_id,"catalog_version":ready["catalog_version"]}for r in ("api","loop")}
            except (OSError,RuntimeError,ValueError,KeyError):pass
            finally:connection.close()
            time.sleep(.25)
        raise RuntimeError("API and loop did not adopt the published catalog")
    def verify_ui(self, changed=()):
        # Static routes are public and must serve the exact selected bytes.
        paths = [("v3/ui/index.html", "/app")]
        paths.extend((name, "/v3-ui/" + name[len(UI_SOURCE_PREFIX):])
                     for name in changed if name.startswith(UI_SOURCE_PREFIX) and
                     name != "v3/ui/index.html")
        for name, route in paths[:17]:
            expected_path = ASSET_ROOT / "current" / name
            expected = expected_path.read_bytes() if expected_path.is_file() else None
            connection = http.client.HTTPConnection({"0.0.0.0":"127.0.0.1","::":"::1"}.get(CONFIG["bind_address"],CONFIG["bind_address"]), CONFIG["port"], timeout=10)
            try:
                connection.request("GET", quote(route, safe="/"))
                response = connection.getresponse()
                body = response.read(16 * 1024 * 1024 + 1)
                if expected is None and response.status == 404:
                    continue
                if response.status != 200 or body != expected:
                    raise RuntimeError("served UI does not match frozen asset release: " + name)
            finally:
                connection.close()

    def inspect(self, name):
        if name not in TARGETS and not any(re.fullmatch(re.escape(target)+r"-pre-build-[0-9a-f-]{36}",name) for target in TARGETS):
            raise ValueError("container outside development target")
        value = json.loads(self.command(["docker", "inspect", name], timeout=30, private_raw=True))[0]
        self.secrets.extend(item.partition("=")[2] for item in value.get("Config", {}).get("Env", []) if "=" in item)
        return value

    def check_space(self, minimum):
        free = shutil.disk_usage(self.source).free
        if free < minimum:
            raise ValueError(f"development build needs {minimum // (1024 ** 3)} GiB free; {free // (1024 ** 3)} GiB available")
        return free

    def prepare_build_space(self):
        # Require room for both images before compiling or changing containers.
        return self.check_space(MIN_SEQUENCE_FREE)

    def cargo_task(self, source, check_dir, task, timeout):
        # Full Rust tests intentionally edit and restore their manifest-local
        # configuration. Supply a disposable application-only copy: neither
        # frozen task inputs nor inspection database backups are writable.
        image = "rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
        command = ["cargo", "check", "--locked"] if task == "check" else ["cargo", "test", "--locked", "--", "--test-threads=1"]
        check_dir.mkdir(parents=True, exist_ok=True)
        scratch = None
        working = "/build/v3"
        if task == "test":
            scratch = check_dir / ("scratch-" + uuid.uuid4().hex)
            blob = release.snapshot(Path(source), CONTEXT, context_digest)
            release.extract(scratch, blob, context_digest)
            working = "/check/" + scratch.name + "/v3"
        source_mount = [] if task == "test" else [
            "--mount", "type=bind,src=" + str(source) + ",dst=/build,readonly"]
        try:
            return self.command(["docker", "run", "--rm", "--user", str(CONFIG["service_uid"])+":"+str(CONFIG["service_gid"]),
                                 *source_mount,
                                 "--mount", "type=bind,src=" + str(check_dir) + ",dst=/check",
                                 "-e", "CARGO_HOME=/check/cargo", "-e", "CARGO_TARGET_DIR=/check/target",
                                 "-e", "CARGO_BUILD_JOBS=2", "-w", working, image, *command],
                                timeout=timeout)
        finally:
            if scratch is not None:
                shutil.rmtree(scratch)

    @contextlib.contextmanager
    def workspace_volumes(self,base,selected):
        # Docker's local volume driver accepts pinned FD devices. Runc rejects
        # direct /proc FD bind sources; daemon-owned mountpoints retain the inode.
        created=[]
        with pinned_directory(base.parent,base.name,CONFIG["worker_uid"]) as writable, pinned_directory(selected.parent,selected.name,CONFIG["service_uid"]) as readonly:
            try:
                for device in (writable,readonly):
                    volume="webagent-workspace-pin-"+uuid.uuid4().hex
                    self.command(["docker","volume","create","--driver","local","--opt","type=none","--opt","device="+device,"--opt","o=bind",volume],timeout=30)
                    created.append(volume)
                yield tuple(created)
            finally:
                for volume in reversed(created):
                    subprocess.run(["docker","volume","rm",volume],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=30)

    def workspace_task(self,base,selected,image,name,args,job):
        with self.workspace_volumes(base,selected) as (base_mount,selected_mount):
            container="webagent-workspace-"+uuid.uuid4().hex
            spec=json.dumps({"name":name,"arguments":args,"workspace":base.name})
            argv=["docker","run","--rm","--name",container,"--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges","--user",str(CONFIG["worker_uid"])+":"+str(CONFIG["app_gid"]),"--pids-limit","128","--memory","1g","--cpus","1","--tmpfs","/tmp:rw,size=64m,mode=1777","--tmpfs","/run:rw,size=16m,mode=1777","--mount","type=volume,src="+base_mount+",dst=/workspaces/"+base.name+",volume-nocopy","--mount","type=volume,src="+selected_mount+",dst=/opt/v3/assets/releases/"+selected.name+",readonly,volume-nocopy","--env","V3_CODE_TEMPLATE=/opt/v3/assets/releases/"+selected.name+"/v3","--env","V3_BUILD_EXECUTOR=1","--entrypoint","python3",image,"-I","/opt/v3/build-service/workspace_executor.py",spec]
            finished=threading.Event()
            def guard():
                while not finished.wait(.5):
                    if job.get("revoked") or job["lease_until"]<=time.time() or self.cancel.is_set():
                        subprocess.run(["docker","stop","--time","2",container],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=10)
                        return
            watcher=threading.Thread(target=guard,daemon=True);watcher.start()
            try:return json.loads(self.command(argv,timeout=650,private_raw=True))
            finally:
                finished.set();watcher.join(timeout=12)
                subprocess.run(["docker","rm","-f",container],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=10)
    def build(self, blob, job_id, role):
        tag = TARGETS[1 if role=="loop" else 0]+":job-"+job_id
        log = self.command(["docker", "buildx", "build", "--builder", "default", "--load",
                            "--progress=plain", "--target", role, "-f", "v3/Dockerfile",
                            "-t", tag, "-"], stdin=blob)
        image = self.command(["docker", "image", "inspect", "--format", "{{.Id}}", tag], timeout=30).strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
            raise ValueError("invalid image identity")
        return image, log

    def export(self, images, destination):
        if any(not re.fullmatch(r"sha256:[0-9a-f]{64}", image) for image in images):
            raise ValueError("invalid rollback image")
        temporary = destination.with_name(".rollback-" + uuid.uuid4().hex + ".tar")
        try:
            self.command(["docker", "save", "-o", str(temporary), *images])
            os.chmod(temporary, 0o600)
            fd = os.open(temporary, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
            try:
                if not stat.S_ISREG(os.fstat(fd).st_mode):
                    raise ValueError("rollback export is not a regular file")
                os.fsync(fd)
            finally:
                os.close(fd)
            os.replace(temporary, destination)
            parent = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(parent)
            finally:
                os.close(parent)
        finally:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass

    def stop(self, name):
        self.command(["docker", "stop", "--time", "30", name], timeout=60)

    def rename(self, name, target):
        self.command(["docker", "rename", name, target], timeout=30)

    def create(self, name, image, previous, release_root=None):
        # Docker engine JSON preserves the inspected container configuration,
        # including private Env values, without shell escaping or printing it.
        old_defaults = json.loads(self.command(["docker", "image", "inspect", previous["Image"]], timeout=30, private_raw=True))[0]["Config"]
        new_defaults = json.loads(self.command(["docker", "image", "inspect", image], timeout=30, private_raw=True))[0]["Config"]
        config = replacement_config(previous["Config"], old_defaults, new_defaults)
        config["Image"] = image
        host = dict(previous["HostConfig"])
        if release_root is not None:
            # Image releases adopt the protected operator budget; retained-image
            # reconfiguration already carries its explicitly selected HostConfig.
            if name == TARGETS[0]:host["PidsLimit"]=CONFIG["api_pids_limit"]
            host = release.release_mounts(host, Path(release_root), ASSET_ROOT)
            if name in TARGETS:
                env = [value for value in config.get("Env", [])
                       if not value.startswith(("V3_UI_ROOT=", "V3_SHARED_UI_ROOT="))]
                env.extend(("V3_UI_ROOT=/opt/v3/assets/current/v3/ui",
                            "V3_SHARED_UI_ROOT=/opt/v3/assets/current/ui"))
                config["Env"] = env
        if name == TARGETS[1]:
            host = loop_plugin_mounts(host)
        # Container-id-derived values must not be reused. Preserve configured
        # mounts, network, limits, ports, restart policy and security options.
        host.pop("ContainerIDFile", None)
        config["HostConfig"] = host
        config.pop("Hostname", None)
        config.pop("Domainname", None)
        config.pop("MacAddress", None)
        conn = DockerHTTP("localhost", timeout=30)
        try:
            conn.request("POST", "/containers/create?name=" + name, body=canonical(config), headers={"Content-Type": "application/json"})
            response = conn.getresponse()
            body = response.read(1024 * 1024)
            if response.status != 201:
                raise RuntimeError("development container create failed (" + str(response.status) + ")")
            return json.loads(body)["Id"]
        finally:
            conn.close()

    def start(self, name):
        self.command(["docker", "start", name], timeout=30)

    def healthy(self, name, expected):
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if self.cancel.is_set():
                raise PermissionError("host broker stopping; deployment outcome requires reconciliation")
            current = self.inspect(name)
            if current["Image"] != expected:
                return False
            state = current["State"]
            if state.get("Status") == "running" and state.get("Health", {}).get("Status") == "healthy":
                return True
            time.sleep(1)
        return False


class Broker:
    def __init__(self, state=STATE, runner=None, recover=True):
        self.state = Path(state)
        if self.state.is_symlink():
            raise ValueError("symlink job state forbidden")
        self.state.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(self.state, 0o700)
        self.lease = os.open(self.state / "broker.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        try:
            fcntl.flock(self.lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(self.lease)
            raise ValueError("development build broker already running")
        self.runner = runner or Runner()
        self.lock = threading.RLock()
        self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=1)
        self.ui_pool = concurrent.futures.ThreadPoolExecutor(max_workers=1)
        self.deploy_lock = threading.RLock()
        self.active_ui = None
        self.jobs = {}
        self.job_cancellations = {}
        self.queue_index = {}
        self.requests = {}
        self.active = None
        self.stopping = False
        self.maintenance_file=self.state/"coordination/maintenance.json"
        self.workspace_leases={}
        self.code_index_leases={}
        self.work_dir = self.state / "coordination"
        if self.work_dir.is_symlink():
            raise ValueError("symlink coordination state forbidden")
        self.work_dir.mkdir(mode=0o700, exist_ok=True)
        self.work_path = self.work_dir / "items.json"
        if self.work_path.is_symlink():
            raise ValueError("symlink coordination receipt forbidden")
        self.work = json.loads(self.work_path.read_text()) if self.work_path.exists() else {}
        if not isinstance(self.work, dict) or len(self.work) > 1000:
            raise ValueError("invalid coordination registry")
        self.code_index_ledger=maintenance_engine.Ledger(Path(CONFIG["install_root"]).parent/"code-index-maintenance"if self.state==Path(CONFIG["state_root"])else self.state/"code-index-maintenance",inventory=getattr(self.runner,"maintenance_inventory",None))
        self.runner.maintenance_snapshot_folder=self.code_index_ledger.folder/"snapshots"
        self.runner.maintenance_provider=self._code_index_snapshot
        self.runner.maintenance_activated=self._code_index_activated
        self.performance = performance.Performance(self)
        self.work_activity = {key: self._work_content(item) for key, item in self.work.items()}
        for path in sorted(self.state.glob("*.json")):
            if path==self.maintenance_file:continue
            if path.is_symlink():
                raise ValueError("symlink receipt forbidden")
            job = json.loads(path.read_text())
            if not re.fullmatch(r"[0-9a-f-]{36}", job.get("job_id", "")):
                raise ValueError("invalid durable job")
            self.jobs[job["job_id"]] = job
            self.requests[job["request_key"]] = job["job_id"]
        if recover:
            adopted = sorted((job for job in self.jobs.values()
                              if job.get("operator_adopted_at")),
                             key=lambda row: row.get("finished_at", 0))
            if adopted:
                latest = adopted[-1]
                superseded = any(job["status"] == "completed" and job.get("effects_started")
                                 and job.get("finished_at", 0) > latest.get("finished_at", 0)
                                 for job in self.jobs.values())
                if not superseded and not self._runtime_release_matches(latest):
                    latest.pop("operator_adopted_at", None)
                    self._save(latest)
        if recover:
            for job in self.jobs.values():
                if job["status"] in ("queued", "running") and not (
                        job.get("isolated") and job["status"] == "queued"):
                    # A restart never repeats a build or external mutation.
                    job["status"] = ("uncertain" if job.get("effects_started") or job["operation"] == "deploy"
                                     else "cancelled" if job.get("cancel_request") else "interrupted")
                    job["reason"] = "broker restarted; no automatic replay"
                    if job["status"] == "uncertain" and self._reconcile(job):
                        job["status"] = "completed"
                        job["reason"] = "recovered matching healthy deployment"
                    self._save(job)
        if recover:
            for job in self.jobs.values():
                if job.get("batch_members") and job["status"] not in ("queued", "running"):
                    self._mirror_batch(job)
            self._start_next()

    def _save(self, job):
        path = self.state / (job["job_id"] + ".json")
        temporary = self.state / (job["job_id"] + ".tmp")
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(canonical(job))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(self.state, os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)

    @staticmethod
    def _work_content(item):
        return canonical({key: value for key, value in item.items()
            if key not in {"last_activity_at", "leftover_notice"}})

    def _save_work(self):
        for key, item in self.work.items():
            content = self._work_content(item)
            if self.work_activity.get(key) != content:
                item["last_activity_at"] = time.time()
                item.pop("leftover_notice", None)
                self.work_activity[key] = content
        temporary = self.work_dir / "items.tmp"
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(canonical(self.work))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, self.work_path)
        directory = os.open(self.work_dir, os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)


    def _leftover_policy(self):
        """Operator-owned timing policy; never accept model-supplied timing."""
        path = self.work_dir / "leftover-notice-policy.json"
        if not path.exists() and not path.is_symlink():
            return {"stale_after_seconds": 86400}
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 4096:
            raise ValueError("Leftover notice policy unavailable")
        value = json.loads(path.read_text())
        seconds = value.get("stale_after_seconds") if isinstance(value, dict) else None
        if (not isinstance(value, dict) or set(value) != {"stale_after_seconds"} or isinstance(seconds, bool)
                or not isinstance(seconds, int) or not 60 <= seconds <= 30 * 86400):
            raise ValueError("Invalid leftover notice policy")
        return value

    def _leftover_state(self, item, policy, now):
        if item.get("status") == "abandoned":
            return None
        job = self.jobs.get(item.get("job_id"), {})
        if item.get("status") == "open":
            last = item.get("last_activity_at", item.get("created_at", now))
            if now - last < policy["stale_after_seconds"]:
                return None
            return "stale_open", int(last)
        if (item.get("status") == "closed"
                and job.get("status") in {"failed", "interrupted", "uncertain", "stale"}
                and not job.get("operator_adopted_at")):
            return job["status"], int(job.get("finished_at", job.get("created_at", now)))
        return None

    def _reconcile_leftover_state(self, now=None):
        # Canonical broker receipts own notice revisions. No session database,
        # model invocation, source deletion or automatic release is involved.
        now = time.time() if now is None else now
        policy = self._leftover_policy()
        changed = False
        for key, item in self.work.items():
            state = self._leftover_state(item, policy, now)
            old = item.get("leftover_notice")
            if state is None:
                if old:
                    item.pop("leftover_notice", None)
                    changed = True
                continue
            status, since = state
            identity = hashlib.sha256(canonical([key, status, since,
                item.get("job_id"), item.get("last_activity_at", item.get("created_at")),
                policy["stale_after_seconds"]])).hexdigest()
            if old and old.get("id") == identity:
                continue
            task_id = item["task_id"]
            message = (f"Task {task_id} is inactive and retains unfinished work. "
                "Investigate its edits; abandon it only after preserving unique work."
                if status == "stale_open" else
                f"Task {task_id} has a {status} build. Investigate the retained work and "
                "reconcile its outcome before clearing it.")
            item["leftover_notice"] = {"id": identity, "task_id": task_id,
                "job_id": item.get("job_id"), "status": status, "since": since,
                "observed_at": int(now), "notification": message,
                "retained_bytes": None}
            changed = True
        if changed:
            self._save_work()
        return policy

    def _leftover_views(self, trusted=None, workspace=None, administrator=False):
        rows = []
        for item in self.work.values():
            notice = item.get("leftover_notice")
            if not notice or notice.get("reviewed_at") is not None:
                continue
            if not administrator and not self._owned(item, trusted, workspace):
                continue
            row = dict(notice)
            row["summary"] = item.get("summary", "")[:200]
            row["origin"] = ("codex" if item["user_id"] == "codex-local" else
                "v2" if item["user_id"] == "python-v2-admin" else "v3")
            if administrator:
                row["user_id"] = item["user_id"]
                row["session_id"] = item["workspace"] if row["origin"] == "v3" else None
            rows.append(row)
        return sorted(rows, key=lambda row: (row["observed_at"], row["id"]), reverse=True)

    def _review_leftover(self, args, trusted, workspace):
        if set(args) != {"notice_id"} or not isinstance(args["notice_id"], str) or not re.fullmatch(r"[0-9a-f]{64}", args["notice_id"]):
            raise ValueError("Exact leftover notice identity required")
        self._incoming_lease(trusted)
        self._reconcile_leftover_state()
        administrator = trusted["user_id"] == "admin" and workspace == "00000000-0000-0000-0000-000000000000"
        for item in self.work.values():
            notice = item.get("leftover_notice", {})
            if notice.get("id") != args["notice_id"]:
                continue
            if not administrator and not self._owned(item, trusted, workspace):
                raise PermissionError("Leftover notice unavailable")
            if notice.get("reviewed_at") is None:
                notice["reviewed_at"] = int(time.time())
                self._save_work()
            # This acknowledges only this revision; artifacts and jobs are intact.
            return {"notice_id": notice["id"], "status": "reviewed"}
        raise ValueError("Leftover notice changed; refresh before reviewing")

    def prune_check_caches(self, now=None):
        """Expire only rebuildable outputs, never source, logs or job artifacts."""
        now = time.time() if now is None else now
        def atomic_json(path, value):
            temporary = path.with_suffix(".tmp")
            with temporary.open("x") as output:
                json.dump(value, output)
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, path)
        with self.lock:
            if self.stopping or self.active or self.active_ui or any(
                    j.get("status") in {"queued", "running", "needs_decision"}
                    for j in self.jobs.values()):
                return {"status": "deferred", "reason": "broker work active"}
            # External operator checks are outside our admission lock. Fail closed
            # on inventory errors; mounted cache directories are always pinned.
            processes = self.runner.command(["ps", "-eo", "comm="], timeout=5)
            if any(p.strip() in {"cargo", "rustc", "rustdoc", "buildx"}
                   for p in processes.splitlines()):
                return {"status": "deferred", "reason": "compiler active"}
            ids = self.runner.command(["docker", "ps", "-aq", "--filter", "name="+TARGETS[0]], timeout=5).split()
            containers = json.loads(self.runner.command(["docker", "inspect", *ids], timeout=10, private_raw=True)) if ids else []
            mounts = [Path(m["Source"]).resolve() for c in containers
                      for m in c.get("Mounts", []) if m.get("Source") and Path(m["Source"]).resolve() != self.state.parent.resolve()]
            removed = []
            # The host terminal-artifact pruner shares this lock. Its task and
            # release retention remains independent from rebuildable cache expiry.
            with (self.state / ".v3-prune.lock").open("a") as pruning_lock:
                try:
                    fcntl.flock(pruning_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    return {"status": "deferred", "reason": "artifact pruning active"}
                candidates = []
                for base in (self.state / "checks", self.work_dir / "checks"):
                    if base.is_symlink() or not base.is_dir():
                        continue
                    for owner in sorted(base.iterdir()):
                        if owner.is_symlink() or not owner.is_dir():
                            continue
                        item = self.work.get(owner.name) or next((t for t in self.work.values()
                            if t.get("task_id") == owner.name), {})
                        for name in ("target", "cargo"):
                            candidates.append((owner / name, item, "rebuildable_check_cache"))
                checkout_target = self.state.parent / "target"
                if not checkout_target.is_symlink():
                    for name in ("debug", "release", "cargo-home", "session-naming-check/debug"):
                        candidates.append((checkout_target / name, {}, "rebuildable_checkout_cache"))
                for key, item in self.work.items():
                    clearance = item.get("retention_clearance", {})
                    job = self.jobs.get(item.get("job_id"), {})
                    if (not re.fullmatch(r"[a-f0-9]{64}", key) or item.get("status") != "closed"
                            or item.get("job_id") == "4f605f3b-4444-4447-a511-3a9d4b9677df"
                            or clearance.get("unique_work_preserved") is not True
                            or clearance.get("effects_reconciled") is not True
                            or now - clearance.get("at", now) < 7200
                            or clearance.get("job_digest") != hashlib.sha256(canonical(job)).hexdigest()):
                        continue
                    root = self.work_dir / "trees" / key
                    if root.is_symlink():
                        continue
                    if root.is_dir():
                        if context_digest(release.snapshot(root, TASK_CONTEXT, context_digest))[0] != clearance.get("source_digest"):
                            continue
                        candidates.append((root, item, "reconciled_task_source"))
                    elif not clearance.get("source_removed_at"):
                        continue
                    job_id = item.get("job_id", "")
                    if re.fullmatch(r"[0-9a-f-]{36}", job_id):
                        candidates.append((self.state / job_id, item, "reconciled_job_artifacts"))
                for cache, item, kind in candidates:
                    if any(parent.is_symlink() for parent in (cache, *cache.parents)) or not cache.is_dir():
                        continue
                    resolved = cache.resolve()
                    if any(resolved == m or resolved in m.parents or m in resolved.parents for m in mounts):
                        continue
                    newest = max(cache.stat().st_mtime, item.get("last_activity_at", 0))
                    entries = 0
                    for directory, dirs, files in os.walk(cache, followlinks=False):
                        for entry in (*dirs, *files):
                            entries += 1
                            newest = max(newest, (Path(directory) / entry).lstat().st_mtime)
                        if entries > 100000 or now - newest < 7200:
                            break
                    if entries > 100000 or now - newest < 7200:
                        continue
                    # Recheck external activity immediately before deletion.
                    processes = self.runner.command(["ps", "-eo", "comm="], timeout=5)
                    if any(p.strip() in {"cargo", "rustc", "rustdoc", "buildx"} for p in processes.splitlines()):
                        return {"status": "deferred", "reason": "compiler started", "removed": removed}
                    current_ids = self.runner.command(["docker", "ps", "-aq", "--filter", "name="+TARGETS[0]], timeout=5).split()
                    current_containers = json.loads(self.runner.command(["docker", "inspect", *current_ids], timeout=10, private_raw=True)) if current_ids else []
                    current_mounts = [Path(m["Source"]).resolve() for c in current_containers
                        for m in c.get("Mounts", []) if m.get("Source") and Path(m["Source"]).resolve() != self.state.parent.resolve()]
                    if any(resolved == m or resolved in m.parents or m in resolved.parents for m in current_mounts):
                        continue
                    size = int(self.runner.command(["du", "-s", "-B1", "--one-file-system", "--", str(cache)], timeout=10).split()[0])
                    receipt = {"path": str(cache.relative_to(self.state.parent)), "bytes": size,
                               "expired_at": now, "idle_seconds": now - newest,
                               "kind": kind}
                    # Persist intent before effects; receipts remain even
                    # when interruption leaves a partially removed cache.
                    receipts = self.state / "cache-pruning"
                    receipts.mkdir(exist_ok=True)
                    receipt_path = receipts / (uuid.uuid4().hex + ".json")
                    atomic_json(receipt_path, dict(receipt, status="removing"))
                    shutil.rmtree(cache)
                    if kind == "reconciled_task_source":
                        item["retention_clearance"]["source_removed_at"] = now
                        self._save_work()
                    atomic_json(receipt_path, dict(receipt, status="removed"))
                    removed.append(receipt)
                    if len(removed) >= 4:
                        return {"status": "completed", "removed": removed}
            return {"status": "completed", "removed": removed}

    def reconcile_leftovers(self):
        # Disk accounting occurs outside the broker lock with a bounded budget.
        # Delayed counts are accepted only for the same still-unreviewed notice.
        with self.lock:
            self._reconcile_leftover_state()
            pending = [(key, dict(item["leftover_notice"])) for key, item in self.work.items()
                if item.get("leftover_notice", {}).get("retained_bytes") is None
                and item.get("leftover_notice") and item["leftover_notice"].get("reviewed_at") is None]
        for key, notice in pending[:4]:
            if not re.fullmatch(r"[0-9a-f]{64}", key):
                continue
            if notice.get("job_id") and not re.fullmatch(r"[0-9a-f-]{36}", notice["job_id"]):
                continue
            paths = [self.work_dir / "trees" / key, self.state / "checks" / key]
            if notice.get("job_id"):
                paths.append(self.state / notice["job_id"])
            paths = [path for path in paths if path.is_dir() and not path.is_symlink()]
            try:
                output = self.runner.command(["du", "-s", "-B1", "--one-file-system", "--",
                    *(str(path) for path in paths)], timeout=2) if paths else ""
                size = sum(int(line.split()[0]) for line in output.splitlines())
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
                continue
            with self.lock:
                current = self.work.get(key, {}).get("leftover_notice", {})
                if current.get("id") == notice["id"] and current.get("reviewed_at") is None:
                    current["retained_bytes"] = size
                    self._save_work()

    def _open_work(self):
        return [item for item in self.work.values() if item["status"] == "open"]

    def _auto_schedule(self, item, trusted, workspace):
        if item.get("job_id") or self._open_work() or (self.active is not None or self.active_ui is not None):
            return item.get("job_id")
        pending = sorted(key for key, value in self.work.items() if value["status"] == "closed" and not value.get("job_id"))
        request_id = "auto-" + hashlib.sha256(canonical(pending)).hexdigest()
        job = self.handle({"name": "administrator_auto_build_deploy", "arguments": {
            "request_id": request_id, "expected_inputs_sha256": item["inputs_sha256"]},
            "workspace": workspace, "trusted": trusted})
        for key in pending:
            self.work[key]["job_id"] = job["job_id"]
        self._save_work()
        return job["job_id"]

    def _current_release_blob(self):
        completed = sorted((job for job in self.jobs.values()
                            if job.get("isolated") and (job["status"] == "completed"
                            or job.get("operator_adopted_at"))),
                           key=lambda job: (job.get("finished_at", job["created_at"]), job["job_id"]))
        if completed:
            job = completed[-1]
            blob = (self.state / job["job_id"] / "candidate.tar").read_bytes()
            if context_digest(blob)[0] != job["inputs_sha256"]:
                raise ValueError("durable release input mismatch")
            return blob
        return self.runner.snapshot()


    def _sync_report(self, live, candidate, changed, offset=0):
        return release.checkout_drift(live, self.runner.snapshot(), candidate,
                                      changed, context_digest, offset)

    def _sync_job(self, job, live, candidate, changed):
        # Recheck at admission and before the first external effect.
        report = self._sync_report(live, candidate, changed)
        job["synchronization"] = report
        if report["total"] and job.get("sync_ack_digest") != report["digest"]:
            job.update(status="needs_decision", phase="checking_checkout",
                       reason="Checkout differs from the deployed release; choose synchronize, proceed or cancel")
            self._save(job)
            return False
        return True

    def _can_start_parallel_ui(self):
        if self.active_ui is not None or self.active is None or self.stopping:
            return False
        active = self.jobs[self.active]
        return (active.get("isolated") and active["operation"] == "build_deploy" and
                active["status"] in ("queued", "running") and
                not active.get("effects_started") and
                active.get("phase") in ("admitted", "building_api", "building_loop") and
                self.runner.ui_hot_swappable())

    def _start_parallel_ui(self, job):
        if not self._maintenance_allowed({"local_caller":job.get("user_id")=="codex-local"},job):return
        try:
            self._lease(job)
            folder = self.state / job["job_id"]
            base = (folder / "base.tar").read_bytes()
            task = (folder / "task.tar").read_bytes()
            live = self._current_release_blob()
            candidate, changed = release.candidate(base, task, live, context_digest, allow_empty=True)
            if not all(hot_source_path(path) for path in changed):
                raise ValueError("parallel UI release contains non-UI source")
            if not self._sync_job(job, live, candidate, changed):
                return
            digest, _ = context_digest(candidate)
            release_root = folder / "release"
            (folder / "candidate.tar").write_bytes(candidate)
            release.extract(release_root, candidate, context_digest)
            job.update(inputs_sha256=digest, base_inputs_sha256=context_digest(live)[0],
                       changed_paths=changed, release_root=str(release_root),
                       parallel_ui=True, phase="admitted")
            self._save(job)
            self.active_ui = job["job_id"]
            self.ui_pool.submit(self._run, job, candidate)
        except Exception as error:
            job.update(status="failed", phase="admission_failed",
                       reason=scrub(str(error), self.runner.secrets),
                       finished_at=time.time())
            self._save(job)

    def _maybe_start_parallel_ui(self):
        if not self._can_start_parallel_ui():
            return
        for job in sorted(self.jobs.values(), key=lambda value: value["created_at"]):
            if job.get("isolated") and job["status"] == "queued" and job["operation"] in HOT_OPERATIONS:
                self._start_parallel_ui(job)
                return

    def _has_unresolved_release(self, current=None):
        completed = [job.get("finished_at", 0) for job in self.jobs.values()
                     if job.get("isolated") and (job["status"] == "completed"
                     or job.get("operator_adopted_at"))
                     and job["operation"] in ("deploy", "build_deploy", "ui_deploy", "index_deploy", "host_service_deploy")
                     and job.get("effects_started")]
        latest = max(completed, default=0)
        return any(job["status"] == "uncertain"
                   and not job.get("operator_adopted_at")
                   and job.get("effects_started")
                   and job.get("finished_at", job["created_at"]) >= latest
                   and job["job_id"] != current
                   for job in self.jobs.values())

    def _maintenance(self):
        if self.maintenance_file.is_symlink():raise PermissionError("Maintenance state replaced")
        try:return json.loads(self.maintenance_file.read_text())
        except FileNotFoundError:return {"drained":False,"task_id":None}
    def _maintenance_allowed(self,trusted,args):
        state=self._maintenance()
        return not state["drained"] or (trusted.get("local_caller") is True and args.get("task_id")==state.get("task_id") and state.get("task_id") is not None)
    def _start_next(self):
        if (self.active is not None or self.active_ui is not None or self.stopping or
                self._has_unresolved_release()):
            return
        # UI releases pass queued image builds, but never an active release.
        pending = sorted(self.jobs.values(), key=lambda value: (
            value.get("operation") not in HOT_OPERATIONS, value["created_at"]))
        for job in pending:
            if not job.get("isolated") or job["status"] != "queued":
                continue
            try:
                task_id=job.get("task_id") or next((v["task_id"] for v in self.work.values() if v.get("job_id")==job["job_id"]),None)
                if not self._maintenance_allowed({"local_caller":job.get("user_id")=="codex-local"},{"task_id":task_id}):continue
                self._lease(job)
                folder = self.state / job["job_id"]
                base = (folder / "base.tar").read_bytes()
                task = (folder / "task.tar").read_bytes()
                live = self._current_release_blob()
                resuming = job.pop("sync_resume", False) and (folder / "admitted_base.tar").is_file()
                if resuming:
                    base = (folder / "admitted_base.tar").read_bytes()
                    task = (folder / "candidate.tar").read_bytes()
                candidate, changed = release.candidate(base, task, live, context_digest, allow_empty=True)
                if not self._sync_job(job, live, candidate, changed):
                    continue
                members = [self.jobs[jid] for jid in job.get("batch_members", ())] if resuming else []
                if not resuming and job["operation"] == "build_deploy" and not job["synchronization"]["total"]:
                    # Merge compatible queued work from the same caller before
                    # compiling. Each task retains its own durable job receipt.
                    for follower in pending:
                        if (follower["job_id"] == job["job_id"] or
                                follower["status"] != "queued" or
                                follower.get("operation") != "build_deploy"):
                            continue
                        if (follower.get("user_id") != job["user_id"] or
                                follower.get("workspace") != job["workspace"] or
                                follower.get("access_fingerprint") != job.get("access_fingerprint")):
                            break
                        if len(members) >= 7:
                            break
                        try:
                            self._lease(follower)
                            peer = self.state / follower["job_id"]
                            peer_base = (peer / "base.tar").read_bytes()
                            peer_task = (peer / "task.tar").read_bytes()
                            peer_candidate, peer_changed = release.candidate(peer_base, peer_task, live, context_digest)
                            if not self._sync_job(follower, live, peer_candidate, peer_changed):
                                continue
                            candidate, peer_changed = release.candidate(
                                peer_base, peer_task, candidate, context_digest)
                            changed = sorted(set(changed) | set(peer_changed))
                            members.append(follower)
                        except Exception as error:
                            follower.update(status="failed", phase="admission_failed",
                                reason=scrub(str(error), self.runner.secrets),
                                finished_at=time.time())
                            self._save(follower)
                digest, _ = context_digest(candidate)
                release_root = folder / "release"
                (folder / "candidate.tar").write_bytes(candidate)
                (folder / "admitted_base.tar").write_bytes(live)
                if release_root.exists() and context_digest(release.snapshot(release_root, CONTEXT, context_digest))[0] != digest and resuming:
                    release_root = folder / ("release-" + digest)
                if release_root.exists():
                    if context_digest(release.snapshot(release_root, CONTEXT, context_digest))[0] != digest:
                        raise ValueError("prepared release changed across broker restart")
                else:
                    release.extract(release_root, candidate, context_digest)
                job.update(inputs_sha256=digest, base_inputs_sha256=context_digest(live)[0],
                           changed_paths=changed, release_root=str(release_root),
                           batch_members=[member["job_id"] for member in members],
                           phase="admitted")
                for member in members:
                    peer = self.state / member["job_id"]
                    (peer / "candidate.tar").write_bytes(candidate)
                    member.update(inputs_sha256=digest, base_inputs_sha256=job["base_inputs_sha256"],
                                  batch_parent=job["job_id"], status="running",
                                  phase="batched_waiting")
                    self._save(member)
                self._save(job)
                self.active = job["job_id"]
                self.pool.submit(self._run, job, candidate)
                return
            except Exception as error:
                job.update(status="failed", phase="admission_failed",
                           reason=scrub(str(error), self.runner.secrets),
                           finished_at=time.time())
                self._save(job)

    def _cancel_job(self, args, trusted, workspace):
        if set(args) != {"job_id", "request_id", "expected_job_ids"} or not isinstance(args["job_id"], str) or not re.fullmatch(r"[0-9a-f-]{36}", args["job_id"]):
            raise ValueError("cancel requires a valid job_id and stable request_id")
        if not isinstance(args["request_id"], str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", args["request_id"]):
            raise ValueError("stable cancellation request_id required")
        self._incoming_lease(trusted)
        job = self.jobs.get(args["job_id"])
        dashboard = trusted["user_id"] == "admin" and workspace == "00000000-0000-0000-0000-000000000000"
        if job is None:
            return {"job": None}
        if not dashboard and not self._owned(job, trusted, workspace):
            raise PermissionError("build cancellation denied")
        parent = self.jobs.get(job.get("batch_parent"), job)
        affected = [parent["job_id"], *parent.get("batch_members", [])]
        expected = args["expected_job_ids"]
        if not isinstance(expected, list) or not 1 <= len(expected) <= 8 or any(not isinstance(value, str) for value in expected):
            raise ValueError("bounded expected job IDs required")
        if parent["status"] in ("queued", "running", "needs_decision") and not parent.get("cancel_request") and sorted(expected) != sorted(affected):
            raise ValueError("Build batch changed; refresh affected jobs before requesting stop")
        # A merged candidate is one tested unit: cancellation stops all members.
        if not parent.get("cancel_request") and parent["status"] in ("queued", "running", "needs_decision"):
            parent["cancel_request"] = {"request_id": args["request_id"],
                "requested_by": trusted["user_id"], "requested_at": time.time(),
                "affected_job_ids": affected}
            parent["revoked"] = True
            active = parent["job_id"] in (self.active, self.active_ui)
            if not active:
                parent.update(status="cancelled", reason="queued job cancelled before execution",
                              finished_at=time.time())
            self._save(parent)  # Persist intent before signalling execution.
            for member_id in parent.get("batch_members", []):
                member = self.jobs[member_id]
                member["cancel_request"] = parent["cancel_request"]
                member["revoked"] = True
                self._save(member)
            if active:
                self.job_cancellations.setdefault(parent["job_id"], threading.Event()).set()
            else:
                self._mirror_batch(parent)
                self._start_next()
        view = self._view(job)
        view.pop("access_fingerprint", None)
        return {"job": view, "affected_job_ids": affected,
                "disposition": "stop_requested" if parent["status"] in ("queued", "running")
                               else parent["status"]}

    def _queue_changes(self, job):
        folder = self.state / job["job_id"]
        paths = [folder / name for name in ("base.tar", "task.tar")]
        stamp = tuple((p.stat().st_size, p.stat().st_mtime_ns) for p in paths)
        cached = self.queue_index.get(job["job_id"])
        if cached and cached[0] == stamp:
            return cached[1]
        # Retain only fingerprints, never source bodies, in the read cache.
        base, task = [release.entries(p.read_bytes(), context_digest) for p in paths]
        changed = {name: ((base[name][0], hashlib.sha256(base[name][1]).hexdigest()) if name in base else None,
                          (task[name][0], hashlib.sha256(task[name][1]).hexdigest()) if name in task else None)
                   for name in sorted(set(base) | set(task)) if base.get(name) != task.get(name)}
        if len(self.queue_index) >= 64:
            self.queue_index.pop(next(iter(self.queue_index)))
        self.queue_index[job["job_id"]] = (stamp, changed)
        return changed

    def _queue_preflight(self, job, administrator=False):
        pending = ("queued", "running", "needs_decision")
        if not job.get("isolated") or job["status"] not in pending:
            return None
        peers = sorted((peer for peer in self.jobs.values()
                        if peer["job_id"] != job["job_id"] and peer.get("isolated")
                        and peer["status"] in pending and not peer.get("batch_parent")
                        and (administrator or (peer["user_id"], peer["workspace"]) ==
                             (job["user_id"], job["workspace"]))),
                       key=lambda peer: (peer.get("operation") not in HOT_OPERATIONS, peer["created_at"], peer["job_id"]))
        report = {"checked_at": time.time(), "peers": [], "peer_count": len(peers),
                  "truncated": False, "advisory": True}
        try:
            own = self._queue_changes(job)
        except (OSError, ValueError, KeyError, tarfile.TarError):
            return report | {"status": "unavailable"}
        conflicts = 0
        for peer in peers[:16]:
            item = {"job_id": peer["job_id"]}
            try:
                other = self._queue_changes(peer)
                overlap = sorted(set(own) & set(other))
                first, second = (own, other) if (
                    job.get("operation") not in HOT_OPERATIONS, job["created_at"], job["job_id"]) <= (
                    peer.get("operation") not in HOT_OPERATIONS, peer["created_at"], peer["job_id"]) else (other, own)
                bad = [name for name in overlap if first[name][1] not in second[name]]
                conflicts += bool(bad)
                same_caller = (job["user_id"], job["workspace"], job.get("access_fingerprint")) == (
                    peer["user_id"], peer["workspace"], peer.get("access_fingerprint"))
                reason = ("Conflicting edits" if bad else
                          "Different caller or authorization" if not same_caller else
                          "Build already started" if job["status"] == "running" or peer["status"] == "running" else
                          "Source decision pending" if "needs_decision" in (job["status"], peer["status"]) else
                          "UI releases run separately" if job["operation"] != "build_deploy" or peer["operation"] != "build_deploy" else
                          "Compatible changes; may share a build")
                item.update(reason=reason, compatible=not bad,
                            shared_build_eligible=reason == "Compatible changes; may share a build",
                            overlap_count=len(overlap), conflict_count=len(bad),
                            paths=(bad or overlap)[:3], paths_truncated=len(bad or overlap) > 3)
            except (OSError, ValueError, KeyError, tarfile.TarError):
                item.update(reason="Queued source unavailable", compatible=None, shared_build_eligible=False)
            # Dashboard/tool responses remain bounded even with a large queue.
            if len(canonical(report | {"peers": report["peers"] + [item]})) > 1472:
                report["truncated"] = True
                break
            report["peers"].append(item)
        report["truncated"] |= len(report["peers"]) < len(peers)
        report["status"] = "conflicts" if conflicts else "checked"
        return report

    def _view(self, job, administrator=False):
        raw = scrub(job.get("log", ""), self.runner.secrets).encode("utf-8")
        tail = raw[-PUBLIC_LOG:].decode("utf-8", "replace")
        # Replacement characters from a split UTF-8 sequence must not expand
        # the public byte limit. Storage can retain a larger bounded log.
        while len(tail.encode("utf-8")) > PUBLIC_LOG:
            tail = tail[1:]
        view = {key: job[key] for key in ("job_id", "operation", "status", "phase", "inputs_sha256", "created_at", "images", "rollback", "tool_name", "access_fingerprint", "isolated", "batch_parent", "batch_members", "synchronization", "sync_ack_digest", "maintenance", "consumer_adoption", "consumer_before", "plugin_artifacts") if key in job}
        if "reason" in job:
            # Compiler failures are at the end of a bounded build transcript.
            view["reason"] = scrub(job["reason"], self.runner.secrets).encode("utf-8")[-4096:].decode("utf-8", "ignore")
        preflight = self._queue_preflight(job, administrator)
        if preflight is not None:
            view["queue_preflight"] = preflight
        parent = self.jobs.get(job.get("batch_parent"), job)
        pending = parent["status"] in ("queued", "running", "needs_decision")
        view["cancellation"] = {"available": pending and not parent.get("cancel_request"),
            "mode": "deployment_stop" if parent.get("effects_started") else
                    "queued" if parent["status"] in ("queued", "needs_decision") and parent["job_id"] not in (self.active, self.active_ui) else "build_stop",
            "requested": bool(parent.get("cancel_request")),
            "affected_job_ids": [parent["job_id"], *parent.get("batch_members", [])]}
        if parent.get("cancel_request"):
            view["cancellation"]["requested_at"] = parent["cancel_request"]["requested_at"]
        return view | {"log": tail, "log_truncated": len(raw) > PUBLIC_LOG, "log_bytes": len(raw)}

    def _rust_test_result(self, job):
        if job.get("rust_tests"):
            return job["rust_tests"]
        if job["operation"] in HOT_OPERATIONS:
            return {"status": "not_required",
                    "summary": "Compatible assets/plugin; no core Rust build required"}
        if job["status"] == "completed" and job.get("images", {}).get("api"):
            return {"status": "passed",
                    "summary": "Confirmed by successful tested API image; detailed test output was not retained"}
        if job["status"] in ("queued", "running"):
            return {"status": "not_started"}
        return {"status": "unavailable",
                "summary": "Structured test result was not recorded; inspect the failure reason"}

    def _lease(self, job):
        if job.get("revoked") or job.get("lease_until", 0) <= time.time():
            raise PermissionError("host job authorization expired or revoked; no automatic replay")

    def _batch_lease(self, job):
        self._lease(job)
        for member_id in job.get("batch_members", []):
            self._lease(self.jobs[member_id])

    def _mirror_batch(self, job):
        for member_id in job.get("batch_members", []):
            member = self.jobs[member_id]
            for key in ("status", "phase", "reason", "started_at", "finished_at",
                        "images", "rollback", "rust_tests", "log", "phase_history",
                        "effects_started"):
                if key in job:
                    member[key] = job[key]
            member["synchronization"] = job.get("synchronization")
            self._save(member)

    def _incoming_lease(self, trusted):
        deadline, fingerprint = trusted.get("lease_until"), trusted.get("access_fingerprint")
        max_lease = 4 * 3600 if trusted.get("local_caller") is True or trusted.get("bridge_caller") is True else 35
        if isinstance(deadline, bool) or not isinstance(deadline, (int, float)) or not time.time() < deadline <= time.time() + max_lease:
            raise ValueError("current bounded host authorization lease required")
        if not isinstance(fingerprint, str) or not re.fullmatch(r"[0-9a-f]{64}", fingerprint):
            raise ValueError("host access fingerprint required")
        return deadline, fingerprint

    def _owned(self, job, trusted, workspace):
        return job["user_id"] == trusted["user_id"] and job["workspace"] == workspace

    def _reconcile(self, job):
        try:
            images = job.get("images", {})
            if set(images) != {"api", "loop"}:
                return False
            if job.get("isolated"):
                candidate = (self.state / job["job_id"] / "candidate.tar").read_bytes()
                if context_digest(candidate)[0] != job["inputs_sha256"]:
                    return False
                if not self.runner.checkout_matches(candidate, job.get("changed_paths", ())):
                    return False
            if "asset_previous" in job:
                if self.runner.current_asset_id() != job["job_id"]:
                    return False
                self.runner.verify_ui(job.get("changed_paths", ()))
            for role, name in zip(("api", "loop"), TARGETS):
                if not self.runner.healthy(name, images[role]):
                    return False
            if job["operation"] in ("ui_deploy","index_deploy","plugin_deploy","host_service_deploy") and job.get("hot_assets"):
                return True
            rollback = job.get("rollback", {})
            if set(rollback) != {"api", "loop"}:
                return False
            archive = self.state / job["job_id"] / "rollback-images.tar"
            if not archive.is_file() or not archive.stat().st_size:
                return False
            for role, name in zip(("api", "loop"), TARGETS):
                if self.runner.inspect(rollback[role])["State"]["Status"] != "exited":
                    return False
            return True
        except Exception:
            return False

    def _reconcile_plugin_publication(self, job, trusted, workspace):
        # Operator continuation after an uncertain adoption check. Never activate
        # assets or execute/build a package again; verify the already selected release.
        if (job.get("operation")!="plugin_deploy" or job.get("status")!="uncertain"
                or job.get("phase")!="checking_ui" or not job.get("hot_assets")
                or not job.get("effects_started")):
            raise ValueError("Only an interrupted plugin adoption check can continue")
        deadline,fingerprint=self._incoming_lease(trusted)
        if (not trusted.get("local_caller") or not self._owned(job,trusted,workspace)
                or job.get("revoked") or job.get("batch_members")
                or job.get("access_fingerprint")!=fingerprint):
            raise PermissionError("Fresh matching operator authority required for reconciliation")
        if self.runner.current_asset_id()!=job["job_id"]:
            raise ValueError("Active publication changed; reconciliation refused")
        candidate=(self.state/job["job_id"]/"candidate.tar").read_bytes()
        if context_digest(candidate)[0]!=job["inputs_sha256"]:
            raise ValueError("Candidate changed; reconciliation refused")
        for role,name in zip(("api","loop"),TARGETS):
            if not self.runner.healthy(name,job["images"][role]):
                raise ValueError("Retained consumer changed or unhealthy")
            identity=job.get("consumer_before",{}).get(role,{}).get("id")
            if identity is not None and self.runner.inspect(name)["Id"]!=identity:
                raise ValueError("Retained consumer identity changed")
        self.runner.verify_ui(job.get("changed_paths",()))
        job["consumer_adoption"]=self.runner.verify_plugin_adoption(job["job_id"])
        # This explicit operator request authorizes only the remaining checked
        # source promotion. Ordinary expired leases still cannot be renewed.
        job["lease_until"]=min(deadline,time.time()+180)
        job["operator_reconciliation_authorized_at"]=time.time()
        self._save(job)
        self._phase(job,"promoting_checkout")
        self.runner.promote_checkout(self._current_release_blob(),candidate,job.get("changed_paths",()))
        job.update(status="completed",phase="finished",finished_at=time.time(),
            operator_reconciled_at=time.time(),
            reason="Existing plugin publication verified and source promoted; no effects replayed",
            synchronization=self._sync_report(candidate,candidate,()))
        self._save(job)
        self._start_next()
        return {"job_id":job["job_id"],"status":"completed","effects_replayed":False}

    def _runtime_release_matches(self, job):
        # An operator may accept a healthy deployed image while checkout
        # promotion remains conflicted; never replay its external effects.
        try:
            if (not job.get("isolated") or job["status"] != "uncertain" or
                    job.get("phase") != "promoting_checkout" or not job.get("effects_started")):
                return False
            candidate = (self.state / job["job_id"] / "candidate.tar").read_bytes()
            if context_digest(candidate)[0] != job["inputs_sha256"]:
                return False
            # Hot UI switches keep the containers; accept only the exact active
            # asset snapshot with both original images healthy and bytes served.
            if job.get("hot_assets"):
                if job["operation"] not in ("ui_deploy","index_deploy","plugin_deploy","host_service_deploy") or self.runner.current_asset_id() != job["job_id"]:
                    return False
                for role, name in zip(("api", "loop"), TARGETS):
                    if not self.runner.healthy(name, job["images"][role]):
                        return False
                self.runner.verify_ui(job.get("changed_paths", ()))
                return True
            archive = self.state / job["job_id"] / "rollback-images.tar"
            if not archive.is_file() or not archive.stat().st_size:
                return False
            for role, name in zip(("api", "loop"), TARGETS):
                if (not self.runner.healthy(name, job["images"][role]) or
                        self.runner.inspect(job["rollback"][role])["State"]["Status"] != "exited"):
                    return False
            if "asset_previous" in job and self.runner.current_asset_id() != job["job_id"]:
                return False
            return True
        except Exception:
            return False


    def unified_files(self, request, trusted, workspace, args):
        # The same caller/session namespace and checked patch gate own task edits.
        name = request["name"]
        task_id = args.get("task_id")
        if not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task_id):
            raise ValueError("task_id must be 8-80 letters, digits, underscores or hyphens")
        allowed = {
            "file_list": {"task_id", "path", "offset", "limit"},
            "file_read": {"task_id", "path", "offset", "limit"},
            "file_search": {"task_id", "path", "query", "regex", "file_glob", "comments_only", "limit"},
            "file_edit": {"task_id", "path", "old_text", "new_text", "expected_sha256", "expected_inputs_sha256", "request_id"},
            "file_write": {"task_id", "path", "content", "expected_inputs_sha256", "request_id"},
        }
        if set(args) - allowed[name]:
            raise ValueError("unsupported task file argument")
        path = args.get("path", "")
        if path == ".": path = ""
        if not isinstance(path, str) or len(path) > 240 or (path and (
                not re.fullmatch(r"[A-Za-z0-9_./-]+", path)
                or any(part in ("", ".", "..") for part in path.split("/")))):
            raise ValueError("task path must be source-relative without parent traversal")
        mutating = name in ("file_edit", "file_write")
        if mutating and (not path or path.split("/")[0] in
                {"data", "workspaces", ".git", "target", "deployment-secrets"}):
            raise ValueError("task write path denied; inspection data is read-only")
        if mutating and (not isinstance(args.get("request_id"), str)
                or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", args["request_id"])):
            raise ValueError("task writes require a stable 8-80 character request_id")
        def call(tool, arguments):
            return self.handle({**request, "name": tool, "arguments": arguments})
        key = hashlib.sha256(canonical([trusted["user_id"], workspace, task_id])).hexdigest()
        item = self.work.get(key)
        created = item is None
        if created:
            call("v3_build_work", {"action": "begin", "task_id": task_id,
                "summary": "Isolated source work: " + task_id, "mode": "isolated"})
            item = self.work[key]
        if item.get("mode") != "isolated" or item["status"] != "open":
            raise ValueError("task is no longer open; use a new task_id")
        forwarded = {"task_id": task_id, "path": path}
        if name in ("file_list", "file_read"):
            forwarded.update({k: args[k] for k in ("offset", "limit") if k in args})
            result = call("v3_source_list" if name == "file_list" else "v3_source_read", forwarded)
            if name == "file_read":
                source = self.work_dir / "trees" / key / "v3" / path
                with source.open("rb") as stream:
                    result["sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
            result["task_created"] = created
            return result
        if name == "file_search":
            if path or args.get("regex") or args.get("file_glob") or args.get("comments_only"):
                raise ValueError("task search supports a literal query across source; omit path and filters")
            result = call("v3_source_search", {"task_id": task_id, "query": args.get("query"),
                "limit": args.get("limit", 30)})
            result["workspace_inputs_sha256"] = context_digest(release.snapshot(
                self.work_dir / "trees" / key, TASK_CONTEXT, context_digest))[0]
            result["task_created"] = created
            return result
        fingerprint = hashlib.sha256(canonical(args)).hexdigest()
        receipt = item.get("applies", {}).get(args["request_id"])
        if receipt:
            if receipt.get("file_request_sha256") != fingerprint:
                raise ValueError("request_id reused with changed file operation")
            return dict(receipt)
        tree = self.work_dir / "trees" / key
        source = tree / "v3" / path
        cursor = tree / "v3"
        for part in path.split("/"):
            cursor = cursor / part
            if cursor.is_symlink(): raise ValueError("task file path contains symlink")
        if source.exists() and (not source.is_file() or source.stat().st_nlink != 1):
            raise ValueError("task file must be a single-link regular file")
        exists = source.exists()
        original = source.read_bytes() if exists else b""
        if len(original) > 1024 * 1024 or b"\0" in original:
            raise ValueError("task edit exceeds text bound")
        before = original.decode("utf-8")
        current = context_digest(release.snapshot(tree, TASK_CONTEXT, context_digest))[0]
        expected = args.get("expected_inputs_sha256")
        if expected is not None and expected != current:
            raise ValueError("stale task inputs; reread the task file")
        if not created and expected is None:
            raise ValueError("existing task writes require expected_inputs_sha256 from a task read or review")
        if name == "file_edit":
            old, new = args.get("old_text"), args.get("new_text")
            if not exists or not isinstance(old, str) or not old or not isinstance(new, str):
                raise ValueError("file_edit requires an existing file and nonempty old_text plus new_text")
            if before.count(old) != 1: raise ValueError("old_text must match exactly once")
            if args.get("expected_sha256") is not None and args["expected_sha256"] != hashlib.sha256(original).hexdigest():
                raise ValueError("file changed since inspection")
            after = before.replace(old, new, 1)
        else:
            after = args.get("content")
            if not isinstance(after, str): raise ValueError("file_write requires content")
        if len(after.encode()) > 1024 * 1024 or "\0" in after:
            raise ValueError("task write exceeds text bound")
        if before == after and exists:
            receipt = {"task_id": task_id, "request_id": args["request_id"],
                "file_request_sha256": fingerprint, "status": "success", "unchanged": True,
                "workspace_inputs_sha256": current, "paths": [path]}
            item.setdefault("applies", {})[args["request_id"]] = receipt
            self._save_work()
            return dict(receipt)
        patch = "diff --git a/" + path + " b/" + path + "\n"
        if not exists: patch += "new file mode 100644\n"
        for line in difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                fromfile="a/" + path if exists else "/dev/null", tofile="b/" + path):
            patch += line if line.endswith("\n") else line + "\n\\ No newline at end of file\n"
        if not exists and not after:
            patch += "--- /dev/null\n+++ b/" + path + "\n"
        result = call("v3_source_apply", {"task_id": task_id, "request_id": args["request_id"],
            "patch": patch, "expected_inputs_sha256": current})
        item["applies"][args["request_id"]]["file_request_sha256"] = fingerprint
        result["file_request_sha256"] = fingerprint
        result["workspace_inputs_sha256"] = context_digest(release.snapshot(tree, TASK_CONTEXT, context_digest))[0]
        result["diff"] = patch[:16000]
        result["diff_truncated"] = len(patch) > 16000
        result["task_created"] = created
        item["applies"][args["request_id"]].update(result)
        self._save_work()
        return result

    def unified_build(self, request, args):
        action = args.get("action")
        allowed = {"action", "task_id", "summary", "expected_inputs_sha256", "sync_decision",
            "expected_sync_digest", "offset", "job_id", "request_id", "check", "timeout_seconds"}
        if set(args) - allowed: raise ValueError("unsupported build_work argument")
        def call(tool, arguments):
            return self.handle({**request, "name": tool, "arguments": arguments})
        if action == "status" and args.get("job_id"):
            return call("v3_worker_job", {"job_id": args["job_id"]})
        if action == "check":
            return call("v3_cargo_task", {"task_id": args.get("task_id"),
                "task": args.get("check", "check"), "expected_inputs_sha256": args.get("expected_inputs_sha256"),
                "timeout_seconds": args.get("timeout_seconds", 600)})
        if action == "receipt":
            return call("v3_source_receipt", {"task_id": args.get("task_id"), "request_id": args.get("request_id")})
        if action == "review":
            status = call("v3_build_work", {"action": "status", "task_id": args.get("task_id")})
            if not status["own"] or status["own"][0]["status"] != "open":
                raise ValueError("open task required for review")
            item = status["own"][0]
            folder = Path(item["workspace_path"]).parent
            baseline = release.entries((folder / "baseline.tar").read_bytes(), context_digest)
            current = release.entries(release.snapshot(folder, TASK_CONTEXT, context_digest), context_digest)
            paths = [path for path in sorted(set(baseline) | set(current))
                if baseline.get(path) != current.get(path) and not release.inspection_path(path)]
            patches = []
            for path in paths:
                old = baseline.get(path, (0, b""))[1].decode("utf-8", "replace")
                new = current.get(path, (0, b""))[1].decode("utf-8", "replace")
                patches.append("".join(difflib.unified_diff(old.splitlines(True), new.splitlines(True),
                    fromfile="a/" + path, tofile="b/" + path)))
                if sum(map(len, patches)) > 16000: break
            diff = "".join(patches)
            return {**item, "paths": paths[:64], "paths_truncated": len(paths) > 64,
                "diff": diff[:16000], "diff_truncated": len(diff) > 16000}
        forwarded = {k: v for k, v in args.items() if k not in
            {"job_id", "request_id", "check", "timeout_seconds"}}
        if action == "begin": forwarded["mode"] = "isolated"
        return call("v3_build_work", forwarded)

    performance_canonical = staticmethod(canonical)
    performance_scrub = staticmethod(scrub)

    def performance_snapshot(self, root):
        blob = release.snapshot(root, TASK_CONTEXT, context_digest)
        return blob, context_digest(blob)[0]

    def performance_extract(self, blob, destination):
        # Performance children receive application build inputs, never task
        # inspection database backups or the writable task tree.
        destination.mkdir(mode=0o700)
        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:") as archive:
            for member in archive:
                name = member.name.removeprefix("./")
                if not any(name == p or name.startswith(p + "/") for p in CONTEXT):
                    continue
                if member.isdir():
                    (destination / name).mkdir(parents=True, exist_ok=True, mode=0o700)
                elif member.isfile():
                    target = destination / name
                    target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                    with target.open("xb") as output:
                        shutil.copyfileobj(archive.extractfile(member), output)
                    target.chmod(member.mode & 0o755)
                else:
                    raise ValueError("performance snapshot special file forbidden")

    def host_source_only(self,changed,blob,trusted):
        # This operator path can publish only already-installed host code and its
        # own docs/tests. Worker modules, policies and application code always build.
        allowed={"v3/build-service/host.py","v3/build-service/README.md","v3/tests/test_build_service.py"}
        if not trusted.get("local_caller") or not changed or not set(changed)<=allowed:return False
        path=Path(CONFIG["service_dir"])/"host.py"
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC)
        try:
            meta=os.fstat(fd)
            if not stat.S_ISREG(meta.st_mode) or meta.st_uid!=0 or meta.st_mode&0o022 or meta.st_nlink!=1:raise PermissionError("Protected installed host code required")
            with os.fdopen(fd,"rb",closefd=False) as stream:installed=stream.read(2*1024*1024+1)
            candidate=release.entries(blob,context_digest).get("v3/build-service/host.py")
            return candidate is not None and candidate[1]==installed
        finally:os.close(fd)

    def workspace_request(self,name,args,trusted,workspace):
        deadline,fingerprint=self._incoming_lease(trusted)
        if not isinstance(args,dict) or args.get("path","").startswith("/"):raise ValueError("Workspace-relative arguments required")
        if name not in {"v3_workspace_status","source_review"} and args.get("action") not in {"review","read"} and not self._maintenance_allowed(trusted,args):raise PermissionError("Build service drained")
        self.runner.ensure_workspace(workspace)
        base=Path(CONFIG["workspace_root"])/workspace
        if base.is_symlink() or not base.is_dir() or base.resolve().parent!=Path(CONFIG["workspace_root"]).resolve():raise ValueError("Owned session workspace unavailable")
        if base.stat().st_uid!=CONFIG["worker_uid"]:raise PermissionError("Workspace identity changed")
        with self.lock:
            if not (ASSET_ROOT/"current").exists():
                if self.active or self.active_ui:raise ValueError("Initial source publication waits for idle builds")
                blob=self.runner.snapshot()
                initial=str(uuid.uuid5(uuid.NAMESPACE_URL,CONFIG["instance"]+":"+context_digest(blob)[0]))
                self.runner.stage_assets(blob,initial);self.runner.activate_assets(initial)
            selected=(ASSET_ROOT/"current").resolve(strict=True)
        if selected.parent!=ASSET_ROOT/"releases":raise ValueError("Published source outside release namespace")
        job={"user_id":trusted["user_id"],"workspace":workspace,"lease_until":deadline,"access_fingerprint":fingerprint}
        key=uuid.uuid4().hex
        with self.lock:self.workspace_leases[key]=job
        try:
            image=self.runner.inspect(TARGETS[0])["Image"]
            result=self.runner.workspace_task(base,selected,image,name,args,job)
            self._lease(job)
            return result
        finally:
            with self.lock:self.workspace_leases.pop(key,None)

    def _maintenance_job_summary(self,job):
        """Attach informational documentation counts to the durable release receipt."""
        path=ASSET_ROOT/"releases"/job["job_id"]/".code-index/manifest.json"
        if path.is_file():job["maintenance"]=json.loads(path.read_bytes()).get("maintenance",{})
        self._save(job)

    def _code_index_authorize(self,lease,key):
        """Reject maintenance when its live authority or isolated task has expired."""
        with self.lock:
            self._lease(lease)
            item=self.work.get(key)
            if not item or item["status"]!="open":raise PermissionError("Maintenance task froze; no further review writes")

    def _code_index_snapshot(self,job_id):
        """Merge eligible task evidence with the current published ledger for final projection."""
        self._code_index_reconcile()
        with self.lock:
            job=self.jobs.get(job_id,{})
            ids={job_id,*job.get("batch_members",[])}
            keys=[key for key,item in self.work.items()if item.get("job_id")in ids]
            return self.code_index_ledger.snapshot(keys)
    def _code_index_reconcile(self):
        # Immutable active projection wins after restart or rollback. No task/private DB is read.
        """Restore the active immutable release projection after restart or rollback."""
        with self.deploy_lock:
            current=self.runner.current_asset_id()
            if current and current!=self.code_index_ledger.active_release() and (ASSET_ROOT/"releases"/current/".code-index/index.sqlite").is_file():
                self._code_index_activated(current)
    def _code_index_activated(self,job_id):
        """Import the paired release ledger; legacy indexes initialize pending reviews."""
        root=ASSET_ROOT/"releases"/job_id
        conn=sqlite3.connect("file:"+str(root/".code-index/index.sqlite")+"?mode=ro",uri=True)
        try:
            if not conn.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='maintenance_records'").fetchone():
                memory=sqlite3.connect(":memory:");memory.row_factory=sqlite3.Row
                try:
                    conn.backup(memory);memory.executescript(self.code_index_ledger.scanner.DDL)
                    self.code_index_ledger.scanner.project_maintenance(memory,[])
                    rows=[json.loads(row[0])for row in memory.execute("SELECT body FROM maintenance_records")]
                finally:memory.close()
            else:rows=[json.loads(row[0])for row in conn.execute("SELECT body FROM maintenance_records")]
        finally:conn.close()
        self.code_index_ledger.activate(job_id,rows)
    def code_index_maintenance(self,args,trusted,workspace):
        """Serve shared review reads or validated isolated-task mutations through broker authority."""
        self._code_index_reconcile()
        actor={"user_id":trusted["user_id"],"session_id":workspace,"transport":"operator"if trusted.get("local_caller")else"webagent"}
        action=args.get("action")
        if args.get("root","live")!="live":
            if args.get("root")!="published"or action not in {"list","get"}:raise ValueError("CLI snapshot reads require root published; private roots use WebAgent workspace tools")
            return self.code_index_ledger.scanner.execute_published("code_index_maintenance",args,ASSET_ROOT/"releases"/self.runner.current_asset_id())
        if action in {"list","get"}:
            result=self.code_index_ledger.list(args)
            result.update(source_kind="untrusted_maintenance_metadata",completion_is_review_assertion=True)
            return result
        key=args.get("task_key")
        if key is not None:
            if not trusted.get("local_caller"):raise PermissionError("Cross-session task selection is operator-only")
            if not isinstance(key,str)or not re.fullmatch("[0-9a-f]{64}",key):raise ValueError("Canonical task key required")
        else:
            task_id=args.get("task_id")
            if not isinstance(task_id,str)or not re.fullmatch("[A-Za-z0-9_-]{8,80}",task_id):raise ValueError("Registered task_id required")
            key=hashlib.sha256(canonical([trusted["user_id"],workspace,task_id])).hexdigest()
        item=self.work.get(key)
        if not item or item.get("mode")!="isolated"or item["status"]!="open":raise ValueError("Open isolated maintenance task required")
        if trusted.get("lease_until",0)<=time.time():raise PermissionError("Maintenance authority expired")
        source=self.work_dir/"trees"/key/"v3"
        if source.is_symlink()or not source.is_dir():raise ValueError("Maintenance source unavailable")
        lease={"user_id":trusted["user_id"],"workspace":workspace,"lease_until":trusted.get("lease_until",0),"access_fingerprint":trusted.get("access_fingerprint"),"revoked":False}
        lease_key=(trusted["user_id"],workspace,args.get("request_id"),uuid.uuid4().hex)
        with self.lock:self.code_index_leases[lease_key]=lease
        try:
            result=self.code_index_ledger.mutate(args,actor,key,item["task_id"],source,authorize=lambda:self._code_index_authorize(lease,key))
        finally:
            with self.lock:self.code_index_leases.pop(lease_key,None)
        if lease.get("lease_until",0)<=time.time()or lease.get("revoked"):raise PermissionError("Maintenance authority expired; inspect the original request receipt")
        result.update(source_kind="untrusted_maintenance_metadata",completion_is_review_assertion=True)
        return result

    def handle(self, request):
        if not isinstance(request, dict) or set(request) - {"id", "name", "arguments", "workspace", "timeout_seconds", "trusted"}:
            raise ValueError("invalid broker request")
        trusted, workspace, args = request.get("trusted"), request.get("workspace"), request.get("arguments", {})
        if not isinstance(trusted, dict) or not {"user_id", "session_id"} <= set(trusted) or set(trusted) - {"user_id", "session_id", "lease_until", "access_fingerprint", "local_caller", "bridge_caller"}:
            raise ValueError("trusted caller required")
        if not isinstance(trusted["user_id"], str) or not 1 <= len(trusted["user_id"]) <= 128 or "\0" in trusted["user_id"]:
            raise ValueError("invalid caller")
        try:
            if str(uuid.UUID(workspace)) != workspace or trusted["session_id"] != workspace:
                raise ValueError("workspace/session mismatch")
        except (TypeError, AttributeError):
            raise ValueError("invalid workspace")
        if not isinstance(args, dict):
            raise ValueError("arguments must be an object")
        name = request.get("name")
        if name in ("workspace_seed","source_review","source_patch","v3_workspace_status","v3_workspace_action") or (name=="v3_cargo_task" and "task_id" not in args):
            return self.workspace_request(name,args,trusted,workspace)
        if name=="code_index_maintenance":
            with self.lock:
                if args.get("action")not in {"list","get"}and not self._maintenance_allowed(trusted,args):raise PermissionError("Build service drained; maintenance disabled")
            return self.code_index_maintenance(args,trusted,workspace)
        with self.lock:
            if name == "build_service_maintenance":
                if not trusted.get("local_caller") or set(args)-{"action","task_id"} or args.get("action") not in {"status","drain","resume"}:raise PermissionError("Operator maintenance required")
                if args["action"]!="status":
                    state={"drained":args["action"]=="drain","task_id":args.get("task_id") if args["action"]=="drain" else None}
                    temporary=self.maintenance_file.with_suffix(".tmp")
                    fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600)
                    with os.fdopen(fd,"w") as stream:json.dump(state,stream);stream.flush();os.fsync(stream.fileno())
                    os.replace(temporary,self.maintenance_file)
                    if not state["drained"]:self._start_next()
                return {**self._maintenance(),"busy":bool(self.active or self.active_ui),"queued":sum(j["status"]=="queued" for j in self.jobs.values())}
            read_actions={"status","review","preflight","receipt","list","get","read","search","job","artifact"}
            read_names={"v3_build_status","v3_worker_job","v3_build_dashboard","v3_build_result","administrator_host_lease","administrator_host_revoke","v3_build_cancel","v3_build_leftovers","v3_source_list","v3_source_read","v3_source_read_batch","v3_source_search","v3_source_receipt","file_list","file_read","file_search"}
            if name not in read_names and args.get("action") not in read_actions and not self._maintenance_allowed(trusted,args):raise PermissionError("Build service drained; new work disabled")
            if name == "performance_work":
                return self.performance.handle(trusted, workspace, args)
            if name == "build_work":
                return self.unified_build(request, args)
            if name in ("file_list", "file_read", "file_search", "file_edit", "file_write"):
                return self.unified_files(request, trusted, workspace, args)
            if name == "v3_build_adopt_release":
                if (not trusted.get("local_caller") or trusted["user_id"] != "codex-local"
                        or workspace != LOCAL_WORKSPACE or set(args) != {"job_id"}):
                    raise PermissionError("local release reconciliation required")
                job = self.jobs.get(args["job_id"])
                if self.active is not None or self.active_ui is not None or not job:
                    raise ValueError("release queue must be idle")
                if any(other["status"] == "uncertain" and other.get("effects_started")
                       and other.get("finished_at", 0) > job.get("finished_at", 0)
                       for other in self.jobs.values()):
                    raise ValueError("newer uncertain release requires review")
                if job.get("operation")=="plugin_deploy" and job.get("phase")=="checking_ui":
                    return self._reconcile_plugin_publication(job,trusted,workspace)
                if not self._runtime_release_matches(job):
                    raise ValueError("uncertain release does not match healthy runtime")
                job["operator_adopted_at"] = time.time()
                job["reason"] = ("healthy deployed images accepted as queue base; "
                                 "checkout promotion conflict remains")
                self._save(job)
                self._start_next()
                return {"job_id": job["job_id"], "status": job["status"],
                        "operator_adopted": True, "checkout_promotion": "conflicted"}
            if name == "v3_build_cancel":
                return self._cancel_job(args, trusted, workspace)
            if name == "v3_build_retention_clearance":
                if (not trusted.get("local_caller") or trusted["user_id"] != "codex-local"
                        or workspace != LOCAL_WORKSPACE):
                    raise PermissionError("local operator reconciliation required")
                if set(args) != {"notice_id", "unique_work_preserved", "effects_reconciled"} or args["unique_work_preserved"] is not True or args["effects_reconciled"] is not True:
                    raise ValueError("Explicit unique-work and effect reconciliation required")
                if self.active or self.active_ui or any(j.get("status") in {"queued", "running", "needs_decision"} for j in self.jobs.values()):
                    raise ValueError("build queue must be idle")
                self._reconcile_leftover_state()
                key, item = next(((k,t) for k,t in self.work.items()
                    if t.get("leftover_notice", {}).get("id") == args["notice_id"]), (None, None))
                if not item or item.get("status") != "closed" or item.get("job_id") == "4f605f3b-4444-4447-a511-3a9d4b9677df":
                    raise ValueError("unchanged unpinned terminal task notice required")
                job = self.jobs.get(item.get("job_id"), {})
                if job.get("status") not in {"failed", "interrupted", "uncertain", "stale"}:
                    raise ValueError("unresolved terminal job required")
                root = self.work_dir / "trees" / key
                source_digest = context_digest(release.snapshot(root, TASK_CONTEXT, context_digest))[0]
                item["retention_clearance"] = {"at": time.time(), "source_digest": source_digest,
                    "job_digest": hashlib.sha256(canonical(job)).hexdigest(),
                    "notice_id": args["notice_id"], "unique_work_preserved": True, "effects_reconciled": True}
                self._save_work()
                return {"task_id": item["task_id"], "status": "cleared_for_retention",
                        "grace_seconds": 7200}
            if name == "v3_build_leftovers":
                if args:
                    raise ValueError("Leftover notice read takes no arguments")
                self._incoming_lease(trusted)
                policy = self._reconcile_leftover_state()
                rows = self._leftover_views(trusted, workspace)
                return {"notices": rows[:20], "total": len(rows), "policy": policy}
            if name == "v3_build_leftover_review":
                return self._review_leftover(args, trusted, workspace)
            if name == "v3_build_dashboard":
                if args or trusted["user_id"] != "admin" or workspace != "00000000-0000-0000-0000-000000000000":
                    raise PermissionError("administrator build dashboard denied")
                self._incoming_lease(trusted)
                tasks = {item.get("job_id"): item for item in self.work.values() if item.get("job_id")}
                jobs = []
                # Adopted releases remain addressable by ID but are no longer pending in the queue.
                recent = (job for job in self.jobs.values() if not job.get("operator_adopted_at"))
                for job in sorted(recent, key=lambda row: row["created_at"], reverse=True):
                    view = self._view(job, administrator=True)
                    view.pop("access_fingerprint", None)
                    view["log"] = view["log"][-2048:]
                    view["task_id"] = tasks.get(job["job_id"], {}).get("task_id")
                    summary = tasks.get(job["job_id"], {}).get("summary")
                    if job["operation"] in HOT_OPERATIONS:
                        summary = "UI release · " + (summary or job["job_id"])
                    elif job.get("batch_parent") or job.get("batch_members"):
                        summary = "Shared build · " + (summary or job["job_id"])
                    view["summary"] = summary
                    view["origin"] = ("v2" if job["user_id"] == "python-v2-admin" else
                                      "codex" if job["user_id"] == "codex-local" else "v3")
                    view["started_at"] = job.get("started_at")
                    view["finished_at"] = job.get("finished_at")
                    view["phase_history"] = job.get("phase_history", [])[-32:]
                    if job["operation"] in HOT_OPERATIONS:
                        legacy_tests = {"status": "not_required",
                            "summary": "Compatible assets/plugin; no core Rust build required"}
                    elif job["status"] == "completed" and job.get("images", {}).get("api"):
                        legacy_tests = {"status": "passed",
                            "summary": "Confirmed by successful tested API image; detailed test output was not retained"}
                    elif job["status"] in ("queued", "running"):
                        legacy_tests = {"status": "not_started"}
                    else:
                        legacy_tests = {"status": "unavailable",
                            "summary": "Structured test result was not recorded; inspect the failure reason"}
                    view["rust_tests"] = job.get("rust_tests") or legacy_tests
                    jobs.append(view)
                containers = {}
                for role, target in zip(("api", "loop"), TARGETS):
                    try:
                        state = self.runner.inspect(target)["State"]
                        containers[role] = {"state": state["Status"],
                                            "health": state.get("Health", {}).get("Status", "unknown")}
                    except Exception:
                        containers[role] = {"state": "unavailable", "health": "unknown"}
                policy = self._reconcile_leftover_state()
                leftovers = self._leftover_views(administrator=True)
                live = self._current_release_blob()
                return {"leftovers": leftovers, "leftover_total": len(leftovers), "leftover_policy": policy,
                        "jobs": jobs, "busy": self.active is not None or self.active_ui is not None,
                        "containers": containers, "synchronization": self._sync_report(live, live, ())}
            if name == "v3_build_result":
                if (trusted["user_id"] != "admin"
                        or workspace != "00000000-0000-0000-0000-000000000000"
                        or set(args) != {"job_id"}
                        or not isinstance(args["job_id"], str)
                        or not re.fullmatch(r"[0-9a-f-]{36}", args["job_id"])):
                    raise PermissionError("administrator build result denied")
                self._incoming_lease(trusted)
                job = self.jobs.get(args["job_id"])
                if job is None:
                    return {"job": None}
                # Return a single durable job, including older jobs outside the
                # dashboard's 20-row window; never expose its host lease.
                view = self._view(job, administrator=True)
                view.pop("access_fingerprint", None)
                view["started_at"] = job.get("started_at")
                view["finished_at"] = job.get("finished_at")
                view["phase_history"] = job.get("phase_history", [])[-32:]
                view["rust_tests"] = self._rust_test_result(job)
                return {"job": view}
            if name in ("administrator_host_lease", "administrator_host_revoke"):
                if args:
                    raise ValueError("internal lease control takes no arguments")
                incoming = self._incoming_lease(trusted) if name == "administrator_host_lease" else None
                updated = self.performance.leases(trusted, workspace, incoming)
                for lease in self.code_index_leases.values():
                    if lease["user_id"]==trusted["user_id"]and lease["workspace"]==workspace:
                        if incoming and not lease["revoked"]and lease["lease_until"]>time.time()and lease["access_fingerprint"]==incoming[1]:lease["lease_until"]=incoming[0]
                        else:lease["revoked"]=True
                        updated+=1
                for job in list(self.workspace_leases.values()):
                    if self._owned(job,trusted,workspace):
                        if incoming and job["lease_until"]>time.time() and job["access_fingerprint"]==incoming[1] and not job.get("revoked"):job["lease_until"]=incoming[0]
                        else:job["revoked"]=True
                        updated+=1
                for job in self.jobs.values():
                    if not self._owned(job, trusted, workspace) or job["status"] not in ("queued", "running"):
                        continue
                    if incoming:
                        deadline, fingerprint = incoming
                        if job.get("revoked"):
                            continue
                        if job.get("access_fingerprint") != fingerprint:
                            job["revoked"] = True
                            self._save(job)
                            updated += 1
                            continue
                        # Expired authorization cannot be revived by renewal.
                        if job.get("lease_until", 0) <= time.time():
                            continue
                        # A queued isolated release or deployment keeps its bounded
                        # commit lease while still accepting live revocation.
                        job["lease_until"] = max(job["lease_until"], deadline) if job.get("effects_started") or job.get("isolated") else deadline
                    else:
                        job["revoked"] = True
                    self._save(job)
                    updated += 1
                return {"updated": updated}
            if name == "v3_build_worker_restart":
                if set(args) != {"request_id"} or not isinstance(args["request_id"], str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", args["request_id"]):
                    raise ValueError("stable restart request_id required")
                self._incoming_lease(trusted)
                if self.active is not None or self.active_ui is not None:
                    raise ValueError("shared V3 build target busy")
                response = worker_control("POST", "/v1/restart", {"request_id": args["request_id"]})
                previous_pid = response.get("previous_pid")
                if response.get("status") != "restarting" or not isinstance(previous_pid, int):
                    raise RuntimeError("Build worker restart response invalid")
                for _ in range(50):
                    time.sleep(0.5)
                    try:
                        status = worker_control("GET", "/v1/status")
                    except (OSError, ValueError, RuntimeError):
                        continue
                    if status.get("pid") and status["pid"] != previous_pid:
                        return {"status": "ok", "service": "v3-build-worker.service",
                                "active": True, "interrupted_job": None}
                raise RuntimeError("Restart requested but fresh worker readiness was not confirmed; inspect host before retry")
            if name == "v3_cargo_task":
                if set(args) - {"task_id", "task", "expected_inputs_sha256", "timeout_seconds"} or not {
                        "task_id", "task", "expected_inputs_sha256"} <= set(args):
                    raise ValueError("task-scoped Cargo check arguments required")
                task_id, task = args["task_id"], args["task"]
                timeout = args.get("timeout_seconds", 600)
                expected = args["expected_inputs_sha256"]
                if (not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task_id)
                        or task not in ("check", "test")
                        or not isinstance(expected, str) or not re.fullmatch(r"[a-f0-9]{64}", expected)
                        or type(timeout) is not int or not 1 <= timeout <= 600):
                    raise ValueError("bounded task Cargo request required")
                key = hashlib.sha256(canonical([trusted["user_id"], workspace, task_id])).hexdigest()
                item = self.work.get(key)
                if not item or item.get("mode") != "isolated" or item["status"] != "open":
                    raise ValueError("open isolated task required")
                root = self.work_dir / "trees" / key
                before = context_digest(release.snapshot(root, TASK_CONTEXT, context_digest))[0]
                if before != expected:
                    raise ValueError("stale task source inputs")
                item["last_activity_at"] = time.time()
                item.pop("leftover_notice", None)
                self._save_work()
                try:
                    log = self.runner.cargo_task(root, self.state / "checks" / key, task, timeout)
                    status = "completed"
                except (RuntimeError, TimeoutError, ValueError) as error:
                    log = scrub(str(error))
                    status = "failed"
                after = context_digest(release.snapshot(root, TASK_CONTEXT, context_digest))[0]
                if after != before:
                    status = "stale"
                item["last_activity_at"] = time.time()
                self._save_work()
                return {"task_id": task_id, "task": task, "status": status,
                        "inputs_sha256": after, "log_excerpt": scrub(log)[-4096:]}
            if name == "v3_source_read_batch":
                if set(args) != {"task_id", "requests"} or not isinstance(args["requests"], list) or not 1 <= len(args["requests"]) <= 4:
                    raise ValueError("one to four task source reads required")
                results = []
                for part in args["requests"]:
                    if not isinstance(part, dict):
                        raise ValueError("source read request must be an object")
                    nested = {"name": "v3_source_read", "arguments": {"task_id": args["task_id"], **part},
                              "trusted": trusted, "workspace": workspace}
                    results.append(self.handle(nested))
                if len(canonical(results)) > 48000:
                    raise ValueError("batch source read exceeds result bound")
                return {"task_id": args["task_id"], "results": results}
            if name == "v3_source_search":
                if set(args) - {"task_id", "query", "limit"} or not {"task_id", "query"} <= set(args):
                    raise ValueError("task_id and literal query required")
                task_id, query = args["task_id"], args["query"]
                limit = args.get("limit", 30)
                if (not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task_id)
                        or not isinstance(query, str) or not 1 <= len(query) <= 120
                        or any(ord(char) < 32 for char in query)
                        or type(limit) is not int or not 1 <= limit <= 50):
                    raise ValueError("bounded task search required")
                key = hashlib.sha256(canonical([trusted["user_id"], workspace, task_id])).hexdigest()
                item = self.work.get(key)
                if not item or item.get("mode") != "isolated" or item["status"] != "open":
                    raise ValueError("open isolated task required")
                root = self.work_dir / "trees" / key
                source = root / "v3"
                matches = []
                scanned = 0
                for folder in ("src", "plugins", "tests", "ui", "docs", "config", "build-service"):
                    for path in sorted((source / folder).rglob("*")):
                        if path.is_symlink() or not path.is_file() or path.suffix not in {
                                ".rs", ".py", ".json", ".md", ".html", ".js", ".css", ".toml", ".sh"}:
                            continue
                        scanned += 1
                        if scanned > 3000:
                            raise ValueError("task source search exceeds file bound")
                        data = path.read_bytes()
                        if len(data) > 1024 * 1024 or b"\0" in data:
                            continue
                        for line_no, line in enumerate(data.decode("utf-8", "replace").splitlines(), 1):
                            if query in line:
                                matches.append({"path": path.relative_to(source).as_posix(),
                                                "line": line_no, "text": line[:200]})
                                if len(matches) >= limit:
                                    return {"task_id": task_id, "matches": matches, "truncated": True}
                return {"task_id": task_id, "matches": matches, "truncated": False}
            if name == "v3_source_list":
                if set(args) - {"task_id", "path", "offset", "limit"} or "task_id" not in args:
                    raise ValueError("source list requires task_id and optional directory path")
                task_id, name_path = args["task_id"], args.get("path", "")
                offset, limit = args.get("offset", 0), args.get("limit", 100)
                if not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task_id):
                    raise ValueError("bounded task ID required")
                if not isinstance(name_path, str) or len(name_path) > 240:
                    raise ValueError("bounded source directory path required")
                parts = name_path.split("/") if name_path else []
                allowed_dirs = {"src", "plugins", "tests", "ui", "docs", "config", "build-service"}
                allowed_files = {"Cargo.toml", "Cargo.lock", "build.rs", "Dockerfile",
                                 ".gitignore", "README.md", "deploy.sh", "launch.py",
                                 "worker.py", "ability_plugin_worker.py", "sandbox.py", "promote.py", "ui-baseline.json"}
                if (name_path and (not re.fullmatch(r"[A-Za-z0-9_./-]+", name_path)
                        or any(part in ("", ".", "..") for part in parts)
                        or (parts[0] not in allowed_dirs and not release.inspection_path("v3/" + name_path)))):
                    raise ValueError("source list path denied")
                if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 100:
                    raise ValueError("bounded source list page required")
                key = hashlib.sha256(canonical([trusted["user_id"], workspace, task_id])).hexdigest()
                item = self.work.get(key)
                if not item or item.get("mode") != "isolated" or item["status"] != "open":
                    raise ValueError("open isolated task required")
                root = self.work_dir / "trees" / key
                source_root = root / "v3"
                source = source_root
                for part in parts:
                    source = source / part
                    if source.is_symlink():
                        raise ValueError("source list path denied")
                if not source.exists():
                    raise ValueError("source directory does not exist")
                if not source.is_dir() or not source.resolve().is_relative_to(source_root.resolve()):
                    raise ValueError("source path is not a directory")
                # The broker task owns these files; session workspace seeding is unrelated.
                entries = []
                for child in source.iterdir():
                    if child.is_symlink():
                        continue
                    relative = child.relative_to(source_root).as_posix()
                    if not parts and child.name not in allowed_dirs | allowed_files:
                        continue
                    if not re.fullmatch(r"[A-Za-z0-9_./-]{1,240}", relative):
                        continue
                    if child.is_dir():
                        kind = "directory"
                    elif child.is_file():
                        kind = "file"
                    else:
                        continue
                    entries.append({"name": child.name, "path": relative, "kind": kind})
                    if len(entries) > 3000:
                        raise ValueError("source directory exceeds entry bound")
                entries.sort(key=lambda entry: entry["name"])
                page = entries[offset:offset + limit]
                return {"task_id": task_id, "path": name_path, "entries": page,
                        "offset": offset, "next_offset": offset + len(page),
                        "has_more": offset + len(page) < len(entries),
                        "workspace_inputs_sha256": context_digest(
                            release.snapshot(root, TASK_CONTEXT, context_digest))[0]}
            if name == "v3_source_read":
                if set(args) - {"task_id", "path", "offset", "limit"} or not {"task_id", "path"} <= set(args):
                    raise ValueError("source read requires task_id and path")
                task_id, name_path = args["task_id"], args["path"]
                if not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task_id):
                    raise ValueError("bounded task ID required")
                if not isinstance(name_path, str) or not re.fullmatch(r"[A-Za-z0-9_./-]{1,240}", name_path):
                    raise ValueError("bounded source path required")
                parts = name_path.split("/")
                if (any(part in ("", ".", "..") for part in parts) or parts[0] == "workspaces"
                        or (parts[0] == "data" and not release.inspection_path("v3/" + name_path))):
                    raise ValueError("source read path denied")
                offset, limit = args.get("offset", 1), args.get("limit", 200)
                if type(offset) is not int or offset < 1 or type(limit) is not int or not 1 <= limit <= 200:
                    raise ValueError("bounded source line range required")
                key = hashlib.sha256(canonical([trusted["user_id"], workspace, task_id])).hexdigest()
                item = self.work.get(key)
                if not item or item.get("mode") != "isolated" or item["status"] != "open":
                    raise ValueError("open isolated task required")
                root = self.work_dir / "trees" / key
                source = root / "v3"
                for part in parts:
                    source = source / part
                    if source.is_symlink():
                        raise ValueError("source file unavailable")
                if source.is_symlink() or not source.resolve().is_relative_to((root / "v3").resolve()):
                    raise ValueError("source file unavailable")
                if source.is_dir():
                    raise ValueError("source path is a directory; use v3_source_list")
                if not source.exists():
                    raise ValueError("source file does not exist")
                if not source.is_file():
                    raise ValueError("source file unavailable")
                if release.inspection_path("v3/" + name_path) and source.suffix in {".db", ".sqlite", ".sqlite3"}:
                    result = read_sqlite_snapshot(source, offset, limit)
                    return {"task_id": task_id, "path": name_path, "offset": offset,
                            **result, "workspace_inputs_sha256": context_digest(
                                release.snapshot(root, TASK_CONTEXT, context_digest))[0]}
                data = source.read_bytes()
                if len(data) > 1024 * 1024 or b"\0" in data:
                    raise ValueError("source read exceeds text bound")
                lines = data.decode("utf-8").splitlines()
                selected = lines[offset - 1:offset - 1 + limit]
                if len(canonical(selected)) > 16000:
                    raise ValueError("source read result exceeds bound; request fewer lines")
                return {"task_id": task_id, "path": name_path, "offset": offset,
                        "lines": selected,
                        "total_lines": len(lines),
                        "workspace_inputs_sha256": context_digest(
                            release.snapshot(root, TASK_CONTEXT, context_digest))[0]}
            if name == "v3_source_receipt":
                if set(args) != {"task_id", "request_id"}:
                    raise ValueError("bridge source receipt request denied")
                item = self.work.get(hashlib.sha256(canonical([trusted["user_id"], workspace, args["task_id"]])).hexdigest())
                receipt = item.get("applies", {}).get(args["request_id"]) if item else None
                if not receipt:
                    raise ValueError("source receipt unavailable")
                return dict(receipt)
            if name == "v3_source_prepare":
                if set(args) != {"task_id", "files", "expected_inputs_sha256"}:
                    raise ValueError("source prepare requires task_id, files and input digest")
                task_id = args["task_id"]
                if not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task_id):
                    raise ValueError("bounded task ID required")
                item = self.work.get(hashlib.sha256(canonical(
                    [trusted["user_id"], workspace, task_id])).hexdigest())
                if not item or item["status"] != "open" or item.get("mode") != "isolated":
                    raise ValueError("open isolated task required")
                tree = self.work_dir / "trees" / item["key"]
                current, _ = context_digest(release.snapshot(tree, TASK_CONTEXT, context_digest))
                if current != args["expected_inputs_sha256"]:
                    raise ValueError("stale source inputs")
                patch = prepare_new_source_files(args["files"])
                if any((tree / "v3" / entry["path"]).exists() for entry in args["files"]):
                    raise ValueError("prepare accepts only new files")
                paths, patch = inspect_source_patch_corrected(tree / "v3", patch)
                prepared = item.setdefault("prepares", {})
                if len(prepared) >= 8:
                    raise ValueError("task has too many prepared patches")
                prepared_id = uuid.uuid4().hex
                prepared[prepared_id] = {"patch": patch, "inputs_sha256": current,
                                         "patch_sha256": hashlib.sha256(patch.encode()).hexdigest(),
                                         "paths": paths}
                self._save_work()
                return {"task_id": task_id, "prepared_id": prepared_id, "patch": patch,
                        "patch_sha256": prepared[prepared_id]["patch_sha256"],
                        "paths": paths, "workspace_inputs_sha256": current}
            if name in ("v3_source_apply", "v3_source_apply_prepared"):
                # Distinct model schemas converge on one checked, receipted broker write.
                required = ({"task_id", "request_id", "prepared_id", "expected_inputs_sha256"}
                            if name == "v3_source_apply_prepared" else
                            {"task_id", "request_id", "patch", "expected_inputs_sha256"})
                if set(args) != required:
                    raise ValueError("source apply arguments do not match tool")
                task_id, request_id = args["task_id"], args["request_id"]
                if not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task_id) or not isinstance(request_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", request_id):
                    raise ValueError("bounded task and request IDs required")
                item = self.work.get(hashlib.sha256(canonical([trusted["user_id"], workspace, task_id])).hexdigest())
                if not item or item["status"] != "open":
                    raise ValueError("registered open bridge task required")
                if name == "v3_source_apply_prepared":
                    prepared_id = args["prepared_id"]
                    if not isinstance(prepared_id, str) or not re.fullmatch(r"[0-9a-f]{32}", prepared_id):
                        raise ValueError("bounded prepared_id required")
                    prepared = item.get("prepares", {}).get(prepared_id)
                    if not prepared or prepared["inputs_sha256"] != args["expected_inputs_sha256"]:
                        raise ValueError("prepared patch unavailable or stale")
                    patch = prepared["patch"]
                else:
                    patch = args["patch"]
                if not isinstance(patch, str) or not 0 < len(patch.encode()) <= 1024 * 1024:
                    raise ValueError("bounded text patch required")
                digest = hashlib.sha256(canonical(args)).hexdigest()
                receipt = item.setdefault("applies", {}).get(request_id)
                if receipt:
                    if receipt["request_sha256"] != digest:
                        raise ValueError("request_id reused with changed source apply")
                    return dict(receipt)
                isolated = item.get("mode") == "isolated"
                if (self.active is not None or self.active_ui is not None) and not isolated:
                    raise ValueError("build active; source apply denied")
                tree = self.work_dir / "trees" / item["key"] if isolated else self.runner.source
                current, _ = context_digest(
                    release.snapshot(tree, TASK_CONTEXT, context_digest) if isolated else self.runner.snapshot())
                if current != args["expected_inputs_sha256"]:
                    raise ValueError("stale source inputs")
                source = tree / "v3"
                paths, patch = inspect_source_patch_corrected(source, patch)
                receipt = {"task_id": task_id, "request_id": request_id, "request_sha256": digest,
                           "patch_sha256": hashlib.sha256(patch.encode()).hexdigest(),
                           "status": "dispatching", "paths": paths}
                item["applies"][request_id] = receipt
                self._save_work()
                try:
                    source_git(source, ["apply", "--unidiff-zero", "--whitespace=error", "-"], patch.encode())
                except Exception:
                    receipt.update(status="uncertain", reason="source apply outcome requires review")
                else:
                    receipt.update(status="success", reason="checked patch applied")
                self._save_work()
                return dict(receipt)
            if name == "v3_build_work":
                if set(args) - {"action", "task_id", "summary", "expected_inputs_sha256", "mode", "sync_decision", "expected_sync_digest", "offset"}:
                    raise ValueError("unsupported work argument")
                action = args.get("action")
                if action == "status":
                    if set(args) not in ({"action"}, {"action", "task_id"}):
                        raise ValueError("status accepts only an optional task_id")
                    selected = args.get("task_id")
                    if selected is not None and (not isinstance(selected, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", selected)):
                        raise ValueError("bounded task_id required")
                    own = [dict(task_id=i["task_id"], status=i["status"], summary=i["summary"],
                                inputs_sha256=i.get("inputs_sha256"), job_id=i.get("job_id"),
                                mode=i.get("mode", "shared"), workspace_path=i.get("workspace_path"),
                                synchronization=i.get("synchronization"),
                                workspace_inputs_sha256=(context_digest(release.snapshot(
                                    self.work_dir / "trees" / i["key"], TASK_CONTEXT, context_digest))[0]
                                    if i.get("mode") == "isolated" and i["status"] == "open" else None))
                           for i in self.work.values() if self._owned(i, trusted, workspace)
                           and (selected is None or i["task_id"] == selected)]
                    return {"open_count": len(self._open_work()), "own": own if selected else own[-20:],
                            "busy": self.active is not None or self.active_ui is not None}
                if action not in ("begin", "finish", "abandon", "preflight", "decide"):
                    raise ValueError("unsupported work action")
                task_id = args.get("task_id")
                if not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", task_id):
                    raise ValueError("bounded stable task_id required")
                key = hashlib.sha256(canonical([trusted["user_id"], workspace, task_id])).hexdigest()
                item = self.work.get(key)
                if action in ("preflight", "decide"):
                    allowed = {"action", "task_id", "offset"} if action == "preflight" else {"action", "task_id", "sync_decision", "expected_sync_digest"}
                    if set(args) - allowed or not item or item.get("mode") != "isolated":
                        raise ValueError("Owned isolated task required")
                    folder = self.work_dir / "trees" / key
                    job = self.jobs.get(item.get("job_id"))
                    if job:
                        base = (self.state / job["job_id"] / "base.tar").read_bytes()
                        task = (self.state / job["job_id"] / "task.tar").read_bytes()
                    elif item["status"] == "open":
                        base = (folder / "baseline.tar").read_bytes()
                        task = release.snapshot(folder, TASK_CONTEXT, context_digest)
                    else:
                        raise ValueError("Task cannot be checked or decided")
                    if job and job.get("batch_parent"):
                        parent = self.jobs[job["batch_parent"]]
                        parent_task = next((i["task_id"] for i in self.work.values() if i.get("job_id") == parent["job_id"]), None)
                        return {"status": "needs_decision", "decision_task_id": parent_task,
                                "synchronization": parent.get("synchronization")}
                    if job and job["status"] == "needs_decision" and (self.state / job["job_id"] / "admitted_base.tar").is_file():
                        base = (self.state / job["job_id"] / "admitted_base.tar").read_bytes()
                        task = (self.state / job["job_id"] / "candidate.tar").read_bytes()
                    live = self._current_release_blob()
                    candidate, changed = release.candidate(base, task, live, context_digest, allow_empty=True)
                    offset = args.get("offset", 0)
                    if type(offset) is not int or offset < 0:
                        raise ValueError("Nonnegative offset required")
                    report = self._sync_report(live, candidate, changed, offset)
                    if action == "preflight":
                        return {"task_id": task_id, "synchronization": report}
                    if not job or job["status"] != "needs_decision":
                        raise ValueError("Only a paused pre-effect job can be decided")
                    if args.get("expected_sync_digest") != report["digest"]:
                        return {"task_id": task_id, "status": "needs_decision", "reason": "Synchronization decision is stale", "synchronization": report}
                    decision = args.get("sync_decision")
                    if decision == "synchronize":
                        return {"task_id": task_id, "status": "needs_synchronization", "synchronization": report}
                    if decision == "cancel":
                        job.update(status="cancelled", phase="finished", reason="Cancelled before deployment", finished_at=time.time())
                    elif decision == "proceed":
                        deadline, fingerprint = self._incoming_lease(trusted)
                        if job.get("revoked") or job.get("access_fingerprint") != fingerprint:
                            raise PermissionError("Deployment authorization changed")
                        job.update(status="queued", phase="waiting", sync_ack_digest=report["digest"], lease_until=deadline, synchronization=report, sync_resume=True)
                    else:
                        raise ValueError("Choose synchronize, proceed or cancel")
                    self._save(job)
                    if decision == "cancel":
                        self._mirror_batch(job)
                    self._start_next()
                    return {"task_id": task_id, "job_id": job["job_id"], "status": job["status"], "synchronization": report}
                if action == "begin":
                    if set(args) not in (
                            {"action", "task_id", "summary"},
                            {"action", "task_id", "summary", "mode"}):
                        raise ValueError("begin requires task_id and summary")
                    mode = args.get("mode", "shared")
                    if mode not in ("shared", "isolated"):
                        raise ValueError("unsupported source work mode")
                    if any(work.get("mode", "shared") != mode for work in self._open_work()):
                        raise ValueError("shared and isolated work modes cannot overlap")
                    if mode == "shared" and any(job.get("isolated") and job["status"] in ("queued", "running") for job in self.jobs.values()):
                        raise ValueError("isolated release queue active")
                    summary = args["summary"]
                    if not isinstance(summary, str) or not 1 <= len(summary) <= 200 or any(ord(c) < 32 for c in summary):
                        raise ValueError("bounded summary required")
                    if item:
                        if item["summary"] != summary or item.get("mode", "shared") != mode:
                            raise ValueError("task_id reused with changed summary or mode")
                        return {"task_id": task_id, "status": item["status"],
                                "open_count": len(self._open_work()),
                                "workspace_path": item.get("workspace_path")}
                    if mode == "shared" and (self.active is not None or self.active_ui is not None):
                        raise ValueError("build or deployment active; wait before beginning new work")
                    if len(self.work) >= 1000:
                        raise ValueError("coordination registry full; administrator maintenance required")
                    item = {"task_id": task_id, "user_id": trusted["user_id"],
                            "workspace": workspace, "summary": summary, "status": "open",
                            "created_at": time.time(), "mode": mode, "key": key, "applies": {}}
                    if mode == "isolated":
                        blob = self._current_release_blob()
                        # Only authenticated admin and the fixed local operator receive
                        # private troubleshooting snapshots, never another task owner.
                        if trusted["user_id"] == "admin" or (
                                trusted.get("local_caller") is True and
                                trusted["user_id"] == "codex-local" and workspace == LOCAL_WORKSPACE):
                            files = release.entries(blob, context_digest)
                            inspection = release.entries(self.runner.snapshot(inspection=True), context_digest)
                            files.update({name: value for name, value in inspection.items()
                                          if release.inspection_path(name)})
                            blob = release.archive(files, context_digest)
                        folder = self.work_dir / "trees" / key
                        release.extract(folder, blob, context_digest)
                        (folder / "baseline.tar").write_bytes(blob)
                        item["workspace_path"] = str(folder / "v3")
                        item["base_inputs_sha256"] = context_digest(blob)[0]
                    self.work[key] = item
                    self._save_work()
                    return {"task_id": task_id, "status": "open", "open_count": len(self._open_work()),
                            "workspace_path": item.get("workspace_path"),
                            "workspace_inputs_sha256": item.get("base_inputs_sha256")}
                if action == "abandon":
                    if set(args) != {"action", "task_id"} or not item or item["status"] != "open":
                        raise ValueError("only an open task in this session may be abandoned")
                    item.update(status="abandoned", finished_at=time.time())
                    self._save_work()
                    return {"task_id": task_id, "status": "abandoned", "open_count": len(self._open_work())}
                if set(args) - {"action", "task_id", "expected_inputs_sha256", "sync_decision", "expected_sync_digest"} or not {"action", "task_id", "expected_inputs_sha256"} <= set(args):
                    raise ValueError("finish requires task_id and expected_inputs_sha256")
                expected = args["expected_inputs_sha256"]
                if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
                    raise ValueError("expected_inputs_sha256 required")
                if not item:
                    raise ValueError("task_id not registered in this session")
                if item["status"] == "closed":
                    if item["inputs_sha256"] != expected:
                        raise ValueError("closed task digest cannot change")
                    job_id = (item.get("job_id") if item.get("mode") == "isolated"
                              else self._auto_schedule(item, trusted, workspace))
                    return {"task_id": task_id, "status": "closed", "job_id": job_id,
                            "open_count": len(self._open_work())}
                if item.get("mode") == "isolated":
                    folder = self.work_dir / "trees" / key
                    task_blob = release.snapshot(folder, TASK_CONTEXT, context_digest)
                    current, _ = context_digest(task_blob)
                    if current != expected:
                        raise ValueError("stale work inputs")
                    base_blob = (folder / "baseline.tar").read_bytes()
                    _, changed = release.candidate(base_blob, task_blob, base_blob, context_digest, allow_empty=True)
                    live = self._current_release_blob()
                    try:
                        candidate, current_changed = release.candidate(base_blob, task_blob, live, context_digest, allow_empty=True)
                    except ValueError:
                        # Existing stale-task admission remains a durable failed job.
                        candidate = release.archive({name: value for name, value in release.entries(task_blob, context_digest).items()
                                                     if not release.inspection_path(name)}, context_digest)
                        current_changed = changed
                    report = self._sync_report(live, candidate, current_changed)
                    item["synchronization"] = report
                    self._save_work()
                    decision = args.get("sync_decision")
                    if decision is not None and decision not in ("synchronize", "proceed", "cancel"):
                        raise ValueError("Choose synchronize, proceed or cancel")
                    if report["total"] or decision is not None:
                        if args.get("expected_sync_digest") != report["digest"]:
                            return {"task_id": task_id, "status": "needs_decision", "reason": "Fresh synchronization decision required", "synchronization": report}
                        if decision == "cancel":
                            item.update(status="abandoned", finished_at=time.time())
                            self._save_work()
                            return {"task_id": task_id, "status": "abandoned", "synchronization": report}
                        if decision != "proceed":
                            return {"task_id": task_id, "status": "needs_synchronization" if decision == "synchronize" else "needs_decision", "synchronization": report}
                    package_changed=[path for path in changed if not hot_source_path(path) and (
                        generic_package_path(path,candidate) or generic_package_path(path,base_blob))]
                    core_inputs=self.runner.core_compile_inputs() if package_changed and hasattr(self.runner,"core_compile_inputs") else set()
                    ui_only = all(hot_source_path(path) or (
                        (generic_package_path(path,candidate) or generic_package_path(path,base_blob))
                        and path not in core_inputs and not native_package_migration(path,base_blob)) for path in changed)
                    index_only=ui_only and(not changed or any(path in INDEX_METADATA_FILES for path in changed))
                    plugin_only=ui_only and any((path.startswith(ABILITY_ROOT) and path not in INDEX_METADATA_FILES) or path.startswith("v3/config/") or path=="v3/src/agent_loop/tools/administrator-schemas.json" for path in changed)
                    host_only = self.host_source_only(current_changed,candidate,trusted)
                    lease_until, fingerprint = self._incoming_lease(trusted)
                    job_id = str(uuid.uuid4())
                    private = self.state / job_id
                    private.mkdir(mode=0o700)
                    (private / "base.tar").write_bytes(base_blob)
                    (private / "task.tar").write_bytes(task_blob)
                    job = {"job_id": job_id, "request_key": "task-" + key,
                           "body_digest": hashlib.sha256(canonical([task_id, current])).hexdigest(),
                           "user_id": trusted["user_id"], "workspace": workspace,
                           "operation": "host_service_deploy" if host_only else "plugin_deploy" if plugin_only else "index_deploy" if index_only else "ui_deploy" if ui_only else "build_deploy", "task_id":task_id, "isolated": True, "inputs_sha256": current,
                           "created_at": time.time(), "status": "queued", "phase": "waiting",
                           "images": {}, "log": "", "tool_name": "v3_build_work",
                           "lease_until": max(lease_until, time.time() + 4 * 3600),
                           "access_fingerprint": fingerprint, "revoked": False}
                    if decision == "proceed":
                        job["sync_ack_digest"] = report["digest"]
                    job["synchronization"] = report
                    if host_only:
                        job["rust_tests"]={"status":"not_required","summary":"Exact installed host broker and its docs/tests only; application runtime unchanged"}
                    if ui_only:
                        job["rust_tests"] = {"status": "not_required",
                            "summary": "Compatible assets/plugin; no core Rust build required"}
                    self._save(job)
                    self.jobs[job_id] = job
                    item.update(status="closed", inputs_sha256=current, job_id=job_id,
                                finished_at=time.time())
                    self._save_work()
                    if ui_only and self._can_start_parallel_ui():
                        self._start_parallel_ui(job)
                    else:
                        self._start_next()
                    return {"task_id": task_id, "status": "closed",
                            "open_count": len(self._open_work()), "job_id": job_id,
                            "inputs_sha256": current}
                if self.active is not None or self.active_ui is not None:
                    raise ValueError("build or deployment active; close after it finishes")
                current, _ = context_digest(self.runner.snapshot())
                if current != expected:
                    raise ValueError("stale work inputs")
                item.update(status="closed", inputs_sha256=current, finished_at=time.time())
                self._save_work()
                job_id = self._auto_schedule(item, trusted, workspace)
                return {"task_id": task_id, "status": "closed", "open_count": len(self._open_work()),
                        "job_id": job_id, "inputs_sha256": current}
            if name == "v3_build_status":
                if args:
                    raise ValueError("status takes no arguments")
                digest, count = context_digest(self.runner.snapshot())
                containers = {}
                for role, target in zip(("api", "loop"), TARGETS):
                    try:
                        value = self.runner.inspect(target)
                        containers[role] = {"image_id": value["Image"], "state": value["State"]["Status"], "health": value["State"].get("Health", {}).get("Status", "unknown")}
                    except Exception:
                        containers[role] = {"state": "unavailable"}
                jobs = [self._view(j) for j in sorted(self.jobs.values(), key=lambda j: j["created_at"], reverse=True) if self._owned(j, trusted, workspace)][:3]
                live = self._current_release_blob()
                return {"inputs_sha256": digest, "input_files": count, "containers": containers, "jobs": jobs, "busy": self.active is not None or self.active_ui is not None,
                        "synchronization": self._sync_report(live, live, ())}
            if name == "v3_worker_job":
                if set(args) != {"job_id"}:
                    raise ValueError("job_id required")
                job = self.jobs.get(args["job_id"])
                participant = any(self._owned(item, trusted, workspace) and item.get("job_id") == args["job_id"]
                                  for item in self.work.values())
                if not job or not (self._owned(job, trusted, workspace) or participant):
                    raise ValueError("job not available to this session")
                return self._view(job)
            operation = ("build_deploy" if name == "administrator_auto_build_deploy"
                         else "build" if name == "v3_build_image" else args.get("operation"))
            allowed = {"request_id", "expected_inputs_sha256"} | ({"operation", "job_id"} if name == "v3_worker_submit" else set())
            if name not in ("v3_build_image", "v3_worker_submit", "administrator_auto_build_deploy") or set(args) - allowed or operation not in ("build", "deploy", "build_deploy"):
                raise ValueError("unsupported fixed build operation")
            if name == "v3_worker_submit" and operation == "build_deploy":
                raise ValueError("unsupported fixed build operation")
            if self.stopping:
                raise PermissionError("host broker is stopping")
            lease_until, access_fingerprint = self._incoming_lease(trusted)
            request_id, expected = args.get("request_id"), args.get("expected_inputs_sha256")
            if not isinstance(request_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", request_id):
                raise ValueError("bounded stable request_id required")
            if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
                raise ValueError("expected_inputs_sha256 required")
            request_key = hashlib.sha256(canonical([trusted["user_id"], workspace, request_id])).hexdigest()
            body_digest = hashlib.sha256(canonical([name, args])).hexdigest()
            if request_key in self.requests:
                job = self.jobs[self.requests[request_key]]
                if job["body_digest"] != body_digest:
                    raise ValueError("request_id reused with changed body")
                return self._view(job)
            if self.active is not None or self.active_ui is not None:
                raise ValueError("shared V3 build target busy; coordinate writers before retry")
            if self._open_work():
                raise ValueError("registered V3 source work remains open")
            if len(self.jobs) >= 1000:
                raise ValueError("durable job limit reached; administrator maintenance required")
            blob = self.runner.snapshot()
            current, _ = context_digest(blob)
            if current != expected:
                raise ValueError("stale build inputs")
            live = self._current_release_blob()
            report = self._sync_report(live, blob, ())
            if report["total"]:
                return {"status": "needs_synchronization", "synchronization": report,
                        "reason": "Legacy checkout builds require synchronization first; use an isolated task to preserve deployed changes"}
            images = {}
            source_job = None
            if operation == "deploy":
                source_job = self.jobs.get(args.get("job_id"))
                if not source_job or not self._owned(source_job, trusted, workspace) or source_job["operation"] != "build" or source_job["status"] != "completed" or source_job["inputs_sha256"] != current:
                    raise ValueError("deploy requires this session's successful unchanged build")
                images = dict(source_job["images"])
            elif "job_id" in args:
                raise ValueError("build does not accept job_id")
            job_id = str(uuid.uuid4())
            job = {"job_id": job_id, "request_key": request_key, "body_digest": body_digest,
                   "user_id": trusted["user_id"], "workspace": workspace, "operation": operation,
                   "inputs_sha256": current, "created_at": time.time(), "status": "queued",
                   "phase": "admitted", "images": images, "log": "",
                   "tool_name": name, "lease_until": lease_until, "access_fingerprint": access_fingerprint, "revoked": False}
            if source_job:
                job["source_job_id"] = source_job["job_id"]
            self._save(job)
            self.jobs[job_id] = job
            self.requests[request_key] = job_id
            self.active = job_id
            self.pool.submit(self._run, job, blob)
            return self._view(job)

    def _phase(self, job, phase):
        with self.lock:
            self._batch_lease(job)
            job["phase"] = phase
            job.setdefault("phase_history", []).append({"phase": phase, "at": time.time()})
            job["phase_history"] = job["phase_history"][-32:]
            self._save(job)

    def _rebase_after_ui(self, job, blob, tested_runtime=False):
        live = self._current_release_blob()
        live_digest = context_digest(live)[0]
        if live_digest == job["base_inputs_sha256"]:
            return blob
        folder = self.state / job["job_id"]
        merged = live
        changed = set()
        for part in (job, *(self.jobs[key] for key in job.get("batch_members", []))):
            part_dir = self.state / part["job_id"]
            merged, paths = release.candidate(
                (part_dir / "base.tar").read_bytes(),
                (part_dir / "task.tar").read_bytes(), merged, context_digest, allow_empty=part.get("operation")=="index_deploy")
            changed.update(paths)
        if tested_runtime:
            before = {name: item for name, item in release.entries(blob, context_digest).items()
                      if not hot_source_path(name)}
            after = {name: item for name, item in release.entries(merged, context_digest).items()
                     if not hot_source_path(name)}
            if before != after:
                raise ValueError("runtime build inputs changed during UI release")
            old_ui = release.entries(blob, context_digest)
            new_ui = release.entries(merged, context_digest)
            if any(old_ui.get(name) != new_ui.get(name) for name in RUST_UI_FIXTURES):
                raise ValueError("Rust test fixture changed during image build")
            job["built_inputs_sha256"] = job["inputs_sha256"]
            job["rust_tests"] = {"status": "passed",
                "summary": "Runtime inputs tested before UI asset rebase; UI assets validated separately"}
        elif not all(hot_source_path(path) for path in changed):
            raise ValueError("parallel UI release changed runtime inputs")
        digest = context_digest(merged)[0]
        release_root = folder / ("release-rebased-" + digest[:16])
        if release_root.exists():
            prepared = release.snapshot(release_root, CONTEXT, context_digest)
            if context_digest(prepared)[0] != digest:
                raise ValueError("rebased release snapshot changed")
        else:
            release.extract(release_root, merged, context_digest)
        (folder / "candidate.tar").write_bytes(merged)
        job.update(inputs_sha256=digest, base_inputs_sha256=live_digest,
                   changed_paths=sorted(changed), release_root=str(release_root))
        for member_id in job.get("batch_members", []):
            member = self.jobs[member_id]
            (self.state / member_id / "candidate.tar").write_bytes(merged)
            member.update(inputs_sha256=digest, base_inputs_sha256=live_digest)
            self._save(member)
        self._save(job)
        return merged

    def _run_ui_hot(self, job, blob):
        with self.deploy_lock:
            self._run_ui_hot_locked(job, blob)

    def _run_ui_hot_locked(self, job, blob):
        if self._has_unresolved_release(job["job_id"]):
            raise ValueError("uncertain release must be reconciled before UI switch")
        blob = self._rebase_after_ui(job, blob)
        with self.lock:
            job["hot_assets"] = True
            self._save(job)
        if not self._sync_job(job, self._current_release_blob(), blob, job.get("changed_paths", ())):
            return
        self._phase(job, "indexing_source")
        self.runner.stage_assets(blob, job["job_id"])
        self._maintenance_job_summary(job)
        self.runner.validate_ui(job["job_id"], job["changed_paths"])
        prepared = release.snapshot(Path(job["release_root"]), CONTEXT, context_digest)
        if context_digest(prepared)[0] != job["inputs_sha256"]:
            raise ValueError("frozen UI release changed")
        if context_digest(self._current_release_blob())[0] != job["base_inputs_sha256"]:
            raise ValueError("release base changed before UI switch")
        previous = {role: self.runner.inspect(name)
                    for role, name in zip(("api", "loop"), TARGETS)}
        with self.lock:
            job["images"] = {role: previous[role]["Image"] for role in ("api", "loop")}
            job["consumer_before"]={role:{"id":previous[role]["Id"],"image":previous[role]["Image"]}for role in ("api","loop")}
            job["asset_previous"] = self.runner.current_asset_id()
            self._save(job)
        self._phase(job, "switching_ui")
        with self.lock:
            self._batch_lease(job)
            job["effects_started"] = True
            self._save(job)
        self.runner.activate_assets(job["job_id"])
        self.runner.promote_live_ui(self._current_release_blob(), blob, job["changed_paths"])
        self._phase(job, "checking_ui")
        self.runner.verify_ui(job["changed_paths"])
        for role, name in zip(("api", "loop"), TARGETS):
            if not self.runner.healthy(name, job["images"][role]):
                raise RuntimeError("V3 " + role + " unhealthy after UI switch")
        if job["operation"]=="plugin_deploy":
            if hasattr(self.runner,"verify_plugin_adoption"):
                job["consumer_adoption"]=self.runner.verify_plugin_adoption(job["job_id"])
            for role,name in zip(("api","loop"),TARGETS):
                if self.runner.inspect(name).get("Id")!=previous[role].get("Id"):
                    raise RuntimeError("Unrelated container replaced during plugin publication")
        self._phase(job, "promoting_checkout")
        self.runner.promote_checkout(self._current_release_blob(), blob, job["changed_paths"])
        job["synchronization"] = self._sync_report(blob, blob, ())
        with self.lock:
            self._batch_lease(job)
            job.update(status="completed", phase="finished", reason="Installed host service source published; application images unchanged" if job["operation"]=="host_service_deploy" else "Ability assets and packages published; application images unchanged" if job["operation"]=="plugin_deploy" else "UI assets served from frozen release",
                       finished_at=time.time())
            job.setdefault("phase_history", []).append(
                {"phase": "finished", "at": job["finished_at"]})
            self._save(job)

    def _run(self, job, blob):
        with self.lock:
            event = self.job_cancellations.setdefault(job["job_id"], threading.Event())
            if job.get("cancel_request"):
                event.set()
        if hasattr(self.runner, "job_context"):
            self.runner.job_context.job = job
            self.runner.job_context.event = event
        mutated = False
        deploy_locked = False
        try:
            with self.lock:
                self._batch_lease(job)
                job["status"] = "running"
                job["started_at"] = time.time()
                self._save(job)
            if job["operation"]=="host_service_deploy":
                if not self.host_source_only(job["changed_paths"],blob,{"local_caller":job["user_id"]=="codex-local"}):raise PermissionError("Installed host source changed after admission")
                if not self.runner.ui_hot_swappable():raise ValueError("Host source publication requires frozen asset mounts")
                self._run_ui_hot(job,blob)
                return
            if job["operation"]=="plugin_deploy" and not self.runner.plugin_hot_swappable():
                raise ValueError("General ability runtime contract unavailable; core migration required")
            if job["operation"] in ("index_deploy","plugin_deploy") and not self.runner.ui_hot_swappable():
                raise ValueError("Index maintenance requires hot publication; container replacement is forbidden")
            if job["operation"] in HOT_OPERATIONS and self.runner.ui_hot_swappable():
                self._run_ui_hot(job, blob)
                return
            if job["operation"] in ("build", "build_deploy"):
                self.runner.prepare_build_space()
                # Capture current environment values privately solely to scrub
                # any accidental occurrence in compiler output.
                for target in TARGETS:
                    try:
                        self.runner.inspect(target)
                    except Exception:
                        pass
                for role in ("api", "loop"):
                    self.runner.check_space(MIN_DEPLOY_FREE)
                    self._phase(job, "building_" + role)
                    if role == "api":
                        with self.lock:
                            job["rust_tests"] = {"status": "running"}
                            self._save(job)
                    image, log = self.runner.build(blob, job["job_id"], role)
                    with self.lock:
                        job["images"][role] = image
                        if role == "api":
                            job["rust_tests"] = {"status": "passed",
                                "summary": "Locked Rust tests passed in the tested image build"}
                        job["log"] = scrub(job["log"] + log, self.runner.secrets)
                        self._save(job)
                self.runner.check_space(MIN_DEPLOY_FREE)
            if job["operation"] in ("deploy", "build_deploy", "ui_deploy", "index_deploy", "plugin_deploy"):
                self.deploy_lock.acquire()
                deploy_locked = True
                if self._has_unresolved_release(job["job_id"]):
                    raise ValueError("uncertain release must be reconciled before deployment")
                if job.get("isolated") and job["operation"] == "build_deploy":
                    blob = self._rebase_after_ui(job, blob, tested_runtime=True)
                if job.get("isolated") and not self._sync_job(job, self._current_release_blob(), blob, job.get("changed_paths", ())):
                    return
                self._phase(job, "indexing_source")
                self.runner.stage_assets(blob, job["job_id"])
                self._maintenance_job_summary(job)
                self.runner.validate_ui(job["job_id"], job.get("changed_paths", ()))
                with self.lock:
                    job["asset_previous"] = self.runner.current_asset_id()
                    self._save(job)
                self._phase(job, "capturing_rollback")
                previous = {role: self.runner.inspect(name) for role, name in zip(("api", "loop"), TARGETS)}
                if job["operation"] in HOT_OPERATIONS:
                    job["images"] = {role: previous[role]["Image"] for role in ("api", "loop")}
                private = self.state / job["job_id"]
                private.mkdir(mode=0o700, exist_ok=bool(job.get("isolated")))
                # Credentials remain in protected host state, never tool output.
                fd = os.open(private / "container-config.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(fd, "wb") as stream:
                    stream.write(canonical(previous))
                    stream.flush()
                    os.fsync(stream.fileno())
                self.runner.export([previous[r]["Image"] for r in ("api", "loop")], private / "rollback-images.tar")
                if job.get("isolated"):
                    prepared = release.snapshot(Path(job["release_root"]), CONTEXT, context_digest)
                    if context_digest(prepared)[0] != job["inputs_sha256"]:
                        raise ValueError("frozen release changed before deployment")
                    if context_digest(self._current_release_blob())[0] != job["base_inputs_sha256"]:
                        raise ValueError("release base changed before deployment")
                elif context_digest(self.runner.snapshot())[0] != job["inputs_sha256"]:
                    raise ValueError("build inputs changed before deployment")
                if self._open_work() and not job.get("isolated"):
                    raise ValueError("registered V3 source work reopened before deployment")
                with self.lock:
                    job["rollback"] = {"api": TARGETS[0] + "-pre-build-" + job["job_id"], "loop": TARGETS[1] + "-pre-build-" + job["job_id"]}
                    self._save(job)
                # Record intent before each externally visible mutation. On a
                # crash the next broker only reconciles, never retries it.
                self._phase(job, "stopping_loop")
                with self.lock:
                    self._batch_lease(job)
                    # The API cannot renew while it replaces itself. A bounded
                    # commit lease covers container health and keeps revocation live.
                    job["lease_until"] = max(job["lease_until"], time.time() + 180)
                    for member_id in job.get("batch_members", []):
                        member = self.jobs[member_id]
                        member["lease_until"] = max(member["lease_until"], job["lease_until"])
                        self._save(member)
                    job["effects_started"] = True
                    self._save(job)
                mutated = True
                self.runner.stop(TARGETS[1])
                self._phase(job, "stopping_api")
                self.runner.stop(TARGETS[0])
                self._phase(job, "switching_ui")
                self.runner.activate_assets(job["job_id"])
                for role, name in zip(("api", "loop"), TARGETS):
                    self._phase(job, "renaming_" + role)
                    self.runner.rename(name, job["rollback"][role])
                    self._phase(job, "creating_" + role)
                    if job.get("isolated"):
                        self.runner.create(name, job["images"][role], previous[role],
                                           job["release_root"])
                    else:
                        self.runner.create(name, job["images"][role], previous[role])
                    self._phase(job, "starting_" + role)
                    self.runner.start(name)
                # API readiness depends on the loop heartbeat. Start both
                # replacements before waiting for either, retaining a durable
                # checkpoint and live lease check for every health phase.
                for role, name in zip(("api", "loop"), TARGETS):
                    self._phase(job, "checking_" + role)
                    if not self.runner.healthy(name, job["images"][role]):
                        raise RuntimeError("replacement V3 " + role + " not healthy; rollback material retained")
                if self.runner.ui_hot_swappable():
                    self.runner.promote_live_ui(self._current_release_blob(), blob, job.get("changed_paths", ()))
                    self._phase(job, "checking_ui")
                    self.runner.verify_ui(job.get("changed_paths", ()))
            if job.get("isolated"):
                self._phase(job, "promoting_checkout")
                self.runner.promote_checkout(self._current_release_blob(), blob, job["changed_paths"])
                job["synchronization"] = self._sync_report(blob, blob, ())
            after, _ = context_digest(
                release.snapshot(Path(job["release_root"]), CONTEXT, context_digest)
                if job.get("isolated") else self.runner.snapshot())
            with self.lock:
                self._batch_lease(job)
                job["status"] = "completed" if after == job["inputs_sha256"] else ("uncertain" if mutated else "stale")
                job["reason"] = "inputs unchanged" if after == job["inputs_sha256"] else "build inputs changed; artifact cannot be used for deployment"
                job["phase"] = "finished"
                job["finished_at"] = time.time()
                job.setdefault("phase_history", []).append({"phase": "finished", "at": job["finished_at"]})
                self._save(job)
        except Exception as error:
            with self.lock:
                job["status"] = "uncertain" if mutated or job.get("effects_started") else ("cancelled" if isinstance(error, PermissionError) else "failed")
                job["reason"] = scrub(str(error), self.runner.secrets)
                if getattr(error, "log", None):
                    job["log"] = scrub(job.get("log", "") + error.log, self.runner.secrets)[-MAX_LOG:]
                job["finished_at"] = time.time()
                if job.get("isolated") and job.get("phase") == "promoting_checkout":
                    try:
                        job["synchronization"] = self._sync_report(self._current_release_blob(), blob, job.get("changed_paths", ()))
                        job["synchronization"]["post_deployment"] = True
                    except Exception:
                        job["synchronization"] = {"status": "unavailable", "guidance": "Inspect source synchronization before another release"}
                if job.get("rust_tests", {}).get("status") == "running":
                    job["rust_tests"] = {"status": "unconfirmed",
                        "summary": "Image build stopped before Rust test success was confirmed"}
                self._save(job)
        finally:
            if hasattr(self.runner, "job_context"):
                self.runner.job_context.job = None
                self.runner.job_context.event = None
            if deploy_locked:
                self.deploy_lock.release()
            with self.lock:
                self._mirror_batch(job)
                self.job_cancellations.pop(job["job_id"], None)
                if job.get("parallel_ui"):
                    self.active_ui = None
                    if job["status"] not in ("uncertain", "interrupted"):
                        self._maybe_start_parallel_ui()
                else:
                    self.active = None
                if job["status"] not in ("uncertain", "interrupted"):
                    self._start_next()

    def request_shutdown(self):
        with self.lock:
            self.stopping = True
            if hasattr(self.runner, "cancel"):
                self.runner.cancel.set()
            for job in self.jobs.values():
                if job["status"] in ("queued", "running"):
                    job["revoked"] = True
                    self._save(job)

    def shutdown(self):
        self.performance.shutdown()
        self.pool.shutdown(wait=True)
        self.ui_pool.shutdown(wait=True)
        if self.lease is not None:
            os.close(self.lease)
            self.lease = None


async def serve(broker, listener, bridge_listener=None):
    async def client(reader, writer, bridge=False):
        request = None
        try:
            sock = writer.get_extra_info("socket")
            _, uid, _ = struct.unpack("3i", sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if uid not in (CONFIG["service_uid"],CONFIG["app_uid"]) or (bridge and uid!=CONFIG["service_uid"]):
                raise ValueError("broker caller identity denied")
            raw = await asyncio.wait_for(reader.readline(), 5)
            if len(raw) > 2 * 1024 * 1024 or not raw.endswith(b"\n"):
                raise ValueError("bounded request required")
            request = json.loads(raw)
            if bridge:
                if set(request) - {"id", "name", "arguments"} or request.get("name") not in ("build_work", "file_list", "file_read", "file_search", "file_edit", "file_write", "v3_build_work", "v3_build_status", "v3_worker_job", "v3_source_prepare", "v3_source_apply", "v3_source_apply_prepared", "v3_source_receipt", "v3_source_read", "v3_source_list", "v3_source_read_batch", "v3_source_search", "v3_cargo_task"):
                    raise ValueError("bridge operation denied")
                request["workspace"] = BRIDGE_WORKSPACE
                request["trusted"] = {"user_id": "python-v2-admin", "session_id": BRIDGE_WORKSPACE,
                    "lease_until": time.time() + 4 * 3600,
                    "access_fingerprint": hashlib.sha256(b"python-v2-build-bridge").hexdigest(),
                    "bridge_caller": True}
            elif uid == CONFIG["service_uid"]:
                if set(request) - {"id", "name", "arguments"} or request.get("name") not in ("build_work", "file_list", "file_read", "file_search", "file_edit", "file_write", "v3_build_work", "v3_build_status", "v3_worker_job", "v3_source_prepare", "v3_source_apply", "v3_source_apply_prepared", "v3_source_receipt", "v3_source_read", "v3_source_list", "v3_source_read_batch", "v3_source_search", "v3_cargo_task", "build_service_maintenance", "workspace_seed", "source_review", "source_patch", "v3_workspace_status", "v3_workspace_action", "v3_build_adopt_release", "v3_build_cancel", "v3_build_leftovers", "v3_build_leftover_review", "v3_build_retention_clearance", "performance_work", "code_index_maintenance"):
                    raise ValueError("local caller operation denied")
                request["workspace"] = LOCAL_WORKSPACE
                request["trusted"] = {"user_id": "codex-local", "session_id": LOCAL_WORKSPACE,
                    "lease_until": time.time() + 4 * 3600,
                    "access_fingerprint": hashlib.sha256(b"local-codex-build").hexdigest(),
                    "local_caller": True}
            elif request.get("trusted", {}).get("local_caller") is not None or request.get("trusted", {}).get("bridge_caller") is not None:
                raise ValueError("local or bridge authority cannot be supplied by the app")
            result = await asyncio.to_thread(broker.handle, request)
            response = {"id": request.get("id"), "status": "success", "output": result}
        except Exception as error:
            response = {"id": request.get("id") if isinstance(request, dict) else None,
                        "status": "failed", "output": scrub(str(error), broker.runner.secrets)}
        writer.write(canonical(response) + b"\n")
        await writer.drain()
        writer.close()
        await writer.wait_closed()
    # Root bound this inherited descriptor through its retained directory FD.
    # That /proc/self/fd address is not valid in the unprivileged child, and
    # Python 3.13+ socket cleanup tries to stat it. The supervisor owns cleanup.
    options = {"cleanup_socket": False} if "cleanup_socket" in inspect.signature(asyncio.get_running_loop().create_unix_server).parameters else {}
    server = await asyncio.start_unix_server(
        client, sock=listener, limit=2 * 1024 * 1024 + 8192, **options)
    if bridge_listener is None:
        async with server:
            await server.serve_forever()
    else:
        bridge_server = await asyncio.start_unix_server(
            lambda reader, writer: client(reader, writer, True),
            sock=bridge_listener, limit=2 * 1024 * 1024 + 8192, **options)
        async with server, bridge_server:
            await asyncio.gather(server.serve_forever(), bridge_server.serve_forever())


def main():
    """Validate structured operator input and call the existing authenticated broker transport."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--listener-fd", type=int, required=True)
    parser.add_argument("--bridge-listener-fd", type=int)
    parser.add_argument("--snapshot-helper")
    parser.add_argument("--snapshot-socket")
    arguments = parser.parse_args()
    if os.geteuid() != CONFIG["service_uid"]:
        raise SystemExit("broker must run as its configured service identity with a protected inherited listener")
    listener = socket.socket(fileno=arguments.listener_fd)
    if listener.family != socket.AF_UNIX:
        raise SystemExit("Unix listener required")
    bridge_listener = socket.socket(fileno=arguments.bridge_listener_fd) if arguments.bridge_listener_fd is not None else None
    broker = Broker(runner=Runner(snapshot_helper=arguments.snapshot_helper, snapshot_socket=arguments.snapshot_socket))
    async def run():
        task = asyncio.create_task(serve(broker, listener, bridge_listener))
        async def notice_postprocess():
            while True:
                try:
                    await asyncio.to_thread(broker.reconcile_leftovers)
                except Exception:
                    print("Leftover notice reconciliation deferred", flush=True)
                try:
                    result = await asyncio.to_thread(broker.prune_check_caches)
                    if result.get("removed"):
                        print("Check-cache pruning: " + str(len(result["removed"])) + " folders removed", flush=True)
                except Exception:
                    # Fixed diagnostic only: Docker inspection contains private configuration.
                    print("Check-cache pruning deferred", flush=True)
                await asyncio.sleep(60)
        notices = asyncio.create_task(notice_postprocess())
        def stop():
            broker.request_shutdown()
            task.cancel()
        loop = asyncio.get_running_loop()
        for signum in (signal.SIGTERM, signal.SIGINT):
            loop.add_signal_handler(signum, stop)
        try:
            await task
        except asyncio.CancelledError:
            pass
        finally:
            notices.cancel()
            await asyncio.gather(notices, return_exceptions=True)
    try:
        asyncio.run(run())
    finally:
        broker.request_shutdown()
        broker.shutdown()


if __name__ == "__main__":
    main()
