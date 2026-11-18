#!/usr/bin/python3 -I
"""Build helper: runs the broker's Rust test half on a second machine (the NUC).

Installed on the helper at /usr/local/lib/broker-helper/remote_helper.py and reached only
as the SSH forced command of the broker's own key, so the server never gets a shell:

    command="/usr/bin/python3 -I /usr/local/lib/broker-helper/remote_helper.py serve",restrict ssh-ed25519 ...

The server names one fixed verb per connection (SSH_ORIGINAL_COMMAND) and streams data on
stdin. Containers use the broker's compiler image by exact ID and the same isolation flags
as the server's phases. One job holds the helper at a time. `prune` runs from a timer.
The server trusts passing results, rechecks binary hashes, and confirms any failure itself.
"""
import fcntl,hashlib,json,os,re,shutil,subprocess,sys,tarfile,time
from pathlib import Path

STATE=Path(os.environ.get("BROKER_HELPER_STATE","/var/lib/broker-helper"))
SETTINGS=Path("/etc/broker-helper.json")
WORK=STATE/"work";SOURCE=WORK/"source";OUTPUT=WORK/"output";CARGO=STATE/"cargo"
LOCK=STATE/"job.lock";STATUS=STATE/"status.json";PRUNE=STATE/"prune.json"
VERSION=2
TAG="broker-helper-compiler"
JOB=re.compile(r"[a-f0-9-]{36}");IMAGE=re.compile(r"sha256:[a-f0-9]{64}");KEY=re.compile(r"[a-f0-9]{64}")
PHASES={"compile_test","test"}
DEFAULTS={"uid":1000,"gid":1000,"cargo_build_jobs":12,"memory_bytes":13*1024**3,"min_free_bytes":15*1024**3,
    "target_max_bytes":100*1024**3,"cargo_keep":3,"stale_lock_seconds":7200}
RUSTFLAGS="-C debuginfo=0 -C link-arg=-fuse-ld=mold"


def settings():
    try:value=json.loads(SETTINGS.read_text())
    except (OSError,ValueError):value={}
    return {**DEFAULTS,**{k:v for k,v in value.items()if k in DEFAULTS}}


def write_json(path,value):
    path.parent.mkdir(parents=True,exist_ok=True)
    temporary=path.with_suffix(".new");temporary.write_text(json.dumps(value,sort_keys=True));os.replace(temporary,path)


def status(**value):
    write_json(STATUS,{"updated_at":time.time(),**value})


def fail(message,code=2):
    sys.stderr.write(message[:2000]+"\n");sys.exit(code)


def run(argv,timeout=60,**kwargs):
    return subprocess.run(argv,capture_output=True,text=True,timeout=timeout,**kwargs)


def tree_digest(root):
    """sha256 over (path, executable bit, content sha256) of every regular file; the server computes the same."""
    rows=[]
    for path in sorted(root.rglob("*")):
        if path.is_symlink():rows.append((path.relative_to(root).as_posix(),"link",os.readlink(path)));continue
        if not path.is_file():continue
        with path.open("rb")as stream:sha=hashlib.file_digest(stream,"sha256").hexdigest()
        rows.append((path.relative_to(root).as_posix(),"x"if path.stat().st_mode&0o111 else"f",sha))
    return hashlib.sha256(json.dumps(rows).encode()).hexdigest()


def safe_member(member,root):
    name=Path(member.name)
    if name.is_absolute()or".."in name.parts or not(member.isfile()or member.isdir()or member.issym()):fail("unsafe archive member")
    if member.issym()and(Path(member.linkname).is_absolute()or".."in Path(member.linkname).parts):fail("unsafe archive link")
    return root/name


def sync_source(stream,root):
    """Mirror the streamed tree into root, rewriting only changed files so unchanged ones keep
    their mtimes and Cargo rebuilds only what changed; files absent from the stream are removed."""
    root.mkdir(parents=True,exist_ok=True);seen=set();changed=0
    with tarfile.open(fileobj=stream,mode="r|")as archive:
        for member in archive:
            target=safe_member(member,root);seen.add(target)
            if member.isdir():target.mkdir(parents=True,exist_ok=True);continue
            target.parent.mkdir(parents=True,exist_ok=True)
            if member.issym():
                if target.is_symlink()and os.readlink(target)==member.linkname:continue
                if target.exists()or target.is_symlink():shutil.rmtree(target)if target.is_dir()and not target.is_symlink()else target.unlink()
                os.symlink(member.linkname,target);changed+=1;continue
            body=archive.extractfile(member).read();mode=0o755 if member.mode&0o111 else 0o644
            if target.is_file()and not target.is_symlink()and target.read_bytes()==body:
                if target.stat().st_mode&0o777!=mode:target.chmod(mode)
                continue
            if target.is_symlink()or target.is_dir():shutil.rmtree(target)if target.is_dir()and not target.is_symlink()else target.unlink()
            temporary=target.with_name(target.name+".helper-new");temporary.write_bytes(body);temporary.chmod(mode);os.replace(temporary,target);changed+=1
    removed=0
    for path in sorted(root.rglob("*"),reverse=True):
        if path in seen:continue
        if path.is_dir()and not path.is_symlink():
            if not any(path.iterdir()):path.rmdir()
            continue
        path.unlink();removed+=1
    return {"changed":changed,"removed":removed,"sha256":tree_digest(root)}


def held(job):
    try:value=json.loads(LOCK.read_text())
    except (OSError,ValueError):return None
    return value if value.get("job_id")==job else None


def free_bytes():
    return shutil.disk_usage(STATE).free


def cmd_status(args):
    lock=None
    try:lock=json.loads(LOCK.read_text())
    except (OSError,ValueError):pass
    images=run(["docker","image","ls","--no-trunc","--format","{{.ID}}"]).stdout.split()
    print(json.dumps({"version":VERSION,"busy":lock,"free_bytes":free_bytes(),"cpus":os.cpu_count(),"images":images,
        "cargo_keys":sorted(p.name for p in CARGO.iterdir())if CARGO.is_dir()else[]}))


def cmd_begin(args):
    job,=args
    if not JOB.fullmatch(job):fail("job id")
    cfg=settings();STATE.mkdir(parents=True,exist_ok=True)
    with open(STATE/".lock","w")as guard:
        fcntl.flock(guard,fcntl.LOCK_EX)
        try:current=json.loads(LOCK.read_text())
        except (OSError,ValueError):current=None
        if current and current.get("job_id")!=job and time.time()-current.get("at",0)<cfg["stale_lock_seconds"]:
            print(json.dumps({"decision":"busy","job_id":current.get("job_id")}));return
        if free_bytes()<cfg["min_free_bytes"]:
            cmd_prune([])
            if free_bytes()<cfg["min_free_bytes"]:
                status(state="refusing",reason="disk below %dG free"%(cfg["min_free_bytes"]//2**30))
                print(json.dumps({"decision":"refused","reason":"disk_below_floor","free_bytes":free_bytes()}));return
        kill_containers()
        write_json(LOCK,{"job_id":job,"at":time.time()})
    # Per-job outputs never carry over; the warm compile cache (target/) does.
    for name in ("artifacts","stable","test-artifacts.json"):
        path=OUTPUT/name
        if path.is_dir():shutil.rmtree(path)
        elif path.exists():path.unlink()
    OUTPUT.mkdir(parents=True,exist_ok=True)
    status(state="running",job_id=job,stage="preparing",started_at=time.time())
    print(json.dumps({"decision":"accepted","cpus":os.cpu_count()}))


def cmd_end(args):
    job,result=args
    # Only the job holding the helper may free it or report its result.
    if held(job):
        LOCK.unlink()
        status(state="idle",last_job=job,last_result=result if result in {"passed","failed","fallback","cancelled"}else"ended")
    print("{}")


def require(job):
    if not JOB.fullmatch(job)or not held(job):fail("job does not hold the helper")


def cmd_sync_source(args):
    job,=args;require(job)
    status(state="running",job_id=job,stage="receiving source",started_at=json.loads(LOCK.read_text())["at"])
    print(json.dumps(sync_source(sys.stdin.buffer,SOURCE/"v3")))


def cmd_has_cargo(args):
    key,=args
    if not KEY.fullmatch(key):fail("cargo key")
    print(json.dumps({"present":(CARGO/key/"complete").is_file()}))
    if (CARGO/key).is_dir():os.utime(CARGO/key)


def cmd_put_cargo(args):
    job,key=args;require(job)
    if not KEY.fullmatch(key):fail("cargo key")
    temporary=CARGO/(key+".receiving");shutil.rmtree(temporary,ignore_errors=True);temporary.mkdir(parents=True)
    with tarfile.open(fileobj=sys.stdin.buffer,mode="r|")as archive:
        for member in archive:
            target=safe_member(member,temporary)
            if not member.name.startswith("registry"):fail("cargo archive outside registry")
            archive.extract(member,temporary,filter="data")
    (temporary/"complete").write_text(key)
    shutil.rmtree(CARGO/key,ignore_errors=True);os.replace(temporary,CARGO/key)
    print(json.dumps({"stored":key}))


def cmd_load_image(args):
    job,image=args;require(job)
    if not IMAGE.fullmatch(image):fail("image id")
    loaded=subprocess.run(["docker","load"],stdin=sys.stdin.buffer,capture_output=True,text=True,timeout=1800)
    if loaded.returncode:fail("docker load failed: "+loaded.stderr)
    keep_image(image)
    print(json.dumps({"present":image_present(image)}))


def keep_image(image):
    """An image saved by ID loads untagged, and Docker prunes untagged images as dangling; tag it
    and note its last use so cleanup keeps the two most recent."""
    run(["docker","image","tag",image,TAG+":"+image[7:19]])
    mark=STATE/"images"/image[7:19];mark.parent.mkdir(parents=True,exist_ok=True);mark.touch()


def image_present(image):
    found=run(["docker","image","inspect","--format","{{.Id}}",image])
    return found.returncode==0 and found.stdout.strip()==image


def kill_containers():
    names=run(["docker","ps","-aq","--filter","label=broker-helper.job"]).stdout.split()
    if names:run(["docker","rm","-f",*names],timeout=120)


def cmd_run(args):
    """One phase container. stdin: {"argv": [...], "image": ID, "cargo_key": KEY, "incremental": "0"|"1"}."""
    job,phase,timeout=args;require(job)
    if phase not in PHASES or not timeout.isdigit():fail("phase")
    request=json.loads(sys.stdin.buffer.read(1024*1024))
    argv,image,key,incremental=request["argv"],request["image"],request["cargo_key"],request["incremental"]
    if not(isinstance(argv,list)and argv and all(isinstance(a,str)for a in argv))or not IMAGE.fullmatch(image)or not KEY.fullmatch(key)or incremental not in {"0","1"}:fail("request")
    if not image_present(image):fail("compiler image missing",3)
    keep_image(image)
    if not (CARGO/key/"complete").is_file():fail("cargo registry missing",3)
    cfg=settings();name="broker-helper-"+job.replace("-","")+"-"+phase+"-"+os.urandom(4).hex()
    status(state="running",job_id=job,stage=phase.replace("_"," "),started_at=json.loads(LOCK.read_text())["at"])
    (OUTPUT/"target").mkdir(parents=True,exist_ok=True)
    create=["docker","create","--name",name,"--label","broker-helper.job="+job,"--cap-drop","ALL","--security-opt","no-new-privileges",
        "--pids-limit","256","--user","%d:%d"%(cfg["uid"],cfg["gid"]),"--network","none","--memory",str(cfg["memory_bytes"]),"--memory-swap",str(cfg["memory_bytes"]),
        "--mount","type=bind,src=%s,dst=/build%s"%(SOURCE/"v3",""if phase=="test"else",readonly"),
        "--mount","type=bind,src=%s,dst=/output"%OUTPUT,"--mount","type=bind,src=%s,dst=/output/cargo"%(CARGO/key),"-w","/build",
        "-e","CARGO_HOME=/output/cargo","-e","CARGO_TARGET_DIR=/output/target","-e","CARGO_BUILD_JOBS=%d"%cfg["cargo_build_jobs"],
        "-e","CARGO_INCREMENTAL="+incremental,"--env","RUSTFLAGS="+RUSTFLAGS,"-e","CARGO_PROFILE_DEV_DEBUG=0","-e","CARGO_PROFILE_TEST_DEBUG=0","-e","CARGO_PROFILE_RELEASE_DEBUG=0",
        *(["-e","RUST_MIN_STACK=33554432","--tmpfs","/tmp:rw,exec,nosuid,nodev,size=2g,mode=1777"]if phase=="test"else[]),
        image,*argv]
    created=run(create)
    if created.returncode:fail("container create failed: "+created.stderr)
    deadline=time.monotonic()+int(timeout);parent=os.getppid()
    try:
        logs=subprocess.Popen(["docker","start","-a",name],stdout=sys.stdout.buffer,stderr=subprocess.STDOUT)
        while logs.poll()is None:
            # The server ended the connection (job cancelled or timed out): stop at once.
            if os.getppid()!=parent or time.monotonic()>deadline:
                run(["docker","kill",name]);logs.wait(30);fail("phase stopped: "+("deadline"if time.monotonic()>deadline else"server disconnected"),124)
            time.sleep(0.5)
        code=int(run(["docker","inspect","--format","{{.State.ExitCode}}",name]).stdout.strip()or 1)
    finally:run(["docker","rm","-f",name],timeout=120)
    sys.stdout.flush();sys.exit(code)


def output_path(relative):
    path=Path(relative)
    if path.is_absolute()or".."in path.parts or not relative or not re.fullmatch(r"[A-Za-z0-9_./+-]+",relative):fail("path")
    full=OUTPUT/path
    if full.is_symlink()or not full.is_file():fail("missing output: "+relative)
    return full


def cmd_fetch(args):
    """stdout: tar of the named output files (stdin: JSON list of paths relative to /output)."""
    job,=args;require(job)
    names=json.loads(sys.stdin.buffer.read(4*1024*1024))
    if not isinstance(names,list)or len(names)>4096:fail("names")
    with tarfile.open(fileobj=sys.stdout.buffer,mode="w|")as archive:
        for name in names:archive.add(output_path(name),arcname=name,recursive=False)


def cmd_stage(args):
    """Copy test executables to the paths the suite script names; return their sha256 for the server to compare."""
    job,=args;require(job)
    pairs=json.loads(sys.stdin.buffer.read(4*1024*1024));hashes={}
    for source,destination in pairs:
        if not re.fullmatch(r"(tests|test-support/debug)/[A-Za-z0-9_-]+",destination):fail("stage destination")
        origin=output_path(source);target=OUTPUT/"artifacts"/destination
        target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(origin,target);target.chmod(0o555)
        with target.open("rb")as stream:hashes[destination]=hashlib.file_digest(stream,"sha256").hexdigest()
    print(json.dumps(hashes))


def cmd_cancel(args):
    job,=args
    if JOB.fullmatch(job)and held(job):kill_containers();LOCK.unlink(missing_ok=True);status(state="idle",last_job=job,last_result="cancelled")
    print("{}")


def size(path):
    return sum(p.lstat().st_size for p in path.rglob("*")if p.is_file()and not p.is_symlink())if path.is_dir()else 0


def cmd_prune(args):
    """Timer cleanup: never while a job holds the helper. Removes the compile cache when over its cap,
    old crate stores, stopped helper containers and unused images; records what it freed."""
    cfg=settings()
    try:current=json.loads(LOCK.read_text())
    except (OSError,ValueError):current=None
    if current and time.time()-current.get("at",0)<cfg["stale_lock_seconds"]:return
    before=free_bytes();removed=[]
    kill_containers()
    if size(OUTPUT/"target")>cfg["target_max_bytes"]:shutil.rmtree(OUTPUT/"target");removed.append("target")
    if CARGO.is_dir():
        stores=sorted((p for p in CARGO.iterdir()if p.is_dir()),key=lambda p:p.stat().st_mtime,reverse=True)
        for store in stores[cfg["cargo_keep"]:]:shutil.rmtree(store);removed.append("cargo:"+store.name[:12])
    # Keep the two most recently used compiler images (current and previous recipe); drop older ones.
    tags=run(["docker","image","ls",TAG,"--format","{{.Tag}}"]).stdout.split()
    used=sorted(tags,key=lambda tag:(STATE/"images"/tag).stat().st_mtime if (STATE/"images"/tag).exists()else 0,reverse=True)
    for tag in used[2:]:
        if run(["docker","image","rm",TAG+":"+tag]).returncode==0:removed.append("image:"+tag)
    run(["docker","image","prune","-f"],timeout=300)
    freed=max(0,free_bytes()-before)
    if removed or not PRUNE.exists():write_json(PRUNE,{"at":time.time(),"freed_bytes":freed,"removed":removed})
    if not args:return
    print(json.dumps({"freed_bytes":freed,"removed":removed}))


VERBS={"status":cmd_status,"begin":cmd_begin,"end":cmd_end,"sync-source":cmd_sync_source,"has-cargo":cmd_has_cargo,
    "put-cargo":cmd_put_cargo,"load-image":cmd_load_image,"run":cmd_run,"fetch":cmd_fetch,"stage":cmd_stage,"cancel":cmd_cancel}


def main():
    if sys.argv[1:]==["prune"]:cmd_prune(["timer"]);return
    if sys.argv[1:]!=["serve"]:fail("usage: remote_helper.py serve|prune")
    words=os.environ.get("SSH_ORIGINAL_COMMAND","").split()
    if not words or words[0]not in VERBS or len(words)>4:fail("unknown verb")
    VERBS[words[0]](words[1:])


if __name__=="__main__":main()
