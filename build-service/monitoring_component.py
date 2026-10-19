"""Fixed standalone System Monitoring build/publication contract; no runtime activation.

Independent source is never imported into the application Cargo workspace.
The host/API bridge and service activation require separate reviewed integration.
"""
import hashlib,json,os,re,shutil,tomllib
from pathlib import Path
PREFIX="v3/services/system-monitoring/"
BINARY="webagent-system-monitoring"
RECIPE="system-monitoring-v1:rust1.90:locked:offline"
def package_path(path):
    """Recognize only this registered source subtree; generated artifacts are never source."""
    return path.startswith(PREFIX)

def only_paths(paths):
    """A service-only artifact publication needs no model-callable ability runtime."""
    return bool(paths)and all(package_path(path)for path in paths)

def inventory(folder):
    """Confine compiler inputs and reject ambient Cargo configuration, links and generated output."""
    if folder.is_symlink()or not folder.is_dir():raise ValueError("Monitoring source directory required")
    result={};size=0
    for path in sorted(folder.rglob("*")):
        if path.is_symlink():raise ValueError("Monitoring source symlink")
        if path.is_dir():continue
        name=path.relative_to(folder).as_posix()
        if not path.is_file()or path.stat().st_nlink!=1:raise ValueError("Monitoring regular source required")
        if not(name in {"Cargo.toml","Cargo.lock","component.json","config.default.json"}or name.startswith(("src/","tests/","protocol/","packaging/"))):raise ValueError("Unexpected monitoring source")
        data=path.read_bytes();size+=len(data)
        if len(data)>1024*1024 or size>8*1024*1024 or len(result)>=256:raise ValueError("Monitoring source bounds")
        if path.name=="build.rs":raise ValueError("Monitoring build scripts require recipe review")
        result[name]=hashlib.sha256(data).hexdigest()
    for name in ("Cargo.toml","Cargo.lock","component.json","src/main.rs"):
        if name not in result:raise ValueError("Incomplete monitoring component")
    manifest=tomllib.loads((folder/"Cargo.toml").read_text())
    package=manifest.get("package",{})
    if package.get("name")!=BINARY or package.get("edition")!="2021"or package.get("build",False)is not False:raise ValueError("Monitoring Cargo identity")
    if manifest.get("workspace")!={}or manifest.get("patch")or manifest.get("replace")or manifest.get("lib")or manifest.get("bin"):raise ValueError("Monitoring standalone workspace required")
    groups=[manifest.get(k,{})for k in ("dependencies","dev-dependencies","build-dependencies")]
    for target in manifest.get("target",{}).values():groups.extend(target.get(k,{})for k in ("dependencies","dev-dependencies","build-dependencies"))
    if manifest.get("build-dependencies"):raise ValueError("Monitoring build dependencies require recipe review")
    for group in groups:
        for dep in group.values():
            if isinstance(dep,dict)and any(k in dep for k in ("path","git","workspace","registry")):raise ValueError("Monitoring dependency escapes independent registry closure")
    declaration=json.loads((folder/"component.json").read_bytes())
    expected={"schema_version":1,"id":"system-monitoring","interface_version":1,"binary":BINARY,"toolchain":"1.90.0"}
    if declaration!=expected or type(declaration.get("schema_version"))is not int or type(declaration.get("interface_version"))is not int:raise ValueError("Monitoring descriptor contract")
    return result

def build(runner,job_id,root,state,assets,executor,scheduler):
    """Test/build only this own-workspace crate through protected measured compiler admission."""
    folder=root/PREFIX
    if not folder.exists():return []
    inputs=inventory(folder)
    key=hashlib.sha256(json.dumps({"inputs":inputs,"recipe":RECIPE,"toolchain":executor.RUST},sort_keys=True).encode()).hexdigest()
    with runner.plugin_build_lock(key):
        artifact=assets/"artifacts"/key;binary=artifact/BINARY;receipt=artifact/"monitoring-receipt.json"
        reused=False
        if receipt.exists():
            if receipt.is_symlink()or binary.is_symlink()or not binary.is_file():raise ValueError("Monitoring artifact identity")
            record=json.loads(receipt.read_bytes())
            if record.get("inputs")!=inputs or record.get("recipe")!=RECIPE or record.get("artifact_sha256")!=hashlib.sha256(binary.read_bytes()).hexdigest():raise ValueError("Monitoring artifact changed")
            reused=True
        else:
            job=getattr(runner.job_context,"job",None)
            if not job or job["job_id"]!=job_id or root!=Path(job["release_root"]):raise ValueError("Owned frozen monitoring source required")
            cache=state/"monitoring-builds"/key;cache.mkdir(parents=True,exist_ok=True);source=cache/"source"
            if source.exists():shutil.rmtree(source)
            shutil.copytree(folder,source);(cache/"cargo").mkdir(exist_ok=True)
            runner.executor.dependencies(job,source,cache,prepare=True)
            for command in (["cargo","test","--locked","--offline"],["cargo","build","--release","--locked","--offline","--bin",BINARY]):
                runner.executor.phase(job,"plugin_compile","plugin",source,cache,command,RECIPE,600,network=False)
            if inventory(folder)!=inputs or inventory(source)!=inputs:raise ValueError("Monitoring build inputs changed")
            compiled={}
            for name in executor.Executor.compiler_dependencies(cache/"target"):
                name=executor.Executor.local_dependency(name)
                if name is None:continue
                name=name.removeprefix("v3/")
                if name.startswith("target/"):continue
                if name not in inputs:raise ValueError("Undeclared monitoring compiler input")
                compiled[name]=inputs[name]
            if not any(name.startswith("src/")for name in compiled):raise ValueError("Missing monitoring compiler attestation")
            output=cache/"target/release"/BINARY
            if output.is_symlink()or not output.is_file()or output.stat().st_size>32*1024*1024:raise ValueError("Monitoring output identity")
            artifact.mkdir(parents=True,exist_ok=True);shutil.copyfile(output,binary);binary.chmod(0o555)
            scheduler.Scheduler.write_record(receipt,{"inputs":inputs,"recipe":RECIPE,"compiled_inputs":compiled,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest()})
    return [{"component":"system-monitoring","build_key":key,"artifact_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"reused":reused}]

def seal(destination,outputs,assets):
    """Publish immutable executable plus receipt; never start a host process or touch live source."""
    for output in outputs:
        binary=assets/"artifacts"/output["build_key"]/BINARY
        if binary.is_symlink()or hashlib.sha256(binary.read_bytes()).hexdigest()!=output["artifact_sha256"]:raise ValueError("Monitoring artifact changed before sealing")
        target=destination/PREFIX/"artifact"/BINARY;target.parent.mkdir(parents=True,exist_ok=True)
        if target.exists():
            if target.is_symlink()or hashlib.sha256(target.read_bytes()).hexdigest()!=output["artifact_sha256"]:raise ValueError("Monitoring sealed output changed")
        else:shutil.copyfile(binary,target);target.chmod(0o555)
        receipt=target.parent/"publication.json"
        stable={k:output[k]for k in ("component","build_key","artifact_sha256")}
        encoded=json.dumps(stable,sort_keys=True)+"\n"
        if receipt.exists():
            if receipt.is_symlink()or receipt.read_text()!=encoded:raise ValueError("Monitoring sealed receipt changed")
        else:receipt.write_text(encoded);receipt.chmod(0o444)
