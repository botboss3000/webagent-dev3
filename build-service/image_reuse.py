"""Exact, durable per-role image checkpoints for release resume."""
import datetime
import hashlib
import json
from pathlib import Path
import re

IMAGE = re.compile(r"sha256:[0-9a-f]{64}")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def source_receipt(runner, job, blob):
    """Rehash compiler evidence against the exact retained source and artifact files."""
    compiler_id=(job.get("packaging_resume") or {}).get("source_job_id",job["job_id"])
    if job.get("packaging_resume") and not re.fullmatch(r"[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}",compiler_id):
        raise ValueError("Compiler source job identity invalid")
    verified = Path(runner.source_config["state_root"]) / "execution" / compiler_id / "verified.json"
    if verified.is_symlink() or not verified.is_file():
        raise ValueError("Retained compiler receipt unavailable")
    raw = verified.read_bytes()
    row = json.loads(raw)
    compiler_blob = runner.execution_application_blob(blob)
    if row.get("inputs_sha256") != runner.context_digest(compiler_blob)[0]:
        raise ValueError("Retained compiler inputs changed")
    if not isinstance(row.get("artifacts"),dict) or not row["artifacts"]:
        raise ValueError("Retained compiler receipt incomplete")
    artifacts=verified.parent/"output/artifacts"
    if runner.executor.artifact_hashes(artifacts)!=row["artifacts"]:
        raise ValueError("Retained compiler artifact bytes changed")
    return digest(raw)


def recipe_receipt(runner):
    """Bind resumption to installed packaging implementation, not caller text."""
    names = ("executor.py", "packaging_cache.py", "image_reuse.py")
    root = Path(runner.source_config["service_dir"])
    result = {}
    for name in names:
        path = root / name
        if path.is_symlink() or not path.is_file():
            raise ValueError("Installed packaging recipe unavailable")
        result[name] = digest(path.read_bytes())
    return result


def record(runner, job, blob, role, image, context_sha256):
    if role not in ("api", "loop") or not IMAGE.fullmatch(image):
        raise ValueError("Fixed image role and immutable identity required")
    if not re.fullmatch(r"[a-f0-9]{64}", context_sha256):
        raise ValueError("Verified packaging context digest required")
    return {"version": 1, "job_id": job["job_id"], "role": role,
            "source_sha256": digest(blob), "compiler_receipt_sha256": source_receipt(runner, job, blob),
            "packaging_context_sha256": context_sha256, "recipe": recipe_receipt(runner),
            "dependency_bases": {k: {p: v[p] for p in ("key", "manifest_digest")}
                                 for k, v in job.get("dependency_bases", {}).items()},
            "image_id": image}


def reuse(runner, job, blob, role):
    """Only an exact saved checkpoint may skip packaging and its downloads."""
    row = job.get("image_receipts", {}).get(role)
    if row is None:
        return None
    if row.get("version") != 1 or row.get("job_id") != job["job_id"] or row.get("role") != role:
        raise ValueError("Image checkpoint identity changed")
    if row.get("source_sha256") != digest(blob):
        raise ValueError("Image checkpoint source changed")
    if row.get("compiler_receipt_sha256") != source_receipt(runner, job, blob):
        raise ValueError("Image checkpoint compiler evidence changed")
    if row.get("recipe") != recipe_receipt(runner):
        raise ValueError("Image checkpoint packaging recipe changed")
    if row.get("image_id") != job.get("images", {}).get(role) or not IMAGE.fullmatch(row["image_id"]):
        raise ValueError("Image checkpoint role identity changed")
    actual = runner.command(["docker", "image", "inspect", "--format", "{{.Id}}", row["image_id"]], timeout=30).strip()
    if actual != row["image_id"]:
        raise ValueError("Retained immutable image unavailable")
    return actual

# These copies are declared by the pinned API/loop Dockerfile. Unknown packaging
# layouts require a new reviewed proof rather than guessing from an image tag.
COPIES = {
    "api": {
        "webagent-v3": "/usr/local/bin/webagent-v3",
        "core-inputs.txt": "/usr/local/lib/webagent/core-inputs.txt",
        "webagent-mcp": "/usr/local/bin/webagent-mcp",
        "webagent-diagnostics": "/usr/local/bin/webagent-diagnostics",
        "webagent-optimization": "/usr/local/bin/webagent-optimization",
        "webagent-context-service": "/usr/local/lib/webagent/engine/context-assembly/webagent-context-service",
        "webagent-background-service": "/usr/local/lib/webagent/engine/background-dispatch/webagent-background-service",
    },
    "loop": {
        "webagent-v3": "/usr/local/bin/webagent-v3",
        "core-inputs.txt": "/usr/local/lib/webagent/core-inputs.txt",
        "webagent-context-service": "/usr/local/lib/webagent/engine/context-assembly/webagent-context-service",
        "webagent-background-service": "/usr/local/lib/webagent/engine/background-dispatch/webagent-background-service",
    },
}



def failed_retry_mode(prior, merged_sha256, rebuild_if_stale):
    """Choose image reuse or a new cached build from exact source bindings."""
    if type(rebuild_if_stale) is not bool or not re.fullmatch(r"[a-f0-9]{64}", merged_sha256):
        raise ValueError("Exact retry source and rebuild choice required")
    if prior.get("status") != "failed" or prior.get("operation") != "build_deploy" or prior.get("effects_started"):
        raise ValueError("Only failed pre-activation releases may retry")
    inherited = prior.get("legacy_image_proof")
    images = prior.get("images", {})
    if inherited is not None:
        if images or not isinstance(inherited, dict) or not re.fullmatch(r"[a-f0-9]{64}", inherited.get("source_sha256", "")):
            raise ValueError("Inherited image evidence inconsistent")
        bound = inherited["source_sha256"]
    elif set(images) == set(COPIES):
        bound = prior["inputs_sha256"]
    else:
        raise ValueError("Failed release has no verified image pair")
    if merged_sha256 == bound:
        return "reuse_inherited" if inherited is not None else "reuse"
    return "cached_rebuild" if rebuild_if_stale else "needs_rebuild"



def cached_rebuild_lineage(prior, jobs, lineage_job_id=None):
    """Bind a no-effect cached-build retry to a previously verified image lineage."""
    if prior.get("retry_mode") != "cached_rebuild" or prior.get("images") or prior.get("legacy_image_proof"):
        raise ValueError("Cached rebuild lineage requires a no-image cached retry")
    saved = prior.get("cached_rebuild_lineage")
    if saved is not None:
        if not isinstance(saved, dict) or saved.get("version") != 1:
            raise ValueError("Cached rebuild lineage changed")
        if lineage_job_id is not None and lineage_job_id != saved.get("lineage_job_id"):
            raise ValueError("Cached rebuild lineage request changed")
        lineage_job_id = saved.get("lineage_job_id")
    if not isinstance(lineage_job_id, str) or not re.fullmatch(r"[0-9a-f-]{36}", lineage_job_id):
        raise ValueError("Verified lineage job ID required")
    lineage = jobs.get(lineage_job_id)
    if not isinstance(lineage, dict) or lineage.get("status") != "failed" or lineage.get("effects_started"):
        raise ValueError("Verified lineage job unavailable")
    if lineage.get("task_id") != prior.get("task_id") or lineage.get("source_generation") != prior.get("source_generation"):
        raise ValueError("Cached rebuild lineage ownership changed")
    if lineage.get("changed_paths") != prior.get("changed_paths"):
        raise ValueError("Cached rebuild lineage scope changed")
    proof = lineage.get("legacy_image_proof")
    if not isinstance(proof, dict) or set(proof) != {"source_job_id", "source_sha256", "compiler_receipt_sha256", "images"}:
        raise ValueError("Verified lineage image proof unavailable")
    if not isinstance(proof["source_job_id"], str) or not re.fullmatch(r"[0-9a-f-]{36}", proof["source_job_id"]):
        raise ValueError("Verified lineage source identity invalid")
    if not re.fullmatch(r"[a-f0-9]{64}", proof["source_sha256"]) or not re.fullmatch(r"[a-f0-9]{64}", proof["compiler_receipt_sha256"]):
        raise ValueError("Verified lineage evidence invalid")
    if set(proof["images"]) != set(COPIES) or any(not IMAGE.fullmatch(value) for value in proof["images"].values()):
        raise ValueError("Verified lineage images invalid")
    return {"version": 1, "lineage_job_id": lineage_job_id, "legacy_image_proof": proof}


def legacy_pair(runner, prior, blob):
    """Prove a failed pre-activation job's retained image pair before a new attempt uses it."""
    if (prior.get("status") != "failed" or prior.get("operation") != "build_deploy"
            or prior.get("effects_started") or set(prior.get("images", {})) != set(COPIES)):
        raise ValueError("Legacy image attempt is not a failed pre-activation pair")
    if prior.get("inputs_sha256") != runner.context_digest(blob)[0]:
        raise ValueError("Legacy image source binding changed")
    original = Path(runner.source_config["state_root"]) / prior["job_id"] / "candidate.tar"
    if original.is_symlink() or runner.context_digest(original.read_bytes())[0] != prior["inputs_sha256"]:
        raise ValueError("Original sealed candidate changed")
    receipt = source_receipt(runner, prior, blob)
    row_path = Path(runner.source_config["state_root"]) / "execution" / prior["job_id"] / "verified.json"
    compiler = json.loads(row_path.read_bytes())
    phases = prior.get("phase_history", [])
    names = [phase.get("phase") for phase in phases]
    if names[:4] != ["building_api", "building_loop", "preparing_plugins", "preparing_ui"] or names[-1] != "building_api":
        raise ValueError("Legacy image phase history incomplete")
    windows = {"api": (phases[0]["at"], phases[1]["at"]),
               "loop": (phases[1]["at"], phases[2]["at"])}
    result = {}
    for role, copies in COPIES.items():
        image = prior["images"][role]
        if not isinstance(image, str) or not IMAGE.fullmatch(image):
            raise ValueError("Legacy image identity invalid")
        tag = "webagent-v3-" + role + "-" + prior["job_id"] + ":latest"
        tagged = runner.command(["docker", "image", "inspect", "--format", "{{.Id}}", tag], timeout=30).strip()
        immutable = runner.command(["docker", "image", "inspect", "--format", "{{.Id}}", image], timeout=30).strip()
        if image != tagged or image != immutable:
            raise ValueError("Legacy tagged image changed")
        created = runner.command(["docker", "image", "inspect", "--format", "{{.Created}}", image], timeout=30).strip()
        when = datetime.datetime.fromisoformat(created.replace("Z", "+00:00")).timestamp()
        if not windows[role][0] <= when <= windows[role][1]:
            raise ValueError("Legacy image creation outside verified phase")
        argv = ["docker", "run", "--rm", "--network", "none", "--read-only",
                "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                "--pids-limit", "32", "--memory", "256m", "--user", "1000:1000",
                "--entrypoint", "/usr/bin/sha256sum", image, *copies.values()]
        observed = runner.command(argv, timeout=90).splitlines()
        if len(observed) != len(copies):
            raise ValueError("Legacy image artifact inventory incomplete")
        for (name, path), line in zip(copies.items(), observed):
            if line != compiler["artifacts"].get(name, "") + "  " + path:
                raise ValueError("Legacy image artifact checksum changed")
        result[role] = image
    return {"source_job_id": prior["job_id"], "source_sha256": prior["inputs_sha256"],
            "compiler_receipt_sha256": receipt, "images": result}

def packaging_checkpoint(runner,prior,blob):
    """Accept only a completed compiler certificate from an exact failed pre-activation attempt."""
    if prior.get("status")!="failed" or prior.get("effects_started") or prior.get("operation")!="build_deploy":
        raise ValueError("Failed pre-activation packaging attempt required")
    if prior.get("inputs_sha256")!=runner.context_digest(blob)[0]:raise ValueError("Packaging retry source changed")
    preflight=prior.get("packaging_preflight",{})
    if preflight.get("source_sha256")!=digest(blob) or preflight.get("recipe")!=recipe_receipt(runner):
        raise ValueError("Exact packaging preflight recipe unavailable or changed")
    compiler_hash=source_receipt(runner,prior,blob)
    checkpoint=prior.get("compiler_checkpoint")
    if checkpoint!={"source_sha256":digest(blob),"compiler_receipt_sha256":compiler_hash,"recipe":recipe_receipt(runner)}:
        raise ValueError("Completed compiler checkpoint unavailable or changed")
    source_id=(prior.get("packaging_resume") or {}).get("source_job_id",prior["job_id"])
    if not re.fullmatch(r"[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}",source_id):raise ValueError("Compiler source job identity invalid")
    return {"version":1,"source_job_id":source_id,"source_sha256":digest(blob),"compiler_receipt_sha256":compiler_hash,"recipe":recipe_receipt(runner)}


def packaging_artifacts(runner,job,blob):
    """Read retained artifacts without copying or invoking the compiler/test executor."""
    proof=job["packaging_resume"]
    if proof.get("version")!=1 or proof.get("source_sha256")!=digest(blob) or proof.get("recipe")!=recipe_receipt(runner):
        raise ValueError("Packaging resume binding changed")
    if proof.get("compiler_receipt_sha256")!=source_receipt(runner,job,blob):
        raise ValueError("Packaging resume compiler certificate changed")
    return Path(runner.source_config["state_root"])/"execution"/proof["source_job_id"]/"output/artifacts"


def prepare_retry_archives(state,coordination,identity,source,body_digest,write_record,*,recover_legacy=False):
    """Durably prepare only exact retry source archives; interrupted copies never become final files."""
    import os,stat,shutil,uuid
    names=("base.tar","task.tar","candidate.tar","admitted_base.tar")
    def file_binding(path):
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_CLOEXEC)
        try:
            meta=os.fstat(fd)
            if not stat.S_ISREG(meta.st_mode)or meta.st_nlink!=1:raise ValueError("Retry archive must be a regular single-link file")
            sha=hashlib.sha256();size=0
            with os.fdopen(fd,"rb",closefd=False)as stream:
                while chunk:=stream.read(1024*1024):sha.update(chunk);size+=len(chunk)
            if size!=meta.st_size:raise ValueError("Retry archive changed while reading")
            return {"sha256":sha.hexdigest(),"size":size}
        finally:os.close(fd)
    if source.is_symlink()or not source.is_dir():raise ValueError("Original retry archive directory unavailable")
    expected={name:file_binding(source/name)for name in names if (source/name).exists()or(source/name).is_symlink()}
    if not {"base.tar","task.tar","candidate.tar"}<=set(expected):raise ValueError("Original retry source archives incomplete")
    root=coordination/"retry-preparations"
    if root.is_symlink():raise ValueError("Retry preparation journal symlink")
    root.mkdir(mode=0o700,exist_ok=True)
    journal=root/(identity+".json");folder=state/identity
    binding={"body_digest":body_digest,"source_job_id":source.name,"archives":expected}
    if journal.is_symlink()or folder.is_symlink():raise ValueError("Retry preparation symlink")
    managed=journal.exists()
    if managed:
        file_binding(journal);record=json.loads(journal.read_bytes())
        if record["binding"]!=binding:raise ValueError("Retry preparation binding changed")
    else:
        if folder.exists()and not recover_legacy:raise ValueError("Legacy retry preparation requires recover_preparation=true")
        record={"state":"preparing","binding":binding,"legacy_recovery":folder.exists()}
    if folder.exists()and not folder.is_dir():raise ValueError("Retry preparation directory unavailable")
    allowed=set(expected)|{name+".preparing"for name in expected}
    if folder.exists()and any(p.name not in allowed for p in folder.iterdir()):raise ValueError("Unknown retry preparation contents; preserve for separate reconciliation")
    # Validate everything before recording adoption or moving a partial file.
    for path in folder.iterdir()if folder.exists()else():
        actual=file_binding(path)
        if path.name in expected and actual!=expected[path.name]and not(record["legacy_recovery"]and record["state"]=="preparing"):
            raise ValueError("Completed retry archive changed")
    write_record(journal,record)
    folder.mkdir(mode=0o700,exist_ok=True)
    def sync(directory):
        fd=os.open(directory,os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)
    def preserve(path):
        archive=root/(identity+"-preserved")
        if archive.is_symlink():raise ValueError("Retry preservation symlink")
        archive.mkdir(mode=0o700,exist_ok=True)
        target=archive/(path.name+"-"+uuid.uuid4().hex)
        os.rename(path,target);sync(folder);sync(archive)
    for name,wanted in expected.items():
        destination=folder/name;temporary=folder/(name+".preparing")
        if temporary.exists()or temporary.is_symlink():preserve(temporary)
        if destination.exists():
            if file_binding(destination)==wanted:continue
            preserve(destination)
        with (source/name).open("rb")as incoming,temporary.open("xb")as outgoing:
            shutil.copyfileobj(incoming,outgoing,1024*1024);outgoing.flush();os.fsync(outgoing.fileno())
        if file_binding(temporary)!=wanted:raise ValueError("Retry source changed during copy")
        os.replace(temporary,destination);sync(folder)
    if {name:file_binding(source/name)for name in expected}!=expected or {name:file_binding(folder/name)for name in expected}!=expected:
        raise ValueError("Retry archives changed during preparation")
    write_record(journal,{**record,"state":"ready"});sync(state)
    return {"identity":identity,"binding":binding,"state":"ready"}
