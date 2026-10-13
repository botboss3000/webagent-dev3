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


class PackagingStreamTests(unittest.TestCase):
 def test_fixed_command_accepts_bounded_file_stream(self):
  """The broker feeds a disk stream without retaining its entire packaging tar in memory."""
  import io
  runner=fixtures.host.Runner()
  result=runner.command(["python3","-I","-c","import sys; print(len(sys.stdin.buffer.read()))"],
      stdin=io.BytesIO(b"x"*(2*1024*1024)),timeout=10)
  self.assertEqual(result.strip(),str(2*1024*1024))
