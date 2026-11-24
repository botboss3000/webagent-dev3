#!/usr/bin/env python3
"""Report known vulnerabilities and package lists (SBOM) for the deployed v3 release.

Scans the running v3 images, the release's Cargo.lock and its copied UI libraries
(ui/vendor/package-lock.json, checked against each file's own version header) with a digest-pinned Trivy
whenever the deployed images change, and at least weekly against a fresh advisory
database. Report-only: it never blocks, rebuilds or restarts anything.
Reports: /home/ar/webagent-dev3/supply-chain/reports/<time>/, newest summary at latest.json.

Source of truth: build-service/supply_chain_scan.py. A broker install copies it to
/usr/local/sbin/v3-supply-chain-scan (the install loop in lifecycle.py), which the host timer
webagent-dev-supply-chain-scan.timer runs as ar every 15 minutes. The broker imports
summary_view() to give administrators a bounded projection of latest.json (v3_supply_chain).
"""
import fcntl, hashlib, json, os, re, shutil, stat, subprocess, sys, time
from collections import Counter
from pathlib import Path

TRIVY = "aquasec/trivy@sha256:790fd66d1697395d8693d1acaa1ea702406a20f109b6fc7bac8db623eca6c06c"  # v0.76.1
ROOT = Path("/home/ar/webagent-dev3/supply-chain")
CONTAINERS = {"api": "webagent-dev-v3", "loop": "webagent-dev-v3-loop", "gate": "webagent-dev-v3-gate"}
# Stream D6: the worker and browser containers have their own images (or, before that, run the API
# image); each is scanned when the container exists. When one of them runs an image another role
# already has (the worker on the API image), that role's reports are copied instead of a rescan.
OPTIONAL_CONTAINERS = {"worker": "webagent-dev-v3-worker", "browser": "webagent-dev-v3-browser"}
WEEK = 7 * 86400
KEEP = 20
# Crate sources copied into the api image for optimization work carry their own
# upstream Cargo.lock files; those are not what the app ships (the release lock is).
SKIP_DIRS = "opt/optimization-cargo"
# UI libraries copied into the repository (no package manager): their versions are recorded in
# an npm-format lockfile so Trivy matches npm advisories; each file's licence header must agree.
UI_LOCK = "ui/vendor/package-lock.json"
UI_FILES = {"marked": "ui/vendor/marked.min.js", "dompurify": "ui/vendor/purify.min.js",
            "lucide": "ui/vendor/lucide/lucide.min.js"}


def ui_versions(release):
    """Recorded versions against the versions in the files' headers; mismatches are reported."""
    lock = Path(release) / UI_LOCK
    if not lock.is_file():
        return None, {"missing": UI_LOCK}
    recorded = {name.removeprefix("node_modules/"): entry.get("version")
                for name, entry in json.loads(lock.read_text()).get("packages", {}).items() if name}
    check = {"recorded": recorded, "mismatch": []}
    for name, path in UI_FILES.items():
        file = Path(release) / path
        head = file.read_bytes()[:400].decode("utf-8", "replace") if file.is_file() else ""
        found = re.search(rf"(?i)\b{name}\s+v?(\d+\.\d+\.\d+)", head)
        actual = found.group(1) if found else None
        if actual != recorded.get(name):
            check["mismatch"].append({"library": name, "file": path, "header": actual, "recorded": recorded.get(name)})
    return lock, check


def run(argv, **kw):
    return subprocess.run(argv, check=True, text=True, capture_output=True, **kw).stdout


def trivy(args, out, mounts):
    volumes = [a for src, dst in mounts for a in ("-v", f"{src}:{dst}:ro")]
    with open(out, "w") as f:
        subprocess.run(["docker", "run", "--rm", "--network", "bridge", "--user", f"{os.getuid()}:{os.getgid()}",
                        "-v", f"{ROOT}/cache:/cache", *volumes, TRIVY, *args, "--cache-dir", "/cache", "-q"],
                       check=True, stdout=f, timeout=1800)


def findings(report):
    return [v for r in report.get("Results") or [] for v in r.get("Vulnerabilities") or []]


def record_failure(error):
    '''Keep the failure in latest.json so a broken scanner is visible, not only a failed unit.'''
    latest = ROOT / "latest.json"
    try:
        last = json.loads(latest.read_text()) if latest.is_file() else {}
    except ValueError:
        last = {}
    last.update(status="failed", failed_at=time.time(), error=str(error)[:500])
    latest.write_text(json.dumps(last, indent=1))


def optional_images():
    """The image of each `OPTIONAL_CONTAINERS` container that exists; none when Docker cannot say."""
    images = {}
    for role, name in OPTIONAL_CONTAINERS.items():
        try:
            found = subprocess.run(["docker", "inspect", "-f", "{{.Image}}", name], text=True,
                                   capture_output=True, timeout=30)
        except (OSError, subprocess.SubprocessError):
            continue
        if found.returncode == 0 and re.fullmatch(r"sha256:[0-9a-f]{64}", found.stdout.strip()):
            images[role] = found.stdout.strip()
    return images


def main():
    force = "--force" in sys.argv
    (ROOT / "cache").mkdir(parents=True, exist_ok=True)
    lock = open(ROOT / ".lock", "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        return 0
    images = {role: run(["docker", "inspect", "-f", "{{.Image}}", name]).strip() for role, name in CONTAINERS.items()}
    images.update(optional_images())
    # The release tree is the parent of the frozen abilities mount (.../releases/<job>/v3).
    abilities = run(["docker", "inspect", "-f",
                     '{{range .Mounts}}{{if eq .Destination "/opt/v3/abilities"}}{{.Source}}{{end}}{{end}}', CONTAINERS["api"]]).strip()
    if not abilities:
        raise SystemExit("v3 release mount /opt/v3/abilities not found on the API container")
    release = str(Path(abilities).parent)
    cargo_lock = Path(release) / "Cargo.lock"
    lock_sha = hashlib.sha256(cargo_lock.read_bytes()).hexdigest()
    ui_lock, ui_check = ui_versions(release)
    ui_sha = hashlib.sha256(ui_lock.read_bytes()).hexdigest() if ui_lock else None
    key = hashlib.sha256(json.dumps([images, lock_sha] + ([ui_sha] if ui_sha else []), sort_keys=True).encode()).hexdigest()[:16]
    latest = ROOT / "latest.json"
    if latest.is_file() and not force:
        last = json.loads(latest.read_text())
        # A recorded failure is retried on the next run even when nothing changed.
        if last.get("key") == key and last.get("status", "ok") != "failed" and time.time() - last.get("scanned_at", 0) < WEEK:
            # Heartbeat: the dashboard tells a stopped scanner from an unchanged release.
            last["checked_at"] = time.time()
            temporary = ROOT / "latest.json.new"
            temporary.write_text(json.dumps(last, indent=1))
            os.replace(temporary, latest)
            return 0

    folder = ROOT / "reports" / time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    work = ROOT / "tmp"
    shutil.rmtree(work, ignore_errors=True)
    folder.mkdir(parents=True)
    work.mkdir()
    started = time.time()
    try:
        # The first call refreshes the advisory database; later calls reuse it.
        trivy(["fs", "--scanners", "vuln", "--format", "json", "/src"], folder / "cargo.json",
              [(cargo_lock, "/src/Cargo.lock")])
        if ui_lock:
            trivy(["fs", "--skip-db-update", "--scanners", "vuln", "--format", "json", "/src"], folder / "ui.json",
                  [(ui_lock, f"/src/{UI_LOCK}")])
        scanned = {}
        for role, image in images.items():
            if role in OPTIONAL_CONTAINERS and image in scanned:
                for suffix in (".json", ".sbom.cdx.json"):
                    shutil.copyfile(folder / f"{scanned[image]}{suffix}", folder / f"{role}{suffix}")
                continue
            scanned[image] = role
            run(["docker", "save", image, "-o", str(work / f"{role}.tar")], timeout=1800)
            common = ["image", "--skip-db-update", "--skip-dirs", SKIP_DIRS, "--input", f"/in/{role}.tar"]
            trivy([*common, "--scanners", "vuln", "--format", "json"], folder / f"{role}.json", [(work, "/in")])
            trivy([*common, "--format", "cyclonedx"], folder / f"{role}.sbom.cdx.json", [(work, "/in")])
            (work / f"{role}.tar").unlink()
    finally:
        shutil.rmtree(work, ignore_errors=True)

    summary = {"status": "ok", "key": key, "scanned_at": time.time(), "checked_at": time.time(), "seconds": round(time.time() - started),
               "trivy": TRIVY, "release": release, "cargo_lock_sha256": lock_sha, "images": images,
               "report": str(folder), "ui_libraries": ui_check, "targets": {}}
    for target in ["cargo", *(["ui"] if ui_lock else []), *images]:
        found = findings(json.loads((folder / f"{target}.json").read_text()))
        fixable = [v for v in found if v.get("FixedVersion")]
        summary["targets"][target] = {
            "by_severity": dict(Counter(v["Severity"] for v in found)),
            "fixable_by_severity": dict(Counter(v["Severity"] for v in fixable)),
            # Critical first, then high; each row ends with its severity.
            "fixable_high_critical": sorted({(v["PkgName"], v["InstalledVersion"], v["VulnerabilityID"], v["FixedVersion"],
                                              v["Severity"]) for v in fixable if v["Severity"] in ("HIGH", "CRITICAL")},
                                            key=lambda row: (SEVERITIES.index(row[4]), row)),
        }
    # The report folder keeps every row; latest.json (read by the broker, at most SUMMARY_LIMIT)
    # lists the first MAX_FINDINGS rows per target and counts the rest, so a large report is
    # never refused for its size.
    (folder / "summary.json").write_text(json.dumps(summary, indent=1))
    for target in summary["targets"].values():
        rows = target["fixable_high_critical"]
        target["fixable_high_critical"], target["fixable_not_listed"] = rows[:MAX_FINDINGS], len(rows[MAX_FINDINGS:])
    temporary = ROOT / "latest.json.new"
    temporary.write_text(json.dumps(summary, indent=1))
    os.replace(temporary, latest)
    for old in sorted((ROOT / "reports").iterdir())[:-KEEP]:
        shutil.rmtree(old)
    return 0


# ---- Administrator projection (imported by the broker, which runs as root) ----
# latest.json is written by the unprivileged scanner, so it is untrusted input: one bounded
# regular file owned by the expected account, strict types, and anything unexpected refuses
# the whole report rather than passing part of it on.
SUMMARY_LIMIT = 256 * 1024
MAX_TARGETS = 16
MAX_FINDINGS = 200
# Rows one target may list in latest.json (the scanner writes at most MAX_FINDINGS).
MAX_ROWS = 2000
SEVERITIES = ("CRITICAL", "HIGH", "MEDIUM", "LOW", "UNKNOWN")
_TARGET = re.compile(r"[a-z][a-z0-9_-]{0,31}")
_TEXT = re.compile(r"[A-Za-z0-9][A-Za-z0-9._:+~@/, -]{0,127}")
_RELEASE = re.compile(r".*/releases/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/v3")


def _text(value):
    if not isinstance(value, str) or not _TEXT.fullmatch(value):
        raise ValueError("Invalid text field")
    return value


def _time(value, optional=False):
    if value is None and optional:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not 0 <= value < 1e11:
        raise ValueError("Invalid time field")
    return float(value)


def _counts(value):
    if not isinstance(value, dict) or any(key not in SEVERITIES for key in value):
        raise ValueError("Invalid severity counts")
    if any(isinstance(n, bool) or not isinstance(n, int) or not 0 <= n <= 10_000_000 for n in value.values()):
        raise ValueError("Invalid severity count")
    return {key: value[key] for key in SEVERITIES if key in value}


def summary_view(raw):
    """Projection of latest.json bytes for administrators; ValueError when anything is malformed."""
    if not isinstance(raw, bytes) or len(raw) > SUMMARY_LIMIT:
        raise ValueError("Summary too large")
    value = json.loads(raw.decode("utf-8", "strict"))
    if not isinstance(value, dict):
        raise ValueError("Summary must be an object")
    status = value.get("status", "ok")
    if status not in ("ok", "failed"):
        raise ValueError("Invalid status")
    view = {"status": status, "scanned_at": _time(value.get("scanned_at"), optional=True),
            "checked_at": _time(value.get("checked_at"), optional=True),
            "failed_at": _time(value.get("failed_at"), optional=True), "release": None, "targets": {}}
    if status == "failed":
        error = value.get("error")
        if not isinstance(error, str):
            raise ValueError("Invalid error")
        # Only a short printable line: the full text stays in the host journal.
        view["error"] = "".join(c if c.isprintable() else " " for c in error[:200])
    release = value.get("release")
    if release is not None:
        if not isinstance(release, str) or len(release) > 512:
            raise ValueError("Invalid release")
        found = _RELEASE.fullmatch(release)
        view["release"] = found.group(1) if found else None
    targets = value.get("targets", {})
    if not isinstance(targets, dict) or len(targets) > MAX_TARGETS:
        raise ValueError("Invalid targets")
    # Every row is checked; at most MAX_FINDINGS are passed on, critical before high across all
    # targets, and each target counts the rest as fixable_not_shown (counts stay exact).
    ranked = []
    for order, (name, target) in enumerate(targets.items()):
        if not _TARGET.fullmatch(name) or not isinstance(target, dict):
            raise ValueError("Invalid target")
        rows, not_listed = target.get("fixable_high_critical", []), target.get("fixable_not_listed", 0)
        if not isinstance(rows, list) or len(rows) > MAX_ROWS:
            raise ValueError("Invalid findings")
        if isinstance(not_listed, bool) or not isinstance(not_listed, int) or not 0 <= not_listed <= 10_000_000:
            raise ValueError("Invalid findings count")
        for index, row in enumerate(rows):
            # (package, installed version, identifier, fixed version[, severity]); rows written
            # before the severity was added rank as high.
            if not isinstance(row, list) or len(row) not in (4, 5) or (len(row) == 5 and row[4] not in ("CRITICAL", "HIGH")):
                raise ValueError("Invalid finding")
            package, installed, identifier, fixed = (_text(item) for item in row[:4])
            rank = SEVERITIES.index(row[4]) if len(row) == 5 else 1
            ranked.append((rank, order, index, name,
                           {"package": package, "installed": installed, "id": identifier, "fixed": fixed}))
        view["targets"][name] = {"by_severity": _counts(target.get("by_severity", {})),
                                 "fixable_by_severity": _counts(target.get("fixable_by_severity", {})),
                                 "fixable_high_critical": [], "fixable_not_shown": len(rows) + not_listed}
    for _rank, _order, _index, name, finding in sorted(ranked, key=lambda item: item[:3])[:MAX_FINDINGS]:
        view["targets"][name]["fixable_high_critical"].append(finding)
        view["targets"][name]["fixable_not_shown"] -= 1
    libraries = value.get("ui_libraries")
    if libraries is None:
        view["ui_libraries"] = None
    elif not isinstance(libraries, dict):
        raise ValueError("Invalid UI library check")
    elif "missing" in libraries:
        view["ui_libraries"] = {"missing": True, "recorded": {}, "mismatch": []}
    else:
        recorded, mismatch = libraries.get("recorded", {}), libraries.get("mismatch", [])
        if not isinstance(recorded, dict) or len(recorded) > 32 or not isinstance(mismatch, list) or len(mismatch) > 32:
            raise ValueError("Invalid UI library check")
        rows = []
        for row in mismatch:
            if not isinstance(row, dict):
                raise ValueError("Invalid UI library mismatch")
            header, wanted = row.get("header"), row.get("recorded")
            rows.append({"library": _text(row.get("library")),
                         "header": None if header is None else _text(header),
                         "recorded": None if wanted is None else _text(wanted)})
        view["ui_libraries"] = {"missing": False, "mismatch": rows,
                                "recorded": {_text(k): _text(v) for k, v in recorded.items()}}
    return view


def read_summary(root, owner_uid):
    """Read root/latest.json without following links: a regular file of owner_uid within the limit.

    Returns {"available": False, "reason": ...} instead of raising, so a missing or refused
    report shows as a state on the dashboard."""
    directory = None
    fd = None
    try:
        directory = os.open(str(root), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        fd = os.open("latest.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=directory)
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != owner_uid or info.st_nlink != 1:
            return {"available": False, "reason": "refused"}
        if info.st_size > SUMMARY_LIMIT:
            return {"available": False, "reason": "too_large"}
        raw = os.read(fd, SUMMARY_LIMIT + 1)
    except FileNotFoundError:
        return {"available": False, "reason": "missing"}
    except OSError:
        return {"available": False, "reason": "refused"}
    finally:
        if fd is not None:
            os.close(fd)
        if directory is not None:
            os.close(directory)
    if len(raw) > SUMMARY_LIMIT:
        return {"available": False, "reason": "too_large"}
    try:
        return {"available": True, **summary_view(raw)}
    except (ValueError, UnicodeDecodeError):
        return {"available": False, "reason": "malformed"}


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        record_failure(error)
        print(f"supply-chain scan failed: {error}", file=sys.stderr)
        sys.exit(1)
