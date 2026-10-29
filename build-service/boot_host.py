"""Trusted v3 boot authority: event transport, persistence, grants and Docker effects.

Only boot.plan selection is replaceable. No production/v2 names or remote HTTP
probes are accepted. Config: protected build-service.json fixes all target names;
the live file crates/modules/engine/engine-components.json owns disabled/revision. Host state owns manual
and uncertain pauses. Healthy idle has no scan timer.
"""
import ctypes,hashlib,http.client,json,os,selectors,socket,struct,subprocess,threading,time
from pathlib import Path
from urllib.parse import quote
import config,engine_components,uuid,fcntl,contextlib,signal

def bounded_json(path,limit=65536):
    """Reject oversized or redirected control metadata before parsing its contract."""
    if path.is_symlink() or not path.is_file() or path.stat().st_size>limit:raise ValueError("Boot metadata bounds")
    raw=path.read_bytes()
    if len(raw)>limit:raise ValueError("Boot metadata raced bounds")
    return json.loads(raw)

LIMIT=65536
TERMINAL={"completed","failed","stale","cancelled","interrupted","uncertain","needs_decision"}

def atomic(path,value):
    """Durably replace host-owned bounded state without exposing private controls."""
    temporary=path.with_suffix(".new")
    with open(temporary,"w",encoding="utf-8") as stream:
        os.chmod(temporary,0o600);json.dump(value,stream);stream.flush();os.fsync(stream.fileno())
    os.replace(temporary,path)

class Docker:
    """Fixed-target Docker Unix transport; never evaluates commands from component replies."""
    def __init__(self,names):
        self.names=tuple(names)
    def connection(self,timeout=10):
        connection=http.client.HTTPConnection("localhost",timeout=timeout)
        connection.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
        connection.sock.settimeout(timeout);connection.sock.connect("/var/run/docker.sock")
        return connection
    def inspect(self,name):
        assert name in self.names
        connection=self.connection()
        try:
            connection.request("GET","/containers/"+quote(name,safe="")+"/json")
            response=connection.getresponse()
            if response.status==404:return {"exists":False}
            if response.status!=200:raise OSError("Docker observation unavailable")
            raw=response.read(2*1024*1024+1)
            if len(raw)>2*1024*1024:raise ValueError("Docker observation bounds")
            value=json.loads(raw);state=value["State"]
            # Docker can start a healthy process after a failed boot-time bind
            # while retaining no bridge endpoint or published port. Internal
            # HEALTHCHECK alone cannot certify the configured ingress path.
            host=value.get("HostConfig",{});network=value.get("NetworkSettings",{})
            bridge_missing=state["Running"] and host.get("NetworkMode")=="bridge" and not network.get("Networks",{}).get("bridge",{}).get("IPAddress")
            published=network.get("Ports") or {}
            port_missing=state["Running"] and any(binding not in (published.get(port) or [])
                for port,bindings in (host.get("PortBindings") or {}).items() for binding in (bindings or []))
            network_fault=bridge_missing or port_missing
            return {"exists":True,"id":value["Id"],"running":state["Running"],
                "restarting":state["Restarting"],"started":state["StartedAt"],
                "health":"unhealthy" if state["Running"] and network_fault else state.get("Health",{}).get("Status",""),
                "bridge_missing":bridge_missing,"network_fault":network_fault}
        finally:connection.close()
    def action(self,name,operation,expected):
        """Recheck container identity/start generation immediately before a permitted effect."""
        assert name in self.names and operation in {"start","stop","restart"}
        if self.inspect(name)!=expected:raise RuntimeError("Container generation changed")
        connection=self.connection(timeout=40)
        try:
            if operation in {"start","restart"} and expected.get("bridge_missing"):
                # Repair only Docker's configured default bridge, under the same
                # generation fence/effect lease. An uncertain connect is never retried.
                connection.request("POST","/networks/bridge/connect",
                    body=json.dumps({"Container":expected["id"]}),headers={"Content-Type":"application/json"})
                response=connection.getresponse();response.read(4096)
                if response.status!=200:raise RuntimeError("Docker network repair failed or uncertain")
                connection.close();connection=self.connection(timeout=40)
                repaired=self.inspect(name)
                if any(repaired.get(k)!=expected.get(k) for k in ("id","started","running","restarting")) or repaired.get("bridge_missing"):
                    raise RuntimeError("Container changed during network repair")
            connection.request("POST","/containers/"+quote(expected["id"],safe="")+"/"+operation+"?t=10")
            response=connection.getresponse();response.read(4096)
            if response.status not in {204,304}:raise RuntimeError("Docker effect failed or uncertain")
        finally:connection.close()
    def watch(self,wake):
        """Block on lifecycle/health transitions; reconnect only after actual transport failure."""
        filters=json.dumps({"type":["container"],"container":list(self.names),
            "event":["start","stop","die","destroy","create","restart","health_status"]},separators=(",",":"))
        while True:
            try:
                connection=self.connection(timeout=None)
                connection.request("GET","/events?since="+str(int(time.time())-2)+"&filters="+quote(filters,safe=""))
                response=connection.getresponse()
                if response.status!=200:raise OSError("Docker events unavailable")
                wake()
                while True:
                    raw=response.readline(65537)
                    if not raw or len(raw)>65536:raise OSError("Docker event stream lost")
                    event=json.loads(raw)
                    if event.get("Actor",{}).get("Attributes",{}).get("name") in self.names:wake()
            except (OSError,ValueError,http.client.HTTPException):
                wake();time.sleep(30)
            finally:
                if "connection" in locals():connection.close()

class Engine:
    """Validate sealed publications and retire old confined processes before adopting new bytes."""
    def __init__(self,cfg,home):
        self.cfg=cfg;self.home=home;self.process=None;self.identity=None;self.revision=None;self.control_revision=None;self.required=False
    def retire(self):
        if self.process:
            self.process.stdin.close()
            try:self.process.wait(timeout=.2)
            except subprocess.TimeoutExpired:os.killpg(self.process.pid,signal.SIGKILL);self.process.wait()
            self.process=None
    def adopt(self):
        """Invalid/disabled/withdrawn packages pause live effects; no linked fallback."""
        root=(Path(self.cfg["state_root"])/"ui-assets/current/v3").resolve(strict=True)
        namespace=bounded_json(root/"plugins/engine/catalog.json")
        if namespace.get("schema_version")!=1 or len(namespace.get("components",[]))>256 or len(namespace.get("withdrawn",[]))>256:raise ValueError("Boot membership bounds")
        uuid.UUID(namespace["revision"])
        ids=[item["id"] for item in namespace["components"]]
        if ids!=sorted(set(ids)) or "boot-supervisor" in ids and "boot-supervisor" in namespace["withdrawn"]:raise ValueError("Boot membership conflict")
        controls=bounded_json(Path(self.cfg["checkout_root"])/"crates/modules/engine/engine-components.json",32768)
        if type(controls.get("revision"))is not int or not isinstance(controls.get("disabled"),list) or len(controls["disabled"])>256:raise ValueError("Boot control bounds")
        if namespace["revision"]!=root.parent.name:raise ValueError("Boot publication revision mismatch")
        revision=namespace["revision"];control_revision=controls["revision"]
        members=[x for x in namespace["components"] if x["id"]=="boot-supervisor"]
        if not members:
            self.required=False
            if (root/"plugins/engine/boot-supervisor").exists():raise ValueError("Unregistered boot package")
            self.retire();self.identity=None;self.revision=revision;self.control_revision=control_revision;return False
        path=root/"plugins/engine/boot-supervisor/component.json"
        descriptor=engine_components.descriptor(path);member=members[0]
        binary=path.parent/descriptor["executable"]
        if binary.is_symlink() or not binary.is_file() or binary.stat().st_size>32*1024*1024:raise ValueError("Boot artifact identity")
        digest=hashlib.sha256(binary.read_bytes()).hexdigest()
        if member["descriptor_sha256"]!=hashlib.sha256(path.read_bytes()).hexdigest() or member["artifact_sha256"]!=digest or descriptor.get("artifact_sha256")!=digest:
            raise ValueError("Boot publication changed")
        self.revision=revision;self.control_revision=control_revision
        if "boot-supervisor" in controls["disabled"]:
            self.required=False
            self.retire();self.identity=None;return False
        self.required=True
        identity=(digest,descriptor["implementation_version"])
        if self.identity!=identity:self.retire();self.identity=identity
        if self.process is None:
            # Root installs verified bytes in a protected traversable directory;
            # the child then drops every group/UID and receives no ambient environment.
            target=self.home/("artifact-"+digest)
            if not target.exists():target.write_bytes(binary.read_bytes());target.chmod(0o555)
            if hashlib.sha256(target.read_bytes()).hexdigest()!=digest:raise ValueError("Cached boot artifact changed")
            launcher=self.home/"launcher"
            receipt=bounded_json(self.home/"launcher.json")
            if launcher.is_symlink() or launcher.stat().st_uid!=0 or launcher.stat().st_mode&0o022 or hashlib.sha256(launcher.read_bytes()).hexdigest()!=receipt["sha256"]:
                raise ValueError("Boot host launcher identity")
            self.process=subprocess.Popen([str(launcher),str(target)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,env={},close_fds=True,group=self.cfg["app_gid"],
                user=self.cfg["app_uid"],extra_groups=[],start_new_session=True)
        return True
    def plan(self,request):
        """Bound private-pipe request/reply and identity; deadline failure retires the child."""
        raw=json.dumps({"version":1,"id":"boot","run_key":"host","operation":"prepare","input":request}).encode()+b"\n"
        if len(raw)>LIMIT:raise ValueError("Boot input bounds")
        try:
            self.process.stdin.write(raw);self.process.stdin.flush()
            reply=b"";deadline=time.monotonic()+1
            with selectors.DefaultSelector() as selector:
                selector.register(self.process.stdout,selectors.EVENT_READ)
                while not reply.endswith(b"\n"):
                    if not selector.select(max(0,deadline-time.monotonic())):raise TimeoutError("Boot planner deadline")
                    chunk=os.read(self.process.stdout.fileno(),min(4096,LIMIT+1-len(reply)))
                    if not chunk:raise ValueError("Boot service exited")
                    reply+=chunk
                    if len(reply)>LIMIT:raise ValueError("Boot reply bounds")
            if len(reply)>LIMIT or not reply.endswith(b"\n"):raise ValueError("Boot reply bounds")
            value=json.loads(reply)
            if value["version"]!=1 or value["id"]!="boot" or value["result"]["implementation_version"]!=self.identity[1]:raise ValueError("Boot reply identity")
            return value["result"]["plan"]
        except BaseException:self.retire();raise

class Host:
    """Own recovery inputs, retry/crash deadlines, selective restoration and manual authority."""
    def __init__(self,cfg,home,docker=None,engine=None):
        self.cfg=cfg;self.home=home;home.mkdir(parents=True,exist_ok=True);home.chmod(0o755)
        self.names=tuple(cfg[k] for k in ("api_container","loop_container","gate_container"))
        self.docker=docker or Docker(self.names);self.engine=engine or Engine(cfg,home)
        self.path=home/"state.json"
        self.lock_path=home/"effects.lock"
        fd=os.open(self.lock_path,os.O_RDONLY|os.O_CREAT|os.O_NOFOLLOW,0o644)
        os.close(fd)
        self.state=bounded_json(self.path) if self.path.exists() else {"version":1,"services":{},"pending":True}
        if self.state.get("version")!=1:raise ValueError("Boot host state version")
        for name in self.names:self.state["services"].setdefault(name,{"desired":True,"uncertain":False,"attempts":[],"after":0,"unhealthy_since":None,"healthy_since":None})
        self.observations={};self.paused=True;self.deadline=None
        self.host_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    @contextlib.contextmanager
    def effect_lease(self):
        """Serialize Docker effects against the broker's publication/deployment lease."""
        fd=os.open(self.lock_path,os.O_RDONLY|os.O_NOFOLLOW)
        try:
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
            yield
        finally:os.close(fd)
    def recover(self,name,operation,observation,wall,generation):
        """Revalidate authority under the shared lease before durably admitting a recovery."""
        try:
            with self.effect_lease():
                if self.busy() or not self.engine.adopt() or self.engine.identity!=generation:return
                rt=self.state["services"][name]
                rt["attempts"].append(wall);rt["after"]=wall+30000;rt["uncertain"]=True
                atomic(self.path,self.state)
                try:self.docker.action(name,operation,observation);rt["uncertain"]=False
                except (OSError,RuntimeError,http.client.HTTPException):pass
        except BlockingIOError:
            # A real fault was ready while deployment held its lease: one bounded retry.
            self.deadline=min(self.deadline or time.monotonic()*1000+1000,time.monotonic()*1000+1000)
    def busy(self):
        """Any unfinished broker release fences autonomous container effects."""
        for path in Path(self.cfg["state_root"]).glob("*.json"):
            if path.name=="maintenance.json":continue
            try:
                job=json.loads(path.read_bytes())
                if job.get("job_id") and job.get("status") not in TERMINAL:return True
            except (OSError,ValueError):return True
        return False
    def snapshot(self):
        return {"version":1,"mode":"live","revision":self.engine.revision,
            "host_sha256":self.host_sha256,"required":self.engine.required,
            "controls_revision":self.engine.control_revision,"component_paused":self.paused,
            "pending":self.state["pending"],"services":{name:{**self.state["services"][name],
            "observation":self.observations.get(name,{})} for name in self.names},
            "artifact_sha256":self.engine.identity[0] if self.engine.identity else None}
    def reconcile(self,now=None):
        """Retain pending input while unavailable; fresh host predicates fence restored effects."""
        now=time.monotonic()*1000 if now is None else now
        wall=time.time()*1000;self.deadline=None;self.state["pending"]=True
        available=False
        try:available=self.engine.adopt()
        except (OSError,ValueError,KeyError):self.engine.retire();self.deadline=now+30000
        self.paused=not available
        busy=self.busy()
        projection=[]
        for name in self.names:
            rt=self.state["services"][name]
            try:o=self.docker.inspect(name)
            except (OSError,ValueError,http.client.HTTPException):o={"unknown":True}
            self.observations[name]=o
            health=o.get("health");unhealthy=health=="unhealthy"
            if health=="healthy" and rt["attempts"]:
                if rt.get("healthy_since") is None:rt["healthy_since"]=wall
                reset=rt["healthy_since"]+120000
                if wall>=reset:rt["attempts"]=[];rt["healthy_since"]=None
                else:self.deadline=min(self.deadline or now+120000,now+reset-wall)
            else:rt["healthy_since"]=None
            if unhealthy:
                if rt["unhealthy_since"] is None:rt["unhealthy_since"]=wall
            else:rt["unhealthy_since"]=None
            # Wall-clock deadlines survive host restarts; projection uses monotonic time.
            due=max(rt["after"],(rt["unhealthy_since"] or 0)+45000 if unhealthy else 0)
            recent=[t for t in rt["attempts"] if wall-t<3600000]
            state="unknown" if o.get("unknown") else "missing" if not o.get("exists") else "restarting" if o.get("restarting") else "running" if o.get("running") else "stopped"
            projection.append({"state":state,"unhealthy":unhealthy,
                "eligible":rt["desired"] and not rt["uncertain"] and len(recent)<3 and not busy,
                "dependency_ready":True,"safe_after_ms":int(now+max(0,due-wall))})
        if available:
            try:
                request={"version":1,"now_ms":int(now),"services":projection}
                plan=self.engine.plan(request)
                if set(plan)!={"version","actions","next_deadline_ms"} or type(plan["version"])is not int or plan["version"]!=1:raise ValueError("Boot plan contract")
                seen=set()
                for action in plan["actions"]:
                    index=action["index"];operation=action["operation"]
                    if type(index)is not int or index in seen or not 0<=index<len(self.names):raise ValueError("Boot action position")
                    seen.add(index);p=projection[index]
                    expected="start" if p["state"]=="stopped" else "restart" if p["state"]=="running" and p["unhealthy"] else None
                    if operation!=expected or not p["eligible"] or not p["dependency_ready"] or p["safe_after_ms"]>now:raise ValueError("Boot effect denied")
                t=plan["next_deadline_ms"]
                if t is not None and (type(t)is not int or t<=now or not any(p["eligible"] and p["safe_after_ms"]==t and (p["state"]=="stopped" or p["state"]=="running" and p["unhealthy"]) for p in projection)):raise ValueError("Boot deadline denied")
                if t is not None:self.deadline=min(self.deadline or t,t)
                for action in plan["actions"]:
                    name=self.names[action["index"]];rt=self.state["services"][name]
                    self.recover(name,action["operation"],self.observations[name],wall,self.engine.identity)
                # Retain real faults even when an active release fences their eligibility.
                faults=any(self.state["services"][name]["desired"] and not self.state["services"][name]["uncertain"]
                    and len([t for t in self.state["services"][name]["attempts"] if wall-t<3600000])<3
                    and (p["state"] in {"unknown","stopped"} or p["state"]=="running" and p["unhealthy"])
                    for name,p in zip(self.names,projection))
                self.state["pending"]=bool(plan["actions"] or t is not None or faults)
            except (OSError,ValueError,KeyError,TypeError,TimeoutError,BrokenPipeError):
                self.paused=True;self.engine.retire();self.deadline=now+30000
        # Actual Docker unavailability needs retry; healthy and deliberate withdrawal do not.
        if any(o.get("unknown") for o in self.observations.values()):self.deadline=now+30000
        atomic(self.path,self.state);atomic(self.home/"adoption.json",self.snapshot())
    def needs_job_refresh(self):
        """Job writes matter only for retained faults; healthy idle ignores compiler log churn."""
        return self.state["pending"] and not self.paused
    def manual(self,name,operation):
        """Authenticated host UI control never accepts names outside protected v3 configuration."""
        if name not in self.names or operation not in {"start","stop","restart"}:raise ValueError("Boot control denied")
        with self.effect_lease():
            if self.busy():raise ValueError("Release owns containers")
            rt=self.state["services"][name];rt.update(desired=operation!="stop",uncertain=True,attempts=[],after=0)
            atomic(self.path,self.state);expected=self.docker.inspect(name)
            self.docker.action(name,operation,expected);rt["uncertain"]=False;atomic(self.path,self.state)

def serve(cfg):
    """Block on Docker events, protected catalog/control changes or authenticated local UI."""
    home=Path(cfg["install_root"]).parent/"boot-supervisor";host=Host(cfg,home)
    wake_read,wake_write=socket.socketpair();wake_read.setblocking(False);wake_write.setblocking(False)
    def wake():
        try:wake_write.send(b"x")
        except BlockingIOError:pass
    threading.Thread(target=host.docker.watch,args=(wake,),daemon=True).start()
    libc=ctypes.CDLL(None,use_errno=True);fd=libc.inotify_init1(os.O_NONBLOCK|os.O_CLOEXEC)
    if fd<0:raise OSError("Boot event watch unavailable")
    watches={}
    for kind,path in [("publication",Path(cfg["state_root"])/"ui-assets"),("controls",Path(cfg["checkout_root"])/"data/config"),("jobs",Path(cfg["state_root"]))]:
        watch=libc.inotify_add_watch(fd,os.fsencode(path),0x00000008|0x00000080|0x00000100|0x00000200)
        if watch<0:raise OSError("Boot watch registration failed")
        watches[watch]=kind
    path=home/"ui.sock";path.unlink(missing_ok=True)
    listener=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);listener.bind(str(path));listener.listen(4)
    os.chown(path,0,cfg["source_uid"]);os.chmod(path,0o660)
    with selectors.DefaultSelector() as selector:
        for stream in (wake_read,fd,listener):selector.register(stream,selectors.EVENT_READ)
        host.reconcile()
        while True:
            timeout=None if host.deadline is None else max(0,(host.deadline-time.monotonic()*1000)/1000)
            events=selector.select(timeout);changed=not events
            for key,_ in events:
                if key.fileobj is listener:
                    connection,_=listener.accept();connection.settimeout(1)
                    try:
                        _,uid,_=struct.unpack("3i",connection.getsockopt(socket.SOL_SOCKET,socket.SO_PEERCRED,12))
                        if uid not in {0,cfg["source_uid"],cfg["service_uid"]}:raise PermissionError("Boot peer denied")
                        raw=connection.makefile("rb").readline(LIMIT+1)
                        if len(raw)>LIMIT or not raw.endswith(b"\n"):raise ValueError("Boot UI bounds")
                        request=json.loads(raw)
                        if request.get("operation")=="manual":
                            if uid not in {0,cfg["source_uid"]}:raise PermissionError("Boot control peer denied")
                            host.manual(request["name"],request["action"]);changed=True
                        elif request.get("operation")!="status":raise ValueError("Boot operation denied")
                        connection.sendall(json.dumps({"status":"success","output":host.snapshot()}).encode()+b"\n")
                    except (OSError,ValueError,KeyError,PermissionError,RuntimeError):
                        try:connection.sendall(b'{"status":"error"}\n')
                        except OSError:pass
                    finally:connection.close()
                else:
                    if key.fileobj is wake_read:
                        try:os.read(key.fd,65536)
                        except BlockingIOError:pass
                        changed=True
                    else:
                        try:data=os.read(key.fd,65536)
                        except BlockingIOError:data=b""
                        offset=0
                        while offset+16<=len(data):
                            watch,mask,cookie,length=struct.unpack_from("iIII",data,offset);offset+=16+length
                            if watches.get(watch)!="jobs" or host.needs_job_refresh():changed=True
            if changed:host.reconcile()

if __name__=="__main__":serve(config.load())
