"""Narrow root helper for attributable build containers, never arbitrary host processes."""
import http.client,json,os,re,socket,struct,subprocess,time
from pathlib import Path
PHASES={"compile_test","compile_release","plugin_compile","test","dependency","package"}
PARKABLE={"compile_test","compile_release","plugin_compile"}
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
        self.config=config;self.slice="webagent-build-"+config["instance"]+".slice"
    def pool(self):
        """Resolve only the fixed instance slice, rejecting root and unrelated cgroups."""
        result=subprocess.run(["systemctl","show",self.slice,"--property=ControlGroup","--value"],capture_output=True,timeout=3,check=True)
        relative=result.stdout.decode().strip()
        if not relative.startswith("/")or relative=="/"or".."in Path(relative).parts:raise ValueError("Build slice unavailable")
        return Path("/sys/fs/cgroup")/relative.lstrip("/")
    def initialize(self):
        """Give the aggregate build pool a protected CPU budget and emergency memory boundary."""
        policy=self.config.get("scheduler",{})
        if policy.get("mode")!="enforce":raise PermissionError("Scheduler enforcement not enabled")
        subprocess.run(["systemctl","start",self.slice],check=True,timeout=5,stdout=subprocess.DEVNULL)
        memory=host_memory();reserve=max(policy.get("reserve_bytes",768*1024**2),memory["MemTotal"]*policy.get("reserve_percent",10)//100)
        cpus=max(1,(os.cpu_count()or 1)-1);pool=self.pool()
        current=int((pool/"memory.current").read_text())if(pool/"memory.current").exists()else 0
        budget=current+max(64*1024**2,memory["MemAvailable"]-reserve);swap=int(memory.get("SwapTotal",0)//2)
        # CPUAccounting is deprecated on current systemd; explicit resource properties enable controllers.
        subprocess.run(["systemctl","set-property","--runtime",self.slice,
            "CPUQuota="+str(cpus*100)+"%","MemoryMax="+str(budget),
            "MemoryHigh="+str(max(current,budget-256*1024**2)),"MemorySwapMax="+str(swap),
            "TasksAccounting=yes"],check=True,timeout=5,stdout=subprocess.DEVNULL)
        return {"slice":self.slice,"cpu_budget":cpus,"memory_budget":budget,"reserve":reserve,"swap_budget":swap}
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
        return {"io_read_bytes":io_values.get("rbytes",0),"io_write_bytes":io_values.get("wbytes",0),"memory":number("memory.current"),"peak_memory":number("memory.peak"),"swap":number("memory.swap.current"),"cpu_usec":cpu.get("usage_usec",0),"pids":number("pids.current"),"frozen":bool(events.get("frozen"))}
    def handle(self,args):
        """Operations are bounded and caller-independent; only the authenticated broker can reach them."""
        if os.geteuid()!=0:raise PermissionError("Protected root helper required")
        if not isinstance(args,dict)or set(args)-{"operation","container","bytes","cpu_millis","expected_id"}:raise ValueError("Unsupported resource arguments")
        operation=args.get("operation")
        if operation=="initialize":
            if set(args)!={"operation"}:raise ValueError("Initialize takes no target")
            return self.initialize()
        if operation=="identity":
            name=args.get("container","")
            match=re.fullmatch(re.escape("webagent-build-"+self.config["instance"]+"-")+r"([a-f0-9]{32})-([a-z_]+)-([a-f0-9]{8})",name)
            if not match or match[2]not in PHASES:raise PermissionError("Not a build container")
            value=inspect(name,missing_ok=True)
            if value is None:return {"missing":True}
            labels=value.get("Config",{}).get("Labels",{})
            if labels.get("webagent.scheduler.instance")!=self.config["instance"]or labels.get("webagent.scheduler.job")!=match[1]or labels.get("webagent.scheduler.phase")!=match[2]or value.get("HostConfig",{}).get("CgroupParent")!=self.slice:
                raise PermissionError("Recovery container identity changed")
            return {"container_id":value["Id"],"state":value["State"]["Status"],"phase":match[2]}
        if operation not in {"stats","freeze","thaw","reclaim","cpu"}:raise ValueError("Unsupported resource operation")
        group,phase,identity=self.resolve(args.get("container",""))
        if operation!="stats"and args.get("expected_id")!=identity:raise PermissionError("Container identity changed")
        if operation!="stats"and self.config.get("scheduler",{}).get("mode")!="enforce":raise PermissionError("Resource control disabled")
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
        except (OSError,ValueError,PermissionError,TimeoutError,subprocess.SubprocessError):
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
