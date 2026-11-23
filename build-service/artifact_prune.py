#!/usr/bin/env python3
"""V3-only development artifact retention. Never operates Docker resources."""
import argparse, fcntl, hashlib, json, os, pathlib, re, shutil, subprocess, time
ROOT = pathlib.Path("/home/ar/webagent-dev3/build-jobs")
PIN = "4f605f3b-4444-4447-a511-3a9d4b9677df"
AGE = 7200
DONE = {"completed", "cancelled"}
# Running-job phases that only compile or test; pruning old terminal folders may overlap them.
# Any other phase (rollback capture, container swap, promotion, publication) holds pruning back.
SAFE_PHASES = {"checking_source","checking_runtime_mounts","verifying_dependency_cache","preparing_ui","checking_ui",
    "preparing_plugins","compiling_plugins","building_api","building_loop","preparing_runtime_base_api",
    "preparing_runtime_base_loop","preparing_runtime_base_voice","checking_candidate_images","frontend_tests"}
STALL_WARN = 6*3600
def load(path):
    return json.loads(path.read_text())
def fingerprint(root, jobs):
    # Only terminal job records: running jobs rewrite theirs on every phase change.
    paths = [root/(k+".json") for k,j in sorted(jobs.items()) if j.get("status") in DONE]
    return [(str(p), hashlib.sha256(p.read_bytes()).hexdigest()) for p in paths]
def build_busy(root):
    """Reason to hold back, or None: a running job outside the compile/test phases, or awaiting a decision."""
    for p in root.glob("*.json"):
        try:j=load(p)
        except (OSError,ValueError):return "unreadable job record "+p.name
        if j.get("status")=="needs_decision":return "decision-waiting build"
        if j.get("status")=="running" and (j.get("phase") not in SAFE_PHASES or set(j.get("active_phases") or [])-SAFE_PHASES):
            return "build in a release or rollback phase"
    return None
def status(root, reason):
    """Record the last run in operator-pruning/status.json; warn once deferring passes STALL_WARN."""
    folder=root/"operator-pruning";folder.mkdir(exist_ok=True);path=folder/"status.json"
    try:old=load(path)
    except (OSError,ValueError):old={}
    now=time.time();ok=reason is None
    since=now if ok else old.get("deferred_since") or now
    new={"checked_at":now,"last_ran_at":now if ok else old.get("last_ran_at"),"deferred_since":None if ok else since,"reason":reason}
    if not ok and now-since>=STALL_WARN:print("warning: pruning deferred for %.1f h: %s"%((now-since)/3600,reason))
    tmp=path.with_suffix(".new");tmp.write_text(json.dumps(new)+"\n");os.replace(tmp,path)
def plan(root, items, jobs, mounts, now):
    protected = [pathlib.Path(m).resolve() for m in mounts]
    def blocked(path):
        p=path.resolve()
        return path.is_symlink() or any(p==m or p in m.parents or m in p.parents for m in protected)
    def old(path):
        return path.exists() and now-path.stat().st_mtime >= AGE
    def terminal(t):
        j=jobs.get(t.get("job_id"), {})
        return t.get("job_id") != PIN and (t.get("status")=="abandoned" or
            t.get("status")=="closed" and j.get("status") in DONE)
    out=[]
    for key,t in items.items():
        if not re.fullmatch(r"[0-9a-f]{64}",key) or not terminal(t): continue
        finished = t.get("finished_at", jobs.get(t.get("job_id"), {}).get("finished_at", 0))
        if now - finished < AGE: continue
        for path in [root/"coordination/trees"/key,root/"checks"/key]:
            if old(path) and not blocked(path):out.append(path)
    # Keep the two newest releases the broker can use as its base (host.py _current_release_blob:
    # isolated, completed or adopted, by finish time); check jobs never count, as well as all mounted paths.
    completed=sorted((j for j in jobs.values() if j.get("isolated") and (j.get("status")=="completed" or j.get("operator_adopted_at"))),
                     key=lambda j:(j.get("finished_at",j.get("created_at",0)),j.get("job_id","")),reverse=True)
    recent={j["job_id"] for j in completed[:2]}
    for key,j in jobs.items():
        path=root/key
        if key==PIN or key in recent or j.get("status") not in DONE or now-j.get("finished_at",0)<AGE:continue
        if old(path) and not blocked(path):out.append(path)
    # Preserve current UI selection and two newest immutable asset releases.
    assets=root/"ui-assets/releases"
    if assets.exists():
        folders=sorted((p for p in assets.iterdir() if p.is_dir() and not p.is_symlink()),key=lambda p:p.stat().st_mtime,reverse=True)
        for path in folders[2:]:
            j=jobs.get(path.name,{})
            if j.get("status")=="completed" and old(path) and not blocked(path):out.append(path)
    return out
def main():
    ap=argparse.ArgumentParser(description=__doc__);ap.add_argument("--apply",action="store_true");args=ap.parse_args()
    with (ROOT/".v3-prune.lock").open("a") as lock:
        try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:print("deferred: another prune holds the lock");return
        busy=build_busy(ROOT)
        if busy:print("deferred: "+busy);status(ROOT,busy);return
        items=load(ROOT/"coordination/items.json")
        jobs={p.stem:load(p) for p in ROOT.glob("*.json")};before=fingerprint(ROOT,jobs)
        containers=json.loads(subprocess.check_output(["docker","inspect","webagent-dev-v3","webagent-dev-v3-loop","webagent-dev-v3-browser"],text=True))
        if any(c["State"].get("Health",{}).get("Status")!="healthy" for c in containers):
            print("deferred: v3 runtime not healthy");status(ROOT,"v3 runtime not healthy");return
        mounts=[m["Source"] for c in containers for m in c["Mounts"] if m["Source"].startswith(str(ROOT))]
        current=ROOT/"ui-assets/current"
        if current.exists():mounts.append(str(current.resolve()))
        # Broad /live-checkout mount is intentionally not a blanket artifact pin.
        candidates=plan(ROOT,items,jobs,mounts,time.time())
        total=0;removed=[]
        for path in candidates:
            size=int(subprocess.check_output(["du","-sk",str(path)],text=True).split()[0])*1024
            if args.apply:
                if before!=fingerprint(ROOT,jobs) or build_busy(ROOT):print("deferred: job records or build phase changed; cleanup stopped");break
                # Task records change during builds, so re-plan instead of fingerprinting items.json.
                if path not in plan(ROOT,load(ROOT/"coordination/items.json"),jobs,mounts,time.time()):continue
                shutil.rmtree(path)
            total+=size;removed.append({"path":str(path),"bytes":size})
        report={"applied":args.apply,"count":len(removed),"GiB":round(total/1024**3,3),"artifacts":removed,"preserved_job":PIN}
        print(json.dumps({k:v for k,v in report.items() if k!="artifacts"}))
        if args.apply:status(ROOT,None)
        if args.apply and removed:
            folder=ROOT/"operator-pruning";folder.mkdir(exist_ok=True)
            (folder/(time.strftime("%Y%m%dT%H%M%SZ",time.gmtime())+".json")).write_text(json.dumps(report,indent=2)+"\n")
if __name__=="__main__":main()
