"""RAM workspace isolation, full-budget admission and fail-closed packaging contracts."""
import importlib.util,json,tempfile,threading,time,types,unittest,uuid
from pathlib import Path
from unittest.mock import patch

def load(name):
    """Load the candidate broker module without installed service state."""
    spec=importlib.util.spec_from_file_location("ram_"+name,Path(__file__).resolve().parents[1]/"build-service"/(name+".py"))
    value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value
executor=load("executor");resources=load("resources");config=load("config")
POLICY={"mode":"enforce","reserve_percent":15,"package_ram_bytes":9_999_998_976,"package_tmpfs_bytes":7_999_995_904,"package_cache_max_bytes":8*1024**3}

class RamPackagingTests(unittest.TestCase):
    """Exercise mount and proof gates before allowing the workload to run."""
    def run_case(self,mount="tmpfs",cap=9_999_998_976,error=None,cached=False):
        """Use real executor ordering and mocked fixed Docker/kernel boundaries."""
        identity="a"*64;calls=[];cleanup=[];job={"job_id":str(uuid.uuid4()),"task_id":"fixture","package_memory_cap":4026531840}
        r=types.SimpleNamespace(enabled=True,lock=threading.RLock(),rows={})
        def enter(name,*args,**kwargs):
            """Capture admission before any container effect."""
            calls.append(("admit",args,kwargs));r.rows[name]={}
        r.enter=enter;r.attached=lambda *args:None
        def command(argv,**kwargs):
            """Capture exact mounts and return only the requested filesystem type."""
            calls.append(("command",argv));return mount if argv[:2]==["docker","exec"]else ""
        def request(args):
            """Return independently bound identity and enforced memory/swap proof."""
            if args["operation"]=="identity":return {"container_id":identity,"state":"created","phase":"package"}
            return {"container_id":identity,"memory_max":cap,"swap_max":0,"oom_events":0,"oom_kills":0,"peak_memory":123}
        runner=types.SimpleNamespace(command=command,resource_request=request,job_context=types.SimpleNamespace())
        worker=executor.Executor(runner,r,{"instance":"development","scheduler":POLICY});worker.finish_phase=lambda *args:cleanup.append(args)
        caught=None;executed=False
        with patch.object(executor.subprocess,"run"):
            try:
                with worker._packager(job,"generic","dedicated-test-cache" if cached else None):
                    executed=True
                    if error:raise error
            except Exception as failure:caught=failure
        return caught,executed,calls,cleanup,job
    def test_mount_and_full_budget_precede_workload(self):
        """RAM scratch is fixed, swap-free and included in the protected container ceiling."""
        error,executed,calls,cleanup,job=self.run_case();self.assertIsNone(error);self.assertTrue(executed)
        self.assertEqual(calls[0][2]["hard_cap"],9_999_998_976)
        create=next(c[1]for c in calls if c[0]=="command"and c[1][:2]==["docker","create"])
        self.assertIn("/var/lib/buildkit:rw,exec,nosuid,nodev,size=7999995904,mode=0700",create)
        self.assertEqual(create[create.index("--memory")+1],"9999998976")
        self.assertEqual(create[create.index("--memory-swap")+1],"9999998976")
        self.assertEqual(job["package_cap_outcomes"][0]["scratch_bytes"],7_999_995_904);self.assertTrue(cleanup[0][2])
    def test_persistent_cache_has_protected_buildkit_gc_budget(self):
        error,executed,calls,cleanup,job=self.run_case(cached=True)
        self.assertIsNone(error);self.assertTrue(executed)
        create=next(c[1]for c in calls if c[0]=="command"and c[1][:2]==["docker","create"])
        self.assertIn("type=volume,src=dedicated-test-cache,dst=/var/lib/buildkit,volume-nocopy",create)
        self.assertIn("--oci-worker-gc",create)
        self.assertNotIn("--oci-worker-gc=false",create)
        self.assertEqual(create[create.index("--oci-worker-gc-keepstorage")+1],"2147,4294,8589")
        self.assertFalse(any(c[0]=="command"and c[1][:2]==["docker","exec"]for c in calls))

    def test_missing_mount_and_incorrect_kernel_cap_forbid_workload(self):
        """A requested RAM mount or Docker flag is not sufficient proof."""
        for kwargs in [{"mount":"overlayfs"},{"cap":4026531840}]:
            error,executed,calls,cleanup,job=self.run_case(**kwargs)
            self.assertIsInstance(error,ValueError);self.assertFalse(executed);self.assertFalse(cleanup[0][2])
    def test_scratch_exhaustion_does_not_retry_on_disk(self):
        """Retain the primary failure and failed receipt, with one exact cleanup."""
        error,executed,calls,cleanup,job=self.run_case(error=RuntimeError("No space left on device"))
        self.assertEqual(str(error),"No space left on device");self.assertEqual(len(cleanup),1);self.assertFalse(cleanup[0][2])
        self.assertEqual(job["package_cap_outcomes"][0]["outcome"],"failed")
        self.assertEqual(sum(c[0]=="command"and c[1][:2]==["docker","create"]for c in calls),1)

class RamAdmissionTests(unittest.TestCase):
    """Reserve the full RAM ceiling rather than calibrating down to small prior samples."""
    def test_global_headroom_and_same_task_serialization(self):
        """Independent tasks cannot double-spend RAM; same-task work waits for packaging."""
        with tempfile.TemporaryDirectory()as folder:
            sample={"available":24_000_000_000,"total":32_000_000_000,"pressure":{"cpu":0,"memory":0,"io":0},"swap_free":0,"swap_total":0}
            runner=types.SimpleNamespace(resource_request=lambda args:{})
            r=resources.Resources(folder,runner,POLICY,sampler=lambda:sample.copy())
            try:
                r.profiles["ram"]=[64*1024**2]
                r.enter("one",{"job_id":"one","task_id":"task-one"},"package","buildkit:v0.25.1:role-context-v2:api:ram-v1","package",timeout=.02,hard_cap=9_999_998_976)
                self.assertEqual(r.rows["one"]["estimate"],9_999_998_976);self.assertEqual(r.reserve(),4_800_000_000)
                with self.assertRaises(TimeoutError):r.enter("two",{"job_id":"two","task_id":"task-two"},"package","buildkit:v0.25.1:role-context-v2:loop:ram-v1","package",timeout=.02,hard_cap=9_999_998_976)
                with self.assertRaises(TimeoutError):r.enter("same",{"job_id":"same","task_id":"task-one"},"index_scan","index","index",timeout=.02)
                r.rows["one"]["state"]="finished"
                r.enter("two",{"job_id":"two","task_id":"task-two"},"package","buildkit:v0.25.1:role-context-v2:loop:ram-v1","package",timeout=.02,hard_cap=9_999_998_976)
            finally:r.close()
    def test_invalid_protected_configuration_is_rejected(self):
        """Reject over-budget, unpaired, unsafe-reserve and boolean policy values."""
        account=types.SimpleNamespace(pw_uid=1000,pw_gid=1000,pw_name="operator",pw_dir="/tmp")
        with patch.object(config.pwd,"getpwnam",return_value=account):
            base=config.defaults();base.update(service_account="operator",source_owner="operator",scheduler=POLICY)
            self.assertEqual(config.validate(base)["scheduler"]["package_ram_bytes"],9_999_998_976)
            for changes in [{"package_ram_bytes":10_000_000_000},{"reserve_percent":10},{"package_ram_bytes":10_000_000_001},{"package_ram_bytes":0},{"package_tmpfs_bytes":True},{"package_tmpfs_bytes":9_000_000_000},{"mode":"observe"}]:
                with self.assertRaises(ValueError):config.validate({**base,"scheduler":{**POLICY,**changes}})
