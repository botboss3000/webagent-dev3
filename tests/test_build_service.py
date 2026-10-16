"""Portable configuration and persistent build admission contract."""
import importlib.util,tempfile,time,types,unittest,uuid,json
from pathlib import Path
from unittest.mock import patch
import test_admin_build as fixtures
def module(name,path):
 spec=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
config=module("portable_config",Path(__file__).resolve().parents[1]/"build-service/config.py")
class ConfigurationTests(unittest.TestCase):
 def account(self,name):return types.SimpleNamespace(pw_name=name,pw_uid=2301 if name=="builder" else 2402,pw_gid=2301,pw_dir="/srv/build-home")
 def test_arbitrary_account_paths_and_two_instances(self):
  for ident,root in (("one","/srv/one"),("two","/srv/two")):
   value=config.defaults(root);value.update(instance=ident,service_account="builder",source_owner="owner",api_container=ident+"-api",loop_container=ident+"-loop",runtime_root=root+"/sockets",service_dir="/var/lib/webagent/"+ident+"/build-service/releases/test")
   with patch.object(config.pwd,"getpwnam",side_effect=self.account):result=config.validate(value)
   self.assertEqual(result["service_uid"],2301);self.assertEqual(result["source_uid"],2402)
   self.assertEqual(result["checkout_root"],root);self.assertEqual(result["api_container"],ident+"-api")
 def test_rejects_paths_targets_and_colliding_identities(self):
  value=config.defaults("/srv/one");value.update(service_account="builder",source_owner="owner")
  for overrides in ({"checkout_root":"/srv/../private"},{"checkout_root":"/srv/x,secret"},{"api_container":"webagent"},{"loop_container":value["api_container"]},{"port":True},{"app_uid":2301},{"public_origin":"https://user:password@example.invalid"}):
   with patch.object(config.pwd,"getpwnam",side_effect=self.account),self.assertRaises(ValueError):config.validate({**value,**overrides})
 def test_api_thread_budget_is_bounded_and_legacy_compatible(self):
  """A protected operator budget is typed; omitted legacy values remain 256."""
  value=config.defaults("/srv/one");value.update(service_account="builder",source_owner="owner")
  with patch.object(config.pwd,"getpwnam",side_effect=self.account):
   for invalid in (True,0,255,4097):
    with self.assertRaises(ValueError):config.validate({**value,"api_pids_limit":invalid})
   legacy={k:v for k,v in value.items()if k!="api_pids_limit"}
   self.assertEqual(config.validate(legacy)["api_pids_limit"],256)
   self.assertEqual(config.validate({**value,"api_pids_limit":1024})["api_pids_limit"],1024)
 def test_unprotected_config_rejected(self):
  with tempfile.TemporaryDirectory() as base:
   path=Path(base)/"config";path.write_text("{}");path.chmod(0o666)
   with self.assertRaises(PermissionError):config.load(path)
   link=Path(base)/"link";link.symlink_to(path)
   with self.assertRaises(PermissionError):config.load(link)
class MaintenanceTests(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.state=Path(self.temp.name)/"jobs";self.runner=fixtures.FakeRunner()
  self.broker=fixtures.host.Broker(self.state,self.runner);self.sid=str(uuid.uuid4())
  self.trusted={"user_id":"codex-local","session_id":self.sid,"lease_until":time.time()+30,"access_fingerprint":"f"*64,"local_caller":True}
 def tearDown(self):self.broker.shutdown();self.temp.cleanup()
 def call(self,name,args,trusted=None):return self.broker.handle({"id":"fixture","name":name,"arguments":args,"workspace":self.sid,"trusted":trusted or self.trusted})
 def test_drain_persists_and_blocks_new_work_but_allows_owner(self):
  self.call("build_service_maintenance",{"action":"drain","task_id":"migration-task"})
  with self.assertRaisesRegex(PermissionError,"drained"):self.call("v3_build_work",{"action":"begin","task_id":"another-task","summary":"Blocked","mode":"isolated"})
  result=self.call("v3_build_work",{"action":"begin","task_id":"migration-task","summary":"Allowed","mode":"isolated"});self.assertEqual(result["status"],"open")
  self.broker.shutdown();self.broker=fixtures.host.Broker(self.state,self.runner)
  self.assertTrue(self.call("build_service_maintenance",{"action":"status"})["drained"])
  self.call("build_service_maintenance",{"action":"resume"})
  result=self.call("v3_build_work",{"action":"begin","task_id":"another-task","summary":"Allowed","mode":"isolated"});self.assertEqual(result["status"],"open")
 def test_app_cannot_open_maintenance_or_override_with_task_id(self):
  app={k:v for k,v in self.trusted.items() if k!="local_caller"}
  with self.assertRaises(PermissionError):self.call("build_service_maintenance",{"action":"drain"},app)
  self.call("build_service_maintenance",{"action":"drain","task_id":"migration-task"})
  with self.assertRaises(PermissionError):self.call("v3_build_work",{"action":"begin","task_id":"migration-task","summary":"Blocked","mode":"isolated"},app)

class MountBoundaryTests(unittest.TestCase):
 def test_pinned_inode_survives_path_replacement_and_rejects_symlink(self):
  import os
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);source=root/"session";source.mkdir();identity=source.stat().st_ino
   with fixtures.host.pinned_directory(root,"session",os.getuid()) as mount:
    source.rename(root/"original");source.symlink_to("/etc",target_is_directory=True)
    self.assertEqual(Path(mount).stat().st_ino,identity)
   with self.assertRaises(OSError):
    with fixtures.host.pinned_directory(root,"session"):pass

class InitialPublicationTests(unittest.TestCase):
 def test_first_workspace_request_stages_snapshot_before_release_id(self):
  from unittest.mock import Mock
  import os
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);workspace=str(uuid.uuid4());(root/workspace).mkdir()
   runner=Mock();runner.snapshot.return_value=b"immutable-source"
   runner.stage_assets.side_effect=RuntimeError("publication checkpoint")
   broker=fixtures.host.Broker(root/"jobs",runner)
   trusted={"user_id":"operator","session_id":workspace,"lease_until":time.time()+30,"access_fingerprint":"f"*64,"local_caller":True}
   try:
    with patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"workspace_root":str(root),"worker_uid":os.getuid(),"instance":"fresh"}),patch.object(fixtures.host,"ASSET_ROOT",root/"assets"),patch.object(fixtures.host,"context_digest",return_value=("f"*64,{})):
     with self.assertRaisesRegex(RuntimeError,"publication checkpoint"):broker.workspace_request("source_review",{},trusted,workspace)
    args=runner.stage_assets.call_args.args
    self.assertEqual(args[0],b"immutable-source")
    self.assertEqual(str(uuid.UUID(args[1])),args[1])
   finally:broker.shutdown()

class WorkspaceVolumeTests(unittest.TestCase):
 def test_pinned_volumes_are_removed_after_executor_failure(self):
  from unittest.mock import Mock
  import os
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);base=root/"workspace";selected=root/"release"
   base.mkdir();selected.mkdir();runner=fixtures.host.Runner();runner.command=Mock(return_value="created")
   with patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"worker_uid":os.getuid(),"service_uid":os.getuid()}),patch.object(fixtures.host.subprocess,"run") as cleanup:
    with self.assertRaisesRegex(RuntimeError,"executor"):
     with runner.workspace_volumes(base,selected) as volumes:
      self.assertEqual(len(volumes),2)
      for command in runner.command.call_args_list:
       self.assertTrue(any(arg.startswith("device=/proc/") for arg in command.args[0]))
      raise RuntimeError("executor")
    self.assertEqual(cleanup.call_count,2)
    self.assertEqual({call.args[0][-1] for call in cleanup.call_args_list},set(volumes))

class PublishedPermissionsTests(unittest.TestCase):
 def test_source_and_index_are_traversable_for_distinct_container_uid(self):
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);source=root/"v3";index=root/".code-index"
   source.mkdir(mode=0o700);index.mkdir(mode=0o700)
   (source/"Cargo.toml").write_text("source");(index/"manifest.json").write_text("index")
   fixtures.host.Runner().readable_assets(root)
   for path in (root,source,index):self.assertEqual(path.stat().st_mode&0o777,0o755)
   (root/"escape").symlink_to("/etc")
   with self.assertRaises(PermissionError):fixtures.host.Runner().readable_assets(root)

class ExecutorResponseTests(unittest.TestCase):
 def test_json_booleans_are_parsed_before_log_redaction(self):
  from unittest.mock import Mock
  import contextlib
  runner=fixtures.host.Runner();runner.command=Mock(return_value='{"status":"success","truncated":false}')
  @contextlib.contextmanager
  def volumes(*args):yield ("workspace-volume","source-volume")
  runner.workspace_volumes=volumes
  with patch.object(fixtures.host.subprocess,"run"):
   result=runner.workspace_task(Path("/workspace/session"),Path("/source/release"),"image","source_review",{}, {"lease_until":time.time()+30})
  self.assertFalse(result["truncated"])
  self.assertTrue(runner.command.call_args.kwargs["private_raw"])

class OriginConfigurationTests(unittest.TestCase):
 def test_ui_verification_uses_configured_instance_origin(self):
  from unittest.mock import Mock
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);entry=root/"current/v3/ui/index.html";entry.parent.mkdir(parents=True);entry.write_bytes(b"app")
   connection=Mock();response=connection.getresponse.return_value;response.status=200;response.read.return_value=b"app"
   with patch.object(fixtures.host,"ASSET_ROOT",root),patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"bind_address":"127.0.0.1","port":18081}),patch.object(fixtures.host.http.client,"HTTPConnection",return_value=connection) as factory:
    fixtures.host.Runner().verify_ui()
   factory.assert_called_once_with("127.0.0.1",18081,timeout=10)

class HostSourceReleaseTests(unittest.TestCase):
 def test_application_changes_and_app_callers_cannot_skip_image_build(self):
  with tempfile.TemporaryDirectory() as temporary:
   broker=fixtures.host.Broker(Path(temporary)/"jobs",fixtures.FakeRunner())
   try:
    for path in ("v3/src/app.rs","v3/worker.py","v3/build-service/workspace_executor.py","v3/build-service/config.py","v3/plugins/abilities/catalog.json"):
     self.assertFalse(broker.host_source_only([path],b"unused",{"local_caller":True}))
    self.assertFalse(broker.host_source_only(["v3/build-service/host.py"],b"unused",{}))
   finally:broker.shutdown()
 def test_installed_host_must_match_candidate(self):
  import os,tarfile,io
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);host=root/"host.py";host.write_bytes(b"installed host");host.chmod(0o644)
   blob=io.BytesIO()
   with tarfile.open(fileobj=blob,mode="w") as archive:
    item=tarfile.TarInfo("v3/build-service/host.py");item.size=len(b"candidate host");item.mode=0o644;archive.addfile(item,io.BytesIO(b"candidate host"))
   broker=fixtures.host.Broker(root/"jobs",fixtures.FakeRunner())
   try:
    with patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"service_dir":str(root)}),patch.object(fixtures.host,"context_digest",return_value=("f"*64,{})):
     if os.getuid()==0:self.assertFalse(broker.host_source_only(["v3/build-service/host.py"],blob.getvalue(),{"local_caller":True}))
     else:
      with self.assertRaises(PermissionError):broker.host_source_only(["v3/build-service/host.py"],blob.getvalue(),{"local_caller":True})
   finally:broker.shutdown()

class InstalledOwnershipPublicationTests(unittest.TestCase):
 def test_changed_ownership_requires_exact_protected_executable(self):
  """A broker-only ownership update requires installed bytes and cannot admit application changes."""
  import os
  from types import SimpleNamespace
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary);(root/"host.py").write_bytes(b"installed host");(root/"ownership.py").write_bytes(b"installed ownership")
   broker=fixtures.host.Broker(root/"jobs",fixtures.FakeRunner())
   protected=SimpleNamespace(st_mode=0o100644,st_uid=0,st_nlink=1)
   def identity(fd):
    """Supply root-owned inode metadata to exercise the installed-byte gate."""
    return protected
   blob=fixtures.context(extra={"v3/build-service/host.py":"installed host","v3/build-service/ownership.py":"installed ownership"})
   changed=["v3/build-service/ownership.py","v3/tests/test_scheduler_ownership.py"]
   try:
    with patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"service_dir":str(root)}),patch.object(fixtures.host.os,"fstat",side_effect=identity):
     self.assertTrue(broker.host_source_only(changed,blob,{"local_caller":True}))
     (root/"ownership.py").write_bytes(b"different installed bytes")
     self.assertFalse(broker.host_source_only(changed,blob,{"local_caller":True}))
     self.assertFalse(broker.host_source_only(changed,blob,{}))
     self.assertFalse(broker.host_source_only(changed+["v3/worker.py"],blob,{"local_caller":True}))
   finally:broker.shutdown()


class DashboardHistoryTests(unittest.TestCase):
 def test_administrator_feed_includes_all_notices_and_builds(self):
  fixture=fixtures.BuildTests();fixture.setUp()
  try:
   broker=fixture.broker
   for index in range(65):
    key=f"{index:064x}"
    broker.work[key]={"task_id":f"retained-{index}","user_id":"codex-local",
     "workspace":fixture.workspace,"status":"open","created_at":time.time()-90000}
    broker.work_activity[key]=broker._work_content(broker.work[key])
   for index in range(35):
    job_id=str(uuid.uuid4())
    broker.jobs[job_id]={"job_id":job_id,"status":"completed","phase":"finished",
     "operation":"build_deploy","created_at":index,"user_id":"codex-local",
     "workspace":fixture.workspace,"access_fingerprint":"f"*64}
   trusted={"user_id":"admin","session_id":"00000000-0000-0000-0000-000000000000",
    "lease_until":time.time()+30,"access_fingerprint":"a"*64}
   result=fixture.call("v3_build_dashboard",trusted=trusted,workspace=trusted["session_id"])
   self.assertEqual(result["leftover_total"],65)
   self.assertEqual(len(result["leftovers"]),65)
   self.assertEqual(len(result["jobs"]),35)
   self.assertEqual([job["created_at"] for job in result["jobs"]],list(range(34,-1,-1)))
   self.assertNotIn("access_fingerprint",json.dumps(result))
   with self.assertRaises(PermissionError):fixture.call("v3_build_dashboard")
  finally:fixture.tearDown()


class DiagnosticsProbeTests(unittest.TestCase):
 def test_environment_redaction_does_not_corrupt_private_protocol_boolean(self):
  """A container env value can equal JSON true; parse this fixed private frame."""
  runner=fixtures.host.Runner();runner.secrets=["true"]
  reply=json.dumps({"status":"success","output":{"protocol":1,"interface":1,"stateful":True}})
  def command(argv,**kwargs):
   return reply if kwargs.get("private_raw") else fixtures.host.scrub(reply,runner.secrets)
  with patch.object(runner,"command",side_effect=command) as called:
   self.assertTrue(runner.diagnostics_hot_swappable())
   self.assertTrue(called.call_args.kwargs["private_raw"])

class CodebasePublicationMetadataTests(unittest.TestCase):
 def fixture(self,temporary):
  import shutil
  folder=Path(temporary)/"package"
  shutil.copytree(Path(__file__).resolve().parents[1]/"plugins/abilities/Administrator/codebase_admin",folder)
  return folder
 def test_catalog_types_utf8_bounds_and_contract_are_validated_before_activation(self):
  with tempfile.TemporaryDirectory() as temporary:
   folder=self.fixture(temporary);path=folder/"codebase_admin.json";original=json.loads(path.read_bytes())
   for field,value in (("display_name",[]),("description",""),("description","é"*501),("skill_summary",True),("icon",[]),("order",True),("placeholder",0)):
    with self.subTest(field=field):
     path.write_text(json.dumps({**original,field:value}))
     with self.assertRaises(ValueError):fixtures.host.validate_codebase_package(folder)
   path.write_text(json.dumps(original));fixtures.host.validate_codebase_package(folder)
   path=folder/"plugin.json";value=json.loads(path.read_bytes());value["protocol"]=True;path.write_text(json.dumps(value))
   with self.assertRaises(ValueError):fixtures.host.validate_codebase_package(folder)
 def test_empty_guidance_and_new_catalog_ids_cannot_enter_the_hot_lane(self):
  with tempfile.TemporaryDirectory() as temporary:
   folder=self.fixture(temporary);(folder/"codebase_admin.skill.md").write_text("  ")
   with self.assertRaises(ValueError):fixtures.host.validate_codebase_package(folder)
  self.assertFalse(fixtures.host.codebase_source_path(fixtures.host.CODEBASE_ROOT+"skills/new_catalog_id.skill.md"))
  self.assertFalse(fixtures.host.codebase_source_path(fixtures.host.CODEBASE_ROOT+"tools/not_registered.skill.md"))
 def test_package_tests_use_retained_image_and_isolated_worker_identity(self):
  from unittest.mock import Mock
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);job=str(uuid.uuid4());folder=root/"releases"/job/fixtures.host.CODEBASE_ROOT
   folder.parent.mkdir(parents=True);import shutil;shutil.copytree(Path(__file__).resolve().parents[1]/"plugins/abilities/Administrator/codebase_admin",folder)
   runner=fixtures.host.Runner();runner.inspect=Mock(return_value={"Image":"sha256:retained-runtime"})
   runner.command=Mock(side_effect=["OK",json.dumps({"catalog_version":"f"*64}),"syntax valid"])
   with patch.object(fixtures.host,"ASSET_ROOT",root):
    runner.validate_ui(job,[fixtures.host.CODEBASE_ROOT+"runner.py"])
   args=runner.command.call_args_list[0].args[0]
   self.assertEqual(args[:3],["docker","run","--rm"])
   self.assertEqual(args[args.index("--network")+1],"none")
   self.assertEqual(args[args.index("--user")+1],str(fixtures.host.CONFIG["worker_uid"])+":"+str(fixtures.host.CONFIG["worker_gid"]))
   self.assertIn("sha256:retained-runtime",args)
   self.assertEqual(args[-3:],["-I","-B","/package/test_runner.py"])


 def test_foundation_catalog_validation_uses_tested_candidate_engine(self):
  from unittest.mock import Mock
  from types import SimpleNamespace
  with tempfile.TemporaryDirectory() as temporary:
   root=Path(temporary);job=str(uuid.uuid4());(root/"releases"/job).mkdir(parents=True)
   runner=fixtures.host.Runner();runner.inspect=Mock(return_value={"Image":"sha256:old-engine"})
   runner.job_context=SimpleNamespace(job={"operation":"build_deploy","images":{"api":"sha256:tested-candidate"}})
   runner.command=Mock(side_effect=[json.dumps({"catalog_version":"f"*64}),"syntax valid"])
   with patch.object(fixtures.host,"ASSET_ROOT",root):
    runner.validate_ui(job,["v3/config/prompts.json"])
   args=runner.command.call_args_list[0].args[0]
   self.assertIn("sha256:tested-candidate",args)
   self.assertNotIn("sha256:old-engine",args)
   self.assertEqual(args[-1],"--validate-assets=/candidate/v3")


class CodebaseRegisteredGuidanceTests(unittest.TestCase):
 def test_every_installed_selectable_skill_has_an_independent_lane(self):
  folder=Path(__file__).resolve().parents[1]/"plugins/abilities/Administrator/codebase_admin/skills"
  paths=list(folder.glob("*.skill.md"))
  self.assertGreaterEqual(len(paths),4)
  for path in paths:
   self.assertTrue(fixtures.host.ability_asset_path(fixtures.host.CODEBASE_ROOT+"skills/"+path.name),path.name)


class PluginCompilationConcurrencyTests(unittest.TestCase):
 def test_independent_compilers_overlap_and_publications_serialize(self):
  """Exercise the actual hot-release lock boundary with two bounded compiler barriers."""
  import concurrent.futures,threading
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary);runner=fixtures.FakeRunner();broker=fixtures.host.Broker(root/"jobs",runner)
   broker.scheduler.mode="enforce"
   lock=threading.Lock();entered=set();compiled=set();both=threading.Event();published=[]
   active=[0];maximum=[0]
   jobs=[{"job_id":str(uuid.uuid4()),"operation":"plugin_deploy","status":"running",
      "lease_until":time.time()+120,"created_at":time.time(),"release_root":str(root/str(i))}
      for i in range(2)]
   for job in jobs:broker.jobs[job["job_id"]]=job
   def compile_package(job_id,source_root=None):
    """Wait for both independent compilers, then model a verified artifact cache hit."""
    with lock:
     if job_id in compiled:return []
     entered.add(job_id)
     if len(entered)==2:both.set()
    if not both.wait(2):raise TimeoutError("Publication lock prevented independent compilation")
    with lock:compiled.add(job_id)
    return []
   def publish(job,blob):
    """Revalidation hits the cache while the actual publication mutex remains exclusive."""
    compile_package(job["job_id"])
    with lock:active[0]+=1;maximum[0]=max(maximum[0],active[0])
    try:time.sleep(.02);published.append(job["job_id"])
    finally:
     with lock:active[0]-=1
   try:
    with patch.object(runner,"build_ability_artifacts",create=True,side_effect=compile_package),patch.object(broker,"_run_ui_hot_locked",side_effect=publish):
     with concurrent.futures.ThreadPoolExecutor(max_workers=2)as pool:
      futures=[pool.submit(broker._run_ui_hot,job,b"frozen source")for job in jobs]
      for future in futures:future.result(timeout=4)
    self.assertEqual(entered,{job["job_id"]for job in jobs})
    self.assertEqual(set(published),entered);self.assertEqual(maximum[0],1)
   finally:broker.shutdown()


class PluginStagingRebaseTests(unittest.TestCase):
 def test_hot_plugin_stages_only_after_publication_rebase(self):
  """Run the broker entry point against the real immutable stager after another plugin publishes."""
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary);runner=fixtures.FakeRunner();runner.hot_assets=True
   broker=fixtures.host.Broker(root/"jobs",runner);broker.scheduler.mode="enforce"
   original=fixtures.context(extra={"v3/ui/index.html":"entry","v3/plugins/abilities/Developer/probe/probe.skill.md":"beta"})
   rebased=fixtures.context(extra={"v3/ui/index.html":"entry","v3/plugins/abilities/Developer/probe/probe.skill.md":"beta","v3/plugins/abilities/Developer/other/other.skill.md":"alpha"})
   job={"job_id":str(uuid.uuid4()),"operation":"plugin_deploy","status":"queued",
        "lease_until":time.time()+120,"created_at":time.time(),"release_root":str(root/"frozen"),"inputs_sha256":fixtures.host.context_digest(original)[0]}
   broker.jobs[job["job_id"]]=job;compile_roots=[]
   def compile_package(job_id,source_root=None):
    """Record the source passed to compilation without executing a candidate tool."""
    compile_roots.append(source_root);return []
   def stage(blob,job_id):
    """Use production immutable staging so an early original candidate cannot be overwritten."""
    return fixtures.host.release.stage_assets(root/"assets",job_id,blob,fixtures.host.context_digest)
   def publish(value,blob):
    """Model a current publication rebase, then exercise the actual staging contract."""
    stage(rebased,value["job_id"]);value.update(status="completed",phase="finished")
   try:
    with patch.object(broker,"_batch_lease"),patch.object(runner,"plugin_hot_swappable",create=True,return_value=True),patch.object(runner,"stage_assets",side_effect=stage),patch.object(runner,"build_ability_artifacts",create=True,side_effect=compile_package),patch.object(broker,"_run_ui_hot_locked",side_effect=publish):
     broker._run(job,original)
    self.assertEqual(job["status"],"completed",job.get("reason"))
    self.assertEqual(compile_roots,[Path(job["release_root"])])
    frozen=fixtures.host.release.snapshot(root/"assets/releases"/job["job_id"],("v3","ui"),fixtures.host.context_digest)
    self.assertEqual(fixtures.host.context_digest(frozen)[0],fixtures.host.context_digest(rebased)[0])
   finally:broker.shutdown()


class PackagingStreamTests(unittest.TestCase):
 def test_fixed_command_accepts_bounded_file_stream(self):
  """The broker feeds a disk stream without retaining its entire packaging tar in memory."""
  import io
  runner=fixtures.host.Runner()
  result=runner.command(["python3","-I","-c","import sys; print(len(sys.stdin.buffer.read()))"],
      stdin=io.BytesIO(b"x"*(2*1024*1024)),timeout=10)
  self.assertEqual(result.strip(),str(2*1024*1024))

class DocumentationHotRoutingTests(unittest.TestCase):
 """Exercise actual isolated finish routing with production compiler attestations."""
 setUp=fixtures.BuildTests.setUp
 tearDown=fixtures.BuildTests.tearDown
 call=fixtures.BuildTests.call
 wait=fixtures.BuildTests.wait
 def route(self,extra=None,attestation=True,ledger=True):
  """Classify a leaf implementation, descriptor, UI and ledger candidate through the broker."""
  if getattr(self,"_route_started",False):self.tearDown();self.setUp()
  self._route_started=True
  self.runner.blob=fixtures.context(extra={"v3/plugins/engine/context-assembly/component.json":"{}"})
  if attestation is True:self.runner.core_compile_inputs=lambda:{"v3/src/main.rs","v3/crates/context-contracts/src/lib.rs"}
  elif attestation is False:self.runner.core_compile_inputs=lambda:(_ for _ in()).throw(ValueError("unknown installed manifest"))
  elif attestation is not None:self.runner.core_compile_inputs=lambda:attestation
  task=self.call("v3_build_work",{"action":"begin","task_id":"docs-routing-"+uuid.uuid4().hex,"summary":"Leaf plus ordinary ledger","mode":"isolated"})
  paths={"crates/context-assembly/src/lib.rs":"leaf","plugins/engine/context-assembly/component.json":"{\"updated\":true}","ui/catalog.css":"body{}","docs/engine-modularization.md":"ledger",**(extra or {})}
  if not ledger:paths.pop("docs/engine-modularization.md")
  for name,body in paths.items():
   path=Path(task["workspace_path"])/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(body)
  digest=self.call("v3_build_work",{"action":"status","task_id":task["task_id"]})["own"][0]["workspace_inputs_sha256"]
  result=self.call("v3_build_work",{"action":"finish","task_id":task["task_id"],"expected_inputs_sha256":digest})
  return self.wait(result)["operation"]
 def test_leaf_descriptor_ui_and_ledger_publish_without_core_build(self):
  """The exact four changed categories retain independent engine publication."""
  self.assertEqual(self.route(),"plugin_deploy")
 def test_embedded_ledger_forces_core_build(self):
  """A compiler-declared doc must not be treated as an ordinary asset."""
  self.assertEqual(self.route(attestation={"v3/src/main.rs","v3/docs/engine-modularization.md"}),"build_deploy")
 def test_shared_contract_cargo_and_root_core_require_build(self):
  """Shared contracts and root build inputs never become leaf-only assets."""
  for name in ("crates/context-contracts/src/lib.rs","Cargo.toml","src/main.rs"):
   with self.subTest(name=name):self.assertEqual(self.route(extra={name:"changed"}),"build_deploy")
 def test_absent_unknown_or_unrecognized_attestation_requires_build(self):
  """Missing or unrecognized installed evidence cannot grant documentation publication."""
  for value in (None,False,set(),{"v3/docs/other.md"}):
   with self.subTest(value=value):self.assertEqual(self.route(attestation=value),"build_deploy")
 def test_leaf_without_ledger_requires_known_compiler_attestation(self):
  """A leaf-only candidate cannot bypass unavailable installed compiler evidence."""
  for value in (None,False,set(),{"v3/docs/other.md"}):
   with self.subTest(value=value):self.assertEqual(self.route(attestation=value,ledger=False),"build_deploy")
 def test_installed_docs_inputs_are_normalized_and_retained(self):
  """Absolute and relative compiler paths preserve embedded docs identities."""
  runner=fixtures.host.Runner(source=Path(self.temp.name),snapshot_socket=None)
  runner.command=lambda *a,**kw:"/build/src/main.rs\n/build/docs/../docs/engine-modularization.md\ndocs/other.md\n"
  self.assertEqual(runner.core_compile_inputs(),{"v3/src/main.rs","v3/docs/engine-modularization.md","v3/docs/other.md"})

class ReleaseClaimsBrokerTests(unittest.TestCase):
 setUp=MaintenanceTests.setUp
 tearDown=MaintenanceTests.tearDown
 call=MaintenanceTests.call
 def test_release_preserves_draft_and_retry_repairs_registry(self):
  import hashlib
  self.call("v3_build_work",{"action":"begin","task_id":"release-fixture","summary":"Release fixture","mode":"isolated"})
  key=hashlib.sha256(fixtures.host.canonical([self.trusted["user_id"],self.sid,"release-fixture"])).hexdigest()
  item=self.broker.work[key];store=self.broker.scheduler.store
  owner=self.broker.scheduler.owner(self.trusted,self.sid);value=store.get(key)
  a=store.acquire(key,owner,value["revision"],value["epoch"],["module:engine","module:broker"],"fixture-acquire")["task"]
  args={"action":"release_claims","task_id":"release-fixture","scopes":["module:engine"],"reason":"Defer checks","request_id":"fixture-release","expected_revision":a["revision"],"expected_epoch":a["epoch"]}
  self.broker.jobs["fixture-active"]={"ownership_id":key,"status":"running"}
  with self.assertRaisesRegex(ValueError,"execution"):self.call("build_work",args)
  self.assertIsNone(self.broker.scheduler.pending(key))
  self.broker.jobs.pop("fixture-active")
  with patch.object(self.broker,"_save_work",side_effect=OSError("injected save failure")):
   with self.assertRaises(OSError):self.call("build_work",args)
  r=self.call("build_work",args);self.assertEqual(r["scopes"],["module:broker"])
  self.assertEqual(self.call("build_work",args),r);self.assertEqual(item["ownership_epoch"],r["epoch"])
  self.assertTrue(Path(item["workspace_path"]).is_dir())
  with self.assertRaisesRegex(ValueError,"reused"):
   self.call("build_work",{**args,"reason":"Different payload"})


 def test_frozen_release_retains_promotion_and_epoch(self):
  import hashlib
  self.call("v3_build_work",{"action":"begin","task_id":"frozen-release-fixture","summary":"Frozen fixture","mode":"isolated"})
  key=hashlib.sha256(fixtures.host.canonical([self.trusted["user_id"],self.sid,"frozen-release-fixture"])).hexdigest()
  item=self.broker.work[key];store=self.broker.scheduler.store;owner=self.broker.scheduler.owner(self.trusted,self.sid)
  value=store.get(key)
  value=store.acquire(key,owner,value["revision"],value["epoch"],["module:broker","module:engine","path:src/fixture.rs"],"frozen-acquire")["task"]
  identity=str(uuid.uuid4());blob=self.broker.scheduler_task_snapshot(Path(item["workspace_path"]).parent)
  folder=self.state/identity;folder.mkdir();(folder/"candidate.tar").write_bytes(blob)
  item.update(status="closed",job_id=identity)
  job={"job_id":identity,"ownership_id":key,"ownership_epoch":value["epoch"],"source_generation":value["generation"],"inputs_sha256":self.broker.scheduler_digest(blob),"user_id":self.trusted["user_id"],"workspace":self.sid,"status":"running","created_at":time.time(),"operation":"build_deploy","changed_paths":["v3/src/fixture.rs"]}
  self.broker.jobs[identity]=job
  args={"action":"release_claims","task_id":item["task_id"],"scopes":["module:broker"],"reason":"Yield completed broker editing","request_id":"frozen-release","expected_revision":value["revision"],"expected_epoch":value["epoch"],"frozen_job_id":identity}
  with patch.object(self.broker,"_start_next"):
   result=self.call("build_work",args)
  self.assertEqual(result["epoch"],value["epoch"])
  self.assertEqual(result["scopes"],["module:engine","path:src/fixture.rs"])
  self.assertEqual(self.call("build_work",args),result)
  self.broker.scheduler.assert_job(job)
  args.update(scopes=["module:engine"],request_id="reject-promotion-release",expected_revision=result["revision"])
  with self.assertRaisesRegex(ValueError,"promotion"):self.call("build_work",args)
  self.assertIsNone(self.broker.scheduler.pending(key))

 def test_legacy_pending_release_rejects_active_job_terminally(self):
  import hashlib
  self.call("v3_build_work",{"action":"begin","task_id":"legacy-release-fixture","summary":"Legacy fixture","mode":"isolated"})
  key=hashlib.sha256(fixtures.host.canonical([self.trusted["user_id"],self.sid,"legacy-release-fixture"])).hexdigest()
  item=self.broker.work[key];store=self.broker.scheduler.store;owner=self.broker.scheduler.owner(self.trusted,self.sid)
  value=store.get(key);value=store.acquire(key,owner,value["revision"],value["epoch"],["module:broker"],"legacy-acquire")["task"]
  args={"action":"release_claims","task_id":item["task_id"],"scopes":["module:broker"],"reason":"Legacy release retry","request_id":"legacy-release","expected_revision":value["revision"],"expected_epoch":value["epoch"]}
  path=self.broker.scheduler.journal_path(owner,args["request_id"],"release-"+key)
  self.broker.scheduler.write_record(path,{"digest":hashlib.sha256(fixtures.host.canonical(args)).hexdigest(),"state":"pending","task_key":key,"request_id":args["request_id"]})
  self.broker.jobs["active"]={"ownership_id":key,"status":"running"}
  with self.assertRaisesRegex(ValueError,"execution"):self.call("build_work",args)
  self.assertIsNone(self.broker.scheduler.pending(key))
  self.assertEqual(json.loads(path.read_text())["state"],"rejected")
  self.assertEqual(store.get(key)["scopes"],["module:broker"])


class UIRuntimeRetentionTests(unittest.TestCase):
 def fixture(self):
  """Model Docker identity and creation flags without invoking an application container."""
  runner=fixtures.host.Runner.__new__(fixtures.host.Runner);calls=[];store={};image="sha256:"+"d"*64
  def inspect(name):
   if name not in store:raise RuntimeError("missing holder")
   return json.loads(json.dumps(store[name]))
  def command(args,**kw):
   calls.append(args)
   if args[:3]==["docker","image","inspect"]:return image
   if args[:2]==["docker","inspect"]:return json.dumps([inspect(args[2])])
   if args[:2]==["docker","ps"]:return "\n".join(v["Id"]for v in store.values())
   if args[:2]==["docker","create"]:
    def option(name):return args[args.index(name)+1]
    labels=dict(args[n+1].split("=",1)for n,v in enumerate(args)if v=="--label")
    store[option("--name")]={"Id":"a"*64,"Image":image,"Mounts":[],"State":{"Status":"created","Pid":0},"Config":{"Labels":labels,"Entrypoint":[option("--entrypoint")],"Cmd":args[-1:],"User":option("--user")},"HostConfig":{"NetworkMode":option("--network"),"ReadonlyRootfs":"--read-only"in args,"Memory":int(option("--memory")[:-1])*1024**2,"MemorySwap":int(option("--memory-swap")[:-1])*1024**2,"NanoCpus":int(float(option("--cpus"))*10**9),"PidsLimit":int(option("--pids-limit")),"CapDrop":[option("--cap-drop")],"SecurityOpt":[option("--security-opt")],"RestartPolicy":{"Name":option("--restart")}}}
   elif args[:2]==["docker","start"]:store[args[2]]["State"]={"Status":"running","Pid":4321}
   else:raise AssertionError("Unexpected Docker operation")
   return ""
  runner.command=command
  def application_inspect(_name):raise AssertionError("Retention must not broaden the application-only inspection boundary")
  runner.inspect=application_inspect
  return runner,calls,store,image
 def test_running_holder_is_bounded_isolated_and_reused(self):
  runner,calls,store,image=self.fixture();first=runner.retain_ui_test_runtime(image,"e"*64);second=runner.retain_ui_test_runtime(image,"e"*64)
  self.assertFalse(first["reused"]);self.assertTrue(second["reused"]);self.assertEqual(first["container_id"],second["container_id"])
  self.assertEqual(sum(c[:2]==["docker","create"]for c in calls),1);self.assertEqual(sum(c[:2]==["docker","start"]for c in calls),1)
  self.assertEqual(first["memory_max"],33554432);self.assertEqual(first["swap_max"],0);self.assertEqual(first["cpu_limit"],0.01)
  self.assertEqual(store[first["container"]]["Config"]["Entrypoint"],["sleep"])
 def test_changed_existing_holder_is_rejected_without_mutating_it(self):
  runner,calls,store,image=self.fixture();first=runner.retain_ui_test_runtime(image,"e"*64);store[first["container"]]["HostConfig"]["Privileged"]=True
  mutations=sum(c[1]in {"create","start"}for c in calls)
  with self.assertRaisesRegex(ValueError,"identity or budget"):runner.retain_ui_test_runtime(image,"e"*64)
  self.assertEqual(sum(c[1]in {"create","start"}for c in calls),mutations)
 def test_mutable_image_reference_is_rejected_before_docker(self):
  runner,calls,_store,_image=self.fixture()
  with self.assertRaisesRegex(ValueError,"Pinned"):runner.retain_ui_test_runtime("node:latest","e"*64)
  self.assertEqual(calls,[])

 def test_at_most_two_runtime_versions_can_be_retained(self):
  runner,calls,store,image=self.fixture();runner.retain_ui_test_runtime(image,"e"*64);runner.retain_ui_test_runtime(image,"f"*64)
  with self.assertRaisesRegex(ValueError,"third version"):runner.retain_ui_test_runtime(image,"b"*64)
  self.assertEqual(len(store),2)


class RollbackPatchModeTests(unittest.TestCase):
 def test_acl_modes_preserve_git_executable_identity_and_apply(self):
  import subprocess,stat
  raw="diff --git a/probe.css b/probe.css\nold mode 100775\nnew mode 100774\n--- a/probe.css\n+++ b/probe.css\n@@ -1,2 +1 @@\n alpha\n-beta\n"
  patch=fixtures.host.checked_git_modes(raw)
  self.assertNotIn("old mode",patch);self.assertNotIn("new mode",patch)
  with tempfile.TemporaryDirectory()as temporary:
   path=Path(temporary)/"probe.css";path.write_text("alpha\nbeta\n");path.chmod(0o775)
   subprocess.run(["git","apply","--check","-"],input=patch.encode(),cwd=temporary,check=True,capture_output=True)
   subprocess.run(["git","apply","-"],input=patch.encode(),cwd=temporary,check=True,capture_output=True)
   self.assertEqual(path.read_text(),"alpha\n");self.assertEqual(stat.S_IMODE(path.stat().st_mode),0o775)
 def test_executable_transitions_new_and_deleted_modes_are_canonical(self):
  value=fixtures.host.checked_git_modes("old mode 100775\nnew mode 100664\nnew file mode 100664\ndeleted file mode 100775\n")
  self.assertEqual(value,"old mode 100755\nnew mode 100644\nnew file mode 100644\ndeleted file mode 100755\n")
 def test_invalid_or_unpaired_modes_fail_closed(self):
  for value in ["new mode 100755\n","old mode 100755\n","new file mode 104755\n","deleted file mode 120777\n"]:
   with self.subTest(value=value),self.assertRaises(ValueError):fixtures.host.checked_git_modes(value)

class RollbackModeDispatchTests(unittest.TestCase):
 setUp=fixtures.BuildTests.setUp
 tearDown=fixtures.BuildTests.tearDown
 call=fixtures.BuildTests.call
 def test_public_operator_preview_translates_modes_without_changing_bindings(self):
  raw={"patch":"old mode 100775\nnew mode 100774\ncontent\n","expected_inputs_sha256":"f"*64,"changed_paths":["v3/ui/probe.css"]}
  with patch.object(self.broker.ui_publication,"rollback_preview",return_value=raw):
   result=self.call("build_work",{"action":"ui_rollback_preview","task_id":"mode-fixture","release_job_id":str(uuid.uuid4())})
  self.assertEqual(result,{**raw,"patch":"content\n"});self.assertTrue(raw["patch"].startswith("old mode"))


class SuccessorRecoveryTests(unittest.TestCase):
    """Terminal proposal imports use real source receipts and independent destination claims."""
    from test_scheduler_broker import SchedulerBrokerTests as _base
    setUp=_base.setUp
    tearDown=_base.tearDown
    call=_base.call
    begin=_base.begin
    write=_base.write

    def proposal(self,yield_claims=True):
        item=self.begin("old-proposal-task")
        self.write(item,"README.md","proposal\n")
        tree=Path(item["workspace_path"]).parent
        blob=self.broker.scheduler_task_snapshot(tree)
        job_id=str(uuid.uuid4());folder=self.broker.state/job_id;folder.mkdir()
        (folder/"task.tar").write_bytes(blob)
        value=self.broker.scheduler.store.get(item["key"])
        job={"job_id":job_id,"task_id":item["task_id"],"user_id":item["user_id"],
            "workspace":self.workspace,"status":"failed","effects_started":False,
            "source_generation":value["generation"],"isolated":True,"request_key":job_id}
        self.broker.jobs[job_id]=job;item.update(status="closed",job_id=job_id)
        if yield_claims:
            self.broker.scheduler.store.release_scopes(value["id"],value["owner"],value["revision"],value["epoch"],value["scopes"],"fixture yield","yield-fixture-scopes")
        return item,job

    def request(self,job):
        item=self.begin("new-proposal-task")
        tree=Path(item["workspace_path"]).parent
        return item,{"action":"successor_task","task_id":item["task_id"],"source_job_id":job["job_id"],
            "request_id":"successor-fixture-import","expected_inputs_sha256":self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree))}

    def test_import_rebases_preserves_original_and_replays(self):
        original,job=self.proposal();old=Path(original["workspace_path"])/"README.md"
        self.runner.blob=fixtures.context(extra={"v3/README.md":"original\n","v3/docs/later.md":"later\n"})
        item,args=self.request(job);result=self.call("build_work",args)
        self.assertEqual(result["status"],"imported")
        self.assertEqual((Path(item["workspace_path"])/"README.md").read_text(),"proposal\n")
        self.assertEqual((Path(item["workspace_path"])/"docs/later.md").read_text(),"later\n")
        self.assertEqual(old.read_text(),"proposal\n");self.assertEqual(original["status"],"closed")
        self.assertEqual(self.call("build_work",args),result)
        self.assertTrue(self.broker.scheduler.store.get(item["key"])["scopes"])
        self.assertFalse(self.broker.scheduler.store.get(original["key"])["scopes"])
        self.assertEqual(item["successor"]["source_summary"],original["summary"])
        with self.assertRaisesRegex(ValueError,"reused"):
            self.call("build_work",{**args,"expected_inputs_sha256":"0"*64})
        self.assertFalse(self.runner.calls)

    def test_same_file_conflict_does_not_claim_or_write(self):
        original,job=self.proposal()
        self.runner.blob=fixtures.context(extra={"v3/README.md":"other writer\n"})
        item,args=self.request(job)
        with self.assertRaisesRegex(ValueError,"source conflict"):self.call("build_work",args)
        self.assertFalse(self.broker.scheduler.store.get(item["key"])["scopes"])
        self.assertEqual((Path(item["workspace_path"])/"README.md").read_text(),"other writer\n")

    def test_original_claims_must_be_yielded(self):
        original,job=self.proposal(False);item,args=self.request(job)
        with self.assertRaisesRegex(ValueError,"Yield original"):self.call("build_work",args)

    def test_uncertain_effects_and_changed_frozen_source_rejected(self):
        original,job=self.proposal();item,args=self.request(job)
        job["effects_started"]=True
        with self.assertRaisesRegex(ValueError,"Reconcile effects"):self.call("build_work",args)
        job["effects_started"]=False
        (Path(original["workspace_path"])/"README.md").write_text("tampered\n")
        with self.assertRaisesRegex(ValueError,"Frozen proposal changed"):self.call("build_work",args)

    def test_dirty_or_stale_destination_rejected(self):
        original,job=self.proposal();item,args=self.request(job)
        self.runner.blob=fixtures.context(extra={"v3/README.md":"original\n","v3/docs/later.md":"late\n"})
        with self.assertRaisesRegex(ValueError,"baseline is stale"):self.call("build_work",args)
        self.runner.blob=fixtures.context(extra={"v3/README.md":"original\n"})
        self.write(item,"README.md","unrelated draft\n")
        with self.assertRaisesRegex(ValueError,"Unchanged empty"):self.call("build_work",args)

    def test_interrupted_receipt_completion_does_not_apply_twice(self):
        original,job=self.proposal();item,args=self.request(job)
        save=self.broker.scheduler.write_record
        def fail_once(path,record):
            if record.get("state")=="done":raise RuntimeError("lost reply")
            return save(path,record)
        with patch.object(self.broker.scheduler,"write_record",side_effect=fail_once):
            with self.assertRaisesRegex(RuntimeError,"lost reply"):self.call("build_work",args)
        value=self.broker.scheduler.store.get(item["key"])
        result=self.call("build_work",args)
        self.assertEqual(result["status"],"imported")
        self.assertEqual(self.broker.scheduler.store.get(item["key"])["generation"],value["generation"])

    def test_unauthorized_or_missing_source_rejected(self):
        original,job=self.proposal();item,args=self.request(job)
        with self.assertRaises(PermissionError):self.call("build_work",{**args,"source_job_id":str(uuid.uuid4())})
        self.trusted.pop("local_caller")
        with self.assertRaises(PermissionError):self.call("build_work",args)

    def test_proposal_patch_deletion_rename_modes_and_unterminated_text(self):
        before=fixtures.context(extra={"v3/docs/old.md":"old","v3/docs/remove.md":"gone\n"})
        after=fixtures.context(extra={"v3/docs/new.md":"old"})
        patch_text=fixtures.host.release.proposal_patch(before,after,fixtures.host.context_digest)
        self.assertIn("deleted file mode 100644",patch_text)
        self.assertIn("new file mode 100644",patch_text)
        self.assertIn("\\ No newline at end of file",patch_text)
        many=fixtures.context(extra={"v3/docs/f"+str(i)+".md":"x" for i in range(65)})
        with self.assertRaisesRegex(ValueError,"path bound"):fixtures.host.release.proposal_patch(fixtures.context(),many,fixtures.host.context_digest)

    def test_saved_import_rechecks_source_and_target_before_retry(self):
        original,job=self.proposal();item,args=self.request(job)
        normal=self.broker.handle
        def blocked(request):
            if request.get("name")=="v3_source_apply":raise ValueError("fixture wait")
            return normal(request)
        with patch.object(self.broker,"handle",side_effect=blocked):
            with self.assertRaisesRegex(ValueError,"fixture wait"):self.call("build_work",args)
        job["status"]="running"
        with self.assertRaisesRegex(ValueError,"source resumed"):self.call("build_work",args)
        job["status"]="failed"
        self.write(item,"README.md","new draft\n")
        with self.assertRaisesRegex(ValueError,"stale source inputs"):self.call("build_work",args)

    def test_mode_only_and_binary_proposals(self):
        release=fixtures.host.release;validate=fixtures.host.context_digest
        before=release.entries(fixtures.context(extra={"v3/docs/file.md":"same\n"}),validate)
        after=dict(before);after["v3/docs/file.md"]=(0o755,b"same\n")
        text=release.proposal_patch(release.archive(before,validate),release.archive(after,validate),validate)
        self.assertIn("old mode 100644\nnew mode 100755",text)
        after["v3/docs/file.md"]=(0o644,b"binary\0")
        with self.assertRaisesRegex(ValueError,"Binary successor"):
            release.proposal_patch(release.archive(before,validate),release.archive(after,validate),validate)


class ReleaseReconciliationTests(unittest.TestCase):
    """Recover journal-only failures without reissuing an ownership mutation."""
    from test_scheduler_broker import SchedulerBrokerTests as _base
    setUp=_base.setUp
    tearDown=_base.tearDown
    call=_base.call
    begin=_base.begin
    write=_base.write

    def pending_release(self,committed=False):
        import hashlib
        item=self.begin("release-recovery-task");self.write(item,"README.md","draft\n")
        value=self.broker.scheduler.store.get(item["key"])
        owner=self.broker.scheduler.owner(self.trusted,self.workspace)
        original={"action":"release_claims","task_id":item["task_id"],"request_id":"old-release-request", "reason":"fixture yield",
            "scopes":value["scopes"],"expected_revision":value["revision"],"expected_epoch":value["epoch"]}
        path=self.broker.scheduler.journal_path(owner,original["request_id"],"release-"+item["key"])
        journal={"digest":hashlib.sha256(fixtures.host.canonical(original)).hexdigest(),"state":"pending", "task_key":item["key"],"request_id":original["request_id"]}
        self.broker.scheduler.write_record(path,journal)
        if committed:
            token="release-"+hashlib.sha256(fixtures.host.canonical([owner,item["key"],original["request_id"]])).hexdigest()[:56]
            self.broker.scheduler.store.release_scopes(item["key"],owner,value["revision"],value["epoch"],value["scopes"],original["reason"],token)
        now=self.broker.scheduler.store.get(item["key"])
        args={"action":"reconcile_release","task_id":item["task_id"],"original_request_id":original["request_id"],
            "request_id":"settle-release-request","expected_revision":now["revision"],"expected_epoch":now["epoch"]}
        return item,args,path,original

    def test_abort_uncommitted_retains_claims_source_and_allows_fresh_release(self):
        item,args,path,original=self.pending_release()
        before=self.broker.scheduler.store.get(item["key"]);source=Path(item["workspace_path"])/"README.md"
        result=self.call("build_work",args)
        self.assertEqual(result["status"],"aborted_uncommitted");self.assertFalse(result["scope_mutation_performed"])
        self.assertEqual(before,self.broker.scheduler.store.get(item["key"]))
        self.assertEqual(source.read_text(),"draft\n");self.assertIsNone(self.broker.scheduler.pending(item["key"]))
        self.assertEqual(self.call("build_work",args),result)
        with self.assertRaisesRegex(ValueError,"explicitly reconciled"):self.call("build_work",original)
        fresh={**original,"request_id":"fresh-release-request"}
        self.assertFalse(self.call("build_work",fresh)["scopes"])

    def test_settle_committed_preserves_newer_claims(self):
        item,args,path,original=self.pending_release(True)
        store=self.broker.scheduler.store;value=store.get(item["key"])
        result=store.acquire(value["id"],value["owner"],value["revision"],value["epoch"],["module:asset:docs/other.md","path:docs/other.md"],"new-fixture-claims")
        value=result["task"];item["ownership_inputs_sha256"]="new-input-binding"
        args.update(expected_revision=value["revision"],expected_epoch=value["epoch"])
        settled=self.call("build_work",args)
        self.assertEqual(settled["status"],"settled_committed")
        self.assertEqual(store.get(item["key"]),value)
        self.assertEqual(item["ownership_inputs_sha256"],"new-input-binding")
        self.assertEqual(self.call("build_work",original),settled["release_receipt"])

    def test_retry_after_journal_settlement_does_not_release_again(self):
        item,args,path,original=self.pending_release(True);write=self.broker.scheduler.write_record
        def fail(p,value):
            if p!=path and value.get("state")=="done":raise RuntimeError("lost completion")
            return write(p,value)
        with patch.object(self.broker.scheduler,"write_record",side_effect=fail):
            with self.assertRaisesRegex(RuntimeError,"lost completion"):self.call("build_work",args)
        before=self.broker.scheduler.store.get(item["key"])
        self.assertEqual(self.call("build_work",args)["status"],"settled_committed")
        self.assertEqual(self.broker.scheduler.store.get(item["key"]),before)
        self.assertIsNone(self.broker.scheduler.pending(item["key"]))

    def test_stale_cas_wrong_request_and_changed_retry_rejected(self):
        item,args,path,original=self.pending_release()
        with self.assertRaisesRegex(ValueError,"Stale recovery"):
            self.call("build_work",{**args,"expected_revision":args["expected_revision"]-1})
        with self.assertRaises(FileNotFoundError):self.call("build_work",{**args,"original_request_id":"unknown-release-request"})
        self.call("build_work",args)
        with self.assertRaisesRegex(ValueError,"reused"):self.call("build_work",{**args,"expected_epoch":args["expected_epoch"]+1})

    def test_symlink_and_nonoperator_rejected(self):
        item,args,path,original=self.pending_release();other=path.with_suffix(".original");path.rename(other);path.symlink_to(other)
        with self.assertRaises(OSError):self.call("build_work",args)
        self.trusted.pop("local_caller")
        with self.assertRaises(PermissionError):self.call("build_work",args)

    def test_recovery_never_cancels_active_execution(self):
        item,args,path,original=self.pending_release()
        self.broker.jobs["fixture-active"]={"ownership_id":item["key"],"status":"running","created_at":time.time(),"operation":"build_deploy","job_id":"fixture-active"}
        result=self.call("build_work",args)
        self.assertEqual(result["status"],"aborted_uncommitted")
        self.assertEqual(self.broker.jobs["fixture-active"]["status"],"running")
        self.assertTrue(self.broker.scheduler.store.get(item["key"])["scopes"])

    def test_interrupted_abort_fences_original_retry_until_reconciled(self):
        item,args,path,original=self.pending_release();write=self.broker.scheduler.write_record
        def fail(p,value):
            if p==path and value.get("state")=="rejected":raise RuntimeError("interrupted settlement")
            return write(p,value)
        with patch.object(self.broker.scheduler,"write_record",side_effect=fail):
            with self.assertRaisesRegex(RuntimeError,"interrupted settlement"):self.call("build_work",args)
        before=self.broker.scheduler.store.get(item["key"])
        with self.assertRaisesRegex(ValueError,"reconciliation in progress"):self.call("build_work",original)
        self.assertEqual(self.call("build_work",args)["status"],"aborted_uncommitted")
        self.assertEqual(self.broker.scheduler.store.get(item["key"]),before)
        self.assertIsNone(self.broker.scheduler.pending(item["key"]))
