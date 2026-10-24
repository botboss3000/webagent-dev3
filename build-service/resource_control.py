"""Narrow root helper for attributable build containers, never arbitrary host processes."""
import http.client,json,os,re,socket,struct,subprocess,time,threading,hashlib,importlib.util
from pathlib import Path
PHASES={"ui_test","index_scan","compile_test","compile_release","compile_linked","plugin_compile","plugin_test","test","dependency","package","engine_metadata","linked_smoke","live_smoke"}
PARKABLE={"compile_test","compile_release","compile_linked","plugin_compile"}
class Docker(http.client.HTTPConnection):
    """Use the local engine API without shell arguments or returning private container configuration."""
    def connect(self):
        """Open the fixed local Docker Unix transport without exposing its configuration."""
        self.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);self.sock.settimeout(self.timeout);self.sock.connect("/var/run/docker.sock")
def inspect(name,missing_ok=False):
    """Read one scheduler-derived container identity; secrets stay inside the privileged helper."""
    connection=Docker("localhost",timeout=3)
    try:
        connection.request("GET","/containers/"+name+"/json");response=connection.getresponse();body=response.read(1024*1024)
        if response.status==404 and missing_ok:return None
        if response.status!=200:raise ValueError("Scheduler container unavailable")
        return json.loads(body)
    finally:connection.close()
def host_memory():
    """Read global pressure without inspecting application or production data."""
    values={}
    for line in Path("/proc/meminfo").read_text().splitlines():
        key,raw=line.split(":",1);values[key]=int(raw.split()[0])*1024
    return values
# @code-index {"version":1,"feature":"controlled-build-scheduler","aliases":["parallel builds","source ownership","compiler parking"],"purpose":"Protected cgroup controls. Config: build-service/scheduler-defaults.json -> mode, reserve_bytes, reserve_percent; installation overrides at restart.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/mode","/orphan_after_seconds","/reserve_bytes","/reserve_percent","/max_phases","/max_parked","/memory_pressure_percent"]}],"opportunities":[]}}
class Control:
    """Resolve cgroups from exact Docker identities and reject all non-scheduler targets."""
    def __init__(self,config):
        """Bind privileged controls to the reviewed installation instance."""
        self.config=config;self.memory_lock=threading.Lock();self.slice="webagent-build-"+config["instance"]+".slice"
    def pool(self):
        """Resolve only the fixed instance slice, rejecting root and unrelated cgroups."""
        result=subprocess.run(["systemctl","show",self.slice,"--property=ControlGroup","--value"],capture_output=True,timeout=3,check=True)
        relative=result.stdout.decode().strip()
        if not relative.startswith("/")or relative=="/"or".."in Path(relative).parts:raise ValueError("Build slice unavailable")
        return Path("/sys/fs/cgroup")/relative.lstrip("/")
    # @code-index {"version":1,"feature":"compiler-memory-policy","aliases":["compiler memory defaults","task memory overrides","live memory growth"],"purpose":"Protected compiler ceilings and live growth. Config: build-service/scheduler-defaults.json -> compiler_memory_default_bytes, compiler_memory_min_bytes, compiler_memory_max_bytes, allow_live_memory_increase; installation JSON overrides.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/compiler_memory_default_bytes","/compiler_memory_min_bytes","/compiler_memory_max_bytes","/allow_live_memory_increase"]}]}}
    def memory_policy(self):
        """Allow validated live CPU/memory settings; keep topology and other policy fixed."""
        path=os.environ.get("WEBAGENT_BUILD_CONFIG")
        current=self.config
        if path:
            spec=importlib.util.spec_from_file_location("root_memory_policy",Path(self.config["service_dir"])/"config.py");cfg=importlib.util.module_from_spec(spec);spec.loader.exec_module(cfg)
            current=cfg.load(path)
            keys=cfg.RESOURCE_KEYS
            if {k:v for k,v in current.items()if k!="scheduler"}!={k:v for k,v in self.config.items()if k!="scheduler"}or {k:v for k,v in current["scheduler"].items()if k not in keys}!={k:v for k,v in self.config["scheduler"].items()if k not in keys}:raise PermissionError("Non-resource installation change requires maintenance")
        policy=current.get("scheduler",{})
        defaults={"compiler_memory_default_bytes":12*1024**3,"compiler_memory_min_bytes":1024**3,"compiler_memory_max_bytes":16*1024**3,"allow_live_memory_increase":False}
        values={k:policy.get(k,v)for k,v in sorted(defaults.items())}
        return {"values":values,"sha256":hashlib.sha256(json.dumps(values,sort_keys=True,separators=(",",":")).encode()).hexdigest()}

    def initialize(self,required=None):
        """Bound the aggregate pool and acknowledge whether full admitted demand fits host reserve."""
        policy=self.config.get("scheduler",{})
        if policy.get("mode")!="enforce":raise PermissionError("Scheduler enforcement not enabled")
        memory=host_memory();reserve=max(policy.get("reserve_bytes",768*1024**2),memory["MemTotal"]*max(15,policy.get("reserve_percent",10))//100)
        if required is not None and(type(required)is not int or required<=0 or required>memory["MemTotal"]):raise ValueError("Invalid aggregate reservation")
        subprocess.run(["systemctl","start",self.slice],check=True,timeout=5,stdout=subprocess.DEVNULL)
        pool=self.pool();current=int((pool/"memory.current").read_text())if(pool/"memory.current").exists()else 0
        budget=min(memory["MemTotal"]-reserve,current+max(0,memory["MemAvailable"]-reserve))
        if required is not None and required>budget:return {"admitted":False,"memory_budget":budget,"reserve":reserve}
        old=(pool/"memory.max").read_text().strip()if(pool/"memory.max").exists()else"max"
        cpus=max(1,(os.cpu_count()or 1)-1);swap=memory.get("SwapTotal",0)//2
        if old!="max"and required is not None and int(old)>=required:return {"admitted":True,"memory_budget":int(old),"reserve":reserve}
        if budget<current or budget<64*1024**2:return {"admitted":False,"memory_budget":budget,"reserve":reserve}
        subprocess.run(["systemctl","set-property","--runtime",self.slice,
            "CPUQuota="+str(cpus*100)+"%","MemoryMax="+str(budget),
            "MemoryHigh="+str(max(current,budget-256*1024**2)),"MemorySwapMax="+str(swap),
            "TasksAccounting=yes"],check=True,timeout=5,stdout=subprocess.DEVNULL)
        return {"admitted":True,"slice":self.slice,"cpu_budget":cpus,"memory_budget":budget,"reserve":reserve,"swap_budget":swap}

    def seal(self,args,group,phase,identity):
        """Seal only an already enforced zero-swap cap into its exact transient unit."""
        if phase not in {"compile_test","compile_release","compile_linked","plugin_compile","plugin_test","package"}:
            raise PermissionError("Only capped compiler and package phases may be sealed")
        unit="docker-"+identity+".scope"
        if group.name!=unit:raise PermissionError("Seal scope identity changed")
        value=inspect(args["container"]);host=value.get("HostConfig",{})
        if value.get("Id")!=identity or host.get("CgroupParent")!=self.slice:
            raise PermissionError("Seal Docker identity changed")
        cap=host.get("Memory")
        if type(cap)is not int or cap<=0 or type(host.get("MemorySwap"))is not int or host["MemorySwap"]!=cap:
            raise PermissionError("Seal requires an existing finite zero-swap Docker cap")
        before=self.stats(group)
        if before["memory_max"]!=cap or before["swap_max"]!=0:
            raise PermissionError("Seal requires an existing enforced zero-swap kernel cap")
        subprocess.run(["systemctl","set-property","--runtime",unit,"MemorySwapMax=0"],
            check=True,timeout=3,stdout=subprocess.DEVNULL)
        result=subprocess.run(["systemctl","show",unit,"--property=MemorySwapMax","--value"],
            capture_output=True,check=True,timeout=3)
        if result.stdout.decode().strip()!="0":raise ValueError("Zero-swap unit seal not confirmed")
        again,again_phase,again_id=self.resolve(args["container"])
        if (again,again_phase,again_id)!=(group,phase,identity):raise PermissionError("Seal target changed")
        observed=self.stats(group)
        if observed["memory_max"]!=cap or observed["swap_max"]!=0:
            raise PermissionError("Seal changed or lost the existing kernel cap")
        return {"container_id":identity,"phase":phase,"unit_swap_max":0,**observed}

    def increase_memory(self,args,group,phase,identity):
        """Increase one exact compiler limit via Docker; no arbitrary cgroup target or swap growth."""
        if phase not in PARKABLE:raise PermissionError("Only compiler memory may increase")
        policy=self.memory_policy();values=policy["values"];cap=args.get("memory_bytes");old=args.get("previous_bytes")
        if not values["allow_live_memory_increase"]or args.get("policy_sha256")!=policy["sha256"]:raise PermissionError("Memory policy changed or disabled")
        if type(cap)is not int or not values["compiler_memory_min_bytes"]<=cap<=values["compiler_memory_max_bytes"]or cap%os.sysconf("SC_PAGE_SIZE"):raise ValueError("Memory request outside protected policy")
        deadline=args.get("lease_until")
        if type(deadline)not in (int,float)or not __import__("math").isfinite(deadline)or deadline<=time.time():raise PermissionError("Memory authority expired")
        before=self.stats(group)
        if type(old)is not int or old!=before["memory_max"]or cap<=old or before["swap_max"]!=0:raise PermissionError("Memory binding changed; only zero-swap increases allowed")
        required=args.get("reserved_bytes")
        if type(required)is not int or required<cap:raise ValueError("Complete aggregate reservation required")
        memory=host_memory();reserve=max(self.config.get("scheduler",{}).get("reserve_bytes",768*1024**2),memory["MemTotal"]*max(15,self.config.get("scheduler",{}).get("reserve_percent",10))//100)
        if memory["MemAvailable"]-reserve<cap-old:raise PermissionError("Host headroom changed")
        budget=self.initialize(required)
        if not budget["admitted"]:raise PermissionError("Aggregate build boundary cannot admit growth")
        # Recheck exact target after the budget handshake and immediately before Docker changes it.
        again,again_phase,again_id=self.resolve(args["container"])
        if again!=group or again_phase!=phase or again_id!=identity:raise PermissionError("Memory target changed")
        observed=self.stats(group)
        if observed["memory_max"]!=old or observed["swap_max"]!=0:raise PermissionError("Memory ceiling raced another control")
        if time.time()>=deadline:raise PermissionError("Memory authority expired")
        connection=Docker("localhost",timeout=3)
        try:
            body=json.dumps({"Memory":cap,"MemorySwap":cap})
            connection.request("POST","/containers/"+identity+"/update",body,{"Content-Type":"application/json"})
            response=connection.getresponse();response.read(16384)
            if response.status!=200:raise RuntimeError("Docker memory update failed")
        finally:connection.close()
        observed=self.stats(group)
        if observed["memory_max"]!=cap or observed["swap_max"]!=0:raise ValueError("Kernel memory increase not confirmed")
        return {"container_id":identity,"phase":phase,**observed}

    def resolve(self,name):
        """Verify labels, running PID and cgroup ancestry before any read or control write."""
        match=re.fullmatch(re.escape("webagent-build-"+self.config["instance"]+"-")+r"([a-f0-9]{32})-([a-z_]+)-([a-f0-9]{8})",name)
        if not match or match[2]not in PHASES:raise PermissionError("Not a build container")
        value=inspect(name);labels=value.get("Config",{}).get("Labels",{})
        if labels.get("webagent.scheduler.instance")!=self.config["instance"]or labels.get("webagent.scheduler.job")!=match[1]or labels.get("webagent.scheduler.phase")!=match[2]:
            raise PermissionError("Build identity labels changed")
        pid=value["State"].get("Pid",0)
        if type(pid)is not int or pid<=0 or not value["State"].get("Running"):raise ValueError("Build container not running")
        lines=Path("/proc/"+str(pid)+"/cgroup").read_text().splitlines()
        relative=next((line[3:]for line in lines if line.startswith("0::")),None)
        if not relative or ".."in Path(relative).parts:raise ValueError("Unified cgroup unavailable")
        group=Path("/sys/fs/cgroup")/relative.lstrip("/");pool=self.pool()
        # BuildKit moves its daemon into an internal subgroup. Attribute the complete exact container scope.
        scope=next((cursor for cursor in [group,*group.parents]if cursor.name=="docker-"+value["Id"]+".scope"),None)
        if scope is None:raise PermissionError("PID cgroup identity changed")
        if not scope.is_relative_to(pool)or scope==pool:raise PermissionError("Build cgroup escaped slice")
        group=scope
        for cursor in [group,*group.parents]:
            if cursor==Path("/sys/fs/cgroup"):break
            if cursor.is_symlink():raise PermissionError("Cgroup symlink")
        return group,match[2],value["Id"]
    @staticmethod
    def stats(group):
        """Report content-free resource counters and the kernel's actual freeze acknowledgment."""
        def number(name):
            """Read a kernel counter, treating absent optional counters as zero."""
            path=group/name
            return int(path.read_text().strip())if path.exists()else 0
        cpu={line.split()[0]:int(line.split()[1])for line in (group/"cpu.stat").read_text().splitlines()}
        events={line.split()[0]:int(line.split()[1])for line in (group/"cgroup.events").read_text().splitlines()}
        io_values={}
        for line in(group/"io.stat").read_text().splitlines():
            for item in line.split()[1:]:
                key,amount=item.split("=",1);io_values[key]=io_values.get(key,0)+int(amount)
        memory_events=dict(line.split()for line in (group/"memory.events").read_text().splitlines())
        memory_limit=(group/"memory.max").read_text().strip();swap_limit=(group/"memory.swap.max").read_text().strip()
        quota=(group/"cpu.max").read_text().split()if(group/"cpu.max").exists()else None
        cpu_limits={"cpu_limit_millis":None if quota is None or quota[0]=="max"else int(quota[0])*1000/int(quota[1]),
                    "cpu_periods":cpu.get("nr_periods"),"cpu_throttled_periods":cpu.get("nr_throttled"),"cpu_throttled_usec":cpu.get("throttled_usec")}
        return {**cpu_limits,"memory_max":None if memory_limit=="max"else int(memory_limit),"swap_max":None if swap_limit=="max"else int(swap_limit),"oom_events":int(memory_events.get("oom",0)),"oom_kills":int(memory_events.get("oom_kill",0)),"io_read_bytes":io_values.get("rbytes",0),"io_write_bytes":io_values.get("wbytes",0),"memory":number("memory.current"),"peak_memory":number("memory.peak"),"swap":number("memory.swap.current"),"cpu_usec":cpu.get("usage_usec",0),"pids":number("pids.current"),"frozen":bool(events.get("frozen"))}
    def retention(self):
        """Load only the installed root-owned retention implementation selected by protected config."""
        import importlib.util
        path=Path(self.config["service_dir"])/"docker_retention.py"
        s=path.lstat()
        if not path.is_file()or path.is_symlink()or s.st_uid!=0 or s.st_mode&0o022:raise PermissionError("Protected retention implementation required")
        spec=importlib.util.spec_from_file_location("protected_docker_retention",path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
        return m.Retention(self.config)
    def handle(self,args):
        """Operations are bounded and caller-independent; only the authenticated broker can reach them."""
        if os.geteuid()!=0:raise PermissionError("Protected root helper required")
        if isinstance(args,dict)and args.get("operation")in {"retention_pin","disk_admit","disk_finish","retention_status"}:
            operation=args["operation"]
            if operation=="disk_admit"and not self.config.get("scheduler",{}).get("task_aware_cleanup"):raise PermissionError("Task-aware cleanup disabled")
            allowed={"operation","image","job_id"}if operation=="retention_pin"else {"operation"}if operation=="retention_status"else {"operation","container","phase","profile","measured_bytes"}
            if set(args)-allowed:raise ValueError("Unsupported retention arguments")
            retention=self.retention()
            if operation=="retention_pin":return retention.pin(args.get("image"),args.get("job_id"))
            if operation=="retention_status":
                with retention.locked():return {"last_cleanup":retention.load().get("last_cleanup",{}),"free_bytes":retention.usage().free}
            try:return retention.disk(args.get("container",""),args.get("phase"),args.get("profile"),finish=operation=="disk_finish",measured_bytes=args.get("measured_bytes",0))
            except TimeoutError:
                if operation=="disk_admit":return {"admitted":False,"reason":"cleanup_busy"}
                raise
        if not isinstance(args,dict)or set(args)-{"operation","container","bytes","cpu_millis","expected_id","memory_bytes","previous_bytes","reserved_bytes","policy_sha256","required_bytes","lease_until"}:raise ValueError("Unsupported resource arguments")
        operation=args.get("operation")
        if operation=="initialize":
            if set(args)-{"operation","required_bytes"}:raise ValueError("Initialize takes only aggregate demand")
            with self.memory_lock:return self.initialize(args.get("required_bytes"))
        if operation=="memory_policy":
            if set(args)!={"operation"}:raise ValueError("Memory policy takes no target")
            return self.memory_policy()
        if operation=="identity":
            name=args.get("container","")
            match=re.fullmatch(re.escape("webagent-build-"+self.config["instance"]+"-")+r"([a-f0-9]{32})-([a-z_]+)-([a-f0-9]{8})",name)
            if not match or match[2]not in PHASES:raise PermissionError("Not a build container")
            value=inspect(name,missing_ok=True)
            if value is None:return {"missing":True}
            labels=value.get("Config",{}).get("Labels",{})
            if labels.get("webagent.scheduler.instance")!=self.config["instance"]or labels.get("webagent.scheduler.job")!=match[1]or labels.get("webagent.scheduler.phase")!=match[2]or value.get("HostConfig",{}).get("CgroupParent")!=self.slice:
                raise PermissionError("Recovery container identity changed")
            if self.config.get("scheduler",{}).get("task_aware_cleanup"):
                self.retention().pin(value["Image"],str(__import__("uuid").UUID(match[1])),value)
            return {"container_id":value["Id"],"state":value["State"]["Status"],"phase":match[2]}
        if operation not in {"stats","freeze","thaw","reclaim","cpu","memory","seal"}:raise ValueError("Unsupported resource operation")
        group,phase,identity=self.resolve(args.get("container",""))
        if operation!="stats"and args.get("expected_id")!=identity:raise PermissionError("Container identity changed")
        if operation!="stats"and self.config.get("scheduler",{}).get("mode")!="enforce":raise PermissionError("Resource control disabled")
        if operation=="seal":
            if set(args)!={"operation","container","expected_id"}:raise ValueError("Exact seal arguments required")
            with self.memory_lock:return self.seal(args,group,phase,identity)
        if operation=="memory":
            if set(args)!={"operation","container","memory_bytes","previous_bytes","reserved_bytes","policy_sha256","expected_id","lease_until"}:raise ValueError("Exact memory update arguments required")
            with self.memory_lock:return self.increase_memory(args,group,phase,identity)
        if operation in {"freeze","reclaim"}and phase not in PARKABLE:raise PermissionError("This phase cannot be parked")
        if operation in {"freeze","thaw"}:
            (group/"cgroup.freeze").write_text("1"if operation=="freeze"else"0")
            deadline=time.monotonic()+2;desired=operation=="freeze"
            while time.monotonic()<deadline:
                if self.stats(group)["frozen"]==desired:break
                time.sleep(.02)
            else:raise TimeoutError("Freeze acknowledgment unavailable")
        if operation=="reclaim":
            amount=args.get("bytes")
            if type(amount)is not int or not 1<=amount<=256*1024**2:raise ValueError("Bounded reclaim required")
            if not self.stats(group)["frozen"]:raise PermissionError("Freeze before reclaim")
            # A partial reclaim is normal. The controller credits measured headroom only.
            try:
                try:(group/"memory.reclaim").write_text(str(amount)+" swappiness=200")
                except OSError as error:
                    if error.errno!=22:raise
                    (group/"memory.reclaim").write_text(str(amount))
            except OSError as error:
                if error.errno!=11:raise
        if operation=="cpu":
            millis=args.get("cpu_millis")
            if type(millis)is not int or not 100<=millis<=max(1000,(os.cpu_count()or 1)*1000):raise ValueError("Bounded CPU allocation required")
            (group/"cpu.max").write_text(str(millis*100)+" 100000")
            if self.stats(group)["cpu_limit_millis"]!=millis:raise ValueError("CPU quota readback differs from allocation")
        return {"container_id":identity,"phase":phase,**self.stats(group)}

def serve(listener,stopping,config):
    """Authenticate the dedicated helper socket; a blocked reclaim cannot stall source snapshots."""
    import concurrent.futures,threading
    control=Control(config);pool=concurrent.futures.ThreadPoolExecutor(max_workers=4);slots=threading.BoundedSemaphore(4)
    def client(connection):
        """Authenticate one bounded broker request and release its worker slot after replying."""
        try:
            connection.settimeout(5)
            _,uid,_=struct.unpack("3i",connection.getsockopt(socket.SOL_SOCKET,socket.SO_PEERCRED,12))
            if uid!=config["service_uid"]:raise PermissionError("Resource caller denied")
            raw=bytearray()
            while not raw.endswith(b"\n")and len(raw)<=2048:
                part=connection.recv(2049-len(raw))
                if not part:raise ValueError("Incomplete resource request")
                raw.extend(part)
            if len(raw)>2048:raise ValueError("Resource frame oversized")
            result=control.handle(json.loads(raw))
            connection.sendall(json.dumps({"status":"success","output":result},separators=(",",":")).encode()+b"\n")
        except (OSError,ValueError,PermissionError,RuntimeError,TimeoutError,subprocess.SubprocessError):
            try:connection.sendall(b'{"status":"failed","output":"Resource control unavailable"}\n')
            except OSError:pass
        finally:connection.close();slots.release()
    listener.settimeout(.5)
    try:
        while not stopping.is_set():
            try:connection,_=listener.accept()
            except socket.timeout:continue
            except OSError:break
            if not slots.acquire(blocking=False):connection.close();continue
            pool.submit(client,connection)
    finally:pool.shutdown(wait=False,cancel_futures=False)
