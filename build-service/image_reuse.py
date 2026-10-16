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
    verified = Path(runner.source_config["state_root"]) / "execution" / job["job_id"] / "verified.json"
    if verified.is_symlink() or not verified.is_file():
        raise ValueError("Retained compiler receipt unavailable")
    raw = verified.read_bytes()
    row = json.loads(raw)
    compiler_blob = runner.execution_application_blob(blob)
    if row.get("inputs_sha256") != runner.context_digest(compiler_blob)[0]:
        raise ValueError("Retained compiler inputs changed")
    if not isinstance(row.get("artifacts"),dict) or not row["artifacts"]:
        raise ValueError("Retained compiler receipt incomplete")
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
