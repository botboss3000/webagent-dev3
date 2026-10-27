"""Attributable compilation/testing and artifact-fed packaging in the pinned toolchain."""
import contextlib,hashlib,io,json,os,posixpath,re,shlex,shutil,stat,subprocess,tarfile,threading,time,types,uuid,tomllib
from pathlib import Path
FEATURE_PROFILES={"default":(),"live":("--no-default-features","--features","live-context")}
def check_profile(value=None):
    """Only fixed feature sets may select a compiler recipe; arbitrary Cargo arguments are forbidden."""
    if value is None:return "default"
    if not isinstance(value,str)or value not in FEATURE_PROFILES:raise ValueError("Unsupported compile-check feature profile")
    return value

def check_argv(task,profile=None):
    """Preserve default check/test commands and expose live compilation as a standalone check."""
    selected=check_profile(profile)
    if task not in {"check","test"}or(selected!="default"and task!="check"):
        raise ValueError("Feature profiles apply only to compile checks")
    return (["cargo","check","--locked","--offline",*FEATURE_PROFILES[selected]]if task=="check"else["cargo","test","--locked","--offline","--no-run"])

RUST="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
COMPILER_RECIPE=RUST+":jobs3:debug0:mold1.10.1:complete-tests-v8"
RUSTFLAGS="-C debuginfo=0 -C link-arg=-fuse-ld=mold"
# Rust test harness threads need explicit stack headroom: an overflow aborts the
# whole binary (853 tests) and costs a retry, while the extra stack is virtual.
TEST_STACK_BYTES=32*1024*1024
# Suite and retained-binary replays run the whole workspace harness; parallel
# test threads must stay a pinned recipe value so test_proof stays deterministic.
# Single named tests (focused checks) keep one thread: parallelism is meaningless
# there. Evidence: identical pass/fail/ignored counts at 1 vs 4 threads.
TEST_THREADS=8
# Compiled-dependency snapshots: stub compiles mirror the engine profiles exactly.
SNAPSHOT_RECIPE="compiled-dependencies-v2"
SNAPSHOT_COMPILES="set -e\ncargo test --workspace --locked --offline --no-run\ncargo build --workspace --locked --offline --bins\ncargo build --workspace --release --locked --offline --no-default-features --features webagent-v3/live-context"
SNAPSHOT_KEEP=3
SNAPSHOT_HEADROOM=16*1024**3
# Workspace snapshots: module crate objects reused across builds. Unchanged sources
# get FROZEN_MTIME so Cargo's mtime freshness keeps their objects; changed or new
# files keep their extraction time and rebuild with every dependent.
WORKSPACE_RECIPE="compiled-workspace-v1"
WORKSPACE_KEEP=2
FROZEN_MTIME=946684800
STABLE_FEATURE="stable-abilities"  # root feature a stable release adds to link ability crates
MOLD_VERSION="1.10.1+dfsg-1"
MOLD_SHA256="f85b0102cabeaee34b76ab195aacfa9ff9442d670948c88f6f353dd666ea5b64"
MOLD_COMPILER_DOCKERFILE_SHA256="129a45c426d4683ab5a563ebdf764bf829826a415305dce15ab36c780a3ba815"
MOLD_COMPILER_STAGE_SHA256="046c3f98cc75313c2dd6fee391328f5beea8dd537745429542e7c4710bd7e635"
BUILDKIT="moby/buildkit:v0.25.1@sha256:79cc6476ab1a3371c9afd8b44e7c55610057c43e18d9b39b68e2b0c2475cc1b6"
COMPILER_STAGE_SHA256="474b034d7a843ebf324b654a785b5a5b23135804ef39c662f92fcb3e160dfeaf"
# Coordinated Automation extraction removes only its root-bin distribution closure.
EXTRACTED_AUTOMATION_COMPILER_STAGE_SHA256="9a6fa4db4b7a122990056f005117a0c4e0944243e02a2ee42b9957ec12f65908"
CONTEXT_COMPILER_STAGE_SHA256="04d76d4efb9fe52ecc1f7ef778aef9a0c096fea97b22011e628d6891bf2fa072"
CONTEXT_EXTRACTED_AUTOMATION_COMPILER_STAGE_SHA256="a47b7c8928469f1d9dc535cf9d1df9c6b256052c84f10bd238f36a3b9541597d"
SHARED_RELEASE_CONTEXT_COMPILER_STAGE_SHA256="1b281eed8db8d8ee5fe71e5efab39e1684fe1256f9ad6e12fdeb179d472403cc"
PARALLEL_COMPILER_STAGE_SHA256="0cff9cfb6139da95442c7199d49867a88f6becdeba5eaf81e84376792011b020"
NEW_CONTEXT_BINARIES={"webagent-assets-service","webagent-capability-service","webagent-provider-service"}
BINARIES=["webagent-v3","webagent-mcp","webagent-diagnostics","webagent-optimization","webagent-outbound-notifications","webagent-context-service","webagent-background-service","webagent-boot-service","webagent-boot-host","webagent-assets-service","webagent-capability-service","webagent-provider-service"]
# Official non-test debug ELFs needed by actual subprocess integration tests.
SERVICE_SOURCES={
 "webagent-context-service":"/build/crates/context-service/src/main.rs",
 "webagent-background-service":"/build/crates/background-service/src/main.rs",
 "webagent-boot-service":"/build/crates/boot-service/src/main.rs",
 "webagent-assets-service":"/build/crates/assets-service/src/main.rs",
 "webagent-capability-service":"/build/crates/capability-service/src/main.rs",
 "webagent-provider-service":"/build/crates/provider-service/src/main.rs",
}
SMOKE_CASE_KEYS=("prepared","background_prepared","boot_prepared","assets_prepared","capability_prepared","provider_prepared")
# @code-index {"version":1,"feature":"build-artifact-execution","aliases":["task fencing","verified builds"],"purpose":"Pinned attributable compiler/test/packaging recipes and verified content-addressed artifact reuse.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Identity, exact input verification and completion rules cannot be disabled by model or preference configuration."}}
COMPILER_MEMORY_CAP=4026531840
# @code-index {"version":1,"feature":"compiler-memory-policy","aliases":["compiler memory defaults","task memory overrides","live memory growth"],"purpose":"Protected compiler ceilings and live growth. Config: build-service/scheduler-defaults.json -> compiler_memory_default_bytes, compiler_memory_min_bytes, compiler_memory_max_bytes, allow_live_memory_increase; installation JSON overrides.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/compiler_memory_default_bytes","/compiler_memory_min_bytes","/compiler_memory_max_bytes","/allow_live_memory_increase"]}]}}
def compiler_cap(value,policy=None):
    """Validate a pinned compiler ceiling; protected bounds apply when admitting a new request."""
    if value is None:return None
    policy=policy or{};minimum=policy.get("compiler_memory_min_bytes",1024**3);maximum=policy.get("compiler_memory_max_bytes",16*1024**3)
    if type(value)is not int or not minimum<=value<=maximum or value%os.sysconf("SC_PAGE_SIZE"):raise ValueError("Unsupported compiler memory cap")
    return value

def package_cap(value):
    """Preserve the separate legacy disk-packaging ceiling; RAM packaging remains policy-owned."""
    if value is not None and(type(value)is not int or value!=COMPILER_MEMORY_CAP):raise ValueError("Unsupported package memory cap")
    return value

def test_filter(value):
    """Allow a bounded Rust unit-name substring, never command options or arbitrary arguments."""
    if value is None:return None
    if not isinstance(value,str)or not re.fullmatch(r"[A-Za-z0-9_:]{1,160}",value):raise ValueError("Invalid bounded Rust test filter")
    return value

# Protected registrations; source paths select fixed assertions, never shell text.
HOST_FOCUSED_POLICY="registered-host-v2"
HOST_FOCUSED_REGISTRY=(
 {"id":"host-prompt-restoration","test":"provider::tests::configured_base_instructions_loads_and_recovers_from_file_changes",
  "paths":("src/engine/","crates/modules/engine/","src/models/provider/","crates/modules/models/","config/prompts.json","src/agent_loop/","src/context/","src/engine_components.rs","src/main.rs","Cargo.toml","Cargo.lock","build.rs","crates/context-contracts/","crates/context-assembly/","crates/context-service/","plugins/engine/context-assembly/")},
 {"id":"host-offline-descriptor-identity","test":"components::offline_smoke_uses_frozen_descriptor_versions",
  "package":"webagent-engine","target":"webagent_engine","target_kind":"lib","source_path":"/build/crates/modules/engine/src/lib.rs",
  "paths":("src/engine/","crates/modules/engine/","src/engine_components.rs","src/context/","src/agent_loop/","src/main.rs","Cargo.toml","Cargo.lock","build.rs","crates/context-contracts/","crates/context-assembly/","crates/context-service/","crates/engine-service/","crates/context-assets/","crates/assets-contracts/","crates/assets-service/","crates/capability-contracts/","crates/capability-context/","crates/capability-service/","crates/provider-contracts/","crates/provider-projection/","crates/provider-service/","crates/background-contracts/","crates/background-dispatch/","crates/background-service/","crates/boot-contracts/","crates/boot-supervisor/","crates/boot-service/","plugins/engine/")},
)
HOST_FOCUSED_POLICY_SHA256=hashlib.sha256(json.dumps({"version":HOST_FOCUSED_POLICY,"registry":HOST_FOCUSED_REGISTRY,"component_limit":3},sort_keys=True).encode()).hexdigest()

class Executor:
    """Fixed phase recipes; model arguments never become host commands or mount paths."""
    def __init__(self,runner,resources,config):
        """Bind fixed build recipes to the broker resource controller and reviewed installation."""
        self.runner=runner;self.resources=resources;self.config=config
    def compiler_recipe(self):
        """Bind every object/output certificate to the actual immutable linker image."""
        return COMPILER_RECIPE+":"+self.runner.compiler_runtime()["image"]
    def preflight_packaging(self,job,blob):
        """Pin all local execution images and verify OCI dependencies before compiler admission."""
        references={"compiler":self.runner.compiler_runtime(job)["image"],"toolchain":RUST,"packager":BUILDKIT}
        cache=getattr(self.runner,"package_cache",None)
        if cache is None or not cache.enabled:
            files=self.runner.execution_entries(blob)
            text=files["v3/Dockerfile"][1].decode()
            for number,ref in enumerate(re.findall(r"^FROM ([^ \n]+)",text,re.M)):
                if "@sha256:" not in ref:raise ValueError("Pinned packaging base required: "+ref)
                references["runtime-"+str(number)]=ref
        retained={}
        for role,reference in references.items():
            try:
                result=self.runner.retain_image(reference,job["job_id"])
                if getattr(self.runner,"retention_enabled",False):
                    if not isinstance(result,dict) or result.get("retained") is not True:raise ValueError("Retention receipt unavailable")
                    actual=result.get("image")
                else:
                    actual=self.runner.command(["docker","image","inspect","--format","{{.Id}}",reference],timeout=15).strip()
                if not isinstance(actual,str) or not re.fullmatch(r"sha256:[a-f0-9]{64}",actual):raise ValueError("Image identity unavailable")
                retained[role]={"reference":reference,"image_id":actual}
            except (OSError,RuntimeError,ValueError,TimeoutError,PermissionError) as error:
                raise ValueError("Required "+role+" dependency missing or cannot be retained: "+reference+"; provision this pinned image before retry") from error
        if cache is not None and cache.enabled:
            try:job["dependency_bases"]=cache.ready(blob)
            except (KeyError,OSError,ValueError) as error:
                raise ValueError("Verified runtime dependency bases unavailable; run supported dependency preparation before compiling") from error
        job["packaging_preflight"]={"version":1,"source_sha256":hashlib.sha256(blob).hexdigest(),"images":retained,"verified_at":time.time()}
        getattr(self.runner,"resource_progress",lambda _:None)(job)
        return job["packaging_preflight"]

    @staticmethod
    def test_stack_env(phase):
        """Give Rust test-harness phases stack headroom; other phases keep the default."""
        return ["-e","RUST_MIN_STACK="+str(TEST_STACK_BYTES)]if phase=="test"else[]
    @staticmethod
    def cpu_weight_args(phase):
        """The live release compile runs beside the full suite inside the build slice's CPU
        quota; give it the lowest weight so timing-sensitive tests get CPU first and the
        compile uses only what they leave idle. Same work, so no added CPU-seconds."""
        return ["--cpu-shares","2"]if phase=="compile_release"else[]
    @staticmethod
    def suite_totals(output):
        """Sum one cargo test run's result counts; an empty run proves nothing executed."""
        totals=[0,0,0]
        for line in output.splitlines():
            match=re.search(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored",line)
            if match:totals=[current+int(value) for current,value in zip(totals,match.groups())]
        return totals
    def job_lock(self):
        """Serialize read-modify-write job updates with receipt saves while phases overlap."""
        return getattr(self.runner,"observation_lock",None)or contextlib.nullcontext()
    def concurrent_phase(self,job,*args,**kwargs):
        """Start one phase on a thread carrying this job's context; join with result()."""
        shared=getattr(self.runner,"job_context",None)
        context={key:value for key,value in vars(shared).items()if key not in {"phase_container","observation_scope","verification_id","last_observation"}}if shared is not None else {}
        outcome={}
        def run():
            for key,value in context.items():setattr(shared,key,value)
            try:outcome["value"]=self.phase(job,*args,**kwargs)
            except BaseException as error:outcome["error"]=error
        worker=threading.Thread(target=run,name="phase-"+args[0],daemon=True);worker.start()
        def result():
            worker.join()
            if "error"in outcome:raise outcome["error"]
            return outcome["value"]
        return types.SimpleNamespace(result=result)
    def phase(self,job,phase,kind,source,output,argv,profile,timeout=3600,network=False):
        """Expose bounded phase timing and current work without command text or private paths."""
        started=time.monotonic();token=uuid.uuid4().hex;outcome="failed"
        job["active_verification"]={"id":token,"phase":phase,"recipe":profile,"started_at":time.time()}
        if getattr(self.runner,"job_context",None)is not None:self.runner.job_context.verification_id=token
        # Every running phase is listed: overlapping phases share one live observation.
        with self.job_lock():job["active_phases"]=[a for a in job.get("active_phases",[])if a.get("id")!=token]+[{"id":token,"phase":phase,"recipe":profile,"started_at":time.time()}]
        getattr(self.runner,"resource_progress",lambda _:None)(job)
        try:
            result=self._phase_workload(job,phase,kind,source,output,argv,profile,timeout,network)
            outcome="completed";return result
        finally:
            with self.job_lock():
                job.setdefault("phase_timings",[]).append({"phase":phase,"recipe":profile,"seconds":round(time.monotonic()-started,3),"outcome":outcome})
                job["phase_timings"]=job["phase_timings"][-128:]
                job["active_phases"]=[a for a in job.get("active_phases",[])if a.get("id")!=token]
            if job.get("active_verification",{}).get("id")==token:job.pop("active_verification",None)
            getattr(self.runner,"resource_progress",lambda _:None)(job)

    def _phase_workload(self,job,phase,kind,source,output,argv,profile,timeout=3600,network=False):
        """Launch one identified container and preserve counters before removing a terminal container."""
        image=self.runner.compiler_runtime(job)["image"]
        jobhex=uuid.UUID(job["job_id"]).hex
        name="webagent-build-"+self.config["instance"]+"-"+jobhex+"-"+phase+"-"+uuid.uuid4().hex[:8]
        output.mkdir(parents=True,exist_ok=True)
        cap=compiler_cap(job.get("compiler_memory_cap"))if phase in {"compile_test","compile_release","compile_linked","plugin_compile","plugin_test"}and kind in {"engine","plugin"}else None
        if cap is not None and not self.resources.enabled:raise ValueError("Compiler cap requires enforced resource admission")
        admission_started=time.monotonic()
        self.resources.enter(name,job,phase,profile,kind,timeout=timeout,hard_cap=cap)
        admission_seconds=time.monotonic()-admission_started
        timeout=max(1,timeout-admission_seconds);identity=None;success=False;diagnostic_complete=False;primary=False
        parent="webagent-build-"+self.config["instance"]+".slice"
        create=["docker","create","--name",name,"--cgroup-parent",parent,"--label","webagent.scheduler.instance="+self.config["instance"],"--label","webagent.scheduler.job="+jobhex,"--label","webagent.scheduler.phase="+phase,
            "--cap-drop","ALL","--security-opt","no-new-privileges","--pids-limit","256","--user",str(self.config["service_uid"])+":"+str(self.config["service_gid"]),
            "--network","bridge"if network else"none","--mount","type=bind,src="+str(source)+",dst=/build"+(""if phase=="test"else",readonly"),
            "--mount","type=bind,src="+str(output)+",dst=/output","-w","/build",
            "-e","CARGO_HOME=/output/cargo","-e","CARGO_TARGET_DIR=/output/target","-e","CARGO_BUILD_JOBS=3","-e","CARGO_INCREMENTAL=0","--env","RUSTFLAGS="+RUSTFLAGS,
            "-e","CARGO_PROFILE_DEV_DEBUG=0","-e","CARGO_PROFILE_TEST_DEBUG=0","-e","CARGO_PROFILE_RELEASE_DEBUG=0",*self.test_stack_env(phase),*self.cpu_weight_args(phase),image,"sleep","infinity"]
        if cap is not None:create[create.index(image):create.index(image)]=["--memory",str(cap),"--memory-swap",str(cap)]
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
            if cap is not None:self.runner.resource_request({"operation":"seal","container":name,"expected_id":identity})
            self.resources.attached(name,identity)
            self.runner.job_context.phase_container=name
            if cap is not None:
                observed=self.runner.resource_request({"operation":"stats","container":name})
                if observed.get("container_id")!=identity or observed.get("memory_max")!=cap or observed.get("swap_max")!=0:raise ValueError("Compiler memory cap was not enforced by the kernel")
                job.setdefault("compiler_cap_outcomes",[]).append({"phase":phase,"recipe":profile,"requested_bytes":cap,"observed_memory_max":observed["memory_max"],"observed_swap_max":observed["swap_max"],"container_id":identity,"outcome":"running"})
                getattr(self.runner,"resource_progress",lambda _:None)(job)
            # Docker wait reports exit status as data. Logs and counters remain available afterward.
            self.runner.job_context.observation_scope={"phase":phase,"recipe":profile,"kind":kind,
                "target":job.get("test_filter")or("workspace"if "--workspace"in argv else phase),
                "verification_id":getattr(self.runner.job_context,"verification_id",None),"admission_seconds":round(admission_seconds,3)}
            self.runner.job_context.last_observation=None
            try:
                logs=self.runner.command(["docker","exec",name,*argv],timeout=timeout)
            finally:
                self.runner.job_context.observation_scope=None
                # This thread's own observation: a concurrent phase of the job may own the shared one.
                observed=getattr(self.runner.job_context,"last_observation",None)
                if observed:
                    with self.job_lock():
                        job.setdefault("execution_history",[]).append({k:v for k,v in observed.items()if k!="events"})
                        job["execution_history"]=job["execution_history"][-128:]
            self.persist_phase_log(job,logs)
            success=True;return logs
        except BaseException as error:
            primary=True
            diagnostic_complete=(isinstance(error,RuntimeError) and cap is None and argv[:2]==["cargo","check"] and getattr(error,"exit_code",None)==1 and self.resources.diagnostic_check(str(error)))
            raise
        finally:
            self.runner.job_context.phase_container=None
            oom=False;proof_error=None
            try:
                if cap is not None and identity is not None:
                    try:
                        observed=self.runner.resource_request({"operation":"stats","container":name})
                        oom=observed.get("oom_kills",0)>0 or observed.get("oom_events",0)>0
                        # Final kernel proof must match the durable reservation after any resize.
                        with self.resources.lock:
                            row=self.resources.rows[name];change=row.get("memory_change")
                            if change and change.get("state")in {"intent","uncertain"}:
                                self.resources.reconcile_memory(name,observed)
                            final_cap=row.get("hard_cap",cap)
                            if observed.get("container_id")!=identity or observed.get("memory_max")!=final_cap or observed.get("swap_max")!=0 or row.get("memory_change",{}).get("state")in {"intent","uncertain"}:
                                raise ValueError("Compiler memory completion proof changed or needs reconciliation")
                            timeline=list(row.get("memory_history",[]))
                    except (OSError,ValueError,RuntimeError,TimeoutError,PermissionError)as error:
                        proof_error=error;observed={};timeline=[];success=False
                    if job.get("compiler_cap_outcomes"):
                        result=job["compiler_cap_outcomes"][-1];result.update(final_memory_max=observed.get("memory_max"),memory_changes=timeline,oom_events=observed.get("oom_events"),oom_kills=observed.get("oom_kills"),peak_memory=observed.get("peak_memory"),outcome="oom"if oom else("completed"if success and proof_error is None else"failed"))
                        getattr(self.runner,"resource_progress",lambda _:None)(job)
            finally:
                try:
                    try:
                        measure=getattr(self.runner,"measure_disk_output",None)
                        if callable(measure)and success and not oom and proof_error is None:measure(name,output)
                    finally:self.finish_phase(name,identity,success and not oom and proof_error is None,diagnostic_complete and not oom and proof_error is None)
                except BaseException:
                    if not primary:raise
            if not primary:
                if oom:raise RuntimeError("Compiler memory cap exceeded (OOM); check failed")
                if proof_error is not None:raise RuntimeError("Compiler memory completion evidence unavailable or changed")from proof_error
    # @code-index {"version":1,"feature":"ram-packaging","aliases":["RAM build scratch","tmpfs packaging"],"purpose":"Fixed RAM BuildKit workspace and proof. Config: build-service/scheduler-defaults.json -> package_ram_bytes, package_tmpfs_bytes, reserve_percent.","span_lines":65,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/package_ram_bytes","/package_tmpfs_bytes","/reserve_percent"]}],"opportunities":[]}}
    @contextlib.contextmanager
    def packager(self,job,role="generic"):
        """Keep the cache volume exclusively owned until this BuildKit daemon is removed."""
        cache=getattr(self.runner,"package_cache",None)
        with (cache.claim(job) if cache is not None else contextlib.nullcontext(None)) as volume:
            with self._packager(job,role,volume) as builder:
                yield builder

    @contextlib.contextmanager
    def _packager(self,job,role,volume):
        """Bound only this dedicated BuildKit ancestor, preserving reserve and measured role profiles."""
        name="webagent-build-"+self.config["instance"]+"-"+uuid.UUID(job["job_id"]).hex+"-package-"+uuid.uuid4().hex[:8]
        if role not in {"api","loop","generic"}:raise ValueError("Fixed packaging role required")
        cap=package_cap(job.get("package_memory_cap"));identity=None;success=False;oom=False;proof_error=None;primary=False;stage="container_create";cap_outcome=None
        policy=self.config.get("scheduler",{});scratch=0 if volume else policy.get("package_tmpfs_bytes",0)
        # Persistent cache uses disk; the RAM scratch recipe remains for legacy jobs.
        if volume:cap=(policy.get("package_cache_prepare_memory_bytes",3*1024**3)
                       if getattr(self.runner.job_context,"preparing_runtime_base",False)
                       else policy.get("package_ram_bytes",0) or cap)
        elif scratch:cap=policy["package_ram_bytes"]
        if cap is not None and not self.resources.enabled:raise ValueError("Package cap requires enforced admission")
        profile="buildkit:v0.25.1:runtime-base-v1:"+role if getattr(self.runner.job_context,"preparing_runtime_base",False) else "buildkit:v0.25.1:role-context-v2:"+role+(":disk-cache-v1"if volume else ":ram-v1"if scratch else "")
        self.resources.enter(name,job,"package",profile,"package",timeout=600,hard_cap=cap)
        try:
            create=["docker","create","--name",name,"--privileged","--cgroup-parent","webagent-build-"+self.config["instance"]+".slice",
                "--label","webagent.scheduler.instance="+self.config["instance"],"--label","webagent.scheduler.job="+uuid.UUID(job["job_id"]).hex,"--label","webagent.scheduler.phase=package","--pids-limit","512"]
            if cap is not None:create +=["--memory",str(cap),"--memory-swap",str(cap)]
            if scratch:create +=["--tmpfs","/var/lib/buildkit:rw,exec,nosuid,nodev,size="+str(scratch)+",mode=0700"]
            if volume:create +=["--mount","type=volume,src="+volume+",dst=/var/lib/buildkit,volume-nocopy"]
            create +=[BUILDKIT,"--addr","unix:///run/buildkit/buildkitd.sock"]
            if volume:
                # BuildKit owns only this v3 cache volume. Keep its GC budget tied to
                # protected policy; Docker's unrelated caches are never pruned here.
                budget_mb=policy["package_cache_max_bytes"]//1_000_000
                reserved_mb=max(1,budget_mb//4)
                free_mb=max(reserved_mb,budget_mb//2)
                create +=["--oci-worker-gc","--oci-worker-gc-keepstorage",
                          f"{reserved_mb},{free_mb},{budget_mb}"]
            self.runner.command(create,timeout=30,private_raw=True)
            # Docker diagnostics share the command stream; only the protected helper identifies this exact container.
            stage="container_identity";created=self.runner.resource_request({"operation":"identity","container":name})
            candidate=created.get("container_id")
            if not isinstance(candidate,str)or not re.fullmatch(r"[a-f0-9]{64}",candidate)or created.get("state")!="created"or created.get("phase")!="package":raise ValueError("Packaging container identity invalid")
            identity=candidate
            with self.resources.lock:self.resources.rows[name]["container_id"]=identity
            stage="container_start";self.runner.command(["docker","start",name],timeout=30)
            if cap is not None:
                stage="admission_seal";self.runner.resource_request({"operation":"seal","container":name,"expected_id":identity})
            self.resources.attached(name,identity)
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
            stage="workload";self.runner.job_context.phase_container=name
            self.runner.job_context.observation_scope={"phase":"package","recipe":profile,"kind":"package","target":role}
            try:
                yield name;success=True
            finally:
                self.runner.job_context.observation_scope=None
                observed=job.get("execution_observation",{})
                if observed.get("phase")=="package":
                    job.setdefault("execution_history",[]).append({k:v for k,v in observed.items()if k!="events"})
                    job["execution_history"]=job["execution_history"][-128:]
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

    def probe(self,job):
        """Run a fixed networkless toolchain/limit probe with no application-source or state mounts."""
        root=Path(self.config["state_root"])/"checks"/job["job_id"]
        source=root/"empty-source";source.mkdir(parents=True,exist_ok=True)
        job["artifact_component"]="resource-probe:"+job["job_id"]
        if job.get("check_task")=="execution_probe":
            # Fixed, source-free acceptance of the actual docker-exec stream and libtest.
            program="#[test] fn slow_observation_probe() { std::thread::sleep(std::time::Duration::from_secs(3)); }"
            script="set -e\nprintf '%s' "+shlex.quote(program)+" > /output/observation.rs\nrustc --test /output/observation.rs -o /output/observation-test\ni=0; while [ $i -lt 8192 ]; do printf 'bounded observation output line 01234567890123456789012345678901234567890123456789\\n'; i=$((i+1)); done\n/output/observation-test --test-threads=1"
            return self.phase(job,"test","test",source,root/"output",["sh","-c",script],"resource:execution-probe-v1",90)
        return self.phase(job,"compile_test","engine",source,root/"output",["sh","-c","rustc --version && sleep 30"],"resource:memory-probe-v1",90)

    @staticmethod
    def dependency_binding(source):
        """Bind downloads to locked dependencies, manifests, Cargo configuration and pinned toolchain."""
        import platform
        paths=[source/"Cargo.lock",*sorted(source.rglob("Cargo.toml")),
            *sorted((source/".cargo").glob("**/*"))]
        if len(paths)>4096:raise ValueError("Dependency manifest bound exceeded")
        inputs={}
        for path in paths:
            if path.is_symlink():raise ValueError("Dependency input symlink")
            if path.is_dir():continue
            if not path.is_file():raise ValueError("Locked dependency input unavailable: "+path.name)
            inputs[path.relative_to(source).as_posix()]=hashlib.sha256(path.read_bytes()).hexdigest()
        return {"schema_version":1,"toolchain":RUST,"architecture":platform.machine(),"inputs":inputs}

    @staticmethod
    def verify_dependency_archives(source,cargo):
        """Require each crates.io archive to match the checksum in this exact lockfile."""
        locked=tomllib.loads((source/"Cargo.lock").read_text());required={}
        for package in locked.get("package",[]):
            origin=package.get("source")
            if origin is None:continue
            if origin!="registry+https://github.com/rust-lang/crates.io-index":
                raise ValueError("Shared offline cache requires reviewed support for non-crates.io dependencies")
            checksum=package.get("checksum")
            if not isinstance(checksum,str)or not re.fullmatch(r"[a-f0-9]{64}",checksum):raise ValueError("Locked registry checksum missing")
            required[package["name"]+"-"+package["version"]+".crate"]=checksum
        found={}
        for path in (cargo/"registry/cache").glob("*/*.crate"):
            if path.is_symlink()or not path.is_file():raise ValueError("Dependency archive identity invalid")
            if path.name in required:
                sha=hashlib.sha256(path.read_bytes()).hexdigest()
                if sha!=required[path.name]:raise ValueError("Dependency archive checksum mismatch: "+path.name)
                found[path.name]=sha
        missing=sorted(set(required)-set(found))
        if missing:raise ValueError("Missing locked dependency archives: "+", ".join(missing[:12]))
        return len(found)

    def dependencies(self,job,source,output,*,prepare=False,timeout=600):
        """Share verified downloads only; compiler objects and tests retain independent proof rules."""
        binding=self.dependency_binding(source);key=hashlib.sha256(json.dumps(binding,sort_keys=True).encode()).hexdigest()
        root=Path(self.config["state_root"])/"cargo-dependencies"
        if root.is_symlink():raise ValueError("Shared dependency directory symlink")
        root.mkdir(mode=0o700,parents=True,exist_ok=True)
        destination=root/key;cargo=output/"cargo";cargo.mkdir(parents=True,exist_ok=True)
        if cargo.is_symlink():raise ValueError("Private Cargo directory symlink")
        names=("registry",)
        def inventory(folder):return self.artifact_hashes(folder)
        def persist(value):
            job["dependency_cache"]={"key":key,"toolchain":RUST,"lockfile_sha256":binding["inputs"]["Cargo.lock"],**value}
            getattr(self.runner,"resource_progress",lambda _:None)(job)
        def preserve_private():
            # Cargo-home configuration is compiler-writable scratch too; never inherit it into a trusted fetch.
            if cargo.is_symlink():raise ValueError("Private dependency symlink")
            if any(cargo.iterdir()):
                cargo.rename(output/("previous-cargo-"+uuid.uuid4().hex));cargo.mkdir()
        validated=False
        with self.runner.plugin_build_lock("dependencies-"+key):
            if destination.is_symlink():raise ValueError("Shared dependency cache symlink")
            if destination.exists():
                receipt=destination/"receipt.json";shared=destination/"cargo"
                if receipt.is_symlink()or not receipt.is_file()or receipt.stat().st_size>16*1024*1024:raise ValueError("Dependency receipt unavailable")
                saved=json.loads(receipt.read_bytes())
                if saved.get("binding")!=binding or inventory(shared)!=saved.get("inventory"):raise ValueError("Verified dependency cache changed")
                preserve_private()
                for name in names:
                    if (shared/name).exists():shutil.copytree(shared/name,cargo/name)
                if inventory(shared)!=saved["inventory"]or {p:sha for p,sha in inventory(cargo).items()if p.startswith("registry/")}!=saved["inventory"]:
                    raise ValueError("Dependency copy verification failed")
                persist({"decision":"hit","reason":"matching_lock_manifests_toolchain","archives":saved["archives"]})
                log="Verified shared Cargo downloads reused: "+key+"\n"
            else:
                persist({"decision":"miss","reason":"no_verified_dependency_entry","preparation_required":not prepare})
                if not prepare:
                    packages=tomllib.loads((source/"Cargo.lock").read_text()).get("package",[])
                    required=[p["name"]+"@"+p["version"]for p in packages if p.get("source")][:8]
                    raise ValueError("Offline dependency cache miss: "+key+"; unverified locked inputs: "+", ".join(required)+"; run build_work check=dependencies on this task before compilation")
                # A compiler child can mutate its scratch. Fresh fetch never certifies old private bytes.
                preserve_private()
                log=self.phase(job,"dependency","dependency",source,output,["cargo","fetch","--locked"],"cargo-dependencies:rust1.90:fetch-v1",timeout,network=True)
                count=self.verify_dependency_archives(source,cargo)
                self.phase(job,"dependency","dependency",source,output,["sh","-c","cargo metadata --locked --offline --format-version 1 > /output/dependency-metadata.json"],"cargo-dependencies:rust1.90:offline-v1",timeout)
                validated=True
                captured={p:sha for p,sha in inventory(cargo).items()if p.startswith("registry/")}
                temporary=root/(key+".preparing-"+uuid.uuid4().hex);temporary.mkdir(mode=0o700);copy=temporary/"cargo";copy.mkdir()
                for name in names:
                    if (cargo/name).exists():shutil.copytree(cargo/name,copy/name)
                if inventory(copy)!=captured:raise ValueError("Dependency snapshot changed during publication")
                with (temporary/"receipt.json").open("x")as stream:
                    json.dump({"binding":binding,"inventory":captured,"archives":count},stream,sort_keys=True);stream.flush();os.fsync(stream.fileno())
                if self.dependency_binding(source)!=binding:raise ValueError("Dependency inputs changed during preparation")
                os.rename(temporary,destination)
                fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY)
                try:os.fsync(fd)
                finally:os.close(fd)
                persist({"decision":"prepared","reason":"locked_fetch_checksum_and_offline_metadata_verified","archives":count})
        try:
            if not validated:log+=self.phase(job,"dependency","dependency",source,output,["sh","-c","cargo metadata --locked --offline --format-version 1 > /output/dependency-metadata.json"],"cargo-dependencies:rust1.90:offline-v1",timeout)
        except (RuntimeError,ValueError)as error:
            raise ValueError("Offline dependency preflight failed for "+key+": "+str(error)[:1600])from error
        if self.dependency_binding(source)!=binding:raise ValueError("Dependency inputs changed during offline verification")
        return log

    def snapshot_binding(self,source):
        """Key compiled third-party objects by locked inputs, compiler image, flags and every compile recipe."""
        return {"dependencies":self.dependency_binding(source),"recipe":self.compiler_recipe(),"rustflags":RUSTFLAGS,
            "version":SNAPSHOT_RECIPE,"compiles":[SNAPSHOT_COMPILES,self.live_recipe()]}

    @staticmethod
    def snapshot_stub(source,metadata,stub):
        """Copy exact manifests and replace every workspace target with an empty stub; return member names."""
        if metadata.is_symlink()or not metadata.is_file()or metadata.stat().st_size>64*1024*1024:raise ValueError("Snapshot metadata unavailable")
        value=json.loads(metadata.read_bytes());members=set(value.get("workspace_members")or[])
        if not members:raise ValueError("Snapshot workspace members missing")
        manifests=set()
        for relative,sha in Executor.dependency_binding(source)["inputs"].items():
            body=(source/relative).read_bytes()
            if hashlib.sha256(body).hexdigest()!=sha:raise ValueError("Snapshot manifest changed")
            destination=stub/relative;destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(body);manifests.add(relative)
        names=set()
        for package in value.get("packages",[]):
            if package.get("id")not in members:continue
            name=package.get("name")
            if not isinstance(name,str)or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}",name):raise ValueError("Snapshot package name invalid")
            names.add(name)
            for target in package.get("targets",[]):
                kinds=target.get("kind");path=target.get("src_path");label=target.get("name")
                if not isinstance(kinds,list)or not isinstance(label,str)or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}",label):raise ValueError("Snapshot target invalid")
                if not isinstance(path,str)or not path.startswith("/build/"):raise ValueError("Snapshot target outside workspace")
                relative=posixpath.normpath(path[len("/build/"):])
                if relative.startswith("..")or relative.startswith("/")or not relative.endswith(".rs")or relative in manifests:raise ValueError("Snapshot target path invalid")
                destination=stub/relative;destination.parent.mkdir(parents=True,exist_ok=True)
                if not destination.parent.resolve().is_relative_to(stub.resolve()):raise ValueError("Snapshot target path escaped")
                library=any(kind in {"lib","rlib","proc-macro"}for kind in kinds)
                destination.write_text("#![allow(warnings)]\n"+(""if library else"fn main(){}\n"))
                names.add(label)
        if "webagent-v3"not in names:raise ValueError("Snapshot root package missing")
        return sorted(names),sorted(package.get("name")for package in value["packages"]if package.get("id")in members)

    @staticmethod
    def snapshot_leftovers(target,names):
        """List products of stubbed workspace crates that cleaning left in a dependency snapshot."""
        forms={name.replace("-","_")for name in names};found=[]
        def stubbed(stem):
            crate=stem.rsplit("-",1)[0].replace("-","_")
            return crate in forms or(crate.startswith("lib")and crate[3:]in forms)
        for profile in ("debug","release"):
            base=target/profile
            if not base.is_dir():continue
            for path in base.iterdir():
                if path.is_file()and path.name.split(".",1)[0].replace("-","_")in forms:found.append(path.relative_to(target).as_posix())
            for folder in (".fingerprint","build","deps"):
                if (base/folder).is_dir():found+=[path.relative_to(target).as_posix()for path in (base/folder).iterdir()if stubbed(path.name.split(".",1)[0])]
        return sorted(found)

    def dependency_snapshot(self,job,source,output):
        """Seed a fresh private target with third-party objects compiled from a stubbed workspace.

        No task source is compiled into the snapshot: every workspace target is an
        empty stub and all stub products are cleaned and checked before atomic
        publication. Each job gets a verified private copy, Cargo fingerprints
        still decide freshness and every test, smoke and equivalence gate runs
        unchanged. Any failure leaves an empty target, so the job compiles cold.
        """
        target=output/"target";started=time.monotonic()
        def persist(decision,**value):
            job["dependency_snapshot"]={"decision":decision,"seconds":round(time.monotonic()-started,3),**value}
            getattr(self.runner,"resource_progress",lambda _:None)(job)
        if target.is_dir()and not target.is_symlink()and not any(target.iterdir()):target.rmdir()
        if target.exists()or target.is_symlink():
            persist("skipped",reason="private_target_present");return ""
        try:
            binding=self.snapshot_binding(source);key=hashlib.sha256(json.dumps(binding,sort_keys=True).encode()).hexdigest()
            fetched=hashlib.sha256(json.dumps(binding["dependencies"],sort_keys=True).encode()).hexdigest()
            downloads=Path(self.config["state_root"])/"cargo-dependencies"/fetched/"cargo"
            root=Path(self.config["state_root"])/"compiled-dependencies"
            if root.is_symlink():raise ValueError("Snapshot directory symlink")
            root.mkdir(mode=0o700,parents=True,exist_ok=True);destination=root/key;log=""
            with self.runner.plugin_build_lock("compiled-dependencies-"+key):
                if destination.is_symlink():raise ValueError("Snapshot symlink")
                decision="hit"
                if not destination.exists():
                    reserve=int(self.config.get("scheduler",{}).get("disk_min_free_bytes",5*1024**3))+SNAPSHOT_HEADROOM
                    if shutil.disk_usage(root).free<reserve:
                        persist("skipped",key=key,reason="insufficient_disk");return ""
                    temporary=root/(key+".preparing-"+uuid.uuid4().hex);temporary.mkdir(mode=0o700)
                    try:
                        stub=temporary/"stub";work=temporary/"output";stub.mkdir();work.mkdir()
                        names,packages=self.snapshot_stub(source,output/"dependency-metadata.json",stub)
                        if downloads.is_symlink()or not (downloads/"registry").is_dir():raise ValueError("Verified downloads unavailable")
                        shutil.copytree(downloads/"registry",work/"cargo/registry")
                        selected=" ".join("-p "+shlex.quote(name)for name in packages)
                        log=self.phase(job,"dependency","engine",stub,work,["sh","-c",SNAPSHOT_COMPILES+"\ncargo clean --locked --offline "+selected+"\ncargo clean --release --locked --offline "+selected],"compiled-dependencies:rust1.90:stub-v1")
                        built=work/"target"
                        if built.is_symlink()or not built.is_dir():raise ValueError("Snapshot target missing")
                        leftovers=self.snapshot_leftovers(built,names)
                        if leftovers:raise ValueError("Stub products survived cleaning: "+", ".join(leftovers[:6]))
                        inventory=self.artifact_hashes(built);built.rename(temporary/"target")
                        shutil.rmtree(stub);shutil.rmtree(work)
                        with (temporary/"receipt.json").open("x")as stream:
                            json.dump({"binding":binding,"inventory":inventory,"members":names},stream,sort_keys=True);stream.flush();os.fsync(stream.fileno())
                        if self.snapshot_binding(source)!=binding:raise ValueError("Snapshot inputs changed during preparation")
                        os.rename(temporary,destination)
                    finally:
                        if temporary.exists():shutil.rmtree(temporary,ignore_errors=True)
                    fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY)
                    try:os.fsync(fd)
                    finally:os.close(fd)
                    decision="prepared"
                    entries=[path for path in root.iterdir()if not path.is_symlink()and path!=destination]
                    stale=sorted((path for path in entries if re.fullmatch(r"[a-f0-9]{64}",path.name)),key=lambda path:path.stat().st_mtime,reverse=True)[SNAPSHOT_KEEP-1:]
                    stale+=[path for path in entries if ".preparing-"in path.name and time.time()-path.stat().st_mtime>6*3600]
                    for path in stale:shutil.rmtree(path,ignore_errors=True)
                receipt=destination/"receipt.json"
                if receipt.is_symlink()or not receipt.is_file()or receipt.stat().st_size>64*1024*1024:raise ValueError("Snapshot receipt unavailable")
                saved=json.loads(receipt.read_bytes())
                if saved.get("binding")!=binding:raise ValueError("Snapshot receipt binding mismatch")
                shutil.copytree(destination/"target",target,symlinks=True)
                if self.artifact_hashes(target)!=saved.get("inventory"):raise ValueError("Snapshot copy verification failed")
                os.utime(destination)
            persist(decision,key=key,files=len(saved["inventory"]),downloads=fetched)
            return log+"Verified compiled-dependency snapshot "+decision+": "+key+"\n"
        except PermissionError:
            raise
        except (OSError,RuntimeError,ValueError,KeyError,TypeError)as error:
            if target.is_symlink():target.unlink()
            elif target.exists():shutil.rmtree(target)
            persist("unavailable",reason=type(error).__name__,detail=str(error)[:300])
            return "Compiled-dependency snapshot unavailable; compiling cold: "+str(error)[:300]+"\n"

    @staticmethod
    def workspace_stub(source,metadata,stub):
        """Copy the application with only the root package's targets stubbed; return (root names, real file hashes).

        Module crates compile for real with the real manifests, so feature resolution
        matches the engine build. A module build script or proc-macro would run task
        code at compile time, so either refuses the snapshot."""
        if metadata.is_symlink()or not metadata.is_file()or metadata.stat().st_size>64*1024*1024:raise ValueError("Workspace metadata unavailable")
        value=json.loads(metadata.read_bytes());members=set(value.get("workspace_members")or[])
        hashes={}
        for path in sorted(source.rglob("*")):
            if path.is_symlink():raise ValueError("Workspace source symlink")
            if path.is_file():hashes[path.relative_to(source).as_posix()]=hashlib.sha256(path.read_bytes()).hexdigest()
        shutil.copytree(source,stub,symlinks=True)
        names=set();roots=0
        for package in value.get("packages",[]):
            if package.get("id")not in members:continue
            kinds={kind for target in package.get("targets",[])for kind in target.get("kind",[])}
            if package.get("name")!="webagent-v3":
                if kinds&{"custom-build","proc-macro"}:raise ValueError("Workspace snapshot requires build-script-free module crates: "+str(package.get("name")))
                continue
            roots+=1;names.add("webagent-v3")
            for target in package.get("targets",[]):
                path=target.get("src_path");label=target.get("name")
                if not isinstance(label,str)or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}",label)or not isinstance(path,str)or not path.startswith("/build/"):raise ValueError("Workspace root target invalid")
                relative=posixpath.normpath(path[len("/build/"):])
                if relative.startswith("..")or relative.startswith("/")or not relative.endswith(".rs")or relative not in hashes:raise ValueError("Workspace root target path invalid")
                library=any(kind in {"lib","rlib","proc-macro"}for kind in target.get("kind",[]))
                (stub/relative).write_text("#![allow(warnings)]\n"+(""if library else"fn main(){}\n"))
                names.add(label)
        if roots!=1:raise ValueError("Workspace root package missing")
        return sorted(names),hashes

    @staticmethod
    def root_leftovers(target,names):
        """Root package products left in a workspace snapshot: its fingerprints and build
        script, and its binaries' executables and dep-info. Module crates may share a name
        with a root binary (webagent-diagnostics), so a deps entry counts only when no
        library artifact with the same hash sits beside it."""
        bins={name.replace("-","_")for name in names if name!="build-script-build"}
        found=[]
        for profile in ("debug","release"):
            base=target/profile
            if not base.is_dir():continue
            for folder in (".fingerprint","build"):
                if (base/folder).is_dir():found+=[p.relative_to(target).as_posix()for p in (base/folder).iterdir()if p.name.rsplit("-",1)[0]=="webagent-v3"]
            deps=base/"deps"
            if deps.is_dir():
                for p in deps.iterdir():
                    stem=p.name[:-2]if p.name.endswith(".d")else p.name
                    if "."in stem or p.name.startswith("lib")or stem.rsplit("-",1)[0]not in bins:continue
                    if not any((deps/("lib"+stem+suffix)).exists()for suffix in (".rlib",".rmeta",".so")):found.append(p.relative_to(target).as_posix())
            found+=[p.relative_to(target).as_posix()for p in base.iterdir()if p.is_file()and p.name.split(".",1)[0].replace("-","_")in bins]
        return sorted(found)

    @staticmethod
    def freshen(root,hashes):
        """Give files whose bytes match the snapshot record FROZEN_MTIME; changed and new files stay newer."""
        frozen=0
        for path in root.rglob("*"):
            if path.is_file()and not path.is_symlink():
                relative=path.relative_to(root).as_posix()
                if relative in hashes and hashlib.sha256(path.read_bytes()).hexdigest()==hashes[relative]:
                    os.utime(path,(FROZEN_MTIME,FROZEN_MTIME),follow_symlinks=False);frozen+=1
        return frozen

    def workspace_snapshot(self,job,source,output):
        """Reuse module crate objects compiled from earlier sources in a trusted stub-root phase.

        The phase copies the newest snapshot for this dependency key, rebuilds only the
        module crates whose sources changed (plus dependents) in a copy of the current
        source with the root package stubbed and cleaned, and publishes the result.
        The job's target is replaced only after the copy verifies; any failure keeps
        the dependency snapshot and the job compiles its modules as before."""
        started=time.monotonic();dependency=job.get("dependency_snapshot")or{}
        def persist(decision,**value):
            job["workspace_snapshot"]={"decision":decision,"seconds":round(time.monotonic()-started,3),**value}
            getattr(self.runner,"resource_progress",lambda _:None)(job)
        if dependency.get("decision")not in {"hit","prepared"}:
            persist("skipped",reason="no_dependency_snapshot");return ""
        target=output/"target";replacement=output/"workspace-target"
        try:
            base=dependency["key"];root=Path(self.config["state_root"])/"compiled-workspace"/base
            if root.parent.is_symlink()or root.is_symlink():raise ValueError("Workspace snapshot directory symlink")
            root.mkdir(mode=0o700,parents=True,exist_ok=True);log=""
            if not re.fullmatch(r"[a-f0-9]{64}",str(dependency.get("downloads",""))):raise ValueError("Dependency download key unavailable")
            downloads=Path(self.config["state_root"])/"cargo-dependencies"/dependency["downloads"]/"cargo"
            with self.runner.plugin_build_lock("compiled-workspace-"+base):
                temporary=root/("preparing-"+uuid.uuid4().hex);temporary.mkdir(mode=0o700)
                try:
                    stub=temporary/"stub";work=temporary/"output";work.mkdir()
                    names,hashes=self.workspace_stub(source,output/"dependency-metadata.json",stub)
                    binding={"dependency":base,"recipe":WORKSPACE_RECIPE,"compiles":SNAPSHOT_COMPILES,"compiler":self.compiler_recipe()}
                    key=hashlib.sha256(json.dumps({"binding":binding,"hashes":hashes},sort_keys=True).encode()).hexdigest()
                    destination=root/key;decision="hit"
                    if destination.is_symlink():raise ValueError("Workspace snapshot symlink")
                    if not destination.exists():
                        reserve=int(self.config.get("scheduler",{}).get("disk_min_free_bytes",5*1024**3))+SNAPSHOT_HEADROOM
                        if shutil.disk_usage(root).free<reserve:
                            persist("skipped",reason="insufficient_disk");return ""
                        previous=None
                        for candidate in sorted((p for p in root.iterdir()if re.fullmatch(r"[a-f0-9]{64}",p.name)and not p.is_symlink()),key=lambda p:p.stat().st_mtime,reverse=True):
                            saved=self.workspace_receipt(candidate)
                            if saved is not None and saved.get("binding")==binding:previous=(candidate,saved);break
                        # Seed from the previous snapshot (verified against its receipt) or from
                        # the job's dependency-snapshot target (verified when it was copied).
                        expected=previous[1]["inventory"]if previous else self.artifact_hashes(target)
                        shutil.copytree(previous[0]/"target"if previous else target,work/"target",symlinks=True)
                        if self.artifact_hashes(work/"target")!=expected:raise ValueError("Workspace seed copy verification failed")
                        if downloads.is_symlink()or not (downloads/"registry").is_dir():raise ValueError("Verified downloads unavailable")
                        shutil.copytree(downloads/"registry",work/"cargo/registry")
                        # Unchanged files keep the previous objects fresh; without a previous
                        # snapshot every module compiles once from the dependency seed.
                        self.freshen(stub,previous[1]["hashes"]if previous else hashes)
                        log=self.phase(job,"dependency","engine",stub,work,["sh","-c",SNAPSHOT_COMPILES+"\ncargo clean --locked --offline -p webagent-v3\ncargo clean --release --locked --offline -p webagent-v3"],"compiled-workspace:rust1.90:stub-root-v1")
                        built=work/"target"
                        if built.is_symlink()or not built.is_dir():raise ValueError("Workspace snapshot target missing")
                        leftovers=self.root_leftovers(built,names)
                        if leftovers:raise ValueError("Root stub products survived cleaning: "+", ".join(leftovers[:6]))
                        inventory=self.artifact_hashes(built);built.rename(temporary/"target")
                        with (temporary/"receipt.json").open("x")as stream:
                            json.dump({"binding":binding,"hashes":hashes,"inventory":inventory,"seeded_from":previous[0].name if previous else "dependency"},stream,sort_keys=True);stream.flush();os.fsync(stream.fileno())
                        os.rename(temporary,destination);decision="prepared"
                        entries=[p for p in root.iterdir()if not p.is_symlink()and p!=destination]
                        stale=sorted((p for p in entries if re.fullmatch(r"[a-f0-9]{64}",p.name)),key=lambda p:p.stat().st_mtime,reverse=True)[WORKSPACE_KEEP-1:]
                        stale+=[p for p in entries if p.name.startswith("preparing-")and time.time()-p.stat().st_mtime>6*3600]
                        for path in stale:shutil.rmtree(path,ignore_errors=True)
                finally:
                    if temporary.exists():shutil.rmtree(temporary,ignore_errors=True)
                saved=self.workspace_receipt(destination)
                if saved is None or saved.get("binding")!=binding or saved.get("hashes")!=hashes:raise ValueError("Workspace snapshot receipt mismatch")
                if replacement.exists():shutil.rmtree(replacement)
                shutil.copytree(destination/"target",replacement,symlinks=True)
                if self.artifact_hashes(replacement)!=saved["inventory"]:raise ValueError("Workspace snapshot copy verification failed")
                shutil.rmtree(target);replacement.rename(target)
                os.utime(destination)
            frozen=self.freshen(source,hashes)
            persist(decision,key=key,files=len(saved["inventory"]),frozen_sources=frozen)
            return log+"Verified workspace snapshot "+decision+": "+key+"\n"
        except PermissionError:
            raise
        except (OSError,RuntimeError,ValueError,KeyError,TypeError)as error:
            if replacement.exists():shutil.rmtree(replacement,ignore_errors=True)
            persist("unavailable",reason=type(error).__name__,detail=str(error)[:300])
            return "Workspace snapshot unavailable; module crates compile in the build: "+str(error)[:300]+"\n"

    @staticmethod
    def workspace_receipt(folder):
        """A bounded workspace snapshot receipt, or None."""
        receipt=folder/"receipt.json"
        if receipt.is_symlink()or not receipt.is_file()or receipt.stat().st_size>64*1024*1024:return None
        try:return json.loads(receipt.read_bytes())
        except ValueError:return None

    def verification_source(self,blob):
        """Conservatively bind every captured application/fixture file, excluding private inspection data."""
        files=self.runner.execution_entries(blob)
        return hashlib.sha256(json.dumps([(name,mode,hashlib.sha256(body).hexdigest())for name,(mode,body)in sorted(files.items())],separators=(",",":")).encode()).hexdigest()

    def test_proof(self,blob):
        import platform
        return {"policy":"exact-source-tests-v1","source_sha256":self.verification_source(blob),"toolchain":RUST,
            "compiler_recipe":self.compiler_recipe(),"architecture":platform.machine(),"kernel":platform.release(),"executor_sha256":hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "identities":{k:self.config.get(k)for k in ("service_uid","service_gid","app_uid","app_gid","worker_uid","worker_gid")}}

    def verification_binding(self,job,blob,task,profile):
        return {**self.test_proof(blob),"task":task,"feature_profile":profile,"test_filter":job.get("test_filter"),
            "compiler_memory_cap":job.get("compiler_memory_cap"),"executor_sha256":hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}

    def cached_check_result(self,binding):
        key=hashlib.sha256(json.dumps(binding,sort_keys=True).encode()).hexdigest()
        folder=Path(self.config["state_root"])/"verification-certificates"
        if folder.is_symlink():raise ValueError("Verification certificate directory symlink")
        folder.mkdir(mode=0o700,parents=True,exist_ok=True);path=folder/(key+".json")
        if path.is_symlink():raise ValueError("Verification certificate symlink")
        if not path.exists():return key,None
        if path.stat().st_size>2*1024*1024:raise ValueError("Verification certificate exceeds bound")
        value=json.loads(path.read_bytes())
        if value.get("binding")!=binding or not isinstance(value.get("log"),str):raise ValueError("Verification certificate binding changed")
        identity=value.get("job_id","")
        if not re.fullmatch(r"[a-f0-9-]{36}",identity):raise ValueError("Verification certificate job identity invalid")
        receipt=Path(self.config["state_root"])/(identity+".json")
        if receipt.is_symlink():raise ValueError("Verification job receipt symlink")
        if not receipt.is_file():return key,None
        previous=json.loads(receipt.read_bytes())
        if previous.get("status")!="completed"or previous.get("operation")!="check"or previous.get("verification_key")!=key or previous.get("revoked"):
            return key,None
        return key,value

    def record_check_result(self,job,binding,key,log):
        folder=Path(self.config["state_root"])/"verification-certificates"
        if folder.is_symlink():raise ValueError("Verification certificate directory symlink")
        folder.mkdir(mode=0o700,parents=True,exist_ok=True)
        value={"binding":binding,"job_id":job["job_id"],"log":log[-1024*1024:]}
        temporary=folder/(key+".new-"+uuid.uuid4().hex)
        with temporary.open("x")as stream:json.dump(value,stream,sort_keys=True);stream.flush();os.fsync(stream.fileno())
        os.replace(temporary,folder/(key+".json"));job["verification_key"]=key
        getattr(self.runner,"resource_progress",lambda _:None)(job)

    @staticmethod
    def focused_test_plan(changed,metadata):
        """Registered host assertions precede at most three affected library packages."""
        paths=sorted({p.removeprefix("v3/")for p in changed});plan=[];packages=[]
        for rule in HOST_FOCUSED_REGISTRY:
            affected=[p for p in paths if any(p==prefix or prefix.endswith("/")and p.startswith(prefix)for prefix in rule["paths"])]
            if affected:plan.append({"id":rule["id"],"package":rule.get("package","webagent-v3"),"target":rule.get("target","webagent-v3"),"test":rule["test"],"feature_profile":"default","affected_paths":affected,**{key:rule[key]for key in ("target_kind","source_path")if key in rule}})
        local=[]
        for item in metadata.get("packages",[]):
            path=item.get("manifest_path","")
            if item.get("source")is None and path.startswith("/build/")and path.endswith("/Cargo.toml"):
                local.append((path[len("/build/"):-len("/Cargo.toml")],item.get("name","")))
        for relative in paths:
            if not relative.endswith((".rs","Cargo.toml")):continue
            matches=[(prefix,name)for prefix,name in local if prefix and relative.startswith(prefix+"/")]
            if matches:
                _,name=max(matches,key=lambda entry:len(entry[0]))
                if re.fullmatch(r"[a-zA-Z][a-zA-Z0-9_-]{0,79}",name)and name not in packages:packages.append(name)
        return plan+[{"package":name}for name in packages[:3]]

    def focused_tests(self,job,application,output,retained=None,fresh_tests=None):
        """Fail fast without certifying a partial pass or replacing the required suite.

        Each phase uses the normal host cancellation/admission controls. Guards bind
        bytes and file modes before any successful assertion can be reused.
        """
        source=application.parent;before=self.runner.execution_source_digest(source)
        metadata_path=output/"dependency-metadata.json"
        metadata=self.metadata_receipt(metadata_path)if retained is None and metadata_path.is_file()else {"packages":[]}
        plan=self.focused_test_plan(job.get("changed_paths",[]),metadata)
        proof={"policy":HOST_FOCUSED_POLICY,"policy_sha256":HOST_FOCUSED_POLICY_SHA256,"required_suite":"workspace",
            "inputs_sha256":job.get("inputs_sha256"),"source_sha256":before,"toolchain":RUST,"selection":plan,
            "reuse":"verified_retained_test_binaries"if retained is not None else"fresh_workspace_test_binaries"if fresh_tests is not None else"existing_private_cargo_target",
            "status":"running"if plan else"not_applicable","runs":[]}
        job["focused_verification"]=proof;started=time.monotonic();deadline=started+600
        progress=getattr(self.runner,"resource_progress",lambda _:None);progress(job)
        manifest=None
        def unchanged():
            try:return self.runner.execution_source_digest(source)==before
            except (OSError,ValueError):return False
        def authorized():
            event=getattr(getattr(self.runner,"job_context",None),"event",None)
            if job.get("revoked")or job.get("lease_until",float("inf"))<=time.time()or(event is not None and event.is_set()):
                raise PermissionError("Focused verification cancelled or authorization expired")
        try:
            manifest=self.test_manifest(retained if retained is not None else fresh_tests)if (retained is not None or fresh_tests is not None)and plan else None
            for item in plan:
                label=item.get("test",item["package"]);entry={**item,"feature_profile":"default","status":"running","matched":0,"started_at":time.time()}
                proof["runs"].append(entry);run_started=time.monotonic();progress(job)
                try:
                    authorized();remaining=deadline-time.monotonic()
                    if remaining<=0:raise TimeoutError("Focused verification stage deadline exceeded")
                    argv=["cargo","test","--locked","--offline","-p",item["package"]]
                    if "test"in item:argv.extend((["--lib"]if item.get("target_kind")=="lib"else["--bin",item["target"]])+[item["test"],"--","--exact","--test-threads=1"])
                    else:argv.extend(["--","--test-threads=1"])
                    if retained is not None or fresh_tests is not None and "test"in item:
                        matches=[row for row in manifest["tests"]if row["target_name"]==item.get("target")and row["source_path"]==item.get("source_path","/build/src/main.rs")]
                        if item.get("target_kind")=="lib":
                            # This is one fixed protected registration, not a model-selected package/target.
                            if (item["package"],item["target"],item["source_path"])!=("webagent-engine","webagent_engine","/build/crates/modules/engine/src/lib.rs"):raise ValueError("Unregistered focused library target")
                            prefix="path+file:///build/crates/modules/engine#webagent-engine@"
                            matches=[row for row in matches if row.get("kind")==["lib"]and row.get("package_id","").startswith(prefix)and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?",row["package_id"][len(prefix):])]
                        else:
                            if (item["package"],item.get("target"),item.get("source_path","/build/src/main.rs"))!=("webagent-v3","webagent-v3","/build/src/main.rs"):raise ValueError("Unregistered focused binary target")
                            prefix="path+file:///build#webagent-v3@"
                            matches=[row for row in matches if row.get("kind")==["bin"]and row.get("package_id","").startswith(prefix)and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?",row["package_id"][len(prefix):])]
                        if len(matches)!=1:raise ValueError("Registered retained host test binary unavailable")
                        argv=["/output/artifacts/"+matches[0]["file"],item["test"],"--exact","--test-threads=1"]
                    entry["argv"]=argv;entry["recipe"]="engine:rust1.90:focused-v2:"+item.get("id",item["package"])
                    log=self.phase(job,"test","test",application,output,argv,entry["recipe"],min(120 if "test"in item else 300,remaining))
                    authorized()
                    counts=re.findall(r"test result: ok\. ([0-9]+) passed; ([0-9]+) failed",log[-1024*1024:])
                    entry["matched"]=sum(int(passed)for passed,failed in counts)
                    if any(int(failed)for passed,failed in counts):raise ValueError("Test summary reports failed assertions")
                    if not entry["matched"]:raise ValueError("Zero matched assertions for registered selection")
                    if "test"in item and entry["matched"]!=1:raise ValueError("Exact registered test count differs from one")
                    if not unchanged():raise ValueError("Focused test changed captured inputs, including file modes")
                    entry.update(status="passed",inputs_unchanged=True)
                except PermissionError as error:
                    entry.update(status="cancelled",reason=str(error)[:1200]);proof["status"]="cancelled";raise
                except Exception as error:
                    entry.update(status="failed",reason=str(error)[:1200]);proof["status"]="failed"
                    raise ValueError("Focused verification failed: "+label+": "+str(error)[:1200])from error
                finally:
                    entry["seconds"]=round(time.monotonic()-run_started,3)
                    entry["inputs_unchanged"]=unchanged()
                    if not entry["inputs_unchanged"]:entry["status"]="input_mutated";proof["status"]="failed"
                    progress(job)
            if plan:proof["status"]="passed"
            return "Fail-fast registered checks complete; required workspace suite follows.\n"
        except BaseException:
            if proof["status"]=="running":proof["status"]="failed"
            raise
        finally:
            proof["seconds"]=round(time.monotonic()-started,3);progress(job)

    def check(self,job,blob,task,timeout,extract):
        """Use the same attributable recipes for bounded foreground checks and release compilation."""
        profile=check_profile(job.get("feature_profile"));argv=None if task=="dependencies"else check_argv(task,profile)
        job["artifact_component"]="engine"
        root=Path(self.config["state_root"])/"checks"/job["job_id"];source=root/"source";output=root/"output"
        blob=self.runner.execution_application_blob(blob);root.mkdir(parents=True,exist_ok=True);extract(source,blob);output.mkdir(exist_ok=True)
        application=source/"v3";before=self.runner.execution_source_digest(source);start=time.monotonic()
        binding=None;key=None
        if task!="dependencies":
            binding=self.verification_binding(job,blob,task,profile);key,cached=self.cached_check_result(binding)
            job["verification_cache"]={"decision":"hit"if cached else"miss","key":key,"reason":"exact_source_recipe_profile_and_fixture_binding"if cached else"no_completed_matching_certificate"}
            if cached:
                job["verification_cache"]["source_job_id"]=cached["job_id"]
                getattr(self.runner,"resource_progress",lambda _:None)(job)
                return "Reused completed verification from "+cached["job_id"]+"; no compiler or tests executed for this receipt.\n"+cached["log"]
        log=self.dependencies(job,application,output,prepare=task=="dependencies",timeout=min(timeout,600))
        if task=="dependencies":return log
        remaining=max(1,timeout-int(time.monotonic()-start))
        recipe="engine:rust1.90:"+("check"if task=="check"else"compile_test")+(":live-context-v1"if profile=="live"else"")
        log+=self.phase(job,"compile_test","engine",application,output,argv,recipe,remaining)
        if task=="test":
            selected=test_filter(job.get("test_filter"))
            runtime=["cargo","test","--locked","--offline"]+([selected]if selected is not None else[])+["--","--test-threads=1"]+(["--nocapture"]if selected is not None else[])
            log+=self.phase(job,"test","test",application,output,runtime,"engine:rust1.90:test",max(1,timeout-int(time.monotonic()-start)))
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Check changed its captured application inputs")
        self.record_check_result(job,binding,key,log)
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
    def cached_engine(self,blob,output,mode="dev"):
        """Reuse only artifacts whose pinned recipe, release mode and complete compiled inputs still match the candidate."""
        cache=Path(self.config["state_root"])/"engine-artifacts";cache.mkdir(mode=0o700,exist_ok=True)
        files=self.runner.execution_entries(blob);rust=self.rust_inputs(files)
        for path in sorted(cache.glob("*.json"),key=lambda p:p.stat().st_mtime,reverse=True)[:64]:
            if path.is_symlink():raise ValueError("Engine cache receipt symlink")
            value=json.loads(path.read_text())
            if value["recipe"]!=self.compiler_recipe() or value["rust_inputs"]!=rust or value.get("release_mode","dev")!=mode:continue
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
    def smoke_recipe(mode,linked=None):
        """Write only fixed mode-proof artifacts; structured JSON never enters public log scrubbing."""
        if mode=="linked"and linked=="/output/target/debug/webagent-v3":return ["sh","-c","set -e\ntest ! -L /output/stable/linked-smoke-result.json\n/output/target/debug/webagent-v3 --context-smoke > /output/stable/linked-smoke-result.json"]
        # The root test binary writes the proof through its ignored hook; a filter that
        # matches nothing writes no file, which the caller detects.
        if mode=="linked"and linked is not None and re.fullmatch(r"/output/artifacts/tests/webagent_v3-[a-f0-9]{16}",linked):return ["sh","-c","set -e\ntest ! -L /output/stable/linked-smoke-result.json\nrm -f /output/stable/linked-smoke-result.json\nWEBAGENT_CONTEXT_SMOKE_OUT=/output/stable/linked-smoke-result.json "+linked+" engine_components::smoke_probe::context_smoke_to_file --exact --ignored --test-threads=1"]
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
        with self.job_lock():job["log"]=(job.get("log","")+logs).encode("utf-8")[-1024*1024:].decode("utf-8","ignore")
        getattr(self.runner,"resource_progress",lambda _:None)(job)

    @staticmethod
    def smoke_result(text,mode):
        """Validate every registered pure component's two optimized deterministic cases."""
        if len(text)>65536:raise ValueError("Context smoke result exceeds bound")
        value=json.loads(text)
        if not isinstance(value,dict)or value.get("mode")!=mode or value.get("equivalence")is not True or type(value.get("interface_version"))is not int or value["interface_version"]not in (1,2):raise ValueError("Optimized context smoke failed")
        if value["interface_version"]==1 and any(key in value for key in SMOKE_CASE_KEYS[3:]):raise ValueError("Legacy smoke cannot attest newer component proofs")
        for key in SMOKE_CASE_KEYS[:3]if value["interface_version"]==1 else SMOKE_CASE_KEYS:
            if not isinstance(value.get(key),list)or len(value[key])!=2:raise ValueError("Optimized component smoke failed")
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
        if value["interface_version"]!=linked["interface_version"]or any(value[key]!=linked[key]for key in (SMOKE_CASE_KEYS[:3]if value["interface_version"]==1 else SMOKE_CASE_KEYS)):raise ValueError("Optimized linked/live results differ")
        return value
    @staticmethod
    def linked_recipe():
        """Share optimized dependency objects while keeping the linked executable as a frozen artifact."""
        return ["cargo","build","--release","--locked","--offline","--bin","webagent-v3"]
    @staticmethod
    def live_recipe(stable=False):
        """The deployed release build; a stable release also links ability crates (STABLE_FEATURE)."""
        features="webagent-v3/live-context"+(",webagent-v3/"+STABLE_FEATURE if stable else "")
        return ["sh","-c","set -e\ncargo clean --release --package webagent-v3 --locked --offline\ncargo build --workspace --release --locked --offline --no-default-features --features "+features]
    def linked_dependencies(self,metadata,target,files):
        """Attest linked implementation inputs separately from the deployed live dependency closure."""
        captured={}
        for name in self.core_dependencies(metadata,target):
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
            if state["job_id"]!=previous.name or state["status"]not in {"failed","completed"}or state.get("compiler_recipe")!=self.compiler_recipe():continue
            old_blob=self.runner.execution_application_blob(captured.read_bytes())
            old=self.runner.execution_entries(old_blob)
            if self.rust_inputs(old)!=self.rust_inputs(files):continue
            if self.runner.execution_source_digest(previous/"source")!=self.runner.execution_source_digest_blob(old_blob):
                # A final UI rebase can differ from the original compiler snapshot; never seed mismatching bytes.
                job.setdefault("cargo_cache_rejections",[]).append({"job_id":previous.name,"reason":"previous_execution_source_differs_from_candidate"})
                continue
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
    def retain_engine(self,job,blob,dependencies,hashes,log=None):
        """Publish a content-addressed cache receipt only after tests, source and every output are verified."""
        files=self.runner.execution_entries(blob);inputs={}
        for name in dependencies:
            relative=self.local_dependency(name)
            if relative is not None:
                if relative not in files:raise ValueError("Compiled local input absent from captured source")
                inputs[relative]=hashlib.sha256(files[relative][1]).hexdigest()
        value={"recipe":self.compiler_recipe(),"rust_inputs":self.rust_inputs(files),"compiled_inputs":inputs,
            "artifacts":hashes,"job_id":job["job_id"],"release_mode":job.get("release_mode","dev")}
        if log is not None:value.update(test_proof=self.test_proof(blob),test_log=log[-1024*1024:])
        if not inputs:raise ValueError("Compiled input closure missing")
        key=hashlib.sha256(json.dumps({k:v for k,v in value.items()if k not in {"job_id","artifacts","test_proof","test_log"}},sort_keys=True).encode()).hexdigest()
        cache=Path(self.config["state_root"])/"engine-artifacts";cache.mkdir(mode=0o700,exist_ok=True)
        temporary=cache/(key+".new-"+uuid.uuid4().hex);temporary.write_text(json.dumps(value));temporary.chmod(0o600);os.replace(temporary,cache/(key+".json"))

    # @code-index {"version":1,"feature":"automation-drop-in","aliases":["standalone automation","automation ability plugin"],"purpose":"Verified standalone Automation compilation, offline dependency provisioning and complete native tests; host publication and permission boundaries remain authoritative.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"The native package contract, test proof and compiler boundaries are invariants; no setting may bypass them."}}
    def provision_native_registry(self,cache):
        """Provide a native ability's locked offline dependencies from attested compiler registry bytes.

        No network or application state is supplied. A prior compiler child can
        modify its scratch cache, so existing bytes must match a host-held engine
        receipt before reuse; mismatches require explicit reconciliation.
        """
        state=Path(self.config['state_root']);cache=Path(cache)
        if cache.parent!=state/'plugin-builds'or not re.fullmatch(r'[a-f0-9]{64}',cache.name)or cache.is_symlink():
            raise ValueError('Owned plugin dependency scratch required')
        cargo=cache/'cargo';target=cargo/'registry'
        if cargo.is_symlink()or target.is_symlink()or(target.exists()and not target.is_dir()):
            raise ValueError('Plugin dependency registry identity invalid')
        candidates=[]
        for path in sorted((state/'engine-artifacts').glob('*.json'),key=lambda p:p.stat().st_mtime,reverse=True)[:64]:
            if path.is_symlink()or not path.is_file()or path.stat().st_size>16*1024*1024:
                raise ValueError('Compiler dependency receipt identity invalid')
            value=json.loads(path.read_bytes())
            if value.get('recipe')!=self.compiler_recipe():continue
            jid=value.get('job_id','')
            if not re.fullmatch(r'[a-f0-9-]{36}',jid):raise ValueError('Compiler dependency job identity invalid')
            prefix='optimization-cargo/registry/'
            hashes={name.removeprefix(prefix):sha for name,sha in value.get('artifacts',{}).items()if name.startswith(prefix)}
            if not hashes:continue
            if any(not name or name.startswith('/')or any(p in ('','.','..')for p in name.split('/'))
                or not isinstance(sha,str)or not re.fullmatch(r'[a-f0-9]{64}',sha)for name,sha in hashes.items()):
                raise ValueError('Compiler dependency inventory invalid')
            candidates.append((jid,hashes))
        def proof(jid,hashes,reused):
            """Return bounded provenance plus an in-memory inventory for the post-compile check."""
            digest=hashlib.sha256(json.dumps(hashes,sort_keys=True,separators=(',',':')).encode()).hexdigest()
            return {'job_id':jid,'sha256':digest,'hashes':hashes,'reused':reused}
        if target.exists():
            actual=self.artifact_hashes(target)
            for jid,hashes in candidates:
                if actual==hashes:return proof(jid,hashes,True)
            raise ValueError('Plugin dependency scratch differs from verified compiler registry')
        for jid,hashes in candidates:
            artifacts=state/'execution'/jid/'output/artifacts';parent=artifacts/'optimization-cargo';source=parent/'registry'
            if artifacts.is_symlink()or parent.is_symlink()or source.is_symlink():
                raise ValueError('Compiler dependency source symlink')
            if not source.is_dir():continue
            if self.artifact_hashes(source)!=hashes:
                raise ValueError('Verified compiler dependency registry changed')
            cargo.mkdir(parents=True,exist_ok=True)
            shutil.copytree(source,target)
            if self.artifact_hashes(source)!=hashes or self.artifact_hashes(target)!=hashes:
                raise ValueError('Plugin dependency provisioning changed inputs')
            return proof(jid,hashes,False)
        raise ValueError('Verified offline plugin dependencies unavailable; controlled provisioning required')

    @staticmethod
    def collect_runtime_artifacts(application,output,artifacts,legacy_automation,context_components=False):
        """Copy the fixed core closure and only the reviewed recipe's optional legacy distribution.

        The extracted Automation ability is built by the existing standalone plugin builder.
        Missing mandatory outputs remain fatal; stale Cargo outputs never add a package.
        """
        binaries=[name for name in BINARIES if context_components or name not in NEW_CONTEXT_BINARIES]+(["webagent-automation"]if legacy_automation else[])
        for name in binaries:
            destination=artifacts/name
            shutil.copyfile(output/"target/release"/name,destination);destination.chmod(0o555)
        packages=[("outbound_notifications","webagent-outbound-notifications")]
        if legacy_automation:packages.insert(0,("automation","webagent-automation"))
        for package,binary in packages:
            folder=artifacts/"modules"/package;folder.mkdir(parents=True)
            shutil.copyfile(artifacts/binary,folder/binary)
            for path in(application/"src/plugin_packages"/package).iterdir():
                if path.name in {"package.json","tool-schemas.json"}or path.name.startswith("page."):
                    if path.is_symlink()or not path.is_file():raise ValueError("Invalid module distribution asset")
                    shutil.copyfile(path,folder/path.name)

    def validate_compiler_recipe(self,blob):
        """Accept only the two reviewed test/build recipes and reject inconsistent extraction inputs.

        Returns whether this captured application still builds the legacy Automation binary.
        Tests, locked compilation and stable/live equivalence recipes are unchanged.
        """
        files=self.runner.execution_entries(blob);text=files["v3/Dockerfile"][1].decode()
        start=text.index("FROM rust:");end=text.index("FROM debian:",start)
        recipe=hashlib.sha256(text[start:end].encode()).hexdigest()
        if recipe in {COMPILER_STAGE_SHA256,CONTEXT_COMPILER_STAGE_SHA256}:return True
        if recipe not in {EXTRACTED_AUTOMATION_COMPILER_STAGE_SHA256,CONTEXT_EXTRACTED_AUTOMATION_COMPILER_STAGE_SHA256,SHARED_RELEASE_CONTEXT_COMPILER_STAGE_SHA256,PARALLEL_COMPILER_STAGE_SHA256,MOLD_COMPILER_STAGE_SHA256}:
            raise ValueError("Compiler Dockerfile recipe changed; migrate the reviewed attributable engine recipe")
        cargo=tomllib.loads(files["v3/Cargo.toml"][1].decode())
        if any(target.get("name")=="webagent-automation"for target in cargo.get("bin",[]))or "v3/src/plugin_packages/automation/main.rs"in files:
            raise ValueError("Extracted Automation recipe still has a legacy root binary")
        return False
    def context_component_recipe(self,blob):
        """Select the additional mandatory outputs from exact reviewed source, never stale binaries."""
        text=self.runner.execution_entries(blob)["v3/Dockerfile"][1].decode()
        stage=text[text.index("FROM rust:"):text.index("FROM debian:",text.index("FROM rust:"))]
        return hashlib.sha256(stage.encode()).hexdigest()in {CONTEXT_COMPILER_STAGE_SHA256,CONTEXT_EXTRACTED_AUTOMATION_COMPILER_STAGE_SHA256,SHARED_RELEASE_CONTEXT_COMPILER_STAGE_SHA256,PARALLEL_COMPILER_STAGE_SHA256,MOLD_COMPILER_STAGE_SHA256}

    def retain_test_manifest(self,output,artifacts):
        """Retain Cargo-attested tests and exact debug support; never substitute optimized binaries."""
        metadata=output/"test-artifacts.json"
        if metadata.is_symlink()or not metadata.is_file()or metadata.stat().st_size>32*1024*1024:raise ValueError("Cargo test metadata identity")
        tests=[];support=None;additional_support=[];official_records={};folder=artifacts/"tests";folder.mkdir()
        for line in metadata.read_text().splitlines():
            if len(line)>1024*1024:raise ValueError("Cargo test metadata line bound")
            value=json.loads(line)
            if value.get("reason")!="compiler-artifact"or not value.get("executable"):continue
            target=value["target"];executable=value["executable"]
            test=value.get("profile",{}).get("test")is True
            official=not test and target.get("name")in SERVICE_SOURCES and SERVICE_SOURCES[target["name"]]==target.get("src_path") and target.get("kind")==["bin"]
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
                if binary in official_records:
                    with source.open("rb")as stream:source_hash=hashlib.file_digest(stream,"sha256").hexdigest()
                    if official_records[binary]!=record or source_hash!=self.artifact_hashes(destination.parent)[destination.name]:raise ValueError("Repeated official service provenance changed")
                    continue
                official_records[binary]=record.copy()
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
        if not isinstance(extra,list)or len(extra)>len(SERVICE_SOURCES)-1:raise ValueError("Service support inventory incomplete")
        seen=set()
        for item in extra:
            binary=item.get("destination","").removeprefix("target/debug/")
            source=SERVICE_SOURCES.get(binary)
            if binary=="webagent-context-service"or binary in seen or source is None or item.get("file")!="test-support/debug/"+binary or item.get("destination")!="target/debug/"+binary or item.get("profile")!="debug"or item.get("source_path")!=source:raise ValueError("Service support provenance invalid")
            seen.add(binary)
            related=[test for test in value["tests"]if test.get("kind")==["test"]and test.get("source_path","").startswith(source.removesuffix("/src/main.rs")+"/tests/")]
            if related and not all(test.get("package_id")==item.get("package_id")for test in related):raise ValueError("Service integration provenance invalid")
        for binary,source in SERVICE_SOURCES.items():
            if binary=="webagent-context-service":continue
            if any(test.get("kind")==["test"]and test.get("source_path","").startswith(source.removesuffix("/src/main.rs")+"/tests/")for test in value["tests"])and binary not in seen:raise ValueError("Service support inventory incomplete")
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
    def linked_test_binary(manifest):
        """The retained unit-test executable of the root app binary, in its default (linked) features."""
        found=[item["file"]for item in manifest["tests"]if item.get("target_name")=="webagent-v3"and item.get("kind")==["bin"]and item.get("source_path")=="/build/src/main.rs"]
        if len(found)!=1:raise ValueError("Linked root test binary missing")
        return "/output/artifacts/"+found[0]
    @staticmethod
    def restore_test_support(output,artifacts,manifest):
        """Restore only attested debug support at Cargo's fixed integration-test executable path."""
        debug=output/"target/debug";debug.mkdir(parents=True,exist_ok=True)
        for item in [manifest["support"],*manifest.get("additional_support",[])]:
            target=output/item["destination"];shutil.copyfile(artifacts/item["file"],target);target.chmod(0o555)
    def attest_release_base(self,job,blob):
        """Re-attest the deferred publication rebase before expensive work; optional on non-broker runners."""
        attest=getattr(self.runner,"attest_release_base",None)
        if attest is not None:attest(job,blob)
    def engine(self,job,blob,extract,digest):
        """Compile/test once, retain input attestations and produce the complete existing image artifact set."""
        job["artifact_component"]="engine"
        context_blob=blob;self.attest_release_base(job,context_blob)
        blob=self.runner.execution_application_blob(blob);legacy_automation=self.validate_compiler_recipe(blob)
        root=Path(self.config["state_root"])/"execution"/job["job_id"];source=root/"source";output=root/"output"
        if (root/"verified.json").exists():
            receipt=json.loads((root/"verified.json").read_text())
            if receipt["inputs_sha256"]!=digest(blob):raise ValueError("Artifact source changed")
            if self.artifact_hashes(output/"artifacts")!=receipt["artifacts"]:raise ValueError("Retained artifact changed")
            if receipt.get("test_proof")!=self.test_proof(blob):raise ValueError("Retained verification recipe/environment changed; reconcile through a successor")
            return output/"artifacts",receipt["log"]
        job["compiler_recipe"]=self.compiler_recipe()
        blob=self.runner.execution_application_blob(blob)
        root.mkdir(parents=True,exist_ok=True);extract(source,blob);application=source/"v3";output.mkdir(exist_ok=True)
        stable_release=job.get("release_mode")=="stable"
        cached=self.cached_engine(blob,output,"stable"if stable_release else"dev")
        if cached:
            artifacts,old=cached;logs="Verified compiler artifacts reused; locked Rust tests previously passed on identical compiled inputs.\n"
            manifest=self.test_manifest(artifacts)
            self.restore_test_support(output,artifacts,manifest)
            reused_tests=old.get("test_proof")==self.test_proof(blob)and isinstance(old.get("test_log"),str)
            if reused_tests:
                job["test_reuse"]={"source_job_id":old["job_id"],"binding":old["test_proof"],"decision":"reused_completed_workspace_suite"}
                logs="Exact source, toolchain and fixture proof matched; complete workspace test evidence reused from "+old["job_id"]+".\n"+old["test_log"]
            else:
                logs+=self.focused_tests(job,application,output,retained=artifacts)
                replayed=0;results=0
                for binary in sorted((artifacts/"tests").iterdir()):
                    if not re.fullmatch(r"[a-z0-9_]+-[a-f0-9]{16}",binary.name):raise ValueError("Cached test executable name invalid")
                    run=self.phase(job,"test","test",application,output,["/output/artifacts/tests/"+binary.name,"--test-threads="+str(TEST_THREADS)],"engine:rust1.90:test")
                    logs+=run;replayed+=1;results+=len(re.findall(r"test result:",run))
                if replayed!=results:raise ValueError("Parallel replay skipped a retained test binary")
                counts=Executor.suite_totals(logs)
                job["suite_counts"]={"passed":counts[0],"failed":counts[1],"ignored":counts[2]}
            if self.runner.execution_source_digest(source)!=self.runner.execution_source_digest_blob(blob):
                raise ValueError("Cached tests changed candidate source")
            logs=logs.replace("locked Rust tests previously passed on identical compiled inputs.","current candidate ran retained tests from identical compiled inputs.")
            if self.artifact_hashes(artifacts)!=old["artifacts"]:raise ValueError("Retained test run changed verified compiler artifacts")
            if not reused_tests:self.retain_engine(job,blob,[name.removeprefix("v3/")for name in old["compiled_inputs"]],old["artifacts"],logs)
            receipt={"inputs_sha256":digest(blob),"artifacts":old["artifacts"],"log":logs,"reused_from":old["job_id"],"test_proof":self.test_proof(blob)}
            temporary=root/"verified.new";temporary.write_text(json.dumps(receipt));os.replace(temporary,root/"verified.json")
            if not (artifacts/"stable/smoke.json").is_file():raise ValueError("Retained optimized linked smoke proof missing")
            job["linked_stable"]=json.loads((artifacts/"stable/smoke.json").read_text())
            job["optimized_mode_equivalence"]=json.loads((artifacts/"stable/mode-equivalence.json").read_text())
            job["compiler_artifact_reuse"]=old["job_id"]
            try:
                rust_digest=self.verification_source(blob)
                proof_store=Path(self.config["state_root"])/"equivalence-proofs"/(rust_digest[:16])
                if not (proof_store/"mode-equivalence.json").is_file() and (artifacts/"stable"/"mode-equivalence.json").is_file():
                    proof_store.mkdir(parents=True,exist_ok=True)
                    (proof_store/"mode-equivalence.json").write_text((artifacts/"stable"/"mode-equivalence.json").read_text())
            except (OSError,ValueError,json.JSONDecodeError):
                pass
            job["linker_proof"]=self.metadata_receipt(artifacts/"stable/linker-proof.json")
            return artifacts,logs
        seeded=self.seed_cargo_cache(job,blob,output)
        cargo=output/"cargo";cargo.mkdir(exist_ok=True)
        inputs=digest(blob);before=self.runner.execution_source_digest(source)
        logs=self.dependencies(job,application,output,prepare=True)
        if seeded:logs+=self.phase(job,"dependency","dependency",application,output,["cargo","clean","--package","webagent-v3"],"engine:rust1.90:clean-root",600)
        else:logs+=self.dependency_snapshot(job,application,output);logs+=self.workspace_snapshot(job,application,output)
        self.attest_release_base(job,context_blob)
        logs+=self.phase(job,"compile_test","engine",application,output,["sh","-c","set -e\ncargo test --workspace --locked --offline --no-run --message-format=json-render-diagnostics > /output/test-artifacts.json\ncargo build --workspace --exclude webagent-v3 --locked --offline --bins --message-format=json-render-diagnostics >> /output/test-artifacts.json"],"engine:rust1.90:compile_test")
        artifacts=output/"artifacts";artifacts.mkdir(exist_ok=True)
        manifest=self.retain_test_manifest(output,artifacts)
        logs+=self.focused_tests(job,application,output,fresh_tests=artifacts)
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Focused tests changed captured application inputs")
        # Linked reference: the root crate's default-features (linked) debug test binary,
        # which compile_test built and the suite runs, executes the context smoke through
        # an ignored test hook. No separate debug app binary or linked release is compiled.
        stable=output/"stable";stable.mkdir(exist_ok=True)
        linked=self.linked_test_binary(manifest)
        logs+=self.phase(job,"linked_smoke","test",application,output,self.smoke_recipe("linked",linked),"engine:rust1.90:linked-smoke",60)
        if not (stable/"linked-smoke-result.json").exists():
            # Sources without the hook run nothing; build the debug app once, as before.
            logs+=self.phase(job,"linked_smoke","engine",application,output,["cargo","build","--locked","--offline","-p","webagent-v3","--bin","webagent-v3"],"engine:rust1.90:compile_test")
            linked="/output/target/debug/webagent-v3"
            logs+=self.phase(job,"linked_smoke","test",application,output,self.smoke_recipe("linked",linked),"engine:rust1.90:linked-smoke",60)
        proof=self.linked_smoke(self.smoke_artifact(stable/"linked-smoke-result.json"))
        # The full suite runs beside the live (deployed) release compile. Tests mount the
        # application writable, so the compile reads a verified private copy that
        # no test container mounts; the post-suite digest check still applies.
        linked_source=root/"linked-source"
        if linked_source.exists():shutil.rmtree(linked_source)
        shutil.copytree(source,linked_source,symlinks=True)
        if self.runner.execution_source_digest(linked_source)!=before:raise ValueError("Compile source copy differs from captured inputs")
        self.attest_release_base(job,context_blob)
        suite=self.concurrent_phase(job,"test","test",application,output,["cargo","test","--workspace","--locked","--offline","--","--test-threads="+str(TEST_THREADS)],"engine:rust1.90:test")
        try:live_log=self.phase(job,"compile_release","engine",linked_source/"v3",output,self.live_recipe(stable_release),"engine:rust1.90:release")
        finally:
            try:suite_output=suite.result()
            finally:shutil.rmtree(linked_source,ignore_errors=True)
        logs+=suite_output
        counts=Executor.suite_totals(suite_output)
        if not any(counts):raise ValueError("Suite executed no tests")
        job["suite_counts"]={"passed":counts[0],"failed":counts[1],"ignored":counts[2]}
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Tests left changed application inputs")
        logs+=live_log
        logs+=self.phase(job,"live_smoke","test",application,output,self.smoke_recipe("live"),"engine:rust1.90:live-smoke",90)
        live_proof=self.live_smoke(self.smoke_artifact(stable/"live-smoke-result.json"),proof)
        job["optimized_mode_equivalence"]={"status":"passed","linked":proof,"live":live_proof,"linked_profile":"debug"}
        (stable/"mode-equivalence.json").write_text(json.dumps(job["optimized_mode_equivalence"],sort_keys=True)+"\n")
        getattr(self.runner,"resource_progress",lambda _:None)(job)
        if self.runner.execution_source_digest(source)!=before:raise ValueError("Compilation changed application inputs")
        artifacts=output/"artifacts";artifacts.mkdir(exist_ok=True)
        self.collect_runtime_artifacts(application,output,artifacts,legacy_automation,self.context_component_recipe(blob))
        shutil.copytree(stable,artifacts/"stable")
        # Preserve the offline optimization registry independently of optional abilities.
        registry=artifacts/"optimization-cargo/registry";registry.parent.mkdir()
        shutil.copytree(cargo/"registry",registry)
        dependencies=self.compiler_dependencies(output/"target")
        self.phase(job,"engine_metadata","dependency",application,output,["sh","-c","cargo metadata --locked --offline --format-version 1 --no-default-features --features webagent-v3/live-context > /output/core-metadata.json"],"engine:rust1.90:metadata")
        metadata=self.metadata_receipt(output/"core-metadata.json")
        core=self.core_dependencies(metadata,output/"target")
        (artifacts/"core-inputs.txt").write_text("\n".join(sorted(core))+"\n")
        files=self.runner.execution_entries(blob);compiled_inputs={}
        for name in core:
            relative=self.local_dependency(name)
            if relative is None:continue
            if relative not in files:raise ValueError("Compiled input absent from captured source")
            compiled_inputs[relative]=hashlib.sha256(files[relative][1]).hexdigest()
        if "v3/src/main.rs"not in compiled_inputs:raise ValueError("Live root dependency attestation missing")
        # Built-input attestation for UI rebases now names the deployed (live) closure.
        job["linked_stable"]={"mode":"linked","profile":"debug","compiled_inputs":compiled_inputs,"smoke":proof,"deployed":False}
        (artifacts/"stable/smoke.json").write_text(json.dumps(job["linked_stable"],sort_keys=True)+"\n")
        if "v3/src/main.rs"not in {self.local_dependency(name)for name in dependencies}:raise ValueError("Compiler dependency attestation missing")
        logs+=self.phase(job,"engine_metadata","dependency",application,output,
            ["sh","-c","set -e\nreadelf -p .comment "+linked+" > /output/linked-comment.txt\nreadelf -p .comment /output/target/release/webagent-v3 > /output/live-comment.txt\nmkdir -p /output/compiler\ninstall -m755 /usr/bin/mold /output/compiler/mold\ninstall -m644 /usr/share/doc/mold/copyright /usr/share/doc/mold/LICENSE.gz /output/compiler/"],"engine:rust1.90:mold-proof",60)
        comments={}
        for mode in ("linked","live"):
            body=(output/(mode+"-comment.txt")).read_bytes()
            if len(body)>32768 or not re.search(rb"\bmold 1\.10\.1\b",body):raise ValueError("Optimized "+mode+" root ELF lacks required Mold linker comment")
            comments[mode]=body.decode("utf-8","strict")
        if hashlib.sha256((output/"compiler/mold").read_bytes()).hexdigest()!=MOLD_SHA256:raise ValueError("Packaged Mold executable differs from compiler")
        shutil.copytree(output/"compiler",artifacts/"compiler")
        job["linker_proof"]={"compiler_recipe":self.compiler_recipe(),"comments":comments,
            "linked_sha256":hashlib.sha256((output/linked.removeprefix("/output/")).read_bytes()).hexdigest(),"linked_profile":"debug","linked_binary":linked,
            "live_sha256":hashlib.sha256((artifacts/"webagent-v3").read_bytes()).hexdigest(),"mold_sha256":MOLD_SHA256}
        (artifacts/"stable/linker-proof.json").write_text(json.dumps(job["linker_proof"],sort_keys=True)+"\n")
        hashes=self.artifact_hashes(artifacts);self.retain_engine(job,blob,dependencies,hashes,logs)
        receipt={"inputs_sha256":inputs,"artifacts":hashes,"log":logs,"test_proof":self.test_proof(blob)}
        temporary=root/"verified.new";temporary.write_text(json.dumps(receipt));os.replace(temporary,root/"verified.json")
        return artifacts,logs
    @staticmethod
    def runtime_python_modes(dockerfile):
        """Normalize only baked runtime scripts; frozen source and private modes stay intact."""
        source="COPY v3/worker.py v3/ability_plugin_worker.py v3/sandbox.py v3/launch.py v3/promote.py /opt/v3/"
        normalized=source.replace("COPY ","COPY --chmod=0755 ",1)
        if dockerfile.splitlines().count(source)+dockerfile.splitlines().count(normalized)!=1:
            raise ValueError("Reviewed runtime Python COPY contract required")
        return dockerfile.replace(source,normalized)

    def package(self,blob,artifacts,entries,role=None,destination=None,recipe_transform=None):
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
        if recipe_transform is not None:
            dockerfile=recipe_transform(dockerfile,role)
        else:
            dockerfile=dockerfile[:start]+"FROM "+("scratch"if role=="loop"else RUST)+" AS tested_builder\nCOPY scheduler-out /out\n"+dockerfile[end:]
        dockerfile=self.runtime_python_modes(dockerfile)
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
