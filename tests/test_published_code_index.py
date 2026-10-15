import importlib.util,json,os,shutil,tempfile,unittest,uuid
from pathlib import Path
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[1]
def load(name,path):
 s=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);return m
idx=load("test_idx",ROOT/"plugins/abilities/Administrator/code_index/adapter.py")
rel=load("test_rel",ROOT/"build-service/release.py")
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
 def test_configuration_metadata_survives_published_to_private_projection(self):
  config={"status":"configured","references":[{"path":"config/settings.json","pointers":["/color"]}],"opportunities":[]}
  marker={"version":1,"feature":"bubble-color","aliases":["bubble color"],"purpose":"Paint bubbles","configuration":config}
  j,d,m=self.stage(self.blob({"v3/ui/paint.js":(420,("// @code-index "+json.dumps(marker)+"\nfunction paint() {}\n").encode()),"v3/config/settings.json":(420,b'{"color":"blue"}\n')}))
  args={"root":"published","action":"snippet","feature_id":"bubble-color","file_path":"v3/ui/paint.js","start_line":2,"end_line":2,"max_chars":512}
  published=idx.execute_published("code_index_lookup",args,d)
  self.assertEqual(published["status"],"success",published)
  self.assertEqual(published["results"][0]["configuration_snippets"][0]["text"],'"blue"')
  base=self.root/"session";base.mkdir();shutil.copytree(d/"v3",base/"webagent-v3");idx.seed_private_index(d,base/"webagent-v3")
  args.update(root="webagent-v3",file_path="ui/paint.js")
  private=idx.execute("code_index_lookup",args,base)
  self.assertEqual(private["status"],"success",private)
  self.assertEqual(private["results"][0]["configuration"],config)
  self.assertEqual(private["results"][0]["configuration_snippets"][0]["text"],'"blue"')

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
  workspace=load("published_test_workspace",ROOT/"build-service/workspace.py")
  before=workspace.capture(a/"webagent-v3")
  (a/"webagent-v3/.code-index/derived-only").write_bytes(b"x"* (5*1024*1024))
  self.assertEqual(before,workspace.capture(a/"webagent-v3"))
  self.assertEqual("success",idx.execute_published("code_index_coverage",{"root":"published"},d)["status"])


 def test_published_feature_breadcrumbs_survive_private_seed_projection(self):
  target=b'{"composer":"enabled"}\n';digest=idx.sha(target)
  js=b'// @code-index {"version":1,"feature":"chat-footer","aliases":["chat footer"],"purpose":"Chat footer JavaScript","span_lines":2}\nfunction mountChat() {}\n'
  css=b'/* @code-index {"version":1,"feature":"chat-footer","aliases":["chat footer"],"purpose":"Chat footer CSS","span_lines":2} */\n.chat-footer { display:flex; }\n'
  feature_map=json.dumps({"version":1,"features":[{"id":"chat-footer","aliases":["chat footer"],"purpose":"Shared chat footer","configuration":{"status":"opportunity","references":[],"opportunities":["Consider footer layout settings."]},"locations":[{"path":"config/settings.json","start_line":1,"end_line":1,"sha256":digest}]}]}).encode()+b"\n"
  blob=self.blob({"v3/config/settings.json":(420,target),"v3/plugins/abilities/Administrator/code_index/code-index-features.json":(420,feature_map),
                  "v3/ui/chat.js":(420,js),"v3/ui/chat.css":(420,css)})
  release,folder,manifest=self.stage(blob)
  result=idx.execute_published("code_index_lookup",{"root":"published","action":"search","query":"chat footer","limit":20,"offset":0},folder)
  rows=[row for row in result["results"] if row.get("feature_id")=="chat-footer"]
  self.assertEqual({row["file_path"] for row in rows},{"v3/config/settings.json","v3/ui/chat.js","v3/ui/chat.css"})
  self.assertTrue(all(row["source_path"]=="v3/plugins/abilities/Administrator/code_index/code-index-features.json" or row["source_path"]==row["file_path"] for row in rows))
  workspace=self.root/"private";workspace.mkdir();shutil.copytree(folder/"v3",workspace/"webagent-v3")
  idx.seed_private_index(folder,workspace/"webagent-v3")
  private=idx.execute("code_index_lookup",{"root":"webagent-v3","action":"search","query":"chat footer","limit":20,"offset":0},workspace)
  private_rows=[row for row in private["results"] if row.get("feature_id")=="chat-footer"]
  self.assertEqual({row["file_path"] for row in private_rows},{"config/settings.json","ui/chat.js","ui/chat.css"})
  self.assertTrue(all(row["stale"] is False for row in private_rows))

 def test_published_feature_map_uses_v3_namespace_when_shared_ui_path_collides(self):
  marker=b'// @code-index {"version":1,"feature":"chat-footer","aliases":["chat footer"],"purpose":"V3 chat footer behavior","span_lines":2}\nfunction mountV3ChatFooter() {}\n'
  shared=b'function mountSharedLegacyFooter() {}\n'
  feature_map=json.dumps({"version":1,"features":[{"id":"chat-footer","aliases":["chat footer"],"purpose":"Chat footer","locations":[{"path":"ui/main-panel/agents/identity/tool-output-policy.js","start_line":2,"end_line":2}]}]}).encode()+b"\n"
  blob=self.blob({
   "v3/ui/main-panel/agents/identity/tool-output-policy.js":(420,marker),
   "ui/main-panel/agents/identity/tool-output-policy.js":(420,shared),
   "v3/plugins/abilities/Administrator/code_index/code-index-features.json":(420,feature_map),
  })
  j,d,m=self.stage(blob)
  result=idx.execute_published("code_index_lookup",{"root":"published","action":"search","query":"chat footer","limit":20,"offset":0},d)
  self.assertEqual("success",result["status"],result)
  rows=[row for row in result["results"] if row.get("feature_id")=="chat-footer"]
  self.assertEqual({"v3/ui/main-panel/agents/identity/tool-output-policy.js"},{row["file_path"] for row in rows})
  self.assertTrue(all(row["source_path"]=="v3/plugins/abilities/Administrator/code_index/code-index-features.json" or row["source_path"]==row["file_path"] for row in rows))
  self.assertNotIn("ui/main-panel/agents/identity/tool-output-policy.js",{row["file_path"] for row in rows})
  workspace=self.root/"private";workspace.mkdir();shutil.copytree(d/"v3",workspace/"webagent-v3")
  idx.seed_private_index(d,workspace/"webagent-v3")
  private=idx.execute("code_index_lookup",{"root":"webagent-v3","action":"search","query":"chat footer","limit":20,"offset":0},workspace)
  private_rows=[row for row in private["results"] if row.get("feature_id")=="chat-footer"]
  self.assertEqual({"ui/main-panel/agents/identity/tool-output-policy.js"},{row["file_path"] for row in private_rows})
  self.assertTrue(all(row["stale"] is False for row in private_rows))

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

 def test_published_search_prefers_explicit_path_and_keeps_multiline_metadata_bound(self):
  target=b"pub fn project(\n    history: &[Value],\n    now: Instant,\n) -> Vec<Value> {\n    history.iter().filter(|item| item.is_read_only()).cloned().collect()\n}\n"
  j,d,m=self.stage(self.blob({"v3/src/context/tool_output.rs":(420,target),
      "v3/src/unrelated.rs":(420,b"pub fn project_test_helper() {}\n")}))
  before_source=idx.source_digest(d);before_index=(d/".code-index/index.sqlite").read_bytes()
  query="src/context/tool_output.rs project. Use supplied snippets only; call no tools."
  search=idx.execute_published("code_index_lookup",
   {"root":"published","action":"search","query":query,"limit":10,"offset":0},d)
  self.assertEqual("success",search["status"])
  self.assertEqual(j,search["publication"]["release_id"])
  candidate=search["results"][0]
  self.assertEqual("v3/src/context/tool_output.rs",candidate["file_path"])
  self.assertEqual("project",candidate["label"])
  self.assertEqual(6,candidate["line_count"])
  self.assertEqual("uncertain",candidate["parser_status"])
  self.assertGreaterEqual(candidate["ranking_score"],candidate["score"])
  snippet=idx.execute_published("code_index_lookup",
   {"root":"published","action":"snippet","file_path":candidate["file_path"],
    "start_line":candidate["start_line"],"end_line":min(candidate["start_line"]+39,candidate["line_count"]),"max_chars":12000},d)
  self.assertEqual("success",snippet["status"])
  self.assertEqual(search["publication"],snippet["publication"])
  source=snippet["results"][0]
  self.assertEqual(6,source["end_line"])
  self.assertIn("history.iter().filter",source["text"])
  self.assertIn("collect()",source["text"])
  self.assertEqual(before_source,idx.source_digest(d))
  self.assertEqual(before_index,(d/".code-index/index.sqlite").read_bytes())

 def test_seed_pins_one_release_and_copies_private_bootstrap(self):
  worker=load("published_seed_worker",ROOT/"worker.py")
  j,d,m=self.stage(self.blob());rel.activate_assets(self.assets,j)
  base=self.root/"workspace";base.mkdir()
  with patch.object(worker,"TEMPLATE",self.assets/"current/v3"):
   spec=worker.workspace_support()._seed_spec(base)
  k,e,n=self.stage(self.blob({"v3/ui/index.html":(420,b"<div>later</div>\n")}))
  rel.activate_assets(self.assets,k)
  self.assertEqual(str(d/"v3"),spec["template"])
  result=worker._seed_inner(spec)
  stage=base/spec["stage"]
  self.assertEqual(j,idx.read_published(stage)[1]["release_id"])
  self.assertEqual(b"<div>app</div>\n",(stage/"ui/index.html").read_bytes())
  stage.rename(base/"webagent-v3")
  with self.assertRaises(ValueError):worker.workspace_support()._seed_spec(base)

class IncrementalScanTests(unittest.TestCase):
 def test_only_changed_files_are_parsed_and_deletions_removed(self):
  import sqlite3
  with tempfile.TemporaryDirectory()as tmp:
   root=Path(tmp);(root/"a.py").write_text("def first(): pass\n");(root/"b.py").write_text("def second(): pass\n")
   conn=sqlite3.connect(":memory:");conn.row_factory=sqlite3.Row;conn.execute("PRAGMA foreign_keys=ON");conn.executescript(idx.DDL)
   fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY)
   try:
    args={"action":"scan","scope":"","limit":1,"offset":0};idx.scan(conn,fd,args)
    with patch.object(idx,"scan_data",wraps=idx.scan_data)as parse:
     result=idx.scan(conn,fd,args);self.assertEqual(parse.call_count,0);self.assertEqual(result["reused_files"],2)
     (root/"a.py").write_text("def changed(): pass\n");idx.scan(conn,fd,args);self.assertEqual(parse.call_count,1)
     (root/"b.py").unlink();idx.scan(conn,fd,args)
     self.assertEqual(conn.execute("SELECT COUNT(*) FROM files").fetchone()[0],1)
   finally:os.close(fd);conn.close()

class PerFileRecipeTests(unittest.TestCase):
 def test_narrow_scan_does_not_certify_other_files_after_parser_recipe_changes(self):
  import sqlite3
  with tempfile.TemporaryDirectory()as tmp:
   root=Path(tmp)
   for p in ("a.py","b.py"):(root/p).write_text("def fixture(): pass\n")
   conn=sqlite3.connect(":memory:");conn.row_factory=sqlite3.Row;conn.execute("PRAGMA foreign_keys=ON");conn.executescript(idx.DDL)
   fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY)
   try:
    args={"action":"scan","scope":"","limit":1,"offset":0};idx.scan(conn,fd,args)
    conn.execute("UPDATE file_recipes SET recipe='obsolete'");conn.commit()
    idx.scan(conn,fd,{**args,"action":"scan_file","file_path":"a.py"})
    with patch.object(idx,"scan_data",wraps=idx.scan_data)as parse:
     r=idx.scan(conn,fd,args);self.assertEqual(parse.call_count,1);self.assertEqual(r["reused_files"],1)
   finally:os.close(fd);conn.close()

class PrivateCheckpointSeedTests(unittest.TestCase):
 setUp=PublishedTests.setUp
 tearDown=PublishedTests.tearDown
 blob=PublishedTests.blob
 stage=PublishedTests.stage
 def test_settled_private_source_is_not_reparsed_during_final_publication(self):
  from test_code_index_maintenance import ledger
  old,previous,_manifest=self.stage(self.blob())
  source=self.root/"private";shutil.copytree(previous/"v3",source)
  (source/"new.py").write_text("def settled(): return 1\n")
  cache=self.root/"cache";ledger.source_inventory(idx,source,"",cache,published_seed=previous)
  job=str(uuid.uuid4());candidate=rel.stage_assets(self.assets,job,self.blob({"v3/new.py":(420,(source/"new.py").read_bytes())}),lambda _:None)
  with patch.object(idx,"scan_data",wraps=idx.scan_data)as parser:
   manifest=idx.build_published(candidate,candidate/".code-index",job,"a"*64,seed=previous,private_seeds=[cache])
   self.assertEqual(parser.call_count,0)
  self.assertEqual(manifest["indexing"]["parsed_files"],0)
  self.assertEqual(manifest["gaps"],0)
 def test_integration_changes_refresh_only_the_invalidated_private_file(self):
  from test_code_index_maintenance import ledger
  _old,previous,_manifest=self.stage(self.blob())
  source=self.root/"private";shutil.copytree(previous/"v3",source)
  (source/"new.py").write_text("def settled(): return 1\n")
  cache=self.root/"cache";ledger.source_inventory(idx,source,"",cache,published_seed=previous)
  job=str(uuid.uuid4());candidate=rel.stage_assets(self.assets,job,self.blob({"v3/new.py":(420,b"def integrated(): return 2\n")}),lambda _:None)
  with patch.object(idx,"scan_data",wraps=idx.scan_data)as parser:
   manifest=idx.build_published(candidate,candidate/".code-index",job,"b"*64,seed=previous,private_seeds=[cache])
   self.assertEqual(parser.call_count,1)
  self.assertEqual(manifest["indexing"]["parsed_files"],1)
 def test_tampered_seed_never_publishes(self):
  from test_code_index_maintenance import ledger
  _old,previous,_manifest=self.stage(self.blob())
  source=self.root/"private";shutil.copytree(previous/"v3",source)
  cache=self.root/"cache";ledger.source_inventory(idx,source,"",cache,published_seed=previous)
  with(cache/"index.sqlite").open("ab")as stream:stream.write(b"changed")
  job=str(uuid.uuid4());candidate=rel.stage_assets(self.assets,job,self.blob(),lambda _:None)
  with self.assertRaisesRegex(ValueError,"hash"):
   idx.build_published(candidate,candidate/".code-index",job,"c"*64,seed=previous,private_seeds=[cache])
  self.assertFalse((candidate/".code-index").exists())
