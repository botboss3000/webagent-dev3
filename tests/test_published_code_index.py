import importlib.util,json,os,shutil,tempfile,unittest,uuid
from pathlib import Path
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[1]
def load(name,path):
 s=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);return m
idx=load("test_idx",ROOT/"plugins/abilities/Administrator/code_index/adapter.py")
rel=load("test_rel",ROOT/"plugins/abilities/Administrator/v3_build/release.py")
class PublishedTests(unittest.TestCase):
 def setUp(self):
  self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);self.assets=self.root/"assets"
 def tearDown(self):self.tmp.cleanup()
 def blob(self,extra=None):
  f={"v3/ui/index.html":(420,b"<div>app</div>\n"),"v3/src/main.rs":(420,b"fn main() {}\n"),
     "v3/config/settings.json":(420,b"{}\n"),"v3/docs/example.md":(420,b"docs\n"),"ui/shared/css/app.css":(420,b"body {}\n"),
     "v3/data/config/fixture.json":(420,b"{}\n")};f.update(extra or {})
  return rel.archive(f,lambda _:None)
 def stage(self,blob):
  j=str(uuid.uuid4());d=rel.stage_assets(self.assets,j,blob,lambda _:None)
  m=idx.build_published(d,d/".code-index",j,idx.sha(blob));return j,d,m
 def test_private_bootstrap_parent_swap_stays_confined(self):
  base=self.root/"session";own=base/"a/b";foreign=self.root/"foreign/b"
  for root,symbol in ((own,"own_symbol"),(foreign,"foreign_symbol")):
   root.mkdir(parents=True);(root/"source.py").write_text("def "+symbol+"(): pass\n")
   idx.build_published(root,root/".code-index",str(uuid.uuid4()),"0"*64)
  original=idx.open_path
  def swap(fd,parts,directory=False):
   opened=original(fd,parts,directory)
   if parts==["a","b"] and directory:
    (base/"a").rename(base/"a-old");(base/"a").symlink_to(foreign.parent,target_is_directory=True)
   return opened
  with patch.object(idx,"open_path",side_effect=swap):
   result=idx.execute("code_index_lookup",{"root":"a/b","action":"symbol","query":""},base)
  self.assertEqual("success",result["status"],result)
  text=json.dumps(result);self.assertIn("own_symbol",text);self.assertNotIn("foreign_symbol",text)
 def test_full_ui_pair_and_rollback(self):
  j,d,m=self.stage(self.blob());self.assertEqual(4,m["file_count"])
  self.assertTrue(m["parser_limitations"]);self.assertTrue(m["exclusions"])
  self.assertIsNone(rel.activate_assets(self.assets,j))
  b=self.blob({"v3/ui/index.html":(420,b"<div>new</div>\n")});k,e,n=self.stage(b)
  self.assertEqual(j,rel.activate_assets(self.assets,k))
  r=idx.execute_published("code_index_coverage",{"root":"published"},e)
  self.assertEqual("success",r["status"]);self.assertEqual(k,r["publication"]["release_id"])
  self.assertEqual(0,r["gaps"]);self.assertEqual(0,r["stale_count"]);self.assertEqual(0,r["unindexed_count"])
  self.assertEqual(k,rel.activate_assets(self.assets,j));self.assertEqual(j,rel.current_asset_id(self.assets))
  self.assertEqual(m["index_sha256"],idx.read_published(d,j)[1]["index_sha256"])
  rel.stage_assets(self.assets,k,b,lambda _:None)
 def test_final_rebase_delete_and_rename(self):
  base=self.blob({"v3/ui/old.js":(420,b"const old=1;\n")});f=rel.entries(base,lambda _:None)
  f.pop("v3/ui/old.js");f["v3/ui/new.js"]=(420,b"const renamed=1;\n")
  live=self.blob({"v3/ui/old.js":(420,b"const old=1;\n"),"v3/src/other.rs":(420,b"fn concurrent() {}\n")})
  merged,_=rel.candidate(base,rel.archive(f,lambda _:None),live,lambda _:None);j,d,m=self.stage(merged)
  r=idx.execute_published("code_index_scan",{"root":"published","action":"discover"},d)
  paths={x["path"] for x in r["results"]};self.assertIn("v3/src/other.rs",paths)
  self.assertIn("v3/ui/new.js",paths);self.assertNotIn("v3/ui/old.js",paths)
 def test_failure_and_interrupt_retain_previous(self):
  j,d,m=self.stage(self.blob());rel.activate_assets(self.assets,j)
  k=str(uuid.uuid4());e=rel.stage_assets(self.assets,k,self.blob(),lambda _:None)
  with patch.object(idx,"MAX_TOTAL_LINES",1):
   with self.assertRaises(ValueError):idx.build_published(e,e/".code-index",k,idx.sha(self.blob()))
  with self.assertRaises(ValueError):rel.activate_assets(self.assets,k)
  self.assertEqual(j,rel.current_asset_id(self.assets))
  k,e,m=self.stage(self.blob())
  with patch.object(rel.os,"replace",side_effect=OSError("interrupted")):
   with self.assertRaises(OSError):rel.activate_assets(self.assets,k)
  self.assertEqual(j,rel.current_asset_id(self.assets))
 def test_digest_mismatch(self):
  j,d,m=self.stage(self.blob())
  with self.assertRaisesRegex(ValueError,"digest mismatch"):rel.verify_index(d,j,"0"*64)
  (d/"v3/src/main.rs").write_text("fn changed() {}\n")
  self.assertEqual("error",idx.execute_published("code_index_coverage",{"root":"published"},d)["status"])
  with self.assertRaises(ValueError):rel.activate_assets(self.assets,j)
 def test_limit_never_publishes_partial(self):
  j=str(uuid.uuid4());d=rel.stage_assets(self.assets,j,self.blob(),lambda _:None)
  with patch.object(idx,"MAX_FILES",1):
   with self.assertRaises(ValueError):idx.build_published(d,d/".code-index",j,idx.sha(self.blob()))
  self.assertFalse((d/".code-index/manifest.json").exists())
 def test_session_isolation_and_published_read_only(self):
  j,d,m=self.stage(self.blob())
  self.assertEqual("error",idx.execute_published("code_index_scan",{"root":"published","action":"scan"},d)["status"])
  for name in ("a","b"):
   p=self.root/name;p.mkdir();shutil.copytree(d/"v3",p/"webagent-v3");idx.seed_private_index(d,p/"webagent-v3")
  a,b=self.root/"a",self.root/"b"
  self.assertEqual("success",idx.execute("code_index_coverage",{},a)["status"])
  (a/"webagent-v3/src/main.rs").write_text("fn private_change() {}\n")
  self.assertEqual("success",idx.execute("code_index_scan",{"action":"scan"},a)["status"])
  self.assertEqual(0,idx.execute("code_index_coverage",{},a)["stale_count"])
  self.assertEqual(0,idx.execute("code_index_coverage",{},b)["stale_count"])
  workspace=load("published_test_workspace",ROOT/"plugins/abilities/Administrator/v3_workspace/adapter.py")
  before=workspace.capture(a/"webagent-v3")
  (a/"webagent-v3/.code-index/derived-only").write_bytes(b"x"* (5*1024*1024))
  self.assertEqual(before,workspace.capture(a/"webagent-v3"))
  self.assertEqual("success",idx.execute_published("code_index_coverage",{"root":"published"},d)["status"])

 def test_published_search_snippet_are_bound_to_one_immutable_release(self):
  j,d,m=self.stage(self.blob())
  before_source=idx.source_digest(d)
  before_index=(d/".code-index/index.sqlite").read_bytes()
  search=idx.execute_published("code_index_lookup",
   {"root":"published","action":"search","query":"main","limit":8,"offset":0},d)
  self.assertEqual("success",search["status"])
  self.assertEqual(j,search["publication"]["release_id"])
  self.assertEqual(m["index_sha256"],search["publication"]["index_sha256"])
  candidate=next(row for row in search["results"] if row["file_path"]=="v3/src/main.rs")
  snippet=idx.execute_published("code_index_lookup",
   {"root":"published","action":"snippet","file_path":candidate["file_path"],
    "start_line":candidate["start_line"],"end_line":candidate["end_line"],"max_chars":512},d)
  self.assertEqual("success",snippet["status"])
  self.assertEqual(search["publication"],snippet["publication"])
  self.assertEqual("fn main() {}",snippet["results"][0]["text"])
  self.assertEqual(before_source,idx.source_digest(d))
  self.assertEqual(before_index,(d/".code-index/index.sqlite").read_bytes())
  self.assertEqual("error",idx.execute_published("code_index_scan",
   {"root":"published","action":"scan"},d)["status"])
  self.assertEqual(before_source,idx.source_digest(d))
  self.assertEqual(before_index,(d/".code-index/index.sqlite").read_bytes())

 def test_seed_pins_one_release_and_copies_private_bootstrap(self):
  worker=load("published_seed_worker",ROOT/"worker.py")
  j,d,m=self.stage(self.blob());rel.activate_assets(self.assets,j)
  base=self.root/"workspace";base.mkdir()
  with patch.object(worker,"TEMPLATE",self.assets/"current/v3"):
   spec=worker._seed_spec(base)
  k,e,n=self.stage(self.blob({"v3/ui/index.html":(420,b"<div>later</div>\n")}))
  rel.activate_assets(self.assets,k)
  self.assertEqual(str(d/"v3"),spec["template"])
  result=worker._seed_inner(spec)
  stage=base/spec["stage"]
  self.assertEqual(j,idx.read_published(stage)[1]["release_id"])
  self.assertEqual(b"<div>app</div>\n",(stage/"ui/index.html").read_bytes())
  stage.rename(base/"webagent-v3")
  with self.assertRaises(ValueError):worker._seed_spec(base)
