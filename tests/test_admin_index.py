"""Public five-tool behavior against temporary source snapshots only."""
import importlib.util
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

spec=importlib.util.spec_from_file_location("v3_admin_index",Path(__file__).parents[1]/"plugins/abilities/Administrator/code_index/adapter.py")
index=importlib.util.module_from_spec(spec)
spec.loader.exec_module(index)

class AdminIndexTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.base=Path(self.temp.name)
        self.repo=self.base/"webagent-v3"
        (self.repo/"ui").mkdir(parents=True)
        (self.repo/"src").mkdir()
        (self.repo/"ui/app.js").write_text("function greet() {\n  document.querySelector('#hello').addEventListener('click', () => {});\n  fetch('/api/v3/sessions');\n}\n// 🙂 é\n",encoding="utf-8")
        (self.repo/"ui/view.html").write_text('<div data-id="x"><script src="app.js"></script></div>\n\n',encoding="utf-8")
        (self.repo/"ui/style.css").write_text("#hello {\n color: red;\n}\n",encoding="utf-8")
        (self.repo/"worker.py").write_text("class Worker:\n    def read(self):\n        return 'é'\n",encoding="utf-8")
        (self.repo/"src/main.rs").write_text("fn main() {}\n",encoding="utf-8")
    def tearDown(self):
        self.temp.cleanup()
    def call(self,name,**args):
        result=index.execute("code_index_"+name,args,self.base)
        self.assertEqual(result["status"],"success",result)
        self.assertLessEqual(len(json.dumps(result,ensure_ascii=False).encode()),index.MAX_OUTPUT_BYTES)
        return result
    def feature_map(self):
        """Create the ability-owned feature-map fixture without changing targets."""
        path=self.repo/"plugins/abilities/Administrator/code_index/code-index-features.json"
        path.parent.mkdir(parents=True,exist_ok=True)
        return path
    def scan(self,**args):
        return self.call("scan",action="scan",**args)

    def test_configuration_assessment_and_current_source_json(self):
        config={"status":"configured","references":[{"path":"config/presentation.json","pointers":["/color","/missing"]}],"opportunities":[]}
        marker={"version":1,"feature":"bubble-color","aliases":["bubble color"],"purpose":"Paint chat bubbles","configuration":config}
        (self.repo/"ui/app.js").write_text("// @code-index "+json.dumps(marker)+"\nfunction paint() { return true; }\n")
        folder=self.repo/"config";folder.mkdir()
        (folder/"presentation.json").write_text('{"color":"blue"}')
        self.scan()
        found=self.call("lookup",action="search",query="bubble color")["results"][0]
        self.assertEqual(found["configuration"]["status"],"configured")
        snippet=self.call("lookup",action="snippet",file_path="ui/app.js",feature_id="bubble-color",start_line=2,end_line=2,max_chars=512)["results"][0]
        settings=snippet["configuration_snippets"]
        self.assertEqual(settings[0]["text"],'"blue"')
        self.assertEqual(settings[1]["status"],"unset")
        self.assertEqual(settings[0]["sha256"],index.sha(b'{"color":"blue"}'))
        self.assertLessEqual(len(snippet["text"].encode())+sum(len(row.get("text","").encode()) for row in settings),512)
        bad=index.execute("code_index_lookup",{"action":"snippet","file_path":"worker.py","feature_id":"bubble-color","start_line":1,"end_line":1},self.base)
        self.assertEqual(bad["status"],"error")
        (folder/"presentation.json").write_text('{"color":"red"}')
        stale=self.call("lookup",action="snippet",file_path="ui/app.js",feature_id="bubble-color",start_line=2,end_line=2)["results"][0]
        self.assertEqual(stale["configuration_snippets"][0]["status"],"stale")
        self.scan()
        fresh=self.call("lookup",action="snippet",file_path="ui/app.js",feature_id="bubble-color",start_line=2,end_line=2)["results"][0]
        self.assertEqual(fresh["configuration_snippets"][0]["text"],'"red"')

    def test_configuration_opportunities_validation_and_legacy_database(self):
        opportunity={"status":"opportunity","references":[],"opportunities":["Make retry delay configurable."]}
        self.assertEqual(index._configuration(opportunity),opportunity)
        for bad in [{"status":"configured"},{"status":"opportunity","references":[]},
                    {"status":"not_applicable"},{"status":"configured","references":[{"path":"../secret.json","pointers":["/x"]}]},
                    {"status":"configured","references":[{"path":"config/x.json","pointers":["/bad~2"]}]}]:
            with self.assertRaises(ValueError):index._configuration(bad)
        self.scan()
        key=index.sha(b"webagent-v3")
        path=self.base/".code-index"/(key+".sqlite")
        conn=sqlite3.connect(path);conn.execute("DROP TABLE feature_configurations");conn.commit();conn.close()
        self.call("lookup",action="search",query="worker")
        self.scan()

    def test_complete_surface_physical_lines_symbols_edges_and_progress(self):
        discovered=self.call("scan",action="discover")
        self.assertEqual(discovered["total"],5)
        scanned=self.scan(expected_revision=0,batch_id="first")
        self.assertEqual(scanned["indexed_files"],5)
        self.assertEqual(scanned["revision"],1)
        coverage=self.call("coverage")
        self.assertEqual(coverage["gaps"],0)
        self.assertEqual(coverage["total_lines"],14)
        self.assertEqual(coverage["unindexed_count"],0)
        self.assertEqual(coverage["stale_count"],0)
        line=self.call("lookup",action="line",file_path="worker.py",line_no=3)["results"][0]
        self.assertEqual(line["owner_symbol"],"read")
        self.assertFalse(line["stale"])
        symbol=self.call("lookup",action="symbol",query="Worker")["results"][0]
        self.assertEqual(symbol["kind"],"class")
        self.assertEqual(symbol["uncertain"],0)
        node=self.call("trace",kind="selector",value="#hello")["results"][0]
        self.assertEqual(node["file_path"],"ui/app.js")
        self.assertEqual(node["line_no"],2)
        self.assertEqual(self.call("trace",kind="event",value="click")["total"],1)
        self.assertEqual(self.call("trace",kind="endpoint",value="/api/v3/sessions")["total"],1)
        self.assertEqual(self.call("trace",kind="asset",value="app.js")["total"],1)
        progress=self.call("progress",action="get",batch_id="first")["results"][0]
        self.assertEqual(progress["status"],"completed")
        self.assertEqual(progress["indexed_files"],5)
        self.assertEqual(self.call("progress")["total"],1)
    def test_automatic_search_and_snippet_are_bounded_and_freshness_checked(self):
        self.scan()
        found=self.call("lookup",action="search",query="worker read",limit=10)
        self.assertGreater(found["total"],0)
        candidate=next(row for row in found["results"] if row["file_path"]=="worker.py")
        self.assertEqual(candidate["kind"],"function")
        excerpt=self.call("lookup",action="snippet",file_path="worker.py",
            start_line=candidate["start_line"],end_line=candidate["end_line"],max_chars=512)
        source=excerpt["results"][0]
        self.assertEqual(source["sha256"],self.call("lookup",action="file",file_path="worker.py")["results"][0]["sha256"])
        self.assertIn("def read",source["text"])
        self.assertFalse(source["stale"])
        (self.repo/"worker.py").write_text("class Worker:\n    def changed(self):\n        return 7\n",encoding="utf-8")
        stale=index.execute("code_index_lookup",{"action":"snippet","file_path":"worker.py","start_line":1,"end_line":2},self.base)
        self.assertEqual(stale["status"],"error")
        for args in [
            {"action":"search","query":"   "},
            {"action":"snippet","file_path":"worker.py","start_line":1,"end_line":81},
            {"action":"snippet","file_path":"worker.py","start_line":2,"end_line":1},
            {"action":"snippet","file_path":"worker.py","start_line":1,"end_line":2,"max_chars":12001},
        ]:
            self.assertEqual(index.execute("code_index_lookup",args,self.base)["status"],"error",args)

    def test_explicit_path_and_symbol_rank_with_bounded_context_metadata(self):
        target=self.repo/"src/context/tool_output.rs"
        target.parent.mkdir(parents=True)
        target.write_text("pub fn project(\n    history: &[Value],\n    now: Instant,\n) -> Vec<Value> {\n    history.iter().filter(|item| item.is_read_only()).cloned().collect()\n}\n",encoding="utf-8")
        (self.repo/"src/unrelated.rs").write_text("pub fn project_test_helper() {}\n",encoding="utf-8")
        self.scan()
        query="src/context/tool_output.rs project. Use supplied snippets only; call no tools."
        found=self.call("lookup",action="search",query=query,limit=10)
        candidate=found["results"][0]
        self.assertEqual("src/context/tool_output.rs",candidate["file_path"])
        self.assertEqual("project",candidate["label"])
        self.assertEqual(6,candidate["line_count"])
        self.assertEqual("uncertain",candidate["parser_status"])
        self.assertIsInstance(candidate["score"],float)
        self.assertGreaterEqual(candidate["ranking_score"],candidate["score"])
        excerpt=self.call("lookup",action="snippet",file_path=candidate["file_path"],
            start_line=candidate["start_line"],end_line=min(candidate["start_line"]+39,candidate["line_count"]),max_chars=12000)
        source=excerpt["results"][0]
        self.assertEqual(6,source["end_line"])
        self.assertIn("history.iter().filter",source["text"])
        self.assertIn("collect()",source["text"])

    def test_changed_deleted_source_requires_explicit_refresh(self):
        self.scan()
        old=self.call("lookup",action="file",file_path="ui/app.js")["results"][0]["sha256"]
        (self.repo/"ui/app.js").write_text("function changed() {}\n",encoding="utf-8")
        stale=self.call("lookup",action="file",file_path="ui/app.js")
        self.assertTrue(stale["results"][0]["stale"])
        self.assertEqual(stale["results"][0]["sha256"],old)
        self.assertEqual(self.call("coverage",action="stale")["total"],1)
        refreshed=self.call("scan",action="scan_file",file_path="ui/app.js",expected_revision=1)
        self.assertEqual(refreshed["revision"],2)
        self.assertFalse(self.call("lookup",action="file",file_path="ui/app.js")["results"][0]["stale"])
        (self.repo/"ui/view.html").unlink()
        self.assertEqual(self.call("coverage",action="stale")["total"],1)
        removed=self.scan(expected_revision=2)
        self.assertEqual(removed["removed_count"],1)
        self.assertEqual(self.call("lookup",action="file",file_path="ui/view.html")["total"],0)
        self.assertEqual(self.call("coverage")["stale_count"],0)
    def test_source_race_and_stale_revision_preserve_previous_index(self):
        self.scan()
        result=index.execute("code_index_scan",{"action":"scan","expected_revision":0},self.base)
        self.assertEqual(result["status"],"error")
        self.assertIn("revision conflict",result["output"])
        before=self.call("lookup",action="file",file_path="ui/app.js")["results"][0]["sha256"]
        original=index.read_source;reads=0
        def changed(rootfd,path):
            nonlocal reads
            data=original(rootfd,path)
            if path=="ui/app.js":
                reads+=1
                if reads==2:return b"function raced() {}\n"
            return data
        with mock.patch.object(index,"read_source",side_effect=changed):
            result=index.execute("code_index_scan",{"action":"scan","batch_id":"raced"},self.base)
        self.assertEqual(result["status"],"error")
        self.assertIn("before index commit",result["output"])
        self.assertEqual(self.call("lookup",action="file",file_path="ui/app.js")["results"][0]["sha256"],before)
        self.assertEqual(self.call("progress",action="get",batch_id="raced")["results"][0]["status"],"failed")
        self.assertEqual(self.call("coverage")["revision"],1)
    def test_unicode_invalid_python_and_js_fallback_remain_line_complete(self):
        (self.repo/"worker.py").write_text("def broken(:\n# 🙂\n",encoding="utf-8")
        with mock.patch.object(index,"Language",None):
            self.scan()
        coverage=self.call("coverage")
        self.assertEqual(coverage["gaps"],0)
        files={path:self.call("lookup",action="file",file_path=path)["results"][0]
            for path in [item["path"] for item in self.call("scan",action="discover")["results"]]}
        self.assertEqual(files["worker.py"]["parser_status"],"invalid")
        self.assertEqual(files["ui/app.js"]["parser_status"],"uncertain")
        self.assertIn("tree_sitter_unavailable",files["ui/app.js"]["parser_notes"])
        self.assertEqual(self.call("lookup",action="line",file_path="worker.py",line_no=2)["total"],1)
        self.assertEqual(files["src/main.rs"]["parser_notes"],"rust_lexical_only")
    def test_paths_symlinks_derived_index_and_other_workspace_are_isolated(self):
        self.scan()
        for args in [{"action":"scan","root":"../"}, {"action":"scan","scope":"../secret"},
                     {"action":"scan_file","file_path":"/etc/passwd"},
                     {"action":"scan","user_id":"foreign"}]:
            self.assertEqual(index.execute("code_index_scan",args,self.base)["status"],"error")
        outside=self.base/"private.py";outside.write_text("PRIVATE_SECRET = 1\n")
        (self.repo/"ui/escape.py").symlink_to(outside)
        result=index.execute("code_index_scan",{"action":"scan"},self.base)
        self.assertEqual(result["status"],"error")
        (self.repo/"ui/escape.py").unlink()
        (self.base/"alias").symlink_to(self.repo,target_is_directory=True)
        self.assertEqual(index.execute("code_index_coverage",{"root":"alias"},self.base)["status"],"error")
        second=self.base/"second";(second/"webagent-v3/ui").mkdir(parents=True)
        (second/"webagent-v3/ui/other.js").write_text("function other() {}\n")
        self.assertEqual(index.execute("code_index_lookup",{"action":"symbol","query":"greet"},second)["total"],0)
        self.assertEqual(index.execute("code_index_scan",{"action":"scan"},second)["status"],"success")
        self.assertEqual(self.call("lookup",action="symbol",query="other")["total"],0)
        # Derived files never alias a runtime DB or a sibling inode.
        key=index.sha(b"webagent-v3")
        db=self.base/".code-index"/(key+".sqlite")
        db.unlink();db.symlink_to(outside)
        self.assertEqual(index.execute("code_index_coverage",{},self.base)["status"],"error")
        self.assertEqual(outside.read_text(),"PRIVATE_SECRET = 1\n")
    def test_page_bound_and_parser_limits_are_explicit(self):
        for number in range(100):
            (self.repo/"ui"/("item%03d.json"%number)).write_text("{}\n")
        self.scan()
        page=self.call("coverage",action="report",limit=3)
        self.assertEqual(page["count"],3)
        self.assertTrue(page["has_more"])
        more=self.call("coverage",action="report",limit=3,offset=page["next_offset"])
        self.assertNotEqual(page["results"][0]["path"],more["results"][0]["path"])
        (self.repo/"ui/huge.py").write_bytes(b"x"*(index.MAX_FILE_BYTES+1))
        self.assertEqual(index.execute("code_index_scan",{"action":"scan"},self.base)["status"],"error")

    def test_all_optional_fields_are_projected_per_action_and_zero_revision_is_enforced(self):
        common={"root":"","limit":1,"offset":0}
        padded={"scope":"","file_path":"","batch_id":"","expected_revision":0}
        discovered=self.call("scan",action="discover",**common,**padded)
        self.assertEqual(discovered["root"],"webagent-v3")
        self.assertEqual(discovered["total"],5)
        scanned=self.call("scan",action="scan",**common,**padded)
        self.assertEqual(scanned["revision"],1)
        self.assertTrue(scanned["batch_id"])
        # Numeric zero is an explicit optimistic precondition, never an empty optional.
        stale=index.execute("code_index_scan",{"action":"scan",**common,**padded},self.base)
        self.assertEqual(stale["status"],"error");self.assertIn("revision conflict",stale["output"])
        progress=self.call("progress",action="list",batch_id="",**common)
        self.assertEqual(progress["total"],1)
        self.call("progress",action="get",batch_id=scanned["batch_id"],**common)
        symbols=self.call("lookup",action="symbol",query="Worker",file_path="",line_no=1,**common)
        self.assertEqual(symbols["total"],1)
        edges=self.call("lookup",action="edge",query="click",file_path="",line_no=1,**common)
        self.assertEqual(edges["total"],1)
        file=self.call("lookup",action="file",file_path="worker.py",query="",line_no=1,**common)
        self.assertEqual(file["total"],1)
        line=self.call("lookup",action="line",file_path="worker.py",query="",line_no=3,**common)
        self.assertEqual(line["results"][0]["line_no"],3)
        refresh=self.call("scan",action="scan_file",scope="",file_path="worker.py",
            batch_id="",expected_revision=1,**common)
        self.assertEqual(refresh["revision"],2)
        # A nonempty supported filter remains semantic and cannot be silently ignored.
        self.assertEqual(self.call("lookup",action="symbol",query="Worker",
            file_path="ui/app.js",line_no=1)["total"],0)
    def test_required_paths_batches_unknown_fields_and_confined_roots_remain_strict(self):
        self.scan()
        for name,args in [
            ("scan",{"action":"scan_file","file_path":""}),
            ("lookup",{"action":"file","file_path":"","query":"","line_no":1}),
            ("lookup",{"action":"line","file_path":"","line_no":1}),
            ("progress",{"action":"get","batch_id":""}),
            ("progress",{"action":"get"}),
            ("trace",{"kind":"symbol","value":"Worker","action":"symbol"}),
            ("scan",{"action":"discover","undeclared":"ignored?"}),
            ("lookup",{"action":"symbol","root":"../outside","file_path":"","query":""}),
        ]:
            self.assertEqual(index.execute("code_index_"+name,args,self.base)["status"],"error",(name,args))
        # Only known irrelevant properties are discarded, never arbitrary fields.
        result=self.call("scan",action="discover",file_path="../irrelevant",batch_id="not a batch",
            expected_revision=-1)
        self.assertEqual(result["total"],5)


    def test_feature_breadcrumbs_group_comment_and_map_locations(self):
        config=self.repo/"config";config.mkdir()
        js=self.repo/"ui/chat-footer.js";css=self.repo/"ui/chat-footer.css";target=config/"chat-ui.json"
        js.write_text('// @code-index {"version":1,"feature":"chat-footer","aliases":["message composer","chat footer"],"purpose":"Locates the JavaScript footer behavior","span_lines":2}\nfunction mountFooter() {\n  return true;\n}\n',encoding="utf-8")
        (self.repo/"ui/chat-sidebar.js").write_text('// @code-index {"version":1,"feature":"chat-sidebar","aliases":["chat navigation"],"purpose":"The chat sidebar navigation"}\nfunction sidebar() {}\n',encoding="utf-8")
        css.write_text('/* @code-index {"version":1,"feature":"chat-footer","aliases":["message composer","chat footer"],"purpose":"Locates the composer footer styles","span_lines":2} */\n#chat-footer { display: flex; }\n* { box-sizing: border-box; }\n',encoding="utf-8")
        target.write_text('{"footer":"chat composer"}\n',encoding="utf-8")
        digest=index.sha(target.read_bytes())
        self.feature_map().write_text(json.dumps({"version":1,"features":[
            {"id":"chat-footer","aliases":["message composer","chat footer"],"purpose":"The shared chat composer footer",
             "locations":[{"path":"config/chat-ui.json","start_line":1,"end_line":1,"purpose":"The JSON footer configuration"},{"path":"ui/chat-footer.css","start_line":2,"end_line":3,"purpose":"CSS ID and universal selector rules"}]}
        ]}),encoding="utf-8")
        self.scan()
        found=self.call("lookup",action="search",query="I need to update the chat footer in the composer",limit=20)
        rows=[row for row in found["results"] if row.get("feature_id")=="chat-footer"]
        self.assertEqual({row["file_path"] for row in rows},{"ui/chat-footer.js","ui/chat-footer.css","config/chat-ui.json"})
        first_three=self.call("lookup",action="search",query="please update chat footer",limit=3)
        self.assertEqual({row["file_path"] for row in first_three["results"] if row.get("feature_id")=="chat-footer"},
            {"ui/chat-footer.js","ui/chat-footer.css","config/chat-ui.json"})
        described=self.call("lookup",action="search",query="composer footer styles",limit=20)
        self.assertTrue(any(row.get("feature_id")=="chat-footer" for row in described["results"]))
        self.assertTrue(all(row["kind"]=="code_index_feature" for row in rows))
        self.assertIn("JSON footer configuration",next(row for row in rows if row["file_path"]=="config/chat-ui.json")["location_purpose"])
        keys=[(row["feature_id"],row["file_path"],row["start_line"],row["end_line"]) for row in rows]
        self.assertEqual(len(keys),len(set(keys)))
        first=self.call("lookup",action="search",query="chat footer",limit=1,offset=0)
        second=self.call("lookup",action="search",query="chat footer",limit=1,offset=1)
        self.assertNotEqual(first["results"][0]["file_path"],second["results"][0]["file_path"])
        map_path=self.feature_map()
        map_path.write_text(map_path.read_text(encoding="utf-8").replace("The shared chat composer footer","The updated chat composer footer"),encoding="utf-8")
        stale=self.call("lookup",action="search",query="chat footer",limit=20)
        self.assertTrue(any(row["stale"] for row in stale["results"] if row.get("feature_id")=="chat-footer"))
        self.call("scan",action="scan_file",file_path="plugins/abilities/Administrator/code_index/code-index-features.json",batch_id="feature-map-refresh")

    def test_feature_parser_ignores_string_literals_and_retires_removed_markers(self):
        source='const a = "// @code-index {bad}";\nconst b = '+chr(96)+'/* @code-index {bad} */'+chr(96)+';\n'
        self.assertEqual(index._marker_rows("x.js",source,index.sha(source.encode())),[])
        css='/* @code-index {"version":1,"feature":"css-selectors","aliases":[],"purpose":"CSS selector examples"} */\n#chat-footer { display: flex; }\n* { box-sizing: border-box; }\n'
        css_rows=index._marker_rows("x.css",css,index.sha(css.encode()))
        self.assertEqual(css_rows[0][0],"css-selectors")
        self.assertEqual((css_rows[0][4],css_rows[0][5]),(2,3))
        css_line='// @code-index {"version":1,"feature":"fake","aliases":[],"purpose":"fake"}\n.x { display: block; }\n'
        self.assertEqual(index._marker_rows("x.css",css_line,index.sha(css_line.encode())),[])
        multiline='/* @code-index {\n"version":1} */\n.x {}\n'
        with self.assertRaises(ValueError):index._marker_rows("x.css",multiline,index.sha(multiline.encode()))
        rust='const S: &str = r###"// @code-index {bad}"###;\nfn main() {}\n'
        self.assertEqual(index._marker_rows("x.rs",rust,index.sha(rust.encode())),[])
        long_raw='const S = r'+('#'*20)+'"before\n// @code-index {ignored}\nfn hidden() {}'+('"'+('#'*20))+';\nfn main() {}\n'
        self.assertEqual(index._marker_rows("x.rs",long_raw,index.sha(long_raw.encode())),[])
        ordinary='// This comment discusses @code-index syntax.\nfn main() {}\n'
        self.assertEqual(index._marker_rows("x.rs",ordinary,index.sha(ordinary.encode())),[])
        py='text = """# @code-index {bad}"""\n'
        self.assertEqual(index._marker_rows("x.py",py,index.sha(py.encode())),[])
        path=self.repo/"ui/marked.js"
        path.write_text('// @code-index {"version":1,"feature":"marked","aliases":[],"purpose":"marked behavior"}\nfunction marked() {}\n',encoding="utf-8")
        self.scan()
        self.assertGreater(self.call("lookup",action="search",query="marked behavior",limit=10)["total"],0)
        path.write_text("function marked() {}\n",encoding="utf-8")
        self.call("scan",action="scan_file",file_path="ui/marked.js",batch_id="marker-retire")
        self.assertFalse(any(row.get("feature_id")=="marked" for row in self.call("lookup",action="search",query="marked behavior",limit=10)["results"]))

    def test_feature_map_rejects_stale_target_and_duplicate_ids(self):
        config=self.repo/"config";config.mkdir()
        target=config/"component.json";target.write_text('{"component":true}\n',encoding="utf-8")
        digest=index.sha(target.read_bytes());map_path=self.feature_map()
        feature={"id":"one","aliases":["component"],"purpose":"component config","locations":[{"path":"config/component.json","start_line":1,"end_line":1,"sha256":digest}]}
        map_path.write_text(json.dumps({"version":1,"features":[feature,feature]}),encoding="utf-8")
        result=index.execute("code_index_scan",{"action":"scan","scope":"","limit":20,"offset":0},self.base)
        self.assertEqual(result["status"],"error")
        feature["locations"][0]["path"]="../outside.json"
        map_path.write_text(json.dumps({"version":1,"features":[feature]}),encoding="utf-8")
        self.assertEqual(index.execute("code_index_scan",{"action":"scan","scope":"","limit":20,"offset":0},self.base)["status"],"error")
        feature["locations"][0]["path"]="config/component.json"
        map_path.write_text(json.dumps({"version":1,"features":[feature]}),encoding="utf-8")
        self.scan();target.write_text('{"component":false}\n',encoding="utf-8")
        result=index.execute("code_index_scan",{"action":"scan_file","file_path":"config/component.json","batch_id":"stale-feature","limit":20,"offset":0},self.base)
        self.assertEqual(result["status"],"error")


    def test_feature_markers_support_python_rust_and_reject_malformed_payloads(self):
        py='# @code-index {"version":1,"feature":"py-feature","aliases":["python path"],"purpose":"Python operation"}\ndef run():\n    return 1\n'
        rs='// @code-index {"version":1,"feature":"rust-feature","aliases":["rust path"],"purpose":"Rust operation"}\nfn run() {}\n'
        self.assertEqual(index._marker_rows("worker.py",py,index.sha(py.encode()))[0][0],"py-feature")
        self.assertEqual(index._marker_rows("main.rs",rs,index.sha(rs.encode()))[0][0],"rust-feature")
        for text in [
            '// @code-index {"version":1,"feature":"bad","aliases":[],"purpose":"x","unknown":true}\nfn x() {}\n',
            '// @code-index {"version":1,"feature":"bad","aliases":"wrong","purpose":"x"}\nfn x() {}\n',
            '// @code-index {"version":1,"feature":"bad","aliases":[],"purpose":"x"\nfn x() {}\n',
        ]:
            with self.assertRaises(ValueError):index._marker_rows("x.rs",text,index.sha(text.encode()))


    def test_feature_schema_rejects_bad_types_duplicate_keys_and_bounds(self):
        valid='// @code-index {payload}\nfn run() {{}}\n'
        base={"version":1,"feature":"valid-id","aliases":[],"purpose":"valid purpose"}
        invalid=[]
        for version in (1.0,True,2):
            obj={**base,"version":version};invalid.append(json.dumps(obj))
        for span in (0,81,None,True):
            invalid.append(json.dumps({**base,"span_lines":span}))
        invalid.extend([
            json.dumps({**base,"feature":"bad_id"}),
            json.dumps({**base,"aliases":["Footer"," footer "]}),
            json.dumps({**base,"aliases":["x"]*17}),
            json.dumps({**base,"aliases":["bad\x01alias"]}),
            json.dumps({**base,"purpose":" "}),
            json.dumps({**base,"purpose":"x"*513}),
            '{"version":1,"feature":"a","feature":"b","aliases":[],"purpose":"p"}',
        ])
        for payload in invalid:
            text=valid.format(payload=payload)
            with self.subTest(payload=payload[:100]),self.assertRaises(ValueError):
                index._marker_rows("x.rs",text,index.sha(text.encode()))

        target={"path":"src/worker.py","start_line":1,"end_line":1}
        feature={"id":"valid-id","aliases":[],"purpose":"valid purpose","locations":[target]}
        malformed=[]
        for version in (1.0,True,2):
            malformed.append(json.dumps({"version":version,"features":[feature]}))
        malformed.extend([
            '{"version":1,"version":1,"features":[]}',
            json.dumps({"version":1,"features":[{**feature,"extra":1}]}),
            json.dumps({"version":1,"features":[{**feature,"id":"bad_id"}]}),
            json.dumps({"version":1,"features":[{**feature,"aliases":["Footer"," footer "]}]}),
            json.dumps({"version":1,"features":[{**feature,"aliases":["bad\x01alias"]}]}),
            json.dumps({"version":1,"features":[{**feature,"aliases":["x"]*17}]}),
            json.dumps({"version":1,"features":[{**feature,"purpose":" " }]}),
            json.dumps({"version":1,"features":[{**feature,"purpose":"x"*513}]}),
            json.dumps({"version":1,"features":[{**feature,"locations":[{**target,"end_line":81}]}]}),
            json.dumps({"version":1,"features":[{**feature,"locations":[target]*17}]}),
            '{"version":1,"features":[{"id":"valid-id","id":"other","aliases":[],"purpose":"p","locations":[{"path":"src/worker.py","start_line":1,"end_line":1}]}]}',
        ])
        for raw in malformed:
            with self.subTest(map=raw[:100]),self.assertRaises(ValueError):
                index._map_rows(raw.encode())

    def test_explicit_path_ranks_ordinary_candidate_ahead_of_unrelated_feature(self):
        config=self.repo/"config";config.mkdir()
        worker=self.repo/"src/worker.py";worker.parent.mkdir(parents=True,exist_ok=True)
        worker.write_text("def update_chat_footer_worker():\n    return True\n",encoding="utf-8")
        feature_file=self.repo/"ui/feature.js"
        feature_file.write_text('// @code-index {"version":1,"feature":"chat-footer","aliases":["chat footer"],"purpose":"chat footer behavior"}\nfunction unrelatedFooter() {}\n',encoding="utf-8")
        self.feature_map().write_text(json.dumps({"version":1,"features":[]}),encoding="utf-8")
        self.scan()
        result=self.call("lookup",action="search",query="src/worker.py chat footer",limit=20)
        self.assertTrue(result["results"])
        self.assertEqual(result["results"][0]["file_path"],"src/worker.py")

if __name__=="__main__":unittest.main()
