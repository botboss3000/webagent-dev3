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
_scheduler_spec=importlib.util.spec_from_file_location("build_scheduler",_module_root/"scheduler.py")
scheduler_module=importlib.util.module_from_spec(_scheduler_spec);_scheduler_spec.loader.exec_module(scheduler_module)
resource_module=scheduler_module.sibling("resources")
executor_module=scheduler_module.sibling("executor")
engine_components=scheduler_module.sibling("engine_components")
index_queue_module=scheduler_module.sibling("index_queue")
index_coordinator_module=scheduler_module.sibling("index_coordinator")
ui_policy_module=scheduler_module.sibling("ui_policy")
ui_publication_module=scheduler_module.sibling("ui_publication")
review_store_module=scheduler_module.sibling("review_store")
review_policy_module=scheduler_module.sibling("review_policy")
post_deployment_module=scheduler_module.sibling("post_deployment")
packaging_cache_module=scheduler_module.sibling("packaging_cache")
image_reuse_module=scheduler_module.sibling("image_reuse")

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
    "v3/src", "v3/crates", "v3/ui", "v3/plugins/engine", "v3/plugins/abilities", "v3/plugins/gates", "v3/worker.py",
    "v3/sandbox.py", "v3/ability_plugin_worker.py", "v3/launch.py", "v3/promote.py", "v3/ui-baseline.json", "v3/tests",
    "v3/README.md", "v3/docs", "v3/deploy.sh", "v3/config", "v3/build-service",
    "v3/data/config/gates/message_visibility_v1.json",
    "v3/data/config/gates/closing_status_v1.json",
    "ui/shared/css/app1.css", "ui/shared/css/app2.css", "ui/shared/css/app3.css",
    "ui/shared/css/design-system.css", "ui/shared/css/index.css",
    "ui/main-panel/agents/agents.html", "ui/main-panel/agents/agents.css",
    "ui/vendor/fonts", "ui/vendor/lucide", "ui/favicon.svg")
UI_SOURCE_PREFIX = "v3/ui/"
def checked_git_modes(patch):
    """Render receipt POSIX modes as Git owner-execute modes for checked source patches."""
    def mode(line,prefix):
        value=line.removeprefix(prefix).strip()
        if not re.fullmatch(r"100[0-7]{3}",value):raise ValueError("Unsupported rollback file mode")
        return "100755"if int(value,8)&0o100 else"100644"
    lines=patch.splitlines(keepends=True);out=[];i=0
    while i<len(lines):
        line=lines[i]
        if line.startswith("old mode "):
            if i+1>=len(lines)or not lines[i+1].startswith("new mode "):raise ValueError("Paired rollback modes required")
            before=mode(line,"old mode ");after=mode(lines[i+1],"new mode ")
            if before!=after:out.extend(["old mode "+before+"\n","new mode "+after+"\n"])
            i+=2;continue
        if line.startswith("new mode "):raise ValueError("Unpaired rollback mode")
        if line.startswith("new file mode "):line="new file mode "+mode(line,"new file mode ")+"\n"
        elif line.startswith("deleted file mode "):line="deleted file mode "+mode(line,"deleted file mode ")+"\n"
        out.append(line);i+=1
    return "".join(out)

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

GIT_CONTROL_ROOT="v3/plugins/abilities/Administrator/git_control/"
GIT_CONTROL_TOOLS=["repository_access","git_tool","resolve_conflict","commit_and_push"]
GIT_CONTROL_MANIFEST={"version":1,"id":"git_control","protocol":1,"runtime":"python3","entrypoint":"transport.py","interface":1,
    "tools":GIT_CONTROL_TOOLS,"build":{"inputs":["adapter.py","transport.py","file_manager.py","askpass.py"],
    "dependencies":[],"toolchain":"image-python3","flags":["-I","-B"],
    "assets":["git_control.json","git_control.skill.md","tools"]}}
def git_control_source_path(path):
    if not path.startswith(GIT_CONTROL_ROOT):return False
    relative=path[len(GIT_CONTROL_ROOT):]
    return relative in {"adapter.py","transport.py","file_manager.py","askpass.py","test_runner.py","plugin.json",
        "git_control.json","git_control.skill.md"} or relative.startswith("tools/")and relative.endswith(".skill.md")
def validate_git_control_package(folder):
    folder=Path(folder)
    for name in ("plugin.json","adapter.py","transport.py","file_manager.py","askpass.py","test_runner.py","git_control.json","git_control.skill.md"):
        item=folder/name
        if item.is_symlink()or not item.is_file()or item.stat().st_size>2*1024*1024:raise ValueError("Incomplete Git Control package")
    if canonical(json.loads((folder/"plugin.json").read_bytes()))!=canonical(GIT_CONTROL_MANIFEST):
        raise ValueError("Incompatible Git Control contract")
    declaration=json.loads((folder/"git_control.json").read_bytes())
    if declaration.get("tools")!=GIT_CONTROL_TOOLS or declaration.get("kind")!="ability" or declaration.get("readiness")!="ready":
        raise ValueError("Incompatible Git Control catalog")
    for item in folder.rglob("*"):
        if item.is_symlink():raise ValueError("Git Control package symlink")
        if item.is_file()and item.suffix==".py":compile(item.read_bytes(),str(item),"exec")
        if item.is_file()and item.suffix==".md" and not item.read_text(encoding="utf-8").strip():
            raise ValueError("Empty Git Control guidance")

ABILITY_ROOT="v3/plugins/abilities/"
def ability_asset_path(path):
    if not path.startswith(ABILITY_ROOT):return False
    # Rust adapters and legacy Python helpers still have core/image consumers.
    return path.endswith((".json",".md")) or codebase_source_path(path) or diagnostics_source_path(path) or git_control_source_path(path)
def engine_component_source_path(path):
    """Recognize every registered implementation/contract family at each publication fence."""
    return path.startswith(("v3/plugins/engine/",
        "v3/crates/context-service/","v3/crates/context-assembly/","v3/crates/context-contracts/",
        "v3/crates/background-service/","v3/crates/background-dispatch/","v3/crates/background-contracts/",
        "v3/crates/boot-service/","v3/crates/boot-supervisor/","v3/crates/boot-contracts/",
        "v3/crates/assets-service/","v3/crates/context-assets/","v3/crates/assets-contracts/",
        "v3/crates/capability-service/","v3/crates/capability-context/","v3/crates/capability-contracts/",
        "v3/crates/provider-service/","v3/crates/provider-projection/","v3/crates/provider-contracts/",
        "v3/crates/engine-service/"))

def generic_package_path(path,blob):
    """Classify registered engine and isolated ability inputs; authority and actual core dependencies still fence publication."""
    if engine_components.package_path(path,release.entries(blob,context_digest)):return True
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
    if installed is not None:
        installed_manifest=json.loads(installed[1])
        if installed_manifest.get("dispatch")=="isolated" or (parts[1]=="git_control" and installed_manifest==GIT_CONTROL_MANIFEST):
            return False
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
MIN_SEQUENCE_FREE = 5 * 1024 * 1024 * 1024

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
            digest=hashlib.sha256();size=0
            with archive.extractfile(member)as body:
                while chunk:=body.read(1024*1024):digest.update(chunk);size+=len(chunk)
            if size!=member.size:raise ValueError("Incomplete snapshot input")
            entries[name] = (member.mode & 0o777, size, digest.hexdigest())
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
    allowed_dirs = {"crates", "src", "plugins", "tests", "ui", "docs", "config", "build-service"}
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
    """Own fixed host recipes, confined snapshots, measured execution and verified publication effects."""
    def __init__(self, source=SOURCE, snapshot_helper=None, snapshot_socket=None):
        self.source, self.snapshot_helper = Path(source), snapshot_helper
        self.source_config=CONFIG
        self.context_digest=context_digest
        self.secrets = []
        self.snapshot_socket = snapshot_socket or str(Path(CONFIG["runtime_root"])/"snapshot.sock")
        self.cancel = threading.Event()
        self.job_context = threading.local()

    def retain_image(self,image,job_id=None):
        """Pin exact dependencies before create/run and register outputs before later phases."""
        if getattr(self,"retention_enabled",False):
            return self.resource_request({"operation":"retention_pin","image":image,"job_id":job_id})
    def preflight_deploy_images(self,images,job_id):
        """Recheck both immutable outputs immediately before stopping either consumer."""
        if not isinstance(images,dict)or set(images)!={"api","loop"}or any(not isinstance(image,str)or not re.fullmatch(r"sha256:[a-f0-9]{64}",image)for image in images.values())or not isinstance(job_id,str)or not re.fullmatch(r"[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}",job_id):
            raise ValueError("Exact deployment image pair and job required")
        for role in ("api","loop"):
            actual=self.command(["docker","image","inspect","--format","{{.Id}}",images[role]],timeout=10,private_raw=True).strip()
            if actual!=images[role]:raise ValueError("Prepared deployment image unavailable")
        for role in ("api","loop"):self.retain_image(images[role],job_id)
        return dict(images)

    def cleanup_status(self):
        """Expose aggregate cleanup telemetry without other tasks' artifact identities."""
        if not getattr(self,"retention_enabled",False):return None
        result=self.resource_request({"operation":"retention_status"});last=result.get("last_cleanup",{})
        return {"free_bytes":result.get("free_bytes"),"usage_percent":last.get("usage_percent"),"removed_count":len(last.get("removed",[])),"protected_count":last.get("protected_count",len(last.get("protected",[]))),"target_reached":last.get("target_reached"),"deferred":last.get("deferred")}
    def disk_admission(self,name,job,phase,profile):
        """Reserve disk before a phase; caller-owned waits remain cancellable and visible."""
        if not getattr(self,"retention_enabled",False):return {"admitted":True}
        return self.resource_request({"operation":"disk_admit","container":name,"phase":phase,"profile":profile})
    def measure_disk_output(self,name,output):
        """Measure only the phase output footprint; bounded traversal never touches application state."""
        if not getattr(self,"retention_enabled",False):return
        total=0;count=0;deadline=time.monotonic()+1
        for parent,dirs,files in os.walk(output,followlinks=False):
            dirs[:]=[d for d in dirs if not (Path(parent)/d).is_symlink()]
            for entry in files:
                count+=1
                if count>50000 or time.monotonic()>deadline:return
                p=Path(parent)/entry
                if not p.is_symlink():total+=p.stat().st_blocks*512
        self.disk_finish(name,total)
    def disk_finish(self,name,measured_bytes=0):
        """Release exact phase demand; output footprints conservatively calibrate the same recipe."""
        if getattr(self,"retention_enabled",False):return self.resource_request({"operation":"disk_finish","container":name,"measured_bytes":measured_bytes})
    def command(self, argv, stdin=None, timeout=3600, output_file=None, private_raw=False):
        # Explicit arguments only; the model never supplies argv. Environment
        # has no application tokens, cookies or provider credentials.
        if getattr(self,"retention_enabled",False)and len(argv)>2 and argv[:2]in (["docker","create"],["docker","run"]):
            image=next((a for a in argv[2:]if isinstance(a,str)and(re.fullmatch(r"sha256:[a-f0-9]{64}",a)or "@sha256:"in a)),None)
            if image:
                job=getattr(self.job_context,"job",None)or{};self.retain_image(image,job.get("job_id"))
        env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": CONFIG["service_home"], "LANG": "C.UTF-8"}
        with subprocess.Popen(argv, cwd=self.source, env=env, stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True) as proc:
            output = bytearray()
            def feed():
                try:
                    if stdin is not None:
                        if hasattr(stdin,"read"):shutil.copyfileobj(stdin,proc.stdin,1024*1024)
                        else:proc.stdin.write(stdin)
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
                    if job and job.get("internal_index")and(job.get("revoked")or job.get("lease_until",0)<=time.time()):
                        raise PermissionError("Index scan authorization expired or revoked")
                    if self.cancel.is_set() or (event is not None and event.is_set()
                                                and not job.get("effects_started")):
                        error = PermissionError("host job stop requested; reconcile any interrupted effects")
                        # Private Docker inspection may contain values not yet
                        # learned by the scrubber. Never publish partial raw data.
                        error.log = "" if private_raw else scrub(bytes(output).decode("utf-8", "replace"), self.secrets)
                        raise error
                    phase=getattr(self.job_context,"phase_container",None)
                    paused=self.resources.elapsed_pause(phase)if phase and hasattr(self,"resources")else 0
                    if time.monotonic() > deadline+paused:
                        error=TimeoutError("fixed build task timeout")
                        error.log=""if private_raw else scrub(bytes(output).decode("utf-8","replace"),self.secrets)
                        raise error
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

    def resource_request(self,args):
        """Only the protected service peer can control an exact registered build container."""
        args=dict(args)
        if args.get("operation") not in {"stats","initialize","identity","memory_policy","retention_pin","disk_admit","disk_finish","retention_status"}:
            with self.resources.lock:
                row=self.resources.rows.get(args.get("container"))
                if not row or not row.get("container_id"):raise PermissionError("Registered compiler identity required")
                args["expected_id"]=row["container_id"]
        connection=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);connection.settimeout(15 if args.get("operation")in {"retention_pin","disk_admit","disk_finish","retention_status"}else 6)
        try:
            connection.connect(str(Path(CONFIG["runtime_root"])/"resource.sock"));connection.sendall(canonical(args)+b"\n")
            raw=bytearray()
            while not raw.endswith(b"\n")and len(raw)<65536:
                part=connection.recv(65536-len(raw))
                if not part:raise RuntimeError("Protected resource helper unavailable")
                raw.extend(part)
            result=json.loads(raw)
            if result.get("status")!="success":raise RuntimeError("Protected resource operation unavailable")
            return result["output"]
        finally:connection.close()
    def cleanup_phase(self,name):
        """Cancel/remove only the exact compiler container registered by this executor."""
        with self.resources.lock:
            row=self.resources.rows.get(name);identity=row.get("container_id")if row else None
        if row is None:raise PermissionError("Unregistered compiler cleanup denied")
        if not identity:
            recovered=self.resource_request({"operation":"identity","container":name})
            if recovered.get("missing"):return
            identity=recovered.get("container_id")
            if not identity or not re.fullmatch(r"[a-f0-9]{64}",identity):raise PermissionError("Compiler recovery identity unavailable")
            with self.resources.condition:self.resources.rows[name]["container_id"]=identity;self.resources.persist()
        if not re.fullmatch(r"[a-f0-9]{64}",identity):raise PermissionError("Compiler cleanup identity unavailable")
        connection=DockerHTTP("localhost",timeout=10)
        try:
            connection.request("GET","/containers/"+name+"/json");response=connection.getresponse();body=response.read(1024*1024)
            if response.status==404:return
            if response.status!=200:raise RuntimeError("Compiler cleanup unavailable")
            value=json.loads(body);labels=value.get("Config",{}).get("Labels",{})
            if value["Id"]!=identity or labels.get("webagent.scheduler.instance")!=CONFIG["instance"]:
                raise PermissionError("Compiler cleanup identity changed")
        finally:connection.close()
        connection=DockerHTTP("localhost",timeout=30)
        try:
            connection.request("DELETE","/containers/"+identity+"?force=true&v=true");response=connection.getresponse();response.read(4096)
            if response.status not in {204,404}:raise RuntimeError("Compiler removal unconfirmed")
        finally:connection.close()
    execution_application_blob=staticmethod(lambda blob:release.archive({n:v for n,v in release.entries(blob,context_digest).items()if not release.inspection_path(n)},context_digest))
    execution_entries=staticmethod(lambda blob:release.entries(blob,context_digest))
    execution_source_digest_blob=staticmethod(lambda blob:context_digest(blob)[0])
    execution_source_digest=staticmethod(lambda root:context_digest(release.snapshot(root,CONTEXT,context_digest))[0])

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
        paths=[destination,*destination.rglob("*")]
        # Validate the whole source-only tree before changing any permission bits.
        for path in paths:
            if path.is_symlink():raise PermissionError("Published asset symlink forbidden")
            if release.inspection_path(path.relative_to(destination).as_posix()):
                raise PermissionError("Private inspection data cannot be published")
            if not path.is_dir() and not path.is_file():raise PermissionError("Published asset is not regular source")
        for path in paths:
            if path.is_dir():path.chmod(0o755)
            else:path.chmod((path.stat().st_mode&0o111)|0o444)

    def stage_assets(self, blob, job_id, *, asset_root=None, base_release_id=None):
        scanner = Path(CONFIG["service_dir"])/"index-scanner.py"
        require_paired_index_parser(blob, scanner)
        with tarfile.open(fileobj=io.BytesIO(blob))as archive:
            try:engine=archive.extractfile("v3/plugins/abilities/Administrator/code_index/maintenance.py").read()
            except KeyError:raise ValueError("Candidate maintenance engine missing")from None
        if hashlib.sha256(engine).hexdigest()!=hashlib.sha256(Path(_maintenance_path).read_bytes()).hexdigest():raise ValueError("Install the reviewed maintenance engine while the broker is idle before publication")
        engine_outputs=self.build_engine_artifacts(job_id,source_root=Path((getattr(self.job_context,"job",None)or{}).get("release_root",ASSET_ROOT/"releases"/job_id)))
        destination = release.stage_assets(Path(asset_root)if asset_root is not None else ASSET_ROOT, job_id, blob, context_digest)
        engine_components.seal(destination,engine_outputs,ASSET_ROOT)
        if base_release_id is None:engine_components.seal_catalog(destination,job_id,ASSET_ROOT)
        else:engine_components.seal_catalog(destination,job_id,ASSET_ROOT,base_release_id=base_release_id)
        self.readable_assets(destination)
        job=getattr(self.job_context,"job",None)or{}
        if CONFIG.get("scheduler",{}).get("post_deployment_indexing",False)and job.get("publication_policy_version")==1:
            release.write_source_receipt(destination,job_id,context_digest(blob)[0])
            job.setdefault("post_deployment",{"index_status":"awaiting_deployment","attempts":0,"parser_service_dir":CONFIG["service_dir"]})
            getattr(self,"resource_progress",lambda _job:None)(job)
            return destination
        return self.index_assets(destination,blob,job_id,base_release_id=base_release_id)

    def index_assets(self,destination,blob,job_id,*,base_release_id=None):
        """Attach derived metadata once, after exact deployed source has been frozen."""
        destination=Path(destination)
        scanner=Path(CONFIG["service_dir"])/"index-scanner.py"
        source_receipt=release.verify_publication(destination,job_id,context_digest(blob)[0])if(destination/".code-index/source-release.json").is_file()else None
        job=getattr(self.job_context,"job",None)or{}
        pinned=job.get("post_deployment",{}).get("parser_service_dir")
        if pinned:
            package=Path(pinned)
            if package.parent!=Path(CONFIG["install_root"])/"releases"or not re.fullmatch(r"[a-f0-9]{64}",package.name):raise ValueError("Invalid pinned protected parser package")
            scanner=package/"index-scanner.py"
        require_paired_index_parser(blob,scanner)
        index = destination / ".code-index"
        for orphan in [*destination.glob(".index-stage-*"),*index.glob(".index-stage-*")]:
            uuid.UUID(orphan.name.removeprefix(".index-stage-"))
            if orphan.is_symlink() or not orphan.is_dir():
                raise ValueError("Invalid interrupted index staging directory")
            shutil.rmtree(orphan)
        if (index/"manifest.json").is_file():
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
        temporary = (index if source_receipt is not None else destination) / (".index-stage-" + str(uuid.uuid4()))
        temporary.mkdir(mode=0o755)
        try:
            image = (getattr(self.job_context,"job",None)or{}).get("images",{}).get("api")or self.inspect(TARGETS[0])["Image"]
            active=base_release_id if base_release_id is not None else self.current_asset_id()
            seed_root=ASSET_ROOT/"releases"/active if active else None
            seed=self.verified_index_seed(seed_root)
            if seed_root is not None and seed is None:
                job=getattr(self.job_context,"job",None)
                if job is not None:
                    job.setdefault("index_seed_rejections",[]).append({"release_id":active,"reason":"Invalid optional published index seed; full candidate verification required"})
                    getattr(self,"resource_progress",lambda _:None)(job)
            seed_mount=["-v",str(seed)+":/seed:ro"]if seed else []
            private_seeds=getattr(self,"index_seed_provider",lambda _job:[])(job_id)
            if len(private_seeds)>64:raise ValueError("Private seed mount limit exceeded")
            seed_names=[]
            for number,folder in enumerate(private_seeds):
                if Path(folder).is_symlink():raise ValueError("Private index seed symlink")
                name="/private-seed-"+str(number);seed_names.append(name)
                seed_mount.extend(["-v",str(folder)+":"+name+":ro"])
            program = ("import runpy,json; m=runpy.run_path('/scanner.py'); "
                       "m['build_published']('/source','/output'," + repr(job_id) +
                       "," + repr(context_digest(blob)[0]) + ",json.load(open('/maintenance.json')),seed="+("'/seed'"if seed else "None")+",private_seeds="+repr(seed_names)+")")
            self.index_command(["docker", "run", "--rm", "--network", "none",
                "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                "--user", str(CONFIG["service_uid"])+":"+str(CONFIG["service_gid"]), "--memory", "1g", "--pids-limit", "64",
                "--tmpfs", "/tmp:rw,size=64m,mode=1777",
                *seed_mount, "-v", str(input_path)+":/maintenance.json:ro",
                "-v", str(scanner)+":/scanner.py:ro",
                "-v", str(destination)+":/source:ro",
                "-v", str(temporary)+":/output:rw", "--entrypoint", "python3",
                image, "-B", "-c", program], timeout=120)
            fd = os.open(temporary, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:os.fsync(fd)
            finally:os.close(fd)
            if source_receipt is not None:release.verify_publication(destination,job_id,context_digest(blob)[0])
            if source_receipt is not None:
                # The existing metadata directory contains only the source receipt.
                # Publish the immutable database first, then its manifest as commit.
                os.replace(temporary/"index.sqlite",index/"index.sqlite")
                os.replace(temporary/"manifest.json",index/"manifest.json")
                fd=os.open(index,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
                try:os.fsync(fd)
                finally:os.close(fd)
            else:os.rename(temporary, index)
            fd = os.open(destination, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:os.fsync(fd)
            finally:os.close(fd)
            release.verify_index(destination, job_id, context_digest(blob)[0])
            self.readable_assets(destination)
        finally:
            if temporary.exists():
                shutil.rmtree(temporary)
        return destination

    def index_command(self,arguments,timeout=120,kind="index"):
        """Run the protected parser in a measured, attributable, bounded index container."""
        resources=getattr(self,"resources",None)
        job=getattr(self.job_context,"job",None)
        if not CONFIG.get("scheduler",{}).get("component_indexing",False)or resources is None or not resources.enabled or not job:
            return self.command(arguments,timeout=timeout)
        if arguments[:3]!=["docker","run","--rm"]or arguments.count("--entrypoint")!=1:raise ValueError("Fixed index container recipe required")
        entry=arguments.index("--entrypoint")
        if kind not in {"index","ui_test"}or arguments[entry+1]!=("python3"if kind=="index"else"node"):raise ValueError("Fixed protected verification recipe required")
        phase="index_scan"if kind=="index"else"ui_test"
        image=arguments[entry+2];program=arguments[entry+3:]
        name="webagent-build-"+CONFIG["instance"]+"-"+uuid.UUID(job["job_id"]).hex+"-"+phase+"-"+uuid.uuid4().hex[:8]
        profile=kind+":"+hashlib.sha256((Path(CONFIG["service_dir"])/("index-scanner.py"if kind=="index"else"ui_policy.py")).read_bytes()).hexdigest()+":"+image
        deadline=time.monotonic()+timeout
        resources.enter(name,job,phase,profile,kind,timeout=timeout)
        identity=None;success=False;oom=False
        try:
            create=["docker","create","--name",name,"--cgroup-parent","webagent-build-"+CONFIG["instance"]+".slice",
                "--label","webagent.scheduler.instance="+CONFIG["instance"],"--label","webagent.scheduler.job="+uuid.UUID(job["job_id"]).hex,
                "--label","webagent.scheduler.phase="+phase,"--memory-swap","1g",*arguments[3:entry],"--entrypoint","sleep",image,"infinity"]
            self.command(create,timeout=30,private_raw=True)
            value=self.resource_request({"operation":"identity","container":name})
            candidate=value.get("container_id")
            if not isinstance(candidate,str)or not re.fullmatch(r"[a-f0-9]{64}",candidate)or value.get("state")!="created"or value.get("phase")!=phase:raise ValueError("Index container identity invalid")
            identity=candidate
            self.command(["docker","start",name],timeout=30)
            resources.attached(name,identity);self.job_context.phase_container=name
            result=self.command(["docker","exec",name,arguments[entry+1],*(["-I"]if kind=="index"else[]),*program],timeout=max(1,deadline-time.monotonic()))
            counters=self.resource_request({"operation":"stats","container":name})
            with resources.lock:resources.rows[name]["metrics"]=counters
            oom=counters.get("oom_kills",0)>0 or counters.get("oom_events",0)>0
            if oom:raise RuntimeError("Index scan exceeded memory budget")
            success=True;return result
        finally:
            self.job_context.phase_container=None
            self.executor.finish_phase(name,identity,success and not oom)

    def verified_index_seed(self,root):
        """Discard an invalid optional cache; exact candidate verification remains mandatory."""
        if root is None:return None
        try:maintenance_engine.load_scanner().read_published(root)
        except (OSError,ValueError,sqlite3.Error):return None
        return root

    def maintenance_inventory(self,root,scope,folder):
        """Scan with the protected engine in the same API-image parser environment as publication."""
        folder=Path(folder);folder.mkdir(mode=0o700,parents=True,exist_ok=True)
        invocation=folder/("scan-"+uuid.uuid4().hex);invocation.mkdir(mode=0o700)
        # Copy a complete private seed into this attempt; no scanner shares a writable DB.
        for name in ("index.sqlite","seed.json"):
            source=folder/name
            if source.is_symlink():raise ValueError("Maintenance seed symlink rejected")
            if source.is_file():
                if source.stat().st_size>256*1024*1024:raise ValueError("Maintenance seed exceeds bound")
                shutil.copyfile(source,invocation/name)
        scanner=Path(CONFIG["service_dir"])/"index-scanner.py"
        engine=Path(CONFIG["service_dir"])/"index-maintenance.py"
        image=self.inspect(TARGETS[0])["Image"]
        active=self.current_asset_id();published_root=ASSET_ROOT/"releases"/active if active else None
        published=self.verified_index_seed(published_root)
        # The scanner binds the directory basename to the actual release ID.
        published_target="/published/"+active if published else None
        published_mount=["-v",str(published)+":"+published_target+":ro"]if published else []
        program=("import runpy,json; m=runpy.run_path('/index-maintenance.py'); "
                 "value=m['source_inventory'](m['load_scanner'](),'/source',"+repr(scope)+",'/output',published_seed="+(repr(published_target)if published else "None")+"); "
                 "json.dump(value,open('/output/inventory.json','w'))")
        try:
            self.index_command(["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL",
                "--security-opt","no-new-privileges","--user",str(CONFIG["service_uid"])+":"+str(CONFIG["service_gid"]),
                "--memory","1g","--pids-limit","64","--tmpfs","/tmp:rw,size=64m,mode=1777",
                *published_mount,"-v",str(root)+":/source:ro","-v",str(invocation)+":/output:rw",
                "-v",str(scanner)+":/index-scanner.py:ro","-v",str(engine)+":/index-maintenance.py:ro",
                "--entrypoint","python3",image,"-I","-B","-c",program],timeout=120)
            result=invocation/"inventory.json"
            if result.is_symlink()or result.stat().st_size>64*1024*1024:raise ValueError("Maintenance inventory exceeds bound")
            files,units,scan=json.loads(result.read_bytes());scan["parser_environment"]=image
            if published_root is not None and published is None:scan["rejected_published_seed"]=active
            os.replace(invocation/"index.sqlite",folder/"index.sqlite")
            os.replace(invocation/"seed.json",folder/"seed.json")
            shutil.rmtree(invocation)
            return files,units,scan
        except BaseException:
            shutil.rmtree(invocation);raise

    def plugin_artifact_verified(self,artifact,entry,key):
        """Reuse a compiler result only when its immutable input key and stored output digest agree."""
        binary=artifact/entry;receipt=artifact/"compiler-receipt.json"
        if not binary.is_file()or not receipt.is_file():return False
        if binary.is_symlink()or receipt.is_symlink()or binary.stat().st_nlink!=1 or binary.stat().st_size>32*1024*1024:
            raise ValueError("Plugin cache identity invalid")
        value=json.loads(receipt.read_bytes())
        if value!={"input_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"recipe":executor_module.RUST+":release:locked:offline:debug0"}:
            raise ValueError("Plugin cache output differs from verified compilation")
        return True

    @contextlib.contextmanager
    def plugin_build_lock(self,key):
        """Serialize one artifact key while unrelated package builds retain independent execution."""
        folder=STATE/"plugin-build-locks";folder.mkdir(mode=0o700,parents=True,exist_ok=True)
        fd=os.open(folder/key,os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW,0o600)
        try:
            deadline=time.monotonic()+3600
            while True:
                try:fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);break
                except BlockingIOError:
                    job=getattr(self.job_context,"job",None);event=getattr(self.job_context,"event",None)
                    if self.cancel.is_set()or(event is not None and event.is_set())or(job and(job.get("revoked")or job.get("lease_until",0)<=time.time())):
                        raise PermissionError("Artifact admission cancelled")
                    if time.monotonic()>deadline:raise TimeoutError("Artifact lock timeout")
                    time.sleep(.1)
            yield
        finally:os.close(fd)

    def build_engine_artifacts(self,job_id,source_root=None):
        """Use the registered service recipe and seal only publication generations."""
        self.inspect_api_name=TARGETS[0]
        root=ASSET_ROOT/"releases"/job_id if source_root is None else Path(source_root)
        if source_root is not None and (root/engine_components.ROOT/engine_components.ID/"component.json").exists():
            active=getattr(self.job_context,"job",None)
            if not active or active["job_id"]!=job_id or root!=Path(active["release_root"])or root.resolve()!=root:raise ValueError("Owned frozen engine compiler source required")
        return engine_components.build(self,job_id,root,STATE,ASSET_ROOT,executor_module,scheduler_module)

    def build_ability_artifacts(self,job_id,source_root=None):
        """Compile fixed declared inputs before publication; revalidation reuses verified immutable artifacts."""
        root=ASSET_ROOT/"releases"/job_id if source_root is None else Path(source_root)
        if source_root is not None:
            active=getattr(self.job_context,"job",None)
            if not active or active["job_id"]!=job_id or root!=Path(active["release_root"])or root.resolve()!=root:
                raise ValueError("Owned frozen compiler source required")
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
            with self.plugin_build_lock(key):
                artifact=ASSET_ROOT/"artifacts"/key;binary=artifact/entry
                if self.plugin_artifact_verified(artifact,entry,key):
                    outputs.append({"package":manifest["id"],"build_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"reused":True});continue
                legacy=ASSET_ROOT/"artifacts"/legacy_key/entry
                if self.plugin_artifact_verified(legacy.parent,entry,legacy_key):
                    artifact.mkdir(parents=True,exist_ok=True);temporary=artifact/("."+entry+".new")
                    shutil.copyfile(legacy,temporary);temporary.chmod(0o555);os.replace(temporary,binary)
                    scheduler_module.Scheduler.write_record(artifact/"compiler-receipt.json",{"input_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"recipe":executor_module.RUST+":release:locked:offline:debug0"})
                    outputs.append({"package":package.name,"build_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"reused":True,"migrated_from":legacy_key})
                    continue
                cache=STATE/"plugin-builds"/key;cache.mkdir(parents=True,exist_ok=True)
                # Only the declared application package enters the builder. No credentials,
                # state, workspaces, control sockets or network enter compilation.
                source=cache/"source";source.mkdir(exist_ok=True)
                for name in names:
                    target=source/name;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes((package/name).read_bytes())
                controlled=getattr(getattr(self,"resources",None),"enabled",False)
                if controlled:
                    active=getattr(self.job_context,"job",None)
                    if active is None or active["job_id"]!=job_id:raise ValueError("Owned attributable plugin job required")
                    # Reuse only registry bytes bound to a completed engine compiler receipt.
                    registry=self.executor.provision_native_registry(cache)
                    self.executor.phase(active,"plugin_test","plugin",source,cache,
                        ["cargo","test","--locked","--offline","--","--test-threads=1"],
                        "plugin:rust1.90:"+manifest["id"]+":tests",600)
                    self.executor.phase(active,"plugin_compile","plugin",source,cache,
                        ["cargo","build","--release","--locked","--offline","--bin",entry],
                        "plugin:rust1.90:"+manifest["id"],600)
                    if self.executor.artifact_hashes(cache/"cargo/registry")!=registry["hashes"]:
                        raise ValueError("Native compilation changed verified dependency registry")
                else:
                    self.command(["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges",
                        "--user",str(CONFIG["service_uid"])+":"+str(CONFIG["service_gid"]),"--pids-limit","128","--memory","2g","--cpus","1",
                        "--tmpfs","/tmp:rw,size=128m,mode=1777","--mount","type=bind,src="+str(source)+",dst=/package,readonly",
                        "--mount","type=bind,src="+str(cache)+",dst=/output","-w","/package",
                        "-e","CARGO_HOME=/output/cargo","-e","CARGO_TARGET_DIR=/output/target","-e","CARGO_BUILD_JOBS=1","-e","CARGO_PROFILE_RELEASE_DEBUG=0",
                        "rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f",
                        "cargo","build","--release","--locked","--offline","--bin",entry],timeout=600)
                if any(hashlib.sha256((source/name).read_bytes()).hexdigest()!=inputs[name]for name in names):
                    raise ValueError("Plugin build inputs changed")
                output=cache/"target/release"/entry
                if output.is_symlink()or not output.is_file()or output.stat().st_size>32*1024*1024:raise ValueError("Invalid plugin artifact")
                artifact.mkdir(parents=True,exist_ok=True);temporary=artifact/("."+entry+".new")
                shutil.copyfile(output,temporary);temporary.chmod(0o555);os.replace(temporary,binary)
                scheduler_module.Scheduler.write_record(artifact/"compiler-receipt.json",{"input_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"recipe":executor_module.RUST+":release:locked:offline:debug0"})
                outputs.append({"package":manifest["id"],"build_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"reused":False})
        return outputs

    # @code-index {"version":1,"feature":"ui-verification-runtime","aliases":["ui verification runtime"],"purpose":"Run selected fixtures in an isolated measured browser worker; reuse exact evidence. Config: build-service/scheduler-defaults.json -> ui_test_image, component_indexing.","span_lines":70,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/ui_test_image","/component_indexing"]}],"opportunities":[]}}
    def retain_ui_test_runtime(self,image,recipe_hash):
        """Keep the reviewed test image available without application mounts or browser work."""
        if not re.fullmatch(r"sha256:[a-f0-9]{64}",image)or not re.fullmatch(r"[a-f0-9]{64}",recipe_hash):raise ValueError("Pinned UI runtime identity required")
        actual=self.command(["docker","image","inspect","--format","{{.Id}}",image],timeout=10,private_raw=True).strip()
        if not re.fullmatch(r"sha256:[a-f0-9]{64}",actual):raise ValueError("UI runtime image unavailable")
        name="webagent-"+CONFIG["instance"]+"-ui-test-retain-"+hashlib.sha256(canonical([actual,recipe_hash])).hexdigest()[:16]
        labels={"webagent.retention.instance":CONFIG["instance"],"webagent.retention.kind":"ui-test-runtime","webagent.retention.image":actual,"webagent.retention.recipe":recipe_hash}
        def inspect_holder():
            """Inspect only this host-derived holder; application inspection stays confined."""
            values=json.loads(self.command(["docker","inspect",name],timeout=30,private_raw=True))
            if not isinstance(values,list)or len(values)!=1 or not isinstance(values[0],dict):raise ValueError("UI runtime holder inspection invalid")
            return values[0]
        reused=True
        try:value=inspect_holder()
        except RuntimeError:
            pins=self.command(["docker","ps","-a","--no-trunc","--filter","label=webagent.retention.instance="+CONFIG["instance"],"--filter","label=webagent.retention.kind=ui-test-runtime","--format","{{.ID}}"],timeout=10,private_raw=True).split()
            if len(pins)>=2 or any(not re.fullmatch(r"[a-f0-9]{64}",p)for p in pins):raise ValueError("Retire an unused prior UI runtime holder before retaining a third version")
            reused=False
            args=["docker","create","--name",name,"--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges","--restart","unless-stopped","--memory","32m","--memory-swap","32m","--cpus","0.01","--pids-limit","8","--user",str(CONFIG["app_uid"])+":"+str(CONFIG["app_gid"])]
            for key,entry in labels.items():args.extend(["--label",key+"="+entry])
            self.command([*args,"--entrypoint","sleep",actual,"infinity"],timeout=30,private_raw=True);value=inspect_holder()
        policy=value.get("HostConfig",{});settings=value.get("Config",{})
        if(value.get("Image")!=actual or any(settings.get("Labels",{}).get(k)!=v for k,v in labels.items())or value.get("Mounts")or settings.get("Entrypoint")!=["sleep"]or settings.get("Cmd")!=["infinity"]or settings.get("User")!=str(CONFIG["app_uid"])+":"+str(CONFIG["app_gid"])or settings.get("Healthcheck")or policy.get("NetworkMode")!="none"or not policy.get("ReadonlyRootfs")or policy.get("Privileged")or policy.get("CapAdd")or policy.get("CapDrop")!=["ALL"]or not any(s in {"no-new-privileges","no-new-privileges:true"}for s in policy.get("SecurityOpt",[]))or policy.get("PortBindings")or policy.get("PublishAllPorts")or policy.get("Memory")!=33554432 or policy.get("MemorySwap")!=33554432 or policy.get("NanoCpus")!=10000000 or policy.get("PidsLimit")!=8 or policy.get("RestartPolicy",{}).get("Name")!="unless-stopped"):
            raise ValueError("UI runtime retention identity or budget changed")
        if value.get("State",{}).get("Status")in {"created","exited"}:
            self.command(["docker","start",name],timeout=30,private_raw=True);value=inspect_holder()
        if value.get("State",{}).get("Status")!="running"or value.get("State",{}).get("Pid",0)<=0:raise ValueError("UI runtime retention lease is not running")
        return {"container":name,"container_id":value["Id"],"image":actual,"recipe_sha256":recipe_hash,"reused":reused,"memory_max":33554432,"swap_max":0,"cpu_limit":0.01,"pids_limit":8}

    def prepare_ui_test_runtime(self,job):
        """Build only the fixed protected ancillary test recipe in the measured packager."""
        recipe=Path(CONFIG["service_dir"])/"ui-test.Dockerfile"
        if recipe.is_symlink()or not recipe.is_file():raise ValueError("Protected UI verification recipe unavailable")
        body=recipe.read_bytes();root=STATE/"checks"/job["job_id"]/"ui-test-runtime"
        root.mkdir(mode=0o700,parents=True,exist_ok=False);(root/"Dockerfile").write_bytes(body)
        image_file=root/"image.id";tag="webagent-v3-ui-tests:"+job["job_id"]
        # Pulling the fixed packager image does not compile application source.
        self.command(["docker","pull",executor_module.BUILDKIT],timeout=180)
        with self.executor.packager(job,"generic")as builder:
            self.command(["docker","buildx","build","--builder",builder,"--load","--progress","plain","--tag",tag,"--iidfile",str(image_file),str(root)],timeout=600)
        image=image_file.read_text().strip()
        if not re.fullmatch(r"sha256:[a-f0-9]{64}",image):raise ValueError("UI test image identity invalid")
        if recipe.read_bytes()!=body:raise ValueError("Protected UI test recipe changed")
        result={"image":image,"recipe_sha256":hashlib.sha256(body).hexdigest(),"application_images_changed":False}
        # Only the host's explicit operator job requests a durable availability lease.
        if job.get("retain_ui_runtime"):
            result["retention_pin"]=self.retain_ui_test_runtime(image,result["recipe_sha256"])
            result["image"]=result["retention_pin"]["image"]
        return result

    def validate_selected_tests(self,job_id,root,evidence):
        """Execute dependency-selected frontend fixtures in one isolated browser-capable worker."""
        tests=evidence["tests"]
        image=CONFIG.get("scheduler",{}).get("ui_test_image")
        recipe="node:experimental-vm-modules:test-concurrency=1:network-none:v1"
        binding=hashlib.sha256(canonical({"dependencies":evidence["sha256"],"runtime_images":evidence.get("runtime_images",{}),"image":image,"recipe":recipe})).hexdigest()
        job=getattr(self.job_context,"job",None)
        if job and job.get("ui_test_evidence",{}).get("binding")==binding:
            return {**job["ui_test_evidence"],"reused":True}
        if tests:
            if not isinstance(image,str)or not re.fullmatch(r"sha256:[a-f0-9]{64}",image):raise ValueError("Protected pinned UI test runtime required")
            if len(tests)>256 or any(not p.startswith("v3/ui/")or ".."in Path(p).parts or not(root/p).is_file()for p in tests):raise ValueError("Bounded selected frontend fixtures required")
            # No application credentials, mounts, authentication state or external network.
            self.index_command(["docker","run","--rm","--network","none","--init","--read-only","--cap-drop","ALL",
                "--security-opt","no-new-privileges","--user",str(CONFIG["app_uid"])+":"+str(CONFIG["app_gid"]),
                "--memory","1g","--pids-limit","256","--cpus","1","--shm-size","256m",
                "--tmpfs","/tmp:rw,size=512m,mode=1777","--env","HOME=/tmp","--env","NODE_PATH=/opt/ui-tests/node_modules",
                "--env","PLAYWRIGHT_MODULE=/opt/ui-tests/playwright-entry.cjs",
                "--env","CHROMIUM_PATH=/opt/ui-tests/chromium","--env","BROWSER_EXECUTABLE=/opt/ui-tests/chromium",
                "--mount","type=bind,src="+str(root)+",dst=/candidate,readonly","--workdir","/candidate/v3",
                "--entrypoint","node",image,"--experimental-vm-modules","--test","--test-concurrency=1",*["/candidate/"+p for p in tests]],timeout=300,kind="ui_test")
        return {"binding":binding,"image":image,"recipe":recipe,"tests":tests,"status":"passed"if tests else"not_applicable","reused":False}

    def validate_ui(self, job_id, changed, *, root=None):
        """Validate complete candidate assets and registered packages against the owning runtime before activation."""
        root = Path(root)if root is not None else ASSET_ROOT / "releases" / job_id
        (ASSET_ROOT/"artifacts").mkdir(parents=True,exist_ok=True)
        active=getattr(getattr(self,"job_context",None),"job",None)
        # Compilation consumes the owned frozen source; the sealed staging tree is
        # validation input only and must not become a compiler authority bypass.
        compiler_root=Path(active["release_root"])if active and active.get("job_id")==job_id and active.get("release_root")else None
        artifacts=self.build_ability_artifacts(job_id,source_root=compiler_root)
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
        if any(git_control_source_path(name) for name in changed):
            validate_git_control_package(root / GIT_CONTROL_ROOT)
            image=(active["images"]["api"] if active is not None and active.get("operation")=="build_deploy" else self.inspect(TARGETS[0])["Image"])
            self.command(["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges",
                "--user",str(CONFIG["worker_uid"])+":"+str(CONFIG["worker_gid"]),"--pids-limit","64","--memory","256m",
                "--tmpfs","/tmp:rw,size=64m,mode=1777","--mount","type=bind,src="+str(root)+",dst=/candidate,readonly",
                "--entrypoint","python3",image,"-I","-B","/candidate/v3/plugins/abilities/Administrator/git_control/test_runner.py"],timeout=30)
        if any(diagnostics_source_path(name) for name in changed):
            validate_diagnostics_package(root / DIAGNOSTICS_ROOT)
            if (active is None or active.get("operation")!="build_deploy") and not self.diagnostics_hot_swappable():
                raise ValueError("Installed worker lacks native Diagnostics package support")
            image=(active["images"]["api"] if active is not None and active.get("operation")=="build_deploy" else self.inspect(TARGETS[0])["Image"])
            self.command(["docker","run","--rm","--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges",
                "--user",str(CONFIG["worker_uid"])+":"+str(CONFIG["worker_gid"]),"--pids-limit","64","--memory","256m",
                "--tmpfs","/tmp:rw,size=64m,mode=1777","--mount","type=bind,src="+str(root / DIAGNOSTICS_ROOT)+",dst=/package,readonly",
                "--entrypoint","python3",image,"-I","-B","/package/test_runner.py"],timeout=60)
        if any(name.startswith(ABILITY_ROOT) or engine_component_source_path(name) or name.startswith("v3/config/") or name=="v3/src/agent_loop/tools/administrator-schemas.json" for name in changed):
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
        """Preserve normalized installed compiler inputs, including embedded documentation."""
        import posixpath
        raw=self.command(["docker","exec",TARGETS[0],"cat","/usr/local/lib/webagent/core-inputs.txt"],timeout=10,private_raw=True)
        paths=set()
        for path in raw.splitlines():
            if path.startswith("/build/"):path=path[len("/build/"):]
            normalized=posixpath.normpath(path)
            if normalized.startswith(("src/","crates/","plugins/","config/","docs/")):paths.add("v3/"+normalized)
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

    def boot_adoption(self):
        """Read the protected local boot host over its authenticated bounded Unix socket."""
        path=Path(CONFIG["install_root"]).parent/"boot-supervisor/ui.sock"
        if not path.exists():return None
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as peer:
            peer.settimeout(2);peer.connect(str(path))
            _,uid,_=struct.unpack("3i",peer.getsockopt(socket.SOL_SOCKET,socket.SO_PEERCRED,12))
            if uid!=0:raise ValueError("Boot authority peer changed")
            peer.sendall(b'{"operation":"status"}\n')
            raw=peer.makefile("rb").readline(65537)
        if len(raw)>65536 or not raw.endswith(b"\n"):raise ValueError("Boot adoption bounds")
        reply=json.loads(raw)
        if reply.get("status")!="success":raise ValueError("Boot adoption unavailable")
        value=reply["output"]
        if value.get("version")!=1 or value.get("mode")!="live":raise ValueError("Boot mode unavailable")
        expected=hashlib.sha256((Path(CONFIG["service_dir"])/"boot_host.py").read_bytes()).hexdigest()
        if value.get("host_sha256")!=expected or type(value.get("required"))is not bool:
            raise ValueError("Boot host adoption identity")
        if value["required"] and value.get("component_paused"):
            raise ValueError("Required boot service unavailable")
        return value

    def engine_execution_modes(self,expected_revision=None):
        """Read actual compiled execution modes from both protected engine adoption receipts."""
        program=("import json,pathlib; root=pathlib.Path('/state/publication'); "
            "print(json.dumps({r:json.loads((root/('engine-'+r+'.json')).read_text()) for r in ['api','loop']}))")
        value=json.loads(self.command(["docker","exec","--user",str(CONFIG["app_uid"]),TARGETS[0],"python3","-I","-B","-c",program],timeout=10,private_raw=True))
        if any(value.get(role,{}).get("mode")not in {"live","linked"}or(expected_revision is not None and value[role].get("revision")!=expected_revision)for role in ("api","loop")):
            raise ValueError("Actual engine execution mode or adoption unavailable")
        modes={role:value[role]["mode"]for role in ("api","loop")}
        boot=self.boot_adoption()
        if boot is not None:
            if expected_revision is not None and boot["revision"]!=expected_revision:raise ValueError("Boot adoption stale")
            modes["boot"]=boot["mode"]
        return modes
    def engine_hot_swappable(self):
        """Linked, missing or unknown modes require a coordinated core release, never service substitution."""
        try:return all(mode=="live"for mode in self.engine_execution_modes(self.current_asset_id()).values())
        except (OSError,ValueError,RuntimeError,KeyError):return False

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
                if response.status==200 and all(receipt[r]["revision"]==job_id and receipt[r]["catalog_version"]==ready["catalog_version"] and receipt[r].get("engine_revision")==job_id for r in ("api","loop")):
                    boot=self.boot_adoption()
                    if boot is not None and boot["revision"]!=job_id:continue
                    result={r:{"revision":job_id,"catalog_version":ready["catalog_version"],"engine_revision":job_id}for r in ("api","loop")}
                    if boot is not None:result["boot"]={"revision":job_id,"engine_revision":job_id,"mode":boot["mode"],"component_paused":boot["component_paused"]}
                    return result
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
        for name, route in paths:
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
        # Honor the protected free-space floor here too. Individual phases still
        # reserve their additional measured/compiler/package demand separately.
        minimum = CONFIG.get("scheduler", {}).get("disk_min_free_bytes", MIN_SEQUENCE_FREE)
        return self.check_space(minimum)

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
            argv=["docker","run","--rm","--name",container,"--network","none","--read-only","--cap-drop","ALL","--security-opt","no-new-privileges","--user",str(CONFIG["worker_uid"])+":"+str(CONFIG["app_gid"]),"--pids-limit","128","--memory","1g","--cpus","1","--tmpfs","/tmp:rw,size=64m,mode=1777","--tmpfs","/run:rw,size=16m,mode=1777","--mount","type=volume,src="+base_mount+",dst=/workspaces/"+base.name+",volume-nocopy","--mount","type=volume,src="+selected_mount+",dst=/opt/v3/assets/releases/"+selected.name+",readonly,volume-nocopy","--mount","type=bind,src="+str(self.source_config["service_dir"])+",dst=/opt/v3/build-service,readonly","--env","V3_CODE_TEMPLATE=/opt/v3/assets/releases/"+selected.name+"/v3","--env","V3_BUILD_EXECUTOR=1","--entrypoint","python3",image,"-I","/opt/v3/build-service/workspace_executor.py",spec]
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
        if hasattr(self,"resources")and self.resources.enabled:
            job=getattr(self.job_context,"job",None)
            if not job or job["job_id"]!=job_id:raise PermissionError("Bound compiler job required")
            if job.get("image_receipts",{}).get(role):
                image=image_reuse_module.reuse(self,job,blob,role)
                self.retain_image(image,job_id)
                return image,"Verified existing "+role+" image reused; no packaging or package download.\n"
            if job.get("legacy_image_proof"):
                cache=getattr(self,"package_cache",None)
                if cache is None or not cache.enabled:raise ValueError("Verified dependency cache required before legacy image recovery")
                prior=cache.broker.jobs[job["legacy_image_proof"]["source_job_id"]]
                proof=image_reuse_module.legacy_pair(self,prior,blob)
                if proof!=job["legacy_image_proof"]:raise ValueError("Legacy image evidence changed")
                self.retain_image(proof["images"][role],job_id)
                return proof["images"][role],"Verified prior "+role+" image and compiler receipt reused; no packaging or package download.\n"
            preflight=self.executor.preflight_packaging(job,blob)
            preflight["recipe"]=image_reuse_module.recipe_receipt(self)
            self.resource_progress(job)
            if job.get("packaging_resume"):
                artifacts=image_reuse_module.packaging_artifacts(self,job,blob)
                compiler_log="Verified frozen compiler artifacts reused for packaging; compilation and tests were not repeated.\n"
            else:
                artifacts,compiler_log=self.executor.engine(job,blob,lambda root,data:release.extract(root,data,context_digest),lambda data:context_digest(data)[0])
            job["compiler_checkpoint"]={"source_sha256":image_reuse_module.digest(blob),"compiler_receipt_sha256":image_reuse_module.source_receipt(self,job,blob),"recipe":image_reuse_module.recipe_receipt(self)}
            self.resource_progress(job)
            packaging_path=Path(CONFIG["state_root"])/"execution"/job_id/("package-"+role+".tar")
            packaging_path.parent.mkdir(parents=True,exist_ok=True)
            if packaging_path.exists():raise ValueError("Packaging context requires reconciliation")
            try:
                cache=getattr(self,"package_cache",None)
                if cache is not None and cache.enabled:
                    cache.ready(blob)
                    self.job_context.source_blob=blob
                transform=packaging_cache_module.package_dockerfile if cache is not None and cache.enabled else None
                packaging=self.executor.package(blob,artifacts,lambda data:release.entries(data,context_digest),role=role,destination=packaging_path,recipe_transform=transform)
                image,package_log=self._build_packaged(packaging,job_id,role)
                with packaging_path.open("rb") as stream:
                    context_sha256=hashlib.file_digest(stream,"sha256").hexdigest()
            finally:packaging_path.unlink(missing_ok=True)
            self.retain_image(image,job_id)
            checkpoint=image_reuse_module.record(self,job,blob,role,image,context_sha256)
            job.setdefault("image_receipts",{})[role]=checkpoint
            job.setdefault("images",{})[role]=image
            self.resource_progress(job)
            return image,(compiler_log if role=="api"else"")+package_log
        tag = TARGETS[1 if role=="loop" else 0]+":job-"+job_id
        log = self.command(["docker", "buildx", "build", "--builder", "default", "--load",
                            "--progress=plain", "--target", role, "-f", "v3/Dockerfile",
                            "-t", tag, "-"], stdin=blob)
        image = self.command(["docker", "image", "inspect", "--format", "{{.Id}}", tag], timeout=30).strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
            raise ValueError("invalid image identity")
        self.retain_image(image,job_id)
        return image, log

    def _build_packaged(self,blob,job_id,role):
        """Package already verified artifacts; BuildKit does not execute Cargo in this context."""
        tag="webagent-v3-"+role+"-"+job_id
        job=getattr(self.job_context,"job",None)
        if job is None or job["job_id"]!=job_id:raise ValueError("Owned packaging job required")
        cache=getattr(self,"package_cache",None)
        inputs=cache.package_inputs(job,self.job_context.source_blob) if cache is not None and cache.enabled else []
        with self.executor.packager(job,role=role)as builder:
            with Path(blob).open("rb")as stream:
                log=self.command(["docker","buildx","build","--builder",builder,"--load","--progress=plain","--target",role,"-f","v3/Dockerfile","-t",tag,*inputs,"-"],stdin=stream,timeout=3600)
        image=self.command(["docker","image","inspect","--format","{{.Id}}",tag],timeout=30).strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}",image):raise ValueError("Image identity unavailable")
        return image,log


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

    def published_runtime_source(self, release_root):
        """Bind runtime mounts to the verified readable copy, keeping task modes private."""
        job=getattr(self.job_context,"job",None)or{}
        if not job.get("job_id") or Path(job.get("release_root",""))!=Path(release_root):
            raise ValueError("Owned frozen runtime source required")
        destination=ASSET_ROOT/"releases"/job["job_id"]
        release.verify_publication(destination,job["job_id"],job["inputs_sha256"])
        return destination

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
            host = release.release_mounts(host, self.published_runtime_source(release_root), ASSET_ROOT)
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

    def remove(self, name):
        self.command(["docker", "rm", name], timeout=30)

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
    """Authenticate callers and durably coordinate isolated source, releases, recovery and ownership."""
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
        self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=CONFIG.get("scheduler",{}).get("max_phases",4)if CONFIG.get("scheduler",{}).get("mode")=="enforce"else 1)
        self.ui_pool = concurrent.futures.ThreadPoolExecutor(max_workers=1)
        self.deploy_lock = threading.RLock()
        self.active_ui = None
        self.jobs = {}
        self.job_cancellations = {}
        self.queue_index = {}
        self.requests = {}
        self.active = None
        self.active_jobs=set()
        self.check_runs={}
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
        self.code_index_ledger=review_store_module.ledger_class(maintenance_engine)(Path(CONFIG["install_root"]).parent/"code-index-maintenance"if self.state==Path(CONFIG["state_root"])else self.state/"code-index-maintenance",inventory=getattr(self.runner,"maintenance_inventory",None),max_bytes=CONFIG.get("code_index_ledger_max_bytes",768*1024**2))
        self.review_policy=review_policy_module.ReviewPolicy(self,release,ASSET_ROOT)
        self.runner.maintenance_snapshot_folder=self.code_index_ledger.folder/"snapshots"
        self.runner.maintenance_provider=self._code_index_snapshot
        self.runner.maintenance_activated=self._code_index_activated
        self.performance = performance.Performance(self)
        self.scheduler=scheduler_module.Scheduler(self,CONFIG)
        self.resources=resource_module.Resources(self.state,self.runner,CONFIG.get("scheduler",{}))
        self.runner.resources=self.resources
        self.runner.retention_enabled=CONFIG.get("scheduler",{}).get("task_aware_cleanup",False)
        self.runner.resource_progress=self._resource_progress
        self.runner.executor=executor_module.Executor(self.runner,self.resources,CONFIG)
        self.package_cache=packaging_cache_module.PackagingCache(self,CONFIG,release.entries,context_digest)
        self.runner.package_cache=self.package_cache
        self.resources.start()
        self.index_coordinator=index_coordinator_module.IndexCoordinator(self,index_queue_module,release,context_digest,TASK_CONTEXT,ASSET_ROOT,scheduler_module.ownership.source_path)
        self.runner.index_seed_provider=self.index_coordinator.publication_seeds
        self.publication_targets=TARGETS
        self.ui_publication=ui_publication_module.UIPublication(self,release,context_digest,CONTEXT,ASSET_ROOT,ui_policy_module)
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
        self.post_deployment=post_deployment_module.PostDeployment(self,release,ASSET_ROOT,context_digest)
        self.maintenance_activities=post_deployment_module.MaintenanceActivities(self,release,ASSET_ROOT)
        if recover:
            for job in self.jobs.values():
                if job.get("batch_members") and job["status"] not in ("queued", "running"):
                    self._mirror_batch(job)
            self.scheduler.migrate_legacy()
            self._start_next()
        self.post_deployment.resume()

    scheduler_entries=staticmethod(lambda blob:release.entries(blob,context_digest))
    scheduler_extract=staticmethod(lambda root,blob:release.extract(root,blob,context_digest))
    scheduler_digest=staticmethod(lambda blob:context_digest(blob)[0])
    scheduler_task_snapshot=staticmethod(lambda root:release.snapshot(root,TASK_CONTEXT,context_digest))
    def scheduler_preserve_inspection(self,blob,tree):
        """Copy only previously authorized read-only inspection snapshots into the new private generation."""
        if not tree.exists():return blob
        current=release.entries(blob,context_digest)
        previous=release.entries(release.snapshot(tree,TASK_CONTEXT,context_digest),context_digest)
        current.update({name:value for name,value in previous.items()if release.inspection_path(name)})
        return release.archive(current,context_digest)
    def scheduler_verify_health(self):
        """Completion validates actual retained consumers, not only a past successful job flag."""
        for name in TARGETS:
            current=self.runner.inspect(name)
            if not self.runner.healthy(name,current["Image"]):raise ValueError("Current consumer unhealthy")

    def _resource_progress(self,job):
        """Publish bounded phase admission details under normal durable job serialization."""
        if job.get("internal_index"):
            coordinator=getattr(self,"index_coordinator",None)
            if coordinator is not None and job.get("checkpoint_key"):coordinator.queue.progress(job["checkpoint_key"],job.get("resource_waiting"))
            return
        with self.lock:self._save(job)

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
                    if not self.scheduler.retention_allowed(key):
                        continue
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
                    if kind in {"reconciled_task_source", "reconciled_job_artifacts"} and not self.scheduler.retention_allowed(item["key"]):
                        continue
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

    def post_deployment_candidate(self, job):
        """Return the sealed candidate, reconstructing a missing derived archive only from its retained execution source."""
        folder = self.state / job["job_id"]
        candidate = folder / "candidate.tar"
        if candidate.is_symlink():
            raise ValueError("Post-deployment candidate archive symlink")
        if candidate.is_file():
            blob = candidate.read_bytes()
            if context_digest(blob)[0] != job["inputs_sha256"]:
                raise ValueError("Post-deployment candidate archive changed")
            return blob
        source = self.state / "execution" / job["job_id"] / "source"
        published = ASSET_ROOT / "releases" / job["job_id"]
        if (source.is_symlink() or not source.is_dir() or published.is_symlink()
                or not published.is_dir()):
            raise ValueError("Post-deployment immutable source unavailable")
        # The sealed candidate hash is the recovery authority. The execution
        # snapshot may differ in non-candidate metadata and file modes from the
        # published asset tree, so its broader source digest is not comparable.
        release.verify_publication(published, job["job_id"], job["inputs_sha256"])
        blob = release.snapshot(source, CONTEXT, context_digest)
        if context_digest(blob)[0] != job["inputs_sha256"]:
            raise ValueError("Retained execution candidate digest differs")
        candidate.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        try:
            fd = os.open(candidate, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        except FileExistsError:
            blob = candidate.read_bytes()
            if context_digest(blob)[0] != job["inputs_sha256"]:
                raise ValueError("Concurrent post-deployment candidate recovery changed")
            return blob
        with os.fdopen(fd, "wb") as stream:
            stream.write(blob); stream.flush(); os.fsync(stream.fileno())
        parent = os.open(candidate.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
        return blob

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
        if self.scheduler.mode=="enforce":return False
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
                     and job["operation"] in ("deploy", "build_deploy", "ui_deploy", "index_deploy", "host_service_deploy", "plugin_deploy")
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
        self.scheduler.housekeeping()
        controlled=self.scheduler.mode=="enforce"
        if self.stopping or self._has_unresolved_release()or (not controlled and(self.active is not None or self.active_ui is not None)):
            return
        if controlled and len(self.active_jobs)>=CONFIG["scheduler"]["max_phases"]:return
        # UI releases pass queued image builds, but never an active release.
        pending = sorted(self.jobs.values(), key=lambda value: (
            value.get("operation") not in HOT_OPERATIONS, value["created_at"]))
        for job in pending:
            if job["operation"]=="check"and job["status"]=="queued"and job["job_id"]in self.check_runs:
                if controlled and not self.scheduler.execution_eligible(job,[self.jobs[key]for key in self.active_jobs]):continue
                call=self.check_runs.pop(job["job_id"])
                self.active_jobs.add(job["job_id"]);self.active=next(iter(self.active_jobs),None)
                job.update(status="running",phase="checking_source",started_at=time.time())
                job.setdefault("phase_history",[]).append({"phase":"checking_source","at":job["started_at"]})
                self._save(job);self.pool.submit(self._execute_check,*call)
                if not controlled:return
                if len(self.active_jobs)>=CONFIG["scheduler"]["max_phases"]:return
                continue
            if not job.get("isolated") or job["status"] != "queued":
                continue
            if job["job_id"]in self.active_jobs:continue
            if controlled and not self.scheduler.execution_eligible(job,[self.jobs[key]for key in self.active_jobs]):continue
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
                if not controlled and not resuming and job["operation"] == "build_deploy" and not job["synchronization"]["total"]:
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
                self.active_jobs.add(job["job_id"])
                self.active=next(iter(self.active_jobs))
                # Source archives are durable before compiler admission; retain only the candidate passed to the worker.
                del base,task,live
                peer_base=peer_task=None
                self.pool.submit(self._run, job, candidate)
                if controlled:
                    if len(self.active_jobs)>=CONFIG["scheduler"]["max_phases"]:return
                    continue
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
        if parent["status"] in ("queued", "running", "needs_decision", "preparing_retry") and not parent.get("cancel_request") and sorted(expected) != sorted(affected):
            raise ValueError("Build batch changed; refresh affected jobs before requesting stop")
        # A merged candidate is one tested unit: cancellation stops all members.
        if not parent.get("cancel_request") and parent["status"] in ("queued", "running", "needs_decision", "preparing_retry"):
            parent["cancel_request"] = {"request_id": args["request_id"],
                "requested_by": trusted["user_id"], "requested_at": time.time(),
                "affected_job_ids": affected}
            parent["revoked"] = True
            active = parent["job_id"]in self.active_jobs or parent["job_id"]==self.active_ui or parent["status"]=="running"
            if not active:
                parent.update(status="cancelled", reason="queued job cancelled before execution",
                              finished_at=time.time())
            self._save(parent)  # Persist intent before signalling execution.
            if not active:self.check_runs.pop(parent["job_id"],None)
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
        view = {key: job[key] for key in ("job_id", "operation", "status", "phase", "inputs_sha256", "created_at", "images", "rollback", "tool_name", "access_fingerprint", "isolated", "batch_parent", "batch_members", "synchronization", "sync_ack_digest", "maintenance", "consumer_adoption", "consumer_before", "plugin_artifacts", "resource_waiting", "engine_execution_modes", "engine_artifacts", "compiler_memory_cap", "compiler_cap_outcomes", "test_filter", "feature_profile", "linked_stable", "optimized_mode_equivalence", "package_memory_cap", "package_cap_outcomes", "package_failures", "publication_policy_version", "preparation", "dependency_bases", "dependency_refresh", "packaging_preflight", "packaging_resume", "compiler_checkpoint") if key in job}
        if job.get("review_wait"):
            review=job["review_wait"]
            view["review_wait"]={k:review[k]for k in ("state","started_at","last_activity_at","expires_at")if k in review}
            view["review_wait"]["expiring_soon"]=0<review.get("expires_at",0)-time.time()<=60
        if job.get("deferred_documentation"):
            approval=job["deferred_documentation"]
            try:self.review_policy.check_complete(job);documentation_state="complete"
            except ValueError:documentation_state="pending"
            view["deferred_documentation"]={"state":documentation_state,"approved_at":approval["approved_at"],"pending_paths":[]if documentation_state=="complete"else approval.get("pending_paths",[])[:32],"completion":approval.get("completion")}
        if job.get("post_deployment"):
            view["post_deployment"]={k:job["post_deployment"][k]for k in ("index_status","attempts","queued_at","started_at","finished_at","error")if k in job["post_deployment"]}
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
        if self.scheduler.mode=="enforce"and job.get("ownership_id"):self.scheduler.assert_job(job)
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
        job["engine_execution_modes"]=self.runner.engine_execution_modes(job["job_id"])
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
        if task_id is None and name in ("file_edit","file_write"):
            request_id=args.get("request_id")
            if not isinstance(request_id,str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",request_id):raise ValueError("Stable implicit source request ID required")
            owned=[i for i in self.work.values()if self._owned(i,trusted,workspace)and i.get("implicit_source")]
            previous=next((i for i in owned if request_id in i.get("applies",{})),None)
            current=previous or next((i for i in sorted(owned,key=lambda v:v["created_at"],reverse=True)if i["status"]=="open"),None)
            task_id=current["task_id"]if current else"auto-source-"+hashlib.sha256(canonical([trusted["user_id"],workspace,request_id])).hexdigest()[:48]
            args={**args,"task_id":task_id}
            implicit=True
        else:implicit=False
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
            if implicit:item["implicit_source"]=True;self._save_work()
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

    # @code-index {"version":1,"feature":"owner-claim-release","aliases":["release reservations","unblock engine"],"purpose":"Release exactly held owner claims without deleting source; fence old epochs and retain recovery receipts.","span_lines":48,"configuration":{"status":"not_applicable","rationale":"Ownership fencing and authenticated scope release are fixed authority invariants."}}
    def reconcile_release(self,request,args):
        """Settle one exact release journal; never execute or repeat a scope release."""
        required={"action","task_id","original_request_id","request_id","expected_revision","expected_epoch"}
        trusted=request["trusted"];workspace=request["workspace"]
        if set(args)!=required or not trusted.get("local_caller"):
            raise PermissionError("Exact operator release reconciliation required")
        self._incoming_lease(trusted)
        item=self.scheduler.item(args,trusted,workspace);key=item["key"]
        owner=self.scheduler.owner(trusted,workspace)
        path=self.scheduler.journal_path(owner,args["request_id"],"release-recovery-"+key)
        original_path=self.scheduler.journal_path(owner,args["original_request_id"],"release-"+key)
        digest=hashlib.sha256(canonical(args)).hexdigest()
        def read(p):
            fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC)
            try:
                meta=os.fstat(fd)
                if not stat.S_ISREG(meta.st_mode) or meta.st_nlink!=1 or meta.st_size>1024*1024:raise ValueError("Invalid release recovery journal")
                with os.fdopen(fd,"rb",closefd=False)as stream:return json.loads(stream.read(1024*1024+1))
            finally:os.close(fd)
        record=read(path) if path.exists() or path.is_symlink() else None
        if record:
            if record["digest"]!=digest:raise ValueError("Recovery request ID reused")
            if record["state"]=="done":return record["result"]
        previous=read(original_path)
        if previous.get("task_key")!=key or previous.get("request_id")!=args["original_request_id"]:
            raise ValueError("Original release journal identity differs")
        replay=previous.get("reconciled_by")==args["request_id"] and record is not None
        if previous.get("state")!="pending" and not replay:raise ValueError("Pending release journal required")
        current=self.scheduler.store.get(key)
        if current["owner"]!=owner:raise PermissionError("Current task owner required")
        token="release-"+hashlib.sha256(canonical([owner,key,args["original_request_id"]])).hexdigest()[:56]
        committed=self.scheduler.store.receipt(token)
        if committed is not None and (committed.get("id")!=key or committed.get("owner")!=owner):
            raise ValueError("Release receipt identity differs")
        receipt_hash=None if committed is None else hashlib.sha256(canonical(committed)).hexdigest()
        if record:
            if record["receipt_sha256"]!=receipt_hash or record["original_digest"]!=previous.get("digest"):
                raise ValueError("Release recovery evidence changed")
        else:
            if type(args["expected_revision"])is not int or type(args["expected_epoch"])is not int or current["revision"]!=args["expected_revision"] or current["epoch"]!=args["expected_epoch"]:
                raise ValueError("Stale recovery ownership binding")
            record={"state":"pending","digest":digest,"task_key":key,"request_id":args["request_id"],
                "receipt_sha256":receipt_hash,"original_digest":previous.get("digest"),"ownership":current}
            self.scheduler.write_record(path,record)
        if committed is None:
            # No SQLite receipt means its atomic scope mutation never committed.
            settled={**previous,"state":"rejected","error":"Uncommitted release explicitly reconciled; use a fresh request with current ownership", "reconciled_by":args["request_id"]}
            outcome="aborted_uncommitted"
        else:
            evidence=committed.get("evidence",{})
            release_id=evidence.get("post_deployment_release")
            if release_id:
                job=self.jobs.get(release_id)
                if not job or job.get("ownership_id")!=key or job.get("source_generation")!=committed["generation"]:
                    raise ValueError("Committed deployment release requires separate reconciliation")
                job["write_claim_release"]=evidence["deployment"];self._save(job)
            # Preserve newer source-generation and input bindings after the old release committed.
            item.update(ownership_epoch=current["epoch"],ownership_revision=current["revision"])
            if current["revision"]==committed["revision"]:item.pop("ownership_inputs_sha256",None)
            self._save_work()
            settled={**previous,"state":"done","result":committed,"reconciled_by":args["request_id"]}
            outcome="settled_committed"
        self.scheduler.write_record(original_path,settled)
        result={"status":outcome,"task_id":item["task_id"],"original_request_id":args["original_request_id"],
            "scope_mutation_performed":False,"ownership":record["ownership"],"release_receipt":committed}
        self.scheduler.write_record(path,{**record,"state":"done","result":result})
        self._start_next()
        return result

    def release_claims(self,request,args):
        """Release explicit write scopes; sealed execution retains only a bound promotion authority."""
        allowed={"action","task_id","scopes","reason","request_id","expected_revision","expected_epoch","commit","frozen_job_id"}
        if set(args)-allowed:raise ValueError("Unsupported release arguments")
        if not isinstance(args.get("reason"),str)or not 1<=len(args["reason"].encode())<=500:raise ValueError("Bounded release reason required")
        trusted=request["trusted"];workspace=request["workspace"]
        self._incoming_lease(trusted)
        item=self.scheduler.item(args,trusted,workspace);owner=self.scheduler.owner(trusted,workspace)
        path=self.scheduler.journal_path(owner,args.get("request_id"),"release-"+item["key"])
        digest=hashlib.sha256(canonical(args)).hexdigest()
        if path.is_symlink():raise PermissionError("Release request symlink")
        record=json.loads(path.read_text())if path.exists()else None
        if record and record["digest"]!=digest:raise ValueError("Request ID reused")
        if record and record["state"]=="rejected":raise ValueError(record["error"])
        if record and record["state"]=="done":return record["result"]
        if record and self.scheduler.pending(item["key"],except_path=path):raise ValueError("Release reconciliation in progress; finish its exact recovery request")
        token="release-"+hashlib.sha256(canonical([owner,item["key"],args.get("request_id")])).hexdigest()[:56]
        frozen=record.get("frozen") if record else None
        if not record:
            value=self.scheduler.ensure(item,trusted,workspace)
            if type(args.get("expected_revision")) is not int or type(args.get("expected_epoch")) is not int or value["revision"]!=args["expected_revision"] or value["epoch"]!=args["expected_epoch"]:raise ValueError("Stale ownership revision or epoch")
            active=[j for j in self.jobs.values()if j.get("ownership_id")==item["key"]and j["status"]in {"queued","running","interrupted","uncertain","needs_decision"}]
            if active:
                job_id=args.get("frozen_job_id")
                if len(active)!=1 or not isinstance(job_id,str) or active[0].get("job_id")!=job_id or item.get("job_id")!=job_id:
                    raise ValueError("Reconcile task execution before releasing claims")
                job=active[0];candidate=self.state/job_id/"candidate.tar"
                if job["status"] not in {"queued","running"} or job.get("effects_started") or not candidate.is_file() or self.scheduler_digest(candidate.read_bytes())!=job.get("inputs_sha256") or job.get("ownership_id")!=value["id"] or job.get("ownership_epoch")!=value["epoch"] or job.get("source_generation")!=value["generation"]:
                    raise ValueError("Exact frozen pre-effect execution required")
                scopes=args.get("scopes")
                if not isinstance(scopes,list) or not scopes or not set(scopes)<=set(value["scopes"]):
                    raise ValueError("Only held scopes can be released")
                protected=self.scheduler.components(item).scopes(job.get("changed_paths",[]))
                if any(scheduler_module.ownership.scope_overlap(a,b) for a in scopes for b in protected):raise ValueError("Retain affected source promotion reservations")
                frozen={"job_id":job_id,"inputs_sha256":job["inputs_sha256"],"generation":value["generation"],"released_scopes":sorted(scopes)}
            if any(lease.get("task_key")==item["key"] for lease in self.code_index_leases.values()):raise ValueError("Wait for this task maintenance before releasing claims")
            if self.scheduler.pending(item["key"]):raise ValueError("Scheduler transition requires recovery")
            record={"digest":digest,"state":"pending","task_key":item["key"],"request_id":args["request_id"],"frozen":frozen}
            self.scheduler.write_record(path,record)
        result=self.scheduler.store.receipt(token)
        if result is None:
            try:
                active=[j for j in self.jobs.values() if j.get("ownership_id")==item["key"] and j["status"] in {"queued","running","interrupted","uncertain","needs_decision"}]
                if active and frozen is None:raise ValueError("Reconcile task execution before releasing claims")
                if any(l.get("task_key")==item["key"] for l in self.code_index_leases.values()):raise ValueError("Wait for this task maintenance before releasing claims")
                deployed=self.jobs.get(item.get("job_id"),{})
                if frozen is not None:
                    result=self.scheduler.store.release_frozen_scopes(item["key"],owner,args["expected_revision"],args["expected_epoch"],args["scopes"],frozen,token)
                elif deployed.get("status")=="completed"and(deployed.get("post_deployment")or deployed.get("deferred_documentation")):
                    evidence=self.scheduler.verified(item,{"job_id":deployed["job_id"],"commit":args.get("commit"),"acceptance":[{"check":"Healthy exact committed deployment; metadata follows without source write reservations","passed":True}]},deployment_only=True)
                    result=self.scheduler.store.release_deployed_scopes(item["key"],owner,args["expected_revision"],args["expected_epoch"],args.get("scopes"),evidence,token)
                else:
                    result=self.scheduler.store.release_scopes(item["key"],owner,args["expected_revision"],args["expected_epoch"],args.get("scopes"),args.get("reason"),token)
            except (ValueError,PermissionError) as error:
                self.scheduler.write_record(path,{**record,"state":"rejected","error":str(error)[:512]})
                raise
        current=self.scheduler.store.get(item["key"])
        if item.get("job_id") and current["evidence"].get("post_deployment_release")==item["job_id"]:
            deployed=self.jobs[item["job_id"]];deployed["write_claim_release"]=current["evidence"]["deployment"];self._save(deployed)
        item.update(ownership_epoch=current["epoch"],ownership_revision=current["revision"])
        item.pop("ownership_inputs_sha256",None);self._save_work()
        self.scheduler.write_record(path,{**record,"state":"done","result":result})
        self._start_next()
        return result


    def successor_task(self,request,args):
        """Import a terminal frozen proposal through normal checked writes; never replay deployment."""
        required={"action","task_id","source_job_id","request_id","expected_inputs_sha256"}
        trusted=request["trusted"];workspace=request["workspace"]
        if set(args)!=required or not trusted.get("local_caller"):
            raise PermissionError("Exact operator successor request required")
        self._incoming_lease(trusted)
        item=self.scheduler.item(args,trusted,workspace)
        owner=self.scheduler.owner(trusted,workspace)
        path=self.scheduler.journal_path(owner,args["request_id"],"successor-"+item["key"])
        digest=hashlib.sha256(canonical(args)).hexdigest()
        if path.is_symlink():raise PermissionError("Successor receipt symlink")
        if path.exists():
            record=json.loads(path.read_text())
            if record["digest"]!=digest:raise ValueError("Successor request ID reused")
            if record["state"]=="done":return record["result"]
        else:
            job=self.jobs.get(args["source_job_id"],{})
            if not job or not self._owned(job,trusted,workspace):raise PermissionError("Owned frozen source job required")
            previous=self.scheduler.item({"task_id":job.get("task_id")},trusted,workspace)
            if previous["key"]==item["key"]:raise ValueError("Distinct successor task required")
            if previous.get("job_id")!=job.get("job_id") or previous.get("status")!="closed" or previous.get("mode")!="isolated":
                raise ValueError("Latest closed isolated proposal required")
            if job.get("status")not in {"failed","cancelled","interrupted","stale"} or job.get("effects_started") or job.get("batch_parent") or job.get("batch_members"):
                raise ValueError("Reconcile effects before importing a terminal preactivation proposal")
            if any(j.get("task_id")==previous["task_id"] and j.get("user_id")==previous["user_id"] and j.get("workspace")==workspace and j.get("status")in {"queued","running","uncertain","needs_decision"} for j in self.jobs.values()):
                raise ValueError("Source task has unresolved execution")
            held=self.scheduler.store.get(previous["key"])
            if held["generation"]!=job.get("source_generation"):raise ValueError("Frozen ownership generation changed")
            if held["scopes"] or held["requested"]:raise ValueError("Yield original reservations through release_claims before successor import")
            if self.scheduler.pending(previous["key"]) or self.scheduler.pending(item["key"]):raise ValueError("Scheduler transition requires recovery")
            if item.get("status")!="open" or item.get("mode")!="isolated" or item.get("successor"):
                raise ValueError("Fresh open isolated successor required")
            tree=Path(item["workspace_path"]).parent
            snapshot=release.snapshot(tree,TASK_CONTEXT,context_digest)
            baseline=(tree/"baseline.tar").read_bytes()
            if context_digest(snapshot)[0]!=args["expected_inputs_sha256"] or context_digest(snapshot)[0]!=context_digest(baseline)[0]:
                raise ValueError("Unchanged empty successor and exact input binding required")
            live=self._current_release_blob()
            current=release.entries(snapshot,context_digest);published=release.entries(live,context_digest)
            if {p:v for p,v in current.items() if not release.inspection_path(p)}!=published:
                raise ValueError("Successor baseline is stale; begin a fresh task")
            source_tree=Path(previous["workspace_path"]).parent
            frozen=(self.state/job["job_id"]/"task.tar").read_bytes()
            if context_digest(release.snapshot(source_tree,TASK_CONTEXT,context_digest))[0]!=context_digest(frozen)[0]:
                raise ValueError("Frozen proposal changed")
            source_base=(source_tree/"baseline.tar").read_bytes()
            if context_digest(source_base)[0]!=previous["base_inputs_sha256"]:raise ValueError("Original source baseline changed")
            merged,changed=release.candidate(source_base,frozen,live,context_digest)
            patch=checked_git_modes(release.proposal_patch(snapshot,merged,context_digest))
            if not patch:raise ValueError("Proposal already present in current release")
            record={"state":"prepared","digest":digest,"source_job_id":job["job_id"],
                "source_task_id":previous["task_id"],"source_task_key":previous["key"],
                "source_summary":previous["summary"],"source_generation":job.get("source_generation"),
                "source_sha256":context_digest(frozen)[0],"baseline_sha256":context_digest(live)[0],
                "changed_paths":changed,"patch":patch,"request_id":args["request_id"]}
            self.scheduler.write_record(path,record)
        # Preparation alone grants no future authority over a proposal that has since resumed.
        if args["request_id"] not in item.get("applies",{}):
            previous=self.work[record["source_task_key"]];job=self.jobs[record["source_job_id"]]
            state=self.scheduler.store.get(previous["key"])
            if previous.get("job_id")!=job["job_id"] or previous.get("status")!="closed" or job.get("effects_started") or job.get("status")not in {"failed","cancelled","interrupted","stale"}:
                raise ValueError("Successor source resumed; reconcile before importing")
            if state["scopes"] or state["requested"] or state["generation"]!=record["source_generation"]:
                raise ValueError("Successor source ownership changed")
            if any(j.get("task_id")==previous["task_id"] and j.get("user_id")==previous["user_id"] and j.get("workspace")==workspace and j.get("status")in {"queued","running","uncertain","needs_decision"} for j in self.jobs.values()):
                raise ValueError("Successor source has unresolved execution")
            if self.scheduler_digest(self.scheduler_task_snapshot(Path(previous["workspace_path"]).parent))!=record["source_sha256"]:
                raise ValueError("Successor source bytes changed")
        # A saved checked-apply receipt settles an interrupted response without a second edit.
        result=self.handle({**request,"name":"v3_source_apply","arguments":{
            "task_id":item["task_id"],"request_id":args["request_id"],
            "expected_inputs_sha256":args["expected_inputs_sha256"],"patch":record["patch"]}})
        if result.get("status")!="success":
            return {"status":"requires_reconciliation","source_apply":result,"source_job_id":record["source_job_id"]}
        provenance={k:v for k,v in record.items() if k not in {"state","digest","patch"}}
        item["successor"]=provenance
        original=self.work[record["source_task_key"]]
        if item["key"] not in original.setdefault("successor_tasks",[]):original["successor_tasks"].append(item["key"])
        self._save_work()
        result={"status":"imported","task_id":item["task_id"],"provenance":provenance,
            "source_apply":result,"artifact_reuse":"requires_exact_input_verification","publication_required":True}
        self.scheduler.write_record(path,{**record,"state":"done","result":result})
        return result

    def unified_build(self, request, args):
        action = args.get("action")
        if action=="reconcile_release":return self.reconcile_release(request,args)
        if action=="successor_task":return self.successor_task(request,args)
        if action=="release_claims":return self.release_claims(request,args)
        if action=="reload_memory_policy":return self.reload_memory_policy(args,request["trusted"])
        if action in self.scheduler.actions:return self.scheduler.handle(args,request["trusted"],request["workspace"])
        allowed = {"action", "task_id", "summary", "expected_inputs_sha256", "sync_decision",
            "expected_sync_digest", "offset", "job_id", "request_id", "check", "timeout_seconds", "compiler_memory_cap", "package_memory_cap", "test_filter", "feature_profile"}
        if set(args) - allowed: raise ValueError("unsupported build_work argument")
        def call(tool, arguments):
            return self.handle({**request, "name": tool, "arguments": arguments})
        if action == "status" and args.get("job_id"):
            return call("v3_worker_job", {"job_id": args["job_id"]})
        if action == "check":
            return call("v3_cargo_task", {"task_id": args.get("task_id"),
                "task": args.get("check", "check"), "expected_inputs_sha256": args.get("expected_inputs_sha256"),
                "timeout_seconds": args.get("timeout_seconds", 600),**({"request_id":args["request_id"]}if "request_id"in args else{}),**({"compiler_memory_cap":args["compiler_memory_cap"]}if "compiler_memory_cap"in args else{}),**({"test_filter":args["test_filter"]}if "test_filter"in args else{}),**({"feature_profile":args["feature_profile"]}if "feature_profile"in args else{})})
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
        # own docs/tests and paired ownership classifier. Each changed executable
        # must match protected installation. Worker, policy and app changes still build.
        review_modules={"buildctl.py","index_queue.py","engine_components.py","post_deployment.py","release.py","workspace_support.py","review_store.py","review_policy.py","index_coordinator.py","scheduler.py","ui_publication.py","resources.py","resource_control.py","executor.py","config.py","lifecycle.py","scheduler-defaults.json","docker_retention.py","docker_guard_dispatch.py","apt_prefetch.py","packaging_cache.py","image_reuse.py","install.py"}
        allowed={"v3/tests/test_scheduler_executor.py","v3/tests/test_buildctl_wait.py","v3/tests/test_index_queue.py","v3/tests/test_scheduler_post_deployment.py","v3/tests/test_build_cache_retention.py","v3/tests/test_build_index_resources.py","v3/tests/test_scheduler_review_store.py","v3/tests/test_scheduler_review_policy.py","v3/tests/test_scheduler_docker_retention.py","v3/tests/test_admin_build_install.py"}|{"v3/build-service/"+n for n in review_modules}|{"v3/tests/test_scheduler_review_lease.py","v3/tests/test_build_ui_publication.py","v3/tests/test_scheduler_memory_limits.py","v3/tests/test_scheduler_resources.py","v3/tests/test_scheduler_ram_packaging.py","v3/tests/test_scheduler_package_cache.py","v3/tests/test_scheduler_image_reuse.py"}|{"v3/build-service/host.py","v3/build-service/ownership.py","v3/build-service/README.md",
                 "v3/tests/test_build_service.py","v3/tests/test_scheduler_ownership.py","v3/tests/test_admin_build.py",
                 "v3/build-service/boot_host.py","v3/tests/test_boot_supervisor.py","v3/tests/test_boot_transport.py",
                 "v3/docs/boot-supervisor.md","v3/docs/engine-modularization.md"}
        if not trusted.get("local_caller") or not changed or not set(changed)<=allowed:return False
        try:files=release.entries(blob,context_digest)
        except (ValueError,tarfile.TarError):return False
        selected=sorted(n for n in review_modules if "v3/build-service/"+n in changed)+["host.py"]+[name for name in ("ownership.py","boot_host.py")if "v3/build-service/"+name in changed]
        for name in selected:
            path=Path(CONFIG["service_dir"])/("bootstrap.py"if name=="install.py"else name)
            fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC)
            try:
                meta=os.fstat(fd)
                if not stat.S_ISREG(meta.st_mode)or meta.st_uid!=0 or meta.st_mode&0o022 or meta.st_nlink!=1:raise PermissionError("Protected installed broker code required")
                with os.fdopen(fd,"rb",closefd=False)as stream:installed=stream.read(2*1024*1024+1)
                candidate=files.get("v3/build-service/"+name)
                if candidate is None or candidate[1]!=installed:return False
            finally:os.close(fd)
        return True

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
        """Fence live authority for open, sealed-prepared or approved deployed review sources."""
        with self.lock:
            self._lease(lease)
            item=self.work.get(key)
            prepared=self.jobs.get(lease.get("prepared_job_id"))
            deployed=self.jobs.get(lease.get("deployed_job_id"))
            if not item:raise PermissionError("Maintenance task unavailable")
            if deployed:
                approval=deployed.get("deferred_documentation",{}).get("binding",{})
                if deployed.get("revoked") or deployed.get("ownership_id")!=key or item.get("job_id")!=deployed["job_id"] or deployed["status"]!="completed" or any(approval.get(k)!=deployed.get(k) for k in ("job_id","inputs_sha256","source_generation","ownership_epoch")):
                    raise PermissionError("Deferred review generation changed")
                state=self.scheduler.store.assert_owned(key,self.scheduler.owner(lease,lease["workspace"]),deployed["ownership_epoch"],[])
                if state["generation"]!=deployed["source_generation"]:raise PermissionError("Deferred review source generation changed")
            elif item["status"]!="open":
                if not prepared or prepared.get("ownership_id")!=key or prepared["status"]!="needs_decision"or prepared["phase"]!="awaiting_prepared_reviews":raise PermissionError("Maintenance task froze; select its sealed prepared review candidate")
                self.scheduler.store.assert_owned(key,self.scheduler.owner(lease,lease["workspace"]),prepared["ownership_epoch"],[])
                if not self.scheduler.review_wait(prepared,activity=True,authority_until=lease["lease_until"]):raise PermissionError("Prepared review delegation expired or changed")

    def _code_index_snapshot(self,job_id):
        """Merge eligible task evidence with the current published ledger for final projection."""
        self._code_index_reconcile()
        with self.lock:
            job=self.jobs.get(job_id,{})
            ids={job_id,*job.get("batch_members",[])}
            keys=[key for key,item in self.work.items()if item.get("job_id")in ids]
        # Never wait for a consistent multi-store cut while holding host authority.
        keys.extend(self.review_policy.task_keys())
        return self.code_index_ledger.snapshot(list(dict.fromkeys(keys)))
    def _code_index_reconcile(self):
        # Immutable active projection wins after restart or rollback. No task/private DB is read.
        """Restore the active immutable release projection after restart or rollback."""
        # A matching immutable projection needs no publication serialization.
        current=self.runner.current_asset_id()
        if not current or current==self.code_index_ledger.active_release():return
        with self.deploy_lock:
            # The pointer may have changed while waiting; never import the earlier one.
            current=self.runner.current_asset_id()
            if current and current!=self.code_index_ledger.active_release() and (ASSET_ROOT/"releases"/current/".code-index/index.sqlite").is_file():
                self._code_index_activated(current)
    def _code_index_activated(self,job_id):
        """Import the paired release ledger; legacy indexes initialize pending reviews."""
        root=ASSET_ROOT/"releases"/job_id
        if not(root/".code-index/index.sqlite").is_file():
            receipt=release.verify_publication(root,job_id)
            if receipt.get("publication_mode")=="post_deployment_indexing_v1":return
            raise ValueError("Published index unavailable")
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
    def finish_retry_registration(self,request,args,job):
        """Recover the preparation-to-queue boundary without replaying compilation or publication."""
        trusted=request["trusted"];workspace=request["workspace"]
        deadline,fingerprint=self._incoming_lease(trusted)
        item=self.scheduler.item(args,trusted,workspace);state=self.scheduler.store.get(item["key"])
        if not self._owned(job,trusted,workspace)or job.get("ownership_id")!=item["key"]or job.get("retry_of")!=args["job_id"]:
            raise PermissionError("Retry preparation owner changed")
        if job.get("access_fingerprint")!=fingerprint or job.get("revoked") or self._maintenance()["drained"]:
            raise PermissionError("Retry preparation authority changed or broker drained")
        if item.get("status")!="closed"or item.get("job_id")not in {job["retry_of"],job["job_id"]}or state["generation"]!=job["source_generation"]or state["epoch"]!=job["ownership_epoch"]or state["state"]!="owned":
            raise ValueError("Retry preparation source ownership changed")
        self.scheduler.store.assert_owned(item["key"],self.scheduler.owner(trusted,workspace),state["epoch"],self.scheduler.components(item).scopes(job["changed_paths"]))
        private=release.snapshot(Path(item["workspace_path"]).parent,TASK_CONTEXT,context_digest)
        if context_digest(private)[0]!=item.get("inputs_sha256"):raise ValueError("Frozen task changed during retry preparation")
        prior=self.jobs.get(job["retry_of"],{})
        if prior.get("status")!="failed"or prior.get("effects_started"):raise ValueError("Original retry outcome changed")
        if any(j.get("ownership_id")==item["key"]and j.get("job_id")!=job["job_id"]and j.get("status")in {"queued","running","needs_decision","uncertain","interrupted"}for j in self.jobs.values()):
            raise ValueError("Reconcile other task execution before admitting prepared retry")
        # Revalidate copied archives on every recovery; compiler/image evidence is checked again in execution.
        proof=image_reuse_module.prepare_retry_archives(self.state,self.work_dir,job["job_id"],self.state/job["retry_of"],
            job["body_digest"],self.scheduler.write_record,recover_legacy=args.get("recover_preparation",False))
        if proof!=job.get("retry_preparation"):raise ValueError("Retry preparation receipt changed")
        item.update(job_id=job["job_id"],finished_at=time.time());self._save_work()
        job.update(status="queued",phase="waiting",lease_until=max(deadline,time.time()+4*3600))
        self._save(job);self.jobs[job["job_id"]]=job;self.requests[job["request_key"]]=job["job_id"]
        self._start_next()
        return self._view(job)

    def retry_verified_failed_release(self,request,args):
        """Queue a new attempt while preserving the failed job and its exact evidence."""
        allowed={"action","task_id","job_id","request_id","expected_revision","expected_epoch","sync_decision","expected_sync_digest","rebuild_if_stale","lineage_job_id","recover_preparation"}
        if (set(args)-allowed or not {"action","task_id","job_id","request_id","expected_revision","expected_epoch"}<=set(args)
                or type(args.get("recover_preparation",False)) is not bool
                or type(args.get("rebuild_if_stale",False)) is not bool
                or ("lineage_job_id" in args and (not isinstance(args["lineage_job_id"],str) or not re.fullmatch(r"[0-9a-f-]{36}",args["lineage_job_id"])))
                or not request["trusted"].get("local_caller")):
            raise PermissionError("Exact operator retry request required")
        trusted=request["trusted"];workspace=request["workspace"];owner=self.scheduler.owner(trusted,workspace)
        self.scheduler.journal_path(owner,args["request_id"],"failed-image-retry")
        packaging_only=args["action"]=="retry_packaging_release"
        request_key=("packaging-retry-" if packaging_only else "failed-image-retry-")+hashlib.sha256(canonical([owner,args["task_id"],args["job_id"],args["request_id"]])).hexdigest()
        with self.lock:
            prior=self.jobs.get(args["job_id"])
            existing=next((row for row in self.jobs.values()if row.get("request_key")==request_key),None)
            identity=str(uuid.uuid5(uuid.NAMESPACE_URL,request_key))
            durable=self.state/(identity+".json");loaded_from_disk=False
            if existing is None and (durable.exists()or durable.is_symlink()):
                fd=os.open(durable,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC)
                try:
                    meta=os.fstat(fd)
                    if not stat.S_ISREG(meta.st_mode)or meta.st_nlink!=1 or meta.st_size>4*1024*1024:raise ValueError("Invalid durable retry receipt")
                    with os.fdopen(fd,"rb",closefd=False)as stream:existing=json.loads(stream.read(4*1024*1024+1));loaded_from_disk=True
                finally:os.close(fd)
            if existing:
                if existing.get("request_key")!=request_key or existing.get("job_id")!=identity or existing["body_digest"]!=hashlib.sha256(canonical(args)).hexdigest():raise ValueError("Retry request ID reused with changed input")
                if existing.get("status")=="preparing_retry"or(loaded_from_disk and existing.get("status")=="queued"and not existing.get("effects_started")):return self.finish_retry_registration(request,args,existing)
                return self._view(existing)
            item=self.scheduler.item(args,trusted,workspace)
            state=self.scheduler.ensure(item,trusted,workspace)
            if item.get("status")!="closed" or item.get("job_id")!=args["job_id"] or not prior or not self._owned(prior,trusted,workspace):
                raise ValueError("Owned failed release and closed task required")
            if state["revision"]!=args["expected_revision"] or state["epoch"]!=args["expected_epoch"] or state["state"]!="owned":
                raise ValueError("Recover and reserve the current task generation first")
            if state["generation"]!=prior.get("source_generation"):
                raise ValueError("Task source generation changed after image build")
            scopes=self.scheduler.components(item).scopes(prior.get("changed_paths",()))
            self.scheduler.store.assert_owned(item["key"],owner,state["epoch"],scopes)
            if prior.get("status")!="failed" or prior.get("effects_started") or prior.get("operation")!="build_deploy":
                raise ValueError("Only failed pre-activation image jobs may be retried")
            if not self.package_cache.enabled:raise ValueError("Verified dependency cache is not enabled")
            deadline,fingerprint=self._incoming_lease(trusted)
            old_folder=self.state/prior["job_id"]
            candidate=(old_folder/"candidate.tar").read_bytes()
            if context_digest(candidate)[0]!=prior["inputs_sha256"]:
                raise ValueError("Failed candidate changed")
            private_source=release.snapshot(Path(item["workspace_path"]).parent,TASK_CONTEXT,context_digest)
            if context_digest(private_source)[0]!=item["inputs_sha256"]:
                raise ValueError("Original task snapshot changed")
            live=self._current_release_blob()
            merged,changed=release.candidate((old_folder/"base.tar").read_bytes(),(old_folder/"task.tar").read_bytes(),live,context_digest)
            report=self._sync_report(live,merged,changed)
            if report["total"] and (args.get("sync_decision")!="proceed" or args.get("expected_sync_digest")!=report["digest"]):
                return {"status":"needs_decision","synchronization":report}
            if args.get("sync_decision") not in (None,"proceed"):
                raise ValueError("Only an exact proceed decision may retry")
        # A disk/admission failure after a requested cached rebuild has no image
        # pair of its own. It may continue only through an explicit, verified
        # predecessor lineage; the new attempt still rebuilds its exact candidate.
        lineage=None;packaging_resume=None
        if packaging_only:
            if context_digest(merged)[0]!=prior["inputs_sha256"]:
                return {"status":"needs_rebuild","reason":"Current release changes the frozen source; packaging-only retry cannot reuse this compiler certificate","merged_inputs_sha256":context_digest(merged)[0]}
            packaging_resume=image_reuse_module.packaging_checkpoint(self.runner,prior,candidate)
            mode="packaging_resume"
        elif prior.get("retry_mode")=="cached_rebuild" and not prior.get("images") and not prior.get("legacy_image_proof"):
            lineage=image_reuse_module.cached_rebuild_lineage(prior,self.jobs,args.get("lineage_job_id"))
            mode="cached_rebuild"
        else:
            # The merge may change source without changing this task's own files.
            # Reuse only exact-bound images; a fresh cached build requires opt-in.
            mode=image_reuse_module.failed_retry_mode(prior,context_digest(merged)[0],args.get("rebuild_if_stale",False))
        if mode=="needs_rebuild":
            return {"status":"needs_rebuild","reason":"Merged source differs from retained image binding; request rebuild_if_stale to compile and package with verified cache",
                    "merged_inputs_sha256":context_digest(merged)[0]}
        if mode=="packaging_resume":
            self.package_cache.ready(candidate)
            proof=None
        elif mode=="cached_rebuild":
            if lineage is not None:
                inherited=lineage["legacy_image_proof"]
                source=self.jobs.get(inherited["source_job_id"])
                if source is None:raise ValueError("Verified lineage source unavailable")
                source_blob=(self.state/source["job_id"]/"candidate.tar").read_bytes()
                verified=image_reuse_module.legacy_pair(self.runner,source,source_blob)
                if verified!=inherited:raise ValueError("Verified lineage evidence changed")
            self.package_cache.ready(merged)
            proof=None
        elif mode=="reuse":
            self.package_cache.ready(candidate)
            proof=image_reuse_module.legacy_pair(self.runner,prior,candidate)
        else:
            inherited=prior["legacy_image_proof"]
            original=self.jobs.get(inherited["source_job_id"])
            if original is None:raise ValueError("Original image job unavailable")
            original_blob=(self.state/original["job_id"]/"candidate.tar").read_bytes()
            self.package_cache.ready(candidate)
            proof=image_reuse_module.legacy_pair(self.runner,original,original_blob)
            if proof!=inherited:raise ValueError("Inherited image evidence changed")
        with self.lock:
            state=self.scheduler.store.get(item["key"])
            if item.get("job_id")!=prior["job_id"] or state["revision"]!=args["expected_revision"] or state["epoch"]!=args["expected_epoch"] or state["state"]!="owned":
                raise ValueError("Ownership changed during image verification")
            self.scheduler.store.assert_owned(item["key"],owner,state["epoch"],scopes)
            if self._maintenance()["drained"]:raise ValueError("Maintenance drain prevents release retry")
            identity=str(uuid.uuid5(uuid.NAMESPACE_URL,request_key))
            folder=self.state/identity
            preparation=image_reuse_module.prepare_retry_archives(self.state,self.work_dir,identity,old_folder,
                hashlib.sha256(canonical(args)).hexdigest(),self.scheduler.write_record,recover_legacy=args.get("recover_preparation",False))
            job={"job_id":identity,"request_key":request_key,"body_digest":hashlib.sha256(canonical(args)).hexdigest(),
                 "user_id":trusted["user_id"],"workspace":workspace,"task_id":item["task_id"],"operation":"build_deploy",
                 "isolated":True,"status":"preparing_retry","phase":"retry_prepared","created_at":time.time(),
                 "retry_of":prior["job_id"],"retry_preparation":preparation,
                 "inputs_sha256":prior["inputs_sha256"],"base_inputs_sha256":prior["base_inputs_sha256"],
                 "changed_paths":prior["changed_paths"],"release_root":prior["release_root"],
                 "publication_policy_version":prior.get("publication_policy_version",0),
                 "compiler_memory_cap":prior.get("compiler_memory_cap"),"package_memory_cap":prior.get("package_memory_cap"),
                 "images":{},"log":"","tool_name":"build_work","lease_until":max(deadline,time.time()+4*3600),
                 "access_fingerprint":fingerprint,"revoked":False,
                 "ownership_id":item["key"],"ownership_epoch":state["epoch"],"source_generation":state["generation"],
                 "synchronization":report,"legacy_image_proof":proof,"retry_mode":mode,"cached_rebuild_lineage":lineage,"packaging_resume":packaging_resume}
            if args.get("sync_decision")=="proceed":job["sync_ack_digest"]=report["digest"]
            # A durable, non-executable receipt precedes the task linkage and queue admission.
            self._save(job)
            return self.finish_retry_registration(request,args,job)

    def prepare_dependency_cache(self,request,args):
        """Operator schedules pinned runtime dependency preparation before an application build."""
        if set(args)-{"action","task_id","request_id","refresh"} or not {"action","task_id","request_id"}<=set(args) or not request["trusted"].get("local_caller") or type(args.get("refresh",False))is not bool:
            raise PermissionError("Bounded operator cache preparation required")
        trusted=request["trusted"];workspace=request["workspace"];owner=self.scheduler.owner(trusted,workspace)
        self.scheduler.journal_path(owner,args["request_id"],"dependency-cache")
        request_key="dependency-cache-"+hashlib.sha256(canonical([owner,args["task_id"],args["request_id"],args.get("refresh",False)])).hexdigest()
        with self.lock:
            deadline,fingerprint=self._incoming_lease(trusted)
            item=self.scheduler.item(args,trusted,workspace)
            previous=next((j for j in self.jobs.values()if j.get("request_key")==request_key),None)
            if previous:return self._view(previous)
            if item["status"]!="open" or not self.resources.enabled:raise ValueError("Open owned task and enforced resource admission required")
            state=self.scheduler.ensure(item,trusted,workspace)
            self.scheduler.store.assert_owned(item["key"],owner,item["ownership_epoch"],["module:broker"])
            if self._maintenance()["drained"]:raise ValueError("Broker maintenance drain prevents cache preparation")
            blob=self._current_release_blob()
            identity=str(uuid.uuid4());event=threading.Event()
            job={"job_id":identity,"task_id":item["task_id"],"request_key":request_key,"user_id":trusted["user_id"],"workspace":workspace,
                 "operation":"check","status":"running","phase":"preparing_dependency_cache","created_at":time.time(),"started_at":time.time(),
                 "lease_until":deadline,"access_fingerprint":fingerprint,"ownership_id":item["key"],"ownership_epoch":state["epoch"],"source_generation":state["generation"],
                 "inputs_sha256":context_digest(blob)[0],"images":{},"log":"","phase_history":[],"summary":"Prepare pinned runtime dependencies",
                 "rust_tests":{"status":"not_required"},"dependency_refresh":args.get("refresh",False)}
            self.jobs[identity]=job;self.active_jobs.add(identity);self.active=next(iter(self.active_jobs))
            self.job_cancellations[identity]=event;self._save(job)
            self.pool.submit(self._prepare_dependency_cache_job,job,blob,event)
            return self._view(job)

    def _prepare_dependency_cache_job(self,job,blob,event):
        self.runner.job_context.job=job;self.runner.job_context.event=event
        try:
            self._lease(job)
            job["dependency_bases"]=self.package_cache.prewarm(job,blob,refresh=job["dependency_refresh"])
            self._lease(job)
            job["log"] = scrub(job.get("log", "") + self.package_cache.verify_consumption(job,blob), self.runner.secrets)
            self._lease(job)
            job.update(status="completed",phase="finished",reason="Pinned runtime bases and offline packaging consumption verified; application containers unchanged")
        except BaseException as error:
            job.update(status="cancelled"if isinstance(error,PermissionError)else"failed",phase="finished",reason=scrub(str(error),self.runner.secrets))
        finally:
            self.runner.job_context.job=None;self.runner.job_context.event=None
            with self.lock:
                job["finished_at"]=time.time();self._save(job);self.active_jobs.discard(job["job_id"])
                self.active=next(iter(self.active_jobs),None);self.job_cancellations.pop(job["job_id"],None)

    def provision_ui_test_runtime(self,request,args):
        """Create a durable operator check for the fixed ancillary browser runtime, never deploy it."""
        if set(args)!={"action","task_id","request_id"}or not request["trusted"].get("local_caller"):raise PermissionError("Operator UI test provisioning required")
        trusted=request["trusted"];workspace=request["workspace"]
        owner=self.scheduler.owner(trusted,workspace)
        # Reuse the normal journal's bounded identifier validation.
        self.scheduler.journal_path(owner,args["request_id"],"ui-test-runtime")
        request_key="ui-test-runtime-"+hashlib.sha256(canonical([owner,args["task_id"],args["request_id"]])).hexdigest()
        with self.deploy_lock:
            with self.lock:
                deadline,fingerprint=self._incoming_lease(trusted)
                item=self.scheduler.item(args,trusted,workspace)
                previous=next((j for j in self.jobs.values()if j.get("request_key")==request_key),None)
                if previous:return {"job_id":previous["job_id"],"status":previous["status"],"ui_test_runtime":previous.get("ui_test_runtime")}
                maintenance=self._maintenance()
                if not maintenance["drained"]or maintenance.get("task_id")!=item["task_id"]or self.active_jobs or any(j["status"]in {"queued","running","needs_decision"}for j in self.jobs.values())or not self.resources.enabled:
                    raise ValueError("Idle task-owned drain and enforced resource admission required")
                if item["status"]!="open":raise ValueError("Open broker task required")
                state=self.scheduler.ensure(item,trusted,workspace)
                self.scheduler.store.assert_owned(item["key"],owner,item["ownership_epoch"],["module:broker"])
                identity=str(uuid.uuid4());event=threading.Event()
                job={"job_id":identity,"task_id":item["task_id"],"request_key":request_key,"user_id":trusted["user_id"],"workspace":workspace,
                     "operation":"check","status":"running","phase":"preparing_ui_test_runtime","created_at":time.time(),"started_at":time.time(),
                     "lease_until":deadline,"access_fingerprint":fingerprint,"ownership_id":item["key"],"ownership_epoch":state["epoch"],"source_generation":state["generation"],
                     "inputs_sha256":hashlib.sha256((Path(CONFIG["service_dir"])/"ui-test.Dockerfile").read_bytes()).hexdigest(),"images":{},"log":"","phase_history":[],
                     "summary":"Provision isolated frontend test runtime","retain_ui_runtime":True,"package_memory_cap":4026531840,"rust_tests":{"status":"not_required"}}
                self.jobs[identity]=job;self.active_jobs.add(identity);self.active=next(iter(self.active_jobs));self.job_cancellations[identity]=event;self._save(job)
            self.runner.job_context.job=job;self.runner.job_context.event=event
            try:
                self._lease(job)
                job["ui_test_runtime"]=self.runner.prepare_ui_test_runtime(job)
                self._lease(job)
                job.update(status="completed",phase="finished",reason="Ancillary verification image prepared; application containers unchanged")
            except Exception as error:
                job.update(status="cancelled"if isinstance(error,PermissionError)else"failed",phase="finished",reason=scrub(str(error),self.runner.secrets))
            finally:
                self.runner.job_context.job=None;self.runner.job_context.event=None
                with self.lock:
                    job["finished_at"]=time.time();self._save(job);self.active_jobs.discard(identity);self.active=next(iter(self.active_jobs),None);self.job_cancellations.pop(identity,None)
            return {"job_id":identity,"status":job["status"],"ui_test_runtime":job.get("ui_test_runtime")}

    # @code-index {"version":1,"feature":"partitioned-review-storage","aliases":["review ledger migration"],"purpose":"Authorize explicit lossless partitioning during an idle task-owned maintenance drain.","span_lines":40,"configuration":{"status":"not_applicable","rationale":"Idle migration, exact ownership and lossless storage switching are fixed host integrity gates."}}
    def maintain_review_storage(self,request,args):
        """Run explicit lossless storage maintenance during an idle task-owned drain."""
        if set(args)!={"action","task_id","request_id"}or not request["trusted"].get("local_caller"):
            raise PermissionError("Operator history maintenance required")
        partition=args["action"]=="partition_review_storage"
        trusted=request["trusted"];workspace=request["workspace"]
        with self.deploy_lock,self.lock:
            self._incoming_lease(trusted)
            item=self.scheduler.item(args,trusted,workspace)
            owner=self.scheduler.owner(trusted,workspace)
            legacy_receipt=self.scheduler.journal_path(owner,args["request_id"],"review-partition"if partition else"index-history")
            folder=self.work_dir/"review-storage-requests"
            if folder.is_symlink():raise PermissionError("Storage journal directory symlink")
            folder.mkdir(mode=0o700,exist_ok=True)
            # Storage housekeeping is not an ownership transition. A failed copy
            # must retain its retry receipt without fencing unrelated source edits.
            receipt=legacy_receipt if legacy_receipt.exists()else folder/legacy_receipt.name
            digest=hashlib.sha256(canonical(args)).hexdigest()
            if receipt.exists():
                old=json.loads(receipt.read_bytes())
                if old["digest"]!=digest:raise ValueError("History request_id reused")
                if old.get("state")=="done":return old["result"]
            maintenance=self._maintenance()
            if not maintenance["drained"]or maintenance.get("task_id")!=item["task_id"]or self.active_jobs or any(j["status"]in {"queued","running","needs_decision"}for j in self.jobs.values())or self.code_index_leases or self.index_coordinator.active:
                raise ValueError("Idle task-owned drain with no active review or checkpoint required")
            def authorize():
                """Keep maintenance bound to live host authority and held index ownership."""
                self._incoming_lease(trusted)
                self.scheduler.store.assert_owned(item["key"],owner,item["ownership_epoch"],["module:broker"if partition else"module:ability:Administrator/code_index"])
            authorize()
            self.scheduler.write_record(receipt,{"digest":digest,"task_key":item["key"],"state":"pending","request_id":args["request_id"]})
            result=(self.code_index_ledger.partition_storage(authorize)if partition else self.code_index_ledger.compact_history(authorize))
            self.scheduler.write_record(receipt,{"digest":digest,"task_key":item["key"],"state":"done","request_id":args["request_id"],"result":result})
            return result

    def compact_index_history(self,request,args):
        """Preserve the existing bounded history-maintenance entry point."""
        return self.maintain_review_storage(request,args)

    def code_index_maintenance(self,args,trusted,workspace):
        """Record private or approved immutable-release assertions without a publication lock."""
        # Private task records are independently source-bound. A stale published
        # projection can miss reuse, but matching hashes are still required to
        # inherit evidence; final publication reconciles and verifies the exact pair.
        # Never wait for deployment merely to record or read private task evidence.
        actor={"user_id":trusted["user_id"],"session_id":workspace,"transport":"operator"if trusted.get("local_caller")else"webagent"}
        action=args.get("action")
        if args.get("root","live")!="live":
            if args.get("root")!="published"or action not in {"list","get"}:raise ValueError("CLI snapshot reads require root published; private roots use WebAgent workspace tools")
            return self.code_index_ledger.scanner.execute_published("code_index_maintenance",args,ASSET_ROOT/"releases"/self.runner.current_asset_id())
        if action in {"list","get"}:
            if not any(args.get(k)and args[k]!="published"for k in ("task_key","task_id")):
                self._code_index_reconcile()
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
        prepared_id=args.get("prepared_job_id")
        prepared=self.jobs.get(prepared_id)if prepared_id is not None else None
        deployed_id=args.get("deployed_job_id")
        deployed=self.jobs.get(deployed_id)if deployed_id is not None else None
        implicit_deployed=False
        if deployed_id is None and prepared_id is None and item and item["status"]=="closed":
            latest=self.jobs.get(item.get("job_id"))
            if latest and latest.get("status")=="completed"and latest.get("deferred_documentation"):
                deployed=latest;deployed_id=latest["job_id"];implicit_deployed=True
        if deployed_id is not None and(prepared_id is not None or not(trusted.get("local_caller")or implicit_deployed)or not deployed or not self._owned(deployed,trusted,workspace)or deployed.get("ownership_id")!=key or item is None or item.get("job_id")!=deployed_id or deployed["status"]!="completed"or deployed.get("synchronization",{}).get("status")!="synchronized"or not deployed.get("deferred_documentation")):raise PermissionError("Owned approved deployed review candidate required")
        if prepared_id is not None and(not trusted.get("local_caller")or not prepared or not self._owned(prepared,trusted,workspace)or prepared.get("ownership_id")!=key or prepared["status"]!="needs_decision"or prepared["phase"]!="awaiting_prepared_reviews"):raise PermissionError("Owned sealed review candidate required")
        if not item or item.get("mode")!="isolated"or(item["status"]!="open"and prepared is None and deployed is None):raise ValueError("Open task or sealed prepared review candidate required")
        if trusted.get("lease_until",0)<=time.time():raise PermissionError("Maintenance authority expired")
        source=Path(prepared["prepared_assets"])/"v3"if prepared is not None else self.work_dir/"trees"/key/"v3"
        if deployed is not None:source=ASSET_ROOT/"releases"/deployed_id/"v3"
        if source.is_symlink()or not source.is_dir():raise ValueError("Maintenance source unavailable")
        if deployed is not None and not self.review_policy.allows(deployed,source.parent):raise PermissionError("Deployed documentation approval changed")
        if prepared is not None:release.verify_index(source.parent,prepared_id,prepared["inputs_sha256"])
        lease={"user_id":trusted["user_id"],"workspace":workspace,"lease_until":trusted.get("lease_until",0),"access_fingerprint":trusted.get("access_fingerprint"),"revoked":False}
        lease["task_key"]=key
        if deployed is not None:lease["deployed_job_id"]=deployed_id
        if prepared is not None:lease["prepared_job_id"]=prepared_id
        args={k:v for k,v in args.items()if k not in {"prepared_job_id","deployed_job_id"}}
        lease_key=(trusted["user_id"],workspace,args.get("request_id"),uuid.uuid4().hex)
        with self.lock:
            if deployed is not None:
                deployed["documentation_review_revision"]=deployed.get("documentation_review_revision",0)+1
                deployed["deferred_documentation"].pop("completion",None)
                self._save(deployed)
            self.code_index_leases[lease_key]=lease
        context=getattr(self.runner,"job_context",None)
        previous=getattr(context,"job",None)if context is not None else None
        if context is not None and action=="sync":
            lease.update(job_id=str(uuid.uuid4()),internal_index=True)
            context.job=lease
        try:
            result=self.code_index_ledger.mutate(args,actor,key,item["task_id"],source,authorize=lambda:self._code_index_authorize(lease,key))
        finally:
            if context is not None:context.job=previous
            with self.lock:self.code_index_leases.pop(lease_key,None)
        if lease.get("lease_until",0)<=time.time()or lease.get("revoked"):raise PermissionError("Maintenance authority expired; inspect the original request receipt")
        result.update(source_kind="untrusted_maintenance_metadata",completion_is_review_assertion=True)
        if deployed is not None and action=="record"and args.get("status")=="complete"and result.get("record",result).get("unit_id")=="__file__":
            self.review_policy.complete_if_ready(deployed)
        return result

    def reload_memory_policy(self,args,trusted):
        """Adopt only protected memory fields after the root helper confirms the same policy."""
        if not trusted.get("local_caller")or set(args)!={"action","expected_policy_sha256","policy_sha256"}:raise PermissionError("Protected operator memory reload required")
        fresh=_config_module.load()
        if {k:v for k,v in fresh.items()if k!="scheduler"}!={k:v for k,v in CONFIG.items()if k!="scheduler"}or {k:v for k,v in fresh["scheduler"].items()if k not in _config_module.MEMORY_KEYS}!={k:v for k,v in CONFIG["scheduler"].items()if k not in _config_module.MEMORY_KEYS}:raise ValueError("Other installation changes require maintenance")
        expected=_config_module.memory_policy_digest(fresh["scheduler"])
        if args["policy_sha256"]!=expected:raise ValueError("Protected memory policy changed")
        proof=self.runner.resource_request({"operation":"memory_policy"})
        if proof["sha256"]!=expected or proof["values"]!=_config_module.memory_policy(fresh["scheduler"]):raise ValueError("Broker/helper memory policy mismatch")
        result=self.resources.adopt_memory_policy(fresh["scheduler"],args["expected_policy_sha256"])
        CONFIG["scheduler"].update(proof["values"])
        return result

    def cargo_request(self,args,trusted,workspace):
        """Capture under the coordinator lock; compile an isolated copy outside it and attest unchanged inputs."""
        if set(args)-{"task_id","task","expected_inputs_sha256","timeout_seconds","request_id","compiler_memory_cap","test_filter","feature_profile"}or not{"task_id","task","expected_inputs_sha256"}<=set(args):
            raise ValueError("Task-scoped Cargo check arguments required")
        task_id,task=args["task_id"],args["task"];expected=args["expected_inputs_sha256"];timeout=args.get("timeout_seconds",600)
        if not isinstance(task_id,str)or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",task_id)or task not in {"check","test","resource_probe"}or not isinstance(expected,str)or not re.fullmatch(r"[a-f0-9]{64}",expected)or type(timeout)is not int or not 1<=timeout<=600:
            raise ValueError("Bounded task Cargo request required")
        request_id=args.get("request_id")
        if request_id is not None and (not isinstance(request_id,str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}",request_id)):
            raise ValueError("Stable check request ID required")
        request_key="check-"+hashlib.sha256(canonical([trusted["user_id"],workspace,task_id,request_id])).hexdigest() if request_id else None
        if task=="resource_probe"and not(trusted.get("local_caller")and self.resources.enabled):raise PermissionError("Resource probe requires enforced operator channel")
        profile=executor_module.check_profile(args.get("feature_profile"))
        if profile!="default":
            if task!="check":raise ValueError("Feature profiles apply only to compile checks")
            if not self.resources.enabled:raise PermissionError("Live feature checks require enforced resource admission")
        selected=executor_module.test_filter(args.get("test_filter"))
        if selected is not None and (task!="test"or trusted.get("local_caller")is not True):raise PermissionError("Filtered tests require a local operator test request")
        cap=executor_module.compiler_cap(args.get("compiler_memory_cap"))
        if cap is not None and (trusted.get("local_caller")is not True or not self.resources.enabled):raise PermissionError("Enforced compiler cap requires local operator")
        signature=[task_id,task,expected,timeout]if cap is None else[task_id,task,expected,timeout,cap]
        if selected is not None:signature+=[selected]
        if profile!="default":signature+=[{"feature_profile":profile}]
        request_signature=hashlib.sha256(canonical(signature)).hexdigest()
        with self.lock:
            self._incoming_lease(trusted)
            if request_key:
                previous=next((j for j in self.jobs.values() if j.get("request_key")==request_key),None)
                if previous:
                    if previous.get("check_signature")!=request_signature:raise ValueError("Check request ID reused with different inputs")
                    return (previous.get("check_result") if previous["status"] not in {"queued","running"} else None) or {"task_id":task_id,"job_id":previous["job_id"],"task":task,"feature_profile":previous.get("feature_profile","default"),"status":previous["status"],"inputs_sha256":previous["inputs_sha256"],"log_excerpt":scrub(previous.get("log",""))[-4096:]}
            # Retry signatures describe caller input, not a subsequently changed default.
            cap=executor_module.compiler_cap(args.get("compiler_memory_cap"),CONFIG.get("scheduler",{}))
            if cap is None and self.resources.enabled:cap=CONFIG.get("scheduler",{}).get("compiler_memory_default_bytes",5*1024**3)
            if not self._maintenance_allowed(trusted,{"task_id":task_id}):raise PermissionError("Build service drained")
            self._incoming_lease(trusted)
            key=hashlib.sha256(canonical([trusted["user_id"],workspace,task_id])).hexdigest();item=self.work.get(key)
            if not item or item.get("mode")!="isolated"or item["status"]!="open":raise ValueError("Open isolated task required")
            root=self.work_dir/"trees"/key;blob=release.snapshot(root,TASK_CONTEXT,context_digest);before=context_digest(blob)[0]
            if before!=expected:raise ValueError("Stale task source inputs")
            if any(j.get("task_id")==task_id and self._owned(j,trusted,workspace)and j["status"]in {"running","queued"}and (j["operation"]!="check"or self.scheduler.mode!="enforce"or not request_id)for j in self.jobs.values()):
                raise ValueError("This task already has pending execution")
            ownership_value=None
            if self.scheduler.mode=="enforce":
                self.scheduler.submit(item,trusted,workspace,["build-service/resources.py"]if task=="resource_probe"else["src","Cargo.toml","Cargo.lock"],before)
                ownership_value=self.scheduler.store.get(key)
            job_id=str(uuid.uuid4());event=threading.Event();self.job_cancellations[job_id]=event
            job={"job_id":job_id,"request_key":request_key or "check-"+job_id,"check_signature":request_signature,"task_id":task_id,"user_id":trusted["user_id"],"workspace":workspace,
                "operation":"check","status":"queued","phase":"queued_check","created_at":time.time(),"started_at":None if request_id else time.time(),
                "inputs_sha256":before,"lease_until":time.time()+timeout+120,"access_fingerprint":trusted["access_fingerprint"],
                "images":{},"log":"","phase_history":[],"summary":"Isolated Cargo "+task,"compiler_memory_cap":cap,"test_filter":selected,"feature_profile":profile}
            if ownership_value:
                job.update(ownership_id=key,ownership_epoch=ownership_value["epoch"],source_generation=ownership_value["generation"])
            self.jobs[job_id]=job;self._save(job)
            item["last_check_job_id"]=job_id
            item["last_activity_at"]=time.time();item.pop("leftover_notice",None);self._save_work()
            self.check_runs[job_id]=(job,item,root,blob,before,task,timeout,event)
            self._start_next()
        if request_id:
            return {"task_id":task_id,"job_id":job_id,"task":task,"feature_profile":profile,"status":"queued","inputs_sha256":before,"log_excerpt":""}
        # Legacy synchronous callers wait outside the coordination lock.
        deadline=time.monotonic()+timeout
        while time.monotonic()<deadline:
            with self.lock:
                if job.get("check_result"):return job["check_result"]
                if job["status"]not in {"queued","running"}:
                    return {"task_id":task_id,"job_id":job_id,"task":task,"feature_profile":profile,"status":job["status"],"inputs_sha256":before,"log_excerpt":""}
            time.sleep(0.05)
        raise TimeoutError("Check remains queued or running; inspect the existing job receipt")

    def _execute_check(self,job,item,root,blob,before,task,timeout,event):
        """Execute a captured check outside coordination; interrupted jobs require an explicit new request."""
        job_id=job["job_id"];task_id=job["task_id"]
        with self.lock:
            job.update(status="running",phase="checking_source",started_at=time.time());self._save(job)
        try:
            self._lease(job)
            if event.is_set():raise PermissionError("Check cancelled before execution")
            if context_digest(release.snapshot(root,TASK_CONTEXT,context_digest))[0]!=before:
                raise ValueError("Task changed before check execution")
            if hasattr(self.runner,"job_context"):self.runner.job_context.job=job;self.runner.job_context.event=event
            if self.resources.enabled:
                log=self.runner.executor.probe(job)if task=="resource_probe"else self.runner.executor.check(job,blob,task,timeout,lambda p,b:release.extract(p,b,context_digest))
            else:log=self.runner.cargo_task(root,self.state/"checks"/job_id,task,timeout)
            status="completed"
        except Exception as error:
            log=scrub(str(error));status="cancelled"if isinstance(error,PermissionError)else"failed"
        finally:
            if hasattr(self.runner,"job_context"):self.runner.job_context.job=None;self.runner.job_context.event=None
        with self.lock:
            after=context_digest(release.snapshot(root,TASK_CONTEXT,context_digest))[0]
            if after!=before:status="stale"
            job.update(status=status,phase="finished",finished_at=time.time(),log=scrub(log)[-MAX_LOG:],reason="Inputs unchanged"if after==before else"Task changed during check")
            job["rust_tests"]={"status":("passed" if status=="completed" else status) if task=="test" else "not_run","check":task,"feature_profile":job.get("feature_profile","default")}
            job["check_result"]={"task_id":task_id,"job_id":job_id,"task":task,"feature_profile":job.get("feature_profile","default"),"status":status,"inputs_sha256":after,"log_excerpt":scrub(log)[-4096:]}
            job.setdefault("phase_history",[]).append({"phase":"finished","at":job["finished_at"]})
            self._save(job);self.job_cancellations.pop(job_id,None);item["last_activity_at"]=time.time();self._save_work()
            self.active_jobs.discard(job_id);self.active=next(iter(self.active_jobs),None)
            self._start_next()
        return job["check_result"]

    def handle(self, request):
        """Authorize broker operations and classify actual compiler consumers before a frozen release is admitted."""
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
        if name=="v3_cargo_task":return self.cargo_request(args,trusted,workspace)
        if name=="build_work"and args.get("action") in {"retry_verified_failed_release","retry_packaging_release"}:
            return self.retry_verified_failed_release(request,args)
        if name=="build_work"and args.get("action")=="dependency_cache_status":
            if set(args)!={"action"} or not trusted.get("local_caller"):raise PermissionError("Operator dependency cache read required")
            return self.package_cache.status()
        if name=="build_work"and args.get("action")=="prepare_dependency_cache":return self.prepare_dependency_cache(request,args)
        if name=="build_work"and args.get("action")=="prepare_ui_test_runtime":return self.provision_ui_test_runtime(request,args)
        if name=="build_work"and args.get("action")in self.review_policy.actions:return self.review_policy.handle(request,args)
        if name=="build_work" and args.get("action") in self.maintenance_activities.actions:return self.maintenance_activities.handle(request,args)
        if name=="build_work"and args.get("action")=="retry_post_deployment":return self.post_deployment.retry(request,args)
        if name=="build_work"and args.get("action")in {"compact_index_history","partition_review_storage"}:return self.maintain_review_storage(request,args)
        if name=="build_work"and args.get("action")in self.index_coordinator.actions:return self.index_coordinator.handle(request,args)
        if name=="build_work"and args.get("action")in self.ui_publication.recovery_actions:return self.ui_publication.recover(request,args)
        if name=="build_work"and args.get("action")=="ui_rollback_preview":
            result=dict(self.ui_publication.rollback_preview(request,args))
            result["patch"]=checked_git_modes(result["patch"])
            return result
        if name=="build_work"and args.get("action")=="check":
            if set(args)-{"action","task_id","check","expected_inputs_sha256","timeout_seconds","request_id","compiler_memory_cap","test_filter","feature_profile"}:raise ValueError("Unsupported check arguments")
            return self.cargo_request({"task_id":args.get("task_id"),"task":args.get("check","check"),"expected_inputs_sha256":args.get("expected_inputs_sha256"),"timeout_seconds":args.get("timeout_seconds",600),**({"request_id":args["request_id"]} if "request_id" in args else {}),**({"compiler_memory_cap":args["compiler_memory_cap"]}if "compiler_memory_cap"in args else{}),**({"test_filter":args["test_filter"]}if "test_filter"in args else{}),**({"feature_profile":args["feature_profile"]}if "feature_profile"in args else{})},trusted,workspace)
        with self.lock:
            if name == "build_service_maintenance":
                if not trusted.get("local_caller") or set(args)-{"action","task_id"} or args.get("action") not in {"status","drain","resume"}:raise PermissionError("Operator maintenance required")
                if args["action"]!="status":
                    state={"drained":args["action"]=="drain","task_id":args.get("task_id") if args["action"]=="drain" else None}
                    temporary=self.maintenance_file.with_suffix(".tmp")
                    fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600)
                    with os.fdopen(fd,"w") as stream:json.dump(state,stream);stream.flush();os.fsync(stream.fileno())
                    os.replace(temporary,self.maintenance_file)
                    if not state["drained"]:
                        self._start_next();self.post_deployment.start()
                return {**self._maintenance(),"busy":bool(self.active or self.active_ui or self.post_deployment.active or self.maintenance_activities.active or any(j["status"]=="running"for j in self.jobs.values())),"queued":sum(j["status"]=="queued" for j in self.jobs.values())}
            read_actions={"heartbeat","status","review","preflight","receipt","list","get","read","search","job","artifact","reservations","resources","takeover_preview"}
            read_names={"v3_build_status","v3_worker_job","v3_build_dashboard","v3_build_result","administrator_host_lease","administrator_host_revoke","v3_build_cancel","v3_build_leftovers","v3_source_list","v3_source_read","v3_source_read_batch","v3_source_search","v3_source_receipt","file_list","file_read","file_search"}
            if name not in read_names and args.get("action") not in read_actions and not self._maintenance_allowed(trusted,args):raise PermissionError("Build service drained; new work disabled")
            if name == "performance_work":
                return self.performance.handle(trusted, workspace, args)
            if name == "v3_build_scheduler":
                if trusted["user_id"]!="admin"or workspace!="00000000-0000-0000-0000-000000000000":raise PermissionError("Administrator scheduler denied")
                self._incoming_lease(trusted)
                return self.scheduler.handle(args,trusted,workspace)
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
                    view["task_id"] = tasks.get(job["job_id"], {}).get("task_id") or job.get("task_id")
                    summary = tasks.get(job["job_id"], {}).get("summary") or job.get("summary")
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
                        "containers": containers, "synchronization": self._sync_report(live, live, ()),
                        "scheduler":self.scheduler.list(trusted,workspace,True),"resources":self.resources.snapshot()}
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
                view["resources"]=self.resources.snapshot(job["job_id"])
                return {"job": view}
            if name in ("administrator_host_lease", "administrator_host_revoke"):
                if args:
                    raise ValueError("internal lease control takes no arguments")
                incoming = self._incoming_lease(trusted) if name == "administrator_host_lease" else None
                updated = self.performance.leases(trusted, workspace, incoming)
                updated+=self.index_coordinator.update_leases(trusted,workspace,incoming)
                updated+=self.maintenance_activities.update_leases(trusted,workspace,incoming)
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
                    review_waiting=job.get("status")=="needs_decision"and job.get("phase")=="awaiting_prepared_reviews"
                    deferred_reviewing=job.get("status")=="completed"and bool(job.get("deferred_documentation"))and not job["deferred_documentation"].get("completion")
                    if not self._owned(job, trusted, workspace) or(job["status"]not in ("queued","running")and not review_waiting and not deferred_reviewing):
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
                    if review_waiting:self.scheduler.review_wait(job,activity=bool(incoming),authority_until=incoming[0]if incoming else None)
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
                allowed_dirs = {"crates", "src", "plugins", "tests", "ui", "docs", "config", "build-service"}
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
                self.scheduler.guard(item,trusted,workspace,paths,request_id)
                if isolated and context_digest(release.snapshot(tree,TASK_CONTEXT,context_digest))[0]!=current:
                    raise ValueError("Queued task refreshed to current source; reread before applying a patch")
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
                    self.scheduler.edited(item,trusted,workspace,request_id)
                self._save_work()
                return dict(receipt)
            if name == "v3_build_work":
                if set(args) - {"action", "task_id", "summary", "expected_inputs_sha256", "mode", "sync_decision", "expected_sync_digest", "offset", "compiler_memory_cap", "package_memory_cap"}:
                    raise ValueError("unsupported work argument")
                action = args.get("action")
                if action == "status":
                    if set(args) not in ({"action"}, {"action", "task_id"}):
                        raise ValueError("status accepts only an optional task_id")
                    selected = args.get("task_id")
                    if selected is not None and (not isinstance(selected, str) or not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", selected)):
                        raise ValueError("bounded task_id required")
                    own = [dict(task_id=i["task_id"], status=i["status"], summary=i["summary"],
                                inputs_sha256=i.get("inputs_sha256"), job_id=i.get("job_id"), last_check_job_id=i.get("last_check_job_id"),
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
                    if self.scheduler.mode=="enforce"and mode!="isolated":raise ValueError("Enforced ownership requires isolated source tasks")
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
                    self.scheduler.ensure(item,trusted,workspace)
                    return {"task_id": task_id, "status": "open", "open_count": len(self._open_work()),
                            "workspace_path": item.get("workspace_path"),
                            "workspace_inputs_sha256": item.get("base_inputs_sha256")}
                if action == "abandon":
                    if set(args) != {"action", "task_id"} or not item or item["status"] != "open":
                        raise ValueError("only an open task in this session may be abandoned")
                    item.update(status="abandoned", finished_at=time.time())
                    self._save_work()
                    return {"task_id": task_id, "status": "abandoned", "open_count": len(self._open_work())}
                if set(args) - {"action", "task_id", "expected_inputs_sha256", "sync_decision", "expected_sync_digest", "compiler_memory_cap", "package_memory_cap"} or not {"action", "task_id", "expected_inputs_sha256"} <= set(args):
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
                    self.scheduler.submit(item,trusted,workspace,changed,current)
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
                    docs_changed=any(path.startswith("v3/docs/")and path.endswith(".md")for path in changed)
                    core_inputs=set();docs_attested=False
                    if package_changed or docs_changed or any(path.startswith("v3/ui/")for path in changed):
                        try:
                            core_inputs=self.runner.core_compile_inputs()
                            docs_attested=isinstance(core_inputs,set)and "v3/src/main.rs"in core_inputs
                            if not isinstance(core_inputs,set):core_inputs=set()
                        except (AttributeError,OSError,RuntimeError,ValueError,TimeoutError):
                            core_inputs=set()
                    # Ordinary notes are assets only when installed compiler evidence excludes them.
                    ui_only = all((hot_source_path(path) or (
                        docs_attested and path.startswith("v3/docs/")and path.endswith(".md")) or (
                        (generic_package_path(path,candidate) or generic_package_path(path,base_blob))
                        and not native_package_migration(path,base_blob)))
                        and path not in core_inputs and not path.startswith(("v3/crates/context-contracts/","v3/crates/background-contracts/","v3/crates/boot-contracts/","v3/crates/assets-contracts/","v3/crates/capability-contracts/","v3/crates/provider-contracts/"))for path in changed)
                    if (package_changed or docs_changed)and not docs_attested:ui_only=False
                    if CONFIG.get("scheduler",{}).get("concurrent_ui_preparation",False)and any(path.startswith("v3/ui/")for path in changed)and not docs_attested:ui_only=False
                    engine_changed=any(engine_component_source_path(path)for path in changed)
                    if engine_changed and not self.runner.engine_hot_swappable():ui_only=False
                    index_only=ui_only and(not changed or any(path in INDEX_METADATA_FILES for path in changed))
                    package_cap=executor_module.package_cap(args.get("package_memory_cap"))
                    if package_cap is not None and (trusted.get("local_caller")is not True or not self.resources.enabled):raise PermissionError("Enforced package cap requires local operator")
                    cap=executor_module.compiler_cap(args.get("compiler_memory_cap"),CONFIG.get("scheduler",{}))
                    if cap is None and self.resources.enabled:cap=CONFIG.get("scheduler",{}).get("compiler_memory_default_bytes",5*1024**3)
                    if args.get("compiler_memory_cap")is not None and (trusted.get("local_caller")is not True or not self.resources.enabled):raise PermissionError("Enforced compiler cap requires local operator")
                    plugin_only=ui_only and any((path.startswith(ABILITY_ROOT) and path not in INDEX_METADATA_FILES) or generic_package_path(path,candidate) or generic_package_path(path,base_blob) or path.startswith("v3/config/") or path=="v3/src/agent_loop/tools/administrator-schemas.json" for path in changed)
                    host_only = self.host_source_only(current_changed,candidate,trusted)
                    lease_until, fingerprint = self._incoming_lease(trusted)
                    job_id = str(uuid.uuid4())
                    private = self.state / job_id
                    private.mkdir(mode=0o700)
                    (private / "base.tar").write_bytes(base_blob)
                    (private / "task.tar").write_bytes(task_blob)
                    job = {"job_id": job_id, "request_key": "task-" + key,
                           "body_digest": hashlib.sha256(canonical([task_id, current,cap,package_cap])).hexdigest(),
                           "user_id": trusted["user_id"], "workspace": workspace,
                           "publication_policy_version":1 if CONFIG.get("scheduler",{}).get("concurrent_ui_preparation",False)else 0, "operation": "host_service_deploy" if host_only else "plugin_deploy" if plugin_only else "index_deploy" if index_only else "ui_deploy" if ui_only else "build_deploy", "task_id":task_id, "isolated": True, "inputs_sha256": current,
                           "created_at": time.time(), "status": "queued", "phase": "waiting", "compiler_memory_cap":cap,"package_memory_cap":package_cap,
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
                    job.update(ownership_id=item["key"],ownership_epoch=item.get("ownership_epoch"),source_generation=item.get("source_generation",0))
                    self._save(job)
                    self.jobs[job_id] = job
                    item.update(status="closed", inputs_sha256=current, job_id=job_id,
                                finished_at=time.time())
                    self._save_work()
                    # These attested archives were persisted above; do not overlap their inspection buffers with build preparation.
                    del base_blob,task_blob,live,candidate
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

    def _rebase_after_ui(self, job, blob, tested_runtime=False, live_blob=None):
        live = self._current_release_blob()if live_blob is None else live_blob
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
                      if not hot_source_path(name)and not generic_package_path(name,blob)}
            after = {name: item for name, item in release.entries(merged, context_digest).items()
                     if not hot_source_path(name)and not generic_package_path(name,merged)}
            if before != after:
                delta={name for name in set(before)|set(after) if before.get(name)!=after.get(name)}
                # Only a fully installed host-only broker source release may rebase
                # a tested application image without rebuilding that image.
                if not self.host_source_only(delta,merged,{"local_caller":True}):
                    raise ValueError("runtime build inputs changed during UI release")
            old_ui = release.entries(blob, context_digest)
            new_ui = release.entries(merged, context_digest)
            if hasattr(self.runner,"core_compile_inputs")and any(old_ui.get(name)!=new_ui.get(name)for name in self.runner.core_compile_inputs()):
                raise ValueError("Compiler-attested runtime input changed during rebase")
            if any(old_ui.get(name) != new_ui.get(name) for name in RUST_UI_FIXTURES):
                raise ValueError("Rust test fixture changed during image build")
            job["built_inputs_sha256"] = job["inputs_sha256"]
            job["rust_tests"] = {"status": "passed",
                "summary": "Runtime inputs tested before UI asset rebase; UI assets validated separately"}
        elif not all(hot_source_path(path)or generic_package_path(path,blob)or generic_package_path(path,merged)for path in changed):
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
        with self.lock:
            job.update(inputs_sha256=digest, base_inputs_sha256=live_digest,
                       changed_paths=sorted(changed), release_root=str(release_root))
            for member_id in job.get("batch_members", []):
                member = self.jobs[member_id]
                (self.state / member_id / "candidate.tar").write_bytes(merged)
                member.update(inputs_sha256=digest, base_inputs_sha256=live_digest)
                self._save(member)
            self._save(job)
        return merged

    @contextlib.contextmanager
    def boot_effect_lease(self):
        """Serialize publication/container replacement against the trusted boot host."""
        path=Path(CONFIG["install_root"]).parent/"boot-supervisor/effects.lock"
        if not path.exists():
            if path.with_name("ui.sock").exists():raise ValueError("Boot effect lease missing")
            yield;return
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
        try:
            info=os.fstat(fd)
            if not stat.S_ISREG(info.st_mode)or info.st_uid!=0 or info.st_nlink!=1 or info.st_mode&0o022:raise ValueError("Boot effect lease identity")
            fcntl.flock(fd,fcntl.LOCK_EX)
            yield
        finally:os.close(fd)

    def _run_ui_hot(self, job, blob):
        """Compile independent packages outside publication, then rebase and revalidate under its short lock."""
        if any(engine_component_source_path(path)for path in job.get("changed_paths",[]))and not self.runner.engine_hot_swappable():
            raise ValueError("Independent engine replacement requires live mode; publish a coordinated core release")
        if self.scheduler.mode=="enforce"and job["operation"]=="plugin_deploy":
            self._phase(job,"compiling_plugins")
            job["plugin_artifacts"]=self.runner.build_ability_artifacts(job["job_id"],source_root=Path(job["release_root"]))
            job["engine_artifacts"]=self.runner.build_engine_artifacts(job["job_id"],source_root=Path(job["release_root"]))
        if self.ui_publication.enabled(job):
            return self.ui_publication.run(job,blob,self._run_ui_hot_locked)
        with self.deploy_lock,self.boot_effect_lease():
            self._run_ui_hot_locked(job, blob)

    def _run_ui_hot_locked(self, job, blob):
        if any(engine_component_source_path(path)for path in job.get("changed_paths",[]))and not self.runner.engine_hot_swappable():
            raise ValueError("Independent engine replacement requires live mode; publish a coordinated core release")
        if self._has_unresolved_release(job["job_id"]):
            raise ValueError("uncertain release must be reconciled before UI switch")
        if not job.get("prepared_publication"):
            blob = self._rebase_after_ui(job, blob)
        with self.lock:
            job["hot_assets"] = True
            self._save(job)
        if not job.get("prepared_publication"):
            if not self._sync_job(job, self._current_release_blob(), blob, job.get("changed_paths", ())):return
            self._phase(job, "indexing_source")
            self.runner.stage_assets(blob, job["job_id"])
            self.runner.validate_ui(job["job_id"], job["changed_paths"])
        self._maintenance_job_summary(job)
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
                job["engine_execution_modes"]=self.runner.engine_execution_modes(job["job_id"])
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
        """Execute a sealed job; hot plugins compile before rebasing and stage immutable assets only afterward."""
        with self.lock:
            event = self.job_cancellations.setdefault(job["job_id"], threading.Event())
            if job.get("cancel_request"):
                event.set()
        if hasattr(self.runner, "job_context"):
            self.runner.job_context.job = job
            self.runner.job_context.event = event
        mutated = False
        deploy_locked = False
        boot_guard = None
        publication_started = None
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
                if job.get("isolated") and job["operation"]=="build_deploy" and not job.get("packaging_resume"):
                    previous=sorted((prior for prior in self.jobs.values()
                        if prior["job_id"]!=job["job_id"] and prior.get("task_id")==job.get("task_id")
                        and prior.get("status")=="failed" and not prior.get("effects_started")
                        and prior.get("inputs_sha256")==context_digest(blob)[0] and set(prior.get("images",{}))=={"api","loop"}),
                        key=lambda prior:prior.get("finished_at",0),reverse=True)
                    if previous:
                        if not self.package_cache.enabled:raise ValueError("Persistent dependency cache required before failed image recovery")
                        self.package_cache.ready(blob)
                        proof=image_reuse_module.legacy_pair(self.runner,previous[0],blob)
                        with self.lock:
                            job["legacy_image_proof"]=proof
                            self._save(job)
                if job.get("legacy_image_proof"):self.runner.check_space(MIN_DEPLOY_FREE)
                else:self.runner.prepare_build_space()
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
                if self.scheduler.mode=="enforce":
                    self._phase(job,"preparing_plugins")
                    # Prepare confined artifacts first; freeze assets only after the final locked rebase.
                    job["engine_artifacts"]=self.runner.build_engine_artifacts(job["job_id"],source_root=Path(job["release_root"]))
                    job["plugin_artifacts"]=self.runner.build_ability_artifacts(job["job_id"],source_root=Path(job["release_root"]))
            if job["operation"] in ("deploy", "build_deploy", "ui_deploy", "index_deploy", "plugin_deploy"):
                if self.ui_publication.enabled(job):
                    admitted=self.ui_publication.admit(job,blob,tested_runtime=job["operation"]=="build_deploy")
                    if admitted is None:return
                    blob,boot_guard=admitted;deploy_locked=True;publication_started=time.monotonic()
                else:
                    self.deploy_lock.acquire();deploy_locked = True
                    boot_guard=self.boot_effect_lease();boot_guard.__enter__()
                if self._has_unresolved_release(job["job_id"]):
                    raise ValueError("uncertain release must be reconciled before deployment")
                if not job.get("prepared_publication"):
                    if job.get("isolated") and job["operation"] == "build_deploy":
                        blob = self._rebase_after_ui(job, blob, tested_runtime=True)
                    if job.get("isolated") and not self._sync_job(job, self._current_release_blob(), blob, job.get("changed_paths", ())):return
                    self._phase(job, "indexing_source")
                    self.runner.stage_assets(blob, job["job_id"])
                    self.runner.validate_ui(job["job_id"], job.get("changed_paths", ()))
                self._maintenance_job_summary(job)
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
                # Packaging and final preparation may be separated by a rebase.
                # An absent output must fail while both original consumers still run.
                self._phase(job, "checking_candidate_images")
                self.runner.preflight_deploy_images(job["images"],job["job_id"])
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
            if hasattr(self.runner,"engine_execution_modes"):
                job["engine_execution_modes"]=self.runner.engine_execution_modes(job["job_id"])
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
            try:
                if publication_started is not None:
                    with self.lock:
                        job["preparation"]["publication_lock_seconds"]=time.monotonic()-publication_started;self._save(job)
            finally:
                try:
                    if boot_guard is not None:boot_guard.__exit__(None,None,None)
                finally:
                    if deploy_locked:self.deploy_lock.release()
            with self.lock:
                self._mirror_batch(job)
                self.job_cancellations.pop(job["job_id"], None)
                if job.get("parallel_ui"):
                    self.active_ui = None
                    if job["status"] not in ("uncertain", "interrupted"):
                        self._maybe_start_parallel_ui()
                else:
                    self.active_jobs.discard(job["job_id"])
                    self.active=next(iter(self.active_jobs),None)
                if job["status"] not in ("uncertain", "interrupted"):
                    self._start_next()
            if job["status"]=="completed":self.post_deployment.enqueue(job)

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
        if hasattr(self,"maintenance_activities"):self.maintenance_activities.close()
        if hasattr(self,"post_deployment"):self.post_deployment.close()
        if hasattr(self,"index_coordinator"):self.index_coordinator.close()
        if hasattr(self,"resources"):self.resources.close()
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
                        "status": "failed", "output": getattr(error,"details",scrub(str(error), broker.runner.secrets))}
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
