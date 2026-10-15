"""Versioned conservative UI dependency evidence; declarations never grant write authority."""
import hashlib
import json
from pathlib import Path, PurePosixPath
import posixpath
import re

VERSION = 1
IMPORT = re.compile(r"(?:\bimport\s+(?:[^;]+?\s+from\s+)?|\bexport\s+[^;]+?\s+from\s+|\bimport\s*\()\s*['\"]([^'\"]+)['\"]")
ASSET = re.compile(r"(?:src|href)\s*=\s*['\"]([^'\"]+)['\"]|(?:url\(|@import\s+)\s*['\"]?([^)'\"\s;]+)", re.I)
DYNAMIC = re.compile(r"\bimport\s*\(\s*[^'\"\s]|\b(?:fetch|loadPartial|loadHTML)\s*\(")


# @code-index {"version":1,"feature":"ui-dependency-policy","aliases":["ui dependency policy"],"purpose":"Select dependency-bound frontend verification with conservative unknowns. Config: build-service/scheduler-defaults.json -> ui_test_image supplies the host verification runtime.","span_lines":70,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/ui_test_image"]}],"opportunities":[]}}
def dependencies(files, changed):
    """Return affected consumers and exact dependency hashes, falling back for dynamic loading."""
    ui = {p: v for p, v in files.items() if p.startswith("v3/ui/")}
    graph = {}; unknown = set()
    for path, (_mode, data) in ui.items():
        if PurePosixPath(path).suffix not in {".js", ".mjs", ".cjs", ".html", ".css"}:
            continue
        text = data.decode("utf-8", "strict"); edges = set()
        for match in IMPORT.finditer(text):
            ref = match[1].split("?", 1)[0].split("#", 1)[0]
            if ref.startswith("."):
                target = posixpath.normpath(posixpath.join(posixpath.dirname(path), ref))
            elif ref.startswith("/"):
                target = "v3/ui/" + ref.lstrip("/")
            else:
                unknown.add(path); continue
            if not target.startswith("v3/ui/") or target not in files:
                unknown.add(path)
            else:
                edges.add(target)
        for match in ASSET.finditer(text):
            ref = (match[1] or match[2]).split("?", 1)[0].split("#", 1)[0]
            if not ref or ref.startswith(("data:", "https:", "http:", "//")):
                continue
            target = "v3/ui/" + ref.lstrip("/") if ref.startswith("/") else posixpath.normpath(posixpath.join(posixpath.dirname(path), ref))
            if target in files:
                edges.add(target)
            else:
                unknown.add(path)
        if DYNAMIC.search(text):
            unknown.add(path)
        graph[path] = edges
    affected = set(changed)
    while True:
        consumers = {p for p, edges in graph.items() if edges & affected}
        if consumers <= affected:
            break
        affected |= consumers
    conservative = bool(unknown or any(not p.startswith("v3/ui/") for p in changed))
    closure = set(ui)|set(changed) if conservative else set(affected)
    pending = list(closure)
    while pending:
        for dep in graph.get(pending.pop(), set()):
            if dep not in closure:
                closure.add(dep); pending.append(dep)
    # Tests select from verified frontend fixtures; an unresolved graph keeps the full set.
    tests = sorted(p for p in ui if p.endswith((".test.cjs", ".test.js", ".test.mjs")) and (conservative or posixpath.dirname(p) in {posixpath.dirname(a) for a in affected}))
    closure.update(tests)
    hashes = {p: hashlib.sha256(files[p][1]).hexdigest() for p in sorted(closure) if p in files}
    payload = dict(version=VERSION, policy_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), affected=sorted(affected), dependencies=hashes, tests=tests, conservative=conservative)
    payload["sha256"] = hashlib.sha256(json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return payload
