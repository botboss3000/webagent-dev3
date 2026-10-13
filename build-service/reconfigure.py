"""Root operator reconfiguration of retained binaries; never a model tool."""
import copy,hashlib,importlib.util,json,os,socket,subprocess,time,uuid
from pathlib import Path
def module(name,path):
    """Load an exact reviewed local operator module; callers never choose an application executable."""
    spec=importlib.util.spec_from_file_location(name,path);value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
def plan(previous,candidate):
    """Reject migrations and select only listener, origin or API-resource consumers for retained-image replacement."""
    changed={key for key in previous if previous[key]!=candidate.get(key)}
    allowed={"bind_address","port","public_origin","api_pids_limit"}
    if changed-allowed:raise ValueError("This reconfiguration cannot relocate storage, sockets, identities or installed code")
    roles=set()
    if changed&{"bind_address","port","api_pids_limit"}:roles.add("api")
    if "public_origin"in changed:roles.update(("api","loop"))
    return {"changed_fields":sorted(changed),"services":sorted(roles),"compile_required":False}
def probe_listener(address,port):
    """Reject active listeners while allowing a released listener's residual TCP TIME_WAIT connections."""
    family=socket.AF_INET6 if ":"in address else socket.AF_INET
    with socket.socket(family,socket.SOCK_STREAM)as check:
        check.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
        check.bind((address,port))
def operator(config,path,arguments):
    """Call the protected broker as its configured operator without printing private container settings."""
    result=subprocess.run(["sudo","-n","-u",config["service_account"],"env","WEBAGENT_BUILD_CONFIG="+str(path),"python3","-B",str(Path(config["service_dir"])/"buildctl.py"),*arguments],check=True,stdout=subprocess.PIPE)
    return json.loads(result.stdout)
def ensure_reconciled(state):
    """Refuse a new topology change while an earlier durable reconfiguration outcome remains unresolved."""
    for path in (Path(state)/"reconfiguration").glob("*/receipt.json"):
        value=json.loads(path.read_bytes())
        if value.get("status")not in {"completed","rolled_back","failed_no_application_effects"}:
            raise RuntimeError("Reconcile the retained infrastructure receipt before another reconfiguration")
def apply(config_path,candidate_path,task_id,dry_run=False):
    """Validate protected configuration, reserve an idle drain, and replace only selected services with rollback."""
    if os.geteuid()!=0:raise PermissionError("Infrastructure reconfiguration requires the protected root operator")
    folder=Path(__file__).resolve().parent
    cfg=module("reconfiguration_config",folder/"config.py")
    previous=cfg.load(config_path);candidate=cfg.load(candidate_path)
    change=plan(previous,candidate)
    if dry_run or not change["services"]:return {**change,"status":"validated"}
    ensure_reconciled(previous["state_root"])
    state=operator(previous,config_path,["maintenance","status"])
    if state.get("busy")or state.get("queued")or not state.get("drained")or state.get("task_id")!=task_id:
        raise RuntimeError("Reserve an idle broker maintenance drain for this exact task")
    os.environ["WEBAGENT_BUILD_CONFIG"]=str(config_path)
    host=module("reconfiguration_host",Path(previous["service_dir"])/"host.py")
    runner=host.Runner()
    before={role:runner.inspect(previous[role+"_container"])for role in ("api","loop")}
    if "api"in change["services"]:
        address=candidate["bind_address"]
        if address not in ("0.0.0.0","::")and address!=previous["bind_address"]:
            # Binding establishes whether this host owns the requested interface.
            family=socket.AF_INET6 if ":"in address else socket.AF_INET
            with socket.socket(family,socket.SOCK_STREAM)as check:check.bind((address,0))
        if (candidate["bind_address"],candidate["port"])!=(previous["bind_address"],previous["port"]):
            probe_listener(address,candidate["port"])
    receipt_id=str(uuid.uuid4());root=Path(previous["state_root"])/"reconfiguration"/receipt_id;root.mkdir(parents=True,mode=0o700)
    source_keys=cfg.defaults().keys()
    old_raw={k:v for k,v in previous.items()if k in source_keys}
    new_raw={k:v for k,v in candidate.items()if k in source_keys}
    receipt={**change,"id":receipt_id,"task_id":task_id,"status":"prepared","before":{role:{"id":value["Id"],"image":value["Image"]}for role,value in before.items()}}
    def save():
        temporary=root/"receipt.new";temporary.write_text(json.dumps(receipt,sort_keys=True));temporary.chmod(0o600);os.replace(temporary,root/"receipt.json")
    save()
    lifecycle=module("reconfiguration_lifecycle",folder/"lifecycle.py")
    unit="webagent-build-"+previous["instance"]+".service"
    try:subprocess.run(["systemctl","stop",unit],check=True)
    except BaseException:
        receipt["status"]="failed_no_application_effects";save();raise
    renamed=[];created=[]
    try:
        receipt["status"]="reconfiguring";save()
        # The config archive contains installation topology, never application data.
        lifecycle.atomic(root/"previous.json",old_raw)
        lifecycle.atomic(config_path,new_raw)
        for role in change["services"]:
            name=previous[role+"_container"];backup=name+"-reconfigure-"+receipt_id
            runner.stop(name);runner.rename(name,backup);renamed.append((name,backup))
            selected=copy.deepcopy(before[role])
            if role=="api":
                selected["HostConfig"]["PidsLimit"]=candidate["api_pids_limit"]
                selected["HostConfig"]["PortBindings"]={"8080/tcp":[{"HostIp":candidate["bind_address"],"HostPort":str(candidate["port"])}]}
            selected["Config"]["Env"]=[v for v in selected["Config"].get("Env",[])if not v.startswith("V3_PUBLIC_ORIGIN=")]+["V3_PUBLIC_ORIGIN="+candidate["public_origin"]]
            runner.create(name,before[role]["Image"],selected);created.append(name);runner.start(name)
        for role in change["services"]:
            if not runner.healthy(previous[role+"_container"],before[role]["Image"]):raise RuntimeError("Reconfigured service failed readiness")
        for role in set(before)-set(change["services"]):
            if runner.inspect(previous[role+"_container"])["Id"]!=before[role]["Id"]:raise RuntimeError("Unrelated service changed")
        receipt["status"]="completed";save()
    except BaseException:
        receipt["status"]="rolling_back";save()
        for name in created:
            subprocess.run(["docker","rm","-f",name],check=True,stdout=subprocess.DEVNULL)
        for name,backup in reversed(renamed):runner.rename(backup,name);runner.start(name)
        lifecycle.atomic(config_path,old_raw)
        receipt["status"]="rolled_back";save();raise
    finally:subprocess.run(["systemctl","start",unit],check=True)
    return {**change,"status":receipt["status"],"receipt_id":receipt_id}
