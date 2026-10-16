"""Frozen task contexts and versioned development release mounts."""
import io
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import stat
import tarfile
import uuid


# Task-only troubleshooting data; never part of an image/release candidate.
INSPECTION_CONTEXT = ("v3/data/user_data/admin", "v3/data/agent_data/shared_default")
MAX_INSPECTION_FILE = 256 * 1024 * 1024


def inspection_path(name):
    return any(name == prefix or name.startswith(prefix + "/")
               for prefix in INSPECTION_CONTEXT)


def entries(blob, validate):
    validate(blob)
    result = {}
    with tarfile.open(fileobj=io.BytesIO(blob), mode="r:") as archive:
        for member in archive:
            if member.isfile():
                result[member.name.removeprefix("./")] = (
                    member.mode & 0o777, archive.extractfile(member).read())
    return result


def archive(files, validate):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w") as stream:
        for name, (mode, body) in sorted(files.items()):
            info = tarfile.TarInfo(name)
            info.size, info.mode = len(body), mode
            stream.addfile(info, io.BytesIO(body))
    blob = output.getvalue()
    validate(blob)
    return blob


def extract(root, blob, validate):
    if root.exists():
        raise ValueError("release snapshot already exists")
    root.mkdir(parents=True, mode=0o700)
    for name, (mode, body) in entries(blob, validate).items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("xb") as output:
            output.write(body)
        path.chmod(mode)
        with path.open("rb") as stream:
            os.fsync(stream.fileno())
    # Persist staged source directories before any release pointer selects them.
    for directory in sorted((p for p in root.rglob("*") if p.is_dir()),
                            key=lambda p:len(p.parts),reverse=True):
        fd=os.open(directory,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
        try:os.fsync(fd)
        finally:os.close(fd)
    fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try:os.fsync(fd)
    finally:os.close(fd)


def snapshot(root, allowlist, validate):
    files = {}
    for allowed in allowlist:
        path = root / allowed
        if not path.exists():
            continue
        paths = [path] if path.is_file() else path.rglob("*")
        for entry in paths:
            if entry.is_symlink():
                raise ValueError("source workspace symlink forbidden")
            if entry.is_dir():
                continue
            if not entry.is_file():
                raise ValueError("source workspace special file forbidden")
            name = entry.relative_to(root).as_posix()
            if "__pycache__" in entry.parts or name.endswith((".pyc", ".pyo")):
                continue
            info = entry.stat()
            if info.st_size > (MAX_INSPECTION_FILE if inspection_path(name) else 16 * 1024 * 1024) or len(files) > 20000:
                raise ValueError("source workspace input bound exceeded")
            files[name] = (info.st_mode & 0o777, entry.read_bytes())
    return archive(files, validate)


def candidate_entries(blob,validate):
    """Validate every member while retaining source bytes and only streamed inspection attestations."""
    validate(blob);source={};inspection={};seen=set()
    with tarfile.open(fileobj=io.BytesIO(blob),mode="r:")as archive:
        for member in archive:
            if not member.isfile():continue
            name=member.name.removeprefix("./")
            if name in seen:raise ValueError("Duplicate candidate input")
            seen.add(name)
            with archive.extractfile(member)as body:
                if inspection_path(name):
                    if member.size>MAX_INSPECTION_FILE:raise ValueError("Inspection input exceeds bound")
                    digest=hashlib.sha256();size=0
                    while chunk:=body.read(1024*1024):digest.update(chunk);size+=len(chunk)
                    if size!=member.size:raise ValueError("Incomplete inspection input")
                    inspection[name]=(member.mode&0o777,size,digest.hexdigest())
                else:source[name]=(member.mode&0o777,body.read())
    return source,inspection


def candidate(base_blob, task_blob, live_blob, validate, *, allow_empty=False):
    (base,base_inspection),(task,task_inspection),(live,_) = (candidate_entries(value,validate)for value in (base_blob,task_blob,live_blob))
    # Same attested identity without holding private inspection bodies in memory.
    if base_inspection!=task_inspection:raise ValueError("read-only inspection snapshot changed")
    changed = [name for name in sorted(set(base) | set(task))
               if base.get(name) != task.get(name)]
    if (not changed and not allow_empty) or len(changed) > 256 or any(
            not name.startswith("v3/") or name.startswith("v3/data/")
            for name in changed):
        raise ValueError("isolated task has no bounded V3 source changes")
    merged = dict(live)
    for name in changed:
        if live.get(name) not in (base.get(name), task.get(name)):
            raise ValueError("source conflict: " + name)
        if name in task:
            merged[name] = task[name]
        else:
            merged.pop(name, None)
    return archive(merged, validate), changed


def compensating_candidate(before_blob,after_blob,live_blob,owned_paths,validate):
    """Invert only one receipt's changed source onto latest source, preserving later independent edits."""
    before,_=candidate_entries(before_blob,validate);after,_=candidate_entries(after_blob,validate)
    paths=sorted(set(owned_paths))
    if not paths or len(paths)>256 or any(not path.startswith("v3/")or path.startswith("v3/data/")for path in paths):raise ValueError("Bounded receipt-owned source paths required")
    reverse=dict(after)
    for path in paths:
        if path in before:reverse[path]=before[path]
        else:reverse.pop(path,None)
    # Reuse the normal explicit same-file conflict contract; no automatic hunk merge.
    return candidate(archive(after,validate),archive(reverse,validate),live_blob,validate)


def release_mounts(previous, root, asset_root=None):
    """Pin code mounts and expose the read-only active guidance snapshot to API and loop."""
    mapping = {
        "/live-v3": "v3",
        "/opt/v3/config": "v3/config",
        "/opt/v3/ui": "v3/ui",
        "/opt/v3/plugins/abilities": "v3/plugins/abilities",
        "/opt/v3/shared-ui/shared/css": "ui/shared/css",
        "/opt/v3/shared-ui/main-panel/agents": "ui/main-panel/agents",
        "/opt/v3/shared-ui/vendor/fonts": "ui/vendor/fonts",
        "/opt/v3/shared-ui/vendor/lucide": "ui/vendor/lucide",
        "/opt/v3/shared-ui/favicon.svg": "ui/favicon.svg",
    }
    host = dict(previous)
    mounts = []
    seen = set()
    api = any(item.get("Target") == "/live-checkout"
              for item in host.get("Mounts") or [])
    legacy_assets = {"/opt/v3/ui", "/opt/v3/shared-ui/shared/css",
                     "/opt/v3/shared-ui/main-panel/agents",
                     "/opt/v3/shared-ui/vendor/fonts",
                     "/opt/v3/shared-ui/vendor/lucide",
                     "/opt/v3/shared-ui/favicon.svg"}
    for mount in host.get("Mounts") or []:
        mount = dict(mount)
        target = mount.get("Target")
        if api and asset_root is not None and target in legacy_assets:
            continue
        if asset_root is not None and target == "/opt/v3/assets":
            mount["Source"] = str(asset_root)
            mount["ReadOnly"] = True
            seen.add(target)
        elif target in mapping:
            source = root / mapping[target]
            if not source.exists() or source.is_symlink():
                raise ValueError("release mount input unavailable: " + str(target))
            mount["Source"] = str(source)
            mount["ReadOnly"] = True
            seen.add(target)
        mounts.append(mount)
    if asset_root is not None and "/opt/v3/assets" not in seen:
        if not asset_root.is_dir() or asset_root.is_symlink():
            raise ValueError("stable asset mount unavailable")
        mounts.append({"Type": "bind", "Source": str(asset_root),
                       "Target": "/opt/v3/assets", "ReadOnly": True})
        seen.add("/opt/v3/assets")
    required = ({"/live-v3", "/opt/v3/assets" if asset_root is not None else "/opt/v3/ui"} if api
                else {"/opt/v3/plugins/abilities"} | ({"/opt/v3/assets"} if asset_root is not None else set()))
    if not required <= seen:
        raise ValueError("required release mounts absent")
    host["Mounts"] = mounts
    return host

def promote_checkout(root, before_blob, after_blob, changed, validate, standalone=False):
    """Copy a healthy release's changed files into the shared source checkout."""
    before, after = entries(before_blob, validate), entries(after_blob, validate)
    root = Path(root).resolve()
    actions = []
    for name in changed:
        if not name.startswith("v3/") or name.startswith("v3/data/"):
            raise ValueError("checkout promotion path outside V3 source")
        # Archives keep stable v3/ names; the independent checkout starts at that root.
        path = root / (name.removeprefix("v3/") if standalone else name)
        for parent in (path, *path.parents):
            if parent == root:
                break
            if parent.is_symlink():
                raise ValueError("checkout promotion symlink forbidden")
        if path.exists():
            if not path.is_file():
                raise ValueError("checkout promotion special file forbidden")
            info = path.stat()
            current = (info.st_mode & 0o777, path.read_bytes())
        else:
            current = None
        current_body = current[1] if current is not None else None
        before_body = before[name][1] if name in before else None
        after_body = after[name][1] if name in after else None
        if current_body not in (before_body, after_body):
            raise ValueError("checkout changed during release: " + name)
        if current != after.get(name):
            actions.append((path, after.get(name)))
    for path, target in actions:
        if target is None:
            path.unlink()
            continue
        mode, body = target
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_name(path.name + ".release-" + uuid.uuid4().hex)
        try:
            with temporary.open("xb") as output:
                output.write(body)
                output.flush()
                os.fsync(output.fileno())
            temporary.chmod(mode)
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)
    return len(actions)


def checkout_matches(root, after_blob, changed, validate, standalone=False):
    """Reconcile a completed deployment without writing the shared checkout."""
    after = entries(after_blob, validate)
    root = Path(root).resolve()
    for name in changed:
        if not name.startswith("v3/") or name.startswith("v3/data/"):
            return False
        # Archives keep stable v3/ names; the independent checkout starts at that root.
        path = root / (name.removeprefix("v3/") if standalone else name)
        for parent in (path, *path.parents):
            if parent == root:
                break
            if parent.is_symlink():
                return False
        if path.exists():
            if not path.is_file():
                return False
            info = path.stat()
            current = (info.st_mode & 0o777, path.read_bytes())
        else:
            current = None
        if current != after.get(name):
            return False
    return True


def stage_assets(root, job_id, blob, validate):
    """Freeze the tested source, including served UI and workspace templates."""
    uuid.UUID(job_id)
    files = entries(blob, validate)
    if "v3/ui/index.html" not in files:
        raise ValueError("UI release has no application entrypoint")
    if root.is_symlink():
        raise ValueError("asset root symlink forbidden")
    root.mkdir(parents=True, exist_ok=True, mode=0o755)
    root.chmod(0o755)
    revisions = root / "releases"
    if revisions.is_symlink():
        raise ValueError("asset revisions symlink forbidden")
    revisions.mkdir(exist_ok=True, mode=0o755)
    revisions.chmod(0o755)
    destination = revisions / job_id
    if destination.exists():
        existing = {name:item for name,item in entries(
            snapshot(destination, ("v3", "ui"), lambda _: None), lambda _: None).items()
            if not name.startswith("v3/.code-index/")}
        for name in list(existing):
            if name=="v3/plugins/engine/catalog.json":
                if name in files:existing[name]=files[name]
                else:existing.pop(name)
            elif name.startswith("v3/plugins/engine/")and "/artifact/"in name:existing.pop(name)
            elif name.startswith("v3/plugins/engine/")and name.endswith("/component.json")and name in files:
                value=json.loads(existing[name][1]);value.pop("artifact_sha256",None)
                wanted=json.loads(files[name][1]);wanted.pop("artifact_sha256",None)
                if value==wanted:existing[name]=files[name]
        if existing != files:
            raise ValueError("frozen UI asset release changed")
    else:
        extract(destination, archive(files, lambda _: None), lambda _: None)
        destination.chmod(0o755)
    return destination



def asset_source_digest(destination):
    """Hash exact source/artifact bytes without running an index or its size-bound parser."""
    destination=Path(destination);rows=[]
    for path in sorted(destination.rglob("*")):
        relative=path.relative_to(destination).as_posix()
        if relative.split("/")[0]in {".code-index",".git"}:continue
        info=path.lstat()
        if stat.S_ISDIR(info.st_mode):continue
        if not stat.S_ISREG(info.st_mode):raise ValueError("Invalid immutable source inode")
        if len(rows)>=50000:raise ValueError("Source receipt file bound exceeded")
        content=hashlib.sha256()
        with path.open("rb")as stream:
            for block in iter(lambda:stream.read(1024*1024),b""):content.update(block)
        rows.append([relative,info.st_mode&0o100,content.hexdigest()])
    return hashlib.sha256(json.dumps(rows,separators=(",",":")).encode()).hexdigest()


def write_source_receipt(destination, job_id, candidate_digest):
    """Seal exact immutable source independently of derived post-deployment metadata."""
    destination=Path(destination);uuid.UUID(job_id)
    value={"schema_version":1,"publication_mode":"post_deployment_indexing_v1",
           "release_id":job_id,"candidate_sha256":candidate_digest,
           "source_sha256":asset_source_digest(destination)}
    metadata=destination/".code-index"
    if metadata.is_symlink():raise ValueError("Source metadata directory symlink")
    metadata.mkdir(mode=0o755,exist_ok=True)
    path=metadata/"source-release.json"
    if path.exists():
        if path.is_symlink()or json.loads(path.read_bytes())!=value:raise ValueError("Immutable source receipt differs")
    else:
        temporary=metadata/(".source-release-"+uuid.uuid4().hex)
        fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o644)
        with os.fdopen(fd,"w")as stream:
            json.dump(value,stream,sort_keys=True,separators=(",",":"));stream.flush();os.fsync(stream.fileno())
        os.rename(temporary,path)
        for parent in (metadata,destination):
            fd=os.open(parent,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
            try:os.fsync(fd)
            finally:os.close(fd)
    return value


def verify_publication(destination, job_id, candidate_digest=None):
    """Validate source activation without requiring its derived index to exist."""
    destination=Path(destination);path=destination/".code-index/source-release.json";uuid.UUID(job_id)
    if not path.exists():return verify_index(destination,job_id,candidate_digest)
    if destination.is_symlink()or (destination/".code-index").is_symlink()or path.is_symlink()or not path.is_file()or path.stat().st_size>4096:raise ValueError("Invalid source publication receipt")
    value=json.loads(path.read_bytes())
    if any(not isinstance(value.get(k),str)or len(value[k])!=64 or any(c not in "0123456789abcdef"for c in value[k])for k in ("candidate_sha256","source_sha256")):raise ValueError("Invalid source publication digest")
    if set(value)!={"schema_version","publication_mode","release_id","candidate_sha256","source_sha256"}or value["schema_version"]!=1 or value["publication_mode"]!="post_deployment_indexing_v1"or value["release_id"]!=job_id or candidate_digest is not None and value["candidate_sha256"]!=candidate_digest:raise ValueError("Source publication receipt mismatch")
    expected=write_source_receipt(destination,job_id,value["candidate_sha256"])
    if value!=expected:raise ValueError("Published source bytes changed")
    return value


def install_prepared_assets(root, job_id, prepared, candidate_digest):
    """Atomically admit one verified complete snapshot only after the host wins publication."""
    uuid.UUID(job_id);root=Path(root);prepared=Path(prepared)
    if root.is_symlink()or prepared.is_symlink()or not prepared.is_dir():raise ValueError("Prepared asset path invalid")
    manifest=verify_publication(prepared,job_id,candidate_digest)
    revisions=root/"releases"
    if revisions.is_symlink():raise ValueError("Asset revisions symlink forbidden")
    revisions.mkdir(mode=0o755,parents=True,exist_ok=True)
    destination=revisions/job_id
    if destination.exists():
        existing=verify_publication(destination,job_id,candidate_digest)
        if existing["source_sha256"]!=manifest["source_sha256"]or existing.get("index_sha256")!=manifest.get("index_sha256"):raise ValueError("Immutable prepared release differs")
        return destination
    previous_parent=prepared.parent
    os.rename(prepared,destination)
    for parent in (revisions,previous_parent):
        fd=os.open(parent,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
        try:os.fsync(fd)
        finally:os.close(fd)
    return destination

def current_asset_id(root):
    pointer = root / "current"
    if not pointer.exists() and not pointer.is_symlink():
        return None
    if not pointer.is_symlink():
        raise ValueError("asset pointer is not a symlink")
    target = os.readlink(pointer)
    if not target.startswith("releases/") or "/" in target[len("releases/"):]:
        raise ValueError("asset pointer target invalid")
    job_id = target[len("releases/"):]
    uuid.UUID(job_id)
    if not (root / target).is_dir():
        raise ValueError("asset pointer target unavailable")
    return job_id


def verify_index(destination, job_id, candidate_digest=None):
    # Installed broker loads only its protected scanner. Unit tests load the
    # reviewed checkout adapter; no module inside a candidate is executed.
    scanner = Path(__file__).with_name("index-scanner.py")
    if not scanner.exists():
        scanner = Path(__file__).resolve().parent.parent / "plugins/abilities/Administrator/code_index/adapter.py"
    spec = importlib.util.spec_from_file_location("published_index", scanner)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    _blob, manifest = module.read_published(Path(destination), job_id)
    if candidate_digest is not None and manifest["candidate_sha256"] != candidate_digest:
        raise ValueError("Published candidate/index digest mismatch")
    return manifest


def activate_assets(root, job_id):
    """Atomically select a complete immutable UI snapshot."""
    uuid.UUID(job_id)
    target = root / "releases" / job_id
    if not target.is_dir() or target.is_symlink():
        raise ValueError("asset release unavailable")
    verify_publication(target, job_id)
    previous = current_asset_id(root)
    temporary = root / ("current-" + uuid.uuid4().hex)
    try:
        os.symlink("releases/" + job_id, temporary)
        os.replace(temporary, root / "current")
        directory = os.open(root, os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)
    return previous

def checkout_drift(live_blob, checkout_blob, candidate_blob, changed, validate, offset=0):
    """Content-free report over allowlisted V3 source, never runtime data."""
    def source(blob):
        # Git source identity tracks owner executable status, not ACL masks
        # or checkout read/write permissions. ACL masks can expose group execute
        # in st_mode even when no file entry grants execute.
        return {name: (mode & 0o100, body)
                for name, (mode, body) in entries(blob, validate).items()
                if name.startswith("v3/") and not name.startswith("v3/data/")}
    live, checkout, candidate = map(source, (live_blob, checkout_blob, candidate_blob))
    def fingerprint(values):
        return [(name, mode, hashlib.sha256(body).hexdigest())
                for name, (mode, body) in sorted(values.items())]
    changed = set(changed)
    rows = []
    for name in sorted(set(live) | set(checkout)):
        previous, current, target = live.get(name), checkout.get(name), candidate.get(name)
        if current == previous:
            continue
        rows.append({"path": name.removeprefix("v3/"),
            "kind": "missing" if current is None else "checkout_only" if previous is None else "modified",
            "affected_by_build": name in changed,
            "promotion_conflict": name in changed and current not in (previous, target),
            "matches_candidate": current == target})
    digest = hashlib.sha256(json.dumps([fingerprint(live), fingerprint(checkout),
        fingerprint(candidate), sorted(changed)], separators=(",", ":")).encode()).hexdigest()
    return {"status": "drift" if rows else "synchronized", "digest": digest,
        "total": len(rows), "affected": sum(row["affected_by_build"] for row in rows),
        "promotion_conflicts": sum(row["promotion_conflict"] for row in rows),
        "files": rows[offset:offset+40], "offset": offset,
        "next_offset": offset+40 if len(rows)>offset+40 else None,
        "choices": ["synchronize", "proceed", "cancel"] if rows else [],
        "guidance": "Synchronize the listed source first, proceed using the frozen deployed release without overwriting conflicts, or cancel. Proceed preserves deployed changes but does not repair checkout drift." if rows else "Checkout source matches the deployed release."}


def proposal_patch(before_blob,after_blob,validate):
    """Bounded UTF-8 proposal for checked source application, including deletions and Git modes."""
    import difflib
    before=entries(before_blob,validate);after=entries(after_blob,validate)
    changed=[p for p in sorted(set(before)|set(after)) if before.get(p)!=after.get(p) and not inspection_path(p)]
    if len(changed)>64:raise ValueError("Successor proposal exceeds checked source path bound")
    patches=[]
    for path in changed:
        if not path.startswith("v3/"):raise ValueError("Read-only shared input changed")
        name=path[3:];old=before.get(path);new=after.get(path)
        a=old[1].decode("utf-8","strict") if old else ""
        b=new[1].decode("utf-8","strict") if new else ""
        if "\0" in a+b:raise ValueError("Binary successor proposal requires explicit recovery")
        header="diff --git a/"+name+" b/"+name+"\n"
        mode=lambda value:"100755" if value[0]&0o100 else "100644"
        if old is None:header+="new file mode "+mode(new)+"\n"
        elif new is None:header+="deleted file mode "+mode(old)+"\n"
        elif mode(old)!=mode(new):header+="old mode "+mode(old)+"\nnew mode "+mode(new)+"\n"
        diff="".join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile="a/"+name if old else "/dev/null",tofile="b/"+name if new else "/dev/null"))
        diff="".join(line if line.endswith("\n") else line+"\n\\ No newline at end of file\n" for line in diff.splitlines(True))
        patches.append(header+diff)
    patch="".join(patches)
    if len(patch.encode())>1024*1024:raise ValueError("Successor proposal exceeds checked patch byte bound")
    return patch
