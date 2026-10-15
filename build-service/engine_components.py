"""Fixed engine component compilation and generation-local executable sealing.

Manifests describe build inputs; they cannot add host authority or choose commands.
"""
import hashlib,json,os,re,shutil,tomllib
from pathlib import Path
ROOT="v3/plugins/engine/"
ID="context-assembly"
BINARY="webagent-context-service"
SOURCE_INPUTS=["crates/context-service","crates/context-assembly","crates/context-contracts","Cargo.toml","Cargo.lock","config/context-compaction.json"]

REGISTRY={"boot-supervisor":{"binary":"webagent-boot-service","inputs":["crates/boot-service","crates/boot-supervisor","crates/boot-contracts","crates/engine-service","Cargo.toml","Cargo.lock"],"operation":"boot.plan","limit":65536,"timeout":1000},ID:{"binary":BINARY,"inputs":SOURCE_INPUTS,"operation":"context.prepare","limit":16777216,"timeout":30000},
 "background-dispatch":{"binary":"webagent-background-service","inputs":["crates/background-service","crates/background-dispatch","crates/background-contracts","crates/engine-service","Cargo.toml","Cargo.lock"],"operation":"background.plan","limit":65536,"timeout":1000}}

def descriptor(path):
    """Validate the registered engine interface before building or replacing its artifact."""
    if path.is_symlink()or not path.is_file()or path.stat().st_size>65536:raise ValueError("Engine descriptor identity")
    value=json.loads(path.read_bytes())
    identity=value.get("id");registered=REGISTRY.get(identity)
    if registered is None:raise ValueError("Unsupported engine identity")
    binary=registered["binary"]
    interface=value.get("interface_version")
    if interface not in ((1,2)if identity=="background-dispatch"else(1,)):raise ValueError("Unsupported engine interface")
    expected={"schema_version":1,"id":identity,"group":"engine-loop","interface_version":interface,"operation":registered["operation"],"modes":["live","linked"],"runtime_requirements":[],"executable":"artifact/"+binary}
    if set(value)-set(expected)-{"implementation_version","build","limits","artifact_sha256"}or type(value.get("schema_version"))is not int or type(value.get("interface_version"))is not int:raise ValueError("Engine contract fields")
    if any(value.get(k)!=v for k,v in expected.items()):raise ValueError("Unsupported engine contract")
    if not isinstance(value.get("implementation_version"),str)or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+",value["implementation_version"]):raise ValueError("Engine implementation version")
    if value.get("build")!={"package":binary,"source_inputs":registered["inputs"],"toolchain":"1.90.0","features":[],"runtime_assets":[]}:raise ValueError("Unsupported engine build recipe")
    if value.get("limits")!={"max_request_bytes":registered["limit"],"max_response_bytes":registered["limit"],"timeout_ms":registered["timeout"],"max_concurrency":1}:raise ValueError("Unsupported engine limits")
    if any(type(number)is not int for number in value["limits"].values()):raise ValueError("Engine limit types")
    return value

def package_path(path,entries):
    """Only registered engine sources qualify; missing manifests support deliberate removal."""
    if path==ROOT+"catalog.json":return True
    for identity,registered in REGISTRY.items():
        if path.startswith(ROOT+identity+"/"):return True
        if any(path=="v3/"+name or path.startswith("v3/"+name+"/")for name in registered["inputs"]if name.startswith("crates/")):return True
    return False

def closure(root,identity=ID):
    """Expand local Cargo path dependencies, rejecting undeclared outside-workspace inputs."""
    result={"Cargo.toml","Cargo.lock"};pending=[REGISTRY[identity]["inputs"][0]];seen=set()
    while pending:
        name=pending.pop()
        if name in seen:continue
        seen.add(name);folder=root/name
        if not name.startswith("crates/")or not folder.is_dir()or folder.is_symlink():raise ValueError("Confined workspace crate required")
        manifest=folder/"Cargo.toml"
        parsed=tomllib.loads(manifest.read_text())
        if parsed.get("package",{}).get("build"):raise ValueError("Engine build scripts require coordinated recipe review")
        for path in folder.rglob("*"):
            if path.is_symlink():raise ValueError("Engine source symlink")
            if path.is_file():
                relative=path.relative_to(root).as_posix()
                if path.stat().st_size>16*1024*1024:raise ValueError("Engine source bound")
                result.add(relative)
        groups=[parsed.get("dependencies",{}),parsed.get("build-dependencies",{}),parsed.get("dev-dependencies",{})]
        for target in parsed.get("target",{}).values():groups.extend(target.get(k,{})for k in ("dependencies","build-dependencies","dev-dependencies"))
        for group in groups:
            for dependency in group.values():
                if isinstance(dependency,dict)and "path"in dependency:
                    path=(folder/dependency["path"]).resolve()
                    if not path.is_relative_to(root):raise ValueError("Engine dependency escapes workspace")
                    pending.append(path.relative_to(root).as_posix())
                elif isinstance(dependency,dict)and dependency.get("workspace"):raise ValueError("Inherited dependencies require explicit closure support")
    fixture=root/"config/context-compaction.json"
    if fixture.is_symlink():raise ValueError("Engine test fixture symlink")
    if identity==ID and fixture.is_file():result.add("config/context-compaction.json")
    return {name:hashlib.sha256((root/name).read_bytes()).hexdigest()for name in sorted(result)}

def build(runner,job_id,root,state,assets,executor,scheduler):
    """Build every installed registered engine component using its fixed closure."""
    outputs=[]
    for identity in REGISTRY:outputs.extend(build_one(runner,job_id,root,state,assets,executor,scheduler,identity))
    return outputs

def build_one(runner,job_id,root,state,assets,executor,scheduler,identity):
    """Test and compile only the engine service, retaining attested dependency/output hashes."""
    declaration=root/ROOT/identity/"component.json"
    binary_name=REGISTRY[identity]["binary"];service_source=REGISTRY[identity]["inputs"][0]
    if not declaration.exists():return []
    value=descriptor(declaration);application=root/"v3";inputs=closure(application,identity)
    recipe=executor.RUST+":engine-"+("context"if identity==ID else identity)+"-v1:release:locked:offline:debug0"
    key=hashlib.sha256(json.dumps({"inputs":inputs,"recipe":recipe,"build":value["build"]},sort_keys=True).encode()).hexdigest()
    with runner.plugin_build_lock(key):
        artifact=assets/"artifacts"/key;binary=artifact/binary_name;receipt=artifact/"engine-receipt.json"
        reused=False
        if receipt.exists():
            if receipt.is_symlink()or binary.is_symlink()or not binary.is_file():raise ValueError("Engine artifact identity")
            saved=json.loads(receipt.read_bytes())
            if saved.get("input_key")!=key or saved.get("recipe")!=recipe or saved.get("artifact_sha256")!=hashlib.sha256(binary.read_bytes()).hexdigest():raise ValueError("Engine artifact changed")
            if any(name not in inputs or inputs[name]!=sha for name,sha in saved.get("compiled_inputs",{}).items())or not saved.get("compiled_inputs"):raise ValueError("Engine dependency attestation changed")
            reused=True
        else:
            active=getattr(runner.job_context,"job",None)
            if active is None or active["job_id"]!=job_id:raise ValueError("Owned engine compilation required")
            cache=state/"engine-component-builds"/key;cache.mkdir(parents=True,exist_ok=True)
            source=cache/"source"
            if source.exists():shutil.rmtree(source)
            source.mkdir()
            # Cargo metadata needs every declared workspace member; only the
            # registered package and its dependency closure compile or key reuse.
            for name in ("Cargo.toml","Cargo.lock","build.rs","src","crates","config","plugins"):
                path=application/name
                if path.is_symlink():raise ValueError("Compiler source symlink")
                if path.is_dir():shutil.copytree(path,source/name)
                elif path.is_file():shutil.copyfile(path,source/name)
            reused_from=seed_cache(cache,assets,state,inputs,recipe,executor)
            cargo=cache/"cargo";cargo.mkdir(exist_ok=True)
            if not (cargo/"registry").exists():
                runner.command(["docker","cp",runner.inspect_api_name+":/opt/optimization-cargo/registry",str(cargo)],timeout=120)
            def phase(name,argv,network=False):
                """Run a fixed component recipe through measured protected phase admission."""
                return runner.executor.phase(active,name,"dependency"if name=="engine_metadata"else"plugin",source,cache,argv,"plugin:rust1.90:engine-"+("context"if identity==ID else identity),600,network=network)
            phase("engine_metadata",["sh","-c","cargo metadata --locked --offline --format-version 1 --no-deps > /output/component-metadata.json"])
            metadata=executor.Executor.metadata_receipt(cache/"component-metadata.json")
            local={Path(p["manifest_path"]).parent.as_posix().removeprefix("/build/"):p for p in metadata["packages"]}
            if service_source not in local or local[service_source]["name"]!=binary_name:raise ValueError("Cargo metadata disagrees with engine declaration")
            packages=[local[name]["name"]for name in local if any(path.startswith(name+"/")for path in inputs)]
            phase("plugin_compile",["cargo","test","--locked","--offline",*[arg for package in sorted(packages)for arg in ("-p",package)]])
            phase("plugin_compile",["cargo","build","--release","--locked","--offline","-p",binary_name])
            if closure(application,identity)!=inputs or closure(source,identity)!=inputs:raise ValueError("Engine source changed during build")
            dependencies=executor.Executor.compiler_dependencies(cache/"target")
            compiled={}
            for name in dependencies:
                relative=executor.Executor.local_dependency(name)
                if relative is None:continue
                relative=relative.removeprefix("v3/")
                if relative.startswith("target/"):continue
                if relative not in inputs:raise ValueError("Engine compiled undeclared input")
                compiled[relative]=inputs[relative]
            if not any(name.startswith(service_source+"/")for name in compiled):raise ValueError("Engine compiler attestation missing")
            output=cache/"target/release"/binary_name
            if output.is_symlink()or not output.is_file()or output.stat().st_size>32*1024*1024:raise ValueError("Engine artifact bound")
            artifact.mkdir(parents=True,exist_ok=True);shutil.copyfile(output,binary);binary.chmod(0o555)
            scheduler.Scheduler.write_record(receipt,{"input_key":key,"recipe":recipe,"compiled_inputs":compiled,"inputs":inputs,"cargo_cache_hashes":{name:executor.Executor.artifact_hashes(cache/name)for name in ("cargo","target")},"reused_from":reused_from,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest()})
    return [{"package":identity,"build_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"reused":reused}]

def seal(root,outputs,assets):
    """Materialize the verified artifact inside the immutable generation, never live source."""
    for output in outputs:
        folder=root/ROOT/output["package"];path=folder/"component.json";value=descriptor(path)
        binary_name=REGISTRY[output["package"]]["binary"]
        binary=assets/"artifacts"/output["build_key"]/binary_name
        if hashlib.sha256(binary.read_bytes()).hexdigest()!=output["artifact_sha256"]:raise ValueError("Engine artifact changed before publication")
        target=folder/"artifact"/binary_name;target.parent.mkdir(exist_ok=True)
        if target.exists()and hashlib.sha256(target.read_bytes()).hexdigest()!=output["artifact_sha256"]:raise ValueError("Sealed engine artifact changed")
        if not target.exists():shutil.copyfile(binary,target);target.chmod(0o555)
        if "artifact_sha256"in value:
            if value["artifact_sha256"]!=output["artifact_sha256"]:raise ValueError("Sealed engine descriptor changed")
            continue
        value["artifact_sha256"]=output["artifact_sha256"]
        path.write_text(json.dumps(value,sort_keys=True,indent=2)+"\n");path.chmod(0o444)

def seal_catalog(root,job_id,assets,base_release_id=None):
    """Bind executable membership and withdrawals to the protected prior active generation."""
    import uuid
    uuid.UUID(job_id);folder=root/ROOT;folder.mkdir(parents=True,exist_ok=True)
    if base_release_id is not None:uuid.UUID(base_release_id)
    base=assets/"releases"/base_release_id if base_release_id is not None else assets/"current"
    previous=base/ROOT/"catalog.json";prior_ids=set();withdrawn=set()
    if previous.exists():
        if previous.is_symlink()or previous.stat().st_size>65536:raise ValueError("Prior engine catalog identity")
        prior=json.loads(previous.read_bytes())
        if prior.get("schema_version")!=1:raise ValueError("Prior engine catalog schema")
        prior_ids={item["id"]for item in prior["components"]};withdrawn=set(prior["withdrawn"])
    else:
        for path in (base/ROOT).glob("*/component.json"):
            prior_ids.add(descriptor(path)["id"])
    components=[]
    for path in sorted(folder.glob("*/component.json")):
        value=descriptor(path);binary=path.parent/value["executable"]
        if binary.is_symlink()or not binary.is_file()or hashlib.sha256(binary.read_bytes()).hexdigest()!=value.get("artifact_sha256"):raise ValueError("Unsealed engine membership")
        components.append({"id":value["id"],"descriptor_sha256":hashlib.sha256(path.read_bytes()).hexdigest(),"artifact_sha256":value["artifact_sha256"]})
    active={item["id"]for item in components}
    payload={"schema_version":1,"revision":job_id,"components":sorted(components,key=lambda item:item["id"]),"withdrawn":sorted((withdrawn|prior_ids)-active)}
    path=folder/"catalog.json";encoded=json.dumps(payload,sort_keys=True,indent=2)+"\n"
    if path.exists()and path.read_text()==encoded:return payload
    if path.is_symlink():raise ValueError("Engine membership symlink")
    path.write_text(encoded);path.chmod(0o444)
    return payload

def seed_cache(cache,assets,state,inputs,recipe,executor):
    """Reuse only integrity-checked Cargo caches from the same locked manifests and pinned recipe."""
    for receipt in sorted((assets/"artifacts").glob("*/engine-receipt.json"),key=lambda path:path.stat().st_mtime,reverse=True)[:32]:
        if receipt.is_symlink():raise ValueError("Engine cache receipt symlink")
        previous=json.loads(receipt.read_bytes());key=previous.get("input_key","")
        if not re.fullmatch(r"[a-f0-9]{64}",key)or previous.get("recipe")!=recipe:continue
        old_inputs=previous.get("inputs",{})
        manifests={name:sha for name,sha in inputs.items()if name.endswith("Cargo.toml")or name=="Cargo.lock"}
        if manifests!={name:sha for name,sha in old_inputs.items()if name.endswith("Cargo.toml")or name=="Cargo.lock"}:continue
        source=state/"engine-component-builds"/key
        recorded=previous.get("cargo_cache_hashes")
        if not recorded or source==cache or not all((source/name).is_dir()for name in ("cargo","target")):continue
        actual={name:executor.Executor.artifact_hashes(source/name)for name in ("cargo","target")}
        if actual!=recorded:raise ValueError("Retained engine Cargo cache changed")
        for name in ("cargo","target"):
            if (cache/name).exists():shutil.rmtree(cache/name)
            shutil.copytree(source/name,cache/name)
        if actual!={name:executor.Executor.artifact_hashes(source/name)for name in ("cargo","target")}or actual!={name:executor.Executor.artifact_hashes(cache/name)for name in ("cargo","target")}:
            raise ValueError("Engine Cargo cache changed during reuse")
        return key
    return None
