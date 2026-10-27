"""Portable configuration and persistent build admission contract."""
import importlib.util,tempfile,time,types,unittest,uuid,json
from pathlib import Path
from unittest.mock import patch
import test_admin_build as fixtures
def module(name,path):
 spec=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m
config=module("portable_config",Path(__file__).resolve().parents[1]/"build-service/config.py")
class CandidateModeTests(unittest.TestCase):
    def test_mode_only_rewrite_is_not_a_change_but_executable_bit_is(self):
        release=fixtures.host.release;validate=fixtures.host.context_digest
        base=release.entries(fixtures.context(extra={"v3/docs/a.md":"same","v3/README.md":"echo"}),validate)
        task=dict(base);task["v3/docs/a.md"]=(0o600,base["v3/docs/a.md"][1])
        _,changed=release.candidate(release.archive(base,validate),release.archive(task,validate),release.archive(base,validate),validate,allow_empty=True)
        self.assertEqual(changed,[])
        task["v3/README.md"]=(base["v3/README.md"][0]|0o111,base["v3/README.md"][1])
        merged,changed=release.candidate(release.archive(base,validate),release.archive(task,validate),release.archive(base,validate),validate,allow_empty=True)
        self.assertEqual(changed,["v3/README.md"])
        self.assertEqual(release.entries(merged,validate)["v3/docs/a.md"],base["v3/docs/a.md"])

class LiveFileTests(unittest.TestCase):
    def setUp(self):
        self.release=fixtures.host.release;self.validate=fixtures.host.context_digest
    def manifest(self,*files):
        return json.dumps({"version":1,"files":list(files)})
    def blob(self,files):
        return fixtures.context(extra=files)
    def test_manifest_accepts_config_files_and_rejects_code_or_broker_paths(self):
        release=self.release
        parse=lambda *files:release.live_patterns({release.LIVE_MANIFEST:(0o644,self.manifest(*files).encode())})
        self.assertEqual(parse("ui/chat/chat.json","crates/modules/models/providers/*.json"),
                         ("v3/crates/modules/models/providers/*.json","v3/ui/chat/chat.json"))
        self.assertEqual(release.live_patterns({}),())
        for bad in ("src/main.rs","build-service/scheduler-defaults.json","tests/a.json","data/config/chat.json",
                    "crates/*/x.json","ui/../src/a.json","live-files.json",".git/config.json","ui/a b.json"):
            with self.assertRaises(ValueError,msg=bad):parse(bad)
        with self.assertRaises(ValueError):parse(*[f"ui/{n}.json"for n in range(257)])
        patterns=parse("crates/modules/models/providers/*.json")
        self.assertTrue(release.is_live("v3/crates/modules/models/providers/deepseek.json",patterns))
        self.assertFalse(release.is_live("v3/crates/modules/models/providers/old/deepseek.json",patterns))
        self.assertFalse(release.is_live("v3/crates/modules/models/src/lib.rs",patterns))
    def test_tasks_cannot_change_live_files_but_can_list_new_ones(self):
        release=self.release
        live=self.blob({release.LIVE_MANIFEST:self.manifest("ui/chat.json"),"v3/ui/chat.json":"{}","v3/README.md":"a"})
        edited=self.blob({release.LIVE_MANIFEST:self.manifest("ui/chat.json"),"v3/ui/chat.json":'{"x":1}',"v3/README.md":"b"})
        with self.assertRaisesRegex(ValueError,"edited in place"):release.candidate(live,edited,live,self.validate)
        _,changed=release.candidate(live,edited,live,self.validate,skip_live=True)
        self.assertEqual(changed,["v3/README.md"])
        # Creating a file and listing it in the same task is an ordinary source change.
        listed=self.blob({release.LIVE_MANIFEST:self.manifest("ui/chat.json","ui/new.json"),"v3/ui/chat.json":"{}","v3/ui/new.json":"{}","v3/README.md":"a"})
        _,changed=release.candidate(live,listed,live,self.validate)
        self.assertEqual(changed,[release.LIVE_MANIFEST,"v3/ui/new.json"])
        broken=self.blob({release.LIVE_MANIFEST:self.manifest("src/main.rs"),"v3/ui/chat.json":"{}","v3/README.md":"a"})
        with self.assertRaisesRegex(ValueError,"live-files.json"):release.candidate(live,broken,live,self.validate)
    def test_live_edits_are_not_checkout_drift(self):
        release=self.release
        live=self.blob({release.LIVE_MANIFEST:self.manifest("ui/chat.json"),"v3/ui/chat.json":"{}","v3/README.md":"a"})
        checkout=self.blob({release.LIVE_MANIFEST:self.manifest("ui/chat.json"),"v3/ui/chat.json":'{"x":1}',"v3/README.md":"a"})
        report=release.checkout_drift(live,checkout,live,[],self.validate)
        self.assertEqual((report["status"],report["total"]),("synchronized",0))
        other=self.blob({release.LIVE_MANIFEST:self.manifest("ui/chat.json"),"v3/ui/chat.json":'{"x":1}',"v3/README.md":"b"})
        report=release.checkout_drift(live,other,live,[],self.validate)
        self.assertEqual([row["path"]for row in report["files"]],["README.md"])
        self.assertEqual(report["digest"],release.checkout_drift(live,self.blob({release.LIVE_MANIFEST:self.manifest("ui/chat.json"),
            "v3/ui/chat.json":'{"x":2}',"v3/README.md":"b"}),live,[],self.validate)["digest"])
    def test_committer_commits_only_settled_live_paths_and_keeps_staged_work(self):
        import subprocess,threading
        with tempfile.TemporaryDirectory()as folder:
            root=Path(folder)
            git=lambda *args:subprocess.run(["git","-C",folder,"-c","user.name=t","-c","user.email=t@example.test",*args],check=True,capture_output=True,text=True).stdout
            git("init","-q");(root/"ui").mkdir();(root/"ui/chat.json").write_text("{}");(root/"README.md").write_text("a")
            git("add","-A");git("commit","-q","-m","base")
            def command(argv,timeout=None,private_raw=False,**_):
                return subprocess.run([argv[0],"-c","user.name=t","-c","user.email=t@example.test",*argv[1:]],check=True,capture_output=True,text=True).stdout
            runner=types.SimpleNamespace(command=command,git_lock=threading.Lock())
            patterns=("v3/ui/*.json",)
            (root/"README.md").write_text("staged");git("add","README.md")
            (root/"ui/chat.json").write_text('{"x":1}');(root/"ui/new.json").write_text("{}")
            with patch.dict(fixtures.host.CONFIG,{"checkout_root":folder}):
                commit_live=fixtures.host.Runner.commit_live
                self.assertIsNone(commit_live(runner,patterns,now=time.time()))
                head=commit_live(runner,patterns,now=time.time()+60)
                self.assertIsNone(commit_live(runner,patterns,now=time.time()+60))
            self.assertEqual(git("rev-parse","HEAD").strip(),head)
            self.assertEqual(git("show","--name-only","--format=%s","HEAD").split(),["Live","config","edit:","ui/chat.json,","ui/new.json","ui/chat.json","ui/new.json"])
            self.assertEqual(git("diff","--cached","--name-only").split(),["README.md"])
            self.assertEqual(git("show","HEAD:ui/chat.json"),'{"x":1}')
            # A file still equal to its published release was promoted; the release commit owns it.
            (root/"ui/released.json").write_text("{}");(root/"ui/edited.json").write_text("{}")
            released=lambda path:{"ui/released.json":b"{}","ui/edited.json":b"[]"}.get(path)
            with patch.dict(fixtures.host.CONFIG,{"checkout_root":folder}):
                fixtures.host.Runner.commit_live(runner,patterns,now=time.time()+60,released=released)
            self.assertEqual(git("show","--name-only","--format=","HEAD").split(),["ui/edited.json"])
            self.assertIn("ui/released.json",git("status","--porcelain"))
class LiveFileAccessTests(unittest.TestCase):
    def setUp(self):
        self.release=fixtures.host.release
    def test_loop_mounts_whole_checkout_read_only_and_stays_a_loop(self):
        with tempfile.TemporaryDirectory()as folder:
            root=Path(folder)/"release";(root/"v3/plugins/abilities").mkdir(parents=True)
            loop={"Mounts":[{"Type":"bind","Source":"/checkout/plugins","Target":"/live-checkout/plugins","ReadOnly":True},
                            {"Type":"bind","Source":"/old","Target":"/opt/v3/plugins/abilities","ReadOnly":True}]}
            mounted=self.release.release_mounts(loop,root)["Mounts"]
            checkout=next(m for m in mounted if m["Target"]=="/live-checkout")
            self.assertEqual((checkout["Source"],checkout["ReadOnly"]),("/checkout",True))
            # A read-only checkout does not make the loop look like the API on the next release.
            self.assertEqual(self.release.release_mounts({"Mounts":mounted},root)["Mounts"],mounted)
            # A loop holding both mounts (as deployed) must never produce a duplicate target.
            both=[*mounted,{"Type":"bind","Source":"/checkout/plugins","Target":"/live-checkout/plugins","ReadOnly":True}]
            targets=[m["Target"]for m in self.release.release_mounts({"Mounts":both},root)["Mounts"]]
            self.assertEqual(len(targets),len(set(targets)));self.assertIn("/live-checkout",targets)
    def test_expose_and_write_live_files(self):
        with tempfile.TemporaryDirectory()as folder:
            root=Path(folder);folder_path=root/"crates/x";folder_path.mkdir(parents=True);folder_path.chmod(0o700)
            live=folder_path/"a.json";live.write_text("{}");live.chmod(0o600);(folder_path/"b.rs").write_text("")
            (folder_path/"b.rs").chmod(0o600)
            self.release.expose_live(root,("v3/crates/x/*.json",))
            self.assertEqual(live.stat().st_mode&0o777,0o644);self.assertEqual(folder_path.stat().st_mode&0o777,0o705)
            self.assertEqual((folder_path/"b.rs").stat().st_mode&0o777,0o600)
            import hashlib
            current=hashlib.sha256(b"{}").hexdigest()
            with self.assertRaisesRegex(ValueError,"changed"):self.release.write_live(root,"crates/x/a.json",b'{"a":1}',None)
            digest=self.release.write_live(root,"crates/x/a.json",b'{"a":1}',current)
            self.assertEqual((live.read_text(),digest),('{"a":1}',hashlib.sha256(b'{"a":1}').hexdigest()))
            with self.assertRaisesRegex(ValueError,"changed"):self.release.write_live(root,"crates/x/a.json",b"{}",current)
            self.release.write_live(root,"crates/x/new.json",b"{}",None)
            (root/"link").symlink_to(folder_path)
            with self.assertRaisesRegex(ValueError,"link"):self.release.write_live(root,"link/a.json",b"{}",digest)
            self.assertEqual(sorted(p.name for p in folder_path.iterdir()),["a.json","b.rs","new.json"])
    def test_broker_writes_only_listed_valid_files(self):
        broker=types.SimpleNamespace(live_patterns=lambda:("v3/ui/chat.json",),runner=types.SimpleNamespace(git_lock=__import__("threading").Lock()))
        write=lambda args:fixtures.host.Broker.live_file_write(broker,args)
        with tempfile.TemporaryDirectory()as folder,patch.dict(fixtures.host.CONFIG,{"checkout_root":folder}):
            (Path(folder)/"ui").mkdir();(Path(folder)/"ui/chat.json").write_text("{}")
            import hashlib
            with self.assertRaisesRegex(ValueError,"not a live file"):write({"path":"ui/app.js","content":"x","expected_sha256":None})
            with self.assertRaisesRegex(ValueError,"invalid"):write({"path":"ui/chat.json","content":"{","expected_sha256":hashlib.sha256(b"{}").hexdigest()})
            result=write({"path":"ui/chat.json","content":'{"a":1}',"expected_sha256":hashlib.sha256(b"{}").hexdigest()})
            self.assertEqual(result["sha256"],hashlib.sha256(b'{"a":1}').hexdigest())
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
 def test_cpu_policy_requires_typed_budget_and_host_headroom(self):
  value=config.defaults("/srv/one");value.update(service_account="builder",source_owner="owner")
  with patch.object(config.os,"cpu_count",return_value=4),patch.object(config.pwd,"getpwnam",side_effect=self.account):
   for budget in (True,-1,999,3001):
    with self.assertRaises(ValueError):config.validate({**value,"scheduler":{**value["scheduler"],"cpu_budget_millis":budget}})
   for priority in ("",None,"unlimited"):
    with self.assertRaises(ValueError):config.validate({**value,"scheduler":{**value["scheduler"],"cpu_priority":priority}})
   accepted=config.validate({**value,"scheduler":{**value["scheduler"],"cpu_priority":"broker","cpu_budget_millis":3000}})
   self.assertEqual(config.cpu_policy(accepted["scheduler"]),{"cpu_priority":"broker","cpu_budget_millis":3000})

 def test_unprotected_config_rejected(self):
  with tempfile.TemporaryDirectory() as base:
   path=Path(base)/"config";path.write_text("{}");path.chmod(0o666)
   with self.assertRaises(PermissionError):config.load(path)
   link=Path(base)/"link";link.symlink_to(path)
   with self.assertRaises(PermissionError):config.load(link)
class LiveResourcePolicyTests(unittest.TestCase):
 def test_reload_requires_operator_protected_revision_and_matching_helper(self):
  from unittest.mock import Mock
  host=fixtures.host;broker=object.__new__(host.Broker);broker.runner=Mock();broker.resources=Mock()
  policy=host._config_module.resource_policy(host.CONFIG["scheduler"])
  fresh={**host.CONFIG,"scheduler":{**host.CONFIG["scheduler"],**policy,"cpu_priority":"broker"}}
  args={"action":"reload_resource_policy","expected_policy_sha256":"old","policy_sha256":host._config_module.resource_policy_digest(fresh["scheduler"])}
  with self.assertRaises(PermissionError):broker.reload_resource_policy(args,{})
  broker.resources.adopt_resource_policy.assert_not_called()
  with patch.object(host._config_module,"load",return_value=fresh),patch.object(host,"CONFIG",{**host.CONFIG,"scheduler":dict(host.CONFIG["scheduler"])}):
   with self.assertRaises(ValueError):broker.reload_resource_policy({**args,"policy_sha256":"wrong"},{"local_caller":True})
   broker.runner.resource_request.return_value={"sha256":"wrong","values":{}}
   with self.assertRaises(ValueError):broker.reload_resource_policy(args,{"local_caller":True})
   broker.resources.adopt_resource_policy.assert_not_called()
   broker.runner.resource_request.return_value={"sha256":host._config_module.memory_policy_digest(fresh["scheduler"]),"values":host._config_module.memory_policy(fresh["scheduler"])}
   broker.reload_resource_policy(args,{"local_caller":True})
   broker.resources.adopt_resource_policy.assert_called_once_with(fresh["scheduler"],"old")
   changed={**fresh,"port":1}
   with patch.object(host._config_module,"load",return_value=changed),self.assertRaisesRegex(ValueError,"maintenance"):broker.reload_resource_policy(args,{"local_caller":True})

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

class InstalledWorkspacePublicationTests(unittest.TestCase):
 def test_workspace_publication_requires_exact_protected_code(self):
  import types
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary);(root/"host.py").write_bytes(b"host");(root/"workspace.py").write_bytes(b"workspace")
   blob=fixtures.context(extra={"v3/build-service/host.py":"host","v3/build-service/workspace.py":"workspace"})
   broker=fixtures.host.Broker(root/"jobs",fixtures.FakeRunner());changed=["v3/build-service/workspace.py","v3/tests/test_admin_workspace.py"]
   protected=types.SimpleNamespace(st_mode=0o100644,st_uid=0,st_nlink=1)
   try:
    with patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"service_dir":str(root)}),patch.object(fixtures.host.os,"fstat",return_value=protected):
     self.assertTrue(broker.host_source_only(changed,blob,{"local_caller":True}))
     self.assertFalse(broker.host_source_only(changed,blob,{}))
     self.assertFalse(broker.host_source_only(changed+["v3/worker.py"],blob,{"local_caller":True}))
     (root/"workspace.py").write_bytes(b"different")
     self.assertFalse(broker.host_source_only(changed,blob,{"local_caller":True}))
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
     "workspace":fixture.workspace,"access_fingerprint":"f"*64,
     "compiler_checkpoint":{"fixture":"x"*100000}}
   trusted={"user_id":"admin","session_id":"00000000-0000-0000-0000-000000000000",
    "lease_until":time.time()+30,"access_fingerprint":"a"*64}
   result=fixture.call("v3_build_dashboard",trusted=trusted,workspace=trusted["session_id"])
   self.assertEqual(result["leftover_total"],65)
   self.assertEqual(len(result["leftovers"]),65)
   self.assertEqual(len(result["jobs"]),35)
   self.assertEqual([job["created_at"] for job in result["jobs"]],list(range(34,-1,-1)))
   self.assertNotIn("access_fingerprint",json.dumps(result))
   self.assertNotIn("compiler_checkpoint",json.dumps(result))
   self.assertLess(len(json.dumps(result)),256*1024)
   with self.assertRaises(PermissionError):fixture.call("v3_build_dashboard")
  finally:fixture.tearDown()


class DiagnosticsProbeTests(unittest.TestCase):
 def test_environment_redaction_does_not_corrupt_private_protocol_boolean(self):
  """A container env value can equal JSON true; parse this fixed private frame."""
  runner=fixtures.host.Runner();runner.secrets=["true"]
  reply=json.dumps({"version":2,"id":"diagnostics-protocol","result":{"status":"success","output":{"protocol":1,"interface":1,"stateful":True}}})
  def command(argv,**kwargs):
   return reply if kwargs.get("private_raw") else fixtures.host.scrub(reply,runner.secrets)
  with patch.object(runner,"command",side_effect=command) as called:
   self.assertTrue(runner.diagnostics_hot_swappable())
   self.assertTrue(called.call_args.kwargs["private_raw"])

class CodebasePublicationMetadataTests(unittest.TestCase):
 def test_worker_capability_probe_envelopes_are_fenced(self):
  runner=fixtures.host.Runner()
  runner.inspect=lambda name:{"Mounts":[{"Destination":"/opt/v3/assets","Source":str(fixtures.host.ASSET_ROOT),"RW":False}],"Config":{"Env":["V3_UI_ROOT=/opt/v3/assets/current/v3/ui"]}}
  probes=((runner.plugin_hot_swappable,"plugin-protocol",{"protocol":1,"catalog":1,"dispatch":1}),(runner.diagnostics_hot_swappable,"diagnostics-protocol",{"protocol":1,"interface":1,"stateful":True}))
  for probe,request_id,output in probes:
   def command(argv,**kwargs):
    self.assertIn("'version':2",argv[-1])
    self.assertIn(request_id,argv[-1])
    self.assertTrue(kwargs.get("private_raw"))
    compile(argv[-1],"<worker capability probe>","exec")
    return json.dumps({"version":2,"id":request_id,"result":{"status":"success","output":output}})
   with patch.object(runner,"command",side_effect=command):
    self.assertTrue(probe())
   for invalid in ({"status":"success","output":output},{"version":1,"id":request_id,"result":{"status":"success","output":output}},{"version":2,"id":"wrong-id","result":{"status":"success","output":output}},{"version":2,"id":request_id,"result":{"status":"success","output":{}}}):
    with patch.object(runner,"command",return_value=json.dumps(invalid)):
     self.assertFalse(probe())
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
  value=store.acquire(key,owner,value["revision"],value["epoch"],["module:broker","module:general","path:src/fixture.rs"],"frozen-acquire")["task"]
  identity=str(uuid.uuid4());blob=self.broker.scheduler_task_snapshot(Path(item["workspace_path"]).parent)
  folder=self.state/identity;folder.mkdir();(folder/"candidate.tar").write_bytes(blob)
  item.update(status="closed",job_id=identity)
  job={"job_id":identity,"ownership_id":key,"ownership_epoch":value["epoch"],"source_generation":value["generation"],"inputs_sha256":self.broker.scheduler_digest(blob),"user_id":self.trusted["user_id"],"workspace":self.sid,"status":"running","created_at":time.time(),"operation":"build_deploy","changed_paths":["v3/src/fixture.rs"]}
  self.broker.jobs[identity]=job
  args={"action":"release_claims","task_id":item["task_id"],"scopes":["module:broker"],"reason":"Yield completed broker editing","request_id":"frozen-release","expected_revision":value["revision"],"expected_epoch":value["epoch"],"frozen_job_id":identity}
  with patch.object(self.broker,"_start_next"):
   result=self.call("build_work",args)
  self.assertEqual(result["epoch"],value["epoch"])
  self.assertEqual(result["scopes"],["module:general","path:src/fixture.rs"])
  self.assertEqual(self.call("build_work",args),result)
  self.broker.scheduler.assert_job(job)
  args.update(scopes=["module:general"],request_id="reject-promotion-release",expected_revision=result["revision"])
  with self.assertRaisesRegex(ValueError,"promotion"):self.call("build_work",args)
  self.assertIsNone(self.broker.scheduler.pending(key))

 def test_tree_inputs_digest_reuses_only_unchanged_trees(self):
  import hashlib
  self.call("v3_build_work",{"action":"begin","task_id":"tree-digest-fixture","summary":"Digest fixture","mode":"isolated"})
  key=hashlib.sha256(fixtures.host.canonical([self.trusted["user_id"],self.sid,"tree-digest-fixture"])).hexdigest()
  root=self.broker.work_dir/"trees"/key;first=self.broker._tree_inputs_digest(root)
  self.assertEqual(self.broker._tree_inputs_digest(root),first)
  source=Path(self.broker.work[key]["workspace_path"])/"src"/"digest_fixture.rs";source.parent.mkdir(parents=True,exist_ok=True);source.write_text("fn changed() {}\n")
  self.assertNotEqual(self.broker._tree_inputs_digest(root),first)

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

    def test_pending_withdrawal_unblocks_successor_without_reopening_source(self):
        original,job=self.proposal();store=self.broker.scheduler.store
        holder=self.begin("successor-queue-holder");v=store.get(holder["key"])
        store.acquire(v["id"],v["owner"],v["revision"],v["epoch"],["module:engine"],"successor-holder")
        v=store.get(original["key"])
        waiting=store.acquire(v["id"],v["owner"],v["revision"],v["epoch"],["module:engine"],"successor-wait")["task"]
        source=self.broker.scheduler_task_snapshot(Path(original["workspace_path"]).parent)
        item,args=self.request(job)
        with self.assertRaisesRegex(ValueError,"reservations"):self.call("build_work",args)
        result=self.call("build_work",{"action":"clear_pending_requests","task_id":original["task_id"],"expected_revision":waiting["revision"],"expected_epoch":waiting["epoch"],"reason":"Import preserved source","request_id":"successor-withdraw"})
        v=store.get(holder["key"])
        store.release_scopes(v["id"],v["owner"],v["revision"],v["epoch"],v["scopes"],"fixture holder finished","successor-holder-release")
        args["request_id"]="successor-after-withdraw"
        self.assertEqual(self.call("build_work",args)["status"],"imported")
        self.assertEqual(result["generation"],waiting["generation"])
        self.assertEqual(self.broker.scheduler_task_snapshot(Path(original["workspace_path"]).parent),source)
        self.assertEqual(original["status"],"closed")

    def test_same_file_conflict_does_not_claim_or_write(self):
        original,job=self.proposal()
        self.runner.blob=fixtures.context(extra={"v3/README.md":"other writer\n"})
        item,args=self.request(job)
        with self.assertRaisesRegex(ValueError,"source conflict"):self.call("build_work",args)
        self.assertFalse(self.broker.scheduler.store.get(item["key"])["scopes"])
        self.assertEqual((Path(item["workspace_path"])/"README.md").read_text(),"other writer\n")

    def test_atomic_transfer_preserves_closed_source_and_review_lookup(self):
        original,job=self.proposal(False);item,args=self.request(job);args["transfer_claims"]=True
        held=self.broker.scheduler.store.get(original["key"])
        result=self.call("build_work",args)
        self.assertEqual(result["status"],"imported")
        self.assertEqual(original["status"],"closed")
        source=self.broker.scheduler.store.get(original["key"])
        self.assertEqual(source["state"],"abandoned");self.assertEqual(source["epoch"],held["epoch"]+1)
        self.assertEqual(source["scopes"],[])
        self.assertEqual(set(self.broker.scheduler.store.get(item["key"])["scopes"]),set(held["scopes"]))
        self.assertIn(original["key"],self.broker.review_policy.task_keys())
        self.assertEqual(self.call("build_work",args),result)
        self.assertEqual((Path(original["workspace_path"])/"README.md").read_text(),"proposal\n")

    def test_transfer_crash_after_sqlite_commit_recovers_once(self):
        original,job=self.proposal(False);item,args=self.request(job);args["transfer_claims"]=True
        normal=self.broker.scheduler.store.transfer
        def lost(**kwargs):normal(**kwargs);raise RuntimeError("lost transfer response")
        with patch.object(self.broker.scheduler.store,"transfer",side_effect=lost):
            with self.assertRaisesRegex(RuntimeError,"lost transfer"):self.call("build_work",args)
        epoch=self.broker.scheduler.store.get(original["key"])["epoch"]
        self.assertEqual(self.call("build_work",args)["status"],"imported")
        self.assertEqual(self.broker.scheduler.store.get(original["key"])["epoch"],epoch)

    def test_transfer_conflict_does_not_move_claims(self):
        original,job=self.proposal(False)
        self.runner.blob=fixtures.context(extra={"v3/README.md":"conflict\n"})
        item,args=self.request(job);args["transfer_claims"]=True
        before=self.broker.scheduler.store.get(original["key"])
        with self.assertRaisesRegex(ValueError,"source conflict"):self.call("build_work",args)
        self.assertEqual(before,self.broker.scheduler.store.get(original["key"]))
        self.assertFalse(self.broker.scheduler.store.get(item["key"])["scopes"])

    def test_transfer_rejects_unrelated_scopes_and_stale_retry(self):
        original,job=self.proposal(False);item,args=self.request(job);args["transfer_claims"]=True
        store=self.broker.scheduler.store;value=store.get(original["key"])
        store.acquire(value["id"],value["owner"],value["revision"],value["epoch"],[*value["scopes"],"path:docs/extra.md"],"extra-successor-scope")
        with self.assertRaisesRegex(ValueError,"Yield unrelated"):self.call("build_work",args)

    def test_transferred_apply_response_loss_and_source_resumption_are_fenced(self):
        original,job=self.proposal(False);item,args=self.request(job);args["transfer_claims"]=True
        normal=self.broker.handle
        def blocked(request):
            if request.get("name")=="v3_source_apply":raise RuntimeError("lost before apply")
            return normal(request)
        with patch.object(self.broker,"handle",side_effect=blocked):
            with self.assertRaisesRegex(RuntimeError,"lost before apply"):self.call("build_work",args)
        self.assertFalse(self.broker.scheduler.store.get(original["key"])["scopes"])
        job["effects_started"]=True
        with self.assertRaisesRegex(ValueError,"source resumed"):self.call("build_work",args)
        job["effects_started"]=False
        result=self.call("build_work",args);value=self.broker.scheduler.store.get(item["key"])
        self.assertEqual(result["status"],"imported")
        self.assertEqual(self.call("build_work",args),result)
        self.assertEqual(value,self.broker.scheduler.store.get(item["key"]))

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


class FeatureProfileBrokerTests(unittest.TestCase):
    from test_scheduler_broker import SchedulerBrokerTests as _base
    setUp=_base.setUp
    tearDown=_base.tearDown
    call=_base.call
    begin=_base.begin
    def request(self):
        item=self.begin("feature-profile-fixture");tree=Path(item["workspace_path"]).parent
        return item,{"action":"check","task_id":item["task_id"],"check":"check","expected_inputs_sha256":self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(tree)),"timeout_seconds":30,"request_id":"feature-check-request"}
    def test_profiles_bind_receipts_and_default_signature_remains_compatible(self):
        import hashlib
        item,args=self.request();self.broker.resources.enabled=True
        with patch.object(self.broker,"_start_next"):
            default=self.call("build_work",args)
            job=self.broker.jobs[default["job_id"]]
            signature=[item["task_id"],"check",args["expected_inputs_sha256"],30]
            self.assertEqual(job["check_signature"],hashlib.sha256(fixtures.host.canonical(signature)).hexdigest())
            self.assertEqual(default["feature_profile"],"default")
            with self.assertRaisesRegex(ValueError,"different inputs"):self.call("build_work",{**args,"feature_profile":"live"})
            live=self.call("build_work",{**args,"request_id":"live-check-request","feature_profile":"live"})
            self.assertEqual(live["feature_profile"],"live");self.assertNotEqual(live["job_id"],default["job_id"])
            self.assertNotEqual(self.broker.jobs[live["job_id"]]["check_signature"],job["check_signature"])
            call=self.broker.check_runs[live["job_id"]]
            with patch.object(self.runner.executor,"check",return_value="live compiler result"):
                self.broker._execute_check(*call)
            receipt=self.call("build_work",{**args,"request_id":"live-check-request","feature_profile":"live"})
            self.assertEqual(receipt["feature_profile"],"live");self.assertEqual(receipt["status"],"completed")
            self.assertEqual(self.broker._view(self.broker.jobs[live["job_id"]])["feature_profile"],"live")
        self.assertFalse(self.runner.calls)
        self.assertEqual(item["status"],"open");self.assertNotIn("job_id",item)
    def test_invalid_profiles_never_create_jobs(self):
        item,args=self.request();self.broker.resources.enabled=True
        for value in ["arbitrary","--features all",[],{},True]:
            with self.assertRaisesRegex(ValueError,"Unsupported"):self.call("build_work",{**args,"feature_profile":value})
        with self.assertRaisesRegex(ValueError,"only to compile"):self.call("build_work",{**args,"check":"test","feature_profile":"live"})
        self.assertFalse(self.broker.jobs)
    def test_live_requires_normal_resource_admission(self):
        item,args=self.request();self.broker.resources.enabled=False
        with self.assertRaisesRegex(PermissionError,"resource admission"):self.call("build_work",{**args,"feature_profile":"live"})

    def test_live_input_race_is_stale_without_publication(self):
        item,args=self.request();self.broker.resources.enabled=True
        with patch.object(self.broker,"_start_next"):
            result=self.call("build_work",{**args,"feature_profile":"live"})
            invocation=self.broker.check_runs[result["job_id"]]
            def mutate(*arguments):
                (Path(item["workspace_path"])/"README.md").write_text("changed while checking\n")
                return "compiler passed"
            with patch.object(self.runner.executor,"check",side_effect=mutate):
                receipt=self.broker._execute_check(*invocation)
        self.assertEqual(receipt["status"],"stale");self.assertEqual(receipt["feature_profile"],"live")
        self.assertNotEqual(receipt["inputs_sha256"],args["expected_inputs_sha256"])
        self.assertFalse(self.runner.calls);self.assertEqual(item["status"],"open")
    def test_unified_forwarding_preserves_profile_and_request_identity(self):
        item,args=self.request()
        with patch.object(self.broker,"cargo_request",return_value={"status":"queued"})as call:
            self.broker.unified_build({"trusted":self.trusted,"workspace":self.workspace},{**args,"feature_profile":"live"})
        self.assertEqual(call.call_args.args[0]["feature_profile"],"live")
        self.assertEqual(call.call_args.args[0]["request_id"],args["request_id"])

class SubmissionRecoveryTests(unittest.TestCase):
    """Unchanged source can submit after yielding claims without receipt rewrites."""
    from test_scheduler_broker import SchedulerBrokerTests as _base
    setUp=_base.setUp
    tearDown=_base.tearDown
    call=_base.call
    begin=_base.begin
    write=_base.write

    def proposal(self):
        item=self.begin("unchanged-submit-fixture")
        self.write(item,"README.md","retained maintenance proposal\n")
        blob=self.broker.scheduler_task_snapshot(Path(item["workspace_path"]).parent)
        return item,self.broker.scheduler_digest(blob)

    def submit(self,item,digest):
        self.broker.scheduler.submit(item,self.trusted,self.workspace,["v3/README.md"],digest)
        return self.broker.scheduler.store.get(item["key"])

    def release(self,item):
        value=self.broker.scheduler.store.get(item["key"])
        return self.call("build_work",{"action":"release_claims","task_id":item["task_id"],
            "scopes":value["scopes"],"reason":"Yield unpublished maintenance claims",
            "request_id":"submit-yield-"+str(value["revision"]),
            "expected_revision":value["revision"],"expected_epoch":value["epoch"]})

    def test_legacy_receipt_survives_release_and_unchanged_resubmission(self):
        import hashlib
        item,digest=self.proposal();scheduler=self.broker.scheduler;store=scheduler.store
        value=store.get(item["key"])
        legacy="submit-"+hashlib.sha256((item["key"]+digest).encode()).hexdigest()[:48]
        original=store.edited(value["id"],value["owner"],value["revision"],value["epoch"],legacy)
        item["ownership_inputs_sha256"]=digest;item["source_generation"]=original["generation"]
        self.broker._save_work();self.release(item)
        self.assertNotIn("ownership_inputs_sha256",item)
        result=self.submit(item,digest)
        self.assertEqual(result["generation"],original["generation"]+1)
        self.assertEqual(store.receipt(legacy),original)
        self.assertEqual(self.submit(item,digest)["generation"],result["generation"])
        current=self.broker.scheduler_digest(self.broker.scheduler_task_snapshot(Path(item["workspace_path"]).parent))
        self.assertEqual(current,digest)
        self.assertFalse(self.broker.jobs)

    def test_committed_submission_recovers_lost_work_save_after_restart(self):
        item,digest=self.proposal();saved=dict(item)
        with patch.object(self.broker,"_save_work",side_effect=OSError("fixture lost work save")):
            with self.assertRaises(OSError):self.submit(item,digest)
        store=self.broker.scheduler.store
        committed=store.get(item["key"])
        item.clear();item.update(saved)
        self.broker.scheduler.store=type(store)(store.path)
        recovered=self.submit(item,digest)
        self.assertEqual(recovered["generation"],committed["generation"])
        self.assertEqual(recovered["revision"],committed["revision"])
        self.assertEqual(item["source_generation"],committed["generation"])
        self.assertEqual(item["ownership_inputs_sha256"],digest)

    def test_stale_work_marker_cannot_reuse_released_epoch(self):
        item,digest=self.proposal();first=self.submit(item,digest)
        self.release(item);item["ownership_inputs_sha256"]=digest
        second=self.submit(item,digest)
        self.assertGreater(second["epoch"],first["epoch"])
        self.assertEqual(second["generation"],first["generation"]+1)
        self.assertEqual(second["evidence"]["submission"]["epoch"],second["epoch"])


class RuntimeReadabilityTests(unittest.TestCase):
    """Validate proposed mount isolation and identities before a candidate can affect runtime."""
    def setUp(self):
        import copy
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
        self.assets=self.root/"assets";self.assets.mkdir();self.identity=str(uuid.uuid4());self.publication=self.assets/"releases"/self.identity
        (self.publication/"v3/plugins/abilities").mkdir(parents=True);(self.publication/"v3/config").mkdir();(self.assets/"artifacts").mkdir()
        self.config={**fixtures.host.CONFIG,"state_root":str(self.root/"jobs")}
        self.previous={role:{"Id":role+"-before","Image":"sha256:"+("1"if role=="api"else"2")*64,
            "HostConfig":{"Mounts":[{"Type":"bind","Source":str(self.root/"private-data"),"Target":"/state","ReadOnly":False},
                {"Type":"bind","Source":str(self.root/"live"),"Target":"/live-checkout","ReadOnly":False},
                {"Type":"bind","Source":str(self.root/"socket"),"Target":"/run/control.sock","ReadOnly":True},
                {"Type":"bind","Source":str(self.root/"old"),"Target":"/live-v3","ReadOnly":True},
                {"Type":"bind","Source":str(self.root/"old"),"Target":"/opt/v3/plugins/abilities","ReadOnly":True}]}}for role in ("api","loop")}
        self.job={"job_id":self.identity,"inputs_sha256":"a"*64,"images":{role:value["Image"]for role,value in self.previous.items()}}
        self.calls=[]
        def command(argv,timeout):
            self.calls.append(argv)
            return json.dumps({"files":10,"runtime_files":5}if "python3"in argv else{"catalog_version":"fixture"})
        self.runner=types.SimpleNamespace(command=command,inspect=lambda name:self.previous["api"if name==fixtures.host.TARGETS[0]else"loop"],current_asset_id=lambda:self.identity)
    def run_preflight(self,publication_id=None):
        with patch.object(fixtures.host,"CONFIG",self.config),patch.object(fixtures.host,"ASSET_ROOT",self.assets),                patch.object(fixtures.host.release,"verify_publication",return_value={"candidate_sha256":"a"*64}),                patch.object(fixtures.host.subprocess,"run"):
            return fixtures.host.Runner.preflight_runtime_mounts(self.runner,self.job,self.previous,publication_id)
    def test_checks_both_application_images_and_worker_with_source_only_mounts(self):
        proof=self.run_preflight()
        self.assertEqual(len(self.calls),4);self.assertEqual(proof["status"],"passed")
        self.assertEqual([(c["role"],c["identity"])for c in proof["checks"]],[("api","application"),("api","worker_catalog"),("api","worker"),("loop","application")])
        for argv in self.calls:
            self.assertEqual(argv[argv.index("--network")+1],"none");self.assertIn("--read-only",argv)
            self.assertNotIn("/state"," ".join(argv));self.assertNotIn("control.sock"," ".join(argv))
            for i,arg in enumerate(argv):
                if arg=="--mount":self.assertTrue(argv[i+1].endswith(",readonly"))
        self.assertEqual(self.calls[0][self.calls[0].index("--user")+1],str(self.config["app_uid"])+":"+str(self.config["app_gid"]))
        self.assertEqual(self.calls[1][self.calls[1].index("--user")+1],str(self.config["worker_uid"])+":"+str(self.config["worker_gid"]))
    def test_worker_probe_checks_baked_bytes_and_compiles_all_five_scripts(self):
        import subprocess
        self.run_preflight();argv=next(c for c in self.calls if "python3"in c);program=argv[argv.index("-c")+1]
        self.assertIn("/opt/v3",program);self.assertIn("Baked runtime Python source differs",program)
        self.assertIn("compile(body",program);self.assertIn("runtime_files",program)
        self.assertEqual(argv[argv.index("--user")+1],str(self.config["worker_uid"])+":"+str(self.config["worker_gid"]))
        baked=self.root/"baked";baked.mkdir()
        for name in ("worker.py","ability_plugin_worker.py","sandbox.py","launch.py","promote.py"):
            (baked/name).write_text("pass\n");(self.publication/"v3"/name).write_text("pass\n")
        program=program.replace('pathlib.Path("/opt/v3")','pathlib.Path('+repr(str(baked))+')')
        result=subprocess.run(["python3","-I","-B","-c",program,str(self.publication)],capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr);self.assertEqual(json.loads(result.stdout)["runtime_files"],5)
        (baked/"worker.py").write_text("changed=True\n")
        result=subprocess.run(["python3","-I","-B","-c",program,str(self.publication)],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0);self.assertIn("differs from frozen",result.stderr)
    def test_incomplete_baked_probe_does_not_certify_readability(self):
        self.runner.command=lambda argv,timeout:json.dumps({"files":10}if "python3"in argv else{"catalog_version":"fixture"})
        with self.assertRaisesRegex(ValueError,"incomplete"):self.run_preflight()
        self.assertNotIn("runtime_preflight",self.job)

    def test_failure_does_not_record_success_or_change_current_pointer(self):
        def rejected(argv,timeout):raise RuntimeError("candidate unreadable")
        self.runner.command=rejected
        with self.assertRaisesRegex(RuntimeError,"candidate unreadable"):self.run_preflight()
        self.assertNotIn("runtime_preflight",self.job);self.assertFalse((self.assets/"current").exists())
    def test_changed_runtime_and_pointer_are_fenced(self):
        before=self.runner.inspect
        self.runner.inspect=lambda name:{**before(name),"Id":"newer-runtime"}
        with self.assertRaisesRegex(ValueError,"Runtime changed"):self.run_preflight()
        self.runner.inspect=before;self.runner.current_asset_id=lambda:str(uuid.uuid4())
        with self.assertRaisesRegex(ValueError,"source changed"):self.run_preflight(self.identity)
    def test_private_view_symlink_is_rejected(self):
        view=Path(self.config["state_root"])/self.identity/"runtime-preflight-view";view.parent.mkdir(parents=True);view.symlink_to(self.root)
        with self.assertRaisesRegex(ValueError,"view symlink"):self.run_preflight()
    def test_semantically_invalid_catalog_cannot_pass(self):
        self.runner.command=lambda argv,timeout:"{}"
        with self.assertRaisesRegex(ValueError,"catalog validator"):self.run_preflight()

    def test_actual_worker_probe_rejects_malformed_configuration(self):
        import subprocess,sys
        self.run_preflight();argv=next(c for c in self.calls if "python3"in c);program=argv[argv.index("-c")+1]
        source=self.root/"worker-fixture";(source/"v3/config").mkdir(parents=True);(source/"v3/config/example.json").write_text("{}")
        baked=self.root/"configuration-runtime";baked.mkdir()
        for name in ("worker.py","ability_plugin_worker.py","sandbox.py","launch.py","promote.py"):
            (baked/name).write_text("pass\n");(source/"v3"/name).write_text("pass\n")
        program=program.replace('pathlib.Path("/opt/v3")','pathlib.Path('+repr(str(baked))+')')
        passed=subprocess.run([sys.executable,"-c",program,str(source)],capture_output=True,timeout=5)
        self.assertEqual(passed.returncode,0)
        (source/"v3/config/example.json").write_text("malformed-json")
        failed=subprocess.run([sys.executable,"-c",program,str(source)],capture_output=True,timeout=5)
        self.assertNotEqual(failed.returncode,0)

    def test_restrictive_broker_umask_does_not_hide_public_probe_directories(self):
        import os
        previous=os.umask(0o077)
        try:
            self.run_preflight()
            view=Path(self.config["state_root"])/self.identity/"runtime-preflight-view"
            for folder in (view,view/"releases",view/"releases"/self.identity,view/"artifacts"):
                self.assertEqual(folder.stat().st_mode&0o777,0o755)
        finally:os.umask(previous)

class RuntimePrestopGateTests(unittest.TestCase):
    setUp=fixtures.BuildTests.setUp
    tearDown=fixtures.BuildTests.tearDown
    call=fixtures.BuildTests.call
    build=fixtures.BuildTests.build
    wait=fixtures.BuildTests.wait
    deploy=fixtures.BuildTests.deploy
    def test_unreadable_candidate_fails_before_any_consumer_mutation(self):
        import copy
        built=self.wait(self.build());before=copy.deepcopy(self.runner.containers);self.runner.calls.clear()
        def unreadable(job,previous):raise ValueError("runtime mount unreadable")
        self.runner.preflight_runtime_mounts=unreadable
        result=self.wait(self.deploy(built))
        self.assertEqual(result["status"],"failed");self.assertEqual(result["phase"],"checking_runtime_mounts")
        self.assertEqual(self.runner.containers,before)
        self.assertFalse(any(c[0]in {"stop","rename","create","start","activate_assets","promote"}for c in self.runner.calls))
    def test_successful_gate_precedes_first_stop(self):
        built=self.wait(self.build());self.runner.calls.clear()
        self.runner.preflight_runtime_mounts=lambda job,previous:self.runner.calls.append(("runtime_mount_preflight",))
        result=self.wait(self.deploy(built));self.assertEqual(result["status"],"completed")
        gate=next(i for i,c in enumerate(self.runner.calls)if c[0]=="runtime_mount_preflight")
        self.assertTrue(all(gate<i for i,c in enumerate(self.runner.calls)if c[0]=="stop"))

class RecoveryDockerBoundaryTests(unittest.TestCase):
 def test_discovery_distinguishes_absence_errors_and_identity_races(self):
  from unittest.mock import Mock
  runner=fixtures.host.Runner();name=fixtures.host.TARGETS[0]
  runner.command=Mock(return_value="")
  self.assertIsNone(runner.inspect_recovery(name))
  runner.command.side_effect=RuntimeError("Docker unavailable")
  with self.assertRaisesRegex(RuntimeError,"Docker unavailable"):runner.inspect_recovery(name)
  runner.command=Mock(return_value="a"*64+"\n")
  with patch.object(runner,"inspect",return_value={"Id":"b"*64,"Name":"/"+name}):
   with self.assertRaisesRegex(ValueError,"discovery changed"):runner.inspect_recovery(name)
  with self.assertRaisesRegex(ValueError,"outside development"):runner.inspect_recovery("production")

 def test_created_release_containers_have_exact_recovery_labels(self):
  from unittest.mock import Mock
  runner=fixtures.host.Runner();job=str(uuid.uuid4());runner.job_context.job={"job_id":job}
  runner.command=Mock(return_value=json.dumps([{"Config":{}}]))
  connection=Mock();response=connection.getresponse.return_value;response.status=201;response.read.return_value=json.dumps({"Id":"a"*64}).encode()
  previous={"Image":"sha256:"+"1"*64,"Config":{"Env":[],"Labels":{"existing":"preserved"}},"HostConfig":{}}
  with patch.object(fixtures.host,"DockerHTTP",return_value=connection),patch.object(runner,"published_runtime_source",return_value=Path("/published")),patch.object(fixtures.host.release,"release_mounts",return_value={}):
   self.assertEqual(runner.create(fixtures.host.TARGETS[0],"sha256:"+"2"*64,previous,"/frozen"),"a"*64)
  labels=json.loads(connection.request.call_args.kwargs["body"])["Labels"]
  self.assertEqual(labels,{"existing":"preserved","webagent.release.job":job,"webagent.release.role":"api","webagent.release.instance":fixtures.host.CONFIG["instance"]})
 def test_rejected_create_keeps_a_bounded_scrubbed_docker_reason(self):
  from unittest.mock import Mock
  runner=fixtures.host.Runner();runner.secrets=["inspected-private-value"]
  runner.command=Mock(return_value=json.dumps([{"Config":{}}]))
  private="private-env-value-"+uuid.uuid4().hex
  previous={"Image":"sha256:"+"1"*64,"Config":{"Env":["SETTING="+private]},"HostConfig":{}}
  message="Duplicate mount point: /live-checkout; "+private+" inspected-private-value "+"x"*900
  connection=Mock();response=connection.getresponse.return_value;response.status=400;response.read.return_value=json.dumps({"message":message}).encode()
  with patch.object(fixtures.host,"DockerHTTP",return_value=connection):
   with self.assertRaises(RuntimeError)as caught:runner.create(fixtures.host.TARGETS[1],"sha256:"+"2"*64,previous)
  reason=str(caught.exception)
  self.assertTrue(reason.startswith("development container create failed (400): Duplicate mount point: /live-checkout"))
  self.assertNotIn(private,reason);self.assertNotIn("inspected-private-value",reason);self.assertLess(len(reason),400)
  # Non-JSON bodies are bounded too, and a duplicate target is refused before Docker is asked.
  response.read.return_value=b"<html>"+private.encode()+b"</html>"
  with patch.object(fixtures.host,"DockerHTTP",return_value=connection):
   with self.assertRaises(RuntimeError)as caught:runner.create(fixtures.host.TARGETS[1],"sha256:"+"2"*64,previous)
  self.assertNotIn(private,str(caught.exception))
  duplicate={**previous,"HostConfig":{"Mounts":[{"Type":"bind","Source":"/a","Target":"/live-checkout"},{"Type":"bind","Source":"/b","Target":"/live-checkout/"}]}}
  connection.reset_mock()
  with patch.object(fixtures.host,"DockerHTTP",return_value=connection):
   with self.assertRaisesRegex(ValueError,"duplicate /live-checkout"):runner.create(fixtures.host.TARGETS[0],"sha256:"+"2"*64,duplicate)
  connection.request.assert_not_called()

 def test_consecutive_releases_inherit_stable_mounts_and_settings(self):
  """Each release inherits the previous container; a later one must not drift (job 204bdea6)."""
  from unittest.mock import Mock
  with tempfile.TemporaryDirectory()as folder:
   root=Path(folder);checkout=root/"checkout";assets=root/"assets";data=root/"data"
   for path in ("plugins","workspaces","runtime","gate-runtime"):(checkout/path).mkdir(parents=True)
   data.mkdir();assets.mkdir()
   def published(job):
    target=assets/"releases"/job
    for path in ("v3/config","v3/plugins/abilities"):(target/path).mkdir(parents=True,exist_ok=True)
    return target
   bind=lambda source,target,ro=False:{"Type":"bind","Source":str(source),"Target":target,"ReadOnly":ro}
   first=published(str(uuid.uuid4()))
   common=[bind(first/"v3/config","/opt/v3/config",True),bind(checkout/"workspaces","/workspaces"),bind(checkout/"runtime","/run/v3"),
           bind(assets,"/opt/v3/assets",True),bind(data,"/state")]
   tmpfs={"/run":"rw,nosuid,nodev,size=16m","/tmp":"rw,nosuid,nodev,size=32m"}
   # The deployed shapes: the API's writable checkout; the loop's read-only checkout plus nested plugins.
   containers={"api":{"Mounts":[*common,bind(first/"v3","/live-v3",True),bind(checkout,"/live-checkout")],"Tmpfs":tmpfs,"PidsLimit":10},
               "loop":{"Mounts":[*common,bind(checkout/"gate-runtime","/run/v3/gate"),bind(first/"v3/plugins/abilities","/opt/v3/plugins/abilities",True),
                                 bind(checkout,"/live-checkout",True),bind(checkout/"plugins","/live-checkout/plugins",True)],"Tmpfs":tmpfs,"Memory":5}}
   image={"Env":["PATH=/usr/bin"],"Cmd":["v3"],"Labels":{}}
   previous={role:{"Image":"sha256:"+"1"*64,"Config":{**image,"Env":["PATH=/usr/bin","SETTING=kept"]},"HostConfig":host}
             for role,host in containers.items()}
   runner=fixtures.host.Runner();runner.command=Mock(return_value=json.dumps([{"Config":image}]))
   generations=[]
   with patch.object(fixtures.host,"SOURCE",checkout),patch.object(fixtures.host,"ASSET_ROOT",assets):
    for _ in range(3):
     job=str(uuid.uuid4());runner.job_context.job={"job_id":job}
     with patch.object(runner,"published_runtime_source",return_value=published(job)):
      configs={role:runner.container_config(name,"sha256:"+"2"*64,previous[role],"/frozen")for role,name in zip(("api","loop"),fixtures.host.TARGETS)}
     generations.append((job,configs))
     previous={role:{"Image":config["Image"],"Config":{k:v for k,v in config.items()if k!="HostConfig"},"HostConfig":config["HostConfig"]}
               for role,config in configs.items()}
   def shape(job,config):
    # Only the release directory may change between generations.
    mounts=sorted((m["Target"],m["Source"].replace(job,"JOB"),bool(m.get("ReadOnly")))for m in config["HostConfig"]["Mounts"])
    host={k:v for k,v in config["HostConfig"].items()if k!="Mounts"}
    labels={k:v for k,v in config["Labels"].items()if k!="webagent.release.job"}
    return mounts,host,sorted(config["Env"]),labels,config.get("Cmd")
   for role in ("api","loop"):
    shapes=[shape(job,configs[role])for job,configs in generations]
    self.assertEqual(shapes[1],shapes[2],role)
    targets=[m[0]for m in shapes[2][0]]
    self.assertEqual(len(targets),len(set(targets)),role)
    self.assertIn("SETTING=kept",shapes[2][2])
   job,configs=generations[2]
   loop={m[0]:m[1:]for m in shape(job,configs["loop"])[0]}
   self.assertEqual(loop["/live-checkout"],(str(checkout),True));self.assertEqual(loop["/live-checkout/plugins"],(str(checkout/"plugins"),True))
   api={m[0]:m[1:]for m in shape(job,configs["api"])[0]}
   self.assertEqual(api["/live-checkout"],(str(checkout),False))

class AutomaticBrokerIdentityTests(unittest.TestCase):
    from test_scheduler_broker import SchedulerBrokerTests as _base
    setUp=_base.setUp
    tearDown=_base.tearDown
    call=_base.call

    def write_new(self,path="docs/first.md",request="automatic-first-write",**kwargs):
        return self.call("file_write",{"path":path,"content":"first\n","broker_id":"new","request_id":request,**kwargs})

    def test_new_default_switch_and_returned_identity(self):
        first=self.write_new();bid=first["broker_id"]
        self.assertTrue(first["task_created"])
        second=self.write_new("docs/second.md","automatic-second-write")
        self.assertNotEqual(bid,second["broker_id"])
        current=self.call("file_read",{"path":"docs/second.md"})
        self.assertEqual(current["broker_id"],second["broker_id"])
        switched=self.call("file_read",{"path":"docs/first.md","broker_id":bid})
        self.assertEqual(switched["broker_id"],bid)
        self.assertEqual(self.call("file_read",{"path":"docs/first.md"})["broker_id"],bid)

    def test_retry_new_after_restart_never_creates_twice(self):
        args={"path":"docs/first.md","content":"first\n","broker_id":"new","request_id":"automatic-first-write"}
        first=self.call("file_write",args);count=len(self.broker.work)
        state=self.broker.state;self.broker.shutdown();self.broker=fixtures.host.Broker(state,self.runner)
        self.assertEqual(self.call("file_write",args),first);self.assertEqual(len(self.broker.work),count)
        with self.assertRaisesRegex(ValueError,"reused"):self.call("file_write",{**args,"content":"different"})

    def test_branch_defaults_do_not_redirect_other_branches(self):
        self.trusted["broker_scope"]="branch-one";first=self.write_new()
        self.trusted["broker_scope"]="branch-two";second=self.write_new("docs/second.md","automatic-second-write")
        self.trusted["broker_scope"]="branch-one"
        self.assertEqual(self.call("file_read",{"path":"docs/first.md"})["broker_id"],first["broker_id"])
        self.trusted["broker_scope"]="branch-two"
        self.assertEqual(self.call("file_read",{"path":"docs/second.md"})["broker_id"],second["broker_id"])

    def test_trusted_provenance_requires_no_model_session_id(self):
        self.trusted.pop("local_caller",None);self.trusted["lease_until"]=time.time()+30;first=self.write_new()
        self.assertEqual(first["broker_context"]["provenance"],{"origin":"webagent","session_id":self.workspace})
        with self.assertRaises(ValueError):self.write_new("docs/spoof.md","automatic-spoof-write",session_id=str(uuid.uuid4()))

    def test_explicit_unknown_closed_and_mixed_aliases_do_not_create(self):
        count=len(self.broker.work)
        with self.assertRaisesRegex(ValueError,"Unknown Broker ID"):self.call("file_read",{"path":"README.md","broker_id":"unknown-broker"})
        self.assertEqual(len(self.broker.work),count)
        first=self.write_new();bid=first["broker_id"]
        item=next(item for item in self.broker.work.values()if item["task_id"]==bid);item["status"]="closed";self.broker._save_work()
        with self.assertRaisesRegex(ValueError,"closed"):self.call("file_read",{"path":"README.md","broker_id":bid})
        with self.assertRaisesRegex(ValueError,"not both"):self.call("file_read",{"path":"README.md","broker_id":bid,"task_id":bid})
        with self.assertRaisesRegex(ValueError,"never a release"):self.call("build_work",{"action":"finish","broker_id":"new","request_id":"automatic-bad-finish"})

    def test_existing_work_keeps_digest_and_ownership_gates(self):
        first=self.write_new();bid=first["broker_id"]
        args={"path":"docs/first.md","content":"changed\n","broker_id":bid,"request_id":"automatic-second-edit"}
        with self.assertRaisesRegex(ValueError,"require expected"):self.call("file_write",args)
        with self.assertRaisesRegex(ValueError,"stale task inputs"):self.call("file_write",{**args,"expected_inputs_sha256":"0"*64,"request_id":"automatic-stale-edit"})
        self.assertEqual(self.call("file_write",{**args,"request_id":"automatic-valid-edit","expected_inputs_sha256":first["workspace_inputs_sha256"]})["status"],"success")

    def test_new_write_receipt_replays_after_closure_without_reopening(self):
        args={"path":"docs/first.md","content":"first\n","broker_id":"new","request_id":"automatic-first-write"}
        first=self.call("file_write",args);item=next(i for i in self.broker.work.values()if i["task_id"]==first["broker_id"])
        item["status"]="closed";self.broker._save_work();count=len(self.broker.work)
        again=self.call("file_write",args)
        self.assertEqual(again["broker_id"],first["broker_id"]);self.assertEqual(again["file_request_sha256"],first["file_request_sha256"])
        self.assertEqual(item["status"],"closed");self.assertEqual(len(self.broker.work),count)

    def test_first_read_and_begin_create_without_extra_operation(self):
        read=self.call("file_list",{"path":"","broker_id":"new","request_id":"automatic-initial-read"})
        self.assertTrue(read["broker_id"].startswith("broker-"))
        begun=self.call("build_work",{"action":"begin","broker_id":"new","summary":"Separate outcome","request_id":"automatic-explicit-begin"})
        self.assertNotEqual(read["broker_id"],begun["broker_id"])
        self.assertEqual(self.call("build_work",{"action":"review"})["broker_id"],begun["broker_id"])

class FrontendAggregateBudgetTests(unittest.TestCase):
 def test_selected_serial_fixture_budget_is_bounded(self):
  self.assertEqual(fixtures.host.Runner.ui_test_timeout([]),300)
  self.assertEqual(fixtures.host.Runner.ui_test_timeout(["fixture"]*16),720)
  self.assertEqual(fixtures.host.Runner.ui_test_timeout(["fixture"]*256),900)

class PublicationWaitUnitTests(unittest.TestCase):
 def setUp(self):
  import threading
  self.saved=[];self.broker=types.SimpleNamespace(lock=threading.RLock(),deploy_lock=threading.RLock(),stopping=False,job_cancellations={},_batch_lease=lambda job:None,_save=lambda job:self.saved.append(json.loads(json.dumps(job))))
  self.pub=fixtures.host.ui_publication_module.UIPublication(self.broker,None,None,None,None,None);self.pub.wait_timeout=.2
  self.job={"job_id":str(uuid.uuid4()),"lease_until":time.time()+60,"preparation":{}}
 def occupy(self):
  import threading
  held=threading.Event();release=threading.Event()
  def owner():
   self.broker.deploy_lock.acquire();self.pub.holder={"job_id":"known-owner","since":time.time()};held.set();release.wait(3);self.pub.holder=None;self.broker.deploy_lock.release()
  worker=threading.Thread(target=owner);worker.start();self.assertTrue(held.wait(1));self.addCleanup(worker.join,3);self.addCleanup(release.set);return release
 def test_timeout_exposes_owner_and_subsequent_acquisition_succeeds(self):
  release=self.occupy()
  with self.assertRaises(fixtures.host.ui_publication_module.PublicationWaitTimeout):self.pub.acquire(self.job)
  receipt=self.job["publication_wait"];self.assertEqual(receipt["state"],"timed_out");self.assertEqual(receipt["owner"]["job_id"],"known-owner");self.assertGreater(receipt["elapsed_seconds"],.15)
  release.set();time.sleep(.02);self.pub.acquire(self.job);self.pub.release_locks(None)
 def test_cancel_waiting_and_release_owner_unchanged(self):
  import threading
  self.occupy();event=threading.Event();self.broker.job_cancellations[self.job["job_id"]]=event
  timer=threading.Timer(.03,event.set);timer.start()
  with self.assertRaisesRegex(PermissionError,"cancelled"):self.pub.acquire(self.job)
  timer.join();self.assertEqual(self.job["publication_wait"]["state"],"cancelled");self.assertEqual(self.pub.holder["job_id"],"known-owner")
 def test_wait_never_holds_authority_and_inverse_order_fails_fast(self):
  def probe():
   self.assertFalse(self.broker.lock._is_owned());return True
  self.pub.wait_for(self.job,"boot_effect_lease",probe)
  with self.broker.lock:
   with self.assertRaisesRegex(RuntimeError,"authority lock"):self.pub.acquire(self.job)
 def test_boot_cleanup_error_does_not_strand_publication(self):
  self.pub.acquire(self.job)
  class Broken:
   def __exit__(self,*args):raise RuntimeError("cleanup failed")
  with self.assertRaisesRegex(RuntimeError,"cleanup failed"):self.pub.release_locks(Broken())
  self.pub.acquire(self.job);self.pub.release_locks(None)
 def test_receipt_error_after_acquisition_does_not_leak(self):
  release=self.occupy();self.pub.wait_timeout=1
  def save(job):
   if job.get("publication_wait",{}).get("state")=="waiting":release.set()
   if job.get("publication_wait",{}).get("state")=="acquired":raise RuntimeError("receipt error")
  self.broker._save=save
  with self.assertRaisesRegex(RuntimeError,"receipt error"):self.pub.acquire(self.job)
  self.broker._save=lambda job:None;self.pub.acquire(self.job);self.pub.release_locks(None)
 def test_boot_file_wait_is_bounded_and_closes_its_descriptor(self):
  import fcntl,os
  from unittest.mock import Mock
  with tempfile.TemporaryDirectory()as folder:
   root=Path(folder);path=root/"boot-supervisor/effects.lock";path.parent.mkdir();path.touch();path.chmod(0o644)
   held=os.open(path,os.O_RDONLY);fcntl.flock(held,fcntl.LOCK_EX);self.broker.ui_publication=self.pub
   real_stat=os.fstat;real_close=os.close
   def root_owned(fd):
    v=real_stat(fd);return types.SimpleNamespace(st_mode=v.st_mode,st_uid=0,st_nlink=v.st_nlink)
   try:
    with patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"install_root":str(root/"installation")}),patch.object(fixtures.host.os,"fstat",side_effect=root_owned),patch.object(fixtures.host.os,"close",wraps=real_close)as closed:
     with self.assertRaises(fixtures.host.ui_publication_module.PublicationWaitTimeout):
      with fixtures.host.Broker.boot_effect_lease(self.broker,job=self.job):pass
     self.assertEqual(self.job["publication_wait"]["resource"],"boot_effect_lease");self.assertEqual(closed.call_count,1)
     fcntl.flock(held,fcntl.LOCK_UN)
     with fixtures.host.Broker.boot_effect_lease(self.broker,job=self.job):pass
   finally:os.close(held)
 def test_timeout_is_resumable_without_effects(self):
  self.occupy()
  self.pub._admit=lambda *args:self.pub.acquire(self.job)
  self.assertIsNone(self.pub.admit(self.job,b"source"));self.assertEqual(self.job["status"],"needs_decision");self.assertFalse(self.job.get("effects_started"))

class AppShellVerificationTests(unittest.TestCase):
 frozen=b'<html><head><title>WebAgent</title>\n<link rel="manifest" href="/m.json">\n</head><body></body></html>'
 block=(b'<meta name="description" content="WebAgent">\n<meta property="og:title" content="WebAgent">\n'
        b'<link rel="canonical" href="https://dev.webagent.live/app">\n<script type="application/ld+json">{"@type":"WebApplication"}</script>')
 def served(self,block=None):
  marker=self.frozen.index(b"</title>")+8
  return self.frozen[:marker]+b"\n"+(self.block if block is None else block)+self.frozen[marker:]
 def test_exact_shell_or_one_bounded_seo_block_after_title(self):
  match=fixtures.host.Runner.shell_matches
  self.assertTrue(match(self.frozen,self.frozen));self.assertTrue(match(self.frozen,self.served()))
 def test_anything_else_fails_like_a_byte_mismatch(self):
  match=fixtures.host.Runner.shell_matches;block=self.block
  for body in [self.served(block.replace(b"</script>",b"</script><script>alert(1)</script>")),
               self.served(block.replace(b'<script type="application/ld+json">',b"<script>")),
               self.served(block.replace(b'{"@type"',b'<img src=x onerror=1>{"@type"')),
               self.served(b'<meta name="description" content="x"><iframe src="/"></iframe><script type="application/ld+json">{}</script>'),
               self.served(b'<meta name="keywords" content="x"><script type="application/ld+json">{}</script>'),
               self.served(block+b"\n"+block),
               self.served(b'<meta name="description" content="'+b"x"*(16*1024)+b'"><script type="application/ld+json">{}</script>'),
               self.served()[:-4]+b"XXXX",
               self.frozen.replace(b"<head>",b"<head>"+self.block)]:
   self.assertFalse(match(self.frozen,body))
  self.assertFalse(match(b"<html>no title</html>",b"<html>no title</html>"+block))

class SameJobPhaseOverlapTests(unittest.TestCase):
 def test_phases_of_one_job_share_a_component_while_other_jobs_wait(self):
  from test_scheduler_resources import Fake,G
  with tempfile.TemporaryDirectory()as folder:
   runner=Fake();scheduler=fixtures.host.resource_module.Resources(folder,runner,{"mode":"enforce"},sampler=lambda:{"available":20*G,"total":24*G,"pressure":{}})
   runner.resource_progress=lambda job:None
   job={"job_id":"engine-job","lease_until":time.time()+60,"artifact_component":"engine"}
   scheduler.enter("suite",job,"test","engine:rust1.90:test","test",timeout=.2)
   scheduler.enter("linked",job,"compile_linked","engine:rust1.90:linked-release","engine",timeout=.2)
   try:
    other={"job_id":"other-job","lease_until":time.time()+60,"artifact_component":"engine"}
    with self.assertRaisesRegex(TimeoutError,"component_busy"):scheduler.enter("other",other,"compile_test","engine:rust1.90:compile_test","engine",timeout=.02)
   finally:scheduler.finish("suite",True);scheduler.finish("linked",True)
 def test_engine_component_tests_publish_through_the_host_lane(self):
  _,allowed=fixtures.host.Broker.host_source_scope(None)
  self.assertIn("v3/tests/test_engine_components.py",allowed)
  self.assertIn("v3/tests/test_build_lifecycle_entry.py",allowed)
 def test_receipt_save_retries_a_snapshot_changed_by_an_overlapping_phase(self):
  from unittest.mock import patch
  with tempfile.TemporaryDirectory()as folder:
   broker=types.SimpleNamespace(state=Path(folder));job={"job_id":"overlap"}
   real=fixtures.host.canonical
   with patch.object(fixtures.host,"canonical",side_effect=[RuntimeError("dictionary changed size during iteration"),real({"job_id":"overlap"})]):
    fixtures.host.Broker._save(broker,job)
   self.assertEqual(json.loads((Path(folder)/"overlap.json").read_text()),{"job_id":"overlap"})

class BrowserAdmissionReceiptTests(unittest.TestCase):
 def test_owner_timeout_and_cancelled_wait_receipts_are_retained(self):
  import threading
  from test_scheduler_resources import Fake,G
  with tempfile.TemporaryDirectory()as folder:
   runner=Fake();scheduler=fixtures.host.resource_module.Resources(folder,runner,{"mode":"enforce"},sampler=lambda:{"available":20*G,"total":24*G,"pressure":{}})
   runner.resource_progress=lambda job:None
   try:
    scheduler.enter("first",{"job_id":"first","lease_until":time.time()+60},"ui_test","ui-profile","ui_test",timeout=.2)
    job={"job_id":"second","lease_until":time.time()+60}
    with self.assertRaisesRegex(TimeoutError,"component_busy"):scheduler.enter("second",job,"ui_test","ui-profile","ui_test",timeout=.02)
    self.assertEqual(job["phase"],"waiting_frontend_admission");self.assertEqual(job["last_resource_wait"]["owners"][0]["job_id"],"first");self.assertEqual(job["last_resource_wait"]["state"],"timed_out")
    job={"job_id":"third","lease_until":time.time()+60};timer=threading.Timer(.02,lambda:job.update(revoked=True));timer.start()
    with self.assertRaises(PermissionError):scheduler.enter("third",job,"ui_test","ui-profile","ui_test",timeout=1)
    timer.join();self.assertEqual(job["last_resource_wait"]["state"],"cancelled")
    scheduler.finish("first",True);job={"job_id":"fourth","lease_until":time.time()+60};scheduler.enter("fourth",job,"ui_test","ui-profile","ui_test",timeout=.1);scheduler.finish("fourth",True)
   finally:scheduler.close()

class AdmissionCleanupTests(unittest.TestCase):
 def test_failed_receipt_does_not_keep_admission_or_disk_reservation(self):
  from test_scheduler_resources import Fake,G
  from unittest.mock import Mock
  with tempfile.TemporaryDirectory()as folder:
   runner=Fake();runner.resource_progress=Mock(side_effect=RuntimeError("receipt error"));runner.disk_finish=Mock()
   scheduler=fixtures.host.resource_module.Resources(folder,runner,{"mode":"enforce"},sampler=lambda:{"available":20*G,"total":24*G,"pressure":{}})
   try:
    with self.assertRaisesRegex(RuntimeError,"receipt error"):scheduler.enter("phase",{"job_id":"job"},"ui_test","ui-profile","ui_test",timeout=.1)
    self.assertEqual(scheduler.rows["phase"]["state"],"failed");runner.disk_finish.assert_called_once_with("phase")
    runner.resource_progress=Mock();scheduler.enter("next",{"job_id":"next"},"ui_test","ui-profile","ui_test",timeout=.1);scheduler.finish("next",True)
   finally:scheduler.close()
 def test_admission_timeout_pauses_but_execution_timeout_fails(self):
  broker=types.SimpleNamespace(lock=__import__("threading").RLock(),_save=lambda job:None)
  pub=fixtures.host.ui_publication_module.UIPublication(broker,None,None,None,None,None)
  def fail(*args):raise fixtures.host.resource_module.AdmissionTimeout("browser admission timed out")
  pub._admit=fail;job={"job_id":"test"};self.assertIsNone(pub.admit(job,b"source"));self.assertEqual(job["status"],"needs_decision")
  def execution(*args):raise TimeoutError("fixed build task timeout")
  pub._admit=execution
  with self.assertRaisesRegex(TimeoutError,"fixed build"):pub.admit({"job_id":"test"},b"source")


class ClearPendingRequestsTests(unittest.TestCase):
    """Withdraw a terminal proposal's queued intent without touching frozen source."""
    from test_scheduler_broker import SchedulerBrokerTests as _base
    setUp=_base.setUp
    tearDown= _base.tearDown
    call= _base.call
    begin= _base.begin
    action= _base.action
    def fixture(self):
        holder=self.begin("pending-holder");item=self.begin("pending-terminal")
        store=self.broker.scheduler.store;owner=self.broker.scheduler.owner(self.trusted,self.workspace)
        def acquire(i,r):
            v=store.get(i["key"])
            return store.acquire(i["key"],owner,v["revision"],v["epoch"],["module:engine"],r)
        acquire(holder,"holder-acquire");self.assertFalse(acquire(item,"terminal-wait")["acquired"])
        item.update(status="closed",job_id="cancelled-job")
        self.broker.jobs["cancelled-job"]={"job_id":"cancelled-job","ownership_id":item["key"],"status":"cancelled"}
        self.broker._save_work()
        return item,store.get(item["key"])
    def test_preserves_source_generation_evidence_state_and_retries(self):
        item,before=self.fixture();tree=Path(item["workspace_path"]).parent
        source=self.broker.scheduler_task_snapshot(tree);baseline=(tree/"baseline.tar").read_bytes()
        result,request=self.action(item["task_id"],"clear_pending_requests",reason="Import successor")
        self.assertEqual(result["requested"],[]);self.assertEqual(result["scopes"],[])
        for k in ("generation","evidence","state","heartbeat","owner"):
            self.assertEqual(result[k],before[k],k)
        self.assertEqual(result["epoch"],before["epoch"]+1)
        self.assertEqual(self.call("build_work",request),result)
        self.assertEqual(self.broker.scheduler_task_snapshot(tree),source)
        self.assertEqual((tree/"baseline.tar").read_bytes(),baseline)
        self.assertEqual(item["status"],"closed");self.assertEqual(item["job_id"],"cancelled-job")
        with self.assertRaisesRegex(ValueError,"reused"):
            self.call("build_work",{**request,"reason":"changed"})
    def test_rejects_open_active_uncertain_and_effectful_tasks(self):
        item,before=self.fixture();job=self.broker.jobs["cancelled-job"]
        item["status"]="open"
        with self.assertRaisesRegex(ValueError,"Closed"):self.action(item["task_id"],"clear_pending_requests",reason="withdraw")
        item["status"]="closed"
        for status in ("queued","running","interrupted","uncertain","needs_decision"):
            job["status"]=status
            with self.assertRaisesRegex(ValueError,"execution"):self.action(item["task_id"],"clear_pending_requests",reason="withdraw")
        job.update(status="failed",effects_started=True)
        with self.assertRaisesRegex(ValueError,"effects"):self.action(item["task_id"],"clear_pending_requests",reason="withdraw")
        self.assertEqual(self.broker.scheduler.store.get(item["key"]),before)
    def test_rejects_held_scopes_and_stale_or_foreign_owner(self):
        item,before=self.fixture()
        request={"task_id":item["task_id"],"action":"clear_pending_requests","request_id":"stale-clear","expected_revision":before["revision"]-1,"expected_epoch":before["epoch"],"reason":"withdraw"}
        with self.assertRaisesRegex(ValueError,"Stale"):self.call("build_work",request)
        with patch.dict(self.trusted,user_id="other-owner"):
            with self.assertRaises(ValueError):self.action(item["task_id"],"clear_pending_requests",reason="withdraw")
        # A separate closed fixture with held scopes must use normal release instead.
        holder=next(i for i in self.broker.work.values()if i["task_id"]=="pending-holder")
        holder.update(status="closed");self.broker.jobs["held-job"]={"ownership_id":holder["key"],"status":"cancelled"}
        with self.assertRaisesRegex(ValueError,"held claims"):self.action(holder["task_id"],"clear_pending_requests",reason="withdraw")
        self.assertEqual(self.broker.scheduler.store.get(item["key"]),before)
    def test_commit_then_journal_failure_replays_without_second_mutation(self):
        item,before=self.fixture();write=self.broker.scheduler.write_record
        def fail_done(path,record):
            if record.get("state")=="done":raise OSError("lost response")
            return write(path,record)
        request={"task_id":item["task_id"],"action":"clear_pending_requests","request_id":"lost-clear","expected_revision":before["revision"],"expected_epoch":before["epoch"],"reason":"withdraw"}
        with patch.object(self.broker.scheduler,"write_record",side_effect=fail_done):
            with self.assertRaises(OSError):self.call("build_work",request)
        after=self.broker.scheduler.store.get(item["key"])
        self.assertEqual(self.call("build_work",request),after)
        self.assertEqual(self.broker.scheduler.store.get(item["key"]),after)
    def test_restart_preserves_receipt_and_clear_does_not_resurrect_orphan(self):
        item,before=self.fixture();store=self.broker.scheduler.store
        with store.transaction()as db:store.update(db,store.get(item["key"],db),state="orphaned")
        result,request=self.action(item["task_id"],"clear_pending_requests",reason="withdraw")
        reopened=type(store)(store.path)
        self.assertEqual(reopened.get(item["key"]),result);self.assertEqual(result["state"],"orphaned")
        self.assertEqual(self.call("build_work",request),result)

class FailedReleaseLifecycleTests(unittest.TestCase):
    """A failed release keeps its claims for a bounded fix window; terminal tasks never block others."""
    from test_scheduler_broker import SchedulerBrokerTests as _base
    setUp=_base.setUp
    tearDown=_base.tearDown
    call=_base.call
    begin=_base.begin
    action=_base.action
    write=_base.write
    def failed(self,label="failed-release",status="failed",**job):
        item=self.begin(label);self.write(item,"README.md",label+"\n")
        job_id=str(uuid.uuid4());store=self.broker.scheduler.store
        self.broker.jobs[job_id]={"job_id":job_id,"ownership_id":item["key"],"task_id":item["task_id"],"status":status,
            "effects_started":False,"finished_at":store.clock(),**job}
        item.update(status="closed",job_id=job_id);self.broker._save_work()
        return item,store.get(item["key"])
    def test_reopen_keeps_tree_and_claims_and_replays(self):
        item,before=self.failed();tree=Path(item["workspace_path"]).parent
        source=self.broker.scheduler_task_snapshot(tree);failed=item["job_id"]
        result,request=self.action(item["task_id"],"reopen")
        self.assertEqual((result["status"],result["failed_job_id"],result["scopes"]),("open",failed,before["scopes"]))
        self.assertEqual((item["status"],item["failed_jobs"]),("open",[failed]));self.assertNotIn("job_id",item)
        self.assertEqual(self.broker.scheduler_task_snapshot(tree),source)
        self.assertEqual(self.call("build_work",request),result)
        with self.assertRaisesRegex(ValueError,"reopen"):self.action(item["task_id"],"reopen")
        self.write(item,"README.md","fixed\n")
        self.assertEqual(self.broker.scheduler.store.get(item["key"])["scopes"],before["scopes"])
    def test_reopen_refuses_effects_live_and_completed_jobs(self):
        item,before=self.failed();job=self.broker.jobs[item["job_id"]]
        for change in ({"effects_started":True},{"status":"running"},{"status":"uncertain"},{"status":"completed"}):
            job.update({"status":"failed","effects_started":False,**change})
            with self.assertRaisesRegex(ValueError,"reopen"):self.action(item["task_id"],"reopen")
        self.assertEqual(item["status"],"closed")
    def test_grace_window_then_heartbeat_loss_release_every_claim(self):
        item,before=self.failed();scheduler=self.broker.scheduler;store=scheduler.store
        scheduler.settle();self.assertEqual(store.get(item["key"])["scopes"],before["scopes"])
        with patch.object(store,"clock",return_value=store.clock()+scheduler.failure_grace+1):scheduler.settle()
        after=store.get(item["key"])
        self.assertEqual((after["state"],after["scopes"],after["requested"],after["epoch"]),("waiting",[],[],before["epoch"]+1))
        other,held=self.failed("orphaned-release")
        with store.transaction()as db:store.update(db,store.get(other["key"],db),state="orphaned")
        scheduler.settle();self.assertEqual(store.get(other["key"])["scopes"],[])
        # A reopened task reacquires its claims on the next apply.
        self.action(item["task_id"],"reopen");self.write(item,"README.md","fixed\n")
        self.assertEqual(store.get(item["key"])["state"],"owned")
    def test_completed_release_keeps_claims_until_commit_release(self):
        item,before=self.failed(status="completed")
        with patch.object(self.broker.scheduler.store,"clock",return_value=time.time()+86400):self.broker.scheduler.settle()
        self.assertEqual(self.broker.scheduler.store.get(item["key"])["scopes"],before["scopes"])
    def test_closed_and_abandoned_tasks_drop_queued_requests_at_once(self):
        holder=self.begin("queue-holder");self.write(holder,"README.md","holder\n")
        item=self.begin("queued-closed");store=self.broker.scheduler.store
        v=store.get(item["key"]);self.assertFalse(store.acquire(item["key"],v["owner"],v["revision"],v["epoch"],["path:README.md"],"queue-wait")["acquired"])
        item.update(status="closed",job_id="cancelled-queue-job")
        self.broker.jobs["cancelled-queue-job"]={"job_id":"cancelled-queue-job","ownership_id":item["key"],"status":"cancelled"}
        self.broker.scheduler.settle();self.assertEqual(store.get(item["key"])["requested"],[])
        abandoned=self.begin("queued-abandoned");v=store.get(abandoned["key"])
        store.acquire(abandoned["key"],v["owner"],v["revision"],v["epoch"],["path:README.md"],"queue-wait-2")
        abandoned["status"]="abandoned";self.broker.scheduler.settle()
        self.assertEqual((store.get(abandoned["key"])["state"],store.get(abandoned["key"])["requested"]),("abandoned",[]))
        self.assertTrue(store.get(holder["key"])["scopes"])
    def test_abandon_releases_claims_and_requests_in_one_call(self):
        item,before=self.failed()
        result=self.call("build_work",{"action":"abandon","task_id":item["task_id"]})
        self.assertEqual((result["status"],result["released_scopes"],result["cleared_requests"]),("abandoned",len(before["scopes"]),0))
        after=self.broker.scheduler.store.get(item["key"])
        self.assertEqual((after["state"],after["scopes"]),("abandoned",[]))
        opened=self.begin("abandon-open");self.write(opened,"docs/open.md","x\n")
        self.assertEqual(self.call("build_work",{"action":"abandon","task_id":"abandon-open"})["cleared_requests"],0)
        self.assertEqual(self.broker.scheduler.store.get(opened["key"])["scopes"],[])
        live,_=self.failed("abandon-live",status="running")
        with self.assertRaisesRegex(ValueError,"abandoned"):self.call("build_work",{"action":"abandon","task_id":"abandon-live"})
    def test_blockers_and_reservations_name_the_owning_session(self):
        self.call("build_work",{"action":"begin","task_id":"named-holder","summary":"Fix [origin=claude; session=Landing page and SEO]"})
        holder=next(i for i in self.broker.work.values()if i["task_id"]=="named-holder");self.write(holder,"README.md","held\n")
        other=self.begin("blocked-writer")
        with self.assertRaises(ValueError)as caught:self.write(other,"README.md","blocked\n")
        blockers=json.loads(str(caught.exception))["blockers"]
        self.assertEqual([(b["label"],b["session"])for b in blockers],[("named-holder","Landing page and SEO")])
        rows=self.call("build_work",{"action":"reservations"})["tasks"]
        self.assertEqual({r["label"]:r["session"]for r in rows}["named-holder"],"Landing page and SEO")
        self.assertIsNone({r["label"]:r["session"]for r in rows}["blocked-writer"])

class ContinuationReconciliationTests(unittest.TestCase):
    """Legacy switched orphan continuations settle without release effects."""
    from test_scheduler_broker import SchedulerBrokerTests as _base
    setUp=_base.setUp
    tearDown=_base.tearDown
    call=_base.call
    begin=_base.begin
    write=_base.write
    publish=_base.publish
    action=_base.action
    def fixture(self,switched=True):
        import hashlib
        item=self.begin("orphan-continue-task");self.write(item,"README.md","published\n");job,commit=self.publish(item)
        scheduler=self.broker.scheduler;store=scheduler.store
        with store.transaction()as db:store.update(db,store.get(item["key"],db),state="orphaned")
        value=store.get(item["key"]);owner=scheduler.owner(self.trusted,self.workspace)
        job["source_generation"]=value["generation"]
        item["base_inputs_sha256"]=self.broker.scheduler_digest((Path(item["workspace_path"]).parent/"baseline.tar").read_bytes())
        original={"action":"continue","task_id":item["task_id"],"request_id":"legacy-orphan-continue","expected_revision":value["revision"],"expected_epoch":value["epoch"]}
        path=scheduler.journal_path(owner,original["request_id"],item["key"])
        digest=hashlib.sha256(fixtures.host.scheduler_module.ownership.canonical([owner,original]).encode()).hexdigest()
        record={"digest":digest,"state":"pending","request_id":original["request_id"],"task_key":item["key"],"value":value,"previous_job":job["job_id"]}
        if switched:
            scheduler.write_record(path,record)
            with self.assertRaisesRegex(ValueError,"Editing ownership"):
                scheduler.continuation(item,original,value,record,path,owner,value["revision"],value["epoch"],"legacy-transition-token")
        request={"action":"reconcile_continuation","task_id":item["task_id"],"original_request_id":original["request_id"],"expected_transition_sha256":digest,"expected_revision":value["revision"],"expected_epoch":value["epoch"],"request_id":"recover-legacy-continuation"}
        return item,job,value,path,original,request
    def test_new_orphan_request_fails_before_journal_or_filesystem_changes(self):
        item,job,value,path,original,request=self.fixture(False)
        tree=Path(item["workspace_path"]).parent;before=self.broker.scheduler_task_snapshot(tree)
        with self.assertRaisesRegex(ValueError,"Recover editing ownership"):self.call("build_work",original)
        self.assertFalse(path.exists());self.assertFalse((self.broker.work_dir/"generations"/item["key"]).exists())
        self.assertEqual(self.broker.scheduler_task_snapshot(tree),before)
    def test_reconcile_preserves_both_generations_and_original_release(self):
        item,job,value,path,original,request=self.fixture();tree=Path(item["workspace_path"]).parent
        before=self.broker.scheduler_task_snapshot(tree);archive=self.broker.work_dir/"generations"/item["key"]/job["job_id"]
        frozen=self.broker.scheduler_task_snapshot(archive);job_before=json.loads(json.dumps(job))
        result=self.call("build_work",request);v=result["ownership"]
        self.assertEqual((v["revision"],v["epoch"],v["generation"]),(value["revision"]+1,value["epoch"]+1,value["generation"]+1))
        self.assertEqual(v["scopes"],value["scopes"]);self.assertFalse(result["deployment_replayed"])
        self.assertEqual(self.broker.scheduler_task_snapshot(tree),before);self.assertEqual(self.broker.scheduler_task_snapshot(archive),frozen)
        self.assertEqual(job,job_before);self.assertEqual(item["status"],"open")
        self.assertEqual(self.call("build_work",request),result)
        self.assertEqual(self.call("build_work",original)["status"],"open")
        self.write(item,"README.md","authorized correction\n")
    def test_recovery_rejects_stale_binding_and_foreign_caller(self):
        item,job,value,path,original,request=self.fixture()
        with self.assertRaisesRegex(ValueError,"Stale"):self.call("build_work",{**request,"expected_revision":value["revision"]-1})
        with self.assertRaisesRegex(ValueError,"identity"):self.call("build_work",{**request,"expected_transition_sha256":"0"*64})
        with patch.dict(self.trusted,local_caller=False):
            with self.assertRaises((PermissionError,ValueError)):self.call("build_work",request)
        self.assertEqual(self.broker.scheduler.store.get(item["key"]),value)
    def test_recovery_rejects_modified_correction_or_archive(self):
        item,job,value,path,original,request=self.fixture();tree=Path(item["workspace_path"])
        file=tree/"README.md";before=file.read_bytes();file.write_text("unreviewed correction\n")
        with self.assertRaisesRegex(ValueError,"source or archive"):self.call("build_work",request)
        file.write_bytes(before)
        archive=self.broker.work_dir/"generations"/item["key"]/job["job_id"]/"v3/README.md";archive.write_text("changed archive\n")
        with self.assertRaisesRegex(ValueError,"source or archive"):self.call("build_work",request)
        self.assertEqual(self.broker.scheduler.store.get(item["key"]),value)
    def test_recovery_refuses_unresolved_execution(self):
        item,job,value,path,original,request=self.fixture()
        self.broker.jobs["uncertain-correction"]={"ownership_id":item["key"],"status":"uncertain"}
        with self.assertRaisesRegex(ValueError,"active execution"):self.call("build_work",request)
        self.assertEqual(self.broker.scheduler.store.get(item["key"]),value)
    def test_committed_mutation_with_failed_save_reconciles_once(self):
        item,job,value,path,original,request=self.fixture()
        with patch.object(self.broker,"_save_work",side_effect=OSError("lost save")):
            with self.assertRaises(OSError):self.call("build_work",request)
        committed=self.broker.scheduler.store.get(item["key"])
        result=self.call("build_work",request)
        self.assertEqual(result["ownership"],committed);self.assertEqual(self.broker.scheduler.store.get(item["key"]),committed)
        self.assertEqual(json.loads(path.read_text())["state"],"done")

class CompilerManifestRecoveryTests(unittest.TestCase):
    """Unhealthy API recovery reads immutable compiler evidence without mounting state."""
    def fixture(self,manifest="/build/src/main.rs\n/build/src/context/tool_output.rs\n"):
        self.calls=[];self.inspect_count=0
        self.identity={"Id":"a"*64,"Image":"sha256:"+"b"*64}
        def command(argv,timeout,private_raw):
            self.calls.append(argv)
            if argv[1]=="exec":raise RuntimeError("container restarting")
            return manifest
        self.runner=types.SimpleNamespace(command=command,inspect=lambda name:dict(self.identity))
    def test_fallback_preserves_exact_image_and_required_manifest(self):
        self.fixture()
        with patch.object(fixtures.host.subprocess,"run")as cleanup:
            result=fixtures.host.Runner.core_compile_inputs(self.runner)
        self.assertEqual(result,{"v3/src/main.rs","v3/src/context/tool_output.rs"})
        argv=self.calls[1];self.assertIn("--read-only",argv);self.assertNotIn("--mount",argv);self.assertNotIn("-v",argv)
        self.assertEqual(argv[argv.index("--network")+1],"none")
        self.assertEqual(argv[-2],self.identity["Image"]);self.assertEqual(argv[-1],"/usr/local/lib/webagent/core-inputs.txt")
        self.assertEqual(argv[argv.index("--entrypoint")+1],"/bin/cat");cleanup.assert_called_once()
    def test_changed_identity_or_missing_main_manifest_fails_closed(self):
        self.fixture("/build/config/example.json\n")
        with patch.object(fixtures.host.subprocess,"run"):
            with self.assertRaisesRegex(ValueError,"manifest unavailable"):fixtures.host.Runner.core_compile_inputs(self.runner)
        self.fixture()
        def inspect(name):
            self.inspect_count+=1
            return {**self.identity,"Id":("c"*64 if self.inspect_count>1 else self.identity["Id"])}
        self.runner.inspect=inspect
        with patch.object(fixtures.host.subprocess,"run"):
            with self.assertRaisesRegex(ValueError,"runtime changed"):fixtures.host.Runner.core_compile_inputs(self.runner)
    def test_available_live_manifest_uses_no_fallback(self):
        self.fixture();self.runner.command=lambda *a,**kw:"/build/src/main.rs\n"
        with patch.object(fixtures.host.subprocess,"run")as cleanup:
            self.assertEqual(fixtures.host.Runner.core_compile_inputs(self.runner),{"v3/src/main.rs"})
        cleanup.assert_not_called()

class UIFixtureEvidenceReuseTests(unittest.TestCase):
    def setUp(self):
        import types
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
        self.files={"v3/ui/a.js":(420,b"export const a=1;"),"v3/ui/a.test.mjs":(420,b"import './a.js';"),
            "v3/ui/b.test.mjs":(420,b"import fs from 'node:fs'; fs.readFileSync(variable);"),"v3/ui/unrelated.js":(420,b"const other=1;")}
        for path,(_,body)in self.files.items():
            target=self.root/path;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(body)
        self.calls=[];self.job={"job_id":"first"}
        self.runner=types.SimpleNamespace(job_context=types.SimpleNamespace(job=self.job),
            index_command=lambda argv,**kwargs:self.calls.append(argv),ui_test_timeout=lambda tests:120)
        self.config={**fixtures.host.CONFIG,"scheduler":{**fixtures.host.CONFIG.get("scheduler",{}),"ui_test_image":"sha256:"+"1"*64}}
        state=patch.object(fixtures.host,"STATE",self.root/"state");state.start();self.addCleanup(state.stop)
    def run_tests(self,images=None):
        evidence=fixtures.host.ui_policy_module.dependencies(self.files,["v3/ui/a.js"])
        evidence["runtime_images"]=images or {"api":"sha256:"+"2"*64,"loop":"sha256:"+"3"*64}
        with patch.object(fixtures.host,"CONFIG",self.config):
            return fixtures.host.Runner.validate_selected_tests(self.runner,"fixture-job",self.root,evidence)
    def test_unrelated_rebase_reuses_resolved_fixture_and_executes_unknown(self):
        first=self.run_tests();self.job["ui_test_evidence"]=first
        self.files["v3/ui/unrelated.js"]=(420,b"const other=2;")
        second=self.run_tests()
        self.assertEqual(second["reused_tests"],["v3/ui/a.test.mjs"])
        self.assertEqual(second["executed_tests"],["v3/ui/b.test.mjs"])
        self.assertNotIn("/candidate/v3/ui/a.test.mjs",self.calls[-1])
        self.assertFalse(first["fixtures"]["v3/ui/a.test.mjs"]["reused"])
        self.assertEqual(second["fixtures"]["v3/ui/a.test.mjs"]["verified_at"],first["fixtures"]["v3/ui/a.test.mjs"]["verified_at"])
    def test_relevant_change_and_test_runtime_change_require_execution(self):
        first=self.run_tests();self.job["ui_test_evidence"]=first
        self.files["v3/ui/a.js"]=(420,b"export const a=2;")
        second=self.run_tests();self.assertIn("v3/ui/a.test.mjs",second["executed_tests"])
        self.files["v3/ui/a.js"]=(420,b"export const a=1;")
        self.config["scheduler"]["ui_test_image"]="sha256:"+"4"*64
        self.assertEqual(set(self.run_tests()["executed_tests"]),set(first["tests"]))
    def test_exact_bindings_reuse_without_frontend_admission(self):
        first=self.run_tests();self.job["ui_test_evidence"]=first;before=len(self.calls)
        second=self.run_tests();self.assertTrue(second["reused"]);self.assertEqual(len(self.calls),before)
    def test_failed_or_legacy_evidence_never_reuses(self):
        first=self.run_tests();self.job["ui_test_evidence"]={**first,"status":"failed"}
        import shutil;shutil.rmtree(self.root/"state/ui-test-evidence")  # judge the job's own evidence only
        self.assertFalse(self.run_tests()["reused"])
        self.job["ui_test_evidence"]={"status":"passed","binding":first["binding"]}
        shutil.rmtree(self.root/"state/ui-test-evidence")
        self.assertFalse(self.run_tests()["reused"])
    def test_execution_exception_records_no_new_success(self):
        first=self.run_tests();self.job["ui_test_evidence"]=first
        self.files["v3/ui/a.js"]=(420,b"changed")
        self.runner.index_command=lambda *a,**kw:(_ for _ in ()).throw(PermissionError("cancelled"))
        with self.assertRaises(PermissionError):self.run_tests()
        self.assertIs(self.job["ui_test_evidence"],first)

    def test_runtime_image_policy_and_fixture_changes_invalidate(self):
        first=self.run_tests();self.job["ui_test_evidence"]=first
        # Fixtures never mount or reach runtime images: a resolved fixture reuses its
        # cross-release record; the unresolved fixture still runs.
        changed=self.run_tests({"api":"sha256:"+"5"*64,"loop":"sha256:"+"3"*64})
        self.assertEqual((sorted(changed["reused_tests"]),changed["executed_tests"]),(["v3/ui/a.test.mjs","v3/ui/b.test.mjs"],[]))
        self.files["v3/ui/a.test.mjs"]=(420,b"import './a.js'; // new assertions")
        self.assertIn("v3/ui/a.test.mjs",self.run_tests()["executed_tests"])
        self.files["v3/ui/a.test.mjs"]=(420,b"import './a.js';")
        self.job["ui_test_evidence"]={**first,"fixtures":{p:{**row,"binding":"old recipe"}for p,row in first["fixtures"].items()}}
        # A real recipe change also changes every cross-release key; model it by dropping the old records.
        import shutil;shutil.rmtree(self.root/"state/ui-test-evidence")
        self.assertEqual(set(self.run_tests()["executed_tests"]),set(first["tests"]))

class InstalledUIPolicyPublicationTests(unittest.TestCase):
 def test_ui_policy_requires_exact_installed_bytes_and_operator(self):
  import types
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary)
   for name,body in [("host.py",b"installed host"),("ui_policy.py",b"installed policy")]:
    (root/name).write_bytes(body)
   broker=fixtures.host.Broker(root/"jobs",fixtures.FakeRunner())
   blob=fixtures.context(extra={"v3/build-service/host.py":"installed host","v3/build-service/ui_policy.py":"installed policy"})
   changed=["v3/build-service/ui_policy.py","v3/tests/test_ui_policy.py"]
   protected=types.SimpleNamespace(st_mode=0o100644,st_uid=0,st_nlink=1)
   with patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"service_dir":str(root)}),patch.object(fixtures.host.os,"fstat",return_value=protected):
    self.assertTrue(broker.host_source_only(changed,blob,{"local_caller":True}))
    self.assertFalse(broker.host_source_only(changed,blob,{}))
    (root/"ui_policy.py").write_bytes(b"different policy")
    self.assertFalse(broker.host_source_only(changed,blob,{"local_caller":True}))
   broker.shutdown()


class RuntimeDocumentationRebaseTests(unittest.TestCase):
 def attempt(self,live_extra,installed=None,built=None):
  h=fixtures.host
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary);runner=fixtures.FakeRunner();broker=h.Broker(root/"jobs",runner)
   base=fixtures.context(extra={"v3/src/main.rs":"fn main(){}","v3/docs/note.md":"old note","v3/ui/panel.js":"old ui"})
   proposal=fixtures.context(extra={"v3/src/main.rs":"fn main(){}","v3/docs/note.md":"old note","v3/ui/panel.js":"old ui","v3/src/change.rs":"task change"})
   live_files={"v3/src/main.rs":"fn main(){}","v3/docs/note.md":"old note","v3/ui/panel.js":"old ui",**live_extra}
   live=fixtures.context(extra={n:v for n,v in live_files.items()if v is not None})
   job={"job_id":str(uuid.uuid4()),"base_inputs_sha256":h.context_digest(base)[0],"inputs_sha256":h.context_digest(proposal)[0],"linked_stable":{"compiled_inputs":built if built is not None else {"v3/src/main.rs":"hash"}}}
   folder=broker.state/job["job_id"];folder.mkdir();(folder/"base.tar").write_bytes(base);(folder/"task.tar").write_bytes(proposal)
   runner.core_compile_inputs=lambda: installed if installed is not None else {"v3/src/main.rs"}
   try:
    result=broker._rebase_after_ui(job,proposal,tested_runtime=True,live_blob=live)
    return h.release.entries(result,h.context_digest),job
   finally:broker.shutdown()
 def test_notes_and_ui_survive_rebase_with_both_compiler_attestations(self):
  entries,job=self.attempt({"v3/docs/note.md":"new note","v3/ui/panel.js":"new ui"})
  self.assertEqual(entries["v3/docs/note.md"][1],b"new note");self.assertEqual(entries["v3/ui/panel.js"][1],b"new ui");self.assertIn("v3/src/change.rs",entries);self.assertIn("built_inputs_sha256",job)
 def test_embedded_note_or_missing_attestation_is_rejected(self):
  for installed,built in [({"v3/src/main.rs","v3/docs/note.md"},{"v3/src/main.rs":"hash"}),({"v3/src/main.rs"},{"v3/src/main.rs":"hash","v3/docs/note.md":"hash"}),(set(),{"v3/src/main.rs":"hash"}),({"v3/src/main.rs"},{})]:
   with self.subTest(installed=installed,built=built),self.assertRaisesRegex(ValueError,"runtime build inputs"):
    self.attempt({"v3/docs/note.md":"new note"},installed,built)
 def test_runtime_change_is_rejected_even_with_notes(self):
  with self.assertRaisesRegex(ValueError,"runtime build inputs|Compiler-attested"):
   self.attempt({"v3/docs/note.md":"new note","v3/src/main.rs":"fn main(){panic!()}"} )
 def test_rename_and_deletion_of_unembedded_notes_are_safe(self):
  entries,_=self.attempt({"v3/docs/note.md":None,"v3/docs/new.md":"renamed note"})
  self.assertIn("v3/docs/new.md",entries);self.assertNotIn("v3/docs/note.md",entries)


class InputDriftFailFastTests(unittest.TestCase):
 """Prove the deferred rebase is re-attested before compilation instead of after the suite."""
 def attest(self,live_extra,job_extra=None):
  h=fixtures.host
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary);runner=fixtures.FakeRunner();broker=h.Broker(root/"jobs",runner)
   base=fixtures.context(extra={"v3/src/main.rs":"fn main(){}","v3/ui/panel.js":"old ui"})
   proposal=fixtures.context(extra={"v3/src/main.rs":"fn main(){}","v3/ui/panel.js":"old ui","v3/src/change.rs":"task change"})
   live_files={"v3/src/main.rs":"fn main(){}","v3/ui/panel.js":"old ui",**live_extra}
   live=fixtures.context(extra={n:v for n,v in live_files.items()if v is not None})
   job={"job_id":str(uuid.uuid4()),"operation":"build_deploy","isolated":True,"base_inputs_sha256":h.context_digest(base)[0],"inputs_sha256":h.context_digest(proposal)[0]}
   job.update(job_extra or {})
   folder=broker.state/job["job_id"];folder.mkdir();(folder/"base.tar").write_bytes(base);(folder/"task.tar").write_bytes(proposal)
   runner.core_compile_inputs=lambda:{"v3/src/main.rs"}
   try:
    with patch.object(broker,"_current_release_blob",return_value=live):return broker.attest_release_base(job,proposal)
   finally:broker.shutdown()
 def test_concurrent_runtime_change_fails_with_the_publication_class(self):
  with self.assertRaisesRegex(ValueError,"runtime build inputs changed during UI release"):
   self.attest({"v3/src/main.rs":"fn main(){panic!()}"})
 def test_concurrent_hot_and_note_changes_stay_rebasable(self):
  self.attest({"v3/ui/panel.js":"new ui","v3/docs/note.md":"new note"})
 def test_unchanged_live_base_and_non_publication_jobs_are_skipped(self):
  self.attest({})
  self.attest({"v3/src/main.rs":"fn main(){panic!()}"},{"operation":"build"})
  self.attest({"v3/src/main.rs":"fn main(){panic!()}"},{"prepared_publication":True})
 def test_attestation_leaves_the_job_and_its_state_folder_untouched(self):
  h=fixtures.host
  with tempfile.TemporaryDirectory()as temporary:
   root=Path(temporary);runner=fixtures.FakeRunner();broker=h.Broker(root/"jobs",runner)
   base=fixtures.context(extra={"v3/src/main.rs":"fn main(){}"})
   proposal=fixtures.context(extra={"v3/src/main.rs":"fn main(){}","v3/src/change.rs":"task change"})
   live=fixtures.context(extra={"v3/src/main.rs":"fn main(){}","v3/ui/panel.js":"new ui"})
   job={"job_id":str(uuid.uuid4()),"operation":"build_deploy","isolated":True,"base_inputs_sha256":h.context_digest(base)[0],"inputs_sha256":h.context_digest(proposal)[0]}
   folder=broker.state/job["job_id"];folder.mkdir();(folder/"base.tar").write_bytes(base);(folder/"task.tar").write_bytes(proposal)
   runner.core_compile_inputs=lambda:{"v3/src/main.rs"}
   snapshot=dict(job)
   try:
    with patch.object(broker,"_current_release_blob",return_value=live):broker.attest_release_base(job,proposal)
   finally:broker.shutdown()
   self.assertEqual(job,snapshot);self.assertEqual(sorted(p.name for p in folder.iterdir()),["base.tar","task.tar"])


class FailureFieldTests(unittest.TestCase):
 """P2: a receipt names its kind, failing tests and the real cause line."""
 def fields(self,text,error=None):
  h=fixtures.host
  return h.failure_fields(error if error is not None else ValueError(text))
 def test_stack_overflow_is_a_test_failure_with_the_cause_line(self):
  fields=self.fields("fixed command failed: \nrunning 853 tests\ntest actions::x::y ... ok\nthread 'actions::x::z' has overflowed its stack\nfatal runtime error: stack overflow, aborting\n")
  self.assertEqual(fields["failure_kind"],"test")
  self.assertIn("overflowed its stack",fields["failure_line"])
 def test_failing_tests_are_named_from_libtest_output(self):
  fields=self.fields("fixed command failed: \ntest alpha ... FAILED\ntest beta ... ok\nfailures:\n    gamma::delta\n    epsilon\n\n")
  self.assertEqual(fields["failure_kind"],"test")
  self.assertEqual(fields["failing_tests"],["gamma::delta","epsilon"])
 def test_compile_drift_and_authority_kinds(self):
  self.assertEqual(self.fields("error[E0432]: unresolved import `x`")["failure_kind"],"compile")
  self.assertEqual(self.fields("error: could not compile `webagent-v3` due to previous error")["failure_kind"],"compile")
  self.assertEqual(self.fields("runtime build inputs changed during UI release")["failure_kind"],"input-drift")
  self.assertEqual(self.fields("Compilation changed application inputs")["failure_kind"],"input-drift")
  self.assertEqual(fixtures.host.failure_fields(PermissionError("Installed host source changed after admission"))["failure_kind"],"authority")
  self.assertEqual(self.fields("Sealed engine descriptor changed")["failure_kind"],"contract")
 def test_fields_stay_bounded(self):
  self.assertLessEqual(len(self.fields("panicked at 'x'\n"+"A"*600)["failure_line"]),500)
  fields=self.fields("failures:\n"+"".join("    name-%d\n"%i for i in range(7)))
  self.assertLessEqual(len(fields["failing_tests"]),5)


class HostPhaseTimingTests(unittest.TestCase):
 """P3: host-side publication and packaging work appears in phase_timings."""
 def test_completed_and_failed_outcomes_are_recorded(self):
  h=fixtures.host;job={}
  with h.host_phase(job,"rollback_export"):pass
  with self.assertRaises(ValueError):
   with h.host_phase(job,"recreate_api"):raise ValueError("stop")
  self.assertEqual([t["phase"] for t in job["phase_timings"]],["rollback_export","recreate_api"])
  self.assertEqual([t["outcome"] for t in job["phase_timings"]],["completed","failed"])
  self.assertTrue(all(t["recipe"].startswith("host:v1:") and t["seconds"]>=0 for t in job["phase_timings"]))
 def test_phase_timings_stay_bounded(self):
  h=fixtures.host;job={"phase_timings":[{"phase":"x"} for _ in range(200)]}
  with h.host_phase(job,"checking_ui"):pass
  self.assertLessEqual(len(job["phase_timings"]),128)
  self.assertEqual(job["phase_timings"][-1]["phase"],"checking_ui")


class InstalledInventoryEnginePublicationTests(unittest.TestCase):
 def test_maintenance_requires_exact_installed_engine_and_operator(self):
  with tempfile.TemporaryDirectory()as tmp:
   root=Path(tmp);(root/"host.py").write_bytes(b"host");(root/"index-maintenance.py").write_bytes(b"engine")
   broker=fixtures.host.Broker(root/"jobs",fixtures.FakeRunner())
   path="v3/plugins/abilities/Administrator/code_index/maintenance.py"
   blob=fixtures.context(extra={"v3/build-service/host.py":"host",path:"engine"})
   protected=types.SimpleNamespace(st_mode=0o100644,st_uid=0,st_nlink=1)
   try:
    with patch.object(fixtures.host,"CONFIG",{**fixtures.host.CONFIG,"service_dir":str(root)}),patch.object(fixtures.host.os,"fstat",return_value=protected):
     self.assertTrue(broker.host_source_only([path],blob,{"local_caller":True}))
     self.assertFalse(broker.host_source_only([path],blob,{}))
     (root/"index-maintenance.py").write_bytes(b"other")
     self.assertFalse(broker.host_source_only([path],blob,{"local_caller":True}))
     self.assertFalse(broker.host_source_only([path,"v3/worker.py"],blob,{"local_caller":True}))
   finally:broker.shutdown()

class EngineModulePathTests(unittest.TestCase):
 def test_native_schema_keeps_asset_publication_after_move(self):
  self.assertTrue(fixtures.host.hot_source_path("v3/src/engine/tools/administrator-schemas.json"))
  self.assertTrue(fixtures.host.hot_source_path("v3/config/administration/native-tools.json"))
  self.assertFalse(fixtures.host.hot_source_path("v3/src/engine/loop/run.rs"))
  self.assertFalse(fixtures.host.hot_source_path("v3/src/engine/tools/dispatch.rs"))

class ExternalDefaultRoutingTests(unittest.TestCase):
 """Real isolated finish classification requires installed compiler evidence for external defaults."""
 setUp=fixtures.BuildTests.setUp
 tearDown=fixtures.BuildTests.tearDown
 call=fixtures.BuildTests.call
 wait=fixtures.BuildTests.wait
 def route_default(self,name,evidence):
  if getattr(self,"_route_started",False):self.tearDown();self.setUp()
  self._route_started=True
  if evidence is False:self.runner.core_compile_inputs=lambda:(_ for _ in()).throw(ValueError("manifest unavailable"))
  else:self.runner.core_compile_inputs=lambda:evidence
  task=self.call("v3_build_work",{"action":"begin","task_id":"external-default-"+uuid.uuid4().hex,"summary":"External default fixture","mode":"isolated"})
  path=Path(task["workspace_path"])/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text("{}")
  digest=self.call("v3_build_work",{"action":"status","task_id":task["task_id"]})["own"][0]["workspace_inputs_sha256"]
  result=self.call("v3_build_work",{"action":"finish","task_id":task["task_id"],"expected_inputs_sha256":digest})
  return self.wait(result)["operation"]
 def test_defaults_publish_only_when_current_compiler_excludes_them(self):
  for name in ("config/prompts.json","config/tool-output-retention.json","config/administration/native-tools.json"):
   with self.subTest(name=name):
    self.assertEqual(self.route_default(name,{"v3/src/main.rs"}),"plugin_deploy")
    self.assertEqual(self.route_default(name,{"v3/src/main.rs","v3/"+name}),"build_deploy")
 def test_unavailable_or_unrecognized_evidence_never_grants_default_publication(self):
  for name in ("config/prompts.json","config/administration/native-tools.json"):
   for evidence in (False,set(),{"v3/docs/note.md"}):
    with self.subTest(name=name,evidence=evidence):
     self.assertEqual(self.route_default(name,evidence),"build_deploy")

class CommandObservationTests(unittest.TestCase):
 def test_stream_redaction_all_byte_boundaries(self):
  host=fixtures.host
  value='synthetic-'+'credential-value'
  text=('hello π🙂\n'+'author'+'ization: Bear'+'er '+value+'\n'+'pass'+'word='+value+'\n'+'ending '+value).encode()
  for split in range(len(text)+1):
   out=[];parser=host.CommandOutput([value],out.append)
   parser.feed(text[:split]);parser.feed(text[split:]);parser.feed(final=True)
   result=''.join(out)
   self.assertNotIn(value,result);self.assertIn('π🙂',result)
  out=[];parser=host.CommandOutput([value],out.append)
  for byte in text:parser.feed(bytes([byte]))
  parser.feed(final=True);self.assertNotIn(value,''.join(out))

 def test_multiline_and_overlong_records_never_publish_partial_values(self):
  host=fixtures.host;out=[];value='first-fragment\n'+'second-fragment'
  parser=host.CommandOutput([value],out.append)
  for data in [b'first-frag',b'ment\nsecond-',b'fragment\n']:
   parser.feed(data)
  self.assertNotIn('first-fragment',''.join(out));self.assertNotIn('second-fragment',''.join(out))
  out.clear();parser=host.CommandOutput([],out.append)
  parser.feed(('x'*20000+'author'+'ization:\n').encode());parser.feed(b'fixture-hidden\n')
  self.assertNotIn('fixture-hidden',''.join(out));self.assertLessEqual(len(parser.pending),parser.limit)
  out.clear();parser=host.CommandOutput([],out.append)
  parser.feed(('author'+'ization:\n').encode());parser.feed(b'fixture-hidden\n')
  self.assertNotIn('fixture-hidden',''.join(out))

 def test_unfinished_record_is_withheld_until_final_flush(self):
  out=[];parser=fixtures.host.CommandOutput([],out.append)
  parser.feed(b'finished without newline');self.assertEqual(out,[])
  parser.feed(final=True);self.assertEqual(out,['finished without newline'])

 def test_bounded_events_heartbeat_and_observed_slow_test(self):
  import threading
  clock=[100.0];wall=[1000.0];saved=[];job={'job_id':str(uuid.uuid4())}
  runner=types.SimpleNamespace(secrets=[],job_context=threading.local(),resource_progress=lambda j:saved.append(json.loads(json.dumps(j))))
  observer=fixtures.host.CommandObservation(runner,job,{'phase':'test','recipe':'fixed-test','kind':'test'},123,lambda:clock[0],lambda:wall[0])
  observer.flush();observer.feed(b'test fixture::slow ... ')
  clock[0]+=65;wall[0]+=65;observer.flush()
  self.assertEqual(job['execution_observation']['test_progress']['active']['observed_seconds'],65)
  observer.feed(b'ok\n');observer.flush(final=True,outcome='completed')
  result=job['execution_observation'];self.assertEqual(result['test_progress']['completed'],1)
  self.assertEqual(result['test_progress']['slowest'][0]['seconds'],65)
  self.assertEqual(result['state'],'terminal');self.assertEqual(result['heartbeat_at'],1065)
  for _ in range(100):observer.feed(b'x'*4095+b'\n')
  observer.flush(final=True,outcome='completed')
  self.assertLessEqual(len(job['execution_observation']['events']),32)
  self.assertGreater(job['execution_observation']['events'][0]['cursor'],1)

 def test_resource_pause_and_sampling_are_observational(self):
  import threading
  clock=[100.0];job={'job_id':str(uuid.uuid4())}
  resources=types.SimpleNamespace(lock=threading.RLock(),rows={'phase':{'state':'parked','container_id':'a'*64,'metrics':{'sampled_at':900,'memory':1024}}},elapsed_pause=lambda _:5)
  context=threading.local();context.phase_container='phase'
  runner=types.SimpleNamespace(secrets=[],job_context=context,resources=resources)
  observer=fixtures.host.CommandObservation(runner,job,{'phase':'compile_test','recipe':'fixed'},123,lambda:clock[0],lambda:1000)
  clock[0]+=10;observer.flush()
  result=job['execution_observation'];self.assertEqual(result['state'],'paused')
  self.assertEqual(result['paused_seconds'],5);self.assertEqual(result['execution_seconds'],5)
  self.assertEqual(result['container_id'],'a'*64)

 def test_real_command_streams_before_exit_and_private_output_is_absent(self):
  import threading,sys
  host=fixtures.host
  with tempfile.TemporaryDirectory()as directory:
   runner=host.Runner(directory);job={'job_id':str(uuid.uuid4())};seen=[]
   runner.resource_progress=lambda j:seen.append((time.monotonic(),json.loads(json.dumps(j))))
   runner.job_context.job=job;runner.job_context.observation_scope={'phase':'test','recipe':'fixture','kind':'test'}
   runner.command([sys.executable,'-c','import time; print("visible",flush=True); time.sleep(2.3); print("done")'])
   last=seen[-1][0]
   self.assertTrue(any('visible' in str(row['execution_observation'].get('events')) and at<last-.1 for at,row in seen))
   self.assertEqual(job['execution_observation']['outcome'],'completed')
   seen.clear()
   runner.command([sys.executable,'-c','print("private fixture")'],private_raw=True)
   self.assertEqual(seen,[])
   with self.assertRaisesRegex(RuntimeError,'fixed private command failed')as caught:
    runner.command([sys.executable,'-c','import sys; print("private fixture"); sys.exit(1)'],private_raw=True)
   self.assertNotIn('private fixture',str(caught.exception))

 def test_timeout_cancel_and_reporting_failure_preserve_execution(self):
  import sys
  host=fixtures.host
  with tempfile.TemporaryDirectory()as directory:
   runner=host.Runner(directory);job={'job_id':str(uuid.uuid4())}
   runner.job_context.job=job;runner.job_context.observation_scope={'phase':'test','recipe':'fixture','kind':'test'}
   with self.assertRaises(TimeoutError):runner.command([sys.executable,'-c','import time; time.sleep(5)'],timeout=.1)
   self.assertEqual(job['execution_observation']['outcome'],'timed_out')
   runner.cancel.set()
   with self.assertRaises(PermissionError):runner.command([sys.executable,'-c','import time; time.sleep(5)'])
   self.assertEqual(job['execution_observation']['outcome'],'cancelled')
   runner.cancel.clear()
   def unavailable(_):raise OSError('fixture persistence failure')
   runner.resource_progress=unavailable
   self.assertIn('survives',runner.command([sys.executable,'-c','print("survives")']))
   self.assertEqual(job['execution_observation']['reporting'],'unavailable')


class ObservationAuthorizationTests(MaintenanceTests):
 def install_job(self):
  job={'job_id':str(uuid.uuid4()),'status':'completed','phase':'done','operation':'check',
       'user_id':'codex-local','workspace':self.sid,'created_at':time.time(),'request_key':uuid.uuid4().hex,
       'execution_observation':{'heartbeat_at':time.time()-30,'next_cursor':4,'events':[
          {'cursor':3,'at':1000,'text':'third\n'},{'cursor':4,'at':1001,'text':'fourth\n'}]}}
  self.broker.jobs[job['job_id']]=job;self.broker._save(job);return job
 def test_cursor_authorization_gaps_and_restart(self):
  job=self.install_job();args={'job_id':job['job_id'],'log_cursor':0}
  result=self.call('v3_worker_job',args)['execution_observation']
  self.assertTrue(result['log_read']['gap']);self.assertTrue(result['stale'])
  args['log_cursor']=4
  self.assertEqual(self.call('v3_worker_job',args)['execution_observation']['log_read']['events'],[])
  with self.assertRaises(ValueError):self.call('v3_worker_job',args,{**self.trusted,'user_id':'other'})
  for invalid in (-1,True,'4',2**63):
   with self.assertRaises(ValueError):self.call('v3_worker_job',{**args,'log_cursor':invalid})
  self.broker.shutdown();self.broker=fixtures.host.Broker(self.state,self.runner)
  result=self.call('v3_worker_job',{'job_id':job['job_id'],'log_cursor':2})
  self.assertEqual([e['cursor']for e in result['execution_observation']['log_read']['events']],[3,4])
  self.assertEqual(result['execution_observation']['state'],'terminal')
 def test_concurrent_readers_do_not_consume_each_others_cursor(self):
  from concurrent.futures import ThreadPoolExecutor
  job=self.install_job()
  with ThreadPoolExecutor(max_workers=8)as pool:
   results=list(pool.map(lambda _:self.call('v3_worker_job',{'job_id':job['job_id'],'log_cursor':3}),range(64)))
  self.assertTrue(all([e['cursor']for e in r['execution_observation']['log_read']['events']]==[4]for r in results))

 def test_running_restart_retains_cursor_and_never_replays(self):
  job=self.install_job();job["status"]="running";self.broker._save(job)
  self.broker.shutdown();self.broker=fixtures.host.Broker(self.state,self.runner)
  result=self.call('v3_worker_job',{'job_id':job['job_id'],'log_cursor':4})
  self.assertEqual(result['status'],'interrupted')
  self.assertEqual(result['execution_state'],'terminal')
  self.assertEqual(result['execution_observation']['log_read']['events'],[])

 def test_wait_state_and_expired_authority(self):
  job=self.install_job();job['status']='running';job['resource_waiting']={'state':'waiting','since':time.time(),'phase':'test'}
  result=self.call('v3_worker_job',{'job_id':job['job_id'],'log_cursor':0})
  self.assertEqual(result['execution_state'],'resource_waiting')
  self.assertEqual(result['execution_observation']['state'],'resource_waiting')
  with self.assertRaises(ValueError):
   self.call('v3_worker_job',{'job_id':job['job_id'],'log_cursor':0},{**self.trusted,'lease_until':0})

 def test_actual_phase_precedes_first_command_and_unknown_is_explicit(self):
  job=self.install_job();job.pop('execution_observation');job['status']='running'
  result=self.call('v3_worker_job',{'job_id':job['job_id'],'log_cursor':0})
  self.assertEqual(result['execution_state'],'unavailable')
  job['active_verification']={'id':'bounded-check','phase':'compile_test'}
  result=self.call('v3_worker_job',{'job_id':job['job_id'],'log_cursor':0})
  self.assertEqual(result['phase'],'compile_test')
  self.assertEqual(result['execution_state'],'preparing')
  self.assertEqual(result['execution_observation']['reporting'],'unavailable')

 def test_cursor_poll_does_not_repeat_legacy_log_or_load_queue_archives(self):
  job=self.install_job();job['log']='retained terminal log'
  with patch.object(self.broker,'_queue_preflight',side_effect=AssertionError('cursor poll must not inspect archives')):
   result=self.call('v3_worker_job',{'job_id':job['job_id'],'log_cursor':4})
  self.assertNotIn('log',result);self.assertTrue(result['legacy_log_omitted'])
  self.assertEqual(self.call('v3_worker_job',{'job_id':job['job_id']})['log'],'retained terminal log')


class CommandReturnBoundaryTests(unittest.TestCase):
 def test_unobserved_protocol_and_binary_exports_keep_their_output_contract(self):
  import sys,io
  with tempfile.TemporaryDirectory()as directory:
   runner=fixtures.host.Runner(directory)
   self.assertEqual(runner.command([sys.executable,'-c','print("x"*20000)']),"x"*20000+"\n")
   runner.job_context.job={'job_id':str(uuid.uuid4())}
   runner.job_context.observation_scope={'phase':'test','recipe':'fixture','kind':'test'}
   out=io.BytesIO()
   runner.command([sys.executable,'-c','import sys; sys.stdout.buffer.write(bytes([255])*100)'],output_file=out)
   self.assertEqual(out.getvalue(),bytes([255])*100)
   self.assertNotIn('execution_observation',runner.job_context.job)

 def test_active_test_identity_cannot_disclose_a_secret_prefix(self):
  import threading
  runner=types.SimpleNamespace(secrets=['fixture::slow '+'... ok'],job_context=threading.local())
  job={};observer=fixtures.host.CommandObservation(runner,job,{'phase':'test','recipe':'fixed','kind':'test'},123)
  observer.feed(b'test namespace::fixture::slow ... ')
  self.assertIsNone(observer.active)
  self.assertNotIn('fixture::slow',json.dumps(job))
  observer.feed(b'ok\n');observer.flush(final=True,outcome='completed')
  self.assertNotIn('fixture::slow',json.dumps(job))

 def test_active_name_inspection_does_not_consume_sensitive_continuation(self):
  import threading
  runner=types.SimpleNamespace(secrets=[],job_context=threading.local())
  job={};observation=fixtures.host.CommandObservation(runner,job,{'phase':'test','kind':'test','recipe':'fixture'},1)
  observation.feed(('token'+'=\n').encode())
  observation.feed(b'test namespace::fixture ... ')
  self.assertIsNone(observation.active)
  self.assertTrue(observation.parser.sensitive_continuation)
  observation.feed(b'ok\n');observation.flush(final=True,outcome='completed')
  self.assertNotIn('namespace::fixture',json.dumps(job))

class SourceControlContractTests(unittest.TestCase):
 def test_coordinated_native_tools_keep_an_exact_closed_contract(self):
  import copy,shutil
  host=fixtures.host
  native=["source_remote_read","source_remote_commit","source_site_create","source_site_status",
          "source_preview_start","source_preview_stop","source_publish","source_rollback"]
  tools=["repository_access","git_tool","resolve_conflict","commit_and_push",*native]
  self.assertEqual(host.GIT_CONTROL_TOOLS,tools)
  source=Path(__file__).resolve().parents[1]/"plugins/abilities/Administrator/git_control"
  with tempfile.TemporaryDirectory() as temporary:
   folder=Path(temporary)/"git_control";shutil.copytree(source,folder)
   manifest=json.loads((folder/"plugin.json").read_bytes());manifest["tools"]=tools
   catalog=json.loads((folder/"git_control.json").read_bytes());catalog["tools"]=tools
   (folder/"plugin.json").write_text(json.dumps(manifest));(folder/"git_control.json").write_text(json.dumps(catalog))
   host.validate_git_control_package(folder)
   for filename,value in (("plugin.json",manifest),("git_control.json",catalog)):
    changed=copy.deepcopy(value);changed["tools"].append("unregistered_source_tool")
    (folder/filename).write_text(json.dumps(changed))
    with self.assertRaisesRegex(ValueError,"Incompatible Git Control"):
     host.validate_git_control_package(folder)
    (folder/filename).write_text(json.dumps(value))
   changed=copy.deepcopy(manifest);changed["build"]["inputs"].append("unregistered.py")
   (folder/"plugin.json").write_text(json.dumps(changed))
   with self.assertRaisesRegex(ValueError,"Incompatible Git Control contract"):
    host.validate_git_control_package(folder)


class TerminalScratchRetentionTests(unittest.TestCase):
 """Terminal job scratch expires without touching retry, open or retained evidence."""
 def setUp(self):
  self.host=module("build_host_scratch",Path(__file__).resolve().parents[1]/"build-service/host.py")
 def job(self,ident,status="completed",created_at=0):
  return {"job_id":ident,"status":status,"created_at":created_at,"task_id":"task-"+ident}
 def item(self,ident,**extra):
  return {"key":ident.replace("-","")[:64],"job_id":ident,"task_id":"task-"+ident,"status":"closed",**extra}
 def eligible(self,jobs,work):
  return [pair[0] for pair in self.host.terminal_scratch_jobs(jobs,work)]

 def test_container_mounts_batches_and_projects(self):
  calls=[]
  class Runner:
   def command(self,argv,timeout=0,private_raw=False):
    calls.append(argv)
    return "".join('[{"Source":"/mnt/%s/%s","Destination":"/x"}]\n'%(len(calls),index) for index,_ in enumerate(argv[4:]))+'[]\n'
  rows=self.host.container_mounts(Runner(),[str(index)for index in range(70)])
  self.assertEqual([32,32,6],[len(argv)-4 for argv in calls])
  self.assertEqual(70,len(rows))
  self.assertTrue(all("--format" in argv for argv in calls))
  self.assertEqual(Path("/mnt/1/0"),rows[0])
 def test_terminal_job_scratch_is_eligible(self):
  job=self.job("11111111-1111-1111-1111-111111111111",status="cancelled")
  self.assertEqual([job["job_id"]],self.eligible({job["job_id"]:job},{job["job_id"]:self.item(job["job_id"])}))
 def test_two_newest_completed_jobs_are_retained(self):
  jobs={ident:self.job(ident,created_at=index)for index,ident in enumerate((
   "11111111-1111-1111-1111-111111111111","22222222-2222-2222-2222-222222222222","33333333-3333-3333-3333-333333333333"))}
  work={ident:self.item(ident)for ident in jobs}
  self.assertEqual(["11111111-1111-1111-1111-111111111111"],self.eligible(jobs,work))
 def test_resume_source_and_its_chain_are_retained(self):
  source="11111111-1111-1111-1111-111111111111";middle="22222222-2222-2222-2222-222222222222"
  jobs={source:self.job(source,created_at=1),middle:self.job(middle,created_at=2),
   "33333333-3333-3333-3333-333333333333":self.job("33333333-3333-3333-3333-333333333333",created_at=3),
   "44444444-4444-4444-4444-444444444444":{**self.job("44444444-4444-4444-4444-444444444444",status="failed",created_at=4),
    "packaging_resume":{"source_job_id":middle}},
   "55555555-5555-5555-5555-555555555555":{**self.job("55555555-5555-5555-5555-555555555555",status="failed",created_at=5),
    "packaging_resume":{"source_job_id":source}}}
  work={ident:self.item(ident)for ident in jobs}
  self.assertEqual([],self.eligible(jobs,work))
 def test_open_task_notice_and_failed_job_are_retained(self):
  open_job=self.job("11111111-1111-1111-1111-111111111111")
  noticed=self.job("22222222-2222-2222-2222-222222222222")
  failed=self.job("33333333-3333-3333-3333-333333333333",status="failed")
  running=self.job("44444444-4444-4444-4444-444444444444",status="running")
  jobs={job["job_id"]:job for job in (open_job,noticed,failed,running)}
  work={open_job["job_id"]:self.item(open_job["job_id"],status="open"),
   noticed["job_id"]:self.item(noticed["job_id"],leftover_notice={"id":"n"*64,"reviewed_at":None}),
   failed["job_id"]:self.item(failed["job_id"]),running["job_id"]:self.item(running["job_id"])}
  self.assertEqual([],self.eligible(jobs,work))
