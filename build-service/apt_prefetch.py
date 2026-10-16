"""Download only signed APT selections, retaining verified archives for later builds."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import time
from urllib.parse import unquote, urlsplit, urlunsplit

HOSTS = {"deb.debian.org", "security.debian.org"}
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9+.:~_-]*\.deb")
HASH = re.compile(r"[a-f0-9]{64}")
MAX_PACKAGE = 512 * 1024**2
MAX_TOTAL = 2 * 1024**3


# @code-index {"version":1,"feature":"deployment-dependency-cache","aliases":["APT prefetch","verified package archives"],"purpose":"Keep official Debian archives bound to signed APT selections before installation. Config: build-service/scheduler-defaults.json -> package_cache_download_workers; host supplies the bounded worker count.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/package_cache_download_workers"]}]}}
def selections(output):
    """Require a strong digest for every APT URI and upgrade official transport to HTTPS."""
    result = {}
    total = 0
    for line in output.splitlines():
        if not line.startswith("'"):
            continue
        values = shlex.split(line)
        if len(values) != 4:
            raise ValueError("Unexpected APT URI record")
        uri, filename, size, digest = values
        parts = urlsplit(uri)
        decoded = unquote(parts.path)
        if (parts.scheme not in {"http", "https"} or parts.hostname not in HOSTS
                or parts.username or parts.password or parts.port not in {None, 80, 443}
                or parts.query or parts.fragment or ".." in decoded.split("/")
                or not decoded.startswith(("/debian/pool/", "/debian-security/pool/"))
                or any(ord(c) < 32 for c in decoded)):
            raise ValueError("Only official Debian pool sources are permitted")
        # APT's local archive name includes a URL-escaped version epoch even
        # when Debian's pool filename omits that epoch. Preserve APT's exact
        # local name, but bind it to the epoch-free official pool basename.
        local_name = re.sub(r"%3[aA]", ":", filename)
        if not NAME.fullmatch(local_name) or "%" in local_name:
            raise ValueError("APT archive name is not confined")
        expected_pool = re.sub(r"^([a-z0-9+.-]+)_[0-9]+:", r"\1_", local_name, count=1)
        if unquote(parts.path.rsplit("/", 1)[-1]) != expected_pool:
            raise ValueError("APT archive name is not confined")
        if not size.isdecimal() or not 0 < int(size) <= MAX_PACKAGE:
            raise ValueError("Package size outside bounded policy")
        if not digest.startswith("SHA256:") or not HASH.fullmatch(digest[7:]):
            raise ValueError("Signed SHA256 package selection required")
        entry = {"filename": filename, "bytes": int(size), "sha256": digest[7:],
                 "url": urlunsplit(("https", parts.hostname, parts.path, "", ""))}
        if filename in result:
            if result[filename] != entry:
                raise ValueError("Conflicting package selections")
            continue
        result[filename] = entry
        total += entry["bytes"]
        if len(result) > 1024 or total > MAX_TOTAL:
            raise ValueError("Package selection exceeds bounded cache phase")
    return list(result.values())


def verified(path, entry):
    """Read exact bytes; a filename, timestamp or size alone never establishes a hit."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return False
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size != entry["bytes"]:
            return False
        digest = hashlib.sha256()
        for data in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(data)
    return digest.hexdigest() == entry["sha256"]


def fetch(entry, archives, seed=None, command=subprocess.run):
    """Publish an archive only after verification; interrupted downloads remain private partials."""
    archives = Path(archives)
    if archives.is_symlink():
        raise ValueError("Cache directory symlink")
    archives.mkdir(parents=True, exist_ok=True)
    partial = archives / "partial"
    if partial.is_symlink():
        raise ValueError("Partial directory symlink")
    partial.mkdir(exist_ok=True)
    target = archives / entry["filename"]
    if target.is_symlink():
        raise ValueError("Archive symlink")
    if verified(target, entry):
        return {**entry, "outcome": "cached"}
    stage = partial / (entry["filename"] + ".webagent-partial")
    if stage.is_symlink():
        raise ValueError("Partial archive symlink")
    outcome = "downloaded"
    source = Path(seed) / entry["filename"] if seed else None
    if source is not None and verified(source, entry):
        with source.open("rb") as incoming, stage.open("wb") as outgoing:
            shutil.copyfileobj(incoming, outgoing, 1024 * 1024)
            outgoing.flush()
            os.fsync(outgoing.fileno())
        outcome = "seeded"
    else:
        argv = ["curl", "--fail", "--silent", "--show-error",
                "--proto", "=https", "--proto-redir", "=https",
                "--location", "--max-redirs", "3",
                "--connect-timeout", "8", "--max-time", "90",
                "--speed-limit", "65536", "--speed-time", "15",
                "--retry", "2", "--retry-all-errors", "--retry-delay", "1",
                "--continue-at", "-", "--output", str(stage), entry["url"]]
        result = command(argv, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=285)
        if result.returncode:
            raise RuntimeError("Official Debian download failed; verified archives retained")
    if not verified(stage, entry):
        stage.unlink(missing_ok=True)
        raise ValueError("Downloaded archive differs from signed APT selection")
    with stage.open("rb") as stream:
        os.fsync(stream.fileno())
    os.replace(stage, target)
    fd = os.open(archives, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    return {**entry, "outcome": outcome}


def plan(packages, command=subprocess.run):
    """Ask APT's signed resolver for the exact missing archives without installing anything."""
    if (not packages or len(packages) > 128
            or any(not re.fullmatch(r"[a-z0-9][a-z0-9+.-]*(?::[a-z0-9]+)?(?:=[A-Za-z0-9.+:~_-]+)?", p)
                   for p in packages)):
        raise ValueError("Bounded Debian package names required")
    argv = ["apt-get", "-o", "Acquire::ForceHash=SHA256",
            "-o", "APT::Get::AllowUnauthenticated=false",
            "-o", "Acquire::AllowInsecureRepositories=false",
            "-o", "Acquire::AllowDowngradeToInsecureRepositories=false",
            "--print-uris", "--download-only", "--assume-yes", "--no-install-recommends",
            "install", *packages]
    result = command(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=60)
    if result.returncode:
        raise RuntimeError("Signed APT dependency resolution failed")
    return selections(result.stdout)


def prefetch(entries, archives, seed=None, workers=4):
    """Bound network concurrency; failures never erase completed, verified downloads."""
    if type(workers) is not int or not 1 <= workers <= 4:
        raise ValueError("One through four package workers required")
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(fetch, entry, archives, seed) for entry in entries]
        return [future.result() for future in futures]


def main():
    """Prepare the archive cache; the subsequent apt-get --no-download remains authoritative."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--archives", default="/var/cache/apt/archives")
    parser.add_argument("--seed", default="/scheduler-seed")
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--manifest", default="/usr/local/share/webagent/runtime-base-downloads.json")
    parser.add_argument("packages", nargs="+")
    args = parser.parse_args()
    started = time.monotonic()
    entries = plan(args.packages)
    results = prefetch(entries, args.archives, args.seed, args.workers)
    metadata = {}
    for path in sorted(Path("/var/lib/apt/lists").glob("*InRelease")):
        if path.is_symlink():
            raise ValueError("Signed metadata symlink")
        metadata[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
    manifest = {"version": 1, "signed_metadata": metadata, "archives": results}
    destination = Path(args.manifest)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(manifest, sort_keys=True))
    print(json.dumps({"packages": len(results), "bytes": sum(r["bytes"] for r in results),
                      "cached": sum(r["outcome"] == "cached" for r in results),
                      "seeded": sum(r["outcome"] == "seeded" for r in results),
                      "elapsed_seconds": time.monotonic() - started}), flush=True)


if __name__ == "__main__":
    main()
