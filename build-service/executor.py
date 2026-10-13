"""Attributable compilation/testing and artifact-fed packaging in the pinned toolchain."""
import contextlib,hashlib,io,json,os,posixpath,re,shlex,shutil,subprocess,tarfile,time,uuid
from pathlib import Path
RUST="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
BUILDKIT="moby/buildkit:v0.25.1@sha256:79cc6476ab1a3371c9afd8b44e7c55610057c43e18d9b39b68e2b0c2475cc1b6"
COMPILER_STAGE_SHA256="696af9712c1bf8f3a1d24f86e1aeeaf2c23c6aa67b3796d95e3b0553683501f2"
BINARIES=["webagent-v3","webagent-mcp","webagent-diagnostics","webagent-optimization","webagent-automation","webagent-outbound-notifications"]
# @code-index {"version":1,"feature":"build-artifact-execution","aliases":["task fencing","verified builds"],"purpose":"Pinned attributable compiler/test/packaging recipes and verified content-addressed artifact reuse.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Identity, exact input verification and completion rules cannot be disabled by model or preference configuration."}}
class Executor:
    """Fixed phase recipes; model arguments never become host commands or mount paths."""
    def __init__(self,runner,resources,config):
        """Bind fixed build recipes to the broker resource controller and reviewed installation."""
        self.runner=runner;self.resources=resources;self.config=config
    def phase(self,job,phase,kind,source,output,argv,profile,timeout=3600,network=False):
        """Launch one identified container and preserve counters before removing a terminal container."""
        jobhex=uuid.UUID(job["job_id"]).hex
        name="webagent-build-"+self.config["instance"]+"-"+jobhex+"-"+phase+"-"+uuid.uuid4().hex[:8]
        output.mkdir(parents=True,exist_ok=True)
        self.resources.enter(name,job,phase,profile,kind);identity=None;success=False
        parent="webagent-build-"+self.config["instance"]+".slice"
        create=["docker","create","--name",name,"--cgroup-parent",parent,"--label","webagent.scheduler.instance="+self.config["instance"],"--label","webagent.scheduler.job="+jobhex,"--label","webagent.scheduler.phase="+phase,
            "--cap-drop","ALL","--security-opt","no-new-privileges","--pids-limit","256","--user",str(self.config["service_uid"])+":"+str(self.config["service_gid"]),
            "--network","bridge"if network else"none","--mount","type=bind,src="+str(source)+",dst=/build"+(""if phase=="test"else",readonly"),
            "--mount","type=bind,src="+str(output)+",dst=/output","-w","/build",
            "-e","CARGO_HOME=/output/cargo","-e","CARGO_TARGET_DIR=/output/target","-e","CARGO_BUILD_JOBS=1","-e","CARGO_INCREMENTAL=0",
            "-e","CARGO_PROFILE_DEV_DEBUG=0","-e","CARGO_PROFILE_TEST_DEBUG=0","-e","CARGO_PROFILE_RELEASE_DEBUG=0",RUST,"sleep","infinity"]
        try:
            identity=self.runner.command(create,timeout=30,private_raw=True).strip()
            if len(identity)!=64 or any(c not in"0123456789abcdef"for c in identity):raise ValueError("Compiler container identity invalid")
            with self.resources.lock:self.resources.rows[name]["container_id"]=identity
            self.runner.command(["docker","start",name],timeout=30)
            self.resources.attached(name,identity)
            self.runner.job_context.phase_container=name
            # Docker wait reports exit status as data. Logs and counters remain available afterward.
            logs=self.runner.command(["docker","exec",name,*argv],timeout=timeout)
            success=True;return logs
        finally:
            self.runner.job_context.phase_container=None
            self.finish_phase(name,identity,success)
    @contextlib.contextmanager
    def packager(self,job,role="generic"):
        """Attribute a dedicated BuildKit daemon and keep measured profiles separate for each runtime role."""
        name="webagent-build-"+self.config["instance"]+"-"+uuid.UUID(job["job_id"]).hex+"-package-"+uuid.uuid4().hex[:8]
        if role not in {"api","loop","generic"}:raise ValueError("Fixed packaging role required")
        identity=None;success=False;self.resources.enter(name,job,"package","buildkit:v0.25.1:role-context-v2:"+role,"package")
        try:
            identity=self.runner.command(["docker","create","--name",name,"--privileged",
                "--cgroup-parent","webagent-build-"+self.config["instance"]+".slice",
                "--label","webagent.scheduler.instance="+self.config["instance"],
                "--label","webagent.scheduler.job="+uuid.UUID(job["job_id"]).hex,
                "--label","webagent.scheduler.phase=package","--pids-limit","512",
                BUILDKIT,"--addr","unix:///run/buildkit/buildkitd.sock"],timeout=30,private_raw=True).strip()
            if not re.fullmatch(r"[a-f0-9]{64}",identity):raise ValueError("Packaging container identity invalid")
            with self.resources.lock:self.resources.rows[name]["container_id"]=identity
            self.runner.command(["docker","start",name],timeout=30);self.resources.attached(name,identity)
            self.runner.command(["docker","buildx","create","--name",name,"--driver","remote","docker-container://"+name],timeout=30)
            self.runner.command(["docker","buildx","inspect",name,"--bootstrap"],timeout=60)
            self.runner.job_context.phase_container=name
            yield name
            success=True
        finally:
            self.runner.job_context.phase_container=None
            try:
                environment={"PATH":"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin","LANG":"C.UTF-8","HOME":self.config.get("service_home",os.path.expanduser("~"))}
                subprocess.run(["docker","buildx","rm",name],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=30,env=environment)
            finally:self.finish_phase(name,identity,success)
    def finish_phase(self,name,identity,success):
        """Record a terminal phase only after exact-container cleanup is confirmed."""
        if identity is not None:
            try:
                counters=self.runner.resource_request({"operation":"stats","container":name})
                with self.resources.lock:self.resources.rows[name]["metrics"]=counters
            except (OSError,ValueError,RuntimeError,TimeoutError):pass
        if identity is None:
            with self.resources.condition:
                self.resources.rows[name].update(state="recovery_required",reason="Container creation outcome requires exact identity reconciliation")
                self.resources.persist();self.resources.condition.notify_all()
            return
        try:
            self.runner.cleanup_phase(name)
        except (OSError,ValueError,RuntimeError,PermissionError,subprocess.SubprocessError):
            with self.resources.condition:
                self.resources.rows[name].update(state="recovery_required",reason="Container cleanup needs explicit reconciliation")
                self.resources.persist();self.resources.condition.notify_all()
            raise
        self.resources.finish(name,success)

    def check(self,job,blob,task,timeout,extract):
        """Use the same attributable recipes for bounded foreground checks and release compilation."""
        job["artifact_component"]="engine"
        root=Path(self.config["state_root"])/"checks"/job["job_id"];source=root/"source";output=root/"output"
        blob=self.runner.execution_application_blob(blob);root.mkdir(parents=True,exist_ok=True);extract(source,blob);output.mkdir(exist_ok=True)
        application=source/"v3";before=self.runner.execution_source_digest(source);start=time.monotonic()
        log=self.phase(job,"dependency","dependency",application,output,["cargo","fetch","--locked"],"engine:rust1.90:dependency",min(timeout,600),network=True)
        remaining=max(1,timeout-int(time.monotonic()-start))
        argv=["cargo","check","--locked","--offline"]if task=="check"else["cargo","test","--locked","--offline","--no-run"]
        log+=self.phase(job,"compile_test","engine",application,output,argv,"engine:rust1.90:"+task,remaining)
        if task=="test":
            log+=self.phase(job,"test","test",application,output,["cargo","test","--locked","--offline","--","--test-threads=1"],"engine:rust1.90:test",max(1,timeout-int(time.monotonic()-start)))
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Check changed its captured application inputs")
        return log

    @staticmethod
    def artifact_hashes(folder):
        """Bind every packaged output, including descriptors and offline registry, to its exact bytes."""
        if folder.is_symlink():raise ValueError("Artifact symlink")
        hashes={}
        for path in sorted(folder.rglob("*")):
            if path.is_symlink():raise ValueError("Artifact symlink")
            if path.is_file():
                digest=hashlib.sha256()
                with path.open("rb")as stream:
                    for chunk in iter(lambda:stream.read(1024*1024),b""):digest.update(chunk)
                hashes[path.relative_to(folder).as_posix()]=digest.hexdigest()
        return hashes
    @staticmethod
    def rust_inputs(files):
        """Detect all core Rust/Cargo source changes and added files, independent of old compiler dependencies."""
        return {name:hashlib.sha256(body).hexdigest()for name,(_,body)in files.items()
            if name in {"v3/Cargo.toml","v3/Cargo.lock","v3/build.rs"}or(name.startswith("v3/src/")and(name.endswith(".rs")or name.startswith("v3/src/plugin_packages/")))}
    def cached_engine(self,blob,output):
        """Reuse only artifacts whose pinned recipe and complete compiled inputs still match the candidate."""
        cache=Path(self.config["state_root"])/"engine-artifacts";cache.mkdir(mode=0o700,exist_ok=True)
        files=self.runner.execution_entries(blob);rust=self.rust_inputs(files)
        for path in sorted(cache.glob("*.json"),key=lambda p:p.stat().st_mtime,reverse=True)[:64]:
            if path.is_symlink():raise ValueError("Engine cache receipt symlink")
            value=json.loads(path.read_text())
            if value["recipe"]!=RUST+":single-job:debug0:v1"or value["rust_inputs"]!=rust:continue
            if any(name not in files or hashlib.sha256(files[name][1]).hexdigest()!=sha for name,sha in value["compiled_inputs"].items()):continue
            if not re.fullmatch(r"[a-f0-9-]{36}",value["job_id"]):raise ValueError("Engine cache job identity invalid")
            source=Path(self.config["state_root"])/"execution"/value["job_id"]/"output/artifacts"
            if self.artifact_hashes(source)!=value["artifacts"]:raise ValueError("Cached compiler output changed")
            destination=output/"artifacts";shutil.copytree(source,destination)
            return destination,value
        return None
    @staticmethod
    def local_dependency(name):
        """Normalize actual Cargo dep-info paths while rejecting escaped local inputs."""
        if name.startswith("/build/"):name=name[len("/build/"):]
        elif name.startswith("/"):return None
        name=posixpath.normpath(name)
        if name in {"",".",".."}or name.startswith("../"):raise ValueError("Compiler dependency escapes source")
        return "v3/"+name
    @staticmethod
    def compiler_dependencies(target):
        """Read Cargo make-style dependencies, excluding its environment/comment metadata."""
        dependencies=set()
        for profile in ("debug","release"):
            for path in(target/profile/"deps").glob("*.d"):
                for line in path.read_text().replace(chr(92)+chr(10)," ").splitlines():
                    if not line.lstrip().startswith("#")and ":"in line:
                        dependencies.update(shlex.split(line.split(":",1)[1]))
        return dependencies
    def seed_cargo_cache(self,job,blob,output):
        """Reuse compatible dependency objects, then rebuild the root package and rerun current tests."""
        files=self.runner.execution_entries(blob);folder=Path(self.config["state_root"])/"execution"
        if folder.is_symlink():raise ValueError("Compiler cache directory replaced")
        with self.resources.lock:phases=list(self.resources.rows.values())
        for previous in sorted(folder.iterdir(),key=lambda p:p.stat().st_mtime,reverse=True)[:64]:
            if previous.name==job["job_id"]or not re.fullmatch(r"[a-f0-9-]{36}",previous.name):continue
            success={r["phase"]for r in phases if r["job_id"]==previous.name and r["state"]=="finished"
                and r["profile"]in {"engine:rust1.90:compile_test","engine:rust1.90:test","engine:rust1.90:release"}}
            if not {"compile_test","test","compile_release"}<=success:continue
            receipt=Path(self.config["state_root"])/(previous.name+".json")
            captured=Path(self.config["state_root"])/previous.name/"candidate.tar"
            if any(path.is_symlink()for path in (previous,receipt,captured,previous/"source",previous/"output",previous/"output/cargo",previous/"output/target")):
                raise ValueError("Compiler cache symlink")
            if not receipt.is_file()or not captured.is_file():continue
            state=json.loads(receipt.read_text())
            if state["job_id"]!=previous.name or state["status"]not in {"failed","completed"}:continue
            old_blob=self.runner.execution_application_blob(captured.read_bytes())
            old=self.runner.execution_entries(old_blob)
            if self.rust_inputs(old)!=self.rust_inputs(files):continue
            if self.runner.execution_source_digest(previous/"source")!=self.runner.execution_source_digest_blob(old_blob):
                raise ValueError("Previous compiler source changed")
            target=previous/"output/target";cargo=previous/"output/cargo"
            if not target.is_dir()or not cargo.is_dir():continue
            dependencies=self.compiler_dependencies(target)
            local={self.local_dependency(name)for name in dependencies}-{None}
            if "v3/src/main.rs"not in local:continue
            if any(name not in old or name not in files or old[name]!=files[name]for name in local):continue
            before={name:self.artifact_hashes(previous/"output"/name)for name in ("cargo","target")}
            for name in ("cargo","target"):shutil.copytree(previous/"output"/name,output/name)
            if before!={name:self.artifact_hashes(previous/"output"/name)for name in ("cargo","target")}:
                raise ValueError("Previous dependency cache changed during copy")
            job["cargo_cache_reuse"]=previous.name
            return True
        return False
    def retain_engine(self,job,blob,dependencies,hashes):
        """Publish a content-addressed cache receipt only after tests, source and every output are verified."""
        files=self.runner.execution_entries(blob);inputs={}
        for name in dependencies:
            relative=self.local_dependency(name)
            if relative is not None:
                if relative not in files:raise ValueError("Compiled local input absent from captured source")
                inputs[relative]=hashlib.sha256(files[relative][1]).hexdigest()
        value={"recipe":RUST+":single-job:debug0:v1","rust_inputs":self.rust_inputs(files),"compiled_inputs":inputs,
            "artifacts":hashes,"job_id":job["job_id"]}
        if not inputs:raise ValueError("Compiled input closure missing")
        key=hashlib.sha256(json.dumps({k:v for k,v in value.items()if k not in {"job_id","artifacts"}},sort_keys=True).encode()).hexdigest()
        cache=Path(self.config["state_root"])/"engine-artifacts";cache.mkdir(mode=0o700,exist_ok=True)
        temporary=cache/(key+".new");temporary.write_text(json.dumps(value));temporary.chmod(0o600);os.replace(temporary,cache/(key+".json"))

    def validate_compiler_recipe(self,blob):
        """Reject a changed compiler Dockerfile stage rather than silently discard reviewed build instructions."""
        text=self.runner.execution_entries(blob)["v3/Dockerfile"][1].decode()
        start=text.index("FROM rust:");end=text.index("FROM debian:",start)
        if hashlib.sha256(text[start:end].encode()).hexdigest()!=COMPILER_STAGE_SHA256:
            raise ValueError("Compiler Dockerfile recipe changed; migrate the reviewed attributable engine recipe")
    def engine(self,job,blob,extract,digest):
        """Compile/test once, retain input attestations and produce the complete existing image artifact set."""
        job["artifact_component"]="engine"
        blob=self.runner.execution_application_blob(blob);self.validate_compiler_recipe(blob)
        root=Path(self.config["state_root"])/"execution"/job["job_id"];source=root/"source";output=root/"output"
        if (root/"verified.json").exists():
            receipt=json.loads((root/"verified.json").read_text())
            if receipt["inputs_sha256"]!=digest(blob):raise ValueError("Artifact source changed")
            if self.artifact_hashes(output/"artifacts")!=receipt["artifacts"]:raise ValueError("Retained artifact changed")
            return output/"artifacts",receipt["log"]
        blob=self.runner.execution_application_blob(blob)
        root.mkdir(parents=True,exist_ok=True);extract(source,blob);application=source/"v3";output.mkdir(exist_ok=True)
        cached=self.cached_engine(blob,output)
        if cached:
            artifacts,old=cached;logs="Verified compiler artifacts reused; locked Rust tests previously passed on identical compiled inputs.\n"
            for binary in sorted((artifacts/"tests").iterdir()):
                if not re.fullmatch(r"[a-z0-9_]+-[a-f0-9]{16}",binary.name):raise ValueError("Cached test executable name invalid")
                logs+=self.phase(job,"test","test",application,output,["/output/artifacts/tests/"+binary.name,"--test-threads=1"],"engine:rust1.90:test")
            if self.runner.execution_source_digest(source)!=self.runner.execution_source_digest_blob(blob):
                raise ValueError("Cached tests changed candidate source")
            logs=logs.replace("locked Rust tests previously passed on identical compiled inputs.","current candidate ran retained tests from identical compiled inputs.")
            receipt={"inputs_sha256":digest(blob),"artifacts":old["artifacts"],"log":logs,"reused_from":old["job_id"]}
            temporary=root/"verified.new";temporary.write_text(json.dumps(receipt));os.replace(temporary,root/"verified.json")
            job["compiler_artifact_reuse"]=old["job_id"]
            return artifacts,logs
        seeded=self.seed_cargo_cache(job,blob,output)
        cargo=output/"cargo";cargo.mkdir(exist_ok=True)
        # First provisioning has no running API. The locked dependency phase can seed
        # its own registry; a retained runtime registry is only a compatible cache.
        if not seeded:
            try:self.runner.command(["docker","exec",self.config["api_container"],"test","-d","/opt/optimization-cargo/registry"],timeout=10)
            except RuntimeError:pass
            else:self.runner.command(["docker","cp",self.config["api_container"]+":/opt/optimization-cargo/registry",str(cargo)],timeout=120)
        inputs=digest(blob);before=self.runner.execution_source_digest(source)
        dependency=["sh","-c","set -e\ncargo clean --package webagent-v3\ncargo fetch --locked"]if seeded else["cargo","fetch","--locked"]
        logs=("Compatible Cargo dependency cache copied; root package rebuilt and current tests rerun.\n"if seeded else"")
        logs+=self.phase(job,"dependency","dependency",application,output,dependency,"engine:rust1.90:dependency",600,network=True)
        logs+=self.phase(job,"compile_test","engine",application,output,["cargo","test","--locked","--no-run"],"engine:rust1.90:compile_test")
        logs+=self.phase(job,"test","test",application,output,["cargo","test","--locked","--","--test-threads=1"],"engine:rust1.90:test")
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Tests left changed application inputs")
        logs+=self.phase(job,"compile_release","engine",application,output,["cargo","build","--release","--locked","--offline"],"engine:rust1.90:release")
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Compilation changed application inputs")
        artifacts=output/"artifacts";artifacts.mkdir(exist_ok=True);hashes={}
        for name in BINARIES:
            source_binary=output/"target/release"/name
            destination=artifacts/name;shutil.copyfile(source_binary,destination);destination.chmod(0o555)
            hashes[name]=hashlib.sha256(destination.read_bytes()).hexdigest()
        tests=artifacts/"tests";tests.mkdir()
        for binary in BINARIES:
            for path in(output/"target/debug/deps").glob(binary.replace("-","_")+"-*"):
                if path.is_file()and re.fullmatch(r"[a-z0-9_]+-[a-f0-9]{16}",path.name)and os.access(path,os.X_OK):
                    with path.open("rb")as stream:magic=stream.read(4)
                    if magic==b"\x7fELF":shutil.copyfile(path,tests/path.name);(tests/path.name).chmod(0o555)
        if not any(tests.iterdir()):raise ValueError("Retained locked test executables missing")
        # Preserve packaged module descriptors and the offline optimization registry.
        for package,binary in [("automation","webagent-automation"),("outbound_notifications","webagent-outbound-notifications")]:
            folder=artifacts/"modules"/package;folder.mkdir(parents=True)
            shutil.copyfile(artifacts/binary,folder/binary)
            for path in(application/"src/plugin_packages"/package).iterdir():
                if path.name in {"package.json","tool-schemas.json"}or path.name.startswith("page."):shutil.copyfile(path,folder/path.name)
        registry=artifacts/"optimization-cargo/registry";registry.parent.mkdir()
        shutil.copytree(cargo/"registry",registry)
        dependencies=self.compiler_dependencies(output/"target")
        (artifacts/"core-inputs.txt").write_text("\n".join(sorted(dependencies))+"\n")
        if "v3/src/main.rs"not in {self.local_dependency(name)for name in dependencies}:raise ValueError("Compiler dependency attestation missing")
        hashes=self.artifact_hashes(artifacts);self.retain_engine(job,blob,dependencies,hashes)
        receipt={"inputs_sha256":inputs,"artifacts":hashes,"log":logs}
        temporary=root/"verified.new";temporary.write_text(json.dumps(receipt));os.replace(temporary,root/"verified.json")
        return artifacts,logs
    def package(self,blob,artifacts,entries,role=None,destination=None):
        """Stream the runtime stage's declared artifact closure; retained tests stay only in the compiler cache."""
        files=entries(blob);dockerfile=files["v3/Dockerfile"][1].decode()
        prefixes=None
        if role is not None:
            if role not in {"api","loop"}:raise ValueError("Fixed packaging role required")
            stages=list(re.finditer(r"^FROM .+ AS ([a-z_]+)$",dockerfile,re.M))
            selected=next((i for i,value in enumerate(stages)if value[1]==role),None)
            if selected is None:raise ValueError("Runtime image stage unavailable")
            stage=dockerfile[stages[selected].end():stages[selected+1].start()if selected+1<len(stages)else len(dockerfile)]
            prefixes=[]
            for line in stage.splitlines():
                if not line.startswith("COPY --from=tested_builder "):continue
                parts=shlex.split(line)
                if len(parts)!=4:raise ValueError("Unsupported artifact COPY contract")
                name=parts[2]
                if name.startswith("/out/"):
                    relative=name[len("/out/"):].rstrip("/")
                    if not re.fullmatch(r"[A-Za-z0-9_./-]+",relative)or ".."in Path(relative).parts:raise ValueError("Unsafe runtime artifact")
                    if not(artifacts/relative).exists():raise ValueError("Required runtime artifact missing")
                    prefixes.append(relative)
            if not prefixes:raise ValueError("Runtime artifact closure missing")
        marker="FROM debian:bookworm-slim"
        start=dockerfile.index("FROM rust:");end=dockerfile.index(marker,start)
        dockerfile=dockerfile[:start]+"FROM "+("scratch"if role=="loop"else RUST)+" AS tested_builder\nCOPY scheduler-out /out\n"+dockerfile[end:]
        files["v3/Dockerfile"]=(0o644,dockerfile.encode())
        stream=io.BytesIO()if destination is None else Path(destination).open("xb")
        try:
            archive=tarfile.open(fileobj=stream,mode="w")
            with archive:
                for name,(mode,body)in sorted(files.items()):
                    member=tarfile.TarInfo(name);member.mode=mode;member.size=len(body);archive.addfile(member,io.BytesIO(body))
                for path in sorted(artifacts.rglob("*")):
                    if path.is_symlink():raise ValueError("Artifact symlink")
                    if path.is_file():
                        relative=path.relative_to(artifacts).as_posix()
                        if prefixes is not None and not any(relative==name or relative.startswith(name+"/")for name in prefixes):continue
                        member=tarfile.TarInfo("scheduler-out/"+relative);member.mode=path.stat().st_mode&0o777;member.size=path.stat().st_size
                        with path.open("rb")as body:archive.addfile(member,body)
            if destination is None:return stream.getvalue()
            stream.flush();os.fsync(stream.fileno())
            Path(destination).chmod(0o600)
            return Path(destination)
        finally:stream.close()
