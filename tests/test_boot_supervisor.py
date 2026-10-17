"""Host authority regressions for idle recovery and drop-in lifecycle."""
import importlib.util,json,sys,tempfile,unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/"build-service"))
spec=importlib.util.spec_from_file_location("boot_host",ROOT/"build-service/boot_host.py")
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
class FakeDocker:
    def __init__(self,names):
        self.state={n:{"exists":True,"id":n+"-generation","running":True,"restarting":False,"started":"first","health":"healthy"} for n in names};self.effects=[]
    def inspect(self,name):return dict(self.state[name])
    def action(self,name,operation,expected):
        if self.state[name]!=expected:raise RuntimeError("Changed generation")
        self.effects.append((name,operation));self.state[name]["running"]=operation!="stop";self.state[name]["health"]="starting"
class FakeEngine:
    def __init__(self):self.available=True;self.revision="fixture";self.control_revision=0;self.identity=("a"*64,"0.1.0");self.requests=[];self.required=True
    def adopt(self):return self.available
    def retire(self):pass
    def plan(self,r):
        self.requests.append(r);actions=[];times=[]
        for i,s in enumerate(r["services"]):
            if not s["eligible"]:continue
            if s["state"]=="stopped" or s["state"]=="running" and s["unhealthy"]:
                if s["safe_after_ms"]>r["now_ms"]:times.append(s["safe_after_ms"])
                else:actions.append({"index":i,"operation":"start" if s["state"]=="stopped" else "restart"})
        return {"version":1,"actions":actions,"next_deadline_ms":min(times) if times else None}
class BootTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        root=Path(self.tmp.name)
        self.cfg={"api_container":"webagent-dev-v3","loop_container":"webagent-dev-v3-loop","gate_container":"webagent-dev-v3-gate","state_root":str(root/"jobs")}
        (root/"jobs").mkdir()
        self.names=[self.cfg[k] for k in ("api_container","loop_container","gate_container")]
        self.docker=FakeDocker(self.names);self.engine=FakeEngine();self.host=m.Host(self.cfg,root/"host",self.docker,self.engine)
    def test_shared_deployment_lease_fences_recovery(self):
        import fcntl,os
        name=self.names[0];self.docker.state[name]["running"]=False
        fd=os.open(self.host.lock_path,os.O_RDONLY)
        try:
            fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);self.host.reconcile(100)
            self.assertEqual(self.docker.effects,[])
            self.assertFalse(self.host.state["services"][name]["uncertain"])
        finally:os.close(fd)
        self.host.reconcile(101);self.assertEqual(self.docker.effects,[(name,"start")])
    def test_log_churn_does_not_refresh_healthy_host(self):
        self.host.reconcile(100)
        self.assertFalse(self.host.needs_job_refresh())
        self.docker.state[self.names[0]]["running"]=False
        (Path(self.cfg["state_root"])/"job.json").write_text(json.dumps({"job_id":"fixture","status":"running"}))
        self.host.reconcile(101);self.assertTrue(self.host.state["pending"])
        self.assertTrue(self.host.needs_job_refresh())
        self.assertEqual(self.docker.effects,[])
    def test_boolean_plan_version_is_rejected(self):
        self.engine.plan=lambda r:{"version":True,"actions":[],"next_deadline_ms":None}
        self.host.reconcile(100);self.assertTrue(self.host.paused)
    def test_healthy_is_totally_blocked(self):
        self.host.reconcile(100);self.assertIsNone(self.host.deadline);self.assertFalse(self.host.state["pending"]);self.assertEqual(self.docker.effects,[])
    def test_stopped_desired_starts(self):
        self.docker.state[self.names[0]]["running"]=False
        self.host.reconcile(100);self.assertEqual(self.docker.effects,[(self.names[0],"start")])
    def test_native_restarting_is_left_to_docker(self):
        self.docker.state[self.names[0]].update(running=False,restarting=True)
        self.host.reconcile(100);self.assertEqual(self.docker.effects,[]);self.assertIsNone(self.host.deadline)
    def test_withdrawal_retains_input_and_selective_restore(self):
        for name in self.names:self.docker.state[name]["running"]=False
        self.host.state["services"][self.names[1]]["desired"]=False
        self.host.state["services"][self.names[2]]["uncertain"]=True
        self.engine.available=False;self.host.reconcile(100)
        self.assertTrue(self.host.state["pending"]);self.assertTrue(self.host.paused);self.assertEqual(self.docker.effects,[])
        self.engine.available=True;self.host.reconcile(101)
        self.assertEqual(self.docker.effects,[(self.names[0],"start")])
    def test_manual_stop_persists_across_host_restart(self):
        name=self.names[0];self.host.manual(name,"stop")
        restored=m.Host(self.cfg,self.host.home,self.docker,self.engine);restored.reconcile(100)
        self.assertFalse(restored.state["services"][name]["desired"]);self.assertEqual(self.docker.effects,[(name,"stop")])
    def test_generation_change_is_uncertain_and_never_retried(self):
        name=self.names[0];self.docker.state[name]["running"]=False
        def raced(name,operation,expected):raise RuntimeError("Changed generation")
        self.docker.action=raced;self.host.reconcile(100);self.host.reconcile(200)
        self.assertTrue(self.host.state["services"][name]["uncertain"]);self.assertFalse(self.engine.requests[-1]["services"][0]["eligible"])
    def test_release_fences_recovery(self):
        (Path(self.cfg["state_root"])/"job.json").write_text(json.dumps({"job_id":"fixture","status":"running"}))
        self.docker.state[self.names[0]]["running"]=False;self.host.reconcile(100)
        self.assertEqual(self.docker.effects,[])
    def test_healthy_restart_forgery_rejected(self):
        self.engine.plan=lambda r:{"version":1,"actions":[{"index":0,"operation":"restart"}],"next_deadline_ms":None}
        self.host.reconcile(100);self.assertTrue(self.host.paused);self.assertEqual(self.docker.effects,[])
    def test_excluded_names_cannot_be_controlled(self):
        for name in ["webagent","webagent-dev-v2","webagent-dev"]:
            with self.assertRaises(ValueError):self.host.manual(name,"start")
        self.assertEqual(self.docker.effects,[])
    def test_unhealthy_has_real_grace_deadline(self):
        self.docker.state[self.names[0]]["health"]="unhealthy";self.host.reconcile(100)
        self.assertGreater(self.host.deadline,100);self.assertEqual(self.docker.effects,[])
class NetworkTests(unittest.TestCase):
    def observe(self,mode="bridge",networks=None,bindings=None,ports=None):
        from unittest.mock import MagicMock,patch
        value={"Id":"fixed","State":{"Running":True,"Restarting":False,"StartedAt":"first","Health":{"Status":"healthy"}},
            "HostConfig":{"NetworkMode":mode,"PortBindings":bindings or {}},
            "NetworkSettings":{"Networks":networks or {},"Ports":ports or {}}}
        peer=MagicMock();peer.getresponse.return_value.status=200
        peer.getresponse.return_value.read.return_value=json.dumps(value).encode()
        d=m.Docker(["api"])
        with patch.object(d,"connection",return_value=peer):return d.inspect("api")
    def test_healthy_process_without_bridge_is_unhealthy(self):
        value=self.observe();self.assertEqual(value["health"],"unhealthy");self.assertTrue(value["bridge_missing"])
    def test_missing_published_port_is_unhealthy(self):
        value=self.observe(networks={"bridge":{"IPAddress":"172.17.0.2"}},bindings={"8080/tcp":[{"HostIp":"100.112.23.15","HostPort":"8081"}]})
        self.assertTrue(value["network_fault"]);self.assertFalse(value["bridge_missing"])
    def test_complete_ingress_is_healthy_and_none_network_is_valid(self):
        ports={"8080/tcp":[{"HostIp":"100.112.23.15","HostPort":"8081"}]}
        self.assertEqual(self.observe(networks={"bridge":{"IPAddress":"172.17.0.2"}},bindings=ports,ports=ports)["health"],"healthy")
        self.assertFalse(self.observe(mode="none")["network_fault"])
    def test_repair_is_fixed_target_and_generation_fenced(self):
        from unittest.mock import MagicMock,patch
        d=m.Docker(["api"]);before=self.observe();after={**before,"bridge_missing":False,"network_fault":False,"health":"healthy"}
        connect=MagicMock();connect.getresponse.return_value.status=200
        restart=MagicMock();restart.getresponse.return_value.status=204
        with patch.object(d,"inspect",side_effect=[before,after]),patch.object(d,"connection",side_effect=[connect,restart]):d.action("api","restart",before)
        self.assertEqual(connect.request.call_args.args[:2],("POST","/networks/bridge/connect"))
        self.assertEqual(json.loads(connect.request.call_args.kwargs["body"]),{"Container":"fixed"})
        self.assertEqual(restart.request.call_args.args[1],"/containers/fixed/restart?t=10")
        with patch.object(d,"inspect",return_value={**before,"id":"new"}),patch.object(d,"connection") as connection:
            with self.assertRaises(RuntimeError):d.action("api","restart",before)
            connection.assert_not_called()
    def test_failed_connect_remains_uncertain_without_restart(self):
        from unittest.mock import MagicMock,patch
        d=m.Docker(["api"]);before=self.observe();peer=MagicMock();peer.getresponse.return_value.status=500
        with patch.object(d,"inspect",return_value=before),patch.object(d,"connection",return_value=peer):
            with self.assertRaises(RuntimeError):d.action("api","restart",before)
        self.assertEqual(peer.request.call_count,1)
if __name__=="__main__":unittest.main()
