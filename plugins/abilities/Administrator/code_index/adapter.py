"""Derived source index for one Rust-authorized caller/session workspace.

No plugin import appoints a tool or grants access. Rust owns policy and dispatch.
Only the derived index is written; source files are never changed by scanning.
"""
from __future__ import annotations
import ast
import fcntl
import hashlib
import heapq
import json
import io
import tokenize
import os
import re
import sqlite3
import stat
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional
try:
    from tree_sitter import Language, Parser
    import tree_sitter_javascript
except ImportError:
    Language = Parser = tree_sitter_javascript = None

MAX_FILES = 5000
MAX_ENTRIES = 20000
MAX_FILE_BYTES = 512 * 1024
MAX_TOTAL_BYTES = 32 * 1024 * 1024
MAX_LINES = 20000
MAX_TOTAL_LINES = 300000
MAX_OUTPUT_BYTES = 64 * 1024
MAX_DB_BYTES = 128 * 1024 * 1024
SUFFIXES = {".js", ".mjs", ".cjs", ".css", ".html", ".json", ".py", ".rs", ".toml"}
EXCLUDED = {".git", "target", "data", "workspaces", "runtime", "rollback", ".code-index",
    "v2_ref", "node_modules", "vendor", "backups", "__pycache__", ".venv", ".cache"}
FUNCTION_RE = re.compile(r"(?:^|[;{}\s])(?:async\s+)?function\s*\*?\s+([A-Za-z_$][\w$]*)\s*\(([^)]*)\)|(?:^|[;{}\s])(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\(([^)]*)\)\s*=>")
CLASS_RE = re.compile(r"(?:^|[;{}\s])class\s+([A-Za-z_$][\w$]*)")
IMPORT_RE = re.compile(r"(?:import\s+(?:[^;]+?\s+from\s+)?|import\s*\()\s*['\"]([^'\"]+)['\"]")
SELECTOR_RE = re.compile(r"(?:querySelector(?:All)?|closest|matches)\s*\(\s*['\"]([^'\"]+)['\"]")
EVENT_RE = re.compile(r"(?:addEventListener|on)\s*\(\s*['\"]([^'\"]+)['\"]")
DATA_RE = re.compile(r"\b(data-[A-Za-z0-9_-]+)\b")
ENDPOINT_RE = re.compile(r"['\"\x60]((?:/api/|/session-|/chat/)[^'\"\x60\s?]*)")
CSS_RULE_RE = re.compile(r"^\s*([^@/][^{]+)\{")
HTML_TAG_RE = re.compile(r"<\s*([A-Za-z][\w:-]*)\b")
HTML_ASSET_RE = re.compile(r"(?:src|href)\s*=\s*['\"]([^'\"]+)['\"]", re.I)

DDL = """
CREATE TABLE IF NOT EXISTS meta(id INTEGER PRIMARY KEY CHECK(id=1),schema_version INTEGER NOT NULL,revision INTEGER NOT NULL);
INSERT OR IGNORE INTO meta VALUES(1,1,0);
CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY,language TEXT NOT NULL,sha256 TEXT NOT NULL,line_count INTEGER NOT NULL,scanned_at TEXT NOT NULL,parser_status TEXT NOT NULL,parser_notes TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS lines(file_path TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,line_no INTEGER NOT NULL,kind TEXT NOT NULL,owner_symbol TEXT,text_digest TEXT NOT NULL,note TEXT NOT NULL,PRIMARY KEY(file_path,line_no));
CREATE TABLE IF NOT EXISTS symbols(file_path TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,name TEXT NOT NULL,kind TEXT NOT NULL,start_line INTEGER NOT NULL,end_line INTEGER NOT NULL,signature TEXT NOT NULL,parent_symbol TEXT,uncertain INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS symbols_name ON symbols(name);
CREATE TABLE IF NOT EXISTS edges(file_path TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,line_no INTEGER NOT NULL,edge_kind TEXT NOT NULL,value TEXT NOT NULL,owner_symbol TEXT);
CREATE INDEX IF NOT EXISTS edges_value ON edges(value);
CREATE TABLE IF NOT EXISTS feature_locations(feature_id TEXT,source_path TEXT,source_line INTEGER,file_path TEXT,start_line INTEGER,end_line INTEGER,aliases TEXT,purpose TEXT,location_purpose TEXT,origin TEXT,target_sha256 TEXT,source_sha256 TEXT,PRIMARY KEY(feature_id,source_path,source_line,file_path,start_line,end_line));
CREATE INDEX IF NOT EXISTS feature_locations_id ON feature_locations(feature_id);
CREATE TABLE IF NOT EXISTS batches(batch_id TEXT PRIMARY KEY,scope TEXT NOT NULL,status TEXT NOT NULL,requested_files INTEGER NOT NULL,indexed_files INTEGER NOT NULL,started_at TEXT NOT NULL,completed_at TEXT,error TEXT NOT NULL,revision INTEGER);
"""

MAX_FEATURES=128
MAX_FEATURE_LOCATIONS=16
FEATURE_ID_RE=re.compile(r"[a-z0-9][a-z0-9-]{0,47}\Z")
FEATURE_MAP_PATH="plugins/abilities/Administrator/code_index/code-index-features.json"
RUST_RAW_RE=re.compile(r"(?:br|rb|r)(#+)?\"")
RUST_CHAR_RE=re.compile(r"'(?:\\.|[^'\\])'")
def _unique_json(text):
    def pairs(items):
        out={}
        for k,v in items:
            if k in out:raise ValueError("duplicate JSON key")
            out[k]=v
        return out
    return json.loads(text,object_pairs_hook=pairs)
def _comments(source,suffix):
    if suffix==".py":
        try:
            for t in tokenize.generate_tokens(io.StringIO(source).readline):
                if t.type==tokenize.COMMENT:yield t.start[0],t.string[1:]
        except (tokenize.TokenError,IndentationError):return
        return
    i=0;line=1;n=len(source)
    while i<n:
        c=source[i]
        if c=="\n":line+=1;i+=1;continue
        raw=RUST_RAW_RE.match(source,i) if suffix==".rs" and c in "rb" else None
        if raw:
            close="\""+raw.group(1);j=source.find(close,i+len(raw.group(0)))
            if j<0:return
            line+=source[i:j+len(close)].count("\n");i=j+len(close);continue
        if suffix==".rs" and c==chr(39):
            char=RUST_CHAR_RE.match(source,i)
            if char:i+=len(char.group(0));continue
            i+=1;continue
        if c in (chr(39),chr(34),chr(96)):
            q=c;i+=1
            while i<n:
                if source[i]=="\\":i+=2;continue
                if source[i]==q:i+=1;break
                if source[i]=="\n":line+=1
                i+=1
            continue
        if source.startswith("//",i):
            j=source.find("\n",i);j=n if j<0 else j
            yield line,source[i+2:j];i=j;continue
        if source.startswith("/*",i):
            start=i+2;j=start;depth=1
            while j<n and depth:
                if suffix==".rs" and source.startswith("/*",j):depth+=1;j+=2;continue
                if source.startswith("*/",j):depth-=1
                j+=2 if source.startswith("*/",j) else 1
            if depth:yield line,source[start:];return
            yield line,source[start:j-2];line+=source[i:j].count("\n");i=j;continue
        i+=1
def _pathnorm(path):
    parts=relative_parts(path)
    if parts[0] in {"v3","webagent-v3"}:parts=parts[1:]
    if not parts:raise ValueError("invalid feature path")
    return "/".join(parts)
def _has_code_line(line,suffix):
    text=line.strip()
    if not text:return False
    if suffix==".py":return not text.startswith("#")
    if suffix==".css":return not text.startswith(("/*","//","*/"))
    return not text.startswith(("//","/*"))
def _marker_rows(path,source,digest):
    suffix=Path(path).suffix.lower()
    if suffix not in {".js",".mjs",".cjs",".css",".rs",".py"}:return []
    lines=source.splitlines();rows=[];seen=set()
    for line,body in _comments(source,suffix):
        if not body.lstrip().startswith("@code-index"):continue
        if "\n" in body or "\r" in body:raise ValueError("Code Index feature marker must be single-line")
        text=lines[line-1].lstrip() if 0<line<=len(lines) else ""
        starts=text.startswith("#") if suffix==".py" else text.startswith("/*") if suffix==".css" else text.startswith("//")
        if not starts:continue
        m=re.fullmatch(r"\s*@code-index\s+(\{.*\})\s*",body,re.S)
        if not m or len(m.group(1).encode())>2048:raise ValueError("malformed Code Index feature marker")
        obj=_unique_json(m.group(1))
        if not isinstance(obj,dict) or not {"version","feature","aliases","purpose"}<=set(obj) or set(obj)-{"version","feature","aliases","purpose","span_lines"}:raise ValueError("invalid Code Index feature marker fields")
        fid,aliases,purpose,span=obj["feature"],obj["aliases"],obj["purpose"],obj.get("span_lines",40)
        if type(obj["version"]) is not int or obj["version"]!=1 or not isinstance(fid,str) or not FEATURE_ID_RE.fullmatch(fid):raise ValueError("invalid Code Index feature marker identity")
        if fid in seen:raise ValueError("duplicate Code Index feature in file")
        if not isinstance(aliases,list) or len(aliases)>16 or any(not isinstance(x,str) or not x.strip() or len(x.encode())>128 or any(ord(c)<32 for c in x) for x in aliases) or len({x.strip().casefold() for x in aliases})!=len(aliases):raise ValueError("invalid Code Index feature aliases")
        if not isinstance(purpose,str) or not purpose.strip() or len(purpose.encode())>512:raise ValueError("invalid Code Index feature purpose")
        if not isinstance(span,int) or isinstance(span,bool) or not 1<=span<=80:raise ValueError("invalid Code Index feature span")
        seen.add(fid);start=line+1;end=min(len(lines),start+(span or 40)-1)
        code=[x for x in lines[start-1:end] if _has_code_line(x,suffix)]
        if not code:raise ValueError("Code Index marker span has no code")
        rows.append((fid,path,line,path,start,end,json.dumps(aliases,ensure_ascii=False),purpose.strip(),purpose.strip(),"comment",digest,digest))
    return rows
def _map_rows(raw):
    if len(raw)>128*1024:raise ValueError("Code Index feature map exceeds limit")
    doc=_unique_json(raw.decode("utf-8"))
    if not isinstance(doc,dict) or set(doc)!={"version","features"} or type(doc["version"]) is not int or doc["version"]!=1:raise ValueError("invalid Code Index feature map")
    if not isinstance(doc["features"],list) or len(doc["features"])>128:raise ValueError("invalid Code Index feature list")
    seen=set();out=[]
    for f in doc["features"]:
        if not isinstance(f,dict) or set(f)!={"id","aliases","purpose","locations"}:raise ValueError("invalid Code Index feature entry")
        fid=f["id"]
        if not isinstance(fid,str) or not FEATURE_ID_RE.fullmatch(fid) or fid in seen:raise ValueError("invalid or duplicate feature map ID")
        seen.add(fid);aliases=f["aliases"];purpose=f["purpose"];locs=f["locations"]
        if not isinstance(aliases,list) or len(aliases)>16 or any(not isinstance(x,str) or not x.strip() or len(x.encode())>128 or any(ord(c)<32 for c in x) for x in aliases) or len({x.strip().casefold() for x in aliases})!=len(aliases):raise ValueError("invalid feature map aliases")
        if not isinstance(purpose,str) or not purpose.strip() or len(purpose.encode())>512:raise ValueError("invalid feature map purpose")
        if not isinstance(locs,list) or not 1<=len(locs)<=16:raise ValueError("invalid feature map locations")
        for loc in locs:
            if not isinstance(loc,dict) or not {"path","start_line","end_line"}<=set(loc) or set(loc)-{"path","start_line","end_line","purpose","sha256"}:raise ValueError("invalid feature map location")
            target=_pathnorm(loc["path"]);start,end=loc["start_line"],loc["end_line"];desc=loc.get("purpose",purpose);digest=loc.get("sha256")
            if any(not isinstance(x,int) or isinstance(x,bool) for x in (start,end)) or start<1 or end<start or end-start>=80:raise ValueError("invalid feature map line range")
            if not isinstance(desc,str) or not desc.strip() or len(desc.encode())>512 or (digest is not None and (not isinstance(digest,str) or not re.fullmatch(r"[0-9a-f]{64}",digest))):raise ValueError("invalid feature map target binding")
            out.append((fid,FEATURE_MAP_PATH,1,target,start,end,json.dumps(aliases,ensure_ascii=False),purpose.strip(),desc.strip(),"map",digest,""))
    return out
def refresh_features(conn,rootfd):
    indexed={r["path"]:r for r in conn.execute("SELECT path,sha256 FROM files")}
    # Published archives preserve both v3/ and shared top-level UI namespaces.
    # Feature bindings belong to the release's v3 tree; shared UI inputs must
    # never shadow a same-named V3 target. Private workspace indexes keep their
    # existing root-relative behavior.
    published=any(p.startswith("v3/") for p in indexed)
    feature_sources={p:r for p,r in indexed.items() if not published or p.startswith("v3/")}
    normalized={}
    for p,row in feature_sources.items():
        key=_pathnorm(p)
        if key in normalized:raise ValueError("Normalized Code Index feature path collision")
        normalized[key]=(p,row)
    rows=[]
    for p,r in feature_sources.items():
        if Path(p).suffix.lower() not in {".js",".mjs",".cjs",".css",".rs",".py"}:continue
        data=read_source(rootfd,p)
        if sha(data)!=r["sha256"]:raise ValueError("feature source changed during scan")
        rows.extend(_marker_rows(p,data.decode("utf-8"),r["sha256"]))
    map_candidates=(p for p in feature_sources if _pathnorm(p)==FEATURE_MAP_PATH)
    map_path=next(map_candidates,None)
    if map_path:
        raw=read_source(rootfd,map_path)
        if sha(raw)!=feature_sources[map_path]["sha256"]:raise ValueError("feature map changed during scan")
        for row in _map_rows(raw):
            target=normalized.get(row[3])
            if target is None:raise ValueError("feature map target is unindexed")
            actual,meta=target;data=read_source(rootfd,actual);lines=data.decode("utf-8").splitlines()
            suffix=Path(actual).suffix.lower()
            if (row[10] and meta["sha256"]!=row[10]) or sha(data)!=meta["sha256"] or row[5]>len(lines) or not any(_has_code_line(x,suffix) for x in lines[row[4]-1:row[5]]):raise ValueError("feature map target is stale")
            rows.append(row[:1]+(map_path,)+row[2:3]+(actual,)+row[4:10]+(meta["sha256"],feature_sources[map_path]["sha256"]))
    if len(rows)>MAX_FILES+MAX_FEATURES*MAX_FEATURE_LOCATIONS:raise ValueError("Code Index feature row limit exceeded")
    conn.execute("DELETE FROM feature_locations")
    conn.executemany("INSERT INTO feature_locations VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",rows)

def sha(data):
    return hashlib.sha256(data).hexdigest()
def now():
    return datetime.now(timezone.utc).isoformat()
def relative_parts(value, allow_empty=False):
    if not isinstance(value,str) or len(value.encode())>512 or "\0" in value or "\\" in value:
        raise ValueError("Invalid relative source path")
    if allow_empty and value in ("", "."):
        return []
    pieces=value.split("/")
    if not pieces or value.startswith("/") or any(p in ("",".","..") for p in pieces):
        raise ValueError("Only confined relative paths are supported")
    return pieces
def open_child_dir(fd,name):
    return os.open(name,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=fd)
def open_path(fd,parts,directory=False):
    current=os.dup(fd)
    try:
        for name in parts[:-1] if parts else []:
            following=open_child_dir(current,name);os.close(current);current=following
        if not parts:
            return os.dup(current)
        return os.open(parts[-1],os.O_RDONLY|os.O_NOFOLLOW|(os.O_DIRECTORY if directory else os.O_NONBLOCK),dir_fd=current)
    finally:
        os.close(current)
def read_source(rootfd,path):
    parts=relative_parts(path)
    if any(p in EXCLUDED for p in parts):
        raise ValueError("Excluded source path")
    fd=open_path(rootfd,parts)
    try:
        before=os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_size>MAX_FILE_BYTES:
            raise ValueError("Source file type/size exceeds limit")
        data=bytearray()
        while len(data)<=MAX_FILE_BYTES:
            chunk=os.read(fd,min(65536,MAX_FILE_BYTES+1-len(data)))
            if not chunk:break
            data.extend(chunk)
        after=os.fstat(fd)
        if len(data)>MAX_FILE_BYTES or (before.st_size,before.st_mtime_ns,before.st_ino)!=(after.st_size,after.st_mtime_ns,after.st_ino):
            raise ValueError("Source changed during bounded read")
        return bytes(data)
    finally:os.close(fd)
def source_files(rootfd,scope,deadline):
    parts=relative_parts(scope,True)
    if any(p in EXCLUDED for p in parts):
        raise ValueError("Excluded source scope")
    start=open_path(rootfd,parts)
    found=[];entries=0
    def walk(fd,prefix,depth):
        nonlocal entries
        if depth>32 or time.monotonic()>deadline:raise ValueError("Source scan bound exceeded")
        children=[]
        with os.scandir(fd) as iterator:
            for entry in iterator:
                entries+=1
                if entries>MAX_ENTRIES:raise ValueError("Source enumeration limit exceeded")
                children.append(entry)
        children.sort(key=lambda e:e.name)
        for entry in children:
            if time.monotonic()>deadline:raise ValueError("Source scan deadline exceeded")
            if entry.name in EXCLUDED:continue
            path="/".join(prefix+[entry.name])
            if len(path.encode())>512:raise ValueError("Source path exceeds limit")
            if entry.is_symlink():raise ValueError("Symlink source entries are unsupported")
            if entry.is_dir(follow_symlinks=False):
                child=open_child_dir(fd,entry.name)
                try:walk(child,prefix+[entry.name],depth+1)
                finally:os.close(child)
            elif entry.is_file(follow_symlinks=False) and Path(entry.name).suffix.lower() in SUFFIXES:
                if entry.name.endswith((".min.js",".min.css")):continue
                found.append(path)
                if len(found)>MAX_FILES:raise ValueError("Source file count exceeds limit")
            elif not entry.is_file(follow_symlinks=False):
                raise ValueError("Unsupported source filesystem entry")
    try:
        meta=os.fstat(start)
        if stat.S_ISREG(meta.st_mode):
            path="/".join(parts)
            if Path(path).suffix.lower() not in SUFFIXES:raise ValueError("Unsupported source suffix")
            found=[path]
        elif stat.S_ISDIR(meta.st_mode):walk(start,parts,0)
        else:raise ValueError("Invalid source scope")
    finally:os.close(start)
    return sorted(found)
def owners(lines,known):
    # Sweep spans once: line completeness must not require an O(lines*symbols) walk.
    starts=sorted(enumerate(known),key=lambda pair:pair[1]["start"]);pending=[];at=0;result={}
    for number in range(1,len(lines)+1):
        while at<len(starts) and starts[at][1]["start"]<=number:
            serial,item=starts[at];at+=1
            heapq.heappush(pending,(item["end"]-item["start"],item["end"],serial,item["name"]))
        while pending and pending[0][1]<number:heapq.heappop(pending)
        result[number]=pending[0][3] if pending else None
    return result

def brace_delta(line: str, in_comment: bool) -> tuple[int, bool]:
    """Count JS braces while avoiding ordinary quotes and comments."""
    delta, index, quote, escaped = 0, 0, None, False
    while index < len(line):
        char, pair = line[index], line[index:index + 2]
        if in_comment:
            if pair == "*/":
                in_comment, index = False, index + 2
                continue
            index += 1
            continue
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            index += 1
            continue
        if pair == "/*":
            in_comment, index = True, index + 2
            continue
        if pair == "//":
            break
        if char in "'\"`":
            quote = char
        elif char == "{":
            delta += 1
        elif char == "}":
            delta -= 1
        index += 1
    return delta, in_comment

def python_symbols(source: str) -> list[dict[str, Any]]:
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return []
    result: list[dict[str, Any]] = []
    parents: list[str] = []

    class Visitor(ast.NodeVisitor):
        def visit_ClassDef(self, node: ast.ClassDef) -> None:
            result.append({"name": node.name, "kind": "class", "start": node.lineno, "end": node.end_lineno or node.lineno, "signature": f"class {node.name}", "uncertain": False, "parent": parents[-1] if parents else None})
            parents.append(node.name)
            self.generic_visit(node)
            parents.pop()

        def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
            self._function(node)

        def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
            self._function(node)

        def _function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
            args = [arg.arg for arg in node.args.args]
            result.append({"name": node.name, "kind": "function", "start": node.lineno, "end": node.end_lineno or node.lineno, "signature": f"{node.name}({', '.join(args)})", "uncertain": False, "parent": parents[-1] if parents else None})
            parents.append(node.name)
            self.generic_visit(node)
            parents.pop()

    Visitor().visit(tree)
    return sorted(result, key=lambda item: (item["start"], item["end"], item["name"]))

def node_text(source: bytes, node: Any) -> str:
    return source[node.start_byte:node.end_byte].decode("utf-8", "replace")

def javascript_symbols(source: str) -> tuple[Optional[list[dict[str, Any]]], str]:
    """Return AST-backed JavaScript symbols, or a reason to use the fallback."""
    if not all((Language, Parser, tree_sitter_javascript)):
        return None, "tree_sitter_unavailable"
    try:
        language = Language(tree_sitter_javascript.language())
        parser = Parser(language)
        encoded = source.encode("utf-8", "replace")
        tree = parser.parse(encoded)
    except Exception as exc:
        return None, f"tree_sitter_error:{type(exc).__name__}"
    if tree.root_node.has_error:
        return None, "tree_sitter_syntax_error"

    result: list[dict[str, Any]] = []
    symbol_types = {"function_declaration", "class_declaration", "method_definition", "generator_function_declaration"}
    callable_values = {"arrow_function", "function_expression", "generator_function"}

    def add(node: Any, name_node: Any, kind: str, parents: list[str]) -> Optional[str]:
        if not name_node:
            return None
        name = node_text(encoded, name_node).strip()
        if not name:
            return None
        first_line = node_text(encoded, node).splitlines()[0].strip()
        result.append({
            "name": name,
            "kind": kind,
            "start": node.start_point[0] + 1,
            "end": node.end_point[0] + 1,
            "signature": first_line[:300],
            "uncertain": False,
            "parent": parents[-1] if parents else None,
        })
        return name

    def walk(node: Any, parents: list[str]) -> None:
        own_name: Optional[str] = None
        if node.type in symbol_types:
            kind = "class" if node.type == "class_declaration" else "function"
            own_name = add(node, node.child_by_field_name("name"), kind, parents)
        elif node.type == "variable_declarator":
            value = node.child_by_field_name("value")
            if value and value.type in callable_values:
                own_name = add(value, node.child_by_field_name("name"), "function", parents)
        child_parents = parents + [own_name] if own_name else parents
        for child in node.children:
            walk(child, child_parents)

    walk(tree.root_node, [])
    return sorted(result, key=lambda item: (item["start"], item["end"], item["name"])), ""

def symbols(lines: list[str], suffix: str, source: str) -> list[dict[str, Any]]:
    if suffix == ".py":
        return python_symbols(source)
    if suffix in {".css", ".html", ".json"}:
        return []
    starts: list[dict[str, Any]] = []
    for line_no, line in enumerate(lines, 1):
        match = CLASS_RE.search(line)
        if match:
            starts.append({"name": match.group(1), "kind": "class", "start": line_no, "signature": match.group(0).strip(), "parent": None})
            continue
        match = FUNCTION_RE.search(line)
        if match:
            name, args = match.group(1) or match.group(3), match.group(2) or match.group(4) or ""
            starts.append({"name": name, "kind": "function", "start": line_no, "signature": f"{name}({args})", "parent": None})
    by_line = {item["start"]: item for item in starts}
    result: list[dict[str, Any]] = []
    opened: list[tuple[dict[str, Any], int]] = []
    depth, in_comment = 0, False
    for line_no, line in enumerate(lines, 1):
        before = depth
        delta, in_comment = brace_delta(line, in_comment)
        start = by_line.get(line_no)
        if start:
            start["end"] = line_no
            start["uncertain"] = "{" not in line
            if start["uncertain"]:
                result.append(start)
            else:
                opened.append((start, before + delta))
        depth += delta
        still_open: list[tuple[dict[str, Any], int]] = []
        for item, target_depth in opened:
            if depth < target_depth:
                item["end"], item["uncertain"] = line_no, False
                result.append(item)
            else:
                still_open.append((item, target_depth))
        opened = still_open
    for item, _ in opened:
        item["end"], item["uncertain"] = len(lines), True
        result.append(item)
    return sorted(result, key=lambda item: (item["start"], item["end"], item["name"]))

def line_kind(line: str, suffix: str, in_comment: bool) -> tuple[str, bool]:
    text = line.strip()
    if not text:
        return "blank", in_comment
    if in_comment:
        return "comment", "*/" not in text
    if text.startswith("/*"):
        return "comment", "*/" not in text
    if text.startswith("//") or (suffix == ".css" and text.startswith("*")):
        return "comment", False
    if suffix == ".css":
        return ("css_rule" if CSS_RULE_RE.match(line) else "css_declaration" if ":" in line else "top_level"), False
    if suffix == ".html":
        return ("markup_tag" if HTML_TAG_RE.search(line) else "markup"), False
    if suffix == ".json":
        return ("json_property" if re.match(r'\s*"[^"\\]+"\s*:', line) else "json_syntax"), False
    if suffix == ".py":
        if text.startswith("#"):
            return "comment", False
        if re.match(r"\s*(?:async\s+)?def\s+", line):
            return "function", False
        if re.match(r"\s*class\s+", line):
            return "class", False
        if re.match(r"\s*(?:from|import)\s+", line):
            return "import_export", False
        return "python_statement", False
    if text.startswith(("import ", "export ")):
        return "import_export", False
    if CLASS_RE.search(line):
        return "class", False
    if FUNCTION_RE.search(line):
        return "function", False
    if EVENT_RE.search(line):
        return "event_handler", False
    if re.match(r"\s*(?:const|let|var)\s+", line):
        return "declaration", False
    return "top_level", False

def extract_edges(lines: list[str], known: list[dict[str, Any]]) -> list[tuple[int, str, str, Optional[str]]]:
    edges = []
    for line_no, line in enumerate(lines, 1):
        line_owner = known[line_no]
        for matcher, kind in ((IMPORT_RE, "import"), (SELECTOR_RE, "selector"), (EVENT_RE, "event"), (ENDPOINT_RE, "endpoint")):
            edges.extend((line_no, kind, match.group(1), line_owner) for match in matcher.finditer(line))
        edges.extend((line_no, "data_attribute", match.group(1), line_owner) for match in DATA_RE.finditer(line))
        edges.extend((line_no, "asset", match.group(1), line_owner) for match in HTML_ASSET_RE.finditer(line))
    return edges


def scan_data(path,data):
    try:source=data.decode("utf-8")
    except UnicodeDecodeError as exc:raise ValueError("Source must be UTF-8 text") from exc
    lines=source.split("\n")
    if source.endswith("\n"):lines.pop()
    if not source:lines=[]
    if len(lines)>MAX_LINES:raise ValueError("Physical line limit exceeded")
    suffix=Path(path).suffix.lower()
    status,notes="ok",""
    if suffix in {".js",".mjs",".cjs"}:
        try:known,reason=javascript_symbols(source)
        except (RecursionError,MemoryError):known,reason=None,"parser_resource_limit"
        if known is None:
            known=symbols(lines,suffix,source)
            for item in known:item["uncertain"]=True
            status,notes="uncertain",reason
    elif suffix==".py":
        try:
            ast.parse(source)
            known=python_symbols(source)
        except (SyntaxError,RecursionError,MemoryError) as exc:
            known=[];status,notes="invalid","python_parse:"+type(exc).__name__
    elif suffix==".rs":
        known=[]
        for number,line in enumerate(lines,1):
            match=re.search(r"\b(fn|struct|enum|trait|mod)\s+([A-Za-z_]\w*)",line)
            if match:known.append({"name":match[2],"kind":match[1],"start":number,"end":number,
                "signature":line.strip()[:300],"parent":None,"uncertain":True})
        status,notes="uncertain","rust_lexical_only"
    else:
        known=[]
        if suffix==".toml":status,notes="uncertain","toml_lexical_only"
    if len(known)>2000:raise ValueError("Symbol count limit exceeded")
    ownership=owners(lines,known);ledger=[];comment=False
    for number,line in enumerate(lines,1):
        kind,comment=line_kind(line,suffix,comment)
        ledger.append((number,kind,ownership[number],sha(line.encode()),"parser_uncertain" if status!="ok" else ""))
    edges=extract_edges(lines,ownership)
    if len(edges)>20000:raise ValueError("Static edge count exceeds limit")
    if any(len(value.encode())>512 for _,_,value,_ in edges):raise ValueError("Static edge value exceeds limit")
    return {"path":path,"language":{".js":"javascript",".mjs":"javascript",".cjs":"javascript",
        ".py":"python",".rs":"rust"}.get(suffix,suffix[1:]),"sha256":sha(data),"line_count":len(lines),
        "parser_status":status,"parser_notes":notes,"symbols":known,"ledger":ledger,"edges":edges}
def store_scan(conn,scan):
    path=scan["path"]
    conn.execute("DELETE FROM files WHERE path=?",(path,))
    conn.execute("INSERT INTO files VALUES(?,?,?,?,?,?,?)",(path,scan["language"],scan["sha256"],
        scan["line_count"],now(),scan["parser_status"],scan["parser_notes"]))
    conn.executemany("INSERT INTO lines VALUES(?,?,?,?,?,?)",[(path,*row) for row in scan["ledger"]])
    conn.executemany("INSERT INTO symbols VALUES(?,?,?,?,?,?,?,?)",[(path,item["name"],item["kind"],
        item["start"],item["end"],item["signature"][:300],item.get("parent"),int(item["uncertain"])) for item in scan["symbols"]])
    conn.executemany("INSERT INTO edges VALUES(?,?,?,?,?)",[(path,*row) for row in scan["edges"]])
def snapshot(rootfd,paths,deadline):
    total=0;records=[]
    for path in paths:
        if time.monotonic()>deadline:raise ValueError("Source scan deadline exceeded")
        data=read_source(rootfd,path);total+=len(data)
        if total>MAX_TOTAL_BYTES:raise ValueError("Source byte limit exceeded")
        records.append((path,data))
    return records
def stale_files(conn,rootfd):
    stale=[];deadline=time.monotonic()+30;total=0
    rows=conn.execute("SELECT path,sha256 FROM files ORDER BY path").fetchall()
    if len(rows)>MAX_FILES:raise ValueError("Index file count exceeds limit")
    for row in rows:
        if time.monotonic()>deadline:raise ValueError("Freshness deadline exceeded")
        try:
            data=read_source(rootfd,row["path"]);total+=len(data)
            if sha(data)!=row["sha256"]:stale.append({"path":row["path"],"reason":"digest_changed"})
        except (OSError,ValueError):stale.append({"path":row["path"],"reason":"missing_or_unreadable"})
        if total>MAX_TOTAL_BYTES:raise ValueError("Freshness source byte limit exceeded")
    return stale
def page(rows,offset,limit):
    selected=[];used=0
    for row in rows[offset:offset+limit]:
        size=len(json.dumps(row,ensure_ascii=False).encode())
        if used+size>12*1024:break
        selected.append(row);used+=size
    next_offset=offset+len(selected)
    return {"results":selected,"count":len(selected),"total":len(rows),
        "next_offset":next_offset,"has_more":next_offset<len(rows)}
def coverage(conn,rootfd,args):
    expected=set(source_files(rootfd,"",time.monotonic()+30))
    rows=conn.execute("""SELECT f.path,f.line_count,f.parser_status,
        COUNT(l.line_no) AS ledger_rows,
        SUM(CASE WHEN l.line_no BETWEEN 1 AND f.line_count THEN 1 ELSE 0 END) AS covered_lines
        FROM files f LEFT JOIN lines l ON l.file_path=f.path GROUP BY f.path ORDER BY f.path""").fetchall()
    if len(rows)>MAX_FILES:raise ValueError("Index file count exceeds limit")
    files=[];total=0;covered=0
    for row in rows:
        item=dict(row);item["covered_lines"]=item["covered_lines"] or 0
        item["gaps"]=max(0,item["line_count"]-item["covered_lines"])
        total+=item["line_count"];covered+=item["covered_lines"];files.append(item)
    stale=stale_files(conn,rootfd);unindexed=sorted(expected-{r["path"] for r in rows})
    kind=args["action"]
    if kind=="gaps":result=page([f for f in files if f["gaps"]],args["offset"],args["limit"])
    elif kind=="stale":result=page(stale,args["offset"],args["limit"])
    else:
        result=page(files,args["offset"],args["limit"])
        result.update(file_count=len(files),total_lines=total,covered_lines=covered,
            gaps=total-covered,percent=100 if not total else round(covered*100/total,2),
            stale_count=len(stale),unindexed_count=len(unindexed),
            stale=page(stale,args["offset"],args["limit"]),
            unindexed=page([{"path":p} for p in unindexed],args["offset"],args["limit"]))
    return result
def scan(conn,rootfd,args):
    scope=args.get("file_path") if args["action"]=="scan_file" else args.get("scope","")
    if scope is None:raise ValueError("scan_file requires file_path")
    deadline=time.monotonic()+30
    paths=source_files(rootfd,scope,deadline)
    if args["action"]=="scan_file" and len(paths)!=1:raise ValueError("scan_file requires one source file")
    batch=args.get("batch_id") or str(uuid.uuid4())
    if not re.fullmatch(r"[a-zA-Z0-9_-]{1,80}",batch):raise ValueError("Invalid batch_id")
    if conn.execute("SELECT 1 FROM batches WHERE batch_id=?",(batch,)).fetchone():
        raise ValueError("Batch identifier already used; inspect progress")
    revision=conn.execute("SELECT revision FROM meta WHERE id=1").fetchone()[0]
    if args.get("expected_revision",revision)!=revision:raise ValueError("Index revision conflict")
    conn.execute("INSERT INTO batches VALUES(?,?,'in_progress',?,0,?,NULL,'',NULL)",(batch,scope,len(paths),now()))
    conn.commit()
    try:
        records=snapshot(rootfd,paths,deadline);scans=[];lines=0
        for path,data in records:
            if time.monotonic()>deadline:raise ValueError("Parser deadline exceeded")
            item=scan_data(path,data);lines+=item["line_count"]
            if lines>MAX_TOTAL_LINES:raise ValueError("Index line count exceeds limit")
            scans.append(item)
        conn.execute("BEGIN IMMEDIATE")
        if conn.execute("SELECT revision FROM meta WHERE id=1").fetchone()[0]!=revision:
            raise ValueError("Index revision changed during scan")
        # Recheck exact source bytes and full-scope enumeration before replacing
        # derived rows; a raced scan never claims to index the newer content.
        if paths!=source_files(rootfd,scope,deadline):raise ValueError("Source enumeration changed during scan")
        for path,data in records:
            if sha(read_source(rootfd,path))!=sha(data):raise ValueError("Source changed before index commit")
        removed=[]
        if args["action"]=="scan":
            prefix=scope.rstrip("/")
            known=[row[0] for row in conn.execute("SELECT path FROM files")]
            removed=[p for p in known if (not prefix or p==prefix or p.startswith(prefix+"/")) and p not in paths]
            conn.executemany("DELETE FROM files WHERE path=?",[(p,) for p in removed])
        for item in scans:store_scan(conn,item)
        refresh_features(conn,rootfd)
        if conn.execute("SELECT COALESCE(SUM(line_count),0) FROM files").fetchone()[0]>MAX_TOTAL_LINES:
            raise ValueError("Stored index line limit exceeded")
        if conn.execute("SELECT COUNT(*) FROM files").fetchone()[0]>MAX_FILES:raise ValueError("Stored file count exceeds limit")
        conn.execute("UPDATE meta SET revision=revision+1 WHERE id=1")
        conn.execute("UPDATE batches SET status='completed',indexed_files=?,completed_at=?,revision=? WHERE batch_id=?",
            (len(scans),now(),revision+1,batch))
        conn.execute("DELETE FROM batches WHERE batch_id NOT IN (SELECT batch_id FROM batches ORDER BY started_at DESC LIMIT 100)")
        conn.commit()
        result=page([{"path":i["path"],"lines":i["line_count"],"symbols":len(i["symbols"]),
            "edges":len(i["edges"]),"parser_status":i["parser_status"]} for i in scans],0,args["limit"])
        result.update(batch_id=batch,indexed_files=len(scans),removed_count=len(removed),
            removed=page([{"path":p} for p in removed],0,args["limit"]),revision=revision+1)
        return result
    except Exception as exc:
        conn.rollback()
        conn.execute("UPDATE batches SET status='failed',completed_at=?,error=? WHERE batch_id=?",
            (now(),type(exc).__name__+": "+str(exc)[:240],batch));conn.commit()
        raise

ACTIONS={"code_index_scan":{"discover","scan","scan_file"},
    "code_index_lookup":{"file","line","symbol","edge","search","snippet"},
    "code_index_coverage":{"report","gaps","stale"},
    "code_index_trace":{"symbol","selector","event","endpoint","data_attribute","import","asset"},
    "code_index_progress":{"list","get"}}
FIELDS={"code_index_scan":{"scope","file_path","batch_id","expected_revision"},
    "code_index_lookup":{"query","file_path","line_no","start_line","end_line","max_chars"},
    "code_index_coverage":set(),"code_index_trace":{"kind","value"},
    "code_index_progress":{"batch_id"}}
def validated(name,args):
    if name not in ACTIONS or not isinstance(args,dict) or set(args)-({"root","limit","offset"}|FIELDS[name]|({"action"} if name!="code_index_trace" else set())):
        raise ValueError("Invalid Code Index arguments")
    action=args.get("action",{"code_index_coverage":"report","code_index_progress":"list"}.get(name,""))
    if name=="code_index_trace":action=args.get("kind","")
    if action not in ACTIONS[name]:raise ValueError("Unsupported Code Index action")
    # Providers may populate every optional schema property. Only this action's
    # fields carry semantics; absent mutation fields cannot accidentally appoint paths.
    fields={
        "code_index_scan":{"discover":{"scope"},"scan":{"scope","batch_id","expected_revision"},
            "scan_file":{"file_path","batch_id","expected_revision"}},
        "code_index_lookup":{"file":{"file_path"},"line":{"file_path","line_no"},
            "symbol":{"query","file_path"},"edge":{"query","file_path"},
            "search":{"query"},"snippet":{"file_path","start_line","end_line","max_chars"}},
        "code_index_coverage":{"report":set(),"gaps":set(),"stale":set()},
        "code_index_trace":{kind:{"value"} for kind in ACTIONS["code_index_trace"]},
        "code_index_progress":{"list":set(),"get":{"batch_id"}},
    }[name][action]
    result={key:value for key,value in args.items() if key in fields|{"root","limit","offset"}}
    result["action"]=action
    result.setdefault("root","webagent-v3")
    if result["root"]=="":result["root"]="webagent-v3"
    result.setdefault("limit",50);result.setdefault("offset",0)
    if result.get("batch_id")=="" and name=="code_index_scan":result.pop("batch_id")
    if result.get("file_path")=="" and name=="code_index_lookup" and action in {"symbol","edge"}:
        result.pop("file_path")
    if name=="code_index_scan" and action=="scan_file" or name=="code_index_lookup" and action in {"file","line","snippet"}:
        if not result.get("file_path"):raise ValueError("This action requires file_path")
    if name=="code_index_lookup" and action=="search" and not result.get("query", "").strip():
        raise ValueError("Search requires a non-empty query")
    if name=="code_index_progress" and action=="get" and not result.get("batch_id"):
        raise ValueError("get progress requires batch_id")
    if type(result["limit"]) is not int or not 1<=result["limit"]<=200 or type(result["offset"]) is not int or not 0<=result["offset"]<=MAX_FILES:
        raise ValueError("Invalid result page bounds")
    relative_parts(result["root"],True)
    for field in ("scope","file_path"):
        if field in result:relative_parts(result[field],field=="scope")
    for field in ("query","value"):
        if field in result and (not isinstance(result[field],str) or len(result[field].encode())>512 or "\0" in result[field]):
            raise ValueError("Invalid query bound")
    if "line_no" in result and (type(result["line_no"]) is not int or not 1<=result["line_no"]<=MAX_LINES):
        raise ValueError("Invalid line number")
    for field in ("start_line","end_line"):
        if field in result and (type(result[field]) is not int or not 1<=result[field]<=MAX_LINES):
            raise ValueError("Invalid snippet line bound")
    if name=="code_index_lookup" and action=="snippet":
        if result.get("start_line",0)>result.get("end_line",0) or result.get("end_line",0)-result.get("start_line",0)>79:
            raise ValueError("Snippet may contain at most 80 ordered lines")
        result.setdefault("max_chars",12000)
        if type(result["max_chars"]) is not int or not 256<=result["max_chars"]<=12000:
            raise ValueError("Invalid snippet character bound")
    if "expected_revision" in result and (type(result["expected_revision"]) is not int or not 0<=result["expected_revision"]<2**63):
        raise ValueError("Invalid index revision")
    if "batch_id" in result and (not isinstance(result["batch_id"],str) or not re.fullmatch(r"[a-zA-Z0-9_-]{1,80}",result["batch_id"])):
        raise ValueError("Invalid batch_id")
    return result
SEARCH_PATH_RE = re.compile(r"(?<![\w./-])(?:[\w.-]+/)+[\w.-]+\.[A-Za-z0-9]{1,12}(?![\w./-])")
SEARCH_STOP_WORDS = {"a", "an", "and", "are", "as", "at", "be", "call", "codebase", "do", "explain", "for", "from", "grounded", "how", "if", "in", "is", "it", "no", "of", "only", "or", "please", "retrieved", "short", "snippets", "source", "supplied", "the", "this", "to", "tools", "use", "using", "with"}
def search_path_key(value):
    parts=[part.lower() for part in value.replace("\\", "/").split("/") if part not in ("", ".")]
    while parts and parts[0] in {"v3", "webagent-v3"}:
        parts.pop(0)
    return "/".join(parts)
def search_terms(query, paths):
    residual=query
    for path in paths:
        residual=residual.replace(path, " ")
    return sorted({term.lower() for term in re.findall(r"[A-Za-z0-9_]{2,}",residual)
                   if term.lower() not in SEARCH_STOP_WORDS})[:24]
def search_path_rank(candidate, paths):
    candidate_key=search_path_key(candidate)
    rank=0
    for path in paths:
        key=search_path_key(path)
        if candidate_key==key:
            rank=max(rank,3)
        elif candidate_key.endswith("/"+key):
            rank=max(rank,2)
        elif key.endswith("/"+candidate_key):
            rank=max(rank,1)
    return rank

def lookup(conn,rootfd,args):
    action=args["action"];path=args.get("file_path");query=args.get("query","")
    if action=="file":
        rows=conn.execute("SELECT * FROM files WHERE (? IS NULL OR path=?) ORDER BY path",(path,path)).fetchall()
    elif action=="line":
        if not path or "line_no" not in args:raise ValueError("line lookup needs file_path and line_no")
        rows=conn.execute("SELECT * FROM lines WHERE file_path=? AND line_no=?",(path,args["line_no"])).fetchall()
    elif action=="symbol":
        rows=conn.execute("SELECT * FROM symbols WHERE instr(name,?)>0 AND (? IS NULL OR file_path=?) ORDER BY file_path,start_line,name LIMIT 5001",(query,path,path)).fetchall()
    elif action=="edge":
        rows=conn.execute("SELECT * FROM edges WHERE instr(value,?)>0 AND (? IS NULL OR file_path=?) ORDER BY file_path,line_no,value LIMIT 5001",(query,path,path)).fetchall()
    elif action=="search":
        explicit_paths=SEARCH_PATH_RE.findall(query)
        terms=search_terms(query,explicit_paths)
        if not terms:
            terms=[term.lower() for term in re.findall(r"[A-Za-z0-9_]{2,}",query) if term.lower() not in SEARCH_STOP_WORDS][:24]
        if not terms:raise ValueError("Search query has no indexable terms")
        # The files table is bounded to MAX_FILES; load its display metadata once
        # rather than issuing a query for every candidate result.
        metadata={row["path"]:{"line_count":row["line_count"],"parser_status":row["parser_status"]}
                  for row in conn.execute("SELECT path,line_count,parser_status FROM files ORDER BY path LIMIT 5000")}
        candidates={}
        def add_candidate(key,item,haystack,symbol_name=None):
            hits=sum(1 for term in terms if term in haystack.lower())
            path_rank=search_path_rank(item["file_path"],explicit_paths)
            symbol_rank=2 if symbol_name and symbol_name.lower() in terms else 1 if symbol_name and any(term in symbol_name.lower() for term in terms) else 0
            if not hits and not path_rank:return
            item["score"]=round(hits/len(terms),4)
            # score is lexical overlap; ranking_score adds deterministic path/symbol
            # boosts for ordering and is not a Jev/provider confidence score.
            item["ranking_score"]=round(path_rank*100+symbol_rank*10+item["score"],4)
            item.update(metadata.get(item["file_path"],{}))
            item["_path_rank"]=path_rank
            item["_symbol_rank"]=symbol_rank
            previous=candidates.get(key)
            if previous is None or (path_rank,symbol_rank,item["score"])>(previous["_path_rank"],previous["_symbol_rank"],previous["score"]):
                candidates[key]=item
        for row in conn.execute("SELECT file_path,name,kind,start_line,end_line,signature FROM symbols ORDER BY file_path,start_line LIMIT 20000"):
            item={"file_path":row["file_path"],"start_line":row["start_line"],"end_line":row["end_line"],"label":row["name"],"kind":row["kind"]}
            add_candidate((row["file_path"],row["start_line"],row["end_line"]),item,
                row["file_path"]+" "+row["name"]+" "+row["signature"],row["name"])
        for row in conn.execute("SELECT file_path,line_no,edge_kind,value FROM edges ORDER BY file_path,line_no LIMIT 20000"):
            item={"file_path":row["file_path"],"start_line":row["line_no"],"end_line":row["line_no"],"label":row["value"],"kind":row["edge_kind"]}
            add_candidate((row["file_path"],row["line_no"],row["line_no"]),item,
                row["file_path"]+" "+row["edge_kind"]+" "+row["value"])
        for path in metadata:
            item={"file_path":path,"start_line":1,"end_line":1,"label":path,"kind":"file"}
            add_candidate((path,1,1),item,path)
        feature_groups={}
        for row in conn.execute("SELECT * FROM feature_locations ORDER BY feature_id,file_path,start_line LIMIT ?",(MAX_FILES+MAX_FEATURES*MAX_FEATURE_LOCATIONS,)):
            group=feature_groups.setdefault(row["feature_id"],{"aliases":[],"purpose":row["purpose"],"map_purpose":None,"rows":[]})
            if row["origin"]=="map":group["map_purpose"]=row["purpose"]
            group["rows"].append(row)
            for alias in json.loads(row["aliases"]):
                if len(group["aliases"])<16 and alias.casefold() not in {existing.casefold() for existing in group["aliases"]}:
                    group["aliases"].append(alias)
        query_norm=" ".join(re.findall(r"[a-z0-9]+",query.lower()))
        linked=[]
        for fid,group in feature_groups.items():
            labels=[fid.replace("-"," "),*group["aliases"]]
            normalized=[" ".join(re.findall(r"[a-z0-9]+",label.lower())) for label in labels]
            padded=" "+query_norm+" "
            exact_len=max((len(label) for label in normalized if label and (" "+label+" ") in padded),default=0)
            purpose=group["map_purpose"] or group["purpose"]
            purposes=" ".join(dict.fromkeys(row["location_purpose"] for row in group["rows"][:16]))
            hay=" ".join([fid,*group["aliases"],purpose,purposes]).lower()
            hits=sum(1 for term in terms if term in hay)
            if not exact_len and not hits:continue
            per_file={}
            for row in group["rows"]:
                item_hay=(hay+" "+row["location_purpose"]).lower()
                local_hits=sum(1 for term in terms if term in item_hay)
                path_rank=search_path_rank(row["file_path"],explicit_paths)
                item={"file_path":row["file_path"],"start_line":row["start_line"],"end_line":row["end_line"],
                    "label":fid,"kind":"code_index_feature","feature_id":fid,"aliases":group["aliases"],
                    "purpose":purpose,"location_purpose":row["location_purpose"],"feature_origin":row["origin"],
                    "source_path":row["source_path"],"source_sha256":row["source_sha256"],"target_sha256":row["target_sha256"],
                    "score":round(max(hits,local_hits)/len(terms),4),
                    "ranking_score":round(1000+exact_len*10+path_rank*10+max(hits,local_hits)/len(terms),4),
                    "_exact_len":exact_len,"_path_rank":path_rank,"_hits":max(hits,local_hits),**metadata.get(row["file_path"],{})}
                previous=per_file.get(row["file_path"])
                if previous is None or (item["_path_rank"],item["_hits"],-item["start_line"])>(previous["_path_rank"],previous["_hits"],-previous["start_line"]):
                    per_file[row["file_path"]]=item
            linked.append(((-exact_len,-max((x["_path_rank"] for x in per_file.values()),default=0),-hits,fid),list(per_file.values())))
        linked.sort(key=lambda pair:pair[0])
        features=[]
        for _,items in linked:
            items.sort(key=lambda x:(-x["_path_rank"],-x["_hits"],x["file_path"],x["start_line"]))
            features.extend(items[:MAX_FEATURE_LOCATIONS])
        feature_keys={(x["file_path"],x["start_line"],x["end_line"]) for x in features}
        ordinary=sorted(candidates.values(),key=lambda item:(-item["_path_rank"],-item["_symbol_rank"],-item["score"],item["file_path"],item["start_line"]))
        ordinary=[item for item in ordinary
                  if (item["file_path"],item["start_line"],item["end_line"]) not in feature_keys]
        rows=features+ordinary
        if explicit_paths:
            rows.sort(key=lambda item:(-item.get("_path_rank",search_path_rank(item["file_path"],explicit_paths)),
                0 if item["kind"]=="code_index_feature" else 1,-item.get("_exact_len",0),-item.get("_hits",0),
                -item.get("_symbol_rank",0),-item["score"],item["file_path"],item["start_line"]))
        rows=[{key:value for key,value in item.items() if not key.startswith("_")} for item in rows][:MAX_FILES+1]
        result=page(rows,args["offset"],args["limit"]);result["result_cap_reached"]=len(rows)>MAX_FILES
        return result
    elif action=="snippet":
        path=args["file_path"];metadata=conn.execute("SELECT sha256 FROM files WHERE path=?",(path,)).fetchone()
        if metadata is None:raise ValueError("File is not indexed")
        data=read_source(rootfd,path)
        if sha(data)!=metadata["sha256"]:raise ValueError("Indexed source is stale; rescan before reading")
        lines=data.decode("utf-8").splitlines();start=args["start_line"];end=args["end_line"]
        if end>len(lines):raise ValueError("Snippet line range exceeds source")
        text="\n".join(lines[start-1:end])
        while len(text.encode("utf-8"))>args["max_chars"] and end>start:
            end-=1;text="\n".join(lines[start-1:end])
        if len(text.encode("utf-8"))>args["max_chars"]:raise ValueError("Single source line exceeds snippet character bound")
        return {"results":[{"file_path":path,"start_line":start,"end_line":end,"sha256":metadata["sha256"],"text":text,"stale":False}],"offset":0,"limit":1,"total":1,"result_cap_reached":False}
    else:
        raise ValueError("Unsupported lookup action")
    result=page([dict(row) for row in rows[:MAX_FILES]],args["offset"],args["limit"])
    result["result_cap_reached"]=len(rows)>MAX_FILES
    return result
def trace(conn,args):
    value=args.get("value")
    if value is None:raise ValueError("trace requires value")
    kind=args["action"]
    if kind=="symbol":rows=conn.execute("SELECT * FROM symbols WHERE name=? ORDER BY file_path,start_line LIMIT 5001",(value,)).fetchall()
    else:rows=conn.execute("SELECT * FROM edges WHERE edge_kind=? AND value=? ORDER BY file_path,line_no LIMIT 5001",(kind,value)).fetchall()
    result=page([dict(row) for row in rows[:MAX_FILES]],args["offset"],args["limit"])
    result.update(kind=kind,value=value,result_cap_reached=len(rows)>MAX_FILES)
    return result
def annotate_freshness(conn,rootfd,result):
    paths={row.get("file_path",row.get("path")) for row in result["results"]}
    paths.update(row["source_path"] for row in result["results"] if row.get("source_path"))
    state={};total=0
    for path in sorted(paths):
        expected=conn.execute("SELECT sha256 FROM files WHERE path=?",(path,)).fetchone()
        try:
            data=read_source(rootfd,path);total+=len(data)
            stale=expected is None or sha(data)!=expected[0]
        except (OSError,ValueError):stale=True
        if total>MAX_TOTAL_BYTES:raise ValueError("Lookup freshness byte limit exceeded")
        state[path]=stale
    for row in result["results"]:
        row["stale"]=state[row.get("file_path",row.get("path"))] or bool(row.get("source_path") and state.get(row["source_path"],True))
    result.update(stale_count=sum(state.values()),freshness_scope="returned_files")
    return result
def dispatch(conn,rootfd,name,args):
    if name=="code_index_scan":
        if args["action"]!="discover":return scan(conn,rootfd,args)
        paths=source_files(rootfd,args.get("scope",""),time.monotonic()+30)
        known={row["path"]:row["sha256"] for row in conn.execute("SELECT path,sha256 FROM files")}
        records=snapshot(rootfd,paths,time.monotonic()+30)
        return page([{"path":p,"indexed":p in known,"stale":p in known and sha(data)!=known[p]} for p,data in records],args["offset"],args["limit"])
    if name=="code_index_lookup":return annotate_freshness(conn,rootfd,lookup(conn,rootfd,args))
    if name=="code_index_coverage":return coverage(conn,rootfd,args)
    if name=="code_index_trace":return annotate_freshness(conn,rootfd,trace(conn,args))
    if args["action"]=="get":
        if "batch_id" not in args:raise ValueError("get progress requires batch_id")
        rows=conn.execute("SELECT * FROM batches WHERE batch_id=?",(args["batch_id"],)).fetchall()
    else:rows=conn.execute("SELECT * FROM batches ORDER BY started_at DESC LIMIT 100").fetchall()
    return page([dict(row) for row in rows],args["offset"],args["limit"])
def publish(conn,indexfd,key):
    blob=conn.serialize()
    if len(blob)>MAX_DB_BYTES:raise ValueError("Derived index publication exceeds limit")
    temp=key+"."+uuid.uuid4().hex+".tmp"
    tempfd=os.open(temp,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=indexfd)
    try:
        with os.fdopen(tempfd,"wb",closefd=False) as output:
            output.write(blob);output.flush();os.fsync(tempfd)
        os.rename(temp,key+".sqlite",src_dir_fd=indexfd,dst_dir_fd=indexfd)
        os.fsync(indexfd)
    finally:
        os.close(tempfd)
        try:os.unlink(temp,dir_fd=indexfd)
        except FileNotFoundError:pass

# The release builder uses the installed, protected copy of this parser. Candidate
# source is data only; neither its adapter nor any submitted Python is imported.
INDEX_VERSION = 2

def parser_versions():
    from importlib.metadata import version, PackageNotFoundError
    result = {"python_ast":os.sys.version.split()[0], "rust":"lexical", "toml":"lexical"}
    for package in ("tree-sitter", "tree-sitter-javascript"):
        try:result[package] = version(package)
        except PackageNotFoundError:result[package] = "unavailable"
    return result

def source_digest(root):
    root = Path(root)
    rows = []
    total = 0
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root).as_posix()
        if rel.split("/")[0] in {".code-index", ".git"}:
            continue
        if path.is_symlink():
            raise ValueError("Published source symlink forbidden")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError("Published source must be regular")
        info = path.stat()
        if info.st_size > 16 * 1024 * 1024 or len(rows) >= MAX_ENTRIES:
            raise ValueError("Published source manifest bound exceeded")
        total += info.st_size
        if total > 128 * 1024 * 1024:
            raise ValueError("Published source byte bound exceeded")
        data = path.read_bytes()
        rows.append([rel, info.st_mode & 0o100, sha(data)])
    return sha(json.dumps(rows, separators=(",", ":")).encode())

def build_published(root, output, release_id, candidate_digest):
    """Build a complete fresh index; publish no files until coverage validates."""
    uuid.UUID(release_id)
    if not re.fullmatch("[0-9a-f]{64}", candidate_digest):
        raise ValueError("Invalid candidate digest")
    root, output = Path(root), Path(output)
    before = source_digest(root)
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    conn = sqlite3.connect(":memory:")
    try:
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA foreign_keys=ON")
        conn.executescript(DDL)
        scan(conn, fd, {"action":"scan", "scope":"", "limit":1, "offset":0})
        report = coverage(conn, fd, {"action":"report", "limit":1, "offset":0})
        if any(report[k] for k in ("gaps", "stale_count", "unindexed_count")):
            raise ValueError("Published index coverage incomplete")
        if source_digest(root) != before:
            raise ValueError("Published source changed during indexing")
        excluded = []
        supported = set(source_files(fd, "", time.monotonic()+30))
        for path in sorted(root.rglob("*")):
            if not path.is_file():
                continue
            rel = path.relative_to(root).as_posix()
            if rel not in supported:
                reason = ("excluded_directory" if any(p in EXCLUDED for p in path.relative_to(root).parts)
                          else "generated_minified" if path.name.endswith((".min.js", ".min.css"))
                          else "unsupported_suffix")
                excluded.append({"path":rel, "reason":reason})
        blob = conn.serialize()
        if len(blob) > MAX_DB_BYTES:
            raise ValueError("Published index database bound exceeded")
        limitations = [dict(row) for row in conn.execute(
            "SELECT path,parser_status,parser_notes FROM files WHERE parser_status!='ok' ORDER BY path")]
        manifest = {"index_version":INDEX_VERSION, "schema_version":1,
            "parser_sha256":sha(Path(__file__).read_bytes()), "parser_versions":parser_versions(), "release_id":release_id,
            "candidate_sha256":candidate_digest, "source_sha256":before,
            "index_sha256":sha(blob), "file_count":report["file_count"],
            "total_lines":report["total_lines"], "gaps":0, "stale_count":0,
            "unindexed_count":0, "exclusions":excluded, "parser_limitations":limitations}
        encoded = json.dumps(manifest,sort_keys=True).encode()
        if len(encoded) > 2*1024*1024:
            raise ValueError("Published manifest bound exceeded")
        output.mkdir(exist_ok=True)
        (output/"index.sqlite").write_bytes(blob)
        (output/"manifest.json").write_bytes(encoded)
        for path in (output/"index.sqlite", output/"manifest.json"):
            path.chmod(0o644)
            with path.open("rb") as stream:
                os.fsync(stream.fileno())
        return manifest
    finally:
        conn.close()
        os.close(fd)

def read_published(root, expected_release=None):
    root = Path(root)
    folder = root/".code-index"
    if root.is_symlink() or folder.is_symlink():
        raise ValueError("Published index path invalid")
    manifest_path, dbpath = folder/"manifest.json", folder/"index.sqlite"
    if any(p.is_symlink() or not p.is_file() for p in (manifest_path,dbpath)):
        raise ValueError("Published index unavailable")
    if manifest_path.stat().st_size>2*1024*1024 or dbpath.stat().st_size>MAX_DB_BYTES:
        raise ValueError("Published index bound exceeded")
    manifest = json.loads(manifest_path.read_bytes())
    blob = dbpath.read_bytes()
    if (manifest.get("index_version")!=INDEX_VERSION or manifest.get("schema_version")!=1
        or manifest.get("index_sha256")!=sha(blob)
        or manifest.get("source_sha256")!=source_digest(root)
        or any(manifest.get(k)!=0 for k in ("gaps","stale_count","unindexed_count"))
        or expected_release is not None and manifest.get("release_id")!=expected_release):
        raise ValueError("Published source/index binding mismatch")
    return blob, manifest


def seed_private_index(release, stage):
    """Project a verified complete release into its private V3-only seed."""
    blob, manifest = read_published(release, Path(release).name)
    conn = sqlite3.connect(":memory:")
    try:
        conn.deserialize(blob)
        conn.execute("PRAGMA trusted_schema=OFF")
        # This is a fresh private projection, not a mutable published database.
        conn.execute("PRAGMA foreign_keys=OFF")
        for table, column in (("files","path"),("lines","file_path"),
                              ("symbols","file_path"),("edges","file_path")):
            conn.execute(f"DELETE FROM {table} WHERE {column} NOT LIKE 'v3/%'")
            conn.execute(f"UPDATE {table} SET {column}=substr({column},4)")
        conn.execute("DELETE FROM feature_locations WHERE file_path NOT LIKE 'v3/%'")
        conn.execute("UPDATE feature_locations SET file_path=substr(file_path,4) WHERE file_path LIKE 'v3/%'")
        conn.execute("UPDATE feature_locations SET source_path=substr(source_path,4) WHERE source_path LIKE 'v3/%'")
        conn.execute("DELETE FROM batches")
        conn.commit()
        if conn.execute("PRAGMA foreign_key_check").fetchone():
            raise ValueError("Private index projection is inconsistent")
        private = conn.serialize()
        manifest = dict(manifest)
        manifest.update(source_sha256=source_digest(stage), index_sha256=sha(private),
            file_count=conn.execute("SELECT COUNT(*) FROM files").fetchone()[0],
            total_lines=conn.execute("SELECT COALESCE(SUM(line_count),0) FROM files").fetchone()[0])
        for field in ("exclusions","parser_limitations"):
            manifest[field]=[{**item,"path":item["path"][3:]} for item in manifest[field]
                             if item["path"].startswith("v3/")]
        folder=Path(stage)/".code-index";folder.mkdir(mode=0o700)
        (folder/"index.sqlite").write_bytes(private)
        (folder/"manifest.json").write_text(json.dumps(manifest,sort_keys=True))
        read_published(stage)
    finally:
        conn.close()

def execute_published(name, args, release):
    """Called only after Rust authorization; one resolved fixed release per call."""
    conn = None
    fd = None
    try:
        args = validated(name,args)
        if name=="code_index_scan" and args["action"]!="discover":
            raise ValueError("Published index is immutable; scan a private workspace")
        blob, manifest = read_published(Path(release), Path(release).name)
        conn = sqlite3.connect(":memory:")
        conn.deserialize(blob)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA trusted_schema=OFF")
        conn.execute("PRAGMA query_only=ON")
        fd = os.open(Path(release),os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
        result = dispatch(conn,fd,name,args)
        result.update(status="success",root="published",source_kind="untrusted_source_index",
                      publication={k:manifest[k] for k in ("release_id","source_sha256",
                        "candidate_sha256","index_version","parser_sha256","index_sha256")},
                      parser_limitations=page(manifest["parser_limitations"],args["offset"],args["limit"]),
                      exclusions=page(manifest["exclusions"],args["offset"],args["limit"]),
                      auto_rescan=False)
        if len(json.dumps(result).encode())>MAX_OUTPUT_BYTES:
            raise ValueError("Published index result exceeds response limit")
        return result
    except (OSError,ValueError,sqlite3.Error,KeyError) as exc:
        return {"status":"error","error_kind":"index_rejected","output":str(exc)[:300]}
    finally:
        if conn is not None:conn.close()
        if fd is not None:os.close(fd)

def read_seeded_index(rootfd):
    """Read bootstrap bytes through the already confined source inode."""
    folder=open_child_dir(rootfd,".code-index")
    def bounded(name,limit):
        fd=os.open(name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=folder)
        try:
            before=os.fstat(fd)
            if not stat.S_ISREG(before.st_mode) or before.st_nlink!=1 or before.st_size>limit:
                raise ValueError("Invalid seeded index file")
            raw=bytearray()
            while len(raw)<=limit:
                chunk=os.read(fd,min(65536,limit+1-len(raw)))
                if not chunk:break
                raw.extend(chunk)
            after=os.fstat(fd)
            if len(raw)>limit or (before.st_ino,before.st_size,before.st_mtime_ns)!=(after.st_ino,after.st_size,after.st_mtime_ns):
                raise ValueError("Seeded index changed while reading")
            return bytes(raw)
        finally:os.close(fd)
    try:
        manifest=json.loads(bounded("manifest.json",2*1024*1024))
        blob=bounded("index.sqlite",MAX_DB_BYTES)
        if (not isinstance(manifest,dict) or manifest.get("index_version")!=INDEX_VERSION
            or manifest.get("schema_version")!=1 or manifest.get("index_sha256")!=sha(blob)
            or any(manifest.get(k)!=0 for k in ("gaps","stale_count","unindexed_count"))):
            raise ValueError("Seeded index binding mismatch")
        # Private edits are allowed; dispatch checks row freshness on rootfd.
        return blob
    finally:os.close(folder)


def execute(name,args,base):
    """Rust supplies base after caller/session policy. Never accept a runtime DB path."""
    fds=[];conn=None;indexfd=None;key=None;save=False
    try:
        args=validated(name,args);basefd=os.open(base,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW);fds.append(basefd)
        rootfd=open_path(basefd,relative_parts(args["root"],True),True);fds.append(rootfd)
        try:os.mkdir(".code-index",mode=0o700,dir_fd=basefd)
        except FileExistsError:pass
        indexfd=open_child_dir(basefd,".code-index");fds.append(indexfd)
        key=sha(args["root"].encode())
        lockfd=os.open(key+".lock",os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW,0o600,dir_fd=indexfd);fds.append(lockfd)
        if not stat.S_ISREG(os.fstat(lockfd).st_mode) or os.fstat(lockfd).st_nlink!=1:
            raise ValueError("Invalid index lock file")
        fcntl.flock(lockfd,fcntl.LOCK_EX|fcntl.LOCK_NB)
        dbfd=os.open(key+".sqlite",os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW,0o600,dir_fd=indexfd);fds.append(dbfd)
        meta=os.fstat(dbfd)
        if not stat.S_ISREG(meta.st_mode) or meta.st_nlink!=1 or meta.st_size>MAX_DB_BYTES:raise ValueError("Invalid derived index file")
        # Never give SQLite a caller-writable filename. Deserialize the confined
        # derived inode into memory; atomic publication below uses anchored fds.
        raw=bytearray()
        while len(raw)<=MAX_DB_BYTES:
            chunk=os.read(dbfd,min(65536,MAX_DB_BYTES+1-len(raw)))
            if not chunk:break
            raw.extend(chunk)
        if len(raw)>MAX_DB_BYTES:raise ValueError("Derived index exceeds limit")
        # A newly seeded source may carry an immutable bootstrap index.
        # Existing private rows always win, including matching isolated edits.
        if not raw:
            try:raw.extend(read_seeded_index(rootfd))
            except (OSError,ValueError):
                # Missing or unusable bootstrap: rebuild inside this workspace.
                pass
        conn=sqlite3.connect(":memory:",timeout=1)
        if raw:conn.deserialize(bytes(raw))
        conn.row_factory=sqlite3.Row
        conn.execute("PRAGMA trusted_schema=OFF")
        conn.execute("PRAGMA journal_mode=MEMORY");conn.execute("PRAGMA foreign_keys=ON")
        conn.execute("PRAGMA max_page_count="+str(MAX_DB_BYTES//4096))
        conn.executescript(DDL)
        if conn.execute("SELECT schema_version FROM meta WHERE id=1").fetchone()[0]!=1:raise ValueError("Unsupported derived index schema")
        save=name=="code_index_scan" and args["action"]!="discover"
        result=dispatch(conn,rootfd,name,args)
        result.update(status="success",root=args["root"],revision=conn.execute("SELECT revision FROM meta WHERE id=1").fetchone()[0],
            source_kind="untrusted_source_index",auto_rescan=False)
        if len(json.dumps(result,ensure_ascii=False).encode())>MAX_OUTPUT_BYTES:raise ValueError("Index result exceeds response limit; request a smaller page")
        if save:publish(conn,indexfd,key);save=False
        return result
    except BlockingIOError:return {"status":"error","error_kind":"index_busy","output":"Index scan already running"}
    except (OSError,ValueError,sqlite3.Error,RecursionError) as exc:
        if save and conn is not None:
            try:publish(conn,indexfd,key)
            except (OSError,ValueError,sqlite3.Error):pass
        return {"status":"error","error_kind":"index_rejected","output":type(exc).__name__+": "+str(exc)[:300]}
    finally:
        if conn is not None:conn.close()
        for fd in reversed(fds):os.close(fd)
def schemas():
    path={"type":"string","maxLength":512}
    text={"type":"string","maxLength":512}
    common={"root":path,"limit":{"type":"integer","minimum":1,"maximum":200},
        "offset":{"type":"integer","minimum":0,"maximum":MAX_FILES}}
    extra={
        "code_index_scan":{"scope":path,"file_path":path,"batch_id":{"type":"string","maxLength":80},
            "expected_revision":{"type":"integer","minimum":0}},
        "code_index_lookup":{"query":text,"file_path":path,"line_no":{"type":"integer","minimum":1,"maximum":MAX_LINES},"start_line":{"type":"integer","minimum":1,"maximum":MAX_LINES},"end_line":{"type":"integer","minimum":1,"maximum":MAX_LINES},"max_chars":{"type":"integer","minimum":256,"maximum":12000}},
        "code_index_coverage":{},
        "code_index_trace":{"kind":{"type":"string","enum":sorted(ACTIONS["code_index_trace"])},"value":text},
        "code_index_progress":{"batch_id":{"type":"string","maxLength":80}}}
    result={}
    for name in ACTIONS:
        properties={**common,**extra[name]}
        if name!="code_index_trace":properties["action"]={"type":"string","enum":sorted(ACTIONS[name])}
        required={"code_index_scan":["action"],"code_index_lookup":["action"],
            "code_index_trace":["kind","value"]}.get(name,[])
        result[name]={"type":"object","properties":properties,"required":required,"additionalProperties":False}
    return result
