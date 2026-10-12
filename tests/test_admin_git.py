"""Real temporary Git repositories; never runtime data or server repositories."""
import asyncio,hashlib,importlib.util,subprocess,tempfile,unittest
from pathlib import Path
FILE=Path(__file__).resolve().parents[1]/"plugins/abilities/Administrator/git_control/adapter.py"
spec=importlib.util.spec_from_file_location("admin_git",FILE);git=importlib.util.module_from_spec(spec);spec.loader.exec_module(git)
class GitTests(unittest.IsolatedAsyncioTestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.base=Path(self.tmp.name);self.repo=self.base/"webagent-v3";self.repo.mkdir()
  self.run_git("init","-q","--initial-branch=main")
  self.run_git("config","user.name","Fixture");self.run_git("config","user.email","fixture@localhost")
  (self.repo/"file.txt").write_text("baseline\n");self.run_git("add","file.txt");self.run_git("commit","-qm","baseline")
  self.remote=self.base/"remote.git";subprocess.run(["git","init","--bare","-q",str(self.remote)],check=True)
  self.run_git("remote","add","origin",str(self.remote))
 def tearDown(self):self.tmp.cleanup()
 def run_git(self,*args,allowed=(0,)):
  p=subprocess.run(["git",*args],cwd=self.repo,capture_output=True)
  self.assertIn(p.returncode,allowed,p.stderr.decode());return p.stdout.decode().strip()
 async def call(self,name,args):
  return await git.execute(name,args,self.base)
 async def test_file_manager_regular_commit_with_dirty_gitlink_and_saved_author(self):
  fm_spec=importlib.util.spec_from_file_location("file_manager",FILE.with_name("file_manager.py"))
  fm=importlib.util.module_from_spec(fm_spec);fm_spec.loader.exec_module(fm)
  nested=self.repo/"reference";nested.mkdir()
  subprocess.run(["git","init","-q",str(nested)],check=True)
  self.run_git("update-index","--add","--cacheinfo","160000,"+self.run_git("rev-parse","HEAD")+",reference")
  self.run_git("commit","-qm","reference pointer")
  (nested/"dirty.txt").write_text("reference work stays separate")
  (self.repo/"file.txt").write_text("updated source\n")
  for name in ("target","runtime","workspaces","build-jobs"):
   (self.repo/name).mkdir();(self.repo/name/"private.txt").write_text("runtime fixture")
  view=await fm._git_status(git,self.repo,"__root__")
  rows=view["staged"]+view["unstaged"]+view["untracked"]
  self.assertEqual(view["git_user_name"],"Fixture")
  self.assertEqual(view["git_user_email"],"fixture@localhost")
  self.assertFalse(next(row for row in rows if row["path"]=="reference")["committable"])
  self.assertEqual({row["path"] for row in rows if row["committable"]},{"file.txt"})
  result=await fm._git_commit(git,self.repo,{"repo":"__root__","paths":["file.txt"],"message":"regular files only","expected_head":view["head"],"expected_review_sha256":view["review_sha256"]})
  self.assertEqual(result["status"],"committed")
  self.assertEqual(self.run_git("show","--format=%an <%ae>","--no-patch"),"Fixture <fixture@localhost>")
  self.assertEqual(self.run_git("diff-tree","--no-commit-id","--name-only","-r","HEAD"),"file.txt")
  await git.network(self.base,self.repo,"origin","push")
  pushed=subprocess.run(["git","--git-dir",str(self.remote),"rev-parse","main"],capture_output=True,check=True).stdout.decode().strip()
  self.assertEqual(pushed,self.run_git("rev-parse","HEAD"))

 async def test_file_manager_commit_more_than_64_paths(self):
  fm_spec=importlib.util.spec_from_file_location("file_manager",FILE.with_name("file_manager.py"))
  fm=importlib.util.module_from_spec(fm_spec);fm_spec.loader.exec_module(fm)
  names=[f"change-{i:03}.txt" for i in range(100)]
  for name in names:(self.repo/name).write_text("ordinary source change\n")
  view=await fm._git_status(git,self.repo,"__root__")
  with self.assertRaisesRegex(git.Rejected,"1 to 64"):
   git.paths(self.repo,names)
  original_head=self.run_git("rev-parse","HEAD")
  result=await fm._git_commit(git,self.repo,{"repo":"__root__","paths":names,"message":"large regular file commit","expected_head":view["head"],"expected_review_sha256":view["review_sha256"]})
  self.assertEqual(result["status"],"committed")
  self.assertEqual(set(self.run_git("diff-tree","--no-commit-id","--name-only","-r","HEAD").splitlines()),set(names))
  self.assertEqual(self.run_git("rev-parse","HEAD^"),original_head)
  self.assertEqual(self.run_git("status","--porcelain"),"")

 async def test_file_manager_commit_path_validation(self):
  fm_spec=importlib.util.spec_from_file_location("file_manager",FILE.with_name("file_manager.py"))
  fm=importlib.util.module_from_spec(fm_spec);fm_spec.loader.exec_module(fm)
  for names in ([],["file.txt"]*2,[None],[f"file-{i}.txt" for i in range(fm.MAX_ENTRIES+1)],["../escape"]):
   with self.subTest(paths=names[:3]):
    with self.assertRaises((git.Rejected,fm.Rejected)):
     await fm._git_commit(git,self.repo,{"paths":names})
  with self.assertRaises(git.Rejected):
   git.paths(self.repo,["file.txt"]*65,max_paths=fm.MAX_ENTRIES)

 async def test_large_outgoing_history_push_and_streamed_secret_guard(self):
  (self.repo/"large.txt").write_text("ordinary source line\n"*12000)
  self.run_git("add","large.txt");self.run_git("commit","-qm","large source")
  with self.assertRaises(git.Rejected):
   await git.command(self.repo,["log","-p","HEAD","--"])
  await git.network(self.base,self.repo,"origin","push")
  pushed=subprocess.run(["git","--git-dir",str(self.remote),"rev-parse","main"],capture_output=True,check=True).stdout.decode().strip()
  self.assertEqual(pushed,self.run_git("rev-parse","HEAD"))
  (self.repo/"large.txt").write_text("ordinary source line\n"*24000+"access_token="+"x"*24+"\n")
  self.run_git("add","large.txt");self.run_git("commit","-qm","guard fixture")
  with self.assertRaisesRegex(git.Rejected,"credential guard"):
   await git.network(self.base,self.repo,"origin","push")
  unchanged=subprocess.run(["git","--git-dir",str(self.remote),"rev-parse","main"],capture_output=True,check=True).stdout.decode().strip()
  self.assertEqual(unchanged,pushed)

 async def test_minified_outgoing_line_pushes_and_long_literal_is_guarded(self):
  (self.repo/"bundle.min.js").write_text("const vector='"+"z"*330000+"';\n")
  self.run_git("add","bundle.min.js");self.run_git("commit","-qm","minified source")
  await git.network(self.base,self.repo,"origin","push")
  pushed=subprocess.run(["git","--git-dir",str(self.remote),"rev-parse","main"],capture_output=True,check=True).stdout.decode().strip()
  self.assertEqual(pushed,self.run_git("rev-parse","HEAD"))
  # Retain the keyword before 64 KiB until the closing quote arrives.
  (self.repo/"bundle.min.js").write_text('access_token="'+"q"*330000+'"\n')
  self.run_git("add","bundle.min.js");self.run_git("commit","-qm","long literal guard fixture")
  with self.assertRaisesRegex(git.Rejected,"credential guard"):
   await git.network(self.base,self.repo,"origin","push")
  unchanged=subprocess.run(["git","--git-dir",str(self.remote),"rev-parse","main"],capture_output=True,check=True).stdout.decode().strip()
  self.assertEqual(unchanged,pushed)

 async def test_agent_and_file_manager_transport_use_same_saved_connection(self):
  from unittest.mock import AsyncMock,patch
  spec=importlib.util.spec_from_file_location("shared_git_transport",FILE.with_name("transport.py"))
  transport=importlib.util.module_from_spec(spec);spec.loader.exec_module(transport)
  payload={"agent_id":"shared_default","connections":[{"url":"https://example.test/team/repo","username":"git","token":"fixture-token"}]}
  agent=transport.connections(payload)
  manager=transport.connections({"repository":payload})
  self.assertEqual(agent,manager)
  environments=[]
  with patch.object(transport.adapter,"command",new=AsyncMock(return_value=b"https://example.test/team/repo")),patch.object(transport.os,"access",return_value=True):
   for rows in (agent,manager):
    lease=transport._CONNECTIONS.set(rows)
    try:environments.append(await transport.authorize(self.repo,"origin","push"))
    finally:transport._CONNECTIONS.reset(lease)
  self.assertEqual(environments[0],environments[1])
  self.assertEqual(environments[0]["V3_GIT_TOKEN"],"fixture-token")

 async def test_portability_fixture_uses_recognized_reserved_credentials(self):
  source=FILE.parents[4]/"tests/test_build_service.py"
  self.assertFalse(git.secret(source.read_bytes()))

 async def test_outgoing_line_scan_remains_bounded(self):
  (self.repo/"oversized.min.js").write_text("z"*(1024*1024+1)+"\n")
  self.run_git("add","oversized.min.js");self.run_git("commit","-qm","oversized scan fixture")
  with self.assertRaisesRegex(git.Rejected,"line exceeds scan bound"):
   await git.network(self.base,self.repo,"origin","push")
  self.assertEqual(subprocess.run(["git","--git-dir",str(self.remote),"rev-parse","--verify","main"],capture_output=True).returncode,128)

 async def test_credential_literals_and_source_expressions(self):
  for source in (
   b"api_key: platformCatalog ? '' : apiKey",
   b"const secret=target.matches(selector)",
   b"let password=credentials.password.clone();",
   b"password=hashlib.sha256(input).hexdigest()",
   b'api_key: key.value.trim()',
   b'api_key=abcdefghijklmnop12345',
   b'https://u:p@example.test/repo',
  ):
   self.assertFalse(git.secret(source),source)
  credential=b'V9m2q7Z4n8R1p6T3s5W0'
  for source in (
   b'api_key="'+credential+b'"',
   b'"access_token": "'+credential+b'"',
   b'PASSWORD='+credential,
   b'secret = "'+b'alphabetical'+b'credential"',
   b'https://'+b'user:password'+b'@github.com/repo',
   b'ghp_' + b'A'*36,
   b'-----BEGIN '+b'PRIVATE KEY-----',
  ):
   self.assertTrue(git.secret(source))
  # Test paths receive the same protection as other source.
  (self.repo/"test_credentials.py").write_bytes(b'api_key="'+credential+b'"\n')
  self.run_git("add","test_credentials.py");self.run_git("commit","-qm","credential fixture")
  with self.assertRaisesRegex(git.Rejected,"credential guard"):
   await git.network(self.base,self.repo,"origin","push")

 async def test_push_definite_auth_failure_and_ambiguous_failure(self):
  from unittest.mock import patch
  class FailedProcess:
   returncode=128
   def __init__(self,output):
    self.stdout=asyncio.StreamReader();self.stdout.feed_data(output);self.stdout.feed_eof()
   async def wait(self):return self.returncode
  async def run(output):
   async def spawn(*args,**kwargs):return FailedProcess(output)
   with patch.object(git.asyncio,"create_subprocess_exec",spawn):
    return await git.command(self.repo,["push","origin","HEAD:refs/heads/main"],mutable=True,env_extra={"V3_GIT_TOKEN":"private-fixture"})
  with self.assertRaisesRegex(git.Rejected,"authentication rejected"):
   await run(b"fatal: Authentication failed for 'https://example.test/repo/'")
  with self.assertRaisesRegex(git.Rejected,"write permission"):
   await run(b"fatal: unable to access 'https://example.test/repo/': The requested URL returned error: 403")
  with self.assertRaisesRegex(git.Rejected,"repository unavailable"):
   await run(b"remote: Repository not found.")
  with self.assertRaises(git.Uncertain):
   await run(b"fatal: remote hung up unexpectedly")
  # Credential withholding always wins over error classification.
  with self.assertRaises(git.Uncertain):
   await run(b"fatal: Authentication failed for private-fixture")

 async def test_status_commit_push_and_dedupe(self):
  initial=await self.call("git_tool",{"operation":"status"})
  self.assertEqual(initial["status"],"success",initial)
  (self.repo/"file.txt").write_text("new source\n")
  view=(await self.call("git_tool",{"operation":"status"}))["output"]
  result=await self.call("commit_and_push",{"paths":["file.txt"],"message":"change","expected_head":view["head"],"expected_review_sha256":view["review_sha256"]})
  self.assertEqual(result["status"],"success",result)
  pushed=subprocess.run(["git","--git-dir",str(self.remote),"rev-parse","main"],capture_output=True,check=True).stdout.decode().strip()
  self.assertEqual(pushed,result["output"]["commit"])
  notice=git.notice(view);self.assertTrue(notice["notify"]);self.assertFalse(git.notice(view,notice["snapshot_sha256"])["notify"])
 async def test_stale_review_paths_and_symlink(self):
  view=await git.snapshot(self.base);(self.repo/"file.txt").write_text("changed after review")
  rejected=await self.call("commit_and_push",{"paths":["file.txt"],"message":"stale","expected_head":view["head"],"expected_review_sha256":view["review_sha256"]})
  self.assertEqual(rejected["error_kind"],"git_rejected")
  for args in [{"operation":"status","repo":"../other"},{"operation":"add","paths":["../escape"],"expected_head":view["head"]},{"operation":"push","force":True,"expected_head":view["head"]}]:
   self.assertEqual((await self.call("git_tool",args))["status"],"error")
  (self.repo/"link").symlink_to(self.base/"remote.git")
  self.assertEqual((await self.call("git_tool",{"operation":"add","paths":["link"],"expected_head":view["head"]}))["status"],"error")
 async def test_secret_and_external_config_guards(self):
  (self.repo/"key.txt").write_text("api_key="+"V9m2q7Z4n8R1p6T3s5W0")
  view=await git.snapshot(self.base)
  result=await self.call("commit_and_push",{"paths":["key.txt"],"message":"unsafe","expected_head":view["head"],"expected_review_sha256":view["review_sha256"]})
  self.assertEqual(result["status"],"error")
  self.run_git("config","remote.origin.url","https://user:password@example.invalid/repository")
  result=await self.call("git_tool",{"operation":"push","expected_head":view["head"]})
  self.assertEqual(result["status"],"error");self.assertNotIn("password@",str(result))
  self.run_git("config","remote.origin.url",str(self.remote));self.run_git("config","filter.unsafe.clean","touch escaped")
  self.assertEqual((await self.call("git_tool",{"operation":"status"}))["status"],"error")
 async def test_conflict_sha_both_and_ours(self):
  self.run_git("checkout","-qb","other");(self.repo/"file.txt").write_text("other side\n");self.run_git("commit","-qam","other")
  self.run_git("checkout","-q","main");(self.repo/"file.txt").write_text("our side\n");self.run_git("commit","-qam","ours")
  self.run_git("merge","other",allowed=(1,))
  content=(self.repo/"file.txt").read_bytes()
  bad=await self.call("resolve_conflict",{"path":"file.txt","strategy":"both","expected_sha256":"0"*64})
  self.assertEqual(bad["status"],"error")
  result=await self.call("resolve_conflict",{"path":"file.txt","strategy":"both","expected_sha256":hashlib.sha256(content).hexdigest()})
  self.assertEqual(result["status"],"success",result)
  self.assertEqual((self.repo/"file.txt").read_text(),"our side\nother side\n")
  result=await self.call("resolve_conflict",{"path":"file.txt","strategy":"ours","expected_sha256":result["output"]["sha256"]})
  self.assertEqual(result["status"],"success",result);self.assertEqual((self.repo/"file.txt").read_text(),"our side\n")
 async def test_hooks_alternates_and_remote_hook_refusal(self):
  hook=self.remote/"hooks/pre-receive";hook.write_text("#!/bin/sh\nexit 0\n");hook.chmod(0o755)
  current=await git.head(self.repo)
  self.assertEqual((await self.call("git_tool",{"operation":"push","expected_head":current}))["status"],"error")
  hook.unlink();(self.repo/".git/objects/info/alternates").write_text("/outside")
  self.assertEqual((await self.call("git_tool",{"operation":"status"}))["status"],"error")
 async def test_operation_mutation_fence_and_unknown_name(self):
  current=await git.head(self.repo)
  self.assertEqual((await self.call("git_tool",{"operation":"branch_create","name":"safe","expected_head":current}))["status"],"success")
  self.assertIn("safe",self.run_git("branch","--list"))
  self.assertEqual((await self.call("git_tool",{"operation":"checkout","ref":"safe","expected_head":"0"*40}))["status"],"error")
  self.assertEqual((await self.call("shell",{"command":"pwd"}))["status"],"error")
 async def test_optional_schema_defaults_and_exact_mutation_paths(self):
  defaults={"repo":"","paths":[],"ref":"","name":"","message":"","remote":"","limit":20,"cached":False,"expected_head":"0"*40}
  for operation in ("status","diff","log","show","branch_list","tag_list","stash_list"):
   result=await self.call("git_tool",{"operation":operation,**defaults})
   self.assertEqual(result["status"],"success",(operation,result))
  current=await git.head(self.repo)
  for operation in ("add","reset","checkout","branch_create"):
   result=await self.call("git_tool",{"operation":operation,**defaults,"expected_head":current})
   self.assertEqual(result["status"],"error",(operation,result))
  self.assertEqual((await self.call("git_tool",{"operation":"status",**defaults,"name":"substantive"}))["status"],"error")
  self.assertEqual((await self.call("git_tool",{"operation":"status",**defaults,"unknown":""}))["status"],"error")
  (self.repo/"file.txt").write_text("blank defaults commit\n")
  view=(await self.call("git_tool",{"operation":"status",**defaults}))["output"]
  result=await self.call("commit_and_push",{"repo":"","remote":"","paths":["file.txt"],"message":"defaults","expected_head":view["head"],"expected_review_sha256":view["review_sha256"]})
  self.assertEqual(result["status"],"success",result)
if __name__=="__main__":unittest.main()
