"""Attributable compilation/testing and artifact-fed packaging in the pinned toolchain."""
import contextlib,hashlib,io,json,os,posixpath,re,shlex,shutil,stat,subprocess,tarfile,time,uuid
from pathlib import Path
RUST="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
BUILDKIT="moby/buildkit:v0.25.1@sha256:79cc6476ab1a3371c9afd8b44e7c55610057c43e18d9b39b68e2b0c2475cc1b6"
COMPILER_STAGE_SHA256="474b034d7a843ebf324b654a785b5a5b23135804ef39c662f92fcb3e160dfeaf"
BINARIES=["webagent-v3","webagent-mcp","webagent-diagnostics","webagent-optimization","webagent-automation","webagent-outbound-notifications","webagent-context-service","webagent-background-service","webagent-boot-service","webagent-boot-host"]
# @code-index {"version":1,"feature":"build-artifact-execution","aliases":["task fencing","verified builds"],"purpose":"Pinned attributable compiler/test/packaging recipes and verified content-addressed artifact reuse.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Identity, exact input verification and completion rules cannot be disabled by model or preference configuration."}}
COMPILER_MEMORY_CAP=4026531840
def compiler_cap(value):
    """Accept only the explicit reviewed compiler ceiling; omission preserves normal admission."""
    if value is None:return None
    if type(value)is not int or value!=COMPILER_MEMORY_CAP:raise ValueError("Unsupported compiler memory cap")
    return value

def test_filter(value):
    """Allow a bounded Rust unit-name substring, never command options or arbitrary arguments."""
    if value is None:return None
    if not isinstance(value,str)or not re.fullmatch(r"[A-Za-z0-9_:]{1,160}",value):raise ValueError("Invalid bounded Rust test filter")
    return value

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
        cap=compiler_cap(job.get("compiler_memory_cap"))if phase in {"compile_test","compile_release","compile_linked"}and kind=="engine"else None
        if cap is not None and not self.resources.enabled:raise ValueError("Compiler cap requires enforced resource admission")
        admission_started=time.monotonic()
        self.resources.enter(name,job,phase,profile,kind,timeout=timeout,hard_cap=cap)
        timeout=max(1,timeout-(time.monotonic()-admission_started));identity=None;success=False;diagnostic_complete=False
        parent="webagent-build-"+self.config["instance"]+".slice"
        create=["docker","create","--name",name,"--cgroup-parent",parent,"--label","webagent.scheduler.instance="+self.config["instance"],"--label","webagent.scheduler.job="+jobhex,"--label","webagent.scheduler.phase="+phase,
            "--cap-drop","ALL","--security-opt","no-new-privileges","--pids-limit","256","--user",str(self.config["service_uid"])+":"+str(self.config["service_gid"]),
            "--network","bridge"if network else"none","--mount","type=bind,src="+str(source)+",dst=/build"+(""if phase=="test"else",readonly"),
            "--mount","type=bind,src="+str(output)+",dst=/output","-w","/build",
            "-e","CARGO_HOME=/output/cargo","-e","CARGO_TARGET_DIR=/output/target","-e","CARGO_BUILD_JOBS=1","-e","CARGO_INCREMENTAL=0",
            "-e","CARGO_PROFILE_DEV_DEBUG=0","-e","CARGO_PROFILE_TEST_DEBUG=0","-e","CARGO_PROFILE_RELEASE_DEBUG=0",RUST,"sleep","infinity"]
        if cap is not None:create[create.index(RUST):create.index(RUST)]=["--memory",str(cap),"--memory-swap",str(cap)]
        try:
            self.runner.command(create,timeout=30,private_raw=True)
            # Only the protected helper binds exact name, labels and cgroup parent;
            # Docker's merged diagnostic stream is not a machine identity channel.
            created=self.runner.resource_request({"operation":"identity","container":name})
            candidate=created.get("container_id")
            if not isinstance(candidate,str)or not re.fullmatch(r"[a-f0-9]{64}",candidate)or created.get("state")!="created"or created.get("phase")!=phase:raise ValueError("Compiler container identity invalid")
            identity=candidate
            with self.resources.lock:self.resources.rows[name]["container_id"]=identity
            self.runner.command(["docker","start",name],timeout=30)
            self.resources.attached(name,identity)
            self.runner.job_context.phase_container=name
            if cap is not None:
                observed=self.runner.resource_request({"operation":"stats","container":name})
                if observed.get("memory_max")!=cap or observed.get("swap_max")!=0:raise ValueError("Compiler memory cap was not enforced by the kernel")
                job.setdefault("compiler_cap_outcomes",[]).append({"phase":phase,"recipe":profile,"requested_bytes":cap,"observed_memory_max":observed["memory_max"],"observed_swap_max":observed["swap_max"],"container_id":identity,"outcome":"running"})
                getattr(self.runner,"resource_progress",lambda _:None)(job)
            # Docker wait reports exit status as data. Logs and counters remain available afterward.
            logs=self.runner.command(["docker","exec",name,*argv],timeout=timeout)
            self.persist_phase_log(job,logs)
            success=True;return logs
        except RuntimeError as error:
            diagnostic_complete=(cap is None and argv[:2]==["cargo","check"] and getattr(error,"exit_code",None)==1 and self.resources.diagnostic_check(str(error)))
            raise
        finally:
            self.runner.job_context.phase_container=None
            oom=False
            if cap is not None and identity is not None:
                observed=self.runner.resource_request({"operation":"stats","container":name})
                oom=observed.get("oom_kills",0)>0
                if job.get("compiler_cap_outcomes"):
                    result=job["compiler_cap_outcomes"][-1];result.update(oom_events=observed.get("oom_events",0),oom_kills=observed.get("oom_kills",0),peak_memory=observed.get("peak_memory",0),outcome="oom"if oom else("completed"if success else"failed"))
                    getattr(self.runner,"resource_progress",lambda _:None)(job)
            self.finish_phase(name,identity,success and not oom,diagnostic_complete and not oom)
            if oom:raise RuntimeError("Compiler memory cap exceeded (OOM); check failed")
    # @code-index {"version":1,"feature":"ram-packaging","aliases":["RAM build scratch","tmpfs packaging"],"purpose":"Fixed RAM BuildKit workspace and proof. Config: build-service/scheduler-defaults.json -> package_ram_bytes, package_tmpfs_bytes, reserve_percent.","span_lines":65,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/package_ram_bytes","/package_tmpfs_bytes","/reserve_percent"]}],"opportunities":[]}}
    @contextlib.contextmanager
    def packager(self,job,role="generic"):
        """Bound only this dedicated BuildKit ancestor, preserving reserve and measured role profiles."""
        name="webagent-build-"+self.config["instance"]+"-"+uuid.UUID(job["job_id"]).hex+"-package-"+uuid.uuid4().hex[:8]
        if role not in {"api","loop","generic"}:raise ValueError("Fixed packaging role required")
        cap=compiler_cap(job.get("package_memory_cap"));identity=None;success=False;oom=False;proof_error=None;primary=False;stage="container_create";cap_outcome=None
        policy=self.config.get("scheduler",{});scratch=policy.get("package_tmpfs_bytes",0)
        # Installation policy alone selects this disposable RAM workspace. Model requests cannot add mounts.
        if scratch:cap=policy["package_ram_bytes"]
        if cap is not None and not self.resources.enabled:raise ValueError("Package cap requires enforced admission")
        self.resources.enter(name,job,"package","buildkit:v0.25.1:role-context-v2:"+role+(":ram-v1"if scratch else""),"package",timeout=600,hard_cap=cap)
        try:
            create=["docker","create","--name",name,"--privileged","--cgroup-parent","webagent-build-"+self.config["instance"]+".slice",
                "--label","webagent.scheduler.instance="+self.config["instance"],"--label","webagent.scheduler.job="+uuid.UUID(job["job_id"]).hex,"--label","webagent.scheduler.phase=package","--pids-limit","512"]
            if cap is not None:create +=["--memory",str(cap),"--memory-swap",str(cap)]
            if scratch:create +=["--tmpfs","/var/lib/buildkit:rw,exec,nosuid,nodev,size="+str(scratch)+",mode=0700"]
            create +=[BUILDKIT,"--addr","unix:///run/buildkit/buildkitd.sock"]
            self.runner.command(create,timeout=30,private_raw=True)
            # Docker diagnostics share the command stream; only the protected helper identifies this exact container.
            stage="container_identity";created=self.runner.resource_request({"operation":"identity","container":name})
            candidate=created.get("container_id")
            if not isinstance(candidate,str)or not re.fullmatch(r"[a-f0-9]{64}",candidate)or created.get("state")!="created"or created.get("phase")!="package":raise ValueError("Packaging container identity invalid")
            identity=candidate
            with self.resources.lock:self.resources.rows[name]["container_id"]=identity
            stage="container_start";self.runner.command(["docker","start",name],timeout=30);self.resources.attached(name,identity)
            stage="initial_attestation"
            if cap is not None:
                observed=self.runner.resource_request({"operation":"stats","container":name})
                if observed.get("container_id")!=identity or observed.get("memory_max")!=cap or observed.get("swap_max")!=0 or (observed.get("oom_kills",0)>0 or observed.get("oom_events",0)>0):raise ValueError("Dedicated BuildKit ancestor memory cap not enforced")
                cap_outcome={"role":role,"requested_bytes":cap,"observed_memory_max":observed["memory_max"],"observed_swap_max":observed["swap_max"],"container_id":identity,"initial_oom_events":observed.get("oom_events",0),"initial_oom_kills":observed.get("oom_kills",0),"outcome":"running"}
                job.setdefault("package_cap_outcomes",[]).append(cap_outcome)
                getattr(self.runner,"resource_progress",lambda _:None)(job)
            if scratch:
                # Verify the actual mount before BuildKit can unpack any candidate layers.
                actual=self.runner.command(["docker","exec",name,"stat","-f","-c","%T","/var/lib/buildkit"],timeout=10).strip()
                if actual!="tmpfs":raise ValueError("RAM packaging scratch mount not enforced")
                cap_outcome["scratch_bytes"]=scratch
            stage="builder_create"
            self.runner.command(["docker","buildx","create","--name",name,"--driver","remote","docker-container://"+name],timeout=30)
            stage="builder_bootstrap";self.runner.command(["docker","buildx","inspect",name,"--bootstrap"],timeout=60)
            stage="workload";self.runner.job_context.phase_container=name;yield name;success=True
        except BaseException as error:
            primary=True;job.setdefault("package_failures",[]).append({"role":role,"stage":stage,"exception_type":type(error).__name__})
            getattr(self.runner,"resource_progress",lambda _:None)(job);raise
        finally:
            self.runner.job_context.phase_container=None
            try:
                if cap is not None and identity is not None:
                    try:
                        observed=self.runner.resource_request({"operation":"stats","container":name});oom=observed.get("oom_kills",0)>0 or observed.get("oom_events",0)>0
                        if observed.get("container_id")!=identity or observed.get("memory_max")!=cap or observed.get("swap_max")!=0:proof_error=ValueError("BuildKit ancestor cap changed during packaging")
                    except (OSError,ValueError,RuntimeError,TimeoutError,PermissionError):
                        observed={};success=False;proof_error=ValueError("BuildKit cap completion evidence unavailable")
                        job.setdefault("package_failures",[]).append({"role":role,"stage":"completion_attestation","exception_type":"EvidenceUnavailable"})
                        getattr(self.runner,"resource_progress",lambda _:None)(job)
                    if cap_outcome is not None:
                        cap_outcome.update(oom_events=observed.get("oom_events"),oom_kills=observed.get("oom_kills"),peak_memory=observed.get("peak_memory"),outcome="oom"if oom else("completed"if success and proof_error is None else"failed"))
                        getattr(self.runner,"resource_progress",lambda _:None)(job)
            finally:
                try:
                    environment={"PATH":"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin","LANG":"C.UTF-8","HOME":self.config.get("service_home",os.path.expanduser("~"))}
                    subprocess.run(["docker","buildx","rm",name],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=30,env=environment)
                finally:self.finish_phase(name,identity,success and not oom and proof_error is None)
            if oom:raise RuntimeError("Package memory cap exceeded (OOM); publication failed")
            if proof_error is not None and not primary:raise proof_error
    def finish_phase(self,name,identity,success,diagnostic_complete=False):
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
        self.resources.finish(name,success,diagnostic_complete=diagnostic_complete)

    def check(self,job,blob,task,timeout,extract):
        """Use the same attributable recipes for bounded foreground checks and release compilation."""
        job["artifact_component"]="engine"
        root=Path(self.config["state_root"])/"checks"/job["job_id"];source=root/"source";output=root/"output"
        blob=self.runner.execution_application_blob(blob);root.mkdir(parents=True,exist_ok=True);extract(source,blob);output.mkdir(exist_ok=True)
        application=source/"v3";before=self.runner.execution_source_digest(source);start=time.monotonic()
        log=self.phase(job,"dependency","dependency",application,output,["cargo","fetch","--locked"],"engine:rust1.90:dependency",min(timeout,600),network=True)
        remaining=max(1,timeout-int(time.monotonic()-start))
        argv=["cargo","check","--locked","--offline"]if task=="check"else["cargo","test","--locked","--offline","--no-run"]
        log+=self.phase(job,"compile_test","engine",application,output,argv,"engine:rust1.90:"+("check"if task=="check"else"compile_test"),remaining)
        if task=="test":
            selected=test_filter(job.get("test_filter"))
            runtime=["cargo","test","--locked","--offline"]+([selected]if selected is not None else[])+["--","--test-threads=1"]+(["--nocapture"]if selected is not None else[])
            log+=self.phase(job,"test","test",application,output,runtime,"engine:rust1.90:test",max(1,timeout-int(time.monotonic()-start)))
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
            if name in {"v3/Cargo.toml","v3/Cargo.lock","v3/build.rs"}or(name.startswith("v3/crates/")or name.startswith("v3/src/")and(name.endswith(".rs")or name.startswith("v3/src/plugin_packages/")))}
    def cached_engine(self,blob,output):
        """Reuse only artifacts whose pinned recipe and complete compiled inputs still match the candidate."""
        cache=Path(self.config["state_root"])/"engine-artifacts";cache.mkdir(mode=0o700,exist_ok=True)
        files=self.runner.execution_entries(blob);rust=self.rust_inputs(files)
        for path in sorted(cache.glob("*.json"),key=lambda p:p.stat().st_mtime,reverse=True)[:64]:
            if path.is_symlink():raise ValueError("Engine cache receipt symlink")
            value=json.loads(path.read_text())
            if value["recipe"]!=RUST+":single-job:debug0:complete-tests-v5"or value["rust_inputs"]!=rust:continue
            if any(name not in files or hashlib.sha256(files[name][1]).hexdigest()!=sha for name,sha in value["compiled_inputs"].items()):continue
            if not re.fullmatch(r"[a-f0-9-]{36}",value["job_id"]):raise ValueError("Engine cache job identity invalid")
            source=Path(self.config["state_root"])/"execution"/value["job_id"]/"output/artifacts"
            if self.artifact_hashes(source)!=value["artifacts"]:raise ValueError("Cached compiler output changed")
            self.test_manifest(source)
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
    @staticmethod
    def smoke_recipe(mode):
        """Write only fixed mode-proof artifacts; structured JSON never enters public log scrubbing."""
        if mode=="linked":return ["sh","-c","set -e\ntest ! -L /output/stable/linked-smoke-result.json\n/output/stable/webagent-v3 --context-smoke > /output/stable/linked-smoke-result.json"]
        if mode=="live":return ["sh","-c","set -e\ntest ! -L /output/stable/live-smoke-result.json\n/output/target/release/webagent-v3 --context-smoke > /output/stable/live-smoke-result.json"]
        raise ValueError("Fixed context smoke mode required")
    @staticmethod
    def smoke_artifact(path):
        """Read a bounded single-link regular proof file without redacting its typed JSON values."""
        try:fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
        except OSError:raise ValueError("Context smoke artifact identity invalid")from None
        with os.fdopen(fd,"rb")as stream:
            info=os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode)or info.st_nlink!=1 or info.st_size>65536:raise ValueError("Context smoke artifact identity or bound invalid")
            body=stream.read(65537)
            if len(body)>65536:raise ValueError("Context smoke artifact exceeds bound")
            return body.decode("utf-8")
    def persist_phase_log(self,job,logs):
        """Retain already-scrubbed successful phase output before later recipe validation can fail."""
        job["log"]=(job.get("log","")+logs).encode("utf-8")[-1024*1024:].decode("utf-8","ignore")
        getattr(self.runner,"resource_progress",lambda _:None)(job)

    @staticmethod
    def smoke_result(text,mode):
        """Validate both context and background planning proofs for the two optimized deterministic cases."""
        if len(text)>65536:raise ValueError("Context smoke result exceeds bound")
        value=json.loads(text)
        if value.get("mode")!=mode or value.get("equivalence")is not True or value.get("interface_version")!=1 or not isinstance(value.get("prepared"),list)or len(value["prepared"])!=2:raise ValueError("Optimized context smoke failed")
        if not isinstance(value.get("background_prepared"),list)or len(value["background_prepared"])!=2:raise ValueError("Optimized background smoke failed")
        if not isinstance(value.get("boot_prepared"),list)or len(value["boot_prepared"])!=2:raise ValueError("Optimized boot smoke failed")
        if mode=="linked"and value.get("service_started")is not False:raise ValueError("Linked smoke started external service")
        if mode=="live"and(value.get("service_reused")is not True or value.get("service_drained")is not True):raise ValueError("Live smoke lifecycle failed")
        return value
    @staticmethod
    def linked_smoke(text):
        """Require optimized linked context/planner equivalence before retaining the stable executable."""
        return Executor.smoke_result(text,"linked")
    @staticmethod
    def live_smoke(text,linked):
        """Require actual host service reuse/drain and exact prepared-result equivalence across modes."""
        value=Executor.smoke_result(text,"live")
        if value["prepared"]!=linked["prepared"]or value["background_prepared"]!=linked["background_prepared"]or value["boot_prepared"]!=linked["boot_prepared"]:raise ValueError("Optimized linked/live results differ")
        return value
    @staticmethod
    def linked_recipe():
        """Keep linked outputs and dep-info in their fixed separate compiler target directory."""
        return ["cargo","build","--release","--locked","--offline","--bin","webagent-v3","--target-dir","/output/linked-target"]
    def linked_dependencies(self,target,files):
        """Attest linked implementation inputs separately from the deployed live dependency closure."""
        captured={}
        for name in self.compiler_dependencies(target):
            relative=self.local_dependency(name)
            if relative is None:continue
            if relative not in files:raise ValueError("Linked compiler input absent from captured source")
            captured[relative]=hashlib.sha256(files[relative][1]).hexdigest()
        if "v3/crates/context-assembly/src/lib.rs"not in captured:raise ValueError("Linked implementation dependency attestation missing")
        return captured

    @staticmethod
    def metadata_receipt(path):
        """Read bounded compiler metadata from its output artifact rather than truncated public logs."""
        if path.is_symlink()or not path.is_file()or path.stat().st_nlink!=1 or path.stat().st_size>16*1024*1024:raise ValueError("Compiler metadata artifact identity")
        return json.loads(path.read_bytes())

    @staticmethod
    def core_dependencies(metadata,target):
        """Attest only the live root's resolved local dependency closure, excluding service-only crates."""
        packages={p["id"]:p for p in metadata["packages"]};nodes={n["id"]:n for n in metadata["resolve"]["nodes"]}
        roots=[p["id"]for p in metadata["packages"]if p["name"]=="webagent-v3"]
        if len(roots)!=1:raise ValueError("Main package metadata missing")
        pending=roots[:];seen=set();dependencies=set()
        while pending:
            identity=pending.pop()
            if identity in seen:continue
            seen.add(identity);package=packages[identity]
            pending.extend(d["pkg"]for d in nodes[identity].get("deps",[])if any(k["kind"]in (None,"normal","build")for k in d.get("dep_kinds",[])))
            if package.get("source")is not None:continue
            for item in package["targets"]:
                if not any(kind in {"bin","lib","rlib","proc-macro"}for kind in item["kind"]):continue
                for path in(target/"release/deps").glob(item["name"].replace("-","_")+"-*.d"):
                    for line in path.read_text().replace(chr(92)+chr(10)," ").splitlines():
                        if not line.lstrip().startswith("#")and ":"in line:dependencies.update(shlex.split(line.split(":",1)[1]))
        if not any(name in {"src/main.rs","/build/src/main.rs"}for name in dependencies):raise ValueError("Live root dependency attestation missing")
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
        value={"recipe":RUST+":single-job:debug0:complete-tests-v5","rust_inputs":self.rust_inputs(files),"compiled_inputs":inputs,
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
    def retain_test_manifest(self,output,artifacts):
        """Retain Cargo-attested tests and exact debug support; never substitute optimized binaries."""
        metadata=output/"test-artifacts.json"
        if metadata.is_symlink()or not metadata.is_file()or metadata.stat().st_size>32*1024*1024:raise ValueError("Cargo test metadata identity")
        tests=[];support=None;additional_support=[];folder=artifacts/"tests";folder.mkdir()
        for line in metadata.read_text().splitlines():
            if len(line)>1024*1024:raise ValueError("Cargo test metadata line bound")
            value=json.loads(line)
            if value.get("reason")!="compiler-artifact"or not value.get("executable"):continue
            target=value["target"];executable=value["executable"]
            test=value.get("profile",{}).get("test")is True
            official=not test and (target.get("name"),target.get("src_path"))in {("webagent-context-service","/build/crates/context-service/src/main.rs"),("webagent-background-service","/build/crates/background-service/src/main.rs")}
            if not test and not official:continue
            expected="/output/target/debug/"
            if not executable.startswith(expected)or ".."in Path(executable).parts:raise ValueError("Cargo test executable escaped fixed target")
            source=output/"target/debug"/executable.removeprefix(expected)
            if source.is_symlink()or not source.is_file():raise ValueError("Cargo test executable identity")
            with source.open("rb")as stream:
                if stream.read(4)!=b"\x7fELF":raise ValueError("Cargo test executable identity")
            if test:
                name=source.name
                if not re.fullmatch(r"[a-z0-9_]+-[a-f0-9]{16}",name):raise ValueError("Cargo test executable name")
                destination=folder/name;record={"file":"tests/"+name,"package_id":value["package_id"],"target_name":target["name"],"source_path":target["src_path"],"kind":target["kind"]};tests.append(record)
            else:
                binary=target["name"];destination=artifacts/"test-support/debug"/binary;destination.parent.mkdir(parents=True,exist_ok=True)
                record={"file":"test-support/debug/"+binary,"destination":"target/debug/"+binary,"package_id":value["package_id"],"source_path":target["src_path"],"profile":"debug"}
                if binary=="webagent-context-service":support=record
                else:additional_support.append(record)
            shutil.copyfile(source,destination);destination.chmod(0o555);record["sha256"]=self.artifact_hashes(destination.parent)[destination.name]
        if not support or not any(item["target_name"]=="service"and item["kind"]==["test"]and item["source_path"]=="/build/crates/context-service/tests/service.rs"and item["package_id"]==support["package_id"]for item in tests):raise ValueError("Official service integration/support provenance missing")
        manifest={"schema_version":1,"tests":sorted(tests,key=lambda item:item["file"]),"support":support,"additional_support":additional_support}
        (artifacts/"test-manifest.json").write_text(json.dumps(manifest,sort_keys=True)+"\n")
        return manifest
    def test_manifest(self,artifacts):
        """Reject incomplete, changed or provenance-mismatched retained test closures before replay."""
        value=self.metadata_receipt(artifacts/"test-manifest.json")
        if value.get("schema_version")!=1 or not value.get("tests")or not isinstance(value.get("support"),dict):raise ValueError("Retained test manifest incomplete")
        support=value["support"]
        if support.get("file")!="test-support/debug/webagent-context-service"or support.get("destination")!="target/debug/webagent-context-service"or support.get("profile")!="debug"or support.get("source_path")!="/build/crates/context-service/src/main.rs":raise ValueError("Retained support provenance invalid")
        if not any(item.get("target_name")=="service"and item.get("kind")==["test"]and item.get("source_path")=="/build/crates/context-service/tests/service.rs"and item.get("package_id")==support.get("package_id")for item in value["tests"]):raise ValueError("Retained integration provenance missing")
        extra=value.get("additional_support",[])
        integration=[item for item in value["tests"]if item.get("source_path")=="/build/crates/background-service/tests/service.rs"]
        if len(extra)>1 or bool(integration)!=bool(extra):raise ValueError("Background support inventory incomplete")
        for item in extra:
            if item.get("file")!="test-support/debug/webagent-background-service"or item.get("destination")!="target/debug/webagent-background-service"or item.get("profile")!="debug"or item.get("source_path")!="/build/crates/background-service/src/main.rs"or not any(test.get("package_id")==item.get("package_id")for test in integration):raise ValueError("Background support provenance invalid")
        support_files={item["file"]for item in [support,*extra]}
        names=[]
        for item in [*value["tests"],support,*extra]:
            name=item.get("file","")
            if name in names or not(name in support_files or re.fullmatch(r"tests/[a-z0-9_]+-[a-f0-9]{16}",name)):raise ValueError("Retained test path invalid")
            names.append(name);path=artifacts/name
            if path.is_symlink()or not path.is_file()or path.stat().st_nlink!=1 or self.artifact_hashes(path.parent)[path.name]!=item.get("sha256"):raise ValueError("Retained test output changed")
        if {p.relative_to(artifacts).as_posix()for p in (artifacts/"tests").iterdir()}!={item["file"]for item in value["tests"]}:raise ValueError("Retained test manifest inventory mismatch")
        return value
    @staticmethod
    def restore_test_support(output,artifacts,manifest):
        """Restore only attested debug support at Cargo's fixed integration-test executable path."""
        debug=output/"target/debug";debug.mkdir(parents=True,exist_ok=True)
        for item in [manifest["support"],*manifest.get("additional_support",[])]:
            target=output/item["destination"];shutil.copyfile(artifacts/item["file"],target);target.chmod(0o555)
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
            manifest=self.test_manifest(artifacts)
            self.restore_test_support(output,artifacts,manifest)
            for binary in sorted((artifacts/"tests").iterdir()):
                if not re.fullmatch(r"[a-z0-9_]+-[a-f0-9]{16}",binary.name):raise ValueError("Cached test executable name invalid")
                logs+=self.phase(job,"test","test",application,output,["/output/artifacts/tests/"+binary.name,"--test-threads=1"],"engine:rust1.90:test")
            if self.runner.execution_source_digest(source)!=self.runner.execution_source_digest_blob(blob):
                raise ValueError("Cached tests changed candidate source")
            logs=logs.replace("locked Rust tests previously passed on identical compiled inputs.","current candidate ran retained tests from identical compiled inputs.")
            receipt={"inputs_sha256":digest(blob),"artifacts":old["artifacts"],"log":logs,"reused_from":old["job_id"]}
            temporary=root/"verified.new";temporary.write_text(json.dumps(receipt));os.replace(temporary,root/"verified.json")
            if not (artifacts/"stable/smoke.json").is_file():raise ValueError("Retained optimized linked smoke proof missing")
            job["linked_stable"]=json.loads((artifacts/"stable/smoke.json").read_text())
            job["optimized_mode_equivalence"]=json.loads((artifacts/"stable/mode-equivalence.json").read_text())
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
        logs+=self.phase(job,"compile_test","engine",application,output,["sh","-c","cargo test --workspace --locked --no-run --message-format=json-render-diagnostics > /output/test-artifacts.json"],"engine:rust1.90:compile_test")
        logs+=self.phase(job,"test","test",application,output,["cargo","test","--workspace","--locked","--","--test-threads=1"],"engine:rust1.90:test")
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Tests left changed application inputs")
        logs+=self.phase(job,"compile_linked","engine",application,output,self.linked_recipe(),"engine:rust1.90:linked-release")
        stable=output/"stable";stable.mkdir(exist_ok=True)
        shutil.copyfile(output/"linked-target/release/webagent-v3",stable/"webagent-v3");(stable/"webagent-v3").chmod(0o555)
        logs+=self.phase(job,"linked_smoke","test",application,output,self.smoke_recipe("linked"),"engine:rust1.90:linked-smoke",60)
        proof=self.linked_smoke(self.smoke_artifact(stable/"linked-smoke-result.json"))
        linked_inputs=self.linked_dependencies(output/"linked-target",self.runner.execution_entries(blob))
        (stable/"dependencies.json").write_text(json.dumps(linked_inputs,sort_keys=True)+"\n")
        job["linked_stable"]={"mode":"linked","target_directory":"linked-target","compiled_inputs":linked_inputs,"artifact_sha256":hashlib.sha256((stable/"webagent-v3").read_bytes()).hexdigest(),"smoke":proof,"retained_artifact":"stable/webagent-v3","deployed":False}
        (stable/"smoke.json").write_text(json.dumps(job["linked_stable"],sort_keys=True)+"\n")
        getattr(self.runner,"resource_progress",lambda _:None)(job)
        logs+=self.phase(job,"compile_release","engine",application,output,["cargo","build","--workspace","--release","--locked","--offline","--no-default-features","--features","webagent-v3/live-context"],"engine:rust1.90:release")
        logs+=self.phase(job,"live_smoke","test",application,output,self.smoke_recipe("live"),"engine:rust1.90:live-smoke",90)
        live_proof=self.live_smoke(self.smoke_artifact(stable/"live-smoke-result.json"),proof)
        job["optimized_mode_equivalence"]={"status":"passed","linked":proof,"live":live_proof}
        (stable/"mode-equivalence.json").write_text(json.dumps(job["optimized_mode_equivalence"],sort_keys=True)+"\n")
        getattr(self.runner,"resource_progress",lambda _:None)(job)
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Compilation changed application inputs")
        artifacts=output/"artifacts";artifacts.mkdir(exist_ok=True);hashes={}
        for name in BINARIES:
            source_binary=output/"target/release"/name
            destination=artifacts/name;shutil.copyfile(source_binary,destination);destination.chmod(0o555)
            hashes[name]=hashlib.sha256(destination.read_bytes()).hexdigest()
        shutil.copytree(stable,artifacts/"stable")
        self.retain_test_manifest(output,artifacts)
        # Preserve packaged module descriptors and the offline optimization registry.
        for package,binary in [("automation","webagent-automation"),("outbound_notifications","webagent-outbound-notifications")]:
            folder=artifacts/"modules"/package;folder.mkdir(parents=True)
            shutil.copyfile(artifacts/binary,folder/binary)
            for path in(application/"src/plugin_packages"/package).iterdir():
                if path.name in {"package.json","tool-schemas.json"}or path.name.startswith("page."):shutil.copyfile(path,folder/path.name)
        registry=artifacts/"optimization-cargo/registry";registry.parent.mkdir()
        shutil.copytree(cargo/"registry",registry)
        dependencies=self.compiler_dependencies(output/"target")
        self.phase(job,"engine_metadata","dependency",application,output,["sh","-c","cargo metadata --locked --offline --format-version 1 --no-default-features --features webagent-v3/live-context > /output/core-metadata.json"],"engine:rust1.90:metadata")
        metadata=self.metadata_receipt(output/"core-metadata.json")
        core=self.core_dependencies(metadata,output/"target")
        (artifacts/"core-inputs.txt").write_text("\n".join(sorted(core))+"\n")
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
